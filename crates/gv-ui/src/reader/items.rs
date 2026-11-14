// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the Reading view lays out per virtual item.
//!
//! - Paragraphs and headings are the render tree's `gv-inline` paragraphs.
//! - A code chunk (and an HTML block, shown as code-like text until the HTML
//!   pipeline exists) becomes one paragraph of its lines separated by hard
//!   breaks, in the code style, with tabs expanded to stops of four. Without
//!   soft wrap it is laid out at [`NO_WRAP`] and scrolls sideways.
//! - Front matter lines become a paragraph in the properties style, keys in
//!   `text.secondary`.
//! - Thematic breaks, empty list items and empty quotes have fixed heights.
//!
//! Paragraph source offsets are relative to the node's start, as in the
//! render tree. [`ItemCache`] keeps the [`InlineBlock`] of each item that
//! was laid out recently (bounded); [`DocItems`] is the engine's view of a
//! document.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::WindowTextSystem;
use gpui::assets::IconName;
use gv_inline::{Affinity, Bias, Paragraph, ParagraphBuilder};
use gv_render_tree::{
    BlockNode, BoxRef, CodeBlock, ColorToken, InlineContent, Item, ItemId, ItemKind, LinkSuffix,
    Metrics, NodeKind, RenderTree, StatusIcon, StyleTable, TextRole, Variant, link_decor,
};

use super::engine::{Items, TextAnchor};
use crate::icons::FILE_PLUS_DASHED;
use crate::inline::{BoxContent, FontFamilies, FontTable, InlineBlock, InlineText, font_of};

/// The width code is laid out at when it does not wrap.
pub(crate) const NO_WRAP: f32 = 1.0e5;

/// Item layouts kept at most; the least recently used half goes when the
/// cache is full.
const CACHE_CAP: usize = 1_000;

/// Columns of a tab stop in code.
const TAB_STOP: usize = 4;

/// The font of every style of `styles`: the text family for prose, the
/// mono family for code.
pub(crate) fn font_table(styles: &StyleTable, families: &FontFamilies) -> FontTable {
    let mut table = FontTable::new();
    for (id, spec) in styles.ids() {
        let family = match spec.family {
            gv_render_tree::Family::Text => families.text.clone(),
            gv_render_tree::Family::Mono => families.mono.clone(),
        };
        table.set(id, font_of(family, f32::from(spec.weight), spec.italic));
    }
    table
}

/// Whether an item draws the code block frame (code and HTML chunks).
pub(crate) fn is_code(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::Code { .. } | ItemKind::Html { .. })
}

/// The chunk position of a code item: (first, last).
pub(crate) fn chunk_ends(kind: &ItemKind) -> (bool, bool) {
    let (k, n) = kind.chunk();
    (k == 0, k + 1 >= n)
}

/// Height of a code item's frame: border, head and padding on the first
/// chunk, padding and border on the last.
pub(crate) fn chrome_height(kind: &ItemKind, m: &Metrics) -> f32 {
    if !is_code(kind) {
        return 0.0;
    }
    let (first, last) = chunk_ends(kind);
    let mut h = 0.0;
    if first {
        h += m.code_border + m.code_head + m.code_padding_y;
    }
    if last {
        h += m.code_padding_y + m.code_border;
    }
    h
}

/// Px from the top of an item's box (its gap included) to its text.
pub(crate) fn text_top(item: &Item, m: &Metrics) -> f32 {
    let mut y = item.gap_before;
    if is_code(&item.kind) && chunk_ends(&item.kind).0 {
        y += m.code_border + m.code_head + m.code_padding_y;
    }
    y
}

/// Horizontal inset of code text inside the item's content box.
pub(crate) fn code_inset(m: &Metrics) -> f32 {
    m.code_border + m.code_padding_x
}

/// The width an item's text is laid out at, for a column `column` wide.
pub(crate) fn text_width(item: &Item, column: f32, m: &Metrics, wrapped: bool) -> f32 {
    let content = (column - item.content_x(m)).max(1.0);
    if is_code(&item.kind) {
        if wrapped {
            (content - 2.0 * code_inset(m)).max(1.0)
        } else {
            NO_WRAP
        }
    } else {
        content
    }
}

/// The Lucide path of a link status icon.
pub(crate) fn status_icon_path(icon: StatusIcon) -> gpui::SharedString {
    match icon {
        StatusIcon::TriangleAlert => IconName::TriangleAlert.path(),
        StatusIcon::FilePlusDashed => FILE_PLUS_DASHED.into(),
        StatusIcon::LogOut => IconName::LogOut.path(),
        StatusIcon::ArrowUpRight => IconName::ArrowUpRight.path(),
        StatusIcon::Lock => IconName::Lock.path(),
    }
}

/// The block of a paragraph or heading: the render tree's paragraph, with
/// the status icons of its links. The ambiguous count is painted by the
/// reader next to its icon, so that box stays blank here.
fn inline_block(content: &InlineContent) -> InlineBlock {
    let mut block = InlineBlock::new(content.paragraph.clone());
    for (i, link) in content.links.iter().enumerate() {
        let Ok(i) = u32::try_from(i) else {
            break;
        };
        let id = BoxRef::LinkSuffix(i).id();
        match link_decor(link.status).suffix {
            Some(LinkSuffix::Icon { count: Some(_), .. }) => {
                block.set_box_content(id, BoxContent::Blank)
            }
            Some(LinkSuffix::Icon { icon, color, .. }) => block.set_box_content(
                id,
                BoxContent::Icon {
                    path: status_icon_path(icon),
                    color: color.role(),
                },
            ),
            Some(LinkSuffix::Approx) | None => {}
        }
    }
    block
}

/// Appends `text` (from `source`, node-relative) as one code run, with tabs
/// expanded to the next stop of [`TAB_STOP`] columns. `indent` leading
/// characters are virtual (a partly consumed tab) and map to the source
/// start.
fn push_code_line(
    b: &mut ParagraphBuilder,
    text: &str,
    indent: usize,
    source: Range<usize>,
    style: gv_inline::RunStyle,
) {
    if text.is_empty() {
        return;
    }
    let indent = indent.min(text.len());
    let mut out = String::with_capacity(text.len());
    let mut anchors: Vec<(usize, usize)> = Vec::new();
    out.push_str(&text[..indent]);
    if indent > 0 {
        anchors.push((indent, source.start));
    }
    let content = &text[indent..];
    // A replaced character makes the line longer than its source: then it
    // maps as one unit.
    let exact = content.len() == source.len();
    let mut column = indent;
    for (i, ch) in content.char_indices() {
        if ch == '\t' && exact {
            let spaces = TAB_STOP - column % TAB_STOP;
            anchors.push((out.len(), source.start + i));
            out.extend(std::iter::repeat_n(' ', spaces));
            anchors.push((out.len(), source.start + i + 1));
            column += spaces;
        } else {
            out.push(ch);
            column += 1;
        }
    }
    b.text_mapped(&out, style, source, &anchors);
}

/// The paragraph of code lines `lines` of `code` (node-relative sources).
pub(crate) fn code_paragraph(
    code: &CodeBlock,
    lines: Range<usize>,
    styles: &StyleTable,
) -> Paragraph {
    let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Code));
    let style = styles.run_style(TextRole::Code, Variant::PLAIN);
    if let Some(first) = code.lines.get(lines.start) {
        b.source_anchor(first.source.start);
    }
    let first_line = lines.start;
    for i in lines {
        let Some(line) = code.lines.get(i) else {
            break;
        };
        if i > first_line
            && let Some(prev) = code.lines.get(i - 1)
        {
            b.hard_break(prev.source.end..line.start.max(prev.source.end));
        }
        push_code_line(
            &mut b,
            code.line(i),
            usize::from(line.indent),
            line.source.clone(),
            style,
        );
    }
    b.build()
}

/// The paragraph of front matter lines: `key:` in `text.secondary`, the
/// value in `text.primary`.
fn properties_paragraph(code: &CodeBlock, lines: Range<usize>, styles: &StyleTable) -> Paragraph {
    let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Properties));
    let value = styles.run_style(TextRole::Properties, Variant::PLAIN);
    let key = value.with_color(ColorToken::TextSecondary.role());
    if let Some(first) = code.lines.get(lines.start) {
        b.source_anchor(first.source.start);
    }
    let first_line = lines.start;
    for i in lines {
        let Some(line) = code.lines.get(i) else {
            break;
        };
        if i > first_line
            && let Some(prev) = code.lines.get(i - 1)
        {
            b.hard_break(prev.source.end..line.start.max(prev.source.end));
        }
        let text = code.line(i);
        let src = line.source.clone();
        let split = text
            .find(':')
            .filter(|_| text.len() == src.len() && line.indent == 0);
        match split {
            Some(k) => {
                b.text(&text[..=k], key, src.start..src.start + k + 1);
                if k + 1 < text.len() {
                    b.text(&text[k + 1..], value, src.start + k + 1..src.end);
                }
            }
            None => push_code_line(&mut b, text, usize::from(line.indent), src, value),
        }
    }
    b.build()
}

/// The text block of an item, if it has text.
fn make_block(item: &Item, node: &BlockNode, styles: &StyleTable) -> Option<InlineBlock> {
    match (&item.kind, &node.kind) {
        (ItemKind::Paragraph | ItemKind::Heading { .. }, _) => {
            node.inline().map(|c| inline_block(c))
        }
        (ItemKind::Code { lines, .. } | ItemKind::Html { lines, .. }, _) => {
            let code = node.lines()?;
            let range = lines.start as usize..lines.end as usize;
            Some(InlineBlock::new(Arc::new(code_paragraph(
                code, range, styles,
            ))))
        }
        (ItemKind::Properties { lines, .. }, NodeKind::Properties { lines: code, .. }) => {
            let range = lines.start as usize..lines.end as usize;
            Some(InlineBlock::new(Arc::new(properties_paragraph(
                code, range, styles,
            ))))
        }
        _ => None,
    }
}

#[derive(Debug)]
struct Cached {
    block: Option<Rc<RefCell<InlineBlock>>>,
    used: u64,
}

/// The text blocks of recently laid out items, bounded in number.
#[derive(Debug, Default)]
pub(crate) struct ItemCache {
    items: HashMap<ItemId, Cached>,
    tick: u64,
    /// Blocks made (each is shaped on first layout).
    pub built: u64,
    /// Blocks evicted.
    pub evicted: u64,
}

impl ItemCache {
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Forgets the layouts of `ids` (their content changed).
    pub fn forget(&mut self, ids: impl IntoIterator<Item = ItemId>) {
        for id in ids {
            self.items.remove(&id);
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The text block of item `ix` of `tree`, made on first use.
    pub fn block(&mut self, tree: &RenderTree, ix: usize) -> Option<Rc<RefCell<InlineBlock>>> {
        let item = tree.items().get(ix)?;
        let tick = self.tick;
        if let Some(c) = self.items.get_mut(&item.id) {
            c.used = tick;
            return c.block.clone();
        }
        let node = tree.node_of(ix)?;
        let block =
            make_block(item, node, &tree.options().styles).map(|b| Rc::new(RefCell::new(b)));
        if block.is_some() {
            self.built += 1;
        }
        self.items.insert(
            item.id,
            Cached {
                block: block.clone(),
                used: tick,
            },
        );
        block
    }

    /// The block of item `id` if it is cached (no layout).
    pub fn cached(&self, id: ItemId) -> Option<Rc<RefCell<InlineBlock>>> {
        self.items.get(&id).and_then(|c| c.block.clone())
    }

    /// Ends a frame: items used from now on are newer; over the cap, the
    /// least recently used half goes.
    pub fn end_frame(&mut self) {
        self.tick += 1;
        if self.items.len() <= CACHE_CAP {
            return;
        }
        let mut ticks: Vec<u64> = self.items.values().map(|c| c.used).collect();
        let cut = ticks.len() - CACHE_CAP / 2;
        let (_, &mut cutoff, _) = ticks.select_nth_unstable(cut);
        let before = self.items.len();
        self.items.retain(|_, c| c.used >= cutoff);
        self.evicted += (before - self.items.len()) as u64;
    }
}

/// The engine's view of a document: its render tree, laid out through the
/// cache with the window's text system.
pub(crate) struct DocItems<'a> {
    pub tree: &'a RenderTree,
    pub cache: &'a mut ItemCache,
    pub text: &'a RefCell<InlineText>,
    pub ts: &'a WindowTextSystem,
}

impl DocItems<'_> {
    fn metrics(&self) -> &Metrics {
        &self.tree.options().metrics
    }

    fn is_wrapped(&self, item: &Item) -> bool {
        self.tree.code_wrap(item.id.block)
    }
}

impl Items for DocItems<'_> {
    fn len(&self) -> usize {
        self.tree.items().len()
    }

    fn id(&self, ix: usize) -> ItemId {
        self.tree.items()[ix].id
    }

    fn index_of(&self, id: ItemId) -> Option<usize> {
        self.tree.index_of(id)
    }

    fn estimate(&self, ix: usize, width: f32) -> f32 {
        self.tree.items()[ix].height_estimate(width)
    }

    fn measure(&mut self, ix: usize, width: f32) -> f32 {
        let item = &self.tree.items()[ix];
        let m = *self.metrics();
        let content = match self.cache.block(self.tree, ix) {
            Some(block) => {
                let w = text_width(item, width, &m, self.is_wrapped(item));
                let layout = self
                    .text
                    .borrow_mut()
                    .layout(&mut block.borrow_mut(), w, self.ts);
                chrome_height(&item.kind, &m) + layout.height()
            }
            None => match item.kind {
                ItemKind::ThematicBreak => m.rule_height,
                _ => self.tree.options().styles.role(TextRole::Body).line_height,
            },
        };
        item.gap_before + content
    }

    fn text_anchor(&mut self, ix: usize, offset: f32) -> Option<TextAnchor> {
        let item = &self.tree.items()[ix];
        let node = self.tree.node_of(ix)?;
        let block = self.cache.cached(item.id)?;
        let block = block.borrow();
        let layout = block.layout()?;
        let local = offset - text_top(item, self.metrics());
        let line = &layout.lines()[layout.line_at_y(local.max(0.0))];
        let start = layout
            .paragraph()
            .source_offset(line.range.start, Affinity::Downstream);
        Some(TextAnchor {
            source: node.to_document(start),
            delta: local - line.top,
        })
    }

    fn offset_of(&mut self, ix: usize, anchor: &TextAnchor, width: f32) -> Option<f32> {
        let item = &self.tree.items()[ix];
        let node = self.tree.node_of(ix)?;
        let m = *self.metrics();
        let block = self.cache.block(self.tree, ix)?;
        let w = text_width(item, width, &m, self.is_wrapped(item));
        let layout = self
            .text
            .borrow_mut()
            .layout(&mut block.borrow_mut(), w, self.ts);
        let local = anchor.source.checked_sub(node.source.start)?;
        let (logical, _) = layout.paragraph().logical_offset(local, Bias::Left);
        let line = &layout.lines()[layout.line_for(logical, Affinity::Downstream)];
        let top = text_top(item, &m);
        Some(top + line.top + anchor.delta.clamp(-top, line.height))
    }
}

#[cfg(test)]
mod tests {
    use gv_inline::ItemKind as PieceKind;

    use super::*;

    fn tree(md: &str) -> RenderTree {
        let syntax = gv_syntax::parse(md);
        let model = gv_model::DocModel::build(&syntax, md);
        RenderTree::build(
            &syntax,
            &model,
            md,
            gv_render_tree::RenderOptions::commonmark(),
            gv_render_tree::Sources::default(),
        )
    }

    fn code_item(t: &RenderTree) -> usize {
        t.items()
            .iter()
            .position(|it| is_code(&it.kind))
            .expect("a code item")
    }

    #[test]
    fn code_lines_map_back_to_their_source() {
        let md = "- x\n\n  ```rs\n  a\tb\n\n  cd\n  ```\n";
        let t = tree(md);
        let ix = code_item(&t);
        let node = t.node_of(ix).expect("node");
        let code = node.lines().expect("lines");
        let p = code_paragraph(code, 0..code.lines.len(), &t.options().styles);
        // Tab expanded to the next stop; lines joined by hard breaks.
        assert_eq!(p.text(), "a   b\n\ncd");
        let pieces: Vec<_> = p.items().iter().map(|i| i.kind().clone()).collect();
        assert!(matches!(pieces[1], PieceKind::HardBreak));
        // Every text position maps into the document's code text.
        let src = |logical| node.to_document(p.source_offset(logical, Affinity::Downstream));
        assert_eq!(&md[src(0)..src(0) + 1], "a");
        assert_eq!(&md[src(4)..src(4) + 1], "b");
        assert_eq!(&md[src(7)..src(7) + 2], "cd");
        // Inside the expanded tab: the tab itself.
        assert_eq!(&md[src(2)..src(2) + 1], "\t");
    }

    #[test]
    fn link_icons_do_not_land_on_images() {
        // Image 0 and link 0 of one paragraph: distinct boxes.
        let md = "![pic](pic.png) [web](https://example.org)\n";
        let syntax = gv_syntax::parse(md);
        let model = gv_model::DocModel::build(&syntax, md);
        let links = |_: &gv_render_tree::LinkTarget<'_>| gv_render_tree::LinkStatus::External;
        let images = |_: &gv_render_tree::LinkTarget<'_>| gv_render_tree::ImageSize::Known {
            width: 40.0,
            height: 20.0,
        };
        let t = RenderTree::build(
            &syntax,
            &model,
            md,
            gv_render_tree::RenderOptions::commonmark(),
            gv_render_tree::Sources {
                links: &links,
                images: &images,
            },
        );
        let content = t.nodes()[0].inline().expect("inline");
        let block = inline_block(content);
        assert!(block.box_content(BoxRef::Image(0).id()).is_none());
        assert!(matches!(
            block.box_content(BoxRef::LinkSuffix(0).id()),
            Some(BoxContent::Icon { .. })
        ));
    }

    #[test]
    fn front_matter_keys_are_secondary() {
        let code = CodeBlock {
            fenced: false,
            info: None,
            language: None,
            text: "title: Hi".to_owned(),
            lines: Box::new([gv_render_tree::CodeLine {
                text: 0..9,
                source: 4..13,
                start: 4,
                indent: 0,
                units: 9,
            }]),
        };
        let p = properties_paragraph(&code, 0..1, &StyleTable::preview());
        assert_eq!(p.text(), "title: Hi");
        let key = p.items()[0].style().expect("text").color;
        assert_eq!(key, ColorToken::TextSecondary.role());
        assert_eq!(p.items()[1].source(), 10..13);
    }
}

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
//! - A display math block becomes a centred paragraph of one math box,
//!   typeset by the shared [`math_engine`] with the document's macros
//!   (MathJax scope: definitions flow to later formulas, from the render
//!   tree's one pass over all formulas). A formula that does not render
//!   shows the error in `status.error` above its source in the code style.
//! - Inline formulas are typeset the same way and their boxes resized; one
//!   that does not render keeps its estimated box, drawn as a placeholder
//!   (not designed).
//! - A table row is one paragraph laid out without wrapping: its gap boxes
//!   are set from the table's column widths, measured from the cells of
//!   its first rows ([`TABLE_SAMPLE`]) and widened when a later row is
//!   wider (a one-time reflow of the rows laid out). Columns align per
//!   the delimiter row.
//! - Thematic breaks, empty list items and empty quotes have fixed heights.
//! - A Mermaid diagram is one box of the diagram's size, painted from the
//!   engine's theme-agnostic display list ([`super::diagram`]); it keeps
//!   its estimated height, blank, until its job lands. A fallback card or
//!   the "not recognised" note is drawn above the source in the code style,
//!   inside a code block's frame.
//! - Images are boxes sized from their headers when the document was
//!   built; their pixels come from background jobs ([`super::media`]). A
//!   `<picture>` is one image box, the source picked by the theme.
//!   Blocks whose images or diagram are pending are not kept, so they ask
//!   again every frame they are drawn (and their jobs are cancelled when
//!   they are not).
//!
//! Paragraph source offsets are relative to the node's start, as in the
//! render tree. [`ItemCache`] keeps the [`InlineBlock`] of each item that
//! was laid out recently (bounded); [`DocItems`] is the engine's view of a
//! document.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use gpui::WindowTextSystem;
use gpui::assets::IconName;
use gv_highlight::{HighlightKey, TokenClass};
use gv_inline::{
    Affinity, Align, Background, Bias, BoxKind, InlineBox, Paragraph, ParagraphBuilder,
};
use gv_math::MathEngine;
use gv_render_tree::{
    BlockNode, BoxRef, CodeBlock, ColorToken, InlineContent, InlineRole, Item, ItemId, ItemKind,
    LinkSuffix, MATH_DISPLAY_SCALE, Metrics, NodeKind, Property, RenderTree, StatusIcon,
    StyleTable, TableBlock, TextRole, Variant, link_decor,
};
use gv_syntax::BlockId;
use gv_syntax::table::Alignment;

use super::code_highlight::{self, Jobs, LineSpans};
use super::diagram::{Diagrams, Shown};
use super::engine::{Items, TextAnchor};
use super::media::{ImageShown, ImageTarget, ImageTargets, Images, SchemeOnly, scheme_only};
use crate::icons::FILE_PLUS_DASHED;
use crate::inline::{BoxContent, FontFamilies, FontTable, InlineBlock, InlineText, font_of};

/// The width code is laid out at when it does not wrap.
pub(crate) const NO_WRAP: f32 = 1.0e5;

/// Item layouts kept at most; the least recently used half goes when the
/// cache is full.
const CACHE_CAP: usize = 1_000;

/// Columns of a tab stop in code.
const TAB_STOP: usize = 4;

/// Rows of a table whose cells set its column widths before any is shown
/// (later rows widen a column when they need to).
pub(crate) const TABLE_SAMPLE: usize = 100;

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

/// The vertical padding of a table row's cells.
pub(crate) fn table_padding_y(kind: &ItemKind, m: &Metrics) -> f32 {
    match kind {
        ItemKind::TableRow { row: 0, .. } => m.table_header_padding_y,
        _ => m.table_padding_y,
    }
}

/// Whether an item is a Mermaid diagram (its padding is the code
/// block's, without a head).
pub(crate) fn is_diagram(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::Diagram { .. })
}

/// The chunk position of a code item: (first, last).
pub(crate) fn chunk_ends(kind: &ItemKind) -> (bool, bool) {
    let (k, n) = kind.chunk();
    (k == 0, k + 1 >= n)
}

/// Height of a code item's frame: border, head and padding on the first
/// chunk, padding and border on the last.
pub(crate) fn chrome_height(kind: &ItemKind, m: &Metrics) -> f32 {
    if let ItemKind::TableRow { .. } = kind {
        // The frame's top border or the line above the row, the padding,
        // and the frame's bottom border under the last row.
        let last = chunk_ends(kind).1;
        return m.table_border
            + 2.0 * table_padding_y(kind, m)
            + if last { m.table_border } else { 0.0 };
    }
    if is_diagram(kind) {
        return 2.0 * m.code_padding_y;
    }
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
    let mut y = item.gap_before + item.frame_insets().0;
    if is_code(&item.kind) && chunk_ends(&item.kind).0 {
        y += m.code_border + m.code_head + m.code_padding_y;
    }
    if let ItemKind::TableRow { .. } = item.kind {
        y += m.table_border + table_padding_y(&item.kind, m);
    }
    if is_diagram(&item.kind) {
        y += m.code_padding_y;
    }
    y
}

/// Horizontal inset of code text inside the item's content box.
pub(crate) fn code_inset(m: &Metrics) -> f32 {
    m.code_border + m.code_padding_x
}

/// The width an item's text is laid out at, for a column `column` wide.
pub(crate) fn text_width(item: &Item, column: f32, m: &Metrics, wrapped: bool) -> f32 {
    let mut content = (column - item.content_x(m) - item.content_right(m)).max(1.0);
    if let ItemKind::Properties { .. } = item.kind {
        // The keys' column.
        content = (content - m.property_key - m.property_gap).max(1.0);
    }
    if let ItemKind::TableRow { .. } = item.kind {
        return NO_WRAP;
    }
    if is_diagram(&item.kind) {
        return (content - 2.0 * code_inset(m)).max(1.0);
    }
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

/// Appends `text` (from `source`, node-relative) as code runs, with tabs
/// expanded to the next stop of [`TAB_STOP`] columns. `indent` leading
/// characters are virtual (a partly consumed tab) and map to the source
/// start. `spans` (line-relative, sorted, not overlapping) take their own
/// style; the rest of the line takes `style`.
fn push_code_line(
    b: &mut ParagraphBuilder,
    text: &str,
    indent: usize,
    source: Range<usize>,
    style: gv_inline::RunStyle,
    spans: &[(Range<usize>, gv_inline::RunStyle)],
) {
    if text.is_empty() {
        return;
    }
    let indent = indent.min(text.len());
    // A replaced character makes the line longer than its source: then it
    // maps as one unit, in one run.
    let exact = text.len() - indent == source.len();
    // The runs: gaps in `style`, the virtual indent with the first one.
    let mut runs: Vec<(Range<usize>, gv_inline::RunStyle)> =
        Vec::with_capacity(2 * spans.len() + 1);
    let mut at = 0;
    if exact {
        for (r, s) in spans {
            let (start, end) = (r.start.max(at).max(indent), r.end.min(text.len()));
            if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                continue;
            }
            if start > at {
                runs.push((at..start, style));
            }
            runs.push((start..end, *s));
            at = end;
        }
    }
    if at < text.len() {
        runs.push((at..text.len(), style));
    }
    // Text offset to source offset (exact lines only past the indent).
    let source_of = |t: usize| {
        if exact {
            source.start + t.saturating_sub(indent)
        } else if t == 0 {
            source.start
        } else {
            source.end
        }
    };
    let mut column = 0;
    for (r, run_style) in runs {
        let mut out = String::with_capacity(r.len());
        let mut anchors: Vec<(usize, usize)> = Vec::new();
        let run_source = source_of(r.start)..source_of(r.end);
        let virtual_end = indent.min(r.end);
        if r.start < virtual_end {
            out.push_str(&text[r.start..virtual_end]);
            column += virtual_end - r.start;
            anchors.push((out.len(), source.start));
        }
        let from = r.start.max(virtual_end);
        for (i, ch) in text[from..r.end].char_indices() {
            if ch == '\t' && exact {
                let spaces = TAB_STOP - column % TAB_STOP;
                let at = source_of(from + i);
                anchors.push((out.len(), at));
                out.extend(std::iter::repeat_n(' ', spaces));
                anchors.push((out.len(), at + 1));
                column += spaces;
            } else {
                out.push(ch);
                column += 1;
            }
        }
        b.text_mapped(&out, run_style, run_source, &anchors);
    }
}

/// The paragraph of code lines `lines` of `code` (node-relative sources),
/// coloured by `highlight` where it has spans.
pub(crate) fn code_paragraph(
    code: &CodeBlock,
    lines: Range<usize>,
    styles: &StyleTable,
    highlight: Option<&LineSpans>,
) -> Paragraph {
    let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Code));
    if let Some(first) = code.lines.get(lines.start) {
        b.source_anchor(first.source.start);
    }
    push_code_lines(&mut b, code, lines, styles, highlight);
    b.build()
}

/// The run style of a highlighted class in code.
fn class_style(styles: &StyleTable, class: TokenClass) -> gv_inline::RunStyle {
    let font = class.font_style();
    let mut variant = Variant::PLAIN;
    if font.bold {
        variant = variant.with(Variant::STRONG);
    }
    if font.italic {
        variant = variant.with(Variant::ITALIC);
    }
    styles
        .run_style(TextRole::Code, variant)
        .with_color(code_highlight::token_of(class).role())
}

/// Appends code lines `lines` of `code`, separated by hard breaks.
fn push_code_lines(
    b: &mut ParagraphBuilder,
    code: &CodeBlock,
    lines: Range<usize>,
    styles: &StyleTable,
    highlight: Option<&LineSpans>,
) {
    let style = styles.run_style(TextRole::Code, Variant::PLAIN);
    let highlight = highlight.filter(|h| !h.is_empty());
    let mut class_styles: HashMap<TokenClass, gv_inline::RunStyle> = HashMap::new();
    let mut spans: Vec<(Range<usize>, gv_inline::RunStyle)> = Vec::new();
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
        spans.clear();
        if let Some(h) = highlight {
            spans.extend(h.line(i, text.len()).map(|(r, class)| {
                let s = *class_styles
                    .entry(class)
                    .or_insert_with(|| class_style(styles, class));
                (r, s)
            }));
        }
        push_code_line(
            b,
            text,
            usize::from(line.indent),
            line.source.clone(),
            style,
            &spans,
        );
    }
}

/// The paragraph of the front matter properties written on `lines`: one
/// line per property with its values (the keys are drawn in their column
/// by the view: [`property_rows`]), tags as pills, other lists joined with
/// commas.
fn properties_paragraph(
    code: &CodeBlock,
    props: &[Property],
    lines: Range<usize>,
    styles: &StyleTable,
) -> Paragraph {
    let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Properties));
    let value = styles.run_style(TextRole::Properties, Variant::PLAIN);
    let zoom = styles.zoom();
    let pill = styles
        .run_style(TextRole::Properties, Variant::PLAIN)
        .with_background(Background {
            role: ColorToken::AccentTint.role(),
            padding_x: TAG_PILL_PADDING_X * zoom,
            padding_y: 0.0,
            radius: TAG_PILL_RADIUS * zoom,
        });
    let mut first = true;
    let mut prev_end: Option<usize> = None;
    for p in props.iter().filter(|p| lines.contains(&(p.line as usize))) {
        let Some(line) = code.lines.get(p.line as usize) else {
            continue;
        };
        let src = line.source.clone();
        if first {
            b.source_anchor(src.start);
        } else if let Some(end) = prev_end {
            b.hard_break(end..src.start.max(end));
        }
        first = false;
        prev_end = Some(src.end);
        if p.values.is_empty() {
            b.text(" ", value, src.end..src.end);
            continue;
        }
        if p.tags {
            for (i, v) in p.values.iter().enumerate() {
                if i > 0 {
                    b.text("\u{a0}", value, src.end..src.end);
                }
                let tag = if v.starts_with('#') {
                    v.to_string()
                } else {
                    format!("#{v}")
                };
                b.text(&tag, pill, src.clone());
            }
        } else {
            let joined = p.values.join(", ");
            b.text(&joined, value, src.clone());
        }
    }
    if first && let Some(l) = code.lines.get(lines.start) {
        b.source_anchor(l.source.start);
    }
    b.build()
}

/// The properties written on `lines` of front matter with the item index
/// of their first piece in [`properties_paragraph`]'s paragraph.
pub(crate) fn property_rows<'p>(
    props: &'p [Property],
    lines: Range<usize>,
    paragraph: &Paragraph,
) -> Vec<(usize, &'p Property)> {
    let mut starts = vec![0];
    for (ix, it) in paragraph.items().iter().enumerate() {
        if it.is_hard_break() {
            starts.push(ix + 1);
        }
    }
    props
        .iter()
        .filter(|p| lines.contains(&(p.line as usize)))
        .zip(starts)
        .map(|(p, ix)| (ix, p))
        .collect()
}

/// A tag pill: radius 9, padding 0 8 (the design's 18 px pill).
const TAG_PILL_RADIUS: f32 = 9.0;
const TAG_PILL_PADDING_X: f32 = 8.0;

/// The process-wide math engine: its layout cache is shared by every view
/// (layouts are in em and theme-independent).
pub(crate) fn math_engine() -> &'static MathEngine {
    static ENGINE: OnceLock<MathEngine> = OnceLock::new();
    ENGINE.get_or_init(MathEngine::default)
}

/// The block of a display math item (of node `node_ix`): one centred math
/// box, or the error above the source.
fn math_block(tree: &RenderTree, node_ix: usize, code: &CodeBlock) -> InlineBlock {
    let styles = &tree.options().styles;
    let macros = tree.math_macros(node_ix, 0);
    let source = match (code.lines.first(), code.lines.last()) {
        (Some(a), Some(b)) => a.source.start..b.source.end.max(a.source.start),
        _ => 0..0,
    };
    match math_engine().render(&code.text, true, &macros) {
        Ok(layout) => {
            let em = styles.role(TextRole::Body).size * MATH_DISPLAY_SCALE;
            let style = styles
                .paragraph_style(TextRole::Body)
                .with_align(Align::Center);
            let mut b = ParagraphBuilder::new(style);
            b.inline_box(
                InlineBox::new(
                    BoxKind::Math,
                    layout.width * em,
                    layout.height * em,
                    layout.depth * em,
                ),
                source,
            );
            let mut block = InlineBlock::new(Arc::new(b.build()));
            block.set_box_content(
                0,
                BoxContent::Math {
                    layout,
                    em,
                    color: ColorToken::TextPrimary.role(),
                    text_font: gpui::font(crate::metrics::font::SYSTEM_FALLBACK),
                },
            );
            block
        }
        Err(e) => {
            let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Code));
            let style = styles
                .run_style(TextRole::Code, Variant::PLAIN)
                .with_color(ColorToken::StatusError.role());
            b.source_anchor(source.start);
            b.text(&e.to_string(), style, source.start..source.start);
            if !code.lines.is_empty() {
                b.hard_break(source.start..source.start);
                push_code_lines(&mut b, code, 0..code.lines.len(), styles, None);
            }
            InlineBlock::new(Arc::new(b.build()))
        }
    }
}

/// The formulas of `content` typeset, its boxes sized to them: the
/// paragraph and the boxes' contents, or `None` without formulas.
/// `first` is the index of its first formula among the node's (see
/// [`RenderTree::math_macros`]); `role` is its text role.
fn typeset_math(
    tree: &RenderTree,
    node: usize,
    first: usize,
    content: &InlineContent,
    role: TextRole,
) -> Option<(Paragraph, Vec<(u32, BoxContent)>)> {
    if content.math.is_empty() {
        return None;
    }
    let size = tree.options().styles.role(role).size;
    let mut p = (*content.paragraph).clone();
    let mut boxes = Vec::new();
    for (ix, (item, span)) in content
        .paragraph
        .items()
        .iter()
        .zip(content.spans.iter())
        .enumerate()
    {
        let (InlineRole::Math(i), Some(old)) = (span.role, item.inline_box()) else {
            continue;
        };
        let Some(m) = content.math.get(i as usize) else {
            continue;
        };
        let macros = tree.math_macros(node, first + i as usize);
        // A formula that does not render keeps its estimated box (drawn as
        // a placeholder; not designed).
        let Ok(layout) = math_engine().render(&m.tex, m.display, &macros) else {
            continue;
        };
        let em = size * if m.display { MATH_DISPLAY_SCALE } else { 1.0 };
        p.replace_box(
            ix,
            InlineBox {
                width: layout.width * em,
                ascent: layout.height * em,
                descent: layout.depth * em,
                ..*old
            },
        );
        boxes.push((
            BoxRef::Math(i).id(),
            BoxContent::Math {
                layout,
                em,
                color: role.color().role(),
                text_font: gpui::font(crate::metrics::font::SYSTEM_FALLBACK),
            },
        ));
    }
    Some((p, boxes))
}

/// The block of inline content `content` (of node `node`, whose formulas
/// before it number `first`): its paragraph with formulas typeset, the
/// link status icons, and `gaps` (a table row's gap box widths by column)
/// applied.
fn content_block(
    tree: &RenderTree,
    node: usize,
    first: usize,
    content: &InlineContent,
    role: TextRole,
    gaps: Option<&[f32]>,
) -> InlineBlock {
    let math = typeset_math(tree, node, first, content, role);
    let mut block = inline_block(content);
    let mut paragraph = math.as_ref().map(|(p, _)| p.clone());
    if let Some(gaps) = gaps {
        let p = paragraph.get_or_insert_with(|| (*content.paragraph).clone());
        for (ix, span) in content.spans.iter().enumerate() {
            if let (InlineRole::CellGap(col), Some(old)) =
                (span.role, content.paragraph.items()[ix].inline_box())
                && let Some(&w) = gaps.get(col as usize)
            {
                p.replace_box(
                    ix,
                    InlineBox {
                        width: w.max(0.0),
                        ..*old
                    },
                );
            }
        }
    }
    if let Some(p) = paragraph {
        block.set_paragraph(Arc::new(p));
    }
    for (id, b) in math.map(|(_, b)| b).unwrap_or_default() {
        block.set_box_content(id, b);
    }
    block
}

/// The text role of an item's inline content.
fn content_role(kind: &ItemKind) -> TextRole {
    match kind {
        ItemKind::Heading { level } => TextRole::heading(*level),
        ItemKind::TableRow { row: 0, .. } => TextRole::TableHeader,
        ItemKind::TableRow { .. } => TextRole::TableCell,
        _ => TextRole::Body,
    }
}

/// The formulas of table `t` before row `row`.
fn formulas_before(t: &TableBlock, row: usize) -> usize {
    t.rows.iter().take(row).map(|r| r.content.math.len()).sum()
}

/// Column widths of a table: the widest cell content per column, and the
/// cell widths of the rows measured.
#[derive(Debug, Default)]
pub(crate) struct TableWidths {
    pub columns: Vec<f32>,
    rows: HashMap<u32, Rc<[f32]>>,
}

impl TableWidths {
    /// The gap box widths of a row with cell widths `cells`: padding, and
    /// the room the column alignments leave.
    fn gaps(&self, cells: &[f32], aligns: &[Alignment], pad: f32) -> Vec<f32> {
        let n = self.columns.len();
        let offset = |c: usize| {
            let room = (self.columns[c] - cells.get(c).copied().unwrap_or(0.0)).max(0.0);
            match aligns.get(c) {
                Some(Alignment::Center) => room / 2.0,
                Some(Alignment::Right) => room,
                _ => 0.0,
            }
        };
        let rest = |c: usize| {
            (self.columns[c] - cells.get(c).copied().unwrap_or(0.0)).max(0.0) - offset(c)
        };
        (0..=n)
            .map(|c| {
                if c == 0 {
                    pad + if n > 0 { offset(0) } else { 0.0 }
                } else if c < n {
                    rest(c - 1) + 2.0 * pad + offset(c)
                } else {
                    rest(c - 1) + pad
                }
            })
            .collect()
    }

    /// The table's width: its columns with their padding.
    pub fn width(&self, pad: f32) -> f32 {
        self.columns.iter().map(|w| w + 2.0 * pad).sum()
    }
}

/// The text block of an item, if it has text.
fn make_block(item: &Item, node: &BlockNode, styles: &StyleTable) -> Option<InlineBlock> {
    match (&item.kind, &node.kind) {
        (ItemKind::Paragraph | ItemKind::Heading { .. } | ItemKind::Details, _) => {
            node.inline().map(|c| inline_block(c))
        }
        (ItemKind::Code { lines, .. } | ItemKind::Html { lines, .. }, _) => {
            let code = node.lines()?;
            let range = lines.start as usize..lines.end as usize;
            Some(InlineBlock::new(Arc::new(code_paragraph(
                code, range, styles, None,
            ))))
        }
        (
            ItemKind::Properties { lines, .. },
            NodeKind::Properties {
                lines: code, props, ..
            },
        ) => {
            let range = lines.start as usize..lines.end as usize;
            Some(InlineBlock::new(Arc::new(properties_paragraph(
                code, props, range, styles,
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
    /// Column widths of the tables laid out, by table block.
    tables: HashMap<BlockId, TableWidths>,
    tick: u64,
    /// Blocks made (each is shaped on first layout).
    pub built: u64,
    /// Blocks evicted.
    pub evicted: u64,
    /// Code highlighting of the code blocks laid out.
    pub code: CodeHighlights,
    /// `<details>` opened or closed by the reader, by the summary row's
    /// block (others are as their `open` attribute says). Kept across
    /// loads of the document.
    pub folds: HashMap<BlockId, bool>,
    /// Mermaid diagrams drawn and their jobs.
    pub diagrams: Diagrams,
    /// Decoded images and their jobs.
    pub images: Images,
    /// What the document's images resolved to.
    pub targets: Option<Arc<ImageTargets>>,
    /// The view's theme scheme, scale and motion setting.
    pub view: ViewFacts,
    /// Items whose layout depends on the theme's scheme (pictures,
    /// `#gh-*-mode-only` images).
    scheme_items: std::collections::HashSet<ItemId>,
    /// The image cards of the items laid out (images that cannot be shown).
    pub image_cards: HashMap<ItemId, Vec<ImageCard>>,
    /// The diagram items shown as a card or as their source, with their
    /// card (none for the "not recognised" note).
    pub diagram_frames: HashMap<ItemId, Option<Arc<gv_diagram::FallbackCard>>>,
    /// What each remote image not shown says, by URL (the remote-content
    /// policy's answer, set by the view).
    remote: HashMap<String, super::remote::RemoteShown>,
    /// Items with a remote image's card.
    remote_items: std::collections::HashSet<ItemId>,
}

/// What the view shows images and pictures with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ViewFacts {
    /// The app theme is dark (Semi-light is light).
    pub dark: bool,
    /// Device pixels per px.
    pub scale: f32,
    /// Animations stay on their first frame.
    pub reduce_motion: bool,
}

impl Default for ViewFacts {
    fn default() -> Self {
        Self {
            dark: false,
            scale: 1.0,
            reduce_motion: false,
        }
    }
}

/// An image that cannot be shown, drawn as a card over its box.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ImageCard {
    /// The paragraph item of its box.
    pub piece: usize,
    pub message: String,
    /// The file, when *Open externally* is offered.
    pub open: Option<std::path::PathBuf>,
}

/// Code blocks met while drawing.
#[derive(Debug)]
struct CodeEntry {
    /// The node the key was computed for.
    node: Arc<CodeBlock>,
    /// Its cache key; `None` when it stays plain.
    key: Option<HighlightKey>,
    /// Its lines in the highlighted text.
    lines: Arc<[Range<usize>]>,
}

/// Code highlighting in the Reading view: what was asked for while
/// drawing, and the items to lay out again when a result lands.
#[derive(Debug, Default)]
pub(crate) struct CodeHighlights {
    jobs: Jobs,
    /// Items laid out while their block's highlighting was pending.
    waiting: HashMap<HighlightKey, Vec<ItemId>>,
    /// Jobs to start.
    requests: Vec<(HighlightKey, Arc<CodeBlock>)>,
    /// Per block, the code it showed last and its key.
    blocks: HashMap<BlockId, CodeEntry>,
    /// Per block, the last result drawn: it colours a newer version of the
    /// block line by line while that one is highlighted.
    last: HashMap<BlockId, LineSpans>,
}

/// Code blocks remembered at most (all are forgotten beyond).
const CODE_BLOCKS_CAP: usize = 4_096;

impl CodeHighlights {
    /// What item `item` of code block `block` (`code`) is drawn with: the
    /// block's highlighting if it is cached, else its previous one while a
    /// job runs.
    fn spans(&mut self, block: BlockId, code: &Arc<CodeBlock>, item: ItemId) -> Option<LineSpans> {
        if self.blocks.len() > CODE_BLOCKS_CAP {
            self.blocks.clear();
            self.last.clear();
        }
        let entry = match self.blocks.get(&block) {
            Some(e) if Arc::ptr_eq(&e.node, code) => e,
            _ => {
                let info = code.info.as_deref().or(code.language.as_deref());
                let key = info
                    .filter(|_| code.fenced)
                    .and_then(|info| code_highlight::key_for(&code.text, info));
                let lines: Arc<[Range<usize>]> =
                    code.lines.iter().map(|l| l.text.clone()).collect();
                self.blocks.insert(
                    block,
                    CodeEntry {
                        node: Arc::clone(code),
                        key,
                        lines,
                    },
                );
                self.blocks.get(&block)?
            }
        };
        let key = entry.key?;
        if let Some(h) = code_highlight::cached(&key) {
            let spans = LineSpans::new(h, Arc::clone(&entry.lines));
            self.last.insert(block, spans.clone());
            return Some(spans);
        }
        let waiting = self.waiting.entry(key).or_default();
        if !waiting.contains(&item) {
            waiting.push(item);
        }
        if self.jobs.start(key) {
            self.requests.push((key, Arc::clone(code)));
        }
        let last = self.last.get(&block).cloned();
        #[cfg(test)]
        if last.is_none() {
            code_highlight::tests::count_pending_draw();
        }
        last
    }

    /// The jobs to start (each key once).
    pub fn take_requests(&mut self) -> Vec<(HighlightKey, Arc<CodeBlock>)> {
        std::mem::take(&mut self.requests)
    }

    /// The job for `key` ended: the items drawn while it ran, to lay out
    /// again.
    pub fn landed(&mut self, key: &HighlightKey) -> Vec<ItemId> {
        self.jobs.finish(key);
        self.waiting.remove(key).unwrap_or_default()
    }

    /// Forgets the blocks (another document is shown).
    pub fn forget_blocks(&mut self) {
        self.blocks.clear();
        self.last.clear();
    }
}

impl ItemCache {
    pub fn clear(&mut self) {
        self.items.clear();
        self.tables.clear();
        self.scheme_items.clear();
        self.remote_items.clear();
        self.image_cards.clear();
        self.diagram_frames.clear();
    }

    /// Sets what the view shows images with: a new scheme forgets the
    /// items that depend on it (returned: their heights are stale), a new
    /// scale or motion setting every image's.
    pub fn set_view(&mut self, view: ViewFacts) -> Vec<ItemId> {
        if view == self.view {
            return Vec::new();
        }
        let old = std::mem::replace(&mut self.view, view);
        let mut ids: Vec<ItemId> = Vec::new();
        if old.dark != view.dark {
            ids.extend(self.scheme_items.drain());
        }
        if old.scale != view.scale || old.reduce_motion != view.reduce_motion {
            ids.extend(
                self.items
                    .keys()
                    .copied()
                    .filter(|id| !ids.contains(id))
                    .collect::<Vec<_>>(),
            );
        }
        self.forget(ids.iter().copied());
        ids
    }

    /// Sets what the remote images not shown say. When that changes, the
    /// items with their cards are forgotten and returned (laid out again).
    pub fn set_remote(
        &mut self,
        remote: HashMap<String, super::remote::RemoteShown>,
    ) -> Vec<ItemId> {
        if remote == self.remote {
            return Vec::new();
        }
        self.remote = remote;
        let ids: Vec<ItemId> = self.remote_items.drain().collect();
        self.forget(ids.iter().copied());
        ids
    }

    /// Forgets the layouts of `ids` (their content changed), and the
    /// column widths of their tables.
    pub fn forget(&mut self, ids: impl IntoIterator<Item = ItemId>) {
        for id in ids {
            self.items.remove(&id);
            self.image_cards.remove(&id);
            self.diagram_frames.remove(&id);
            if self.tables.remove(&id.block).is_some() {
                self.items
                    .retain(|k, _| k.block != id.block || k.chunk == 0);
            }
        }
    }

    /// Whether the `<details>` whose summary row is block `block` of
    /// `tree` is open.
    pub fn details_open(&self, tree: &RenderTree, block: BlockId) -> bool {
        if let Some(&open) = self.folds.get(&block) {
            return open;
        }
        tree.index_of(ItemId { block, chunk: 0 })
            .and_then(|ix| tree.node_of(ix))
            .is_some_and(|n| matches!(n.kind, NodeKind::Details { open: true, .. }))
    }

    /// Whether item `ix` of `tree` is folded away in a closed `<details>`.
    pub fn hidden(&self, tree: &RenderTree, ix: usize) -> bool {
        tree.items().get(ix).is_some_and(|item| {
            item.context.iter().any(|f| {
                matches!(f.kind, gv_render_tree::FrameKind::Details)
                    && !self.details_open(tree, f.id)
            })
        })
    }

    /// The column widths of table `block`, once measured.
    pub fn table(&self, block: BlockId) -> Option<&TableWidths> {
        self.tables.get(&block)
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
        let role = content_role(&item.kind);
        self.image_cards.remove(&item.id);
        self.diagram_frames.remove(&item.id);
        let mut pending = false;
        let block = match (&node.kind, &item.kind) {
            (NodeKind::Math(code), _) => Some(math_block(tree, item.node, code)),
            (NodeKind::Diagram(code), _) => {
                Some(self.diagram_block(tree, item, code, &mut pending))
            }
            (NodeKind::Picture(p), ItemKind::Picture) => {
                self.scheme_items.insert(item.id);
                self.picture_block(tree, item, node, p, &mut pending)
            }
            (NodeKind::Table(t), ItemKind::TableRow { row, .. }) => {
                let content = &t.rows.get(*row as usize)?.content;
                let first = formulas_before(t, *row as usize);
                let widths = self.tables.get(&item.id.block);
                let cells = widths.and_then(|w| w.rows.get(row).cloned());
                let gaps = widths.zip(cells).map(|(w, cells)| {
                    let pad = tree.options().metrics.table_padding_x;
                    w.gaps(&cells, &t.aligns, pad)
                });
                let mut block =
                    content_block(tree, item.node, first, content, role, gaps.as_deref());
                self.place_images(item.id, content, &mut block, &mut pending);
                if gaps.is_none() || pending {
                    // Not measured yet: laid out without its column widths,
                    // and not kept.
                    return Some(Rc::new(RefCell::new(block)));
                }
                Some(block)
            }
            (
                NodeKind::Paragraph(c)
                | NodeKind::Heading { content: c, .. }
                | NodeKind::Details { summary: c, .. },
                _,
            ) => {
                let mut block = content_block(tree, item.node, 0, c, role, None);
                self.place_images(item.id, c, &mut block, &mut pending);
                Some(block)
            }
            (NodeKind::Code(code), ItemKind::Code { lines, .. }) => {
                let spans = self.code.spans(item.id.block, code, item.id);
                let range = lines.start as usize..lines.end as usize;
                Some(InlineBlock::new(Arc::new(code_paragraph(
                    code,
                    range,
                    &tree.options().styles,
                    spans.as_ref(),
                ))))
            }
            _ => make_block(item, node, &tree.options().styles),
        }
        .map(|b| Rc::new(RefCell::new(b)));
        if block.is_some() {
            self.built += 1;
        }
        if pending {
            // Laid out again every frame until its content lands, so its
            // jobs are asked for while it shows.
            return block;
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

    /// The block of a diagram item: the diagram, a placeholder of the
    /// estimated height while it is pending, or a card or note above the
    /// source.
    fn diagram_block(
        &mut self,
        tree: &RenderTree,
        item: &Item,
        code: &Arc<CodeBlock>,
        pending: &mut bool,
    ) -> InlineBlock {
        let styles = &tree.options().styles;
        let zoom = styles.zoom();
        let source = match (code.lines.first(), code.lines.last()) {
            (Some(a), Some(b)) => a.source.start..b.source.end.max(a.source.start),
            _ => 0..0,
        };
        let one_box = |w: f32, h: f32, content: Option<BoxContent>| {
            let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Body));
            b.inline_box(InlineBox::new(BoxKind::Other, w, h, 0.0), source.clone());
            let mut block = InlineBlock::new(Arc::new(b.build()));
            block.set_box_content(0, content.unwrap_or(BoxContent::Blank));
            block
        };
        let (title, card) = match self.diagrams.shown(item.id.block, code, item.id) {
            Shown::Rendered(d) => {
                let (w, h) = (d.width * zoom, d.height * zoom);
                return one_box(
                    w,
                    h,
                    Some(BoxContent::Diagram {
                        diagram: d,
                        scale: zoom,
                    }),
                );
            }
            Shown::Pending => {
                *pending = true;
                let lh = styles.role(TextRole::Code).line_height;
                #[allow(clippy::cast_precision_loss)]
                let h = lh * code.lines.len().max(1) as f32;
                return one_box(1.0, h, None);
            }
            Shown::Card(card) => {
                let mut title = format!("{}: {}", card.title(), card.message);
                if let Some((line, col)) = card.line_col {
                    title.push_str(&format!(" (line {line}, column {col})"));
                }
                (title, Some(card))
            }
            Shown::NotRecognised => (gv_diagram::NOT_RECOGNISED_NOTE.to_owned(), None),
            Shown::Unavailable => (
                "Renderer error: the diagram renderer could not start.".to_owned(),
                None,
            ),
        };
        let error = card.is_some();
        self.diagram_frames.insert(item.id, card);
        let spans = self.code.spans(item.id.block, code, item.id);
        let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Code));
        let style = styles
            .run_style(TextRole::Code, Variant::PLAIN.with(Variant::STRONG))
            .with_color(if error {
                ColorToken::StatusError.role()
            } else {
                ColorToken::TextSecondary.role()
            });
        b.source_anchor(source.start);
        b.text(&title, style, source.start..source.start);
        if !code.lines.is_empty() {
            b.hard_break(source.start..source.start);
            b.hard_break(source.start..source.start);
            push_code_lines(&mut b, code, 0..code.lines.len(), styles, spans.as_ref());
        }
        InlineBlock::new(Arc::new(b.build()))
    }

    /// The block of a `<picture>`: one image box, the source picked by the
    /// theme's scheme; `None` when its size is not known (the view draws
    /// the alt text).
    fn picture_block(
        &mut self,
        tree: &RenderTree,
        item: &Item,
        node: &BlockNode,
        p: &gv_render_tree::Picture,
        pending: &mut bool,
    ) -> Option<InlineBlock> {
        let dark = self.view.dark;
        let gv_render_tree::ImageSize::Known { width, height } = p.size(dark) else {
            return None;
        };
        let url = p.pick(dark)?;
        let opts = tree.options();
        let zoom = opts.styles.zoom();
        let (mut w, mut h) = (width * zoom, height * zoom);
        let max = opts.metrics.image_max_width;
        if w > max && max > 0.0 {
            h *= max / w;
            w = max;
        }
        let mut b = ParagraphBuilder::new(opts.styles.paragraph_style(TextRole::Body));
        b.inline_box(
            InlineBox::new(BoxKind::Image, w, h, 0.0).with_id(BoxRef::Image(0).id()),
            0..node.source.len(),
        );
        let mut block = InlineBlock::new(Arc::new(b.build()));
        let target = self.targets.as_ref().and_then(|t| t.get(url));
        let alt = p.alt.clone();
        self.show_image(target, &alt, item.id, 0, (w, h), &mut block, pending);
        Some(block)
    }

    /// Sets the content of the image boxes of `content` in `block`: the
    /// decoded image, nothing while it decodes (a placeholder), a card
    /// when it cannot be shown; `#gh-*-mode-only` images of the other
    /// scheme take no room.
    fn place_images(
        &mut self,
        id: ItemId,
        content: &InlineContent,
        block: &mut InlineBlock,
        pending: &mut bool,
    ) {
        if content.images.is_empty() {
            return;
        }
        let paragraph = Arc::clone(block.paragraph());
        let mut hidden: Vec<usize> = Vec::new();
        for (ix, (piece, span)) in paragraph
            .items()
            .iter()
            .zip(content.spans.iter())
            .enumerate()
        {
            let (InlineRole::Image(i), Some(b)) = (span.role, piece.inline_box()) else {
                continue;
            };
            let Some(info) = content.images.get(i as usize) else {
                continue;
            };
            if let Some(only) = scheme_only(&info.destination) {
                self.scheme_items.insert(id);
                if (only == SchemeOnly::Dark) != self.view.dark {
                    hidden.push(ix);
                    continue;
                }
            }
            let target = self.targets.as_ref().and_then(|t| t.get(&info.destination));
            let size = (b.width, b.ascent + b.descent);
            self.show_image(target, &info.alt, id, ix, size, block, pending);
        }
        if !hidden.is_empty() {
            let mut p = (*paragraph).clone();
            for ix in hidden {
                if let Some(old) = p.items()[ix].inline_box().copied() {
                    p.replace_box(
                        ix,
                        InlineBox {
                            width: 0.0,
                            ascent: 0.0,
                            descent: 0.0,
                            ..old
                        },
                    );
                }
            }
            block.set_paragraph(Arc::new(p));
        }
    }

    /// Shows image `target` (described by `alt`) in the box of paragraph
    /// item `piece` of `block` (`size` px).
    #[allow(clippy::too_many_arguments)]
    fn show_image(
        &mut self,
        target: Option<ImageTarget>,
        alt: &str,
        id: ItemId,
        piece: usize,
        (w, h): (f32, f32),
        block: &mut InlineBlock,
        pending: &mut bool,
    ) {
        let Some(item) = block.paragraph().items().get(piece) else {
            return;
        };
        let Some(box_id) = item.inline_box().map(|b| b.id) else {
            return;
        };
        let card = |message: String, open: Option<std::path::PathBuf>| ImageCard {
            piece,
            message,
            open,
        };
        let (target, remote) = match target {
            Some(ImageTarget::Fetched { path, probe, .. }) => {
                (Some(ImageTarget::Local { path, probe }), true)
            }
            other => (other, false),
        };
        match target {
            Some(ImageTarget::Remote(url)) => {
                // The placeholder: what the remote-content policy says.
                block.set_box_content(box_id, BoxContent::Blank);
                self.remote_items.insert(id);
                let message = match self.remote.get(&url) {
                    Some(shown) => shown.card_message(alt),
                    None => super::remote::RemoteShown::unknown(&url).card_message(alt),
                };
                self.image_cards
                    .entry(id)
                    .or_default()
                    .push(card(message, None));
            }
            Some(ImageTarget::Local { path, probe: Ok(_) }) => {
                let Some(root) = self.targets.as_ref().map(|t| t.root().to_path_buf()) else {
                    return;
                };
                let device = |v: f32| {
                    // In range: a box's side in device pixels.
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let d = (v * self.view.scale).ceil().max(1.0) as u32;
                    d
                };
                let request = gv_media::DecodeRequest {
                    fit: Some((device(w), device(h))),
                    animate: !self.view.reduce_motion,
                };
                match self.images.shown(&path, &root, request, id, remote) {
                    ImageShown::Image(image) => block.set_box_content(
                        box_id,
                        BoxContent::Image(gpui::ImageSource::Render(image)),
                    ),
                    ImageShown::Pending => *pending = true,
                    ImageShown::Failed(e) => {
                        block.set_box_content(box_id, BoxContent::Blank);
                        let open = (e.offers_open_externally() && !remote).then_some(path);
                        self.image_cards
                            .entry(id)
                            .or_default()
                            .push(card(e.card_message(), open));
                    }
                }
            }
            Some(ImageTarget::Local {
                path,
                probe: Err(e),
            }) => {
                block.set_box_content(box_id, BoxContent::Blank);
                let open = (e.offers_open_externally() && !remote).then_some(path);
                self.image_cards
                    .entry(id)
                    .or_default()
                    .push(card(e.card_message(), open));
            }
            _ => {}
        }
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

    /// Measures what item `ix` needs before it is laid out: when it is a
    /// table row, the cells of its row and, the first time, the column
    /// widths of its table from the first [`TABLE_SAMPLE`] rows. A row
    /// wider than the columns widens them, and the rows laid out before
    /// are laid out again.
    pub(crate) fn prepare(&mut self, ix: usize) {
        let tree = self.tree;
        let Some(item) = tree.items().get(ix) else {
            return;
        };
        let ItemKind::TableRow { row, rows } = item.kind else {
            return;
        };
        let Some(node) = tree.node_of(ix) else {
            return;
        };
        let NodeKind::Table(t) = &node.kind else {
            return;
        };
        let id = item.id.block;
        if self
            .cache
            .tables
            .get(&id)
            .is_some_and(|w| w.rows.contains_key(&row))
        {
            return;
        }
        let mut widths = self.cache.tables.remove(&id).unwrap_or_default();
        let first_time = widths.rows.is_empty();
        let n = t.aligns.len();
        widths.columns.resize(n, 0.0);
        let sample = u32::try_from(TABLE_SAMPLE).unwrap_or(u32::MAX);
        let measure: Vec<u32> = if first_time {
            (0..rows.min(sample)).chain(std::iter::once(row)).collect()
        } else {
            vec![row]
        };
        let mut grew = false;
        for r in measure {
            if widths.rows.contains_key(&r) {
                continue;
            }
            let cells = self.cell_widths(t, item.node, r as usize);
            for (c, &w) in cells.iter().enumerate().take(n) {
                if w > widths.columns[c] + 0.01 {
                    widths.columns[c] = w;
                    grew = true;
                }
            }
            widths.rows.insert(r, cells.into());
        }
        if grew && !first_time {
            self.cache
                .items
                .retain(|k, _| k.block != id || k.chunk == 0);
        }
        self.cache.tables.insert(id, widths);
    }

    /// The content widths of the cells of row `row` of table `t` (of node
    /// `node`), laid out without wrapping.
    fn cell_widths(&mut self, t: &TableBlock, node: usize, row: usize) -> Vec<f32> {
        let Some(r) = t.rows.get(row) else {
            return Vec::new();
        };
        let role = if r.header {
            TextRole::TableHeader
        } else {
            TextRole::TableCell
        };
        let first = formulas_before(t, row);
        let mut block = content_block(self.tree, node, first, &r.content, role, None);
        let layout = self.text.borrow_mut().layout(&mut block, NO_WRAP, self.ts);
        let items = layout.paragraph().items();
        let xs: Vec<f32> = r
            .content
            .spans
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.role, InlineRole::CellGap(_)))
            .filter_map(|(ix, _)| {
                let at = items.get(ix)?.range().start;
                Some(layout.caret(at, Affinity::Downstream).x)
            })
            .collect();
        xs.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect()
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
        if self.cache.hidden(self.tree, ix) {
            return 0.0;
        }
        self.tree.items()[ix].height_estimate(width)
    }

    fn measure(&mut self, ix: usize, width: f32) -> f32 {
        if self.cache.hidden(self.tree, ix) {
            return 0.0;
        }
        self.prepare(ix);
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
        let (above, below) = item.frame_insets();
        item.gap_before + above + content + below
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
        self.prepare(ix);
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

/// The engine's view of a render tree that only estimates: for splicing
/// the height index outside a frame, where nothing is laid out.
pub(crate) struct TreeItems<'a>(pub &'a RenderTree);

impl Items for TreeItems<'_> {
    fn len(&self) -> usize {
        self.0.items().len()
    }

    fn id(&self, ix: usize) -> ItemId {
        self.0.items()[ix].id
    }

    fn index_of(&self, id: ItemId) -> Option<usize> {
        self.0.index_of(id)
    }

    fn estimate(&self, ix: usize, width: f32) -> f32 {
        self.0.items()[ix].height_estimate(width)
    }

    fn measure(&mut self, ix: usize, width: f32) -> f32 {
        self.estimate(ix, width)
    }

    fn text_anchor(&mut self, _: usize, _: f32) -> Option<TextAnchor> {
        None
    }

    fn offset_of(&mut self, _: usize, _: &TextAnchor, _: f32) -> Option<f32> {
        None
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

    fn math_tree(md: &str) -> RenderTree {
        let profile = gv_profile::ProfileId::GfmFile;
        let syntax = gv_syntax::parse_with(md, gv_syntax::ParseOptions::builtin(profile));
        let model = gv_model::DocModel::build(&syntax, md);
        RenderTree::build(
            &syntax,
            &model,
            md,
            gv_render_tree::RenderOptions::builtin(profile),
            gv_render_tree::Sources::default(),
        )
    }

    #[test]
    fn inline_formulas_are_typeset_with_the_macros_before_them() {
        let md = "$$\\gdef\\R{\\mathbb{R}}x$$\n\nLet $x \\in \\R$ and $\\frac{1}{2$.\n";
        let t = math_tree(md);
        let mut cache = ItemCache::default();
        let block = cache.block(&t, 1).expect("the paragraph");
        let block = block.borrow();
        let p = block.paragraph();
        let boxes: Vec<_> = p.items().iter().filter_map(|i| i.inline_box()).collect();
        assert_eq!(boxes.len(), 2);
        // `\R` is defined by the math block before: it renders.
        let id = BoxRef::Math(0).id();
        let Some(BoxContent::Math { layout, em, .. }) = block.box_content(id) else {
            panic!("the first formula renders")
        };
        assert!((layout.width * em - boxes[0].width).abs() < 0.01);
        // The second does not: it keeps its estimated box, without content.
        assert!(block.box_content(BoxRef::Math(1).id()).is_none());
    }

    #[test]
    fn gaps_place_cells_in_their_columns() {
        let w = TableWidths {
            columns: vec![10.0, 20.0, 30.0],
            rows: HashMap::new(),
        };
        let aligns = [Alignment::Left, Alignment::Center, Alignment::Right];
        let gaps = w.gaps(&[4.0, 10.0, 6.0], &aligns, 2.0);
        // Left: right after the padding; centred: 5 of 10 spare before;
        // right-aligned: all 24 spare before.
        assert_eq!(
            gaps,
            vec![2.0, 6.0 + 4.0 + 5.0, 5.0 + 4.0 + 24.0, 0.0 + 2.0]
        );
        // Cells end where their columns say.
        let mut x = 0.0;
        let cells = [4.0, 10.0, 6.0];
        let mut ends = Vec::new();
        for (g, c) in gaps.iter().zip(cells) {
            x += g + c;
            ends.push(x);
        }
        assert_eq!(ends, vec![6.0, 31.0, 70.0]);
        assert_eq!(x + gaps[3], w.width(2.0));
    }

    fn math_items(t: &RenderTree) -> Vec<Rc<RefCell<InlineBlock>>> {
        let mut cache = ItemCache::default();
        (0..t.items().len())
            .filter(|&ix| matches!(t.items()[ix].kind, ItemKind::Math { .. }))
            .map(|ix| cache.block(t, ix).expect("a math block"))
            .collect()
    }

    #[test]
    fn math_fences_become_one_centred_math_box() {
        let md =
            "```math\n\\newcommand{\\RR}{\\mathbb{R}}\\RR\n```\n\n```math\nx \\in \\RR^n\n```\n";
        let t = math_tree(md);
        let blocks = math_items(&t);
        assert_eq!(blocks.len(), 2);
        let body = t.options().styles.role(TextRole::Body).size;
        for block in &blocks {
            let block = block.borrow();
            let p = block.paragraph();
            assert_eq!(p.style().align, Align::Center);
            let b = p.items()[0].inline_box().expect("a math box");
            assert_eq!(b.kind, BoxKind::Math);
            // At least the width of one display-size glyph.
            assert!(b.width > body * 0.5 && b.ascent > 0.0, "{b:?}");
            let Some(BoxContent::Math { layout, em, .. }) = block.box_content(0) else {
                panic!("math content")
            };
            assert!((layout.width * em - b.width).abs() < 0.01);
        }
    }

    #[test]
    fn a_formula_that_fails_shows_the_error_above_its_source() {
        let md = "```math\n\\frac{1}{2\n```\n";
        let t = math_tree(md);
        let blocks = math_items(&t);
        let block = blocks[0].borrow();
        assert!(block.box_content(0).is_none());
        let text = block.paragraph().text().to_owned();
        assert!(text.ends_with("\n\\frac{1}{2"), "{text:?}");
        assert!(text.len() > "\\frac{1}{2".len() + 10, "{text:?}");
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
        let p = code_paragraph(code, 0..code.lines.len(), &t.options().styles, None);
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
    fn highlighted_code_keeps_its_source_map() {
        let md = "```rust\nlet\tx = \"highlighted_code_keeps\";\n```\n";
        let t = tree(md);
        let ix = code_item(&t);
        let node = t.node_of(ix).expect("node");
        let code = node.lines().expect("lines");
        let key = code_highlight::key_for(&code.text, "rust").expect("rust");
        let lines: Arc<[Range<usize>]> = code.lines.iter().map(|l| l.text.clone()).collect();
        let spans = LineSpans::new(code_highlight::highlight(key, &code.text), lines);
        let styles = &t.options().styles;
        let p = code_paragraph(code, 0..code.lines.len(), styles, Some(&spans));
        // The tab is expanded as without highlighting.
        assert_eq!(p.text(), "let x = \"highlighted_code_keeps\";");
        let colour = |at: usize| {
            let ix = p.item_at(at).expect("an item");
            p.items()[ix].style().expect("a run").color
        };
        let plain = styles.run_style(TextRole::Code, Variant::PLAIN).color;
        assert_eq!(colour(0), crate::inline::Token::SyntaxTag.role());
        assert_eq!(colour(8), crate::inline::Token::SyntaxString.role());
        assert_eq!(
            colour(p.text().len() - 1),
            crate::inline::Token::TextSecondary.role()
        );
        assert_eq!(colour(4), plain);
        // Every run still maps into the document's code text.
        let src = |logical| node.to_document(p.source_offset(logical, Affinity::Downstream));
        assert_eq!(&md[src(0)..src(0) + 3], "let");
        assert_eq!(&md[src(3)..src(3) + 1], "\t");
        assert_eq!(&md[src(4)..src(4) + 1], "x");
        assert_eq!(&md[src(8)..src(8) + 3], "\"hi");
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
    fn front_matter_values_and_tag_pills() {
        let text = "title: Hi\ntags: [a, b]";
        let line = |a: usize, b: usize| gv_render_tree::CodeLine {
            text: a..b,
            source: 4 + a..4 + b,
            start: 4 + a,
            indent: 0,
            units: 9,
        };
        let code = CodeBlock {
            fenced: false,
            info: None,
            language: None,
            text: text.to_owned(),
            lines: Box::new([line(0, 9), line(10, 22)]),
        };
        let prop = |key: &str, values: &[&str], line: u32, tags: bool| Property {
            key: Arc::from(key),
            values: values.iter().map(|v| Arc::from(*v)).collect(),
            line,
            tags,
        };
        let props = [
            prop("title", &["Hi"], 0, false),
            prop("tags", &["a", "b"], 1, true),
        ];
        let styles = StyleTable::preview();
        let p = properties_paragraph(&code, &props, 0..2, &styles);
        // The values only (the keys are drawn in their column), one line
        // per property; tags are pills.
        assert_eq!(p.text(), "Hi\n#a\u{a0}#b");
        assert_eq!(
            p.items()[0].style().expect("text").color,
            ColorToken::TextPrimary.role()
        );
        let pill = p.items()[2].style().expect("a tag");
        assert_eq!(
            pill.background.map(|b| b.role),
            Some(ColorToken::AccentTint.role())
        );
        let rows = property_rows(&props, 0..2, &p);
        assert_eq!(rows.iter().map(|(ix, _)| *ix).collect::<Vec<_>>(), [0, 2]);
    }
}

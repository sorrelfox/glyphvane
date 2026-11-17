// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Obsidian blocks: callout titles and folds, and note embeds shown in
//! place (transclusion).
//!
//! A note embed is a paragraph holding only `![[Note]]`, `![[Note#Heading]]`
//! or `![[Note#^block]]`. Its own line stays (a link to the note, the
//! embed's header) and the embedded blocks follow inside a
//! [`FrameKind::Embed`] frame. The embedded note comes from the link
//! source ([`crate::LinkStatusSource::embed`]); its blocks are built as
//! plain text (headings keep their level), because their sources are in
//! another file: every embedded node maps to the end of the embed's line.
//! Embeds inside the embedded note are shown the same way, up to the
//! budgets: [`MAX_EMBED_DEPTH`] nested embeds, [`MAX_EMBED_NODES`] blocks
//! and [`MAX_EMBED_BYTES`] of embedded text per outermost embed. A cycle
//! (the same note and section again inside itself) shows a notice instead.
//!
//! A paragraph that is only a canvas or base embed is a live block instead
//! ([`crate::live`]); inside an embedded note, canvas and base embeds and
//! ` ```base ` and ` ```query ` fences are live blocks too, counted
//! against the depth limit.

use std::ops::Range;
use std::sync::Arc;

use gv_model::{Anchoring, BlockMatch, DocModel, FragmentMatch};
use gv_syntax::inline::{
    InlineInput, InlineKind, InlineOptions, TextProjection, parse_inlines_with,
};
use gv_syntax::{
    BlockId, CalloutFold, Container, ContainerKind, Leaf, LeafKind, SyntaxTree, Visitor,
};
use gv_text::TextSource;

use super::{NodeBuilder, Open};
use crate::inline::{self, is_image_path};
use crate::live::{LiveBlock, LiveKind, split_embed};
use crate::node::{
    BlockNode, CalloutType, CodeBlock, CodeLine, Frame, FrameKind, NodeKind, default_callout_title,
};
use crate::status::EmbedFetch;
use crate::style::TextRole;

/// Embeds nested deeper than this are not shown.
pub const MAX_EMBED_DEPTH: usize = 5;
/// Blocks one outermost embed shows at most.
pub const MAX_EMBED_NODES: usize = 10_000;
/// Embedded text one outermost embed reads at most (2 MB).
pub const MAX_EMBED_BYTES: usize = 2 << 20;

/// The space before a callout's title for its icon: the icon (15) and the
/// gap after it (7), in px at zoom 1.
pub const CALLOUT_TITLE_GUTTER: f32 = 22.0;

/// Synthetic block ids have this bit set (the parser's ids never do).
const SYNTHETIC: u64 = 1 << 63;

/// What one outermost embed has used of the budgets.
#[derive(Default)]
struct Budget {
    nodes: usize,
    bytes: usize,
    /// Notes and sections embedded around the current one, outermost
    /// first: the key, the title and the anchor.
    chain: Vec<(Arc<str>, Arc<str>, Option<String>)>,
}

impl<S: TextSource + ?Sized> NodeBuilder<'_, S> {
    /// A callout starts: without a title of its own it gets the type's
    /// name as its title row, made here.
    pub(super) fn callout_untitled(&mut self, c: &Container, fold: CalloutFold) {
        let Some(marker) = c.marker.clone() else {
            return;
        };
        // A title is the rest of the marker's line.
        let mut line = Vec::new();
        let rest = self.text.line(marker.end, &mut line);
        if rest.iter().any(|b| !b.is_ascii_whitespace()) {
            return;
        }
        let name = match self.frames.last().map(|f| &f.kind) {
            Some(FrameKind::Callout { name, .. }) => Arc::clone(name),
            _ => return,
        };
        self.title_cx = self.callout_title_cx();
        let cx = self.inline_cx(c.id);
        let title = Arc::new(inline::build_plain(
            &default_callout_title(&name),
            marker.clone(),
            marker.start,
            TextRole::Body,
            &cx,
        ));
        self.title_cx = None;
        let context = self.context();
        self.push(
            BlockNode {
                id: c.id,
                kind: NodeKind::CalloutTitle { title, fold },
                source: marker.clone(),
                context,
                block: marker.clone(),
                top: marker.clone(),
                lookups: None,
                table: false,
            },
            None,
        );
        self.callout_body(c.id);
    }

    /// The colour and icon gutter of the innermost callout's title.
    pub(super) fn callout_title_cx(&self) -> Option<(crate::style::ColorToken, f32)> {
        let name = self.frames.iter().rev().find_map(|f| match &f.kind {
            FrameKind::Callout { name, .. } => Some(name),
            _ => None,
        })?;
        let (_, color) = CalloutType::of(name).colors();
        Some((color, CALLOUT_TITLE_GUTTER * self.opts.styles.zoom()))
    }

    /// After a foldable callout's title (node `title`): its other blocks
    /// fold with it.
    pub(super) fn callout_body(&mut self, title: BlockId) {
        let Some(Open {
            kind: ContainerKind::Callout { fold },
            body_frame,
            ..
        }) = self.open.last_mut()
        else {
            return;
        };
        if !fold.is_foldable() || *body_frame || self.frames.len() >= super::MAX_FRAMES {
            return;
        }
        *body_frame = true;
        self.frames.push(Frame {
            id: title,
            kind: FrameKind::Details,
        });
        self.ctx = None;
    }

    /// A paragraph that is only a note embed: its line, then the embedded
    /// blocks in an embed frame. Returns false for other paragraphs.
    pub(super) fn embed_paragraph(&mut self, leaf: &Leaf<'_>) -> bool {
        let Some(parse_opts) = self.parse_opts else {
            return false;
        };
        let Some((inner, anchor)) = self.only_note_embed(leaf) else {
            return false;
        };
        if let Some(kind) = LiveKind::of_embed(split_embed(&inner).0) {
            self.live_embed(leaf, kind, &inner);
            return true;
        }
        let frame = self.frames.len() < super::MAX_FRAMES;
        if frame {
            self.frames.push(Frame {
                id: leaf.id,
                kind: FrameKind::Embed { depth: 0 },
            });
            self.ctx = None;
        }
        // The embed's own line: a link to the note. The embedded blocks map
        // to its end.
        let mut at = leaf.range.end;
        if let Some(n) = self.build_leaf(leaf) {
            at = n.source.end;
            self.push(n, None);
        }
        let mut budget = Budget::default();
        if let Some((key, title)) = self.sources.links.document_key() {
            budget.chain.push((key, title, None));
        }
        match self.sources.links.embed(&inner, None) {
            EmbedFetch::Note { key, title, text } => {
                self.transclude(
                    &text,
                    anchor.as_deref(),
                    &key,
                    &title,
                    at,
                    0,
                    parse_opts,
                    &mut budget,
                );
            }
            EmbedFetch::Unavailable | EmbedFetch::Pending => {}
        }
        if frame {
            self.frames.pop();
            self.ctx = None;
        }
        true
    }

    /// A paragraph that is only a canvas or base embed: a live block,
    /// its own line kept as its label (copy, find, exports).
    fn live_embed(&mut self, leaf: &Leaf<'_>, kind: LiveKind, inner: &str) {
        let Some(mut node) = self.build_leaf(leaf) else {
            return;
        };
        let label = match &node.kind {
            NodeKind::Paragraph(c) => Some(Arc::clone(c)),
            _ => None,
        };
        let (path, anchor) = split_embed(inner);
        node.kind = NodeKind::Live(Arc::new(LiveBlock::new(
            kind,
            Some(path.into()),
            anchor.map(Into::into),
            None,
            label,
            None,
            0,
        )));
        self.push(node, None);
    }

    /// A live block inside a note embed, mapped to `at`: its label is
    /// `label`.
    fn live_node(&mut self, live: LiveBlock, at: usize) {
        self.synthetic += 1;
        let id = BlockId(SYNTHETIC | self.synthetic);
        let context = self.context();
        self.push(
            BlockNode {
                id,
                kind: NodeKind::Live(Arc::new(live)),
                source: at..at,
                context,
                block: at..at,
                top: at..at,
                lookups: None,
                table: false,
            },
            None,
        );
    }

    /// The plain label of an embed inside a note embed.
    fn plain_label(&mut self, text: &str, at: usize) -> Arc<crate::node::InlineContent> {
        let cx = self.inline_cx(BlockId(SYNTHETIC | self.synthetic));
        Arc::new(inline::build_plain(text, at..at, at, TextRole::Body, &cx))
    }

    /// The embed of a paragraph that holds only a note embed (not an
    /// image): what is between its brackets, and its anchor.
    fn only_note_embed(&mut self, leaf: &Leaf<'_>) -> Option<(String, Option<String>)> {
        if !self.inline_opts.embeds {
            return None;
        }
        let [line] = leaf.lines else {
            return None;
        };
        let mut buf = Vec::new();
        self.text.copy_range(line.content.clone(), &mut buf);
        let s = buf.trim_ascii();
        if !(s.starts_with(b"![[") && s.ends_with(b"]]")) {
            return None;
        }
        let segments = [line.content.clone()];
        let tree = parse_inlines_with(
            self.text,
            InlineInput {
                segments: &segments,
            },
            &*self.refs,
            InlineOptions {
                block_ids: false,
                ..self.inline_opts
            },
        );
        let mut it = tree
            .top_level()
            .filter(|n| n.kind() != InlineKind::Whitespace);
        let node = it.next()?;
        if it.next().is_some() {
            return None;
        }
        let w = node.wikilink().filter(|w| w.embed)?;
        if is_image_path(w.path().trim()) {
            return None;
        }
        Some((w.inner.to_string(), w.anchor().map(str::to_owned)))
    }

    /// Pushes the blocks of the embedded `text` (the part `anchor` names)
    /// at `at`, the embed's depth being `depth`.
    #[allow(clippy::too_many_arguments)]
    fn transclude(
        &mut self,
        text: &str,
        anchor: Option<&str>,
        key: &Arc<str>,
        title: &Arc<str>,
        at: usize,
        depth: usize,
        parse_opts: gv_syntax::ParseOptions,
        budget: &mut Budget,
    ) {
        if let Some(i) = budget
            .chain
            .iter()
            .position(|(k, _, a)| k == key && a.as_deref() == anchor)
        {
            let mut names: Vec<&str> = budget.chain[i..].iter().map(|(_, t, _)| &**t).collect();
            names.push(title);
            self.notice(&format!("Embed cycle: {}", names.join(" \u{2192} ")), at);
            return;
        }
        if depth >= MAX_EMBED_DEPTH {
            self.notice(
                &format!("Embeds nested more than {MAX_EMBED_DEPTH} deep are not shown."),
                at,
            );
            return;
        }
        let tree = SyntaxTree::parse_with(text, parse_opts);
        let model = DocModel::build(&tree, text);
        let Some(range) = embed_range(&tree, &model, text, anchor) else {
            let what = anchor.unwrap_or("");
            self.notice(&format!("\u{201c}{what}\u{201d} is not in {title}."), at);
            return;
        };
        if budget.bytes + range.len() > MAX_EMBED_BYTES {
            self.notice("This embed is too large to show here.", at);
            return;
        }
        budget.bytes += range.len();
        budget.chain.push((
            Arc::clone(key),
            Arc::clone(title),
            anchor.map(str::to_owned),
        ));
        let mut blocks = Blocks {
            text,
            tree: &tree,
            out: Vec::new(),
            items: 0,
            first_in_item: false,
            fences: self.opts.obsidian_fences,
        };
        tree.walk_range(range.clone(), &mut blocks);
        for block in blocks.out {
            if !range.contains(&block.start) && block.start != range.start {
                continue;
            }
            budget.nodes += 1;
            if budget.nodes > MAX_EMBED_NODES {
                self.notice("This embed has too many blocks to show them all.", at);
                break;
            }
            match block.kind {
                Block::Text { role, text } => self.plain_node(&text, role, at),
                Block::Fence { kind, code } => {
                    if depth + 1 >= MAX_EMBED_DEPTH {
                        self.notice(
                            &format!(
                                "Embeds nested more than {MAX_EMBED_DEPTH} deep are not shown."
                            ),
                            at,
                        );
                        continue;
                    }
                    let live = LiveBlock::new(
                        kind,
                        None,
                        None,
                        Some(Arc::new(code)),
                        None,
                        Some(Arc::clone(key)),
                        u16::try_from(depth + 1).unwrap_or(u16::MAX),
                    );
                    self.live_node(live, at);
                }
                Block::Embed { inner, .. }
                    if LiveKind::of_embed(split_embed(&inner).0).is_some() =>
                {
                    // A canvas or base inside the embedded note: live too,
                    // its `this` the embedded note, within the depth limit.
                    if depth + 1 >= MAX_EMBED_DEPTH {
                        self.notice(
                            &format!(
                                "Embeds nested more than {MAX_EMBED_DEPTH} deep are not shown."
                            ),
                            at,
                        );
                        continue;
                    }
                    let (path, view) = split_embed(&inner);
                    let kind = LiveKind::of_embed(path).unwrap_or(LiveKind::Canvas);
                    let label = self.plain_label(&format!("\u{29c9} {inner}"), at);
                    let live = LiveBlock::new(
                        kind,
                        Some(path.into()),
                        view.map(Into::into),
                        None,
                        Some(label),
                        Some(Arc::clone(key)),
                        u16::try_from(depth + 1).unwrap_or(u16::MAX),
                    );
                    self.live_node(live, at);
                }
                Block::Embed { inner, anchor } => {
                    let pushed = self.frames.len() < super::MAX_FRAMES;
                    if pushed {
                        self.synthetic += 1;
                        self.frames.push(Frame {
                            id: BlockId(SYNTHETIC | self.synthetic),
                            kind: FrameKind::Embed {
                                depth: u16::try_from(depth + 1).unwrap_or(u16::MAX),
                            },
                        });
                        self.ctx = None;
                    }
                    self.plain_node(&format!("\u{29c9} {inner}"), TextRole::Body, at);
                    match self.sources.links.embed(&inner, Some(key)) {
                        EmbedFetch::Note {
                            key: k,
                            title: t,
                            text: body,
                        } => self.transclude(
                            &body,
                            anchor.as_deref(),
                            &k,
                            &t,
                            at,
                            depth + 1,
                            parse_opts,
                            budget,
                        ),
                        EmbedFetch::Unavailable => {
                            self.notice(&format!("\u{201c}{inner}\u{201d} cannot be shown."), at);
                        }
                        EmbedFetch::Pending => {}
                    }
                    if pushed {
                        self.frames.pop();
                        self.ctx = None;
                    }
                }
            }
        }
        budget.chain.pop();
    }

    /// A notice in an embed (a cycle, a budget, a missing section).
    fn notice(&mut self, text: &str, at: usize) {
        self.plain_node(text, TextRole::Body, at);
    }

    /// A plain-text node of an embed, mapped to `at`.
    fn plain_node(&mut self, text: &str, role: TextRole, at: usize) {
        self.synthetic += 1;
        let id = BlockId(SYNTHETIC | self.synthetic);
        let cx = self.inline_cx(id);
        let content = Arc::new(inline::build_plain(text, at..at, at, role, &cx));
        // Headings too are paragraphs (in a heading's type): they are no
        // anchors of this document.
        let kind = NodeKind::Paragraph(content);
        let context = self.context();
        self.push(
            BlockNode {
                id,
                kind,
                source: at..at,
                context,
                block: at..at,
                top: at..at,
                lookups: None,
                table: false,
            },
            None,
        );
    }
}

/// The part of an embedded note `anchor` names: the whole note without
/// its front matter, a heading's section (up to the next heading of the
/// same or a higher level), or a block (a list item with its children;
/// the block before a block id on a line of its own). `None` when the
/// anchor names nothing.
fn embed_range(
    tree: &SyntaxTree,
    model: &DocModel,
    text: &str,
    anchor: Option<&str>,
) -> Option<Range<usize>> {
    let Some(anchor) = anchor.map(str::trim).filter(|a| !a.is_empty()) else {
        let start = model.front_matter().map_or(0, |f| f.range.end);
        return Some(start..text.len());
    };
    let parts: Vec<&str> = anchor
        .split('#')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if let Some(id) = parts.last().and_then(|p| p.strip_prefix('^')) {
        let BlockMatch::Found { index, .. } = model.match_block(id) else {
            return None;
        };
        let a = model.block_anchors().get(index)?;
        return Some(block_range(tree, text, a.range.clone(), a.own_line));
    }
    match model.match_fragment(&parts, Anchoring::HeadingText) {
        FragmentMatch::Heading { index, .. } => model.section(index),
        FragmentMatch::Top => Some(0..text.len()),
        _ => None,
    }
}

/// The block a block id names: the list item whose first paragraph ends
/// with it, else the paragraph; for an id alone, the block before it.
fn block_range(tree: &SyntaxTree, text: &str, para: Range<usize>, own_line: bool) -> Range<usize> {
    struct Find {
        para: Range<usize>,
        own_line: bool,
        /// Blocks seen, as (range, is an item).
        blocks: Vec<(Range<usize>, bool)>,
        open: Vec<(usize, bool)>,
    }
    impl Visitor for Find {
        fn enter(&mut self, c: &Container) {
            self.open
                .push((c.range.start, c.kind == ContainerKind::Item));
        }
        fn exit(&mut self, c: &Container) {
            self.open.pop();
            self.blocks
                .push((c.range.clone(), c.kind == ContainerKind::Item));
        }
        fn leaf(&mut self, l: &Leaf<'_>) {
            self.blocks.push((l.range.clone(), false));
        }
    }
    let mut f = Find {
        para: para.clone(),
        own_line,
        blocks: Vec::new(),
        open: Vec::new(),
    };
    tree.walk(&mut f);
    if f.own_line {
        // The outermost block that ends last before the id's paragraph.
        return f
            .blocks
            .iter()
            .filter(|(r, _)| r.end <= f.para.start && *r != f.para)
            .max_by_key(|(r, _)| (r.end, std::cmp::Reverse(r.start)))
            .map_or(para, |(r, _)| r.clone());
    }
    // The innermost item that starts on the paragraph's line.
    f.blocks
        .iter()
        .filter(|(r, item)| *item && r.start <= f.para.start && f.para.end <= r.end)
        .filter(|(r, _)| line_of(text, r.start) == line_of(text, f.para.start))
        .min_by_key(|(r, _)| r.len())
        .map_or(para, |(r, _)| r.clone())
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

/// One embedded block.
struct EmbeddedBlock {
    start: usize,
    kind: Block,
}

enum Block {
    Text {
        role: TextRole,
        text: String,
    },
    Embed {
        inner: String,
        anchor: Option<String>,
    },
    /// A ` ```base ` or ` ```query ` fence (live where fences are).
    Fence {
        kind: LiveKind,
        code: CodeBlock,
    },
}

/// Collects the embedded blocks of a range as plain text.
struct Blocks<'t> {
    text: &'t str,
    tree: &'t SyntaxTree,
    out: Vec<EmbeddedBlock>,
    /// Open list items.
    items: usize,
    /// The next leaf is the first of a list item.
    first_in_item: bool,
    /// ` ```base ` and ` ```query ` fences are live blocks.
    fences: bool,
}

impl Visitor for Blocks<'_> {
    fn enter(&mut self, c: &Container) {
        if c.kind == ContainerKind::Item {
            self.items += 1;
            self.first_in_item = true;
        }
    }

    fn exit(&mut self, c: &Container) {
        if c.kind == ContainerKind::Item {
            self.items = self.items.saturating_sub(1);
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        let start = leaf.range.start;
        let first = std::mem::take(&mut self.first_in_item);
        let (role, mut text) = match leaf.kind {
            LeafKind::Heading { level, .. } => (TextRole::heading(level), self.inline_text(leaf)),
            LeafKind::Paragraph | LeafKind::CalloutTitle => {
                let lines: Vec<&str> = leaf
                    .lines
                    .iter()
                    .filter(|l| l.has_content)
                    .map(|l| self.text[l.content.clone()].trim())
                    .collect();
                if let [one] = lines[..]
                    && one.starts_with("![[")
                    && one.ends_with("]]")
                    && !one[3..one.len() - 2].contains("]]")
                {
                    let inner = &one[3..one.len() - 2];
                    let path = inner.split(['#', '|']).next().unwrap_or("");
                    if !is_image_path(path.trim()) {
                        let anchor = inner
                            .split('|')
                            .next()
                            .and_then(|t| t.split_once('#'))
                            .map(|(_, a)| a.to_owned());
                        self.out.push(EmbeddedBlock {
                            start,
                            kind: Block::Embed {
                                inner: inner.to_owned(),
                                anchor,
                            },
                        });
                        return;
                    }
                }
                (TextRole::Body, self.inline_text(leaf))
            }
            LeafKind::FencedCode if self.fences && self.live_fence(leaf).is_some() => {
                if let Some((kind, code)) = self.live_fence(leaf) {
                    self.out.push(EmbeddedBlock {
                        start,
                        kind: Block::Fence { kind, code },
                    });
                }
                return;
            }
            LeafKind::FencedCode
            | LeafKind::IndentedCode
            | LeafKind::MathBlock
            | LeafKind::Html { .. } => {
                let lines: Vec<&str> = leaf
                    .lines
                    .iter()
                    .map(|l| &self.text[l.content.clone()])
                    .collect();
                (TextRole::Body, lines.join("\n"))
            }
            LeafKind::Table => {
                let lines: Vec<&str> = leaf
                    .lines
                    .iter()
                    .filter(|l| l.has_content && !l.marker)
                    .map(|l| self.text[l.content.clone()].trim())
                    .collect();
                (TextRole::Body, lines.join("\n"))
            }
            LeafKind::ThematicBreak
            | LeafKind::LinkRefDef
            | LeafKind::FrontMatter { .. }
            | LeafKind::Comment => return,
        };
        if text.trim().is_empty() {
            return;
        }
        if first && self.items > 0 {
            text = format!("\u{2022} {text}");
        }
        self.out.push(EmbeddedBlock {
            start,
            kind: Block::Text { role, text },
        });
    }
}

impl Blocks<'_> {
    /// A ` ```base ` or ` ```query ` fence's kind and lines (mapped to
    /// the embed's line, like the other embedded blocks).
    fn live_fence(&self, leaf: &Leaf<'_>) -> Option<(LiveKind, CodeBlock)> {
        let info = self.text.get(leaf.info.clone()?)?.trim();
        let kind = LiveKind::of_fence(info.split_ascii_whitespace().next()?)?;
        let mut text = String::new();
        let mut lines = Vec::with_capacity(leaf.lines.len());
        for (i, l) in leaf.lines.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let start = text.len();
            text.push_str(self.text.get(l.content.clone()).unwrap_or(""));
            lines.push(CodeLine {
                units: crate::inline::width_units(&text[start..]),
                text: start..text.len(),
                source: 0..0,
                start: 0,
                indent: 0,
            });
        }
        Some((
            kind,
            CodeBlock {
                fenced: true,
                info: Some(info.into()),
                language: Some(kind.name().into()),
                text,
                lines: lines.into_boxed_slice(),
            },
        ))
    }

    /// The text of a paragraph or heading as a reader sees it.
    fn inline_text(&self, leaf: &Leaf<'_>) -> String {
        let tree = self.tree.inlines(leaf, self.text);
        let mut s = String::new();
        for n in tree.top_level() {
            n.push_text(TextProjection::TextContent, &mut s);
        }
        s.replace('\n', " ")
    }
}

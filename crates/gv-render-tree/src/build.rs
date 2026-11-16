// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! From the block walk to render nodes.
//!
//! [`NodeBuilder`] is a [`Visitor`]: containers become frames of the
//! context stack, leaves become [`BlockNode`]s. In an incremental update it
//! takes over the content of unchanged leaves from the previous tree.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gv_syntax::inline::{
    InlineInput, InlineOptions, RefLookup, parse_inlines_with, parse_table_cell, unescape_string,
};
use gv_syntax::{BlockId, Container, ContainerKind, Leaf, LeafKind, LeafLine, RefMap, Visitor};
use gv_text::TextSource;

use crate::footnote::FootnoteIndex;
use crate::html::{self, DetailsPart};
use crate::inline::{self, InlineCx, width_units};
use crate::node::{
    BlockNode, CodeBlock, CodeLine, Context, Frame, FrameKind, ListMarker, Lookups, NodeKind,
    Picture, TableBlock, TableRow,
};
use crate::status::{ImageSize, LinkTarget, Sources};
use crate::style::TextRole;
use crate::tree::{FrontMatterDisplay, RenderOptions};

/// Containers deeper than this draw nothing more: their content is placed
/// at this depth. Keeps contexts small on pathological nesting.
pub const MAX_FRAMES: usize = 32;

/// The previous tree's nodes that an update may take over.
pub(crate) struct Reuse<'a> {
    /// Old nodes of the rebuilt region by block id and whether they are a
    /// table (a table and the paragraph before it share their text block's
    /// id), with their index.
    pub by_id: HashMap<(BlockId, bool), (usize, &'a BlockNode)>,
    /// The reparsed bytes in the new text.
    pub window: Range<usize>,
    /// New length minus old length.
    pub delta: isize,
    /// Whether the old tree's reference map is the new one.
    pub same_refs: bool,
}

/// The nodes of a footnote definition, for the footnote section.
pub(crate) struct Section {
    /// The normalized label.
    pub label: Arc<str>,
    /// Its number; `None` when nothing refers to it (it is not shown).
    pub number: Option<u32>,
    /// The definition's block (its range is set when it ends).
    pub id: BlockId,
    pub range: Range<usize>,
    pub nodes: Vec<BlockNode>,
}

/// A `<details>` whose blocks are being built: the depth of its container
/// and the frames before its own.
struct OpenDetails {
    depth: usize,
    frames: usize,
}

/// An open container.
struct Open {
    id: BlockId,
    kind: ContainerKind,
    /// Number of nodes when it was entered.
    first_node: usize,
    /// Number of nodes where its nodes go (the document's, or a footnote
    /// definition's) when it was entered.
    first_made: usize,
    /// Lists: items seen so far.
    items: u32,
    /// Lists: number of lists around it.
    list_depth: u16,
    /// Whether it has a frame (not past [`MAX_FRAMES`]).
    framed: bool,
}

pub(crate) struct NodeBuilder<'a, S: TextSource + ?Sized> {
    pub text: &'a S,
    pub refs: Arc<RefMap>,
    pub opts: &'a RenderOptions,
    /// The inline switches of the syntax tree's profile.
    pub inline_opts: InlineOptions,
    pub sources: Sources<'a>,
    pub reuse: Option<Reuse<'a>>,
    frames: Vec<Frame>,
    open: Vec<Open>,
    ctx: Option<Context>,
    pub nodes: Vec<BlockNode>,
    /// Per node: the index of the old node it was taken over from.
    pub origin: Vec<Option<usize>>,
    pub reused: usize,
    pub built: usize,
    scratch: Vec<u8>,
    segments: Vec<Range<usize>>,
    /// The document's footnote numbers.
    pub footnotes: Option<Arc<FootnoteIndex>>,
    /// Whether the blocks of footnote definitions go to [`Self::sections`]
    /// (else to `nodes`, like any block).
    pub divert: bool,
    /// The footnote definitions met, in document order.
    pub sections: Vec<Section>,
    /// Indices into `sections` of the definitions open now.
    diverting: Vec<usize>,
    /// The frames around each open footnote definition, set aside.
    saved_frames: Vec<Vec<Frame>>,
    /// The `<details>` open now.
    details: Vec<OpenDetails>,

    /// The paragraph that gets footnote backreferences: its block and the
    /// number of references.
    pub backrefs: Option<(BlockId, u32)>,
}

impl<'a, S: TextSource + ?Sized> NodeBuilder<'a, S> {
    pub fn new(
        text: &'a S,
        refs: Arc<RefMap>,
        opts: &'a RenderOptions,
        inline_opts: InlineOptions,
        sources: Sources<'a>,
        reuse: Option<Reuse<'a>>,
    ) -> Self {
        Self {
            text,
            refs,
            opts,
            inline_opts,
            sources,
            reuse,
            frames: Vec::new(),
            open: Vec::new(),
            ctx: None,
            nodes: Vec::new(),
            origin: Vec::new(),
            reused: 0,
            built: 0,
            scratch: Vec::new(),
            segments: Vec::new(),
            footnotes: None,
            divert: true,
            sections: Vec::new(),
            diverting: Vec::new(),
            saved_frames: Vec::new(),
            details: Vec::new(),
            backrefs: None,
        }
    }

    fn inline_cx(&self, block: BlockId) -> InlineCx<'_> {
        InlineCx {
            styles: &self.opts.styles,
            metrics: &self.opts.metrics,
            sources: self.sources,
            block,
            footnotes: self.footnotes.as_deref(),
        }
    }

    fn context(&mut self) -> Context {
        if let Some(c) = &self.ctx {
            return Arc::clone(c);
        }
        let c: Context = self.frames.as_slice().into();
        self.ctx = Some(Arc::clone(&c));
        c
    }

    /// Number of nodes where new nodes go now.
    fn made(&self) -> usize {
        match self.diverting.last() {
            Some(&k) if self.divert => self.sections[k].nodes.len(),
            _ => self.nodes.len(),
        }
    }

    fn push(&mut self, node: BlockNode, origin: Option<usize>) {
        if self.divert
            && let Some(&k) = self.diverting.last()
        {
            self.sections[k].nodes.push(node);
            return;
        }
        self.nodes.push(node);
        self.origin.push(origin);
    }

    /// Takes over the old node of `leaf` (and returns its index) if the
    /// leaf lies outside the reparsed window and its reference lookups
    /// resolve as before.
    fn try_reuse(&mut self, leaf: &Leaf<'_>) -> Option<(BlockNode, usize)> {
        let r = self.reuse.as_ref()?;
        let range = leaf.range.clone();
        let shift = if range.end <= r.window.start {
            0
        } else if range.start >= r.window.end {
            r.delta
        } else {
            return None;
        };
        let (index, old) = *r.by_id.get(&(leaf.id, leaf.kind == LeafKind::Table))?;
        let old_range = add(&range, -shift);
        if old.block != old_range {
            return None;
        }
        if !r.same_refs
            && let Some(lookups) = &old.lookups
            && lookups
                .iter()
                .any(|(label, def)| self.refs.lookup(label) != def.as_ref())
        {
            return None;
        }
        if self.ctx.is_none() && *old.context == *self.frames {
            // Same containers as before: share the old context.
            self.ctx = Some(Arc::clone(&old.context));
        }
        Some((
            BlockNode {
                id: old.id,
                kind: old.kind.clone(),
                source: add(&old.source, shift),
                context: self.context(),
                block: range.clone(),
                top: range,
                lookups: old.lookups.clone(),
                table: old.table,
            },
            index,
        ))
    }

    /// Builds the node of a rendered leaf.
    fn build_leaf(&mut self, leaf: &Leaf<'_>) -> Option<BlockNode> {
        let range = leaf.range.clone();
        let first_content = leaf.lines.first().map_or(range.start, |l| l.content.start);
        let (kind, start, lookups) = match leaf.kind {
            LeafKind::LinkRefDef => return None,
            LeafKind::FrontMatter { kind } => {
                if self.opts.front_matter == FrontMatterDisplay::Hidden {
                    return None;
                }
                let lines = Arc::new(self.code_block(leaf, range.start, false, false));
                let props = crate::properties::parse(kind, &lines).into();
                (
                    NodeKind::Properties { kind, lines, props },
                    range.start,
                    None,
                )
            }
            LeafKind::Paragraph => {
                let (content, lookups) = self.inline(leaf, first_content, TextRole::Body);
                (NodeKind::Paragraph(content), first_content, lookups)
            }
            LeafKind::Table => {
                let (table, lookups) = self.table(leaf, first_content);
                (NodeKind::Table(table), first_content, lookups)
            }
            LeafKind::MathBlock => {
                let code = Arc::new(self.code_block(leaf, range.start, false, false));
                (NodeKind::Math(code), range.start, None)
            }
            LeafKind::Heading { level, setext } => {
                let start = if setext { first_content } else { range.start };
                let (content, lookups) = self.inline(leaf, start, TextRole::heading(level));
                (
                    NodeKind::Heading {
                        level,
                        setext,
                        anchor: None,
                        content,
                    },
                    start,
                    lookups,
                )
            }
            LeafKind::ThematicBreak => (NodeKind::ThematicBreak, range.start, None),
            LeafKind::FencedCode | LeafKind::IndentedCode => {
                let fenced = leaf.kind == LeafKind::FencedCode;
                let code = Arc::new(self.code_block(leaf, range.start, fenced, !fenced));
                let lang = code.language.as_deref().filter(|_| fenced);
                let kind = match lang {
                    Some("math") if self.opts.math_fences => NodeKind::Math(code),
                    Some("mermaid") if self.opts.mermaid => NodeKind::Diagram(code),
                    _ => NodeKind::Code(code),
                };
                (kind, range.start, None)
            }
            LeafKind::Html { kind } => {
                let code = Arc::new(self.code_block(leaf, range.start, false, false));
                if let Some(mut p) = html::picture(&code.text) {
                    self.size_picture(&mut p, leaf.id, range.clone());
                    (NodeKind::Picture(Arc::new(p)), range.start, None)
                } else if let Some(DetailsPart::Open { open, summary }) = html::details(&code.text)
                {
                    let summary = self.summary(&code, range.start, summary);
                    (NodeKind::Details { summary, open }, range.start, None)
                } else {
                    (
                        NodeKind::Html {
                            kind,
                            raw_html_pending: true,
                            code,
                        },
                        range.start,
                        None,
                    )
                }
            }
        };
        self.built += 1;
        // A paragraph and a table split from one text block end with their
        // own last line, so their sources do not overlap.
        let end = match leaf.kind {
            LeafKind::Paragraph | LeafKind::Table => leaf
                .lines
                .last()
                .map_or(range.end, |l| l.line_ending.end.max(l.content.end))
                .min(range.end),
            _ => range.end,
        };
        Some(BlockNode {
            id: leaf.id,
            kind,
            source: start..end.max(start),
            context: self.context(),
            block: range.clone(),
            top: range,
            lookups,
            table: leaf.kind == LeafKind::Table,
        })
    }

    /// Asks the image sizes of the sources a `<picture>` shows in a light
    /// and a dark scheme.
    fn size_picture(&self, p: &mut Picture, block: BlockId, range: Range<usize>) {
        for dark in [false, true] {
            p.sizes[usize::from(dark)] = match p.pick(dark) {
                Some(url) => self.sources.images.size(&LinkTarget {
                    kind: gv_model::LinkKind::Inline,
                    image: true,
                    destination: url,
                    title: None,
                    range: range.clone(),
                    block,
                }),
                None => ImageSize::Failed,
            };
        }
    }

    /// Parses the cells of a table and builds its rows (header, then
    /// body; the delimiter row is syntax).
    fn table(&mut self, leaf: &Leaf<'_>, base: usize) -> (Arc<TableBlock>, Option<Lookups>) {
        let delim = leaf.lines.get(1).map_or(0..0, |l| l.content.clone());
        self.scratch.clear();
        self.text.copy_range(delim, &mut self.scratch);
        let aligns = gv_syntax::table::delimiter_row(&self.scratch).unwrap_or_default();
        let columns = aligns.len();
        let footnotes = self.footnotes.clone();
        let cx = InlineCx {
            styles: &self.opts.styles,
            metrics: &self.opts.metrics,
            sources: self.sources,
            block: leaf.id,
            footnotes: footnotes.as_deref(),
        };
        let mut rows = Vec::with_capacity(leaf.lines.len().saturating_sub(1));
        let mut labels: Vec<(Box<str>, Option<gv_syntax::inline::RefDef>)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let n = leaf.lines.len();
        for (i, line) in leaf.lines.iter().enumerate() {
            if i == 1 {
                continue;
            }
            let header = i == 0;
            self.scratch.clear();
            self.text
                .copy_range(line.content.clone(), &mut self.scratch);
            let cells: Vec<_> = gv_syntax::table::row_cells(&self.scratch)
                .unwrap_or_default()
                .into_iter()
                .take(columns)
                .map(|r| {
                    let abs = line.content.start + r.start..line.content.start + r.end;
                    let tree = (!abs.is_empty()).then(|| {
                        parse_table_cell(self.text, abs.clone(), &*self.refs, self.inline_opts)
                    });
                    if let Some(t) = &tree {
                        for l in t.label_lookups() {
                            if seen.insert(l.to_owned()) {
                                labels.push((l.into(), self.refs.lookup(l).cloned()));
                            }
                        }
                    }
                    (abs, tree)
                })
                .collect();
            let role = if header {
                TextRole::TableHeader
            } else {
                TextRole::TableCell
            };
            let content = inline::build_row(&cells, columns, line.content.end, base, role, &cx);
            // Rows tile the table: the header row also covers the
            // delimiter row; each row ends where the next one starts.
            let start = if header { base } else { line_start(line) };
            let end = match leaf.lines.get(if header { 2 } else { i + 1 }) {
                Some(next) if i + 1 < n => line_start(next),
                _ => leaf
                    .lines
                    .last()
                    .map_or(line.content.end, |l| l.line_ending.end.max(l.content.end)),
            };
            rows.push(TableRow {
                header,
                content: Arc::new(content),
                source: start.saturating_sub(base)..end.max(start).saturating_sub(base),
            });
        }
        let lookups = (!labels.is_empty()).then(|| Lookups::from(labels));
        (
            Arc::new(TableBlock {
                aligns: aligns.into_boxed_slice(),
                rows: rows.into_boxed_slice(),
            }),
            lookups,
        )
    }

    /// Parses and builds the inline content of a paragraph or heading.
    fn inline(
        &mut self,
        leaf: &Leaf<'_>,
        base: usize,
        role: TextRole,
    ) -> (Arc<crate::node::InlineContent>, Option<Lookups>) {
        self.segments.clear();
        self.segments.extend(
            leaf.lines
                .iter()
                .filter(|l| l.has_content)
                .map(|l| l.content.clone()),
        );
        let tree = parse_inlines_with(
            self.text,
            InlineInput {
                segments: &self.segments,
            },
            &*self.refs,
            self.inline_opts,
        );
        let cx = self.inline_cx(leaf.id);
        let content = match self.backrefs {
            Some((id, count)) if id == leaf.id && role == TextRole::Body => {
                let end = leaf.lines.last().map_or(base, |l| l.content.end);
                inline::build_with_backrefs(&tree, base, end, count, &cx)
            }
            _ => inline::build(&tree, base, role, &cx),
        };
        let mut seen = std::collections::HashSet::new();
        let lookups: Vec<(Box<str>, Option<gv_syntax::inline::RefDef>)> = tree
            .label_lookups()
            .filter(|l| seen.insert(*l))
            .map(|l| (l.into(), self.refs.lookup(l).cloned()))
            .collect();
        let lookups = (!lookups.is_empty()).then(|| Lookups::from(lookups));
        (Arc::new(content), lookups)
    }

    /// The lines of a code block, HTML block or front matter.
    fn code_block(&mut self, leaf: &Leaf<'_>, base: usize, fenced: bool, trim: bool) -> CodeBlock {
        let mut lines: &[LeafLine] = leaf.lines;
        if trim {
            while let Some(last) = lines.last() {
                self.scratch.clear();
                self.text
                    .copy_range(last.content.clone(), &mut self.scratch);
                if self.scratch.iter().all(|&b| b == b' ' || b == b'\t') {
                    lines = &lines[..lines.len() - 1];
                } else {
                    break;
                }
            }
        }
        let (info, language) = match &leaf.info {
            Some(r) if fenced => {
                self.scratch.clear();
                self.text.copy_range(r.clone(), &mut self.scratch);
                let decoded = unescape_string(&String::from_utf8_lossy(&self.scratch));
                let info = decoded.trim_matches(SPACE).to_owned();
                let word = info.split(SPACE).next().unwrap_or("").to_owned();
                (
                    (!info.is_empty()).then(|| Arc::from(info)),
                    (!word.is_empty()).then(|| Arc::from(word)),
                )
            }
            _ => (None, None),
        };
        let mut text = String::new();
        let mut out = Vec::with_capacity(lines.len());
        let mut prev_end: Option<usize> = None;
        for l in lines {
            if !text.is_empty() || !out.is_empty() {
                text.push('\n');
            }
            let t0 = text.len();
            for _ in 0..l.virtual_spaces {
                text.push(' ');
            }
            self.scratch.clear();
            self.text.copy_range(l.content.clone(), &mut self.scratch);
            let s = String::from_utf8_lossy(&self.scratch);
            if s.contains('\0') {
                text.push_str(&s.replace('\0', "\u{fffd}"));
            } else {
                text.push_str(&s);
            }
            let rel = |o: usize| o.saturating_sub(base);
            let start = prev_end.unwrap_or(l.content.start);
            out.push(CodeLine {
                units: width_units(&text[t0..]),
                text: t0..text.len(),
                source: rel(l.content.start)..rel(l.content.end),
                start: rel(start),
                indent: l.virtual_spaces,
            });
            prev_end = Some(l.line_ending.end.max(l.content.end));
        }
        CodeBlock {
            fenced,
            info,
            language,
            text,
            lines: out.into_boxed_slice(),
        }
    }

    /// The summary row's content: the summary's text where the block's
    /// text has it, else `Details` (as browsers show it).
    fn summary(
        &mut self,
        code: &CodeBlock,
        base: usize,
        summary: Option<(String, Range<usize>)>,
    ) -> Arc<crate::node::InlineContent> {
        let cx = self.inline_cx(BlockId(0));
        let (text, source) = match summary {
            Some((text, r)) => {
                // The text range in the block's text, in the document.
                let to_doc = |t: usize| {
                    let line = code
                        .lines
                        .iter()
                        .rposition(|l| l.text.start <= t)
                        .unwrap_or(0);
                    let l = &code.lines[line];
                    base + (l.source.start + t.saturating_sub(l.text.start)).min(l.source.end)
                };
                (text, to_doc(r.start)..to_doc(r.end))
            }
            None => ("Details".to_owned(), base..base),
        };
        Arc::new(inline::build_plain(
            &text,
            source,
            base,
            TextRole::Body,
            &cx,
        ))
    }

    /// Ends the `<details>` opened at depth `depth` or deeper.
    fn close_details(&mut self, depth: usize) {
        while let Some(d) = self.details.last()
            && d.depth >= depth
        {
            self.frames.truncate(d.frames);
            self.ctx = None;
            self.details.pop();
        }
    }

    /// Sets the top-level block of the nodes from `first` on.
    fn set_top(&mut self, first: usize, top: Range<usize>) {
        for n in &mut self.nodes[first..] {
            n.top = top.clone();
        }
    }
}

const SPACE: [char; 6] = [' ', '\t', '\n', '\r', '\u{b}', '\u{c}'];

/// Where a table row's source starts: its content (container prefixes and
/// indentation before it belong to the row above).
fn line_start(l: &LeafLine) -> usize {
    l.content.start
}

fn add(r: &Range<usize>, d: isize) -> Range<usize> {
    let f = |x: usize| x.saturating_add_signed(d);
    f(r.start)..f(r.end)
}

impl<S: TextSource + ?Sized> Visitor for NodeBuilder<'_, S> {
    fn enter(&mut self, c: &Container) {
        let list_depth = self
            .open
            .iter()
            .filter(|o| {
                matches!(
                    o.kind,
                    ContainerKind::BulletList { .. } | ContainerKind::OrderedList { .. }
                )
            })
            .count();
        let list_depth = u16::try_from(list_depth).unwrap_or(u16::MAX);
        let quote_depth = self.open.iter().filter(|o| o.kind.is_quote()).count();
        let quote_depth = u16::try_from(quote_depth).unwrap_or(u16::MAX);
        if c.kind == ContainerKind::FootnoteDefinition {
            self.enter_footnote(c);
            return;
        }
        let framed = self.frames.len() < MAX_FRAMES;
        let kind = match c.kind {
            ContainerKind::BlockQuote => FrameKind::Quote { depth: quote_depth },
            ContainerKind::Alert { kind } => FrameKind::Alert { kind },
            // Entered by `enter_footnote` above.
            ContainerKind::FootnoteDefinition => return,
            ContainerKind::BulletList { marker, tight } => FrameKind::List {
                ordered: false,
                tight,
                marker,
                start: 0,
                depth: list_depth,
            },
            ContainerKind::OrderedList {
                start,
                delimiter,
                tight,
            } => FrameKind::List {
                ordered: true,
                tight,
                marker: delimiter,
                start,
                depth: list_depth,
            },
            ContainerKind::Item => {
                let marker = match self.open.last_mut() {
                    Some(list) => {
                        let index = list.items;
                        list.items += 1;
                        match list.kind {
                            ContainerKind::OrderedList {
                                start, delimiter, ..
                            } => ListMarker::Ordered {
                                number: u64::from(start) + u64::from(index),
                                delimiter,
                            },
                            _ => ListMarker::Bullet {
                                depth: list.list_depth,
                            },
                        }
                    }
                    None => ListMarker::Bullet { depth: 0 },
                };
                match self.task_status(c) {
                    Some(status) => FrameKind::Item {
                        marker: ListMarker::Task {
                            checked: status != ' ',
                            status,
                        },
                    },
                    None => FrameKind::Item { marker },
                }
            }
        };
        if framed {
            self.frames.push(Frame { id: c.id, kind });
            self.ctx = None;
        }
        self.open.push(Open {
            id: c.id,
            kind: c.kind,
            first_node: self.nodes.len(),
            first_made: self.made(),
            items: 0,
            list_depth,
            framed,
        });
    }

    fn exit(&mut self, c: &Container) {
        let Some(o) = self.open.pop() else {
            return;
        };
        debug_assert_eq!(o.id, c.id);
        // `<details>` opened inside end with it.
        self.close_details(self.open.len() + 1);
        if o.kind == ContainerKind::FootnoteDefinition {
            self.exit_footnote(c, &o);
            return;
        }
        let empty = self.made() == o.first_made;
        if empty && matches!(o.kind, ContainerKind::Alert { .. }) {
            // An alert needs a body (GitHub): it is the block quote it is,
            // holding its marker.
            self.empty_alert(c);
        } else if empty && matches!(o.kind, ContainerKind::Item | ContainerKind::BlockQuote) {
            let kind = if o.kind == ContainerKind::Item {
                NodeKind::EmptyItem
            } else {
                NodeKind::EmptyQuote
            };
            let context = self.context();
            self.push(
                BlockNode {
                    id: c.id,
                    kind,
                    source: c.range.clone(),
                    context,
                    block: c.range.clone(),
                    top: c.range.clone(),
                    lookups: None,
                    table: false,
                },
                None,
            );
        }
        if o.framed {
            self.frames.pop();
            self.ctx = None;
        }
        if self.open.is_empty() {
            self.set_top(o.first_node, c.range.clone());
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if leaf.kind == LeafKind::LinkRefDef {
            return;
        }
        // `<details>`: an HTML block that closes the innermost one here
        // ends its frame (and is not shown when it holds only the tag).
        let depth = self.open.len();
        let mut part = None;
        if let LeafKind::Html { .. } = leaf.kind {
            self.scratch.clear();
            for l in leaf.lines {
                self.text.copy_range(l.content.clone(), &mut self.scratch);
                self.scratch.push(b'\n');
            }
            part = html::details(&String::from_utf8_lossy(&self.scratch));
            if let Some(DetailsPart::Close { only }) = part
                && self.details.last().is_some_and(|d| d.depth == depth)
            {
                self.close_details(depth);
                if only {
                    return;
                }
            }
        }
        let made = match self.try_reuse(leaf) {
            Some((n, index)) => {
                self.reused += 1;
                self.push(n, Some(index));
                true
            }
            None => match self.build_leaf(leaf) {
                Some(n) => {
                    self.push(n, None);
                    true
                }
                None => false,
            },
        };
        if made && let Some(DetailsPart::Open { .. }) = part {
            // The blocks after it, up to its `</details>`, fold with it.
            let frames = self.frames.len();
            if frames < MAX_FRAMES {
                self.frames.push(Frame {
                    id: leaf.id,
                    kind: FrameKind::Details,
                });
                self.ctx = None;
            }
            self.details.push(OpenDetails { depth, frames });
        }
    }
}

impl<S: TextSource + ?Sized> NodeBuilder<'_, S> {
    /// The status character of a task item's checkbox, if the item is one.
    fn task_status(&mut self, c: &Container) -> Option<char> {
        let m = c.marker.clone().filter(|_| c.kind == ContainerKind::Item)?;
        self.scratch.clear();
        self.text.copy_range(m, &mut self.scratch);
        let s = String::from_utf8_lossy(&self.scratch);
        s.strip_prefix('[')?.chars().next()
    }

    /// A footnote definition starts: its blocks go to its section, in a
    /// footnote frame of their own (the containers around the definition
    /// do not show in the footnote section).
    fn enter_footnote(&mut self, c: &Container) {
        self.scratch.clear();
        if let Some(l) = c.footnote_label() {
            self.text.copy_range(l, &mut self.scratch);
        }
        let label = gv_syntax::inline::normalize_label(&String::from_utf8_lossy(&self.scratch));
        let number = self.footnotes.as_ref().and_then(|f| f.number(&label));
        let label: Arc<str> = Arc::from(label);
        self.saved_frames.push(std::mem::take(&mut self.frames));
        self.frames.push(Frame {
            id: c.id,
            kind: FrameKind::Footnote {
                number: number.unwrap_or(0),
                label: Arc::clone(&label),
            },
        });
        self.ctx = None;
        self.sections.push(Section {
            label,
            number,
            id: c.id,
            range: c.range.clone(),
            nodes: Vec::new(),
        });
        self.diverting.push(self.sections.len() - 1);
        self.open.push(Open {
            id: c.id,
            kind: c.kind,
            first_node: self.nodes.len(),
            first_made: self.made(),
            items: 0,
            list_depth: 0,
            framed: true,
        });
    }

    fn exit_footnote(&mut self, c: &Container, o: &Open) {
        if let Some(k) = self.diverting.pop()
            && let Some(s) = self.sections.get_mut(k)
        {
            s.range = c.range.clone();
            for n in &mut s.nodes {
                n.top = c.range.clone();
            }
        }
        self.frames = self.saved_frames.pop().unwrap_or_default();
        self.ctx = None;
        if self.open.is_empty() {
            self.set_top(o.first_node, c.range.clone());
        }
    }

    /// An alert without blocks: a block quote holding its `[!TYPE]`.
    fn empty_alert(&mut self, c: &Container) {
        let Some(marker) = c.marker.clone() else {
            return;
        };
        let saved = self.frames.last().cloned();
        let depth = self.open.iter().filter(|o| o.kind.is_quote()).count();
        if let Some(f) = self.frames.last_mut() {
            f.kind = FrameKind::Quote {
                depth: u16::try_from(depth).unwrap_or(u16::MAX),
            };
            self.ctx = None;
        }
        let lines = [gv_syntax::LeafLine {
            line: c.line,
            content: marker.clone(),
            has_content: true,
            ..gv_syntax::LeafLine::default()
        }];
        let leaf = Leaf {
            id: c.id,
            kind: LeafKind::Paragraph,
            lines: &lines,
            info: None,
            open_line: c.line,
            start_line: c.line,
            end_line: c.line,
            range: c.range.clone(),
        };
        if let Some(mut n) = self.build_leaf(&leaf) {
            n.source = marker.start..c.range.end.max(marker.end);
            self.push(n, None);
        }
        if let (Some(f), Some(saved)) = (self.frames.last_mut(), saved) {
            *f = saved;
            self.ctx = None;
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Virtual items: the render tree flattened into leaf-level pieces for the
//! Reading view's virtual list.
//!
//! - A paragraph, heading, thematic break or empty container is one item;
//!   code blocks, HTML blocks and front matter are split into chunks of at
//!   most [`RenderOptions::chunk_lines`](crate::RenderOptions) lines; a
//!   table is one item per row.
//! - [`ItemId`]s are the block's id plus the chunk index, so they survive
//!   edits elsewhere in the document. Table rows count chunks from 1: a
//!   table shares its block id with the paragraph before it in the same
//!   text block.
//! - Each item carries its containers ([`Item::context`]) and how they
//!   meet its neighbours: [`Item::gap_before`] is the collapsed margin
//!   above its content; the innermost [`Item::opens`] frames start at this
//!   item (their bar starts below the gap, list markers are drawn on the
//!   first line), and the innermost [`Item::closes`] frames end with it.
//! - Height estimates are functions of the width ([`Estimate::height`]),
//!   from character counts and the average advances in
//!   [`Metrics`](crate::Metrics); nothing is shaped.
//!
//! # Painting an item (for the UI)
//!
//! The item's box is `gap_before + content`. Frame `i` of the context is
//! indented by the indents of frames `0..i` ([`Item::frame_x`]). A quote
//! frame draws its bar from the top of the box, or from `gap_before` if it
//! opens here, to the bottom of the box. An item frame that opens here
//! draws its marker on the first line, in the indent.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gv_syntax::BlockId;

use crate::metrics::{Metrics, Spacing};
use crate::node::{BlockNode, CodeBlock, Context, Frame, FrameKind, Measure, NodeKind, Units};
use crate::style::{Family, StyleTable, TextRole};

/// Identity of a virtual item: stable across edits that do not touch its
/// block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId {
    /// The block ([`BlockNode::id`]).
    pub block: BlockId,
    /// The chunk of the block (0 for unsplit blocks).
    pub chunk: u32,
}

/// What a virtual item shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    Paragraph,
    Heading {
        level: u8,
    },
    /// Lines `lines` of a code block, chunk `chunk` of `chunks`.
    Code {
        chunk: u32,
        chunks: u32,
        lines: Range<u32>,
    },
    /// Lines of an HTML block shown as code-like text (raw HTML pending).
    Html {
        chunk: u32,
        chunks: u32,
        lines: Range<u32>,
    },
    /// Lines of front matter (properties placeholder).
    Properties {
        chunk: u32,
        chunks: u32,
        lines: Range<u32>,
    },
    /// A display math block, one item; `lines` are all the block's source
    /// lines (shown as code-like text with the error when the formula does
    /// not render).
    Math {
        lines: Range<u32>,
    },
    /// A Mermaid diagram, one item; `lines` are all the block's source
    /// lines (shown as code-like text below a fallback card when the
    /// diagram does not render).
    Diagram {
        lines: Range<u32>,
    },
    /// Row `row` of a table of `rows` rows (row 0 is the header).
    TableRow {
        row: u32,
        rows: u32,
    },
    ThematicBreak,
    /// A list item without content: its marker.
    EmptyItem,
    /// A block quote without content: its bar.
    EmptyQuote,
    /// The summary row of a `<details>` element.
    Details,
    /// A `<picture>` element.
    Picture,
    /// A live block ([`crate::LiveBlock`]), one item estimated at its
    /// reserved height.
    Live {
        kind: crate::LiveKind,
    },
}

impl ItemKind {
    /// The chunk index and count (0 of 1 for unsplit blocks).
    pub fn chunk(&self) -> (u32, u32) {
        match self {
            Self::Code { chunk, chunks, .. }
            | Self::Html { chunk, chunks, .. }
            | Self::Properties { chunk, chunks, .. } => (*chunk, *chunks),
            Self::TableRow { row, rows } => (*row, *rows),
            _ => (0, 1),
        }
    }

    /// The block's lines this chunk shows.
    pub fn lines(&self) -> Option<Range<u32>> {
        match self {
            Self::Code { lines, .. }
            | Self::Html { lines, .. }
            | Self::Properties { lines, .. }
            | Self::Math { lines }
            | Self::Diagram { lines } => Some(lines.clone()),
            _ => None,
        }
    }

    fn is_first_chunk(&self) -> bool {
        self.chunk().0 == 0
    }

    fn is_last_chunk(&self) -> bool {
        let (k, n) = self.chunk();
        k + 1 >= n
    }

    /// A short name, for dumps.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Paragraph => "paragraph",
            Self::Heading { .. } => "heading",
            Self::Code { .. } => "code",
            Self::Html { .. } => "html",
            Self::Properties { .. } => "properties",
            Self::Math { .. } => "math",
            Self::Diagram { .. } => "diagram",
            Self::TableRow { .. } => "table-row",
            Self::ThematicBreak => "thematic-break",
            Self::EmptyItem => "empty-item",
            Self::EmptyQuote => "empty-quote",
            Self::Details => "details",
            Self::Picture => "picture",
            Self::Live { .. } => "live",
        }
    }
}

/// A height estimate as a function of the width.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    /// Height that does not depend on the width: paddings, borders, the
    /// code head, boxes taller than their line.
    pub fixed: f32,
    /// Height of one line.
    pub line_height: f32,
    /// Horizontal space taken from the width before lines are filled:
    /// container indents and the block's own padding.
    pub inset: f32,
    /// Lines wrap at the width (code blocks scroll unless wrapped).
    pub wrap: bool,
    /// Advance in px of each forced line.
    pub widths: Arc<[f32]>,
}

impl Estimate {
    fn fixed(height: f32) -> Self {
        Self {
            fixed: height,
            line_height: 0.0,
            inset: 0.0,
            wrap: false,
            widths: Arc::from([]),
        }
    }

    /// Number of lines at `width`.
    pub fn lines(&self, width: f32) -> f32 {
        if !self.wrap {
            return self.widths.len() as f32;
        }
        let avail = (width - self.inset).max(1.0);
        self.widths
            .iter()
            .map(|w| (w / avail).ceil().max(1.0))
            .sum()
    }

    /// The estimated content height at `width` (non-increasing in the
    /// width).
    pub fn height(&self, width: f32) -> f32 {
        self.fixed + self.lines(width) * self.line_height
    }
}

/// One virtual item.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ItemId,
    pub kind: ItemKind,
    /// Index of its node in [`RenderTree::nodes`](crate::RenderTree::nodes).
    pub node: usize,
    /// The bytes it renders (absolute). Items are in document order and
    /// their sources do not overlap; the chunks of a block tile its source.
    pub source: Range<usize>,
    /// Its containers, outermost first.
    pub context: Context,
    /// Collapsed margin above the content (px).
    pub gap_before: f32,
    /// The innermost `opens` frames of the context start at this item.
    pub opens: u8,
    /// The innermost `closes` frames of the context end with this item.
    pub closes: u8,
    /// See [`Item::frame_insets`] (set with the gaps).
    pub insets: (f32, f32),
    pub estimate: Estimate,
}

impl Item {
    /// Estimated height of the item's box at `width`, gap included.
    pub fn height_estimate(&self, width: f32) -> f32 {
        self.gap_before + self.insets.0 + self.estimate.height(width) + self.insets.1
    }

    /// The px its frames add inside its box above and below its content: a
    /// callout's padding and title row where it starts, its padding where
    /// it ends.
    pub fn frame_insets(&self) -> (f32, f32) {
        self.insets
    }

    /// The x offset of the content's right edge from the column's (the
    /// callouts' right padding).
    pub fn content_right(&self, metrics: &Metrics) -> f32 {
        self.context.iter().map(|f| right_indent(f, metrics)).sum()
    }

    /// Whether frame `i` of the context starts at this item.
    pub fn opens_frame(&self, i: usize) -> bool {
        i + usize::from(self.opens) >= self.context.len()
    }

    /// Whether frame `i` of the context ends with this item.
    pub fn closes_frame(&self, i: usize) -> bool {
        i + usize::from(self.closes) >= self.context.len()
    }

    /// The x offset of frame `i` (the indents of the frames around it).
    pub fn frame_x(&self, i: usize, metrics: &Metrics) -> f32 {
        self.context[..i.min(self.context.len())]
            .iter()
            .map(|f| indent(f, metrics))
            .sum()
    }

    /// The x offset of the content (all frames' indents).
    pub fn content_x(&self, metrics: &Metrics) -> f32 {
        self.frame_x(self.context.len(), metrics)
    }
}

/// The indent a frame gives its content.
pub fn indent(frame: &Frame, m: &Metrics) -> f32 {
    match frame.kind {
        FrameKind::Quote { .. } => m.quote_indent,
        FrameKind::List { .. } | FrameKind::Details => 0.0,
        FrameKind::Item { .. } | FrameKind::Footnote { .. } => m.list_indent,
        FrameKind::Alert { .. } | FrameKind::Callout { .. } | FrameKind::Embed { .. } => {
            m.callout_padding_x
        }
    }
}

/// The indent a frame gives its content from the right.
pub fn right_indent(frame: &Frame, m: &Metrics) -> f32 {
    match frame.kind {
        FrameKind::Alert { .. } | FrameKind::Callout { .. } | FrameKind::Embed { .. } => {
            m.callout_padding_x
        }
        _ => 0.0,
    }
}

/// What items are made with.
pub(crate) struct Flatten<'a> {
    pub styles: &'a StyleTable,
    pub metrics: &'a Metrics,
    pub chunk_lines: usize,
    /// Per-block soft wrap of code blocks (else [`Metrics::wrap_code`]).
    pub code_wrap: &'a HashMap<BlockId, bool>,
}

impl Flatten<'_> {
    /// Appends the items of node `ix` (gaps and frame bounds are set by
    /// [`link`]).
    pub fn node_items(&self, node: &BlockNode, ix: usize, out: &mut Vec<Item>) {
        let m = self.metrics;
        let inset: f32 = node
            .context
            .iter()
            .map(|f| indent(f, m) + right_indent(f, m))
            .sum();
        let mk = |chunk: u32, kind: ItemKind, source: Range<usize>, estimate: Estimate| Item {
            id: ItemId {
                block: node.id,
                chunk,
            },
            kind,
            node: ix,
            source,
            context: node.context.clone(),
            gap_before: 0.0,
            opens: 0,
            closes: 0,
            insets: (0.0, 0.0),
            estimate,
        };
        match &node.kind {
            NodeKind::Paragraph(c) => {
                let e = self.text_estimate(&c.measure, TextRole::Body, inset);
                out.push(mk(0, ItemKind::Paragraph, node.source.clone(), e));
            }
            NodeKind::Heading { level, content, .. } => {
                let e = self.text_estimate(&content.measure, TextRole::heading(*level), inset);
                let kind = ItemKind::Heading { level: *level };
                out.push(mk(0, kind, node.source.clone(), e));
            }
            NodeKind::Math(code) => {
                // Until the reader lays the formula out: a line of display
                // math (about twice a body line) per source line.
                let lh = self.styles.role(TextRole::Body).line_height;
                let n = code.lines.len().max(1);
                #[allow(clippy::cast_precision_loss)]
                let e = Estimate::fixed(lh * 2.0 * n.min(20) as f32);
                let lines = 0..to_u32(code.lines.len());
                out.push(mk(0, ItemKind::Math { lines }, node.source.clone(), e));
            }
            NodeKind::Diagram(code) => {
                // Until the view has the diagram: its source's height in
                // the code style, with the code block's padding.
                let spec = self.styles.role(TextRole::Code);
                let n = code.lines.len().max(1);
                #[allow(clippy::cast_precision_loss)]
                let e = Estimate::fixed(spec.line_height * n as f32 + 2.0 * m.code_padding_y);
                let lines = 0..to_u32(code.lines.len());
                out.push(mk(0, ItemKind::Diagram { lines }, node.source.clone(), e));
            }
            NodeKind::Table(table) => {
                let rows = to_u32(table.rows.len());
                for (k, row) in table.rows.iter().enumerate() {
                    let k32 = to_u32(k);
                    let role = if row.header {
                        TextRole::TableHeader
                    } else {
                        TextRole::TableCell
                    };
                    let spec = self.styles.role(role);
                    let pad = if row.header {
                        m.table_header_padding_y
                    } else {
                        m.table_padding_y
                    };
                    let columns = table.aligns.len().max(1);
                    #[allow(clippy::cast_precision_loss)]
                    let width = row
                        .content
                        .measure
                        .lines
                        .iter()
                        .map(|u| self.advance(u))
                        .sum::<f32>()
                        + 2.0 * m.table_padding_x * columns as f32;
                    let last = k + 1 == table.rows.len();
                    let e = Estimate {
                        fixed: 2.0 * pad
                            + m.table_border
                            + if last { m.table_border } else { 0.0 }
                            + row.content.measure.extra_height,
                        line_height: spec.line_height,
                        inset,
                        wrap: false,
                        widths: Arc::from([width]),
                    };
                    let kind = ItemKind::TableRow { row: k32, rows };
                    let source =
                        node.to_document(row.source.start)..node.to_document(row.source.end);
                    out.push(mk(k32 + 1, kind, source, e));
                }
            }
            NodeKind::ThematicBreak => {
                out.push(mk(
                    0,
                    ItemKind::ThematicBreak,
                    node.source.clone(),
                    Estimate::fixed(m.rule_height),
                ));
            }
            NodeKind::Details { summary, .. } => {
                let e = self.text_estimate(&summary.measure, TextRole::Body, inset);
                out.push(mk(0, ItemKind::Details, node.source.clone(), e));
            }
            NodeKind::CalloutTitle { title, .. } => {
                let e = self.text_estimate(&title.measure, TextRole::Body, inset);
                out.push(mk(0, ItemKind::Paragraph, node.source.clone(), e));
            }
            NodeKind::Picture(p) => {
                // The light scheme's image, until the view knows the
                // scheme; its alt text's line without one.
                let h = match p.size(false) {
                    crate::ImageSize::Known { width, height }
                        if width > 0.0 && height > 0.0 && width.is_finite() =>
                    {
                        let zoom = self.styles.zoom();
                        let w = width * zoom;
                        let max = m.image_max_width;
                        height * zoom * if w > max && max > 0.0 { max / w } else { 1.0 }
                    }
                    _ => self.styles.role(TextRole::Body).line_height,
                };
                out.push(mk(
                    0,
                    ItemKind::Picture,
                    node.source.clone(),
                    Estimate::fixed(h),
                ));
            }
            NodeKind::Live(live) => {
                let h = live.kind.reserved_height() * self.styles.zoom();
                let kind = ItemKind::Live { kind: live.kind };
                out.push(mk(0, kind, node.source.clone(), Estimate::fixed(h)));
            }
            NodeKind::EmptyItem | NodeKind::EmptyQuote => {
                let kind = if matches!(node.kind, NodeKind::EmptyItem) {
                    ItemKind::EmptyItem
                } else {
                    ItemKind::EmptyQuote
                };
                let lh = self.styles.role(TextRole::Body).line_height;
                out.push(mk(0, kind, node.source.clone(), Estimate::fixed(lh)));
            }
            NodeKind::Code(code)
            | NodeKind::Html { code, .. }
            | NodeKind::Properties { lines: code, .. } => {
                // Code and HTML blocks have the code block's frame and head;
                // front matter is a list of properties.
                let chrome = !matches!(node.kind, NodeKind::Properties { .. });
                let wrap = self.code_wrap.get(&node.id).copied().unwrap_or(m.wrap_code);
                let n = code.lines.len();
                let per = self.chunk_lines.max(1);
                let chunks = n.div_ceil(per).max(1);
                let chunks32 = u32::try_from(chunks).unwrap_or(u32::MAX);
                for k in 0..chunks {
                    let lines = k * per..((k + 1) * per).min(n);
                    let start = if k == 0 {
                        node.source.start
                    } else {
                        node.to_document(code.lines[lines.start].start)
                    };
                    let end = if k + 1 == chunks {
                        node.source.end
                    } else {
                        node.to_document(code.lines[lines.end].start)
                    };
                    let lines32 = to_u32(lines.start)..to_u32(lines.end);
                    let k32 = to_u32(k);
                    let kind = match node.kind {
                        NodeKind::Code(_) => ItemKind::Code {
                            chunk: k32,
                            chunks: chunks32,
                            lines: lines32,
                        },
                        NodeKind::Html { .. } => ItemKind::Html {
                            chunk: k32,
                            chunks: chunks32,
                            lines: lines32,
                        },
                        _ => ItemKind::Properties {
                            chunk: k32,
                            chunks: chunks32,
                            lines: lines32,
                        },
                    };
                    let ends = (k == 0, k + 1 == chunks);
                    let e = self.lines_estimate(code, lines, inset, (chrome, wrap), ends);
                    out.push(mk(k32, kind, start..end.max(start), e));
                }
            }
        }
    }

    fn text_estimate(&self, measure: &Measure, role: TextRole, inset: f32) -> Estimate {
        let spec = self.styles.role(role);
        Estimate {
            fixed: measure.extra_height,
            line_height: spec.line_height,
            inset,
            wrap: true,
            widths: measure.lines.iter().map(|u| self.advance(u)).collect(),
        }
    }

    /// The estimate of `lines` of a code block (`chrome`: with the code
    /// block's frame; `wrap`: a framed block soft-wraps; `ends`: whether
    /// the chunk is the first and the last).
    fn lines_estimate(
        &self,
        code: &CodeBlock,
        lines: Range<usize>,
        inset: f32,
        (chrome, wrap): (bool, bool),
        (first, last): (bool, bool),
    ) -> Estimate {
        let m = self.metrics;
        let role = if chrome {
            TextRole::Code
        } else {
            TextRole::Properties
        };
        let spec = self.styles.role(role);
        let mut fixed = 0.0;
        let mut inset = inset;
        if chrome {
            if first {
                fixed += m.code_border + m.code_head + m.code_padding_y;
            }
            if last {
                fixed += m.code_padding_y + m.code_border;
            }
            inset += 2.0 * (m.code_padding_x + m.code_border);
        }
        let advance = spec.size
            * match spec.family {
                Family::Mono => m.mono_advance_em,
                Family::Text => m.text_advance_em,
            };
        Estimate {
            fixed,
            line_height: spec.line_height,
            inset,
            wrap: !chrome || wrap,
            widths: code.lines[lines]
                .iter()
                .map(|l| l.units as f32 * advance)
                .collect(),
        }
    }

    fn advance(&self, u: &Units) -> f32 {
        let m = self.metrics;
        u.text * m.text_advance_em + u.mono * m.mono_advance_em + u.px
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The margins of an item's own block (top only on its first chunk,
/// bottom only on its last).
fn own_spacing(item: &Item, m: &Metrics) -> Spacing {
    let s = match &item.kind {
        ItemKind::Paragraph if in_tight_item(&item.context) => Spacing::default(),
        ItemKind::Paragraph => m.paragraph,
        ItemKind::Heading { level } => m.heading(*level),
        ItemKind::Code { .. }
        | ItemKind::Html { .. }
        | ItemKind::TableRow { .. }
        | ItemKind::Diagram { .. }
        | ItemKind::Live { .. } => m.code,
        ItemKind::Properties { .. } => m.properties,
        ItemKind::Math { .. } => m.paragraph,
        ItemKind::ThematicBreak => m.rule,
        ItemKind::Details | ItemKind::Picture => m.paragraph,
        ItemKind::EmptyItem | ItemKind::EmptyQuote => Spacing::default(),
    };
    Spacing {
        top: if item.kind.is_first_chunk() {
            s.top
        } else {
            0.0
        },
        bottom: if item.kind.is_last_chunk() {
            s.bottom
        } else {
            0.0
        },
    }
}

/// A paragraph directly in an item of a tight list has no margins.
fn in_tight_item(ctx: &[Frame]) -> bool {
    let n = ctx.len();
    n >= 2
        && matches!(ctx[n - 1].kind, FrameKind::Item { .. })
        && matches!(ctx[n - 2].kind, FrameKind::List { tight: true, .. })
}

/// The margins of frame `i` of `ctx`.
fn frame_spacing(ctx: &[Frame], i: usize, m: &Metrics) -> Spacing {
    match ctx[i].kind {
        FrameKind::Quote { .. } => m.quote,
        FrameKind::List { .. } => {
            if ctx[..i]
                .iter()
                .any(|f| matches!(f.kind, FrameKind::Item { .. }))
            {
                Spacing::default()
            } else {
                m.list
            }
        }
        FrameKind::Item { .. } | FrameKind::Footnote { .. } | FrameKind::Details => {
            Spacing::default()
        }
        FrameKind::Alert { .. } | FrameKind::Callout { .. } | FrameKind::Embed { .. } => m.callout,
    }
}

/// The insets of an item's frames (see [`Item::frame_insets`]).
fn frame_insets(item: &Item, m: &Metrics) -> (f32, f32) {
    let (mut top, mut bottom) = (0.0, 0.0);
    for (i, f) in item.context.iter().enumerate() {
        if let FrameKind::Alert { .. } = f.kind {
            if item.opens_frame(i) {
                top += m.callout_padding_top + m.callout_title + m.callout_title_gap;
            }
            if item.closes_frame(i) {
                bottom += m.callout_padding_bottom;
            }
        }
        // A callout's title is its first item; an embed's line is.
        if let FrameKind::Callout { .. } | FrameKind::Embed { .. } = f.kind {
            if item.opens_frame(i) {
                top += m.callout_padding_top;
            }
            if item.closes_frame(i) {
                bottom += m.callout_padding_bottom;
            }
        }
    }
    (top, bottom)
}

/// Number of leading frames two contexts share.
fn common_prefix(a: &[Frame], b: &[Frame]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x.id == y.id).count()
}

/// Sets `gap_before`, `opens` and `closes` of the items in `range` and of
/// the neighbours that depend on them.
pub(crate) fn link(items: &mut [Item], range: Range<usize>, m: &Metrics) {
    let n = items.len();
    let start = range.start.saturating_sub(1).min(n);
    let end = (range.end + 1).min(n);
    for i in start..end {
        if i == 0 {
            let it = &mut items[0];
            it.gap_before = 0.0;
            it.opens = to_u8(it.context.len());
        } else {
            let (before, after) = items.split_at_mut(i);
            let prev = &mut before[i - 1];
            let cur = &mut after[0];
            let k = common_prefix(&prev.context, &cur.context);
            let mut bottom = own_spacing(prev, m).bottom;
            for f in k..prev.context.len() {
                bottom = bottom.max(frame_spacing(&prev.context, f, m).bottom);
            }
            let mut top = own_spacing(cur, m).top;
            for f in k..cur.context.len() {
                top = top.max(frame_spacing(&cur.context, f, m).top);
            }
            cur.gap_before = bottom.max(top);
            cur.opens = to_u8(cur.context.len() - k);
            prev.closes = to_u8(prev.context.len() - k);
        }
        if i + 1 == n {
            let it = &mut items[i];
            it.closes = to_u8(it.context.len());
        }
    }
    for it in &mut items[start..end] {
        it.insets = frame_insets(it, m);
    }
}

fn to_u8(n: usize) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

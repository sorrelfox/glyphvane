// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The nested block view, derived from the event stream.
//!
//! [`walk`] visits containers (enter and exit) and leaves in document
//! order, with the decisions recorded as markers resolved:
//!
//! - a text block becomes its link reference definitions, then a paragraph
//!   or a setext heading (or nothing, if it held only definitions), or,
//!   with GFM tables, a paragraph and then a table;
//! - a list is tight iff it directly contains no `Loosen` marker (found by
//!   a first pass over the stream, so the flag is known on entering the
//!   list).
//!
//! Both passes iterate the line records once: O(document).
//!
//! [`walk_range`] visits only the blocks that intersect a byte range, in
//! the same order and with the same data as [`walk`] (property-tested). It
//! starts from the checkpoint of the range's first line: the blocks open
//! there and their `Open` events are found through the depth summaries of
//! the line sum tree, list tightness and the ends of blocks that outlast
//! the range likewise. Its cost is the lines of the
//! range and of the leaves that intersect it (a leaf is always visited
//! whole), plus O(log n) summary steps and the events of a few chunks per
//! open block, independent of the document's size.

use std::ops::Range;

use gv_text::TextSource;

use crate::inline::api::{RefDefSpans, parse_ref_def_spans};
use crate::tree::event::{BlockId, BlockKind, Event, FrontMatterKind, TokenKind};
use crate::tree::range::{self, OpenAt};
use crate::tree::{LineView, SyntaxTree, TreeData};

/// A container block as seen by a [`Visitor`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    /// The block's identity.
    pub id: BlockId,
    /// What it is.
    pub kind: ContainerKind,
    /// Byte range; on [`Visitor::enter`] only the start is known (the range
    /// is empty).
    pub range: Range<usize>,
    /// 0-based line of its `Open`.
    pub line: usize,
}

/// Container kinds with their resolved properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerKind {
    /// A block quote.
    BlockQuote,
    /// A bullet list.
    BulletList {
        /// `-`, `+` or `*`.
        marker: u8,
        /// Whether the list is tight.
        tight: bool,
    },
    /// An ordered list.
    OrderedList {
        /// The start number.
        start: u32,
        /// `.` or `)`.
        delimiter: u8,
        /// Whether the list is tight.
        tight: bool,
    },
    /// A list item.
    Item,
}

/// Leaf kinds with their resolved properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafKind {
    /// A paragraph.
    Paragraph,
    /// An ATX or setext heading.
    Heading {
        /// 1 to 6.
        level: u8,
        /// Whether it is a setext heading.
        setext: bool,
    },
    /// A thematic break.
    ThematicBreak,
    /// A fenced code block.
    FencedCode,
    /// An indented code block.
    IndentedCode,
    /// An HTML block.
    Html {
        /// Its start condition (1 to 7).
        kind: u8,
    },
    /// A link reference definition.
    LinkRefDef,
    /// Front matter; its lines are the content between the delimiters.
    FrontMatter {
        /// Its syntax.
        kind: FrontMatterKind,
    },
    /// A GFM table. Its lines are the header row, the delimiter row (whose
    /// `content` is the row without surrounding whitespace, with `marker`
    /// set and `has_content` not) and the body rows; see [`crate::table`]
    /// for splitting them into cells.
    Table,
}

/// One line of a leaf block.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct LeafLine {
    /// 0-based line index.
    pub line: usize,
    /// The content bytes (empty, at the line ending, if there are none).
    pub content: Range<usize>,
    /// Virtual spaces before the content (partially consumed tab).
    pub virtual_spaces: u8,
    /// The line ending bytes (empty at the end of the text).
    pub line_ending: Range<usize>,
    /// Whether the line has a marker token (fence, underline, `#`s).
    pub marker: bool,
    /// Whether the line has a content token.
    pub has_content: bool,
}

/// A leaf block as seen by a [`Visitor`].
#[derive(Clone, Debug)]
pub struct Leaf<'a> {
    /// The identity of the block it comes from (the text block for
    /// paragraphs, headings and definitions split from it).
    pub id: BlockId,
    /// What it is.
    pub kind: LeafKind,
    /// Its content lines: text lines for paragraphs, headings and
    /// definitions; code lines (without fences) for code blocks; all lines
    /// for HTML blocks; the lines between the delimiters for front matter.
    pub lines: &'a [LeafLine],
    /// The info string of a fenced code block.
    pub info: Option<Range<usize>>,
    /// 0-based line of the block's `Open` (for a paragraph after
    /// definitions, the first line of the text block).
    pub open_line: usize,
    /// 0-based first line of this leaf.
    pub start_line: usize,
    /// 0-based last line (the underline of a setext heading).
    pub end_line: usize,
    /// Byte range of the whole block (the text block for parts of one).
    pub range: Range<usize>,
}

impl Leaf<'_> {
    /// The inline content segments: the content range of every line.
    pub fn segments(&self) -> Vec<Range<usize>> {
        self.lines
            .iter()
            .filter(|l| l.has_content)
            .map(|l| l.content.clone())
            .collect()
    }

    /// For a [`LeafKind::LinkRefDef`] leaf, the definition with the
    /// document ranges of its parts (see [`RefDefSpans`]; a part that
    /// spans lines covers the container prefixes between them). `None` for
    /// other leaves. `text` is the text the tree was parsed from.
    pub fn link_ref_def<S: TextSource + ?Sized>(&self, text: &S) -> Option<RefDefSpans> {
        if self.kind != LeafKind::LinkRefDef {
            return None;
        }
        let mut buf = Vec::new();
        // Per line: offset in `buf`, document range of its content.
        let mut starts: Vec<(usize, Range<usize>)> = Vec::with_capacity(self.lines.len());
        for l in self.lines {
            starts.push((buf.len(), l.content.clone()));
            text.copy_range(l.content.clone(), &mut buf);
            buf.push(b'\n');
        }
        let s = std::str::from_utf8(&buf).ok()?;
        let (_, d) = parse_ref_def_spans(s)?;
        let map = |o: usize| {
            let k = starts.partition_point(|(g, _)| *g <= o);
            starts
                .get(k.wrapping_sub(1))
                .map_or(0, |(g, r)| (r.start + (o - g)).min(r.end))
        };
        let map_range = |r: Range<usize>| map(r.start)..map(r.end);
        Some(RefDefSpans {
            range: map_range(d.range),
            label_range: map_range(d.label_range),
            destination_range: map_range(d.destination_range),
            title_range: d.title_range.map(map_range),
            ..d
        })
    }
}

/// Receives the resolved block structure from [`walk`].
pub trait Visitor {
    /// A container starts.
    fn enter(&mut self, container: &Container) {
        let _ = container;
    }
    /// A container ends.
    fn exit(&mut self, container: &Container) {
        let _ = container;
    }
    /// A leaf block, complete.
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        let _ = leaf;
    }
}

/// A [`Visitor`] that only looks at leaves.
#[derive(Debug)]
pub struct LeafFn<F>(pub F);

impl<F: FnMut(&Leaf<'_>)> Visitor for LeafFn<F> {
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        (self.0)(leaf);
    }
}

/// An open container (kept small: deep documents have one per level).
struct CFrame {
    id: BlockId,
    bk: BlockKind,
    start: usize,
    line: usize,
    /// Index of its `Open` among the line's events.
    event: usize,
    depth: u32,
    /// Lists: tightness, once known.
    tight: Option<bool>,
    /// Whether [`Visitor::enter`] was called for it.
    entered: bool,
}

/// The open leaf block (at most one is open at a time: the innermost).
struct Frame {
    id: BlockId,
    bk: BlockKind,
    start: usize,
    line: usize,
    lines: Vec<LeafLine>,
    defs: Vec<u32>,
    setext: Option<(u8, usize)>,
    /// The line of a `TableHeader` marker and the range of its delimiter
    /// row.
    table: Option<(usize, Range<usize>)>,
    info: Option<Range<usize>>,
}

/// Tightness of every list, in the order of the lists' `Open` events.
fn list_tightness(tree: &SyntaxTree) -> Vec<bool> {
    // Per open block: a list (its ordinal), an item, a text block or other.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum F {
        List(u32),
        Item,
        Text,
        Other,
    }
    let mut tight = Vec::new();
    let mut stack: Vec<F> = Vec::new();
    tree.0.for_each_line(|l| {
        for e in l.events {
            if let Some(k) = e.open_kind() {
                stack.push(match k {
                    k if k.is_list() => {
                        tight.push(true);
                        F::List(u32::try_from(tight.len() - 1).unwrap_or(u32::MAX))
                    }
                    BlockKind::Item => F::Item,
                    BlockKind::Text => F::Text,
                    _ => F::Other,
                });
            } else if e.is_close() {
                stack.pop();
            } else if e.loosen_up().is_some() {
                // Emitted with the list, one of its items, or a text block in
                // one of its items on top.
                let n = stack.len();
                let list = match stack.last() {
                    Some(F::List(i)) => Some(*i),
                    Some(F::Item) => match stack.get(n.wrapping_sub(2)) {
                        Some(F::List(i)) => Some(*i),
                        _ => None,
                    },
                    Some(F::Text) => {
                        match (stack.get(n.wrapping_sub(2)), stack.get(n.wrapping_sub(3))) {
                            (Some(F::Item), Some(F::List(i))) => Some(*i),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                if let Some(i) = list
                    && let Some(t) = tight.get_mut(i as usize)
                {
                    *t = false;
                }
            }
        }
    });
    tight
}

/// Where list tightness comes from.
enum Tightness<'t> {
    /// A full walk: precomputed, by list ordinal.
    Precomputed { tight: Vec<bool>, next: usize },
    /// A range walk: queried per list.
    Query(&'t TreeData),
}

/// Replays the event stream into visitor calls.
struct Replay<'t> {
    containers: Vec<CFrame>,
    leaf: Option<Frame>,
    scratch: Vec<LeafLine>,
    /// Reused line buffers of closed leaves.
    spare: Vec<LeafLine>,
    /// Range walks: report only blocks that intersect this (non-empty)
    /// range. Containers that start before it are entered lazily, once
    /// something inside them is reported or they turn out to reach into
    /// the range.
    filter: Option<Range<usize>>,
    tightness: Tightness<'t>,
}

impl Replay<'_> {
    /// Enters the containers not entered yet (outermost first).
    fn enter_pending(&mut self, v: &mut impl Visitor) {
        let first = self.containers.partition_point(|f| f.entered);
        for i in first..self.containers.len() {
            let f = &self.containers[i];
            let tight = if !f.bk.is_list() {
                true
            } else if let Some(t) = f.tight {
                t
            } else if let Tightness::Query(t) = self.tightness {
                range::list_is_tight(
                    t,
                    &OpenAt {
                        depth: f.depth,
                        line: f.line,
                        line_start: 0,
                        event: f.event,
                        id_index: 0,
                        offset: f.start,
                        kind: f.bk,
                        id: f.id,
                    },
                )
            } else {
                true
            };
            let f = &mut self.containers[i];
            f.tight = Some(tight);
            f.entered = true;
            v.enter(&container(f, f.start));
        }
    }

    /// Processes the events of line `l` from event `from` (at byte `off`,
    /// with `id_index` ids already used). In a range walk, returns the
    /// point where the walk stops: an `Open` at or past the end of the
    /// range (no leaf is open then).
    fn line(
        &mut self,
        l: &LineView<'_>,
        from: usize,
        mut off: usize,
        id_index: usize,
        v: &mut impl Visitor,
    ) -> Option<(usize, usize)> {
        let mut ids = l.ids.iter().skip(id_index);
        for (k, e) in l.events.iter().enumerate().skip(from) {
            if let Some(bk) = e.open_kind() {
                if let Some(f) = &self.filter
                    && off >= f.end
                {
                    return Some((k, off));
                }
                let id = ids.next().copied().unwrap_or(BlockId(0));
                if bk.is_container() {
                    let tight = match &mut self.tightness {
                        Tightness::Precomputed { tight, next } if bk.is_list() => {
                            let t = tight.get(*next).copied().unwrap_or(true);
                            *next += 1;
                            Some(t)
                        }
                        _ => None,
                    };
                    let depth = u32::try_from(self.containers.len()).unwrap_or(u32::MAX);
                    self.containers.push(CFrame {
                        id,
                        bk,
                        start: off,
                        line: l.index,
                        event: k,
                        depth,
                        tight,
                        entered: false,
                    });
                    if self.filter.as_ref().is_none_or(|f| off >= f.start) {
                        self.enter_pending(v);
                    }
                } else {
                    let mut lines = std::mem::take(&mut self.spare);
                    lines.clear();
                    self.leaf = Some(Frame {
                        id,
                        bk,
                        start: off,
                        line: l.index,
                        lines,
                        defs: Vec::new(),
                        setext: None,
                        table: None,
                        info: None,
                    });
                }
            } else if e.is_close() {
                if let Some(f) = self.leaf.take() {
                    let report = self
                        .filter
                        .as_ref()
                        .is_none_or(|r| f.start < r.end && off > r.start);
                    if report {
                        self.enter_pending(v);
                        self.spare = emit_leaf(f, off, v, &mut self.scratch);
                    } else {
                        self.spare = f.lines;
                    }
                } else if let Some(top) = self.containers.last() {
                    if !top.entered && self.filter.as_ref().is_some_and(|r| off > r.start) {
                        self.enter_pending(v);
                    }
                    if let Some(f) = self.containers.pop()
                        && f.entered
                    {
                        v.exit(&container(&f, off));
                    }
                }
            } else if let Some(t) = e.token() {
                let r = off..off + t.len as usize;
                off = r.end;
                let Some(f) = self.leaf.as_mut() else {
                    continue;
                };
                if f.lines.last().is_none_or(|x| x.line != l.index) {
                    f.lines.push(LeafLine {
                        line: l.index,
                        content: r.start..r.start,
                        ..LeafLine::default()
                    });
                }
                let Some(ll) = f.lines.last_mut() else {
                    continue;
                };
                match t.kind {
                    TokenKind::Content => {
                        ll.content = r;
                        ll.virtual_spaces = t.virtual_spaces;
                        ll.has_content = true;
                    }
                    TokenKind::LineEnding => {
                        if !ll.has_content {
                            ll.content = r.start..r.start;
                        }
                        ll.line_ending = r;
                    }
                    TokenKind::InfoString => f.info = Some(r),
                    TokenKind::Marker => {
                        ll.marker = true;
                        if let Some((line, delim)) = &mut f.table
                            && *line == l.index
                        {
                            *delim = r;
                        }
                    }
                    _ => {
                        if !ll.has_content {
                            ll.content = r.end..r.end;
                        }
                    }
                }
            } else if let Some(f) = self.leaf.as_mut() {
                match e.to_event(BlockId(0)) {
                    Event::RefDef { lines } => f.defs.push(lines),
                    Event::Setext { level } => f.setext = Some((level, l.index)),
                    Event::TableHeader => f.table = Some((l.index, off..off)),
                    _ => {}
                }
            }
        }
        None
    }
}

/// Visits the blocks of `tree` in document order.
pub fn walk(tree: &SyntaxTree, v: &mut impl Visitor) {
    let mut r = Replay {
        containers: Vec::new(),
        leaf: None,
        scratch: Vec::new(),
        spare: Vec::new(),
        filter: None,
        tightness: Tightness::Precomputed {
            tight: list_tightness(tree),
            next: 0,
        },
    };
    tree.0.for_each_line(|l| {
        r.line(&l, 0, l.start, 0, v);
    });
}

/// Visits the blocks of `tree` that intersect `range`, in document order,
/// exactly as [`walk`] visits them (same data, list tightness and ranges;
/// containers get both `enter` and `exit`).
///
/// A block intersects the range if it shares a byte with it; an empty
/// range at `p` stands for the byte at `p` (at the end of the text: the
/// position after the last byte, which no block covers). Leaves are always
/// visited whole, so the walk reads the lines of the range and of the
/// leaves that intersect it, plus a few chunks of line records per block
/// open at its start or end. See the module documentation.
pub fn walk_range(tree: &SyntaxTree, range: Range<usize>, v: &mut impl Visitor) {
    let t: &TreeData = &tree.0;
    let start = range.start.min(t.len);
    let end = range.end.max(start + 1);
    let l0 = t.cursor_at_byte(start);
    let opens = range::open_blocks_at_line(t, l0.global);
    let mut r = Replay {
        containers: Vec::with_capacity(opens.len()),
        leaf: None,
        scratch: Vec::new(),
        spare: Vec::new(),
        filter: Some(start..end),
        tightness: Tightness::Query(t),
    };
    let mut leaf_open = None;
    for o in &opens {
        if o.kind.is_container() {
            r.containers.push(CFrame {
                id: o.id,
                bk: o.kind,
                start: o.offset,
                line: o.line,
                event: o.event,
                depth: o.depth,
                tight: None,
                entered: false,
            });
        } else {
            // A leaf open at the line start: replay from its `Open`.
            leaf_open = Some(*o);
        }
    }
    let (mut cur, mut from, mut off, mut id_index) = match leaf_open {
        Some(o) => (t.cursor_at_line(o.line), o.event, o.offset, o.id_index),
        None => (l0.clone(), 0, l0.start, 0),
    };
    let mut stop = None;
    while let Some(l) = t.line(&cur) {
        if from == 0 && l.start >= end && r.leaf.is_none() {
            stop = Some((l.index, 0, l.start));
            break;
        }
        if let Some((k, at)) = r.line(&l, from, off, id_index, v) {
            stop = Some((l.index, k, at));
            break;
        }
        if !t.advance(&mut cur) {
            break;
        }
        (from, id_index) = (0, 0);
        off = cur.start;
    }
    if r.containers.is_empty() {
        return;
    }
    // Containers still open: they reach past the range.
    let (line, event, at) = stop.unwrap_or((t.line_count() - 1, usize::MAX, t.len));
    let depth = u32::try_from(r.containers.len()).unwrap_or(u32::MAX);
    let ends = range::closes_from(t, line, event, at, depth);
    r.enter_pending(v);
    while let Some(f) = r.containers.pop() {
        let end = ends.get(r.containers.len()).copied().unwrap_or(t.len);
        v.exit(&container(&f, end));
    }
}

impl SyntaxTree {
    /// [`walk`] over the whole tree.
    pub fn walk(&self, v: &mut impl Visitor) {
        walk(self, v);
    }

    /// [`walk_range`]: the blocks that intersect `range`.
    pub fn walk_range(&self, range: Range<usize>, v: &mut impl Visitor) {
        walk_range(self, range, v);
    }
}

fn container(f: &CFrame, end: usize) -> Container {
    let tight = f.tight.unwrap_or(true);
    let kind = match f.bk {
        BlockKind::BlockQuote => ContainerKind::BlockQuote,
        BlockKind::BulletList { marker } => ContainerKind::BulletList { marker, tight },
        BlockKind::OrderedList { start, delimiter } => ContainerKind::OrderedList {
            start,
            delimiter,
            tight,
        },
        _ => ContainerKind::Item,
    };
    Container {
        id: f.id,
        kind,
        range: f.start..end,
        line: f.line,
    }
}

/// Resolves and visits a closed leaf; returns its line buffer for reuse.
fn emit_leaf(
    f: Frame,
    end: usize,
    v: &mut impl Visitor,
    scratch: &mut Vec<LeafLine>,
) -> Vec<LeafLine> {
    let range = f.start..end;
    let line_of = |lines: &[LeafLine], fallback: usize| lines.first().map_or(fallback, |l| l.line);
    let last_of = |lines: &[LeafLine], fallback: usize| lines.last().map_or(fallback, |l| l.line);
    let mk = |kind, lines: &[LeafLine], info: Option<Range<usize>>| {
        (kind, line_of(lines, f.line), last_of(lines, f.line), info)
    };
    match f.bk {
        BlockKind::Text => {
            scratch.clear();
            scratch.extend(f.lines.iter().filter(|l| l.has_content).cloned());
            let mut i = 0;
            for &d in &f.defs {
                let n = (d as usize).min(scratch.len() - i);
                let lines = &scratch[i..i + n];
                v.leaf(&Leaf {
                    id: f.id,
                    kind: LeafKind::LinkRefDef,
                    lines,
                    info: None,
                    open_line: f.line,
                    start_line: line_of(lines, f.line),
                    end_line: last_of(lines, f.line),
                    range: range.clone(),
                });
                i += n;
            }
            if let Some((dline, delim)) = &f.table {
                // Content lines before the delimiter row: the last is the
                // header, the others (after the definitions) a paragraph.
                let pre = scratch.partition_point(|l| l.line < *dline);
                let header = pre.saturating_sub(1).max(i);
                let para = &scratch[i..header];
                if !para.is_empty() {
                    v.leaf(&Leaf {
                        id: f.id,
                        kind: LeafKind::Paragraph,
                        lines: para,
                        info: None,
                        open_line: f.line,
                        start_line: line_of(para, f.line),
                        end_line: last_of(para, f.line),
                        range: range.clone(),
                    });
                }
                let mut lines: Vec<LeafLine> = scratch[header..pre].to_vec();
                for l in f.lines.iter().filter(|l| l.line >= *dline) {
                    let mut l = l.clone();
                    if l.line == *dline {
                        l.content = delim.clone();
                    }
                    lines.push(l);
                }
                v.leaf(&Leaf {
                    id: f.id,
                    kind: LeafKind::Table,
                    lines: &lines,
                    info: None,
                    open_line: f.line,
                    start_line: line_of(&lines, *dline),
                    end_line: last_of(&lines, *dline),
                    range,
                });
                return f.lines;
            }
            let rest = &scratch[i..];
            if let Some((level, uline)) = f.setext {
                v.leaf(&Leaf {
                    id: f.id,
                    kind: LeafKind::Heading {
                        level,
                        setext: true,
                    },
                    lines: rest,
                    info: None,
                    open_line: f.line,
                    start_line: line_of(rest, uline),
                    end_line: uline,
                    range,
                });
            } else if !rest.is_empty() {
                v.leaf(&Leaf {
                    id: f.id,
                    kind: LeafKind::Paragraph,
                    lines: rest,
                    info: None,
                    open_line: f.line,
                    start_line: line_of(rest, f.line),
                    end_line: last_of(rest, f.line),
                    range,
                });
            }
        }
        _ => {
            // The body of a fenced block: without the opening line, and
            // without the closing line if there is one.
            let body = || {
                let mut body = f.lines.get(1..).unwrap_or(&[]);
                if body.last().is_some_and(|l| l.marker) {
                    body = &body[..body.len() - 1];
                }
                body
            };
            let (kind, lines): (LeafKind, &[LeafLine]) = match f.bk {
                BlockKind::AtxHeading { level } => (
                    LeafKind::Heading {
                        level,
                        setext: false,
                    },
                    &f.lines[..],
                ),
                BlockKind::ThematicBreak => (LeafKind::ThematicBreak, &f.lines[..]),
                BlockKind::FencedCode => (LeafKind::FencedCode, body()),
                BlockKind::FrontMatter { kind } => (LeafKind::FrontMatter { kind }, body()),
                BlockKind::IndentedCode => (LeafKind::IndentedCode, &f.lines[..]),
                BlockKind::Html { kind } => (LeafKind::Html { kind }, &f.lines[..]),
                _ => (LeafKind::Paragraph, &f.lines[..]),
            };
            let (kind, start_line, end_line, info) = mk(kind, lines, f.info.clone());
            let fenced = matches!(kind, LeafKind::FencedCode | LeafKind::FrontMatter { .. });
            v.leaf(&Leaf {
                id: f.id,
                kind,
                lines,
                info,
                open_line: f.line,
                start_line: if fenced { f.line } else { start_line },
                end_line,
                range,
            });
        }
    }
    f.lines
}

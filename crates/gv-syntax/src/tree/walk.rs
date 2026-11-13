// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The nested block view, derived from the event stream.
//!
//! [`walk`] visits containers (enter and exit) and leaves in document
//! order, with the decisions recorded as markers resolved:
//!
//! - a text block becomes its link reference definitions, then a paragraph
//!   or a setext heading (or nothing, if it held only definitions);
//! - a list is tight iff it directly contains no `Loosen` marker (found by
//!   a first pass over the stream, so the flag is known on entering the
//!   list).
//!
//! Both passes iterate the line records once: O(document).

use std::ops::Range;

use crate::tree::SyntaxTree;
use crate::tree::event::{BlockId, BlockKind, TokenKind};

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
    /// for HTML blocks.
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
    tight: bool,
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
            } else if *e == crate::tree::event::Ev::LOOSEN {
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

/// Visits the blocks of `tree` in document order.
pub fn walk(tree: &SyntaxTree, v: &mut impl Visitor) {
    let tight = list_tightness(tree);
    let mut lists = 0usize;
    let mut containers: Vec<CFrame> = Vec::new();
    let mut leaf: Option<Frame> = None;
    let mut scratch: Vec<LeafLine> = Vec::new();
    // Reused line buffers of closed leaves.
    let mut spare: Vec<LeafLine> = Vec::new();
    tree.0.for_each_line(|l| {
        let mut off = l.start;
        let mut ids = l.ids.iter();
        for e in l.events {
            if let Some(bk) = e.open_kind() {
                let id = ids.next().copied().unwrap_or(BlockId(0));
                if bk.is_container() {
                    let mut f = CFrame {
                        id,
                        bk,
                        start: off,
                        line: l.index,
                        tight: true,
                    };
                    if bk.is_list() {
                        f.tight = tight.get(lists).copied().unwrap_or(true);
                        lists += 1;
                    }
                    v.enter(&container(&f, off));
                    containers.push(f);
                } else {
                    let mut lines = std::mem::take(&mut spare);
                    lines.clear();
                    leaf = Some(Frame {
                        id,
                        bk,
                        start: off,
                        line: l.index,
                        lines,
                        defs: Vec::new(),
                        setext: None,
                        info: None,
                    });
                }
            } else if e.is_close() {
                if let Some(f) = leaf.take() {
                    spare = emit_leaf(f, off, v, &mut scratch);
                } else if let Some(f) = containers.pop() {
                    v.exit(&container(&f, off));
                }
            } else if let Some(t) = e.token() {
                let r = off..off + t.len as usize;
                off = r.end;
                let Some(f) = leaf.as_mut() else { continue };
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
                    TokenKind::Marker => ll.marker = true,
                    _ => {
                        if !ll.has_content {
                            ll.content = r.end..r.end;
                        }
                    }
                }
            } else if let Some(f) = leaf.as_mut() {
                match e.to_event(BlockId(0)) {
                    crate::tree::event::Event::RefDef { lines } => f.defs.push(lines),
                    crate::tree::event::Event::Setext { level } => {
                        f.setext = Some((level, l.index))
                    }
                    _ => {}
                }
            }
        }
    });
}

fn container(f: &CFrame, end: usize) -> Container {
    let kind = match f.bk {
        BlockKind::BlockQuote => ContainerKind::BlockQuote,
        BlockKind::BulletList { marker } => ContainerKind::BulletList {
            marker,
            tight: f.tight,
        },
        BlockKind::OrderedList { start, delimiter } => ContainerKind::OrderedList {
            start,
            delimiter,
            tight: f.tight,
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
            let (kind, lines): (LeafKind, &[LeafLine]) = match f.bk {
                BlockKind::AtxHeading { level } => (
                    LeafKind::Heading {
                        level,
                        setext: false,
                    },
                    &f.lines[..],
                ),
                BlockKind::ThematicBreak => (LeafKind::ThematicBreak, &f.lines[..]),
                BlockKind::FencedCode => {
                    let mut body = f.lines.get(1..).unwrap_or(&[]);
                    if body.last().is_some_and(|l| l.marker) {
                        body = &body[..body.len() - 1];
                    }
                    (LeafKind::FencedCode, body)
                }
                BlockKind::IndentedCode => (LeafKind::IndentedCode, &f.lines[..]),
                BlockKind::Html { kind } => (LeafKind::Html { kind }, &f.lines[..]),
                _ => (LeafKind::Paragraph, &f.lines[..]),
            };
            let (kind, start_line, end_line, info) = mk(kind, lines, f.info.clone());
            v.leaf(&Leaf {
                id: f.id,
                kind,
                lines,
                info,
                open_line: f.line,
                start_line: if matches!(kind, LeafKind::FencedCode) {
                    f.line
                } else {
                    start_line
                },
                end_line,
                range,
            });
        }
    }
    f.lines
}

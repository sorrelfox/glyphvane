// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Footnote numbering: a document-global pass over the footnote references
//! in document order (GitHub's and comrak's rule: a footnote's number is
//! the order of its first reference; definitions inside footnotes count
//! where they are written). Obsidian's inline footnotes (`^[…]`) are
//! numbered in the same sequence, each under a label of its own made from
//! where it starts ([`inline_label`]).

use std::collections::HashMap;
use std::ops::Range;

use gv_syntax::inline::{
    InlineInput, InlineKind, InlineOptions, InlineTree, parse_inlines_with, parse_table_cell,
};
use gv_syntax::{BlockId, Leaf, LeafKind, RefMap, SyntaxTree, Visitor};
use gv_text::TextSource;

/// The label of the inline footnote (`^[…]`) that starts at document
/// offset `start`. It cannot clash with a written label: those never
/// hold control characters.
pub fn inline_label(start: usize) -> String {
    format!("\u{1}^[{start}")
}

/// Where an inline footnote is, to build its definition in the footnote
/// section from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InlineNote {
    /// Its label ([`inline_label`]) and number.
    pub label: String,
    pub number: u32,
    /// The block it is written in, and that block's range.
    pub block: BlockId,
    pub block_range: Range<usize>,
    /// The note, `^[` to `]`.
    pub source: Range<usize>,
    /// What to parse again to find it: a paragraph's or heading's
    /// content lines, or a table cell (one range).
    pub segments: Box<[Range<usize>]>,
    pub cell: bool,
}

/// The numbers of a document's footnotes and how often each is
/// referenced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FootnoteIndex {
    /// Normalized label to (number, references).
    numbers: HashMap<String, (u32, u32)>,
    /// The inline footnotes, in document order.
    pub(crate) inline: Vec<InlineNote>,
}

impl FootnoteIndex {
    /// Numbers the footnotes of `tree` (the parse of `text`); `None` when
    /// the document has none (no definition and no inline footnote).
    pub(crate) fn scan<S: TextSource + ?Sized>(
        tree: &SyntaxTree,
        text: &S,
        refs: &RefMap,
    ) -> Option<FootnoteIndex> {
        let opts = tree.options().inline();
        let defined = refs.footnote_count() > 0;
        if !defined && !opts.inline_footnotes {
            return None;
        }
        struct Scan<'a, S: TextSource + ?Sized> {
            text: &'a S,
            refs: &'a RefMap,
            opts: InlineOptions,
            /// Whether the document defines footnotes (else only inline
            /// ones count, and only blocks holding `^[` are parsed).
            defined: bool,
            index: FootnoteIndex,
            next: u32,
            buf: Vec<u8>,
        }
        impl<S: TextSource + ?Sized> Scan<'_, S> {
            fn count(&mut self, label: &str) -> Option<u32> {
                if let Some(e) = self.index.numbers.get_mut(label) {
                    e.1 += 1;
                    None
                } else {
                    self.next += 1;
                    self.index.numbers.insert(label.to_owned(), (self.next, 1));
                    Some(self.next)
                }
            }

            /// Counts the references and inline footnotes of `t`, in
            /// document order.
            fn count_tree(
                &mut self,
                t: &InlineTree,
                leaf: &Leaf<'_>,
                segments: &[Range<usize>],
                cell: bool,
            ) {
                for n in t.nodes() {
                    match n.kind() {
                        InlineKind::FootnoteRef if self.defined => {
                            let label = n.footnote_label().unwrap_or("").to_owned();
                            self.count(&label);
                        }
                        InlineKind::InlineFootnote => {
                            let source = n.range();
                            let label = inline_label(source.start);
                            if let Some(number) = self.count(&label) {
                                self.index.inline.push(InlineNote {
                                    label,
                                    number,
                                    block: leaf.id,
                                    block_range: leaf.range.clone(),
                                    source,
                                    segments: segments.into(),
                                    cell,
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }

            /// Whether `range` may hold an inline footnote, or any
            /// reference when the document defines footnotes.
            fn worth(&mut self, range: Range<usize>) -> bool {
                if self.defined {
                    return true;
                }
                self.buf.clear();
                self.text.copy_range(range, &mut self.buf);
                self.buf.windows(2).any(|w| w == b"^[")
            }
        }
        impl<S: TextSource + ?Sized> Visitor for Scan<'_, S> {
            fn leaf(&mut self, leaf: &Leaf<'_>) {
                match leaf.kind {
                    LeafKind::Paragraph | LeafKind::Heading { .. } => {
                        if !self.worth(leaf.range.clone()) {
                            return;
                        }
                        let segments = leaf.segments();
                        let t = parse_inlines_with(
                            self.text,
                            InlineInput {
                                segments: &segments,
                            },
                            self.refs,
                            self.opts,
                        );
                        self.count_tree(&t, leaf, &segments, false);
                    }
                    LeafKind::Table => {
                        if !self.worth(leaf.range.clone()) {
                            return;
                        }
                        for (i, line) in leaf.lines.iter().enumerate() {
                            if i == 1 {
                                continue;
                            }
                            self.buf.clear();
                            self.text.copy_range(line.content.clone(), &mut self.buf);
                            let cells = gv_syntax::table::row_cells(&self.buf).unwrap_or_default();
                            for r in cells {
                                let abs = line.content.start + r.start..line.content.start + r.end;
                                if abs.is_empty() {
                                    continue;
                                }
                                let t =
                                    parse_table_cell(self.text, abs.clone(), self.refs, self.opts);
                                self.count_tree(&t, leaf, std::slice::from_ref(&abs), true);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut scan = Scan {
            text,
            refs,
            opts,
            defined,
            index: FootnoteIndex::default(),
            next: 0,
            buf: Vec::new(),
        };
        gv_syntax::walk(tree, &mut scan);
        (defined || !scan.index.inline.is_empty()).then_some(scan.index)
    }

    /// The number of the footnote with normalized label `label`, if it is
    /// referenced.
    pub fn number(&self, label: &str) -> Option<u32> {
        self.numbers.get(label).map(|&(n, _)| n)
    }

    /// How often the footnote with normalized label `label` is
    /// referenced.
    pub fn references(&self, label: &str) -> u32 {
        self.numbers.get(label).map_or(0, |&(_, c)| c)
    }

    /// Number of referenced footnotes.
    pub fn len(&self) -> usize {
        self.numbers.len()
    }

    /// Whether no footnote is referenced.
    pub fn is_empty(&self) -> bool {
        self.numbers.is_empty()
    }
}

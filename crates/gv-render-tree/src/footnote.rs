// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Footnote numbering: a document-global pass over the footnote references
//! in document order (GitHub's and comrak's rule: a footnote's number is
//! the order of its first reference; definitions inside footnotes count
//! where they are written).

use std::collections::HashMap;

use gv_syntax::inline::{InlineInput, InlineOptions, parse_inlines_with, parse_table_cell};
use gv_syntax::{Leaf, LeafKind, RefMap, SyntaxTree, Visitor};
use gv_text::TextSource;

/// The numbers of a document's footnotes and how often each is
/// referenced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FootnoteIndex {
    /// Normalized label to (number, references).
    numbers: HashMap<String, (u32, u32)>,
}

impl FootnoteIndex {
    /// Numbers the footnotes of `tree` (the parse of `text`); `None` when
    /// the document defines none.
    pub(crate) fn scan<S: TextSource + ?Sized>(
        tree: &SyntaxTree,
        text: &S,
        refs: &RefMap,
    ) -> Option<FootnoteIndex> {
        if refs.footnote_count() == 0 {
            return None;
        }
        struct Scan<'a, S: TextSource + ?Sized> {
            text: &'a S,
            refs: &'a RefMap,
            opts: InlineOptions,
            index: FootnoteIndex,
            next: u32,
            buf: Vec<u8>,
        }
        impl<S: TextSource + ?Sized> Scan<'_, S> {
            fn count(&mut self, label: &str) {
                if let Some(e) = self.index.numbers.get_mut(label) {
                    e.1 += 1;
                } else {
                    self.next += 1;
                    self.index.numbers.insert(label.to_owned(), (self.next, 1));
                }
            }
        }
        impl<S: TextSource + ?Sized> Visitor for Scan<'_, S> {
            fn leaf(&mut self, leaf: &Leaf<'_>) {
                match leaf.kind {
                    LeafKind::Paragraph | LeafKind::Heading { .. } => {
                        let segments = leaf.segments();
                        let t = parse_inlines_with(
                            self.text,
                            InlineInput {
                                segments: &segments,
                            },
                            self.refs,
                            self.opts,
                        );
                        let labels: Vec<String> =
                            t.footnote_refs().map(|(l, _)| l.to_owned()).collect();
                        for l in labels {
                            self.count(&l);
                        }
                    }
                    LeafKind::Table => {
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
                                let t = parse_table_cell(self.text, abs, self.refs, self.opts);
                                let labels: Vec<String> =
                                    t.footnote_refs().map(|(l, _)| l.to_owned()).collect();
                                for l in labels {
                                    self.count(&l);
                                }
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
            opts: tree.options().inline(),
            index: FootnoteIndex::default(),
            next: 0,
            buf: Vec::new(),
        };
        gv_syntax::walk(tree, &mut scan);
        Some(scan.index)
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

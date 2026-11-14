// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The plain-text projection of the render tree: what a user copies, and
//! what find in page searches, with the way back to source offsets.
//!
//! - [`RenderTree::item_text`]: one item's text and its source mapping
//!   (the find index). Paragraph text is the rendered content: escapes and
//!   entities decoded, soft breaks as spaces, hard breaks as `\n`, inline
//!   code as rendered, alt text of images that are not shown; link status
//!   decorations and image boxes are not text. Code is verbatim.
//! - [`RenderTree::plain_text`]: the text of a source range (a selection)
//!   across items: blocks separated by a newline, or a blank line where the
//!   blocks have a gap between them; list markers optional.
//! - [`RenderTree::find`]: matches in every item's text, with source
//!   ranges.

use std::ops::Range;

use gv_inline::{Affinity, Bias, Paragraph};

use crate::item::{Item, ItemKind};
use crate::node::{BlockNode, CodeBlock, FrameKind, InlineContent, ListMarker};
use crate::tree::RenderTree;

/// Where a run of the text comes from: text offset `text` maps to source
/// offset `source`. Between two segments (or the last one and the end) the
/// text maps to the source byte for byte when both spans have the same
/// length, and as one unit otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Seg {
    text: usize,
    source: usize,
}

/// One item's plain text and its mapping to the document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ItemText {
    pub text: String,
    /// Absolute source range of the item.
    pub source: Range<usize>,
    segs: Vec<Seg>,
}

impl ItemText {
    fn seg_end(&self, k: usize) -> Seg {
        self.segs.get(k + 1).copied().unwrap_or(Seg {
            text: self.text.len(),
            source: self
                .segs
                .get(k)
                .map_or(self.source.end, |s| s.source.max(self.source.end)),
        })
    }

    /// The source offset of text offset `t` (positions inside a unit that
    /// is not byte for byte map to its start).
    pub fn source_offset(&self, t: usize) -> usize {
        let t = t.min(self.text.len());
        let k = self.segs.partition_point(|s| s.text <= t);
        let Some(k) = k.checked_sub(1) else {
            return self.source.start;
        };
        let s = self.segs[k];
        let e = self.seg_end(k);
        if e.text - s.text == e.source.saturating_sub(s.source) {
            s.source + (t - s.text)
        } else if t == s.text {
            s.source
        } else {
            e.source
        }
    }

    /// The text offset of source offset `src`: inside a unit that is not
    /// byte for byte, its start (`Bias::Left`) or end (`Bias::Right`).
    pub fn text_offset(&self, src: usize, bias: Bias) -> usize {
        if src <= self.source.start {
            return 0;
        }
        let k = self.segs.partition_point(|s| s.source <= src);
        let Some(k) = k.checked_sub(1) else {
            return 0;
        };
        // The last segment that starts at or before `src` with text.
        let s = self.segs[k];
        let e = self.seg_end(k);
        if src >= e.source {
            return e.text;
        }
        if e.text - s.text == e.source - s.source {
            s.text + (src - s.source)
        } else if src == s.source {
            s.text
        } else {
            match bias {
                Bias::Left => s.text,
                Bias::Right => e.text,
            }
        }
    }

    /// The source range of the text range `r`.
    pub fn source_range(&self, r: Range<usize>) -> Range<usize> {
        let a = self.source_offset(r.start);
        let b = self.source_offset(r.end).max(a);
        a..b
    }
}

/// Builds item texts.
struct TextBuilder {
    text: String,
    segs: Vec<Seg>,
}

impl TextBuilder {
    fn push(&mut self, s: &str, source: usize) {
        self.segs.push(Seg {
            text: self.text.len(),
            source,
        });
        self.text.push_str(s);
    }

    /// Marks where the source continues after the last piece.
    fn end_at(&mut self, source: usize) {
        self.segs.push(Seg {
            text: self.text.len(),
            source,
        });
    }
}

fn paragraph_text(c: &InlineContent, base: usize, b: &mut TextBuilder) {
    let p: &Paragraph = &c.paragraph;
    for (ix, (item, span)) in p.items().iter().zip(c.spans.iter()).enumerate() {
        if !span.role.is_content() {
            continue;
        }
        let text = if item.is_hard_break() {
            "\n"
        } else {
            p.item_text(ix)
        };
        let src = item.source();
        if text.len() == src.len() || item.is_hard_break() {
            b.push(text, base + src.start);
        } else {
            // Not byte for byte (escapes, entities, soft breaks): one
            // segment per character, through the paragraph's mapping.
            let start = item.range().start;
            for (i, ch) in text.char_indices() {
                let s = p.source_offset(start + i, Affinity::Downstream);
                let mut buf = [0; 4];
                b.push(ch.encode_utf8(&mut buf), base + s);
            }
        }
        b.end_at(base + src.end);
    }
}

fn lines_text(code: &CodeBlock, lines: Range<usize>, node: &BlockNode, b: &mut TextBuilder) {
    for (k, i) in lines.enumerate() {
        let Some(l) = code.lines.get(i) else {
            break;
        };
        let start = node.to_document(l.source.start);
        if k > 0 {
            b.push("\n", node.to_document(l.start));
        }
        let text = &code.text[l.text.clone()];
        let indent = usize::from(l.indent).min(text.len());
        if indent > 0 {
            b.push(&text[..indent], start);
            b.end_at(start);
        }
        // A line with a replaced U+0000 is longer than its source: the
        // segment then maps as one unit.
        b.push(&text[indent..], start);
        b.end_at(node.to_document(l.source.end));
    }
}

/// Options of [`RenderTree::plain_text`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyOptions {
    /// Prefix list items with their marker (`-` for bullets, `3.`), indented
    /// two spaces per enclosing item.
    pub list_markers: bool,
    /// Separate blocks that have a gap between them with a blank line
    /// (else every block starts on the next line).
    pub blank_lines: bool,
}

impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            list_markers: true,
            blank_lines: true,
        }
    }
}

/// A find-in-page match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMatch {
    /// The item.
    pub item: usize,
    /// The match in the item's text.
    pub text: Range<usize>,
    /// The match in the document.
    pub source: Range<usize>,
}

/// Options of [`RenderTree::find`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FindOptions {
    /// Match case exactly (else ASCII letters match either case).
    pub case_sensitive: bool,
}

impl RenderTree {
    /// The plain text of item `ix` with its mapping to the document (the
    /// find index of the item).
    pub fn item_text(&self, ix: usize) -> ItemText {
        let Some(item) = self.items().get(ix) else {
            return ItemText::default();
        };
        let Some(node) = self.nodes().get(item.node) else {
            return ItemText::default();
        };
        let mut b = TextBuilder {
            text: String::new(),
            segs: Vec::new(),
        };
        if let Some(c) = node.inline() {
            paragraph_text(c, node.source.start, &mut b);
        } else if let (Some(code), Some(lines)) = (node.lines(), item.kind.lines()) {
            lines_text(code, lines.start as usize..lines.end as usize, node, &mut b);
        }
        ItemText {
            text: b.text,
            source: item.source.clone(),
            segs: b.segs,
        }
    }

    /// The plain text of the document range `range` (a selection in
    /// source offsets).
    pub fn plain_text(&self, range: Range<usize>, opts: CopyOptions) -> String {
        let items = self.items_in(range.clone());
        let mut out = String::new();
        let mut prev: Option<&Item> = None;
        for ix in items {
            let item = &self.items()[ix];
            if matches!(item.kind, ItemKind::ThematicBreak | ItemKind::EmptyQuote) {
                continue;
            }
            let t = self.item_text(ix);
            let a = if range.start <= item.source.start {
                0
            } else {
                t.text_offset(range.start, Bias::Left)
            };
            let b = if range.end >= item.source.end {
                t.text.len()
            } else {
                t.text_offset(range.end, Bias::Right)
            };
            if a >= b && !t.text.is_empty() {
                continue;
            }
            if let Some(p) = prev {
                let same_block = p.node == item.node;
                if same_block || item.gap_before <= 0.0 || !opts.blank_lines {
                    out.push('\n');
                } else {
                    out.push_str("\n\n");
                }
            }
            if opts.list_markers && a == 0 {
                list_prefix(item, &mut out);
            }
            out.push_str(&t.text[a..b.max(a)]);
            prev = Some(item);
        }
        out
    }

    /// The plain text of items `items`, whole.
    pub fn plain_text_items(&self, items: Range<usize>, opts: CopyOptions) -> String {
        let end = items.end.min(self.items().len());
        let start = items.start.min(end);
        if start == end {
            return String::new();
        }
        let r = self.items()[start].source.start..self.items()[end - 1].source.end;
        self.plain_text(r, opts)
    }

    /// Every match of `needle` in the items' texts, in document order.
    pub fn find(&self, needle: &str, opts: FindOptions) -> Vec<FindMatch> {
        let mut out = Vec::new();
        if needle.is_empty() {
            return out;
        }
        let needle_l = needle.to_ascii_lowercase();
        for ix in 0..self.items().len() {
            let t = self.item_text(ix);
            let hay = if opts.case_sensitive {
                std::borrow::Cow::Borrowed(t.text.as_str())
            } else {
                std::borrow::Cow::Owned(t.text.to_ascii_lowercase())
            };
            let n = if opts.case_sensitive {
                needle
            } else {
                needle_l.as_str()
            };
            let mut from = 0;
            while let Some(k) = hay[from..].find(n) {
                let r = from + k..from + k + n.len();
                out.push(FindMatch {
                    item: ix,
                    source: t.source_range(r.clone()),
                    text: r.clone(),
                });
                from = r.end;
            }
        }
        out
    }
}

/// The list prefix of an item: markers of the list items that start here,
/// indented two spaces per enclosing item; continuation blocks are only
/// indented.
fn list_prefix(item: &Item, out: &mut String) {
    let mut markers = Vec::new();
    let mut enclosing = 0;
    for (i, f) in item.context.iter().enumerate() {
        if let FrameKind::Item { marker } = f.kind {
            if item.opens_frame(i) {
                markers.push(match marker {
                    ListMarker::Bullet { .. } => "-".to_owned(),
                    m => m.text(),
                });
            } else {
                enclosing += 1;
            }
        }
    }
    if markers.is_empty() && enclosing == 0 {
        return;
    }
    for _ in 0..enclosing {
        out.push_str("  ");
    }
    for m in markers {
        out.push_str(&m);
        out.push(' ');
    }
}

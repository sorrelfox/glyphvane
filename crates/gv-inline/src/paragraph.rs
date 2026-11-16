// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The content of one paragraph: items, their logical text, UAX #14 break
//! opportunities and the mapping between logical and source offsets.

use std::ops::Range;

use unicode_linebreak::{BreakOpportunity, linebreaks};
use unicode_segmentation::{GraphemeCursor, GraphemeIndices, UnicodeSegmentation};

use crate::style::{Align, InlineBox, ParagraphStyle, RunStyle};

/// The object replacement character stands for an atomic box in the logical
/// text that line breaking sees.
pub const OBJECT: char = '\u{FFFC}';
/// Length in bytes of a box in the logical text.
pub const OBJECT_LEN: usize = OBJECT.len_utf8();
/// A hard break in the logical text.
pub const HARD_BREAK: char = '\n';

/// Which side a position at a boundary belongs to.
///
/// At a soft wrap the same offset is the end of one line (`Upstream`) and the
/// start of the next (`Downstream`). At an item boundary it is the end of
/// one item or the start of the next, which can differ in x (padding) and in
/// source offset (delimiters between the items).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Affinity {
    /// With the content before the position.
    Upstream,
    /// With the content after the position.
    #[default]
    Downstream,
}

/// Which way to move a position that falls inside an atomic unit (a box, a
/// grapheme cluster, an escape sequence in the source).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Bias {
    /// To the unit's start.
    #[default]
    Left,
    /// To the unit's end.
    Right,
}

/// What an item is.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemKind {
    /// A run of text in one style.
    Text(RunStyle),
    /// An atomic box: U+FFFC in the logical text.
    Box(InlineBox),
    /// A forced line break: `\n` in the logical text.
    HardBreak,
}

/// One piece of inline content with its logical and source ranges.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    kind: ItemKind,
    range: Range<usize>,
    source: Range<usize>,
    /// Text items only: `(text offset, source offset)` pairs relative to the
    /// item, where the mapping between them changes (escapes, entities).
    anchors: Box<[Anchor]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Anchor {
    text: u32,
    source: u32,
}

impl Item {
    pub fn kind(&self) -> &ItemKind {
        &self.kind
    }

    /// Byte range in the paragraph's logical text.
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Byte range in the document source.
    pub fn source(&self) -> Range<usize> {
        self.source.clone()
    }

    pub fn len(&self) -> usize {
        self.range.len()
    }

    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }

    pub fn style(&self) -> Option<&RunStyle> {
        match &self.kind {
            ItemKind::Text(style) => Some(style),
            _ => None,
        }
    }

    pub fn inline_box(&self) -> Option<&InlineBox> {
        match &self.kind {
            ItemKind::Box(b) => Some(b),
            _ => None,
        }
    }

    pub fn is_hard_break(&self) -> bool {
        matches!(self.kind, ItemKind::HardBreak)
    }

    /// Source offset of an item-local logical offset (`0..=len`).
    fn source_at(&self, local: usize) -> usize {
        let base = self.source.start;
        if !matches!(self.kind, ItemKind::Text(_)) {
            return if local == 0 { base } else { self.source.end };
        }
        if local >= self.len() {
            return self.source.end;
        }
        let (t0, s0, t1, s1) = self.segment_by_text(local);
        if t1 - t0 == s1 - s0 {
            base + s0 + (local - t0)
        } else {
            // Inside a replaced unit: its source start.
            base + s0
        }
    }

    /// Item-local logical offset of a source offset in `source`.
    fn local_at(&self, src: usize, bias: Bias) -> usize {
        let len = self.len();
        let rel = src.saturating_sub(self.source.start);
        if !matches!(self.kind, ItemKind::Text(_)) {
            return match (rel, bias) {
                (0, _) | (_, Bias::Left) => 0,
                (_, Bias::Right) => len,
            };
        }
        let (t0, s0, t1, s1) = self.segment_by_source(rel);
        let t = if t1 - t0 == s1 - s0 {
            t0 + (rel - s0).min(t1 - t0)
        } else if rel == s0 {
            t0
        } else {
            match bias {
                Bias::Left => t0,
                Bias::Right => t1,
            }
        };
        t.min(len)
    }

    /// The anchor segment `(t0, s0, t1, s1)` (item-relative) holding text
    /// offset `t`.
    fn segment_by_text(&self, t: usize) -> (usize, usize, usize, usize) {
        let k = self.anchors.partition_point(|a| a.text as usize <= t);
        self.segment(k)
    }

    fn segment_by_source(&self, s: usize) -> (usize, usize, usize, usize) {
        let k = self.anchors.partition_point(|a| a.source as usize <= s);
        self.segment(k)
    }

    /// Segment `k` lies between anchor `k - 1` (or the item start) and
    /// anchor `k` (or the item end).
    fn segment(&self, k: usize) -> (usize, usize, usize, usize) {
        let (t0, s0) = match k.checked_sub(1) {
            Some(i) => (
                self.anchors[i].text as usize,
                self.anchors[i].source as usize,
            ),
            None => (0, 0),
        };
        let (t1, s1) = match self.anchors.get(k) {
            Some(a) => (a.text as usize, a.source as usize),
            None => (self.len(), self.source.len()),
        };
        (t0, s0, t1, s1)
    }
}

/// A column of a paragraph laid out side by side (a table row's cell):
/// the logical text from `start` to the next column's start (or the end)
/// wraps within `width` px, `x` px from the block's left edge, its lines
/// aligned by `align` and stacked from the block's top.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    /// Logical offset where the column's text starts.
    pub start: usize,
    pub x: f32,
    pub width: f32,
    pub align: Align,
}

/// A break opportunity before `offset` in the logical text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Break {
    pub(crate) offset: u32,
    pub(crate) mandatory: bool,
}

/// The inline content of one block, ready for shaping. Immutable; share it
/// with `Arc`.
///
/// # Coordinates
///
/// - **Logical offsets** are byte offsets into [`Paragraph::text`]: the
///   items' text concatenated, with U+FFFC ([`OBJECT`]) for each box and
///   `\n` for each hard break. A box therefore has two caret positions:
///   before it (its start) and after it (start + 3).
/// - **Source offsets** are byte offsets into the document. Every item
///   carries its source range; text items may carry anchors where the text
///   and the source differ (escapes, entities). Delimiters between items
///   (`**`, backticks, link syntax) are gaps that belong to no item.
#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    style: ParagraphStyle,
    text: String,
    items: Vec<Item>,
    breaks: Vec<Break>,
    source_anchor: usize,
    /// Side-by-side columns; empty for ordinary flow.
    columns: Box<[Column]>,
}

impl Paragraph {
    pub fn style(&self) -> &ParagraphStyle {
        &self.style
    }

    /// The logical text.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Replaces the box of item `ix` with `inline_box` (its size, for
    /// example, once the content is known). Returns false, changing
    /// nothing, if item `ix` is not a box.
    pub fn replace_box(&mut self, ix: usize, inline_box: InlineBox) -> bool {
        match self.items.get_mut(ix) {
            Some(Item {
                kind: kind @ ItemKind::Box(_),
                ..
            }) => {
                *kind = ItemKind::Box(inline_box.sanitized());
                true
            }
            _ => false,
        }
    }

    /// The columns the paragraph is laid out in, side by side; empty for
    /// ordinary flow.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Lays the paragraph out in `columns`, side by side (see [`Column`]):
    /// each column wraps within its own width and the layout's width is
    /// then ignored. Starts are sorted and clamped to the text; the first
    /// column always starts at 0. An empty list restores ordinary flow.
    pub fn set_columns(&mut self, columns: impl IntoIterator<Item = Column>) {
        let len = self.len();
        let mut columns: Vec<Column> = columns
            .into_iter()
            .map(|c| Column {
                start: c.start.min(len),
                x: if c.x.is_finite() { c.x } else { 0.0 },
                width: if c.width.is_finite() {
                    c.width.max(0.0)
                } else {
                    0.0
                },
                ..c
            })
            .collect();
        columns.sort_by_key(|c| c.start);
        columns.dedup_by_key(|c| c.start);
        if let Some(first) = columns.first_mut() {
            first.start = 0;
        }
        self.columns = columns.into_boxed_slice();
    }

    /// Logical text of item `ix` (U+FFFC for a box, `\n` for a hard break).
    pub fn item_text(&self, ix: usize) -> &str {
        &self.text[self.items[ix].range.clone()]
    }

    pub(crate) fn breaks(&self) -> &[Break] {
        &self.breaks
    }

    /// Logical offsets where a line may end, ascending; `true` where it must
    /// (a hard break or the end of the text).
    pub fn break_opportunities(&self) -> impl Iterator<Item = (usize, bool)> + '_ {
        self.breaks.iter().map(|b| (b.offset as usize, b.mandatory))
    }

    /// The non-empty item holding logical `offset` (the item that starts at
    /// it when it is a boundary); the last non-empty item for the end.
    pub fn item_at(&self, offset: usize) -> Option<usize> {
        let k = self.items.partition_point(|it| it.range.end <= offset);
        if k < self.items.len() {
            return Some(k);
        }
        self.items.iter().rposition(|it| !it.is_empty())
    }

    /// The source offsets spanned by the items (from the first item's start
    /// to the last one's end).
    pub fn source_span(&self) -> Range<usize> {
        match (self.items.first(), self.items.last()) {
            (Some(first), Some(last)) => first.source.start..last.source.end,
            _ => self.source_anchor..self.source_anchor,
        }
    }

    /// Source offset of a logical offset. At an item boundary `affinity`
    /// picks the end of the item before (`Upstream`) or the start of the
    /// item after (`Downstream`); without a neighbour on that side the other
    /// one is used. Offsets inside a box map to its start. An empty
    /// paragraph maps to its items' source, or to its anchor if it has none.
    pub fn source_offset(&self, offset: usize, affinity: Affinity) -> usize {
        let offset = offset.min(self.len());
        let k = self.items.partition_point(|it| it.range.end <= offset);
        if let Some(it) = self.items.get(k)
            && it.range.start < offset
        {
            return it.source_at(offset - it.range.start);
        }
        let next = self.items.get(k).map(|it| it.source.start);
        let prev = self.items[..k]
            .iter()
            .rev()
            .find(|it| !it.is_empty())
            .map(|it| it.source.end);
        let (first, second) = match affinity {
            Affinity::Upstream => (prev, next),
            Affinity::Downstream => (next, prev),
        };
        // Only empty items (or none): their own source, as `source_span`.
        let fallback = match affinity {
            Affinity::Upstream => self.items.last().map(|it| it.source.end),
            Affinity::Downstream => self.items.first().map(|it| it.source.start),
        };
        first.or(second).or(fallback).unwrap_or(self.source_anchor)
    }

    /// Logical offset (on a char boundary, not yet snapped to a caret stop)
    /// and affinity of a source offset.
    ///
    /// - Inside an item, the item's mapping applies; positions inside an
    ///   atomic unit (a box, an escape) move by `bias`.
    /// - In a gap between items (delimiters), the boundary between them;
    ///   exactly at the end of the item before it is `Upstream`, elsewhere in
    ///   the gap `bias` decides (`Left` → `Upstream`).
    /// - Before the first item, the start; after the last, the end.
    pub fn logical_offset(&self, src: usize, bias: Bias) -> (usize, Affinity) {
        let k = self.items.partition_point(|it| it.source.end <= src);
        let Some(it) = self.items.get(k) else {
            return (self.len(), Affinity::Upstream);
        };
        if src < it.source.start {
            let boundary = it.range.start;
            let at_prev_end = k > 0 && self.items[k - 1].source.end == src;
            return match (k, at_prev_end, bias) {
                (0, _, _) => (boundary, Affinity::Downstream),
                (_, true, _) | (_, false, Bias::Left) => (boundary, Affinity::Upstream),
                (_, false, Bias::Right) => (boundary, Affinity::Downstream),
            };
        }
        if src == it.source.start {
            return (it.range.start, Affinity::Downstream);
        }
        let local = it.local_at(src, bias);
        let text = &self.text[it.range.clone()];
        let local = match bias {
            Bias::Left => floor_char(text, local),
            Bias::Right => ceil_char(text, local),
        };
        let affinity = if local == it.len() {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        (it.range.start + local, affinity)
    }

    /// Approximate heap memory held by the paragraph.
    pub fn heap_bytes(&self) -> usize {
        self.text.capacity()
            + self.items.capacity() * size_of::<Item>()
            + self
                .items
                .iter()
                .map(|it| it.anchors.len() * size_of::<Anchor>())
                .sum::<usize>()
            + self.breaks.capacity() * size_of::<Break>()
            + self.columns.len() * size_of::<Column>()
    }
}

/// Builds a [`Paragraph`] item by item.
///
/// Lengths are sanitized on the way in: non-finite values become 0, and
/// sizes, widths, margins, paddings and thicknesses are clamped to zero or
/// more.
///
/// Items are pushed in source order. Source ranges must not overlap; a range
/// that starts before the previous item's end is moved to that end, so the
/// mapping stays monotonic. Item indices (returned by each push) are stable:
/// hit tests report them, so the caller can keep per-item data (link
/// targets, hover regions) in a parallel vector.
#[derive(Debug, Clone)]
pub struct ParagraphBuilder {
    style: ParagraphStyle,
    text: String,
    items: Vec<Item>,
    source_anchor: usize,
    source_end: usize,
}

impl ParagraphBuilder {
    pub fn new(style: ParagraphStyle) -> Self {
        Self {
            style: style.sanitized(),
            text: String::new(),
            items: Vec::new(),
            source_anchor: 0,
            source_end: 0,
        }
    }

    /// The source offset that an empty paragraph maps every position to.
    pub fn source_anchor(&mut self, offset: usize) -> &mut Self {
        self.source_anchor = offset;
        self
    }

    /// A text run whose text equals its source byte for byte.
    pub fn text(&mut self, text: &str, style: RunStyle, source: Range<usize>) -> usize {
        self.text_mapped(text, style, source, &[])
    }

    /// A text run that differs from its source. `anchors` are
    /// `(offset in text, absolute source offset)` pairs at every point where
    /// the mapping changes: around each escape, entity or other replacement.
    /// Between two anchors (or the run's ends) the text maps to the source
    /// one to one when both spans have the same length, and as one atomic
    /// unit otherwise.
    ///
    /// Example: the text `a*b` from the source `a\*b` at 10 has anchors
    /// `(1, 11)` and `(2, 13)`.
    ///
    /// Anchors that are out of order, out of range or not on a char boundary
    /// are dropped.
    pub fn text_mapped(
        &mut self,
        text: &str,
        style: RunStyle,
        source: Range<usize>,
        anchors: &[(usize, usize)],
    ) -> usize {
        let source = self.clamp_source(source);
        let mut kept: Vec<Anchor> = Vec::with_capacity(anchors.len());
        let (mut last_t, mut last_s) = (0, source.start);
        for &(t, s) in anchors {
            let ok = t <= text.len()
                && text.is_char_boundary(t)
                && (source.start..=source.end).contains(&s)
                && t >= last_t
                && s >= last_s
                && (t, s) != (last_t, last_s);
            if ok {
                kept.push(Anchor {
                    text: to_u32(t),
                    source: to_u32(s - source.start),
                });
                (last_t, last_s) = (t, s);
            }
        }
        // An anchor at the very end restates the implicit end.
        if kept
            .last()
            .is_some_and(|a| a.text as usize == text.len() && a.source as usize == source.len())
        {
            kept.pop();
        }
        self.push(
            ItemKind::Text(style.sanitized()),
            text,
            source,
            kept.into_boxed_slice(),
        )
    }

    /// An atomic box.
    pub fn inline_box(&mut self, inline_box: InlineBox, source: Range<usize>) -> usize {
        let source = self.clamp_source(source);
        let mut buf = [0; 4];
        let text: &str = OBJECT.encode_utf8(&mut buf);
        self.push(
            ItemKind::Box(inline_box.sanitized()),
            text,
            source,
            Box::default(),
        )
    }

    /// A forced line break.
    pub fn hard_break(&mut self, source: Range<usize>) -> usize {
        let source = self.clamp_source(source);
        self.push(ItemKind::HardBreak, "\n", source, Box::default())
    }

    pub fn build(self) -> Paragraph {
        let breaks = compute_breaks(&self.text);
        Paragraph {
            style: self.style,
            text: self.text,
            items: self.items,
            breaks,
            source_anchor: self.source_anchor,
            columns: Box::default(),
        }
    }

    fn clamp_source(&mut self, source: Range<usize>) -> Range<usize> {
        let start = source.start.max(self.source_end);
        let end = source.end.max(start);
        self.source_end = end;
        start..end
    }

    fn push(
        &mut self,
        kind: ItemKind,
        text: &str,
        source: Range<usize>,
        anchors: Box<[Anchor]>,
    ) -> usize {
        let start = self.text.len();
        self.text.push_str(text);
        self.items.push(Item {
            kind,
            range: start..self.text.len(),
            source,
            anchors,
        });
        self.items.len() - 1
    }
}

/// UAX #14 opportunities of the logical text. Allowed breaks that are not
/// grapheme boundaries are dropped, so a line never ends inside a cluster.
fn compute_breaks(text: &str) -> Vec<Break> {
    let bytes = text.as_bytes();
    linebreaks(text)
        .filter(|&(pos, op)| {
            pos > 0
                && (op == BreakOpportunity::Mandatory
                    // Between two ASCII characters there is always a grapheme
                    // boundary, except inside CR LF (where UAX #14 does not
                    // break either).
                    || (bytes[pos - 1].is_ascii() && bytes.get(pos).is_none_or(u8::is_ascii))
                    || GraphemeCursor::new(pos, text.len(), true)
                        .is_boundary(text, 0)
                        .unwrap_or(true))
        })
        .map(|(pos, op)| Break {
            offset: to_u32(pos),
            mandatory: op == BreakOpportunity::Mandatory,
        })
        .collect()
}

/// Start offsets of the extended grapheme clusters of `text`, with a fast
/// path for ASCII text (every byte is a cluster unless CR LF).
pub(crate) fn grapheme_starts(text: &str) -> GraphemeStarts<'_> {
    if text.is_ascii() && !text.as_bytes().contains(&b'\r') {
        GraphemeStarts::Ascii(0..text.len())
    } else {
        GraphemeStarts::Unicode(text.grapheme_indices(true))
    }
}

pub(crate) enum GraphemeStarts<'a> {
    Ascii(Range<usize>),
    Unicode(GraphemeIndices<'a>),
}

impl Iterator for GraphemeStarts<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        match self {
            Self::Ascii(r) => r.next(),
            Self::Unicode(g) => g.next().map(|(i, _)| i),
        }
    }
}

pub(crate) fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

pub(crate) fn floor_char(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(crate) fn ceil_char(text: &str, i: usize) -> usize {
    let mut i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Characters that end a line by themselves (UAX #14 classes BK, CR, LF,
/// NL).
pub(crate) fn is_break_char(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// White space that hangs at a line end: not counted when fitting or
/// aligning the line.
pub(crate) fn is_hanging(c: char) -> bool {
    c == ' ' || c == '\t' || is_break_char(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{BoxKind, LineHeight, StyleId};

    fn body() -> RunStyle {
        RunStyle::new(StyleId(0), 10.0)
    }

    fn builder() -> ParagraphBuilder {
        ParagraphBuilder::new(ParagraphStyle::new(StyleId(0), 10.0, LineHeight::Px(16.0)))
    }

    #[test]
    fn logical_text_has_object_and_newline() {
        let mut b = builder();
        b.text("ab", body(), 0..2);
        b.inline_box(InlineBox::new(BoxKind::Math, 5.0, 5.0, 1.0), 2..7);
        b.hard_break(7..8);
        b.text("c", body(), 8..9);
        let p = b.build();
        assert_eq!(p.text(), "ab\u{FFFC}\nc");
        assert_eq!(p.items()[1].range(), 2..5);
        assert_eq!(p.item_at(5), Some(2));
        assert_eq!(p.item_at(p.len()), Some(3));
        let breaks: Vec<_> = p.break_opportunities().collect();
        assert!(breaks.contains(&(6, true)), "{breaks:?}");
    }

    #[test]
    fn boxes_are_replaced_in_place() {
        let mut b = builder();
        b.text("ab", body(), 0..2);
        b.inline_box(InlineBox::new(BoxKind::Math, 5.0, 5.0, 1.0), 2..7);
        let mut p = b.build();
        assert!(!p.replace_box(0, InlineBox::new(BoxKind::Math, 1.0, 1.0, 0.0)));
        assert!(p.replace_box(1, InlineBox::new(BoxKind::Math, 9.0, 4.0, 2.0)));
        let replaced = p.items()[1].inline_box().copied().expect("a box");
        assert_eq!(
            (replaced.width, replaced.ascent, replaced.descent),
            (9.0, 4.0, 2.0)
        );
        assert!(!p.replace_box(5, InlineBox::new(BoxKind::Math, 1.0, 1.0, 0.0)));
    }

    #[test]
    fn escape_anchors_map_both_ways() {
        // Source `x a\*b y` at 100: the run `a*b` comes from `a\*b` (102..106).
        let mut b = builder();
        b.text_mapped("a*b", body(), 102..106, &[(1, 103), (2, 105)]);
        let p = b.build();
        let to: Vec<_> = (0..=3)
            .map(|o| p.source_offset(o, Affinity::Downstream))
            .collect();
        assert_eq!(to, [102, 103, 105, 106]);
        assert_eq!(p.logical_offset(103, Bias::Left), (1, Affinity::Downstream));
        // Between `\` and `*`: inside the escape.
        assert_eq!(p.logical_offset(104, Bias::Left), (1, Affinity::Downstream));
        assert_eq!(
            p.logical_offset(104, Bias::Right),
            (2, Affinity::Downstream)
        );
        assert_eq!(p.logical_offset(106, Bias::Left), (3, Affinity::Upstream));
        assert_eq!(p.logical_offset(50, Bias::Left), (0, Affinity::Downstream));
        assert_eq!(p.logical_offset(500, Bias::Left), (3, Affinity::Upstream));
    }

    #[test]
    fn delimiter_gaps_pick_sides_by_affinity_and_bias() {
        // `a **b** c`: "a " 0..2, "b" 4..5, " c" 7..9.
        let mut b = builder();
        b.text("a ", body(), 0..2);
        b.text("b", body(), 4..5);
        b.text(" c", body(), 7..9);
        let p = b.build();
        assert_eq!(p.source_offset(2, Affinity::Upstream), 2);
        assert_eq!(p.source_offset(2, Affinity::Downstream), 4);
        assert_eq!(p.source_offset(3, Affinity::Upstream), 5);
        assert_eq!(p.source_offset(3, Affinity::Downstream), 7);
        assert_eq!(p.logical_offset(2, Bias::Right), (2, Affinity::Upstream));
        assert_eq!(p.logical_offset(3, Bias::Left), (2, Affinity::Upstream));
        assert_eq!(p.logical_offset(3, Bias::Right), (2, Affinity::Downstream));
        assert_eq!(p.logical_offset(4, Bias::Left), (2, Affinity::Downstream));
        assert_eq!(p.logical_offset(6, Bias::Left), (3, Affinity::Upstream));
    }

    #[test]
    fn boxes_map_to_their_ends() {
        let mut b = builder();
        b.text("x", body(), 0..1);
        b.inline_box(InlineBox::new(BoxKind::Math, 5.0, 5.0, 1.0), 1..6);
        let p = b.build();
        assert_eq!(p.source_offset(1, Affinity::Downstream), 1);
        assert_eq!(p.source_offset(4, Affinity::Upstream), 6);
        assert_eq!(p.logical_offset(3, Bias::Left), (1, Affinity::Downstream));
        assert_eq!(p.logical_offset(3, Bias::Right), (4, Affinity::Upstream));
    }

    #[test]
    fn overlapping_sources_are_clamped_and_bad_anchors_dropped() {
        let mut b = builder();
        b.text("ab", body(), 10..12);
        b.text_mapped("cd", body(), 11..14, &[(1, 9), (3, 13), (1, 13)]);
        let p = b.build();
        assert_eq!(p.items()[1].source(), 12..14);
        assert_eq!(p.items()[1].anchors.len(), 1);
        assert_eq!(p.source_span(), 10..14);
    }

    #[test]
    fn empty_paragraph_maps_to_its_anchor() {
        let mut b = builder();
        b.source_anchor(42);
        let p = b.build();
        assert_eq!(p.source_offset(0, Affinity::Upstream), 42);
        assert_eq!(p.source_span(), 42..42);
        assert_eq!(p.item_at(0), None);
    }
}

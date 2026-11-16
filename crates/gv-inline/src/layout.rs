// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Line boxes and fragments of a paragraph at one width.

use std::ops::Range;
use std::sync::Arc;

use crate::breaking::{LineBreak, break_lines, break_lines_in};
use crate::paragraph::{ItemKind, OBJECT_LEN, Paragraph, is_break_char};
use crate::shaped::Shaped;
use crate::style::Align;

/// An axis-aligned rectangle in the block's coordinates (origin at the
/// block's top left, y down).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }
}

/// One item's piece on one line.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    pub item: usize,
    /// Byte range within the item's logical text (a box is `0..3`).
    pub range: Range<usize>,
    /// Left edge of the content (after padding or margin), alignment
    /// applied.
    pub x: f32,
    pub width: f32,
    /// Background padding or box margin taken by this piece.
    pub pad_left: f32,
    pub pad_right: f32,
    /// Extent above and below the line's baseline, baseline shift included.
    pub ascent: f32,
    pub descent: f32,
    /// Baseline shift of a text piece: its glyphs sit `shift` above the
    /// line's baseline.
    pub shift: f32,
}

impl Fragment {
    /// Left edge including padding or margin.
    pub fn outer_left(&self) -> f32 {
        self.x - self.pad_left
    }

    /// Right edge including padding or margin.
    pub fn outer_right(&self) -> f32 {
        self.x + self.width + self.pad_right
    }
}

/// A line box.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Logical range, trailing white space and the hard break included.
    /// Fragments stop before a forced break (a `\n` is never drawn).
    pub range: Range<usize>,
    /// Indices into [`Layout::fragments`].
    pub frags: Range<usize>,
    /// Left edge of the content after alignment.
    pub x: f32,
    /// Content width without hanging white space.
    pub width: f32,
    pub top: f32,
    pub height: f32,
    /// Baseline y, from the block's top.
    pub baseline: f32,
    /// The line ends with a forced break (hard break or newline).
    pub hard_break: bool,
}

impl Line {
    pub fn bottom(&self) -> f32 {
        self.top + self.height
    }
}

/// A paragraph laid out at one width. Immutable; queries take `&self`.
///
/// A paragraph with [`Paragraph::columns`] is laid out column by column:
/// the lines stay in logical order (each column's lines top to bottom,
/// then the next column's), so lines overlap in y across columns.
#[derive(Debug)]
pub struct Layout<G = ()> {
    pub(crate) shaped: Arc<Shaped<G>>,
    pub(crate) max_width: f32,
    pub(crate) lines: Vec<Line>,
    pub(crate) frags: Vec<Fragment>,
    pub(crate) width: f32,
    pub(crate) height: f32,
    /// Side-by-side columns: each one's x extent and lines; empty for
    /// ordinary flow.
    pub(crate) columns: Vec<ColumnLines>,
}

/// The lines of one column of a columnar layout.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ColumnLines {
    pub(crate) x: f32,
    pub(crate) width: f32,
    pub(crate) lines: Range<usize>,
}

impl<G> Layout<G> {
    /// Breaks the shaped paragraph into lines no wider than `max_width`
    /// where possible, places fragments on a shared baseline per line and
    /// aligns the lines. `f32::INFINITY` gives the max-content layout.
    /// Never shapes. A paragraph with columns is laid out in them instead
    /// (`max_width` is only recorded).
    pub fn new(shaped: Arc<Shaped<G>>, max_width: f32) -> Self {
        let max = sanitize_width(max_width);
        let mut layout = Self {
            shaped,
            max_width: max,
            lines: Vec::new(),
            frags: Vec::new(),
            width: 0.0,
            height: 0.0,
            columns: Vec::new(),
        };
        let shaped = layout.shaped.clone();
        let paragraph = shaped.paragraph();
        let columns = paragraph.columns();
        if columns.is_empty() {
            let align = paragraph.style().align;
            let breaks = break_lines(&shaped, max);
            let (h, w, _) = layout.place_lines(breaks, 0.0, max, align);
            layout.height = h;
            layout.width = w;
            return layout;
        }
        let len = paragraph.text().len();
        for (k, c) in columns.iter().enumerate() {
            let end = columns.get(k + 1).map_or(len, |n| n.start);
            let first = layout.lines.len();
            let breaks = break_lines_in(&shaped, c.start..end, c.width);
            let (h, _, right) = layout.place_lines(breaks, c.x, c.width, c.align);
            layout.height = layout.height.max(h);
            layout.width = layout.width.max(right);
            layout.columns.push(ColumnLines {
                x: c.x,
                width: c.width,
                lines: first..layout.lines.len(),
            });
        }
        layout
    }

    /// Places the lines `breaks` from y 0 at `x0`, aligned by `align`
    /// within `max`. Returns their height, the widest line's content width
    /// and the rightmost content edge.
    fn place_lines(
        &mut self,
        breaks: Vec<LineBreak>,
        x0: f32,
        max: f32,
        align: Align,
    ) -> (f32, f32, f32) {
        let shaped = self.shaped.clone();
        let strut = *shaped.strut();
        let items = shaped.paragraph().items();
        let (mut y, mut widest, mut right) = (0.0f32, 0.0f32, 0.0f32);
        for lb in breaks {
            let first = self.frags.len();
            let mut x = 0.0;
            let (mut ascent, mut descent) = (strut.ascent, strut.descent);
            // A forced break is not drawn: fragments stop before it.
            let end = if lb.hard {
                let text = &shaped.paragraph().text()[lb.range.clone()];
                lb.range.start + text.trim_end_matches(is_break_char).len()
            } else {
                lb.range.end
            };
            for (ix, lo, hi) in shaped.pieces(lb.range.start, end) {
                let it = &items[ix];
                let (pad_l, pad_r) = shaped.padding(ix);
                let (a, d) = shaped.extent(ix);
                let frag = match it.kind() {
                    ItemKind::Text(_) => {
                        let pad_left = if lo == 0 { pad_l } else { 0.0 };
                        let pad_right = if hi == it.len() { pad_r } else { 0.0 };
                        let width = shaped.x_at(ix, hi) - shaped.x_at(ix, lo);
                        let frag = Fragment {
                            item: ix,
                            range: lo..hi,
                            x: x + pad_left,
                            width,
                            pad_left,
                            pad_right,
                            ascent: a,
                            descent: d,
                            shift: shaped.shift(ix),
                        };
                        x += pad_left + width + pad_right;
                        frag
                    }
                    ItemKind::Box(b) => {
                        let frag = Fragment {
                            item: ix,
                            range: 0..OBJECT_LEN,
                            x: x + b.margin_left,
                            width: b.width,
                            pad_left: b.margin_left,
                            pad_right: b.margin_right,
                            ascent: a,
                            descent: d,
                            shift: 0.0,
                        };
                        x += b.advance();
                        frag
                    }
                    ItemKind::HardBreak => continue,
                };
                ascent = ascent.max(frag.ascent);
                descent = descent.max(frag.descent);
                self.frags.push(frag);
            }
            let trimmed = shaped.trim_hanging(lb.range.start, lb.range.end);
            let content = shaped.width(lb.range.start, trimmed);
            let offset = x0 + align_offset(align, max, content);
            if offset != 0.0 {
                for f in &mut self.frags[first..] {
                    f.x += offset;
                }
            }
            widest = widest.max(content);
            right = right.max(offset + content);
            let height = ascent + descent;
            self.lines.push(Line {
                range: lb.range,
                frags: first..self.frags.len(),
                x: offset,
                width: content,
                top: y,
                height,
                baseline: y + ascent,
                hard_break: lb.hard,
            });
            y += height;
        }
        (y, widest, right)
    }

    /// Whether the paragraph is laid out in side-by-side columns.
    pub fn is_columnar(&self) -> bool {
        !self.columns.is_empty()
    }

    /// The lines of the column nearest to x `x` (all lines without
    /// columns).
    pub(crate) fn column_lines_at(&self, x: f32) -> Range<usize> {
        let distance = |c: &ColumnLines| {
            if x < c.x {
                c.x - x
            } else {
                (x - (c.x + c.width)).max(0.0)
            }
        };
        self.columns
            .iter()
            .filter(|c| !c.lines.is_empty())
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .map_or(0..self.lines.len(), |c| c.lines.clone())
    }

    pub fn shaped(&self) -> &Arc<Shaped<G>> {
        &self.shaped
    }

    pub fn paragraph(&self) -> &Arc<Paragraph> {
        self.shaped.paragraph()
    }

    /// The width the layout was made for.
    pub fn max_width(&self) -> f32 {
        self.max_width
    }

    /// Width of the widest line (hanging white space excluded).
    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    /// The lines, top to bottom. There is always at least one.
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// All fragments, line by line, left to right.
    pub fn fragments(&self) -> &[Fragment] {
        &self.frags
    }

    /// The fragments of line `line`.
    pub fn line_fragments(&self, line: usize) -> &[Fragment] {
        &self.frags[self.lines[line].frags.clone()]
    }

    /// The x where the shaped run of a text fragment starts: a glyph of the
    /// run at x `gx` (from the text system) whose cluster starts at
    /// item-local byte `b` is painted at
    /// `run_origin_x(f) + gx + shaped().glyph_shift(f.item, b)`, on the
    /// baseline `line.baseline - f.shift`. Only glyphs with `b` in
    /// `f.range` belong to the fragment.
    pub fn run_origin_x(&self, frag: &Fragment) -> f32 {
        frag.x - self.shaped.x_at(frag.item, frag.range.start)
    }

    /// Lines that overlap `y0..y1`, for culling (every line of a columnar
    /// layout).
    pub fn lines_between(&self, y0: f32, y1: f32) -> Range<usize> {
        if self.is_columnar() {
            return 0..self.lines.len();
        }
        let first = self.lines.partition_point(|l| l.bottom() <= y0);
        let last = self.lines.partition_point(|l| l.top < y1);
        first..last.max(first)
    }

    /// Approximate heap memory held by the layout itself.
    pub fn heap_bytes(&self) -> usize {
        self.lines.capacity() * size_of::<Line>()
            + self.frags.capacity() * size_of::<Fragment>()
            + self.columns.capacity() * size_of::<ColumnLines>()
    }
}

/// NaN and negative widths become 0.
pub(crate) fn sanitize_width(width: f32) -> f32 {
    if width.is_nan() { 0.0 } else { width.max(0.0) }
}

fn align_offset(align: Align, max: f32, content: f32) -> f32 {
    if !max.is_finite() {
        return 0.0;
    }
    match align {
        Align::Left => 0.0,
        Align::Center => ((max - content) / 2.0).max(0.0),
        Align::Right => (max - content).max(0.0),
    }
}

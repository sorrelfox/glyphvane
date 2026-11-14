// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Line boxes and fragments of a paragraph at one width.

use std::ops::Range;
use std::sync::Arc;

use crate::breaking::break_lines;
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
#[derive(Debug)]
pub struct Layout<G = ()> {
    pub(crate) shaped: Arc<Shaped<G>>,
    pub(crate) max_width: f32,
    pub(crate) lines: Vec<Line>,
    pub(crate) frags: Vec<Fragment>,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl<G> Layout<G> {
    /// Breaks the shaped paragraph into lines no wider than `max_width`
    /// where possible, places fragments on a shared baseline per line and
    /// aligns the lines. `f32::INFINITY` gives the max-content layout.
    /// Never shapes.
    pub fn new(shaped: Arc<Shaped<G>>, max_width: f32) -> Self {
        let max = sanitize_width(max_width);
        let breaks = break_lines(&shaped, max);
        let strut = *shaped.strut();
        let align = shaped.paragraph().style().align;
        let items = shaped.paragraph().items();
        let mut lines = Vec::with_capacity(breaks.len());
        let mut frags = Vec::new();
        let (mut y, mut widest) = (0.0f32, 0.0f32);
        for lb in breaks {
            let first = frags.len();
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
                frags.push(frag);
            }
            let trimmed = shaped.trim_hanging(lb.range.start, lb.range.end);
            let content = shaped.width(lb.range.start, trimmed);
            let offset = align_offset(align, max, content);
            if offset != 0.0 {
                for f in &mut frags[first..] {
                    f.x += offset;
                }
            }
            widest = widest.max(content);
            let height = ascent + descent;
            lines.push(Line {
                range: lb.range,
                frags: first..frags.len(),
                x: offset,
                width: content,
                top: y,
                height,
                baseline: y + ascent,
                hard_break: lb.hard,
            });
            y += height;
        }
        Self {
            shaped,
            max_width: max,
            lines,
            frags,
            width: widest,
            height: y,
        }
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

    /// Lines that overlap `y0..y1`, for culling.
    pub fn lines_between(&self, y0: f32, y1: f32) -> Range<usize> {
        let first = self.lines.partition_point(|l| l.bottom() <= y0);
        let last = self.lines.partition_point(|l| l.top < y1);
        first..last.max(first)
    }

    /// Approximate heap memory held by the layout itself.
    pub fn heap_bytes(&self) -> usize {
        self.lines.capacity() * size_of::<Line>() + self.frags.capacity() * size_of::<Fragment>()
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

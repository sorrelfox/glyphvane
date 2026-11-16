// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Geometry queries on a layout: hit testing, carets, selection rectangles,
//! decorations and item regions.

use std::ops::Range;

use crate::layout::{Fragment, Layout, Line, Rect};
use crate::measure;
use crate::paragraph::{Affinity, Bias, ItemKind, is_break_char};
use crate::style::{ColorRole, UnderlineStyle};

/// The result of a hit test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Logical offset: a caret stop.
    pub offset: usize,
    pub affinity: Affinity,
    pub line: usize,
    /// The item under (or nearest to) the point; `None` only for an empty
    /// paragraph.
    pub item: Option<usize>,
    /// The document offset of the position.
    pub source: usize,
}

/// A caret: a vertical bar at `x` from `top` down `height`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caret {
    pub line: usize,
    pub x: f32,
    pub top: f32,
    pub height: f32,
}

/// What a decoration segment draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecorationKind {
    /// A run background; paint it under the selection and the glyphs.
    /// `round_left` / `round_right` say which ends are the run's outer ends
    /// (round those corners).
    Background {
        role: ColorRole,
        radius: f32,
        round_left: bool,
        round_right: bool,
    },
    Underline {
        style: UnderlineStyle,
        color: ColorRole,
    },
    Strikethrough {
        color: ColorRole,
    },
}

/// A decoration on one line. For lines, `rect.height` is the thickness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoration {
    pub kind: DecorationKind,
    pub rect: Rect,
}

impl<G> Layout<G> {
    // ----- lines ------------------------------------------------------------

    /// The line at `y`, clamped to the first and last line. In a columnar
    /// layout, the first column's line at `y` (clamped to its lines), or
    /// the first line of any column at `y` below it.
    pub fn line_at_y(&self, y: f32) -> usize {
        if self.is_columnar() {
            let first = self.column_lines_at(f32::NEG_INFINITY);
            let li = self.line_in(first.clone(), y);
            if y < self.lines[li].bottom() {
                return li;
            }
            return self
                .lines
                .iter()
                .position(|l| l.top <= y && y < l.bottom())
                .unwrap_or(li);
        }
        let ix = self.lines.partition_point(|l| l.bottom() <= y);
        ix.min(self.lines.len() - 1)
    }

    /// The line at `y` among `lines` (top to bottom), clamped to them.
    fn line_in(&self, lines: std::ops::Range<usize>, y: f32) -> usize {
        let (start, end) = (lines.start, lines.end.max(lines.start + 1));
        let ix = start + self.lines[start..end].partition_point(|l| l.bottom() <= y);
        ix.min(end - 1)
    }

    /// The line that shows the caret for `offset`. At a soft wrap,
    /// `Upstream` is the end of the line before and `Downstream` the start
    /// of the line after; after a forced break it is always the line after.
    pub fn line_for(&self, offset: usize, affinity: Affinity) -> usize {
        let n = self.lines.len();
        let k = self.lines.partition_point(|l| l.range.end <= offset);
        if k >= n {
            return n - 1;
        }
        let at_start = offset == self.lines[k].range.start;
        if affinity == Affinity::Upstream && at_start && k > 0 && !self.lines[k - 1].hard_break {
            k - 1
        } else {
            k
        }
    }

    /// The last caret position on a line: its end, or the position before
    /// its forced break.
    pub fn line_end(&self, line: usize) -> usize {
        let l = &self.lines[line];
        if l.hard_break {
            let text = &self.paragraph().text()[l.range.clone()];
            l.range.start + text.trim_end_matches(is_break_char).len()
        } else {
            l.range.end
        }
    }

    // ----- hit testing ------------------------------------------------------

    /// Hit test: a point in block coordinates → the nearest caret stop.
    ///
    /// Points above or below the block use the first or last line. Inside a
    /// text piece the nearest caret stop wins; on a box, the left half means
    /// before it and the right half after it. Where two pieces meet with
    /// padding or a margin between their carets, the nearer caret wins and
    /// the affinity says which one it is. Points past the end of a line
    /// give the line's end (`Upstream`, so the caret stays on that line);
    /// points before its start give the line's start.
    pub fn hit(&self, x: f32, y: f32) -> Hit {
        let li = if self.is_columnar() {
            self.line_in(self.column_lines_at(x), y)
        } else {
            self.line_at_y(y)
        };
        let line = &self.lines[li];
        let frags = &self.frags[line.frags.clone()];
        if frags.is_empty() {
            let item = self.paragraph().item_at(line.range.start);
            return self.make_hit(line.range.start, Affinity::Downstream, li, item);
        }
        let Some(k) = frags.iter().position(|f| x < f.outer_right()) else {
            let item = frags.last().map(|f| f.item);
            return self.make_hit(self.line_end(li), Affinity::Upstream, li, item);
        };
        let f = &frags[k];
        let (offset, mut affinity) = self.offset_in_fragment(f, x);
        let mut item = f.item;
        // At a boundary shared with the neighbouring fragment the two carets
        // can differ (padding, margins): take the strictly nearer one.
        let (fs, fe) = self.frag_offsets(f);
        if affinity == Affinity::Upstream && offset == fe {
            if let Some(g) = frags.get(k + 1)
                && self.frag_offsets(g).0 == fe
                && (x - g.x).abs() < (x - (f.x + f.width)).abs()
            {
                affinity = Affinity::Downstream;
                item = g.item;
            }
        } else if affinity == Affinity::Downstream && offset == fs && k > 0 {
            let p = &frags[k - 1];
            if self.frag_offsets(p).1 == fs && (x - (p.x + p.width)).abs() < (x - f.x).abs() {
                affinity = Affinity::Upstream;
                item = p.item;
            }
        }
        self.make_hit(offset, affinity, li, Some(item))
    }

    /// Logical start and end of a fragment.
    fn frag_offsets(&self, f: &Fragment) -> (usize, usize) {
        let start = self.paragraph().items()[f.item].range().start;
        (start + f.range.start, start + f.range.end)
    }

    fn offset_in_fragment(&self, f: &Fragment, x: f32) -> (usize, Affinity) {
        let items = self.paragraph().items();
        let start = items[f.item].range().start;
        match items[f.item].kind() {
            ItemKind::Text(_) => {
                let stops = self.shaped.stops(f.item);
                let local = x - f.x + measure::x_at(stops, f.range.start);
                let stop = measure::closest_stop(stops, local, f.range.start, f.range.end);
                let affinity = if stop == f.range.end {
                    Affinity::Upstream
                } else {
                    Affinity::Downstream
                };
                (start + stop, affinity)
            }
            _ if x < f.x + f.width / 2.0 => (start, Affinity::Downstream),
            _ => (start + f.range.end, Affinity::Upstream),
        }
    }

    fn make_hit(&self, offset: usize, affinity: Affinity, line: usize, item: Option<usize>) -> Hit {
        Hit {
            offset,
            affinity,
            line,
            item,
            source: self.paragraph().source_offset(offset, affinity),
        }
    }

    /// The item whose fragment (padding and margins included) contains the
    /// point, if any: for hover regions of links, tags and boxes.
    pub fn item_at_point(&self, x: f32, y: f32) -> Option<usize> {
        if self.is_columnar() {
            return self
                .lines
                .iter()
                .filter(|l| l.top <= y && y < l.bottom())
                .flat_map(|l| &self.frags[l.frags.clone()])
                .find(|f| f.outer_left() <= x && x < f.outer_right())
                .map(|f| f.item);
        }
        let li = self.lines.partition_point(|l| l.bottom() <= y);
        let line = self.lines.get(li).filter(|l| l.top <= y)?;
        self.frags[line.frags.clone()]
            .iter()
            .find(|f| f.outer_left() <= x && x < f.outer_right())
            .map(|f| f.item)
    }

    /// The rectangles of an item's fragments, each the full line height
    /// (for hover highlights of a link that wraps).
    pub fn item_rects(&self, item: usize) -> Vec<Rect> {
        let mut out = Vec::new();
        for line in &self.lines {
            for f in &self.frags[line.frags.clone()] {
                if f.item == item {
                    out.push(Rect {
                        x: f.outer_left(),
                        y: line.top,
                        width: f.outer_right() - f.outer_left(),
                        height: line.height,
                    });
                }
            }
        }
        out
    }

    // ----- carets -----------------------------------------------------------

    /// The caret for a logical offset (snapped down to a caret stop).
    ///
    /// `affinity` picks the line at a soft wrap and the side at an item
    /// boundary: `Upstream` is the end of the piece before (inside an
    /// inline-code background), `Downstream` the start of the piece after.
    /// The caret spans the content area of the text at the position (the
    /// strut's next to boxes and on empty lines), baseline shift included.
    pub fn caret(&self, offset: usize, affinity: Affinity) -> Caret {
        let offset = self.shaped.snap(offset, Bias::Left);
        let li = self.line_for(offset, affinity);
        let end = self.line_end(li);
        if offset > end {
            // After a hard break that ends the paragraph: no line follows,
            // so the caret stays at the end of the last line.
            return self.caret(end, Affinity::Upstream);
        }
        let line = &self.lines[li];
        let items = self.paragraph().items();
        let mut chosen: Option<&Fragment> = None;
        let mut before_x = line.x;
        for f in &self.frags[line.frags.clone()] {
            let start = items[f.item].range().start;
            let (fs, fe) = (start + f.range.start, start + f.range.end);
            if offset < fs {
                break;
            }
            if offset <= fe {
                chosen = Some(f);
                if offset < fe || affinity == Affinity::Upstream {
                    break;
                }
            } else {
                before_x = f.x + f.width;
            }
        }
        let strut = self.shaped.strut();
        let (x, ascent, descent, shift) = match chosen {
            Some(f) => {
                let start = items[f.item].range().start;
                match items[f.item].kind() {
                    ItemKind::Text(_) => {
                        let stops = self.shaped.stops(f.item);
                        let x = f.x + measure::x_at(stops, offset - start)
                            - measure::x_at(stops, f.range.start);
                        let m = self.shaped.item_metrics(f.item);
                        (x, m.ascent, m.descent, f.shift)
                    }
                    _ => {
                        let x = if offset == start { f.x } else { f.x + f.width };
                        (x, strut.metrics.ascent, strut.metrics.descent, 0.0)
                    }
                }
            }
            None => (before_x, strut.metrics.ascent, strut.metrics.descent, 0.0),
        };
        Caret {
            line: li,
            x,
            top: line.baseline - shift - ascent,
            height: ascent + descent,
        }
    }

    /// The caret for a document offset. Positions inside an atomic unit (a
    /// box, an escape, a grapheme cluster) move by `bias`.
    pub fn caret_for_source(&self, source: usize, bias: Bias) -> Caret {
        let (offset, affinity) = self.paragraph().logical_offset(source, bias);
        self.caret(self.shaped.snap(offset, bias), affinity)
    }

    // ----- selection --------------------------------------------------------

    /// Selection rectangles for the logical range `a..b`: one per run of
    /// adjacent fragments on a line, the full line box high. Background
    /// padding and box margins are included where the selection covers the
    /// item's end.
    pub fn selection_rects(&self, a: usize, b: usize) -> Vec<Rect> {
        let mut out = Vec::new();
        let a = self.shaped.snap(a, Bias::Left);
        let b = self.shaped.snap(b, Bias::Right);
        if a >= b {
            return out;
        }
        let items = self.paragraph().items();
        let first = self.lines.partition_point(|l| l.range.end <= a);
        for line in &self.lines[first..] {
            if line.range.start >= b {
                break;
            }
            let mut current: Option<Rect> = None;
            for f in &self.frags[line.frags.clone()] {
                let start = items[f.item].range().start;
                let (fs, fe) = (start + f.range.start, start + f.range.end);
                let (lo, hi) = (a.max(fs), b.min(fe));
                if lo >= hi {
                    continue;
                }
                let (x0, x1) = match items[f.item].kind() {
                    ItemKind::Text(_) => {
                        let stops = self.shaped.stops(f.item);
                        let base = f.x - measure::x_at(stops, f.range.start);
                        let mut x0 = base + measure::x_at(stops, lo - start);
                        let mut x1 = base + measure::x_at(stops, hi - start);
                        if lo == fs {
                            x0 -= f.pad_left;
                        }
                        if hi == fe {
                            x1 += f.pad_right;
                        }
                        (x0, x1)
                    }
                    _ => (f.outer_left(), f.outer_right()),
                };
                match &mut current {
                    Some(r) if x0 <= r.right() + 0.5 => r.width = x1.max(r.right()) - r.x,
                    _ => {
                        out.extend(current.take());
                        current = Some(Rect {
                            x: x0,
                            y: line.top,
                            width: x1 - x0,
                            height: line.height,
                        });
                    }
                }
            }
            out.extend(current);
        }
        out
    }

    /// Selection rectangles for a document range. A partly covered box or
    /// escape is included whole.
    pub fn selection_rects_for_source(&self, source: Range<usize>) -> Vec<Rect> {
        let (a, _) = self.paragraph().logical_offset(source.start, Bias::Left);
        let (b, _) = self.paragraph().logical_offset(source.end, Bias::Right);
        self.selection_rects(a, b)
    }

    // ----- decorations ------------------------------------------------------

    /// Backgrounds, underlines and strikethroughs of line `line`, in paint
    /// order (backgrounds first). Adjacent underline and strikethrough
    /// pieces of the same style and position are merged, so dashes and
    /// waves run on across style runs. Hanging white space at the line end
    /// is not decorated.
    pub fn decorations(&self, line: usize) -> Vec<Decoration> {
        let line_box: &Line = &self.lines[line];
        let items = self.paragraph().items();
        let right_limit = line_box.x + line_box.width;
        let mut backgrounds = Vec::new();
        let mut lines: Vec<Decoration> = Vec::new();
        for f in &self.frags[line_box.frags.clone()] {
            let ItemKind::Text(style) = items[f.item].kind() else {
                continue;
            };
            let m = self.shaped.item_metrics(f.item);
            let baseline = line_box.baseline - f.shift;
            if let Some(bg) = style.background {
                let x0 = f.outer_left();
                let x1 = f.outer_right().min(right_limit);
                if x1 > x0 {
                    backgrounds.push(Decoration {
                        kind: DecorationKind::Background {
                            role: bg.role,
                            radius: bg.radius,
                            round_left: f.range.start == 0,
                            round_right: f.range.end == items[f.item].len(),
                        },
                        rect: Rect {
                            x: x0,
                            y: baseline - m.ascent - bg.padding_y,
                            width: x1 - x0,
                            height: m.ascent + m.descent + 2.0 * bg.padding_y,
                        },
                    });
                }
            }
            let x0 = f.x;
            let x1 = (f.x + f.width).min(right_limit);
            if x1 <= x0 {
                continue;
            }
            if let Some(u) = style.underline {
                push_merged(
                    &mut lines,
                    Decoration {
                        kind: DecorationKind::Underline {
                            style: u.style,
                            color: u.color,
                        },
                        rect: Rect {
                            x: x0,
                            y: baseline + u.offset,
                            width: x1 - x0,
                            height: u.thickness,
                        },
                    },
                );
            }
            if let Some(s) = style.strikethrough {
                push_merged(
                    &mut lines,
                    Decoration {
                        kind: DecorationKind::Strikethrough { color: s.color },
                        rect: Rect {
                            x: x0,
                            y: baseline - m.x_height / 2.0 - s.thickness / 2.0,
                            width: x1 - x0,
                            height: s.thickness,
                        },
                    },
                );
            }
        }
        backgrounds.extend(lines);
        backgrounds
    }
}

/// Appends `d`, or extends an earlier segment of the same kind, position
/// and thickness that ends where `d` starts.
fn push_merged(out: &mut Vec<Decoration>, d: Decoration) {
    let joins = |e: &Decoration| {
        e.kind == d.kind
            && (e.rect.y - d.rect.y).abs() < 0.01
            && (e.rect.height - d.rect.height).abs() < 0.01
            && (e.rect.right() - d.rect.x).abs() < 0.5
    };
    match out.iter_mut().rev().find(|e| joins(e)) {
        Some(e) => e.rect.width = d.rect.right() - e.rect.x,
        None => out.push(d),
    }
}

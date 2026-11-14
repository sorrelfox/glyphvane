// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A paragraph with its runs shaped: everything that does not depend on the
//! width. Built once per content and font change; any number of layouts at
//! different widths share it.

use std::sync::Arc;

use crate::cache::RunCache;
use crate::measure::{self, FontMetrics, Measure, ShapedRun, Stop};
use crate::paragraph::{Bias, ItemKind, OBJECT_LEN, Paragraph, grapheme_starts, is_hanging};
use crate::style::{BoxAlign, LineHeight};

/// Tolerance for float comparisons of widths.
pub(crate) const EPS: f32 = 0.01;

/// Stops of an empty run.
static EMPTY_STOPS: [Stop; 1] = [Stop { offset: 0, x: 0.0 }];

/// The line-height strut: the paragraph's primary font and line height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strut {
    /// Metrics of the primary font.
    pub metrics: FontMetrics,
    pub line_height: f32,
    /// The strut's extent above the baseline: ascent plus half-leading.
    pub ascent: f32,
    /// The strut's extent below the baseline: descent plus half-leading.
    pub descent: f32,
}

/// Width-independent data of one item.
#[derive(Debug)]
struct ItemShape<G> {
    run: Option<Arc<ShapedRun<G>>>,
    /// Letter-spaced stops, when the run has letter spacing.
    tracked: Option<Box<[Stop]>>,
    /// The item's primary font (the strut's for boxes and breaks).
    metrics: FontMetrics,
    /// Extent above and below the line's baseline, shift included.
    ascent: f32,
    descent: f32,
    shift: f32,
    /// Background padding (text) or margin (box) at the outer ends.
    pad_left: f32,
    pad_right: f32,
}

/// A paragraph with shaped runs, letter spacing applied, and resolved
/// vertical metrics.
#[derive(Debug)]
pub struct Shaped<G = ()> {
    paragraph: Arc<Paragraph>,
    strut: Strut,
    items: Box<[ItemShape<G>]>,
}

impl<G> Shaped<G> {
    /// Shapes every text run through `cache` (hits are not reshaped) and
    /// resolves metrics and box alignment.
    pub fn new<M>(paragraph: Arc<Paragraph>, measure: &mut M, cache: &mut RunCache<G>) -> Self
    where
        M: Measure<Glyphs = G> + ?Sized,
    {
        let style = *paragraph.style();
        let m = cache.metrics(measure, style.font, style.size);
        let content = m.ascent + m.descent;
        let line_height = match style.line_height {
            LineHeight::Px(px) => px,
            LineHeight::Normal => content + m.line_gap,
        };
        let half_leading = (line_height - content) / 2.0;
        let strut = Strut {
            metrics: m,
            line_height,
            ascent: m.ascent + half_leading,
            descent: m.descent + half_leading,
        };
        let items = paragraph
            .items()
            .iter()
            .enumerate()
            .map(|(ix, item)| match item.kind() {
                ItemKind::Text(run_style) => {
                    let text = paragraph.item_text(ix);
                    let run = (!text.is_empty())
                        .then(|| cache.shape(measure, text, run_style.font, run_style.size));
                    let spacing = run_style.letter_spacing;
                    let tracked = match &run {
                        Some(run) if spacing != 0.0 => Some(track(text, run.stops(), spacing)),
                        _ => None,
                    };
                    let metrics = cache.metrics(measure, run_style.font, run_style.size);
                    // Text adds its content area to the line, not its own
                    // leading; a line height tighter than the run's font
                    // trims it as the strut is trimmed.
                    let trim = ((line_height - metrics.ascent - metrics.descent) / 2.0).min(0.0);
                    let shift = run_style.baseline_shift;
                    ItemShape {
                        run,
                        tracked,
                        metrics,
                        ascent: metrics.ascent + trim + shift,
                        descent: metrics.descent + trim - shift,
                        shift,
                        pad_left: run_style.padding_x(),
                        pad_right: run_style.padding_x(),
                    }
                }
                ItemKind::Box(b) => {
                    let height = b.height();
                    let (ascent, descent) = match b.align {
                        BoxAlign::Baseline => (b.ascent, b.descent),
                        BoxAlign::Middle => {
                            let middle = m.x_height / 2.0;
                            (height / 2.0 + middle, height / 2.0 - middle)
                        }
                        BoxAlign::TextTop => (m.ascent, height - m.ascent),
                        BoxAlign::TextBottom => (height - m.descent, m.descent),
                    };
                    ItemShape {
                        run: None,
                        tracked: None,
                        metrics: m,
                        ascent,
                        descent,
                        shift: 0.0,
                        pad_left: b.margin_left,
                        pad_right: b.margin_right,
                    }
                }
                ItemKind::HardBreak => ItemShape {
                    run: None,
                    tracked: None,
                    metrics: m,
                    ascent: 0.0,
                    descent: 0.0,
                    shift: 0.0,
                    pad_left: 0.0,
                    pad_right: 0.0,
                },
            })
            .collect();
        Self {
            paragraph,
            strut,
            items,
        }
    }

    pub fn paragraph(&self) -> &Arc<Paragraph> {
        &self.paragraph
    }

    pub fn strut(&self) -> &Strut {
        &self.strut
    }

    /// The shaped run of text item `item` (`None` for other items and for
    /// empty text).
    pub fn run(&self, item: usize) -> Option<&Arc<ShapedRun<G>>> {
        self.items[item].run.as_ref()
    }

    /// The primary-font metrics of item `item` (the strut's for boxes and
    /// hard breaks).
    pub fn item_metrics(&self, item: usize) -> &FontMetrics {
        &self.items[item].metrics
    }

    /// Caret stops of text item `item`, letter spacing applied.
    pub fn stops(&self, item: usize) -> &[Stop] {
        let shape = &self.items[item];
        match (&shape.tracked, &shape.run) {
            (Some(tracked), _) => tracked,
            (None, Some(run)) => run.stops(),
            (None, None) => &EMPTY_STOPS,
        }
    }

    /// x of an item-local byte offset in text item `item`, measured from
    /// the run's start, letter spacing applied.
    pub fn x_at(&self, item: usize, byte: usize) -> f32 {
        measure::x_at(self.stops(item), byte)
    }

    /// How far letter spacing moves a glyph whose cluster starts at
    /// item-local `byte`: add it to the glyph's x from the shaped run. Zero
    /// without letter spacing.
    pub fn glyph_shift(&self, item: usize, byte: usize) -> f32 {
        let shape = &self.items[item];
        match (&shape.tracked, &shape.run) {
            (Some(tracked), Some(run)) => {
                let ix = measure::floor_index(tracked, byte);
                tracked.get(ix).map_or(0.0, |s| s.x) - run.stops().get(ix).map_or(0.0, |s| s.x)
            }
            _ => 0.0,
        }
    }

    // ----- caret stops ----------------------------------------------------

    /// Caret positions in logical order: grapheme boundaries that start a
    /// glyph cluster, item boundaries, and both sides of every box and hard
    /// break.
    pub fn caret_stops(&self) -> impl Iterator<Item = usize> + '_ {
        let mut last = None;
        let first = std::iter::once(0);
        let rest = self
            .paragraph
            .items()
            .iter()
            .enumerate()
            .filter(|(_, it)| !it.is_empty())
            .flat_map(move |(ix, it)| {
                let start = it.range().start;
                let stops: Box<dyn Iterator<Item = usize> + '_> = match it.kind() {
                    ItemKind::Text(_) => Box::new(
                        self.stops(ix)
                            .iter()
                            .map(move |s| start + s.offset as usize),
                    ),
                    _ => Box::new([start, it.range().end].into_iter()),
                };
                stops
            });
        first.chain(rest).filter(move |&o| {
            let new = last != Some(o);
            last = Some(o);
            new
        })
    }

    /// The caret stop after `offset` (the end for the end).
    pub fn next_stop(&self, offset: usize) -> usize {
        let p = &self.paragraph;
        if offset >= p.len() {
            return p.len();
        }
        let Some(ix) = p.item_at(offset) else {
            return p.len();
        };
        let it = &p.items()[ix];
        let local = offset - it.range().start;
        it.range().start
            + match it.kind() {
                ItemKind::Text(_) => measure::next_stop(self.stops(ix), local, it.len()),
                _ => it.len(),
            }
    }

    /// The caret stop before `offset` (0 for 0).
    pub fn prev_stop(&self, offset: usize) -> usize {
        let p = &self.paragraph;
        let offset = offset.min(p.len());
        if offset == 0 {
            return 0;
        }
        // The non-empty item holding the position just before `offset`.
        let Some(ix) = p.item_at(offset - 1) else {
            return 0;
        };
        let it = &p.items()[ix];
        let local = offset - it.range().start;
        it.range().start
            + match it.kind() {
                ItemKind::Text(_) => measure::prev_stop(self.stops(ix), local),
                _ => 0,
            }
    }

    /// Snaps `offset` to a caret stop: down for `Left`, up for `Right`.
    pub fn snap(&self, offset: usize, bias: Bias) -> usize {
        let p = &self.paragraph;
        let offset = offset.min(p.len());
        let Some(ix) = p.item_at(offset) else {
            return 0;
        };
        let it = &p.items()[ix];
        let start = it.range().start;
        if offset <= start || offset >= it.range().end {
            return offset;
        }
        let local = offset - start;
        start
            + match (it.kind(), bias) {
                (ItemKind::Text(_), Bias::Left) => measure::floor_stop(self.stops(ix), local),
                (ItemKind::Text(_), Bias::Right) => {
                    measure::ceil_stop(self.stops(ix), local, it.len())
                }
                (_, Bias::Left) => 0,
                (_, Bias::Right) => it.len(),
            }
    }

    // ----- widths -----------------------------------------------------------

    /// Width of the widest line when nothing but hard breaks wraps (CSS
    /// max-content), hanging white space excluded.
    pub fn max_content_width(&self) -> f32 {
        self.widest_segment(|mandatory| mandatory)
    }

    /// Width of the widest piece between two UAX #14 opportunities (CSS
    /// min-content with `overflow-wrap: break-word`; the `anywhere` fallback
    /// is ignored so that table columns are not sized to one grapheme).
    pub fn min_content_width(&self) -> f32 {
        self.widest_segment(|_| true)
    }

    fn widest_segment(&self, ends_segment: impl Fn(bool) -> bool) -> f32 {
        let mut widest = 0.0f32;
        let mut start = 0;
        for b in self.paragraph.breaks() {
            let pos = b.offset as usize;
            if ends_segment(b.mandatory) {
                let trail = self.trim_hanging(start, pos);
                widest = widest.max(self.width(start, trail));
                start = pos;
            }
        }
        widest
    }

    /// Advance of the logical range `a..b`, including background padding
    /// and box margins at item ends inside the range.
    pub fn width(&self, a: usize, b: usize) -> f32 {
        let mut w = 0.0;
        for (ix, lo, hi) in self.pieces(a, b) {
            w += self.piece_width(ix, lo, hi);
        }
        w
    }

    /// `(width(a, mid), width(a, b))` in one pass, for `a <= mid <= b`.
    pub(crate) fn width_split(&self, a: usize, mid: usize, b: usize) -> (f32, f32) {
        let (mut to_mid, mut to_b) = (0.0, 0.0);
        for (ix, lo, hi) in self.pieces(a, b) {
            let start = self.paragraph.items()[ix].range().start;
            let w = self.piece_width(ix, lo, hi);
            to_b += w;
            if start + hi <= mid {
                to_mid += w;
            } else if start + lo < mid {
                to_mid += self.piece_width(ix, lo, mid - start);
            }
        }
        (to_mid, to_b)
    }

    fn piece_width(&self, ix: usize, lo: usize, hi: usize) -> f32 {
        let it = &self.paragraph.items()[ix];
        let shape = &self.items[ix];
        match it.kind() {
            ItemKind::Text(_) => {
                let stops = self.stops(ix);
                let mut w = measure::x_at(stops, hi) - measure::x_at(stops, lo);
                if lo == 0 {
                    w += shape.pad_left;
                }
                if hi == it.len() {
                    w += shape.pad_right;
                }
                w
            }
            ItemKind::Box(b) if lo == 0 && hi == OBJECT_LEN => b.advance(),
            _ => 0.0,
        }
    }

    /// Items overlapping `a..b`, as `(item, local lo, local hi)` with
    /// `lo < hi`.
    pub(crate) fn pieces(
        &self,
        a: usize,
        b: usize,
    ) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        let items = self.paragraph.items();
        let first = items.partition_point(|it| it.range().end <= a);
        items[first..]
            .iter()
            .enumerate()
            .map_while(move |(k, it)| {
                let r = it.range();
                (r.start < b).then(|| {
                    let lo = a.max(r.start) - r.start;
                    let hi = b.min(r.end) - r.start;
                    (first + k, lo, hi)
                })
            })
            .filter(|&(_, lo, hi)| lo < hi)
    }

    /// Start of the hanging white space at the end of `a..b`.
    pub(crate) fn trim_hanging(&self, a: usize, b: usize) -> usize {
        a + self.paragraph.text()[a..b]
            .trim_end_matches(is_hanging)
            .len()
    }

    /// The furthest caret stop `e` in `(a, b]` with `width(a, e) <= max`,
    /// and at least one stop of progress: `overflow-wrap: anywhere` breaks
    /// at grapheme stops, never inside a box.
    pub(crate) fn fit(&self, a: usize, b: usize, max: f32) -> usize {
        let mut acc = 0.0;
        let mut best = None;
        for (ix, lo, hi) in self.pieces(a, b) {
            let it = &self.paragraph.items()[ix];
            let start = it.range().start;
            match it.kind() {
                ItemKind::Text(_) => {
                    let shape = &self.items[ix];
                    let stops = self.stops(ix);
                    let pad = if lo == 0 { shape.pad_left } else { 0.0 };
                    let x0 = measure::x_at(stops, lo);
                    let limit = max + EPS - acc - pad + x0;
                    let Some(mut e) = measure::last_stop_within(stops, lo, hi, limit) else {
                        break;
                    };
                    // The right padding comes with the item's last stop.
                    if e == it.len() && measure::x_at(stops, e) + shape.pad_right > limit {
                        match measure::last_stop_within(stops, lo, e - 1, limit) {
                            Some(earlier) => e = earlier,
                            None => break,
                        }
                    }
                    best = Some(start + e);
                    if e != hi {
                        break;
                    }
                    acc += self.piece_width(ix, lo, hi);
                }
                ItemKind::Box(bx) => {
                    if acc + bx.advance() > max + EPS {
                        break;
                    }
                    acc += bx.advance();
                    best = Some(start + OBJECT_LEN);
                }
                ItemKind::HardBreak => best = Some(start + hi),
            }
        }
        match best {
            Some(e) if e > a => e,
            _ => self.next_stop(a).min(b),
        }
    }

    /// Approximate heap memory held by this shaping, without the shared
    /// runs (the run cache accounts for those) and the paragraph.
    pub fn heap_bytes(&self) -> usize {
        self.items.len() * size_of::<ItemShape<G>>()
            + self
                .items
                .iter()
                .filter_map(|s| s.tracked.as_ref())
                .map(|t| t.len() * size_of::<Stop>())
                .sum::<usize>()
    }

    // ----- crate-internal accessors -------------------------------------

    pub(crate) fn extent(&self, item: usize) -> (f32, f32) {
        let s = &self.items[item];
        (s.ascent, s.descent)
    }

    pub(crate) fn shift(&self, item: usize) -> f32 {
        self.items[item].shift
    }

    pub(crate) fn padding(&self, item: usize) -> (f32, f32) {
        let s = &self.items[item];
        (s.pad_left, s.pad_right)
    }
}

/// Letter spacing: every stop moves right by `spacing` times the number of
/// grapheme clusters before it, so each cluster gets `spacing` after it.
/// x never decreases, even for negative spacing.
fn track(text: &str, stops: &[Stop], spacing: f32) -> Box<[Stop]> {
    let mut graphemes = grapheme_starts(text).peekable();
    let mut before = 0u32;
    let mut prev = 0.0f32;
    stops
        .iter()
        .map(|s| {
            while graphemes.peek().is_some_and(|&g| g < s.offset as usize) {
                graphemes.next();
                before += 1;
            }
            prev = (s.x + spacing * before as f32).max(prev);
            Stop {
                offset: s.offset,
                x: prev,
            }
        })
        .collect()
}

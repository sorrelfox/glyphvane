// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The interface between the layout and a text system.
//!
//! A text run is shaped once, as a whole, in its own font. The layout then
//! reads only advances at caret stops (grapheme boundaries where a glyph
//! cluster starts), so a width change never reshapes.

use unicode_segmentation::UnicodeSegmentation;

use crate::paragraph::{grapheme_starts, to_u32};
use crate::style::StyleId;

/// Metrics of a font at a size, in px, all positive.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FontMetrics {
    /// Height above the baseline.
    pub ascent: f32,
    /// Depth below the baseline.
    pub descent: f32,
    pub line_gap: f32,
    pub x_height: f32,
}

impl FontMetrics {
    /// Absolute, finite values (text systems differ in sign conventions).
    pub fn sanitized(self) -> Self {
        let fix = |v: f32| if v.is_finite() { v.abs() } else { 0.0 };
        Self {
            ascent: fix(self.ascent),
            descent: fix(self.descent),
            line_gap: fix(self.line_gap),
            x_height: fix(self.x_height),
        }
    }

    /// The x-height assumed, per em, for a font that reports none.
    pub const FALLBACK_X_HEIGHT_EM: f32 = 0.5;

    /// `self` with an x-height at `size` px. Fonts without one in their
    /// tables (no OS/2 `sxHeight`) report 0, which would put middle-aligned
    /// boxes and the strikethrough on the baseline: then `measured` (the
    /// height of the `x` glyph, if the text system can tell) is used when it
    /// is plausible (positive and below the ascent), else
    /// [`Self::FALLBACK_X_HEIGHT_EM`] of the size.
    pub fn with_x_height_fallback(self, size: f32, measured: Option<f32>) -> Self {
        if self.x_height > 0.0 && self.x_height.is_finite() {
            return self;
        }
        let x_height = measured
            .filter(|&h| h.is_finite() && h > 0.0 && h < self.ascent)
            .unwrap_or_else(|| {
                let size = if size.is_finite() { size.abs() } else { 0.0 };
                Self::FALLBACK_X_HEIGHT_EM * size
            });
        Self { x_height, ..self }
    }
}

/// A caret stop in a run: a byte offset and the x of the caret there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub offset: u32,
    pub x: f32,
}

/// A shaped text run, reduced to what layout and hit testing need, plus the
/// text system's own data for painting.
///
/// `G` is whatever the painter needs besides advances (for GPUI, the
/// `LineLayout` with glyph ids and positions); tests use `()`.
#[derive(Debug, Clone)]
pub struct ShapedRun<G = ()> {
    len: usize,
    width: f32,
    /// Ascending in offset and in x, from `(0, 0)` to `(len, width)`.
    stops: Box<[Stop]>,
    glyphs: G,
    glyph_bytes: usize,
}

impl<G> ShapedRun<G> {
    /// Builds the caret stops from glyph clusters.
    ///
    /// `clusters` gives `(byte index, x)` for every glyph, in visual order
    /// (left to right). A grapheme boundary is a stop only when a glyph
    /// cluster starts there, so boundaries inside a ligature are not stops.
    /// `width` is the run's total advance. `glyph_bytes` is the heap memory
    /// held by `glyphs`, for the cache budget.
    ///
    /// The input is cleaned rather than trusted: indices past the end and
    /// non-finite positions are dropped, and stop x never decreases.
    pub fn new(
        text: &str,
        width: f32,
        clusters: impl IntoIterator<Item = (usize, f32)>,
        glyphs: G,
        glyph_bytes: usize,
    ) -> Self {
        let mut starts: Vec<(usize, f32)> = clusters
            .into_iter()
            .filter(|&(i, x)| i <= text.len() && x.is_finite())
            .collect();
        // Stable: of several glyphs in one cluster, the first (leftmost) wins.
        starts.sort_by_key(|&(i, _)| i);
        starts.dedup_by_key(|&mut (i, _)| i);
        let mut stops = Vec::with_capacity(starts.len() + 1);
        stops.push(Stop { offset: 0, x: 0.0 });
        let mut last_x = 0.0f32;
        let mut glyph = starts.iter().peekable();
        for boundary in grapheme_starts(text).skip(1) {
            while glyph.peek().is_some_and(|&&(i, _)| i < boundary) {
                glyph.next();
            }
            if let Some(&&(i, x)) = glyph.peek()
                && i == boundary
            {
                last_x = x.max(last_x);
                stops.push(Stop {
                    offset: to_u32(boundary),
                    x: last_x,
                });
            }
        }
        let width = if width.is_finite() {
            width.max(last_x)
        } else {
            last_x
        };
        if !text.is_empty() {
            stops.push(Stop {
                offset: to_u32(text.len()),
                x: width,
            });
        }
        Self {
            len: text.len(),
            width: if text.is_empty() { 0.0 } else { width },
            stops: stops.into_boxed_slice(),
            glyphs,
            glyph_bytes,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Total advance, without letter spacing.
    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn stops(&self) -> &[Stop] {
        &self.stops
    }

    pub fn glyphs(&self) -> &G {
        &self.glyphs
    }

    /// Approximate heap memory held by the run (stops and glyph data).
    pub fn heap_bytes(&self) -> usize {
        self.stops.len() * size_of::<Stop>() + self.glyph_bytes
    }
}

/// A text system as the layout sees it.
///
/// Implementations shape with the platform text system and report the
/// metrics of the **primary** font of a style: the content area of a line
/// comes from them, never from the faces that font fallback used, so an
/// emoji or a CJK fallback does not grow a line.
pub trait Measure {
    /// What the painter needs besides advances.
    type Glyphs;

    /// Shapes `text` as one run in `font` at `size` px, left to right,
    /// without letter spacing.
    fn shape(&mut self, text: &str, font: StyleId, size: f32) -> ShapedRun<Self::Glyphs>;

    /// Metrics of the primary font of `font` at `size` px.
    fn metrics(&mut self, font: StyleId, size: f32) -> FontMetrics;
}

/// A deterministic monospace measurer for tests and benchmarks.
///
/// Every grapheme advances `0.6 em`; wide (East Asian) and emoji graphemes
/// advance `1 em`; controls and zero-width characters advance 0. Metrics are
/// ascent `0.8 em`, descent `0.2 em`, line gap `0.2 em`, x-height
/// `0.5 em`, for every style. With `ligatures`, `fi` and `fl` form one glyph
/// cluster, so there is no caret stop inside them.
#[derive(Debug, Clone, Default)]
pub struct FakeMono {
    /// Runs shaped so far.
    pub shaped: usize,
    pub ligatures: bool,
}

impl FakeMono {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance of one grapheme at `size` px.
    pub fn advance(grapheme: &str, size: f32) -> f32 {
        let Some(c) = grapheme.chars().next() else {
            return 0.0;
        };
        if c.is_control() || matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}') {
            return 0.0;
        }
        let wide = matches!(c as u32,
            0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
            | 0x1F000..=0x1FAFF | 0x20000..=0x3FFFD);
        if wide { size } else { 0.6 * size }
    }
}

impl Measure for FakeMono {
    type Glyphs = ();

    fn shape(&mut self, text: &str, _font: StyleId, size: f32) -> ShapedRun<()> {
        self.shaped += 1;
        let mut x = 0.0;
        let mut clusters = Vec::new();
        let mut graphemes = text.grapheme_indices(true).peekable();
        while let Some((i, g)) = graphemes.next() {
            clusters.push((i, x));
            x += Self::advance(g, size);
            if self.ligatures
                && g == "f"
                && let Some(&(_, next)) = graphemes.peek()
                && (next == "i" || next == "l")
            {
                x += Self::advance(next, size);
                graphemes.next();
            }
        }
        ShapedRun::new(text, x, clusters, (), 0)
    }

    fn metrics(&mut self, _font: StyleId, size: f32) -> FontMetrics {
        FontMetrics {
            ascent: 0.8 * size,
            descent: 0.2 * size,
            line_gap: 0.2 * size,
            x_height: 0.5 * size,
        }
    }
}

// ----- stop arithmetic, shared by raw and letter-spaced stops ------------

/// Index of the last stop at or before `byte` (stops start at 0).
pub(crate) fn floor_index(stops: &[Stop], byte: usize) -> usize {
    stops
        .partition_point(|s| s.offset as usize <= byte)
        .saturating_sub(1)
}

/// x at a byte offset; offsets between stops snap down.
pub(crate) fn x_at(stops: &[Stop], byte: usize) -> f32 {
    stops.get(floor_index(stops, byte)).map_or(0.0, |s| s.x)
}

/// The stop at or before `byte`.
pub(crate) fn floor_stop(stops: &[Stop], byte: usize) -> usize {
    stops
        .get(floor_index(stops, byte))
        .map_or(0, |s| s.offset as usize)
}

/// The first stop at or after `byte`.
pub(crate) fn ceil_stop(stops: &[Stop], byte: usize, len: usize) -> usize {
    let ix = stops.partition_point(|s| (s.offset as usize) < byte);
    stops.get(ix).map_or(len, |s| s.offset as usize)
}

/// The first stop after `byte`, or `len`.
pub(crate) fn next_stop(stops: &[Stop], byte: usize, len: usize) -> usize {
    let ix = stops.partition_point(|s| s.offset as usize <= byte);
    stops.get(ix).map_or(len, |s| s.offset as usize)
}

/// The last stop before `byte`, or 0.
pub(crate) fn prev_stop(stops: &[Stop], byte: usize) -> usize {
    let ix = stops.partition_point(|s| (s.offset as usize) < byte);
    ix.checked_sub(1)
        .and_then(|i| stops.get(i))
        .map_or(0, |s| s.offset as usize)
}

/// The stop nearest to `x` among those in `lo..=hi`; ties go left.
pub(crate) fn closest_stop(stops: &[Stop], x: f32, lo: usize, hi: usize) -> usize {
    let first = stops.partition_point(|s| (s.offset as usize) < lo);
    let last = stops.partition_point(|s| s.offset as usize <= hi);
    let slice = &stops[first..last.max(first)];
    let ix = slice.partition_point(|s| s.x < x);
    match (ix.checked_sub(1).map(|i| slice[i]), slice.get(ix).copied()) {
        (Some(before), Some(after)) => {
            if x - before.x <= after.x - x {
                before.offset as usize
            } else {
                after.offset as usize
            }
        }
        (Some(only), None) | (None, Some(only)) => only.offset as usize,
        (None, None) => lo,
    }
}

/// The furthest stop in `(from, to]` whose x is at most `max_x`, or `None`
/// when even the first stop after `from` is beyond it.
pub(crate) fn last_stop_within(
    stops: &[Stop],
    from: usize,
    to: usize,
    max_x: f32,
) -> Option<usize> {
    let first = stops.partition_point(|s| s.offset as usize <= from);
    let last = stops.partition_point(|s| s.offset as usize <= to);
    let slice = &stops[first..last.max(first)];
    let n = slice.partition_point(|s| s.x <= max_x);
    n.checked_sub(1).map(|i| slice[i].offset as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_x_height_falls_back() {
        let m = |x_height| FontMetrics {
            ascent: 9.0,
            descent: 3.0,
            line_gap: 0.0,
            x_height,
        };
        // Reported: kept, whatever was measured.
        assert_eq!(m(4.0).with_x_height_fallback(12.0, Some(5.0)), m(4.0));
        // Missing: the measured `x` when plausible, else half an em.
        assert_eq!(m(0.0).with_x_height_fallback(12.0, Some(5.5)), m(5.5));
        assert_eq!(m(0.0).with_x_height_fallback(12.0, None), m(6.0));
        for implausible in [0.0, -1.0, 9.0, 12.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                m(0.0).with_x_height_fallback(12.0, Some(implausible)),
                m(6.0),
                "{implausible}"
            );
        }
        assert_eq!(m(f32::NAN).with_x_height_fallback(12.0, None), m(6.0));
        assert_eq!(m(0.0).with_x_height_fallback(f32::NAN, None), m(0.0));
    }

    #[test]
    fn stops_skip_ligature_insides_and_cluster_continuations() {
        // "e\u{301}" is one grapheme; "fi" is one glyph cluster.
        let text = "e\u{301}fix";
        let run = ShapedRun::new(text, 30.0, [(0, 0.0), (3, 6.0), (5, 18.0)], (), 0);
        let offsets: Vec<u32> = run.stops().iter().map(|s| s.offset).collect();
        assert_eq!(offsets, [0, 3, 5, 6]);
        assert_eq!(x_at(run.stops(), 4), 6.0, "inside the ligature snaps down");
        assert_eq!(floor_stop(run.stops(), 4), 3);
        assert_eq!(ceil_stop(run.stops(), 4, text.len()), 5);
        assert_eq!(next_stop(run.stops(), 3, text.len()), 5);
        assert_eq!(prev_stop(run.stops(), 5), 3);
        assert_eq!(prev_stop(run.stops(), 0), 0);
    }

    #[test]
    fn bad_glyph_data_is_cleaned() {
        let run = ShapedRun::new(
            "abc",
            f32::NAN,
            [(1, 9.0), (2, 4.0), (9, 1.0), (0, f32::INFINITY)],
            (),
            0,
        );
        let xs: Vec<f32> = run.stops().iter().map(|s| s.x).collect();
        assert_eq!(xs, [0.0, 9.0, 9.0, 9.0], "monotonic, finite");
        assert_eq!(run.width(), 9.0);
    }

    #[test]
    fn closest_and_last_within() {
        let run = FakeMono::default().shape("abcd", StyleId(0), 10.0);
        let s = run.stops();
        assert_eq!(closest_stop(s, 8.9, 0, 4), 1);
        assert_eq!(closest_stop(s, 9.1, 0, 4), 2);
        assert_eq!(closest_stop(s, 100.0, 1, 2), 2);
        assert_eq!(last_stop_within(s, 0, 4, 13.0), Some(2));
        assert_eq!(last_stop_within(s, 0, 4, 5.0), None);
    }

    #[test]
    fn fake_mono_ligatures_remove_inner_stops() {
        let mut m = FakeMono {
            ligatures: true,
            ..FakeMono::default()
        };
        let run = m.shape("fix", StyleId(0), 10.0);
        let offsets: Vec<u32> = run.stops().iter().map(|s| s.offset).collect();
        assert_eq!(offsets, [0, 2, 3]);
        assert_eq!(run.width(), 18.0);
    }
}

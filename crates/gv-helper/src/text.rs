// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The text of a PDF page and where each character is, inside the helper
//! (the PDF viewer's find and selection).
//!
//! hayro interprets the page with a device that draws nothing: it is
//! handed each run of positioned glyphs (also invisible text, as in
//! scanned pages with a text layer) and keeps every glyph's Unicode text
//! ([`hayro::hayro_interpret::font::Glyph::as_unicode`]: the font's
//! `ToUnicode` map, else its glyph names) and its box on the page. Images
//! are never decoded and Type 3 glyph procedures never run.
//!
//! - **Order.** The content stream's: no reading-order analysis, so a
//!   two-column page may interleave its columns.
//! - **Spaces and line ends.** A gap of more than a quarter of the font
//!   size between two glyphs on a line adds a space, unless the PDF drew
//!   one; a glyph off the previous one's line starts a new line (`\n`).
//! - **Boxes.** Each character has a box in points from the page's top
//!   left (as shown: crop box, rotated): from the glyph's origin to its
//!   advance, and from 0.2 em below the baseline to 0.8 em above it (the
//!   fonts' own metrics are not used). A space or line end added here has
//!   the box of the gap it stands for. A ligature's characters share its
//!   box in equal parts.
//! - **Duplicates.** Text drawn twice in place (filled, then stroked, or a
//!   fake bold) is kept once.
//! - **Limits.** At most [`MAX_CHARS`] characters per page; the parent's
//!   deadline and the helper's memory limit hold as for drawing; a panic
//!   inside hayro is caught and reported as an error.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt as _;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, interpret_page,
};
use hayro::hayro_syntax::page::Page;
use hayro::kurbo::{Affine, BezPath, Point, Rect, Vec2};

use crate::protocol::{PageText, TextBox};

/// The most characters kept of one page.
pub const MAX_CHARS: usize = 200_000;

/// Glyph space units per em (hayro's).
const UNITS: f64 = 1000.0;

/// The box of a glyph below and above its baseline, in em.
const DESCENT: f64 = 0.2;
const ASCENT: f64 = 0.8;

/// A gap wider than this (em) between two glyphs on a line is a space.
const SPACE_GAP: f64 = 0.25;

/// A glyph further than this (em) off the previous one's baseline starts a
/// new line.
const LINE_GAP: f64 = 0.6;

/// Two glyphs with the same text closer than this (em) are one.
const SAME_PLACE: f64 = 0.1;

/// The text of `page` and each character's box.
pub fn extract<'a>(page: &'a Page<'a>, cache: &InterpreterCache<'a>) -> PageText {
    let (w, h) = page.render_dimensions();
    let transform = page.initial_transform(true).to_kurbo();
    let mut context = Context::new(
        transform,
        Rect::new(0.0, 0.0, f64::from(w), f64::from(h)),
        cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    let mut device = TextDevice::default();
    interpret_page(page, &mut context, &mut device);
    device.finish()
}

/// [`extract`], with a panic inside hayro reported as an error.
pub fn extract_caught<'a>(
    page: &'a Page<'a>,
    cache: &InterpreterCache<'a>,
) -> Result<PageText, String> {
    catch_unwind(AssertUnwindSafe(|| extract(page, cache)))
        .map_err(|_| "The text of the PDF page could not be read.".to_owned())
}

/// The glyph placed last.
#[derive(Debug, Clone, Copy)]
struct Last {
    origin: Point,
    /// Where its advance ends.
    end: Point,
    /// The unit vector along its baseline.
    dir: Vec2,
    /// Its font size, points.
    em: f64,
    /// The first character of its text, if it had any.
    first: Option<char>,
}

/// A device that keeps the glyphs' text and boxes and draws nothing.
#[derive(Debug, Default)]
struct TextDevice {
    text: String,
    boxes: Vec<TextBox>,
    last: Option<Last>,
}

impl TextDevice {
    fn full(&self) -> bool {
        self.boxes.len() >= MAX_CHARS
    }

    fn push_char(&mut self, c: char, b: TextBox) {
        if !self.full() {
            self.text.push(c);
            self.boxes.push(b);
        }
    }

    fn ends_with_space(&self) -> bool {
        self.text
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
    }

    /// Keeps one glyph placed by `t` (glyph space to page points), `adv`
    /// units wide, with `unicode` as its text.
    fn glyph(&mut self, t: Affine, adv: f64, unicode: Option<BfString>) {
        let origin = t * Point::ORIGIN;
        let end = t * Point::new(adv, 0.0);
        let up = t * Point::new(0.0, UNITS) - origin;
        let em = up.hypot();
        let along = t * Point::new(UNITS, 0.0) - origin;
        if !(em.is_finite() && em > 1e-3 && along.hypot() > 1e-6) || !end.is_finite() {
            return;
        }
        let dir = along / along.hypot();
        let chars: Vec<char> = match unicode {
            Some(BfString::Char(c)) => vec![c],
            Some(BfString::String(s)) => s.chars().collect(),
            None => Vec::new(),
        };
        let chars: Vec<char> = chars
            .into_iter()
            .filter(|c| !c.is_control() || *c == '\t')
            .map(|c| if c == '\u{a0}' || c == '\t' { ' ' } else { c })
            .collect();
        if let Some(last) = self.last {
            // Drawn twice in place.
            if last.first == chars.first().copied()
                && (origin - last.origin).hypot() < SAME_PLACE * em.min(last.em)
            {
                return;
            }
            let delta = origin - last.end;
            let ahead = delta.dot(last.dir);
            let off = last.dir.cross(delta).abs();
            let gap_box = |a: Point, b: Point, em: f64| {
                let d = DESCENT * em;
                let u = ASCENT * em;
                bbox(&[a, b], up_of(last.dir, d), up_of(last.dir, u))
            };
            if off > LINE_GAP * last.em {
                if !self.text.ends_with('\n') && !self.text.is_empty() {
                    self.push_char('\n', gap_box(last.end, last.end, last.em));
                }
            } else if (ahead > SPACE_GAP * last.em.min(em) || ahead < -2.0 * last.em)
                && !self.ends_with_space()
                && !chars.first().is_some_and(|c| c.is_whitespace())
            {
                self.push_char(' ', gap_box(last.end, origin, last.em));
            }
        }
        self.last = Some(Last {
            origin,
            end,
            dir,
            em,
            first: chars.first().copied(),
        });
        if chars.is_empty() {
            return;
        }
        // The glyph's box, shared out among its characters along the
        // baseline.
        let n = chars.len() as f64;
        for (i, c) in chars.into_iter().enumerate() {
            let x0 = adv * i as f64 / n;
            let x1 = adv * (i + 1) as f64 / n;
            let corners = [
                t * Point::new(x0, -DESCENT * UNITS),
                t * Point::new(x1, -DESCENT * UNITS),
                t * Point::new(x0, ASCENT * UNITS),
                t * Point::new(x1, ASCENT * UNITS),
            ];
            self.push_char(c, bbox(&corners, Vec2::ZERO, Vec2::ZERO));
        }
    }

    fn finish(mut self) -> PageText {
        while self.text.ends_with(['\n', ' ']) {
            self.text.pop();
            self.boxes.pop();
        }
        PageText {
            text: self.text,
            boxes: self.boxes,
        }
    }
}

/// The vector `by` points "up" from a baseline along `dir` (y grows down
/// on the page).
fn up_of(dir: Vec2, by: f64) -> Vec2 {
    Vec2::new(dir.y, -dir.x) * by
}

/// The box around `points`, each also moved by `down` (against up) and
/// `up`.
fn bbox(points: &[Point], down: Vec2, up: Vec2) -> TextBox {
    let mut r = Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in points {
        for q in [*p, *p - down, *p + up] {
            r.x0 = r.x0.min(q.x);
            r.y0 = r.y0.min(q.y);
            r.x1 = r.x1.max(q.x);
            r.y1 = r.y1.max(q.y);
        }
    }
    TextBox {
        x0: r.x0 as f32,
        y0: r.y0 as f32,
        x1: r.x1 as f32,
        y1: r.y1 as f32,
    }
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        if self.full() {
            return;
        }
        let glyphs = run.glyphs();
        for (i, glyph) in glyphs.iter().enumerate() {
            let t = props.transform * glyph.transform();
            let advance = match &**glyph {
                Glyph::Outline(g) => g.advance_width(),
                Glyph::Type3(g) => g.advance_width(),
            };
            let mut adv = advance
                .map(f64::from)
                .filter(|a| a.is_finite() && *a > 0.0)
                .unwrap_or(UNITS / 2.0);
            // Never past the next glyph of the run on the same line.
            if let Some(next) = glyphs.get(i + 1) {
                let to = (props.transform * next.transform()) * Point::ORIGIN;
                if let Some(inv) = invert(t) {
                    let local = inv * to;
                    if local.x > 0.0 && local.x < adv && local.y.abs() < UNITS * 0.1 {
                        adv = local.x;
                    }
                }
            }
            self.glyph(t, adv, glyph.as_unicode());
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

/// The inverse of `t`, if it has one.
fn invert(t: Affine) -> Option<Affine> {
    let det = t.determinant();
    (det.is_finite() && det.abs() > 1e-12).then(|| t.inverse())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph_at(dev: &mut TextDevice, x: f64, y: f64, size: f64, c: char) {
        // Glyph space (1000 units per em, y up) to page points (y down).
        let t = Affine::new([size / UNITS, 0.0, 0.0, -size / UNITS, x, y]);
        dev.glyph(t, 500.0, Some(BfString::Char(c)));
    }

    #[test]
    fn gaps_become_spaces_and_lines_become_line_ends() {
        let mut dev = TextDevice::default();
        // "ab" at 10 pt, 5 pt per glyph; a gap of 10 pt; "c"; next line "d".
        glyph_at(&mut dev, 10.0, 20.0, 10.0, 'a');
        glyph_at(&mut dev, 15.0, 20.0, 10.0, 'b');
        glyph_at(&mut dev, 30.0, 20.0, 10.0, 'c');
        glyph_at(&mut dev, 10.0, 40.0, 10.0, 'd');
        // Drawn again in place (fill, then stroke): kept once.
        glyph_at(&mut dev, 10.0, 40.0, 10.0, 'd');
        let page = dev.finish();
        assert_eq!(page.text, "ab c\nd");
        assert_eq!(page.boxes.len(), page.text.chars().count());
        let a = page.boxes[0];
        assert!(
            (a.x0 - 10.0).abs() < 1e-3 && (a.x1 - 15.0).abs() < 1e-3,
            "{a:?}"
        );
        // 0.8 em above the baseline (y 20) to 0.2 em below.
        assert!(
            (a.y0 - 12.0).abs() < 1e-3 && (a.y1 - 22.0).abs() < 1e-3,
            "{a:?}"
        );
        // The space spans the gap.
        let space = page.boxes[2];
        assert!((space.x0 - 20.0).abs() < 1e-3 && (space.x1 - 30.0).abs() < 1e-3);
    }

    #[test]
    fn ligatures_share_their_box_and_unknown_glyphs_add_no_text() {
        let mut dev = TextDevice::default();
        let t = Affine::new([0.01, 0.0, 0.0, -0.01, 0.0, 10.0]);
        dev.glyph(t, 600.0, Some(BfString::String("fi".into())));
        let t2 = Affine::new([0.01, 0.0, 0.0, -0.01, 6.0, 10.0]);
        dev.glyph(t2, 500.0, None);
        let page = dev.finish();
        assert_eq!(page.text, "fi");
        assert!((page.boxes[0].x1 - 3.0).abs() < 1e-3);
        assert!((page.boxes[1].x0 - 3.0).abs() < 1e-3);
    }
}

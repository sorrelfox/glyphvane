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
//! - **Order.** The content stream's, except on a page in two columns
//!   ([`order_columns`]): there each column is read top to bottom, the
//!   left one first, and text across both (a title, a full-width figure,
//!   a page number) comes where it is between them.
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

mod columns;

use columns::order_columns;

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
    /// Whether each character runs left to right along the page (not
    /// rotated).
    upright: Vec<bool>,
    last: Option<Last>,
}

impl TextDevice {
    fn full(&self) -> bool {
        self.boxes.len() >= MAX_CHARS
    }

    fn push_char(&mut self, c: char, b: TextBox, upright: bool) {
        if !self.full() {
            self.text.push(c);
            self.boxes.push(b);
            self.upright.push(upright);
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
            let last_upright = is_upright(last.dir);
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
                    self.push_char('\n', gap_box(last.end, last.end, last.em), last_upright);
                }
            } else if (ahead > SPACE_GAP * last.em.min(em) || ahead < -2.0 * last.em)
                && !self.ends_with_space()
                && !chars.first().is_some_and(|c| c.is_whitespace())
            {
                self.push_char(' ', gap_box(last.end, origin, last.em), last_upright);
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
        let upright = is_upright(dir);
        for (i, c) in chars.into_iter().enumerate() {
            let x0 = adv * i as f64 / n;
            let x1 = adv * (i + 1) as f64 / n;
            let corners = [
                t * Point::new(x0, -DESCENT * UNITS),
                t * Point::new(x1, -DESCENT * UNITS),
                t * Point::new(x0, ASCENT * UNITS),
                t * Point::new(x1, ASCENT * UNITS),
            ];
            self.push_char(c, bbox(&corners, Vec2::ZERO, Vec2::ZERO), upright);
        }
    }

    fn finish(mut self) -> PageText {
        while self.text.ends_with(['\n', ' ']) {
            self.text.pop();
            self.boxes.pop();
            self.upright.pop();
        }
        match order_columns(&self.text, &self.boxes, &self.upright) {
            Some((text, boxes)) => PageText { text, boxes },
            None => PageText {
                text: self.text,
                boxes: self.boxes,
            },
        }
    }
}

/// Whether a baseline along `dir` runs left to right on the page.
fn is_upright(dir: Vec2) -> bool {
    dir.x > 0.99 && dir.y.abs() < 0.1
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

    /// Draws `s` from (`x`, `y`) at 10 pt, each character (spaces too) a
    /// glyph 5 pt wide.
    fn type_at(dev: &mut TextDevice, x: f64, y: f64, s: &str) {
        for (k, c) in s.chars().enumerate() {
            glyph_at(dev, x + 5.0 * k as f64, y, 10.0, c);
        }
    }

    const TITLE: &str = "A title that runs across both of the columns of this page";

    fn left(k: usize) -> String {
        format!("left {k:02} lorem ipsum dolor sit amet ok")
    }

    fn right(k: usize) -> String {
        format!("right {k:02} lorem ipsum dolor sit amet")
    }

    /// A title, 30 rows in two columns (x 50 to 235 and 300 to 485, a
    /// gutter of 6.5 em) and a centred page number, drawn row by row or
    /// column by column.
    fn two_columns(by_rows: bool) -> PageText {
        let mut dev = TextDevice::default();
        type_at(&mut dev, 50.0, 40.0, TITLE);
        let y = |k: usize| 80.0 + 20.0 * k as f64;
        if by_rows {
            for k in 0..30 {
                type_at(&mut dev, 50.0, y(k), &left(k));
                type_at(&mut dev, 300.0, y(k), &right(k));
            }
        } else {
            for k in 0..30 {
                type_at(&mut dev, 50.0, y(k), &left(k));
            }
            for k in 0..30 {
                type_at(&mut dev, 300.0, y(k), &right(k));
            }
        }
        type_at(&mut dev, 268.0, 720.0, "7");
        dev.finish()
    }

    fn reading_order() -> String {
        let mut lines = vec![TITLE.to_owned()];
        lines.extend((0..30).map(left));
        lines.extend((0..30).map(right));
        lines.push("7".to_owned());
        lines.join("\n")
    }

    #[test]
    fn two_columns_are_read_one_after_the_other() {
        for by_rows in [true, false] {
            let page = two_columns(by_rows);
            assert_eq!(page.text, reading_order(), "drawn by rows: {by_rows}");
            assert_eq!(page.boxes.len(), page.text.chars().count());
            // Each character keeps its box: "right 00" starts the right
            // column.
            let byte = page.text.find("right 00").expect("the right column");
            let at = page.text[..byte].chars().count();
            let r = page.boxes[at];
            assert!(
                (r.x0 - 300.0).abs() < 1e-3 && (r.y1 - 82.0).abs() < 1e-3,
                "{r:?}"
            );
            // A line end between pieces has no width.
            let end = page.boxes[TITLE.chars().count()];
            assert!((end.x1 - end.x0).abs() < 1e-6, "{end:?}");
        }
    }

    #[test]
    fn text_across_the_columns_mid_page_is_read_between_them() {
        let mut dev = TextDevice::default();
        let y = |k: usize| 80.0 + 20.0 * k as f64;
        for k in (0..10).chain(20..30) {
            type_at(&mut dev, 50.0, y(k), &left(k));
            type_at(&mut dev, 300.0, y(k), &right(k));
        }
        // A full-width caption in the middle, drawn last.
        type_at(&mut dev, 50.0, y(15), TITLE);
        let page = dev.finish();
        let mut lines: Vec<String> = (0..10).map(left).collect();
        lines.extend((0..10).map(right));
        lines.push(TITLE.to_owned());
        lines.extend((20..30).map(left));
        lines.extend((20..30).map(right));
        assert_eq!(page.text, lines.join("\n"));
    }

    #[test]
    fn a_page_in_one_column_keeps_the_order_drawn() {
        // Prose across the page, with a centred page number.
        let mut dev = TextDevice::default();
        let mut lines = Vec::new();
        for k in 0..30 {
            let words = ["lorem", "ipsum", "dolor", "sit", "amet", "consectetur"];
            let line: Vec<&str> = (0..14).map(|w| words[(w * 7 + k) % words.len()]).collect();
            let line = line.join(" ");
            type_at(&mut dev, 50.0, 80.0 + 20.0 * k as f64, &line);
            lines.push(line);
        }
        type_at(&mut dev, 268.0, 720.0, "7");
        lines.push("7".to_owned());
        assert_eq!(dev.finish().text, lines.join("\n"));

        // A table of short keys and long values is not two columns.
        let mut dev = TextDevice::default();
        let mut rows = Vec::new();
        for k in 0..30 {
            let key = ["a", "key", "bb", "name"][k % 4];
            let value = right(k);
            type_at(&mut dev, 50.0, 80.0 + 20.0 * k as f64, key);
            type_at(&mut dev, 300.0, 80.0 + 20.0 * k as f64, &value);
            rows.push(format!("{key} {value}"));
        }
        assert_eq!(dev.finish().text, rows.join("\n"));
    }

    #[test]
    fn rotated_text_comes_after_the_columns() {
        let mut dev = TextDevice::default();
        // A note up the left margin, drawn first.
        for (k, c) in "margin".chars().enumerate() {
            let t = Affine::new([0.0, -0.01, -0.01, 0.0, 20.0, 600.0 - 5.0 * k as f64]);
            dev.glyph(t, 500.0, Some(BfString::Char(c)));
        }
        for k in 0..30 {
            let y = 80.0 + 20.0 * k as f64;
            type_at(&mut dev, 50.0, y, &left(k));
            type_at(&mut dev, 300.0, y, &right(k));
        }
        let page = dev.finish();
        assert!(page.text.starts_with("left 00"), "{}", page.text);
        assert!(page.text.ends_with("sit amet\nmargin"), "{}", page.text);
        assert_eq!(page.boxes.len(), page.text.chars().count());
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

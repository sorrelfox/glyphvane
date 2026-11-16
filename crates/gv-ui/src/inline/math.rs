// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Painting a `gv-math` layout as GPUI paths.

use gpui::{
    App, FillOptions, Font, Hsla, PathBuilder, PathStyle, Pixels, Point, SharedString, TextAlign,
    TextRun, Window, point, px,
};
use gv_math::{MathColor, MathItem, MathLayout, MathPath, PathEl};

/// Adds `path`, scaled by `scale` px per unit and moved to `(dx, dy)`.
fn add(b: &mut PathBuilder, path: &MathPath, dx: f32, dy: f32, scale: f32) {
    let p = |x: f32, y: f32| point(px(dx + x * scale), px(dy + y * scale));
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(x, y) => b.move_to(p(x, y)),
            PathEl::LineTo(x, y) => b.line_to(p(x, y)),
            PathEl::QuadTo(x1, y1, x, y) => b.curve_to(p(x, y), p(x1, y1)),
            PathEl::CubicTo(x1, y1, x2, y2, x, y) => {
                b.cubic_bezier_to(p(x, y), p(x1, y1), p(x2, y2));
            }
            PathEl::Close => b.close(),
        }
    }
}

fn color(c: MathColor, current: Hsla) -> Hsla {
    match c {
        MathColor::Current => current,
        MathColor::Rgba([r, g, b, a]) => gpui::Rgba { r, g, b, a }.into(),
    }
}

/// Paints `layout` with its box's top left corner at `origin`, `em` px per
/// em; [`MathColor::Current`] is `current`. Characters outside the KaTeX
/// fonts are shaped with `text_font`.
pub(crate) fn paint_math(
    window: &mut Window,
    cx: &mut App,
    layout: &MathLayout,
    origin: Point<Pixels>,
    em: f32,
    current: Hsla,
    text_font: &Font,
) {
    let (ox, oy) = (f32::from(origin.x), f32::from(origin.y));
    for item in &layout.items {
        match item {
            MathItem::Fill {
                path,
                x,
                y,
                scale,
                color: c,
            } => {
                // Glyph outlines need the non-zero rule (lyon's default is
                // even-odd).
                let mut b =
                    PathBuilder::fill().with_style(PathStyle::Fill(FillOptions::non_zero()));
                add(&mut b, path, ox + x * em, oy + y * em, scale * em);
                if let Ok(p) = b.build() {
                    window.paint_path(p, color(*c, current));
                }
            }
            MathItem::Stroke {
                path,
                x,
                y,
                width,
                color: c,
            } => {
                let mut b = PathBuilder::stroke(px((width * em).max(1.0)));
                add(&mut b, path, ox + x * em, oy + y * em, em);
                if let Ok(p) = b.build() {
                    window.paint_path(p, color(*c, current));
                }
            }
            MathItem::Text {
                ch,
                x,
                y,
                size,
                color: c,
            } => {
                let size = px(size * em);
                let text: SharedString = ch.to_string().into();
                let run = TextRun {
                    len: text.len(),
                    font: text_font.clone(),
                    color: color(*c, current),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(text, size, &[run], None);
                // GPUI centres the glyphs in the line height: a line one em
                // high puts the baseline about 0.8 em below its top.
                let top = point(px(ox + x * em), px(oy + y * em) - size * 0.8);
                let _ = line.paint(top, size, TextAlign::Left, None, window, cx);
            }
        }
    }
}

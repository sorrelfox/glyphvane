// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Formulas as SVG: RaTeX's layout drawn as paths in em units, so the
//! export needs no math font. Characters outside the KaTeX fonts stay
//! `<text>` in the reader's fonts.

use gv_math::{Limits, MacroSet, MathColor, MathItem, MathLayout, MathPath, PathEl, typeset};

use crate::escape::{escaped, num};

/// The SVG of `tex`, or the error message.
pub(crate) fn svg(tex: &str, display: bool, macros: &MacroSet) -> Result<String, String> {
    svg_box(tex, display, macros).map(|b| b.svg)
}

/// A formula's SVG and the size of its box, in em.
#[derive(Debug, Clone)]
pub(crate) struct SvgBox {
    pub svg: String,
    pub height: f32,
    /// How far the box reaches below the baseline.
    pub depth: f32,
}

/// The SVG of `tex` with its box, or the error message.
pub(crate) fn svg_box(tex: &str, display: bool, macros: &MacroSet) -> Result<SvgBox, String> {
    let layout =
        typeset(tex, display, macros, &Limits::default(), None).map_err(|e| e.to_string())?;
    let (_, _, _, height, depth) = bounds(&layout);
    Ok(SvgBox {
        svg: layout_svg(&layout, tex),
        height,
        depth,
    })
}

/// The ink-inclusive box of `l`: left, top, width, height, and the depth
/// below the baseline.
fn bounds(l: &MathLayout) -> (f32, f32, f32, f32, f32) {
    let (mut x0, mut x1) = (0.0f32, l.width);
    let (mut y0, mut y1) = (0.0f32, l.total_height());
    if let Some(ink) = l.ink {
        x0 = x0.min(ink.min_x);
        x1 = x1.max(ink.max_x);
        y0 = y0.min(ink.min_y);
        y1 = y1.max(ink.max_y);
    }
    let (w, h) = ((x1 - x0).max(0.0), (y1 - y0).max(0.0));
    // The baseline sits at `l.height` from the top of the box.
    (x0, y0, w, h, y1 - l.height)
}

fn color(c: MathColor) -> String {
    match c {
        MathColor::Current => "currentColor".to_owned(),
        MathColor::Rgba([r, g, b, a]) => {
            let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            if a >= 1.0 {
                format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
            } else {
                format!(
                    "rgba({},{},{},{})",
                    byte(r),
                    byte(g),
                    byte(b),
                    num(a.clamp(0.0, 1.0))
                )
            }
        }
    }
}

/// `path` as SVG path data, scaled by `s` and moved by `(dx, dy)`.
fn path_data(path: &MathPath, dx: f32, dy: f32, s: f32) -> String {
    let p = |x: f32, y: f32| format!("{} {}", num(dx + x * s), num(dy + y * s));
    let mut d = String::new();
    for el in path.elements() {
        if !d.is_empty() {
            d.push(' ');
        }
        match *el {
            PathEl::MoveTo(x, y) => d.push_str(&format!("M{}", p(x, y))),
            PathEl::LineTo(x, y) => d.push_str(&format!("L{}", p(x, y))),
            PathEl::QuadTo(a, b, x, y) => d.push_str(&format!("Q{} {}", p(a, b), p(x, y))),
            PathEl::CubicTo(a, b, c, e, x, y) => {
                d.push_str(&format!("C{} {} {}", p(a, b), p(c, e), p(x, y)));
            }
            PathEl::Close => d.push('Z'),
        }
    }
    d
}

pub(crate) fn layout_svg(l: &MathLayout, tex: &str) -> String {
    let (x0, y0, w, h, below) = bounds(l);
    let mut s = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"gv-math-svg\" role=\"img\" \
         aria-label=\"{}\" viewBox=\"{} {} {} {}\" width=\"{}em\" height=\"{}em\" \
         style=\"vertical-align:{}em\">",
        escaped(tex),
        num(x0),
        num(y0),
        num(w),
        num(h),
        num(w),
        num(h),
        num(-below),
    );
    for item in &l.items {
        match item {
            MathItem::Fill {
                path,
                x,
                y,
                scale,
                color: c,
            } => {
                s.push_str(&format!(
                    "<path d=\"{}\" fill=\"{}\"/>",
                    path_data(path, *x, *y, *scale),
                    color(*c)
                ));
            }
            MathItem::Stroke {
                path,
                x,
                y,
                width,
                color: c,
            } => {
                s.push_str(&format!(
                    "<path d=\"{}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{}\" \
                     stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
                    path_data(path, *x, *y, 1.0),
                    color(*c),
                    num(*width)
                ));
            }
            MathItem::Text {
                ch,
                x,
                y,
                size,
                color: c,
            } => {
                s.push_str(&format!(
                    "<text x=\"{}\" y=\"{}\" font-size=\"{}\" fill=\"{}\">{}</text>",
                    num(*x),
                    num(*y),
                    num(*size),
                    color(*c),
                    escaped(&ch.to_string())
                ));
            }
        }
    }
    s.push_str("</svg>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_become_paths_in_the_current_colour() {
        let svg = svg("x^2", false, &MacroSet::empty()).expect("typesets");
        assert!(svg.starts_with("<svg "));
        assert!(svg.contains("<path d=\"M"));
        assert!(svg.contains("fill=\"currentColor\""));
        assert!(svg.contains("aria-label=\"x^2\""));
        assert!(!svg.contains("<text"), "KaTeX glyphs are paths");
        assert_eq!(svg, super::svg("x^2", false, &MacroSet::empty()).unwrap());
    }

    #[test]
    fn errors_are_messages() {
        assert!(svg("\\frac{", true, &MacroSet::empty()).is_err());
    }

    #[test]
    fn labels_are_escaped() {
        let svg = svg("a<b", false, &MacroSet::empty()).expect("typesets");
        assert!(svg.contains("aria-label=\"a&lt;b\""));
    }
}

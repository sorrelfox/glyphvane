// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Golden tests: layout boxes (exact, as text) and CPU rasters (PNG, with
//! a tolerance), so a RaTeX or font bump that changes output shows up.
//!
//! `GV_UPDATE_GOLDEN=1 cargo test -p gv-math --test golden` rewrites the
//! files under `tests/golden/`; look at the PNGs before committing them.

use std::fmt::Write as _;
use std::path::PathBuf;

use gv_math::{MacroSet, MathColor, MathItem, MathLayout, MathPath, PathEl, document_macros};
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform,
};

/// Pixels per em of the rasters.
const PPEM: f32 = 32.0;
/// Padding around the ink, in pixels.
const PAD: f32 = 4.0;

struct Case {
    name: &'static str,
    tex: &'static str,
    display: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "fraction",
        tex: r"\frac{a+b}{\sqrt{c^2+d^2}}",
        display: true,
    },
    Case {
        name: "sum_display",
        tex: r"\sum_{i=1}^{n} i^2 = \frac{n(n+1)(2n+1)}{6}",
        display: true,
    },
    Case {
        name: "sum_inline",
        tex: r"\sum_{i=1}^{n} i^2",
        display: false,
    },
    Case {
        name: "integral",
        tex: r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
        display: true,
    },
    Case {
        name: "matrix",
        tex: r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
        display: true,
    },
    Case {
        name: "cases",
        tex: r"f(x)=\begin{cases} x & x\ge 0 \\ -x & x<0 \end{cases}",
        display: true,
    },
    Case {
        name: "delimiters",
        tex: r"\left( \frac{1}{2} \right) \Bigg[ x \Bigg]",
        display: true,
    },
    Case {
        name: "accents",
        tex: r"\hat{x} + \widehat{xyz} + \vec{v} + \overline{AB}",
        display: false,
    },
    Case {
        name: "colors",
        tex: r"a + \color{red} b + \colorbox{yellow}{c}",
        display: false,
    },
    Case {
        name: "mhchem",
        tex: r"\ce{2H2 + O2 -> 2H2O}",
        display: true,
    },
    Case {
        name: "text_cyrillic",
        tex: r"x \text{ где } y",
        display: false,
    },
    Case {
        name: "dashed_array",
        tex: r"\begin{array}{c} a \\ \hdashline b \end{array}",
        display: true,
    },
];

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn update() -> bool {
    std::env::var_os("GV_UPDATE_GOLDEN").is_some_and(|v| v == "1")
}

fn render(case: &Case, macros: &MacroSet) -> MathLayout {
    gv_math::typeset(
        case.tex,
        case.display,
        macros,
        &gv_math::Limits::default(),
        None,
    )
    .unwrap_or_else(|e| panic!("{}: {e}", case.name))
}

fn path(p: &MathPath, dx: f32, dy: f32, s: f32) -> Option<tiny_skia::Path> {
    let mut b = PathBuilder::new();
    for el in p.elements() {
        match *el {
            PathEl::MoveTo(x, y) => b.move_to(dx + x * s, dy + y * s),
            PathEl::LineTo(x, y) => b.line_to(dx + x * s, dy + y * s),
            PathEl::QuadTo(x1, y1, x, y) => {
                b.quad_to(dx + x1 * s, dy + y1 * s, dx + x * s, dy + y * s);
            }
            PathEl::CubicTo(x1, y1, x2, y2, x, y) => b.cubic_to(
                dx + x1 * s,
                dy + y1 * s,
                dx + x2 * s,
                dy + y2 * s,
                dx + x * s,
                dy + y * s,
            ),
            PathEl::Close => b.close(),
        }
    }
    b.finish()
}

fn paint(color: MathColor) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(match color {
        MathColor::Current => Color::BLACK,
        MathColor::Rgba([r, g, b, a]) => Color::from_rgba(r, g, b, a).unwrap_or(Color::BLACK),
    });
    p
}

/// Rasterizes a layout on white; host-font text items become grey boxes
/// (the golden only checks where they go).
fn raster(l: &MathLayout) -> Pixmap {
    let ink = l.ink.expect("ink");
    let min_x = ink.min_x.min(0.0);
    let min_y = ink.min_y.min(0.0);
    let max_x = ink.max_x.max(l.width);
    let max_y = ink.max_y.max(l.total_height());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (w, h) = (
        ((max_x - min_x) * PPEM + 2.0 * PAD).ceil() as u32,
        ((max_y - min_y) * PPEM + 2.0 * PAD).ceil() as u32,
    );
    let mut pm = Pixmap::new(w, h).expect("pixmap");
    pm.fill(Color::WHITE);
    let ox = PAD - min_x * PPEM;
    let oy = PAD - min_y * PPEM;
    for item in &l.items {
        match item {
            MathItem::Fill {
                path: p,
                x,
                y,
                scale,
                color,
            } => {
                if let Some(p) = path(p, ox + x * PPEM, oy + y * PPEM, scale * PPEM) {
                    pm.fill_path(
                        &p,
                        &paint(*color),
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }
            MathItem::Stroke {
                path: p,
                x,
                y,
                width,
                color,
            } => {
                if let Some(p) = path(p, ox + x * PPEM, oy + y * PPEM, PPEM) {
                    let stroke = Stroke {
                        width: width * PPEM,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        ..Stroke::default()
                    };
                    pm.stroke_path(&p, &paint(*color), &stroke, Transform::identity(), None);
                }
            }
            MathItem::Text { x, y, size, .. } => {
                let r = tiny_skia::Rect::from_xywh(
                    ox + x * PPEM,
                    oy + (y - size * 0.7) * PPEM,
                    size * 0.6 * PPEM,
                    size * 0.7 * PPEM,
                );
                if let Some(r) = r {
                    let mut p = Paint::default();
                    p.set_color(Color::from_rgba8(160, 160, 160, 255));
                    pm.fill_rect(r, &p, Transform::identity(), None);
                }
            }
        }
    }
    pm
}

/// Share of pixels whose channels differ by more than 48 (anti-aliasing
/// noise across rasterizer versions stays far below that).
fn differing(a: &Pixmap, b: &Pixmap) -> f64 {
    let bad = a
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.data().as_chunks::<4>().0)
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 48))
        .count();
    #[allow(clippy::cast_precision_loss)]
    let share = bad as f64 / (a.width() * a.height()).max(1) as f64;
    share
}

#[test]
fn layout_boxes_match_the_golden_file() {
    let mut out = String::new();
    let macros = MacroSet::empty();
    for case in CASES {
        let l = render(case, &macros);
        let fills = l
            .items
            .iter()
            .filter(|i| matches!(i, MathItem::Fill { .. }))
            .count();
        let texts = l
            .items
            .iter()
            .filter(|i| matches!(i, MathItem::Text { .. }))
            .count();
        let _ = writeln!(
            out,
            "{:<14} w={:.3} h={:.3} d={:.3} fills={fills} strokes={} texts={texts}",
            case.name,
            l.width,
            l.height,
            l.depth,
            l.items.len() - fills - texts,
        );
    }
    let file = dir().join("boxes.txt");
    if update() {
        std::fs::write(&file, &out).expect("write boxes");
        return;
    }
    let want = std::fs::read_to_string(&file).expect("tests/golden/boxes.txt (GV_UPDATE_GOLDEN=1)");
    assert_eq!(
        out, want,
        "layout boxes changed; rerun with GV_UPDATE_GOLDEN=1 and review"
    );
}

#[test]
fn rasters_match_the_golden_images() {
    let macros = MacroSet::empty();
    let mut failures = Vec::new();
    for case in CASES {
        let pm = raster(&render(case, &macros));
        let file = dir().join(format!("{}.png", case.name));
        if update() {
            pm.save_png(&file).expect("save png");
            continue;
        }
        let want = Pixmap::load_png(&file)
            .unwrap_or_else(|e| panic!("{}: {e} (GV_UPDATE_GOLDEN=1)", file.display()));
        if (want.width(), want.height()) != (pm.width(), pm.height()) {
            failures.push(format!(
                "{}: size {}x{} != golden {}x{}",
                case.name,
                pm.width(),
                pm.height(),
                want.width(),
                want.height()
            ));
            continue;
        }
        let share = differing(&pm, &want);
        if share > 0.005 {
            failures.push(format!(
                "{}: {:.2}% of pixels differ",
                case.name,
                share * 100.0
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn document_macros_render_like_their_expansion() {
    let sets = document_macros([r"\newcommand{\norm}[1]{\left\|#1\right\|}", r"\norm{x}"]);
    let with_macro = Case {
        name: "macro",
        tex: r"\norm{x}",
        display: true,
    };
    let expanded = Case {
        name: "expanded",
        tex: r"\left\|x\right\|",
        display: true,
    };
    let a = raster(&render(&with_macro, &sets[1]));
    let b = raster(&render(&expanded, &MacroSet::empty()));
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    assert!(differing(&a, &b) == 0.0);
}

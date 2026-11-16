// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The rendered diagram: filled paths in CSS pixels, painted with roles.

use std::fmt::Write as _;

use crate::palette::{Palette, Role};

/// One element of a [`DiagramPath`]. Coordinates are CSS pixels, y down,
/// from the diagram's top left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathEl {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    /// Quadratic Bézier: control point, end point.
    QuadTo(f32, f32, f32, f32),
    /// Cubic Bézier: two control points, end point.
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// An outline to fill. Strokes are already converted to outlines (dashes
/// applied), so a host only ever fills.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiagramPath {
    pub(crate) els: Vec<PathEl>,
}

impl DiagramPath {
    /// The elements in order.
    pub fn elements(&self) -> &[PathEl] {
        &self.els
    }

    pub fn is_empty(&self) -> bool {
        self.els.is_empty()
    }
}

/// How a path is filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// The colour of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Paint {
    /// A theme role, resolved by the host at paint time.
    Role(Role),
    /// A literal colour from the source (`classDef`, `style`, C4's own
    /// palette) or one merman derived itself: straight sRGB.
    Rgb([u8; 3]),
}

/// An axis-aligned rectangle in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One drawing item, in paint order.
#[derive(Debug, Clone, PartialEq)]
pub struct DiagramItem {
    pub path: DiagramPath,
    pub rule: FillRule,
    pub paint: Paint,
    /// Opacity `0.0..=1.0`: the fill or stroke opacity times the opacity of
    /// every enclosing group.
    pub opacity: f32,
    /// A rectangular clip, when the source clipped this item.
    pub clip: Option<Rect>,
}

/// A rendered Mermaid diagram, independent of theme and scale.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagram {
    /// The diagram type merman detected (`flowchart`, `sequence`, `pie`,
    /// ...).
    pub family: String,
    /// Size in CSS pixels at zoom 1.
    pub width: f32,
    pub height: f32,
    /// What is drawn, in paint order. The ground is not included: the host
    /// paints [`Role::Background`] (or nothing) under it.
    pub items: Vec<DiagramItem>,
}

impl Diagram {
    /// An estimate of the memory held, for cache budgets.
    pub fn approx_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.family.len()
            + self
                .items
                .iter()
                .map(|i| {
                    std::mem::size_of::<DiagramItem>()
                        + i.path.els.len() * std::mem::size_of::<PathEl>()
                })
                .sum::<usize>()
    }

    /// The literal colours the diagram uses (each once), for tests and
    /// diagnostics: author colours, and any colour merman derived.
    pub fn literal_colors(&self) -> Vec<[u8; 3]> {
        let mut out: Vec<[u8; 3]> = Vec::new();
        for item in &self.items {
            if let Paint::Rgb(c) = item.paint
                && !out.contains(&c)
            {
                out.push(c);
            }
        }
        out
    }

    /// The colour of `paint` under `palette`, straight RGBA.
    pub fn resolve(paint: Paint, palette: &Palette) -> [u8; 4] {
        match paint {
            Paint::Role(r) => palette.get(r),
            Paint::Rgb([r, g, b]) => [r, g, b, 0xFF],
        }
    }

    /// A standalone SVG of the diagram in `palette`'s colours, with text
    /// as outlines: for export, copy and print. `background` fills the
    /// whole canvas first when given.
    pub fn to_svg(&self, palette: &Palette, background: bool) -> String {
        let mut s = String::with_capacity(256 + self.items.len() * 128);
        let _ = write!(
            s,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#,
            w = fmt_num(self.width),
            h = fmt_num(self.height),
        );
        if background {
            let [r, g, b, a] = palette.get(Role::Background);
            let _ = write!(
                s,
                r##"<rect width="100%" height="100%" fill="#{r:02x}{g:02x}{b:02x}"{}/>"##,
                opacity_attr("fill-opacity", f32::from(a) / 255.0),
            );
        }
        let mut clips = 0usize;
        for item in &self.items {
            let [r, g, b, a] = Self::resolve(item.paint, palette);
            let mut clip_ref = String::new();
            if let Some(c) = item.clip {
                clips += 1;
                let _ = write!(
                    s,
                    r#"<clipPath id="c{clips}"><rect x="{}" y="{}" width="{}" height="{}"/></clipPath>"#,
                    fmt_num(c.x),
                    fmt_num(c.y),
                    fmt_num(c.width),
                    fmt_num(c.height),
                );
                clip_ref = format!(r#" clip-path="url(#c{clips})""#);
            }
            let _ = write!(
                s,
                r##"<path d="{}" fill="#{r:02x}{g:02x}{b:02x}"{}{}{clip_ref}/>"##,
                path_data(&item.path),
                opacity_attr("fill-opacity", item.opacity * f32::from(a) / 255.0),
                if item.rule == FillRule::EvenOdd {
                    r#" fill-rule="evenodd""#
                } else {
                    ""
                },
            );
        }
        s.push_str("</svg>");
        s
    }
}

fn opacity_attr(name: &str, o: f32) -> String {
    if o >= 0.999 {
        String::new()
    } else {
        format!(r#" {name}="{}""#, fmt_num(o))
    }
}

fn fmt_num(v: f32) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

fn path_data(p: &DiagramPath) -> String {
    let mut d = String::with_capacity(p.els.len() * 16);
    for el in &p.els {
        match *el {
            PathEl::MoveTo(x, y) => {
                let _ = write!(d, "M{} {}", fmt_num(x), fmt_num(y));
            }
            PathEl::LineTo(x, y) => {
                let _ = write!(d, "L{} {}", fmt_num(x), fmt_num(y));
            }
            PathEl::QuadTo(x1, y1, x, y) => {
                let _ = write!(
                    d,
                    "Q{} {} {} {}",
                    fmt_num(x1),
                    fmt_num(y1),
                    fmt_num(x),
                    fmt_num(y)
                );
            }
            PathEl::CubicTo(x1, y1, x2, y2, x, y) => {
                let _ = write!(
                    d,
                    "C{} {} {} {} {} {}",
                    fmt_num(x1),
                    fmt_num(y1),
                    fmt_num(x2),
                    fmt_num(y2),
                    fmt_num(x),
                    fmt_num(y)
                );
            }
            PathEl::Close => d.push('Z'),
        }
    }
    d
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The laid-out formula: drawing items in em units, ready to paint.

use std::sync::Arc;

use ratex_types::color::Color;
use ratex_types::display_item::{DisplayItem, DisplayList};
use ratex_types::path_command::PathCommand;

use crate::fonts;
use crate::path::{Bounds, MathPath, PathBuilder};

/// The colour of a drawing item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MathColor {
    /// The surrounding text colour (a theme token), so a cached layout
    /// works in every theme.
    Current,
    /// An explicit colour from the source (`\color`, `\textcolor`,
    /// `\colorbox`): straight RGBA, each in `0.0..=1.0`.
    Rgba([f32; 4]),
}

/// Stroke width of unfilled paths, in em (RaTeX's SVG writer uses 1.5
/// units at 40 per em).
const STROKE_EM: f32 = 1.5 / 40.0;

/// One drawing item. Coordinates are in em, y down, relative to the top
/// left corner of the formula box: the baseline is at
/// `y = `[`MathLayout::height`].
#[derive(Debug, Clone, PartialEq)]
pub enum MathItem {
    /// Fill `path` (non-zero rule), scaled by `scale` and moved to
    /// `(x, y)`. Glyph outlines are shared between formulas.
    Fill {
        path: Arc<MathPath>,
        x: f32,
        y: f32,
        scale: f32,
        color: MathColor,
    },
    /// Stroke `path`, moved to `(x, y)`, with a pen `width` em wide (round
    /// caps and joins).
    Stroke {
        path: Arc<MathPath>,
        x: f32,
        y: f32,
        width: f32,
        color: MathColor,
    },
    /// A character outside the bundled KaTeX fonts (CJK, Cyrillic or emoji
    /// in `\text{}`): the host draws it with its own font stack, its
    /// baseline origin at `(x, y)`, `size` em high.
    Text {
        ch: char,
        x: f32,
        y: f32,
        size: f32,
        color: MathColor,
    },
}

/// A formula laid out by RaTeX and converted to paths.
#[derive(Debug, Clone, PartialEq)]
pub struct MathLayout {
    /// Display (block) or text (inline) style.
    pub display: bool,
    /// Advance width, in em.
    pub width: f32,
    /// Height above the baseline, in em.
    pub height: f32,
    /// Depth below the baseline, in em.
    pub depth: f32,
    /// What is drawn, in paint order.
    pub items: Vec<MathItem>,
    /// The ink box of every item (may reach outside `0..width`, for example
    /// italic overhangs), or `None` when nothing is drawn.
    pub ink: Option<Bounds>,
}

impl MathLayout {
    /// The height of the box (height plus depth), in em.
    pub fn total_height(&self) -> f32 {
        self.height + self.depth
    }

    /// Whether some item needs the host's font stack ([`MathItem::Text`]).
    pub fn has_fallback_text(&self) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i, MathItem::Text { .. }))
    }

    /// An estimate of the memory the layout holds, for cache budgets.
    /// Shared glyph outlines are not counted.
    pub fn approx_bytes(&self) -> usize {
        let owned: usize = self
            .items
            .iter()
            .map(|i| match i {
                // Glyph outlines are also held by the outline cache; rules
                // and delimiters own their paths.
                MathItem::Fill { path, .. } | MathItem::Stroke { path, .. }
                    if Arc::strong_count(path) == 1 =>
                {
                    std::mem::size_of_val(path.elements())
                }
                _ => 0,
            })
            .sum();
        std::mem::size_of::<Self>() + self.items.len() * std::mem::size_of::<MathItem>() + owned
    }
}

/// The colour RaTeX is given as the surrounding colour. Items in this exact
/// colour become [`MathColor::Current`]. A user colour equal to it in f32
/// would need an unlikely 6-digit hex; that one would follow the theme too.
pub(crate) const CURRENT: Color = Color::rgb(0.012_345, 0.0, 0.054_321);

fn color(c: &Color) -> MathColor {
    if *c == CURRENT {
        MathColor::Current
    } else {
        let clamp = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        MathColor::Rgba([clamp(c.r), clamp(c.g), clamp(c.b), clamp(c.a)])
    }
}

/// Lengths from RaTeX are f64; anything non-finite becomes 0.
fn f(v: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let v = v as f32;
    if v.is_finite() { v } else { 0.0 }
}

/// Converts RaTeX's display list into a [`MathLayout`].
pub(crate) fn convert(list: &DisplayList, display: bool) -> MathLayout {
    let mut items = Vec::with_capacity(list.items.len());
    for item in &list.items {
        match item {
            DisplayItem::GlyphPath {
                x,
                y,
                scale,
                font,
                char_code,
                color: c,
            } => {
                let Some(ch) = char::from_u32(*char_code) else {
                    continue;
                };
                let outline = fonts::face_index(font).and_then(|face| fonts::glyph(face, ch));
                match outline {
                    Some(path) if path.is_empty() => {}
                    Some(path) => items.push(MathItem::Fill {
                        path,
                        x: f(*x),
                        y: f(*y),
                        scale: f(*scale),
                        color: color(c),
                    }),
                    None => items.push(MathItem::Text {
                        ch,
                        x: f(*x),
                        y: f(*y),
                        size: f(*scale),
                        color: color(c),
                    }),
                }
            }
            DisplayItem::Line {
                x,
                y,
                width,
                thickness,
                color: c,
                dashed,
            } => {
                let (x, y, w, t) = (f(*x), f(*y), f(*width), f(*thickness).max(0.0));
                if *dashed {
                    // Dashes and gaps three thicknesses long, as RaTeX's SVG.
                    let dash = (t * 3.0).max(0.01);
                    let mut b = PathBuilder::default();
                    let mut at = 0.0;
                    while at < w {
                        let len = dash.min(w - at);
                        rect(&mut b, x + at, y - t / 2.0, len, t);
                        at += dash * 2.0;
                    }
                    push_fill(&mut items, b.finish(), color(c));
                } else {
                    push_fill(&mut items, MathPath::rect(x, y - t / 2.0, w, t), color(c));
                }
            }
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color: c,
            } => push_fill(
                &mut items,
                MathPath::rect(f(*x), f(*y), f(*width), f(*height)),
                color(c),
            ),
            DisplayItem::Path {
                x,
                y,
                commands,
                fill,
                color: c,
            } => {
                let path = Arc::new(commands_path(commands));
                if path.is_empty() {
                    continue;
                }
                let (x, y, color) = (f(*x), f(*y), color(c));
                items.push(if *fill {
                    MathItem::Fill {
                        path,
                        x,
                        y,
                        scale: 1.0,
                        color,
                    }
                } else {
                    MathItem::Stroke {
                        path,
                        x,
                        y,
                        width: STROKE_EM,
                        color,
                    }
                });
            }
        }
    }
    let ink = items.iter().filter_map(item_bounds).reduce(Bounds::union);
    MathLayout {
        display,
        width: f(list.width).max(0.0),
        height: f(list.height),
        depth: f(list.depth),
        items,
        ink,
    }
}

fn rect(b: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32) {
    b.move_to(x, y);
    b.line_to(x + w, y);
    b.line_to(x + w, y + h);
    b.line_to(x, y + h);
    b.close();
}

fn push_fill(items: &mut Vec<MathItem>, path: MathPath, color: MathColor) {
    if !path.is_empty() {
        items.push(MathItem::Fill {
            path: Arc::new(path),
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            color,
        });
    }
}

fn commands_path(commands: &[PathCommand]) -> MathPath {
    let mut b = PathBuilder::default();
    for c in commands {
        match *c {
            PathCommand::MoveTo { x, y } => b.move_to(f(x), f(y)),
            PathCommand::LineTo { x, y } => b.line_to(f(x), f(y)),
            PathCommand::QuadTo { x1, y1, x, y } => b.quad_to(f(x1), f(y1), f(x), f(y)),
            PathCommand::CubicTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => b.cubic_to(f(x1), f(y1), f(x2), f(y2), f(x), f(y)),
            PathCommand::Close => b.close(),
        }
    }
    b.finish()
}

/// The ink box of one item, in em.
pub(crate) fn item_bounds(item: &MathItem) -> Option<Bounds> {
    match item {
        MathItem::Fill {
            path, x, y, scale, ..
        } => path.bounds().map(|b| b.transformed(*x, *y, *scale)),
        MathItem::Stroke {
            path, x, y, width, ..
        } => path
            .bounds()
            .map(|b| b.transformed(*x, *y, 1.0).inflated(width / 2.0)),
        MathItem::Text { x, y, size, .. } => Some(Bounds {
            min_x: *x,
            min_y: y - size * 0.8,
            max_x: x + size,
            max_y: y + size * 0.2,
        }),
    }
}

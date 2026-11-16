// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Painting a `gv-diagram` display list as GPUI paths, in theme colours.
//!
//! A diagram is theme-agnostic: its items are painted with [`Role`]s, and
//! each role is a design token ([`token_of`]), so a theme change repaints
//! the same cached diagram in new colours without laying anything out.
//! Strokes are already outlines, so every item is a fill (non-zero or
//! even-odd), at the item's opacity, clipped to its rectangle when it has
//! one. Items outside the content mask are skipped.

use gpui::{
    Bounds, ContentMask, FillOptions, Hsla, PathBuilder, PathStyle, Pixels, Point, Window, point,
    px, size,
};
use gv_diagram::{Diagram, DiagramPath, FillRule, Paint, PathEl, Role};

use super::{Palette, Token};

/// The design token of a diagram colour role.
pub(crate) fn token_of(role: Role) -> Token {
    match role {
        Role::Background => Token::SurfacePage,
        Role::NodeFill => Token::AccentTint,
        Role::NodeBorder => Token::AccentDefault,
        Role::SecondaryFill => Token::SurfaceSunken,
        Role::TertiaryFill => Token::SurfacePanel,
        Role::Border => Token::BorderStrong,
        Role::Grid => Token::BorderDefault,
        Role::Text => Token::TextPrimary,
        Role::Line => Token::TextSecondary,
        Role::NoteFill => Token::StatusWarningTint,
        Role::NoteBorder => Token::StatusWarning,
        Role::CriticalFill => Token::StatusErrorTint,
        Role::Critical => Token::StatusError,
        Role::Category(i) => {
            Token::DIAGRAM_CATEGORIES[usize::from(i) % Token::DIAGRAM_CATEGORIES.len()]
        }
    }
}

/// The colour of `paint` in `palette`, at `opacity`.
pub(crate) fn color(paint: Paint, opacity: f32, palette: &Palette) -> Hsla {
    let mut c = match paint {
        Paint::Role(role) => palette.color(token_of(role)),
        Paint::Rgb([r, g, b]) => gpui::Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        }
        .into(),
    };
    c.a *= opacity.clamp(0.0, 1.0);
    c
}

/// The bounding box of a path's points (control points included), in
/// diagram px.
fn extent(path: &DiagramPath) -> Option<(f32, f32, f32, f32)> {
    let mut b: Option<(f32, f32, f32, f32)> = None;
    let mut add = |x: f32, y: f32| {
        b = Some(match b {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    };
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(x, y) | PathEl::LineTo(x, y) => add(x, y),
            PathEl::QuadTo(x1, y1, x, y) => {
                add(x1, y1);
                add(x, y);
            }
            PathEl::CubicTo(x1, y1, x2, y2, x, y) => {
                add(x1, y1);
                add(x2, y2);
                add(x, y);
            }
            PathEl::Close => {}
        }
    }
    b
}

/// Paints `diagram` with its top left corner at `origin`, `scale` px per
/// diagram px (the zoom), in `palette`'s colours. Returns the items
/// painted.
pub(crate) fn paint_diagram(
    window: &mut Window,
    diagram: &Diagram,
    origin: Point<Pixels>,
    scale: f32,
    palette: &Palette,
) -> usize {
    let (ox, oy) = (f32::from(origin.x), f32::from(origin.y));
    let visible = window.content_mask().bounds;
    let p = |x: f32, y: f32| point(px(ox + x * scale), px(oy + y * scale));
    let mut painted = 0;
    for item in &diagram.items {
        if item.path.is_empty() || item.opacity <= 0.0 {
            continue;
        }
        let Some((x0, y0, x1, y1)) = extent(&item.path) else {
            continue;
        };
        let ext = Bounds::new(
            p(x0, y0),
            size(px((x1 - x0) * scale + 1.0), px((y1 - y0) * scale + 1.0)),
        );
        if !ext.intersects(&visible) {
            continue;
        }
        let rule = match item.rule {
            FillRule::NonZero => FillOptions::non_zero(),
            FillRule::EvenOdd => FillOptions::even_odd(),
        };
        let mut b = PathBuilder::fill().with_style(PathStyle::Fill(rule));
        for el in item.path.elements() {
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
        let Ok(path) = b.build() else {
            continue;
        };
        let c = color(item.paint, item.opacity, palette);
        #[cfg(test)]
        tests::record(c);
        match item.clip {
            Some(r) => {
                let clip =
                    Bounds::new(p(r.x, r.y), size(px(r.width * scale), px(r.height * scale)));
                window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                    window.paint_path(path, c);
                });
            }
            None => window.paint_path(path, c),
        }
        painted += 1;
    }
    painted
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::theme::ThemeId;

    thread_local! {
        static PAINTED: RefCell<Vec<Hsla>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(c: Hsla) {
        PAINTED.with(|p| p.borrow_mut().push(c));
    }

    /// The colours of the diagram items painted on this thread since the
    /// last call.
    pub(crate) fn take_painted() -> Vec<Hsla> {
        PAINTED.with(|p| std::mem::take(&mut *p.borrow_mut()))
    }

    #[test]
    fn roles_map_to_their_tokens_and_categories_wrap() {
        assert_eq!(token_of(Role::NodeFill), Token::AccentTint);
        assert_eq!(token_of(Role::NoteBorder), Token::StatusWarning);
        assert_eq!(token_of(Role::Category(0)), Token::DiagramCategory1);
        assert_eq!(token_of(Role::Category(11)), Token::DiagramCategory12);
        assert_eq!(token_of(Role::Category(12)), Token::DiagramCategory1);
        // Every role has a token whose colour differs between Light and
        // Dark, except the literal colours an author wrote.
        let (light, dark) = (Palette::of(ThemeId::Light), Palette::of(ThemeId::Dark));
        for role in Role::all() {
            assert_ne!(
                color(Paint::Role(role), 1.0, &light),
                color(Paint::Role(role), 1.0, &dark),
                "{role:?}"
            );
        }
        let red = Paint::Rgb([255, 0, 0]);
        assert_eq!(color(red, 1.0, &light), color(red, 1.0, &dark));
        assert!((color(red, 0.5, &light).a - 0.5).abs() < 1e-6);
    }
}

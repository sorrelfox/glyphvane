// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Colours and the shapes painted straight to the scene: group boxes and
//! their background images, edges (Bézier strokes and arrowheads) and,
//! for shapes-only previews (`.canvas` file cards, embeds), every node as
//! a block.
//!
//! Colours come from the theme: a preset (`"1"`–`"6"`) is its
//! `canvas.preset.N` token, a hex colour is used as written.

use gpui::{
    Bounds, ContentMask, Corners, Hsla, PathBuilder, Pixels, Point, Window, fill, point, px, size,
};
use gv_canvas::{
    BackgroundStyle, CanvasColor, Detail, EdgeGeometry, ItemId, LodThresholds, NodeKind, Rect,
    Transform,
};

use super::Doc;
use super::cards::BackgroundImage;
use crate::theme::Theme;

/// Lengths of the canvas in world units (they scale with the zoom). Not
/// designed: values close to Obsidian's.
pub(crate) mod world {
    /// Corner radius of cards and groups.
    pub const RADIUS: f64 = 8.0;
    /// Width of an edge's stroke.
    pub const EDGE_WIDTH: f64 = 2.0;
    /// Width of a card's and a group's border.
    pub const BORDER: f64 = 1.5;
    /// Padding inside a card.
    pub const CARD_PAD: f64 = 12.0;
    /// Font size of group labels, file names above file cards and edge
    /// labels.
    pub const LABEL_SIZE: f64 = 14.0;
}

/// Screen lengths that do not scale with the zoom.
pub(crate) mod screen {
    /// Thinnest stroke drawn, whatever the zoom.
    pub const MIN_STROKE: f32 = 1.0;
    /// The selection ring around the selected node.
    pub const RING: f32 = 2.0;
    /// Smallest label font size; labels smaller than this are hidden.
    pub const MIN_LABEL: f32 = 7.0;
    /// Pointer tolerance for hit-testing edges and borders.
    pub const HIT_TOLERANCE: f64 = 4.0;
}

/// Opacity of a coloured card's or group's fill.
pub(crate) const TINT_ALPHA: f32 = 0.12;
/// Opacity of a block (a card drawn without content at low zoom).
pub(crate) const BLOCK_ALPHA: f32 = 0.55;

/// The theme colours the canvas paints with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Colors {
    pub background: Hsla,
    pub card: Hsla,
    pub card_border: Hsla,
    pub group_fill: Hsla,
    pub group_border: Hsla,
    pub edge: Hsla,
    pub text: Hsla,
    pub label: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub warning: Hsla,
    pub warning_tint: Hsla,
    pub error: Hsla,
    pub presets: [Hsla; 6],
    /// The minimap's box and its border.
    pub map_background: Hsla,
    pub map_border: Hsla,
}

impl Colors {
    pub fn of(theme: &Theme) -> Self {
        Self {
            background: theme.surface.sunken.into(),
            card: theme.surface.page.into(),
            card_border: theme.border.strong.into(),
            group_fill: theme.state.hover.into(),
            group_border: theme.border.strong.into(),
            edge: theme.text.tertiary.into(),
            text: theme.text.primary.into(),
            label: theme.text.secondary.into(),
            muted: theme.text.tertiary.into(),
            accent: theme.accent.default.into(),
            warning: theme.status.warning.into(),
            warning_tint: theme.status.warning_tint.into(),
            error: theme.status.error.into(),
            presets: theme.canvas.preset.map(Into::into),
            map_background: theme.surface.raised.into(),
            map_border: theme.border.default.into(),
        }
    }

    /// The colour of a `color` value.
    pub fn of_value(&self, color: CanvasColor) -> Hsla {
        match color {
            CanvasColor::Preset(p) => self.presets[usize::from(p.number() - 1)],
            CanvasColor::Rgba { r, g, b, a } => gpui::Rgba {
                r: f32::from(r) / 255.0,
                g: f32::from(g) / 255.0,
                b: f32::from(b) / 255.0,
                a: f32::from(a) / 255.0,
            }
            .into(),
        }
    }

    /// Border and fill of a card or group with `color`.
    pub fn frame(&self, color: Option<CanvasColor>, group: bool) -> (Hsla, Hsla) {
        match color {
            Some(c) => {
                let c = self.of_value(c);
                (c, c.opacity(TINT_ALPHA))
            }
            None if group => (self.group_border, self.group_fill),
            None => (self.card_border, self.card),
        }
    }

    /// Fill of a node drawn as a block.
    pub fn block(&self, color: Option<CanvasColor>) -> Hsla {
        match color {
            Some(c) => self.of_value(c).opacity(BLOCK_ALPHA),
            None => self.card_border.opacity(BLOCK_ALPHA),
        }
    }

    /// Colour of an edge.
    pub fn edge(&self, color: Option<CanvasColor>) -> Hsla {
        color.map_or(self.edge, |c| self.of_value(c))
    }
}

/// World point to window point, with `origin` the window position of the
/// view's top-left corner.
pub(crate) fn to_window(
    t: &Transform,
    origin: Point<Pixels>,
    p: gv_canvas::Point,
) -> Point<Pixels> {
    let s = t.world_to_screen(p);
    point(origin.x + px(s.x as f32), origin.y + px(s.y as f32))
}

/// World rectangle to window bounds.
pub(crate) fn rect_to_window(t: &Transform, origin: Point<Pixels>, r: &Rect) -> Bounds<Pixels> {
    let s = t.world_rect_to_screen(r);
    Bounds::new(
        point(origin.x + px(s.x as f32), origin.y + px(s.y as f32)),
        size(px(s.width as f32), px(s.height as f32)),
    )
}

/// A world length on screen, never thinner than [`screen::MIN_STROKE`].
pub(crate) fn stroke(world_len: f64, zoom: f64) -> f32 {
    ((world_len * zoom) as f32).max(screen::MIN_STROKE)
}

/// Paints edge `edge` of `doc`. `detail` decides whether arrowheads are
/// drawn (labels are elements of the view).
pub(crate) fn edge(
    window: &mut Window,
    doc: &Doc,
    edge: usize,
    t: &Transform,
    origin: Point<Pixels>,
    detail: Detail,
    colors: &Colors,
) {
    let g: &EdgeGeometry = doc.layout.edge_geometry(edge);
    let color = colors.edge(doc.canvas.edges()[edge].color);
    let w = |p| to_window(t, origin, p);
    let mut b = PathBuilder::stroke(px(stroke(world::EDGE_WIDTH, t.zoom)));
    // The curve stops at the arrowheads' bases so the stroke does not poke
    // through their tips.
    let start = g.from_arrow.map_or(g.from, |a| a.base());
    let end = g.to_arrow.map_or(g.to, |a| a.base());
    b.move_to(w(start));
    b.cubic_bezier_to(w(end), w(g.c1), w(g.c2));
    if let Ok(path) = b.build() {
        window.paint_path(path, color);
    }
    if detail != Detail::Full {
        return;
    }
    for arrow in [g.from_arrow, g.to_arrow].into_iter().flatten() {
        let mut b = PathBuilder::fill();
        b.move_to(w(arrow.tip));
        b.line_to(w(arrow.left));
        b.line_to(w(arrow.right));
        b.close();
        if let Ok(path) = b.build() {
            window.paint_path(path, color);
        }
    }
}

/// The most tiles a `repeat` background paints; smaller tiles are drawn
/// larger.
const MAX_TILES: f32 = 1024.0;

/// Where a background image `image` (its size in px, a world size at
/// zoom 1) goes in a group drawn at `group` at `zoom`: `cover` fills the
/// group (cropped by the group), `ratio` fits inside it, centred, keeping
/// the image's proportions, `repeat` tiles it from the group's top-left
/// at its own size (larger when it would take more than [`MAX_TILES`]).
pub(crate) fn background_rects(
    group: Bounds<Pixels>,
    image: (f32, f32),
    style: BackgroundStyle,
    zoom: f32,
) -> Vec<Bounds<Pixels>> {
    let (gw, gh) = (f32::from(group.size.width), f32::from(group.size.height));
    let (iw, ih) = (image.0.max(1.0), image.1.max(1.0));
    if gw <= 0.0 || gh <= 0.0 {
        return Vec::new();
    }
    let centred = |scale: f32| {
        let (w, h) = (iw * scale, ih * scale);
        Bounds::new(
            point(
                group.origin.x + px((gw - w) / 2.0),
                group.origin.y + px((gh - h) / 2.0),
            ),
            size(px(w), px(h)),
        )
    };
    match style {
        BackgroundStyle::Cover => vec![centred((gw / iw).max(gh / ih))],
        BackgroundStyle::Ratio => vec![centred((gw / iw).min(gh / ih))],
        BackgroundStyle::Repeat => {
            let (mut tw, mut th) = (iw * zoom, ih * zoom);
            let count = (gw / tw).ceil() * (gh / th).ceil();
            if count > MAX_TILES {
                let k = (count / MAX_TILES).sqrt();
                tw *= k;
                th *= k;
            }
            let mut out = Vec::new();
            let mut y = 0.0;
            while y < gh {
                let mut x = 0.0;
                while x < gw {
                    out.push(Bounds::new(
                        point(group.origin.x + px(x), group.origin.y + px(y)),
                        size(px(tw), px(th)),
                    ));
                    x += tw;
                }
                y += th;
            }
            out
        }
    }
}

/// Paints group `node`'s box (its label is an element of the view), with
/// its background image when it has one loaded. Returns whether a
/// background image was painted.
pub(crate) fn group(
    window: &mut Window,
    doc: &Doc,
    node: usize,
    t: &Transform,
    origin: Point<Pixels>,
    colors: &Colors,
    image: Option<&BackgroundImage>,
) -> bool {
    let n = &doc.canvas.nodes()[node];
    let bounds = rect_to_window(t, origin, &n.rect);
    let (border, background) = colors.frame(n.color, true);
    let radius = px((world::RADIUS * t.zoom) as f32);
    let border_width = px(stroke(world::BORDER, t.zoom));
    let NodeKind::Group {
        background_style, ..
    } = &n.kind
    else {
        return false;
    };
    let Some(image) = image else {
        window.paint_quad(
            fill(bounds, background)
                .corner_radii(radius)
                .border_widths(border_width)
                .border_color(border),
        );
        return false;
    };
    window.paint_quad(fill(bounds, background).corner_radii(radius));
    let style = background_style.unwrap_or(BackgroundStyle::Cover);
    let rects = background_rects(bounds, (image.width, image.height), style, t.zoom as f32);
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for rect in rects {
            let (clip, corners) = match style {
                BackgroundStyle::Cover => (bounds, Corners::all(radius)),
                BackgroundStyle::Ratio | BackgroundStyle::Repeat => (rect, Corners::default()),
            };
            if let Err(e) = window.paint_image(clip, rect, corners, image.image.clone(), 0, false) {
                tracing::debug!("canvas: a group background was not painted: {e}");
                break;
            }
        }
    });
    window.paint_quad(
        fill(bounds, gpui::transparent_black())
            .corner_radii(radius)
            .border_widths(border_width)
            .border_color(border),
    );
    true
}

/// Paints the whole of `doc` as shapes in `bounds`, fitted with `padding`
/// px around: groups, edges as plain strokes and every other node as a
/// block, no text (as Obsidian draws `![[x.canvas]]` embeds). Returns the
/// number of items painted.
pub(crate) fn shapes_only(
    window: &mut Window,
    doc: &Doc,
    bounds: Bounds<Pixels>,
    padding: f32,
    colors: &Colors,
) -> usize {
    let viewport = gv_canvas::Size::new(
        f64::from(f32::from(bounds.size.width)),
        f64::from(f32::from(bounds.size.height)),
    );
    let opts = gv_canvas::FitOptions {
        padding: f64::from(padding),
        min_zoom: 1e-6,
        max_zoom: 1.0,
    };
    let t = doc.layout.fit(viewport, &opts);
    let lod = LodThresholds::SHAPES_ONLY;
    let origin = bounds.origin;
    let mut painted = 0;
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for &item in doc.layout.paint_order() {
            match item {
                ItemId::Edge(e) => {
                    edge(window, doc, e, &t, origin, lod.edge_detail(t.zoom), colors);
                    painted += 1;
                }
                ItemId::Node(n) => {
                    let node = &doc.canvas.nodes()[n];
                    if lod.detail(&node.rect, t.zoom) == Detail::Hidden {
                        continue;
                    }
                    if matches!(node.kind, NodeKind::Group { .. }) {
                        group(window, doc, n, &t, origin, colors, None);
                    } else {
                        let b = rect_to_window(&t, origin, &node.rect);
                        let radius = px((world::RADIUS * t.zoom) as f32);
                        window.paint_quad(fill(b, colors.block(node.color)).corner_radii(radius));
                    }
                    painted += 1;
                }
            }
        }
    });
    painted
}

#[cfg(test)]
mod tests {
    use gv_canvas::Preset;

    use super::*;
    use crate::theme::ThemeId;

    #[test]
    fn presets_map_to_theme_tokens_and_hex_is_kept() {
        for id in ThemeId::ALL {
            let theme = id.theme();
            let colors = Colors::of(theme);
            for (i, p) in Preset::ALL.into_iter().enumerate() {
                let expected: Hsla = theme.canvas.preset[i].into();
                assert_eq!(
                    colors.of_value(CanvasColor::Preset(p)),
                    expected,
                    "{id:?} {p:?}"
                );
            }
            let hex = CanvasColor::parse("#ff000080").expect("hex");
            let c: gpui::Rgba = colors.of_value(hex).into();
            assert!((c.r - 1.0).abs() < 1e-3 && c.g.abs() < 1e-3 && (c.a - 0.5).abs() < 0.01);
        }
        // Light and dark presets differ (each theme has its own shades).
        assert_ne!(
            ThemeId::Light.theme().canvas.preset,
            ThemeId::Dark.theme().canvas.preset
        );
    }

    #[test]
    fn background_styles_cover_fit_or_tile() {
        let group = Bounds::new(point(px(10.0), px(20.0)), size(px(200.0), px(100.0)));
        let one = |style| background_rects(group, (100.0, 100.0), style, 1.0);
        // Cover: 200 × 200, centred, cropped by the group.
        assert_eq!(
            one(BackgroundStyle::Cover),
            vec![Bounds::new(
                point(px(10.0), px(-30.0)),
                size(px(200.0), px(200.0))
            )]
        );
        // Ratio: 100 × 100, centred.
        assert_eq!(
            one(BackgroundStyle::Ratio),
            vec![Bounds::new(
                point(px(60.0), px(20.0)),
                size(px(100.0), px(100.0))
            )]
        );
        // Repeat: tiles of the image's size times the zoom.
        let tiles = background_rects(group, (100.0, 100.0), BackgroundStyle::Repeat, 0.5);
        assert_eq!(tiles.len(), 8);
        assert_eq!(tiles[0].size, size(px(50.0), px(50.0)));
        assert_eq!(tiles[5].origin, point(px(60.0), px(70.0)));
        // A tiny image zoomed out is never thousands of tiles.
        let big = Bounds::new(point(px(0.0), px(0.0)), size(px(4000.0), px(4000.0)));
        let many = background_rects(big, (2.0, 2.0), BackgroundStyle::Repeat, 0.5);
        assert!(many.len() <= MAX_TILES as usize + 128, "{}", many.len());
        assert!(
            background_rects(Bounds::default(), (5.0, 5.0), BackgroundStyle::Cover, 1.0).is_empty()
        );
    }

    #[test]
    fn coloured_frames_are_tinted_and_plain_ones_use_surfaces() {
        let colors = Colors::of(ThemeId::Light.theme());
        let red = CanvasColor::Preset(Preset::Red);
        let (border, fill) = colors.frame(Some(red), false);
        assert_eq!(border, colors.presets[0]);
        assert!((fill.a - TINT_ALPHA * border.a).abs() < 1e-6);
        assert_eq!(colors.frame(None, false), (colors.card_border, colors.card));
        assert_eq!(
            colors.frame(None, true),
            (colors.group_border, colors.group_fill)
        );
    }
}

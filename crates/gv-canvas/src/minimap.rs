// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The minimap: the whole canvas and the part the view shows, scaled into
//! a small box.
//!
//! The map shows the union of the canvas bounds and the visible world
//! area, so the viewport rectangle is always inside it, centred in the
//! box with a margin. A point of the map gives the world point a click
//! there centres the view on.

use crate::geom::{Point, Rect, Size, Transform};

/// When a minimap is worth showing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinimapPolicy {
    /// The view must be at least this wide and tall (px): a small view has
    /// no room for a map.
    pub min_view: Size,
    /// The map is shown only when this much of the content (as a share of
    /// its area, 0–1) is outside the view.
    pub min_hidden: f64,
}

impl Default for MinimapPolicy {
    fn default() -> Self {
        Self {
            min_view: Size::new(360.0, 280.0),
            min_hidden: 0.0,
        }
    }
}

impl MinimapPolicy {
    /// Whether to show a minimap of `content` (the canvas bounds) when the
    /// view of `viewport` size shows `visible` (world units). A canvas that
    /// fits in the view needs none.
    pub fn shows(&self, content: Option<Rect>, visible: &Rect, viewport: Size) -> bool {
        let Some(content) = content else {
            return false;
        };
        if viewport.width < self.min_view.width || viewport.height < self.min_view.height {
            return false;
        }
        if visible.contains_rect(&content) {
            return false;
        }
        let area = content.area();
        if area <= 0.0 {
            return false;
        }
        let shown = intersection(&content, visible).map_or(0.0, |r| r.area());
        (area - shown) / area > self.min_hidden
    }
}

fn intersection(a: &Rect, b: &Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let r = a.right().min(b.right());
    let bt = a.bottom().min(b.bottom());
    (r > x && bt > y).then(|| Rect::new(x, y, r - x, bt - y))
}

/// The mapping between the world and a minimap box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Minimap {
    /// World to map: `map = world * scale + offset`.
    pub map: Transform,
    /// The world area the map shows.
    pub world: Rect,
    /// The viewport rectangle, in map coordinates.
    pub viewport: Rect,
}

impl Minimap {
    /// Lays out a map `size` px big, with `padding` px of margin, of
    /// `content` and the view (`transform` over a `viewport`-sized view).
    pub fn new(
        content: Rect,
        transform: &Transform,
        viewport: Size,
        size: Size,
        padding: f64,
    ) -> Self {
        let visible = transform.visible_world(viewport);
        let world = content.union(&visible);
        let avail_w = (size.width - 2.0 * padding).max(1.0);
        let avail_h = (size.height - 2.0 * padding).max(1.0);
        let scale = (avail_w / world.width.max(1e-9))
            .min(avail_h / world.height.max(1e-9))
            .max(1e-9);
        let offset = Point::new(
            padding + (avail_w - world.width * scale) / 2.0 - world.x * scale,
            padding + (avail_h - world.height * scale) / 2.0 - world.y * scale,
        );
        let map = Transform {
            zoom: scale,
            pan: offset,
        };
        Self {
            map,
            world,
            viewport: map.world_rect_to_screen(&visible),
        }
    }

    /// A world rectangle in map coordinates.
    pub fn rect(&self, world: &Rect) -> Rect {
        self.map.world_rect_to_screen(world)
    }

    /// The world point under map point `p`.
    pub fn to_world(&self, p: Point) -> Point {
        self.map.screen_to_world(p)
    }

    /// The transform that centres the view (keeping its zoom) on the world
    /// point under map point `p`: a click or drag on the map.
    pub fn center_on(&self, p: Point, zoom: f64, viewport: Size) -> Transform {
        Transform::centered_on(self.to_world(p), zoom, viewport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn shown_only_when_content_leaves_the_view() {
        let p = MinimapPolicy::default();
        let content = Some(Rect::new(0.0, 0.0, 1000.0, 1000.0));
        let big = Size::new(800.0, 600.0);
        assert!(!p.shows(None, &Rect::new(0.0, 0.0, 10.0, 10.0), big));
        // Everything in view.
        assert!(!p.shows(content, &Rect::new(-10.0, -10.0, 2000.0, 2000.0), big));
        // Part of it out of view.
        assert!(p.shows(content, &Rect::new(0.0, 0.0, 500.0, 500.0), big));
        // A view too small for a map.
        assert!(!p.shows(
            content,
            &Rect::new(0.0, 0.0, 500.0, 500.0),
            Size::new(300.0, 200.0)
        ));
        let strict = MinimapPolicy {
            min_hidden: 0.9,
            ..p
        };
        assert!(!strict.shows(content, &Rect::new(0.0, 0.0, 500.0, 500.0), big));
    }

    #[test]
    fn layout_fits_content_and_view() {
        let content = Rect::new(0.0, 0.0, 2000.0, 1000.0);
        // Zoom 1, the view shows world (0,0)-(800,600).
        let t = Transform::IDENTITY;
        let m = Minimap::new(
            content,
            &t,
            Size::new(800.0, 600.0),
            Size::new(200.0, 120.0),
            10.0,
        );
        assert!(close(m.map.zoom, 0.09));
        let r = m.rect(&content);
        assert!(close(r.x, 10.0) && close(r.width, 180.0));
        assert!(close(r.y, 15.0) && close(r.height, 90.0), "{r:?}");
        assert!(close(m.viewport.x, 10.0) && close(m.viewport.width, 72.0));
        // A click in the middle of the map centres the view on the middle.
        let at = m.center_on(Point::new(100.0, 60.0), 1.0, Size::new(800.0, 600.0));
        let centre = at.screen_to_world(Point::new(400.0, 300.0));
        assert!(
            close(centre.x, 1000.0) && close(centre.y, 500.0),
            "{centre:?}"
        );
    }

    #[test]
    fn the_viewport_stays_on_the_map_when_panned_away() {
        let content = Rect::new(0.0, 0.0, 100.0, 100.0);
        let t = Transform {
            zoom: 1.0,
            pan: Point::new(-5000.0, 0.0),
        };
        let m = Minimap::new(
            content,
            &t,
            Size::new(800.0, 600.0),
            Size::new(200.0, 150.0),
            8.0,
        );
        let box_ = Rect::new(0.0, 0.0, 200.0, 150.0).inflate(1e-6);
        assert!(box_.contains_rect(&m.viewport), "{:?}", m.viewport);
        assert!(box_.contains_rect(&m.rect(&content)));
    }
}

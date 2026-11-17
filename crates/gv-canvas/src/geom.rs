// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! World-space geometry: points, rectangles, sides and the world-to-screen
//! transform.
//!
//! World coordinates are the canvas file's pixel units (y grows downwards).
//! Screen coordinates are logical pixels of the view, with the origin at the
//! view's top-left corner.

/// A point in world or screen space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Euclidean distance to `other`.
    pub fn distance(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// A width and height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

/// An axis-aligned rectangle: top-left corner plus size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The smallest rectangle that holds every point.
    pub fn from_points(points: impl IntoIterator<Item = Point>) -> Option<Self> {
        let mut it = points.into_iter();
        let first = it.next()?;
        let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
        for p in it {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
        }
        Some(Self::new(x0, y0, x1 - x0, y1 - y0))
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    pub fn area(&self) -> f64 {
        self.width * self.height
    }

    /// Whether `p` lies inside or on the border.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }

    /// Whether `other` lies entirely inside this rectangle (borders included).
    pub fn contains_rect(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// Whether the two rectangles overlap or touch.
    pub fn intersects(&self, other: &Self) -> bool {
        self.x <= other.right()
            && other.x <= self.right()
            && self.y <= other.bottom()
            && other.y <= self.bottom()
    }

    pub fn union(&self, other: &Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Grows the rectangle by `d` on every side (shrinks for negative `d`).
    pub fn inflate(&self, d: f64) -> Self {
        Self::new(
            self.x - d,
            self.y - d,
            (self.width + 2.0 * d).max(0.0),
            (self.height + 2.0 * d).max(0.0),
        )
    }

    /// The middle of one side: where Obsidian attaches edges.
    pub fn side_midpoint(&self, side: Side) -> Point {
        let c = self.center();
        match side {
            Side::Top => Point::new(c.x, self.y),
            Side::Right => Point::new(self.right(), c.y),
            Side::Bottom => Point::new(c.x, self.bottom()),
            Side::Left => Point::new(self.x, c.y),
        }
    }

    /// Shortest distance from `p` to the rectangle (0 inside).
    pub fn distance_to(&self, p: Point) -> f64 {
        let dx = (self.x - p.x).max(0.0).max(p.x - self.right());
        let dy = (self.y - p.y).max(0.0).max(p.y - self.bottom());
        dx.hypot(dy)
    }
}

/// A side of a node, as in an edge's `fromSide` / `toSide`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    /// Parses the JSON Canvas spelling (`top`, `right`, `bottom`, `left`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "top" => Some(Self::Top),
            "right" => Some(Self::Right),
            "bottom" => Some(Self::Bottom),
            "left" => Some(Self::Left),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
        }
    }

    /// Unit vector pointing out of the node through this side.
    pub fn normal(self) -> Point {
        match self {
            Self::Top => Point::new(0.0, -1.0),
            Self::Right => Point::new(1.0, 0.0),
            Self::Bottom => Point::new(0.0, 1.0),
            Self::Left => Point::new(-1.0, 0.0),
        }
    }
}

/// Smallest and largest zoom the view allows by default.
pub const DEFAULT_MIN_ZOOM: f64 = 0.05;
/// See [`DEFAULT_MIN_ZOOM`].
pub const DEFAULT_MAX_ZOOM: f64 = 4.0;

/// Maps world to screen: `screen = world * zoom + pan`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub zoom: f64,
    pub pan: Point,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Self = Self {
        zoom: 1.0,
        pan: Point::new(0.0, 0.0),
    };

    pub fn world_to_screen(&self, p: Point) -> Point {
        Point::new(p.x * self.zoom + self.pan.x, p.y * self.zoom + self.pan.y)
    }

    pub fn screen_to_world(&self, p: Point) -> Point {
        Point::new(
            (p.x - self.pan.x) / self.zoom,
            (p.y - self.pan.y) / self.zoom,
        )
    }

    pub fn world_rect_to_screen(&self, r: &Rect) -> Rect {
        let o = self.world_to_screen(Point::new(r.x, r.y));
        Rect::new(o.x, o.y, r.width * self.zoom, r.height * self.zoom)
    }

    /// The part of the world a view of `viewport` size shows.
    pub fn visible_world(&self, viewport: Size) -> Rect {
        let o = self.screen_to_world(Point::new(0.0, 0.0));
        Rect::new(
            o.x,
            o.y,
            viewport.width / self.zoom,
            viewport.height / self.zoom,
        )
    }

    /// Zooms by `factor` keeping the world point under `screen_anchor` fixed
    /// (wheel and pinch zoom). The new zoom is clamped to `[min, max]`.
    pub fn zoomed_at(&self, screen_anchor: Point, factor: f64, min: f64, max: f64) -> Self {
        let world = self.screen_to_world(screen_anchor);
        let zoom = (self.zoom * factor).clamp(min, max);
        Self {
            zoom,
            pan: Point::new(
                screen_anchor.x - world.x * zoom,
                screen_anchor.y - world.y * zoom,
            ),
        }
    }

    /// Moves the view by a screen-space delta.
    pub fn panned(&self, dx: f64, dy: f64) -> Self {
        Self {
            zoom: self.zoom,
            pan: Point::new(self.pan.x + dx, self.pan.y + dy),
        }
    }

    /// The transform that puts world point `world` at the centre of the view.
    pub fn centered_on(world: Point, zoom: f64, viewport: Size) -> Self {
        Self {
            zoom,
            pan: Point::new(
                viewport.width / 2.0 - world.x * zoom,
                viewport.height / 2.0 - world.y * zoom,
            ),
        }
    }
}

/// Options for [`fit_to_content`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitOptions {
    /// Screen-space margin around the content, in logical pixels.
    pub padding: f64,
    pub min_zoom: f64,
    /// Fitting never zooms in beyond this (small canvases stay at 100 %).
    pub max_zoom: f64,
}

impl Default for FitOptions {
    fn default() -> Self {
        Self {
            padding: 40.0,
            min_zoom: DEFAULT_MIN_ZOOM,
            max_zoom: 1.0,
        }
    }
}

/// The transform that shows all of `content` centred in `viewport`.
pub fn fit_to_content(content: Rect, viewport: Size, opts: &FitOptions) -> Transform {
    let avail_w = (viewport.width - 2.0 * opts.padding).max(1.0);
    let avail_h = (viewport.height - 2.0 * opts.padding).max(1.0);
    let zx = if content.width > 0.0 {
        avail_w / content.width
    } else {
        opts.max_zoom
    };
    let zy = if content.height > 0.0 {
        avail_h / content.height
    } else {
        opts.max_zoom
    };
    let zoom = zx.min(zy).clamp(opts.min_zoom, opts.max_zoom);
    Transform::centered_on(content.center(), zoom, viewport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_round_trip() {
        let t = Transform {
            zoom: 2.0,
            pan: Point::new(10.0, -5.0),
        };
        let p = Point::new(3.0, 4.0);
        let s = t.world_to_screen(p);
        assert_eq!(s, Point::new(16.0, 3.0));
        assert_eq!(t.screen_to_world(s), p);
    }

    #[test]
    fn zoom_keeps_anchor_fixed() {
        let t = Transform::IDENTITY;
        let anchor = Point::new(100.0, 50.0);
        let world = t.screen_to_world(anchor);
        let z = t.zoomed_at(anchor, 2.0, 0.1, 4.0);
        assert_eq!(z.zoom, 2.0);
        assert_eq!(z.world_to_screen(world), anchor);
        assert_eq!(t.zoomed_at(anchor, 100.0, 0.1, 4.0).zoom, 4.0);
    }

    #[test]
    fn fit_centres_and_caps() {
        let content = Rect::new(0.0, 0.0, 2000.0, 1000.0);
        let vp = Size::new(1080.0, 1080.0);
        let t = fit_to_content(content, vp, &FitOptions::default());
        assert!((t.zoom - 0.5).abs() < 1e-9);
        assert_eq!(
            t.world_to_screen(content.center()),
            Point::new(540.0, 540.0)
        );
        let small = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(fit_to_content(small, vp, &FitOptions::default()).zoom, 1.0);
        let visible = t.visible_world(vp);
        assert!(visible.contains_rect(&content));
    }

    #[test]
    fn rect_helpers() {
        let r = Rect::new(0.0, 0.0, 10.0, 20.0);
        assert_eq!(r.side_midpoint(Side::Right), Point::new(10.0, 10.0));
        assert_eq!(r.side_midpoint(Side::Top), Point::new(5.0, 0.0));
        assert_eq!(r.distance_to(Point::new(13.0, 24.0)), 5.0);
        assert!(r.intersects(&Rect::new(10.0, 20.0, 1.0, 1.0)));
        assert!(!r.intersects(&Rect::new(11.0, 0.0, 1.0, 1.0)));
    }
}

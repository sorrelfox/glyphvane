// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Edge geometry: anchor points, the cubic Bézier between them, arrowheads,
//! label position, bounds and distance for hit-testing.

use crate::geom::{Point, Rect, Side};
use crate::model::End;

/// Sizes used to build edge geometry, in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeStyle {
    /// Length of an arrowhead along the curve.
    pub arrow_length: f64,
    /// Full width of an arrowhead at its base.
    pub arrow_width: f64,
    /// Smallest distance of a control point from its anchor.
    pub min_handle: f64,
    /// Largest distance of a control point from its anchor.
    pub max_handle: f64,
}

impl Default for EdgeStyle {
    fn default() -> Self {
        Self {
            arrow_length: 16.0,
            arrow_width: 14.0,
            min_handle: 40.0,
            max_handle: 300.0,
        }
    }
}

/// A filled triangle at one end of an edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrowhead {
    /// The point touching the node.
    pub tip: Point,
    /// The base corners.
    pub left: Point,
    pub right: Point,
}

impl Arrowhead {
    /// Midpoint of the base, where the stroke can stop.
    pub fn base(&self) -> Point {
        Point::new(
            (self.left.x + self.right.x) / 2.0,
            (self.left.y + self.right.y) / 2.0,
        )
    }
}

/// Everything needed to draw one edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeGeometry {
    /// Start anchor, on `from_side` of the source node.
    pub from: Point,
    pub from_side: Side,
    /// End anchor, on `to_side` of the target node.
    pub to: Point,
    pub to_side: Side,
    /// Bézier control points: the curve is `from, c1, c2, to`.
    pub c1: Point,
    pub c2: Point,
    pub from_arrow: Option<Arrowhead>,
    pub to_arrow: Option<Arrowhead>,
    /// Centre of the label (the curve's midpoint).
    pub label_anchor: Point,
    /// Holds the whole curve and the arrowheads (not the label).
    pub bounds: Rect,
}

/// Picks the sides when the file leaves them out: the side of each node that
/// faces the other along the axis where they are further apart.
pub fn auto_sides(from: &Rect, to: &Rect) -> (Side, Side) {
    let a = from.center();
    let b = to.center();
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            (Side::Right, Side::Left)
        } else {
            (Side::Left, Side::Right)
        }
    } else if dy >= 0.0 {
        (Side::Bottom, Side::Top)
    } else {
        (Side::Top, Side::Bottom)
    }
}

/// The side of `rect` facing `toward`.
fn facing_side(rect: &Rect, toward: Point) -> Side {
    let c = rect.center();
    let dx = (toward.x - c.x) / rect.width.max(1.0);
    let dy = (toward.y - c.y) / rect.height.max(1.0);
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 { Side::Right } else { Side::Left }
    } else if dy >= 0.0 {
        Side::Bottom
    } else {
        Side::Top
    }
}

fn add(a: Point, b: Point, k: f64) -> Point {
    Point::new(a.x + b.x * k, a.y + b.y * k)
}

fn arrow(tip: Point, toward_tip: Point, style: &EdgeStyle) -> Arrowhead {
    let len = toward_tip.x.hypot(toward_tip.y);
    let dir = if len > 1e-9 {
        Point::new(toward_tip.x / len, toward_tip.y / len)
    } else {
        Point::new(1.0, 0.0)
    };
    let base = add(tip, dir, -style.arrow_length);
    let perp = Point::new(-dir.y, dir.x);
    Arrowhead {
        tip,
        left: add(base, perp, style.arrow_width / 2.0),
        right: add(base, perp, -style.arrow_width / 2.0),
    }
}

/// Builds the geometry of an edge between two node rectangles.
pub fn edge_geometry(
    from_rect: &Rect,
    to_rect: &Rect,
    from_side: Option<Side>,
    to_side: Option<Side>,
    from_end: End,
    to_end: End,
    style: &EdgeStyle,
) -> EdgeGeometry {
    let (from_side, to_side) = match (from_side, to_side) {
        (Some(f), Some(t)) => (f, t),
        (Some(f), None) => (f, facing_side(to_rect, from_rect.side_midpoint(f))),
        (None, Some(t)) => (facing_side(from_rect, to_rect.side_midpoint(t)), t),
        (None, None) => auto_sides(from_rect, to_rect),
    };
    let from = from_rect.side_midpoint(from_side);
    let to = to_rect.side_midpoint(to_side);
    let handle = (from.distance(to) / 2.0).clamp(style.min_handle, style.max_handle);
    let c1 = add(from, from_side.normal(), handle);
    let c2 = add(to, to_side.normal(), handle);
    // The curve's tangent at each end points along the side normal, so the
    // arrows line up with it.
    let to_arrow =
        (to_end == End::Arrow).then(|| arrow(to, Point::new(to.x - c2.x, to.y - c2.y), style));
    let from_arrow = (from_end == End::Arrow)
        .then(|| arrow(from, Point::new(from.x - c1.x, from.y - c1.y), style));
    let mut pts = vec![from, to, c1, c2];
    for a in from_arrow.iter().chain(to_arrow.iter()) {
        pts.extend([a.left, a.right]);
    }
    let bounds = Rect::from_points(pts).unwrap_or_default();
    let mut g = EdgeGeometry {
        from,
        from_side,
        to,
        to_side,
        c1,
        c2,
        from_arrow,
        to_arrow,
        label_anchor: from,
        bounds,
    };
    g.label_anchor = g.point_at(0.5);
    g
}

impl EdgeGeometry {
    /// The curve's point at parameter `t` in `[0, 1]`.
    pub fn point_at(&self, t: f64) -> Point {
        let u = 1.0 - t;
        let a = u * u * u;
        let b = 3.0 * u * u * t;
        let c = 3.0 * u * t * t;
        let d = t * t * t;
        Point::new(
            a * self.from.x + b * self.c1.x + c * self.c2.x + d * self.to.x,
            a * self.from.y + b * self.c1.y + c * self.c2.y + d * self.to.y,
        )
    }

    /// The curve as a polyline of `segments` pieces (at least 1).
    pub fn flatten(&self, segments: usize) -> Vec<Point> {
        let n = segments.max(1);
        (0..=n)
            .map(|i| self.point_at(i as f64 / n as f64))
            .collect()
    }

    /// Approximate shortest distance from `p` to the curve or its arrowheads.
    pub fn distance_to(&self, p: Point) -> f64 {
        let pts = self.flatten(32);
        let mut best = f64::INFINITY;
        for w in pts.windows(2) {
            best = best.min(segment_distance(p, w[0], w[1]));
        }
        for a in self.from_arrow.iter().chain(self.to_arrow.iter()) {
            if in_triangle(p, a.tip, a.left, a.right) {
                return 0.0;
            }
        }
        best
    }
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(Point::new(a.x + dx * t, a.y + dy * t))
}

fn in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let cross =
        |o: Point, u: Point, v: Point| (u.x - o.x) * (v.y - o.y) - (u.y - o.y) * (v.x - o.x);
    let d1 = cross(p, a, b);
    let d2 = cross(p, b, c);
    let d3 = cross(p, c, a);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> (Rect, Rect) {
        (
            Rect::new(0.0, 0.0, 100.0, 50.0),
            Rect::new(300.0, 0.0, 100.0, 50.0),
        )
    }

    #[test]
    fn explicit_sides_and_arrows() {
        let (a, b) = two();
        let g = edge_geometry(
            &a,
            &b,
            Some(Side::Right),
            Some(Side::Left),
            End::None,
            End::Arrow,
            &EdgeStyle::default(),
        );
        assert_eq!(g.from, Point::new(100.0, 25.0));
        assert_eq!(g.to, Point::new(300.0, 25.0));
        assert_eq!(g.c1, Point::new(200.0, 25.0));
        assert_eq!(g.c2, Point::new(200.0, 25.0));
        assert!(g.from_arrow.is_none());
        let arrow = g.to_arrow.expect("arrow");
        assert_eq!(arrow.tip, g.to);
        assert_eq!(arrow.base(), Point::new(284.0, 25.0));
        assert_eq!(g.label_anchor, Point::new(200.0, 25.0));
        assert!(g.bounds.contains(arrow.left) && g.bounds.contains(g.from));
    }

    #[test]
    fn auto_sides_follow_layout() {
        let (a, b) = two();
        assert_eq!(auto_sides(&a, &b), (Side::Right, Side::Left));
        assert_eq!(auto_sides(&b, &a), (Side::Left, Side::Right));
        let below = Rect::new(0.0, 400.0, 100.0, 50.0);
        assert_eq!(auto_sides(&a, &below), (Side::Bottom, Side::Top));
        // Only the start side given: the end faces the start anchor.
        let g = edge_geometry(
            &a,
            &below,
            Some(Side::Right),
            None,
            End::Arrow,
            End::Arrow,
            &EdgeStyle::default(),
        );
        assert_eq!(g.to_side, Side::Top);
        assert!(g.from_arrow.is_some());
    }

    #[test]
    fn distance_hits_curve() {
        let (a, b) = two();
        let g = edge_geometry(
            &a,
            &b,
            None,
            None,
            End::None,
            End::Arrow,
            &EdgeStyle::default(),
        );
        assert!(g.distance_to(Point::new(200.0, 25.0)) < 1e-6);
        assert!(g.distance_to(Point::new(200.0, 35.0)) > 9.0);
        assert_eq!(g.distance_to(Point::new(295.0, 25.0)), 0.0);
    }
}

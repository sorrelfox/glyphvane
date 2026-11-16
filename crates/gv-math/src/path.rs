// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Backend-neutral vector paths (em units, y down).

/// One element of a [`MathPath`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathEl {
    /// Start a contour at a point.
    MoveTo(f32, f32),
    LineTo(f32, f32),
    /// Quadratic Bézier: control point, end point.
    QuadTo(f32, f32, f32, f32),
    /// Cubic Bézier: two control points, end point.
    CubicTo(f32, f32, f32, f32, f32, f32),
    /// Close the current contour.
    Close,
}

/// A path of contours, filled with the non-zero rule (or stroked).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MathPath {
    els: Vec<PathEl>,
}

/// An axis-aligned rectangle in em units (y down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl Bounds {
    /// The rectangle of a single point.
    pub fn point(x: f32, y: f32) -> Self {
        Self {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
        }
    }

    /// The smallest rectangle covering both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    /// The rectangle scaled by `s` around the origin, then moved by
    /// `(dx, dy)`.
    #[must_use]
    pub fn transformed(self, dx: f32, dy: f32, s: f32) -> Self {
        Self {
            min_x: dx + self.min_x * s,
            min_y: dy + self.min_y * s,
            max_x: dx + self.max_x * s,
            max_y: dy + self.max_y * s,
        }
    }

    /// The rectangle grown by `d` on every side.
    #[must_use]
    pub fn inflated(self, d: f32) -> Self {
        Self {
            min_x: self.min_x - d,
            min_y: self.min_y - d,
            max_x: self.max_x + d,
            max_y: self.max_y + d,
        }
    }

    pub fn width(&self) -> f32 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f32 {
        self.max_y - self.min_y
    }
}

impl MathPath {
    /// The elements in order.
    pub fn elements(&self) -> &[PathEl] {
        &self.els
    }

    pub fn is_empty(&self) -> bool {
        self.els.is_empty()
    }

    /// The bounds of every point, control points included (a cheap,
    /// conservative ink box). `None` for an empty path.
    pub fn bounds(&self) -> Option<Bounds> {
        let mut b: Option<Bounds> = None;
        let mut add = |x: f32, y: f32| {
            let p = Bounds::point(x, y);
            b = Some(b.map_or(p, |b| b.union(p)));
        };
        for el in &self.els {
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

    /// An axis-aligned rectangle as a closed path.
    pub fn rect(x: f32, y: f32, width: f32, height: f32) -> Self {
        let mut b = PathBuilder::default();
        b.move_to(x, y);
        b.line_to(x + width, y);
        b.line_to(x + width, y + height);
        b.line_to(x, y + height);
        b.close();
        b.finish()
    }
}

/// Builds a [`MathPath`], dropping non-finite coordinates.
#[derive(Debug, Default)]
pub struct PathBuilder {
    els: Vec<PathEl>,
}

fn ok(v: &[f32]) -> bool {
    v.iter().all(|v| v.is_finite())
}

impl PathBuilder {
    pub fn move_to(&mut self, x: f32, y: f32) {
        if ok(&[x, y]) {
            self.els.push(PathEl::MoveTo(x, y));
        }
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        if ok(&[x, y]) {
            self.els.push(PathEl::LineTo(x, y));
        }
    }

    pub fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        if ok(&[x1, y1, x, y]) {
            self.els.push(PathEl::QuadTo(x1, y1, x, y));
        }
    }

    pub fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        if ok(&[x1, y1, x2, y2, x, y]) {
            self.els.push(PathEl::CubicTo(x1, y1, x2, y2, x, y));
        }
    }

    pub fn close(&mut self) {
        self.els.push(PathEl::Close);
    }

    pub fn finish(self) -> MathPath {
        MathPath { els: self.els }
    }
}

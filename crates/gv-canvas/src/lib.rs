// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! JSON Canvas 1.0 model that preserves unknown keys.
//!
//! - [`Canvas`] parses a `.canvas` file, keeps its JSON as written (key order
//!   and unknown keys included) and offers validated [`Node`]s and [`Edge`]s.
//!   Bad items are reported one by one as [`Issue`]s; only text that is not a
//!   JSON object fails as a whole.
//! - [`patch`] finds the vault paths in a canvas with their byte ranges and
//!   rewrites them in place, for rename refactors.
//! - [`CanvasView`] is what a renderer draws from: bounds, paint order,
//!   viewport culling, hit-testing, edge geometry, level of detail and
//!   navigation.
//! - [`Minimap`] maps the canvas and the view into a small overview box;
//!   [`MinimapPolicy`] says when one is worth showing.

mod color;
mod edge;
mod geom;
mod minimap;
mod model;
pub mod patch;
mod view;

pub use color::{CanvasColor, Preset};
pub use edge::{Arrowhead, EdgeGeometry, EdgeStyle, auto_sides, edge_geometry};
pub use geom::{
    DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, FitOptions, Point, Rect, Side, Size, Transform,
    fit_to_content,
};
pub use minimap::{Minimap, MinimapPolicy};
pub use model::{
    BackgroundStyle, Canvas, EDGE_KEYS, Edge, End, Issue, IssueItem, IssueKind, MAX_COORDINATE,
    NODE_GENERIC_KEYS, Node, NodeKind, ParseError, Severity, Subpath, node_type_keys,
};
pub use view::{CanvasView, Detail, Direction, ItemId, Layer, LodThresholds};

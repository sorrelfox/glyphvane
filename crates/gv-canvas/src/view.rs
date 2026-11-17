// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The view model a renderer draws from: world bounds, paint order, a
//! spatial index for viewport culling, hit-testing, level of detail and
//! keyboard navigation.
//!
//! Paint order (bottom to top):
//! 1. groups, larger ones first so nested groups stay visible (file order
//!    breaks ties);
//! 2. edges, in file order;
//! 3. all other nodes, in file order (the format's z-order).

use crate::edge::{EdgeGeometry, EdgeStyle, edge_geometry};
use crate::geom::{FitOptions, Point, Rect, Size, Transform, fit_to_content};
use crate::model::Canvas;

/// An item of the view. The numbers are positions in [`Canvas::nodes`] and
/// [`Canvas::edges`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemId {
    Node(usize),
    Edge(usize),
}

/// The paint layer of an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    Groups,
    Edges,
    Nodes,
}

/// How much of a node or edge to draw at the current zoom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Detail {
    /// Everything: card content, labels, arrows.
    Full,
    /// Shapes only: a node is a filled block (with its colour), group labels
    /// and edge labels are hidden; edges are plain strokes.
    Blocks,
    /// Too small on screen to be worth drawing.
    Hidden,
}

/// Zoom and size thresholds for level of detail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodThresholds {
    /// Below this zoom, content is replaced by blocks.
    pub content_min_zoom: f64,
    /// A node whose smaller screen dimension is below this many logical
    /// pixels is drawn as a block whatever the zoom.
    pub content_min_px: f64,
    /// A node whose larger screen dimension is below this is not drawn.
    pub visible_min_px: f64,
}

impl Default for LodThresholds {
    fn default() -> Self {
        Self {
            content_min_zoom: 0.3,
            content_min_px: 24.0,
            visible_min_px: 2.0,
        }
    }
}

impl LodThresholds {
    /// Embeds (`![[x.canvas]]`) draw shapes without card text.
    pub const SHAPES_ONLY: Self = Self {
        content_min_zoom: f64::INFINITY,
        content_min_px: f64::INFINITY,
        visible_min_px: 2.0,
    };

    /// Detail for a rectangle of world size `rect` at `zoom`.
    pub fn detail(&self, rect: &Rect, zoom: f64) -> Detail {
        let w = rect.width * zoom;
        let h = rect.height * zoom;
        if w.max(h) < self.visible_min_px {
            Detail::Hidden
        } else if zoom < self.content_min_zoom || w.min(h) < self.content_min_px {
            Detail::Blocks
        } else {
            Detail::Full
        }
    }

    /// Detail for edges (labels and arrows) at `zoom`.
    pub fn edge_detail(&self, zoom: f64) -> Detail {
        if zoom < self.content_min_zoom {
            Detail::Blocks
        } else {
            Detail::Full
        }
    }
}

/// A direction for spatial keyboard navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// The renderer's view of a [`Canvas`]. Build once per parse.
#[derive(Clone, Debug)]
pub struct CanvasView {
    node_rects: Vec<Rect>,
    node_layers: Vec<Layer>,
    edges: Vec<EdgeGeometry>,
    paint_order: Vec<ItemId>,
    /// Position of each item in `paint_order`.
    node_rank: Vec<usize>,
    edge_rank: Vec<usize>,
    bounds: Option<Rect>,
    grid: Grid,
    reading_order: Vec<usize>,
    /// For each node, the innermost group holding it.
    parent_group: Vec<Option<usize>>,
}

impl CanvasView {
    pub fn new(canvas: &Canvas) -> Self {
        Self::with_style(canvas, &EdgeStyle::default())
    }

    pub fn with_style(canvas: &Canvas, style: &EdgeStyle) -> Self {
        let nodes = canvas.nodes();
        let node_rects: Vec<Rect> = nodes.iter().map(|n| n.rect).collect();
        let node_layers: Vec<Layer> = nodes
            .iter()
            .map(|n| {
                if n.kind.is_group() {
                    Layer::Groups
                } else {
                    Layer::Nodes
                }
            })
            .collect();
        let edges: Vec<EdgeGeometry> = canvas
            .edges()
            .iter()
            .map(|e| {
                edge_geometry(
                    &node_rects[e.from],
                    &node_rects[e.to],
                    e.from_side,
                    e.to_side,
                    e.from_end,
                    e.to_end,
                    style,
                )
            })
            .collect();

        let mut groups: Vec<usize> = (0..nodes.len())
            .filter(|&i| node_layers[i] == Layer::Groups)
            .collect();
        groups.sort_by(|&a, &b| node_rects[b].area().total_cmp(&node_rects[a].area()));
        let mut paint_order: Vec<ItemId> = groups.iter().map(|&i| ItemId::Node(i)).collect();
        paint_order.extend((0..edges.len()).map(ItemId::Edge));
        paint_order.extend(
            (0..nodes.len())
                .filter(|&i| node_layers[i] == Layer::Nodes)
                .map(ItemId::Node),
        );
        let mut node_rank = vec![0; nodes.len()];
        let mut edge_rank = vec![0; edges.len()];
        for (rank, item) in paint_order.iter().enumerate() {
            match *item {
                ItemId::Node(i) => node_rank[i] = rank,
                ItemId::Edge(i) => edge_rank[i] = rank,
            }
        }

        let bounds = node_rects
            .iter()
            .copied()
            .chain(edges.iter().map(|e| e.bounds))
            .reduce(|a, b| a.union(&b));

        let mut grid = Grid::new(bounds.unwrap_or_default(), paint_order.len());
        for item in &paint_order {
            let r = match *item {
                ItemId::Node(i) => node_rects[i],
                ItemId::Edge(i) => edges[i].bounds,
            };
            grid.insert(*item, &r);
        }

        let parent_group = (0..nodes.len())
            .map(|i| {
                groups
                    .iter()
                    .rev() // smallest first
                    .copied()
                    .find(|&g| g != i && node_rects[g].contains_rect(&node_rects[i]))
            })
            .collect();

        let reading_order = reading_order(&node_rects, &node_layers);

        Self {
            node_rects,
            node_layers,
            edges,
            paint_order,
            node_rank,
            edge_rank,
            bounds,
            grid,
            reading_order,
            parent_group,
        }
    }

    /// Bounds of all nodes and edges in world space; `None` when empty.
    pub fn bounds(&self) -> Option<Rect> {
        self.bounds
    }

    /// Every item, bottom to top.
    pub fn paint_order(&self) -> &[ItemId] {
        &self.paint_order
    }

    pub fn node_rect(&self, node: usize) -> Rect {
        self.node_rects[node]
    }

    pub fn node_layer(&self, node: usize) -> Layer {
        self.node_layers[node]
    }

    pub fn edge_geometry(&self, edge: usize) -> &EdgeGeometry {
        &self.edges[edge]
    }

    /// World-space bounds of an item.
    pub fn item_bounds(&self, item: ItemId) -> Rect {
        match item {
            ItemId::Node(i) => self.node_rects[i],
            ItemId::Edge(i) => self.edges[i].bounds,
        }
    }

    fn rank(&self, item: ItemId) -> usize {
        match item {
            ItemId::Node(i) => self.node_rank[i],
            ItemId::Edge(i) => self.edge_rank[i],
        }
    }

    /// Items whose bounds meet the world rectangle `area`, bottom to top.
    /// Pass [`Transform::visible_world`] (inflated a little for edge labels).
    pub fn visible(&self, area: &Rect) -> Vec<ItemId> {
        let mut items = self.grid.query(area);
        items.retain(|&it| self.item_bounds(it).intersects(area));
        items.sort_by_key(|&it| self.rank(it));
        items
    }

    /// The topmost item at world point `p`. `tolerance` is in world units
    /// (screen pixels divided by zoom) and widens edges and node borders.
    /// A group is hit only where nothing above it is.
    pub fn hit_test(&self, p: Point, tolerance: f64) -> Option<ItemId> {
        let tol = tolerance.max(0.0);
        let probe = Rect::new(p.x - tol, p.y - tol, 2.0 * tol, 2.0 * tol);
        let mut items = self.grid.query(&probe);
        items.sort_by_key(|&it| std::cmp::Reverse(self.rank(it)));
        items.into_iter().find(|&it| match it {
            ItemId::Node(i) => self.node_rects[i].distance_to(p) <= tol,
            ItemId::Edge(i) => {
                self.edges[i].bounds.inflate(tol).contains(p) && self.edges[i].distance_to(p) <= tol
            }
        })
    }

    /// The innermost group whose rectangle holds `node` entirely.
    pub fn parent_group(&self, node: usize) -> Option<usize> {
        self.parent_group[node]
    }

    /// Non-group nodes in reading order: rows from top to bottom, left to
    /// right within a row. A node joins a row when its top is above the
    /// middle of the row's first node.
    pub fn reading_order(&self) -> &[usize] {
        &self.reading_order
    }

    /// The node after `current` in reading order (the first one for `None`).
    /// Wraps around. A group as `current` starts from its position in the
    /// layout.
    pub fn next_node(&self, current: Option<usize>) -> Option<usize> {
        self.step(current, true)
    }

    /// The node before `current` in reading order (the last one for `None`).
    pub fn prev_node(&self, current: Option<usize>) -> Option<usize> {
        self.step(current, false)
    }

    fn step(&self, current: Option<usize>, forward: bool) -> Option<usize> {
        let order = &self.reading_order;
        if order.is_empty() {
            return None;
        }
        let pos = current.and_then(|c| order.iter().position(|&n| n == c));
        let next = match (pos, forward) {
            (Some(p), true) => (p + 1) % order.len(),
            (Some(p), false) => (p + order.len() - 1) % order.len(),
            (None, true) => 0,
            (None, false) => order.len() - 1,
        };
        Some(order[next])
    }

    /// The nearest non-group node from `current` in `dir`, weighting
    /// sideways distance double. `None` when nothing lies that way.
    pub fn neighbor(&self, current: usize, dir: Direction) -> Option<usize> {
        let from = self.node_rects.get(current)?.center();
        self.reading_order
            .iter()
            .copied()
            .filter(|&n| n != current)
            .filter_map(|n| {
                let c = self.node_rects[n].center();
                let (along, across) = match dir {
                    Direction::Right => (c.x - from.x, c.y - from.y),
                    Direction::Left => (from.x - c.x, c.y - from.y),
                    Direction::Down => (c.y - from.y, c.x - from.x),
                    Direction::Up => (from.y - c.y, c.x - from.x),
                };
                (along > 0.0).then_some((n, along + 2.0 * across.abs()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n)
    }

    /// The transform that shows the whole canvas; identity-centred when empty.
    pub fn fit(&self, viewport: Size, opts: &FitOptions) -> Transform {
        match self.bounds {
            Some(b) => fit_to_content(b, viewport, opts),
            None => Transform::centered_on(Point::new(0.0, 0.0), 1.0, viewport),
        }
    }

    /// The transform that shows `node` whole, centred, zooming out only if it
    /// does not fit at `zoom`.
    pub fn focus_node(
        &self,
        node: usize,
        zoom: f64,
        viewport: Size,
        opts: &FitOptions,
    ) -> Transform {
        let r = self.node_rects[node];
        let fit = fit_to_content(r, viewport, opts).zoom;
        Transform::centered_on(r.center(), zoom.min(fit.max(opts.min_zoom)), viewport)
    }
}

fn reading_order(rects: &[Rect], layers: &[Layer]) -> Vec<usize> {
    let mut nodes: Vec<usize> = (0..rects.len())
        .filter(|&i| layers[i] == Layer::Nodes)
        .collect();
    nodes.sort_by(|&a, &b| {
        rects[a]
            .y
            .total_cmp(&rects[b].y)
            .then(rects[a].x.total_cmp(&rects[b].x))
    });
    let mut out = Vec::with_capacity(nodes.len());
    let mut row: Vec<usize> = Vec::new();
    let mut row_limit = f64::NEG_INFINITY;
    for n in nodes {
        let r = rects[n];
        if !row.is_empty() && r.y > row_limit {
            row.sort_by(|&a, &b| rects[a].x.total_cmp(&rects[b].x));
            out.append(&mut row);
        }
        if row.is_empty() {
            row_limit = r.y + r.height / 2.0;
        }
        row.push(n);
    }
    row.sort_by(|&a, &b| rects[a].x.total_cmp(&rects[b].x));
    out.append(&mut row);
    out
}

/// A uniform grid over the content bounds. Items that would cover too many
/// cells sit in a list checked on every query.
#[derive(Clone, Debug)]
struct Grid {
    origin: Point,
    cell: f64,
    cols: usize,
    rows: usize,
    cells: Vec<Vec<ItemId>>,
    large: Vec<ItemId>,
}

const MAX_GRID_SIDE: usize = 128;
const MAX_CELLS_PER_ITEM: usize = 64;

impl Grid {
    fn new(bounds: Rect, items: usize) -> Self {
        let side = ((items as f64).sqrt().ceil() as usize).clamp(1, MAX_GRID_SIDE);
        let extent = bounds.width.max(bounds.height).max(1.0);
        let cell = (extent / side as f64).max(64.0);
        let cols = ((bounds.width / cell).ceil() as usize).clamp(1, MAX_GRID_SIDE);
        let rows = ((bounds.height / cell).ceil() as usize).clamp(1, MAX_GRID_SIDE);
        Self {
            origin: Point::new(bounds.x, bounds.y),
            cell,
            cols,
            rows,
            cells: vec![Vec::new(); cols * rows],
            large: Vec::new(),
        }
    }

    fn span(&self, r: &Rect) -> (usize, usize, usize, usize) {
        let to_cell = |v: f64, o: f64, n: usize| -> usize {
            let c = ((v - o) / self.cell).floor();
            if c <= 0.0 { 0 } else { (c as usize).min(n - 1) }
        };
        (
            to_cell(r.x, self.origin.x, self.cols),
            to_cell(r.y, self.origin.y, self.rows),
            to_cell(r.right(), self.origin.x, self.cols),
            to_cell(r.bottom(), self.origin.y, self.rows),
        )
    }

    fn insert(&mut self, item: ItemId, r: &Rect) {
        let (x0, y0, x1, y1) = self.span(r);
        if (x1 - x0 + 1) * (y1 - y0 + 1) > MAX_CELLS_PER_ITEM {
            self.large.push(item);
            return;
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.cells[y * self.cols + x].push(item);
            }
        }
    }

    /// Candidates near `area`, without duplicates (unsorted; callers filter
    /// exactly).
    fn query(&self, area: &Rect) -> Vec<ItemId> {
        let (x0, y0, x1, y1) = self.span(area);
        let mut out: Vec<ItemId> = self.large.clone();
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.extend_from_slice(&self.cells[y * self.cols + x]);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

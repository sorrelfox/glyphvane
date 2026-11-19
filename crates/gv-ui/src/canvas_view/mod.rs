// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Canvas view: a `.canvas` file (JSON Canvas 1.0), read-only.
//!
//! The model, the geometry and the navigation are `gv-canvas`'s; this
//! module draws them and handles input.
//!
//! # Drawing
//!
//! - Bottom to top: groups and edges are painted straight to the scene
//!   ([`paint`]); every other node is an element (a frame with its
//!   content), in file order; then labels, the selection ring, and the
//!   view's own controls (issues, zoom).
//! - Only items meeting the viewport are drawn (the view model's spatial
//!   index). Level of detail: at full detail cards show their content
//!   ([`cards`]), edges their arrowheads and labels; zoomed out, nodes are
//!   coloured blocks and labels go; tiny items are not drawn.
//! - Colours are theme tokens: presets `"1"`–`"6"` are
//!   `canvas.preset.1`–`.6` (provisional), hex colours are used as written.
//!
//! # Input
//!
//! - Drag (left button) pans; the wheel or the trackpad pans; Ctrl-wheel
//!   (Cmd-wheel on macOS) and a pinch zoom at the pointer. The zoom
//!   commands (`view.zoom_in`, `view.zoom_out`, `view.zoom_reset`) zoom the
//!   canvas about its centre while it has the focus.
//! - A click selects a node; a click on a file card opens its file (Mod
//!   for a new tab, middle click for a background tab) and on a link card
//!   follows its URL through the window's link policy. Nothing is ever
//!   moved or written.
//! - Keys ([`key_bindings`]): Tab and Shift+Tab walk the cards in reading
//!   order and then, when the file has problems, the problems chip (Enter
//!   or Space opens its list); Tab past the last stop and Shift+Tab before
//!   the first card leave the canvas (the window's focus order), the arrows
//!   go to the nearest card that way, Enter opens the
//!   selected card's file (Mod+Enter in a new tab), Shift+1 or Home fits
//!   the whole canvas (the `view.zoom_to_fit` command), Shift+arrows pan,
//!   Escape clears the selection. Moving to a card off screen pans to it,
//!   animated unless the system asks for reduced motion.
//! - The minimap ([`minimap`]) in the bottom right corner, shown when part
//!   of the canvas is out of view: a click or a drag there moves the view.
//!
//! # Accessibility
//!
//! The canvas is a named group that takes the keyboard; its cards are an
//! invisible list in reading order whose selected item follows the
//! selection ([`a11y`]). The problems chip is a button that says whether
//! its list is open, the list is a list, and the zoom readout a status.
//!
//! # Problems
//!
//! The file's problems ([`gv_canvas::Issue`]) are counted in a chip at the
//! bottom left; a click lists them. Nodes with a problem show a warning
//! mark. A file that is not a JSON object is not shown; the view says why.
//!
//! # Not designed (built plainly, flagged)
//!
//! The whole surface: card, group and edge styling, the issues chip and
//! list, the zoom readout, the minimap, the loading, empty and error
//! states.

mod a11y;
mod cards;
mod minimap;
mod paint;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, ImageSource, InteractiveElement as _, IntoElement, KeyBinding, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _,
    PinchEvent, Pixels, Point, Render, ScrollEvent, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Task, Window, canvas, div, img, px,
};
use gv_canvas::{
    Canvas, DEFAULT_MAX_ZOOM, DEFAULT_MIN_ZOOM, Detail, Direction, FitOptions, IssueItem, ItemId,
    LodThresholds, NodeKind, Severity, Transform,
};
use gv_vault::Fragment;

use crate::a11y::{A11yExt as _, AccessibleExt as _, Live, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::inline::{FontFamilies, InlineParagraph, Palette};
use crate::keymap::actions::view::{ZoomIn, ZoomOut, ZoomReset, ZoomToFit};
use crate::reader::{Disposition, LinkRequest};
use cards::{Block, Cards, Job, Marker, Parsed, Preview};
use minimap::MinimapState;
use paint::{Colors, screen, world};

/// The key context of the Canvas view.
pub(crate) const KEY_CONTEXT: &str = "Canvas";

gpui::actions!(
    canvas,
    [
        /// Select the next card in reading order.
        NextCard,
        /// Select the previous card in reading order.
        PreviousCard,
        /// Select the nearest card to the left.
        CardLeft,
        /// Select the nearest card to the right.
        CardRight,
        /// Select the nearest card above.
        CardUp,
        /// Select the nearest card below.
        CardDown,
        /// Open the selected card's file or link.
        OpenCard,
        /// Open the selected card's file in a new tab.
        OpenCardInNewTab,
        /// Pan the view.
        PanLeft,
        PanRight,
        PanUp,
        PanDown,
        /// Clear the selection.
        ClearSelection,
    ]
);

/// The Canvas view's keys. They apply while it has the focus.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let c = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("tab", NextCard, c),
        KeyBinding::new("shift-tab", PreviousCard, c),
        KeyBinding::new("left", CardLeft, c),
        KeyBinding::new("right", CardRight, c),
        KeyBinding::new("up", CardUp, c),
        KeyBinding::new("down", CardDown, c),
        KeyBinding::new("enter", OpenCard, c),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-enter"
            } else {
                "ctrl-enter"
            },
            OpenCardInNewTab,
            c,
        ),
        // `view.zoom_to_fit` while the canvas has the focus.
        KeyBinding::new("shift-1", ZoomToFit, c),
        KeyBinding::new("home", ZoomToFit, c),
        KeyBinding::new("shift-left", PanLeft, c),
        KeyBinding::new("shift-right", PanRight, c),
        KeyBinding::new("shift-up", PanUp, c),
        KeyBinding::new("shift-down", PanDown, c),
        KeyBinding::new("escape", ClearSelection, c),
    ]
}

/// Pixels per wheel line.
const LINE_PX: f32 = 20.0;
/// A task checkbox's side in a card, in body ems (the Reading view's 14 px
/// box at its 16 px text), and its corner radius as a part of the side.
const TASK_BOX_EM: f32 = crate::metrics::size::CHECKBOX / 16.0;
const TASK_BOX_RADIUS: f32 = crate::metrics::radius::SMALL_CONTROL / crate::metrics::size::CHECKBOX;
/// Zoom factor per pixel of Ctrl-wheel scrolling (`exp(k * px)`).
const ZOOM_PER_PX: f64 = 0.002;
/// Factor of one zoom command.
const ZOOM_STEP: f64 = 1.25;
/// A press that moves less than this is a click, not a drag.
const DRAG_SLOP: f32 = 3.0;
/// Fraction of the view a pan key moves.
const PAN_KEY_FRACTION: f64 = 0.1;
/// Length of an animated move (none with reduced motion).
const ANIMATION: Duration = Duration::from_millis(180);
/// Margin, in px, kept around a card brought into view.
const REVEAL_MARGIN: f64 = 24.0;

/// A parsed canvas and its view model.
#[derive(Debug)]
pub(crate) struct Doc {
    pub canvas: Canvas,
    pub layout: gv_canvas::CanvasView,
    /// The problems of each node (by position in `canvas.nodes()`).
    pub node_issues: Vec<Vec<String>>,
}

impl Doc {
    pub fn new(canvas: Canvas) -> Self {
        let layout = gv_canvas::CanvasView::new(&canvas);
        let mut node_issues = vec![Vec::new(); canvas.nodes().len()];
        for issue in canvas.issues() {
            if let IssueItem::Node { index, .. } = issue.item
                && issue.severity == Severity::Warning
                && let Some(pos) = canvas.nodes().iter().position(|n| n.index == index)
            {
                node_issues[pos].push(issue.kind.to_string());
            }
        }
        Self {
            canvas,
            layout,
            node_issues,
        }
    }
}

/// A canvas file's text parsed and laid out for a shapes-only preview
/// (a `.canvas` embed in the Reading view). Blocking: the background
/// executor only.
pub(crate) fn preview_doc(text: &str) -> Result<Arc<Doc>, String> {
    let canvas = Canvas::parse(text).map_err(|e| e.to_string())?;
    Ok(Arc::new(Doc::new(canvas)))
}

/// Paints `doc` as shapes in `bounds` (groups, edges, every other node as
/// a block, no text: [`LodThresholds::SHAPES_ONLY`]), fitted with
/// `padding` px around, in `theme`'s colours. Returns the number of items
/// painted.
pub(crate) fn paint_preview(
    window: &mut Window,
    doc: &Doc,
    bounds: Bounds<Pixels>,
    padding: f32,
    theme: &crate::theme::Theme,
) -> usize {
    paint::shapes_only(window, doc, bounds, padding, &Colors::of(theme))
}

/// The number of nodes and edges of a previewed canvas.
#[cfg(test)]
pub(crate) fn preview_size(doc: &Doc) -> (usize, usize) {
    (doc.canvas.nodes().len(), doc.canvas.edges().len())
}

/// What the Canvas view asks of its window.
#[derive(Debug, Clone, PartialEq)]
pub enum CanvasEvent {
    /// Open a file card's file: a note scrolled to `fragment`, or any other
    /// file as the window opens files of its kind.
    OpenFile {
        path: PathBuf,
        fragment: Option<Fragment>,
        disposition: Disposition,
    },
    /// A link card was opened: follow its URL as a link of the canvas
    /// file (the window's URL policy applies).
    FollowLink(LinkRequest),
}

/// Where the view is with its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanvasStatus {
    Empty,
    Loading,
    Ready,
    /// The file could not be shown; why.
    Failed(SharedString),
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    start: Point<Pixels>,
    last: Point<Pixels>,
    moved: bool,
}

#[derive(Debug, Clone, Copy)]
struct Animation {
    from: Transform,
    to: Transform,
    start: Instant,
}

/// The view's place in the window and its size, from the last paint.
type Frame = Rc<Cell<Bounds<Pixels>>>;

/// The Canvas view of one tab.
pub struct CanvasView {
    focus: FocusHandle,
    path: Option<PathBuf>,
    vault_root: Option<PathBuf>,
    status: CanvasStatus,
    doc: Option<Rc<Doc>>,
    transform: Transform,
    /// The first fit waits for the view's size.
    fitted: bool,
    /// The view's size at the last render: a new size keeps the centre.
    shown_size: Option<gv_canvas::Size>,
    frame: Frame,
    selected: Option<usize>,
    drag: Option<Drag>,
    animation: Option<Animation>,
    issues_open: bool,
    /// The problems chip's focus: a stop of Tab after the last card.
    issues_focus: FocusHandle,
    minimap: MinimapState,
    cards: Cards,
    /// Items painted straight to the scene in the last frame (groups and
    /// edges), and node elements made in the last render.
    painted: Rc<Cell<usize>>,
    /// Group background images painted in the last frame.
    backgrounds_painted: Rc<Cell<usize>>,
    nodes_drawn: usize,
    /// PDF cards waiting for their first page (node, file). The helper
    /// draws one page at a time, so the view asks for one at a time
    /// instead of holding a background thread per card.
    pdf_queue: VecDeque<(usize, String)>,
    /// A first page is being drawn.
    pdf_busy: bool,
    /// Set when the pages asked for are no longer wanted (another file,
    /// another vault, the view closed); checked before each request.
    pdf_cancel: Arc<AtomicBool>,
    _load: Option<Task<()>>,
}

impl Drop for CanvasView {
    fn drop(&mut self) {
        self.pdf_cancel.store(true, Ordering::Release);
    }
}

impl std::fmt::Debug for CanvasView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasView")
            .field("path", &self.path)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<CanvasEvent> for CanvasView {}

impl Focusable for CanvasView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CanvasView {
    /// A Canvas view showing `path`. File cards' paths are vault paths
    /// from `vault_root` (the canvas's folder without a vault).
    pub fn open(path: PathBuf, vault_root: Option<PathBuf>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let mut view = Self::new(cx);
            view.vault_root = vault_root;
            view.load(path, cx);
            view
        })
    }

    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        let families = FontFamilies::from_app(cx);
        Self {
            // A stop of the window's Tab order, so Tab past the last
            // card goes on to what follows the canvas.
            focus: cx.focus_handle().tab_index(0).tab_stop(true),
            path: None,
            vault_root: None,
            status: CanvasStatus::Empty,
            doc: None,
            transform: Transform::IDENTITY,
            fitted: false,
            shown_size: None,
            frame: Rc::new(Cell::new(Bounds::default())),
            selected: None,
            drag: None,
            animation: None,
            issues_open: false,
            issues_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            minimap: MinimapState::default(),
            cards: Cards::new(&families),
            painted: Rc::new(Cell::new(0)),
            backgrounds_painted: Rc::new(Cell::new(0)),
            nodes_drawn: 0,
            pdf_queue: VecDeque::new(),
            pdf_busy: false,
            pdf_cancel: Arc::new(AtomicBool::new(false)),
            _load: None,
        }
    }

    /// Drops the PDF pages asked for and stops the one being drawn.
    fn cancel_pdfs(&mut self) {
        self.pdf_cancel.store(true, Ordering::Release);
        self.pdf_cancel = Arc::new(AtomicBool::new(false));
        self.pdf_queue.clear();
    }

    /// Sets the vault the file cards' paths are in.
    pub fn set_vault_root(&mut self, root: Option<PathBuf>, cx: &mut Context<'_, Self>) {
        if root != self.vault_root {
            self.vault_root = root;
            self.cancel_pdfs();
            self.cards.clear();
            cx.notify();
        }
    }

    /// Reads and shows `path` (again, after a change on disk).
    pub fn load(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        // The same file again keeps the view where it is.
        if self.path.as_ref() != Some(&path) || self.doc.is_none() {
            self.fitted = false;
            self.selected = None;
            self.doc = None;
        }
        self.path = Some(path.clone());
        self.status = CanvasStatus::Loading;
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| format!("Cannot read the file: {e}"))?;
                    let canvas = Canvas::parse(&text).map_err(|e| e.to_string())?;
                    Ok::<_, String>(Doc::new(canvas))
                })
                .await;
            this.update(cx, |view, cx| view.finish_load(result, cx))
                .ok();
        }));
        cx.notify();
    }

    fn finish_load(&mut self, result: Result<Doc, String>, cx: &mut Context<'_, Self>) {
        self._load = None;
        self.cancel_pdfs();
        self.cards.clear();
        match result {
            Ok(doc) => {
                if self.selected.is_some_and(|s| s >= doc.canvas.nodes().len()) {
                    self.selected = None;
                }
                self.doc = Some(Rc::new(doc));
                self.status = CanvasStatus::Ready;
            }
            Err(why) => {
                tracing::warn!(path = ?self.path, "cannot show canvas: {why}");
                self.doc = None;
                self.status = CanvasStatus::Failed(why.into());
            }
        }
        cx.notify();
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn status(&self) -> &CanvasStatus {
        &self.status
    }

    /// The parsed canvas, once loaded.
    pub fn canvas(&self) -> Option<&Canvas> {
        self.doc.as_deref().map(|d| &d.canvas)
    }

    /// The transform in effect (where an animation is heading, while one
    /// runs).
    pub fn transform(&self) -> Transform {
        self.animation.map_or(self.transform, |a| a.to)
    }

    /// The selected node (a position in [`Canvas::nodes`]).
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    fn root(&self) -> PathBuf {
        self.vault_root
            .clone()
            .or_else(|| {
                self.path
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            })
            .unwrap_or_default()
    }

    fn viewport(&self) -> gv_canvas::Size {
        let b = self.frame.get();
        gv_canvas::Size::new(
            f64::from(f32::from(b.size.width)),
            f64::from(f32::from(b.size.height)),
        )
    }

    /// The items meeting the viewport, bottom to top.
    pub(crate) fn visible_items(&self) -> Vec<ItemId> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        let t = self.transform;
        // Edge labels reach a little beyond their curves.
        let area = t
            .visible_world(self.viewport())
            .inflate(world::LABEL_SIZE * 2.0);
        doc.layout.visible(&area)
    }

    // ----- moving the view ----------------------------------------------------------

    fn set_transform(&mut self, t: Transform, cx: &mut Context<'_, Self>) {
        self.animation = None;
        self.transform = t;
        cx.notify();
    }

    /// Moves the view to `to`, animated unless motion is reduced.
    fn move_to(&mut self, to: Transform, cx: &mut Context<'_, Self>) {
        if cx.reduce_motion() || to == self.transform {
            self.set_transform(to, cx);
            return;
        }
        self.animation = Some(Animation {
            from: self.transform,
            to,
            start: Instant::now(),
        });
        cx.notify();
    }

    /// Advances a running animation for this frame. Reduced motion, turned
    /// on while it runs, ends it at its target.
    fn step_animation(&mut self, reduce_motion: bool, window: &mut Window) {
        let Some(a) = self.animation else {
            return;
        };
        let t = a.start.elapsed().as_secs_f64() / ANIMATION.as_secs_f64();
        if t >= 1.0 || reduce_motion {
            self.transform = a.to;
            self.animation = None;
            return;
        }
        // Ease out (cubic).
        let e = 1.0 - (1.0 - t).powi(3);
        let lerp = |a: f64, b: f64| a + (b - a) * e;
        // Zoom moves geometrically so the motion looks even.
        let zoom = (a.from.zoom.ln() + (a.to.zoom.ln() - a.from.zoom.ln()) * e).exp();
        self.transform = Transform {
            zoom,
            pan: gv_canvas::Point::new(
                lerp(a.from.pan.x, a.to.pan.x),
                lerp(a.from.pan.y, a.to.pan.y),
            ),
        };
        window.request_animation_frame();
    }

    fn zoom_about(&mut self, anchor: gv_canvas::Point, factor: f64, cx: &mut Context<'_, Self>) {
        let t = self
            .transform()
            .zoomed_at(anchor, factor, DEFAULT_MIN_ZOOM, DEFAULT_MAX_ZOOM);
        self.set_transform(t, cx);
    }

    fn center(&self) -> gv_canvas::Point {
        let v = self.viewport();
        gv_canvas::Point::new(v.width / 2.0, v.height / 2.0)
    }

    /// Shows the whole canvas.
    pub fn fit(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(doc) = &self.doc {
            let to = doc.layout.fit(self.viewport(), &FitOptions::default());
            self.move_to(to, cx);
        }
    }

    /// Pans so node `node` is in view, if it is not.
    fn reveal(&mut self, node: usize, cx: &mut Context<'_, Self>) {
        let Some(doc) = &self.doc else {
            return;
        };
        let t = self.transform();
        let r = doc.layout.node_rect(node);
        let shown = t.world_rect_to_screen(&r);
        let v = self.viewport();
        let m = REVEAL_MARGIN;
        let inside = shown.x >= m
            && shown.y >= m
            && shown.right() <= v.width - m
            && shown.bottom() <= v.height - m;
        if !inside {
            let to = doc
                .layout
                .focus_node(node, t.zoom, v, &FitOptions::default());
            self.move_to(to, cx);
        }
    }

    fn pan_by(&mut self, fx: f64, fy: f64, cx: &mut Context<'_, Self>) {
        let v = self.viewport();
        let t = self.transform().panned(fx * v.width, fy * v.height);
        self.set_transform(t, cx);
    }

    // ----- selection and opening ----------------------------------------------------

    fn select(&mut self, node: Option<usize>, cx: &mut Context<'_, Self>) {
        self.selected = node;
        if let Some(n) = node {
            self.reveal(n, cx);
        }
        cx.notify();
    }

    /// Tab (`forward`) or Shift+Tab: the next or previous card in reading
    /// order, the problems chip (when the file has problems) after the last
    /// card. Past the last stop (before the first card) the key is not
    /// handled (`false`), so the window's focus order leaves the canvas;
    /// the selection is cleared so coming back starts again at an end.
    fn tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        let Some(doc) = self.doc.clone() else {
            return false;
        };
        let chip = !doc.canvas.issues().is_empty();
        let order = doc.layout.reading_order();
        if chip && self.issues_focus.is_focused(window) {
            let Some(&last) = order.last().filter(|_| !forward) else {
                return false;
            };
            window.focus(&self.focus, cx);
            self.select(Some(last), cx);
            return true;
        }
        let edge = if forward { order.last() } else { order.first() };
        let at_edge = match self.selected {
            Some(s) => edge == Some(&s),
            None => order.is_empty(),
        };
        if at_edge {
            self.selected = None;
            cx.notify();
            if chip && forward {
                window.focus(&self.issues_focus, cx);
                return true;
            }
            return false;
        }
        let next = if forward {
            doc.layout.next_node(self.selected)
        } else {
            doc.layout.prev_node(self.selected)
        };
        self.select(next, cx);
        true
    }

    fn select_toward(&mut self, dir: Direction, cx: &mut Context<'_, Self>) {
        let Some(doc) = &self.doc else {
            return;
        };
        let next = match self.selected {
            None => doc.layout.next_node(None),
            Some(s) => doc.layout.neighbor(s, dir).or(Some(s)),
        };
        self.select(next, cx);
    }

    /// Opens node `node`'s file or link; other nodes do nothing.
    pub fn open_node(&mut self, node: usize, disposition: Disposition, cx: &mut Context<'_, Self>) {
        let Some(doc) = &self.doc else {
            return;
        };
        let Some(n) = doc.canvas.nodes().get(node) else {
            return;
        };
        match &n.kind {
            NodeKind::File {
                file, subpath_raw, ..
            } => {
                let Some(path) = cards::file_path(&self.root(), file) else {
                    tracing::warn!(file, "canvas: a file card outside the vault is not opened");
                    return;
                };
                let fragment = subpath_raw
                    .as_deref()
                    .map(|s| s.strip_prefix('#').unwrap_or(s))
                    .filter(|s| !s.is_empty())
                    .map(Fragment::from_wikilink);
                cx.emit(CanvasEvent::OpenFile {
                    path,
                    fragment,
                    disposition,
                });
            }
            NodeKind::Link { url } => {
                cx.emit(CanvasEvent::FollowLink(LinkRequest {
                    destination: Arc::from(url.as_str()),
                    kind: gv_model::LinkKind::UriAutolink,
                    disposition,
                    range: None,
                    from: None,
                }));
            }
            _ => {}
        }
    }

    /// A click at window position `at`.
    fn click(&mut self, at: Point<Pixels>, disposition: Disposition, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let origin = self.frame.get().origin;
        let local = gv_canvas::Point::new(
            f64::from(f32::from(at.x - origin.x)),
            f64::from(f32::from(at.y - origin.y)),
        );
        let t = self.transform();
        let world = t.screen_to_world(local);
        match doc.layout.hit_test(world, screen::HIT_TOLERANCE / t.zoom) {
            Some(ItemId::Node(n)) => {
                if disposition == Disposition::InPlace {
                    self.selected = Some(n);
                }
                self.open_node(n, disposition, cx);
            }
            Some(ItemId::Edge(_)) | None => {
                if disposition == Disposition::InPlace {
                    self.selected = None;
                }
            }
        }
        cx.notify();
    }

    // ----- input handlers -----------------------------------------------------------

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        window.focus(&self.focus, cx);
        self.end_minimap_drag();
        if let Some(a) = self.animation.take() {
            self.transform = a.to;
        }
        self.drag = Some(Drag {
            start: e.position,
            last: e.position,
            moved: false,
        });
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(mut d) = self.drag else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        if !d.moved {
            let off = e.position - d.start;
            let dist = f32::from(off.x).hypot(f32::from(off.y));
            if dist <= DRAG_SLOP {
                return;
            }
            d.moved = true;
        }
        let delta = e.position - d.last;
        d.last = e.position;
        self.drag = Some(d);
        let t = self
            .transform
            .panned(f64::from(f32::from(delta.x)), f64::from(f32::from(delta.y)));
        self.set_transform(t, cx);
    }

    fn mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(d) = self.drag.take() else {
            return;
        };
        if !d.moved {
            let disposition = if e.modifiers.secondary(cx) {
                Disposition::NewTab
            } else {
                Disposition::InPlace
            };
            self.click(e.position, disposition, cx);
        }
    }

    fn middle_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.click(e.position, Disposition::Background, cx);
    }

    fn scroll(&mut self, e: &ScrollEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let d = e.delta.pixel_delta(px(LINE_PX));
        let (dx, dy) = (f64::from(f32::from(d.x)), f64::from(f32::from(d.y)));
        if zoom_modifier(&e.modifiers) {
            let origin = self.frame.get().origin;
            let anchor = gv_canvas::Point::new(
                f64::from(f32::from(e.position.x - origin.x)),
                f64::from(f32::from(e.position.y - origin.y)),
            );
            self.zoom_about(anchor, (dy * ZOOM_PER_PX).exp(), cx);
        } else if e.modifiers.shift && dx == 0.0 {
            let t = self.transform().panned(dy, 0.0);
            self.set_transform(t, cx);
        } else {
            let t = self.transform().panned(dx, dy);
            self.set_transform(t, cx);
        }
        cx.stop_propagation();
    }

    fn pinch(&mut self, e: &PinchEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        let origin = self.frame.get().origin;
        let anchor = gv_canvas::Point::new(
            f64::from(f32::from(e.position.x - origin.x)),
            f64::from(f32::from(e.position.y - origin.y)),
        );
        self.zoom_about(anchor, f64::from(1.0 + e.delta).max(0.01), cx);
    }

    // ----- drawing -------------------------------------------------------------------

    /// Starts loading the previews of file cards `nodes`. PDFs whose
    /// first page is drawn wait in the view's queue.
    fn load_previews(&mut self, nodes: Vec<usize>, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let root = self.root();
        let animate = !cx.reduce_motion();
        let pages = crate::export::pdf_engine(cx).is_some_and(|e| e.draws_pages());
        for node in nodes {
            let NodeKind::File { file, subpath, .. } = &doc.canvas.nodes()[node].kind else {
                continue;
            };
            self.cards.start_preview(node);
            if pages && cards::is_pdf(file) {
                self.pdf_queue.push_back((node, file.clone()));
                continue;
            }
            let file = file.clone();
            let subpath = subpath.clone();
            let root = root.clone();
            let profile = self.cards.profile();
            let weak = Rc::downgrade(&doc);
            cx.spawn(async move |this, cx| {
                let loaded = cx
                    .background_spawn(async move {
                        cards::load_file(&root, &file, subpath.as_ref(), &profile, animate, None)
                    })
                    .await;
                this.update(cx, |view, cx| {
                    // Only for the canvas it was asked for.
                    let same = weak
                        .upgrade()
                        .is_some_and(|d| view.doc.as_ref().is_some_and(|v| Rc::ptr_eq(&d, v)));
                    if same {
                        view.cards.set_preview(node, loaded);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        self.next_pdf(cx);
    }

    /// Draws the first page of the next queued PDF card, unless one is
    /// being drawn: one background job at a time, so cards never hold
    /// several background threads waiting for the helper.
    fn next_pdf(&mut self, cx: &mut Context<'_, Self>) {
        if self.pdf_busy {
            return;
        }
        let Some(doc) = self.doc.clone() else {
            self.pdf_queue.clear();
            return;
        };
        let Some(engine) = crate::export::pdf_engine(cx).filter(|e| e.draws_pages()) else {
            // The engine went: what is left shows as file cards.
            for (node, _) in std::mem::take(&mut self.pdf_queue) {
                self.cards
                    .set_preview(node, cards::Loaded::Other("PDF".into()));
            }
            return;
        };
        let Some((node, file)) = self.pdf_queue.pop_front() else {
            return;
        };
        self.pdf_busy = true;
        let root = self.root();
        let profile = self.cards.profile();
        let cancel = self.pdf_cancel.clone();
        let stale = cancel.clone();
        let weak = Rc::downgrade(&doc);
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    if cancel.load(Ordering::Acquire) {
                        return None;
                    }
                    Some(cards::load_file(
                        &root,
                        &file,
                        None,
                        &profile,
                        false,
                        Some((&*engine, &*cancel)),
                    ))
                })
                .await;
            this.update(cx, |view, cx| {
                view.pdf_busy = false;
                let same = weak
                    .upgrade()
                    .is_some_and(|d| view.doc.as_ref().is_some_and(|v| Rc::ptr_eq(&d, v)));
                if let Some(loaded) = loaded
                    && same
                    && !stale.load(Ordering::Acquire)
                {
                    view.cards.set_preview(node, loaded);
                    cx.notify();
                }
                view.next_pdf(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Starts loading the background images of groups `nodes`.
    fn load_backgrounds(&mut self, nodes: Vec<usize>, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let root = self.root();
        for node in nodes {
            let NodeKind::Group {
                background: Some(background),
                ..
            } = &doc.canvas.nodes()[node].kind
            else {
                continue;
            };
            self.cards.start_background(node);
            let background = background.clone();
            let root = root.clone();
            let weak = Rc::downgrade(&doc);
            cx.spawn(async move |this, cx| {
                let loaded = cx
                    .background_spawn(async move { cards::load_background(&root, &background) })
                    .await;
                this.update(cx, |view, cx| {
                    let same = weak
                        .upgrade()
                        .is_some_and(|d| view.doc.as_ref().is_some_and(|v| Rc::ptr_eq(&d, v)));
                    if same {
                        view.cards.set_background(node, loaded);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// Starts the highlighting and diagram jobs the cards asked for while
    /// they were built; as each lands, the cards that wait for it are
    /// built again.
    fn start_jobs(&mut self, cx: &mut Context<'_, Self>) {
        for job in self.cards.take_jobs() {
            match job {
                Job::Highlight(key, code) => {
                    cx.spawn(async move |this, cx| {
                        cx.background_spawn(async move {
                            crate::reader::code_highlight::highlight(key, &code);
                        })
                        .await;
                        this.update(cx, |view, cx| {
                            if view.cards.highlighted(&key) {
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
                Job::Diagram(request) => {
                    cx.spawn(async move |this, cx| {
                        let (request, outcome) = cx
                            .background_executor()
                            .spawn_with_priority(gpui::Priority::Low, async move {
                                let outcome = crate::reader::diagram::run(&request);
                                (request, outcome)
                            })
                            .await;
                        this.update(cx, |view, cx| {
                            if view.cards.diagram_landed(&request, outcome) {
                                cx.notify();
                            }
                        })
                        .ok();
                    })
                    .detach();
                }
            }
        }
    }

    /// The content of a card `width` wide (world units) holding Markdown.
    fn markdown(
        &mut self,
        node: usize,
        parsed: &Parsed,
        width: f64,
        zoom: f64,
        palette: &Arc<Palette>,
        colors: &Colors,
    ) -> AnyElement {
        let bucket = cards::bucket(zoom);
        let built = self.cards.built(node, parsed, bucket);
        // Lengths built at the bucket's scale, placed at the exact zoom.
        let k = (zoom as f32) / cards::bucket_scale(bucket);
        let pad = (world::CARD_PAD * zoom) as f32;
        let code_pad = built.body_size * cards::CODE_PAD_EM;
        let code_box = |indent: f32, gap: f32| {
            div()
                .flex_none()
                .mt(px(gap))
                .ml(px(indent * k))
                .p(px(code_pad))
                .rounded(px(built.body_size * 0.3))
                .bg(colors.group_fill)
        };
        let mut column = div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .p(px(pad))
            .flex()
            .flex_col()
            .overflow_hidden();
        for (i, block) in built.blocks.iter().enumerate() {
            match block {
                Block::Text {
                    block,
                    indent,
                    gap,
                    marker,
                } => {
                    let gap = if i == 0 { 0.0 } else { gap * k };
                    let mut row = div()
                        .relative()
                        .flex_none()
                        .pt(px(gap))
                        .pl(px(indent * k))
                        .child(InlineParagraph::new(
                            block.clone(),
                            self.cards.text.clone(),
                            palette.clone(),
                        ));
                    let size = built.body_size;
                    match marker {
                        Some(Marker::Text(m)) => {
                            row = row.child(
                                div()
                                    .absolute()
                                    .top(px(gap))
                                    .left(px((indent * k - size * 1.4).max(0.0)))
                                    .text_size(px(size))
                                    .text_color(colors.label)
                                    .child(m.clone()),
                            );
                        }
                        // Read-only, drawn like the Reading view's.
                        Some(Marker::Task { checked }) => {
                            let side = size * TASK_BOX_EM;
                            let line = built.body_line * k;
                            let checkbox = div()
                                .absolute()
                                .top(px(gap + (line - side) / 2.0))
                                .left(px((indent * k - size * 1.4).max(0.0)))
                                .size(px(side))
                                .rounded(px(side * TASK_BOX_RADIUS))
                                .flex()
                                .items_center()
                                .justify_center();
                            let checkbox = if *checked {
                                checkbox
                                    .bg(colors.accent)
                                    .text_color(colors.accent_on)
                                    .child(Icon::MenuCheck.sized(side))
                            } else {
                                checkbox
                                    .bg(colors.card)
                                    .border_1()
                                    .border_color(colors.card_border)
                            };
                            row = row.child(checkbox);
                        }
                        None => {}
                    }
                    column = column.child(row);
                }
                Block::Code { block, indent, gap } => {
                    let gap = if i == 0 { 0.0 } else { gap * k };
                    column = column.child(code_box(*indent, gap).child(InlineParagraph::new(
                        block.clone(),
                        self.cards.text.clone(),
                        palette.clone(),
                    )));
                }
                Block::Diagram { block, indent, gap } => {
                    let gap = if i == 0 { 0.0 } else { gap * k };
                    // Scaled down to the card's width, never up.
                    let room = (width * zoom) as f32 - 2.0 * pad - indent * k - 2.0 * code_pad;
                    block.borrow_mut().fit_boxes(room);
                    column = column.child(code_box(*indent, gap).child(InlineParagraph::new(
                        block.clone(),
                        self.cards.text.clone(),
                        palette.clone(),
                    )));
                }
                Block::Rule { gap } => {
                    column = column.child(
                        div()
                            .flex_none()
                            .mt(px(gap * k))
                            .mb(px(gap * k))
                            .h(px(1.0))
                            .bg(colors.card_border),
                    );
                }
            }
        }
        column.into_any_element()
    }

    /// The content of a card at full detail.
    fn card_content(
        &mut self,
        doc: &Rc<Doc>,
        node: usize,
        zoom: f64,
        palette: &Arc<Palette>,
        colors: &Colors,
        to_load: &mut Vec<usize>,
    ) -> Option<AnyElement> {
        let n = &doc.canvas.nodes()[node];
        let text_px = (world::LABEL_SIZE * zoom) as f32;
        let pad = (world::CARD_PAD * zoom) as f32;
        match &n.kind {
            NodeKind::Text { text } => {
                let parsed = self.cards.text_card(node, text);
                Some(self.markdown(node, &parsed, n.rect.width, zoom, palette, colors))
            }
            NodeKind::File { file, .. } => {
                let name = file.rsplit('/').next().unwrap_or(file).to_owned();
                match self.cards.preview(node).cloned() {
                    None => {
                        to_load.push(node);
                        None
                    }
                    Some(Preview::Pending) => None,
                    Some(Preview::Markdown(parsed)) => {
                        Some(self.markdown(node, &parsed, n.rect.width, zoom, palette, colors))
                    }
                    Some(Preview::Image(image)) => Some(
                        img(ImageSource::Render(image))
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .object_fit(ObjectFit::Contain)
                            .into_any_element(),
                    ),
                    Some(Preview::Canvas(inner)) => {
                        let colors = *colors;
                        Some(
                            canvas(
                                |_, _, _| {},
                                move |bounds, (), window, _| {
                                    paint::shapes_only(window, &inner, bounds, pad, &colors);
                                },
                            )
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .into_any_element(),
                        )
                    }
                    Some(Preview::Other(ext)) => Some(file_card(
                        Icon::OtherFile,
                        name.into(),
                        ext,
                        text_px,
                        pad,
                        colors,
                    )),
                    Some(Preview::Missing(why)) => Some(file_card(
                        Icon::NoticeWarning,
                        name.into(),
                        why,
                        text_px,
                        pad,
                        colors,
                    )),
                }
            }
            NodeKind::Link { url } => Some(link_card(url, text_px, pad, colors)),
            NodeKind::Unknown { type_name } => Some(
                div()
                    .p(px(pad))
                    .text_size(px(text_px))
                    .text_color(colors.muted)
                    .child(format!("Unknown card type \u{201c}{type_name}\u{201d}"))
                    .into_any_element(),
            ),
            NodeKind::Group { .. } => None,
        }
    }

    /// The elements over the painted layers: nodes, labels, selection.
    fn overlays(
        &mut self,
        doc: &Rc<Doc>,
        visible: &[ItemId],
        palette: &Arc<Palette>,
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let t = self.transform;
        let lod = LodThresholds::default();
        let zoom = t.zoom;
        let label_px = (world::LABEL_SIZE * zoom) as f32;
        let show_labels = label_px >= screen::MIN_LABEL;
        let radius = px((world::RADIUS * zoom) as f32);
        let border = px(paint::stroke(world::BORDER, zoom));
        let mut labels = Vec::new();
        let mut nodes = Vec::new();
        let mut to_load = Vec::new();
        let mut backgrounds = Vec::new();
        self.nodes_drawn = 0;
        for &item in visible {
            match item {
                ItemId::Edge(e) => {
                    let Some(label) = &doc.canvas.edges()[e].label else {
                        continue;
                    };
                    if !show_labels || lod.edge_detail(zoom) != Detail::Full {
                        continue;
                    }
                    let at = t.world_to_screen(doc.layout.edge_geometry(e).label_anchor);
                    let w = 4000.0_f32.min(label_px * 40.0);
                    labels.push(
                        div()
                            .absolute()
                            .left(px(at.x as f32 - w / 2.0))
                            .top(px(at.y as f32 - label_px * 0.9))
                            .w(px(w))
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .px(px(label_px * 0.4))
                                    .rounded(px(label_px * 0.3))
                                    .bg(colors.background)
                                    .text_size(px(label_px))
                                    .text_color(colors.label)
                                    .whitespace_nowrap()
                                    .child(label.clone()),
                            )
                            .into_any_element(),
                    );
                }
                ItemId::Node(i) => {
                    let n = &doc.canvas.nodes()[i];
                    let detail = lod.detail(&n.rect, zoom);
                    if detail == Detail::Hidden {
                        continue;
                    }
                    let s = t.world_rect_to_screen(&n.rect);
                    let label = match &n.kind {
                        NodeKind::Group { label, .. } => label.clone(),
                        NodeKind::File { file, .. } => {
                            Some(file.rsplit('/').next().unwrap_or(file).to_owned())
                        }
                        _ => None,
                    };
                    if let Some(label) = label
                        && show_labels
                        && detail == Detail::Full
                    {
                        labels.push(
                            div()
                                .absolute()
                                .left(px(s.x as f32))
                                .top(px(s.y as f32 - label_px * 1.6))
                                .max_w(px((s.width as f32).max(label_px * 8.0)))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_size(px(label_px))
                                .text_color(colors.label)
                                .child(label)
                                .into_any_element(),
                        );
                    }
                    if let NodeKind::Group {
                        background: Some(_),
                        ..
                    } = &n.kind
                        && self.cards.background(i).is_none()
                    {
                        backgrounds.push(i);
                    }
                    if n.kind.is_group() {
                        continue;
                    }
                    self.nodes_drawn += 1;
                    let mut frame = div()
                        .absolute()
                        .left(px(s.x as f32))
                        .top(px(s.y as f32))
                        .w(px(s.width as f32))
                        .h(px(s.height as f32))
                        .rounded(radius)
                        .overflow_hidden();
                    if detail == Detail::Blocks {
                        frame = frame.bg(colors.block(n.color));
                    } else {
                        let (b, fill) = colors.frame(n.color, false);
                        // A tinted card keeps the page under its tint.
                        frame = frame.bg(colors.card).border(border).border_color(b);
                        let mut inner = div().size_full().relative().bg(fill);
                        if let Some(content) =
                            self.card_content(doc, i, zoom, palette, colors, &mut to_load)
                        {
                            inner = inner.child(content);
                        }
                        if !doc.node_issues[i].is_empty() {
                            let icon = (label_px * 0.9).max(8.0);
                            inner = inner.child(
                                div()
                                    .absolute()
                                    .top(px(icon * 0.4))
                                    .right(px(icon * 0.4))
                                    .text_color(colors.warning)
                                    .child(Icon::NoticeWarning.sized(icon)),
                            );
                        }
                        frame = frame.child(inner);
                    }
                    nodes.push(frame.into_any_element());
                    if self.selected == Some(i) {
                        let ring = screen::RING;
                        nodes.push(
                            div()
                                .absolute()
                                .left(px(s.x as f32 - ring * 2.0))
                                .top(px(s.y as f32 - ring * 2.0))
                                .w(px(s.width as f32 + ring * 4.0))
                                .h(px(s.height as f32 + ring * 4.0))
                                .rounded(radius + px(ring * 2.0))
                                .border(px(ring))
                                .border_color(colors.accent)
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        if !to_load.is_empty() {
            self.load_previews(to_load, cx);
        }
        if !backgrounds.is_empty() {
            self.load_backgrounds(backgrounds, cx);
        }
        self.start_jobs(cx);
        // Groups' and edges' labels sit under the cards, as their shapes do.
        labels.extend(nodes);
        labels
    }

    /// The issues chip and list, and the zoom readout.
    fn controls(&self, colors: &Colors, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let text_size = px(crate::metrics::text::UI_SMALL.size);
        let mut bar = div()
            .absolute()
            .bottom(px(8.0))
            .left(px(8.0))
            .right(px(8.0))
            .flex()
            .items_end()
            .justify_between()
            .text_size(text_size);
        let issues = self.doc.as_ref().map_or(&[][..], |d| d.canvas.issues());
        let left = if issues.is_empty() {
            div()
        } else {
            let n = issues.len();
            let label = if n == 1 {
                "1 problem".to_owned()
            } else {
                format!("{n} problems")
            };
            let chip = div()
                .id("canvas-issues")
                .track_focus(&self.issues_focus)
                .debug_selector(|| "canvas-issues".into())
                .a11y_button(label.clone())
                .aria_expanded(self.issues_open)
                .keyboard_control(theme)
                .flex()
                .items_center()
                .gap(px(4.0))
                .px(px(6.0))
                .h(px(crate::metrics::size::CONTROL))
                .rounded(px(crate::metrics::radius::CONTROL))
                .bg(theme.status.warning_tint)
                .text_color(theme.text.primary)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.issues_open = !this.issues_open;
                    cx.notify();
                }))
                .child(
                    div()
                        .text_color(colors.warning)
                        .child(Icon::NoticeWarning.sized(12.0)),
                )
                .child(label)
                .accessible();
            let mut col = div().flex().flex_col().items_start().gap(px(4.0));
            if self.issues_open {
                let mut list = div()
                    .id("canvas-issue-list")
                    .debug_selector(|| "canvas-issue-list".into())
                    .a11y_group(Role::List, "Problems")
                    .max_h(px(240.0))
                    .max_w(px(520.0))
                    .overflow_y_scroll()
                    .p(px(8.0))
                    .rounded(px(crate::metrics::radius::POPOVER))
                    .bg(theme.surface.raised)
                    .border_1()
                    .border_color(theme.border.strong)
                    .text_color(theme.text.primary)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_scroll(|_, _, cx| cx.stop_propagation());
                for (i, issue) in issues.iter().enumerate() {
                    let color = match issue.severity {
                        Severity::Error => colors.error,
                        Severity::Warning => colors.label,
                    };
                    let text = issue.to_string();
                    list = list.child(
                        div()
                            .id(("canvas-issue", i))
                            .a11y_list_item(text.clone(), i, n)
                            .text_color(color)
                            .child(text)
                            .accessible(),
                    );
                }
                col = col.child(list.accessible());
            }
            col.child(chip)
        };
        let zoom = format!("{:.0}%", self.transform.zoom * 100.0);
        bar = bar.child(left).child(
            div()
                .id("canvas-zoom")
                .debug_selector(|| "canvas-zoom".into())
                .role(Role::Status)
                .aria_label(format!("Zoom {zoom}"))
                .px(px(6.0))
                .rounded(px(crate::metrics::radius::CONTROL))
                .bg(colors.background)
                .text_color(theme.text.secondary)
                .child(zoom)
                .accessible()
                .live(Live::Polite),
        );
        bar.into_any_element()
    }
}

/// Whether a wheel event with `m` zooms instead of panning.
fn zoom_modifier(m: &Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform || m.control
    } else {
        m.control
    }
}

/// A file card without a preview: an icon, the name and a note.
fn file_card(
    icon: Icon,
    name: SharedString,
    note: SharedString,
    text_px: f32,
    pad: f32,
    colors: &Colors,
) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .p(px(pad))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(text_px * 0.4))
        .overflow_hidden()
        .text_size(px(text_px))
        .child(
            div()
                .text_color(colors.label)
                .child(icon.sized(text_px * 2.0)),
        )
        .child(div().text_color(colors.text).child(name))
        .child(div().text_color(colors.muted).child(note))
        .into_any_element()
}

/// A link card: the URL, never fetched.
fn link_card(url: &str, text_px: f32, pad: f32, colors: &Colors) -> AnyElement {
    let host = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(url)
        .to_owned();
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .p(px(pad))
        .flex()
        .flex_col()
        .gap(px(text_px * 0.3))
        .overflow_hidden()
        .text_size(px(text_px))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(text_px * 0.4))
                .text_color(colors.text)
                .child(
                    div().text_color(colors.label).child(
                        gpui::component::Icon::new(gpui::assets::IconName::Link)
                            .size(px(text_px * 1.1)),
                    ),
                )
                .child(host),
        )
        .child(div().text_color(colors.muted).child(url.to_owned()))
        .into_any_element()
}

impl Render for CanvasView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.step_animation(cx.reduce_motion(), window);
        let theme = Appearance::theme(cx);
        let colors = Colors::of(theme);
        let palette = Palette::of(Appearance::current_id(cx));
        let viewport = self.viewport();
        // A new size (the sidebar closed, the window resized) keeps the
        // world point at the view's centre where it was.
        if let Some(old) = self.shown_size.replace(viewport)
            && self.fitted
            && old.width > 0.0
            && old.height > 0.0
            && viewport.width > 0.0
            && viewport.height > 0.0
        {
            self.transform.pan.x += (viewport.width - old.width) / 2.0;
            self.transform.pan.y += (viewport.height - old.height) / 2.0;
        }
        if let Some(doc) = &self.doc
            && !self.fitted
            && viewport.width > 0.0
            && viewport.height > 0.0
        {
            self.transform = doc.layout.fit(viewport, &FitOptions::default());
            self.fitted = true;
        }

        let mut root = div()
            .id("canvas-view")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(colors.background)
            .font_family(Appearance::interface_font(cx))
            .text_color(colors.text)
            .on_action(cx.listener(|this, _: &NextCard, window, cx| {
                if !this.tab(true, window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &PreviousCard, window, cx| {
                if !this.tab(false, window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(
                cx.listener(|this, _: &CardLeft, _, cx| this.select_toward(Direction::Left, cx)),
            )
            .on_action(
                cx.listener(|this, _: &CardRight, _, cx| this.select_toward(Direction::Right, cx)),
            )
            .on_action(cx.listener(|this, _: &CardUp, _, cx| this.select_toward(Direction::Up, cx)))
            .on_action(
                cx.listener(|this, _: &CardDown, _, cx| this.select_toward(Direction::Down, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenCard, window, cx| {
                if this.issues_focus.is_focused(window) {
                    // Enter works the problems chip, as a button.
                    cx.propagate();
                    return;
                }
                if let Some(n) = this.selected {
                    this.open_node(n, Disposition::InPlace, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenCardInNewTab, _, cx| {
                if let Some(n) = this.selected {
                    this.open_node(n, Disposition::NewTab, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ZoomToFit, _, cx| this.fit(cx)))
            .on_action(
                cx.listener(|this, _: &PanLeft, _, cx| this.pan_by(PAN_KEY_FRACTION, 0.0, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PanRight, _, cx| this.pan_by(-PAN_KEY_FRACTION, 0.0, cx)),
            )
            .on_action(cx.listener(|this, _: &PanUp, _, cx| this.pan_by(0.0, PAN_KEY_FRACTION, cx)))
            .on_action(
                cx.listener(|this, _: &PanDown, _, cx| this.pan_by(0.0, -PAN_KEY_FRACTION, cx)),
            )
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| {
                this.selected = None;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| {
                let c = this.center();
                this.zoom_about(c, ZOOM_STEP, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| {
                let c = this.center();
                this.zoom_about(c, 1.0 / ZOOM_STEP, cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| {
                let c = this.center();
                let z = this.transform().zoom;
                this.zoom_about(c, 1.0 / z, cx);
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::middle_up))
            .on_scroll(cx.listener(Self::scroll))
            .on_pinch(cx.listener(Self::pinch));

        // The painted layers: groups and edges. The closure also records
        // where the view is, for the next frame's culling and the pointer.
        let frame = self.frame.clone();
        let painted = self.painted.clone();
        let backgrounds_painted = self.backgrounds_painted.clone();
        let images = self.cards.ready_backgrounds();
        let doc = self.doc.clone();
        let t = self.transform;
        let entity = cx.entity_id();
        root = root.child(
            canvas(
                move |bounds, _, cx| {
                    if frame.get() != bounds {
                        let resized = frame.get().size != bounds.size;
                        frame.set(bounds);
                        if resized {
                            cx.notify(entity);
                        }
                    }
                },
                move |bounds, (), window, _| {
                    painted.set(0);
                    backgrounds_painted.set(0);
                    let Some(doc) = doc else {
                        return;
                    };
                    let mut with_image = 0;
                    let viewport = gv_canvas::Size::new(
                        f64::from(f32::from(bounds.size.width)),
                        f64::from(f32::from(bounds.size.height)),
                    );
                    let area = t.visible_world(viewport).inflate(world::LABEL_SIZE * 2.0);
                    let lod = LodThresholds::default();
                    let mut count = 0;
                    for item in doc.layout.visible(&area) {
                        match item {
                            ItemId::Edge(e) => {
                                paint::edge(
                                    window,
                                    &doc,
                                    e,
                                    &t,
                                    bounds.origin,
                                    lod.edge_detail(t.zoom),
                                    &colors,
                                );
                                count += 1;
                            }
                            ItemId::Node(n) if doc.canvas.nodes()[n].kind.is_group() => {
                                if lod.detail(&doc.canvas.nodes()[n].rect, t.zoom) != Detail::Hidden
                                {
                                    if paint::group(
                                        window,
                                        &doc,
                                        n,
                                        &t,
                                        bounds.origin,
                                        &colors,
                                        images.get(&n),
                                    ) {
                                        with_image += 1;
                                    }
                                    count += 1;
                                }
                            }
                            ItemId::Node(_) => {}
                        }
                    }
                    painted.set(count);
                    backgrounds_painted.set(with_image);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        );

        match (&self.status, self.doc.clone()) {
            (CanvasStatus::Ready, Some(doc)) => {
                root = root.child(self.card_list(&doc, cx));
                if viewport.width > 0.0 {
                    let visible = self.visible_items();
                    let overlays = self.overlays(&doc, &visible, &palette, &colors, cx);
                    root = root.children(overlays);
                    if let Some(map) = self.layout_minimap(&doc) {
                        root = root.child(self.minimap_element(&doc, map, &colors, cx));
                    }
                } else {
                    self.minimap.map = None;
                }
                if doc.canvas.nodes().is_empty() {
                    root = root.child(centered_note("This canvas is empty.", &colors));
                }
            }
            (CanvasStatus::Failed(why), _) => {
                self.minimap.map = None;
                root = root.child(centered_note(
                    format!("This canvas cannot be shown: {why}"),
                    &colors,
                ));
            }
            _ => {}
        }
        let name = self.path.as_deref().and_then(Path::file_stem).map_or_else(
            || "Canvas".to_owned(),
            |s| format!("Canvas {}", s.to_string_lossy()),
        );
        root.a11y_group(Role::Group, name)
            .child(self.controls(&colors, cx))
            .accessible()
    }
}

fn centered_note(text: impl Into<SharedString>, colors: &Colors) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .text_color(colors.label)
        .child(text.into())
        .into_any_element()
}

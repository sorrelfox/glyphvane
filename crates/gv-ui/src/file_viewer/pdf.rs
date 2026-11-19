// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The built-in PDF viewer (FR-VIEW-11): the pages of a PDF one under the
//! other, drawn by the PDF engine the application installs
//! ([`crate::export::set_pdf_engine`]: the helper process, hayro).
//!
//! - **Layout.** Pages keep the sizes the PDF gives them (crop box,
//!   rotated), with a gap around each. Only the pages in view and one
//!   either side are laid out and drawn; the others are empty space.
//! - **Zoom.** Fit width (the default), fit page, or a scale where 100 %
//!   shows a PDF point as 4/3 of a pixel (96 dpi), in steps from 25 % to
//!   400 %. Zooming keeps the page in view where it was.
//! - **Drawing.** One page at a time, through the engine: the pages in
//!   view first, then the ones either side. A page drawn at a scale more
//!   than 10 % off the one shown is drawn again (the old image is shown
//!   stretched meanwhile). A page that leaves the pages wanted while it
//!   is being drawn is cancelled (the engine stops the helper). Images
//!   are kept up to [`CACHE_BYTES`], the least recently shown dropped
//!   first. A page that cannot be drawn (too large an image, the 5 s
//!   deadline) says why in its place.
//! - **Navigation.** The toolbar's Previous and Next page, the page shown
//!   ("3 / 12", a polite status) and *Go to page…* (Ctrl+G; the Go
//!   popover). Keys ([`key_bindings`]): Up and Down scroll, Left and
//!   Right go to the previous and next page, Page Up, Page Down, Space and
//!   Shift+Space scroll by a screen, Home and End go to the first and the
//!   last page; `=`/`+` and `-` zoom, `0` fits the width, `1` shows
//!   100 %; `view.zoom_in`, `view.zoom_out` and `view.zoom_reset` (fit
//!   width) work too, and Ctrl-wheel (Cmd-wheel on macOS) zooms.
//! - **Place.** The page at the top, how far down it and the zoom
//!   ([`PdfPlace`]) are kept with the session and shown again when the
//!   tab is restored, and when the PDF is read again after a change.
//! - **Text.** The engine reads a page's text with each character's box
//!   ([`PdfEngine::page_text`]: in the helper process, like drawing, with
//!   the same 5 s deadline), one page at a time on the background
//!   executor, only when it is needed: every page while find is open
//!   (from the page shown on, then from the start), and first the pages a
//!   selection starts on, ends on and runs across, or Select all takes. A
//!   page's text is read once and kept with the viewer. Its order is the
//!   PDF's drawing order, except that a page in two columns is read column
//!   by column (the engine's reading order); a page drawn as an image has
//!   none.
//! - **Find.** Ctrl+F opens the find popover of the Reading view
//!   ([`FindBar`]: match case, whole word and regular expression, case
//!   ignored by default) at the top right of the pages, filled with the
//!   selection when that is one line. Each change of the query matches the
//!   text read so far (on the UI thread: a regular expression over a few
//!   KB a page); pages read later add their matches as they come, so the
//!   count ("3 of 17", a polite status, as in the other find popovers)
//!   grows while the text is read, and says " (searching…)" until every
//!   page has been read. At most 10,000 matches are
//!   kept. The current match is the first at or after the place (the
//!   current match, else the page shown); Enter and Shift+Enter in the
//!   field, F3 and Shift+F3 anywhere in the viewer go to the next and
//!   previous one, wrapping around, and scroll it a third down the view
//!   when it is not in view (vertically only). Esc closes the popover and
//!   gives the keyboard back to the pages. The text matched
//!   ([`pdf_text::search`]) reads a line end as a space (a line end for a
//!   regular expression), and a page's end like a line end before the
//!   next page's text when that was read, so a match can run on to the
//!   next page: it counts once, and each part is highlighted on its page.
//!   Soft hyphens are not read; a hyphen at a line end between a letter
//!   and a lowercase letter is read both with and without it
//!   ("infor-⏎mation" is found as "information" and "infor-mation").
//! - **Highlights.** Over the page image: each match in `accent.tint`, the
//!   current one in `accent.line` (the Reading view's find colours), the
//!   selection in `selection.text`, all at `MARK_OPACITY` so the page
//!   shows through. A run of characters is highlighted as one box per
//!   line, from the characters' boxes (0.2 em below the baseline to 0.8 em
//!   above it).
//! - **Selection.** Dragging selects text from the character nearest the
//!   press to the one nearest the pointer, on the page under the pointer
//!   or the nearest one (the pointer is the text cursor over pages whose
//!   text was read and has some); a click drops it. Above or below a page
//!   (in the gap between pages) the selection's end is that page's start
//!   or end. A selection across pages takes the rest of the first page,
//!   every page between and the last page up to its end, each highlighted;
//!   the pages' text is read as the selection reaches them. As in the
//!   Source view, a double click selects the word (Unicode word boundaries
//!   within the line) and a triple click the line, and dragging on from
//!   either grows the selection by words or lines; Shift+click moves the
//!   selection's end, on any page (after Select all, from the page's start,
//!   or its end for a click on an earlier page). While dragging, the
//!   pointer within [`EDGE`] px of the view's top or bottom edge, or
//!   beyond it, scrolls the view every [`AUTOSCROLL_TICK`] (faster the
//!   further in), the selection following. Ctrl+A selects the text of the
//!   page shown; Ctrl+C copies the selection as plain text, the pages'
//!   text separated by a line end, and words split at a soft hyphen at a
//!   line end (or page end) joined; other hyphens are kept. Copying a selection over pages whose
//!   text is still being read waits for it (it is read first) and then
//!   copies; a page that cannot be read adds nothing.
//! - **Accessibility.** The pages are a list named "Pages"; each page laid
//!   out is an image named "Page N of M" with its position in the list.
//!   The find popover's match count is announced (a polite status).
//!
//! Not designed (flagged): the toolbar, the page gap and frame, the
//! per-page messages, the highlights' opacity over the page image, the
//! edge zone and speed of scrolling while selecting.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase,
    Entity, FocusHandle, Focusable, FontWeight, Hsla, ImageSource, InteractiveElement as _,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    ParentElement as _, Pixels, Point, Render, RenderImage, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task, Window,
    canvas, div, img, point, px,
};
use gv_render_tree::Finder;
use serde::{Deserialize, Serialize};

use super::pdf_text::{self, Mark, PageText, PdfMatch, Query, Spot, Unit};

use crate::a11y::{A11yExt as _, AccessibleExt as _, Live, Role, Toggled};
use crate::appearance::Appearance;
use crate::export::{PageRaster, PdfEngine, PdfError, PdfPageText};
use crate::find::{CloseFind, Find, FindBar, FindBarEvent, FindNext, FindPrevious, FindStatus};
use crate::goto::{self, Choice, GotoEvent, GotoKind, GotoRequest};
use crate::icons::Icon;
use crate::keymap::actions::editor::{Copy, SelectAll};
use crate::keymap::actions::navigation::GoToLine;
use crate::keymap::actions::view::{ZoomIn, ZoomOut, ZoomReset};
use crate::metrics::size::infobar as size;
use crate::metrics::spacing::find as find_spacing;
use crate::metrics::spacing::infobar as spacing;
use crate::metrics::{icon, radius, size as sizes, text};
use crate::theme::Theme;

/// Pixels per PDF point at 100 % (96 dpi).
pub const PX_PER_PT: f32 = 96.0 / 72.0;

/// The space around each page, px (not designed).
const PAGE_GAP: f32 = 12.0;

/// Pixels of one arrow-key scroll.
const SCROLL_STEP: f32 = 48.0;

/// What a screen scroll leaves of the previous screen, px.
const SCREEN_OVERLAP: f32 = 48.0;

/// The zoom steps (1.0 is 100 %).
const ZOOM_STEPS: [f32; 13] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0, 3.0, 4.0,
];

/// The page images kept, in bytes.
pub const CACHE_BYTES: usize = 256 << 20;

/// How far off the scale shown a drawn page may be before it is drawn
/// again.
const SCALE_SLACK: f32 = 0.1;

/// The key context of the PDF viewer's pages.
pub(crate) const KEY_CONTEXT: &str = "PdfViewer";

/// The key context of the whole viewer (the pages, the toolbar and the
/// find popover): find's keys.
pub(crate) const DOCUMENT_CONTEXT: &str = "PdfDocument";

/// At most this many matches are kept (as in the Reading view).
pub(crate) const MAX_MATCHES: usize = 10_000;

/// How opaque the match and selection highlights over a page are: the
/// page is an image, so a highlight lets it show through (not designed).
const MARK_OPACITY: f32 = 0.45;

/// How near the view's top or bottom edge the pointer scrolls the view
/// while dragging a selection, px (not designed).
const EDGE: f32 = 32.0;

/// How often the view scrolls while the pointer is near an edge.
const AUTOSCROLL_TICK: Duration = Duration::from_millis(16);

/// The least and the most one tick scrolls, px: half the distance into
/// the edge zone (beyond the edge counts on), within these.
const AUTOSCROLL_STEP: (f32, f32) = (2.0, 40.0);

gpui::actions!(
    pdf_viewer,
    [
        /// Scroll up a little.
        ScrollUp,
        /// Scroll down a little.
        ScrollDown,
        /// Scroll up by a screen.
        ScreenUp,
        /// Scroll down by a screen.
        ScreenDown,
        /// Go to the previous page.
        PreviousPage,
        /// Go to the next page.
        NextPage,
        /// Go to the first page.
        FirstPage,
        /// Go to the last page.
        LastPage,
        /// Ask for a page number.
        GoToPage,
        /// Fit the pages' width in the view.
        FitWidth,
        /// Fit a whole page in the view.
        FitPage,
        /// Show the pages at 100 %.
        ActualSize,
        /// Enlarge the pages.
        ZoomInPages,
        /// Shrink the pages.
        ZoomOutPages,
    ]
);

/// The PDF viewer's keys.
pub fn key_bindings() -> Vec<KeyBinding> {
    let c = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("up", ScrollUp, c),
        KeyBinding::new("down", ScrollDown, c),
        KeyBinding::new("pageup", ScreenUp, c),
        KeyBinding::new("pagedown", ScreenDown, c),
        KeyBinding::new("space", ScreenDown, c),
        KeyBinding::new("shift-space", ScreenUp, c),
        KeyBinding::new("left", PreviousPage, c),
        KeyBinding::new("right", NextPage, c),
        KeyBinding::new("home", FirstPage, c),
        KeyBinding::new("end", LastPage, c),
        KeyBinding::new("ctrl-g", GoToPage, c),
        KeyBinding::new("0", FitWidth, c),
        KeyBinding::new("1", ActualSize, c),
        KeyBinding::new("=", ZoomInPages, c),
        KeyBinding::new("+", ZoomInPages, c),
        KeyBinding::new("shift-=", ZoomInPages, c),
        KeyBinding::new("-", ZoomOutPages, c),
        KeyBinding::new("ctrl-c", Copy, c),
        KeyBinding::new("ctrl-a", SelectAll, c),
        KeyBinding::new("ctrl-f", Find, Some(DOCUMENT_CONTEXT)),
        KeyBinding::new("f3", FindNext, Some(DOCUMENT_CONTEXT)),
        KeyBinding::new("shift-f3", FindPrevious, Some(DOCUMENT_CONTEXT)),
        KeyBinding::new("escape", CloseFind, Some(DOCUMENT_CONTEXT)),
    ]
}

/// How large the pages are shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PdfZoom {
    /// The widest page fills the view's width.
    #[default]
    FitWidth,
    /// The largest page fits in the view.
    FitPage,
    /// A scale: 1.0 is 100 % ([`PX_PER_PT`] pixels per point).
    Scale(f32),
}

impl PdfZoom {
    fn is_fit_width(&self) -> bool {
        *self == Self::FitWidth
    }
}

/// Where a PDF was read (kept with the session): the page at the top,
/// how far down it, and the zoom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct PdfPlace {
    /// From 0.
    pub page: usize,
    /// Points from the page's top to the view's top; the page's top with
    /// the gap above it when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<f32>,
    #[serde(default, skip_serializing_if = "PdfZoom::is_fit_width")]
    pub zoom: PdfZoom,
}

impl PdfPlace {
    /// The start of the PDF at the default zoom: nothing worth keeping.
    pub fn is_start(&self) -> bool {
        self.page == 0 && self.into.is_none_or(|i| i <= 0.0) && self.zoom.is_fit_width()
    }

    /// The place with what a damaged or edited session file may hold made
    /// usable: an offset that is not a number dropped, a scale brought
    /// into the zoom steps' range.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let zoom = match self.zoom {
            PdfZoom::Scale(s) if s.is_finite() && s > 0.0 => {
                PdfZoom::Scale(s.clamp(ZOOM_STEPS[0], ZOOM_STEPS[ZOOM_STEPS.len() - 1]))
            }
            PdfZoom::Scale(_) => PdfZoom::FitWidth,
            zoom => zoom,
        };
        Self {
            page: self.page,
            into: self.into.filter(|i| i.is_finite()),
            zoom,
        }
    }
}

/// A drawn page.
struct Drawn {
    page: usize,
    /// Device pixels per point it was drawn at.
    scale: f32,
    image: Arc<RenderImage>,
    bytes: usize,
}

/// The page being drawn.
struct InFlight {
    page: usize,
    scale: f32,
    generation: u64,
    cancel: Arc<AtomicBool>,
    _task: Task<()>,
}

/// The page whose text is being read.
struct TextRead {
    page: usize,
    _task: Task<()>,
}

/// Find in the PDF.
#[derive(Default)]
struct PdfFind {
    bar: Option<Entity<FindBar>>,
    open: bool,
    query: Option<Query>,
    /// In page order, then text order.
    matches: Vec<PdfMatch>,
    /// More matches than [`MAX_MATCHES`].
    more: bool,
    current: Option<usize>,
    status: FindStatus,
    _events: Option<Subscription>,
}

/// Selected text: from `anchor` (where it started) to `head` (where it
/// ends), possibly on different pages, widened to whole words or lines by
/// `unit` ([`pdf_text::selection`]).
#[derive(Debug, Clone, Copy, PartialEq)]
struct PdfSelection {
    anchor: Mark,
    head: Mark,
    unit: Unit,
}

impl PdfSelection {
    /// The pages it runs over, first to last.
    fn pages(&self) -> std::ops::RangeInclusive<usize> {
        let (a, b) = (self.anchor.page, self.head.page);
        a.min(b)..=a.max(b)
    }
}

/// Where a page was painted, window px, and the scroll offset then.
type Painted = (Bounds<Pixels>, Point<Pixels>);

/// Where the pages are, for one zoom and view size.
#[derive(Debug, Clone, PartialEq)]
struct Layout {
    /// Pixels per point.
    ppt: f32,
    /// Each page's top, px.
    tops: Vec<f32>,
    width: f32,
    height: f32,
}

impl Layout {
    /// The pages that cross `top..bottom`.
    fn crossing(&self, sizes: &[(f32, f32)], top: f32, bottom: f32) -> Option<(usize, usize)> {
        let first = self
            .tops
            .iter()
            .zip(sizes)
            .position(|(t, (_, h))| t + h * self.ppt > top)?;
        let last = self.tops.iter().rposition(|t| *t < bottom)?;
        (first <= last).then_some((first, last))
    }
}

/// The PDF viewer of one file, made once the PDF is read
/// ([`super::FileViewer`] reads it and asks the engine for its pages).
pub struct PdfViewer {
    path: PathBuf,
    bytes: Arc<[u8]>,
    /// The engine's name for this PDF.
    key: u64,
    /// Each page's size, points.
    sizes: Vec<(f32, f32)>,
    engine: Arc<dyn PdfEngine>,
    focus: FocusHandle,
    zoom: PdfZoom,
    scroll: ScrollHandle,
    /// The view's size as last laid out.
    area: Rc<Cell<Bounds<Pixels>>>,
    /// The page shown (from 0), as last drawn.
    current: usize,
    /// A page to go to once the view is laid out, and how far down it
    /// (points; the page's top with the gap above it when `None`).
    pending: Option<(usize, Option<f32>)>,
    drawn: Vec<Drawn>,
    /// Pages that could not be drawn: the scale tried and why.
    failed: HashMap<usize, (f32, SharedString)>,
    in_flight: Option<InFlight>,
    generation: u64,
    /// Pages drawn so far (for tests).
    draws: usize,
    /// Each page's text, once the engine read it.
    texts: Vec<Option<Arc<PageText>>>,
    /// Pages whose text could not be read, and why.
    text_failed: HashMap<usize, SharedString>,
    reading: Option<TextRead>,
    /// Read every page's text (find is open): from `read_from` on, then
    /// from the start.
    read_all: bool,
    read_from: usize,
    /// Pages whose text to read before the others, in order (to select
    /// in them).
    read_first: Vec<usize>,
    /// Stops the text reads when the viewer goes.
    text_cancel: Arc<AtomicBool>,
    find: PdfFind,
    selection: Option<PdfSelection>,
    /// Ctrl+C waits for the text of pages the selection runs over.
    copy_pending: bool,
    /// The left button went down on a page and has not come up.
    dragging: bool,
    /// Where the pointer was last while dragging, window px.
    pointer: Option<Point<Pixels>>,
    /// Scrolls the view while the pointer is near its top or bottom edge
    /// during a drag.
    autoscroll: Option<Task<()>>,
    /// Where each page laid out was painted last, window px, and the
    /// scroll offset then.
    page_bounds: Rc<RefCell<HashMap<usize, Painted>>>,
    /// The view of the pages (without the toolbar), window px.
    view_bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl std::fmt::Debug for PdfViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfViewer")
            .field("path", &self.path)
            .field("pages", &self.sizes.len())
            .field("zoom", &self.zoom)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl Focusable for PdfViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for PdfViewer {
    fn drop(&mut self) {
        if let Some(job) = self.in_flight.take() {
            job.cancel.store(true, Ordering::Release);
        }
        if self.reading.is_some() {
            self.text_cancel.store(true, Ordering::Release);
        }
    }
}

impl PdfViewer {
    /// A viewer of the PDF at `path` (`bytes`, kept by `engine` as `key`)
    /// whose pages have `sizes` (points), at `zoom`, showing page `page`
    /// (from 0) first, `into` points down it. A page past the end is the
    /// last page.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        path: PathBuf,
        bytes: Arc<[u8]>,
        key: u64,
        sizes: Vec<(f32, f32)>,
        engine: Arc<dyn PdfEngine>,
        zoom: PdfZoom,
        page: Option<(usize, Option<f32>)>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let sizes = sizes
            .into_iter()
            .map(|(w, h)| {
                let ok = |v: f32| if v.is_finite() && v > 0.0 { v } else { 1.0 };
                (ok(w), ok(h))
            })
            .collect::<Vec<_>>();
        let last = sizes.len().saturating_sub(1);
        let count = sizes.len();
        let page = page.map(|(page, into)| {
            if page > last {
                (last, None)
            } else {
                (page, into)
            }
        });
        Self {
            path,
            bytes,
            key,
            sizes,
            engine,
            focus: cx.focus_handle(),
            zoom,
            scroll: ScrollHandle::new(),
            area: Rc::default(),
            current: 0,
            pending: page,
            drawn: Vec::new(),
            failed: HashMap::new(),
            in_flight: None,
            generation: 0,
            draws: 0,
            texts: vec![None; count],
            text_failed: HashMap::new(),
            reading: None,
            read_all: false,
            read_from: 0,
            read_first: Vec::new(),
            text_cancel: Arc::new(AtomicBool::new(false)),
            find: PdfFind::default(),
            selection: None,
            copy_pending: false,
            dragging: false,
            pointer: None,
            autoscroll: None,
            page_bounds: Rc::default(),
            view_bounds: Rc::default(),
        }
    }

    /// The number of pages.
    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    /// The page shown, from 0.
    pub fn current_page(&self) -> usize {
        self.pending.map_or(self.current, |(page, _)| page)
    }

    /// Where the PDF is read, for the session.
    pub fn place(&self) -> PdfPlace {
        let (page, into) = match self.pending {
            Some(pending) => pending,
            None if self.view_size().0 > 0.0 => {
                let layout = self.layout();
                let into = layout
                    .tops
                    .get(self.current)
                    .map(|top| (self.view_top() - top) / layout.ppt)
                    .filter(|i| i.is_finite())
                    .map(|i| (i * 100.0).round() / 100.0);
                (self.current, into)
            }
            None => (self.current, None),
        };
        PdfPlace {
            page,
            into,
            zoom: self.zoom,
        }
    }

    pub fn zoom(&self) -> PdfZoom {
        self.zoom
    }

    /// The pages drawn so far.
    pub fn draws(&self) -> usize {
        self.draws
    }

    /// Whether page `page` (from 0) has an image.
    pub fn page_drawn(&self, page: usize) -> bool {
        self.drawn.iter().any(|d| d.page == page)
    }

    /// Why page `page` (from 0) could not be drawn.
    pub fn page_error(&self, page: usize) -> Option<&SharedString> {
        self.failed.get(&page).map(|(_, why)| why)
    }

    /// The page being drawn, from 0.
    pub fn drawing(&self) -> Option<usize> {
        self.in_flight.as_ref().map(|j| j.page)
    }

    // ----- layout ----------------------------------------------------------------------

    fn view_size(&self) -> (f32, f32) {
        let size = self.area.get().size;
        (f32::from(size.width), f32::from(size.height))
    }

    /// Pixels per point at `zoom` in a view of `aw` × `ah`.
    fn ppt_for(&self, zoom: PdfZoom, (aw, ah): (f32, f32)) -> f32 {
        let max_w = self.sizes.iter().map(|s| s.0).fold(1.0, f32::max);
        let max_h = self.sizes.iter().map(|s| s.1).fold(1.0, f32::max);
        let fit_w = (aw - 2.0 * PAGE_GAP) / max_w;
        let fit_h = (ah - 2.0 * PAGE_GAP) / max_h;
        let ppt = match zoom {
            PdfZoom::Scale(z) => z * PX_PER_PT,
            _ if aw <= 0.0 => PX_PER_PT,
            PdfZoom::FitWidth => fit_w,
            PdfZoom::FitPage if ah <= 0.0 => fit_w,
            PdfZoom::FitPage => fit_w.min(fit_h),
        };
        ppt.clamp(ZOOM_STEPS[0] * PX_PER_PT * 0.5, ZOOM_STEPS[12] * PX_PER_PT)
    }

    fn layout_for(&self, zoom: PdfZoom, area: (f32, f32)) -> Layout {
        let ppt = self.ppt_for(zoom, area);
        let mut tops = Vec::with_capacity(self.sizes.len());
        let mut y = PAGE_GAP;
        let mut width: f32 = 0.0;
        for (w, h) in &self.sizes {
            tops.push(y);
            y += h * ppt + PAGE_GAP;
            width = width.max(w * ppt);
        }
        Layout {
            ppt,
            tops,
            width: (width + 2.0 * PAGE_GAP).max(area.0),
            height: y.max(area.1),
        }
    }

    fn layout(&self) -> Layout {
        self.layout_for(self.zoom, self.view_size())
    }

    /// The zoom shown, 1.0 for 100 %.
    pub fn effective_zoom(&self) -> f32 {
        self.layout().ppt / PX_PER_PT
    }

    /// The top of the view in the pages, px.
    fn view_top(&self) -> f32 {
        -f32::from(self.scroll.offset().y)
    }

    /// The page at the view's top quarter (the last page once scrolled to
    /// the end).
    fn page_at(&self, layout: &Layout, top: f32) -> usize {
        let (_, ah) = self.view_size();
        let max = layout.height - ah;
        if ah > 0.0 && max > 0.0 && top >= max - 1.0 {
            return self.sizes.len().saturating_sub(1);
        }
        let probe = top + ah * 0.25;
        layout
            .tops
            .iter()
            .rposition(|t| *t <= probe.max(PAGE_GAP))
            .unwrap_or(0)
    }

    /// The view's top that shows `page`, `into` points down it.
    fn top_of(layout: &Layout, page: usize, into: Option<f32>) -> Option<f32> {
        let top = layout.tops.get(page)?;
        Some(top + into.map_or(-PAGE_GAP / 2.0, |pt| pt * layout.ppt))
    }

    fn set_top(&self, top: f32) {
        self.scroll
            .set_offset(point(self.scroll.offset().x, px(-top.max(0.0))));
    }

    // ----- navigation ------------------------------------------------------------------

    /// Shows page `page` (from 0) at the top, once the view is laid out.
    pub fn go_to(&mut self, page: usize, cx: &mut Context<'_, Self>) {
        let page = page.min(self.sizes.len().saturating_sub(1));
        let (aw, _) = self.view_size();
        if aw <= 0.0 {
            self.pending = Some((page, None));
        } else {
            let layout = self.layout();
            if let Some(top) = Self::top_of(&layout, page, None) {
                self.set_top(top);
            }
            self.current = page;
            self.pending = None;
        }
        cx.notify();
    }

    /// Shows `place`: its zoom, its page at the top and as far down it,
    /// once the view is laid out (a page past the end is the last page).
    pub fn go_to_place(&mut self, place: PdfPlace, cx: &mut Context<'_, Self>) {
        let last = self.sizes.len().saturating_sub(1);
        let (page, into) = if place.page > last {
            (last, None)
        } else {
            (place.page, place.into)
        };
        self.zoom = place.zoom;
        let (aw, _) = self.view_size();
        if aw <= 0.0 {
            self.pending = Some((page, into));
        } else {
            let layout = self.layout();
            if let Some(top) = Self::top_of(&layout, page, into) {
                self.set_top(top);
            }
            self.current = page;
            self.pending = None;
        }
        cx.notify();
    }

    fn step_page(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let at = self.current_page();
        let to = if forward {
            (at + 1).min(self.sizes.len().saturating_sub(1))
        } else {
            at.saturating_sub(1)
        };
        self.go_to(to, cx);
    }

    fn scroll_by(&mut self, dy: f32, cx: &mut Context<'_, Self>) {
        let layout = self.layout();
        let (_, ah) = self.view_size();
        let max = (layout.height - ah).max(0.0);
        self.set_top((self.view_top() + dy).clamp(0.0, max));
        cx.notify();
    }

    fn screen(&mut self, down: bool, cx: &mut Context<'_, Self>) {
        let (_, ah) = self.view_size();
        let step = (ah - SCREEN_OVERLAP).max(SCROLL_STEP);
        self.scroll_by(if down { step } else { -step }, cx);
    }

    /// Asks for a page number (the Go popover); the view follows the
    /// number typed, and goes back on Escape.
    fn ask_page(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let view = cx.entity().downgrade();
        let back = self.current_page();
        let on_event = Rc::new(move |event: GotoEvent, window: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| match event {
                GotoEvent::Preview(Choice::Line(page, _)) => this.go_to(page.saturating_sub(1), cx),
                GotoEvent::Confirm(Choice::Line(page, _)) => {
                    this.go_to(page.saturating_sub(1), cx);
                    this.focus.focus(window, cx);
                }
                GotoEvent::Cancel => {
                    this.go_to(back, cx);
                    this.focus.focus(window, cx);
                }
                _ => {}
            })
            .ok();
        });
        goto::open(
            GotoRequest {
                title: "Go to page".into(),
                kind: GotoKind::Page {
                    current: self.current_page() + 1,
                    count: self.sizes.len(),
                },
                on_event,
            },
            window,
            cx,
        );
    }

    // ----- zoom ------------------------------------------------------------------------

    /// Shows the pages at `zoom`, keeping the page in view where it was.
    pub fn set_zoom(&mut self, zoom: PdfZoom, cx: &mut Context<'_, Self>) {
        let old = self.layout();
        let top = self.view_top();
        let page = self.page_at(&old, top);
        let into = (top - old.tops.get(page).copied().unwrap_or(0.0)) / old.ppt;
        self.zoom = zoom;
        let new = self.layout();
        if self.pending.is_none() && !new.tops.is_empty() {
            self.set_top(new.tops[page] + into * new.ppt);
        }
        cx.notify();
    }

    /// The next zoom step up (`up`) or down from `current`.
    fn step(current: f32, up: bool) -> f32 {
        if up {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|s| *s > current + 0.005)
                .unwrap_or(ZOOM_STEPS[ZOOM_STEPS.len() - 1])
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|s| *s < current - 0.005)
                .unwrap_or(ZOOM_STEPS[0])
        }
    }

    fn zoom_by_step(&mut self, up: bool, cx: &mut Context<'_, Self>) {
        let to = Self::step(self.effective_zoom(), up);
        self.set_zoom(PdfZoom::Scale(to), cx);
    }

    // ----- drawing pages ---------------------------------------------------------------

    /// Asks for the next page `wanted` (in order) lacks at `scale` device
    /// pixels per point; cancels a page no longer wanted.
    fn pump(&mut self, wanted: &[usize], scale: f32, cx: &mut Context<'_, Self>) {
        if let Some(job) = &self.in_flight {
            let still = wanted.contains(&job.page) && close(job.scale, scale);
            if still {
                return;
            }
            job.cancel.store(true, Ordering::Release);
            self.in_flight = None;
        }
        let next = wanted.iter().copied().find(|page| {
            let drawn = self
                .drawn
                .iter()
                .any(|d| d.page == *page && close(d.scale, scale));
            let failed = self
                .failed
                .get(page)
                .is_some_and(|(tried, _)| close(*tried, scale));
            !drawn && !failed
        });
        let Some(page) = next else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let engine = Arc::clone(&self.engine);
        let bytes = Arc::clone(&self.bytes);
        let key = self.key;
        let flag = Arc::clone(&cancel);
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let page = u32::try_from(page).unwrap_or(u32::MAX);
                    let raster = engine.render_page(key, &bytes, page, scale, &flag)?;
                    to_image(raster)
                        .ok_or_else(|| PdfError::Failed("the page image is empty".to_owned()))
                })
                .await;
            this.update(cx, |viewer, cx| viewer.landed(generation, result, cx))
                .ok();
        });
        self.in_flight = Some(InFlight {
            page,
            scale,
            generation,
            cancel,
            _task: task,
        });
    }

    fn landed(
        &mut self,
        generation: u64,
        result: Result<(Arc<RenderImage>, usize), PdfError>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(job) = self.in_flight.take_if(|job| job.generation == generation) else {
            return;
        };
        match result {
            Ok((image, bytes)) => {
                self.draws += 1;
                self.failed.remove(&job.page);
                self.drawn
                    .retain(|d| !(d.page == job.page && d.scale <= job.scale));
                self.drawn.push(Drawn {
                    page: job.page,
                    scale: job.scale,
                    image,
                    bytes,
                });
                self.evict(job.page);
            }
            Err(PdfError::Cancelled) => {}
            Err(PdfError::Failed(why)) => {
                self.failed.insert(job.page, (job.scale, why.into()));
            }
        }
        cx.notify();
    }

    /// Drops the least recently shown images past [`CACHE_BYTES`], never
    /// `keep`'s.
    fn evict(&mut self, keep: usize) {
        let mut total: usize = self.drawn.iter().map(|d| d.bytes).sum();
        while total > CACHE_BYTES {
            let Some(i) = self.drawn.iter().position(|d| d.page != keep) else {
                break;
            };
            total -= self.drawn.remove(i).bytes;
        }
    }

    /// The best image of page `page` for `scale`, marked as just shown.
    fn image_of(&mut self, page: usize, scale: f32) -> Option<Arc<RenderImage>> {
        let i = self
            .drawn
            .iter()
            .enumerate()
            .filter(|(_, d)| d.page == page)
            .min_by(|(_, a), (_, b)| (a.scale - scale).abs().total_cmp(&(b.scale - scale).abs()))
            .map(|(i, _)| i)?;
        let d = self.drawn.remove(i);
        let image = Arc::clone(&d.image);
        self.drawn.push(d);
        Some(image)
    }

    // ----- text ------------------------------------------------------------------------

    /// The text of page `page` (from 0), once the engine read it.
    pub fn page_text(&self, page: usize) -> Option<&str> {
        self.texts.get(page)?.as_deref().map(PageText::text)
    }

    /// Why the text of page `page` (from 0) could not be read.
    pub fn page_text_error(&self, page: usize) -> Option<&SharedString> {
        self.text_failed.get(&page)
    }

    fn text_missing(&self, page: usize) -> bool {
        self.texts.get(page).is_some_and(Option::is_none) && !self.text_failed.contains_key(&page)
    }

    /// Asks the engine for the next page's text: the one to select in
    /// first, then (while find is open) every page from the one shown on.
    fn pump_text(&mut self, cx: &mut Context<'_, Self>) {
        if self.reading.is_some() {
            return;
        }
        let n = self.sizes.len();
        let mut first = std::mem::take(&mut self.read_first);
        first.retain(|p| self.text_missing(*p));
        let next = (!first.is_empty()).then(|| first.remove(0));
        self.read_first = first;
        let next = next.or_else(|| {
            let from = self.read_from.min(n.saturating_sub(1));
            self.read_all
                .then(|| (from..n).chain(0..from).find(|p| self.text_missing(*p)))
                .flatten()
        });
        let Some(page) = next else {
            return;
        };
        let engine = Arc::clone(&self.engine);
        let bytes = Arc::clone(&self.bytes);
        let key = self.key;
        let cancel = Arc::clone(&self.text_cancel);
        let task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let at = u32::try_from(page).unwrap_or(u32::MAX);
                    engine.page_text(key, &bytes, at, &cancel)
                })
                .await;
            this.update(cx, |viewer, cx| viewer.text_landed(page, result, cx))
                .ok();
        });
        self.reading = Some(TextRead { page, _task: task });
    }

    fn text_landed(
        &mut self,
        page: usize,
        result: Result<PdfPageText, PdfError>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.reading.as_ref().is_some_and(|r| r.page == page) {
            self.reading = None;
        }
        match result {
            Ok(text) => {
                let text = Arc::new(PageText::new(text));
                if let Some(slot) = self.texts.get_mut(page) {
                    *slot = Some(Arc::clone(&text));
                }
                self.match_page(page, cx);
            }
            Err(PdfError::Cancelled) => {}
            Err(PdfError::Failed(why)) => {
                self.text_failed.insert(page, why.into());
            }
        }
        if self.copy_pending && !self.selection_waiting() {
            self.copy_pending = false;
            if let Some(text) = self.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
        self.pump_text(cx);
        if self.find.open {
            self.show_status(cx);
        }
        cx.notify();
    }

    // ----- find --------------------------------------------------------------------------

    /// The find popover, if it was ever opened.
    pub fn find_bar(&self) -> Option<&Entity<FindBar>> {
        self.find.bar.as_ref()
    }

    /// Whether the find popover is open.
    pub fn is_find_open(&self) -> bool {
        self.find.open
    }

    /// The current match: its page (from 0) and its text.
    pub fn current_match(&self) -> Option<(usize, String)> {
        let m = self.find.matches.get(self.find.current?)?;
        let text = self.texts.get(m.page)?.as_ref()?;
        let mut found = text.slice(m.chars.clone()).to_owned();
        if m.spill > 0 {
            let next = self.texts.get(m.page + 1)?.as_ref()?;
            found.push('\n');
            found.push_str(next.slice(0..m.spill));
        }
        Some((m.page, found))
    }

    fn ensure_bar(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Entity<FindBar> {
        if let Some(bar) = &self.find.bar {
            return bar.clone();
        }
        let bar = cx.new(|cx| FindBar::new(window, cx));
        self.find._events = Some(cx.subscribe_in(
            &bar,
            window,
            |this, _, event: &FindBarEvent, window, cx| match event {
                FindBarEvent::Changed => this.search(cx),
                FindBarEvent::Next => this.step_match(true, cx),
                FindBarEvent::Previous => this.step_match(false, cx),
                FindBarEvent::Close => this.close_find(window, cx),
                // A PDF is only read.
                FindBarEvent::Replace | FindBarEvent::ReplaceAll | FindBarEvent::SelectAll => {}
            },
        ));
        self.find.bar = Some(bar.clone());
        bar
    }

    /// Ctrl+F: opens the popover (or keeps it open), fills it with the
    /// selection when that is one line of text, focuses the field with its
    /// text selected, and reads the pages' text from the one shown on.
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let bar = self.ensure_bar(window, cx);
        let prefill = self
            .selected_text()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty() && !t.contains('\n'));
        let was_open = std::mem::replace(&mut self.find.open, true);
        bar.update(cx, |bar, cx| {
            if let Some(text) = &prefill {
                bar.set_query(text, window, cx);
            }
            bar.focus_field(window, cx);
        });
        if !was_open {
            self.read_all = true;
            self.read_from = self.current_page();
        }
        // Reading first: the count says whether pages are still to come.
        self.pump_text(cx);
        if prefill.is_some() || !was_open {
            self.search(cx);
        }
        cx.notify();
    }

    /// Esc or the close button: closes the popover, drops the highlights
    /// and gives the keyboard back to the pages. The query stays for the
    /// next time; the text read stays too.
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.find.open {
            cx.propagate();
            return;
        }
        self.find.open = false;
        self.find.matches.clear();
        self.find.current = None;
        self.find.more = false;
        self.read_all = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn next_or_open(&mut self, forward: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.find.open {
            self.step_match(forward, cx);
        } else {
            self.open_find(window, cx);
        }
    }

    /// Matches the popover's query against the pages read so far; the
    /// others are matched as their text comes.
    fn search(&mut self, cx: &mut Context<'_, Self>) {
        let Some(bar) = self.find.bar.clone() else {
            return;
        };
        if !self.find.open {
            return;
        }
        let (query, options) = bar.read_with(cx, |bar, cx| (bar.query(cx), bar.options()));
        // Keep the place: from the current match, or else the page shown.
        let from = self
            .find
            .current
            .and_then(|k| self.find.matches.get(k))
            .map_or((self.current_page(), 0), |m| (m.page, m.chars.start));
        self.find.matches.clear();
        self.find.current = None;
        self.find.more = false;
        match Finder::new(&query, options) {
            Ok(finder) if finder.is_empty() => {
                self.find.query = None;
                self.find.status = FindStatus::Empty;
            }
            Ok(finder) => {
                let query = Query {
                    finder,
                    regex: options.regex,
                };
                let (matches, more) = pdf_text::search(&self.texts, &query, MAX_MATCHES);
                let k = matches.partition_point(|m| (m.page, m.chars.start) < from);
                self.find.current = match matches.len() {
                    0 => None,
                    n => Some(if k < n { k } else { 0 }),
                };
                self.find.matches = matches;
                self.find.more = more;
                self.find.query = Some(query);
                self.update_status();
                self.reveal_current(cx);
            }
            Err(err) => {
                tracing::debug!("find in PDF: {err}");
                self.find.query = None;
                self.find.status = FindStatus::Invalid;
            }
        }
        self.show_status(cx);
        cx.notify();
    }

    /// Adds the matches of page `page`, whose text just came: its own,
    /// and those of the page before that run on to it.
    fn match_page(&mut self, page: usize, cx: &mut Context<'_, Self>) {
        if !self.find.open {
            return;
        }
        let Some(query) = &self.find.query else {
            return;
        };
        let current = self
            .find
            .current
            .and_then(|k| self.find.matches.get(k))
            .map(|m| (m.page, m.chars.start));
        if pdf_text::rematch(
            &mut self.find.matches,
            &self.texts,
            page,
            query,
            MAX_MATCHES,
        ) {
            self.find.more = true;
        }
        let n = self.find.matches.len();
        match current {
            // The same match, or the next one if it went.
            Some(at) => {
                let k = self
                    .find
                    .matches
                    .partition_point(|m| (m.page, m.chars.start) < at);
                self.find.current = (n > 0).then_some(if k < n { k } else { 0 });
            }
            // The first matches: the first at or after the page shown.
            None if n > 0 => {
                let here = self.current_page();
                let k = self.find.matches.partition_point(|m| m.page < here);
                self.find.current = Some(if k < n { k } else { 0 });
                self.reveal_current(cx);
            }
            None => {}
        }
        self.update_status();
        self.show_status(cx);
    }

    /// Moves the current match forward or back, wrapping around.
    fn step_match(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let n = self.find.matches.len();
        if n == 0 {
            return;
        }
        let next = match (self.find.current, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(k), true) => (k + 1) % n,
            (Some(k), false) => (k + n - 1) % n,
        };
        self.find.current = Some(next);
        self.update_status();
        self.show_status(cx);
        self.reveal_current(cx);
        cx.notify();
    }

    fn update_status(&mut self) {
        self.find.status = FindStatus::Matches {
            current: self.find.current,
            total: self.find.matches.len(),
            more: self.find.more,
        };
    }

    fn show_status(&self, cx: &mut Context<'_, Self>) {
        let status = self.find.status;
        let searching = self.is_searching();
        if let Some(bar) = &self.find.bar {
            bar.update(cx, |bar, cx| {
                bar.set_status(status, cx);
                bar.set_searching(searching, cx);
            });
        }
    }

    /// Whether find is looking for something and pages are still being
    /// read for it (the count says so). While find is open every page
    /// missing is read in turn, so a read under way means more to come.
    pub fn is_searching(&self) -> bool {
        self.find.open && self.find.query.is_some() && self.reading.is_some()
    }

    /// Scrolls the current match into view, a third down the view, when
    /// it is not in view.
    fn reveal_current(&mut self, cx: &mut Context<'_, Self>) {
        let Some(m) = self.find.current.and_then(|k| self.find.matches.get(k)) else {
            return;
        };
        // Its first box: on its page, else on the next (a match that is
        // all line end there).
        let parts = [(m.page, m.chars.clone()), (m.page + 1, 0..m.spill)];
        let Some((page, rect)) = parts.into_iter().find_map(|(page, chars)| {
            let text = self.texts.get(page)?.as_ref()?;
            Some((page, text.rects(chars).first().copied()?))
        }) else {
            return;
        };
        let (aw, ah) = self.view_size();
        if aw <= 0.0 {
            self.pending = Some((page, Some((rect[1] - 2.0 * PAGE_GAP).max(0.0))));
            cx.notify();
            return;
        }
        let layout = self.layout();
        let Some(page_top) = layout.tops.get(page) else {
            return;
        };
        let top = page_top + rect[1] * layout.ppt;
        let bottom = page_top + rect[3] * layout.ppt;
        let view = self.view_top();
        if top < view || bottom > view + ah {
            let max = (layout.height - ah).max(0.0);
            self.set_top((top - ah / 3.0).clamp(0.0, max));
            self.pending = None;
            cx.notify();
        }
    }

    // ----- selection ---------------------------------------------------------------------

    /// Where page `page` is now, window px: where it was painted, moved
    /// by the scrolling since.
    fn page_rect(&self, page: usize) -> Option<Bounds<Pixels>> {
        let (bounds, painted) = *self.page_bounds.borrow().get(&page)?;
        let now = self.scroll.offset();
        Some(Bounds {
            origin: bounds.origin + (now - painted),
            size: bounds.size,
        })
    }

    /// The page under `position` (window px), and the point on it in
    /// points from its top left.
    fn page_point(&self, position: Point<Pixels>) -> Option<(usize, (f32, f32))> {
        let pages: Vec<usize> = self.page_bounds.borrow().keys().copied().collect();
        pages.into_iter().find_map(|page| {
            let b = self.page_rect(page)?;
            b.contains(&position)
                .then(|| (page, self.point_on(&b, position)))
        })
    }

    /// `position` (window px) in points from the top left of a page
    /// painted at `bounds`.
    fn point_on(&self, bounds: &Bounds<Pixels>, position: Point<Pixels>) -> (f32, f32) {
        let ppt = self.layout().ppt.max(f32::EPSILON);
        (
            f32::from(position.x - bounds.origin.x) / ppt,
            f32::from(position.y - bounds.origin.y) / ppt,
        )
    }

    /// The page under `position` (window px) or, off the pages, the
    /// nearest one above or below; and where on it: the point (points from
    /// its top left), or its start or end for a position above or below it.
    fn mark_near(&self, position: Point<Pixels>) -> Option<Mark> {
        let pages: Vec<usize> = self.page_bounds.borrow().keys().copied().collect();
        let y = f32::from(position.y);
        let off = |b: &Bounds<Pixels>| {
            (f32::from(b.top()) - y)
                .max(y - f32::from(b.bottom()))
                .max(0.0)
        };
        let (page, bounds) = pages
            .into_iter()
            .filter_map(|page| Some((page, self.page_rect(page)?)))
            .min_by(|(pa, a), (pb, b)| off(a).total_cmp(&off(b)).then(pa.cmp(pb)))?;
        let spot = if position.y < bounds.top() {
            Spot::Start
        } else if position.y > bounds.bottom() {
            Spot::End
        } else {
            let (x, y) = self.point_on(&bounds, position);
            Spot::At(x, y)
        };
        Some(Mark { page, spot })
    }

    /// The selected characters, page by page.
    fn selected(&self) -> Vec<(usize, std::ops::Range<usize>)> {
        self.selection.map_or_else(Vec::new, |sel| {
            pdf_text::selection(&self.texts, sel.anchor, sel.head, sel.unit)
        })
    }

    /// The selected text, if any (the text of pages not read yet left
    /// out).
    pub fn selected_text(&self) -> Option<String> {
        let runs = self.selected();
        (!runs.is_empty()).then(|| pdf_text::selection_text(&self.texts, &runs))
    }

    /// Whether the selection runs over pages whose text is still to be
    /// read.
    fn selection_waiting(&self) -> bool {
        self.selection
            .is_some_and(|sel| sel.pages().any(|p| self.text_missing(p)))
    }

    /// Reads the text of the pages the selection runs over that is not
    /// read yet, before other pages: its ends first, then the pages
    /// between.
    fn read_selection(&mut self, cx: &mut Context<'_, Self>) {
        let Some(sel) = self.selection else {
            return;
        };
        for page in [sel.anchor.page, sel.head.page]
            .into_iter()
            .chain(sel.pages())
        {
            if self.text_missing(page) && !self.read_first.contains(&page) {
                self.read_first.push(page);
            }
        }
        self.pump_text(cx);
    }

    /// The left button went down on a page: a click starts a selection
    /// (its text is read first if it is not yet), a double click selects
    /// the word, a triple click the line; Shift+click moves the end of the
    /// selection there, on this page or another (as in the Source view).
    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.focus.focus(window, cx);
        self.dragging = false;
        self.pointer = None;
        self.autoscroll = None;
        self.copy_pending = false;
        let Some((page, at)) = self.page_point(event.position) else {
            self.selection = None;
            cx.notify();
            return;
        };
        let unit = match event.click_count {
            0 | 1 => Unit::Char,
            2 => Unit::Word,
            _ => Unit::Line,
        };
        let extend = event.modifiers.shift && unit == Unit::Char;
        let head = Mark {
            page,
            spot: Spot::At(at.0, at.1),
        };
        self.selection = match self.selection {
            Some(sel) if extend => {
                // After Select all, back from the page's end for a click
                // on an earlier page.
                let whole = sel.anchor.spot == Spot::Start
                    && sel.head
                        == Mark {
                            page: sel.anchor.page,
                            spot: Spot::End,
                        };
                let anchor = if whole && page < sel.anchor.page {
                    sel.head
                } else {
                    sel.anchor
                };
                Some(PdfSelection {
                    anchor,
                    head,
                    ..sel
                })
            }
            _ => Some(PdfSelection {
                anchor: head,
                head,
                unit,
            }),
        };
        self.dragging = true;
        if self.text_missing(page) {
            self.read_first.retain(|p| *p != page);
            self.read_first.insert(0, page);
        }
        self.read_selection(cx);
        cx.notify();
    }

    /// The pointer moved while the button is down (anywhere in the
    /// window): the selection follows it, and near the view's top or
    /// bottom edge the view scrolls.
    fn drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<'_, Self>) {
        if !self.dragging {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.release();
            return;
        }
        self.pointer = Some(event.position);
        self.follow_pointer(cx);
        if self.edge_step() != 0.0 && self.autoscroll.is_none() {
            self.autoscroll = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSCROLL_TICK).await;
                    let more = this
                        .update(cx, |viewer, cx| viewer.autoscroll_tick(cx))
                        .unwrap_or(false);
                    if !more {
                        break;
                    }
                }
            }));
        }
    }

    /// The button came up: the drag ends.
    fn release(&mut self) {
        self.dragging = false;
        self.pointer = None;
        self.autoscroll = None;
    }

    /// Moves the selection's end to the pointer ([`Self::mark_near`]),
    /// and reads the text of pages it reaches.
    fn follow_pointer(&mut self, cx: &mut Context<'_, Self>) {
        let Some(head) = self.pointer.and_then(|p| self.mark_near(p)) else {
            return;
        };
        let Some(sel) = &mut self.selection else {
            return;
        };
        if sel.head == head {
            return;
        }
        let new_page = sel.head.page != head.page;
        sel.head = head;
        if new_page {
            self.read_selection(cx);
        }
        cx.notify();
    }

    /// How far one tick scrolls for the pointer's place, px: up (below
    /// 0) near or above the view's top, down near or below its bottom, 0
    /// elsewhere.
    fn edge_step(&self) -> f32 {
        let (Some(pointer), view) = (self.pointer, self.view_bounds.get()) else {
            return 0.0;
        };
        if view.size.height <= px(2.0 * EDGE) {
            return 0.0;
        }
        let y = f32::from(pointer.y);
        let into_top = f32::from(view.top()) + EDGE - y;
        let into_bottom = y - (f32::from(view.bottom()) - EDGE);
        let step = |depth: f32| (depth / 2.0).clamp(AUTOSCROLL_STEP.0, AUTOSCROLL_STEP.1);
        if into_top > 0.0 {
            -step(into_top)
        } else if into_bottom > 0.0 {
            step(into_bottom)
        } else {
            0.0
        }
    }

    /// One tick of scrolling while dragging near an edge; whether to go
    /// on.
    fn autoscroll_tick(&mut self, cx: &mut Context<'_, Self>) -> bool {
        let step = self.edge_step();
        if !self.dragging || step == 0.0 {
            self.autoscroll = None;
            return false;
        }
        self.scroll_by(step, cx);
        self.follow_pointer(cx);
        true
    }

    /// Ctrl+C: copies the selected text.
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selection_waiting() {
            // Once the pages' text is read ([`Self::text_landed`]).
            self.copy_pending = true;
            self.read_selection(cx);
            return;
        }
        match self.selected_text() {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            None => cx.propagate(),
        }
    }

    /// Ctrl+A: selects the text of the page shown.
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<'_, Self>) {
        let page = self.current_page();
        self.copy_pending = false;
        self.selection = Some(PdfSelection {
            anchor: Mark {
                page,
                spot: Spot::Start,
            },
            head: Mark {
                page,
                spot: Spot::End,
            },
            unit: Unit::Char,
        });
        self.read_selection(cx);
        cx.notify();
    }

    /// The highlights over page `page`: the find matches (the current one
    /// in `accent.line`, the others in `accent.tint`, as in the Reading
    /// view) and the selection (`selection.text`; `selected`, as
    /// [`Self::selected`] gives it).
    fn marks(
        &self,
        page: usize,
        ppt: f32,
        theme: &Theme,
        selected: &[(usize, std::ops::Range<usize>)],
    ) -> Vec<AnyElement> {
        let Some(text) = self.texts.get(page).and_then(Option::as_ref) else {
            return Vec::new();
        };
        let mut rects: Vec<([f32; 4], Hsla, &'static str)> = Vec::new();
        if self.find.open {
            let matches = &self.find.matches;
            let start = matches.partition_point(|m| m.page < page);
            // A match from the page before that runs on to this one, then
            // this page's.
            let carried = start
                .checked_sub(1)
                .filter(|k| matches[*k].page + 1 == page && matches[*k].spill > 0)
                .map(|k| (k, 0..matches[k].spill));
            let own = matches[start..]
                .iter()
                .enumerate()
                .take_while(|(_, m)| m.page == page)
                .map(|(k, m)| (start + k, m.chars.clone()));
            for (k, chars) in carried.into_iter().chain(own) {
                let (color, name) = if Some(k) == self.find.current {
                    (theme.accent.line, "pdf-current-match")
                } else {
                    (theme.accent.tint, "pdf-match")
                };
                rects.extend(
                    text.rects(chars)
                        .into_iter()
                        .map(|r| (r, color.into(), name)),
                );
            }
        }
        let color: Hsla = theme.selection.text.into();
        for (_, range) in selected.iter().filter(|(at, _)| *at == page) {
            rects.extend(
                text.rects(range.clone())
                    .into_iter()
                    .map(|r| (r, color, "pdf-selection")),
            );
        }
        rects
            .into_iter()
            .map(|(r, color, name)| {
                div()
                    .debug_selector(move || name.to_owned())
                    .absolute()
                    .left(px(r[0] * ppt))
                    .top(px(r[1] * ppt))
                    .w(px((r[2] - r[0]) * ppt))
                    .h(px((r[3] - r[1]) * ppt))
                    .bg(color)
                    .opacity(MARK_OPACITY)
                    .into_any_element()
            })
            .collect()
    }

    // ----- drawing the view --------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn button(
        id: &'static str,
        label: SharedString,
        name: &'static str,
        pressed: Option<bool>,
        theme: &Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (hover, press) = (theme.state.hover, theme.state.press);
        div()
            .id(id)
            .a11y_button(name)
            .when_some(pressed, |b, on| {
                b.aria_toggled(if on { Toggled::True } else { Toggled::False })
            })
            .keyboard_control(theme)
            .debug_selector(move || id.to_owned())
            .flex_none()
            .h(px(size::BUTTON_HEIGHT))
            .px(px(spacing::BUTTON_PAD_X))
            .flex()
            .items_center()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(theme.border.strong)
            .bg(if pressed == Some(true) {
                theme.accent.tint
            } else {
                theme.surface.raised
            })
            .text_color(theme.text.primary)
            .font_weight(FontWeight(text::weight::ACTIVE))
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(press))
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .accessible()
            .into_any_element()
    }

    fn toolbar(&self, zoom: f32, theme: &Theme, cx: &mut Context<'_, Self>) -> AnyElement {
        let count = self.sizes.len();
        let page = self.current_page() + 1;
        let percent = format!("{:.0} %", zoom * 100.0);
        let facts = if count == 1 {
            "1 page".to_owned()
        } else {
            format!("{count} pages")
        };
        div()
            .id("pdf-viewer-toolbar")
            .a11y_group(Role::Toolbar, "PDF")
            .flex_none()
            .min_h(px(sizes::INFOBAR))
            .flex()
            .items_center()
            .gap(px(spacing::GAP))
            .pt(px(spacing::PAD_TOP))
            .pr(px(spacing::PAD_RIGHT))
            .pb(px(spacing::PAD_BOTTOM))
            .pl(px(spacing::PAD_LEFT))
            .border_b_1()
            .border_color(theme.border.default)
            .bg(theme.surface.panel)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(Icon::PdfFile.sized(icon::ROW)),
            )
            .child(
                div()
                    .debug_selector(|| "pdf-viewer-facts".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text.secondary)
                    .child(facts),
            )
            .child(Self::button(
                "pdf-viewer-previous",
                "\u{2191}".into(),
                "Previous page",
                None,
                theme,
                |this, _, cx| this.step_page(false, cx),
                cx,
            ))
            .child(
                div()
                    .id("pdf-viewer-page")
                    .role(Role::Status)
                    .aria_label("Page")
                    .aria_value(format!("Page {page} of {count}"))
                    .debug_selector(|| "pdf-viewer-page".into())
                    .flex_none()
                    .min_w(px(sizes::INFOBAR * 2.0))
                    .flex()
                    .justify_center()
                    .text_color(theme.text.primary)
                    .child(format!("{page} / {count}"))
                    .accessible()
                    .live(Live::Polite),
            )
            .child(Self::button(
                "pdf-viewer-next",
                "\u{2193}".into(),
                "Next page",
                None,
                theme,
                |this, _, cx| this.step_page(true, cx),
                cx,
            ))
            .child(Self::button(
                "pdf-viewer-go-to",
                "Go to\u{2026}".into(),
                "Go to page",
                None,
                theme,
                |this, window, cx| this.ask_page(window, cx),
                cx,
            ))
            .child(Self::button(
                "pdf-viewer-fit-width",
                "Fit width".into(),
                "Fit width",
                Some(self.zoom == PdfZoom::FitWidth),
                theme,
                |this, _, cx| this.set_zoom(PdfZoom::FitWidth, cx),
                cx,
            ))
            .child(Self::button(
                "pdf-viewer-fit-page",
                "Fit page".into(),
                "Fit page",
                Some(self.zoom == PdfZoom::FitPage),
                theme,
                |this, _, cx| this.set_zoom(PdfZoom::FitPage, cx),
                cx,
            ))
            .child(Self::button(
                "pdf-viewer-zoom-out",
                "\u{2212}".into(),
                "Zoom out",
                None,
                theme,
                |this, _, cx| this.zoom_by_step(false, cx),
                cx,
            ))
            .child(
                div()
                    .id("pdf-viewer-zoom")
                    .role(Role::Status)
                    .aria_label("Zoom")
                    .aria_value(percent.clone())
                    .debug_selector(|| "pdf-viewer-zoom".into())
                    .flex_none()
                    .min_w(px(sizes::INFOBAR * 2.0))
                    .flex()
                    .justify_center()
                    .text_color(theme.text.primary)
                    .child(percent)
                    .accessible()
                    .live(Live::Polite),
            )
            .child(Self::button(
                "pdf-viewer-zoom-in",
                "+".into(),
                "Zoom in",
                None,
                theme,
                |this, _, cx| this.zoom_by_step(true, cx),
                cx,
            ))
            .accessible()
            .into_any_element()
    }
}

/// Whether two scales are within [`SCALE_SLACK`] of each other.
fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= SCALE_SLACK * b.abs().max(f32::EPSILON)
}

/// A drawn page as an image GPUI shows, and its size in bytes (blocking:
/// the background executor).
fn to_image(raster: PageRaster) -> Option<(Arc<RenderImage>, usize)> {
    let bytes = raster.rgba.len();
    let (image, _, _) = crate::export::dialog::raster_image(raster)?;
    Some((image, bytes))
}

/// Ctrl (Cmd or Ctrl on macOS) turns the wheel into zoom.
fn zoom_modifier(m: &gpui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform || m.control
    } else {
        m.control
    }
}

impl Render for PdfViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let area = self.view_size();
        if let Some((page, into)) = self.pending
            && area.0 > 0.0
        {
            let layout = self.layout();
            if let Some(top) = Self::top_of(&layout, page, into) {
                self.set_top(top);
            }
            self.current = page;
            self.pending = None;
        }
        let layout = self.layout();
        let count = self.sizes.len();
        let top = self.view_top().min((layout.height - area.1).max(0.0));
        if self.pending.is_none() {
            self.current = self.page_at(&layout, top);
        }
        // The pages in view, and one either side.
        let (first, last) = layout
            .crossing(&self.sizes, top, top + area.1.max(1.0))
            .unwrap_or((self.current, self.current));
        let from = first.saturating_sub(1);
        let to = (last + 1).min(count.saturating_sub(1));
        let scale = layout.ppt * window.scale_factor();
        let mut wanted: Vec<usize> = (first..=last).collect();
        wanted.extend((from..first).chain(last + 1..=to));
        if area.0 > 0.0 {
            self.pump(&wanted, scale, cx);
        }

        let mut pages = div().relative().w(px(layout.width)).h(px(layout.height));
        // The page a drag started on stays known while it scrolls away.
        let keep = self
            .selection
            .filter(|_| self.dragging)
            .map(|s| s.anchor.page);
        self.page_bounds
            .borrow_mut()
            .retain(|page, _| Some(*page) == keep);
        let selected = self.selected();
        for page in from..=to.min(count.saturating_sub(1)) {
            let (w, h) = self.sizes[page];
            let (pw, ph) = (w * layout.ppt, h * layout.ppt);
            let x = ((layout.width - pw) / 2.0).max(PAGE_GAP);
            let y = layout.tops[page];
            let image = self.image_of(page, scale);
            let error = self.failed.get(&page).map(|(_, why)| why.clone());
            let label = format!("Page {} of {count}", page + 1);
            let marks = self.marks(page, layout.ppt, theme, &selected);
            let has_text = self
                .texts
                .get(page)
                .and_then(Option::as_ref)
                .is_some_and(|t| t.len() > 0);
            let bounds = Rc::clone(&self.page_bounds);
            let scroll = self.scroll.clone();
            pages = pages.child(
                div()
                    .id(("pdf-page", page))
                    .role(Role::Image)
                    .aria_label(label)
                    .aria_position_in_set(page + 1)
                    .aria_size_of_set(count)
                    .debug_selector(move || format!("pdf-page-{}", page + 1))
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(pw))
                    .h(px(ph))
                    .overflow_hidden()
                    .bg(theme.surface.raised)
                    .border_1()
                    .border_color(theme.border.default)
                    .when_some(image, |d, image| {
                        d.child(
                            img(ImageSource::Render(image))
                                .size_full()
                                .object_fit(ObjectFit::Fill),
                        )
                    })
                    .when_some(error, |d, why| {
                        d.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .p(px(PAGE_GAP))
                                .text_size(px(text::UI.size))
                                .line_height(px(text::UI.line))
                                .text_color(theme.text.secondary)
                                .child(format!("This page could not be drawn: {why}")),
                        )
                    })
                    .when(has_text, |d| d.cursor(CursorStyle::IBeam))
                    .children(marks)
                    .child(
                        // Where the page is painted, for the mouse.
                        canvas(
                            move |b, _, _| {
                                bounds.borrow_mut().insert(page, (b, scroll.offset()));
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .accessible(),
            );
        }

        let cell = self.area.clone();
        let entity = cx.entity_id();
        let body = div()
            .id("pdf-viewer-pages")
            .a11y_group(Role::List, "Pages")
            .debug_selector(|| "pdf-viewer-pages".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .bg(theme.surface.sunken)
            .on_scroll(cx.listener(|this, e: &gpui::ScrollEvent, _, cx| {
                if zoom_modifier(&e.modifiers) {
                    let dy = f32::from(e.delta.pixel_delta(px(16.0)).y);
                    if dy != 0.0 {
                        this.zoom_by_step(dy > 0.0, cx);
                    }
                    cx.stop_propagation();
                } else {
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| this.press(e, window, cx)),
            )
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(|this, _: &ScrollUp, _, cx| this.scroll_by(-SCROLL_STEP, cx)))
            .on_action(cx.listener(|this, _: &ScrollDown, _, cx| this.scroll_by(SCROLL_STEP, cx)))
            .on_action(cx.listener(|this, _: &ScreenUp, _, cx| this.screen(false, cx)))
            .on_action(cx.listener(|this, _: &ScreenDown, _, cx| this.screen(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousPage, _, cx| this.step_page(false, cx)))
            .on_action(cx.listener(|this, _: &NextPage, _, cx| this.step_page(true, cx)))
            .on_action(cx.listener(|this, _: &FirstPage, _, cx| this.go_to(0, cx)))
            .on_action(cx.listener(|this, _: &LastPage, _, cx| this.go_to(usize::MAX, cx)))
            .on_action(cx.listener(|this, _: &GoToPage, window, cx| this.ask_page(window, cx)))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.ask_page(window, cx)))
            .on_action(
                cx.listener(|this, _: &FitWidth, _, cx| this.set_zoom(PdfZoom::FitWidth, cx)),
            )
            .on_action(cx.listener(|this, _: &FitPage, _, cx| this.set_zoom(PdfZoom::FitPage, cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| {
                this.set_zoom(PdfZoom::FitWidth, cx);
            }))
            .on_action(cx.listener(|this, _: &ActualSize, _, cx| {
                this.set_zoom(PdfZoom::Scale(1.0), cx);
            }))
            .on_action(cx.listener(|this, _: &ZoomInPages, _, cx| this.zoom_by_step(true, cx)))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom_by_step(true, cx)))
            .on_action(cx.listener(|this, _: &ZoomOutPages, _, cx| this.zoom_by_step(false, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom_by_step(false, cx)))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        // A new view size lays the pages out again.
                        if cell.get().size != bounds.size {
                            cell.set(bounds);
                            cx.notify(entity);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(pages);
        let zoom = layout.ppt / PX_PER_PT;
        // The find popover, at the top right of the pages.
        let popover = self.find.bar.clone().filter(|_| self.find.open).map(|bar| {
            div()
                .absolute()
                .top(px(find_spacing::INSET_TOP))
                .right(px(find_spacing::INSET_RIGHT))
                .child(bar)
        });
        let view = Rc::clone(&self.view_bounds);
        let weak = cx.entity().downgrade();
        // Where the pages' view is, for scrolling near its edges, and the
        // pointer anywhere in the window while dragging a selection.
        let watch = canvas(
            move |bounds, _, _| view.set(bounds),
            move |_, _, window, _| {
                let on_move = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        on_move.update(cx, |this, cx| this.drag(e, cx)).ok();
                    }
                });
                let on_up = weak.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                        on_up.update(cx, |this, _| this.release()).ok();
                    }
                });
            },
        )
        .absolute()
        .size_full();
        div()
            .key_context(DOCUMENT_CONTEXT)
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, window, cx| {
                this.next_or_open(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FindPrevious, window, cx| {
                this.next_or_open(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseFind, window, cx| this.close_find(window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(zoom, theme, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(watch)
                    .child(body.accessible())
                    .children(popover),
            )
    }
}

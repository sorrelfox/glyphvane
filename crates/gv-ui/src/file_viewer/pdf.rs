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
//! - **Accessibility.** The pages are a list named "Pages"; each page laid
//!   out is an image named "Page N of M" with its position in the list.
//!
//! Not designed (flagged): the toolbar, the page gap and frame, the
//! per-page messages.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, FocusHandle, Focusable, FontWeight,
    ImageSource, InteractiveElement as _, IntoElement, KeyBinding, ObjectFit, ParentElement as _,
    Pixels, Render, RenderImage, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Task, Window, canvas, div, img, point, px,
};
use serde::{Deserialize, Serialize};

use crate::a11y::{A11yExt as _, AccessibleExt as _, Live, Role, Toggled};
use crate::appearance::Appearance;
use crate::export::{PageRaster, PdfEngine, PdfError};
use crate::goto::{self, Choice, GotoEvent, GotoKind, GotoRequest};
use crate::icons::Icon;
use crate::keymap::actions::navigation::GoToLine;
use crate::keymap::actions::view::{ZoomIn, ZoomOut, ZoomReset};
use crate::metrics::size::infobar as size;
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
        for page in from..=to.min(count.saturating_sub(1)) {
            let (w, h) = self.sizes[page];
            let (pw, ph) = (w * layout.ppt, h * layout.ppt);
            let x = ((layout.width - pw) / 2.0).max(PAGE_GAP);
            let y = layout.tops[page];
            let image = self.image_of(page, scale);
            let error = self.failed.get(&page).map(|(_, why)| why.clone());
            let label = format!("Page {} of {count}", page + 1);
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
            .flex_1()
            .min_h_0()
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
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                }),
            )
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
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(zoom, theme, cx))
            .child(body.accessible())
    }
}

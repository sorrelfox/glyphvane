// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The PDF viewer in a headless window, with a PDF engine of its own
//! ([`FakePdf`]): pages, drawing only what is in view, navigation, zoom,
//! `#page=` and the error states, what assistive technology is told, and
//! find and text selection over the pages' text (words, lines, Shift+click
//! and scrolling near an edge while dragging).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, Bounds, Entity, Focusable as _, Modifiers, MouseButton, MouseDownEvent,
    MouseUpEvent, Pixels, Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions,
    point, px, size,
};

use super::pdf::{PX_PER_PT, PdfViewer, PdfZoom};
use super::{FileViewer, PDF_MAX_BYTES, ViewerKind, ViewerStatus, kind};
use crate::export::{PageRaster, PdfEngine, PdfError, PdfPageText};
use crate::find::FindBar;
use crate::{AppSettings, ThemeChoice};

/// A PDF engine for tests: every PDF has the pages `sizes` (points); it
/// records the pages drawn and the fitted pages asked for, and fails as
/// told.
#[derive(Debug, Default)]
pub(crate) struct FakePdf {
    pub sizes: Vec<(f32, f32)>,
    /// Opening fails with this message.
    pub open_error: Option<String>,
    /// Drawing this page (from 0) fails with the message.
    pub page_error: Option<(u32, String)>,
    /// Pages drawn at a scale: (key, page, scale).
    pub rendered: Mutex<Vec<(u64, u32, f32)>>,
    /// Pages fitted in a box: (page, max width, max height).
    pub fitted: Mutex<Vec<(u32, u32, u32)>>,
    pub opened: AtomicBool,
    /// Each page's text ([`FakePdf::text_of`] places it).
    pub texts: Vec<String>,
    /// Pages whose text was read, in order.
    pub texted: Mutex<Vec<u32>>,
}

impl FakePdf {
    pub fn pages(sizes: &[(f32, f32)]) -> Self {
        Self {
            sizes: sizes.to_vec(),
            ..Self::default()
        }
    }

    pub fn texted_pages(&self) -> Vec<u32> {
        self.texted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// `text` placed as a PDF would: each line 72 pt from the left, the
    /// first 72 pt from the top and 20 pt apart; each character 6 pt wide
    /// and 12 pt high; a line end has no area.
    pub fn text_of(text: &str) -> PdfPageText {
        let mut boxes = Vec::new();
        let (mut line, mut col) = (0.0_f32, 0.0_f32);
        for c in text.chars() {
            let (x, y) = (72.0 + 6.0 * col, 72.0 + 20.0 * line);
            if c == '\n' {
                boxes.push([x, y, x, y + 12.0]);
                line += 1.0;
                col = 0.0;
            } else {
                boxes.push([x, y, x + 6.0, y + 12.0]);
                col += 1.0;
            }
        }
        PdfPageText {
            text: text.to_owned(),
            boxes,
        }
    }

    pub fn rendered_pages(&self) -> Vec<u32> {
        self.rendered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|r| r.1)
            .collect()
    }

    fn raster(width: u32, height: u32, pages: u32) -> PageRaster {
        PageRaster {
            width,
            height,
            rgba: vec![255; (width * height * 4) as usize],
            pages,
        }
    }
}

impl PdfEngine for FakePdf {
    fn render(
        &self,
        _: &gv_export::TypstDocument,
        _: u32,
        _: &AtomicBool,
    ) -> Result<Vec<u8>, PdfError> {
        Err(PdfError::Failed("not here".into()))
    }

    fn draws_pages(&self) -> bool {
        true
    }

    fn page(
        &self,
        _: &[u8],
        page: u32,
        max_width: u32,
        max_height: u32,
        _: &AtomicBool,
    ) -> Result<PageRaster, PdfError> {
        self.fitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((page, max_width, max_height));
        if let Some((bad, why)) = &self.page_error
            && *bad == page
        {
            return Err(PdfError::Failed(why.clone()));
        }
        let pages = u32::try_from(self.sizes.len()).unwrap_or(0);
        if page >= pages {
            return Err(PdfError::Failed(format!(
                "the PDF has no page {}",
                page + 1
            )));
        }
        Ok(Self::raster(8, 10, pages))
    }

    fn open_document(&self, _: u64, _: &[u8], _: &AtomicBool) -> Result<Vec<(f32, f32)>, PdfError> {
        self.opened.store(true, Ordering::Release);
        match &self.open_error {
            Some(why) => Err(PdfError::Failed(why.clone())),
            None => Ok(self.sizes.clone()),
        }
    }

    fn render_page(
        &self,
        key: u64,
        _: &[u8],
        page: u32,
        scale: f32,
        _: &AtomicBool,
    ) -> Result<PageRaster, PdfError> {
        self.rendered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((key, page, scale));
        if let Some((bad, why)) = &self.page_error
            && *bad == page
        {
            return Err(PdfError::Failed(why.clone()));
        }
        let pages = u32::try_from(self.sizes.len()).unwrap_or(0);
        Ok(Self::raster(4, 4, pages))
    }

    fn page_text(
        &self,
        _: u64,
        _: &[u8],
        page: u32,
        _: &AtomicBool,
    ) -> Result<PdfPageText, PdfError> {
        self.texted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(page);
        let text = self.texts.get(page as usize).map_or("", String::as_str);
        Ok(Self::text_of(text))
    }
}

const LETTER: (f32, f32) = (612.0, 792.0);

fn open(
    cx: &mut TestAppContext,
    file: &Path,
    engine: Option<Arc<FakePdf>>,
    page: Option<usize>,
) -> (AnyWindowHandle, Entity<FileViewer>) {
    let file = file.to_path_buf();
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        cx.set_reduce_motion(true);
        crate::export::set_pdf_engine(cx, engine.map(|e| e as Arc<dyn PdfEngine>));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(800.0), px(600.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| {
            let viewer = FileViewer::open(file, None, cx);
            if let Some(page) = page {
                viewer.update(cx, |v, cx| v.go_to_page(page, cx));
            }
            viewer
        })
        .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..4 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn pdf_file(dir: &Path) -> std::path::PathBuf {
    let file = dir.join("paper.pdf");
    std::fs::write(&file, b"%PDF-1.7\n%%EOF\n").expect("write");
    file
}

fn pdf_of(viewer: &Entity<FileViewer>, vcx: &mut VisualTestContext) -> Entity<PdfViewer> {
    viewer
        .read_with(vcx, |v, _| v.pdf().cloned())
        .expect("the PDF viewer")
}

/// Presses `key` with the pages focused, and draws.
fn press(pdf: &Entity<PdfViewer>, vcx: &mut VisualTestContext, key: &str) {
    vcx.update(|window, cx| {
        let focus = pdf.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    });
    vcx.simulate_keystrokes(key);
    draw(vcx);
}

#[test]
fn pdfs_are_told_apart_by_name() {
    assert_eq!(kind(Path::new("a/Paper.PDF")), Some(ViewerKind::Pdf));
    assert!(!super::sniff_text(Path::new("missing.pdf")));
}

#[gpui::test]
fn a_pdf_shows_its_pages_and_draws_only_those_in_view(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf::pages(&[LETTER; 6]));
    let (window, viewer) = open(cx, &file, Some(Arc::clone(&engine)), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    assert!(matches!(
        viewer.read_with(&vcx, |v, _| v.status().clone()),
        ViewerStatus::Pdf { pages: 6 }
    ));
    let pdf = pdf_of(&viewer, &mut vcx);
    pdf.read_with(&vcx, |p, _| {
        assert_eq!(p.page_count(), 6);
        assert_eq!(p.current_page(), 0);
        assert_eq!(p.zoom(), PdfZoom::FitWidth);
        // The first page fills the width: (800 - 2 × 12) / 612 px a point.
        let expected = (800.0 - 24.0) / 612.0 / PX_PER_PT;
        assert!(
            (p.effective_zoom() - expected).abs() < 0.01,
            "{}",
            p.effective_zoom()
        );
        assert!(p.page_drawn(0) && p.page_drawn(1));
        assert!(!p.page_drawn(3) && !p.page_drawn(5));
    });
    // Only the page in view and the next one were drawn, in that order,
    // at the device scale of the layout.
    let device = vcx.update(|window, _| window.scale_factor());
    let rendered = engine.rendered.lock().expect("lock").clone();
    assert_eq!(rendered.iter().map(|r| r.1).collect::<Vec<_>>(), [0, 1]);
    assert!(rendered.iter().all(|r| r.0 == rendered[0].0), "one key");
    assert!((rendered[0].2 - (800.0 - 24.0) / 612.0 * device).abs() < 0.05);
    // Pages laid out are in the view; the others are not.
    assert!(vcx.debug_bounds("pdf-page-1").is_some());
    assert!(vcx.debug_bounds("pdf-page-5").is_none());
    assert!(vcx.debug_bounds("pdf-viewer-pages").is_some());

    // Scrolled to the end: the last pages are drawn, and it is the page
    // shown.
    press(&pdf, &mut vcx, "end");
    pdf.read_with(&vcx, |p, _| {
        assert_eq!(p.current_page(), 5);
        assert!(p.page_drawn(5) && p.page_drawn(4));
    });
    assert!(vcx.debug_bounds("pdf-page-6").is_some());
    assert!(vcx.debug_bounds("pdf-page-1").is_none());
}

#[gpui::test]
fn the_keyboard_and_the_toolbar_move_between_pages(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf::pages(&[LETTER; 5]));
    let (window, viewer) = open(cx, &file, Some(engine), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    let page = |vcx: &mut VisualTestContext| pdf.read_with(vcx, |p, _| p.current_page());

    press(&pdf, &mut vcx, "right");
    assert_eq!(page(&mut vcx), 1);
    press(&pdf, &mut vcx, "right");
    assert_eq!(page(&mut vcx), 2);
    press(&pdf, &mut vcx, "left");
    assert_eq!(page(&mut vcx), 1);
    press(&pdf, &mut vcx, "end");
    assert_eq!(page(&mut vcx), 4);
    press(&pdf, &mut vcx, "home");
    assert_eq!(page(&mut vcx), 0);
    // A page is about 1,000 px high at fit width: Page Down twice (and
    // Space) reach the second page; Page Up goes back.
    press(&pdf, &mut vcx, "pagedown");
    press(&pdf, &mut vcx, "space");
    assert_eq!(page(&mut vcx), 1);
    press(&pdf, &mut vcx, "pageup");
    press(&pdf, &mut vcx, "shift-space");
    assert_eq!(page(&mut vcx), 0);
    // Down scrolls a little: still the first page.
    press(&pdf, &mut vcx, "down");
    assert_eq!(page(&mut vcx), 0);

    // The toolbar's buttons.
    let next = vcx.debug_bounds("pdf-viewer-next").expect("Next page");
    vcx.simulate_click(next.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(page(&mut vcx), 1);
    let previous = vcx
        .debug_bounds("pdf-viewer-previous")
        .expect("Previous page");
    vcx.simulate_click(previous.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(page(&mut vcx), 0);

    // A page by number (what Go to page confirms).
    vcx.update(|_, cx| viewer.update(cx, |v, cx| v.go_to_page(3, cx)));
    draw(&mut vcx);
    assert_eq!(page(&mut vcx), 3);
    pdf.read_with(&vcx, |p, _| assert!(p.page_drawn(3)));
}

#[gpui::test]
fn zoom_steps_fit_and_keep_the_page(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf::pages(&[LETTER; 4]));
    let (window, viewer) = open(cx, &file, Some(Arc::clone(&engine)), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    let zoom =
        |vcx: &mut VisualTestContext| pdf.read_with(vcx, |p, _| (p.zoom(), p.effective_zoom()));

    press(&pdf, &mut vcx, "right");
    press(&pdf, &mut vcx, "1");
    let (z, effective) = zoom(&mut vcx);
    assert_eq!(z, PdfZoom::Scale(1.0));
    assert!((effective - 1.0).abs() < 1e-4);
    // Zooming keeps the page shown.
    pdf.read_with(&vcx, |p, _| assert_eq!(p.current_page(), 1));
    press(&pdf, &mut vcx, "=");
    assert_eq!(zoom(&mut vcx).0, PdfZoom::Scale(1.1));
    press(&pdf, &mut vcx, "-");
    press(&pdf, &mut vcx, "-");
    assert_eq!(zoom(&mut vcx).0, PdfZoom::Scale(0.9));
    pdf.read_with(&vcx, |p, _| assert_eq!(p.current_page(), 1));
    // The page is drawn again at the new scale.
    let device = vcx.update(|window, _| window.scale_factor());
    let scales: Vec<f32> = engine
        .rendered
        .lock()
        .expect("lock")
        .iter()
        .filter(|r| r.1 == 1)
        .map(|r| r.2)
        .collect();
    assert!(
        scales
            .iter()
            .any(|s| (s - 0.9 * PX_PER_PT * device).abs() < 0.01),
        "{scales:?}"
    );
    press(&pdf, &mut vcx, "0");
    assert_eq!(zoom(&mut vcx).0, PdfZoom::FitWidth);
    // Fit page: the whole page in the view's height.
    let fit = vcx.debug_bounds("pdf-viewer-fit-page").expect("Fit page");
    vcx.simulate_click(fit.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    let (z, effective) = zoom(&mut vcx);
    assert_eq!(z, PdfZoom::FitPage);
    let page = vcx.debug_bounds("pdf-page-2").expect("page 2");
    let body = vcx.debug_bounds("pdf-viewer-pages").expect("pages");
    assert!(page.size.height <= body.size.height, "{page:?} {body:?}");
    assert!(effective < 1.0);
    // The window's zoom commands zoom the pages while they have the
    // keyboard.
    vcx.update(|window, cx| {
        let focus = pdf.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        window.dispatch_action(Box::new(crate::keymap::actions::view::ZoomReset), cx);
    });
    draw(&mut vcx);
    assert_eq!(zoom(&mut vcx).0, PdfZoom::FitWidth);
}

#[gpui::test]
fn a_page_fragment_opens_at_that_page(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf::pages(&[LETTER; 8]));
    let (window, viewer) = open(cx, &file, Some(Arc::clone(&engine)), Some(5));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    pdf.read_with(&vcx, |p, _| {
        assert_eq!(p.current_page(), 5);
        assert!(p.page_drawn(5));
        assert!(!p.page_drawn(0), "the first page is never drawn");
    });
    assert_eq!(engine.rendered_pages()[0], 5, "the page asked for first");
    assert!(vcx.debug_bounds("pdf-page-6").is_some());
}

#[gpui::test]
fn pdfs_that_cannot_be_shown_say_why_and_offer_the_default_app(cx: &mut TestAppContext) {
    use crate::a11y::Role;
    use crate::a11y::testing::{named, with_role};

    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf {
        open_error: Some("The PDF is protected by a password.".into()),
        ..FakePdf::pages(&[LETTER])
    });
    let (window, viewer) = open(cx, &file, Some(engine), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let ViewerStatus::NotShown(why) = viewer.read_with(&vcx, |v, _| v.status().clone()) else {
        panic!("not shown");
    };
    assert_eq!(why.as_ref(), "The PDF is protected by a password.");
    let status = with_role(&mut vcx, Role::Status);
    assert!(
        status[0]
            .label
            .as_deref()
            .is_some_and(|l| l.contains("password"))
    );
    assert_eq!(
        named(&mut vcx, "Open with default app").role,
        Some(Role::Button)
    );
}

#[gpui::test]
fn a_pdf_without_an_engine_or_over_the_cap_is_not_read(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let (window, viewer) = open(cx, &file, None, None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let ViewerStatus::NotShown(why) = viewer.read_with(&vcx, |v, _| v.status().clone()) else {
        panic!("not shown");
    };
    assert!(why.contains("not available"), "{why}");
    assert!(vcx.debug_bounds("file-viewer-open-externally").is_some());

    // Too large: the engine is not even asked (a sparse file).
    let big = dir.path().join("big.pdf");
    std::fs::File::create(&big)
        .and_then(|f| f.set_len(PDF_MAX_BYTES + 1))
        .expect("sparse file");
    let engine = Arc::new(FakePdf::pages(&[LETTER]));
    vcx.update(|_, cx| {
        crate::export::set_pdf_engine(cx, Some(Arc::clone(&engine) as Arc<dyn PdfEngine>));
        viewer.update(cx, |v, cx| v.load(big.clone(), cx));
    });
    draw(&mut vcx);
    let ViewerStatus::NotShown(why) = viewer.read_with(&vcx, |v, _| v.status().clone()) else {
        panic!("not shown");
    };
    assert!(why.contains("too large"), "{why}");
    assert!(!engine.opened.load(Ordering::Acquire));
}

#[gpui::test]
fn a_page_that_cannot_be_drawn_says_why_in_its_place(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf {
        page_error: Some((0, "the PDF engine took longer than 5 s".into())),
        ..FakePdf::pages(&[LETTER; 3])
    });
    let (window, viewer) = open(cx, &file, Some(Arc::clone(&engine)), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    pdf.read_with(&vcx, |p, _| {
        assert!(!p.page_drawn(0));
        assert!(
            p.page_error(0)
                .is_some_and(|why| why.contains("longer than 5 s"))
        );
        // The next page still draws, and the failed one is not retried
        // at the same scale.
        assert!(p.page_drawn(1));
    });
    draw(&mut vcx);
    assert_eq!(
        engine.rendered_pages().iter().filter(|p| **p == 0).count(),
        1
    );
}

/// The PDF viewer for assistive technology: a named group, a toolbar of
/// named buttons, the page and the zoom as polite statuses, and the pages
/// as a list of images named "Page N of M".
#[gpui::test]
fn the_pdf_viewer_names_its_pages_and_controls(cx: &mut TestAppContext) {
    use crate::a11y::testing::{named, with_role};
    use crate::a11y::{Live, Role, Toggled};

    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf::pages(&[LETTER; 5]));
    let (window, _viewer) = open(cx, &file, Some(engine), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);

    assert_eq!(
        named(&mut vcx, "PDF viewer: paper.pdf").role,
        Some(Role::Group)
    );
    assert_eq!(named(&mut vcx, "PDF").role, Some(Role::Toolbar));
    assert_eq!(named(&mut vcx, "Pages").role, Some(Role::List));
    let first = named(&mut vcx, "Page 1 of 5");
    assert_eq!(first.role, Some(Role::Image));
    assert_eq!(named(&mut vcx, "Page 2 of 5").role, Some(Role::Image));
    let page = named(&mut vcx, "Page");
    assert_eq!(page.role, Some(Role::Status));
    assert_eq!(page.live, Some(Live::Polite));
    assert_eq!(page.value.as_deref(), Some("Page 1 of 5"));
    let zoom = named(&mut vcx, "Zoom");
    assert_eq!(zoom.role, Some(Role::Status));
    assert!(zoom.value.as_deref().is_some_and(|v| v.ends_with(" %")));
    assert_eq!(named(&mut vcx, "Fit width").toggled, Some(Toggled::True));
    assert_eq!(named(&mut vcx, "Fit page").toggled, Some(Toggled::False));
    let buttons: Vec<String> = with_role(&mut vcx, Role::Button)
        .into_iter()
        .inspect(|b| assert!(b.focusable && b.clickable, "{b:?}"))
        .filter_map(|b| b.label)
        .collect();
    assert_eq!(
        buttons,
        [
            "Previous page",
            "Next page",
            "Go to page",
            "Fit width",
            "Fit page",
            "Zoom out",
            "Zoom in"
        ]
    );
}

/// A restored place comes back exactly (page, offset in the page, zoom);
/// a page past the end is the last page.
#[gpui::test]
fn a_restored_place_comes_back(cx: &mut TestAppContext) {
    use super::PdfPlace;

    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf::pages(&[LETTER; 8]));
    let (window, viewer) = open(cx, &file, Some(Arc::clone(&engine)), None);
    let place = PdfPlace {
        page: 3,
        into: Some(120.0),
        zoom: PdfZoom::Scale(1.5),
    };
    cx.update(|cx| viewer.update(cx, |v, _| v.restore_pdf(place)));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    pdf.read_with(&vcx, |p, _| {
        assert_eq!(p.current_page(), 3);
        assert_eq!(p.zoom(), PdfZoom::Scale(1.5));
        let back = p.place();
        assert_eq!(back.page, 3);
        assert!(
            (back.into.expect("an offset") - 120.0).abs() < 0.5,
            "{back:?}"
        );
    });
    assert_eq!(
        engine.rendered_pages()[0],
        3,
        "the restored page is drawn first"
    );

    // The PDF is shorter now: the last page.
    let viewer = vcx.update(|_, cx| FileViewer::open(file.clone(), None, cx));
    viewer.update(&mut vcx, |v, _| {
        v.restore_pdf(PdfPlace {
            page: 20,
            into: Some(50.0),
            zoom: PdfZoom::FitWidth,
        });
    });
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    pdf.read_with(&vcx, |p, _| assert_eq!(p.current_page(), 7));
}

#[test]
fn a_place_from_a_damaged_session_is_made_usable() {
    use super::PdfPlace;

    let place = PdfPlace {
        page: 2,
        into: Some(f32::NAN),
        zoom: PdfZoom::Scale(40.0),
    }
    .sanitized();
    assert_eq!(place.into, None);
    assert_eq!(place.zoom, PdfZoom::Scale(4.0));
    let place = PdfPlace {
        zoom: PdfZoom::Scale(f32::INFINITY),
        ..place
    };
    assert_eq!(place.sanitized().zoom, PdfZoom::FitWidth);
    assert!(PdfPlace::default().is_start());
}

// ----- find and selection -------------------------------------------------------------

/// A PDF of Letter pages with `texts`, open and drawn.
fn open_text(
    cx: &mut TestAppContext,
    texts: &[&str],
) -> (
    tempfile::TempDir,
    Arc<FakePdf>,
    VisualTestContext,
    Entity<FileViewer>,
    Entity<PdfViewer>,
) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let engine = Arc::new(FakePdf {
        texts: texts.iter().map(|t| (*t).to_owned()).collect(),
        ..FakePdf::pages(&vec![LETTER; texts.len()])
    });
    let (window, viewer) = open(cx, &file, Some(Arc::clone(&engine)), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    (dir, engine, vcx, viewer, pdf)
}

fn find_bar(pdf: &Entity<PdfViewer>, vcx: &mut VisualTestContext) -> Entity<FindBar> {
    pdf.read_with(vcx, |p, _| p.find_bar().cloned())
        .expect("the find popover exists")
}

/// What the match count says (and what assistive technology hears).
fn count(pdf: &Entity<PdfViewer>, vcx: &mut VisualTestContext) -> Option<String> {
    let bar = find_bar(pdf, vcx);
    bar.read_with(vcx, |b, _| b.count_text())
}

fn current(pdf: &Entity<PdfViewer>, vcx: &mut VisualTestContext) -> Option<(usize, String)> {
    pdf.read_with(vcx, |p, _| p.current_match())
}

/// Presses `keys` wherever the focus is, and draws.
fn keys(vcx: &mut VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    draw(vcx);
}

const TEXTS: [&str; 5] = [
    "Alpha beta",
    "nothing here",
    "ALPHA and alpha",
    "",
    "the alphabet",
];

#[gpui::test]
fn ctrl_f_finds_across_the_pages_and_steps_through_the_matches(cx: &mut TestAppContext) {
    let (_dir, engine, mut vcx, _viewer, pdf) = open_text(cx, &TEXTS);
    // No text is read before it is needed.
    assert!(engine.texted_pages().is_empty());

    press(&pdf, &mut vcx, "ctrl-f");
    assert!(pdf.read_with(&vcx, |p, _| p.is_find_open()));
    assert!(vcx.debug_bounds("find-bar").is_some());
    // Every page's text was read in the background, once each, from the
    // page shown on.
    assert_eq!(engine.texted_pages(), [0, 1, 2, 3, 4]);
    pdf.read_with(&vcx, |p, _| {
        assert_eq!(p.page_text(2), Some("ALPHA and alpha"));
        assert_eq!(p.page_text(3), Some(""));
    });

    // The field has the keyboard; case is ignored.
    vcx.simulate_input("alpha");
    draw(&mut vcx);
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("1 of 4"));
    assert_eq!(current(&pdf, &mut vcx), Some((0, "Alpha".to_owned())));
    assert!(vcx.debug_bounds("pdf-current-match").is_some());

    // Enter and Shift+Enter, F3 and Shift+F3: next and previous; the view
    // follows the current match.
    keys(&mut vcx, "enter");
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("2 of 4"));
    assert_eq!(current(&pdf, &mut vcx), Some((2, "ALPHA".to_owned())));
    assert_eq!(pdf.read_with(&vcx, |p, _| p.current_page()), 2);
    assert!(vcx.debug_bounds("pdf-match").is_some(), "the other match");
    keys(&mut vcx, "f3");
    assert_eq!(current(&pdf, &mut vcx), Some((2, "alpha".to_owned())));
    keys(&mut vcx, "f3");
    assert_eq!(current(&pdf, &mut vcx), Some((4, "alpha".to_owned())));
    assert_eq!(pdf.read_with(&vcx, |p, _| p.current_page()), 4);
    // Wraps around.
    keys(&mut vcx, "f3");
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("1 of 4"));
    keys(&mut vcx, "shift-f3");
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("4 of 4"));
    keys(&mut vcx, "shift-enter");
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("3 of 4"));

    // Typing on keeps the place; no match says so.
    vcx.simulate_input("b");
    draw(&mut vcx);
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("1 of 1"));
    assert_eq!(current(&pdf, &mut vcx), Some((4, "alphab".to_owned())));
    vcx.simulate_input("x");
    draw(&mut vcx);
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("No results"));
    assert_eq!(current(&pdf, &mut vcx), None);

    // Escape closes it, drops the highlights and gives the keyboard back
    // to the pages; the text read is kept.
    keys(&mut vcx, "escape");
    assert!(!pdf.read_with(&vcx, |p, _| p.is_find_open()));
    assert!(vcx.debug_bounds("find-bar").is_none());
    assert!(vcx.debug_bounds("pdf-match").is_none());
    let focused = vcx.update(|window, cx| pdf.read(cx).focus_handle(cx).is_focused(window));
    assert!(focused, "the pages have the keyboard");
    // F3 with find closed opens it again, with the same query.
    keys(&mut vcx, "f3");
    assert!(pdf.read_with(&vcx, |p, _| p.is_find_open()));
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("No results"));
    assert_eq!(engine.texted_pages().len(), 5, "not read again");
}

#[gpui::test]
fn pages_read_later_add_their_matches_from_the_page_shown(cx: &mut TestAppContext) {
    let (_dir, engine, mut vcx, viewer, pdf) = open_text(cx, &TEXTS);
    vcx.update(|_, cx| viewer.update(cx, |v, cx| v.go_to_page(3, cx)));
    draw(&mut vcx);
    // Find opens and the query is typed before any text comes.
    vcx.update(|window, cx| {
        pdf.read(cx).focus_handle(cx).focus(window, cx);
        window.dispatch_action(Box::new(crate::find::Find), cx);
    });
    vcx.update(|window, cx| {
        let bar = pdf.read(cx).find_bar().cloned().expect("the popover");
        bar.update(cx, |bar, cx| {
            bar.set_query("alpha", window, cx);
            cx.emit(crate::find::FindBarEvent::Changed);
        });
    });
    // The count says the pages are still being read.
    assert!(pdf.read_with(&vcx, |p, _| p.is_searching()));
    assert_eq!(
        count(&pdf, &mut vcx).as_deref(),
        Some("No results (searching\u{2026})")
    );
    draw(&mut vcx);
    assert!(!pdf.read_with(&vcx, |p, _| p.is_searching()));
    // Read from the page shown on, then from the start; the first match
    // at or after the page shown is the current one.
    assert_eq!(engine.texted_pages(), [3, 4, 0, 1, 2]);
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("4 of 4"));
    assert_eq!(current(&pdf, &mut vcx), Some((4, "alpha".to_owned())));
    keys(&mut vcx, "enter");
    assert_eq!(current(&pdf, &mut vcx), Some((0, "Alpha".to_owned())));
    assert_eq!(pdf.read_with(&vcx, |p, _| p.current_page()), 0);
}

#[gpui::test]
fn find_runs_on_to_the_next_page_and_over_a_line_end_hyphen(cx: &mut TestAppContext) {
    let (_dir, _engine, mut vcx, _viewer, pdf) = open_text(
        cx,
        &["Read to the end of", "page two, the infor-\nmation age"],
    );
    press(&pdf, &mut vcx, "ctrl-f");
    // From the end of page 1 on to page 2: one match.
    vcx.simulate_input("of page");
    draw(&mut vcx);
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("1 of 1"));
    assert_eq!(current(&pdf, &mut vcx), Some((0, "of\npage".to_owned())));
    assert!(vcx.debug_bounds("pdf-current-match").is_some());
    // A word split by a hyphen at a line end, found whole.
    let bar = find_bar(&pdf, &mut vcx);
    vcx.update(|window, cx| {
        bar.update(cx, |bar, cx| {
            bar.set_query("information", window, cx);
            cx.emit(crate::find::FindBarEvent::Changed);
        });
    });
    draw(&mut vcx);
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("1 of 1"));
    assert_eq!(
        current(&pdf, &mut vcx),
        Some((1, "infor-\nmation".to_owned()))
    );
}

#[gpui::test]
fn dragging_selects_text_that_ctrl_c_copies(cx: &mut TestAppContext) {
    let (_dir, engine, mut vcx, _viewer, pdf) =
        open_text(cx, &["Hello world\nSecond line", "Page two"]);
    let page = vcx.debug_bounds("pdf-page-1").expect("page 1");
    let ppt = pdf.read_with(&vcx, |p, _| p.effective_zoom()) * PX_PER_PT;
    // Points on the page as window pixels (inside its 1 px frame).
    let at = |x: f32, y: f32| {
        point(
            page.origin.x + px(1.0 + x * ppt),
            page.origin.y + px(1.0 + y * ppt),
        )
    };
    // From the left half of "H" to the right half of "d".
    vcx.simulate_mouse_down(
        at(73.0, 78.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_move(
        at(100.0, 80.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_move(
        at(137.0, 78.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_up(
        at(137.0, 78.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    draw(&mut vcx);
    // The page's text was read for it (only that page).
    assert_eq!(engine.texted_pages(), [0]);
    assert_eq!(
        pdf.read_with(&vcx, |p, _| p.selected_text()).as_deref(),
        Some("Hello world")
    );
    assert!(vcx.debug_bounds("pdf-selection").is_some());
    press(&pdf, &mut vcx, "ctrl-c");
    let copied = vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied.as_deref(), Some("Hello world"));

    // Down to the next line, from "w".
    vcx.simulate_mouse_down(
        at(108.0, 78.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_move(
        at(85.0, 97.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_up(
        at(85.0, 97.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    draw(&mut vcx);
    assert_eq!(
        pdf.read_with(&vcx, |p, _| p.selected_text()).as_deref(),
        Some("world\nSe")
    );

    // A click drops the selection; Ctrl+A selects the page's text.
    vcx.simulate_click(at(300.0, 400.0), gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(pdf.read_with(&vcx, |p, _| p.selected_text()), None);
    press(&pdf, &mut vcx, "ctrl-a");
    press(&pdf, &mut vcx, "ctrl-c");
    let copied = vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied.as_deref(), Some("Hello world\nSecond line"));

    // The selection fills the find field.
    vcx.simulate_mouse_down(
        at(73.0, 78.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_move(
        at(101.0, 78.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.simulate_mouse_up(
        at(101.0, 78.0),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    draw(&mut vcx);
    press(&pdf, &mut vcx, "ctrl-f");
    let bar = find_bar(&pdf, &mut vcx);
    assert_eq!(bar.read_with(&vcx, |b, cx| b.query(cx)), "Hello");
    assert_eq!(count(&pdf, &mut vcx).as_deref(), Some("1 of 1"));
}

/// Points on page 1 as window pixels (inside its 1 px frame).
fn page_at(
    pdf: &Entity<PdfViewer>,
    vcx: &mut VisualTestContext,
) -> impl Fn(f32, f32) -> Point<Pixels> + use<> {
    let page = vcx.debug_bounds("pdf-page-1").expect("page 1");
    let ppt = pdf.read_with(vcx, |p, _| p.effective_zoom()) * PX_PER_PT;
    move |x: f32, y: f32| {
        point(
            page.origin.x + px(1.0 + x * ppt),
            page.origin.y + px(1.0 + y * ppt),
        )
    }
}

/// A press of the left button at `at`, the `clicks`th in a row.
fn click(vcx: &mut VisualTestContext, at: Point<Pixels>, clicks: usize, modifiers: Modifiers) {
    vcx.simulate_event(MouseDownEvent {
        position: at,
        button: MouseButton::Left,
        modifiers,
        click_count: clicks,
        first_mouse: false,
    });
    vcx.simulate_event(MouseUpEvent {
        position: at,
        button: MouseButton::Left,
        modifiers,
        click_count: clicks,
    });
    draw(vcx);
}

fn selected(pdf: &Entity<PdfViewer>, vcx: &mut VisualTestContext) -> Option<String> {
    pdf.read_with(vcx, |p, _| p.selected_text())
}

#[gpui::test]
fn double_and_triple_clicks_select_a_word_and_a_line(cx: &mut TestAppContext) {
    let (_dir, _engine, mut vcx, _viewer, pdf) =
        open_text(cx, &["Hello wide world\nSecond line here"]);
    let at = page_at(&pdf, &mut vcx);
    // Character k of line l: its middle.
    let ch = |k: f32, l: f32| at(72.0 + 6.0 * k + 3.0, 78.0 + 20.0 * l);
    let none = Modifiers::none();

    // A double click on "wide" (its "i") selects it.
    click(&mut vcx, ch(7.0, 0.0), 1, none);
    click(&mut vcx, ch(7.0, 0.0), 2, none);
    assert_eq!(selected(&pdf, &mut vcx).as_deref(), Some("wide"));
    assert!(vcx.debug_bounds("pdf-selection").is_some());

    // A triple click selects the line, without its line end.
    click(&mut vcx, ch(7.0, 0.0), 3, none);
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("Hello wide world")
    );

    // Dragging on from a double click grows by whole words.
    vcx.simulate_event(MouseDownEvent {
        position: ch(7.0, 0.0),
        button: MouseButton::Left,
        modifiers: none,
        click_count: 2,
        first_mouse: false,
    });
    vcx.simulate_mouse_move(ch(1.0, 1.0), MouseButton::Left, none);
    vcx.simulate_mouse_up(ch(1.0, 1.0), MouseButton::Left, none);
    draw(&mut vcx);
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("wide world\nSecond")
    );

    // Dragging on from a triple click grows by whole lines.
    vcx.simulate_event(MouseDownEvent {
        position: ch(2.0, 1.0),
        button: MouseButton::Left,
        modifiers: none,
        click_count: 3,
        first_mouse: false,
    });
    vcx.simulate_mouse_move(ch(13.0, 0.0), MouseButton::Left, none);
    vcx.simulate_mouse_up(ch(13.0, 0.0), MouseButton::Left, none);
    draw(&mut vcx);
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("Hello wide world\nSecond line here")
    );
}

#[gpui::test]
fn shift_click_extends_the_selection(cx: &mut TestAppContext) {
    let (_dir, _engine, mut vcx, _viewer, pdf) =
        open_text(cx, &["Hello wide world\nSecond line here", "Page two"]);
    let at = page_at(&pdf, &mut vcx);
    let ch = |k: f32, l: f32| at(72.0 + 6.0 * k + 1.0, 78.0 + 20.0 * l);
    let none = Modifiers::none();
    let shift = Modifiers::shift();

    // A click before "w" of "wide", then Shift+click before "w" of
    // "world": the text between.
    click(&mut vcx, ch(6.0, 0.0), 1, none);
    assert_eq!(selected(&pdf, &mut vcx), None);
    click(&mut vcx, ch(11.0, 0.0), 1, shift);
    assert_eq!(selected(&pdf, &mut vcx).as_deref(), Some("wide "));
    // Again, further on: the same start, a new end.
    click(&mut vcx, ch(6.0, 1.0), 1, shift);
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("wide world\nSecond")
    );
    // Back before the start: the other way.
    click(&mut vcx, ch(0.0, 0.0), 1, shift);
    assert_eq!(selected(&pdf, &mut vcx).as_deref(), Some("Hello "));

    // A Shift+click extends a double-clicked word too, by words.
    click(&mut vcx, ch(13.0, 0.0), 2, none);
    assert_eq!(selected(&pdf, &mut vcx).as_deref(), Some("world"));
    click(&mut vcx, ch(6.0, 0.0), 1, shift);
    assert_eq!(selected(&pdf, &mut vcx).as_deref(), Some("wide world"));

    // After Select all, from the start of the page.
    press(&pdf, &mut vcx, "ctrl-a");
    click(&mut vcx, ch(5.0, 0.0), 1, shift);
    assert_eq!(selected(&pdf, &mut vcx).as_deref(), Some("Hello"));

    // A plain click drops it.
    click(&mut vcx, ch(3.0, 1.0), 1, none);
    assert_eq!(selected(&pdf, &mut vcx), None);
}

#[gpui::test]
fn dragging_near_an_edge_scrolls_the_view(cx: &mut TestAppContext) {
    // Page 1 has 30 lines, taller than the view.
    let lines: Vec<String> = (0..30).map(|k| format!("line {k:02}")).collect();
    let text = lines.join("\n");
    let (_dir, _engine, mut vcx, _viewer, pdf) = open_text(cx, &[text.as_str(), "Page two"]);
    let at = page_at(&pdf, &mut vcx);
    let view = vcx.debug_bounds("pdf-viewer-pages").expect("the pages");
    let none = Modifiers::none();
    // How far down the view is: the page at the top, then how far into it.
    let top = |vcx: &mut VisualTestContext| {
        pdf.read_with(vcx, |p, _| {
            let place = p.place();
            (place.page, place.into.unwrap_or(0.0))
        })
    };

    // Press on the first line, then drag to the middle of the view: no
    // scrolling.
    let start = top(&mut vcx);
    vcx.simulate_mouse_down(at(73.0, 78.0), MouseButton::Left, none);
    // Right of the lines: up to their ends.
    let middle = point(at(300.0, 78.0).x, view.center().y);
    vcx.simulate_mouse_move(middle, MouseButton::Left, none);
    vcx.executor().advance_clock(Duration::from_millis(200));
    draw(&mut vcx);
    assert_eq!(top(&mut vcx), start, "not near an edge");
    let before = selected(&pdf, &mut vcx).expect("a selection");
    assert!(before.starts_with("line 00") && !before.contains("line 29"));

    // Near the bottom edge: the view scrolls while the pointer stays, and
    // the selection follows down to the page's last line.
    let near_bottom = point(middle.x, view.bottom() - px(4.0));
    vcx.simulate_mouse_move(near_bottom, MouseButton::Left, none);
    for _ in 0..20 {
        vcx.executor().advance_clock(Duration::from_millis(100));
        draw(&mut vcx);
    }
    let scrolled = top(&mut vcx);
    assert!(scrolled > start, "{scrolled:?} > {start:?}");
    let after = selected(&pdf, &mut vcx).expect("a selection");
    assert!(
        after.starts_with("line 00") && after.contains("line 29"),
        "{after}"
    );

    // Released: the scrolling stops.
    vcx.simulate_mouse_up(near_bottom, MouseButton::Left, none);
    let stopped = top(&mut vcx);
    vcx.executor().advance_clock(Duration::from_millis(500));
    draw(&mut vcx);
    assert_eq!(top(&mut vcx), stopped);

    // Dragging near the top edge scrolls back up (from page 2: page 1 is
    // out of view).
    let page = vcx.debug_bounds("pdf-page-2").expect("page 2 shown");
    let press_at = point(page.center().x, view.center().y);
    vcx.simulate_mouse_down(press_at, MouseButton::Left, none);
    vcx.simulate_mouse_move(
        point(press_at.x, view.top() + px(4.0)),
        MouseButton::Left,
        none,
    );
    for _ in 0..5 {
        vcx.executor().advance_clock(Duration::from_millis(100));
        draw(&mut vcx);
    }
    assert!(top(&mut vcx) < stopped);
    vcx.simulate_mouse_up(point(press_at.x, view.top()), MouseButton::Left, none);
}

/// Points on page `page` (from 1) as window pixels (inside its 1 px
/// frame).
fn on_page(
    pdf: &Entity<PdfViewer>,
    vcx: &mut VisualTestContext,
    page: usize,
) -> impl Fn(f32, f32) -> Point<Pixels> + use<> {
    let name: &'static str = ["pdf-page-1", "pdf-page-2", "pdf-page-3"][page - 1];
    let bounds = vcx
        .debug_bounds(name)
        .unwrap_or_else(|| panic!("page {page} laid out"));
    let ppt = pdf.read_with(vcx, |p, _| p.effective_zoom()) * PX_PER_PT;
    move |x: f32, y: f32| {
        point(
            bounds.origin.x + px(1.0 + x * ppt),
            bounds.origin.y + px(1.0 + y * ppt),
        )
    }
}

#[gpui::test]
fn a_selection_runs_across_pages(cx: &mut TestAppContext) {
    // Three short pages, all in view at 75 %.
    let dir = tempfile::tempdir().expect("temp dir");
    let file = pdf_file(dir.path());
    let texts = [
        "First line\nend of first",
        "Middle page",
        "Third page\nlast",
    ];
    let engine = Arc::new(FakePdf {
        texts: texts.iter().map(|t| (*t).to_owned()).collect(),
        ..FakePdf::pages(&[(300.0, 150.0); 3])
    });
    let (window, viewer) = open(cx, &file, Some(Arc::clone(&engine)), None);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let pdf = pdf_of(&viewer, &mut vcx);
    vcx.update(|_, cx| pdf.update(cx, |p, cx| p.set_zoom(PdfZoom::Scale(0.75), cx)));
    draw(&mut vcx);
    let none = Modifiers::none();
    // Character k of line l on a page: its left and right quarter.
    let pages: Vec<_> = (1..=3).map(|p| on_page(&pdf, &mut vcx, p)).collect();
    let before = |p: usize, k: f32, l: f32| pages[p](72.0 + 6.0 * k + 1.0, 78.0 + 20.0 * l);
    let after = |p: usize, k: f32, l: f32| pages[p](72.0 + 6.0 * k + 5.0, 78.0 + 20.0 * l);

    // From before "end" on page 1 to after "Third" on page 3: the rest of
    // page 1, page 2 whole and the start of page 3. Each page's text is
    // read as the selection reaches it, its ends first.
    vcx.simulate_mouse_down(before(0, 0.0, 1.0), MouseButton::Left, none);
    vcx.simulate_mouse_move(after(2, 4.0, 0.0), MouseButton::Left, none);
    vcx.simulate_mouse_up(after(2, 4.0, 0.0), MouseButton::Left, none);
    draw(&mut vcx);
    assert_eq!(engine.texted_pages(), [0, 2, 1]);
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("end of first\nMiddle page\nThird")
    );
    assert!(vcx.debug_bounds("pdf-selection").is_some());
    press(&pdf, &mut vcx, "ctrl-c");
    let copied = vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied.as_deref(), Some("end of first\nMiddle page\nThird"));

    // Dragging the other way, up into the gap above page 2: the end of
    // page 1 back to the press.
    vcx.simulate_mouse_down(after(1, 5.0, 0.0), MouseButton::Left, none);
    let gap = pages[1](10.0, 0.0) - point(px(0.0), px(5.0));
    vcx.simulate_mouse_move(gap, MouseButton::Left, none);
    vcx.simulate_mouse_up(gap, MouseButton::Left, none);
    draw(&mut vcx);
    assert_eq!(selected(&pdf, &mut vcx).as_deref(), Some("Middle"));
    // Past page 1's end, below it: the rest of page 1.
    vcx.simulate_mouse_down(before(0, 6.0, 0.0), MouseButton::Left, none);
    let below = pages[0](10.0, 150.0) + point(px(0.0), px(2.0));
    vcx.simulate_mouse_move(below, MouseButton::Left, none);
    vcx.simulate_mouse_up(below, MouseButton::Left, none);
    draw(&mut vcx);
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("line\nend of first")
    );

    // Shift+click on another page moves the end there.
    click(&mut vcx, before(2, 0.0, 1.0), 1, none);
    click(&mut vcx, before(0, 4.0, 1.0), 1, Modifiers::shift());
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("of first\nMiddle page\nThird page\n")
    );
    // After Select all (page 1, the page shown), from its start.
    press(&pdf, &mut vcx, "ctrl-a");
    click(&mut vcx, after(1, 5.0, 0.0), 1, Modifiers::shift());
    assert_eq!(
        selected(&pdf, &mut vcx).as_deref(),
        Some("First line\nend of first\nMiddle")
    );
}

#[gpui::test]
fn copying_text_not_read_yet_waits_for_it(cx: &mut TestAppContext) {
    let (_dir, engine, mut vcx, _viewer, pdf) = open_text(cx, &["Hello world", "Page two"]);
    vcx.update(|window, cx| pdf.read(cx).focus_handle(cx).focus(window, cx));
    draw(&mut vcx);
    assert!(engine.texted_pages().is_empty());
    // Select all and copy before the page's text has come.
    vcx.update(|window, cx| {
        window.dispatch_action(Box::new(crate::keymap::actions::editor::SelectAll), cx);
        window.dispatch_action(Box::new(crate::keymap::actions::editor::Copy), cx);
    });
    draw(&mut vcx);
    assert_eq!(engine.texted_pages(), [0]);
    let copied = vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied.as_deref(), Some("Hello world"));
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The PDF viewer in a headless window, with a PDF engine of its own
//! ([`FakePdf`]): pages, drawing only what is in view, navigation, zoom,
//! `#page=` and the error states, and what assistive technology is told.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use gpui::{
    AnyWindowHandle, Bounds, Entity, Focusable as _, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};

use super::pdf::{PX_PER_PT, PdfViewer, PdfZoom};
use super::{FileViewer, PDF_MAX_BYTES, ViewerKind, ViewerStatus, kind};
use crate::export::{PageRaster, PdfEngine, PdfError};
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
}

impl FakePdf {
    pub fn pages(sizes: &[(f32, f32)]) -> Self {
        Self {
            sizes: sizes.to_vec(),
            ..Self::default()
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

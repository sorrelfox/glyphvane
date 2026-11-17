// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The export options dialog in headless windows: the rows each kind
//! shows, the controls changing the choices, Enter, Esc and the buttons,
//! the choices remembered for the run, the PDF preview redrawn as the
//! options change, and the dialog's accessibility.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_export::{ImageMode, Paper};
use gv_syntax::ParseOptions;

use super::dialog::{self, ExportDialog, Preview};
use super::{ExportChoices, ExportKind, ExportRequest, PageRaster, PdfError, PreviewFn, dialog_of};
use crate::a11y::Role;
use crate::a11y::testing;
use crate::editor::EditorView;
use crate::{AppSettings, ThemeChoice};

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// What the dialog's callback heard: `Some(choices)` or `None` (cancel).
type Answers = Rc<RefCell<Vec<Option<ExportChoices>>>>;

/// The choices each preview was asked for.
type Asked = Arc<Mutex<Vec<ExportChoices>>>;

struct Fixture {
    vcx: VisualTestContext,
    editor: Entity<EditorView>,
    answers: Answers,
}

/// A window holding a focused Source view.
fn window(cx: &mut TestAppContext) -> Fixture {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    };
    let (window, editor) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options, cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_text("# Note\n", cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| editor.read(cx).focus_handle(cx).focus(window, cx));
    draw(&mut vcx);
    Fixture {
        vcx,
        editor,
        answers: Rc::default(),
    }
}

/// A preview that records what it was asked and draws a grey page of the
/// box's size, or fails.
fn fake_preview(asked: &Asked, fail: bool) -> PreviewFn {
    let asked = asked.clone();
    Arc::new(
        move |choices: &ExportChoices, (w, h): (u32, u32), _: &AtomicBool| {
            asked.lock().expect("lock").push(choices.clone());
            if fail {
                return Err(PdfError::Failed("the engine is away".into()));
            }
            Ok(PageRaster {
                width: w / 2,
                height: h / 2,
                rgba: vec![200; (w / 2 * h / 2 * 4) as usize],
                pages: 3,
            })
        },
    )
}

impl Fixture {
    fn open(&mut self, kind: ExportKind, preview: Option<PreviewFn>) {
        let answers = self.answers.clone();
        self.vcx.update(|window, cx| {
            let request = ExportRequest {
                kind,
                choices: dialog::remembered(kind, cx),
                preview,
                on_done: Rc::new(move |choices, _, _| answers.borrow_mut().push(choices)),
            };
            assert!(dialog::open(request, window, cx), "the root has the dialog");
        });
        draw(&mut self.vcx);
    }

    fn dialog(&mut self) -> Entity<ExportDialog> {
        self.vcx
            .update(|window, cx| dialog_of(window, cx))
            .expect("the window root has an export dialog")
    }

    fn is_open(&mut self) -> bool {
        let open = self.dialog().read_with(&self.vcx, |d, _| d.is_open());
        assert_eq!(open, self.drawn("export-dialog"));
        open
    }

    fn choices(&mut self) -> ExportChoices {
        self.dialog()
            .read_with(&self.vcx, |d, _| d.choices().clone())
    }

    fn preview(&mut self) -> Option<Preview> {
        self.dialog()
            .read_with(&self.vcx, |d, _| d.preview().cloned())
    }

    fn drawn(&mut self, selector: &str) -> bool {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        self.vcx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &str) {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        let at = self
            .vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }

    /// Presses and releases `key` (controls act on the release).
    fn press(&mut self, key: &'static str) {
        self.vcx.simulate_keystrokes(key);
        self.vcx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).expect("a key"),
        });
        draw(&mut self.vcx);
    }

    fn answers(&self) -> Vec<Option<ExportChoices>> {
        self.answers.borrow().clone()
    }

    fn editor_focused(&mut self) -> bool {
        let editor = self.editor.clone();
        self.vcx
            .update(|window, cx| editor.read(cx).focus_handle(cx).is_focused(window))
    }
}

#[gpui::test]
fn html_offers_contents_and_images(cx: &mut TestAppContext) {
    let mut f = window(cx);
    f.open(ExportKind::Html, None);
    assert!(f.is_open());
    assert!(f.drawn("export-toc-switch"));
    assert!(f.drawn("export-images-embed") && f.drawn("export-images-assets"));
    for absent in [
        "export-paper-a4",
        "export-landscape",
        "export-margin-plus",
        "export-header-switch",
        "export-preview",
    ] {
        assert!(!f.drawn(absent), "{absent}");
    }
    f.click("export-toc-switch");
    f.click("export-images-assets");
    let choices = f.choices();
    assert!(choices.toc);
    assert_eq!(choices.images, ImageMode::Assets);

    // Enter on the dialog itself confirms; the keyboard goes back.
    f.keys("enter");
    assert!(!f.is_open());
    assert_eq!(f.answers(), vec![Some(choices.clone())]);
    assert!(f.editor_focused());

    // The choices are remembered for the next export, not for the others.
    f.vcx.update(|_, cx| {
        assert_eq!(dialog::remembered(ExportKind::Html, cx), choices);
        assert_eq!(
            dialog::remembered(ExportKind::Pdf, cx),
            ExportChoices::default()
        );
    });
    f.open(ExportKind::Html, None);
    assert_eq!(f.choices(), choices);
}

#[gpui::test]
fn escape_cancel_and_a_click_outside_close_without_exporting(cx: &mut TestAppContext) {
    let mut f = window(cx);
    f.open(ExportKind::Html, None);
    f.click("export-toc-switch");
    f.keys("escape");
    assert!(!f.is_open());
    assert!(f.editor_focused());

    f.open(ExportKind::Html, None);
    f.click("export-cancel");
    assert!(!f.is_open());

    f.open(ExportKind::Html, None);
    f.vcx
        .simulate_click(point(px(5.0), px(790.0)), Modifiers::none());
    draw(&mut f.vcx);
    assert!(!f.is_open());

    assert_eq!(f.answers(), vec![None, None, None]);
    // Nothing cancelled is remembered.
    f.vcx.update(|_, cx| {
        assert!(!dialog::remembered(ExportKind::Html, cx).toc);
    });
}

#[gpui::test]
fn pdf_sets_the_page_and_previews_it(cx: &mut TestAppContext) {
    let mut f = window(cx);
    let asked: Asked = Arc::default();
    f.open(ExportKind::Pdf, Some(fake_preview(&asked, false)));
    for row in [
        "export-paper-a4",
        "export-paper-letter",
        "export-portrait",
        "export-landscape",
        "export-margin-minus",
        "export-margin-plus",
        "export-header-switch",
        "export-page-numbers-switch",
        "export-page-breaks-switch",
        "export-toc-switch",
        "export-preview",
    ] {
        assert!(f.drawn(row), "{row}");
    }
    assert!(!f.drawn("export-images-embed"));

    // The first page is drawn for the defaults, at twice its box.
    match f.preview() {
        Some(Preview::Ready {
            width,
            height,
            pages,
            ..
        }) => {
            assert_eq!((width, height), (168, 238));
            assert_eq!(pages, 3);
        }
        other => panic!("a drawn preview, not {other:?}"),
    }
    assert_eq!(asked.lock().expect("lock").len(), 1);
    assert!(f.drawn("export-preview-page"));

    f.click("export-paper-letter");
    f.click("export-landscape");
    f.click("export-margin-plus");
    f.click("export-margin-plus");
    f.click("export-header-switch");
    f.click("export-page-numbers-switch");
    f.click("export-page-breaks-switch");
    f.click("export-toc-switch");
    let choices = f.choices();
    assert_eq!(choices.page.paper, Paper::Letter);
    assert!(choices.page.landscape);
    assert_eq!(choices.page.margin_mm, 30.0);
    assert!(!choices.page.header && !choices.page.page_numbers);
    assert!(choices.page.break_before_h1 && choices.toc);

    // Each change drew the preview again, the last one for these choices.
    let asked_now = asked.lock().expect("lock").clone();
    assert_eq!(asked_now.len(), 9);
    assert_eq!(asked_now.last(), Some(&choices));

    f.click("export-confirm");
    assert_eq!(f.answers(), vec![Some(choices)]);
}

#[gpui::test]
fn margins_stay_within_their_range(cx: &mut TestAppContext) {
    let mut f = window(cx);
    f.open(ExportKind::Pdf, None);
    for _ in 0..5 {
        f.click("export-margin-minus");
    }
    assert_eq!(f.choices().page.margin_mm, 5.0);
    for _ in 0..20 {
        if !f.drawn("export-margin-plus") {
            break;
        }
        f.click("export-margin-plus");
    }
    assert_eq!(f.choices().page.margin_mm, 60.0);
}

#[gpui::test]
fn a_failed_preview_says_why(cx: &mut TestAppContext) {
    let mut f = window(cx);
    let asked: Asked = Arc::default();
    f.open(ExportKind::Pdf, Some(fake_preview(&asked, true)));
    match f.preview() {
        Some(Preview::Failed(reason)) => assert_eq!(reason.as_ref(), "the engine is away"),
        other => panic!("a failed preview, not {other:?}"),
    }
    // The export itself is still offered.
    f.click("export-confirm");
    assert_eq!(f.answers().len(), 1);
}

#[gpui::test]
fn print_leaves_the_paper_to_the_print_dialog(cx: &mut TestAppContext) {
    let mut f = window(cx);
    let asked: Asked = Arc::default();
    f.open(ExportKind::Print, Some(fake_preview(&asked, false)));
    for absent in [
        "export-paper-a4",
        "export-landscape",
        "export-margin-plus",
        "export-images-embed",
        "export-preview",
    ] {
        assert!(!f.drawn(absent), "{absent}");
    }
    assert!(f.drawn("export-header-switch") && f.drawn("export-toc-switch"));
    assert!(
        asked.lock().expect("lock").is_empty(),
        "no preview for Print"
    );
    f.click("export-page-numbers-switch");
    f.click("export-confirm");
    let choices = f.answers()[0].clone().expect("confirmed");

    // The print dialog's paper with the dialog's page furniture.
    let from_dialog = gv_export::PageOptions {
        size_mm: Some((210.0, 297.0)),
        margin_mm: 12.0,
        ..gv_export::PageOptions::default()
    };
    let page = choices.print_page(from_dialog.clone());
    assert_eq!(page.size_mm, from_dialog.size_mm);
    assert_eq!(page.margin_mm, 12.0);
    assert!(!page.page_numbers && page.header);
}

#[gpui::test]
fn the_dialog_is_a_named_modal_dialog_with_named_controls(cx: &mut TestAppContext) {
    let mut f = window(cx);
    f.open(ExportKind::Pdf, None);
    let dialogs: Vec<_> = testing::with_role(&mut f.vcx, Role::Dialog)
        .into_iter()
        .filter(|n| n.label.as_deref() == Some("Export as PDF"))
        .collect();
    assert_eq!(dialogs.len(), 1);
    assert!(dialogs[0].modal);
    for label in [
        "Paper",
        "Orientation",
        "Margins",
        "Title on every page",
        "Page numbers",
        "New page for each top heading",
        "Table of contents",
        "Cancel",
        "Export…",
    ] {
        assert!(testing::has(&mut f.vcx, label), "{label}");
    }
    // Tab reaches the controls; Space works a switch, and Enter there does
    // not confirm.
    let toc = f.choices().toc;
    for _ in 0..40 {
        if testing::focused(&mut f.vcx).flatten().as_deref() == Some("Table of contents") {
            break;
        }
        f.keys("tab");
    }
    assert_eq!(
        testing::focused(&mut f.vcx).flatten().as_deref(),
        Some("Table of contents")
    );
    f.press("space");
    assert_eq!(f.choices().toc, !toc);
    f.press("enter");
    assert_eq!(f.choices().toc, toc);
    assert!(f.is_open(), "Enter on a control works the control");
}

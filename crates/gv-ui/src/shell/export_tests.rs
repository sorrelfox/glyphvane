// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! *Export as HTML…*, *Open in browser* and *Copy as rich text* in headless
//! GPUI windows over temporary folders. The save dialog, the launcher and
//! the rich-text clipboard are fakes that record what they were asked, so
//! no dialog shows, nothing is launched and the system clipboard is not
//! touched.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{
    App, AppContext as _, Bounds, Entity, Focusable as _, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_vault::files::journal::JournalStore;

use super::session::{self, Session, SessionStore};
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::export::RichClipboard;
use crate::files::save_as::{SaveAnswer, SavePathDialog, SavePrompt};
use crate::keymap::actions::editor::{CopyAsRichText, SelectAll};
use crate::keymap::actions::file as file_commands;
use crate::platform::Launcher;
use crate::{AppSettings, ThemeChoice, ToggleEditMode};

const NOTE: &str =
    "# Title\n\nSome **bold** text and a [link](other.md).\n\n- [x] done\n\n$$\nx^2\n$$\n";

#[derive(Clone, Default)]
struct FakeSaveDialog {
    answers: Rc<RefCell<VecDeque<SaveAnswer>>>,
    asked: Rc<RefCell<Vec<SavePrompt>>>,
}

impl SavePathDialog for FakeSaveDialog {
    fn ask(&self, prompt: &SavePrompt, _: &mut App) -> LocalBoxFuture<'static, SaveAnswer> {
        self.asked.borrow_mut().push(prompt.clone());
        let answer = self
            .answers
            .borrow_mut()
            .pop_front()
            .expect("the test gave an answer");
        async move { answer }.boxed_local()
    }
}

#[derive(Clone, Default)]
struct FakeLauncher(Rc<RefCell<Vec<PathBuf>>>);

impl Launcher for FakeLauncher {
    fn reveal(&self, _: &Path, _: &mut App) {
        panic!("nothing is revealed");
    }

    fn open_with_default_app(&self, path: &Path, _: &mut App) {
        self.0.borrow_mut().push(path.to_path_buf());
    }

    fn open_url(&self, url: &str, _: &mut App) {
        panic!("no URL is opened: {url}");
    }
}

/// A rich-text clipboard that records what it got, or fails.
#[derive(Clone, Default)]
struct FakeClipboard {
    written: Rc<RefCell<Vec<(String, String)>>>,
    fail: Rc<RefCell<Option<String>>>,
}

impl RichClipboard for FakeClipboard {
    fn write(&self, html: &str, text: &str) -> Result<(), String> {
        if let Some(reason) = self.fail.borrow().clone() {
            return Err(reason);
        }
        self.written
            .borrow_mut()
            .push((html.to_owned(), text.to_owned()));
        Ok(())
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
    dialog: FakeSaveDialog,
    launched: FakeLauncher,
    clipboard: FakeClipboard,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("note.md"), NOTE).expect("write");
    std::fs::create_dir(dir.path().join("config")).expect("config");
    let dialog = FakeSaveDialog::default();
    let launched = FakeLauncher::default();
    let clipboard = FakeClipboard::default();
    let target = Some(OpenTarget::new(dir.path().join("note.md")));
    let journal = dir.path().join("config/journal");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let store = JournalStore::open(journal).expect("journal store");
        crate::files::install_store(cx, Some(store), false);
        crate::files::save_as::set_dialog(cx, dialog.clone());
        crate::platform::set_launcher(cx, launched.clone());
        crate::export::set_rich_clipboard(cx, Some(Rc::new(clipboard.clone())));
        session::install(cx, SessionStore::new(Session::default(), None));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        dir,
        workspace,
        vcx,
        dialog,
        launched,
        clipboard,
    }
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

impl Fixture {
    fn dispatch(&mut self, action: impl gpui::Action) {
        self.vcx.dispatch_action(action);
        draw(&mut self.vcx);
    }

    fn notice(&self) -> Option<String> {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            ws.active_notice(cx).map(|(n, _)| n.title.to_string())
        })
    }

    fn focus_reader(&mut self) {
        let reader = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().reader().cloned())
            .expect("the tab shows its Reading view");
        self.vcx
            .update(|window, cx| reader.read(cx).focus_handle(cx).focus(window, cx));
        draw(&mut self.vcx);
    }

    fn system_clipboard(&mut self) -> Option<String> {
        self.vcx
            .update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
    }
}

#[gpui::test]
fn export_html_asks_where_and_writes_the_page(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let target = f.dir.path().join("out");
    f.dialog.answers.borrow_mut().push_back(Ok(Some(target)));
    f.dispatch(file_commands::ExportHtml);
    let asked = f.dialog.asked.borrow().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].suggested_name, "note.html");
    assert_eq!(
        std::fs::canonicalize(&asked[0].directory).ok(),
        std::fs::canonicalize(f.dir.path()).ok()
    );
    // The page is written off the UI thread; `.html` is added.
    let out = f.dir.path().join("out.html");
    let html = std::fs::read_to_string(&out).expect("the export is written");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<title>Title</title>"));
    assert!(html.contains("<h1 id=\"user-content-title\">Title</h1>"));
    assert!(html.contains("<input type=\"checkbox\" disabled checked>"));
    assert!(html.contains("<div class=\"gv-math-block\"><svg"));
    // Links to other notes point to their pages.
    assert!(html.contains("href=\"other.html\""), "{html}");
    // Light theme colours, whatever the window shows.
    let light = crate::theme::ThemeId::Light.theme().text.primary;
    assert!(html.contains(&format!(
        "--text-primary: #{:02x}{:02x}{:02x};",
        light.r, light.g, light.b
    )));
    assert_eq!(f.notice(), None);
}

#[gpui::test]
fn export_html_takes_unsaved_edits(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dispatch(ToggleEditMode);
    let editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor.clone())
        .expect("the tab has a Source view");
    f.vcx.update(|_, cx| {
        editor.update(cx, |e, cx| e.set_text("# Changed\n\nNot saved yet.\n", cx));
    });
    draw(&mut f.vcx);
    let target = f.dir.path().join("out.html");
    f.dialog
        .answers
        .borrow_mut()
        .push_back(Ok(Some(target.clone())));
    f.dispatch(file_commands::ExportHtml);
    let html = std::fs::read_to_string(&target).expect("written");
    assert!(html.contains("<p>Not saved yet.</p>"), "{html}");
}

#[gpui::test]
fn cancelling_or_a_failing_dialog_writes_nothing(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dialog.answers.borrow_mut().push_back(Ok(None));
    f.dispatch(file_commands::ExportHtml);
    assert_eq!(f.notice(), None);
    f.dialog
        .answers
        .borrow_mut()
        .push_back(Err("no portal".to_owned()));
    f.dispatch(file_commands::ExportHtml);
    assert!(f.notice().is_some(), "the failure is shown");
    let files: Vec<_> = std::fs::read_dir(f.dir.path())
        .expect("list")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert!(!files.iter().any(|n| n.to_string_lossy().ends_with(".html")));
}

#[gpui::test]
fn a_write_error_is_shown(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    // A folder where the file should go.
    let target = f.dir.path().join("taken.html");
    std::fs::create_dir(&target).expect("folder");
    f.dialog.answers.borrow_mut().push_back(Ok(Some(target)));
    f.dispatch(file_commands::ExportHtml);
    assert_eq!(f.notice().as_deref(), Some("Not exported."));
}

#[gpui::test]
fn open_in_browser_exports_to_a_temporary_file(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dispatch(file_commands::OpenInBrowser);
    let launched = f.launched.0.borrow().clone();
    assert_eq!(launched.len(), 1, "the page goes to the default app");
    let page = &launched[0];
    assert_eq!(page.file_name().and_then(|n| n.to_str()), Some("note.html"));
    assert!(page.starts_with(std::env::temp_dir()));
    let html = std::fs::read_to_string(page).expect("written");
    assert!(html.contains("<h1 id=\"user-content-title\">Title</h1>"));
    assert!(f.dialog.asked.borrow().is_empty(), "no dialog");
    std::fs::remove_dir_all(page.parent().expect("folder")).expect("clean up");
}

#[gpui::test]
fn copy_as_rich_text_from_the_reading_view(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.focus_reader();
    // Nothing selected: nothing copied.
    f.dispatch(CopyAsRichText);
    assert!(f.clipboard.written.borrow().is_empty());
    f.dispatch(SelectAll);
    f.dispatch(CopyAsRichText);
    let written = f.clipboard.written.borrow().clone();
    assert_eq!(written.len(), 1);
    let (html, text) = &written[0];
    assert!(
        html.starts_with("<h1 id=\"user-content-title\">Title</h1>"),
        "{html}"
    );
    assert!(html.contains("<strong>bold</strong>"));
    assert!(html.contains("<span class=\"gv-math\">") || html.contains("gv-math-block"));
    assert!(!html.contains("<html"), "a fragment");
    assert!(text.starts_with("Title\n"), "{text}");
    assert_eq!(f.notice(), None);
}

#[gpui::test]
fn copy_as_rich_text_from_the_editor(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dispatch(ToggleEditMode);
    assert_eq!(
        f.workspace.read_with(&f.vcx, |ws, _| ws.mode()),
        ViewMode::Edit
    );
    let editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor.clone())
        .expect("editor");
    f.vcx
        .update(|window, cx| editor.read(cx).focus_handle(cx).focus(window, cx));
    f.dispatch(SelectAll);
    f.dispatch(CopyAsRichText);
    let written = f.clipboard.written.borrow().clone();
    assert_eq!(written.len(), 1, "the editor's selection is copied");
    assert!(written[0].0.contains("<strong>bold</strong>"));
}

#[gpui::test]
fn rich_copy_falls_back_to_plain_text(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    *f.clipboard.fail.borrow_mut() = Some("no data-control protocol".to_owned());
    f.focus_reader();
    f.dispatch(SelectAll);
    f.dispatch(CopyAsRichText);
    assert!(f.clipboard.written.borrow().is_empty());
    let plain = f.system_clipboard().expect("plain text copied");
    assert!(plain.starts_with("Title\n"), "{plain}");
    assert_eq!(f.notice().as_deref(), Some("Copied as plain text."));
}

#[gpui::test]
fn without_a_rich_clipboard_the_copy_is_plain_text(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx
        .update(|_, cx| crate::export::set_rich_clipboard(cx, None));
    f.focus_reader();
    f.dispatch(SelectAll);
    f.dispatch(CopyAsRichText);
    assert!(f.system_clipboard().is_some_and(|t| t.starts_with("Title")));
    assert_eq!(f.notice().as_deref(), Some("Copied as plain text."));
}

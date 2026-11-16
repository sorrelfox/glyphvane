// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The File commands in headless GPUI windows over temporary folders: the
//! open dialogs and the launcher are fakes that record what they were
//! asked, so no dialog shows and nothing is launched.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{
    App, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_model::LinkKind;
use gv_vault::files::journal::JournalStore;

use super::panels::SidebarPanel;
use super::session::{self, Session, SessionSink, SessionStore};
use super::vault::{AutoIndexLimit, VaultStatus};
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::editor::EditorStatus;
use crate::files::notice::{NoticeAction, Topic};
use crate::files::open_dialog::{OpenAnswer, OpenPathDialog, OpenPrompt};
use crate::keymap::actions::{file as file_commands, workspace as workspace_commands};
use crate::platform::Launcher;
use crate::reader::{Disposition, LinkRequest, ReaderEvent};
use crate::{AppSettings, ThemeChoice};

const NOTE: &str = "# Title\n\nFirst paragraph.\n";

// ----- fakes -----------------------------------------------------------------------

/// An open dialog that answers from a list and remembers what it was
/// asked.
#[derive(Clone, Default)]
struct FakeDialog {
    answers: Rc<RefCell<VecDeque<OpenAnswer>>>,
    asked: Rc<RefCell<Vec<OpenPrompt>>>,
}

impl OpenPathDialog for FakeDialog {
    fn ask(&self, prompt: OpenPrompt, _: &mut App) -> LocalBoxFuture<'static, OpenAnswer> {
        self.asked.borrow_mut().push(prompt);
        let answer = self
            .answers
            .borrow_mut()
            .pop_front()
            .expect("the test gave an answer");
        async move { answer }.boxed_local()
    }
}

/// What the fake launcher was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launched {
    Reveal(PathBuf),
    Open(PathBuf),
    Url(String),
}

#[derive(Clone, Default)]
struct FakeLauncher(Rc<RefCell<Vec<Launched>>>);

impl Launcher for FakeLauncher {
    fn reveal(&self, path: &Path, _: &mut App) {
        self.0
            .borrow_mut()
            .push(Launched::Reveal(path.to_path_buf()));
    }

    fn open_with_default_app(&self, path: &Path, _: &mut App) {
        self.0.borrow_mut().push(Launched::Open(path.to_path_buf()));
    }

    fn open_url(&self, url: &str, _: &mut App) {
        self.0.borrow_mut().push(Launched::Url(url.to_owned()));
    }
}

/// A session sink that writes `session.json` in a folder, as the
/// application's does in its data folder.
struct FileSink(PathBuf);

impl SessionSink for FileSink {
    fn save(&self, session: &Session) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(session).map_err(std::io::Error::other)?;
        std::fs::write(&self.0, text)
    }
}

fn load_session(path: &Path) -> Session {
    serde_json::from_str(&std::fs::read_to_string(path).expect("session file")).expect("json")
}

// ----- fixture -----------------------------------------------------------------------

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
    dialog: FakeDialog,
    launched: FakeLauncher,
}

fn options() -> WindowOptions {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

/// A temporary folder with `files` (`name/` makes a folder), the app with
/// the fakes, a journal and a session written to the folder's
/// `config/session.json`, and a window showing `open`.
fn fixture(cx: &mut TestAppContext, files: &[(&str, &str)], open: Option<&str>) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        let path = dir.path().join(name);
        if let Some(folder) = name.strip_suffix('/') {
            std::fs::create_dir_all(dir.path().join(folder)).expect("folder");
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folder");
        }
        std::fs::write(path, text).expect("write");
    }
    std::fs::create_dir(dir.path().join("config")).expect("config");
    let target = open.map(|name| OpenTarget::new(dir.path().join(name)));
    let journal = dir.path().join("config/journal");
    let session_file = dir.path().join("config/session.json");
    let dialog = FakeDialog::default();
    let launched = FakeLauncher::default();
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let store = JournalStore::open(journal).expect("journal store");
        crate::files::install_store(cx, Some(store), false);
        crate::files::open_dialog::set_dialog(cx, dialog.clone());
        crate::platform::set_launcher(cx, launched.clone());
        let sink: Arc<dyn SessionSink> = Arc::new(FileSink(session_file));
        session::install(cx, SessionStore::new(Session::default(), Some(sink)));
        gpui::open_window(options(), cx, |window, cx| {
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
    }
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn draw(&mut self) {
        draw(&mut self.vcx);
    }

    fn answer(&self, answer: OpenAnswer) {
        self.dialog.answers.borrow_mut().push_back(answer);
    }

    fn dispatch(&mut self, action: impl gpui::Action) {
        self.vcx.dispatch_action(action);
        self.draw();
    }

    fn tab_paths(&self) -> Vec<Option<PathBuf>> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.tabs()
                .iter()
                .map(|t| t.path().map(Path::to_path_buf))
                .collect()
        })
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn topic(&self) -> Option<Topic> {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            ws.active_notice(cx).map(|(n, _)| n.topic)
        })
    }

    fn buttons(&self) -> Vec<NoticeAction> {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            ws.active_notice(cx)
                .map(|(n, _)| n.buttons.iter().map(|b| b.action.clone()).collect())
                .unwrap_or_default()
        })
    }

    fn title(&self) -> String {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            ws.active_notice(cx)
                .map(|(n, _)| n.title.to_string())
                .unwrap_or_default()
        })
    }

    /// Clicks the infobar button that runs `action`.
    fn press(&mut self, action: &NoticeAction) {
        let index = self
            .buttons()
            .iter()
            .position(|a| a == action)
            .unwrap_or_else(|| panic!("the infobar offers {action:?}"));
        let selector: &'static str = Box::leak(format!("infobar-action-{index}").into_boxed_str());
        self.click(selector);
    }

    fn click(&mut self, selector: &'static str) {
        self.draw();
        let bounds = self
            .vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        self.vcx.simulate_click(bounds.center(), Modifiers::none());
        self.draw();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.draw();
        self.vcx.debug_bounds(selector).is_some()
    }

    fn launched(&self) -> Vec<Launched> {
        self.launched.0.borrow().clone()
    }

    fn vault(&self) -> Option<(PathBuf, VaultStatus)> {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            let vault = ws.vault.as_ref()?.read(cx);
            Some((vault.root().to_path_buf(), vault.status().clone()))
        })
    }

    fn windows(&mut self) -> usize {
        self.vcx.update(|_, cx| cx.windows().len())
    }

    fn recent(&mut self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        self.vcx
            .update(|_, cx| (session::recent_files(cx), session::recent_folders(cx)))
    }

    /// Waits for the background work (indexing, file creation) to land.
    fn settle(&mut self) {
        for _ in 0..50 {
            self.vcx
                .executor()
                .advance_clock(std::time::Duration::from_millis(50));
            self.draw();
        }
    }

    /// Follows a link from the active Reading view as a click would.
    fn follow(&mut self, destination: &str) {
        let reader = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().reader().cloned())
            .expect("a Reading view");
        let request = LinkRequest {
            destination: Arc::from(destination),
            kind: LinkKind::Inline,
            disposition: Disposition::InPlace,
            range: None,
        };
        reader.update(&mut self.vcx, |_, cx| {
            cx.emit(ReaderEvent::FollowLink(request))
        });
        self.draw();
    }

    fn editor_text(&mut self) -> String {
        let editor = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
            .expect("a Source view");
        self.draw();
        editor.read_with(&self.vcx, |e, _| {
            assert_eq!(e.status(), &EditorStatus::Ready);
            e.editor().expect("loaded").text().to_string()
        })
    }

    fn modified(&self) -> bool {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            ws.active_tab()
                .editor()
                .is_some_and(|e| e.read(cx).is_modified())
        })
    }
}

// ----- Open file --------------------------------------------------------------------

#[gpui::test]
fn open_file_opens_the_chosen_notes_as_tabs(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE), ("b.md", NOTE)], None);
    let (a, b) = (f.path("a.md"), f.path("b.md"));
    f.answer(Ok(Some(vec![a.clone(), b.clone()])));
    f.dispatch(file_commands::Open);
    assert_eq!(*f.dialog.asked.borrow(), [OpenPrompt::Files]);
    assert_eq!(f.tab_paths(), [Some(a.clone()), Some(b.clone())]);
    assert_eq!(f.active_path(), Some(b.clone()));
    let mut recent = f.recent().0;
    recent.sort();
    assert_eq!(recent, [a.clone(), b.clone()], "both are recent files");

    // A file already open is shown, not opened twice.
    f.answer(Ok(Some(vec![a.clone()])));
    f.dispatch(file_commands::Open);
    assert_eq!(f.tab_paths().len(), 2);
    assert_eq!(f.active_path(), Some(a));
}

#[gpui::test]
fn cancelling_open_file_changes_nothing(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE)], Some("a.md"));
    f.answer(Ok(None));
    f.dispatch(file_commands::Open);
    assert_eq!(f.tab_paths(), [Some(f.path("a.md"))]);
    assert_eq!(f.topic(), None);

    // A dialog that cannot be shown says so.
    f.answer(Err("no portal".to_owned()));
    f.dispatch(file_commands::Open);
    assert_eq!(f.topic(), Some(Topic::NotOpened));
    assert_eq!(f.tab_paths().len(), 1);
}

#[gpui::test]
fn other_files_open_with_the_default_app_after_confirmation(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE), ("report.pdf", "%PDF")], Some("a.md"));
    let pdf = f.path("report.pdf");
    f.answer(Ok(Some(vec![pdf.clone()])));
    f.dispatch(file_commands::Open);
    assert_eq!(f.topic(), Some(Topic::Confirm));
    assert!(f.launched().is_empty(), "nothing opens before the answer");
    assert_eq!(f.tab_paths().len(), 1, "no tab for it");

    // Cancel closes the question.
    f.press(&NoticeAction::Cancel);
    assert_eq!(f.topic(), None);
    assert!(f.launched().is_empty());

    // Open hands it to the system.
    f.answer(Ok(Some(vec![pdf.clone()])));
    f.dispatch(file_commands::Open);
    f.press(&NoticeAction::OpenExternally(pdf.clone()));
    assert_eq!(f.launched(), [Launched::Open(pdf)]);
    assert_eq!(f.topic(), None);
}

#[cfg(unix)]
#[gpui::test]
fn programs_are_never_launched(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE), ("setup.exe", "MZ")], Some("a.md"));
    let exe = f.path("setup.exe");
    f.answer(Ok(Some(vec![exe.clone()])));
    f.dispatch(file_commands::Open);
    assert_eq!(f.topic(), Some(Topic::NotOpened));
    assert_eq!(f.buttons(), [NoticeAction::Reveal(exe.clone())]);
    f.press(&NoticeAction::Reveal(exe.clone()));
    assert_eq!(f.launched(), [Launched::Reveal(exe)]);
}

// ----- Open folder ------------------------------------------------------------------

#[gpui::test]
fn open_folder_makes_a_blank_window_its_vault(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("vault/a.md", NOTE), ("vault/b.md", NOTE)], None);
    let folder = f.path("vault");
    f.answer(Ok(Some(vec![folder.clone()])));
    f.dispatch(file_commands::OpenFolder);
    f.settle();
    assert_eq!(*f.dialog.asked.borrow(), [OpenPrompt::Folder]);
    assert_eq!(f.windows(), 1, "the blank window took it");
    assert_eq!(f.vault(), Some((folder.clone(), VaultStatus::Ready)));
    let (sidebar, _) = f.workspace.read_with(&f.vcx, |ws, _| ws.panels_open());
    assert!(sidebar, "the Files panel shows the folder");
    assert_eq!(
        f.workspace.read_with(&f.vcx, |ws, _| ws.sidebar_panel()),
        SidebarPanel::Files
    );
    assert_eq!(f.recent().1, [folder]);
}

#[gpui::test]
fn open_folder_from_a_window_with_a_file_opens_a_new_window(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE), ("vault/b.md", NOTE)], Some("a.md"));
    let folder = f.path("vault");
    f.answer(Ok(Some(vec![folder.clone()])));
    f.dispatch(file_commands::OpenFolder);
    f.settle();
    assert_eq!(f.windows(), 2);
    assert_eq!(f.vault(), None, "this window keeps its file and no vault");
    let other = f
        .vcx
        .update(|_, cx| crate::most_recent_window(cx))
        .expect("the new window");
    let root = f.vcx.update(|_, cx| {
        let ws = other.workspace.upgrade().expect("open").read(cx);
        ws.vault.as_ref().map(|v| v.read(cx).root().to_path_buf())
    });
    assert_eq!(root, Some(folder));

    // Cancelling opens nothing.
    f.answer(Ok(None));
    f.dispatch(file_commands::OpenFolder);
    f.settle();
    assert_eq!(f.windows(), 2);
}

// ----- New note, New window --------------------------------------------------------

#[gpui::test]
fn new_note_creates_untitled_notes_in_the_vault(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("vault/a.md", NOTE)], None);
    let folder = f.path("vault");
    f.answer(Ok(Some(vec![folder.clone()])));
    f.dispatch(file_commands::OpenFolder);
    f.settle();

    f.dispatch(file_commands::NewNote);
    f.settle();
    let first = folder.join("Untitled.md");
    assert!(first.is_file());
    assert_eq!(f.active_path(), Some(first));
    let mode = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().mode());
    assert_eq!(mode, ViewMode::Edit);

    f.dispatch(file_commands::NewNote);
    f.settle();
    assert_eq!(f.active_path(), Some(folder.join("Untitled 1.md")));
    assert_eq!(
        std::fs::read_to_string(folder.join("Untitled.md")).expect("kept"),
        ""
    );
}

#[gpui::test]
fn new_note_without_a_vault_is_untitled(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE)], Some("a.md"));
    f.dispatch(file_commands::NewNote);
    f.settle();
    let untitled = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().untitled_number());
    assert_eq!(untitled, Some(1));
    assert_eq!(f.tab_paths(), [Some(f.path("a.md")), None]);
    let entries = std::fs::read_dir(f.dir.path()).expect("dir").count();
    assert_eq!(entries, 2, "a.md and config/ only: no file was created");
}

#[test]
fn new_note_names_never_collide_in_any_case() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("untitled.MD"), "x").expect("write");
    std::fs::write(dir.path().join("Untitled 1.md"), "x").expect("write");
    let path = super::file_ops::create_note(dir.path(), "Untitled").expect("created");
    assert_eq!(path, dir.path().join("Untitled 2.md"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("untitled.MD")).expect("kept"),
        "x"
    );
}

#[gpui::test]
fn new_window_opens_an_empty_window(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE)], Some("a.md"));
    f.dispatch(workspace_commands::NewWindow);
    f.settle();
    assert_eq!(f.windows(), 2);
    let other = f
        .vcx
        .update(|_, cx| crate::most_recent_window(cx))
        .expect("the new window");
    let blank = f.vcx.update(|_, cx| {
        let ws = other.workspace.upgrade().expect("open").read(cx);
        ws.tabs().len() == 1 && ws.active_tab().path().is_none()
    });
    assert!(blank);
}

// ----- Revert ------------------------------------------------------------------------

#[gpui::test]
fn revert_asks_first_when_there_are_unsaved_edits(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE)], Some("a.md"));
    f.vcx.simulate_keystrokes("secondary-e");
    f.draw();
    assert_eq!(f.editor_text(), NOTE);
    // Autosave is on by default, but waits a second: the edit is unsaved.
    f.vcx.simulate_keystrokes("secondary-end");
    f.vcx.simulate_input(" Added.");
    f.draw();
    assert!(f.modified());

    f.dispatch(file_commands::Revert);
    assert_eq!(f.topic(), Some(Topic::Confirm));
    f.press(&NoticeAction::Cancel);
    assert_eq!(f.topic(), None);
    assert!(f.modified(), "cancelled: the edit stays");
    assert!(f.editor_text().ends_with(" Added."));

    f.dispatch(file_commands::Revert);
    f.press(&NoticeAction::Revert);
    f.draw();
    assert_eq!(f.editor_text(), NOTE);
    assert!(!f.modified());
    assert_eq!(std::fs::read_to_string(f.path("a.md")).expect("read"), NOTE);

    // Undo brings the edit back.
    f.vcx.simulate_keystrokes("secondary-z");
    f.draw();
    assert!(f.editor_text().ends_with(" Added."));
}

#[gpui::test]
fn revert_of_a_clean_document_reads_the_file_again(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE)], Some("a.md"));
    f.vcx.simulate_keystrokes("secondary-e");
    f.draw();
    assert_eq!(f.editor_text(), NOTE);
    // Changed behind the window's back (no watchers in these tests).
    std::fs::write(f.path("a.md"), "# Other\n").expect("write");
    f.dispatch(file_commands::Revert);
    assert_eq!(f.topic(), None, "nothing to lose: no question");
    f.draw();
    assert_eq!(f.editor_text(), "# Other\n");
    assert!(!f.modified());
}

// ----- Recent ---------------------------------------------------------------------------

#[gpui::test]
fn the_recent_panel_lists_opens_and_clears(cx: &mut TestAppContext) {
    let mut f = fixture(
        cx,
        &[("a.md", NOTE), ("b.md", NOTE), ("vault/c.md", NOTE)],
        None,
    );
    let (a, b) = (f.path("a.md"), f.path("b.md"));
    f.vcx.update(|_, cx| {
        session::note_recent_file(cx, &a);
        session::note_recent_file(cx, &b);
        session::note_recent_folder(cx, &f.dir.path().join("vault"));
    });
    f.dispatch(file_commands::OpenRecent);
    assert_eq!(
        f.workspace.read_with(&f.vcx, |ws, _| ws.sidebar_panel()),
        SidebarPanel::Recent
    );
    assert!(f.drawn("sidebar-panel:recent"));
    // The folder first, then the files, most recent first.
    assert!(f.drawn("recent-row-0") && f.drawn("recent-row-2"));
    assert!(!f.drawn("recent-row-3"));
    let focused = f.vcx.update(|window, cx| {
        f.workspace
            .read(cx)
            .recent
            .as_ref()
            .is_some_and(|r| r.read(cx).focus_handle(cx).is_focused(window))
    });
    assert!(focused, "the panel has the keyboard");

    // Down, Down, Enter: the second file, a.md.
    f.vcx.simulate_keystrokes("down down down enter");
    f.draw();
    assert_eq!(f.active_path(), Some(a.clone()));

    // Opening a.md made it the most recent file: b.md is now the third
    // row, and a click opens it.
    f.click("recent-row-2");
    assert_eq!(f.active_path(), Some(b.clone()));

    // Clear empties both lists.
    f.click("recent-clear");
    assert_eq!(f.recent(), (Vec::new(), Vec::new()));
    assert!(f.drawn("files-message"), "the empty state");
}

#[gpui::test]
fn recent_files_and_folders_survive_a_restart(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE), ("vault/b.md", NOTE)], Some("a.md"));
    let file = f.path("config/session.json");
    let folder = f.path("vault");
    f.vcx
        .update(|_, cx| session::note_recent_folder(cx, &folder));
    f.vcx.update(|_, cx| session::save_now(cx));
    let saved = load_session(&file);
    assert_eq!(saved.recent_files, [f.path("a.md")]);
    assert_eq!(saved.recent_folders, std::slice::from_ref(&folder));

    // The next run loads them back and shows them.
    let (files, folders) = f.vcx.update(|_, cx| {
        cx.set_global(SessionStore::new(load_session(&file), None));
        (session::recent_files(cx), session::recent_folders(cx))
    });
    assert_eq!(files, [f.path("a.md")]);
    assert_eq!(folders, [folder]);
    let items = f
        .vcx
        .update(|_, cx| super::recent_panel::recent_items(cx).len());
    assert_eq!(items, 2);

    // Clearing is saved too.
    let sink: Arc<dyn SessionSink> = Arc::new(FileSink(file.clone()));
    f.vcx.update(|_, cx| {
        cx.set_global(SessionStore::new(load_session(&file), Some(sink)));
        session::clear_recent(cx);
        session::save_now(cx);
    });
    let saved = load_session(&file);
    assert!(saved.recent_files.is_empty() && saved.recent_folders.is_empty());
}

// ----- Reveal, Open with default app -------------------------------------------------

#[gpui::test]
fn reveal_and_open_with_go_to_the_launcher(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE)], Some("a.md"));
    let a = f.path("a.md");
    f.dispatch(file_commands::RevealInFileManager);
    f.dispatch(file_commands::OpenWithDefaultApp);
    assert_eq!(
        f.launched(),
        [Launched::Reveal(a.clone()), Launched::Open(a)]
    );

    // An untitled document has no file to hand over.
    f.dispatch(crate::keymap::actions::tabs::New);
    f.dispatch(file_commands::RevealInFileManager);
    assert_eq!(f.launched().len(), 2);
}

#[gpui::test]
fn in_the_file_tree_the_commands_act_on_the_selected_row(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("vault/a.md", NOTE), ("vault/tool.exe", "MZ")], None);
    let folder = f.path("vault");
    f.answer(Ok(Some(vec![folder.clone()])));
    f.dispatch(file_commands::OpenFolder);
    f.settle();
    // The Files panel has the keyboard: Down selects a.md, then tool.exe.
    f.vcx.simulate_keystrokes("down");
    f.draw();
    f.dispatch(file_commands::RevealInFileManager);
    assert_eq!(f.launched(), [Launched::Reveal(folder.join("a.md"))]);
    f.vcx.simulate_keystrokes("down");
    f.draw();
    f.dispatch(file_commands::OpenWithDefaultApp);
    assert_eq!(f.launched().len(), 1, "a program is never opened");
    assert_eq!(f.topic(), Some(Topic::NotOpened));
}

// ----- links ----------------------------------------------------------------------------

#[gpui::test]
fn links_that_leave_glyphvane_ask_first(cx: &mut TestAppContext) {
    let mut f = fixture(
        cx,
        &[("a.md", NOTE), ("doc.pdf", "%PDF"), ("page.html", "<p>")],
        Some("a.md"),
    );
    // Web links go straight to the browser.
    f.follow("https://example.com/");
    assert_eq!(f.launched(), [Launched::Url("https://example.com/".into())]);

    // Other schemes show the full URL first.
    f.follow("ssh://host.example");
    assert_eq!(f.topic(), Some(Topic::Confirm));
    assert_eq!(f.launched().len(), 1);
    f.press(&NoticeAction::Cancel);
    assert_eq!(f.launched().len(), 1, "cancelled");
    f.follow("ssh://host.example");
    f.press(&NoticeAction::OpenUrl("ssh://host.example".into()));
    assert_eq!(f.launched()[1], Launched::Url("ssh://host.example".into()));

    // A file Glyphvane does not show.
    f.follow("doc.pdf");
    assert_eq!(f.topic(), Some(Topic::Confirm));
    f.press(&NoticeAction::OpenExternally(f.path("doc.pdf")));
    assert_eq!(f.launched()[2], Launched::Open(f.path("doc.pdf")));

    // A web page names the browser.
    f.follow("page.html");
    assert!(f.title().contains("browser"), "{}", f.title());
    f.press(&NoticeAction::Reveal(f.path("page.html")));
    assert_eq!(f.launched()[3], Launched::Reveal(f.path("page.html")));
    assert_eq!(f.tab_paths(), [Some(f.path("a.md"))], "no tab was opened");
}

// ----- ask before indexing -----------------------------------------------------------

fn many_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("big/.git/", ""),
        ("big/1.md", NOTE),
        ("big/2.md", NOTE),
        ("big/3.md", NOTE),
        ("big/4.md", NOTE),
        ("big/sub/note.md", NOTE),
    ]
}

#[gpui::test]
fn a_large_folder_is_indexed_only_when_the_user_says_so(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &many_files(), None);
    f.vcx.update(|_, cx| cx.set_global(AutoIndexLimit(3)));
    let big = f.path("big");
    f.answer(Ok(Some(vec![big.clone()])));
    f.dispatch(file_commands::OpenFolder);
    f.settle();
    assert!(matches!(f.vault(), Some((_, VaultStatus::NotIndexed(_)))));
    assert_eq!(f.topic(), Some(Topic::IndexAsk));
    assert!(f.title().contains("more than 3 files"), "{}", f.title());
    assert_eq!(
        f.buttons(),
        [NoticeAction::IndexAnyway, NoticeAction::Cancel]
    );

    f.press(&NoticeAction::IndexAnyway);
    f.settle();
    assert_eq!(f.topic(), None);
    assert_eq!(f.vault(), Some((big, VaultStatus::Ready)));
}

#[gpui::test]
fn cancelling_the_index_question_keeps_the_folder_unindexed(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &many_files(), None);
    f.vcx.update(|_, cx| cx.set_global(AutoIndexLimit(3)));
    f.answer(Ok(Some(vec![f.path("big")])));
    f.dispatch(file_commands::OpenFolder);
    f.settle();
    f.press(&NoticeAction::Cancel);
    f.settle();
    assert_eq!(f.topic(), None);
    assert!(matches!(f.vault(), Some((_, VaultStatus::NotIndexed(_)))));
    // Not asked again in this window.
    f.dispatch(crate::keymap::actions::workspace::ToggleSidebar);
    f.dispatch(crate::keymap::actions::workspace::ToggleSidebar);
    assert_eq!(f.topic(), None);
}

#[gpui::test]
fn only_this_folder_indexes_the_notes_folder(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &many_files(), Some("big/sub/note.md"));
    f.vcx.update(|_, cx| cx.set_global(AutoIndexLimit(3)));
    // The sidebar makes the vault: the repository root, too large.
    f.dispatch(crate::keymap::actions::workspace::ToggleSidebar);
    f.settle();
    let sub = f.path("big/sub");
    assert_eq!(
        f.buttons(),
        [
            NoticeAction::IndexAnyway,
            NoticeAction::IndexFolder(sub.clone()),
            NoticeAction::Cancel
        ]
    );
    f.press(&NoticeAction::IndexFolder(sub.clone()));
    f.settle();
    assert_eq!(f.vault(), Some((sub, VaultStatus::Ready)));
    assert_eq!(f.topic(), None);
}

#[gpui::test]
fn a_filesystem_root_is_never_indexed_unasked(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[], None);
    let root = PathBuf::from(std::path::MAIN_SEPARATOR_STR);
    f.answer(Ok(Some(vec![root.clone()])));
    f.dispatch(file_commands::OpenFolder);
    f.settle();
    assert_eq!(f.topic(), Some(Topic::IndexAsk));
    assert!(f.title().contains("filesystem root"), "{}", f.title());
    assert!(matches!(f.vault(), Some((r, VaultStatus::NotIndexed(_))) if r == root));
    // Never pressed Index here: that would walk the whole disk.
    f.press(&NoticeAction::Cancel);
    assert!(matches!(f.vault(), Some((_, VaultStatus::NotIndexed(_)))));
}

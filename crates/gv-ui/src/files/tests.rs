// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Data safety in headless GPUI windows over temporary files: journaling,
//! Mod+S, the unsaved dot, autosave on the executor's clock, closing with
//! unsaved changes, untitled documents, crash recovery, changes on disk
//! (clean reload, conflicts, deleted files) and save errors. Every file
//! and journal lives in a temporary folder.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_text::{Buffer, Encoding, Transaction};
use gv_vault::files::journal::{DocumentKey, JournalStore};
use gv_vault::files::{CheckMode, DocumentFile, ExternalChange, check_external_change};

use super::notice::{NoticeAction, Topic};
use super::save_as::{SaveAnswer, SavePathDialog, SavePrompt};
use crate::chrome::WindowChrome;
use crate::editor::{EditorStatus, EditorView};
use crate::keymap::actions::file as file_commands;
use crate::keymap::actions::tabs as tab_commands;
use crate::shell::{OpenTarget, ViewMode, Workspace};
use crate::{AppSettings, ThemeChoice};

const NOTE: &str = "# Title\n\nFirst paragraph.\n";

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn options() -> WindowOptions {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

/// A temporary folder with `files`, the app with a journal store in the
/// folder's `journal/` (watchers off unless `watch`), and a window showing
/// `open` (in the folder).
fn fixture(
    cx: &mut TestAppContext,
    files: &[(&str, &[u8])],
    open: Option<&str>,
    watch: bool,
) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, bytes) in files {
        std::fs::write(dir.path().join(name), bytes).expect("write");
    }
    let target = open.map(|name| OpenTarget::new(dir.path().join(name)));
    let journal = dir.path().join("journal");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let store = JournalStore::open(journal).expect("journal store");
        super::install_store(cx, Some(store), watch);
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

    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.path(name)).expect("read")
    }

    fn draw(&mut self) {
        draw(&mut self.vcx);
    }

    /// The journal files in the store (temporary files left out).
    fn journals(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.path().join("journal"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !n.starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Switches the active tab to Edit and returns its loaded Source view.
    fn edit(&mut self) -> Entity<EditorView> {
        self.vcx.simulate_keystrokes("secondary-e");
        self.draw();
        self.editor()
    }

    fn editor(&mut self) -> Entity<EditorView> {
        let editor = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
            .expect("the tab has a Source view");
        self.draw();
        assert_eq!(
            editor.read_with(&self.vcx, |e, _| e.status().clone()),
            EditorStatus::Ready
        );
        editor
    }

    fn text(&self, editor: &Entity<EditorView>) -> String {
        editor.read_with(&self.vcx, |e, _| {
            e.editor().expect("loaded").text().to_string()
        })
    }

    fn modified(&self, editor: &Entity<EditorView>) -> bool {
        editor.read_with(&self.vcx, |e, _| e.is_modified())
    }

    fn topic(&self, editor: &Entity<EditorView>) -> Option<Topic> {
        editor.read_with(&self.vcx, |e, _| e.notice().map(|n| n.topic))
    }

    /// Types `text` at the end of the document.
    fn type_at_end(&mut self, text: &str) {
        self.vcx.simulate_keystrokes("secondary-end");
        self.vcx.simulate_input(text);
        self.draw();
    }

    fn save(&mut self) {
        self.vcx.simulate_keystrokes("secondary-s");
        self.draw();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.draw();
        self.vcx.debug_bounds(selector).is_some()
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

    /// What the watcher would report for `name` now, against the Source
    /// view's baseline.
    fn change_of(&self, editor: &Entity<EditorView>, name: &str) -> ExternalChange {
        let (_, baseline) = editor
            .read_with(&self.vcx, |e, _| e.disk_baseline())
            .expect("a baseline");
        check_external_change(&self.path(name), &baseline, CheckMode::Thorough).expect("check")
    }

    fn report(&mut self, name: &str, change: ExternalChange) {
        let path = self.path(name);
        self.workspace
            .update(&mut self.vcx, |ws, cx| ws.file_changed(&path, change, cx));
        self.draw();
    }

    fn act(&mut self, editor: &Entity<EditorView>, action: NoticeAction) {
        editor.update(&mut self.vcx, |e, cx| e.notice_action(&action, cx));
        self.draw();
    }
}

#[gpui::test]
fn edits_are_journaled_and_mod_s_saves_them(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    assert!(f.journals().is_empty(), "nothing is written before an edit");
    assert!(f.drawn("close-tab-0") && !f.drawn("unsaved-dot-0"));

    f.type_at_end("Added.");
    assert!(f.modified(&editor));
    assert!(f.drawn("unsaved-dot-0"), "the dot replaces the close icon");
    assert!(!f.drawn("close-tab-0"));
    assert_eq!(f.journals().len(), 1, "the edit is in the journal");
    assert_eq!(f.read("note.md"), NOTE.as_bytes(), "not saved yet");

    f.save();
    assert_eq!(f.read("note.md"), format!("{NOTE}Added.").as_bytes());
    assert!(f.journals().is_empty(), "a save clears the journal");
    assert!(!f.modified(&editor));
    assert!(f.drawn("close-tab-0") && !f.drawn("unsaved-dot-0"));

    // Undo goes back past the save: modified again, journaled again.
    f.vcx.simulate_keystrokes("secondary-z secondary-z");
    f.draw();
    assert!(f.modified(&editor));
    assert_eq!(f.journals().len(), 1);
}

#[gpui::test]
fn autosave_runs_after_a_second_without_edits(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    f.type_at_end("A");
    f.vcx.executor().advance_clock(Duration::from_millis(600));
    f.type_at_end("B");
    f.vcx.executor().advance_clock(Duration::from_millis(999));
    f.draw();
    assert_eq!(
        f.read("note.md"),
        NOTE.as_bytes(),
        "the idle delay restarts"
    );
    f.vcx.executor().advance_clock(Duration::from_millis(1));
    f.draw();
    assert_eq!(f.read("note.md"), format!("{NOTE}AB").as_bytes());
    assert!(!f.modified(&editor));
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn autosave_off_leaves_the_file_until_mod_s(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.vcx
        .update(|_, cx| super::override_autosave(cx, Some(false)));
    let editor = f.edit();
    f.type_at_end("A");
    f.vcx.executor().advance_clock(Duration::from_secs(5));
    f.draw();
    assert_eq!(f.read("note.md"), NOTE.as_bytes());
    assert!(f.modified(&editor));
    f.save();
    assert_eq!(f.read("note.md"), format!("{NOTE}A").as_bytes());
}

#[gpui::test]
fn focus_loss_autosaves(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.edit();
    f.vcx.update(|window, _| window.activate_window());
    f.draw();
    f.type_at_end("A");
    f.vcx.deactivate_window();
    f.draw();
    assert_eq!(f.read("note.md"), format!("{NOTE}A").as_bytes());
}

#[gpui::test]
fn closing_a_tab_saves_with_autosave_and_journals_without(cx: &mut TestAppContext) {
    // Autosave on: closing saves first.
    let mut f = fixture(
        cx,
        &[("a.md", NOTE.as_bytes()), ("b.md", NOTE.as_bytes())],
        Some("a.md"),
        false,
    );
    f.edit();
    f.type_at_end("A");
    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    assert_eq!(f.read("a.md"), format!("{NOTE}A").as_bytes());
    assert!(f.journals().is_empty());

    // Autosave off: the file is left alone and the journal kept for the
    // next start (no dialog is designed).
    let target = OpenTarget::new(f.path("b.md"));
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.open_tab(target, window, cx));
    f.vcx
        .update(|_, cx| super::override_autosave(cx, Some(false)));
    f.edit();
    f.type_at_end("B");
    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    assert_eq!(f.read("b.md"), NOTE.as_bytes());
    assert_eq!(
        f.journals().len(),
        1,
        "the unsaved text stays in the journal"
    );

    // Opened again in this session, it comes back with the recovery bar.
    let target = OpenTarget::new(f.path("b.md"));
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.open_tab(target, window, cx));
    let editor = f.edit();
    assert_eq!(f.text(&editor), format!("{NOTE}B"));
    assert!(f.modified(&editor));
    assert_eq!(f.topic(&editor), Some(Topic::Recovered));
}

#[gpui::test]
fn untitled_documents_live_in_the_journal(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.vcx.dispatch_action(tab_commands::New);
    f.draw();
    let (tabs, mode, untitled) = f.workspace.read_with(&f.vcx, |ws, _| {
        (
            ws.tabs().len(),
            ws.active_tab().mode(),
            ws.active_tab().untitled_number(),
        )
    });
    assert_eq!((tabs, mode, untitled), (2, ViewMode::Edit, Some(1)));
    let editor = f.editor();
    f.vcx.simulate_input("Draft");
    f.draw();
    assert_eq!(f.journals(), ["untitled-1"]);
    assert!(f.drawn("unsaved-dot-1"));
    // Save has nowhere to write yet: it asks where, and a cancelled
    // dialog leaves everything as it was.
    let dialog = FakeDialog::install(&mut f, [Ok(None)]);
    f.save();
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.topic(&editor), None);
    assert!(f.modified(&editor));
    assert_eq!(f.journals(), ["untitled-1"]);
}

/// Writes a journal for `path` the way a crashed session leaves it: the
/// file as loaded, then one edit appending `added`.
fn crashed_session(store: &JournalStore, path: &Path, added: &str) {
    let (file, decoded) = DocumentFile::load(path).expect("load");
    let mut buffer = Buffer::from_decoded(&decoded);
    let mut journal = store
        .begin_default(
            DocumentKey::Path(path.to_path_buf()),
            buffer.snapshot(),
            file.format(),
            file.baseline().map(|b| b.hash),
        )
        .expect("journal");
    let edit = Transaction::new().insert(buffer.text().len(), added);
    buffer.apply(&edit).expect("edit");
    journal.record(&edit, buffer.text()).expect("record");
    journal.sync().expect("sync");
}

#[gpui::test]
fn crash_recovery_reopens_the_unsaved_text(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let note = dir.path().join("note.md");
    std::fs::write(&note, NOTE).expect("write");
    std::fs::write(dir.path().join("other.md"), NOTE).expect("write");
    {
        let store = JournalStore::open(dir.path().join("journal")).expect("store");
        crashed_session(&store, &note, "Recovered line.\n");
        let mut untitled = store
            .begin_default(
                DocumentKey::Untitled(3),
                Buffer::new("").snapshot(),
                Default::default(),
                None,
            )
            .expect("journal");
        let edit = Transaction::new().insert(0, "Scratch");
        let mut buffer = Buffer::new("");
        buffer.apply(&edit).expect("edit");
        untitled.record(&edit, buffer.text()).expect("record");
    }
    // The next start: a window for another file, then the recovery check.
    let journal = dir.path().join("journal");
    let other = dir.path().join("other.md");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        super::install_store(cx, Some(JournalStore::open(journal).expect("store")), false);
        gpui::open_window(options(), cx, |window, cx| {
            let target = Some(OpenTarget::new(other));
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("window")
    });
    let mut f = Fixture {
        dir,
        workspace,
        vcx: VisualTestContext::from_window(window, cx),
    };
    f.draw();
    f.vcx.update(|_, cx| super::recover_unsaved(cx));
    f.draw();

    let tabs = f.workspace.read_with(&f.vcx, |ws, _| {
        ws.tabs()
            .iter()
            .map(|t| {
                (
                    t.path().map(Path::to_path_buf),
                    t.untitled_number(),
                    t.mode(),
                )
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(tabs.len(), 3, "{tabs:?}");
    assert!(tabs.contains(&(Some(note.clone()), None, ViewMode::Edit)));
    assert!(tabs.contains(&(None, Some(3), ViewMode::Edit)));

    let editor = f.editor();
    assert_eq!(
        editor.read_with(&f.vcx, |e, _| e.path().map(Path::to_path_buf)),
        Some(note.clone())
    );
    assert_eq!(f.text(&editor), format!("{NOTE}Recovered line.\n"));
    assert!(f.modified(&editor));
    assert_eq!(f.topic(&editor), Some(Topic::Recovered));
    assert!(f.drawn("infobar"));
    assert_eq!(f.read("note.md"), NOTE.as_bytes(), "nothing is written yet");
    // Autosave holds back while the question is open.
    f.vcx.executor().advance_clock(Duration::from_secs(3));
    f.draw();
    assert_eq!(f.read("note.md"), NOTE.as_bytes());

    // Discard goes back to the file on disk and drops its journal.
    f.act(&editor, NoticeAction::DiscardRecovered);
    assert_eq!(f.text(&editor), NOTE);
    assert!(!f.modified(&editor));
    assert_eq!(f.topic(&editor), None);
    assert_eq!(f.journals(), ["untitled-3"]);
    assert_eq!(f.read("note.md"), NOTE.as_bytes());
}

#[gpui::test]
fn keeping_recovered_text_lets_autosave_write_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let note = dir.path().join("note.md");
    std::fs::write(&note, NOTE).expect("write");
    crashed_session(
        &JournalStore::open(dir.path().join("journal")).expect("store"),
        &note,
        "Kept.",
    );
    let journal = dir.path().join("journal");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        super::install_store(cx, Some(JournalStore::open(journal).expect("store")), false);
        gpui::open_window(options(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(None, WindowChrome::Linux, window, cx))
        })
        .expect("window")
    });
    let mut f = Fixture {
        dir,
        workspace,
        vcx: VisualTestContext::from_window(window, cx),
    };
    f.vcx.update(|_, cx| super::recover_unsaved(cx));
    f.draw();
    let editor = f.editor();
    assert_eq!(f.topic(&editor), Some(Topic::Recovered));
    f.click("infobar-action-0");
    assert_eq!(f.topic(&editor), None);
    f.vcx.executor().advance_clock(Duration::from_secs(1));
    f.draw();
    assert_eq!(f.read("note.md"), format!("{NOTE}Kept.").as_bytes());
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn a_clean_buffer_reloads_a_change_on_disk(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    // The caret after "First", which the reload keeps in place.
    let caret = NOTE.find("First").expect("found") + "First".len();
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(caret, cx));
    let theirs = "# New title\n\nFirst paragraph.\n\nMore from elsewhere.\n";
    std::fs::write(f.path("note.md"), theirs).expect("write");
    let change = f.change_of(&editor, "note.md");
    f.report("note.md", change);

    assert_eq!(f.text(&editor), theirs);
    assert!(!f.modified(&editor), "the reloaded text is the saved state");
    assert_eq!(f.topic(&editor), None);
    assert!(f.journals().is_empty());
    let moved = caret + "# New title".len() - "# Title".len();
    let head = editor.read_with(&f.vcx, |e, _| {
        e.editor()
            .expect("loaded")
            .selections()
            .iter()
            .next()
            .map(|s| s.head())
    });
    assert_eq!(head, Some(moved), "the caret follows its text");
    // The reload is undoable.
    f.vcx.simulate_keystrokes("secondary-z secondary-z");
    f.draw();
    assert_eq!(f.text(&editor), NOTE);
}

#[gpui::test]
fn a_change_on_disk_with_unsaved_edits_asks_and_keep_mine_wins(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    f.vcx
        .update(|_, cx| super::override_autosave(cx, Some(false)));
    f.type_at_end("Mine.");
    std::fs::write(f.path("note.md"), "Theirs.\n").expect("write");
    let change = f.change_of(&editor, "note.md");
    f.report("note.md", change);

    assert_eq!(f.topic(&editor), Some(Topic::Conflict));
    assert_eq!(f.text(&editor), format!("{NOTE}Mine."), "nothing is lost");
    let notice = editor
        .read_with(&f.vcx, |e, _| e.notice().cloned())
        .expect("notice");
    assert_eq!(
        notice
            .buttons
            .iter()
            .map(|b| b.label.to_string())
            .collect::<Vec<_>>(),
        ["Compare", "Keep mine", "Take theirs"]
    );
    assert!(
        notice.buttons[0].disabled.is_some(),
        "Compare is not designed yet"
    );
    // A save meanwhile does not overwrite the other version.
    f.save();
    assert_eq!(f.read("note.md"), b"Theirs.\n");
    assert_eq!(f.topic(&editor), Some(Topic::Conflict));

    // Clicking the disabled Compare does nothing; Keep mine is the second.
    f.click("infobar-action-0");
    assert_eq!(f.topic(&editor), Some(Topic::Conflict));
    f.click("infobar-action-1");
    assert_eq!(f.topic(&editor), None);
    assert!(f.modified(&editor));
    f.save();
    assert_eq!(f.read("note.md"), format!("{NOTE}Mine.").as_bytes());
}

#[gpui::test]
fn take_theirs_reloads_and_keeps_the_edits_in_undo(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    f.type_at_end("Mine.");
    std::fs::write(f.path("note.md"), "Theirs.\n").expect("write");
    let change = f.change_of(&editor, "note.md");
    f.report("note.md", change);
    assert_eq!(f.topic(&editor), Some(Topic::Conflict));
    // Autosave holds back while the conflict is open.
    f.vcx.executor().advance_clock(Duration::from_secs(3));
    f.draw();
    assert_eq!(f.read("note.md"), b"Theirs.\n");

    f.act(&editor, NoticeAction::TakeTheirs);
    assert_eq!(f.text(&editor), "Theirs.\n");
    assert!(!f.modified(&editor));
    assert_eq!(f.topic(&editor), None);
    assert!(f.journals().is_empty());
    // One undo takes the reload back: the edits are there again, unsaved
    // and journaled.
    f.vcx.simulate_keystrokes("secondary-z");
    f.draw();
    assert_eq!(f.text(&editor), format!("{NOTE}Mine."));
    assert!(f.modified(&editor));
    assert_eq!(f.journals().len(), 1);
}

#[gpui::test]
fn a_deleted_file_offers_save_and_close(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    std::fs::remove_file(f.path("note.md")).expect("delete");
    let change = f.change_of(&editor, "note.md");
    assert_eq!(change, ExternalChange::Deleted);
    f.report("note.md", change);
    assert_eq!(f.topic(&editor), Some(Topic::Deleted));
    let notice = editor
        .read_with(&f.vcx, |e, _| e.notice().cloned())
        .expect("notice");
    assert!(notice.offers(&NoticeAction::Recreate) && notice.offers(&NoticeAction::Close));

    f.act(&editor, NoticeAction::Recreate);
    assert_eq!(f.read("note.md"), NOTE.as_bytes(), "written again");
    assert_eq!(f.topic(&editor), None);

    // A tab showing only the Reading view gets Close alone.
    f.vcx.simulate_keystrokes("secondary-e");
    f.draw();
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.drop_editors_for_tests(cx));
    std::fs::remove_file(f.path("note.md")).expect("delete");
    f.report("note.md", ExternalChange::Deleted);
    let notice = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().notice().cloned())
        .expect("tab notice");
    assert_eq!(notice.topic, Topic::Deleted);
    assert!(!notice.offers(&NoticeAction::Recreate));
    f.click("infobar-action-0");
    let tabs = f.workspace.read_with(&f.vcx, |ws, _| ws.tabs().len());
    let path = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().path().is_some());
    assert_eq!((tabs, path), (1, false), "the tab closed");
}

#[gpui::test]
fn an_unmappable_character_fails_the_save_with_its_position(cx: &mut TestAppContext) {
    // windows-1252: "Größe über" and an e acute.
    let latin: &[u8] = b"# Gr\xf6\xdfe\n\n\xdcber den Flu\xdf, caf\xe9 und Stra\xdfe.\n";
    let mut f = fixture(cx, &[("latin.md", latin)], Some("latin.md"), false);
    let editor = f.edit();
    let encoding = editor.read_with(&f.vcx, |e, _| e.disk_encoding_for_tests());
    assert_eq!(encoding.map(|e| e.name()), Some("windows-1252"));
    f.type_at_end("π");
    f.save();
    assert_eq!(f.read("latin.md"), latin, "the file is untouched");
    assert!(f.modified(&editor));
    let notice = editor
        .read_with(&f.vcx, |e, _| e.notice().cloned())
        .expect("notice");
    assert_eq!(notice.topic, Topic::SaveFailed);
    let detail = notice.detail.as_deref().unwrap_or_default();
    assert!(
        detail.starts_with("Line 4, column 1: “π” (U+03C0)"),
        "{detail}"
    );
    assert!(detail.contains("windows-1252"), "{detail}");
    assert!(f.drawn("infobar"));

    // Save as UTF-8 converts the whole file.
    f.click("infobar-action-0");
    let saved = String::from_utf8(f.read("latin.md")).expect("UTF-8 now");
    assert_eq!(saved, "# Größe\n\nÜber den Fluß, café und Straße.\nπ");
    assert!(!f.modified(&editor));
    assert_eq!(f.topic(&editor), None);
    assert_eq!(
        editor.read_with(&f.vcx, |e, _| e.disk_encoding_for_tests()),
        Some(Encoding::Utf8)
    );
}

/// Waits for something done by the real file watcher's thread.
fn wait_until(f: &mut Fixture, mut done: impl FnMut(&mut Fixture) -> bool) -> bool {
    for _ in 0..500 {
        f.draw();
        if done(f) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[gpui::test]
fn the_file_watcher_reloads_and_ignores_own_saves(cx: &mut TestAppContext) {
    // The watcher's thread wakes the test's tasks.
    cx.executor().allow_parking();
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), true);
    let editor = f.edit();
    f.type_at_end("Mine.");
    f.save();
    assert_eq!(f.read("note.md"), format!("{NOTE}Mine.").as_bytes());
    // Our own save is not reported back as a change.
    std::thread::sleep(Duration::from_millis(400));
    f.draw();
    assert_eq!(f.topic(&editor), None);
    assert!(!f.modified(&editor));

    // Another program writes: the clean buffer follows.
    std::fs::write(f.path("note.md"), "Elsewhere.\n").expect("write");
    let reloaded = wait_until(&mut f, |f| f.text(&editor) == "Elsewhere.\n");
    assert!(reloaded, "the change on disk reached the editor");
    assert!(!f.modified(&editor));

    // A sync client's conflict copy next to it is reported.
    std::fs::write(
        f.path("note (conflicted copy 2026-11-14).md"),
        "Other device.\n",
    )
    .expect("write");
    let reported = wait_until(&mut f, |f| {
        f.workspace.read_with(&f.vcx, |ws, _| {
            ws.active_tab().notice().map(|n| n.topic) == Some(Topic::SyncConflict)
        })
    });
    assert!(reported, "the conflict copy is reported");
}

#[gpui::test]
fn closing_the_window_saves_first(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.edit();
    f.type_at_end("A");
    assert!(!f.vcx.simulate_close(), "the window waits for its save");
    f.vcx.run_until_parked();
    assert_eq!(f.read("note.md"), format!("{NOTE}A").as_bytes());
    assert!(f.journals().is_empty());
    assert!(f.vcx.windows().is_empty(), "then it closes");
}

#[gpui::test]
fn closing_the_window_without_autosave_keeps_the_journal(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.vcx
        .update(|_, cx| super::override_autosave(cx, Some(false)));
    f.edit();
    f.type_at_end("A");
    assert!(f.vcx.simulate_close(), "nothing to wait for");
    f.vcx.run_until_parked();
    assert_eq!(f.read("note.md"), NOTE.as_bytes());
    assert_eq!(f.journals().len(), 1, "recovered on the next start");
}

/// Clicks the close button the application draws on a client-decorated
/// Linux window.
fn click_linux_close(f: &mut Fixture) {
    f.vcx
        .update(|_, cx| cx.set_global(crate::shell::ClientDecorationsForTests));
    f.draw();
    let close = f
        .vcx
        .debug_bounds("window-close")
        .expect("the Linux close button is drawn")
        .center();
    f.vcx.simulate_click(close, Modifiers::none());
}

#[gpui::test]
fn the_linux_close_button_saves_first(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.edit();
    f.type_at_end("A");
    click_linux_close(&mut f);
    f.vcx.run_until_parked();
    assert_eq!(f.read("note.md"), format!("{NOTE}A").as_bytes());
    assert!(f.journals().is_empty());
    assert!(f.vcx.windows().is_empty(), "then it closes");
}

#[gpui::test]
fn the_linux_close_button_without_autosave_keeps_the_journal(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.vcx
        .update(|_, cx| super::override_autosave(cx, Some(false)));
    f.edit();
    f.type_at_end("A");
    click_linux_close(&mut f);
    f.vcx.run_until_parked();
    assert!(f.vcx.windows().is_empty(), "it closes at once");
    assert_eq!(f.read("note.md"), NOTE.as_bytes());
    assert_eq!(f.journals().len(), 1, "recovered on the next start");
}

#[gpui::test]
fn quitting_saves_and_syncs_first(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.edit();
    f.type_at_end("A");
    let waits = f.vcx.update(|_, cx| crate::shell::prepare_quit(cx));
    assert_eq!(waits.len(), 1);
    f.draw();
    assert_eq!(f.read("note.md"), format!("{NOTE}A").as_bytes());
    assert!(f.journals().is_empty());
    for mut wait in waits {
        assert_eq!(wait.try_recv(), Ok(Some(())), "the queue is done");
    }
}

#[gpui::test]
fn a_file_shown_only_in_preview_reloads(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let path = f.path("note.md");
    let (_, before) = gv_vault::files::DiskState::read(&path).expect("state");
    std::fs::write(&path, "# Changed elsewhere\n").expect("write");
    let change = check_external_change(&path, &before, CheckMode::Thorough).expect("check");
    f.report("note.md", change);
    let reader = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("reader");
    let shown = reader
        .read_with(&f.vcx, |r, _| r.plain_text())
        .unwrap_or_default();
    assert!(shown.contains("Changed elsewhere"), "{shown:?}");
}

// ----- Save As ------------------------------------------------------------------

/// A save dialog that answers from a list and remembers what it was asked.
#[derive(Clone, Default)]
struct FakeDialog {
    answers: Rc<RefCell<VecDeque<SaveAnswer>>>,
    prompts: Rc<RefCell<Vec<SavePrompt>>>,
}

impl SavePathDialog for FakeDialog {
    fn ask(
        &self,
        prompt: &SavePrompt,
        _: &mut gpui::App,
    ) -> futures::future::LocalBoxFuture<'static, SaveAnswer> {
        use futures::FutureExt as _;
        self.prompts.borrow_mut().push(prompt.clone());
        let answer = self
            .answers
            .borrow_mut()
            .pop_front()
            .expect("the test gave an answer");
        async move { answer }.boxed_local()
    }
}

impl FakeDialog {
    fn install(f: &mut Fixture, answers: impl IntoIterator<Item = SaveAnswer>) -> Self {
        let dialog = Self::default();
        dialog.answers.borrow_mut().extend(answers);
        let installed = dialog.clone();
        f.vcx
            .update(|_, cx| super::save_as::set_dialog(cx, installed));
        dialog
    }

    fn answer(&self, answer: SaveAnswer) {
        self.answers.borrow_mut().push_back(answer);
    }

    fn asked(&self) -> usize {
        self.prompts.borrow().len()
    }

    fn last_prompt(&self) -> SavePrompt {
        self.prompts.borrow().last().cloned().expect("asked")
    }
}

impl Fixture {
    fn tab_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn save_as(&mut self) {
        self.vcx.simulate_keystrokes("secondary-shift-s");
        self.draw();
    }
}

#[gpui::test]
fn saving_an_untitled_document_asks_where_and_moves_it_there(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.vcx.dispatch_action(tab_commands::New);
    f.draw();
    let editor = f.editor();
    f.vcx.simulate_input("# Shopping list\n\nMilk.");
    f.draw();
    assert_eq!(f.journals(), ["untitled-1"]);

    // The name typed without an extension gets `.md`.
    let answer = f.path("shopping");
    let dialog = FakeDialog::install(&mut f, [Ok(Some(answer))]);
    f.save();
    assert_eq!(
        dialog.last_prompt(),
        SavePrompt {
            directory: f.dir.path().to_path_buf(),
            suggested_name: "Shopping list.md".to_owned(),
        },
        "the folder of the window's file, and the title"
    );
    let text = "# Shopping list\n\nMilk.";
    assert_eq!(f.read("shopping.md"), text.as_bytes());
    assert_eq!(f.tab_path(), Some(f.path("shopping.md")));
    let untitled = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().untitled_number());
    assert_eq!(untitled, None);
    assert!(!f.modified(&editor));
    assert_eq!(f.topic(&editor), None);
    assert!(f.journals().is_empty(), "the untitled journal is gone");
    assert!(!f.drawn("unsaved-dot-1"));

    // From now on it is a file: edits are journaled under its path, and
    // Save writes it without asking.
    f.type_at_end(" Eggs.");
    let journals = f.journals();
    assert_eq!(journals.len(), 1);
    assert!(!journals[0].starts_with("untitled-"), "{journals:?}");
    f.save();
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.read("shopping.md"), format!("{text} Eggs.").as_bytes());
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn save_as_writes_a_copy_and_the_tab_follows_it(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    f.type_at_end("Added.");
    assert_eq!(f.journals().len(), 1);
    std::fs::create_dir(f.path("sub")).expect("folder");
    let answer = f.path("sub/copy.md");
    let dialog = FakeDialog::install(&mut f, [Ok(Some(answer))]);
    f.save_as();
    assert_eq!(
        dialog.last_prompt(),
        SavePrompt {
            directory: f.dir.path().to_path_buf(),
            suggested_name: "note.md".to_owned(),
        },
        "the file's own folder and name"
    );
    assert_eq!(f.read("sub/copy.md"), format!("{NOTE}Added.").as_bytes());
    assert_eq!(
        f.read("note.md"),
        NOTE.as_bytes(),
        "the original is untouched"
    );
    assert_eq!(f.tab_path(), Some(f.path("sub/copy.md")));
    assert_eq!(
        editor.read_with(&f.vcx, |e, _| e.path().map(Path::to_path_buf)),
        Some(f.path("sub/copy.md"))
    );
    assert!(!f.modified(&editor));
    assert!(f.journals().is_empty());

    // Back in Preview, the Reading view shows the new file.
    f.vcx.simulate_keystrokes("secondary-e");
    f.draw();
    let reader = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("reader");
    let shown = reader.read_with(&f.vcx, |r, _| r.path().map(Path::to_path_buf));
    assert_eq!(shown, Some(f.path("sub/copy.md")));
}

#[gpui::test]
fn save_as_from_preview_loads_the_text_and_copies_it(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let answer = f.path("copy.md");
    let dialog = FakeDialog::install(&mut f, [Ok(Some(answer))]);
    f.vcx.dispatch_action(file_commands::SaveAs);
    f.draw();
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.read("copy.md"), NOTE.as_bytes());
    assert_eq!(f.tab_path(), Some(f.path("copy.md")));
    let mode = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().mode());
    assert_eq!(mode, ViewMode::Preview);
}

#[gpui::test]
fn cancelling_save_as_changes_nothing(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    f.type_at_end("Added.");
    let dialog = FakeDialog::install(&mut f, [Ok(None)]);
    f.save_as();
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.tab_path(), Some(f.path("note.md")));
    assert_eq!(f.read("note.md"), NOTE.as_bytes());
    assert!(f.modified(&editor));
    assert_eq!(f.topic(&editor), None);
    assert_eq!(f.journals().len(), 1);
}

#[gpui::test]
fn an_added_extension_never_replaces_a_file_unasked(cx: &mut TestAppContext) {
    let other = b"Other text.\n";
    let mut f = fixture(
        cx,
        &[("note.md", NOTE.as_bytes()), ("other.md", other)],
        Some("note.md"),
        false,
    );
    let editor = f.edit();
    f.type_at_end("Added.");
    // "other" becomes "other.md", which the dialog did not ask about.
    let answer = f.path("other");
    let dialog = FakeDialog::install(&mut f, [Ok(Some(answer))]);
    f.save_as();
    assert_eq!(f.read("other.md"), other, "not replaced");
    assert_eq!(f.topic(&editor), Some(Topic::SaveFailed));
    let notice = editor
        .read_with(&f.vcx, |e, _| e.notice().cloned())
        .expect("notice");
    assert!(notice.offers(&NoticeAction::SaveAs));
    assert_eq!(f.tab_path(), Some(f.path("note.md")));
    assert!(f.modified(&editor));

    // Chosen as "other.md" in the dialog, which asked about replacing it.
    dialog.answer(Ok(Some(f.path("other.md"))));
    f.click("infobar-action-0");
    assert_eq!(dialog.asked(), 2);
    assert_eq!(f.read("other.md"), format!("{NOTE}Added.").as_bytes());
    assert_eq!(f.tab_path(), Some(f.path("other.md")));
    assert_eq!(f.topic(&editor), None);
}

#[gpui::test]
fn save_as_refuses_a_file_open_in_another_tab(cx: &mut TestAppContext) {
    let mut f = fixture(
        cx,
        &[("note.md", NOTE.as_bytes()), ("other.md", b"Other.\n")],
        Some("other.md"),
        false,
    );
    let target = OpenTarget::new(f.path("note.md"));
    f.workspace
        .update_in(&mut f.vcx, |ws, window, cx| ws.open_tab(target, window, cx));
    let editor = f.edit();
    f.type_at_end("Added.");
    let answer = f.path("other.md");
    let dialog = FakeDialog::install(&mut f, [Ok(Some(answer))]);
    f.save_as();
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.read("other.md"), b"Other.\n");
    assert_eq!(f.topic(&editor), Some(Topic::SaveFailed));
    assert_eq!(f.tab_path(), Some(f.path("note.md")));
}

#[gpui::test]
fn a_dialog_that_cannot_open_shows_the_save_error(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    let _dialog = FakeDialog::install(&mut f, [Err("no file chooser portal".to_owned())]);
    f.save_as();
    assert_eq!(f.topic(&editor), Some(Topic::SaveFailed));
    let detail = editor
        .read_with(&f.vcx, |e, _| e.notice().and_then(|n| n.detail.clone()))
        .expect("detail");
    assert!(detail.contains("no file chooser portal"), "{detail}");
}

#[gpui::test]
fn a_deleted_file_offers_save_as(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.edit();
    std::fs::remove_file(f.path("note.md")).expect("delete");
    f.report("note.md", ExternalChange::Deleted);
    let answer = f.path("moved.md");
    let dialog = FakeDialog::install(&mut f, [Ok(Some(answer))]);
    f.act(&editor, NoticeAction::SaveAs);
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.read("moved.md"), NOTE.as_bytes());
    assert_eq!(f.tab_path(), Some(f.path("moved.md")));
    assert_eq!(f.topic(&editor), None);
}

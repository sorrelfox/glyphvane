// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Closing with unsaved changes (a tab, a window, the Linux close button,
//! quitting), the recovery infobar's Restore, Compare and Discard, the
//! compare and merge view (conflicts, recovery, sync-conflict copies) and
//! the large-file and read-only infobars, in headless windows over
//! temporary files.

use std::path::Path;
use std::time::Duration;

use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use gv_vault::WatchEvent;
use gv_vault::files::ExternalChange;
use gv_vault::files::journal::JournalStore;
use gv_vault::index::conflict::SyncService;

use super::notice::{Notice, NoticeAction, Topic};
use super::tests::{FakeDialog, Fixture, NOTE, crashed_session, fixture, options};
use crate::chrome::WindowChrome;
use crate::compare::{CompareView, HunkKind, Pick};
use crate::editor::EditorView;
use crate::keymap::actions::tabs as tab_commands;
use crate::shell::{OpenTarget, Workspace};
use crate::{AppSettings, ThemeChoice};

impl Fixture {
    /// The window's question, if the active tab shows one.
    fn question(&self) -> Option<Notice> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab()
                .notice()
                .filter(|n| n.topic == Topic::CloseUnsaved)
                .cloned()
        })
    }

    /// Clicks infobar button `index` and runs until idle, without drawing
    /// again (the window may have closed).
    fn press(&mut self, index: usize) {
        self.draw();
        let selector: &'static str = match index {
            0 => "infobar-action-0",
            1 => "infobar-action-1",
            2 => "infobar-action-2",
            _ => "infobar-action-3",
        };
        let at = self
            .vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        self.vcx.run_until_parked();
    }

    fn tab_count(&self) -> usize {
        self.workspace.read_with(&self.vcx, |ws, _| ws.tabs().len())
    }

    fn new_untitled(&mut self, text: &str) -> Entity<EditorView> {
        self.vcx.dispatch_action(tab_commands::New);
        self.draw();
        let editor = self.editor();
        self.vcx.simulate_input(text);
        self.draw();
        editor
    }

    fn open(&mut self, name: &str) {
        let target = OpenTarget::new(self.path(name));
        self.workspace.update_in(&mut self.vcx, |ws, window, cx| {
            ws.open_tab(target, window, cx)
        });
        self.draw();
    }

    fn autosave(&mut self, on: bool) {
        self.vcx
            .update(|_, cx| super::override_autosave(cx, Some(on)));
    }

    fn compare(&self, editor: &Entity<EditorView>) -> Option<Entity<CompareView>> {
        editor.read_with(&self.vcx, |e, _| e.compare_view())
    }

    fn notice(&self, editor: &Entity<EditorView>) -> Option<Notice> {
        editor.read_with(&self.vcx, |e, _| e.notice().cloned())
    }

    fn undo(&mut self, editor: &Entity<EditorView>) {
        editor.update(&mut self.vcx, |e, cx| e.undo_for_tests(cx));
        self.draw();
    }
}

// ----- closing a tab -------------------------------------------------------------

#[gpui::test]
fn closing_an_untitled_tab_asks_and_cancel_keeps_it(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    let editor = f.new_untitled("Draft");
    assert_eq!(f.journals(), ["untitled-1"]);

    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    let question = f.question().expect("the tab asks before closing");
    assert_eq!(question.title.as_ref(), "Save changes to “Untitled”?");
    assert_eq!(question.kind, super::notice::NoticeKind::Warning);
    assert_eq!(
        question
            .buttons
            .iter()
            .map(|b| b.label.to_string())
            .collect::<Vec<_>>(),
        ["Save", "Don't save", "Cancel"]
    );
    assert_eq!(f.tab_count(), 2, "nothing closed yet");

    // Cancel: nothing closes, nothing is lost.
    f.press(2);
    assert!(f.question().is_none());
    assert_eq!(f.tab_count(), 2);
    assert!(f.modified(&editor));
    assert_eq!(f.journals(), ["untitled-1"]);

    // Dismissing the question cancels too.
    f.vcx.simulate_keystrokes("secondary-w");
    f.click("infobar-dismiss");
    assert!(f.question().is_none());
    assert_eq!(f.tab_count(), 2);

    // Don't save: the tab closes and the journal goes.
    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    f.press(1);
    assert_eq!(f.tab_count(), 1);
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn saving_an_untitled_tab_on_close_asks_where(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.new_untitled("# Plan\n\nSteps.");
    // A cancelled save dialog stops the close.
    let dialog = FakeDialog::install(&mut f, [Ok(None)]);
    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    f.press(0);
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.tab_count(), 2, "the close stopped");
    assert!(f.question().is_none());
    assert_eq!(f.journals(), ["untitled-1"]);

    // Answered: the file is written, then the tab closes.
    dialog.answer(Ok(Some(f.path("plan.md"))));
    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    f.press(0);
    assert_eq!(dialog.asked(), 2);
    assert_eq!(f.read("plan.md"), b"# Plan\n\nSteps.");
    assert_eq!(f.tab_count(), 1);
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn closing_a_tab_without_autosave_saves_when_asked(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.autosave(false);
    f.edit();
    f.type_at_end("A");
    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    let question = f.question().expect("asks");
    assert_eq!(question.title.as_ref(), "Save changes to “note.md”?");
    f.press(0);
    assert_eq!(f.read("note.md"), format!("{NOTE}A").as_bytes());
    assert!(f.journals().is_empty());
    let path = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().path().is_some());
    assert!(!path, "the tab closed (an empty one is left)");
}

#[gpui::test]
fn a_failing_save_stops_the_close(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.autosave(false);
    let editor = f.edit();
    f.type_at_end("A");
    f.vcx.simulate_keystrokes("secondary-w");
    f.draw();
    // A folder takes the file's place, so the save cannot write it.
    std::fs::remove_file(f.path("note.md")).expect("remove");
    std::fs::create_dir(f.path("note.md")).expect("folder in the way");
    f.press(0);
    assert_eq!(f.tab_count(), 1);
    let still_open = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().path().is_some());
    assert!(still_open, "the tab stays");
    assert!(f.modified(&editor));
    assert!(f.notice(&editor).is_some(), "the document says why");
    assert_eq!(f.journals().len(), 1, "the text is still journaled");
}

// ----- closing a window, the Linux close button, quitting ---------------------

#[gpui::test]
fn closing_a_window_lists_every_unsaved_document(cx: &mut TestAppContext) {
    let mut f = fixture(
        cx,
        &[("a.md", NOTE.as_bytes()), ("b.md", NOTE.as_bytes())],
        Some("a.md"),
        false,
    );
    f.autosave(false);
    f.edit();
    f.type_at_end("A");
    f.new_untitled("Scratch");
    f.open("b.md");
    // Unchanged: not asked about.
    f.edit();

    assert!(!f.vcx.simulate_close(), "the window asks first");
    f.draw();
    let question = f.question().expect("asks");
    assert_eq!(question.title.as_ref(), "Save changes to 2 documents?");
    let detail = question.detail.expect("names").to_string();
    assert!(detail.starts_with("“a.md”, “Untitled”."), "{detail}");

    // Cancel keeps the window and everything in it.
    f.press(2);
    assert_eq!(f.vcx.windows().len(), 1);
    assert_eq!(f.journals().len(), 2);

    // Don't save closes it; the file is untouched and the journals go.
    assert!(!f.vcx.simulate_close());
    f.press(1);
    assert!(f.vcx.windows().is_empty());
    assert_eq!(f.read("a.md"), NOTE.as_bytes());
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn saving_several_documents_on_window_close(cx: &mut TestAppContext) {
    let mut f = fixture(
        cx,
        &[("a.md", NOTE.as_bytes()), ("b.md", NOTE.as_bytes())],
        Some("a.md"),
        false,
    );
    f.autosave(false);
    f.edit();
    f.type_at_end("A");
    f.new_untitled("Scratch");
    f.open("b.md");
    f.edit();
    f.type_at_end("B");
    let scratch = f.path("scratch.md");
    let dialog = FakeDialog::install(&mut f, [Ok(Some(scratch))]);
    assert!(!f.vcx.simulate_close());
    f.press(0);
    assert_eq!(dialog.asked(), 1, "only the untitled document asks where");
    assert_eq!(f.read("a.md"), format!("{NOTE}A").as_bytes());
    assert_eq!(f.read("b.md"), format!("{NOTE}B").as_bytes());
    assert_eq!(f.read("scratch.md"), b"Scratch");
    assert!(f.vcx.windows().is_empty(), "then it closes");
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn a_cancelled_save_dialog_stops_the_window_close(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("a.md", NOTE.as_bytes())], Some("a.md"), false);
    f.autosave(false);
    f.edit();
    f.type_at_end("A");
    f.new_untitled("Scratch");
    let dialog = FakeDialog::install(&mut f, [Ok(None)]);
    assert!(!f.vcx.simulate_close());
    f.press(0);
    assert_eq!(dialog.asked(), 1);
    assert_eq!(f.vcx.windows().len(), 1, "the close stopped");
    // Saved before the dialog came: a.md is the first in the list.
    assert_eq!(f.read("a.md"), format!("{NOTE}A").as_bytes());
    assert_eq!(f.journals(), ["untitled-1"]);
    assert!(f.question().is_none());
}

#[gpui::test]
fn the_linux_close_button_asks_about_an_untitled_document(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.new_untitled("Draft");
    super::tests::click_linux_close(&mut f);
    f.vcx.run_until_parked();
    assert_eq!(f.vcx.windows().len(), 1);
    assert!(f.question().is_some());
    f.press(2);
    assert_eq!(f.vcx.windows().len(), 1, "Cancel keeps it");
    super::tests::click_linux_close(&mut f);
    f.vcx.run_until_parked();
    f.press(1);
    assert!(f.vcx.windows().is_empty());
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn quitting_asks_and_cancel_quits_nothing(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    f.edit();
    f.type_at_end("A");
    f.new_untitled("Draft");
    f.vcx.update(|_, cx| super::quit(cx));
    f.draw();
    let question = f.question().expect("quitting asks");
    // The file autosaves; only the untitled document is asked about.
    assert_eq!(question.title.as_ref(), "Save changes to “Untitled”?");
    f.press(2);
    assert!(f.question().is_none());
    // Switching tabs autosaved the file; the draft is still journaled.
    assert_eq!(f.journals(), ["untitled-1"], "nothing dropped");

    // Don't save: the file is saved by autosave, the draft dropped.
    f.vcx.update(|_, cx| super::quit(cx));
    f.press(1);
    assert_eq!(f.read("note.md"), format!("{NOTE}A").as_bytes());
    assert!(f.journals().is_empty());
}

// ----- recovery -------------------------------------------------------------------

/// A window over a folder whose journal holds `added` for `note.md`, after
/// the recovery check.
fn recovered(cx: &mut TestAppContext, added: &str) -> (Fixture, Entity<EditorView>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let note = dir.path().join("note.md");
    std::fs::write(&note, NOTE).expect("write");
    crashed_session(
        &JournalStore::open(dir.path().join("journal")).expect("store"),
        &note,
        added,
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
    (f, editor)
}

#[gpui::test]
fn recovery_offers_restore_compare_and_discard(cx: &mut TestAppContext) {
    let (mut f, editor) = recovered(cx, "Recovered.\n");
    let notice = f.notice(&editor).expect("the recovery infobar");
    assert_eq!(notice.topic, Topic::Recovered);
    assert_eq!(
        notice
            .buttons
            .iter()
            .map(|b| (b.label.to_string(), b.disabled.is_none()))
            .collect::<Vec<_>>(),
        [
            ("Restore".to_owned(), true),
            ("Compare".to_owned(), true),
            ("Discard".to_owned(), true)
        ]
    );
    // Restore keeps the recovered text; autosave then writes it.
    f.click("infobar-action-0");
    assert_eq!(f.topic(&editor), None);
    assert_eq!(f.text(&editor), format!("{NOTE}Recovered.\n"));
    f.vcx.executor().advance_clock(Duration::from_secs(2));
    f.draw();
    assert_eq!(f.read("note.md"), format!("{NOTE}Recovered.\n").as_bytes());
}

#[gpui::test]
fn recovery_discard_goes_back_to_the_file(cx: &mut TestAppContext) {
    let (mut f, editor) = recovered(cx, "Recovered.\n");
    f.click("infobar-action-2");
    assert_eq!(f.text(&editor), NOTE);
    assert!(!f.modified(&editor));
    assert!(f.journals().is_empty());
}

#[gpui::test]
fn recovery_compare_takes_the_file_per_change_as_one_undo(cx: &mut TestAppContext) {
    let (mut f, editor) = recovered(cx, "Recovered.\n");
    f.click("infobar-action-1");
    let compare = f.compare(&editor).expect("the compare view opens");
    assert!(f.drawn("compare-view"));
    let hunks: Vec<_> = compare.read_with(&f.vcx, |c, _| {
        c.comparison()
            .hunks()
            .map(|h| (h.kind, h.left.clone(), h.right.clone(), h.pick))
            .collect()
    });
    assert_eq!(
        hunks,
        [(
            HunkKind::Left,
            "Recovered.\n".to_owned(),
            String::new(),
            Some(Pick::Left)
        )],
        "the recovered text against the file, recovered side chosen"
    );
    // Take the file's side, and apply.
    f.click("compare-hunk-0-right");
    f.click("compare-apply");
    assert!(f.compare(&editor).is_none(), "the view closes");
    assert!(!f.drawn("compare-view"));
    assert_eq!(f.text(&editor), NOTE);
    assert_eq!(f.topic(&editor), None, "the question is answered");
    assert_eq!(f.read("note.md"), NOTE.as_bytes(), "nothing written");

    // One undo brings the recovered text back.
    f.undo(&editor);
    assert_eq!(f.text(&editor), format!("{NOTE}Recovered.\n"));
}

#[gpui::test]
fn cancelling_the_compare_view_changes_nothing(cx: &mut TestAppContext) {
    let (mut f, editor) = recovered(cx, "Recovered.\n");
    f.click("infobar-action-1");
    f.click("compare-hunk-0-right");
    f.click("compare-cancel");
    assert!(f.compare(&editor).is_none());
    assert_eq!(f.text(&editor), format!("{NOTE}Recovered.\n"));
    assert_eq!(f.topic(&editor), Some(Topic::Recovered), "still asked");
}

#[gpui::test]
fn a_journal_found_on_open_is_recovered_with_the_infobar(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], None, false);
    let store = JournalStore::open(f.dir.path().join("journal")).expect("store");
    crashed_session(&store, &f.path("note.md"), "Later.\n");
    f.open("note.md");
    let editor = f.edit();
    assert_eq!(f.text(&editor), format!("{NOTE}Later.\n"));
    assert_eq!(f.topic(&editor), Some(Topic::Recovered));
    let notice = f.notice(&editor).expect("notice");
    assert!(notice.offers(&NoticeAction::Compare));
}

// ----- conflicts and sync-conflict copies -------------------------------------------

#[gpui::test]
fn compare_merges_a_change_on_disk_with_unsaved_edits(cx: &mut TestAppContext) {
    let start = "one\ntwo\nthree\nfour\n";
    let mut f = fixture(cx, &[("note.md", start.as_bytes())], Some("note.md"), false);
    f.autosave(false);
    let editor = f.edit();
    f.type_at_end("mine\n");
    std::fs::write(f.path("note.md"), "ONE\ntwo\nthree\nfour\n").expect("write");
    let change = f.change_of(&editor, "note.md");
    f.report("note.md", change);
    assert_eq!(f.topic(&editor), Some(Topic::Conflict));
    let notice = f.notice(&editor).expect("notice");
    let compare_button = notice
        .buttons
        .iter()
        .find(|b| b.action == NoticeAction::Compare)
        .expect("Compare");
    assert!(compare_button.disabled.is_none(), "Compare works now");

    f.act(&editor, NoticeAction::Compare);
    let compare = f.compare(&editor).expect("compare view");
    let kinds: Vec<_> = compare.read_with(&f.vcx, |c, _| {
        c.comparison().hunks().map(|h| (h.kind, h.pick)).collect()
    });
    assert_eq!(
        kinds,
        [
            (HunkKind::Right, Some(Pick::Right)),
            (HunkKind::Left, Some(Pick::Left))
        ]
    );
    f.click("compare-apply");
    assert_eq!(f.text(&editor), "ONE\ntwo\nthree\nfour\nmine\n");
    assert_eq!(f.topic(&editor), None, "resolved like Keep mine");
    assert!(f.modified(&editor));
    assert_eq!(
        f.read("note.md"),
        b"ONE\ntwo\nthree\nfour\n",
        "the disk side is only read"
    );
    // The next save writes the merge over the version on disk.
    editor.update(&mut f.vcx, |e, cx| e.save(cx));
    f.draw();
    assert_eq!(f.read("note.md"), b"ONE\ntwo\nthree\nfour\nmine\n");
    f.undo(&editor);
    assert_eq!(f.text(&editor), format!("{start}mine\n"));
}

#[gpui::test]
fn conflicting_lines_can_take_both_or_stay_marked(cx: &mut TestAppContext) {
    let start = "title\nbody\nend\n";
    let mut f = fixture(cx, &[("note.md", start.as_bytes())], Some("note.md"), false);
    f.autosave(false);
    let editor = f.edit();
    // Change "body" in the buffer.
    editor.update(&mut f.vcx, |e, cx| e.set_cursor_for_tests(6, cx));
    f.vcx.simulate_input("my ");
    f.draw();
    std::fs::write(f.path("note.md"), "title\ntheir body\nend\n").expect("write");
    let change = f.change_of(&editor, "note.md");
    f.report("note.md", change);
    f.act(&editor, NoticeAction::Compare);
    let compare = f.compare(&editor).expect("compare view");
    let unresolved = compare.read_with(&f.vcx, |c, _| c.comparison().unresolved());
    assert_eq!(unresolved, 1);
    assert_eq!(
        compare.read_with(&f.vcx, |c, _| c.comparison().result()),
        "title\n<<<<<<< Your version\nmy body\n=======\ntheir body\n>>>>>>> On disk\nend\n",
        "unresolved conflicts are marked inline"
    );
    f.click("compare-hunk-0-both");
    f.click("compare-apply");
    assert_eq!(f.text(&editor), "title\nmy body\ntheir body\nend\n");
}

#[gpui::test]
fn a_sync_conflict_copy_can_be_compared(cx: &mut TestAppContext) {
    let copy_text = "# Title\n\nFirst paragraph, edited on the phone.\n";
    let copy_name = "note (conflicted copy).md";
    let mut f = fixture(
        cx,
        &[
            ("note.md", NOTE.as_bytes()),
            (copy_name, copy_text.as_bytes()),
        ],
        Some("note.md"),
        false,
    );
    let copy = f.path(copy_name);
    let event = WatchEvent::SyncConflict {
        copy: copy.clone(),
        original: f.path("note.md"),
        service: SyncService::Dropbox,
    };
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.watch_event(event, cx));
    f.draw();
    let notice = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().notice().cloned())
        .expect("the sync-conflict infobar");
    assert!(notice.offers(&NoticeAction::CompareCopy(copy.clone())));
    // From the Reading view: a Source view is made for the compare.
    f.click("infobar-action-0");
    let editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("a Source view");
    f.draw();
    assert!(f.compare(&editor).is_some(), "the copy is compared");
    f.click("compare-all-right");
    f.click("compare-apply");
    assert_eq!(f.text(&editor), copy_text);
    assert_eq!(std::fs::read(&copy).expect("copy"), copy_text.as_bytes());
    assert_eq!(f.read("note.md"), NOTE.as_bytes(), "saved only on request");
}

// ----- large-file and read-only infobars ----------------------------------------

#[gpui::test]
fn large_files_show_the_large_file_infobar(cx: &mut TestAppContext) {
    let long_line = format!("{}\n", "a".repeat(100_001));
    let mut f = fixture(
        cx,
        &[
            ("big.md", long_line.as_bytes()),
            ("small.md", NOTE.as_bytes()),
        ],
        Some("small.md"),
        false,
    );
    let small = f.edit();
    assert_eq!(f.notice(&small), None, "hidden for a small file");
    f.open("big.md");
    let big = f.edit();
    let notice = f.notice(&big).expect("the large-file infobar");
    assert_eq!(notice.topic, Topic::LargeFile);
    assert_eq!(notice.kind, super::notice::NoticeKind::Neutral);
    assert!(notice.offers(&NoticeAction::Details));
    assert!(f.drawn("infobar"));
    f.click("infobar-action-0");
    let details = f.notice(&big).expect("details");
    assert!(!details.offers(&NoticeAction::Details));
    assert!(
        details
            .detail
            .as_ref()
            .is_some_and(|d| d.contains("Soft wrap")),
        "{details:?}"
    );
    // Editing works as usual.
    f.type_at_end("b");
    assert!(f.modified(&big));
    f.click("infobar-dismiss");
    assert_eq!(f.notice(&big), None);
    assert!(!f.drawn("infobar"));
}

#[cfg(unix)]
fn set_read_only(path: &Path, read_only: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if read_only { 0o444 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[cfg(unix)]
#[gpui::test]
fn a_read_only_file_blocks_editing_until_writable(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &[("note.md", NOTE.as_bytes())], Some("note.md"), false);
    set_read_only(&f.path("note.md"), true);
    if std::fs::OpenOptions::new()
        .append(true)
        .open(f.path("note.md"))
        .is_ok()
    {
        // Running as a user permissions do not stop (root): nothing to test.
        set_read_only(&f.path("note.md"), false);
        return;
    }
    let editor = f.edit();
    let notice = f.notice(&editor).expect("the read-only infobar");
    assert_eq!(notice.topic, Topic::ReadOnly);
    assert!(notice.offers(&NoticeAction::SaveAs));
    assert!(editor.read_with(&f.vcx, |e, _| e.is_read_only()));
    f.type_at_end("x");
    assert_eq!(f.text(&editor), NOTE, "typing changes nothing");
    // Viewer mode switching off does not unblock it.
    editor.update(&mut f.vcx, |e, cx| e.set_read_only(false, cx));
    assert!(editor.read_with(&f.vcx, |e, _| e.is_read_only()));

    // Writable again on disk: editing comes back and the infobar goes.
    set_read_only(&f.path("note.md"), false);
    let change = f.change_of(&editor, "note.md");
    assert!(matches!(change, ExternalChange::Touched(_)), "{change:?}");
    f.report("note.md", change);
    assert_eq!(f.notice(&editor), None);
    assert!(!editor.read_with(&f.vcx, |e, _| e.is_read_only()));
    f.type_at_end("x");
    assert_eq!(f.text(&editor), format!("{NOTE}x"));

    // Read-only again while open: blocked again.
    set_read_only(&f.path("note.md"), true);
    let change = f.change_of(&editor, "note.md");
    f.report("note.md", change);
    assert_eq!(f.topic(&editor), Some(Topic::ReadOnly));
    assert!(editor.read_with(&f.vcx, |e, _| e.is_read_only()));
    set_read_only(&f.path("note.md"), false);
}

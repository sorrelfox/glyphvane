// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Version history in headless windows: snapshots on save, the panel's
//! list, preview and events, and Compare and Restore in the Source view.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_vault::files::history::{HistoryPolicy, HistoryStore};
use gv_vault::files::journal::JournalStore;

use super::{HistoryEvent, HistoryPanel, VersionHistory};
use crate::chrome::WindowChrome;
use crate::compare::Pick;
use crate::editor::{EditorStatus, EditorView};
use crate::shell::{OpenTarget, Workspace};
use crate::{AppSettings, ThemeChoice};

const NOTE: &str = "# Title\n\nFirst paragraph.\n";

fn options() -> WindowOptions {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn store_in(dir: &Path) -> HistoryStore {
    HistoryStore::open(dir.join("history"), HistoryPolicy::default()).expect("history store")
}

fn texts(dir: &Path, note: &Path) -> Vec<String> {
    let store = store_in(dir);
    store
        .versions(note)
        .expect("versions")
        .iter()
        .map(|v| String::from_utf8(store.read(v).expect("read")).expect("utf-8"))
        .collect()
}

struct Window {
    dir: tempfile::TempDir,
    note: PathBuf,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

/// A note in a temporary folder, open in a window, with the journal and
/// the version history in the folder.
fn window(cx: &mut TestAppContext) -> Window {
    let dir = tempfile::tempdir().expect("temporary folder");
    let note = dir.path().join("note.md");
    std::fs::write(&note, NOTE).expect("written");
    let target = OpenTarget::new(&note);
    let history = store_in(dir.path());
    let journal = dir.path().join("journal");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        crate::files::install_store(
            cx,
            Some(JournalStore::open(journal).expect("journal")),
            false,
        );
        super::install_store(cx, Some(history));
        gpui::open_window(options(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(Some(target), WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Window {
        dir,
        note,
        workspace,
        vcx,
    }
}

impl Window {
    fn edit(&mut self) -> Entity<EditorView> {
        self.vcx.simulate_keystrokes("secondary-e");
        draw(&mut self.vcx);
        let editor = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
            .expect("a Source view");
        draw(&mut self.vcx);
        assert_eq!(
            editor.read_with(&self.vcx, |e, _| e.status().clone()),
            EditorStatus::Ready
        );
        editor
    }

    fn type_and_save(&mut self, text: &str) {
        self.vcx.simulate_keystrokes("secondary-end");
        self.vcx.simulate_input(text);
        draw(&mut self.vcx);
        self.vcx.simulate_keystrokes("secondary-s");
        draw(&mut self.vcx);
    }

    fn text(&self, editor: &Entity<EditorView>) -> String {
        editor.read_with(&self.vcx, |e, _| {
            e.editor().expect("loaded").text().to_string()
        })
    }
}

#[gpui::test]
fn each_save_with_new_text_keeps_a_version(cx: &mut TestAppContext) {
    let mut w = window(cx);
    let generation = |w: &mut Window| {
        w.vcx
            .update(|_, cx| cx.global::<VersionHistory>().generation())
    };
    w.edit();
    assert!(
        texts(w.dir.path(), &w.note).is_empty(),
        "opening keeps nothing"
    );
    w.type_and_save("One.");
    assert_eq!(texts(w.dir.path(), &w.note), vec![format!("{NOTE}One.")]);
    assert_eq!(generation(&mut w), 1);
    w.type_and_save("Two.");
    assert_eq!(
        texts(w.dir.path(), &w.note),
        vec![format!("{NOTE}One.Two."), format!("{NOTE}One.")]
    );
    // Saving the same text again keeps nothing new.
    w.vcx.simulate_keystrokes("secondary-s");
    draw(&mut w.vcx);
    assert_eq!(texts(w.dir.path(), &w.note).len(), 2);
    assert_eq!(generation(&mut w), 2);
}

#[gpui::test]
fn restore_is_one_undoable_edit(cx: &mut TestAppContext) {
    let mut w = window(cx);
    let editor = w.edit();
    w.type_and_save("One.");
    w.type_and_save("Two.");
    let current = w.text(&editor);
    let old = format!("{NOTE}One.");
    editor.update(&mut w.vcx, |e, cx| e.restore_version(&old, cx));
    draw(&mut w.vcx);
    assert_eq!(w.text(&editor), old);
    assert!(editor.read_with(&w.vcx, |e, _| e.is_modified()));
    assert_eq!(
        std::fs::read_to_string(&w.note).expect("read"),
        current,
        "nothing is written before a save"
    );
    w.vcx.simulate_keystrokes("secondary-z");
    draw(&mut w.vcx);
    assert_eq!(w.text(&editor), current, "one undo brings it back");
}

#[gpui::test]
fn compare_with_a_version_applies_the_chosen_side(cx: &mut TestAppContext) {
    let mut w = window(cx);
    let editor = w.edit();
    w.type_and_save("One.");
    let old: Arc<str> = Arc::from(NOTE);
    editor.update(&mut w.vcx, |e, cx| {
        e.compare_with_version("version of today".into(), old.clone(), cx);
    });
    draw(&mut w.vcx);
    let view = editor
        .read_with(&w.vcx, |e, _| e.compare_view())
        .expect("the compare view is open");
    let (hunks, right) = view.read_with(&w.vcx, |v, _| {
        (
            v.comparison().hunk_count(),
            v.comparison().sides().right.clone(),
        )
    });
    assert_eq!(hunks, 1);
    assert_eq!(right.as_ref(), "version of today");
    view.update(&mut w.vcx, |v, cx| {
        v.pick_all(Pick::Right, cx);
        v.apply(cx);
    });
    draw(&mut w.vcx);
    assert!(editor.read_with(&w.vcx, |e, _| e.compare_view().is_none()));
    assert_eq!(w.text(&editor), NOTE);
    w.vcx.simulate_keystrokes("secondary-z");
    draw(&mut w.vcx);
    assert_eq!(w.text(&editor), format!("{NOTE}One."));
}

/// A panel in a window of its own, over a store with three versions of a
/// note, an hour apart.
/// The events a panel emitted.
type Events = Rc<RefCell<Vec<HistoryEvent>>>;

fn panel(
    cx: &mut TestAppContext,
) -> (
    tempfile::TempDir,
    PathBuf,
    Entity<HistoryPanel>,
    Events,
    VisualTestContext,
) {
    let dir = tempfile::tempdir().expect("temporary folder");
    let note = dir.path().join("note.md");
    let store = store_in(dir.path());
    let start = SystemTime::now() - Duration::from_secs(3 * 3600);
    for (i, text) in ["one\n", "one\ntwo\n", "one\ntwo\nthree\n"]
        .iter()
        .enumerate()
    {
        store
            .record(
                &note,
                text.as_bytes(),
                start + Duration::from_secs(3600 * i as u64),
            )
            .expect("record");
    }
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let path = note.clone();
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        super::install_store(cx, Some(store));
        gpui::open_window(options(), cx, |window, cx| {
            let view = cx.new(|cx| HistoryPanel::new(Some(path), cx));
            cx.subscribe(&view, move |_, event: &HistoryEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
            view.read(cx).focus_handle(cx).focus(window, cx);
            view
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    (dir, note, view, events, vcx)
}

#[gpui::test]
fn the_panel_lists_previews_and_asks_the_window(cx: &mut TestAppContext) {
    let (_dir, note, view, events, mut vcx) = panel(cx);
    let versions = view.read_with(&vcx, |p, _| p.versions().to_vec());
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0].size, "one\ntwo\nthree\n".len() as u64);
    assert!(vcx.debug_bounds("history-row-2").is_some());
    assert!(vcx.debug_bounds("history-preview").is_none());

    // Down selects the newest, then the next.
    vcx.simulate_keystrokes("down down");
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |p, _| p.selected()), Some(1));
    assert_eq!(
        view.read_with(&vcx, |p, _| p.preview_text()).as_deref(),
        Some("one\ntwo\n")
    );
    assert!(vcx.debug_bounds("history-preview").is_some());

    // Enter compares; the buttons compare and restore.
    vcx.simulate_keystrokes("enter");
    let at = vcx.debug_bounds("history-restore").expect("drawn").center();
    vcx.simulate_click(at, Modifiers::none());
    draw(&mut vcx);
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    let got = events.borrow().clone();
    assert_eq!(got.len(), 3, "{got:?}");
    match &got[0] {
        HistoryEvent::Compare {
            path,
            text,
            label,
            time,
        } => {
            assert_eq!(path, &note);
            assert_eq!(text.as_ref(), "one\ntwo\n");
            assert!(label.starts_with("Version of "), "{label}");
            assert_eq!(*time, versions[1].time);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(&got[1], HistoryEvent::Restore { text, .. } if text.as_ref() == "one\ntwo\n"));
    assert_eq!(got[2], HistoryEvent::Dismiss);

    // Copy.
    let at = vcx.debug_bounds("history-copy").expect("drawn").center();
    vcx.simulate_click(at, Modifiers::none());
    draw(&mut vcx);
    let copied = vcx
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("one\ntwo\n"));
}

#[gpui::test]
fn the_panel_follows_new_versions_and_files(cx: &mut TestAppContext) {
    let (dir, note, view, _events, mut vcx) = panel(cx);
    vcx.simulate_keystrokes("down");
    draw(&mut vcx);
    let selected_time = view.read_with(&vcx, |p, _| p.versions()[0].time);

    // A save elsewhere keeps a version: the list grows, the selection
    // stays on its version.
    vcx.update(|_, cx| {
        super::record_saved(note.clone(), gv_text::Buffer::new("four\n").snapshot(), cx);
    });
    draw(&mut vcx);
    let (count, selected) = view.read_with(&vcx, |p, _| (p.versions().len(), p.selected()));
    assert_eq!(count, 4);
    assert_eq!(selected, Some(1));
    assert_eq!(
        view.read_with(&vcx, |p, _| p.versions()[1].time),
        selected_time
    );

    // Another file without versions, then no file.
    let other = dir.path().join("other.md");
    view.update(&mut vcx, |p, cx| p.set_path(Some(other), cx));
    draw(&mut vcx);
    assert!(view.read_with(&vcx, |p, _| p.versions().is_empty() && !p.is_loading()));
    assert!(vcx.debug_bounds("history-message").is_some());
    view.update(&mut vcx, |p, cx| p.set_path(None, cx));
    draw(&mut vcx);
    assert!(view.read_with(&vcx, |p, _| p.path().is_none()));
}

#[test]
fn sizes_and_times_read_well() {
    assert_eq!(super::panel::human_size(512), "512 B");
    assert_eq!(super::panel::human_size(1536), "1.5 KB");
    assert_eq!(super::panel::human_size(3 * 1024 * 1024), "3.0 MB");
    let now = SystemTime::now();
    assert!(super::panel::when(now, now).starts_with("Today "));
    let two_days = now - Duration::from_secs(3 * 86_400);
    assert!(!super::panel::when(two_days, now).starts_with("Today"));
}

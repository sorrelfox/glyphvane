// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The session model, its save schedule, and saving and restoring windows in
//! headless GPUI apps.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui::{AnyWindowHandle, Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::reader::ScrollTarget;
use crate::{
    AppSettings, OpenTarget, Placement, ThemeChoice, ToggleEditMode, TogglePanel, ToggleSidebar,
};

fn place(path: &str, offset: usize) -> HistoryEntry {
    HistoryEntry {
        path: PathBuf::from(path),
        mode: ViewMode::Preview,
        scroll: Some(ScrollTarget::Offset(offset)),
        pdf: None,
    }
}

fn tab(path: &str) -> TabState {
    TabState {
        path: PathBuf::from(path),
        mode: ViewMode::Preview,
        anchor: None,
        back: Vec::new(),
        forward: Vec::new(),
        split_ratio: None,
        folds: Vec::new(),
        reading_folds: Vec::new(),
        split_reading_folds: Vec::new(),
        live_preview: false,
        viewer_mode: None,
        pdf: None,
    }
}

fn sample() -> Session {
    Session {
        version: SESSION_VERSION,
        windows: vec![WindowState {
            bounds: Some(SavedBounds {
                state: BoundsState::Maximized,
                x: 10.0,
                y: 20.0,
                width: 1200.0,
                height: 800.0,
            }),
            tabs: vec![
                TabState {
                    mode: ViewMode::Edit,
                    anchor: Some(Anchor::Offset(120)),
                    back: vec![Place {
                        path: PathBuf::from("/n/a.md"),
                        mode: ViewMode::Preview,
                        anchor: Some(Anchor::Line(3)),
                        pdf: None,
                    }],
                    ..tab("/n/b.md")
                },
                TabState {
                    mode: ViewMode::Split,
                    ..tab("/n/c.md")
                },
            ],
            active_tab: 1,
            sidebar_open: true,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        active_window: Some(0),
        recent_files: vec![PathBuf::from("/n/c.md"), PathBuf::from("/n/b.md")],
        recent_folders: vec![PathBuf::from("/n")],
    }
}

#[test]
fn a_session_survives_json() {
    let session = sample();
    let text = serde_json::to_string_pretty(&session).expect("serializes");
    assert!(text.contains("\"mode\": \"edit\""), "{text}");
    assert!(text.contains("\"offset\": 120"), "{text}");
    assert!(text.contains("\"state\": \"maximized\""), "{text}");
    let back: Session = serde_json::from_str(&text).expect("parses");
    assert_eq!(back, session);
}

#[test]
fn missing_fields_take_their_defaults() {
    let session: Session = serde_json::from_str(
        r#"{"version": 1, "windows": [{"tabs": [{"path": "/n/a.md"}]}], "later": true}"#,
    )
    .expect("parses");
    let window = &session.windows[0];
    assert_eq!(window.tabs, [tab("/n/a.md")]);
    assert_eq!(
        (window.active_tab, window.sidebar_open, window.panel_open),
        (0, false, false)
    );
    assert!(window.bounds.is_none() && session.recent_files.is_empty());
}

#[test]
fn history_comes_back_with_both_stacks() {
    let mut h = History::default();
    for (i, name) in ["a.md", "b.md", "c.md", "d.md"].iter().enumerate() {
        h.push(place(name, i));
    }
    // At e.md; go back twice, so two places are ahead.
    let to = h.go_back(place("e.md", 9)).expect("back");
    h.go_back(to).expect("back");
    assert_eq!((h.back().len(), h.forward().len()), (2, 2));

    let rebuilt = history(&places(h.back()), &places(h.forward()));
    assert_eq!(rebuilt, h);
    assert_eq!(history(&[], &[]), History::default());
}

#[test]
fn stored_history_is_bounded() {
    let entries: Vec<HistoryEntry> = (0..HISTORY_LIMIT + 7).map(|i| place("a.md", i)).collect();
    let kept = places(&entries);
    assert_eq!(kept.len(), HISTORY_LIMIT);
    assert_eq!(kept[0].anchor, Some(Anchor::Offset(7)));
}

#[test]
fn a_link_fragment_is_not_an_anchor() {
    let fragment = ScrollTarget::Fragment(gv_vault::Fragment::from_wikilink("intro"));
    assert_eq!(Anchor::from_target(&fragment), None);
    assert_eq!(
        Anchor::from_target(&ScrollTarget::Line(4)).map(Anchor::target),
        Some(ScrollTarget::Line(4))
    );
}

#[test]
fn files_that_are_gone_are_skipped() {
    let window = WindowState {
        bounds: None,
        tabs: vec![tab("/n/a.md"), tab("/n/gone.md"), tab("/n/c.md")],
        active_tab: 1,
        sidebar_open: true,
        panel_open: false,
        sidebar_panel: None,
        files_sort: None,
        vault: None,
        base: None,
    };
    let exists = |p: &Path| !p.ends_with("gone.md");
    let kept = present(window.clone(), exists).expect("two tabs left");
    assert_eq!(kept.tabs, [tab("/n/a.md"), tab("/n/c.md")]);
    // The active tab went; the one before it takes over.
    assert_eq!(kept.active_tab, 0);
    assert!(kept.sidebar_open);

    let none = |_: &Path| false;
    assert_eq!(present(window.clone(), none), None);
    let empty = WindowState {
        tabs: Vec::new(),
        ..window
    };
    assert_eq!(present(empty.clone(), none), Some(empty));
}

#[test]
fn bounds_are_fitted_to_the_displays() {
    let display = Rect {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };
    let second = Rect {
        x: 1920.0,
        ..display
    };
    let at = |x, y, width, height| Rect {
        x,
        y,
        width,
        height,
    };
    // On screen: kept.
    let r = at(100.0, 50.0, 1000.0, 700.0);
    assert_eq!(fit(r, &[display, second]), Fit::At(r));
    // Mostly on the second display, sticking out: moved inside it.
    assert_eq!(
        fit(at(3000.0, 600.0, 1000.0, 700.0), &[display, second]),
        Fit::At(at(2840.0, 380.0, 1000.0, 700.0))
    );
    // Larger than the display: shrunk to it.
    assert_eq!(
        fit(at(-50.0, 0.0, 4000.0, 3000.0), &[display]),
        Fit::At(display)
    );
    // On a display that is gone: centred.
    assert_eq!(
        fit(at(5000.0, 0.0, 1000.0, 700.0), &[display]),
        Fit::Centered(1000.0, 700.0)
    );
    // Tiny or broken sizes grow to the minimum.
    assert_eq!(
        fit(at(0.0, 0.0, 10.0, f32::NAN), &[]),
        Fit::At(at(0.0, 0.0, MIN_WIDTH, MIN_HEIGHT))
    );
    assert_eq!(
        fit(at(f32::INFINITY, 0.0, 800.0, 600.0), &[display]),
        Fit::Centered(800.0, 600.0)
    );
}

/// `path` made absolute the way the recent lists store it (on Windows
/// `/n/1.md` gains the current drive).
fn abs(path: &str) -> PathBuf {
    std::path::absolute(path).expect("absolute")
}

#[test]
fn recent_lists_move_to_the_front_and_are_bounded() {
    let mut session = Session::default();
    for i in 0..RECENT_LIMIT + 5 {
        session.touch_recent_file(Path::new(&format!("/n/{i}.md")));
    }
    assert_eq!(session.recent_files.len(), RECENT_LIMIT);
    assert_eq!(session.recent_files[0], abs("/n/54.md"));
    session.touch_recent_file(Path::new("/n/30.md"));
    assert_eq!(session.recent_files[0], abs("/n/30.md"));
    assert_eq!(session.recent_files.len(), RECENT_LIMIT);
    assert_eq!(
        session
            .recent_files
            .iter()
            .filter(|p| p.ends_with("30.md"))
            .count(),
        1
    );
    session.touch_recent_folder(Path::new("/n"));
    session.touch_recent_folder(Path::new("/m"));
    session.touch_recent_folder(Path::new("/n"));
    assert_eq!(session.recent_folders, [abs("/n"), abs("/m")]);
}

#[test]
fn changes_are_folded_into_one_save() {
    let mut s = SaveSchedule::default();
    assert!(s.change(), "the first change schedules a save");
    assert!(!s.change(), "later changes wait for it");
    s.start();
    assert!(!s.change(), "a change while saving waits");
    assert!(s.finish(), "and gets the next save");
    assert!(!s.change());
    s.start();
    assert!(!s.finish(), "nothing changed meanwhile");
    assert!(s.change());
}

/// Keeps every saved session, as JSON, like the real file.
#[derive(Debug, Default)]
struct Memory(Mutex<Vec<String>>);

impl SessionSink for Memory {
    fn save(&self, session: &Session) -> std::io::Result<()> {
        let text = serde_json::to_string(session).map_err(std::io::Error::other)?;
        self.0.lock().expect("not poisoned").push(text);
        Ok(())
    }
}

impl Memory {
    fn count(&self) -> usize {
        self.0.lock().expect("not poisoned").len()
    }

    fn last(&self) -> Session {
        let saved = self.0.lock().expect("not poisoned");
        serde_json::from_str(saved.last().expect("saved once")).expect("parses")
    }
}

#[test]
fn an_older_write_never_replaces_a_newer_one() {
    let memory = Arc::new(Memory::default());
    let writer = Writer {
        sink: memory.clone(),
        written: Mutex::new(0),
    };
    let session = Session::default();
    assert!(writer.write(2, &session).expect("written"));
    assert!(!writer.write(1, &session).expect("skipped"));
    assert!(writer.write(3, &session).expect("written"));
    assert_eq!(memory.count(), 2);
}

fn start(cx: &mut TestAppContext, session: Session, memory: &Arc<Memory>) {
    let sink: Arc<dyn SessionSink> = memory.clone();
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        install(cx, SessionStore::new(session, Some(sink)));
    });
}

fn front(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        let w = most_recent_window(cx).expect("a window is open");
        (w.window, w.workspace.upgrade().expect("open"))
    })
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn long_doc(title: &str) -> String {
    let mut s = format!("# {title}\n\n");
    for section in ["One", "Two", "Three", "Four"] {
        s.push_str(&format!("## {section}\n\n"));
        for i in 0..12 {
            s.push_str(&format!(
                "Paragraph {i} of this section has enough words to fill a line or two.\n\n"
            ));
        }
    }
    s
}

fn top_offset(ws: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Option<usize> {
    let reader = ws
        .read_with(vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("a reader");
    match reader.read_with(vcx, |r, _| r.position()) {
        Some(ScrollTarget::Offset(offset)) => Some(offset),
        _ => None,
    }
}

fn scroll_to(ws: &Entity<Workspace>, vcx: &mut VisualTestContext, offset: usize) {
    let reader = ws
        .read_with(vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("a reader");
    reader.update(vcx, |r, cx| r.scroll_to(ScrollTarget::Offset(offset), cx));
    draw(vcx);
}

#[gpui::test]
fn saves_wait_for_the_delay_and_fold_changes(cx: &mut TestAppContext) {
    let memory = Arc::new(Memory::default());
    start(cx, Session::default(), &memory);
    cx.update(|cx| {
        assert_eq!(restore(cx).expect("restores"), None);
        crate::open(cx, Vec::new(), Placement::RecentWindow).expect("an empty window opens");
    });
    cx.executor().advance_clock(SAVE_DELAY * 2);
    cx.run_until_parked();
    let before = memory.count();
    let (window, workspace) = front(cx);
    assert!(workspace.read_with(cx, |ws, _| ws.in_session()));

    cx.dispatch_action(window, TogglePanel);
    cx.executor().advance_clock(SAVE_DELAY / 2);
    cx.run_until_parked();
    cx.dispatch_action(window, ToggleEditMode);
    cx.run_until_parked();
    assert_eq!(
        memory.count(),
        before,
        "nothing is written before the delay"
    );

    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();
    assert_eq!(memory.count(), before + 1, "both changes in one save");
    assert!(memory.last().windows[0].panel_open);

    cx.executor().advance_clock(SAVE_DELAY * 3);
    cx.run_until_parked();
    assert_eq!(memory.count(), before + 1, "no change, no save");
}

#[gpui::test]
fn tabs_modes_and_anchors_come_back(cx: &mut TestAppContext, cx2: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (a_text, b_text) = (long_doc("A"), long_doc("B"));
    let (a, b) = (dir.path().join("a.md"), dir.path().join("b.md"));
    std::fs::write(&a, &a_text).expect("write");
    std::fs::write(&b, &b_text).expect("write");
    let a_at = a_text.find("## Three").expect("heading");
    let b_at = b_text.find("## Two").expect("heading");

    // First run: a plain launch, two files opened, read and switched.
    let memory = Arc::new(Memory::default());
    start(cx, Session::default(), &memory);
    cx.update(|cx| {
        assert_eq!(restore(cx).expect("restores"), None);
        crate::open(cx, Vec::new(), Placement::RecentWindow).expect("an empty window opens");
        crate::open(
            cx,
            vec![OpenTarget::new(&a), OpenTarget::new(&b)],
            Placement::RecentWindow,
        )
        .expect("the files open");
    });
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    scroll_to(&workspace, &mut vcx, b_at);
    vcx.dispatch_action(ToggleEditMode);
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.activate_tab(0, window, cx)));
    draw(&mut vcx);
    scroll_to(&workspace, &mut vcx, a_at);
    vcx.dispatch_action(ToggleSidebar);
    draw(&mut vcx);
    cx.update(save_now);

    let saved = memory.last();
    assert_eq!(saved.windows.len(), 1);
    let w = &saved.windows[0];
    assert_eq!(
        w.tabs
            .iter()
            .map(|t| (t.path.clone(), t.mode, t.anchor))
            .collect::<Vec<_>>(),
        [
            (a.clone(), ViewMode::Preview, Some(Anchor::Offset(a_at))),
            (b.clone(), ViewMode::Edit, Some(Anchor::Offset(b_at))),
        ]
    );
    assert_eq!(
        (w.active_tab, w.sidebar_open, w.panel_open),
        (0, true, false)
    );
    assert!(w.bounds.is_some());
    assert!(saved.recent_files.contains(&a) && saved.recent_files.contains(&b));

    // Second run, a new application with what was saved.
    let memory2 = Arc::new(Memory::default());
    start(cx2, saved.clone(), &memory2);
    let restored = cx2.update(restore).expect("restores");
    assert!(restored.is_some());
    let (window, workspace) = front(cx2);
    let mut vcx = VisualTestContext::from_window(window, cx2);
    draw(&mut vcx);
    let tabs = workspace.read_with(&vcx, |ws, _| {
        ws.tabs()
            .iter()
            .map(|t| (t.path().map(Path::to_path_buf), t.mode()))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        tabs,
        [(Some(a), ViewMode::Preview), (Some(b), ViewMode::Edit)]
    );
    assert_eq!(
        workspace.read_with(&vcx, |ws, _| (ws.active, ws.sidebar_open, ws.panel_open)),
        (0, true, false)
    );
    assert_eq!(top_offset(&workspace, &mut vcx), Some(a_at));
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.activate_tab(1, window, cx)));
    draw(&mut vcx);
    // b.md is in Edit, where the Reading view is not laid out: its anchor
    // is kept as restored, and Preview shows it there.
    vcx.update(|_, cx| save_now(cx));
    let again = memory2.last();
    assert_eq!(again.windows[0].tabs, saved.windows[0].tabs);
    assert_eq!(again.windows[0].active_tab, 1);
    vcx.dispatch_action(ToggleEditMode);
    draw(&mut vcx);
    assert_eq!(top_offset(&workspace, &mut vcx), Some(b_at));
}

#[gpui::test]
fn a_launch_with_a_file_neither_restores_nor_saves_its_panels(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("note.md");
    std::fs::write(&file, "# Note\n").expect("write");
    let stored = sample();
    let memory = Arc::new(Memory::default());
    start(cx, stored.clone(), &memory);

    // A launch with a file: no restore, just the file.
    cx.update(|cx| {
        crate::open(cx, vec![OpenTarget::new(&file)], Placement::RecentWindow)
            .expect("the file opens");
    });
    let (window, workspace) = front(cx);
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    assert_eq!(
        workspace.read_with(cx, |ws, _| (
            ws.in_session(),
            ws.sidebar_open,
            ws.panel_open
        )),
        (false, false, false)
    );

    // Panels opened by hand there are not written anywhere.
    cx.dispatch_action(window, ToggleSidebar);
    cx.dispatch_action(window, TogglePanel);
    cx.executor().advance_clock(SAVE_DELAY * 2);
    cx.run_until_parked();
    cx.update(save_now);
    let saved = memory.last();
    assert_eq!(saved.windows, stored.windows, "the stored windows are kept");
    assert_eq!(saved.active_window, stored.active_window);
    let file = std::path::absolute(&file).expect("absolute");
    assert_eq!(saved.recent_files[0], file, "the file is a recent file");
}

#[gpui::test]
fn a_new_window_for_a_file_stays_out_of_a_restored_session(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("note.md");
    std::fs::write(&file, "# Note\n").expect("write");
    let memory = Arc::new(Memory::default());
    start(cx, Session::default(), &memory);
    cx.update(|cx| {
        assert_eq!(restore(cx).expect("restores"), None);
        crate::open(cx, Vec::new(), Placement::RecentWindow).expect("an empty window opens");
        crate::open(cx, vec![OpenTarget::new(&file)], Placement::NewWindow)
            .expect("the file opens");
    });
    let (window, workspace) = front(cx);
    assert!(!workspace.read_with(cx, |ws, _| ws.in_session()));
    cx.dispatch_action(window, ToggleSidebar);
    cx.run_until_parked();
    cx.update(save_now);
    let saved = memory.last();
    assert_eq!(saved.windows.len(), 1, "only the session window");
    assert!(saved.windows[0].tabs.is_empty());
    assert!(!saved.windows[0].sidebar_open);
}

#[gpui::test]
fn closing_one_of_two_windows_drops_it_but_the_last_stays(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (a, b) = (dir.path().join("a.md"), dir.path().join("b.md"));
    std::fs::write(&a, "# A\n").expect("write");
    std::fs::write(&b, "# B\n").expect("write");
    let stored = Session {
        windows: vec![
            WindowState {
                bounds: None,
                tabs: vec![TabState {
                    path: a.clone(),
                    ..tab("")
                }],
                active_tab: 0,
                sidebar_open: false,
                panel_open: true,
                sidebar_panel: None,
                files_sort: None,
                vault: None,
                base: None,
            },
            WindowState {
                bounds: None,
                tabs: vec![TabState {
                    path: b.clone(),
                    ..tab("")
                }],
                active_tab: 0,
                sidebar_open: false,
                panel_open: false,
                sidebar_panel: None,
                files_sort: None,
                vault: None,
                base: None,
            },
        ],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    let front_window = cx.update(restore).expect("restores").expect("a window");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
    // The last stored window opened last and is in front.
    let (_, workspace) = front(cx);
    assert_eq!(
        workspace.read_with(cx, |ws, _| ws.active_tab().path().map(Path::to_path_buf)),
        Some(b)
    );

    cx.update_window(front_window, |_, window, _| window.remove_window())
        .expect("the window is open");
    cx.run_until_parked();
    cx.update(save_now);
    let saved = memory.last();
    assert_eq!(saved.windows.len(), 1);
    assert_eq!(saved.windows[0].tabs[0].path, a);
    assert!(saved.windows[0].panel_open);
}

#[test]
fn panels_folders_dividers_and_folds_survive_json() {
    let mut session = sample();
    session.windows[0].sidebar_panel = Some("tags".into());
    session.windows[0].files_sort = Some("modified".into());
    session.windows[0].vault = Some(PathBuf::from("/n"));
    session.windows[0].tabs[1].split_ratio = Some(0.3);
    session.windows[0].tabs[1].folds = vec![[0, 40], [60, 90]];
    session.windows[0].tabs[1].reading_folds = vec![crate::reader::fold_state::ReadingFold {
        kind: crate::reader::fold_state::FoldKind::Details,
        line: "<details>".into(),
        nth: 2,
        open: true,
        at: Some([2, 3]),
    }];
    session.windows[0].base = Some(SidebarBase {
        file: Some("Books.base".into()),
        view: Some("Shelf".into()),
        collapsed: true,
    });
    let text = serde_json::to_string(&session).expect("serializes");
    let back: Session = serde_json::from_str(&text).expect("parses");
    assert_eq!(back, session);
    // An older session has none of them.
    let text = serde_json::to_string(&sample()).expect("serializes");
    assert!(
        !text.contains("sidebar_panel")
            && !text.contains("files_sort")
            && !text.contains("folds")
            && !text.contains("base"),
        "{text}"
    );
}

#[test]
fn a_window_opened_on_a_folder_stays_without_its_files() {
    let window = WindowState {
        bounds: None,
        tabs: vec![tab("/n/gone.md")],
        active_tab: 0,
        sidebar_open: true,
        panel_open: false,
        sidebar_panel: None,
        files_sort: None,
        vault: Some(PathBuf::from("/n")),
        base: None,
    };
    let kept = present(window, |_| false).expect("the folder's window stays");
    assert!(kept.tabs.is_empty());
    assert_eq!(kept.vault.as_deref(), Some(Path::new("/n")));
}

#[gpui::test]
fn folders_panels_dividers_and_folds_come_back(cx: &mut TestAppContext, cx2: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let folder = dir.path().to_path_buf();
    let a = folder.join("a.md");
    std::fs::write(&a, long_doc("A")).expect("write");

    // First run: an empty window takes the folder, opens a.md in Split,
    // moves the divider, folds, and shows the Tags panel.
    let memory = Arc::new(Memory::default());
    start(cx, Session::default(), &memory);
    cx.update(|cx| {
        assert_eq!(restore(cx).expect("restores"), None);
        crate::open(cx, Vec::new(), Placement::RecentWindow).expect("an empty window opens");
    });
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_folder(folder.clone(), window, cx);
            ws.open_tab(OpenTarget::new(&a), window, cx);
            ws.set_mode(ViewMode::Split, cx);
        });
    });
    draw(&mut vcx);
    let split = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().split().cloned())
        .expect("a Split view");
    split.update(&mut vcx, |s, cx| s.set_ratio(0.3, cx));
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("a Source view");
    vcx.update(|window, cx| {
        use gpui::Focusable as _;
        editor.read(cx).focus_handle(cx).focus(window, cx);
    });
    vcx.dispatch_action(crate::keymap::actions::view::FoldLevel1);
    draw(&mut vcx);
    let folds = editor.read_with(&vcx, |e, _| e.fold_ranges());
    assert!(!folds.is_empty(), "the section folded");
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.show_sidebar_panel(SidebarPanel::Tags, false, window, cx);
            ws.set_files_sort(SortOrder::ModifiedNewest, cx);
        });
    });
    draw(&mut vcx);
    cx.update(save_now);

    let saved = memory.last();
    let w = &saved.windows[0];
    assert_eq!(w.vault.as_deref(), Some(folder.as_path()));
    assert_eq!(w.sidebar_panel.as_deref(), Some("tags"));
    assert_eq!(w.files_sort.as_deref(), Some("modified"));
    assert!(w.sidebar_open);
    let t = &w.tabs[0];
    assert_eq!(t.mode, ViewMode::Split);
    assert!(
        t.split_ratio.is_some_and(|r| (r - 0.3).abs() < 0.001),
        "{t:?}"
    );
    assert_eq!(
        t.folds,
        folds.iter().map(|r| [r.start, r.end]).collect::<Vec<_>>()
    );

    // Second run: all of it comes back.
    let memory2 = Arc::new(Memory::default());
    start(cx2, saved.clone(), &memory2);
    cx2.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx2);
    let mut vcx = VisualTestContext::from_window(window, cx2);
    draw(&mut vcx);
    draw(&mut vcx);
    let (root, panel) = workspace.read_with(&vcx, |ws, cx| (ws.vault_root(cx), ws.panels.sidebar));
    assert_eq!(root.as_deref(), Some(folder.as_path()));
    assert_eq!(panel, SidebarPanel::Tags);
    let sort = workspace.read_with(&vcx, |ws, cx| {
        (ws.files_sort, ws.files.as_ref().map(|f| f.read(cx).sort()))
    });
    assert_eq!(
        sort,
        (SortOrder::ModifiedNewest, Some(SortOrder::ModifiedNewest)),
        "the Files panel's order comes back"
    );
    let split = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().split().cloned())
        .expect("a Split view");
    assert!((split.read_with(&vcx, |s, _| s.ratio()) - 0.3).abs() < 0.001);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("a Source view");
    assert_eq!(editor.read_with(&vcx, |e, _| e.fold_ranges()), folds);
    vcx.update(|_, cx| save_now(cx));
    assert_eq!(memory2.last().windows, saved.windows);
}

#[gpui::test]
fn a_folder_window_without_files_comes_back_with_its_vault(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("a.md"), "# A\n").expect("write");
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: Vec::new(),
            active_tab: 0,
            sidebar_open: true,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: Some(dir.path().to_path_buf()),
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let (root, open) = workspace.read_with(&vcx, |ws, cx| (ws.vault_root(cx), ws.sidebar_open));
    assert_eq!(root.as_deref(), Some(dir.path()));
    assert!(open);
}

#[gpui::test]
fn reading_view_folds_come_back_with_the_tab(cx: &mut TestAppContext) {
    use crate::reader::fold_state::{FoldKind, ReadingFold};

    let dir = tempfile::tempdir().expect("temp dir");
    // An Obsidian vault: callouts are read as such.
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    std::fs::write(dir.path().join(".obsidian/app.json"), "{}").expect("app.json");
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n\n> [!faq]- Question?\n> The answer.\n").expect("write");
    let folds = vec![ReadingFold {
        kind: FoldKind::Callout,
        line: "Question?".into(),
        nth: 0,
        open: true,
        at: Some([0, 1]),
    }];
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                path: a.clone(),
                reading_folds: folds.clone(),
                ..tab("")
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    // Saved before the Reading view has shown the file: kept as restored.
    cx.update(save_now);
    assert_eq!(memory.last().windows[0].tabs[0].reading_folds, folds);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        draw(&mut vcx);
        let shown = workspace.read_with(&vcx, |ws, cx| {
            ws.active_tab()
                .reader()
                .map(|r| r.read(cx).reading_folds())
                .unwrap_or_default()
        });
        if shown == folds {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{shown:?}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    vcx.update(|_, cx| save_now(cx));
    assert_eq!(memory.last().windows[0].tabs[0].reading_folds, folds);

    // Closing the callout schedules a save that keeps it closed.
    let reader = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("a Reading view");
    let at = reader.read_with(&vcx, |r, _| r.fold_controls())[0];
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.run_until_parked();
    vcx.executor().advance_clock(SAVE_DELAY * 2);
    vcx.run_until_parked();
    let saved = &memory.last().windows[0].tabs[0].reading_folds;
    assert_eq!(saved.len(), 1);
    assert!(!saved[0].open, "{saved:?}");
}

#[gpui::test]
fn split_preview_folds_come_back_with_the_tab(cx: &mut TestAppContext) {
    use crate::reader::fold_state::{FoldKind, ReadingFold};

    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    std::fs::write(dir.path().join(".obsidian/app.json"), "{}").expect("app.json");
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n\n> [!faq]- Question?\n> The answer.\n").expect("write");
    let folds = vec![ReadingFold {
        kind: FoldKind::Callout,
        line: "Question?".into(),
        nth: 0,
        open: true,
        at: Some([0, 1]),
    }];
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                path: a.clone(),
                mode: ViewMode::Split,
                split_reading_folds: folds.clone(),
                ..tab("")
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        draw(&mut vcx);
        let shown = workspace.read_with(&vcx, |ws, cx| {
            ws.active_tab()
                .split_reader(cx)
                .map(|r| r.read(cx).reading_folds())
                .unwrap_or_default()
        });
        if shown == folds {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{shown:?}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    vcx.update(|_, cx| save_now(cx));
    let saved = &memory.last().windows[0].tabs[0];
    assert_eq!(saved.split_reading_folds, folds);
    assert!(
        saved.reading_folds.is_empty(),
        "the panes fold on their own"
    );
}

#[test]
fn a_session_without_split_folds_still_reads() {
    let json = r#"{"path":"/n/a.md","reading_folds":[{"kind":"callout","line":"Q","open":true}]}"#;
    let tab: TabState = serde_json::from_str(json).expect("reads");
    assert_eq!(tab.reading_folds.len(), 1);
    assert!(tab.split_reading_folds.is_empty());
    let back = serde_json::to_string(&tab).expect("json");
    assert!(!back.contains("split_reading_folds"), "{back}");
}

#[test]
fn live_preview_is_stored_only_when_chosen() {
    let json = r#"{"path":"/n/a.md","mode":"edit"}"#;
    let tab: TabState = serde_json::from_str(json).expect("reads");
    assert!(!tab.live_preview, "Source by default");
    let back = serde_json::to_string(&tab).expect("json");
    assert!(!back.contains("live_preview"), "{back}");
    let on = TabState {
        live_preview: true,
        ..tab
    };
    let json = serde_json::to_string(&on).expect("json");
    let back: TabState = serde_json::from_str(&json).expect("reads");
    assert!(back.live_preview);
}

/// A tab restored in Live Preview edits in Live Preview, and is saved so.
#[gpui::test]
fn live_preview_comes_back_with_the_session(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n\nSome **bold** text.\n").expect("write");
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                path: a.clone(),
                mode: ViewMode::Edit,
                live_preview: true,
                ..tab("")
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let chosen = workspace.read_with(&vcx, |ws, cx| {
        ws.active_tab().editor().map(|e| e.read(cx).live_preview())
    });
    assert_eq!(chosen, Some(true));
    vcx.update(|_, cx| save_now(cx));
    assert!(memory.last().windows[0].tabs[0].live_preview);
    // Toggled off: Source, and saved so.
    workspace.update(&mut vcx, |ws, cx| ws.toggle_live_preview(cx));
    let chosen = workspace.read_with(&vcx, |ws, cx| {
        ws.active_tab().editor().map(|e| e.read(cx).live_preview())
    });
    assert_eq!(chosen, Some(false));
    vcx.update(|_, cx| save_now(cx));
    assert!(!memory.last().windows[0].tabs[0].live_preview);
}

#[gpui::test]
fn a_text_file_comes_back_in_edit(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "# not a heading\n").expect("write");
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                path: file.clone(),
                mode: ViewMode::Edit,
                ..tab("")
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let editor = loop {
        draw(&mut vcx);
        let editor = workspace.read_with(&vcx, |ws, cx| {
            ws.active_tab()
                .editor()
                .filter(|e| *e.read(cx).status() == crate::editor::EditorStatus::Ready)
                .cloned()
        });
        if let Some(editor) = editor {
            break editor;
        }
        assert!(std::time::Instant::now() < deadline, "the editor loads");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let mode = workspace.read_with(&vcx, |ws, _| ws.active_tab().mode());
    assert_eq!(mode, ViewMode::Edit);
    assert!(editor.read_with(&vcx, |e, _| e.is_plain()));
    assert!(workspace.read_with(&vcx, |ws, _| ws.active_tab().viewer().is_some()));
    vcx.update(|_, cx| save_now(cx));
    assert_eq!(memory.last().windows[0].tabs[0].mode, ViewMode::Edit);
}

/// A file that is text only by its content (no known name) comes back in
/// Edit at once, not in Preview until its viewer has read it.
#[gpui::test]
fn a_sniffed_text_file_comes_back_in_edit(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("notes.weird");
    std::fs::write(&file, "plain words\n").expect("write");
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                path: file.clone(),
                mode: ViewMode::Edit,
                ..tab("")
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    // Before anything was read: Edit, with its Source view.
    let (mode, editor) = workspace.read_with(&vcx, |ws, _| {
        (ws.active_tab().mode(), ws.active_tab().editor().cloned())
    });
    assert_eq!(mode, ViewMode::Edit);
    let editor = editor.expect("the Source view");
    let viewer = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().viewer().cloned())
        .expect("the viewer");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        draw(&mut vcx);
        let mode = workspace.read_with(&vcx, |ws, _| ws.active_tab().mode());
        assert_eq!(mode, ViewMode::Edit);
        let ready = editor.read_with(&vcx, |e, _| {
            *e.status() == crate::editor::EditorStatus::Ready
        });
        let read = viewer.read_with(&vcx, |v, _| {
            matches!(v.status(), crate::file_viewer::ViewerStatus::Text { .. })
        });
        if ready && read {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the editor loads");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(editor.read_with(&vcx, |e, _| e.is_plain() && !e.is_read_only()));
    vcx.update(|_, cx| save_now(cx));
    assert_eq!(memory.last().windows[0].tabs[0].mode, ViewMode::Edit);
}

#[test]
fn viewer_mode_and_the_pdf_place_are_stored_only_when_set() {
    use crate::file_viewer::{PdfPlace, PdfZoom};

    // A session from before: both missing.
    let json = r#"{"path":"/n/paper.pdf","mode":"preview"}"#;
    let tab: TabState = serde_json::from_str(json).expect("reads");
    assert_eq!(tab.viewer_mode, None);
    assert_eq!(tab.pdf, None);
    let back = serde_json::to_string(&tab).expect("json");
    assert!(
        !back.contains("viewer_mode") && !back.contains("\"pdf\""),
        "{back}"
    );

    let set = TabState {
        viewer_mode: Some(false),
        pdf: Some(PdfPlace {
            page: 4,
            into: Some(36.5),
            zoom: PdfZoom::Scale(1.5),
        }),
        ..tab
    };
    let json = serde_json::to_string(&set).expect("json");
    let back: TabState = serde_json::from_str(&json).expect("reads");
    assert_eq!(back, set);

    // Only the page: the page's top at fit width.
    let json = r#"{"path":"/n/paper.pdf","pdf":{"page":2},"viewer_mode":true}"#;
    let tab: TabState = serde_json::from_str(json).expect("reads");
    assert_eq!(tab.viewer_mode, Some(true));
    assert_eq!(
        tab.pdf,
        Some(PdfPlace {
            page: 2,
            into: None,
            zoom: PdfZoom::FitWidth
        })
    );
    let fit_page: PdfPlace =
        serde_json::from_str(r#"{"page":1,"zoom":"fit-page"}"#).expect("reads");
    assert_eq!(fit_page.zoom, PdfZoom::FitPage);
}

/// A PDF tab comes back at its page and zoom, and is saved where it is read.
#[gpui::test]
fn a_pdf_tab_comes_back_at_its_page(cx: &mut TestAppContext) {
    use crate::file_viewer::pdf_tests::FakePdf;
    use crate::file_viewer::{PdfPlace, PdfZoom};

    let dir = tempfile::tempdir().expect("temp dir");
    let paper = dir.path().join("paper.pdf");
    std::fs::write(&paper, b"%PDF-1.7\n%%EOF\n").expect("write");
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                path: paper.clone(),
                pdf: Some(PdfPlace {
                    page: 5,
                    into: Some(80.0),
                    zoom: PdfZoom::Scale(1.25),
                }),
                ..tab("")
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    let engine = Arc::new(FakePdf::pages(&[(612.0, 792.0); 8]));
    cx.update(|cx| {
        crate::export::set_pdf_engine(cx, Some(engine as Arc<dyn crate::export::PdfEngine>));
    });
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let pdf = loop {
        draw(&mut vcx);
        let viewer = workspace.read_with(&vcx, |ws, _| ws.active_tab().viewer().cloned());
        if let Some(pdf) = viewer.and_then(|v| v.read_with(&vcx, |v, _| v.pdf().cloned())) {
            break pdf;
        }
        assert!(std::time::Instant::now() < deadline, "the PDF viewer shows");
    };
    draw(&mut vcx);
    pdf.read_with(&vcx, |p, _| {
        assert_eq!(p.current_page(), 5);
        assert_eq!(p.zoom(), PdfZoom::Scale(1.25));
    });
    vcx.update(|_, cx| save_now(cx));
    let saved = memory.last().windows[0].tabs[0]
        .pdf
        .expect("the place is kept");
    assert_eq!(saved.page, 5);
    assert_eq!(saved.zoom, PdfZoom::Scale(1.25));
    assert!(
        (saved.into.expect("an offset") - 80.0).abs() < 0.5,
        "{saved:?}"
    );

    // Read further: saved at the new page.
    pdf.update(&mut vcx, |p, cx| p.go_to(2, cx));
    draw(&mut vcx);
    vcx.update(|_, cx| save_now(cx));
    let saved = memory.last().windows[0].tabs[0]
        .pdf
        .expect("the place is kept");
    assert_eq!(saved.page, 2);
}

/// Viewer mode switched for a tab comes back with it, and is saved when
/// switched again.
#[gpui::test]
fn a_tab_viewer_mode_comes_back(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("a.md");
    std::fs::write(&a, "# A\n").expect("write");
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                path: a.clone(),
                viewer_mode: Some(true),
                ..tab("")
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let on = |vcx: &mut VisualTestContext| {
        workspace.read_with(vcx, |ws, cx| ws.viewer_mode_of(ws.active, cx))
    };
    assert!(on(&mut vcx), "viewer mode, though the setting is off");
    workspace.read_with(&vcx, |ws, cx| {
        assert!(!ws.mode_available(ViewMode::Edit, cx));
    });
    vcx.update(|_, cx| save_now(cx));
    assert_eq!(memory.last().windows[0].tabs[0].viewer_mode, Some(true));
    workspace.update(&mut vcx, |ws, cx| ws.toggle_viewer_mode(cx));
    assert!(!on(&mut vcx));
    vcx.update(|_, cx| save_now(cx));
    assert_eq!(memory.last().windows[0].tabs[0].viewer_mode, Some(false));
}

/// A PDF in a tab's history keeps its place in the session: Back from a
/// restored tab shows it there, and it is saved again.
#[gpui::test]
fn a_pdf_in_the_history_comes_back_at_its_page(cx: &mut TestAppContext) {
    use crate::file_viewer::pdf_tests::FakePdf;
    use crate::file_viewer::{PdfPlace, PdfZoom};

    let dir = tempfile::tempdir().expect("temp dir");
    let paper = dir.path().join("paper.pdf");
    std::fs::write(&paper, b"%PDF-1.7\n%%EOF\n").expect("write");
    let note = dir.path().join("note.md");
    std::fs::write(&note, "# Note\n").expect("write");
    let left = PdfPlace {
        page: 5,
        into: Some(40.0),
        zoom: PdfZoom::Scale(1.25),
    };
    let stored = Session {
        windows: vec![WindowState {
            bounds: None,
            tabs: vec![TabState {
                back: vec![Place {
                    path: paper.clone(),
                    mode: ViewMode::Preview,
                    anchor: None,
                    pdf: Some(left),
                }],
                ..tab(note.to_str().expect("utf-8"))
            }],
            active_tab: 0,
            sidebar_open: false,
            panel_open: false,
            sidebar_panel: None,
            files_sort: None,
            vault: None,
            base: None,
        }],
        ..Session::default()
    };
    let memory = Arc::new(Memory::default());
    start(cx, stored, &memory);
    let engine = Arc::new(FakePdf::pages(&[(612.0, 792.0); 8]));
    cx.update(|cx| {
        crate::export::set_pdf_engine(cx, Some(engine as Arc<dyn crate::export::PdfEngine>));
    });
    cx.update(restore).expect("restores").expect("a window");
    let (window, workspace) = front(cx);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_back(window, cx)));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let pdf = loop {
        draw(&mut vcx);
        let viewer = workspace.read_with(&vcx, |ws, _| ws.active_tab().viewer().cloned());
        if let Some(pdf) = viewer.and_then(|v| v.read_with(&vcx, |v, _| v.pdf().cloned())) {
            break pdf;
        }
        assert!(std::time::Instant::now() < deadline, "the PDF viewer shows");
    };
    draw(&mut vcx);
    pdf.read_with(&vcx, |p, _| {
        assert_eq!(p.current_page(), 5);
        assert_eq!(p.zoom(), PdfZoom::Scale(1.25));
    });

    // Forward to the note: the PDF's place is saved with the way back.
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_forward(window, cx)));
    draw(&mut vcx);
    vcx.update(|_, cx| save_now(cx));
    let saved = memory.last().windows[0].tabs[0].clone();
    assert_eq!(saved.path, note);
    let way_back = saved.back.last().expect("the way back");
    assert_eq!(way_back.path, paper);
    let kept = way_back.pdf.expect("the PDF's place is kept");
    assert_eq!(kept.page, 5);
    assert_eq!(kept.zoom, PdfZoom::Scale(1.25));
    assert!(
        (kept.into.expect("an offset") - 40.0).abs() < 0.5,
        "{kept:?}"
    );
}

#[test]
fn a_history_place_without_a_pdf_place_still_reads() {
    let json = r#"{"path":"/n/b.md","back":[{"path":"/n/p.pdf","mode":"preview"}]}"#;
    let tab: TabState = serde_json::from_str(json).expect("reads");
    assert_eq!(tab.back.len(), 1);
    assert_eq!(tab.back[0].pdf, None);
    let back = serde_json::to_string(&tab).expect("json");
    assert!(!back.contains("pdf\":"), "{back}");
}

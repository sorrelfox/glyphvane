// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The File history panel in a window, in headless GPUI windows over a
//! temporary folder: `file.history`, the panel following the active tab,
//! Compare and Restore (queued until the Source view has loaded), Esc, and
//! histories following renames and moves.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_vault::VaultPath;
use gv_vault::files::history::{HistoryPolicy, HistoryStore};

use super::panels::SidebarPanel;
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::editor::{EditorStatus, EditorView};
use crate::history::{HistoryEvent, HistoryPanel};
use crate::keymap::actions::file as commands;
use crate::{AppSettings, ThemeChoice};

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn store_in(dir: &Path) -> HistoryStore {
    HistoryStore::open(dir.join(".data/history"), HistoryPolicy::default()).expect("store")
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    for (name, text) in [("A.md", "# A\n\nnow\n"), ("B.md", "# B\n\nnow\n")] {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    let store = store_in(dir.path());
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    store
        .record(&dir.path().join("A.md"), b"# A\n\nold\n", t0)
        .expect("kept");
    let target = Some(OpenTarget::new(dir.path().join("A.md")));
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        crate::history::install_store(cx, Some(store));
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
    }
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn panel(&self) -> Entity<HistoryPanel> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.history_panel())
            .expect("the File history panel exists")
    }

    fn panel_path(&self) -> Option<PathBuf> {
        self.panel()
            .read_with(&self.vcx, |p, _| p.path().map(Path::to_path_buf))
    }

    fn active_editor(&self) -> Option<Entity<EditorView>> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().editor().cloned())
    }

    /// Waits (in real time: files load on other threads) for `done`.
    fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            draw(&mut self.vcx);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn editor_text(&self) -> Option<String> {
        let editor = self.active_editor()?;
        editor.read_with(&self.vcx, |e, _| {
            (*e.status() == EditorStatus::Ready)
                .then(|| e.editor().map(|e| e.text().to_string()))
                .flatten()
        })
    }

    fn emit(&mut self, event: HistoryEvent) {
        self.panel().update(&mut self.vcx, |_, cx| cx.emit(event));
        draw(&mut self.vcx);
    }
}

#[gpui::test]
fn file_history_shows_the_panel_on_the_active_file(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::History);
    draw(&mut f.vcx);
    let (panel, focused) = f.vcx.update(|window, cx| {
        let ws = f.workspace.read(cx);
        (ws.sidebar_panel(), ws.history_focused(window, cx))
    });
    assert_eq!(panel, SidebarPanel::FileHistory);
    assert!(focused, "the panel has the keyboard");
    assert_eq!(f.panel_path(), Some(f.path("A.md")));
    f.until("the versions", |f| {
        f.panel().read_with(&f.vcx, |p, _| p.versions().len()) == 1
    });

    // Another tab: the panel follows it.
    let b = f.path("B.md");
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        ws.open_tab(OpenTarget::new(b), window, cx);
    });
    draw(&mut f.vcx);
    assert_eq!(f.panel_path(), Some(f.path("B.md")));

    // Esc gives the keyboard back to the document.
    f.emit(HistoryEvent::Dismiss);
    let focused = f
        .vcx
        .update(|window, cx| f.workspace.read(cx).history_focused(window, cx));
    assert!(!focused);
}

#[gpui::test]
fn restore_waits_for_the_source_view_and_replaces_the_text(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::History);
    draw(&mut f.vcx);
    // B is not open: Restore opens it, goes to Edit and restores once
    // loaded.
    let b = f.path("B.md");
    f.emit(HistoryEvent::Restore {
        path: b.clone(),
        time: SystemTime::now(),
        text: Arc::from("# B\n\nrestored\n"),
    });
    let active = f.workspace.read_with(&f.vcx, |ws, _| {
        ws.active_tab().path().map(Path::to_path_buf)
    });
    assert_eq!(active, Some(b.clone()));
    let mode = f.workspace.read_with(&f.vcx, |ws, _| ws.mode());
    assert_eq!(mode, ViewMode::Edit);
    f.until("the restored text", |f| {
        f.editor_text().as_deref() == Some("# B\n\nrestored\n")
    });
    // Nothing is written until saved.
    assert_eq!(std::fs::read_to_string(&b).expect("read"), "# B\n\nnow\n");
}

#[gpui::test]
fn compare_opens_the_compare_view_once_loaded(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::History);
    draw(&mut f.vcx);
    let a = f.path("A.md");
    f.emit(HistoryEvent::Compare {
        path: a,
        time: SystemTime::now(),
        label: "Version".into(),
        text: Arc::from("# A\n\nold\n"),
    });
    f.until("the compare view", |f| {
        f.active_editor()
            .is_some_and(|e| e.read_with(&f.vcx, |e, _| e.compare_view().is_some()))
    });
}

#[gpui::test]
fn histories_follow_renames_and_moved_folders(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let root = f.dir.path().to_path_buf();
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    std::fs::create_dir(root.join("Dir")).expect("dir");
    std::fs::write(root.join("Dir").join("C.md"), "c\n").expect("write");
    store_in(&root)
        .record(&root.join("Dir").join("C.md"), b"c old\n", t0)
        .expect("kept");

    std::fs::rename(root.join("A.md"), root.join("Z.md")).expect("renamed");
    std::fs::rename(root.join("Dir"), root.join("Moved")).expect("moved");
    let vp = |s: &str| VaultPath::new(s).expect("vault path");
    let moved = vec![(vp("A.md"), vp("Z.md")), (vp("Dir"), vp("Moved"))];
    f.workspace.update(&mut f.vcx, |ws, cx| {
        ws.carry_history(&root, &moved, cx);
    });
    let count = |path: &Path| store_in(&root).versions(path).map_or(0, |v| v.len());
    let z = root.join("Z.md");
    let c = root.join("Moved").join("C.md");
    f.until("Z's history", |_| count(&z) == 1);
    f.until("C's history", |_| count(&c) == 1);
    assert_eq!(count(&root.join("A.md")), 0);
    assert_eq!(count(&root.join("Dir").join("C.md")), 0);
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Sync conflicts panel in a window, in headless GPUI windows over a
//! temporary vault: View ▸ Sync conflicts, Compare, Open copy, Keep
//! original through the window's trash, Esc, and new content snapshots.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_vault::files::SyncChoice;
use gv_vault::refactor::Trash;

use super::panels::SidebarPanel;
use super::vault_ops::TrashCan;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::conflicts::SyncConflictsPanel;
use crate::keymap::actions::workspace as commands;
use crate::{AppSettings, ThemeChoice};

const COPY: &str = "Note (conflicted copy 2026-01-02).md";

/// Removes files and remembers them.
struct FakeTrash(Mutex<Vec<PathBuf>>);

impl Trash for FakeTrash {
    fn delete(&self, path: &Path) -> Result<(), String> {
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
        self.0.lock().expect("lock").push(path.to_path_buf());
        Ok(())
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    trash: Arc<FakeTrash>,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    for (name, text) in [
        ("Note.md", "# Note\n\nmine\n"),
        (COPY, "# Note\n\ntheirs\n"),
        ("Other.md", "# Other\n"),
    ] {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    let trash = Arc::new(FakeTrash(Mutex::default()));
    let target = Some(OpenTarget::new(dir.path().join("Other.md")));
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        cx.set_global(TrashCan(trash.clone()));
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
        trash,
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
    fn panel(&self) -> Entity<SyncConflictsPanel> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.conflicts_panel())
            .expect("the sync conflicts panel exists")
    }

    /// Announces the content index's snapshots and draws.
    fn poll(&mut self) {
        let vault = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
        }
        draw(&mut self.vcx);
    }

    fn copies(&self) -> Vec<String> {
        self.panel().read_with(&self.vcx, |p, _| {
            p.entries()
                .iter()
                .map(|e| e.copy.as_str().to_owned())
                .collect()
        })
    }

    /// Waits (in real time: the index has its own thread) until the panel
    /// lists `copies`.
    fn until_copies(&mut self, copies: &[&str]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.poll();
            if self.copies() == copies {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "the panel did not list {copies:?} in time: {:?}",
            self.copies()
        );
    }

    fn active_file(&self) -> String {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab()
                .path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    }
}

#[gpui::test]
fn view_sync_conflicts_lists_the_copies_with_the_keyboard(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowSyncConflicts);
    draw(&mut f.vcx);
    f.workspace.read_with(&f.vcx, |ws, _| {
        assert_eq!(ws.sidebar_panel(), SidebarPanel::SyncConflicts);
    });
    f.until_copies(&[COPY]);
    let panel = f.panel();
    assert!(
        f.vcx
            .update(|window, cx| panel.read(cx).focus_handle(cx).is_focused(window)),
        "View ▸ Sync conflicts gives the panel the keyboard"
    );
    // Esc gives the keyboard back to the document.
    f.vcx.simulate_keystrokes("escape");
    draw(&mut f.vcx);
    assert!(
        !f.vcx
            .update(|window, cx| panel.read(cx).focus_handle(cx).is_focused(window))
    );
    // Another panel takes its place; the keyboard is not left behind.
    f.vcx.dispatch_action(commands::ShowSyncConflicts);
    draw(&mut f.vcx);
    f.vcx.dispatch_action(commands::ShowFiles);
    draw(&mut f.vcx);
    assert!(
        !f.vcx
            .update(|window, cx| panel.read(cx).focus_handle(cx).is_focused(window))
    );
}

#[gpui::test]
fn compare_opens_the_original_against_the_copy(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowSyncConflicts);
    f.until_copies(&[COPY]);
    let panel = f.panel();
    panel.update(&mut f.vcx, |p, cx| p.compare(0, cx));
    draw(&mut f.vcx);
    assert_eq!(f.active_file(), "Note.md");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let shown = f.workspace.read_with(&f.vcx, |ws, cx| {
            ws.active_tab()
                .editor()
                .is_some_and(|e| e.read(cx).compare_view().is_some())
        });
        if shown {
            break;
        }
        assert!(Instant::now() < deadline, "no compare view");
        draw(&mut f.vcx);
        std::thread::sleep(Duration::from_millis(10));
    }

    // Open copy: the copy in a tab.
    panel.update(&mut f.vcx, |p, cx| p.open_copy(0, cx));
    draw(&mut f.vcx);
    assert_eq!(f.active_file(), COPY);
}

#[gpui::test]
fn keep_original_uses_the_windows_trash(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowSyncConflicts);
    f.until_copies(&[COPY]);
    f.panel().update(&mut f.vcx, |p, cx| {
        p.resolve(0, SyncChoice::KeepOriginal, cx)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while f.trash.0.lock().expect("lock").is_empty() {
        assert!(Instant::now() < deadline, "nothing went to the trash");
        draw(&mut f.vcx);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        *f.trash.0.lock().expect("lock"),
        vec![f.dir.path().join(COPY)]
    );
    assert!(f.dir.path().join("Note.md").exists());
    f.until_copies(&[]);
}

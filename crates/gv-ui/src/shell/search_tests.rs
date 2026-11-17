// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Search panel in a window: `search.vault`, the switcher, opening a
//! result, in headless GPUI windows over a temporary vault.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};

use super::panels::SidebarPanel;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::search as commands;
use crate::search_panel::{self, Progress, SearchPanel};
use crate::{AppSettings, ThemeChoice};

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    let files = [
        ("A.md", "# A\n\nNothing here.\n"),
        (
            "B.md",
            "# B\n\n## Part\n\nThe needle is in B.\n\nMore text.\n",
        ),
        ("C.md", "Another needle.\n"),
    ];
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    let target = Some(OpenTarget::new(dir.path().join("A.md")));
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
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
    fn panel(&self) -> Entity<SearchPanel> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.panels.search.clone())
            .expect("the search panel exists")
    }

    /// Announces the content index's snapshots (its thread cannot wake
    /// the window under GPUI's test scheduler) and draws.
    fn poll(&mut self) {
        self.vcx.executor().advance_clock(search_panel::RERUN_DELAY);
        let vault = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
        }
        draw(&mut self.vcx);
    }

    /// Waits (in real time: the index has its own thread) for the search
    /// to finish with results.
    fn until_results(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            self.poll();
            let done = self.panel().read_with(
                &self.vcx,
                |p, _| matches!(p.progress(), Progress::Done(s) if s.files_matched > 0),
            );
            if done {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the search found nothing in time");
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(std::path::Path::to_path_buf)
        })
    }
}

#[gpui::test]
fn search_in_vault_shows_the_panel_and_opens_a_result(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::Vault);
    draw(&mut f.vcx);
    f.workspace.read_with(&f.vcx, |ws, _| {
        assert_eq!(ws.sidebar_panel(), SidebarPanel::Search);
        assert!(ws.panels_open().0);
    });
    let panel = f.panel();
    assert!(
        f.vcx
            .update(|window, cx| panel.read(cx).focus_handle(cx).is_focused(window))
    );
    assert!(f.vcx.debug_bounds("sidebar-panel:search").is_some());

    f.vcx.simulate_input("needle");
    draw(&mut f.vcx);
    f.until_results();
    assert_eq!(
        panel.read_with(&f.vcx, |p, _| p.summary()),
        "2 results in 2 files"
    );
    // Enter opens the first match in the tab, scrolled there.
    f.vcx.dispatch_action(search_panel::OpenSelected);
    draw(&mut f.vcx);
    let opened = f.active_path().expect("a file");
    assert!(
        opened.ends_with("B.md") || opened.ends_with("C.md"),
        "{opened:?}"
    );
    f.workspace
        .read_with(&f.vcx, |ws, _| assert_eq!(ws.tabs().len(), 1));

    // Mod+Enter: a new tab.
    f.vcx
        .update(|window, cx| panel.update(cx, |p, cx| p.focus_field(window, cx)));
    f.vcx.dispatch_action(search_panel::SelectNext);
    f.vcx.dispatch_action(search_panel::OpenSelectedInNewTab);
    draw(&mut f.vcx);
    f.workspace
        .read_with(&f.vcx, |ws, _| assert_eq!(ws.tabs().len(), 2));

    // Esc gives the keyboard back to the document.
    f.vcx
        .update(|window, cx| panel.update(cx, |p, cx| p.focus_field(window, cx)));
    f.vcx.dispatch_action(search_panel::Dismiss);
    draw(&mut f.vcx);
    let reader = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("reader");
    assert!(
        f.vcx
            .update(|window, cx| reader.read(cx).focus_handle(cx).is_focused(window))
    );
    drop(f.dir);
}

#[gpui::test]
fn the_switcher_button_shows_the_search_panel(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(crate::ToggleSidebar);
    draw(&mut f.vcx);
    let at = f
        .vcx
        .debug_bounds("panel-switch:search")
        .expect("the switcher's Search button")
        .center();
    f.vcx.simulate_click(at, Modifiers::none());
    draw(&mut f.vcx);
    f.workspace.read_with(&f.vcx, |ws, _| {
        assert_eq!(ws.sidebar_panel(), SidebarPanel::Search);
        assert!(ws.panels.search.is_some());
    });
    assert!(f.vcx.debug_bounds("sidebar-panel:search").is_some());
    // The query typed later searches the vault once it is indexed.
    let panel = f.panel();
    f.vcx
        .update(|window, cx| panel.update(cx, |p, cx| p.set_query("needle", window, cx)));
    f.until_results();
    assert!(f.vcx.debug_bounds("search-line-0-0").is_some());
}

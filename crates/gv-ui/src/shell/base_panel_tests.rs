// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The sidebar base in the right panel, in headless GPUI windows.

use std::path::Path;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};

use super::{OpenTarget, Workspace};
use crate::bases_view::{BasesEvent, BasesPanel};
use crate::chrome::WindowChrome;
use crate::keymap::actions::workspace::ShowBase;
use crate::reader::Disposition;
use crate::{AppSettings, ThemeChoice};

const ALL: &str = "filters: 'file.ext == \"md\"'\nviews:\n  - type: table\n    name: All\n";
const ONLY_A: &str = "filters: 'file.name == \"a.md\"'\nviews:\n  - type: table\n    name: A\n";

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// Announces the content index's snapshots, then draws.
fn poll(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) {
    let vault = workspace.read_with(vcx, |ws, _| ws.vault.clone());
    if let Some(vault) = vault {
        vault.update(vcx, |v, cx| v.poll_content(cx));
    }
    draw(vcx);
}

/// Polls (in real time: the indexes have their own threads) until `f`
/// holds.
fn until(
    what: &str,
    workspace: &Entity<Workspace>,
    vcx: &mut VisualTestContext,
    f: impl Fn(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        poll(workspace, vcx);
        if f(vcx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{what} did not happen in time");
}

fn matched(panel: &Entity<BasesPanel>, vcx: &mut VisualTestContext) -> Option<usize> {
    panel.read_with(vcx, |p, cx| {
        p.view()
            .and_then(|v| v.read(cx).result().map(|r| r.matched))
    })
}

fn active_name(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> String {
    workspace.read_with(vcx, |ws, _| {
        ws.active_tab()
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

#[gpui::test]
fn show_base_puts_a_base_in_the_right_panel_that_follows_the_active_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    // A vault: indexed without asking.
    std::fs::create_dir_all(dir.path().join(".obsidian")).expect("marker");
    std::fs::write(dir.path().join(".obsidian/app.json"), "{}").expect("app.json");
    for (name, text) in [
        ("books.base", ALL),
        ("a.md", "# A\n"),
        ("b.md", "# B\n"),
        ("x.png", "png"),
    ] {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    assert!(workspace.read_with(&vcx, |ws, _| ws.base_panel().is_none()));

    vcx.dispatch_action(ShowBase);
    draw(&mut vcx);
    let panel = workspace
        .read_with(&vcx, |ws, _| {
            assert!(ws.panel_open, "the right panel shows");
            ws.base_panel()
        })
        .expect("the panel is made");
    assert!(vcx.debug_bounds("right-panel-base").is_some(), "drawn");
    // The bases of the vault are offered once the index is ready.
    until("the bases offered", &workspace, &mut vcx, |vcx| {
        panel.read_with(vcx, |p, _| p.bases().len() == 1)
    });
    assert_eq!(
        panel
            .read_with(&vcx, |p, _| p.active().map(str::to_owned))
            .as_deref(),
        Some("a.md")
    );

    panel.update(&mut vcx, |p, cx| p.choose_base("books.base".into(), cx));
    until("the base's files", &workspace, &mut vcx, |vcx| {
        matched(&panel, vcx) == Some(2)
    });

    // `this` follows the active tab.
    let b = dir.path().join("b.md");
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| ws.open_tab(OpenTarget::new(&b), window, cx));
    });
    draw(&mut vcx);
    assert_eq!(
        panel
            .read_with(&vcx, |p, _| p.active().map(str::to_owned))
            .as_deref(),
        Some("b.md")
    );
    let this = panel.read_with(&vcx, |p, cx| {
        p.view().and_then(|v| v.read(cx).this().map(str::to_owned))
    });
    assert_eq!(this.as_deref(), Some("b.md"));

    // Its files open from the active tab, like a base tab's.
    panel.update(&mut vcx, |_, cx| {
        cx.emit(BasesEvent::OpenFile {
            path: dir.path().join("a.md"),
            fragment: None,
            disposition: Disposition::InPlace,
        });
    });
    draw(&mut vcx);
    assert_eq!(active_name(&workspace, &mut vcx), "a.md");

    // The base file changes on disk: it is read again.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(dir.path().join("books.base"), ONLY_A).expect("write");
    // As the vault's watcher reports it (no watcher thread runs under the
    // test scheduler).
    let root = dir.path().to_path_buf();
    workspace.update(&mut vcx, |ws, cx| {
        let path = gv_vault::VaultPath::new("books.base").expect("path");
        ws.publish_changes(&super::vault_ops::upserts(&root, &path), cx);
    });
    until("the changed base", &workspace, &mut vcx, |vcx| {
        matched(&panel, vcx) == Some(1)
    });
}

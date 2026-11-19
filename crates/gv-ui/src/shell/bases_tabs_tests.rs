// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Base files in tabs, in headless GPUI windows.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_model::LinkKind;
use gv_vault::files::{DiskState, ExternalChange};
use gv_vault::{IndexConfig, PathIndex};

use super::{OpenTarget, ViewMode, Workspace};
use crate::bases_view::{BasesEvent, BasesStatus, BasesView};
use crate::chrome::WindowChrome;
use crate::reader::{Disposition, LinkRequest, ReaderEvent};
use crate::{AppSettings, ThemeChoice};

const BASE: &str = "filters: 'file.ext == \"md\"'\nviews:\n  - type: table\n    name: All\n  - type: list\n    name: Other\n";

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    dir
}

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

/// Announces the content index's snapshots (the test scheduler does not
/// let the index's thread wake the window), then draws.
fn poll(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) {
    let vault = workspace.read_with(vcx, |ws, _| ws.vault.clone());
    if let Some(vault) = vault {
        vault.update(vcx, |v, cx| v.poll_content(cx));
    }
    draw(vcx);
}

fn base(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Entity<BasesView> {
    workspace
        .read_with(vcx, |ws, _| ws.active_tab().bases().cloned())
        .expect("the active tab has a Bases view")
}

fn file_of(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> String {
    workspace.read_with(vcx, |ws, _| {
        ws.active_tab()
            .path()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

fn matched(base: &Entity<BasesView>, vcx: &mut VisualTestContext) -> Option<usize> {
    base.read_with(vcx, |b, _| b.result().map(|r| r.matched))
}

/// Waits (in real time: the indexes have their own threads) until the
/// base shows `n` matched files.
fn until_matched(
    workspace: &Entity<Workspace>,
    base: &Entity<BasesView>,
    n: usize,
    vcx: &mut VisualTestContext,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        poll(workspace, vcx);
        if matched(base, vcx) == Some(n) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "the base did not list {n} files in time: {:?}",
        matched(base, vcx)
    );
}

#[gpui::test]
fn a_base_file_opens_in_the_bases_view_read_only(cx: &mut TestAppContext) {
    let dir = temp(&[("books.base", BASE), ("a.md", "# A\n"), ("b.md", "# B\n")]);
    let (window, workspace) = open(cx, &dir.path().join("books.base"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let view = base(&workspace, &mut vcx);
    assert_eq!(
        view.read_with(&vcx, |b, _| b.status().clone()),
        BasesStatus::Ready
    );
    workspace.read_with(&vcx, |ws, cx| {
        assert!(ws.active_tab().reader().is_none());
        assert!(ws.active_tab().canvas().is_none());
        assert!(!ws.mode_available(ViewMode::Edit, cx));
        assert!(!ws.mode_available(ViewMode::Split, cx));
        // The base needs the vault's files: the window made its vault.
        assert!(ws.vault.is_some());
    });
    vcx.update(|_, cx| workspace.update(cx, |ws, cx| ws.toggle_edit_mode(cx)));
    draw(&mut vcx);
    workspace.read_with(&vcx, |ws, _| {
        assert_eq!(ws.active_tab().mode(), ViewMode::Preview);
        assert!(ws.active_tab().editor().is_none());
    });
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| ws.focus_document(window, cx));
        assert!(view.read(cx).focus_handle(cx).is_focused(window));
    });
    // The vault's content index gives the base its files.
    until_matched(&workspace, &view, 2, &mut vcx);
    // A new note: the next snapshot reaches the base.
    std::fs::write(dir.path().join("c.md"), "# C\n").expect("write");
    let vault = workspace
        .read_with(&vcx, |ws, _| ws.vault.clone())
        .expect("vault");
    let index = PathIndex::build(dir.path(), IndexConfig::default())
        .expect("index")
        .0;
    vault.update(&mut vcx, |v, cx| v.publish(index, cx));
    until_matched(&workspace, &view, 3, &mut vcx);
}

#[gpui::test]
fn a_file_value_opens_its_note_and_back_returns_to_the_base(cx: &mut TestAppContext) {
    let dir = temp(&[("books.base", BASE), ("a.md", "# A\n\nText.\n")]);
    let (window, workspace) = open(cx, &dir.path().join("books.base"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let view = base(&workspace, &mut vcx);
    let note = dir.path().join("a.md");
    view.update(&mut vcx, |_, cx| {
        cx.emit(BasesEvent::OpenFile {
            path: note.clone(),
            fragment: None,
            disposition: Disposition::InPlace,
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    workspace.read_with(&vcx, |ws, _| {
        assert!(ws.active_tab().reader().is_some());
        assert!(ws.active_tab().bases().is_none());
    });
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_back(window, cx)));
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "books.base");
    base(&workspace, &mut vcx);
    workspace.read_with(&vcx, |ws, _| assert!(ws.active_tab().reader().is_none()));

    // A link value: followed as a link of the base file.
    let view = base(&workspace, &mut vcx);
    view.update(&mut vcx, |_, cx| {
        cx.emit(BasesEvent::FollowLink(LinkRequest {
            destination: Arc::from("a"),
            kind: LinkKind::Wikilink,
            disposition: Disposition::NewTab,
            range: None,
            from: None,
            at: None,
        }));
    });
    draw(&mut vcx);
    workspace.read_with(&vcx, |ws, _| assert_eq!(ws.tabs().len(), 2));
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
}

#[gpui::test]
fn a_link_or_file_open_to_a_base_opens_it_in_a_tab(cx: &mut TestAppContext) {
    let dir = temp(&[("books.base", BASE), ("a.md", "# A\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let reader = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("reader");
    reader.update(&mut vcx, |_, cx| {
        cx.emit(ReaderEvent::FollowLink(LinkRequest {
            destination: Arc::from("books.base"),
            kind: LinkKind::Inline,
            disposition: Disposition::InPlace,
            range: None,
            from: None,
            at: None,
        }));
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "books.base");
    base(&workspace, &mut vcx);
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_any_file(dir.path().join("a.md"), window, cx);
            ws.open_any_file(dir.path().join("books.base"), window, cx);
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "books.base");
    workspace.read_with(&vcx, |ws, _| {
        assert!(ws.tabs().iter().all(|t| !t.has_question()))
    });
}

#[gpui::test]
fn a_base_changed_on_disk_or_moved_is_shown_again(cx: &mut TestAppContext) {
    let dir = temp(&[("books.base", BASE), ("a.md", "# A\n")]);
    let path = dir.path().join("books.base");
    let (window, workspace) = open(cx, &path);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let view = base(&workspace, &mut vcx);
    let views = |v: &Entity<BasesView>, vcx: &mut VisualTestContext| {
        v.read_with(vcx, |b, _| b.base().map_or(0, |b| b.base().views.len()))
    };
    assert_eq!(views(&view, &mut vcx), 2);

    std::fs::write(&path, "views:\n  - type: table\n    name: Only\n").expect("write");
    let (bytes, state) = DiskState::read(&path).expect("state");
    vcx.update(|_, cx| {
        workspace.update(cx, |ws, cx| {
            ws.file_changed(&path, ExternalChange::Modified { state, bytes }, cx);
        });
    });
    draw(&mut vcx);
    assert_eq!(views(&view, &mut vcx), 1);

    let moved = dir.path().join("renamed.base");
    std::fs::rename(&path, &moved).expect("rename");
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.show_in_tab(0, moved.clone(), None, window, cx)
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "renamed.base");
    assert_eq!(base(&workspace, &mut vcx), view);
    assert_eq!(
        view.read_with(&vcx, |b, _| b.path().map(Path::to_path_buf)),
        Some(moved)
    );
    assert_eq!(views(&view, &mut vcx), 1);
}

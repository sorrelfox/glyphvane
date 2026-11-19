// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Canvas files in tabs, in headless GPUI windows.

use std::path::Path;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_model::LinkKind;
use gv_vault::files::{DiskState, ExternalChange};

use super::{OpenTarget, ViewMode, Workspace};
use crate::canvas_view::{CanvasEvent, CanvasStatus, CanvasView};
use crate::chrome::WindowChrome;
use crate::reader::{Disposition, LinkRequest, ReaderEvent};
use crate::{AppSettings, ThemeChoice};

const BOARD: &str = r#"{"nodes":[
  {"id":"a","type":"file","file":"a.md","x":0,"y":0,"width":200,"height":100},
  {"id":"t","type":"text","text":"Hello","x":300,"y":0,"width":200,"height":100}
]}"#;

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

fn canvas(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Entity<CanvasView> {
    workspace
        .read_with(vcx, |ws, _| ws.active_tab().canvas().cloned())
        .expect("the active tab has a Canvas view")
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

fn nodes(canvas: &Entity<CanvasView>, vcx: &mut VisualTestContext) -> usize {
    canvas.read_with(vcx, |c, _| c.canvas().map_or(0, |c| c.nodes().len()))
}

#[gpui::test]
fn a_canvas_file_opens_in_the_canvas_view_read_only(cx: &mut TestAppContext) {
    let dir = temp(&[("board.canvas", BOARD), ("a.md", "# A\n")]);
    let (window, workspace) = open(cx, &dir.path().join("board.canvas"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let canvas = canvas(&workspace, &mut vcx);
    assert_eq!(
        canvas.read_with(&vcx, |c, _| c.status().clone()),
        CanvasStatus::Ready
    );
    assert_eq!(nodes(&canvas, &mut vcx), 2);
    workspace.read_with(&vcx, |ws, cx| {
        assert!(ws.active_tab().reader().is_none());
        assert!(!ws.mode_available(ViewMode::Edit, cx));
        assert!(!ws.mode_available(ViewMode::Split, cx));
    });
    // Mod+E does nothing: the tab stays in Preview with its Canvas view.
    vcx.update(|_, cx| workspace.update(cx, |ws, cx| ws.toggle_edit_mode(cx)));
    draw(&mut vcx);
    workspace.read_with(&vcx, |ws, _| {
        assert_eq!(ws.active_tab().mode(), ViewMode::Preview);
        assert!(ws.active_tab().editor().is_none());
    });
    // The keyboard goes to the Canvas view like to a Reading view.
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| ws.focus_document(window, cx));
        assert!(canvas.read(cx).focus_handle(cx).is_focused(window));
    });
}

#[gpui::test]
fn a_file_card_opens_its_note_and_back_returns_to_the_canvas(cx: &mut TestAppContext) {
    let dir = temp(&[("board.canvas", BOARD), ("a.md", "# A\n\nText.\n")]);
    let (window, workspace) = open(cx, &dir.path().join("board.canvas"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let board = canvas(&workspace, &mut vcx);
    let note = dir.path().join("a.md");
    board.update(&mut vcx, |_, cx| {
        cx.emit(CanvasEvent::OpenFile {
            path: note.clone(),
            fragment: None,
            disposition: Disposition::InPlace,
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
    workspace.read_with(&vcx, |ws, _| {
        assert!(ws.active_tab().reader().is_some());
        assert!(ws.active_tab().canvas().is_none());
    });
    vcx.update(|window, cx| workspace.update(cx, |ws, cx| ws.go_back(window, cx)));
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "board.canvas");
    let board = canvas(&workspace, &mut vcx);
    assert_eq!(nodes(&board, &mut vcx), 2);
    workspace.read_with(&vcx, |ws, _| assert!(ws.active_tab().reader().is_none()));

    // Mod+Enter on a card: a new tab.
    board.update(&mut vcx, |_, cx| {
        cx.emit(CanvasEvent::OpenFile {
            path: note.clone(),
            fragment: None,
            disposition: Disposition::NewTab,
        });
    });
    draw(&mut vcx);
    workspace.read_with(&vcx, |ws, _| assert_eq!(ws.tabs().len(), 2));
    assert_eq!(file_of(&workspace, &mut vcx), "a.md");
}

#[gpui::test]
fn a_link_to_a_canvas_opens_it_in_a_tab(cx: &mut TestAppContext) {
    let dir = temp(&[("board.canvas", BOARD), ("a.md", "# A\n")]);
    let (window, workspace) = open(cx, &dir.path().join("a.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let reader = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("reader");
    reader.update(&mut vcx, |_, cx| {
        cx.emit(ReaderEvent::FollowLink(LinkRequest {
            destination: Arc::from("board.canvas"),
            kind: LinkKind::Inline,
            disposition: Disposition::InPlace,
            range: None,
            from: None,
        }));
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "board.canvas");
    let board = canvas(&workspace, &mut vcx);
    assert_eq!(nodes(&board, &mut vcx), 2);
    // File ▸ Open and the Files panel take the same way.
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_any_file(dir.path().join("a.md"), window, cx);
            ws.open_any_file(dir.path().join("board.canvas"), window, cx);
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "board.canvas");
    workspace.read_with(&vcx, |ws, _| {
        assert!(ws.tabs().iter().all(|t| !t.has_question()))
    });
}

#[gpui::test]
fn a_canvas_changed_on_disk_or_moved_is_shown_again(cx: &mut TestAppContext) {
    let dir = temp(&[("board.canvas", BOARD), ("a.md", "# A\n")]);
    let path = dir.path().join("board.canvas");
    let (window, workspace) = open(cx, &path);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let board = canvas(&workspace, &mut vcx);
    assert_eq!(nodes(&board, &mut vcx), 2);

    let one =
        r#"{"nodes":[{"id":"t","type":"text","text":"Only","x":0,"y":0,"width":10,"height":10}]}"#;
    std::fs::write(&path, one).expect("write");
    let (bytes, state) = DiskState::read(&path).expect("state");
    vcx.update(|_, cx| {
        workspace.update(cx, |ws, cx| {
            ws.file_changed(&path, ExternalChange::Modified { state, bytes }, cx);
        });
    });
    draw(&mut vcx);
    assert_eq!(nodes(&board, &mut vcx), 1);

    // A rename: the tab follows with the same Canvas view.
    let moved = dir.path().join("renamed.canvas");
    std::fs::rename(&path, &moved).expect("rename");
    vcx.update(|window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.show_in_tab(0, moved.clone(), None, window, cx)
        });
    });
    draw(&mut vcx);
    assert_eq!(file_of(&workspace, &mut vcx), "renamed.canvas");
    let same = canvas(&workspace, &mut vcx);
    assert_eq!(same, board);
    assert_eq!(
        board.read_with(&vcx, |c, _| c.path().map(Path::to_path_buf)),
        Some(moved)
    );
    assert_eq!(nodes(&board, &mut vcx), 1);
}

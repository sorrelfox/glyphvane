// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Pane cycling (F6, Shift+F6) and Go to heading in the Reading view, in
//! headless windows.

use gpui::{
    AppContext as _, Bounds, Entity, FocusHandle, Focusable as _, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};

use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::goto::goto_of;
use crate::{AppSettings, ThemeChoice};

const TEXT: &str = "# Alpha\n\ntext\n\n## Beta\n\nmore\n";

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

fn open(cx: &mut TestAppContext) -> (tempfile::TempDir, Entity<Workspace>, VisualTestContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("a.md");
    std::fs::write(&file, TEXT).expect("write");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            let target = Some(OpenTarget::new(&file));
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    (dir, workspace, vcx)
}

/// The focus handles of the Files panel, the Reading view, the Source view
/// and the Split view's preview, those that exist.
struct Handles {
    files: Option<FocusHandle>,
    reader: Option<FocusHandle>,
    editor: Option<FocusHandle>,
    split_reader: Option<FocusHandle>,
}

fn handles(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) -> Handles {
    vcx.update(|_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.active_tab();
        Handles {
            files: ws.files.as_ref().map(|f| f.read(cx).focus_handle(cx)),
            reader: tab.reader().map(|r| r.read(cx).focus_handle(cx)),
            editor: tab.editor().map(|e| e.read(cx).focus_handle(cx)),
            split_reader: tab.split_reader(cx).map(|r| r.read(cx).focus_handle(cx)),
        }
    })
}

fn focused(handle: Option<&FocusHandle>, vcx: &mut VisualTestContext) -> bool {
    let handle = handle.expect("the pane exists").clone();
    vcx.update(|window, cx| handle.contains_focused(window, cx))
}

fn keys(vcx: &mut VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    draw(vcx);
}

#[gpui::test]
fn f6_cycles_the_panes_both_ways(cx: &mut TestAppContext) {
    let (_dir, workspace, mut vcx) = open(cx);
    // Only the document: F6 keeps it.
    keys(&mut vcx, "f6");
    let h = handles(&workspace, &mut vcx);
    assert!(focused(h.reader.as_ref(), &mut vcx));
    // With the sidebar: the Files panel, then the document, and around.
    workspace.update_in(&mut vcx, |ws, window, cx| {
        ws.set_sidebar_open(true, window, cx)
    });
    draw(&mut vcx);
    let h = handles(&workspace, &mut vcx);
    keys(&mut vcx, "f6");
    assert!(focused(h.files.as_ref(), &mut vcx));
    keys(&mut vcx, "f6");
    assert!(focused(h.reader.as_ref(), &mut vcx));
    keys(&mut vcx, "shift-f6");
    assert!(focused(h.files.as_ref(), &mut vcx));
    keys(&mut vcx, "shift-f6");
    assert!(focused(h.reader.as_ref(), &mut vcx));
    // Split: Files, Source, preview.
    workspace.update(&mut vcx, |ws, cx| ws.set_mode(ViewMode::Split, cx));
    draw(&mut vcx);
    let h = handles(&workspace, &mut vcx);
    let panes = workspace.update(&mut vcx, |ws, cx| ws.panes(cx).len());
    assert_eq!(panes, 3);
    keys(&mut vcx, "f6");
    let first = [h.files.as_ref(), h.editor.as_ref(), h.split_reader.as_ref()]
        .into_iter()
        .position(|handle| focused(handle, &mut vcx));
    let mut at = first.expect("a pane has the keyboard");
    for _ in 0..4 {
        keys(&mut vcx, "f6");
        at = (at + 1) % 3;
        let handle = [h.files.as_ref(), h.editor.as_ref(), h.split_reader.as_ref()][at];
        assert!(focused(handle, &mut vcx), "pane {at}");
    }
}

#[gpui::test]
fn go_to_heading_in_the_reading_view(cx: &mut TestAppContext) {
    let (_dir, workspace, mut vcx) = open(cx);
    let h = handles(&workspace, &mut vcx);
    assert!(focused(h.reader.as_ref(), &mut vcx));
    keys(&mut vcx, "secondary-shift-o");
    let popover = vcx
        .update(|window, cx| goto_of(window, cx))
        .expect("a Go popover");
    let listed = popover.read_with(&vcx, |p, _| p.listed());
    assert_eq!(listed, vec!["Alpha", "Beta"]);
    vcx.simulate_input("bet");
    draw(&mut vcx);
    keys(&mut vcx, "enter");
    assert!(!popover.read_with(&vcx, |p, _| p.is_open()));
    assert!(focused(h.reader.as_ref(), &mut vcx));
    // The jump is in the history: Back returns.
    let back = workspace.read_with(&vcx, |ws, _| ws.active_tab().history().len());
    assert_eq!(back, 1);
}

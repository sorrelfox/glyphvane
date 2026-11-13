// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use std::path::Path;

use gpui::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, WindowOptions};

use super::{FileLabel, ViewMode, Workspace};
use crate::{Appearance, ThemeChoice, ToggleEditMode, TogglePanel, ToggleSidebar};

fn open_file_window(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, ThemeChoice::Light);
        let file = FileLabel::new(Some(Path::new("notes/README.md")));
        gpui::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(file, window, cx))
        })
        .expect("test window opens")
    })
}

/// (sidebar open, right panel open, mode)
fn layout(workspace: &Entity<Workspace>, cx: &TestAppContext) -> (bool, bool, ViewMode) {
    workspace.read_with(cx, |ws, _| (ws.sidebar_open, ws.panel_open, ws.mode))
}

#[gpui::test]
fn file_window_opens_in_preview_with_panels_hidden(cx: &mut TestAppContext) {
    let (_, workspace) = open_file_window(cx);
    assert_eq!(layout(&workspace, cx), (false, false, ViewMode::Preview));
}

#[gpui::test]
fn only_the_toggles_show_panels(cx: &mut TestAppContext) {
    let (window, workspace) = open_file_window(cx);

    cx.dispatch_action(window, ToggleEditMode);
    assert_eq!(layout(&workspace, cx), (false, false, ViewMode::Edit));

    cx.dispatch_action(window, ToggleSidebar);
    assert_eq!(layout(&workspace, cx), (true, false, ViewMode::Edit));

    cx.dispatch_action(window, TogglePanel);
    cx.dispatch_action(window, ToggleEditMode);
    assert_eq!(layout(&workspace, cx), (true, true, ViewMode::Preview));

    cx.dispatch_action(window, ToggleSidebar);
    cx.dispatch_action(window, TogglePanel);
    assert_eq!(layout(&workspace, cx), (false, false, ViewMode::Preview));
}

#[gpui::test]
fn key_chord_cycles_the_theme(cx: &mut TestAppContext) {
    let (window, _) = open_file_window(cx);
    let mut seen = Vec::new();
    for _ in 0..ThemeChoice::ALL.len() {
        cx.simulate_keystrokes(window, "secondary-k secondary-t");
        seen.push(cx.update(|cx| Appearance::choice(cx)));
    }
    assert_eq!(
        seen,
        [
            ThemeChoice::SemiLight,
            ThemeChoice::Dark,
            ThemeChoice::System,
            ThemeChoice::Light
        ]
    );
}

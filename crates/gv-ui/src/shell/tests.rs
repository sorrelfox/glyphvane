// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use std::path::Path;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Pixels, TestAppContext, VisualTestContext,
    WindowOptions,
};

use super::{FileLabel, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::metrics::size::{TITLE_ROW, icon_button};
use crate::metrics::spacing::{macos_controls, title_row, windows_controls};
use crate::metrics::{HAIRLINE, size};
use crate::{Appearance, ThemeChoice, ToggleEditMode, TogglePanel, ToggleSidebar};

fn open_file_window(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
    open_window_with_chrome(cx, WindowChrome::current())
}

fn open_window_with_chrome(
    cx: &mut TestAppContext,
    chrome: WindowChrome,
) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, ThemeChoice::Light);
        let file = FileLabel::new(Some(Path::new("notes/README.md")));
        gpui::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(file, chrome, window, cx))
        })
        .expect("test window opens")
    })
}

/// Draws the window and returns a context to look up element bounds in.
fn drawn(window: AnyWindowHandle, cx: &mut TestAppContext) -> VisualTestContext {
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    vcx
}

fn bounds(vcx: &mut VisualTestContext, selector: &'static str) -> Option<Bounds<Pixels>> {
    vcx.debug_bounds(selector)
}

fn px_eq(value: Pixels, expected: f32) -> bool {
    (f32::from(value) - expected).abs() < 0.01
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

#[gpui::test]
fn windows_caption_buttons_fill_the_right_of_the_title_row(cx: &mut TestAppContext) {
    let (window, _) = open_window_with_chrome(cx, WindowChrome::Windows);
    let mut vcx = drawn(window, cx);
    let row = bounds(&mut vcx, "title-row").expect("title row is drawn");
    assert!(px_eq(row.origin.y, 0.0) && px_eq(row.size.height, TITLE_ROW));

    let ids = ["caption-minimize", "caption-maximize", "caption-close"];
    let buttons: Vec<_> = ids
        .iter()
        .map(|id| bounds(&mut vcx, id).unwrap_or_else(|| panic!("{id} is drawn")))
        .collect();
    for (id, button) in ids.iter().zip(&buttons) {
        assert!(
            px_eq(button.size.width, windows_controls::WIDTH),
            "{id}: {button:?}"
        );
        // Full height down to the row's bottom border.
        assert!(px_eq(button.origin.y, 0.0), "{id}: {button:?}");
        assert!(
            px_eq(button.size.height, TITLE_ROW - HAIRLINE),
            "{id}: {button:?}"
        );
    }
    for pair in buttons.windows(2) {
        assert!(px_eq(pair[0].right(), f32::from(pair[1].origin.x)));
    }
    assert!(px_eq(buttons[2].right(), f32::from(row.right())));

    let toggle = bounds(&mut vcx, "toggle-panel").expect("panel toggle is drawn");
    let spacing = windows_controls::MARGIN_LEFT + title_row::GAP;
    assert!(px_eq(buttons[0].origin.x - toggle.right(), spacing));
}

#[gpui::test]
fn the_menu_button_follows_the_sidebar(cx: &mut TestAppContext) {
    for chrome in [WindowChrome::Windows, WindowChrome::Linux] {
        let (window, _) = open_window_with_chrome(cx, chrome);

        // Sidebar hidden: the menu button starts the title row.
        let mut vcx = drawn(window, cx);
        let menu = bounds(&mut vcx, "app-menu").expect("menu button is drawn");
        let toggle = bounds(&mut vcx, "toggle-sidebar").expect("sidebar toggle is drawn");
        assert!(
            px_eq(menu.origin.x, title_row::PAD_X),
            "{chrome:?}: {menu:?}"
        );
        assert!(px_eq(menu.size.width, icon_button::MENU_WIDTH));
        assert!(px_eq(menu.size.height, icon_button::HEIGHT));
        assert!(px_eq(menu.center().y, TITLE_ROW / 2.0 - HAIRLINE / 2.0));
        assert!(toggle.origin.x > menu.right());

        // Sidebar shown: it moves to the left of the sidebar head.
        cx.dispatch_action(window, ToggleSidebar);
        let mut vcx = drawn(window, cx);
        let menu = bounds(&mut vcx, "app-menu").expect("menu button is drawn");
        let sidebar = bounds(&mut vcx, "sidebar").expect("sidebar is drawn");
        let toggle = bounds(&mut vcx, "toggle-sidebar").expect("sidebar toggle is drawn");
        assert!(sidebar.contains(&menu.center()), "{chrome:?}: {menu:?}");
        assert!(px_eq(menu.origin.x, title_row::PAD_X));
        assert!(px_eq(
            toggle.origin.x,
            size::SIDEBAR_WIDTH + title_row::PAD_X
        ));
    }
}

#[gpui::test]
fn macos_keeps_room_for_the_traffic_lights(cx: &mut TestAppContext) {
    let (window, _) = open_window_with_chrome(cx, WindowChrome::MacOs);
    let lights_end = macos_controls::INSET_LEFT + macos_controls::WIDTH;

    let mut vcx = drawn(window, cx);
    assert!(bounds(&mut vcx, "app-menu").is_none());
    assert!(bounds(&mut vcx, "caption-close").is_none());
    let slot = bounds(&mut vcx, "title-window-controls").expect("title row keeps room");
    let toggle = bounds(&mut vcx, "toggle-sidebar").expect("sidebar toggle is drawn");
    assert!(px_eq(slot.origin.x, title_row::PAD_X));
    assert!(px_eq(
        slot.right(),
        lights_end + macos_controls::MARGIN_RIGHT
    ));
    assert!(px_eq(
        toggle.origin.x,
        f32::from(slot.right()) + title_row::GAP
    ));

    cx.dispatch_action(window, ToggleSidebar);
    let mut vcx = drawn(window, cx);
    assert!(bounds(&mut vcx, "title-window-controls").is_none());
    let slot = bounds(&mut vcx, "sidebar-window-controls").expect("sidebar head keeps room");
    assert!(px_eq(slot.right(), lights_end));
    assert!(px_eq(slot.size.height, TITLE_ROW));
}

#[gpui::test]
fn linux_draws_no_windows_or_macos_controls(cx: &mut TestAppContext) {
    let (window, _) = open_window_with_chrome(cx, WindowChrome::Linux);
    let mut vcx = drawn(window, cx);
    for id in ["caption-close", "title-window-controls"] {
        assert!(bounds(&mut vcx, id).is_none(), "{id} is drawn on Linux");
    }
}

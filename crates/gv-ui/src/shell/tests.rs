// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use gpui::App;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Pixels, TestAppContext, VisualTestContext,
    WindowOptions,
};

use super::{OpenTarget, TextPosition, ViewMode, Workspace, most_recent_window};
use crate::chrome::WindowChrome;
use crate::metrics::size::{TITLE_ROW, icon_button};
use crate::metrics::spacing::{macos_controls, title_row, windows_controls};
use crate::metrics::{HAIRLINE, size};
use crate::{
    AppSettings, Appearance, Placement, ThemeChoice, ToggleEditMode, TogglePanel, ToggleSidebar,
};

fn open_file_window(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
    open_window_with_chrome(cx, WindowChrome::current())
}

fn open_window_with_chrome(
    cx: &mut TestAppContext,
    chrome: WindowChrome,
) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let file = Some(OpenTarget::new("notes/README.md"));
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
    workspace.read_with(cx, |ws, _| (ws.sidebar_open, ws.panel_open, ws.mode()))
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

/// Opens `paths` as the command line would and returns the window and its
/// workspace.
fn route(
    cx: &mut TestAppContext,
    targets: Vec<OpenTarget>,
    placement: Placement,
) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx: &mut App| {
        let window = crate::open(cx, targets, placement).expect("files open");
        let recent = most_recent_window(cx).expect("a main window is open");
        assert_eq!(recent.window, window);
        (
            window,
            recent.workspace.upgrade().expect("workspace is alive"),
        )
    })
}

fn tab_paths(workspace: &Entity<Workspace>, cx: &TestAppContext) -> (Vec<String>, usize) {
    workspace.read_with(cx, |ws, _| {
        let paths = ws
            .tabs()
            .iter()
            .map(|t| {
                t.path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            })
            .collect();
        (paths, ws.active)
    })
}

#[gpui::test]
fn a_routed_file_opens_as_a_tab_and_leaves_the_panels_alone(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let (first, workspace) = route(cx, vec![OpenTarget::new("a.md")], Placement::RecentWindow);
    assert_eq!(layout(&workspace, cx), (false, false, ViewMode::Preview));
    cx.dispatch_action(first, ToggleSidebar);

    let line = TextPosition {
        line: 12,
        column: Some(3),
    };
    let (second, _) = route(
        cx,
        vec![OpenTarget::new("b.md").at(line)],
        Placement::RecentWindow,
    );
    assert_eq!(second, first);
    assert_eq!(layout(&workspace, cx), (true, false, ViewMode::Preview));
    assert_eq!(
        tab_paths(&workspace, cx),
        (vec!["a.md".into(), "b.md".into()], 1)
    );
    let position = workspace.read_with(cx, |ws, _| ws.active_tab().position());
    assert_eq!(position, Some(line));
}

#[gpui::test]
fn new_window_follows_the_launch_rule(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let (first, workspace) = route(cx, vec![OpenTarget::new("a.md")], Placement::RecentWindow);
    cx.dispatch_action(first, ToggleSidebar);
    cx.dispatch_action(first, TogglePanel);

    let targets = vec![OpenTarget::new("b.md"), OpenTarget::new("c.md")];
    let (second, opened) = route(cx, targets, Placement::NewWindow);
    assert_ne!(second, first);
    assert_eq!(layout(&opened, cx), (false, false, ViewMode::Preview));
    assert_eq!(
        tab_paths(&opened, cx),
        (vec!["b.md".into(), "c.md".into()], 1)
    );
    // The first window is untouched.
    assert_eq!(layout(&workspace, cx), (true, true, ViewMode::Preview));
    assert_eq!(tab_paths(&workspace, cx), (vec!["a.md".into()], 0));
}

#[gpui::test]
fn a_file_replaces_the_empty_tab(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let (first, workspace) = route(cx, Vec::new(), Placement::RecentWindow);
    assert_eq!(tab_paths(&workspace, cx), (vec![String::new()], 0));

    let (second, _) = route(cx, vec![OpenTarget::new("a.md")], Placement::RecentWindow);
    assert_eq!(second, first);
    assert_eq!(tab_paths(&workspace, cx), (vec!["a.md".into()], 0));
}

#[gpui::test]
fn each_tab_keeps_its_mode(cx: &mut TestAppContext) {
    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let (window, workspace) = route(cx, vec![OpenTarget::new("a.md")], Placement::RecentWindow);
    cx.dispatch_action(window, ToggleEditMode);
    route(cx, vec![OpenTarget::new("b.md")], Placement::RecentWindow);
    assert_eq!(layout(&workspace, cx), (false, false, ViewMode::Preview));
    cx.update_window(window, |_, window, cx| {
        workspace.update(cx, |ws, cx| ws.activate_tab(0, window, cx));
    })
    .expect("window is open");
    assert_eq!(layout(&workspace, cx), (false, false, ViewMode::Edit));
}

#[gpui::test]
fn each_window_has_its_own_vault_and_tabs(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("a.md"), "# A\n").expect("write");
    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let (first, workspace) = route(cx, Vec::new(), Placement::RecentWindow);
    cx.update_window(first, |_, window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_folder(dir.path().to_path_buf(), window, cx);
        });
    })
    .expect("window is open");
    cx.dispatch_action(first, crate::keymap::actions::workspace::NewWindow);
    cx.run_until_parked();
    let windows = cx.update(super::main_windows);
    assert_eq!(windows.len(), 2);
    let second = windows
        .iter()
        .find(|w| w.window != first)
        .and_then(|w| w.workspace.upgrade())
        .expect("a second window");
    let (root, tabs) = second.read_with(cx, |ws, cx| (ws.vault_root(cx), ws.tabs().len()));
    assert_eq!((root, tabs), (None, 1), "the new window starts empty");
    let root = workspace.read_with(cx, |ws, cx| ws.vault_root(cx));
    assert_eq!(root.as_deref(), Some(dir.path()));

    // The macOS Window menu's commands have handlers in every window (the
    // test platform cannot minimize or zoom, so only one runs).
    cx.update_window(first, |_, window, cx| {
        assert!(window.is_action_available(&crate::keymap::actions::window::Minimize, cx));
        assert!(window.is_action_available(&crate::keymap::actions::window::Zoom, cx));
        window.dispatch_action(
            Box::new(crate::keymap::actions::window::BringAllToFront),
            cx,
        );
    })
    .expect("window is open");
    cx.run_until_parked();
    assert_eq!(cx.update(super::main_windows).len(), 2);
}

/// A long detail in the infobar wraps in its own column, clear of the
/// buttons and inside the window, at every width.
#[gpui::test]
fn a_long_infobar_detail_wraps_clear_of_the_buttons(cx: &mut TestAppContext) {
    use gpui::{WindowBounds, point, px, size};
    use gv_vault::files::journal::DiskComparison;
    cx.update(|cx| crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light))));
    let mut notice = crate::files::notice::Notice::recovered(DiskComparison::Changed);
    notice.detail = Some(
        "Edits from the last session were restored from the crash journal. The file also \
         changed on disk since then, so compare both versions before you keep either of them."
            .into(),
    );
    for width in [1200.0, 800.0, 500.0] {
        let (window, workspace) = cx.update(|cx| {
            let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(width), px(400.0)));
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..WindowOptions::default()
            };
            gpui::open_window(options, cx, |window, cx| {
                cx.new(|cx| Workspace::with_chrome(None, WindowChrome::Linux, window, cx))
            })
            .expect("test window opens")
        });
        workspace.update(cx, |ws, cx| {
            let active = ws.active;
            ws.tabs[active].notice = Some(notice.clone());
            cx.notify();
        });
        let mut vcx = drawn(window, cx);
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            workspace.read_with(&vcx, |ws, cx| ws.infobar_notice(cx)),
            Some(notice.clone())
        );
        let controls = [
            "infobar-action-0",
            "infobar-action-1",
            "infobar-action-2",
            "infobar-dismiss",
        ];
        crate::files::notice::assert_clear_of_buttons(&mut vcx, width, "infobar-detail", &controls);
    }
}

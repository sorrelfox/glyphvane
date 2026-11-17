// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Roles, names, states and keyboard reachability of the main window and
//! the settings window, in headless windows.

use gpui::{
    AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};

use super::testing::{focused, has, named, nodes, with_role};
use super::{Live, Role, Toggled};
use crate::chrome::WindowChrome;
use crate::shell::{OpenTarget, Workspace};
use crate::{AppSettings, ThemeChoice};

const TEXT: &str = "# Alpha\n\ntext\n";

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

fn key(vcx: &mut VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    draw(vcx);
}

/// Presses and releases `key` (GPUI clicks a focused control on the
/// release of Enter or Space).
fn press(vcx: &mut VisualTestContext, key: &'static str) {
    vcx.simulate_keystrokes(key);
    vcx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse(key).expect("a key"),
    });
    draw(vcx);
}

#[gpui::test]
fn the_title_row_names_its_controls_and_their_states(cx: &mut TestAppContext) {
    let (_dir, _workspace, mut vcx) = open(cx);

    let toolbar = named(&mut vcx, "Title bar");
    assert_eq!(toolbar.role, Some(Role::Toolbar));

    let sidebar = named(&mut vcx, "Sidebar");
    assert_eq!(sidebar.role, Some(Role::Button));
    assert_eq!(sidebar.toggled, Some(Toggled::False));
    assert!(sidebar.focusable && sidebar.clickable);

    // Nowhere to go back to: still announced, as unavailable.
    let back = named(&mut vcx, "Back");
    assert_eq!(back.role, Some(Role::Button));
    assert!(back.disabled);
    assert!(!back.focusable);

    let tabs = with_role(&mut vcx, Role::Tab);
    let doc_tab = tabs
        .iter()
        .find(|t| t.label.as_deref() == Some("a.md"))
        .expect("the document's tab");
    assert_eq!(doc_tab.selected, Some(true));
    assert_eq!(doc_tab.position, Some((1, 1)));
    assert!(has(&mut vcx, "Close a.md"));
    assert_eq!(named(&mut vcx, "Open tabs").role, Some(Role::TabList));

    // The view mode: one radio button per mode, Preview chosen.
    let modes: Vec<_> = with_role(&mut vcx, Role::RadioButton)
        .into_iter()
        .map(|n| (n.label.unwrap_or_default(), n.toggled))
        .collect();
    assert_eq!(
        modes,
        [
            ("Preview".to_owned(), Some(Toggled::True)),
            ("Edit".to_owned(), Some(Toggled::False)),
            ("Split".to_owned(), Some(Toggled::False)),
        ]
    );
    for label in ["New tab", "More actions", "Right panel"] {
        let n = named(&mut vcx, label);
        assert_eq!(n.role, Some(Role::Button), "{label}");
        assert!(n.focusable, "{label} is a Tab stop");
    }
}

#[gpui::test]
fn the_sidebar_switcher_is_a_tab_list(cx: &mut TestAppContext) {
    let (_dir, _workspace, mut vcx) = open(cx);
    key(&mut vcx, "ctrl-b");
    if !has(&mut vcx, "Sidebar panels") {
        // The sidebar key differs on this platform: use the button.
        let at = named(&mut vcx, "Sidebar").bounds.center();
        vcx.simulate_click(at, gpui::Modifiers::none());
        draw(&mut vcx);
    }
    assert_eq!(
        named(&mut vcx, "Sidebar").toggled,
        Some(Toggled::True),
        "the toggle shows the sidebar is open"
    );
    assert_eq!(named(&mut vcx, "Sidebar panels").role, Some(Role::TabList));
    let switch: Vec<_> = nodes(&mut vcx)
        .into_iter()
        .filter(|n| n.role == Some(Role::Tab) && n.label.as_deref() != Some("a.md"))
        .map(|n| (n.label.unwrap_or_default(), n.selected, n.position))
        .collect();
    assert_eq!(
        switch,
        [
            ("Files".to_owned(), Some(true), Some((1, 4))),
            ("Search".to_owned(), Some(false), Some((2, 4))),
            ("Tags".to_owned(), Some(false), Some((3, 4))),
            ("Bookmarks".to_owned(), Some(false), Some((4, 4))),
        ]
    );
}

#[gpui::test]
fn the_status_bar_says_where_the_reader_is(cx: &mut TestAppContext) {
    let (_dir, _workspace, mut vcx) = open(cx);
    assert_eq!(named(&mut vcx, "Status bar").role, Some(Role::Group));
    let location = named(&mut vcx, "Location");
    assert!(
        location
            .value
            .as_deref()
            .is_some_and(|v| v.contains("a.md")),
        "{location:?}"
    );
    let chip = nodes(&mut vcx)
        .into_iter()
        .find(|n| {
            n.label
                .as_deref()
                .is_some_and(|l| l.starts_with("Markdown profile: "))
        })
        .expect("the profile chip");
    assert_eq!(chip.role, Some(Role::Button));
    assert_eq!(chip.expanded, Some(false));
    assert!(chip.focusable);
}

/// Tab and Shift+Tab reach every control of the title row and the status
/// bar from the document, in order, and come back round: no trap.
#[gpui::test]
fn tab_walks_the_window_controls_and_wraps(cx: &mut TestAppContext) {
    let (_dir, _workspace, mut vcx) = open(cx);
    let stops: Vec<String> = nodes(&mut vcx)
        .into_iter()
        .filter(|n| n.focusable && !n.disabled)
        .filter_map(|n| n.label)
        .collect();
    assert!(stops.len() >= 8, "{stops:?}");

    let mut seen: Vec<String> = Vec::new();
    for _ in 0..stops.len() * 2 + 4 {
        key(&mut vcx, "tab");
        let Some(Some(label)) = focused(&mut vcx) else {
            continue;
        };
        if seen.first() == Some(&label) {
            break;
        }
        seen.push(label);
    }
    for stop in &stops {
        assert!(seen.contains(stop), "Tab never reached {stop:?}: {seen:?}");
    }
    // Shift+Tab goes the other way.
    let here = focused(&mut vcx).flatten();
    key(&mut vcx, "shift-tab");
    let before = focused(&mut vcx).flatten();
    key(&mut vcx, "tab");
    assert_eq!(focused(&mut vcx).flatten(), here);
    assert_ne!(before, here);
}

/// Enter and Space work the focused control.
#[gpui::test]
fn space_presses_the_focused_button(cx: &mut TestAppContext) {
    let (_dir, _workspace, mut vcx) = open(cx);
    for _ in 0..40 {
        key(&mut vcx, "tab");
        if focused(&mut vcx).flatten().as_deref() == Some("Sidebar") {
            break;
        }
    }
    assert_eq!(focused(&mut vcx).flatten().as_deref(), Some("Sidebar"));
    press(&mut vcx, "space");
    assert_eq!(named(&mut vcx, "Sidebar").toggled, Some(Toggled::True));
    // The button keeps the keyboard, and Enter works it too.
    press(&mut vcx, "enter");
    assert_eq!(named(&mut vcx, "Sidebar").toggled, Some(Toggled::False));
}

/// A mouse click on a toolbar button leaves the keyboard with the
/// document.
#[gpui::test]
fn clicking_a_button_keeps_the_document_focus(cx: &mut TestAppContext) {
    let (_dir, workspace, mut vcx) = open(cx);
    let reader = vcx.update(|_, cx| {
        workspace
            .read(cx)
            .active_tab()
            .reader()
            .expect("a reader")
            .clone()
    });
    vcx.update(|window, cx| {
        use gpui::Focusable as _;
        reader.read(cx).focus_handle(cx).focus(window, cx);
    });
    draw(&mut vcx);
    let panel = named(&mut vcx, "Right panel");
    vcx.simulate_click(panel.bounds.center(), gpui::Modifiers::none());
    draw(&mut vcx);
    assert_eq!(named(&mut vcx, "Right panel").toggled, Some(Toggled::True));
    let still = vcx.update(|window, cx| {
        use gpui::Focusable as _;
        reader
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
    });
    assert!(still, "the click left the keyboard in the document");
}

#[gpui::test]
fn an_infobar_is_a_live_region(cx: &mut TestAppContext) {
    let (_dir, workspace, mut vcx) = open(cx);
    vcx.update(|_, cx| {
        workspace.update(cx, |ws, cx| ws.show_test_notice(cx));
    });
    draw(&mut vcx);
    let bar = nodes(&mut vcx)
        .into_iter()
        .find(|n| matches!(n.role, Some(Role::Alert | Role::Status)))
        .expect("the infobar");
    assert!(bar.label.is_some());
    assert!(matches!(bar.live, Some(Live::Polite | Live::Assertive)));
    let dismiss = named(&mut vcx, "Dismiss");
    assert!(dismiss.focusable);
}

#[gpui::test]
fn the_command_palette_is_a_modal_list_box(cx: &mut TestAppContext) {
    let (_dir, _workspace, mut vcx) = open(cx);
    key(&mut vcx, "secondary-shift-p");
    let dialog = named(&mut vcx, "Command palette");
    assert_eq!(dialog.role, Some(Role::Dialog));
    assert!(dialog.modal);
    assert_eq!(named(&mut vcx, "Commands").role, Some(Role::ListBox));
    let options = with_role(&mut vcx, Role::ListBoxOption);
    assert!(options.len() > 1);
    assert_eq!(options[0].selected, Some(true));
    let (_, count) = options[0].position.expect("a position in the list");
    let said = |vcx: &mut VisualTestContext| {
        with_role(vcx, Role::Status)
            .into_iter()
            .find(|n| n.live == Some(Live::Polite))
            .and_then(|n| n.label)
            .unwrap_or_default()
    };
    assert!(said(&mut vcx).ends_with(&format!(", 1 of {count}")));
    key(&mut vcx, "down");
    assert!(said(&mut vcx).ends_with(&format!(", 2 of {count}")));
    key(&mut vcx, "escape");
    assert!(!has(&mut vcx, "Command palette"));
}

#[gpui::test]
fn the_files_tree_gives_levels_and_the_chip_its_reason(cx: &mut TestAppContext) {
    let (_dir, _workspace, mut vcx) = open(cx);
    let chip = nodes(&mut vcx)
        .into_iter()
        .find(|n| {
            n.label
                .as_deref()
                .is_some_and(|l| l.starts_with("Markdown profile: "))
        })
        .expect("the profile chip");
    assert!(chip.description.is_some(), "the detection reason");
    key(&mut vcx, "ctrl-b");
    if let Some(row) = with_role(&mut vcx, Role::TreeItem)
        .into_iter()
        .find(|n| n.label.as_deref() == Some("a.md"))
    {
        assert_eq!(row.level, Some(1));
        assert_eq!(row.expanded, None, "a file has nothing to expand");
    }
}

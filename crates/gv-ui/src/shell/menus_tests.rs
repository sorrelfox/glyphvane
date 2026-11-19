// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Context menus and side panels in headless windows: the tab menu, More
//! actions, right-clicks in the views, the context menu key, keeping a
//! menu inside the window, and the sidebar switcher.

use std::path::{Path, PathBuf};

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, MouseButton,
    Pixels, Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};

use super::panels::SidebarPanel;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::workspace as commands;
use crate::menu::{ContextMenu, show_context_menu};
use crate::metrics::size::MENU_WIDTH;
use crate::metrics::spacing::menu as spacing;
use crate::{AppSettings, ThemeChoice, ToggleEditMode};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

struct Fixture {
    dir: tempfile::TempDir,
    window: AnyWindowHandle,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

/// A window `width` × `height` for `a.md` in a folder that also holds
/// `b.md` and `c.md`.
fn fixture_sized(cx: &mut TestAppContext, text: &str, width: f32, height: f32) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    for name in ["a.md", "b.md", "c.md"] {
        std::fs::write(dir.path().join(name), format!("# {name}\n")).expect("write");
    }
    std::fs::write(dir.path().join("a.md"), text).expect("write");
    let file = dir.path().join("a.md");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(width), px(height)));
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
    Fixture {
        dir,
        window,
        workspace,
        vcx,
    }
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    fixture_sized(cx, "# A\n\nSome text.\n", WIDTH, HEIGHT)
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn bounds(&mut self, selector: &str) -> Option<Bounds<Pixels>> {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        self.vcx.debug_bounds(selector)
    }

    fn has(&mut self, selector: &str) -> bool {
        self.bounds(selector).is_some()
    }

    fn click(&mut self, selector: &str) {
        let at = self
            .bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn right_click_at(&mut self, at: Point<Pixels>) {
        self.vcx
            .simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        self.vcx
            .simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn right_click(&mut self, selector: &str) {
        let at = self
            .bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.right_click_at(at);
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }

    fn dispatch(&mut self, action: impl gpui::Action) {
        self.vcx.dispatch_action(action);
        draw(&mut self.vcx);
    }

    fn menu(&mut self) -> Option<ContextMenu> {
        let popover = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.context_menu().clone());
        popover.read_with(&self.vcx, |p, _| p.kind())
    }

    fn highlighted(&mut self) -> Option<&'static str> {
        let popover = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.context_menu().clone());
        popover.read_with(&self.vcx, |p, _| p.highlighted())
    }

    fn tabs(&mut self) -> Vec<String> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.tabs().iter().map(|t| t.label.name.to_string()).collect()
        })
    }

    fn active(&mut self) -> String {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().label.name.to_string())
    }

    fn open(&mut self, name: &str) {
        let path = self.path(name);
        self.workspace.update_in(&mut self.vcx, |ws, window, cx| {
            ws.open_tab(OpenTarget::new(path), window, cx);
        });
        draw(&mut self.vcx);
    }

    fn reader_focused(&mut self) -> bool {
        let workspace = self.workspace.clone();
        self.vcx.update(|window, cx| {
            workspace
                .read(cx)
                .active_tab()
                .reader()
                .is_some_and(|r| r.read(cx).focus_handle(cx).is_focused(window))
        })
    }

    fn editor_focused(&mut self) -> bool {
        let workspace = self.workspace.clone();
        self.vcx.update(|window, cx| {
            workspace
                .read(cx)
                .active_tab()
                .editor()
                .is_some_and(|e| e.read(cx).focus_handle(cx).is_focused(window))
        })
    }

    /// The highlighted item lies inside the menu, which lies inside the
    /// window with the margin.
    fn assert_highlight_shown(&mut self, height: f32) {
        let menu = self.bounds("context-menu").expect("menu drawn");
        assert!(f32::from(menu.top()) >= spacing::EDGE - 0.5, "{menu:?}");
        assert!(
            f32::from(menu.bottom()) <= height - spacing::EDGE + 0.5,
            "{menu:?}"
        );
        let command = self.highlighted().expect("an item is highlighted");
        let item = self
            .bounds(&format!("context-menu-item:{command}"))
            .expect("item drawn");
        assert!(
            item.top() >= menu.top() && item.bottom() <= menu.bottom(),
            "{command}: {item:?} in {menu:?}"
        );
    }
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

// --- The tab menu ---------------------------------------------------------------

/// A right-click on a tab opens the tab menu at the pointer; its commands
/// act on that tab, not the active one.
#[gpui::test]
fn the_tab_menu_acts_on_the_clicked_tab(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.open("b.md");
    f.open("c.md");
    assert_eq!(f.tabs(), ["a.md", "b.md", "c.md"]);
    assert_eq!(f.active(), "c.md");

    let tab = f.bounds("tab-0").expect("tab drawn");
    f.right_click("tab-0");
    assert_eq!(f.menu(), Some(ContextMenu::Tab));
    let menu = f.bounds("context-menu").expect("menu drawn");
    assert_eq!(menu.size.width, px(MENU_WIDTH));
    assert_eq!(menu.origin.x, tab.center().x);
    assert_eq!(menu.origin.y, tab.center().y);
    // Every item of the tab menu has a handler.
    assert!(!f.has("context-menu-disabled:file.reveal_in_file_manager"));
    assert!(!f.has("context-menu-disabled:tabs.close_others"));

    // Copy path copies a.md's path although c.md is active.
    f.click("context-menu-item:file.copy_path");
    assert_eq!(f.menu(), None);
    let copied = f.vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied, Some(path_text(&f.path("a.md"))));

    // Close to the right of b.md.
    f.right_click("tab-1");
    f.click("context-menu-item:tabs.close_to_right");
    assert_eq!(f.tabs(), ["a.md", "b.md"]);

    // Close on the inactive a.md closes a.md.
    assert_eq!(f.active(), "b.md");
    f.right_click("tab-0");
    f.click("context-menu-item:tabs.close");
    assert_eq!(f.tabs(), ["b.md"]);

    // The keys still close the active tab.
    f.open("a.md");
    f.open("c.md");
    f.right_click("tab-1");
    f.click("context-menu-item:tabs.close_others");
    assert_eq!(f.tabs(), ["a.md"]);
}

/// Arrow keys skip dimmed items and wrap; Enter runs; Esc closes and gives
/// the keyboard back.
#[gpui::test]
fn the_menu_keys_move_run_and_close(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.open("b.md");
    assert!(f.reader_focused());
    f.right_click("tab-0");
    assert_eq!(f.highlighted(), None, "a right-click highlights nothing");
    assert!(!f.reader_focused(), "the menu has the keyboard");
    f.keys("down");
    assert_eq!(f.highlighted(), Some("tabs.close"));
    f.keys("up");
    // Up from the first item wraps to the last.
    assert_eq!(f.highlighted(), Some("file.reveal_in_file_manager"));
    f.keys("home");
    assert_eq!(f.highlighted(), Some("tabs.close"));
    f.keys("end");
    assert_eq!(f.highlighted(), Some("file.reveal_in_file_manager"));
    f.keys("escape");
    assert_eq!(f.menu(), None);
    assert!(f.reader_focused());

    f.right_click("tab-0");
    f.keys("down down");
    assert_eq!(f.highlighted(), Some("tabs.close_others"));
    f.keys("enter");
    assert_eq!(f.menu(), None);
    assert_eq!(f.tabs(), ["a.md"]);

    // A click outside closes without running anything.
    f.right_click("tab-0");
    f.vcx
        .simulate_click(point(px(WIDTH / 2.0), px(HEIGHT / 2.0)), Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.menu(), None);
    assert_eq!(f.tabs(), ["a.md"]);
}

/// An untitled tab has no path: the path items are dimmed.
#[gpui::test]
fn path_items_need_a_file(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dispatch(crate::keymap::actions::tabs::New);
    assert_eq!(f.tabs().len(), 2);
    f.right_click("tab-1");
    assert!(f.has("context-menu-disabled:file.copy_path"));
    assert!(f.has("context-menu-disabled:workspace.reveal_in_tree"));
    assert!(!f.has("context-menu-disabled:tabs.close"));
}

/// Reveal in file tree shows the sidebar's Files panel with the tab's file
/// selected, and activates the tab.
#[gpui::test]
fn reveal_in_file_tree_shows_the_file(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.open("b.md");
    f.right_click("tab-0");
    f.click("context-menu-item:workspace.reveal_in_tree");
    assert_eq!(f.active(), "a.md");
    assert!(f.has("files-panel"));
    let (panel, selected, focused) = {
        let workspace = f.workspace.clone();
        f.vcx.update(|window, cx| {
            let ws = workspace.read(cx);
            let files = ws.files.clone().expect("files panel");
            let files = files.read(cx);
            (
                ws.sidebar_panel(),
                files.selected_name(),
                files.focus_handle(cx).is_focused(window),
            )
        })
    };
    assert_eq!(panel, SidebarPanel::Files);
    assert_eq!(selected.as_deref(), Some("a.md"));
    assert!(focused, "the tree has the keyboard");
}

// --- More actions -----------------------------------------------------------------

/// The More actions button opens its menu below it, right-aligned.
#[gpui::test]
fn more_actions_opens_below_its_button(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let button = f.bounds("more-actions").expect("button drawn");
    f.click("more-actions");
    assert_eq!(f.menu(), Some(ContextMenu::MoreActions));
    let menu = f.bounds("context-menu").expect("menu drawn");
    assert_eq!(menu.right(), button.right());
    assert_eq!(menu.top(), button.bottom() + px(spacing::OFFSET_Y));
    assert!(!f.has("context-menu-disabled:file.copy_path"));
    assert!(!f.has("context-menu-disabled:file.history"));
    // Bookmarks belong to a vault, and this window has none yet.
    assert!(f.has("context-menu-disabled:bookmarks.toggle"));
    f.click("context-menu-item:file.copy_path");
    let copied = f.vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied, Some(path_text(&f.path("a.md"))));
    assert!(f.reader_focused());
}

// --- Right-clicks in the views and the menu key ------------------------------

/// In the Source view a right-click opens the editor's menu, which runs
/// the editor's commands; Shift+F10 opens it at the caret.
#[gpui::test]
fn the_editor_menu_opens_on_right_click_and_shift_f10(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dispatch(ToggleEditMode);
    assert!(f.editor_focused());
    f.right_click_at(point(px(WIDTH / 2.0), px(HEIGHT / 2.0)));
    assert_eq!(f.menu(), Some(ContextMenu::Editor));
    assert!(!f.has("context-menu-disabled:editor.copy"));
    assert!(f.has("context-menu-disabled:spell.ignore"));
    f.keys("escape");
    assert!(f.editor_focused());

    f.keys("shift-f10");
    assert_eq!(f.menu(), Some(ContextMenu::Editor));
    assert_eq!(f.highlighted(), Some("editor.cut"));
    // At the caret, at the start of the document.
    let menu = f.bounds("context-menu").expect("menu drawn");
    assert!(f32::from(menu.top()) < HEIGHT / 3.0, "{menu:?}");
    f.keys("escape");
    assert!(f.editor_focused());
}

/// The editor menu's table items run at the caret's cell, and are
/// disabled outside a table (but Format table, which inserts one).
#[gpui::test]
fn the_editor_menu_runs_table_commands(cx: &mut TestAppContext) {
    let text = "Words.\n\n| a | b |\n|---|---|\n| c | d |\n";
    let mut f = fixture_sized(cx, text, WIDTH, HEIGHT);
    f.dispatch(ToggleEditMode);
    assert!(f.editor_focused());
    f.keys("shift-f10");
    assert_eq!(f.menu(), Some(ContextMenu::Editor));
    assert!(f.has("context-menu-disabled:editor.table_insert_row_below"));
    assert!(!f.has("context-menu-disabled:editor.table"));
    f.keys("escape");
    // In the cell `c`.
    let editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("an editor");
    editor.update(&mut f.vcx, |e, cx| e.set_cursor(30, cx));
    draw(&mut f.vcx);
    f.keys("shift-f10");
    assert_eq!(f.menu(), Some(ContextMenu::Editor));
    assert!(!f.has("context-menu-disabled:editor.table_insert_row_below"));
    f.click("context-menu-item:editor.table_insert_row_below");
    assert_eq!(f.menu(), None);
    let now = editor.read_with(&f.vcx, |e, _| {
        e.editor().map(|e| e.text().to_string()).unwrap_or_default()
    });
    assert_eq!(
        now,
        "Words.\n\n| a   | b   |\n| --- | --- |\n| c   | d   |\n|     |     |\n"
    );
    assert!(f.editor_focused());
}

/// In Preview a right-click on a link opens the link menu; elsewhere it
/// opens nothing. There is no *Open in split* (no pane splits).
#[gpui::test]
fn the_link_menu_opens_on_a_link(cx: &mut TestAppContext) {
    let link = "x".repeat(60);
    let text = format!("[{link}](b.md)\n\nPlain words without a link at all here.\n");
    let mut f = fixture_sized(cx, &text, WIDTH, HEIGHT);
    let mut found = false;
    for y in (40..240).step_by(4) {
        f.right_click_at(point(px(WIDTH / 2.0), px(y as f32)));
        if f.menu().is_some() {
            found = true;
            break;
        }
    }
    assert!(found, "a right-click on the link opens a menu");
    assert_eq!(f.menu(), Some(ContextMenu::Link));
    assert!(f.has("context-menu-item:links.follow"));
    assert!(!f.has("context-menu-disabled:links.follow"));
    assert!(!f.has("context-menu-item:links.open_in_split"));
    f.keys("escape");
    assert!(f.reader_focused());
    // Below the text: no menu.
    f.right_click_at(point(px(WIDTH / 2.0), px(HEIGHT - 60.0)));
    assert_eq!(f.menu(), None);
}

/// The file tree: a right-click selects the row and opens its menu; the
/// menu key opens it at the selected row.
#[gpui::test]
fn the_file_tree_menu(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dispatch(commands::ShowFiles);
    assert!(f.has("file-row-1"));
    f.right_click("file-row-1");
    assert_eq!(f.menu(), Some(ContextMenu::FileTree));
    // The file operations run on the selected row.
    for id in [
        "file.new_folder",
        "file.rename",
        "file.move",
        "file.duplicate",
        "file.delete",
    ] {
        assert!(f.has(&format!("context-menu-item:{id}")), "{id}");
        assert!(!f.has(&format!("context-menu-disabled:{id}")), "{id}");
    }
    let selected = {
        let files = f.workspace.read_with(&f.vcx, |ws, _| ws.files.clone());
        files.and_then(|files| files.read_with(&f.vcx, |p, _| p.selected_name()))
    };
    assert_eq!(selected.as_deref(), Some("b.md"));
    f.keys("escape");
    assert_eq!(f.menu(), None);

    f.keys("shift-f10");
    assert_eq!(f.menu(), Some(ContextMenu::FileTree));
    let row = f.bounds("file-row-1").expect("row drawn");
    let menu = f.bounds("context-menu").expect("menu drawn");
    assert_eq!(menu.origin.y, row.bottom());
}

/// A menu taller than the window keeps a margin at both ends and scrolls to
/// the highlighted item; one near the right edge opens to the left.
#[gpui::test]
fn a_menu_stays_inside_a_short_window(cx: &mut TestAppContext) {
    let (width, height) = (640.0, 260.0);
    let mut f = fixture_sized(cx, "# A\n", width, height);
    f.click("more-actions");
    let menu = f.bounds("context-menu").expect("menu drawn");
    assert!(
        (f32::from(menu.size.height) - (height - 2.0 * spacing::EDGE)).abs() < 0.5,
        "{menu:?}"
    );
    f.keys("end");
    f.assert_highlight_shown(height);
    f.keys("home");
    f.assert_highlight_shown(height);
    f.keys("escape");

    // At the bottom right corner: moved up and to the left of the pointer.
    let corner = point(px(width - 20.0), px(height - 20.0));
    let window = f.window;
    f.vcx.update(|window_, cx| {
        assert_eq!(window_.window_handle(), window);
        show_context_menu(ContextMenu::Tab, corner, window_, cx);
    });
    draw(&mut f.vcx);
    let menu = f.bounds("context-menu").expect("menu drawn");
    assert_eq!(menu.right(), corner.x);
    assert!(f32::from(menu.bottom()) <= height - spacing::EDGE + 0.5);
}

// --- Side panels ----------------------------------------------------------------

/// The switcher's buttons pick the panel, each drawn in the sidebar.
#[gpui::test]
fn the_sidebar_switcher_switches_panels(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.dispatch(crate::ToggleSidebar);
    assert!(f.has("files-panel"));
    for panel in [
        SidebarPanel::Search,
        SidebarPanel::Tags,
        SidebarPanel::Bookmarks,
    ] {
        let id = panel.id();
        f.click(&format!("panel-switch:{id}"));
        let shown = f.workspace.read_with(&f.vcx, |ws, _| ws.sidebar_panel());
        assert_eq!(shown, panel);
        assert!(!f.has("files-panel"));
        assert!(f.has(&format!("sidebar-panel:{id}")));
    }
    f.click("panel-switch:files");
    assert!(f.has("files-panel"));
}

/// The View menu's panel commands show their panel, opening the side it is
/// on.
#[gpui::test]
fn the_show_commands_open_their_panels(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    assert!(!f.has("sidebar"));
    f.dispatch(commands::ShowRecent);
    assert!(f.has("sidebar-panel:recent"));
    f.dispatch(commands::ShowSyncConflicts);
    assert!(f.has("sidebar-panel:sync-conflicts"));
    f.dispatch(commands::ShowTags);
    assert!(f.has("sidebar-panel:tags"));
    f.dispatch(commands::ShowBookmarks);
    assert!(f.has("sidebar-panel:bookmarks"));
    f.dispatch(commands::ShowFiles);
    assert!(f.has("files-panel"));
    let focused = {
        let workspace = f.workspace.clone();
        f.vcx.update(|window, cx| {
            workspace
                .read(cx)
                .files
                .as_ref()
                .is_some_and(|files| files.read(cx).focus_handle(cx).is_focused(window))
        })
    };
    assert!(focused, "View > Files gives the tree the keyboard");

    assert!(!f.has("backlinks-head"));
    f.dispatch(commands::ShowBacklinks);
    assert!(f.has("backlinks-head"));
    assert!(f.has("backlinks-list"));
    assert!(f.has("outgoing-links-head"));
    assert!(!f.has("outgoing-list"));
    f.dispatch(commands::ShowOutgoingLinks);
    assert!(f.has("outgoing-list"));
    f.dispatch(commands::ShowLinkChecker);
    assert!(f.has("link-checker-head"));
    f.dispatch(commands::ShowOutline);
    let outline_open = {
        let outline = f.workspace.read_with(&f.vcx, |ws, _| ws.outline.clone());
        outline.is_some_and(|o| o.read_with(&f.vcx, |o, _| o.is_open()))
    };
    assert!(outline_open);
}

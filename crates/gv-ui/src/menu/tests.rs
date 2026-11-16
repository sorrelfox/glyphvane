// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The application menu: its contents per platform, the macOS menu bar, and
//! the popover of Windows and Linux in headless windows.

use std::path::Path;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, Pixels, Point,
    TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_settings::keymap::{CommandRegistry, Keymap, Platform};

use super::model::{ContextMenu, Entry, Role, SystemEntry, app_menus, context_menu};
use super::native_menus;
use crate::chrome::WindowChrome;
use crate::keymap::action_for;
use crate::metrics::size::{MENU_ROW, MENU_WIDTH};
use crate::shell::{OpenTarget, ViewMode, Workspace};
use crate::{AppSettings, Appearance, ThemeChoice};

// --- Contents -------------------------------------------------------------------

fn titles(platform: Platform) -> Vec<&'static str> {
    app_menus(platform).iter().map(|m| m.title).collect()
}

fn items(platform: Platform, menu: &str) -> Vec<&'static str> {
    app_menus(platform)
        .into_iter()
        .find(|m| m.title == menu)
        .map(|m| m.items().map(|i| i.command).collect())
        .unwrap_or_default()
}

#[test]
fn the_menus_follow_each_platform() {
    for platform in [Platform::Linux, Platform::Windows] {
        assert_eq!(
            titles(platform),
            ["File", "Edit", "Format", "View", "Go", "Help"]
        );
        let file = items(platform, "File");
        assert_eq!(file[file.len() - 2..], ["app.settings", "app.quit"]);
        assert_eq!(
            items(platform, "Help"),
            [
                "app.keymap_cheatsheet",
                "app.open_documentation",
                "app.copy_debug_info",
                "app.licenses",
                "app.check_for_updates",
                "app.about"
            ]
        );
    }
    let mac = Platform::MacOs;
    assert_eq!(
        titles(mac),
        [
            "Glyphvane",
            "File",
            "Edit",
            "Format",
            "View",
            "Go",
            "Window",
            "Help"
        ]
    );
    assert_eq!(
        items(mac, "Glyphvane"),
        [
            "app.about",
            "app.check_for_updates",
            "app.settings",
            "app.install_cli",
            "app.quit"
        ]
    );
    assert_eq!(
        items(mac, "Window"),
        [
            "window.minimize",
            "window.zoom",
            "window.bring_all_to_front"
        ]
    );
    assert_eq!(
        items(mac, "Help"),
        [
            "app.keymap_cheatsheet",
            "app.open_documentation",
            "app.copy_debug_info",
            "app.licenses"
        ]
    );
    let file = items(mac, "File");
    assert!(!file.contains(&"app.quit") && !file.contains(&"app.settings"));
    const FILE: [&str; 24] = [
        "file.new_note",
        "tabs.new",
        "workspace.new_window",
        "file.open",
        "file.open_folder",
        "file.open_recent",
        "file.save",
        "file.save_as",
        "file.revert",
        "file.rename",
        "file.move",
        "file.duplicate",
        "file.delete",
        "workspace.reveal_in_tree",
        "file.reveal_in_file_manager",
        "file.open_with_default_app",
        "bookmarks.toggle",
        "file.history",
        "file.export_html",
        "file.export_pdf",
        "file.open_in_browser",
        "file.print",
        "tabs.close",
        "tabs.reopen_closed",
    ];
    assert_eq!(items(mac, "File"), FILE);
    for platform in Platform::ALL {
        assert_eq!(items(platform, "File")[..FILE.len()], FILE);
        assert_eq!(
            items(platform, "Edit"),
            [
                "editor.undo",
                "editor.redo",
                "editor.cut",
                "editor.copy",
                "editor.copy_as_markdown",
                "editor.copy_as_rich_text",
                "links.copy_link_to_heading",
                "editor.paste",
                "editor.paste_plain",
                "editor.select_all",
                "editor.expand_selection",
                "editor.shrink_selection",
                "search.find",
                "search.replace",
                "search.find_next",
                "search.find_previous",
                "search.vault",
                "editor.move_line_up",
                "editor.move_line_down",
                "editor.duplicate_line",
                "editor.join_lines",
                "editor.sort_lines",
                "editor.change_case",
            ]
        );
        assert_eq!(
            items(platform, "Format"),
            [
                "editor.bold",
                "editor.italic",
                "editor.strikethrough",
                "editor.highlight",
                "editor.inline_code",
                "editor.insert_link",
                "editor.heading_1",
                "editor.heading_2",
                "editor.heading_3",
                "editor.heading_4",
                "editor.heading_5",
                "editor.heading_6",
                "editor.paragraph",
                "editor.quote",
                "editor.insert_callout",
                "editor.toggle_task",
                "editor.indent",
                "editor.outdent",
                "editor.renumber_list",
                "editor.insert_footnote",
                "editor.table",
                "notes.insert_template",
                "editor.toggle_comment",
            ]
        );
        assert_eq!(
            items(platform, "View"),
            [
                "view.preview",
                "view.edit",
                "view.split",
                "view.toggle_mode",
                "view.toggle_split",
                "view.toggle_viewer_mode",
                "workspace.toggle_sidebar",
                "workspace.toggle_panel",
                "workspace.show_files",
                "workspace.show_tags",
                "workspace.show_bookmarks",
                "workspace.show_recent",
                "workspace.show_sync_conflicts",
                "workspace.show_outline",
                "workspace.show_backlinks",
                "workspace.show_outgoing_links",
                "workspace.show_link_checker",
                "editor.toggle_whitespace",
                "view.toggle_soft_wrap",
                "view.fold",
                "view.unfold",
                "view.fold_all",
                "view.unfold_all",
                "view.cycle_theme",
                "view.zoom_in",
                "view.zoom_out",
                "view.zoom_reset",
                "window.toggle_full_screen",
            ]
        );
        assert_eq!(
            items(platform, "Go"),
            [
                "navigation.back",
                "navigation.forward",
                "workspace.quick_switcher",
                "navigation.go_to_line",
                "navigation.go_to_heading",
                "navigation.go_to_footnote",
                "links.follow",
                "links.open_in_new_tab",
                "links.open_in_split",
                "links.open_in_new_window",
                "links.preview",
                "links.list",
                "notes.daily_note",
                "navigation.next_misspelling",
                "workspace.focus_next_pane",
                "workspace.focus_previous_pane",
            ]
        );
    }
}

fn context_items(kind: ContextMenu) -> Vec<&'static str> {
    context_menu(kind).items().map(|i| i.command).collect()
}

#[test]
fn the_context_menus_hold_their_commands() {
    assert_eq!(
        context_items(ContextMenu::Link),
        [
            "links.follow",
            "links.open_in_new_tab",
            "links.open_in_split",
            "links.open_in_new_window",
            "links.copy_link",
            "links.copy_resolved_path",
            "links.copy_url",
            "links.reveal_in_tree",
            "links.reveal_in_file_manager",
            "links.show_trace",
            "links.fix",
        ]
    );
    let editor = context_items(ContextMenu::Editor);
    assert_eq!(
        editor[..8],
        [
            "editor.cut",
            "editor.copy",
            "editor.copy_as_markdown",
            "editor.paste",
            "editor.paste_plain",
            "spell.add_to_dictionary",
            "spell.add_to_vault_dictionary",
            "spell.ignore",
        ]
    );
    // Every table command, Insert or format table first.
    let registry = CommandRegistry::builtin();
    let tables: Vec<&str> = registry
        .iter()
        .map(|c| c.id().as_str())
        .filter(|id| id.starts_with("editor.table"))
        .collect();
    assert_eq!(editor[8..], tables[..]);
    assert_eq!(
        context_items(ContextMenu::FileTree),
        [
            "file.new_note",
            "file.new_folder",
            "file.rename",
            "file.move",
            "file.duplicate",
            "file.delete",
            "file.reveal_in_file_manager",
            "file.open_with_default_app",
        ]
    );
    // Not designed: the usual tab menu.
    assert_eq!(
        context_items(ContextMenu::Tab),
        [
            "tabs.close",
            "tabs.close_others",
            "tabs.close_to_right",
            "file.copy_path",
            "workspace.reveal_in_tree",
            "file.reveal_in_file_manager",
        ]
    );
    // Not designed: commands about the active document.
    assert_eq!(
        context_items(ContextMenu::MoreActions),
        [
            "file.copy_path",
            "workspace.reveal_in_tree",
            "file.reveal_in_file_manager",
            "file.open_with_default_app",
            "bookmarks.toggle",
            "file.history",
            "links.list",
            "search.find",
            "view.toggle_viewer_mode",
            "file.export_html",
            "file.export_pdf",
            "file.print",
        ]
    );
}

/// Every menu on every platform, and every context menu.
fn all_menus() -> Vec<super::model::Menu> {
    let mut menus: Vec<_> = Platform::ALL.into_iter().flat_map(app_menus).collect();
    menus.extend(
        [
            ContextMenu::Link,
            ContextMenu::Editor,
            ContextMenu::FileTree,
            ContextMenu::Tab,
            ContextMenu::MoreActions,
        ]
        .into_iter()
        .map(context_menu),
    );
    menus
}

#[test]
fn every_item_names_a_command() {
    let registry = CommandRegistry::builtin();
    for menu in all_menus() {
        let mut seen = std::collections::HashSet::new();
        for item in menu.items() {
            assert!(registry.get(item.command).is_some(), "{}", item.command);
            assert!(action_for(item.command).is_some(), "{}", item.command);
            assert!(seen.insert(item.command), "{} twice", item.command);
        }
        // No separator at either end or twice in a row.
        let separators: Vec<bool> = menu
            .entries
            .iter()
            .map(|e| matches!(e, Entry::Separator))
            .collect();
        assert!(!separators.first().copied().unwrap_or(true));
        assert!(!separators.last().copied().unwrap_or(true));
        assert!(!separators.windows(2).any(|w| w[0] && w[1]));
    }
}

/// Every command is in some menu, except those reached by keys, the
/// palette or a view's own controls only.
#[test]
fn every_command_is_in_a_menu_or_left_out_on_purpose() {
    const NOT_IN_MENUS: &[&str] = &[
        "app.open_menu",
        "settings.import",
        "settings.export",
        "workspace.command_palette",
        "workspace.context_menu",
        "view.fold_level_1",
        "view.fold_level_2",
        "view.fold_level_3",
        "view.fold_level_4",
        "view.fold_level_5",
        "view.fold_level_6",
        "links.create_note",
        "links.check_note",
        "links.check_folder",
        "links.check_vault",
        "refactor.rename_heading",
        "editor.newline",
        "editor.newline_plain",
    ];
    let in_menus: std::collections::HashSet<&str> = all_menus()
        .iter()
        .flat_map(|m| m.items().map(|i| i.command).collect::<Vec<_>>())
        .collect();
    for command in CommandRegistry::builtin().iter() {
        let id = command.id().as_str();
        assert_ne!(
            in_menus.contains(id),
            NOT_IN_MENUS.contains(&id),
            "{id}: in a menu, or listed as left out, but not both"
        );
    }
}

/// The shortcut labels the menus show, per platform.
#[test]
fn shortcut_labels_follow_the_platform() {
    let registry = CommandRegistry::builtin();
    let labels = |platform: Platform| {
        let keymap = Keymap::defaults(&registry, platform);
        app_menus(platform)
            .iter()
            .flat_map(|m| m.items().copied().collect::<Vec<_>>())
            .filter_map(|item| {
                let label = keymap.key_labels(item.command).into_iter().next()?;
                Some(format!("{} {label}", item.title))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        labels(Platform::MacOs),
        [
            "Settings… ⌘,",
            "Quit Glyphvane ⌘Q",
            "New note ⌘N",
            "Save ⌘S",
            "Save as… ⇧⌘S",
            "Close tab ⌘W",
            "Reopen closed tab ⇧⌘T",
            "Undo ⌘Z",
            "Redo ⇧⌘Z",
            "Cut ⌘X",
            "Copy ⌘C",
            "Paste ⌘V",
            "Paste as plain text ⇧⌘V",
            "Select all ⌘A",
            "Expand selection ⌃⇧⌘→",
            "Shrink selection ⌃⇧⌘←",
            "Find ⌘F",
            "Replace ⌥⌘F",
            "Find next ⌘G",
            "Find previous ⇧⌘G",
            "Search in vault ⇧⌘F",
            "Move line up ⌥↑",
            "Move line down ⌥↓",
            "Bold ⌘B",
            "Italic ⌘I",
            "Strikethrough ⇧⌘X",
            "Highlight ⇧⌘H",
            "Inline code ⇧⌘C",
            "Insert link ⌘K",
            "Heading 1 ⌥⌘1",
            "Heading 2 ⌥⌘2",
            "Heading 3 ⌥⌘3",
            "Heading 4 ⌥⌘4",
            "Heading 5 ⌥⌘5",
            "Heading 6 ⌥⌘6",
            "Paragraph ⌥⌘0",
            "Quote ⇧⌘.",
            "Toggle task ⌘L",
            "Indent ⇥",
            "Outdent ⇧⇥",
            "Insert or format table ⌥⌘T",
            "Toggle comment ⌘/",
            "Toggle Preview and Edit ⌘E",
            "Toggle split view ⌥⌘E",
            "Cycle theme ⌘K ⌘T",
            "Zoom in ⌘=",
            "Zoom out ⌘-",
            "Reset zoom ⌘0",
            "Back ⌘[",
            "Forward ⌘]",
            "Quick switcher… ⌘O",
            "Go to line… ⌃G",
            "Go to heading… ⇧⌘O",
            "Follow link ⌥↩",
            "Open link in new tab ⌥⌘↩",
            "Next pane F6",
            "Previous pane ⇧F6",
        ]
    );
    let linux = [
        "New note Ctrl+N",
        "Save Ctrl+S",
        "Save as… Ctrl+Shift+S",
        "Close tab Ctrl+W",
        "Reopen closed tab Ctrl+Shift+T",
        "Settings… Ctrl+,",
        "Quit Ctrl+Q",
        "Undo Ctrl+Z",
        "Redo Ctrl+Shift+Z",
        "Cut Ctrl+X",
        "Copy Ctrl+C",
        "Paste Ctrl+V",
        "Paste as plain text Ctrl+Shift+V",
        "Select all Ctrl+A",
        "Expand selection Alt+Shift+→",
        "Shrink selection Alt+Shift+←",
        "Find Ctrl+F",
        "Replace Ctrl+H",
        "Find next F3",
        "Find previous Shift+F3",
        "Search in vault Ctrl+Shift+F",
        "Move line up Alt+↑",
        "Move line down Alt+↓",
        "Bold Ctrl+B",
        "Italic Ctrl+I",
        "Strikethrough Ctrl+Shift+X",
        "Highlight Ctrl+Shift+H",
        "Inline code Ctrl+Shift+C",
        "Insert link Ctrl+K",
        "Heading 1 Ctrl+Alt+1",
        "Heading 2 Ctrl+Alt+2",
        "Heading 3 Ctrl+Alt+3",
        "Heading 4 Ctrl+Alt+4",
        "Heading 5 Ctrl+Alt+5",
        "Heading 6 Ctrl+Alt+6",
        "Paragraph Ctrl+Alt+0",
        "Quote Ctrl+Shift+.",
        "Toggle task Ctrl+L",
        "Indent Tab",
        "Outdent Shift+Tab",
        "Toggle comment Ctrl+/",
        "Toggle Preview and Edit Ctrl+E",
        "Toggle split view Ctrl+Alt+E",
        "Cycle theme Ctrl+K Ctrl+T",
        "Zoom in Ctrl+=",
        "Zoom out Ctrl+-",
        "Reset zoom Ctrl+0",
        "Back Alt+←",
        "Forward Alt+→",
        "Quick switcher… Ctrl+O",
        "Go to line… Ctrl+G",
        "Go to heading… Ctrl+Shift+O",
        "Follow link Alt+Enter",
        "Open link in new tab Ctrl+Alt+Enter",
        "Next pane F6",
        "Previous pane Shift+F6",
    ];
    assert_eq!(labels(Platform::Linux), linux);
    // Windows also has Insert table (Ctrl+Alt+T opens a terminal on Linux).
    let mut windows = linux.to_vec();
    let outdent = windows
        .iter()
        .position(|l| *l == "Outdent Shift+Tab")
        .unwrap();
    windows.insert(outdent + 1, "Insert or format table Ctrl+Alt+T");
    assert_eq!(labels(Platform::Windows), windows);
}

#[test]
fn the_macos_menu_bar_carries_the_commands_actions() {
    let menus = native_menus(Platform::MacOs);
    let names: Vec<&str> = menus.iter().map(|m| m.name.as_ref()).collect();
    // The system fills the menu named Window with the window list.
    assert_eq!(
        names,
        [
            "Glyphvane",
            "File",
            "Edit",
            "Format",
            "View",
            "Go",
            "Window",
            "Help"
        ]
    );
    let app = &menus[0];
    assert!(app.items.iter().any(|i| matches!(
        i,
        gpui::MenuItem::SystemMenu(menu) if menu.name.as_ref() == SystemEntry::Services.title()
    )));
    let actions = |menu: &gpui::Menu| -> Vec<(String, &'static str, bool)> {
        menu.items
            .iter()
            .filter_map(|i| match i {
                gpui::MenuItem::Action {
                    name,
                    action,
                    os_action,
                    ..
                } => Some((name.to_string(), action.name(), os_action.is_some())),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        actions(app),
        [
            ("About Glyphvane".to_owned(), "app::About", false),
            (
                "Check for updates".to_owned(),
                "app::CheckForUpdates",
                false
            ),
            ("Settings…".to_owned(), "app::Settings", false),
            (
                "Install glyphvane command…".to_owned(),
                "app::InstallCli",
                false
            ),
            ("Hide Glyphvane".to_owned(), "app_menu::Hide", false),
            ("Hide others".to_owned(), "app_menu::HideOthers", false),
            ("Show all".to_owned(), "app_menu::ShowAll", false),
            ("Quit Glyphvane".to_owned(), "app::Quit", false),
        ]
    );
    assert_eq!(
        actions(&menus[2])[..10],
        [
            ("Undo".to_owned(), "editor::Undo", true),
            ("Redo".to_owned(), "editor::Redo", true),
            ("Cut".to_owned(), "editor::Cut", true),
            ("Copy".to_owned(), "editor::Copy", true),
            (
                "Copy as Markdown".to_owned(),
                "editor::CopyAsMarkdown",
                false
            ),
            (
                "Copy as rich text".to_owned(),
                "editor::CopyAsRichText",
                false
            ),
            (
                "Copy link to heading".to_owned(),
                "links::CopyLinkToHeading",
                false
            ),
            ("Paste".to_owned(), "editor::Paste", true),
            (
                "Paste as plain text".to_owned(),
                "editor::PastePlain",
                false
            ),
            ("Select all".to_owned(), "editor::SelectAll", true),
        ]
    );
    let roles: Vec<Option<Role>> = app_menus(Platform::MacOs)[2]
        .items()
        .map(|i| i.role)
        .collect();
    assert_eq!(
        roles[..10],
        [
            Some(Role::Undo),
            Some(Role::Redo),
            Some(Role::Cut),
            Some(Role::Copy),
            None,
            None,
            None,
            Some(Role::Paste),
            None,
            Some(Role::SelectAll),
        ]
    );
    assert!(roles[10..].iter().all(Option::is_none));
}

// --- The popover ----------------------------------------------------------------

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

fn open_sized(
    cx: &mut TestAppContext,
    file: &Path,
    chrome: WindowChrome,
    height: f32,
) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(height)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, chrome, window, cx))
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

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext, chrome: WindowChrome) -> Fixture {
    fixture_sized(cx, chrome, HEIGHT)
}

fn fixture_sized(cx: &mut TestAppContext, chrome: WindowChrome, height: f32) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("a.md");
    std::fs::write(&file, "# A\n\nSome text.\n").expect("write");
    let (window, workspace) = open_sized(cx, &file, chrome, height);
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        _dir: dir,
        workspace,
        vcx,
    }
}

impl Fixture {
    fn bounds(&mut self, selector: &str) -> Option<Bounds<Pixels>> {
        // `debug_bounds` takes a `'static` selector.
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        self.vcx.debug_bounds(selector)
    }

    fn center(&mut self, selector: &str) -> Point<Pixels> {
        self.bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center()
    }

    fn click(&mut self, selector: &str) {
        let at = self.center(selector);
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }

    fn is_open(&mut self) -> bool {
        self.bounds("app-menu-popover").is_some()
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

    fn mode(&mut self) -> ViewMode {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().mode())
    }

    fn sidebar_open(&mut self) -> bool {
        // The sidebar holds the Files panel once shown.
        self.bounds("sidebar").is_some()
    }
}

fn label(keys: &str) -> String {
    let platform = Platform::current();
    gv_settings::keymap::KeySequence::parse(keys, platform)
        .expect("parses")
        .label(platform)
}

/// The button opens the menu below it, drawn like the link menu; Esc closes
/// it and gives the keyboard back to the document.
#[gpui::test]
fn the_menu_button_opens_the_menu(cx: &mut TestAppContext) {
    for chrome in [WindowChrome::Windows, WindowChrome::Linux] {
        let mut f = fixture(cx, chrome);
        assert!(!f.is_open());
        let button = f.bounds("app-menu").expect("the button is drawn");
        f.click("app-menu");
        let menu = f.bounds("app-menu-popover").expect("the menu opens");
        assert_eq!(menu.size.width, px(MENU_WIDTH));
        assert!(menu.origin.y >= button.bottom(), "{menu:?} {button:?}");
        assert_eq!(menu.origin.x, button.origin.x);
        let file = f.bounds("app-menu-top:File").expect("a row per menu");
        assert_eq!(file.size.height, px(MENU_ROW));
        assert!(f.bounds("app-menu-top:Help").is_some());
        assert!(!f.reader_focused(), "the menu has the keyboard");

        f.keys("escape");
        assert!(!f.is_open());
        assert!(f.reader_focused());

        // The button toggles; a click outside closes without doing more.
        f.click("app-menu");
        assert!(f.is_open());
        f.click("app-menu");
        assert!(!f.is_open());
        f.click("app-menu");
        let outside = point(px(WIDTH / 2.0), px(HEIGHT / 2.0));
        f.vcx.simulate_click(outside, Modifiers::none());
        draw(&mut f.vcx);
        assert!(!f.is_open());
        assert!(f.reader_focused());
    }
}

/// Submenus show the keymap's labels for this platform, checks for the
/// current mode, and dim the commands nothing handles yet.
#[gpui::test]
fn the_menu_shows_shortcuts_checks_and_disabled_items(cx: &mut TestAppContext) {
    let mut f = fixture(cx, WindowChrome::Windows);
    f.click("app-menu");
    // Right opens File, the highlighted menu.
    f.keys("right");
    let submenu = f.bounds("app-menu-submenu").expect("File opens");
    let file_row = f.bounds("app-menu-top:File").expect("drawn");
    assert!(submenu.size.width >= px(MENU_WIDTH));
    assert!(submenu.origin.x > file_row.right());
    assert!(
        f.bounds(&format!("app-menu-shortcut:{}", label("secondary-w")))
            .is_some()
    );
    assert!(
        f.bounds(&format!("app-menu-shortcut:{}", label("secondary-shift-t")))
            .is_some()
    );
    let close = f.bounds("app-menu-item:tabs.close").expect("drawn");
    assert_eq!(close.size.height, px(MENU_ROW));

    // A disabled item does nothing and keeps the menu open.
    f.click("app-menu-item:file.export_html");
    assert!(f.is_open());

    // View: the current mode is checked; chords are labelled in full.
    f.click("app-menu-top:View");
    assert!(f.bounds("app-menu-check:view.preview").is_some());
    assert!(f.bounds("app-menu-check:view.edit").is_none());
    assert!(
        f.bounds(&format!(
            "app-menu-shortcut:{}",
            label("secondary-k secondary-t")
        ))
        .is_some()
    );
    f.click("app-menu-item:view.edit");
    assert!(!f.is_open());
    assert_eq!(f.mode(), ViewMode::Edit);
    f.click("app-menu");
    f.click("app-menu-top:View");
    assert!(f.bounds("app-menu-check:view.edit").is_some());
}

/// Arrow keys move through the menus and items, skipping disabled ones;
/// Enter runs the command where the keyboard was.
#[gpui::test]
fn the_menu_runs_commands_from_the_keyboard(cx: &mut TestAppContext) {
    let mut f = fixture(cx, WindowChrome::Linux);
    f.click("app-menu");
    // File, Edit, Format, View: open View; its first item is highlighted.
    f.keys("down down down right");
    assert!(f.bounds("app-menu-item:view.preview").is_some());
    // Preview, Edit, Split, Toggle Preview and Edit, Toggle split view,
    // Toggle sidebar (Viewer mode has no handler yet and is skipped).
    f.keys("down down down down down enter");
    assert!(!f.is_open());
    assert!(f.sidebar_open());
    assert!(f.reader_focused());

    // Edit ▸ Select all, then Edit ▸ Copy, run in the document.
    f.click("app-menu");
    f.keys("down right down down enter");
    f.click("app-menu");
    f.keys("down right enter");
    let copied = f
        .vcx
        .update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert!(copied.is_some_and(|t| t.contains("Some text.")));

    // Left goes back to the menus; Right from a submenu opens the next
    // menu. Cycle theme from the keyboard.
    f.click("app-menu");
    f.keys("right left down right");
    assert!(f.bounds("app-menu-item:editor.copy").is_some());
    // Format, with nothing to format in Preview, then View.
    f.keys("right");
    assert!(f.bounds("app-menu-item:editor.bold").is_some());
    f.keys("right");
    assert!(f.bounds("app-menu-item:view.cycle_theme").is_some());
    // Up wraps to the last enabled item, Reset zoom (full screen is
    // disabled); three more up is Cycle theme.
    f.keys("up up up up enter");
    assert!(!f.is_open());
    assert_eq!(
        f.vcx.update(|_, cx| Appearance::choice(cx)),
        ThemeChoice::SemiLight
    );

    // Go: nothing to go back to, so Back and Forward are disabled; the
    // first enabled item is Go to heading, which opens the Go popover.
    f.click("app-menu");
    f.keys("end up right enter");
    assert!(!f.is_open());
    assert!(f.bounds("goto-popover").is_some());
    f.keys("escape");
    assert!(f.bounds("goto-popover").is_none());
}

/// In a short window a tall submenu moves up to stay inside it, keeping a
/// margin; one taller than the window scrolls, and the keys still reach
/// every item.
#[gpui::test]
fn tall_submenus_fit_short_windows(cx: &mut TestAppContext) {
    use crate::metrics::spacing::menu::EDGE;
    let height = 360.0;
    let mut f = fixture_sized(cx, WindowChrome::Linux, height);
    f.click("app-menu");
    for menu in ["File", "View", "Go"] {
        f.click(&format!("app-menu-top:{menu}"));
        let submenu = f.bounds("app-menu-submenu").expect("submenu drawn");
        assert!(
            f32::from(submenu.top()) >= EDGE - 0.5,
            "{menu}: {submenu:?}"
        );
        assert!(
            f32::from(submenu.bottom()) <= height - EDGE + 0.5,
            "{menu}: {submenu:?}"
        );
    }
    // File is taller than the window: the last item scrolls into view.
    f.click("app-menu-top:File");
    let submenu = f.bounds("app-menu-submenu").expect("submenu drawn");
    assert!(
        (f32::from(submenu.size.height) - (height - 2.0 * EDGE)).abs() < 0.5,
        "{submenu:?}"
    );
    let quit = |f: &mut Fixture| f.bounds("app-menu-item:app.quit").expect("drawn");
    assert!(
        quit(&mut f).top() >= submenu.bottom(),
        "Quit starts out of view"
    );
    f.keys("end");
    let item = quit(&mut f);
    assert!(
        item.top() >= submenu.top() && item.bottom() <= submenu.bottom(),
        "{item:?} in {submenu:?}"
    );
    // Home goes back to the first enabled item, New tab.
    f.keys("home");
    let first = f.bounds("app-menu-item:tabs.new").expect("drawn");
    assert!(
        first.top() >= submenu.top() && first.bottom() <= submenu.bottom(),
        "{first:?} in {submenu:?}"
    );
    assert!(f.is_open());
}

/// F10 opens the menu from the keyboard on Windows and Linux.
#[gpui::test]
fn f10_opens_the_menu(cx: &mut TestAppContext) {
    if Platform::current() == Platform::MacOs {
        return;
    }
    let mut f = fixture(cx, WindowChrome::Linux);
    f.keys("f10");
    assert!(f.is_open());
    f.keys("right");
    assert!(f.bounds("app-menu-item:tabs.close").is_some());
    f.keys("escape");
    assert!(!f.is_open());
    assert!(f.reader_focused());
    // F10 again leaves the menu, as on Windows.
    f.keys("f10");
    assert!(f.is_open());
    f.keys("f10");
    assert!(!f.is_open());
    assert!(f.reader_focused());
}

/// View ▸ Soft wrap is disabled in Preview and toggles soft wrap in the
/// editor.
#[gpui::test]
fn the_soft_wrap_item_works_in_the_editor_only(cx: &mut TestAppContext) {
    let mut f = fixture(cx, WindowChrome::Linux);
    f.click("app-menu");
    f.click("app-menu-top:View");
    f.click("app-menu-item:view.toggle_soft_wrap");
    assert!(f.is_open(), "disabled in Preview");
    f.keys("escape");

    f.keys("secondary-e");
    assert_eq!(f.mode(), ViewMode::Edit);
    let editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    let state = |f: &mut Fixture| {
        editor.read_with(&f.vcx, |e, _| {
            (
                e.soft_wrap(),
                e.editor().expect("loaded").wrap_width().is_some(),
            )
        })
    };
    assert_eq!(state(&mut f), (true, true));
    for on in [false, true] {
        f.click("app-menu");
        f.click("app-menu-top:View");
        f.click("app-menu-item:view.toggle_soft_wrap");
        assert!(!f.is_open());
        assert_eq!(state(&mut f), (on, on));
    }
}

/// In the editor, the Format menu runs the Markdown commands; those the
/// tab's profile lacks (strikethrough in CommonMark) are disabled.
#[gpui::test]
fn the_format_menu_follows_the_profile(cx: &mut TestAppContext) {
    let mut f = fixture(cx, WindowChrome::Linux);
    // Preview: nothing to format.
    f.click("app-menu");
    f.click("app-menu-top:Format");
    f.click("app-menu-item:editor.bold");
    assert!(f.is_open(), "disabled in Preview");
    f.keys("escape");

    // The file's profile is set to CommonMark (detection would choose
    // Extended for a loose folder).
    f.workspace.update(&mut f.vcx, |ws, cx| {
        ws.pick_profile(
            crate::profile::Scope::File,
            Some(gv_profile::ProfileId::CommonMark),
            cx,
        );
    });
    f.keys("secondary-e");
    assert_eq!(f.mode(), ViewMode::Edit);
    let editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    let text = |f: &mut Fixture| {
        editor.read_with(&f.vcx, |e, _| {
            let text = e.editor().expect("loaded").text();
            text.text_for_range(0..text.len()).unwrap_or_default()
        })
    };
    assert_eq!(text(&mut f), "# A\n\nSome text.\n");
    f.click("app-menu");
    f.click("app-menu-top:Format");
    f.click("app-menu-item:editor.strikethrough");
    assert!(f.is_open(), "CommonMark has no strikethrough");
    f.click("app-menu-item:editor.heading_2");
    assert!(!f.is_open());
    assert_eq!(text(&mut f), "## A\n\nSome text.\n");
}

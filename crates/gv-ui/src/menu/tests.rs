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

use super::model::{Entry, Role, SystemEntry, app_menus};
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
        assert_eq!(
            items(platform, "File"),
            [
                "tabs.new",
                "file.open",
                "file.save",
                "file.save_as",
                "tabs.close",
                "tabs.reopen_closed",
                "app.quit"
            ]
        );
        assert_eq!(items(platform, "Help"), ["app.about"]);
    }
    let mac = Platform::MacOs;
    assert_eq!(
        titles(mac),
        ["Glyphvane", "File", "Edit", "Format", "View", "Go"]
    );
    assert_eq!(
        items(mac, "Glyphvane"),
        ["app.about", "app.settings", "app.quit"]
    );
    assert!(!items(mac, "File").contains(&"app.quit"));
    for platform in Platform::ALL {
        assert_eq!(
            items(platform, "Edit"),
            [
                "editor.undo",
                "editor.redo",
                "editor.cut",
                "editor.copy",
                "editor.copy_as_markdown",
                "editor.paste",
                "editor.select_all",
                "search.find",
                "search.replace"
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
                "editor.toggle_task",
                "editor.indent",
                "editor.outdent",
                "editor.renumber_list",
            ]
        );
        assert_eq!(
            items(platform, "View"),
            [
                "view.preview",
                "view.edit",
                "view.split",
                "workspace.toggle_sidebar",
                "workspace.toggle_panel",
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
            ]
        );
        assert_eq!(
            items(platform, "Go"),
            ["navigation.back", "navigation.forward"]
        );
    }
}

#[test]
fn every_item_names_a_command() {
    let registry = CommandRegistry::builtin();
    for platform in Platform::ALL {
        for menu in app_menus(platform) {
            for item in menu.items() {
                assert!(registry.get(item.command).is_some(), "{}", item.command);
                assert!(action_for(item.command).is_some(), "{}", item.command);
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
            "Save ⌘S",
            "Save as… ⇧⌘S",
            "Close tab ⌘W",
            "Reopen closed tab ⇧⌘T",
            "Undo ⌘Z",
            "Redo ⇧⌘Z",
            "Cut ⌘X",
            "Copy ⌘C",
            "Paste ⌘V",
            "Select all ⌘A",
            "Find ⌘F",
            "Replace ⌥⌘F",
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
            "Cycle theme ⌘K ⌘T",
            "Zoom in ⌘=",
            "Zoom out ⌘-",
            "Reset zoom ⌘0",
            "Back ⌘[",
            "Forward ⌘]",
        ]
    );
    let others = [
        "Save Ctrl+S",
        "Save as… Ctrl+Shift+S",
        "Close tab Ctrl+W",
        "Reopen closed tab Ctrl+Shift+T",
        "Quit Ctrl+Q",
        "Undo Ctrl+Z",
        "Redo Ctrl+Shift+Z",
        "Cut Ctrl+X",
        "Copy Ctrl+C",
        "Paste Ctrl+V",
        "Select all Ctrl+A",
        "Find Ctrl+F",
        "Replace Ctrl+H",
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
        "Cycle theme Ctrl+K Ctrl+T",
        "Zoom in Ctrl+=",
        "Zoom out Ctrl+-",
        "Reset zoom Ctrl+0",
        "Back Alt+←",
        "Forward Alt+→",
    ];
    assert_eq!(labels(Platform::Linux), others);
    assert_eq!(labels(Platform::Windows), others);
}

#[test]
fn the_macos_menu_bar_carries_the_commands_actions() {
    let menus = native_menus(Platform::MacOs);
    let names: Vec<&str> = menus.iter().map(|m| m.name.as_ref()).collect();
    assert_eq!(names, ["Glyphvane", "File", "Edit", "Format", "View", "Go"]);
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
            ("Settings…".to_owned(), "app::Settings", false),
            ("Hide Glyphvane".to_owned(), "app_menu::Hide", false),
            ("Hide others".to_owned(), "app_menu::HideOthers", false),
            ("Show all".to_owned(), "app_menu::ShowAll", false),
            ("Quit Glyphvane".to_owned(), "app::Quit", false),
        ]
    );
    assert_eq!(
        actions(&menus[2]),
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
            ("Paste".to_owned(), "editor::Paste", true),
            ("Select all".to_owned(), "editor::SelectAll", true),
            ("Find".to_owned(), "search::Find", false),
            ("Replace".to_owned(), "search::Replace", false),
        ]
    );
    let roles: Vec<Option<Role>> = app_menus(Platform::MacOs)[2]
        .items()
        .map(|i| i.role)
        .collect();
    assert_eq!(
        roles,
        [
            Some(Role::Undo),
            Some(Role::Redo),
            Some(Role::Cut),
            Some(Role::Copy),
            None,
            Some(Role::Paste),
            Some(Role::SelectAll),
            None,
            None
        ]
    );
}

// --- The popover ----------------------------------------------------------------

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

fn open(
    cx: &mut TestAppContext,
    file: &Path,
    chrome: WindowChrome,
) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
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
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("a.md");
    std::fs::write(&file, "# A\n\nSome text.\n").expect("write");
    let (window, workspace) = open(cx, &file, chrome);
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
    f.click("app-menu-item:file.open");
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
    // Preview, Edit, Split, Toggle sidebar.
    f.keys("down down down enter");
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
    // Up wraps to the last enabled item, Cycle theme (zoom is disabled).
    f.keys("up enter");
    assert!(!f.is_open());
    assert_eq!(
        f.vcx.update(|_, cx| Appearance::choice(cx)),
        ThemeChoice::SemiLight
    );

    // Go: nothing to go back to, so both items are disabled.
    f.click("app-menu");
    f.keys("end up right enter");
    assert!(f.is_open(), "no enabled item to run");
    f.keys("escape");
    assert!(!f.is_open());
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

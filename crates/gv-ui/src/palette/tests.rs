// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The command palette in headless windows: opening and closing, filtering,
//! running commands where the keyboard was, availability, shortcut labels
//! and recently run commands.

use std::path::Path;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, Modifiers, Pixels,
    TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_settings::keymap::{KeySequence, Platform};

use super::{CommandPalette, KEY_CONTEXT, palette_of};
use crate::chrome::WindowChrome;
use crate::keymap::context::gpui_context;
use crate::metrics::size::{SWITCHER_ROW, switcher};
use crate::shell::{OpenTarget, ViewMode, Workspace};
use crate::{AppSettings, Appearance, ThemeChoice};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;

fn open(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
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

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("a.md");
    std::fs::write(&file, "# A\n\nSome text.\n").expect("write");
    let (window, workspace) = open(cx, &file);
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

    fn click(&mut self, selector: &str) {
        let at = self
            .bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }

    fn type_text(&mut self, text: &str) {
        self.vcx.simulate_input(text);
        draw(&mut self.vcx);
    }

    fn palette(&mut self) -> Entity<CommandPalette> {
        self.vcx
            .update(|window, cx| palette_of(window, cx))
            .expect("the window root has a palette")
    }

    fn is_open(&mut self) -> bool {
        let open = self.palette().read_with(&self.vcx, |p, _| p.is_open());
        assert_eq!(open, self.bounds("command-palette").is_some());
        open
    }

    fn listed(&mut self) -> Vec<String> {
        self.palette().read_with(&self.vcx, |p, _| {
            p.listed().into_iter().map(str::to_owned).collect()
        })
    }

    fn selected(&mut self) -> Option<String> {
        self.palette()
            .read_with(&self.vcx, |p, _| p.selected().map(str::to_owned))
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

    fn theme(&mut self) -> ThemeChoice {
        self.vcx.update(|_, cx| Appearance::choice(cx))
    }
}

fn label(keys: &str) -> String {
    let platform = Platform::current();
    KeySequence::parse(keys, platform)
        .expect("parses")
        .label(platform)
}

#[test]
fn the_key_context_is_the_models_palette_context() {
    assert_eq!(gpui_context("palette").as_ref(), KEY_CONTEXT);
}

/// Mod+P and Mod+Shift+P open the palette where the quick switcher would
/// be; Esc closes it and gives the keyboard back to the document.
#[gpui::test]
fn the_keys_open_and_close_the_palette(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    assert!(!f.is_open());
    assert!(f.reader_focused());

    f.keys("secondary-p");
    assert!(f.is_open());
    assert!(!f.reader_focused(), "the palette has the keyboard");
    let panel = f.bounds("command-palette").expect("drawn");
    assert_eq!(panel.size.width, px(switcher::WIDTH));
    assert_eq!(panel.origin.y, px(switcher::TOP));
    assert_eq!(panel.center().x, px(WIDTH / 2.0));
    let first = f.listed()[0].clone();
    assert_eq!(f.selected(), Some(first.clone()));
    assert!(
        f.bounds(&format!("command-palette-selected:{first}"))
            .is_some(),
        "the first row is highlighted"
    );
    let row = f
        .bounds(&format!("command-palette-row:{first}"))
        .expect("drawn");
    assert_eq!(row.size.height, px(SWITCHER_ROW));
    assert!(row.origin.y >= panel.origin.y + px(switcher::INPUT_ROW));

    f.keys("escape");
    assert!(!f.is_open());
    assert!(f.reader_focused());

    // Mod+Shift+P opens it too; Mod+P again closes it.
    f.keys("secondary-shift-p");
    assert!(f.is_open());
    f.keys("secondary-p");
    assert!(!f.is_open());
    assert!(f.reader_focused());

    // A click outside closes it without doing anything else.
    f.keys("secondary-p");
    f.vcx
        .simulate_click(point(px(WIDTH / 2.0), px(HEIGHT - 40.0)), Modifiers::none());
    draw(&mut f.vcx);
    assert!(!f.is_open());
    assert!(f.reader_focused());
}

/// Typing filters the commands; Enter runs the best match where the
/// keyboard was.
#[gpui::test]
fn typing_filters_and_enter_runs(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.keys("secondary-p");
    let all = f.listed().len();
    f.type_text("cycle th");
    assert_eq!(f.listed(), ["view.cycle_theme"]);
    assert!(f.listed().len() < all);
    assert_eq!(f.selected().as_deref(), Some("view.cycle_theme"));

    assert_eq!(f.theme(), ThemeChoice::Light);
    f.keys("enter");
    assert!(!f.is_open());
    assert_eq!(f.theme(), ThemeChoice::SemiLight);
    assert!(f.reader_focused());

    // Nothing matches: an empty list, and Enter does nothing.
    f.keys("secondary-p");
    f.type_text("qqqq");
    assert!(f.listed().is_empty());
    assert!(f.bounds("command-palette-empty").is_some());
    f.keys("enter");
    assert!(f.is_open());
    f.keys("escape");
    assert_eq!(f.theme(), ThemeChoice::SemiLight);
}

/// Arrow keys move the highlight and wrap; a click runs a row.
#[gpui::test]
fn arrows_and_clicks_choose_commands(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.keys("secondary-p");
    f.type_text("show");
    let listed = f.listed();
    assert!(listed.contains(&"view.edit".to_owned()), "{listed:?}");
    let first = listed[0].clone();
    let last = listed[listed.len() - 1].clone();
    assert_eq!(f.selected(), Some(first.clone()));
    f.keys("up");
    assert_eq!(f.selected(), Some(last));
    f.keys("down");
    assert_eq!(f.selected(), Some(first));
    f.keys("down");
    assert_eq!(f.selected(), Some(listed[1].clone()));

    // A view's command, run in the document: the workspace shows Edit.
    assert_eq!(f.mode(), ViewMode::Preview);
    f.click("command-palette-row:view.edit");
    assert!(!f.is_open());
    assert_eq!(f.mode(), ViewMode::Edit);
}

/// Only the commands that can run where the keyboard was are listed: no
/// formatting in Preview, formatting in Edit; the palette never lists
/// itself.
#[gpui::test]
fn only_available_commands_are_listed(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.keys("secondary-p");
    let preview = f.listed();
    assert!(preview.contains(&"view.cycle_theme".to_owned()));
    assert!(preview.contains(&"editor.copy".to_owned()), "{preview:?}");
    assert!(preview.contains(&"workspace.toggle_sidebar".to_owned()));
    assert!(!preview.contains(&"editor.bold".to_owned()));
    assert!(!preview.contains(&"navigation.go_to_line".to_owned()));
    assert!(!preview.contains(&"workspace.command_palette".to_owned()));
    f.type_text("bold");
    assert!(!f.listed().contains(&"editor.bold".to_owned()));
    f.keys("escape");

    f.keys("secondary-e");
    assert_eq!(f.mode(), ViewMode::Edit);
    f.keys("secondary-p");
    assert!(f.listed().contains(&"editor.bold".to_owned()));
    f.keys("escape");

    // Sidebar toggled from the palette: run on the workspace, around the
    // editor that had the keyboard.
    f.keys("secondary-p");
    f.type_text("toggle sidebar");
    assert_eq!(f.selected().as_deref(), Some("workspace.toggle_sidebar"));
    f.keys("enter");
    assert!(f.bounds("sidebar").is_some());
}

/// Rows show the keymap's label of each command's primary keys.
#[gpui::test]
fn rows_show_shortcut_labels(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.keys("secondary-p");
    f.type_text("close tab");
    let close = label("secondary-w");
    assert!(
        f.bounds(&format!("command-palette-shortcut:tabs.close:{close}"))
            .is_some(),
        "{close}"
    );
    f.keys("escape");
    f.keys("secondary-p");
    f.type_text("cycle theme");
    let chord = label("secondary-k secondary-t");
    assert!(
        f.bounds(&format!(
            "command-palette-shortcut:view.cycle_theme:{chord}"
        ))
        .is_some(),
        "{chord}"
    );
}

/// With no query, the commands run from the palette come first, newest
/// first.
#[gpui::test]
fn recent_commands_come_first(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.keys("secondary-p");
    // Alphabetical before anything has run: "Close tab" before "Quit".
    let listed = f.listed();
    let at = |id: &str| listed.iter().position(|l| l == id).expect("listed");
    assert!(at("tabs.close") < at("app.quit"), "{listed:?}");
    assert_ne!(f.selected().as_deref(), Some("app.quit"));
    f.keys("escape");
    f.keys("secondary-p");
    f.type_text("cycle theme");
    f.keys("enter");
    f.keys("secondary-p");
    f.type_text("show edit");
    assert_eq!(f.selected().as_deref(), Some("view.edit"));
    f.keys("enter");
    assert_eq!(f.mode(), ViewMode::Edit);

    f.keys("secondary-p");
    let listed = f.listed();
    assert_eq!(listed[..2], ["view.edit", "view.cycle_theme"]);
    assert_eq!(
        listed.iter().filter(|id| *id == "view.edit").count(),
        1,
        "listed once"
    );
    // The one run before it is next.
    f.keys("down enter");
    assert_eq!(f.theme(), ThemeChoice::Dark);
}

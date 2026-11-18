// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Zoom steps and limits, and the zoom commands in a headless window: the
//! Reading view and the Split view's preview scale, the status bar shows
//! the zoom and opens the zoom menu, the keys (with their shifted and
//! keypad forms), Mod+wheel and pinch zoom, and the value is saved in the
//! settings file shortly after the last change and read at the next start.

use std::fs;

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, PinchEvent, ScrollDelta, ScrollEvent,
    TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_settings::{LayerKind, Locations, SettingsFile};

use super::*;
use crate::chrome::WindowChrome;
use crate::metrics::size::document;
use crate::reader::Reader;
use crate::settings::SAVE_DELAY;
use crate::shell::{OpenTarget, ViewMode, Workspace};

#[test]
fn steps_are_ten_points_within_the_settings_range() {
    assert_eq!(limits(), (50, 300));
    assert_eq!(step_in(100), 110);
    assert_eq!(step_out(100), 90);
    // Off the grid: to the next step in the direction asked.
    assert_eq!(step_in(115), 120);
    assert_eq!(step_out(115), 110);
    // The limits hold.
    assert_eq!(step_in(300), 300);
    assert_eq!(step_in(295), 300);
    assert_eq!(step_out(50), 50);
    assert_eq!(step_out(55), 50);
    // From the far ends back in.
    assert_eq!(step_out(300), 290);
    assert_eq!(step_in(50), 60);
    let mut z = DEFAULT;
    for _ in 0..40 {
        z = step_in(z);
    }
    assert_eq!(z, 300);
    for _ in 0..40 {
        z = step_out(z);
    }
    assert_eq!(z, 50);
}

struct Fixture {
    _dir: tempfile::TempDir,
    settings_file: std::path::PathBuf,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..3 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// A config dir with an empty `settings.toml` and a window on a note.
fn fixture(cx: &mut TestAppContext) -> Fixture {
    fixture_with(cx, "version = 1\n")
}

/// A config dir whose `settings.toml` is `settings`, and a window on a
/// note.
fn fixture_with(cx: &mut TestAppContext, settings: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path().join("app"));
    fs::create_dir_all(locations.config_dir()).expect("config dir");
    fs::write(locations.settings_file(), settings).expect("settings written");
    let note = dir.path().join("a.md");
    fs::write(&note, "# Title\n\nSome text to read.\n").expect("note written");
    let file = SettingsFile::load(locations.settings_file(), LayerKind::Global).expect("loads");
    let settings = AppSettings::new(Some(file), None).with_locations(locations.clone());
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, settings);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                Workspace::with_chrome(
                    Some(OpenTarget::new(&note)),
                    WindowChrome::Linux,
                    window,
                    cx,
                )
            })
        })
        .expect("window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        settings_file: locations.settings_file().to_path_buf(),
        _dir: dir,
        workspace,
        vcx,
    }
}

impl Fixture {
    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }

    fn percent(&mut self) -> u32 {
        self.vcx.update(|_, cx| percent(cx))
    }

    /// The settings file once the delayed save has run.
    fn saved(&mut self) -> String {
        self.vcx.executor().advance_clock(SAVE_DELAY);
        draw(&mut self.vcx);
        self.on_disk()
    }

    /// The settings file as it is now.
    fn on_disk(&self) -> String {
        fs::read_to_string(&self.settings_file).expect("settings file")
    }

    fn click(&mut self, selector: &'static str) {
        let at = self
            .vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    /// The middle of the document area.
    fn over_document(&mut self) -> gpui::Point<gpui::Pixels> {
        self.vcx.update(|window, _| window.bounds().size.center())
    }

    fn wheel(&mut self, delta: ScrollDelta, modifiers: Modifiers) {
        let position = self.over_document();
        self.vcx.simulate_event(ScrollEvent {
            position,
            delta,
            modifiers,
            ..ScrollEvent::default()
        });
        draw(&mut self.vcx);
    }

    fn secondary(&mut self) -> Modifiers {
        self.vcx.update(|_, cx| Modifiers::secondary_key(cx))
    }

    fn reader(&mut self) -> Entity<Reader> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().reader().cloned())
            .expect("the tab has a reader")
    }

    /// The zoom the shown document was built at, and the view's scale.
    fn built_zoom(&mut self, reader: &Entity<Reader>) -> (f32, f32) {
        reader.read_with(&self.vcx, |r, _| {
            (r.styles().expect("a document").zoom(), r.scale())
        })
    }

    fn column(&mut self, reader: &Entity<Reader>) -> f32 {
        reader.read_with(&self.vcx, |r, _| r.frame_geometry().1.1)
    }

    fn has(&mut self, selector: &'static str) -> bool {
        self.vcx.debug_bounds(selector).is_some()
    }
}

#[gpui::test]
fn zoom_commands_scale_the_reading_view_and_are_saved(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let reader = f.reader();
    assert_eq!(f.percent(), 100);
    assert_eq!(f.built_zoom(&reader), (1.0, 1.0));
    assert_eq!(f.column(&reader), document::COLUMN_MAX);
    assert!(f.has("status-zoom"), "Preview shows the zoom");

    f.keys("secondary-= secondary-=");
    assert_eq!(f.percent(), 120);
    let saved = f.saved();
    assert!(saved.contains("zoom_percent = 120"), "{saved}");
    assert_eq!(f.built_zoom(&reader), (1.2, 1.2));
    // The readable column grows with the type.
    assert_eq!(f.column(&reader), (document::COLUMN_MAX * 1.2).round());

    f.keys("secondary--");
    assert_eq!(f.percent(), 110);
    assert_eq!(f.built_zoom(&reader), (1.1, 1.1));

    f.keys("secondary-0");
    assert_eq!(f.percent(), 100);
    let saved = f.saved();
    assert!(saved.contains("zoom_percent = 100"), "{saved}");
    assert_eq!(f.built_zoom(&reader), (1.0, 1.0));

    // The upper limit holds.
    for _ in 0..25 {
        f.vcx.update(|_, cx| zoom_in(cx));
    }
    draw(&mut f.vcx);
    assert_eq!(f.percent(), 300);
    assert_eq!(f.built_zoom(&reader), (3.0, 3.0));
    f.saved();

    // The saved value is what a new start reads.
    let reloaded = SettingsFile::load(&f.settings_file, LayerKind::Global).expect("loads");
    let restarted = AppSettings::new(Some(reloaded), None);
    assert_eq!(restarted.settings().appearance.zoom_percent, 300);
}

#[gpui::test]
fn the_split_preview_follows_the_zoom(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.show_mode(ViewMode::Split, cx));
    draw(&mut f.vcx);
    let preview = f
        .workspace
        .read_with(&f.vcx, |ws, cx| {
            ws.active_tab().split().map(|s| s.read(cx).reader().clone())
        })
        .expect("the tab has a split view");
    assert_eq!(f.built_zoom(&preview).0, 1.0);
    // The status bar shows the zoom in Preview only.
    assert!(!f.has("status-zoom"));

    f.vcx.update(|_, cx| zoom_in(cx));
    draw(&mut f.vcx);
    assert_eq!(f.built_zoom(&preview), (1.1, 1.1));
}

#[gpui::test]
fn settings_change_the_reading_view(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let reader = f.reader();
    let set = |f: &mut Fixture, key: &str, value: toml::Value| {
        f.vcx.update(|_, cx| {
            AppSettings::set_value(cx, Target::AllVaults, key, &value).expect("valid")
        });
        draw(&mut f.vcx);
    };
    // A larger preview font scales like the zoom.
    set(
        &mut f,
        "appearance.preview_font_size",
        toml::Value::Float(18.0),
    );
    let (zoom, scale) = f.built_zoom(&reader);
    assert!((zoom - 1.2).abs() < 1e-6 && (scale - 1.2).abs() < 1e-6);
    // The readable width in characters, and off.
    set(
        &mut f,
        "appearance.preview_font_size",
        toml::Value::Float(15.0),
    );
    set(
        &mut f,
        "appearance.readable_line_width_chars",
        toml::Value::Integer(90),
    );
    assert_eq!(
        f.column(&reader),
        (document::COLUMN_MAX * 90.0 / 72.0).round()
    );
    set(
        &mut f,
        "appearance.readable_line_width",
        toml::Value::Boolean(false),
    );
    // The column fills the view between its paddings.
    let width = f
        .vcx
        .update(|window, _| f32::from(window.bounds().size.width));
    assert!(f.column(&reader) > 1000.0 && f.column(&reader) <= width - 2.0 * document::PAD_X);
}

#[gpui::test]
fn the_source_view_and_the_split_editor_follow_the_zoom(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.keys("secondary-e");
    let editor = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    let line = |f: &mut Fixture, editor: &Entity<crate::editor::EditorView>| {
        editor.read_with(&f.vcx, |e, _| e.metrics().line)
    };
    assert_eq!(line(&mut f, &editor), 20.0);

    // Mod+= five times in Edit mode: 150 %, rows 30 px tall.
    f.keys("secondary-= secondary-= secondary-= secondary-= secondary-=");
    assert_eq!(f.percent(), 150);
    assert_eq!(line(&mut f, &editor), 30.0);

    // The Split view's editor side follows too, and Mod+0 resets it.
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.show_mode(ViewMode::Split, cx));
    draw(&mut f.vcx);
    let split_editor = f
        .workspace
        .read_with(&f.vcx, |ws, cx| {
            ws.active_tab().split().map(|s| s.read(cx).editor().clone())
        })
        .expect("the tab has a split view");
    assert_eq!(line(&mut f, &split_editor), 30.0);
    f.keys("secondary-0");
    assert_eq!(f.percent(), 100);
    assert_eq!(line(&mut f, &split_editor), 20.0);
}

#[gpui::test]
fn every_zoom_key_dispatches(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    // Mod+= and Mod++ (Shift+= arrives as `+`, as does the keypad's +
    // on macOS and Windows).
    f.keys("secondary-=");
    assert_eq!(f.percent(), 110);
    f.keys("secondary-+");
    assert_eq!(f.percent(), 120);
    // Mod+- and Mod+0 (the keypad's - and 0 arrive as `-` and `0` on every
    // platform but Linux's `subtract`).
    f.keys("secondary--");
    assert_eq!(f.percent(), 110);
    f.keys("secondary-0");
    assert_eq!(f.percent(), 100);
    // Linux names the keypad's keys `add` and `subtract`; they match the
    // bindings through the character they type.
    if cfg!(target_os = "linux") {
        f.keys("ctrl-add->+ ctrl-add->+");
        assert_eq!(f.percent(), 120);
        // (`ctrl-subtract->-` cannot be written in GPUI's keystroke syntax.)
        let subtract = gpui::Keystroke {
            modifiers: Modifiers::control(),
            key: "subtract".into(),
            key_char: Some("-".into()),
        };
        f.vcx
            .update(|window, cx| window.dispatch_keystroke(subtract, cx));
        draw(&mut f.vcx);
        assert_eq!(f.percent(), 110);
    }
    // In the Source view too.
    f.keys("secondary-e secondary-+");
    assert_eq!(
        f.percent(),
        if cfg!(target_os = "linux") { 120 } else { 110 }
    );
}

#[gpui::test]
fn the_status_bar_zoom_opens_the_zoom_menu(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    assert!(!f.has("zoom-menu"));
    f.click("status-zoom");
    assert!(f.has("zoom-menu"), "a click opens the menu");
    assert!(f.has("zoom-menu-check:100"), "the current level is checked");
    for level in LEVELS {
        let selector: &'static str = Box::leak(format!("zoom-menu-level:{level}").into_boxed_str());
        assert!(f.has(selector), "{selector}");
    }
    let menu = crate::a11y::testing::named(&mut f.vcx, "Zoom");
    assert_eq!(menu.role, Some(gpui::Role::Menu));
    let check = crate::a11y::testing::named(&mut f.vcx, "100%");
    assert_eq!(check.role, Some(gpui::Role::MenuItemCheckBox));
    assert_eq!(check.toggled, Some(gpui::Toggled::True));

    // A level sets the zoom and closes the menu.
    f.click("zoom-menu-level:150");
    assert_eq!(f.percent(), 150);
    assert!(!f.has("zoom-menu"));
    let reader = f.reader();
    assert_eq!(f.built_zoom(&reader), (1.5, 1.5));

    // Zoom in leaves it open, and the zoom off the levels is listed,
    // checked.
    f.click("status-zoom");
    f.click("zoom-menu-item:in");
    assert_eq!(f.percent(), 160);
    assert!(f.has("zoom-menu"));
    assert!(f.has("zoom-menu-check:160"));
    f.click("zoom-menu-item:out");
    assert_eq!(f.percent(), 150);
    // Reset closes it.
    f.click("zoom-menu-item:reset");
    assert_eq!(f.percent(), 100);
    assert!(!f.has("zoom-menu"));

    // From the keyboard: the current level is highlighted; Down and Enter
    // pick the next one, Esc closes without a change.
    f.click("status-zoom");
    f.keys("down enter");
    assert_eq!(f.percent(), 110);
    assert!(!f.has("zoom-menu"));
    f.click("status-zoom");
    f.keys("up up escape");
    assert_eq!(f.percent(), 110);
    assert!(!f.has("zoom-menu"));
    // Home goes to Zoom in.
    f.click("status-zoom");
    f.keys("home enter");
    assert_eq!(f.percent(), 120);
    f.keys("escape");
    assert!(!f.has("zoom-menu"));

    // The menu's level is saved like the keys' zoom.
    let saved = f.saved();
    assert!(saved.contains("zoom_percent = 120"), "{saved}");
}

#[gpui::test]
fn mod_wheel_and_pinch_over_the_document_zoom(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let mods = f.secondary();
    // A plain wheel scrolls; it does not zoom.
    f.wheel(ScrollDelta::Lines(point(0.0, 1.0)), Modifiers::none());
    assert_eq!(f.percent(), 100);
    // Mod+wheel up: one step per notch.
    f.wheel(ScrollDelta::Lines(point(0.0, 1.0)), mods);
    assert_eq!(f.percent(), 110);
    f.wheel(ScrollDelta::Lines(point(0.0, 2.0)), mods);
    assert_eq!(f.percent(), 130);
    // Down zooms out.
    f.wheel(ScrollDelta::Lines(point(0.0, -3.0)), mods);
    assert_eq!(f.percent(), 100);
    // A smooth scroll adds up to whole steps.
    for _ in 0..3 {
        f.wheel(ScrollDelta::Pixels(point(px(0.0), px(20.0))), mods);
    }
    assert_eq!(f.percent(), 110);
    // Turning round drops the rest of a step.
    f.wheel(ScrollDelta::Pixels(point(px(0.0), px(-30.0))), mods);
    assert_eq!(f.percent(), 110);
    f.wheel(ScrollDelta::Pixels(point(px(0.0), px(-30.0))), mods);
    assert_eq!(f.percent(), 100);

    // A pinch: one step per 0.1 of scale.
    let position = f.over_document();
    f.vcx.simulate_event(PinchEvent {
        position,
        delta: 0.25,
        ..PinchEvent::default()
    });
    draw(&mut f.vcx);
    assert_eq!(f.percent(), 120);

    // The Reading view follows.
    let reader = f.reader();
    assert_eq!(f.built_zoom(&reader), (1.2, 1.2));

    // Edit mode zooms the same way.
    f.keys("secondary-e");
    f.wheel(ScrollDelta::Lines(point(0.0, -1.0)), mods);
    assert_eq!(f.percent(), 110);

    // A wheel spin writes the file once, after the last step.
    assert!(!f.on_disk().contains("zoom_percent"), "{}", f.on_disk());
    let saved = f.saved();
    assert!(saved.contains("zoom_percent = 110"), "{saved}");
}

#[gpui::test]
fn the_saved_zoom_applies_at_start(cx: &mut TestAppContext) {
    let mut f = fixture_with(cx, "version = 1\n\n[appearance]\nzoom_percent = 150\n");
    assert_eq!(f.percent(), 150);
    let reader = f.reader();
    assert_eq!(f.built_zoom(&reader), (1.5, 1.5));
    assert!(crate::a11y::testing::has(&mut f.vcx, "Zoom 150%"));
    // A change waits for the delay, or for quitting.
    f.keys("secondary-=");
    assert!(f.on_disk().contains("zoom_percent = 150"));
    f.vcx.update(|_, cx| AppSettings::save_unsaved(cx));
    assert!(
        f.on_disk().contains("zoom_percent = 160"),
        "{}",
        f.on_disk()
    );
}

#[test]
fn gestures_add_up_to_whole_steps() {
    let mut g = Gesture::default();
    assert_eq!(g.add(0.4), 0);
    assert_eq!(g.add(0.4), 0);
    assert_eq!(g.add(0.4), 1);
    assert_eq!(g.add(2.0), 2);
    // Turning round drops the rest.
    assert_eq!(g.add(-0.5), 0);
    assert_eq!(g.add(-0.6), -1);
    assert_eq!(wheel_steps(ScrollDelta::Lines(point(0.0, 1.0))), 1.0);
    assert_eq!(
        wheel_steps(ScrollDelta::Pixels(point(px(0.0), px(-PIXELS_PER_STEP)))),
        -1.0
    );
}

#[test]
fn the_menu_lists_the_levels_and_the_current_zoom() {
    use menu::{Entry, entries};
    let rows = entries(100);
    assert_eq!(&rows[..3], &[Entry::In, Entry::Out, Entry::Reset]);
    assert_eq!(rows.len(), 3 + LEVELS.len());
    let rows = entries(130);
    assert_eq!(rows.len(), 4 + LEVELS.len());
    let levels: Vec<u32> = rows
        .iter()
        .filter_map(|e| match e {
            Entry::Level(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert!(levels.is_sorted());
    assert!(levels.contains(&130));
    // Every level is within the setting's range.
    let (min, max) = limits();
    assert!(LEVELS.iter().all(|l| (min..=max).contains(l)));
}

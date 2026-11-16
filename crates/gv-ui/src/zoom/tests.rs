// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Zoom steps and limits, and the zoom commands in a headless window: the
//! Reading view and the Split view's preview scale, the status bar shows
//! the zoom, and the value is saved in the settings file.

use std::fs;

use gpui::{
    AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_settings::{LayerKind, Locations, SettingsFile};

use super::*;
use crate::chrome::WindowChrome;
use crate::metrics::size::document;
use crate::reader::Reader;
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
    let dir = tempfile::tempdir().expect("temporary folder");
    let locations = Locations::with_root(dir.path().join("app"));
    fs::create_dir_all(locations.config_dir()).expect("config dir");
    fs::write(locations.settings_file(), "version = 1\n").expect("settings written");
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

    fn saved(&self) -> String {
        fs::read_to_string(&self.settings_file).expect("settings file")
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
    assert!(f.saved().contains("zoom_percent = 120"), "{}", f.saved());
    assert_eq!(f.built_zoom(&reader), (1.2, 1.2));
    // The readable column grows with the type.
    assert_eq!(f.column(&reader), (document::COLUMN_MAX * 1.2).round());

    f.keys("secondary--");
    assert_eq!(f.percent(), 110);
    assert_eq!(f.built_zoom(&reader), (1.1, 1.1));

    f.keys("secondary-0");
    assert_eq!(f.percent(), 100);
    assert!(f.saved().contains("zoom_percent = 100"), "{}", f.saved());
    assert_eq!(f.built_zoom(&reader), (1.0, 1.0));

    // The upper limit holds.
    for _ in 0..25 {
        f.vcx.update(|_, cx| zoom_in(cx));
    }
    draw(&mut f.vcx);
    assert_eq!(f.percent(), 300);
    assert_eq!(f.built_zoom(&reader), (3.0, 3.0));

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

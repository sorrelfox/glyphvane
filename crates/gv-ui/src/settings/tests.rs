// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use std::fs;
use std::path::{Path, PathBuf};

use gpui::component::{Theme as ComponentTheme, ThemeMode};
use gpui::{
    AnyWindowHandle, AppContext as _, Hsla, TestAppContext, VisualTestContext, WindowAppearance,
    WindowOptions,
};
use gv_settings::{CURRENT_VERSION, LayerKind, SettingsFile};

use super::{AppSettings, theme_preference};
use crate::appearance::Appearance;
use crate::shell::{OpenTarget, Workspace};
use crate::theme::{PreferredLight, ThemeChoice, ThemeId};

fn settings_with(
    theme: gv_settings::ThemeChoice,
    preferred: gv_settings::PreferredLightTheme,
) -> gv_settings::Settings {
    let mut settings = gv_settings::Settings::default();
    settings.appearance.theme = theme;
    settings.appearance.preferred_light_theme = preferred;
    settings
}

#[test]
fn settings_theme_choices_map_one_to_one() {
    let pairs = [
        (gv_settings::ThemeChoice::System, ThemeChoice::System),
        (gv_settings::ThemeChoice::Light, ThemeChoice::Light),
        (gv_settings::ThemeChoice::SemiLight, ThemeChoice::SemiLight),
        (gv_settings::ThemeChoice::Dark, ThemeChoice::Dark),
    ];
    assert_eq!(pairs.len(), ThemeChoice::ALL.len());
    assert_eq!(pairs.len(), gv_settings::ThemeChoice::VARIANTS.len());
    for (setting, ui) in pairs {
        assert_eq!(ThemeChoice::from(setting), ui);
        assert_eq!(gv_settings::ThemeChoice::from(ui), setting);
    }
    assert_eq!(
        PreferredLight::from(gv_settings::PreferredLightTheme::Light),
        PreferredLight::Light
    );
    assert_eq!(
        PreferredLight::from(gv_settings::PreferredLightTheme::SemiLight),
        PreferredLight::SemiLight
    );
}

#[test]
fn defaults_apply_without_settings_or_override() {
    let defaults = gv_settings::Settings::default();
    assert_eq!(
        theme_preference(None, &defaults),
        (ThemeChoice::System, PreferredLight::Light)
    );
}

#[test]
fn settings_override_defaults() {
    use gv_settings::{PreferredLightTheme as Pref, ThemeChoice as Choice};
    let dark = settings_with(Choice::Dark, Pref::Light);
    assert_eq!(
        theme_preference(None, &dark),
        (ThemeChoice::Dark, PreferredLight::Light)
    );
    let system_sand = settings_with(Choice::System, Pref::SemiLight);
    assert_eq!(
        theme_preference(None, &system_sand),
        (ThemeChoice::System, PreferredLight::SemiLight)
    );
}

#[test]
fn command_line_overrides_settings() {
    use gv_settings::{PreferredLightTheme as Pref, ThemeChoice as Choice};
    let dark = settings_with(Choice::Dark, Pref::SemiLight);
    for choice in ThemeChoice::ALL {
        // The override replaces the choice; the preferred light theme still
        // comes from the settings, since it decides what `system` shows.
        assert_eq!(
            theme_preference(Some(choice), &dark),
            (choice, PreferredLight::SemiLight)
        );
    }
}

#[test]
fn settings_text_resolves_through_the_stack() {
    let text = format!(
        "version = {CURRENT_VERSION}\n[appearance]\ntheme = \"semi-light\"\npreferred_light_theme = \"semi-light\"\n"
    );
    let settings = AppSettings::new(Some(file_with(&text).1), None);
    assert_eq!(
        settings.theme_preference(),
        (ThemeChoice::SemiLight, PreferredLight::SemiLight)
    );
    let overridden = AppSettings::new(Some(file_with(&text).1), Some(ThemeChoice::Dark));
    assert_eq!(
        overridden.theme_preference(),
        (ThemeChoice::Dark, PreferredLight::SemiLight)
    );
}

/// A settings file in a new temporary directory, loaded as the global layer.
fn file_with(text: &str) -> (tempfile::TempDir, SettingsFile) {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("settings.toml");
    fs::write(&path, text).expect("settings file is written");
    let file = SettingsFile::load(&path, LayerKind::Global).expect("settings file loads");
    (dir, file)
}

/// What the file watcher hands over after an edit: the file read again.
fn edit_and_read(path: &Path, text: &str) -> SettingsFile {
    fs::write(path, text).expect("settings file is written");
    SettingsFile::load(path, LayerKind::Global).expect("settings file loads")
}

fn appearance_section(theme: &str) -> String {
    format!("version = {CURRENT_VERSION}\n[appearance]\ntheme = \"{theme}\"\n")
}

fn open_window(cx: &mut TestAppContext, settings: AppSettings) -> AnyWindowHandle {
    cx.update(|cx| {
        crate::init(cx, settings);
        gpui::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(Some(OpenTarget::new("notes/a.md")), window, cx))
        })
        .expect("test window opens")
        .0
    })
}

fn draw(window: AnyWindowHandle, cx: &mut TestAppContext) {
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

/// The theme an open window is drawn with: ours, and the GPUI Component
/// theme that the window root paints the background from.
fn drawn_theme(cx: &mut TestAppContext) -> (ThemeId, ThemeMode, Hsla) {
    cx.update(|cx| {
        let component = ComponentTheme::global(cx);
        (
            Appearance::current_id(cx),
            component.mode,
            component.colors.background,
        )
    })
}

fn expected(id: ThemeId, mode: ThemeMode) -> (ThemeId, ThemeMode, Hsla) {
    (id, mode, id.theme().surface.page.into())
}

#[gpui::test]
fn settings_change_switches_the_theme_of_an_open_window(cx: &mut TestAppContext) {
    let (dir, file) = file_with(&appearance_section("light"));
    let path: PathBuf = file.path().to_path_buf();
    let window = open_window(cx, AppSettings::new(Some(file), None));
    draw(window, cx);
    assert_eq!(drawn_theme(cx), expected(ThemeId::Light, ThemeMode::Light));

    let changes = cx.update(|cx| {
        AppSettings::reload_global(cx, edit_and_read(&path, &appearance_section("dark")))
    });
    assert!(changes.iter().any(|c| c.key == "appearance.theme"));
    draw(window, cx);
    assert_eq!(drawn_theme(cx), expected(ThemeId::Dark, ThemeMode::Dark));

    // Deleting the file goes back to the default, `system`, which follows
    // the test window's light appearance.
    fs::remove_file(&path).expect("settings file is removed");
    let removed = SettingsFile::load(&path, LayerKind::Global).expect("missing file loads");
    cx.update(|cx| AppSettings::reload_global(cx, removed));
    draw(window, cx);
    assert_eq!(cx.update(|cx| Appearance::choice(cx)), ThemeChoice::System);
    assert_eq!(drawn_theme(cx), expected(ThemeId::Light, ThemeMode::Light));
    drop(dir);
}

#[gpui::test]
fn the_theme_chord_saves_the_choice_and_keeps_comments(cx: &mut TestAppContext) {
    let text = format!(
        "# My settings\nversion = {CURRENT_VERSION}\n\n[appearance]\n# Calm.\ntheme = \"semi-light\" # sand\n"
    );
    let (dir, file) = file_with(&text);
    let path = file.path().to_path_buf();
    let window = open_window(cx, AppSettings::new(Some(file), None));

    cx.simulate_keystrokes(window, "secondary-k secondary-t");
    assert_eq!(cx.update(|cx| Appearance::current_id(cx)), ThemeId::Dark);
    let saved = fs::read_to_string(&path).expect("settings file is read");
    assert_eq!(saved, text.replace("\"semi-light\"", "\"dark\""));

    // The watcher's echo of the app's own write changes nothing.
    let echo = SettingsFile::load(&path, LayerKind::Global).expect("settings file loads");
    assert!(
        cx.update(|cx| AppSettings::reload_global(cx, echo))
            .is_empty()
    );
    drop(dir);
}

#[gpui::test]
fn the_command_line_theme_wins_until_the_chord(cx: &mut TestAppContext) {
    let (dir, file) = file_with(&appearance_section("light"));
    let path = file.path().to_path_buf();
    let window = open_window(cx, AppSettings::new(Some(file), Some(ThemeChoice::Dark)));
    assert_eq!(cx.update(|cx| Appearance::current_id(cx)), ThemeId::Dark);

    // A settings change to the theme does not beat the override...
    cx.update(|cx| {
        AppSettings::reload_global(cx, edit_and_read(&path, &appearance_section("semi-light")))
    });
    assert_eq!(cx.update(|cx| Appearance::current_id(cx)), ThemeId::Dark);

    // ...until the chord picks a theme, which is saved and ends it.
    cx.simulate_keystrokes(window, "secondary-k secondary-t");
    assert_eq!(cx.update(|cx| Appearance::choice(cx)), ThemeChoice::System);
    assert_eq!(
        cx.update(|cx| cx.global::<AppSettings>().theme_override()),
        None
    );
    let saved = fs::read_to_string(&path).expect("settings file is read");
    assert!(saved.contains("theme = \"system\""), "{saved}");
    drop(dir);
}

#[gpui::test]
fn without_a_settings_file_the_chord_is_session_only(cx: &mut TestAppContext) {
    let window = open_window(cx, AppSettings::new(None, None));
    cx.simulate_keystrokes(window, "secondary-k secondary-t");
    assert_eq!(cx.update(|cx| Appearance::choice(cx)), ThemeChoice::Light);
    assert_eq!(
        cx.update(|cx| cx.global::<AppSettings>().theme_override()),
        Some(ThemeChoice::Light)
    );
}

#[gpui::test]
fn system_follows_os_appearance_changes(cx: &mut TestAppContext) {
    let text = format!(
        "version = {CURRENT_VERSION}\n[appearance]\npreferred_light_theme = \"semi-light\"\n"
    );
    let (dir, file) = file_with(&text);
    let window = open_window(cx, AppSettings::new(Some(file), None));
    draw(window, cx);
    assert_eq!(
        drawn_theme(cx),
        expected(ThemeId::SemiLight, ThemeMode::Light)
    );
    cx.update(|cx| Appearance::set_os_appearance(cx, WindowAppearance::Dark));
    draw(window, cx);
    assert_eq!(drawn_theme(cx), expected(ThemeId::Dark, ThemeMode::Dark));
    drop(dir);
}

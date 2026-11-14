// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The settings in effect, as a GPUI global, and how they reach the UI.
//!
//! Only the global layer (`settings.toml` in the config dir) is used until
//! vaults exist. Changes apply at once: a reloaded file and a theme picked
//! with the theme chord both update every open window without a restart.
//!
//! **Theme precedence**, highest first:
//!
//! 1. the `--theme` command-line option, for that run only;
//! 2. `appearance.theme` in the settings;
//! 3. the built-in default, `system`.
//!
//! `appearance.preferred_light_theme` comes from the settings (or its
//! default) in every case: it decides what `system` shows in light
//! appearance. Picking a theme with the theme chord writes it to the
//! settings file and ends the command-line override.
//!
//! **Safe mode** (`--safe-mode`, [`AppSettings::with_safe_mode`]) ignores the
//! theme settings for the built-in default (a `--theme` given on the same
//! command line still applies), blocks remote content, and writes nothing:
//! the theme chord changes the theme for that run only.

use std::fmt;

use gpui::{App, Global};
use gv_settings::{Change, Effective, Layer, LayerKind, SettingsFile, SettingsStack, Severity};

use crate::appearance::Appearance;
use crate::theme::{PreferredLight, ThemeChoice};

/// The settings key of the theme choice.
const THEME_KEY: &str = "appearance.theme";

/// The values safe mode puts over every settings file, as a layer of their
/// own.
const SAFE_MODE_VALUES: &str = "[privacy_and_security]\nremote_content = \"block\"\n";

impl From<gv_settings::ThemeChoice> for ThemeChoice {
    fn from(choice: gv_settings::ThemeChoice) -> Self {
        match choice {
            gv_settings::ThemeChoice::System => Self::System,
            gv_settings::ThemeChoice::Light => Self::Light,
            gv_settings::ThemeChoice::SemiLight => Self::SemiLight,
            gv_settings::ThemeChoice::Dark => Self::Dark,
        }
    }
}

impl From<ThemeChoice> for gv_settings::ThemeChoice {
    fn from(choice: ThemeChoice) -> Self {
        match choice {
            ThemeChoice::System => Self::System,
            ThemeChoice::Light => Self::Light,
            ThemeChoice::SemiLight => Self::SemiLight,
            ThemeChoice::Dark => Self::Dark,
        }
    }
}

impl From<gv_settings::PreferredLightTheme> for PreferredLight {
    fn from(theme: gv_settings::PreferredLightTheme) -> Self {
        match theme {
            gv_settings::PreferredLightTheme::Light => Self::Light,
            gv_settings::PreferredLightTheme::SemiLight => Self::SemiLight,
        }
    }
}

/// The theme choice and preferred light theme in effect: the command-line
/// override if there is one, else the settings (whose own fallback is the
/// built-in default).
pub fn theme_preference(
    theme_override: Option<ThemeChoice>,
    settings: &gv_settings::Settings,
) -> (ThemeChoice, PreferredLight) {
    let choice = theme_override.unwrap_or_else(|| settings.appearance.theme.into());
    (choice, settings.appearance.preferred_light_theme.into())
}

/// Logs settings diagnostics: information at `info`, problems as warnings.
pub fn log_diagnostics(effective: &Effective) {
    for diagnostic in effective.diagnostics() {
        match diagnostic.severity {
            Severity::Info => tracing::info!("settings: {diagnostic}"),
            Severity::Warning | Severity::Error => tracing::warn!("settings: {diagnostic}"),
        }
    }
}

/// The application's settings: the layers in effect, the global file they
/// were read from, and the command-line theme override. One per application.
pub struct AppSettings {
    stack: SettingsStack,
    /// The global `settings.toml`, or `None` when it has no location (no
    /// home directory) or could not be read; then settings are not saved.
    global_file: Option<SettingsFile>,
    theme_override: Option<ThemeChoice>,
    safe_mode: bool,
}

impl Global for AppSettings {}

impl fmt::Debug for AppSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppSettings")
            .field(
                "global_file",
                &self.global_file.as_ref().map(SettingsFile::path),
            )
            .field("theme_override", &self.theme_override)
            .field("safe_mode", &self.safe_mode)
            .finish_non_exhaustive()
    }
}

impl AppSettings {
    /// Settings from the global file, if any, with an optional theme override
    /// from the command line.
    pub fn new(global_file: Option<SettingsFile>, theme_override: Option<ThemeChoice>) -> Self {
        let layers = global_file.iter().map(|file| file.layer().clone());
        Self {
            stack: SettingsStack::new(layers),
            global_file,
            theme_override,
            safe_mode: false,
        }
    }

    /// Safe mode for this run: the theme settings give way to the built-in
    /// default, remote content is blocked whatever the settings say, and
    /// nothing is written to the settings file. The blocking values sit in
    /// the highest layer (per-file state, unused so far), so a reloaded
    /// settings file cannot lift them.
    pub fn with_safe_mode(mut self) -> Self {
        self.safe_mode = true;
        self.stack
            .set_layer(Layer::parse(LayerKind::File, None, SAFE_MODE_VALUES));
        self
    }

    /// Whether safe mode is on.
    pub fn safe_mode(&self) -> bool {
        self.safe_mode
    }

    /// The effective settings, with provenance and diagnostics.
    pub fn effective(&self) -> &Effective {
        self.stack.effective()
    }

    /// The settings in effect (all layers merged).
    pub fn settings(&self) -> &gv_settings::Settings {
        self.stack.settings()
    }

    /// The global settings file, if settings are saved.
    pub fn global_file(&self) -> Option<&SettingsFile> {
        self.global_file.as_ref()
    }

    /// The command-line theme override, if still in effect.
    pub fn theme_override(&self) -> Option<ThemeChoice> {
        self.theme_override
    }

    /// The theme choice and preferred light theme in effect.
    pub fn theme_preference(&self) -> (ThemeChoice, PreferredLight) {
        if self.safe_mode {
            return theme_preference(self.theme_override, &gv_settings::Settings::default());
        }
        theme_preference(self.theme_override, self.stack.settings())
    }

    /// Installs the settings as the application's global. Called by
    /// [`crate::init`], which then applies them.
    pub(crate) fn install(self, cx: &mut App) {
        cx.set_global(self);
    }

    /// Live reload: `file` is the global settings file as just read again by
    /// the file watcher. The new values apply at once to every open window.
    /// Returns the settings that changed; nothing changes when the text is
    /// the same as the last one read or written (such as the echo of the
    /// app's own write).
    pub fn reload_global(cx: &mut App, file: SettingsFile) -> Vec<Change> {
        let this = cx.global_mut::<Self>();
        if this
            .global_file
            .as_ref()
            .is_some_and(|current| current.text() == file.text() && current.path() == file.path())
        {
            return Vec::new();
        }
        let changes = this.stack.reload(
            LayerKind::Global,
            Some(file.path().to_path_buf()),
            file.text(),
        );
        this.global_file = Some(file);
        log_diagnostics(this.stack.effective());
        Self::apply(cx);
        changes
    }

    /// The theme chord: switches to the next theme choice and saves it as
    /// `appearance.theme` in the global file (keeping the file's comments and
    /// layout). A command-line override ends, so the saved choice shows.
    /// In safe mode, or when the file cannot be written, the choice applies
    /// for this session only.
    pub fn cycle_theme(cx: &mut App) {
        let next = Appearance::choice(cx).next();
        let this = cx.global_mut::<Self>();
        let saved = match this.global_file.as_mut() {
            Some(_) if this.safe_mode => false,
            Some(file) => match file.set(THEME_KEY, &gv_settings::ThemeChoice::from(next)) {
                Ok(()) => {
                    this.stack.set_layer(file.layer().clone());
                    true
                }
                Err(err) => {
                    tracing::warn!("settings: the theme is not saved: {err}");
                    false
                }
            },
            None => false,
        };
        this.theme_override = if saved { None } else { Some(next) };
        Self::apply(cx);
    }

    /// Pushes the settings in effect into the UI.
    pub(crate) fn apply(cx: &mut App) {
        let (choice, preferred_light) = cx.global::<Self>().theme_preference();
        Appearance::set_preference(cx, choice, preferred_light);
    }
}

#[cfg(test)]
mod tests;

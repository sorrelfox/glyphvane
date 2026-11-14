// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Settings bootstrap: loading the global settings file at startup and
//! watching it for live reload.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::App;
use gv_settings::{LayerKind, Locations, SettingsFile};
use gv_ui::AppSettings;
use gv_ui::settings::log_diagnostics;
use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};

/// How long the file must be quiet before it is read again. Editors and
/// atomic writes produce several events per save.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// The app's locations: below `root` when given (`--config-dir`), else the
/// platform's. `None` when there is no home directory.
pub fn locations(root: Option<&Path>) -> Option<Locations> {
    match root {
        Some(root) => {
            let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
            Some(Locations::with_root(root))
        }
        None => Locations::from_os(),
    }
}

/// Loads the global settings file. A missing file gives the defaults; a file
/// that cannot be read gives the defaults and a warning, and is not written.
/// Diagnostics are logged.
pub fn load(locations: Option<&Locations>) -> Option<SettingsFile> {
    let Some(locations) = locations else {
        tracing::warn!("settings: no home directory, so the default settings are used");
        return None;
    };
    match SettingsFile::load(locations.settings_file(), LayerKind::Global) {
        Ok(file) => Some(file),
        Err(err) => {
            tracing::warn!("settings: cannot read the settings, so the defaults are used: {err}");
            None
        }
    }
}

/// Logs the diagnostics of the settings as loaded.
pub fn report(settings: &AppSettings) {
    log_diagnostics(settings.effective());
}

/// Watches the settings file and applies every change to the running app.
///
/// The folder holding the file is watched rather than the file, because
/// saves usually replace the file (atomic rename) and the file may not exist
/// yet; the folder is created if needed. The watcher thread reads the file
/// and hands it to the UI thread, which applies it.
pub fn watch(path: PathBuf, cx: &mut App) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::warn!(
            "settings: cannot create {}, so changes to the settings file need a restart: {err}",
            dir.display()
        );
        return;
    }
    let (sender, mut receiver) = mpsc::unbounded::<SettingsFile>();
    let name = name.to_owned();
    let file_path = path.clone();
    let debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
        match result {
            Ok(events) => {
                let touched = events
                    .iter()
                    .flat_map(|event| &event.paths)
                    .any(|p| p.file_name() == Some(name.as_os_str()));
                if !touched {
                    return;
                }
                match SettingsFile::load(&file_path, LayerKind::Global) {
                    // The receiver is gone only when the app is quitting.
                    Ok(file) => drop(sender.unbounded_send(file)),
                    Err(err) => tracing::warn!("settings: cannot read the changed settings: {err}"),
                }
            }
            Err(errors) => {
                for err in errors {
                    tracing::warn!("settings: file watcher: {err}");
                }
            }
        }
    });
    let mut debouncer = match debouncer {
        Ok(debouncer) => debouncer,
        Err(err) => {
            tracing::warn!(
                "settings: cannot watch the settings file, so changes need a restart: {err}"
            );
            return;
        }
    };
    if let Err(err) = debouncer.watch(dir, RecursiveMode::NonRecursive) {
        tracing::warn!(
            "settings: cannot watch {}, so changes need a restart: {err}",
            dir.display()
        );
        return;
    }
    cx.spawn(async move |cx| {
        // The watcher lives as long as this task, which lives as long as the
        // application.
        let _debouncer = debouncer;
        while let Some(file) = receiver.next().await {
            let changes = cx.update(|cx| AppSettings::reload_global(cx, file));
            for change in changes.iter().filter(|c| c.value_changed()) {
                tracing::info!(
                    "settings: {} = {} (from {})",
                    change.key,
                    change.new.value,
                    change.new.source
                );
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use gv_settings::{PreferredLightTheme, RemoteContent};
    use gv_ui::ThemeChoice;
    use gv_ui::theme::PreferredLight;

    use super::*;

    const USER_SETTINGS: &str = "\
[appearance]
theme = \"dark\"
preferred_light_theme = \"semi-light\"

[privacy_and_security]
remote_content = \"allow\"
";

    fn user_file(dir: &Path) -> SettingsFile {
        let path = dir.join("settings.toml");
        std::fs::write(&path, USER_SETTINGS).expect("written");
        SettingsFile::load(&path, LayerKind::Global).expect("loaded")
    }

    #[test]
    fn safe_mode_uses_the_default_theme_and_blocks_remote_content() {
        let dir = tempfile::tempdir().expect("temporary folder");
        let normal = AppSettings::new(Some(user_file(dir.path())), None);
        assert_eq!(
            normal.theme_preference(),
            (ThemeChoice::Dark, PreferredLight::SemiLight)
        );
        assert_eq!(
            normal
                .effective()
                .settings()
                .privacy_and_security
                .remote_content,
            RemoteContent::Allow
        );

        let safe = AppSettings::new(Some(user_file(dir.path())), None).with_safe_mode();
        assert!(safe.safe_mode());
        let defaults = gv_settings::Settings::default();
        assert_eq!(
            safe.theme_preference(),
            (
                defaults.appearance.theme.into(),
                defaults.appearance.preferred_light_theme.into()
            )
        );
        assert_eq!(
            defaults.appearance.preferred_light_theme,
            PreferredLightTheme::Light
        );
        assert_eq!(
            safe.effective()
                .settings()
                .privacy_and_security
                .remote_content,
            RemoteContent::Block
        );
        // The other settings still come from the file, which is unchanged.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("settings.toml")).expect("readable"),
            USER_SETTINGS
        );
        assert_eq!(
            safe.global_file().map(SettingsFile::text),
            Some(USER_SETTINGS)
        );
    }

    #[test]
    fn a_theme_on_the_command_line_still_applies_in_safe_mode() {
        let safe = AppSettings::new(None, Some(ThemeChoice::Light)).with_safe_mode();
        assert_eq!(safe.theme_preference().0, ThemeChoice::Light);
        assert_eq!(
            safe.effective()
                .settings()
                .privacy_and_security
                .remote_content,
            RemoteContent::Block
        );
    }
}

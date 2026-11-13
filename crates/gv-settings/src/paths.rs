// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Where Glyphvane keeps its files.
//!
//! | | Linux | macOS | Windows |
//! |---|---|---|---|
//! | Config | `~/.config/glyphvane/` | `~/Library/Application Support/io.github.sorrelfox.Glyphvane/` | `%APPDATA%\sorrelfox\Glyphvane\` |
//! | Data | `~/.local/share/glyphvane/` | same as config | `%LOCALAPPDATA%\sorrelfox\Glyphvane\` |
//! | Cache | `~/.cache/glyphvane/` | `~/Library/Caches/io.github.sorrelfox.Glyphvane/` | `%LOCALAPPDATA%\sorrelfox\Glyphvane\cache\` |
//! | Logs | `~/.local/state/glyphvane/log/` | `~/Library/Logs/Glyphvane/` | `%LOCALAPPDATA%\sorrelfox\Glyphvane\logs\` |
//!
//! The XDG variables (`XDG_CONFIG_HOME` and the rest) are honoured on Linux.

use std::path::{Path, PathBuf};

/// The reverse-DNS qualifier, organization and application name.
const QUALIFIER: &str = "io.github";
const ORGANIZATION: &str = "sorrelfox";
const APPLICATION: &str = "Glyphvane";

/// The name of the settings file in the config dir and in vault overlays.
pub const SETTINGS_FILE: &str = "settings.toml";

/// The folder inside a vault that holds shared settings.
pub const IN_VAULT_FOLDER: &str = ".glyphvane";

/// The app's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locations {
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
}

/// A vault id that is not a single safe path component.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a valid vault id")]
pub struct InvalidVaultId(pub String);

impl Locations {
    /// The platform locations, or `None` when the home directory cannot be
    /// found.
    pub fn from_os() -> Option<Self> {
        let dirs = directories::ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)?;
        Some(Self::from_project_dirs(&dirs))
    }

    #[cfg(target_os = "windows")]
    fn from_project_dirs(dirs: &directories::ProjectDirs) -> Self {
        // `directories` adds `config` and `data` below the app folder on
        // Windows; the plan's layout uses the app folders themselves.
        let roaming = parent_or_self(dirs.config_dir());
        let local = parent_or_self(dirs.data_local_dir());
        Self {
            config: roaming,
            cache: local.join("cache"),
            logs: local.join("logs"),
            data: local,
        }
    }

    #[cfg(target_os = "macos")]
    fn from_project_dirs(dirs: &directories::ProjectDirs) -> Self {
        let logs = directories::BaseDirs::new()
            .map(|b| b.home_dir().join("Library/Logs").join(APPLICATION))
            .unwrap_or_else(|| dirs.data_dir().join("logs"));
        Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
            logs,
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn from_project_dirs(dirs: &directories::ProjectDirs) -> Self {
        let logs = dirs
            .state_dir()
            .map(|d| d.join("log"))
            .unwrap_or_else(|| dirs.data_dir().join("log"));
        Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
            logs,
        }
    }

    /// Everything below `root` (`config/`, `data/`, `cache/`, `logs/`), for
    /// tests and portable setups.
    pub fn with_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        Self {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
            logs: root.join("logs"),
        }
    }

    /// Settings, keymap, themes and user dictionaries.
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// Journals, snapshots and per-vault state.
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// The index, images and highlight caches.
    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    /// Log files.
    pub fn log_dir(&self) -> &Path {
        &self.logs
    }

    /// The global `settings.toml`.
    pub fn settings_file(&self) -> PathBuf {
        self.config.join(SETTINGS_FILE)
    }

    /// `keymap.toml`: user key bindings layered over the platform defaults.
    pub fn keymap_file(&self) -> PathBuf {
        self.config.join("keymap.toml")
    }

    /// The folder of user themes (`*.toml`).
    pub fn themes_dir(&self) -> PathBuf {
        self.config.join("themes")
    }

    /// The folder of user dictionaries (one word per line).
    pub fn dictionaries_dir(&self) -> PathBuf {
        self.config.join("dictionaries")
    }

    /// The per-vault folder in the data dir.
    pub fn vault_data_dir(&self, vault_id: &str) -> Result<PathBuf, InvalidVaultId> {
        check_vault_id(vault_id)?;
        Ok(self.data.join("vaults").join(vault_id))
    }

    /// The vault's settings overlay in the data dir ("this vault").
    pub fn vault_settings_file(&self, vault_id: &str) -> Result<PathBuf, InvalidVaultId> {
        Ok(self.vault_data_dir(vault_id)?.join(SETTINGS_FILE))
    }
}

/// The shared settings file inside a vault: `.glyphvane/settings.toml`.
pub fn in_vault_settings_file(vault_root: &Path) -> PathBuf {
    vault_root.join(IN_VAULT_FOLDER).join(SETTINGS_FILE)
}

#[cfg(target_os = "windows")]
fn parent_or_self(path: &Path) -> PathBuf {
    path.parent().unwrap_or(path).to_path_buf()
}

/// A vault id must be one plain path component: ASCII letters, digits, `-`
/// and `_`, at most 128 bytes.
fn check_vault_id(id: &str) -> Result<(), InvalidVaultId> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(InvalidVaultId(id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_override_places_everything_below_it() {
        let loc = Locations::with_root("/tmp/gv-test");
        assert_eq!(
            loc.settings_file(),
            Path::new("/tmp/gv-test/config/settings.toml")
        );
        assert_eq!(
            loc.keymap_file(),
            Path::new("/tmp/gv-test/config/keymap.toml")
        );
        assert_eq!(
            loc.vault_settings_file("a1b2"),
            Ok(PathBuf::from("/tmp/gv-test/data/vaults/a1b2/settings.toml"))
        );
        assert_eq!(loc.log_dir(), Path::new("/tmp/gv-test/logs"));
    }

    #[test]
    fn vault_ids_must_be_plain_components() {
        let loc = Locations::with_root("/r");
        for bad in ["", "..", "a/b", "a\\b", "é", "a b", &"x".repeat(129)] {
            assert!(loc.vault_data_dir(bad).is_err(), "{bad:?}");
        }
        assert!(loc.vault_data_dir("Vault_01-x").is_ok());
    }

    #[test]
    fn in_vault_file_is_in_the_dot_folder() {
        assert_eq!(
            in_vault_settings_file(Path::new("/notes")),
            Path::new("/notes/.glyphvane/settings.toml")
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_layout_matches_the_plan() {
        let Some(loc) = Locations::from_os() else {
            return;
        };
        let ends = |p: &Path, suffix: &str| p.to_string_lossy().ends_with(suffix);
        assert!(
            ends(loc.config_dir(), "/glyphvane"),
            "{:?}",
            loc.config_dir()
        );
        assert!(ends(loc.data_dir(), "/glyphvane"), "{:?}", loc.data_dir());
        assert!(ends(loc.cache_dir(), "/glyphvane"), "{:?}", loc.cache_dir());
        assert!(ends(loc.log_dir(), "/glyphvane/log"), "{:?}", loc.log_dir());
    }
}

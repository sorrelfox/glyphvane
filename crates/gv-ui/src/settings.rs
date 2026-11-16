// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The settings in effect, as a GPUI global, and how they reach the UI.
//!
//! The global layer (`settings.toml` in the config dir) is always used. The
//! layers of one vault are used on top of it: its overlay in the app data
//! dir ("this vault", written by the settings window) and its in-vault
//! `.glyphvane/settings.toml` (read only; it can only tighten
//! security-relevant settings). The vault is the one of the main window
//! focused last ([`AppSettings::open_vault`]); every window uses it until
//! settings are resolved per window. Changes apply at once: a reloaded file,
//! a theme picked with the theme chord and an edit in the settings window
//! all update every open window without a restart.
//!
//! **Vault ids** (the overlay's folder name in the data dir) are
//! [`vault_id`]: the vault folder's name and a hash of its absolute path.
//! The vault model has no ids of its own yet.
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
use std::path::{Path, PathBuf};

use gpui::{App, Global};
use gv_settings::{
    Change, Diagnostic, Effective, Layer, LayerKind, Locations, Scope, SettingsFile, SettingsStack,
    Severity,
};

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
    /// Where vault overlays live; `None` without a home directory (and in
    /// tests), when vault overlays are kept in memory only.
    locations: Option<Locations>,
    /// The vault whose layers are in use.
    vault: Option<VaultSettings>,
}

impl Global for AppSettings {}

/// Which settings an edit changes: the settings window's scope switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Target {
    /// The global `settings.toml` ("All vaults").
    #[default]
    AllVaults,
    /// The open vault's overlay in the app data dir ("this vault").
    Vault,
}

impl Target {
    /// The layer an edit goes to.
    pub fn layer(self) -> LayerKind {
        match self {
            Self::AllVaults => LayerKind::Global,
            Self::Vault => LayerKind::Vault,
        }
    }
}

/// The settings layers of one vault.
#[derive(Debug, Clone)]
pub struct VaultSettings {
    root: PathBuf,
    name: String,
    id: String,
    /// The overlay in the app data dir; `None` without locations.
    overlay: Option<SettingsFile>,
}

impl VaultSettings {
    /// The vault's root folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The vault's name: its folder's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The vault id that keys its overlay.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The overlay file, when settings are saved.
    pub fn overlay(&self) -> Option<&SettingsFile> {
        self.overlay.as_ref()
    }
}

/// The id of the vault at `root`: its folder name (letters, digits, `-`
/// and `_` only, at most 48 of them) and 16 hex digits of an FNV-1a hash of
/// its absolute path, so two vaults with the same folder name differ.
pub fn vault_id(root: &Path) -> String {
    let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in absolute.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let name: String = absolute
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if name.is_empty() {
        format!("vault-{hash:016x}")
    } else {
        format!("{name}-{hash:016x}")
    }
}

/// Why an edit from the settings window was not made, as a sentence for
/// the row's inline error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRefused(pub String);

impl fmt::Display for EditRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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
            locations: None,
            vault: None,
        }
    }

    /// Where vault overlays are read and written.
    pub fn with_locations(mut self, locations: Locations) -> Self {
        self.locations = Some(locations);
        self
    }

    /// The app's locations, when known.
    pub fn locations(&self) -> Option<&Locations> {
        self.locations.as_ref()
    }

    /// The vault whose layers are in use.
    pub fn vault(&self) -> Option<&VaultSettings> {
        self.vault.as_ref()
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

    /// Whether viewer mode is on (`general.viewer_mode`): documents open
    /// read-only, and Edit and Split are unavailable, except in a tab
    /// opened for an edit someone waits for (`--wait`) and in untitled
    /// documents.
    pub fn viewer_mode(cx: &App) -> bool {
        cx.try_global::<Self>()
            .is_some_and(|this| this.settings().general.viewer_mode)
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

    /// Uses the layers of the vault at `root`: its overlay in the app data
    /// dir and its in-vault `.glyphvane/settings.toml`. Nothing happens when
    /// that vault is already in use. Changed settings apply at once.
    pub fn open_vault(cx: &mut App, root: &Path) {
        let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let this = cx.global_mut::<Self>();
        if this.vault.as_ref().is_some_and(|v| v.root == root) {
            return;
        }
        let id = vault_id(&root);
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let overlay = this
            .locations
            .as_ref()
            .and_then(|l| l.vault_settings_file(&id).ok())
            .and_then(|path| match SettingsFile::load(path, LayerKind::Vault) {
                Ok(file) => Some(file),
                Err(err) => {
                    tracing::warn!("settings: cannot read the vault settings: {err}");
                    None
                }
            });
        let in_vault = SettingsFile::load(
            gv_settings::in_vault_settings_file(&root),
            LayerKind::InVault,
        )
        .map(|file| file.layer().clone())
        .unwrap_or_else(|err| {
            tracing::warn!("settings: cannot read the vault's shared settings: {err}");
            Layer::empty(LayerKind::InVault)
        });
        let overlay_layer = overlay
            .as_ref()
            .map_or_else(|| Layer::empty(LayerKind::Vault), |f| f.layer().clone());
        this.stack.set_layer(in_vault);
        this.stack.set_layer(overlay_layer);
        this.vault = Some(VaultSettings {
            root,
            name,
            id,
            overlay,
        });
        log_diagnostics(this.stack.effective());
        Self::apply(cx);
        cx.refresh_windows();
    }

    /// The settings as `target` sees them: the global file over the
    /// defaults for all vaults, everything in effect for the vault.
    pub fn effective_for(&self, target: Target) -> Effective {
        match target {
            Target::AllVaults => gv_settings::resolve(self.stack.layer(LayerKind::Global)),
            Target::Vault => self.stack.effective().clone(),
        }
    }

    /// Whether the layer of `target` sets `key` itself.
    pub fn sets(&self, target: Target, key: &str) -> bool {
        self.stack
            .layer(target.layer())
            .is_some_and(|layer| layer.get(key).is_some())
    }

    /// The value `target`'s own layer gives `key`, if it sets it.
    pub fn own_value(&self, target: Target, key: &str) -> Option<toml::Value> {
        self.stack
            .layer(target.layer())
            .and_then(|layer| layer.get(key))
            .map(|entry| entry.value.clone())
    }

    /// The problems of `key` in the layers `target` shows: a value that is
    /// invalid or was ignored.
    pub fn problems(&self, target: Target, key: &str) -> Vec<Diagnostic> {
        let layers: &[LayerKind] = match target {
            Target::AllVaults => &[LayerKind::Global],
            Target::Vault => &[LayerKind::Global, LayerKind::InVault, LayerKind::Vault],
        };
        self.stack
            .effective()
            .diagnostics()
            .iter()
            .filter(|d| {
                d.severity >= Severity::Warning
                    && d.key.as_deref() == Some(key)
                    && layers.contains(&d.layer)
            })
            .cloned()
            .collect()
    }

    /// The file edits of `target` go to, if settings are saved.
    pub fn file_of(&self, target: Target) -> Option<&SettingsFile> {
        match target {
            Target::AllVaults => self.global_file.as_ref(),
            Target::Vault => self.vault.as_ref()?.overlay.as_ref(),
        }
    }

    /// Sets `key` to `value` for `target` and applies it at once. The value
    /// is checked against the schema first. It is saved in the target's
    /// file (keeping the file's comments and layout); in safe mode, or when
    /// there is no file, it applies for this session only.
    pub fn set_value(
        cx: &mut App,
        target: Target,
        key: &str,
        value: &toml::Value,
    ) -> Result<(), EditRefused> {
        let spec = gv_settings::spec(key)
            .ok_or_else(|| EditRefused(format!("`{key}` is not a setting")))?;
        if spec.scope == Scope::GlobalOnly && target == Target::Vault {
            return Err(EditRefused(
                "This setting applies to all vaults only.".to_owned(),
            ));
        }
        let value = spec
            .check_owned(value)
            .map_err(|e| EditRefused(sentence(&e.message)))?;
        let this = cx.global_mut::<Self>();
        if target == Target::Vault && this.vault.is_none() {
            return Err(EditRefused("No vault is open.".to_owned()));
        }
        this.edit(
            target,
            |file| file.set_value(key, &value),
            |text| gv_settings::set_value(text, key, &value),
        )?;
        if key == THEME_KEY {
            this.theme_override = None;
        }
        Self::after_edit(cx);
        Ok(())
    }

    /// Removes `key` from the layer of `target`, so the layer below applies
    /// again.
    pub fn reset_value(cx: &mut App, target: Target, key: &str) -> Result<(), EditRefused> {
        let this = cx.global_mut::<Self>();
        if target == Target::Vault && this.vault.is_none() {
            return Err(EditRefused("No vault is open.".to_owned()));
        }
        this.edit(
            target,
            |file| file.remove(key),
            |text| gv_settings::remove_value(text, key),
        )?;
        if key == THEME_KEY {
            this.theme_override = None;
        }
        Self::after_edit(cx);
        Ok(())
    }

    /// Edits the target's file, or its layer in memory in safe mode and
    /// when there is no file.
    fn edit(
        &mut self,
        target: Target,
        in_file: impl FnOnce(&mut SettingsFile) -> Result<(), gv_settings::SettingsError>,
        in_memory: impl FnOnce(&str) -> Result<String, gv_settings::EditError>,
    ) -> Result<(), EditRefused> {
        let kind = target.layer();
        let safe_mode = self.safe_mode;
        let file = match target {
            Target::AllVaults => self.global_file.as_mut(),
            Target::Vault => self.vault.as_mut().and_then(|v| v.overlay.as_mut()),
        };
        match file {
            Some(file) if !safe_mode => {
                in_file(file).map_err(|e| EditRefused(e.to_string()))?;
                let layer = file.layer().clone();
                self.stack.set_layer(layer);
            }
            _ => {
                let text = self
                    .stack
                    .layer(kind)
                    .map(|l| l.text().to_owned())
                    .unwrap_or_default();
                let text = in_memory(&text).map_err(|e| EditRefused(e.to_string()))?;
                let path = self
                    .stack
                    .layer(kind)
                    .and_then(|l| l.path().map(Path::to_path_buf));
                self.stack
                    .set_layer(Layer::parse(kind, path.as_deref(), &text));
            }
        }
        Ok(())
    }

    fn after_edit(cx: &mut App) {
        Self::apply(cx);
        cx.refresh_windows();
    }

    /// Pushes the settings in effect into the UI.
    pub(crate) fn apply(cx: &mut App) {
        let (choice, preferred_light) = cx.global::<Self>().theme_preference();
        Appearance::set_preference(cx, choice, preferred_light);
    }
}

/// "expected a boolean" → "Expected a boolean.", for inline errors.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    let mut text: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !text.ends_with('.') {
        text.push('.');
    }
    text
}

#[cfg(test)]
mod tests;

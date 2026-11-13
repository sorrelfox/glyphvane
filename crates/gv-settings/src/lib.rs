// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Settings: the typed schema, layers with provenance, format-preserving
//! TOML files, versioned migrations, validation and file locations.
//!
//! # Layers
//!
//! Values are layered, lowest priority first ([`LayerKind`]):
//!
//! 1. built-in defaults;
//! 2. the global `settings.toml` in the config dir ("all vaults");
//! 3. the in-vault `.glyphvane/settings.toml`, shared through Git. It is
//!    untrusted: for security-relevant settings ([`Spec::tighten`]) its
//!    values apply only when they make the result stricter;
//! 4. the vault's overlay in the app data dir, keyed by vault id ("this
//!    vault");
//! 5. per-file state, such as a profile override.
//!
//! [`resolve`] combines them; [`Effective::source`] says which layer set each
//! value, for the settings window's "all vaults / this vault" control.
//! [`SettingsStack`] keeps the layers for live reload and reports the changed
//! keys.
//!
//! # Files
//!
//! Files are TOML with `version = N` at the top. [`SettingsFile`] loads one
//! (upgrading older versions with a backup), edits single keys without
//! touching the rest of the text, and writes atomically. Unknown keys are kept
//! and reported as warnings. Invalid values are reported with file, line,
//! column and key ([`Diagnostic`]) and fall back to the layer below; they never
//! stop the other keys from loading.
//!
//! Related files that are not part of this crate's schema: `keymap.toml`
//! ([`Locations::keymap_file`]), user themes ([`Locations::themes_dir`]) and
//! user dictionaries ([`Locations::dictionaries_dir`]).

mod diagnostic;
mod edit;
mod file;
mod json_schema;
pub mod keymap;
mod layer;
mod migrate;
mod paths;
mod resolve;
mod schema;
mod spec;

// The value and document types used in this crate's API.
pub use toml;
pub use toml_edit;

pub use diagnostic::{Diagnostic, DiagnosticKind, Position, Severity};
pub use edit::{EditError, remove_value, set_value};
pub use file::{SettingsError, SettingsFile, atomic_write, backup_path};
pub use json_schema::{SCHEMA_ID, json_schema, json_schema_string};
pub use keymap::{
    CommandId, CommandRegistry, ContextSet, KeySequence, Keymap, KeymapDiagnostic, KeymapIssue,
    Keystroke, Platform, Resolution, UserKeymap,
};
pub use layer::{Entry, Layer, LayerKind};
pub use migrate::{
    CURRENT_VERSION, Migrated, Migration, MigrationError, UNVERSIONED, migrate, migrations,
    rename_key,
};
pub use paths::{
    IN_VAULT_FOLDER, InvalidVaultId, Locations, SETTINGS_FILE, in_vault_settings_file,
};
pub use resolve::{Change, Effective, Resolved, SettingsStack, resolve};
pub use schema::{
    Appearance, Editor, EmojiInsert, ExternalFiles, Files, FootnotePlacement, General,
    HighContrast, LinkEncoding, Links, NewLinkFormat, OpenFilesIn, PreferredLightTheme, Preview,
    PrivacyAndSecurity, ProfileId, ProfileOverride, Profiles, RemoteContent, Settings, Spelling,
    ThemeChoice, Vault, WorkspaceTrust,
};
pub use spec::{
    Kind, Merge, Scope, Section, SettingType, Spec, Tighten, ValueError, sections, spec, specs,
};

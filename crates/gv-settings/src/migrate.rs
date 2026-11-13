// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! File versions and migrations.
//!
//! Every settings file carries `version = N` at the top. A file without the
//! key is version 1. Loading an older file runs the migration chain from its
//! version up to [`CURRENT_VERSION`]; the caller keeps a backup of the old
//! file before writing the result ([`crate::SettingsFile::load`]). A file
//! from a newer version is never migrated or written.

use std::ops::Range;

use toml_edit::{DocumentMut, Item, TableLike};

/// The settings file version this build reads and writes.
pub const CURRENT_VERSION: u32 = 1;

/// The version of a file that has no `version` key.
pub const UNVERSIONED: u32 = 1;

/// One step of the chain: upgrades a file from `from` to `from + 1`.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// The version this step reads.
    pub from: u32,
    /// What the step changes, for the log.
    pub description: &'static str,
    /// Rewrites the document in place. Comments and formatting of untouched
    /// keys survive because the document is edited, not re-serialized.
    pub apply: fn(&mut DocumentMut) -> Result<(), String>,
}

/// The real migration chain. Version 1 is the first version, so it is empty.
const MIGRATIONS: &[Migration] = &[];

/// The migration chain of this build.
pub fn migrations() -> &'static [Migration] {
    MIGRATIONS
}

/// A migrated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrated {
    /// The new file contents.
    pub text: String,
    /// The version the file had.
    pub from: u32,
    /// The version it has now.
    pub to: u32,
}

/// Why a file could not be migrated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrationError {
    /// The file is not valid TOML.
    #[error("{message}")]
    Syntax {
        message: String,
        span: Option<Range<usize>>,
    },
    /// `version` is not a positive integer.
    #[error("`version` must be a whole number from 1 up, found {found}")]
    InvalidVersion {
        found: String,
        span: Option<Range<usize>>,
    },
    /// The file comes from a newer version of the app.
    #[error(
        "this file was written by a newer version of Glyphvane (settings version {found}; \
         this version reads up to {supported}). It is read as far as possible but not \
         changed: settings edited here are not saved to it"
    )]
    Newer { found: u32, supported: u32 },
    /// No step upgrades from this version.
    #[error("no migration from settings version {from}")]
    MissingStep { from: u32 },
    /// A step failed.
    #[error("upgrading from settings version {from} failed: {message}")]
    Step { from: u32, message: String },
}

/// Reads the `version` key: `Ok(None)` when it is missing.
pub(crate) fn read_version(root: &dyn TableLike) -> Result<Option<u32>, MigrationError> {
    let Some(item) = root.get("version") else {
        return Ok(None);
    };
    let invalid = || MigrationError::InvalidVersion {
        found: match item {
            Item::Value(v) => crate::spec::describe(v),
            _ => "a table".to_owned(),
        },
        span: item.span(),
    };
    match item.as_integer() {
        Some(n) if n >= 1 => u32::try_from(n).map(Some).map_err(|_| invalid()),
        _ => Err(invalid()),
    }
}

/// Upgrades `text` to `target` with `chain`. Returns `Ok(None)` when the file
/// is already at `target`.
pub fn migrate(
    text: &str,
    chain: &[Migration],
    target: u32,
) -> Result<Option<Migrated>, MigrationError> {
    let mut doc: DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| MigrationError::Syntax {
                message: e.message().to_owned(),
                span: e.span(),
            })?;
    let had_key = doc.as_table().contains_key("version");
    let from = read_version(doc.as_table())?.unwrap_or(UNVERSIONED);
    if from > target {
        return Err(MigrationError::Newer {
            found: from,
            supported: target,
        });
    }
    if from == target {
        return Ok(None);
    }
    for version in from..target {
        let step = chain
            .iter()
            .find(|m| m.from == version)
            .ok_or(MigrationError::MissingStep { from: version })?;
        (step.apply)(&mut doc).map_err(|message| MigrationError::Step {
            from: version,
            message,
        })?;
    }
    let text = if had_key {
        set_version(&mut doc, target);
        doc.to_string()
    } else {
        format!("version = {target}\n{doc}")
    };
    Ok(Some(Migrated {
        text,
        from,
        to: target,
    }))
}

fn set_version(doc: &mut DocumentMut, version: u32) {
    if let Some(Item::Value(old)) = doc.as_table_mut().get_mut("version") {
        let decor = old.decor().clone();
        let mut new = toml_edit::Value::from(i64::from(version));
        *new.decor_mut() = decor;
        *old = new;
    }
}

/// A migration helper: moves the value at `from` (`section.key`) to `to`,
/// keeping its comments and formatting. The key moves to the end of its
/// section; the target section is created when it is missing. When `to`
/// already has a value, it wins and `from` is dropped. Returns whether
/// `from` was there.
pub fn rename_key(doc: &mut DocumentMut, from: &str, to: &str) -> Result<bool, String> {
    let (from_section, from_key) = split(from)?;
    let (to_section, to_key) = split(to)?;
    let Some(source) = doc.get_mut(from_section).and_then(Item::as_table_like_mut) else {
        return Ok(false);
    };
    let Some(decor) = source
        .get_key_value(from_key)
        .map(|(key, _)| key.leaf_decor().clone())
    else {
        return Ok(false);
    };
    let Some(item) = source.remove(from_key) else {
        return Ok(false);
    };
    if !doc.as_table().contains_key(to_section) {
        let mut table = toml_edit::Table::new();
        table.decor_mut().set_prefix("\n");
        doc.insert(to_section, Item::Table(table));
    }
    let target = doc
        .get_mut(to_section)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| format!("`{to_section}` is not a table"))?;
    let mut key = toml_edit::Key::new(to_key);
    *key.leaf_decor_mut() = decor;
    target.entry_format(&key).or_insert(item);
    Ok(true)
}

fn split(path: &str) -> Result<(&str, &str), String> {
    path.split_once('.')
        .ok_or_else(|| format!("`{path}` is not a `section.key` path"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_file_needs_nothing() {
        let text = "version = 1\n[appearance]\ntheme = \"dark\"\n";
        assert_eq!(migrate(text, migrations(), CURRENT_VERSION), Ok(None));
        assert_eq!(migrate("", migrations(), CURRENT_VERSION), Ok(None));
    }

    #[test]
    fn newer_file_is_refused() {
        let err = migrate("version = 7\n", migrations(), CURRENT_VERSION);
        assert_eq!(
            err,
            Err(MigrationError::Newer {
                found: 7,
                supported: CURRENT_VERSION
            })
        );
        let message = err.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("newer version of Glyphvane"), "{message}");
        assert!(message.contains("not changed"), "{message}");
    }

    #[test]
    fn invalid_versions_are_reported() {
        for text in ["version = 0\n", "version = \"1\"\n", "version = -3\n"] {
            assert!(
                matches!(
                    migrate(text, migrations(), CURRENT_VERSION),
                    Err(MigrationError::InvalidVersion { .. })
                ),
                "{text}"
            );
        }
    }
}

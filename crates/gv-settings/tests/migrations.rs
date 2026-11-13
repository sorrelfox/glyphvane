// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Migrations against fixture files, with a test-only chain v1 → v2 → v3:
//! v2 renamed `appearance.ui_font_size` to `appearance.interface_font_size`,
//! v3 renamed the `open_files_in` value `preview` to `reading`.

use std::fs;
use std::path::{Path, PathBuf};

use gv_settings::toml_edit::{DocumentMut, Item, Value};
use gv_settings::{
    DiagnosticKind, LayerKind, Migration, MigrationError, SettingsError, SettingsFile, migrate,
    rename_key,
};

const TARGET: u32 = 3;

fn rename_ui_font_size(doc: &mut DocumentMut) -> Result<(), String> {
    rename_key(
        doc,
        "appearance.ui_font_size",
        "appearance.interface_font_size",
    )
    .map(drop)
}

fn rename_preview_mode(doc: &mut DocumentMut) -> Result<(), String> {
    if let Some(Item::Value(value)) = doc
        .get_mut("general")
        .and_then(Item::as_table_like_mut)
        .and_then(|t| t.get_mut("open_files_in"))
        && value.as_str() == Some("preview")
    {
        let decor = value.decor().clone();
        *value = Value::from("reading");
        *value.decor_mut() = decor;
    }
    Ok(())
}

static CHAIN: &[Migration] = &[
    Migration {
        from: 1,
        description: "rename appearance.ui_font_size",
        apply: rename_ui_font_size,
    },
    Migration {
        from: 2,
        description: "rename the preview view mode",
        apply: rename_preview_mode,
    },
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/migrations")
        .join(name)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Copies a fixture into a fresh directory as `settings.toml`.
fn staged(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("settings.toml");
    fs::write(&path, read(&fixture(name))).unwrap_or_else(|e| panic!("{e}"));
    (dir, path)
}

#[test]
fn chain_upgrades_and_keeps_comments() {
    let input = read(&fixture("v1-settings.toml"));
    let migrated = migrate(&input, CHAIN, TARGET)
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("nothing migrated"));
    assert_eq!(migrated.from, 1);
    assert_eq!(migrated.to, 3);
    assert_eq!(migrated.text, read(&fixture("v3-expected.toml")));
}

#[test]
fn unversioned_files_count_as_version_one() {
    let input = read(&fixture("unversioned-settings.toml"));
    let migrated = migrate(&input, CHAIN, TARGET)
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("nothing migrated"));
    assert_eq!(migrated.from, 1);
    assert_eq!(migrated.text, read(&fixture("unversioned-expected.toml")));
}

#[test]
fn a_missing_step_is_an_error() {
    let err = migrate("version = 1\n", &CHAIN[1..], TARGET);
    assert_eq!(err, Err(MigrationError::MissingStep { from: 1 }));
}

#[test]
fn loading_writes_a_backup_then_the_upgraded_file() {
    let (dir, path) = staged("v1-settings.toml");
    let original = read(&path);
    let file = SettingsFile::load_with(&path, LayerKind::Global, CHAIN, TARGET)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&dir.path().join("settings.toml.v1.bak")), original);
    assert_eq!(read(&path), read(&fixture("v3-expected.toml")));
    assert_eq!(file.layer().migrated_from(), Some(1));
    assert!(
        file.layer()
            .diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::Migrated)
    );
    assert_eq!(
        file.layer()
            .get("appearance.interface_font_size")
            .map(|e| e.value.clone()),
        Some(gv_settings::toml::Value::Float(14.0))
    );

    // Loading again changes nothing and makes no second backup.
    let again = SettingsFile::load_with(&path, LayerKind::Global, CHAIN, TARGET)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(again.layer().migrated_from(), None);
    assert!(!dir.path().join("settings.toml.v1-2.bak").exists());
}

#[test]
fn an_existing_backup_is_never_overwritten() {
    let (dir, path) = staged("v1-settings.toml");
    let older = dir.path().join("settings.toml.v1.bak");
    fs::write(&older, "an older backup\n").unwrap_or_else(|e| panic!("{e}"));
    SettingsFile::load_with(&path, LayerKind::Global, CHAIN, TARGET)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&older), "an older backup\n");
    assert_eq!(
        read(&dir.path().join("settings.toml.v1-2.bak")),
        read(&fixture("v1-settings.toml"))
    );
}

#[test]
fn in_vault_files_are_upgraded_in_memory_until_edited() {
    let (dir, path) = staged("v1-settings.toml");
    let original = read(&path);
    let mut file = SettingsFile::load_with(&path, LayerKind::InVault, CHAIN, TARGET)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&path), original);
    assert!(file.layer().get("appearance.interface_font_size").is_some());
    assert!(!dir.path().join("settings.toml.v1.bak").exists());

    // An explicit edit writes the upgraded file, with a backup.
    file.set("appearance.zoom_percent", &110u32)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(read(&dir.path().join("settings.toml.v1.bak")), original);
    let now = read(&path);
    assert!(
        now.starts_with("# My Glyphvane settings\nversion = 3 # do not edit by hand\n"),
        "{now}"
    );
    assert!(now.contains("zoom_percent = 110"), "{now}");
}

#[test]
fn newer_files_are_refused_with_a_message() {
    let (_dir, path) = staged("v99-settings.toml");
    let original = read(&path);
    let mut file = SettingsFile::load_with(&path, LayerKind::Global, CHAIN, TARGET)
        .unwrap_or_else(|e| panic!("{e}"));
    let message = file
        .layer()
        .diagnostics()
        .iter()
        .find(|d| d.kind == DiagnosticKind::NewerVersion)
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(
        message.contains(
            "newer version of Glyphvane (settings version 99; this version reads up to 3)"
        ),
        "{message}"
    );
    let err = file.set("appearance.theme", &"light");
    assert!(
        matches!(err, Err(SettingsError::ReadOnly { .. })),
        "{err:?}"
    );
    assert!(matches!(
        file.remove("appearance.theme"),
        Err(SettingsError::ReadOnly { .. })
    ));
    assert_eq!(read(&path), original);
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Format-preserving edits, validation diagnostics and file writes.

mod common;

use std::fs;

use common::{Doc, doc, value_for};
use gv_settings::{
    DiagnosticKind, Layer, LayerKind, Position, SettingsError, SettingsFile, Severity,
    remove_value, set_value, spec,
};
use proptest::prelude::*;

fn layer_of(text: &str) -> Layer {
    Layer::parse(LayerKind::Global, None, text)
}

/// The generated values, read back through a layer.
fn assert_values(layer: &Layer, values: &[(&str, toml::Value)]) {
    for (path, value) in values {
        let entry = layer.get(path).map(|e| &e.value);
        assert_eq!(entry, Some(value), "{path}");
    }
}

/// Whether `new` is `old` with one contiguous run of bytes inserted.
fn is_single_insertion(old: &str, new: &str) -> bool {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = old[prefix..]
        .bytes()
        .rev()
        .zip(new[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    new.len() >= old.len() && prefix + suffix >= old.len()
}

/// A non-empty document, one of its keys and a new valid value for it.
fn doc_with_edit() -> impl Strategy<Value = (Doc, &'static str, toml::Value)> {
    doc()
        .prop_filter("needs a key", |d| !d.values.is_empty())
        .prop_flat_map(|d| {
            let paths: Vec<&'static str> = d.values.iter().map(|(p, _)| *p).collect();
            (Just(d), prop::sample::select(paths))
        })
        .prop_flat_map(|(d, path)| {
            let spec = spec(path).unwrap_or_else(|| panic!("{path}"));
            (Just(d), Just(path), value_for(spec).prop_map(|(_, v)| v))
        })
}

fn pick(doc: &Doc, index: prop::sample::Index) -> Option<&'static str> {
    (!doc.values.is_empty()).then(|| doc.values[index.index(doc.values.len())].0)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Generated documents load without errors and give the written values.
    #[test]
    fn generated_documents_load(doc in doc()) {
        let text = doc.text();
        let layer = layer_of(&text);
        let errors: Vec<_> = layer
            .diagnostics()
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        prop_assert!(errors.is_empty(), "{errors:?}\n{text}");
        assert_values(&layer, &doc.values);
    }

    /// Setting a key that is in the file changes exactly that value's bytes.
    #[test]
    fn setting_an_existing_key_changes_only_its_bytes((doc, path, new_value) in doc_with_edit()) {
        let text = doc.text();
        let out = set_value(&text, path, &new_value).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let expected = doc.render(Some((path, &new_value.to_string())));
        prop_assert_eq!(&out, &expected);
        let layer = layer_of(&out);
        let values: Vec<_> = doc
            .values
            .iter()
            .map(|(p, v)| (*p, if *p == path { new_value.clone() } else { v.clone() }))
            .collect();
        assert_values(&layer, &values);
    }

    /// Setting a key that is not in the file inserts one run of text and
    /// keeps every other byte.
    #[test]
    fn setting_a_new_key_only_inserts(doc in doc(), spec in common::any_spec()) {
        prop_assume!(!doc.has(spec.path));
        let text = doc.text();
        let out = set_value(&text, spec.path, &spec.default)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert!(is_single_insertion(&text, &out), "before:\n{}\nafter:\n{}", text, out);
        let layer = layer_of(&out);
        let mut values = doc.values.clone();
        values.push((spec.path, spec.default.clone()));
        assert_values(&layer, &values);
    }

    /// Removing a key leaves every other value in place.
    #[test]
    fn removing_a_key_keeps_the_others(doc in doc(), index in any::<prop::sample::Index>()) {
        let Some(path) = pick(&doc, index) else { return Ok(()); };
        let out = remove_value(&doc.text(), path).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let layer = layer_of(&out);
        prop_assert!(layer.get(path).is_none());
        let rest: Vec<_> = doc.values.iter().filter(|(p, _)| *p != path).cloned().collect();
        assert_values(&layer, &rest);
    }
}

#[test]
fn invalid_values_report_position_and_fall_back() {
    let text = "version = 1\n\n[appearance]\ntheme = \"blue\"\nzoom_percent = 900\nreadable_line_width = true\n\n[spelling]\nlanguages = [\"en_US\", 3]\n";
    let layer = Layer::parse(LayerKind::Global, Some("/cfg/settings.toml".as_ref()), text);
    let errors: Vec<_> = layer
        .diagnostics()
        .iter()
        .filter(|d| d.kind == DiagnosticKind::InvalidValue)
        .collect();
    assert_eq!(errors.len(), 3, "{errors:?}");

    let theme = errors[0];
    assert_eq!(theme.key.as_deref(), Some("appearance.theme"));
    assert_eq!(theme.position, Some(Position { line: 4, column: 9 }));
    assert_eq!(
        theme.to_string(),
        "/cfg/settings.toml:4:9: error: appearance.theme: expected one of \"system\", \"light\", \
         \"semi-light\", \"dark\", found the string \"blue\"; the value from the layer below is used"
    );
    let zoom = errors[1];
    assert!(
        zoom.message.contains("an integer from 50 to 300"),
        "{}",
        zoom.message
    );
    assert!(
        zoom.message.contains("found the integer 900"),
        "{}",
        zoom.message
    );
    let list = errors[2];
    assert_eq!(
        list.position,
        Some(Position {
            line: 9,
            column: 23
        })
    );
    assert!(
        list.message.contains("item 2 is the integer 3"),
        "{}",
        list.message
    );

    // The valid key in the same section still loads.
    assert_eq!(
        layer
            .get("appearance.readable_line_width")
            .map(|e| &e.value),
        Some(&toml::Value::Boolean(true))
    );
    assert!(layer.get("appearance.theme").is_none());
}

#[test]
fn unknown_keys_are_warnings_and_survive_edits() {
    let text = "future = 1\n[appearance]\nshiny = \"yes\" # from a newer version\ntheme = \"dark\"\n[newsection]\nx = 1\n";
    let layer = layer_of(text);
    let unknown: Vec<_> = layer
        .diagnostics()
        .iter()
        .filter(|d| d.kind == DiagnosticKind::UnknownKey)
        .map(|d| {
            (
                d.severity,
                d.key.clone().unwrap_or_default(),
                d.position.map(|p| p.line),
            )
        })
        .collect();
    assert_eq!(
        unknown,
        vec![
            (Severity::Warning, "future".to_owned(), Some(1)),
            (Severity::Warning, "appearance.shiny".to_owned(), Some(3)),
            (Severity::Warning, "newsection".to_owned(), Some(5)),
        ]
    );
    let out = set_value(
        text,
        "appearance.theme",
        &toml::Value::String("light".to_owned()),
    )
    .unwrap_or_default();
    assert_eq!(out, text.replace("\"dark\"", "\"light\""));
}

#[test]
fn syntax_errors_point_at_the_problem() {
    let layer = layer_of("[appearance]\ntheme = \"dark\n");
    let d = &layer.diagnostics()[0];
    assert_eq!(d.kind, DiagnosticKind::Syntax);
    assert_eq!(d.position.map(|p| p.line), Some(2));
    assert!(layer.is_read_only());
    assert!(layer.entries().next().is_none());
}

#[test]
fn section_that_is_not_a_table_is_an_error() {
    let layer = layer_of("appearance = 3\n[editor]\nprose_font = true\n");
    assert!(
        layer
            .diagnostics()
            .iter()
            .any(|d| d.kind == DiagnosticKind::NotATable && d.key.as_deref() == Some("appearance"))
    );
    assert!(layer.get("editor.prose_font").is_some());
}

#[test]
fn integers_are_accepted_for_number_settings() {
    let layer = layer_of("[appearance]\ninterface_font_size = 13\n");
    assert_eq!(
        layer
            .get("appearance.interface_font_size")
            .map(|e| &e.value),
        Some(&toml::Value::Float(13.0))
    );
}

#[test]
fn settings_file_round_trip_keeps_comments() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("config/settings.toml");
    fs::create_dir_all(path.parent().unwrap_or(dir.path())).unwrap_or_else(|e| panic!("{e}"));
    let original = "version = 1\n# Appearance tweaks\n[appearance]\ntheme = \"dark\"   # night owl\n\n[mystery]\nkeep = true\n";
    fs::write(&path, original).unwrap_or_else(|e| panic!("{e}"));

    let mut file = SettingsFile::load(&path, LayerKind::Global).unwrap_or_else(|e| panic!("{e}"));
    file.set("appearance.theme", &gv_settings::ThemeChoice::SemiLight)
        .unwrap_or_else(|e| panic!("{e}"));
    file.set("editor.prose_font", &true)
        .unwrap_or_else(|e| panic!("{e}"));
    let on_disk = fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        on_disk,
        "version = 1\n# Appearance tweaks\n[appearance]\ntheme = \"semi-light\"   # night owl\n\n[mystery]\nkeep = true\n\n[editor]\nprose_font = true\n"
    );
    assert_eq!(file.text(), on_disk);

    file.remove("editor.prose_font")
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(file.layer().get("editor.prose_font").is_none());

    // No temporary files are left behind.
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap_or(dir.path()))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("gv-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn new_file_starts_with_the_version() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("settings.toml");
    let mut file = SettingsFile::load(&path, LayerKind::Global).unwrap_or_else(|e| panic!("{e}"));
    file.set("appearance.zoom_percent", &125u32)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        fs::read_to_string(&path).unwrap_or_default(),
        "version = 1\n\n[appearance]\nzoom_percent = 125\n"
    );
}

#[test]
fn unsaved_edits_apply_at_once_and_are_written_later() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("settings.toml");
    fs::write(&path, "version = 1\n# mine\n").unwrap_or_else(|e| panic!("{e}"));
    let mut file = SettingsFile::load(&path, LayerKind::Global).unwrap_or_else(|e| panic!("{e}"));
    let zoom = |file: &SettingsFile| {
        file.layer()
            .get("appearance.zoom_percent")
            .map(|e| e.value.clone())
    };
    let disk = || fs::read_to_string(&path).unwrap_or_default();

    // In memory only: the layer has it, the disk does not.
    for percent in [110, 120, 130] {
        file.set_value_unsaved("appearance.zoom_percent", &toml::Value::Integer(percent))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    assert!(file.has_unsaved());
    assert_eq!(zoom(&file), Some(toml::Value::Integer(130)));
    assert_eq!(disk(), "version = 1\n# mine\n");
    // Invalid values are refused as with a write.
    assert!(
        file.set_value_unsaved("appearance.zoom_percent", &toml::Value::Integer(5))
            .is_err()
    );

    // Saved once, with the comments kept.
    file.save().unwrap_or_else(|e| panic!("{e}"));
    assert!(!file.has_unsaved());
    let saved = disk();
    assert!(
        saved.contains("# mine") && saved.contains("[appearance]\nzoom_percent = 130\n"),
        "{saved}"
    );
    assert_eq!(file.text(), disk());
    // Nothing waits: saving again writes nothing.
    fs::write(&path, "version = 1\n").unwrap_or_else(|e| panic!("{e}"));
    file.save().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(disk(), "version = 1\n");

    // Another edit writes the waiting one with it.
    file.reload().unwrap_or_else(|e| panic!("{e}"));
    file.set_value_unsaved("appearance.zoom_percent", &toml::Value::Integer(90))
        .unwrap_or_else(|e| panic!("{e}"));
    file.set("editor.prose_font", &true)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!file.has_unsaved());
    let reread = SettingsFile::load(&path, LayerKind::Global).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(zoom(&reread), Some(toml::Value::Integer(90)));
    assert_eq!(
        reread
            .layer()
            .get("editor.prose_font")
            .map(|e| e.value.clone()),
        Some(toml::Value::Boolean(true))
    );
}

#[test]
fn settings_file_refuses_invalid_and_misplaced_values() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let mut global = SettingsFile::load(dir.path().join("g.toml"), LayerKind::Global)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        global.set("appearance.zoom_percent", &5u32),
        Err(SettingsError::InvalidValue { .. })
    ));
    assert!(matches!(
        global.set("appearance.nope", &true),
        Err(SettingsError::UnknownKey(_))
    ));
    let mut vault = SettingsFile::load(dir.path().join("v.toml"), LayerKind::Vault)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        vault.set("general.check_for_updates", &true),
        Err(SettingsError::GlobalOnly(_))
    ));
    assert!(!dir.path().join("g.toml").exists());
    assert!(!dir.path().join("v.toml").exists());
}

#[test]
fn newer_files_are_never_written() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let path = dir.path().join("settings.toml");
    let text = "version = 99\n[appearance]\ntheme = \"dark\"\nhologram = true\n";
    fs::write(&path, text).unwrap_or_else(|e| panic!("{e}"));
    let mut file = SettingsFile::load(&path, LayerKind::Global).unwrap_or_else(|e| panic!("{e}"));
    let d = file
        .layer()
        .diagnostics()
        .iter()
        .find(|d| d.kind == DiagnosticKind::NewerVersion)
        .cloned();
    let message = d.map(|d| d.message).unwrap_or_default();
    assert!(message.contains("settings version 99"), "{message}");
    // Known keys are still read.
    assert!(file.layer().get("appearance.theme").is_some());
    let err = file.set("appearance.theme", &"light");
    assert!(
        matches!(err, Err(SettingsError::ReadOnly { .. })),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap_or_default(), text);
}

#[cfg(unix)]
#[test]
fn atomic_write_replaces_a_symlink_target_and_keeps_the_link() {
    let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let real = dir.path().join("dotfiles-settings.toml");
    let link = dir.path().join("settings.toml");
    fs::write(&real, "version = 1\n").unwrap_or_else(|e| panic!("{e}"));
    std::os::unix::fs::symlink(&real, &link).unwrap_or_else(|e| panic!("{e}"));
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap_or_else(|e| panic!("{e}"));

    gv_settings::atomic_write(&link, b"version = 1\n[editor]\nprose_font = true\n")
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        fs::symlink_metadata(&link)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    );
    assert!(
        fs::read_to_string(&real)
            .unwrap_or_default()
            .contains("prose_font")
    );
    let mode = fs::metadata(&real)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0);
    assert_eq!(mode, 0o600);
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Settings export and import (FR-SET-05).
//!
//! **Export** writes the values the global settings file sets, validated
//! and normalized, as a settings file of the current version: unknown keys,
//! invalid values and comments are left out, so the file holds exactly what
//! an import would apply.
//!
//! **Import** never writes on its own. [`preview_import`] reads a file
//! (migrating older versions in memory), checks every value against the
//! schema and compares it with the value in effect for all vaults. The
//! caller shows the [`ImportPreview`] and applies the chosen changes one key
//! at a time through the usual format-preserving edit. Keys the file does
//! not set stay as they are (a merge, not a replacement).

use std::path::Path;

use toml::Value;

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::layer::{Layer, LayerKind};
use crate::migrate::CURRENT_VERSION;

/// The file name an export suggests.
pub const EXPORT_FILE_NAME: &str = "glyphvane-settings.toml";

/// The first line of an exported file.
const EXPORT_HEADER: &str = "# Glyphvane settings (all vaults), exported from the settings window.\n# Import it with Settings ▸ About ▸ Import settings.\n";

/// The text of an export of `global`: its valid values, in schema order.
pub fn export_settings(global: &Layer) -> String {
    let mut text = format!("{EXPORT_HEADER}version = {CURRENT_VERSION}\n");
    for spec in crate::spec::specs() {
        let Some(entry) = global.get(spec.path) else {
            continue;
        };
        // Every key comes from the schema and every value passed its check,
        // so the edit cannot fail; a failure would only leave the key out.
        if let Ok(next) = crate::edit::set_value(&text, spec.path, &entry.value) {
            text = next;
        }
    }
    text
}

/// One setting an import would change.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportChange {
    /// The dotted key, such as `appearance.theme`.
    pub key: &'static str,
    /// The value for all vaults now (the file's or the default).
    pub current: Value,
    /// The imported value.
    pub new: Value,
    /// Whether the setting is security-relevant.
    pub security: bool,
    /// Whether the imported value is less strict than the current one (a
    /// security-relevant setting only).
    pub loosens: bool,
}

/// What an import would do.
#[derive(Debug, Clone, Default)]
pub struct ImportPreview {
    /// The settings whose value would change, in schema order.
    pub changes: Vec<ImportChange>,
    /// Settings the file sets to the value already in effect.
    pub unchanged: usize,
    /// Keys left out (unknown keys, invalid values) and other remarks.
    pub problems: Vec<Diagnostic>,
}

impl ImportPreview {
    /// Whether applying it would change nothing.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Why a file cannot be imported at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// The file is not valid TOML, or could not be upgraded.
    #[error("{0}")]
    Unreadable(String),
    /// The file sets no setting this version knows.
    #[error("The file holds no Glyphvane settings.")]
    NoSettings,
}

/// Reads `text` (the file at `path`, for messages) and compares it with
/// `global`, the layer of the global settings file (`None` when there is
/// none, so the defaults are in effect).
pub fn preview_import(
    text: &str,
    path: Option<&Path>,
    global: Option<&Layer>,
) -> Result<ImportPreview, ImportError> {
    let imported = Layer::parse(LayerKind::Global, path, text);
    if let Some(fatal) = imported.diagnostics().iter().find(|d| {
        matches!(
            d.kind,
            DiagnosticKind::Syntax | DiagnosticKind::MigrationFailed
        )
    }) {
        return Err(ImportError::Unreadable(fatal.message.clone()));
    }
    let mut preview = ImportPreview {
        problems: imported
            .diagnostics()
            .iter()
            .filter(|d| d.severity > Severity::Info)
            .cloned()
            .collect(),
        ..ImportPreview::default()
    };
    let mut any = false;
    for spec in crate::spec::specs() {
        let Some(entry) = imported.get(spec.path) else {
            continue;
        };
        any = true;
        let current = global
            .and_then(|layer| layer.get(spec.path))
            .map_or_else(|| spec.default.clone(), |e| e.value.clone());
        if same(&current, &entry.value) {
            preview.unchanged += 1;
            continue;
        }
        let loosens = spec
            .tighten
            .is_some_and(|t| !t.at_least_as_strict(&spec.kind, &entry.value, &current));
        preview.changes.push(ImportChange {
            key: spec.path,
            current,
            new: entry.value.clone(),
            security: spec.is_security_relevant(),
            loosens,
        });
    }
    if !any {
        return Err(ImportError::NoSettings);
    }
    Ok(preview)
}

/// Whether two values are the same setting value (numbers as numbers).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Integer(y)) | (Value::Integer(y), Value::Float(x)) => {
            *x == *y as f64
        }
        _ => a == b,
    }
}

/// A value as the preview shows it: strings without quotes, lists joined
/// with commas, "(none)" for an empty list.
pub fn display_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) if items.is_empty() => "(none)".to_owned(),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.0}"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global(text: &str) -> Layer {
        Layer::parse(LayerKind::Global, None, text)
    }

    #[test]
    fn export_keeps_valid_values_only_and_reads_back() {
        let layer = global(
            "version = 1\n# mine\n[appearance]\ntheme = \"dark\"\nzoom_percent = 9999\nmystery = 1\n\n[spelling]\nlanguages = [\"en_GB\", \"de_DE\"]\n",
        );
        let text = export_settings(&layer);
        assert!(text.starts_with("# Glyphvane settings"));
        assert!(text.contains(&format!("version = {CURRENT_VERSION}")));
        assert!(text.contains("theme = \"dark\""));
        assert!(!text.contains("zoom_percent"), "invalid values stay out");
        assert!(!text.contains("mystery"), "unknown keys stay out");
        assert!(!text.contains("# mine"), "comments stay out");
        let back = global(&text);
        assert!(back.diagnostics().is_empty(), "{:?}", back.diagnostics());
        assert_eq!(
            back.get("spelling.languages").map(|e| e.value.clone()),
            layer.get("spelling.languages").map(|e| e.value.clone())
        );
    }

    #[test]
    fn export_of_nothing_is_just_the_version() {
        let text = export_settings(&Layer::empty(LayerKind::Global));
        let back = global(&text);
        assert_eq!(back.entries().count(), 0);
        assert!(back.diagnostics().is_empty());
    }

    #[test]
    fn preview_lists_changes_against_the_current_values() {
        let current = global("version = 1\n[appearance]\ntheme = \"dark\"\nzoom_percent = 120\n");
        let file = "version = 1\n[appearance]\ntheme = \"light\"\nzoom_percent = 120\neditor_font_size = 14\n[general]\nopen_files_in = \"bogus\"\n[nothing]\nx = 1\n";
        let preview = preview_import(file, None, Some(&current)).expect("readable");
        let keys: Vec<_> = preview.changes.iter().map(|c| c.key).collect();
        assert_eq!(keys, ["appearance.theme", "appearance.editor_font_size"]);
        assert_eq!(preview.unchanged, 1);
        let theme = &preview.changes[0];
        assert_eq!(theme.current, Value::String("dark".into()));
        assert_eq!(theme.new, Value::String("light".into()));
        // The default applies to a key the current file does not set.
        assert_eq!(preview.changes[1].current, Value::Float(12.5));
        // The invalid value and the unknown section are reported, not applied.
        let kinds: Vec<_> = preview.problems.iter().map(|d| d.kind).collect();
        assert!(kinds.contains(&DiagnosticKind::InvalidValue), "{kinds:?}");
        assert!(kinds.contains(&DiagnosticKind::UnknownKey), "{kinds:?}");
    }

    #[test]
    fn preview_marks_security_settings_that_loosen() {
        let file = "version = 1\n[privacy_and_security]\nremote_content = \"allow\"\nconfirm_every_handoff = true\n";
        let preview = preview_import(file, None, None).expect("readable");
        let remote = preview
            .changes
            .iter()
            .find(|c| c.key == "privacy_and_security.remote_content")
            .expect("listed");
        assert!(remote.security && remote.loosens);
        let handoff = preview
            .changes
            .iter()
            .find(|c| c.key == "privacy_and_security.confirm_every_handoff")
            .expect("listed");
        assert!(handoff.security && !handoff.loosens);
    }

    #[test]
    fn unreadable_and_empty_files_are_refused() {
        assert!(matches!(
            preview_import("[appearance\ntheme = 1", None, None),
            Err(ImportError::Unreadable(_))
        ));
        assert_eq!(
            preview_import("version = 1\n[other]\nx = 1\n", None, None).err(),
            Some(ImportError::NoSettings)
        );
        assert_eq!(
            preview_import("", None, None).err(),
            Some(ImportError::NoSettings)
        );
    }

    #[test]
    fn unversioned_files_are_upgraded_first() {
        let preview = preview_import("[appearance]\ntheme = \"dark\"\n", None, None)
            .expect("an unversioned file is read");
        assert_eq!(preview.changes.len(), 1);
    }

    #[test]
    fn values_display_plainly() {
        assert_eq!(display_value(&Value::String("dark".into())), "dark");
        assert_eq!(display_value(&Value::Float(12.0)), "12");
        assert_eq!(display_value(&Value::Float(12.5)), "12.5");
        assert_eq!(display_value(&Value::Array(Vec::new())), "(none)");
        assert_eq!(
            display_value(&Value::Array(vec![
                Value::String("a".into()),
                Value::String("b".into())
            ])),
            "a, b"
        );
        assert_eq!(display_value(&Value::Boolean(true)), "true");
    }
}

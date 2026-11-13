// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Layering: defaults, then each layer in [`LayerKind`] order. Every effective
//! value remembers the layer that set it.
//!
//! Two rules protect security-relevant settings:
//! - a value from an in-vault file is applied only when the result is at
//!   least as strict as the value below it ([`crate::Tighten`]);
//! - a value the global file sets to its `absolute_in_global` value (remote
//!   content `block`) cannot be changed by any vault layer.
//!
//! Rejected values are left out with a warning that points at them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::Value;

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::layer::{Entry, Layer, LayerKind};
use crate::schema::Settings;
use crate::spec::{self, Merge, Spec};

/// An effective value and the layer that set it.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub value: Value,
    pub source: LayerKind,
}

/// The effective settings: typed, plus every value with its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    settings: Settings,
    values: BTreeMap<&'static str, Resolved>,
    diagnostics: Vec<Diagnostic>,
}

/// Layers the given layers over the defaults. Layers are applied in
/// [`LayerKind`] order whatever order they are passed in.
pub fn resolve<'a>(layers: impl IntoIterator<Item = &'a Layer>) -> Effective {
    let mut layers: Vec<&Layer> = layers.into_iter().collect();
    layers.sort_by_key(|layer| layer.kind());
    let mut diagnostics: Vec<Diagnostic> = layers
        .iter()
        .flat_map(|layer| layer.diagnostics().iter().cloned())
        .collect();
    let mut values = BTreeMap::new();
    for spec in spec::specs() {
        let resolved = resolve_one(spec, &layers, &mut diagnostics);
        values.insert(spec.path, resolved);
    }
    let settings = match typed(&values) {
        Ok(settings) => settings,
        Err(message) => {
            // Every value was validated against its spec, and a test checks
            // that the specs match the structs, so this is a defect.
            diagnostics.push(Diagnostic::new(
                Severity::Error,
                DiagnosticKind::InvalidValue,
                LayerKind::Default,
                format!("internal error, defaults used: {message}"),
            ));
            Settings::default()
        }
    };
    Effective {
        settings,
        values,
        diagnostics,
    }
}

fn resolve_one(spec: &Spec, layers: &[&Layer], diagnostics: &mut Vec<Diagnostic>) -> Resolved {
    let mut current = Resolved {
        value: spec.default.clone(),
        source: LayerKind::Default,
    };
    for layer in layers {
        let Some(entry) = layer.get(spec.path) else {
            continue;
        };
        let candidate = match spec.merge {
            Merge::Replace => entry.value.clone(),
            Merge::Union => union(&current.value, &entry.value),
        };
        if let Some(rejection) = reject(spec, layer.kind(), &candidate, &current) {
            diagnostics.push(rejection_diagnostic(spec, layer, entry, rejection));
            continue;
        }
        current = Resolved {
            value: candidate,
            source: layer.kind(),
        };
    }
    current
}

#[derive(Debug, Clone, Copy)]
enum Rejection {
    LockedByGlobal,
    NotTightening,
}

fn reject(
    spec: &Spec,
    kind: LayerKind,
    candidate: &Value,
    current: &Resolved,
) -> Option<Rejection> {
    if kind.is_vault_level()
        && current.source == LayerKind::Global
        && spec
            .absolute_in_global
            .is_some_and(|locked| current.value.as_str() == Some(locked))
        && *candidate != current.value
    {
        return Some(Rejection::LockedByGlobal);
    }
    if kind.is_in_vault()
        && spec
            .tighten
            .is_some_and(|t| !t.at_least_as_strict(&spec.kind, candidate, &current.value))
    {
        return Some(Rejection::NotTightening);
    }
    None
}

fn rejection_diagnostic(spec: &Spec, layer: &Layer, entry: &Entry, why: Rejection) -> Diagnostic {
    let (kind, message) = match why {
        Rejection::LockedByGlobal => (
            DiagnosticKind::LockedByGlobal,
            format!(
                "ignored: the global settings set this to \"{}\", which vault settings cannot change",
                spec.absolute_in_global.unwrap_or_default()
            ),
        ),
        Rejection::NotTightening => (
            DiagnosticKind::NotTightening,
            "ignored: settings inside a vault can make this stricter, never looser".to_owned(),
        ),
    };
    Diagnostic::new(Severity::Warning, kind, layer.kind(), message)
        .key(spec.path)
        .file(layer.path().map(Path::to_path_buf))
        .at(layer.text(), entry.span.clone())
}

/// Items of `lower` then the new items of `higher`.
fn union(lower: &Value, higher: &Value) -> Value {
    let mut items: Vec<Value> = lower.as_array().cloned().unwrap_or_default();
    for item in higher.as_array().into_iter().flatten() {
        if !items.contains(item) {
            items.push(item.clone());
        }
    }
    Value::Array(items)
}

fn typed(values: &BTreeMap<&'static str, Resolved>) -> Result<Settings, String> {
    let mut root = toml::Table::new();
    for (path, resolved) in values {
        let Some((section, key)) = path.split_once('.') else {
            continue;
        };
        let table = root
            .entry(section)
            .or_insert_with(|| Value::Table(toml::Table::new()));
        if let Value::Table(table) = table {
            table.insert(key.to_owned(), resolved.value.clone());
        }
    }
    Value::Table(root)
        .try_into::<Settings>()
        .map_err(|e| e.to_string())
}

impl Effective {
    /// The effective settings, typed.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The effective value at `path`.
    pub fn value(&self, path: &str) -> Option<&Value> {
        self.values.get(path).map(|r| &r.value)
    }

    /// The layer that set the value at `path`.
    pub fn source(&self, path: &str) -> Option<LayerKind> {
        self.values.get(path).map(|r| r.source)
    }

    /// Every setting with its value and source, in path order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &Resolved)> {
        self.values.iter().map(|(k, v)| (*k, v))
    }

    /// Diagnostics from every layer, then from layering.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The settings whose value or source differ in `newer`.
    pub fn diff(&self, newer: &Effective) -> Vec<Change> {
        self.values
            .iter()
            .filter_map(|(path, old)| {
                let new = newer.values.get(path)?;
                (old != new).then(|| Change {
                    key: path,
                    old: old.clone(),
                    new: new.clone(),
                })
            })
            .collect()
    }
}

/// A setting whose effective value or source changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub key: &'static str,
    pub old: Resolved,
    pub new: Resolved,
}

impl Change {
    /// Whether the value changed (and not only the layer that sets it).
    pub fn value_changed(&self) -> bool {
        self.old.value != self.new.value
    }
}

/// The layers in use and their effective settings, for live reload: replace
/// a layer and get the changed keys.
#[derive(Debug, Clone)]
pub struct SettingsStack {
    layers: Vec<Layer>,
    effective: Effective,
}

impl Default for SettingsStack {
    fn default() -> Self {
        Self::new([])
    }
}

impl SettingsStack {
    /// A stack with the given layers. A later layer replaces an earlier one
    /// of the same kind.
    pub fn new(layers: impl IntoIterator<Item = Layer>) -> Self {
        let mut stack = Self {
            layers: Vec::new(),
            effective: resolve([]),
        };
        for layer in layers {
            stack.put(layer);
        }
        stack.effective = resolve(&stack.layers);
        stack
    }

    fn put(&mut self, layer: Layer) {
        self.layers.retain(|l| l.kind() != layer.kind());
        self.layers.push(layer);
        self.layers.sort_by_key(Layer::kind);
    }

    fn recompute(&mut self) -> Vec<Change> {
        let effective = resolve(&self.layers);
        let changes = self.effective.diff(&effective);
        self.effective = effective;
        changes
    }

    /// The effective settings.
    pub fn effective(&self) -> &Effective {
        &self.effective
    }

    /// The effective settings, typed.
    pub fn settings(&self) -> &Settings {
        self.effective.settings()
    }

    /// The layer of `kind`, if present.
    pub fn layer(&self, kind: LayerKind) -> Option<&Layer> {
        self.layers.iter().find(|l| l.kind() == kind)
    }

    /// Adds or replaces the layer of the same kind.
    pub fn set_layer(&mut self, layer: Layer) -> Vec<Change> {
        self.put(layer);
        self.recompute()
    }

    /// Removes the layer of `kind`, as when a file is deleted.
    pub fn remove_layer(&mut self, kind: LayerKind) -> Vec<Change> {
        self.layers.retain(|l| l.kind() != kind);
        self.recompute()
    }

    /// Live reload: parses new contents of the file behind `kind` and returns
    /// the settings that changed. The file watcher calls this.
    pub fn reload(&mut self, kind: LayerKind, path: Option<PathBuf>, text: &str) -> Vec<Change> {
        self.set_layer(Layer::parse(kind, path.as_deref(), text))
    }
}

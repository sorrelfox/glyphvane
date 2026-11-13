// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! One settings layer: the validated values of one file (or of per-file
//! state), with the diagnostics found while reading it.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use toml_edit::{Item, TableLike};

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::migrate::{self, CURRENT_VERSION, Migration, MigrationError};
use crate::spec::{self, Scope};

/// Where a value comes from, lowest priority first. The derived order is the
/// layering order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayerKind {
    /// The built-in defaults.
    Default,
    /// The global `settings.toml` in the config dir ("all vaults").
    Global,
    /// `.glyphvane/settings.toml` inside the vault, shared through Git. It is
    /// untrusted: it can only tighten security-relevant settings.
    InVault,
    /// The vault's overlay in the app data dir, keyed by vault id ("this
    /// vault"). Written only by the app and the user, so it is trusted, but a
    /// global value marked absolute still wins.
    Vault,
    /// Per-file state, such as a profile override.
    File,
}

impl LayerKind {
    /// Every kind, in layering order.
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Global,
        Self::InVault,
        Self::Vault,
        Self::File,
    ];

    /// Whether values come from a file inside the vault, which anyone who
    /// shares the vault can write.
    pub fn is_in_vault(self) -> bool {
        self == Self::InVault
    }

    /// Whether the layer applies to one vault (or one file) only.
    pub fn is_vault_level(self) -> bool {
        self > Self::Global
    }
}

impl fmt::Display for LayerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Default => "defaults",
            Self::Global => "global settings",
            Self::InVault => "in-vault settings",
            Self::Vault => "vault settings",
            Self::File => "file state",
        })
    }
}

/// A validated value and where it is in its file.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The value, normalized (integers widened for number settings).
    pub value: toml::Value,
    /// The byte range of the value in the layer's text.
    pub span: Option<Range<usize>>,
}

/// The validated values of one layer.
#[derive(Debug, Clone)]
pub struct Layer {
    kind: LayerKind,
    path: Option<PathBuf>,
    text: String,
    version: u32,
    migrated_from: Option<u32>,
    read_only: bool,
    entries: BTreeMap<&'static str, Entry>,
    diagnostics: Vec<Diagnostic>,
}

impl Layer {
    /// A layer with no values.
    pub fn empty(kind: LayerKind) -> Self {
        Self {
            kind,
            path: None,
            text: String::new(),
            version: CURRENT_VERSION,
            migrated_from: None,
            read_only: false,
            entries: BTreeMap::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Parses a settings file's contents. Older versions are migrated in
    /// memory (see [`crate::SettingsFile`] for the on-disk upgrade). Never
    /// fails: problems become diagnostics, and an invalid value is left out
    /// so the layer below applies.
    pub fn parse(kind: LayerKind, path: Option<&Path>, text: &str) -> Self {
        Self::parse_with(kind, path, text, migrate::migrations(), CURRENT_VERSION)
    }

    /// [`Layer::parse`] with an explicit migration chain and target version.
    pub fn parse_with(
        kind: LayerKind,
        path: Option<&Path>,
        text: &str,
        chain: &[Migration],
        target: u32,
    ) -> Self {
        let mut layer = Self::empty(kind);
        layer.path = path.map(Path::to_path_buf);
        layer.version = target;
        let mut text = text.to_owned();
        match migrate::migrate(&text, chain, target) {
            Ok(None) => {}
            Ok(Some(migrated)) => {
                layer.migrated_from = Some(migrated.from);
                layer.push(Diagnostic::new(
                    Severity::Info,
                    DiagnosticKind::Migrated,
                    kind,
                    format!(
                        "upgraded from settings version {} to {}",
                        migrated.from, migrated.to
                    ),
                ));
                text = migrated.text;
            }
            Err(MigrationError::Syntax { message, span }) => {
                layer.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::Syntax,
                        kind,
                        format!("not valid TOML, so nothing in this file is used: {message}"),
                    )
                    .at(&text, span),
                );
                layer.text = text;
                layer.read_only = true;
                return layer;
            }
            Err(err @ MigrationError::InvalidVersion { .. }) => {
                let span = match &err {
                    MigrationError::InvalidVersion { span, .. } => span.clone(),
                    _ => None,
                };
                layer.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::InvalidVersion,
                        kind,
                        format!("{err}; the file is read as version {target}"),
                    )
                    .key("version")
                    .at(&text, span),
                );
            }
            Err(err @ MigrationError::Newer { found, .. }) => {
                layer.version = found;
                layer.read_only = true;
                layer.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::NewerVersion,
                        kind,
                        err.to_string(),
                    )
                    .key("version"),
                );
            }
            Err(err @ (MigrationError::MissingStep { .. } | MigrationError::Step { .. })) => {
                layer.push(Diagnostic::new(
                    Severity::Error,
                    DiagnosticKind::MigrationFailed,
                    kind,
                    format!("{err}, so nothing in this file is used"),
                ));
                layer.text = text;
                layer.read_only = true;
                return layer;
            }
        }
        layer.read_entries(&text);
        layer.text = text;
        layer
    }

    /// A layer from values set in code, such as per-file state. Invalid
    /// values become diagnostics, as in a file.
    pub fn from_values<'a>(
        kind: LayerKind,
        values: impl IntoIterator<Item = (&'a str, toml::Value)>,
    ) -> Self {
        let mut layer = Self::empty(kind);
        for (path, value) in values {
            let Some(spec) = spec::spec(path) else {
                layer.push(unknown_key(kind, path));
                continue;
            };
            match spec.check_owned(&value) {
                Ok(value) => layer.insert(spec, value, None, ""),
                Err(err) => layer.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::InvalidValue,
                        kind,
                        err.message,
                    )
                    .key(path),
                ),
            }
        }
        layer
    }

    fn read_entries(&mut self, text: &str) {
        let doc = match toml_edit::Document::parse(text) {
            Ok(doc) => doc,
            Err(e) => {
                // `migrate` parsed the same text, so this is unreachable in
                // practice; report it like any syntax error.
                self.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::Syntax,
                        self.kind,
                        e.message().to_owned(),
                    )
                    .at(text, e.span()),
                );
                return;
            }
        };
        let root = doc.as_table();
        for (name, item) in root.iter() {
            if name == "version" {
                continue;
            }
            if !spec::is_section(name) {
                let span = root.key(name).and_then(toml_edit::Key::span);
                self.push(unknown_key(self.kind, name).at(text, span));
                continue;
            }
            let Some(section) = item.as_table_like() else {
                self.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::NotATable,
                        self.kind,
                        format!(
                            "expected a table of settings, found {}",
                            describe_item(item)
                        ),
                    )
                    .key(name)
                    .at(text, item.span()),
                );
                continue;
            };
            self.read_section(name, section, text);
        }
    }

    fn read_section(&mut self, name: &str, section: &dyn TableLike, text: &str) {
        for (key, item) in section.iter() {
            let path = format!("{name}.{key}");
            let Some(spec) = spec::spec(&path) else {
                let span = section
                    .get_key_value(key)
                    .and_then(|(k, _)| k.span())
                    .or_else(|| item.span());
                self.push(unknown_key(self.kind, &path).at(text, span));
                continue;
            };
            let Item::Value(value) = item else {
                self.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::InvalidValue,
                        self.kind,
                        format!(
                            "expected {}, found {}",
                            spec.kind.expected(),
                            describe_item(item)
                        ),
                    )
                    .key(path)
                    .at(text, item.span()),
                );
                continue;
            };
            match spec.check(value) {
                Ok(checked) => self.insert(spec, checked, value.span(), text),
                Err(err) => self.push(
                    Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::InvalidValue,
                        self.kind,
                        format!("{}; the value from the layer below is used", err.message),
                    )
                    .key(path)
                    .at(text, err.span),
                ),
            }
        }
    }

    fn insert(
        &mut self,
        spec: &'static spec::Spec,
        value: toml::Value,
        span: Option<Range<usize>>,
        text: &str,
    ) {
        if spec.scope == Scope::GlobalOnly && self.kind.is_vault_level() {
            self.push(
                Diagnostic::new(
                    Severity::Warning,
                    DiagnosticKind::GlobalOnly,
                    self.kind,
                    "this setting applies to all vaults and is ignored in vault settings",
                )
                .key(spec.path)
                .at(text, span),
            );
            return;
        }
        self.entries.insert(spec.path, Entry { value, span });
    }

    /// Adds a diagnostic and marks the file as not writable.
    pub(crate) fn mark_read_only(&mut self, diagnostic: Diagnostic) {
        self.read_only = true;
        self.push(diagnostic);
    }

    fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic.file(self.path.clone()));
    }

    /// The layer's kind.
    pub fn kind(&self) -> LayerKind {
        self.kind
    }

    /// The file the layer was read from, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The text the values were read from (after any in-memory migration).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The settings version of the file (after any migration).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The version the file had before an in-memory migration.
    pub fn migrated_from(&self) -> Option<u32> {
        self.migrated_from
    }

    /// Whether the file must not be written: it is from a newer version, not
    /// valid TOML, or could not be migrated.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// The valid value at `path`, if this layer sets it.
    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.get(path)
    }

    /// Every valid value, by path.
    pub fn entries(&self) -> impl Iterator<Item = (&'static str, &Entry)> {
        self.entries.iter().map(|(k, v)| (*k, v))
    }

    /// Problems found while reading the layer.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

fn unknown_key(kind: LayerKind, path: &str) -> Diagnostic {
    Diagnostic::new(
        Severity::Warning,
        DiagnosticKind::UnknownKey,
        kind,
        "unknown setting; it is kept in the file but has no effect in this version",
    )
    .key(path)
}

fn describe_item(item: &Item) -> String {
    match item {
        Item::Value(v) => spec::describe(v),
        Item::Table(_) => "a table".to_owned(),
        Item::ArrayOfTables(_) => "an array of tables".to_owned(),
        Item::None => "nothing".to_owned(),
    }
}

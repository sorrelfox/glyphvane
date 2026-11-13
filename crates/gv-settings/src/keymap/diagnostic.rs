// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Problems found in `keymap.toml` and in the combined keymap.

use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

use crate::diagnostic::{Diagnostic, DiagnosticKind, Position, Severity};
use crate::layer::LayerKind;

/// What a keymap diagnostic is about. [`Diagnostic::kind`] carries the
/// nearest general kind; this is the precise one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeymapIssue {
    /// The file is not valid TOML or not UTF-8. Nothing in it is used.
    Syntax,
    /// A key the keymap format does not have. It is ignored.
    UnknownKey,
    /// A `bind`/`unbind` list, platform section or entry of the wrong type.
    NotATable,
    /// The `version` key is not a whole number from 1 up.
    InvalidVersion,
    /// The file comes from a newer version; it is read but never written.
    NewerVersion,
    /// The file was upgraded from an older version.
    Migrated,
    /// Upgrading the file failed. Nothing in it is used.
    MigrationFailed,
    /// An entry lacks a required field or has a field of the wrong type.
    InvalidEntry,
    /// The keys could not be read.
    InvalidKeys,
    /// The context could not be read.
    InvalidContext,
    /// No command has this id. The entry is ignored.
    UnknownCommand,
    /// The context uses a name the app does not set. The binding is kept.
    UnknownContext,
    /// The binding's context can never be active, or never overlaps the
    /// command's own context.
    NeverActive,
    /// A key that types a character is bound where the editor takes text.
    PrintableKeyInEditor,
    /// An `unbind` entry matched no built-in binding.
    NothingToUnbind,
    /// Two bindings of the same layer and specificity share keys and can
    /// be active together; the later one wins.
    Conflict,
    /// A user binding replaces a built-in binding of another command where
    /// both apply.
    Overrides,
    /// The same command is bound to the same keys twice.
    Duplicate,
    /// A binding's keys are the start of a longer chord that can be active at
    /// the same time.
    ChordPrefix,
}

impl KeymapIssue {
    fn general_kind(self) -> DiagnosticKind {
        match self {
            Self::Syntax => DiagnosticKind::Syntax,
            Self::UnknownKey | Self::UnknownCommand | Self::UnknownContext => {
                DiagnosticKind::UnknownKey
            }
            Self::NotATable => DiagnosticKind::NotATable,
            Self::InvalidVersion => DiagnosticKind::InvalidVersion,
            Self::NewerVersion => DiagnosticKind::NewerVersion,
            Self::Migrated => DiagnosticKind::Migrated,
            Self::MigrationFailed => DiagnosticKind::MigrationFailed,
            Self::InvalidEntry
            | Self::InvalidKeys
            | Self::InvalidContext
            | Self::NeverActive
            | Self::PrintableKeyInEditor
            | Self::NothingToUnbind
            | Self::Conflict
            | Self::Overrides
            | Self::Duplicate
            | Self::ChordPrefix => DiagnosticKind::InvalidValue,
        }
    }
}

/// Where a binding comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingOrigin {
    /// [`LayerKind::Default`] for built-in bindings, [`LayerKind::Global`]
    /// for `keymap.toml`.
    pub layer: LayerKind,
    /// The keymap file, for user bindings read from one.
    pub file: Option<PathBuf>,
    /// The byte range of the entry in the file.
    pub span: Option<Range<usize>>,
    /// The line and column of the start of `span`.
    pub position: Option<Position>,
    /// The entry's key in the file, such as `bind[2]` or `macos.bind[0]`.
    pub key: Option<String>,
}

impl BindingOrigin {
    pub(crate) fn builtin() -> Self {
        Self {
            layer: LayerKind::Default,
            file: None,
            span: None,
            position: None,
            key: None,
        }
    }

    /// Whether the binding is built in.
    pub fn is_builtin(&self) -> bool {
        self.layer == LayerKind::Default
    }
}

impl fmt::Display for BindingOrigin {
    /// `built-in default`, or `keymap.toml:3:1 (bind[0])`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_builtin() {
            return f.write_str("built-in default");
        }
        match &self.file {
            Some(file) => write!(f, "{}", file.display())?,
            None => f.write_str("keymap")?,
        }
        if let Some(p) = self.position {
            write!(f, ":{}:{}", p.line, p.column)?;
        }
        if let Some(key) = &self.key {
            write!(f, " ({key})")?;
        }
        Ok(())
    }
}

/// One keymap problem: a settings [`Diagnostic`] (severity, file, span,
/// line and column, key, message) with the precise issue and, for conflicts,
/// the other binding involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapDiagnostic {
    pub issue: KeymapIssue,
    pub diagnostic: Diagnostic,
    /// The other binding of a conflict, override, duplicate or chord prefix.
    pub related: Option<BindingOrigin>,
}

impl KeymapDiagnostic {
    pub(crate) fn new(
        severity: Severity,
        issue: KeymapIssue,
        layer: LayerKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            issue,
            diagnostic: Diagnostic::new(severity, issue.general_kind(), layer, message),
            related: None,
        }
    }

    /// Places the diagnostic at `span` in `text`; `None` keeps the current
    /// place.
    pub(crate) fn at(mut self, text: &str, span: Option<Range<usize>>) -> Self {
        if span.is_some() {
            self.diagnostic = self.diagnostic.at(text, span);
        }
        self
    }

    pub(crate) fn key(mut self, key: impl Into<String>) -> Self {
        self.diagnostic = self.diagnostic.key(key);
        self
    }

    pub(crate) fn file(mut self, file: Option<PathBuf>) -> Self {
        self.diagnostic = self.diagnostic.file(file);
        self
    }

    /// Places the diagnostic at a binding's entry.
    pub(crate) fn origin(mut self, origin: &BindingOrigin) -> Self {
        self.diagnostic.file = origin.file.clone();
        self.diagnostic.span = origin.span.clone();
        self.diagnostic.position = origin.position;
        self.diagnostic.key = origin.key.clone();
        self
    }

    pub(crate) fn related(mut self, origin: &BindingOrigin) -> Self {
        self.related = Some(origin.clone());
        self
    }

    pub fn severity(&self) -> Severity {
        self.diagnostic.severity
    }
}

impl fmt::Display for KeymapDiagnostic {
    /// `path:line:column: warning: key: message`, leaving out what is
    /// unknown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = &self.diagnostic;
        match &d.file {
            Some(file) => write!(f, "{}", file.display())?,
            None if d.layer == LayerKind::Default => f.write_str("<built-in keymap>")?,
            None => f.write_str("<keymap>")?,
        }
        if let Some(p) = d.position {
            write!(f, ":{}:{}", p.line, p.column)?;
        }
        let severity = match d.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, ": {severity}: ")?;
        if let Some(key) = &d.key {
            write!(f, "{key}: ")?;
        }
        f.write_str(&d.message)
    }
}

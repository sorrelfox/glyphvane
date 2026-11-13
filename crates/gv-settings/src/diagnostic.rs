// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Problems found while loading or layering settings. None of them stops
//! loading: an invalid value falls back to the layer below.

use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

use crate::layer::LayerKind;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// For information, such as a file that was upgraded.
    Info,
    /// Something was ignored on purpose, such as an unknown key.
    Warning,
    /// A value or file could not be used.
    Error,
}

/// What a diagnostic is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// The file is not valid TOML. Nothing in it is used.
    Syntax,
    /// A key this version does not know. It is kept in the file.
    UnknownKey,
    /// A value of the wrong type or out of range. The layer below is used.
    InvalidValue,
    /// A section name used for something other than a table.
    NotATable,
    /// The `version` key is missing a valid number.
    InvalidVersion,
    /// The file was written by a newer version. It is read but never written.
    NewerVersion,
    /// The file was upgraded from an older version.
    Migrated,
    /// Upgrading the file failed. Nothing in it is used.
    MigrationFailed,
    /// A setting that only the global file may set appeared in a vault layer.
    GlobalOnly,
    /// An in-vault value would have relaxed a security-relevant setting.
    NotTightening,
    /// A vault layer tried to change a value the global file made absolute.
    LockedByGlobal,
}

/// A 1-based line and column (in characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    /// The position of byte `offset` in `text`.
    pub fn of(text: &str, offset: usize) -> Self {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let before = &text[..offset];
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        Self {
            line: before.matches('\n').count() + 1,
            column: before[line_start..].chars().count() + 1,
        }
    }
}

/// One problem, with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub kind: DiagnosticKind,
    /// The layer the problem is in.
    pub layer: LayerKind,
    /// The file, for file-backed layers.
    pub file: Option<PathBuf>,
    /// The byte range in the file.
    pub span: Option<Range<usize>>,
    /// The line and column of the start of `span`.
    pub position: Option<Position>,
    /// The dotted key, such as `appearance.theme`.
    pub key: Option<String>,
    /// A sentence for the user.
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn new(
        severity: Severity,
        kind: DiagnosticKind,
        layer: LayerKind,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            kind,
            layer,
            file: None,
            span: None,
            position: None,
            key: None,
            message: message.into(),
        }
    }

    pub(crate) fn at(mut self, text: &str, span: Option<Range<usize>>) -> Self {
        self.position = span.as_ref().map(|s| Position::of(text, s.start));
        self.span = span;
        self
    }

    pub(crate) fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub(crate) fn file(mut self, file: Option<PathBuf>) -> Self {
        self.file = file;
        self
    }
}

impl fmt::Display for Diagnostic {
    /// `path:line:column: error: key: message`, leaving out what is unknown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.file {
            Some(file) => write!(f, "{}", file.display())?,
            None => write!(f, "<{}>", self.layer)?,
        }
        if let Some(p) = self.position {
            write!(f, ":{}:{}", p.line, p.column)?;
        }
        let severity = match self.severity {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, ": {severity}: ")?;
        if let Some(key) = &self.key {
            write!(f, "{key}: ")?;
        }
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_counts_lines_and_characters() {
        let text = "a = 1\nbé = \"x\"\n";
        assert_eq!(Position::of(text, 0), Position { line: 1, column: 1 });
        let offset = text.find('"').unwrap_or_default();
        assert_eq!(Position::of(text, offset), Position { line: 2, column: 6 });
        assert_eq!(Position::of(text, 999), Position { line: 3, column: 1 });
    }
}

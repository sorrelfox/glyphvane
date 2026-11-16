// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The `.glyphvane.toml` folder file: an optional file the user creates
//! (Glyphvane never writes it) to choose the profile for a folder and the
//! folders below it, shared through Git.
//!
//! ```toml
//! profile = "extended"            # any profile id
//! root = true                     # this folder is a vault root
//! [switches]                      # optional per-switch overrides
//! smart_punctuation = true
//! tilde_single = "strike"
//! ```
//!
//! Reading never fails as a whole once the TOML parses: an unknown key is
//! a warning and is ignored, an invalid value is an error and only that key
//! is ignored. A syntax error is one error and the file counts as empty.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{Item, Value};

use crate::compile::SwitchOverrides;
use crate::id::Requested;
use crate::switches::{SwitchError, SwitchValue};

/// The file name.
pub const FILE_NAME: &str = ".glyphvane.toml";

/// What a `.glyphvane.toml` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderConfig {
    /// `profile`: a built-in or planned profile id.
    pub profile: Option<Requested>,
    /// `root`: whether the folder is a vault root.
    pub root: Option<bool>,
    /// `[switches]`.
    pub switches: SwitchOverrides,
}

/// How bad a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Ignored without harm, such as an unknown key.
    Warning,
    /// A value (or the whole file) could not be used.
    Error,
}

/// A problem found in a `.glyphvane.toml`, with its 1-based place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    pub severity: Severity,
    pub line: usize,
    pub column: usize,
    /// The dotted key, such as `switches.tilde_single`.
    pub key: Option<String>,
    pub message: String,
}

impl fmt::Display for ConfigDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let level = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{}:{}: {level}: ", self.line, self.column)?;
        if let Some(key) = &self.key {
            write!(f, "`{key}`: ")?;
        }
        f.write_str(&self.message)
    }
}

/// A `.glyphvane.toml` that was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    pub path: PathBuf,
    pub config: FolderConfig,
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl ConfigFile {
    /// Reads and parses the file at `path`.
    ///
    /// # Errors
    ///
    /// Only when the file cannot be read; its contents never fail.
    pub fn read(path: &Path) -> io::Result<ConfigFile> {
        let text = std::fs::read_to_string(path)?;
        let (config, diagnostics) = parse(&text);
        Ok(ConfigFile {
            path: path.to_path_buf(),
            config,
            diagnostics,
        })
    }

    /// Whether any diagnostic is an error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

/// Parses the text of a `.glyphvane.toml`.
#[must_use]
pub fn parse(text: &str) -> (FolderConfig, Vec<ConfigDiagnostic>) {
    let mut reader = Reader {
        text,
        diagnostics: Vec::new(),
    };
    let mut config = FolderConfig::default();
    let doc = match toml_edit::Document::parse(text) {
        Ok(doc) => doc,
        Err(e) => {
            reader.push(
                Severity::Error,
                e.span(),
                None,
                e.message().trim().to_owned(),
            );
            return (config, reader.diagnostics);
        }
    };
    let root = doc.as_table();
    for (key, item) in root.iter() {
        let key_span = root.key(key).and_then(toml_edit::Key::span);
        match key {
            "profile" => {
                let Some(id) = item.as_str() else {
                    reader.invalid(item, key, "expected a profile id string");
                    continue;
                };
                match Requested::from_id(id) {
                    Some(profile) => config.profile = Some(profile),
                    None => reader.invalid(
                        item,
                        key,
                        &format!("unknown profile `{id}`; the profile is detected instead"),
                    ),
                }
            }
            "root" => match item.as_bool() {
                Some(b) => config.root = Some(b),
                None => reader.invalid(item, key, "expected true or false"),
            },
            "switches" => {
                let Some(table) = item.as_table_like() else {
                    reader.invalid(item, key, "expected a table of switches");
                    continue;
                };
                for (name, value) in table.iter() {
                    let path = format!("switches.{name}");
                    let name_span = table
                        .get_key_value(name)
                        .and_then(|(k, _)| k.span())
                        .or_else(|| value.span());
                    let Some(switch) = switch_value(value) else {
                        reader.invalid(value, &path, "expected a boolean, integer or string");
                        continue;
                    };
                    match config.switches.insert(name, switch) {
                        Ok(()) => {}
                        Err(SwitchError::UnknownKey) => reader.push(
                            Severity::Warning,
                            name_span,
                            Some(path),
                            "unknown switch; ignored".to_owned(),
                        ),
                        Err(SwitchError::Invalid { expected }) => {
                            reader.invalid(value, &path, &format!("expected {expected}"));
                        }
                    }
                }
            }
            _ => reader.push(
                Severity::Warning,
                key_span,
                Some(key.to_owned()),
                "unknown key; ignored".to_owned(),
            ),
        }
    }
    (config, reader.diagnostics)
}

fn switch_value(item: &Item) -> Option<SwitchValue> {
    match item.as_value()? {
        Value::Boolean(b) => Some(SwitchValue::Bool(*b.value())),
        Value::Integer(n) => Some(SwitchValue::Int(*n.value())),
        Value::String(s) => Some(SwitchValue::Word(s.value().clone())),
        _ => None,
    }
}

struct Reader<'a> {
    text: &'a str,
    diagnostics: Vec<ConfigDiagnostic>,
}

impl Reader<'_> {
    fn push(
        &mut self,
        severity: Severity,
        span: Option<std::ops::Range<usize>>,
        key: Option<String>,
        message: String,
    ) {
        let (line, column) = position(self.text, span.map_or(0, |s| s.start));
        self.diagnostics.push(ConfigDiagnostic {
            severity,
            line,
            column,
            key,
            message,
        });
    }

    fn invalid(&mut self, item: &Item, key: &str, message: &str) {
        self.push(
            Severity::Error,
            item.span(),
            Some(key.to_owned()),
            message.to_owned(),
        );
    }
}

/// The 1-based line and column (in characters) of byte `offset`.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[start..].chars().count() + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{PlannedProfile, ProfileId};

    #[test]
    fn the_documented_example_reads_cleanly() {
        let (config, diagnostics) = parse(
            "# .glyphvane.toml\nprofile = \"extended\"\n[switches]\nsmart_punctuation = true\ntilde_single = \"strike\"\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            config.profile,
            Some(Requested::BuiltIn(ProfileId::Extended))
        );
        let switches: Vec<_> = config
            .switches
            .iter()
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect();
        assert_eq!(
            switches,
            [
                ("smart_punctuation".to_owned(), SwitchValue::Bool(true)),
                (
                    "tilde_single".to_owned(),
                    SwitchValue::Word("strike".into())
                ),
            ]
        );
        assert_eq!(config.root, None);
    }

    #[test]
    fn planned_profiles_and_root_are_accepted() {
        let (config, diagnostics) = parse("profile = \"mkdocs\"\nroot = true\n");
        assert!(diagnostics.is_empty());
        assert_eq!(
            config.profile,
            Some(Requested::Planned(PlannedProfile::MkDocs))
        );
        assert_eq!(config.root, Some(true));
    }

    #[test]
    fn unknown_keys_warn_and_the_rest_still_loads() {
        let (config, diagnostics) =
            parse("colour = \"red\"\nprofile = \"gfm-file\"\n[switches]\nsparkles = true\n");
        assert_eq!(config.profile, Some(Requested::BuiltIn(ProfileId::GfmFile)));
        assert!(config.switches.is_empty());
        assert_eq!(diagnostics.len(), 2);
        assert!(diagnostics.iter().all(|d| d.severity == Severity::Warning));
        assert_eq!(diagnostics[0].key.as_deref(), Some("colour"));
        assert_eq!((diagnostics[0].line, diagnostics[0].column), (1, 1));
        assert_eq!(diagnostics[1].key.as_deref(), Some("switches.sparkles"));
        assert_eq!(diagnostics[1].line, 4);
        assert_eq!(
            diagnostics[1].to_string(),
            "4:1: warning: `switches.sparkles`: unknown switch; ignored"
        );
    }

    #[test]
    fn invalid_values_are_errors_for_that_key_only() {
        let (config, diagnostics) = parse(
            "profile = \"markdown-plus\"\nroot = \"yes\"\n[switches]\ntilde_single = \"sub\"\ntables = 1\nhighlight = true\nemoji = [1]\n",
        );
        assert_eq!(config.profile, None);
        assert_eq!(config.root, None);
        assert_eq!(config.switches.iter().count(), 1, "only highlight");
        let keys: Vec<_> = diagnostics.iter().map(|d| d.key.as_deref()).collect();
        assert_eq!(
            keys,
            [
                Some("profile"),
                Some("root"),
                Some("switches.tilde_single"),
                Some("switches.tables"),
                Some("switches.emoji"),
            ]
        );
        assert!(diagnostics.iter().all(|d| d.severity == Severity::Error));
        assert!(
            diagnostics[0]
                .message
                .contains("unknown profile `markdown-plus`")
        );
        assert_eq!((diagnostics[0].line, diagnostics[0].column), (1, 11));
        assert!(diagnostics[2].message.contains("\"subscript\""));
        assert_eq!(diagnostics[3].message, "expected true or false");
    }

    #[test]
    fn a_syntax_error_is_one_error_and_an_empty_config() {
        let (config, diagnostics) = parse("profile = \"extended\"\nswitches = {\n");
        assert_eq!(config, FolderConfig::default());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert!(diagnostics[0].line >= 2);
        let (_, wrong_table) = parse("switches = 3\n");
        assert_eq!(wrong_table[0].message, "expected a table of switches");
    }

    #[test]
    fn files_are_read_from_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "profile = \"commonmark\"\nextra = 1\n").expect("write");
        let file = ConfigFile::read(&path).expect("read");
        assert_eq!(
            file.config.profile,
            Some(Requested::BuiltIn(ProfileId::CommonMark))
        );
        assert!(!file.has_errors());
        assert_eq!(file.diagnostics.len(), 1);
        assert!(ConfigFile::read(&dir.path().join("missing.toml")).is_err());
    }
}

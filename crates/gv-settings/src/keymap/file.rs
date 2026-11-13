// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Reading `keymap.toml`: versions and migrations, entries, and diagnostics
//! with line and column. The format is described in [`super`].

use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use toml_edit::{Item, TableLike, Value};

use super::command::CommandId;
use super::context::ContextExpr;
use super::diagnostic::{BindingOrigin, KeymapDiagnostic, KeymapIssue};
use super::keystroke::{KeySequence, Platform};
use crate::diagnostic::{Position, Severity};
use crate::file::{SettingsError, atomic_write, backup_path};
use crate::layer::LayerKind;
use crate::migrate::{self, Migration, MigrationError};

/// The keymap file version this build reads and writes.
pub const KEYMAP_VERSION: u32 = 1;

/// The keymap's migration chain. Version 1 is the first version, so it is
/// empty. Steps use the settings [`Migration`] type and helpers such as
/// [`crate::rename_key`].
const KEYMAP_MIGRATIONS: &[Migration] = &[];

/// The keymap migration chain of this build.
pub fn keymap_migrations() -> &'static [Migration] {
    KEYMAP_MIGRATIONS
}

/// The layer of user bindings: the keymap file lives in the config dir and
/// applies to all vaults.
const LAYER: LayerKind = LayerKind::Global;

/// Where a user binding applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryContext {
    /// No `context` key: the command's own context.
    Command,
    /// `context = ""`: everywhere.
    Anywhere,
    /// A predicate.
    Expr(ContextExpr),
}

/// What an entry does.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryAction {
    /// `[[bind]]`: bind the keys to a command, with optional arguments.
    Bind {
        command: CommandId,
        args: Option<toml::Value>,
    },
    /// `[[unbind]]`: remove built-in bindings, optionally only those of one
    /// command.
    Unbind { command: Option<CommandId> },
}

/// One valid `bind` or `unbind` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct UserEntry {
    pub action: EntryAction,
    /// The keys; always set for `bind`, optional for `unbind`.
    pub keys: Option<KeySequence>,
    /// For `bind`: where it applies. For `unbind`: which built-in bindings
    /// to remove (`Command` means in any context).
    pub context: EntryContext,
    /// The platform section the entry is in; `None` for top-level entries.
    pub platform: Option<Platform>,
    pub origin: BindingOrigin,
    /// The byte range of the `command` value.
    pub(crate) command_span: Option<Range<usize>>,
    /// The byte range of the `context` value.
    pub(crate) context_span: Option<Range<usize>>,
}

/// The parsed `keymap.toml`, for one platform.
#[derive(Debug, Clone)]
pub struct UserKeymap {
    platform: Platform,
    path: Option<PathBuf>,
    text: String,
    version: u32,
    migrated_from: Option<u32>,
    read_only: bool,
    entries: Vec<UserEntry>,
    diagnostics: Vec<KeymapDiagnostic>,
}

impl UserKeymap {
    /// No user bindings.
    pub fn empty(platform: Platform) -> Self {
        Self {
            platform,
            path: None,
            text: String::new(),
            version: KEYMAP_VERSION,
            migrated_from: None,
            read_only: false,
            entries: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Parses the file's contents for `platform`. Every platform section is
    /// checked; only the top-level entries and those of `platform` apply.
    /// Never fails: problems become diagnostics and invalid entries are left
    /// out.
    pub fn parse(text: &str, path: Option<&Path>, platform: Platform) -> Self {
        Self::parse_with(text, path, platform, keymap_migrations(), KEYMAP_VERSION)
    }

    /// [`UserKeymap::parse`] with an explicit migration chain and target
    /// version.
    pub fn parse_with(
        text: &str,
        path: Option<&Path>,
        platform: Platform,
        chain: &[Migration],
        target: u32,
    ) -> Self {
        let mut keymap = Self::empty(platform);
        keymap.path = path.map(Path::to_path_buf);
        keymap.version = target;
        let mut text = text.to_owned();
        match migrate::migrate(&text, chain, target) {
            Ok(None) => {}
            Ok(Some(migrated)) => {
                keymap.migrated_from = Some(migrated.from);
                keymap.push(KeymapDiagnostic::new(
                    Severity::Info,
                    KeymapIssue::Migrated,
                    LAYER,
                    format!(
                        "upgraded from keymap version {} to {}",
                        migrated.from, migrated.to
                    ),
                ));
                text = migrated.text;
            }
            Err(MigrationError::Syntax { message, span }) => {
                keymap.push(
                    KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::Syntax,
                        LAYER,
                        format!("not valid TOML, so no binding in this file is used: {message}"),
                    )
                    .at(&text, span),
                );
                keymap.text = text;
                keymap.read_only = true;
                return keymap;
            }
            Err(MigrationError::InvalidVersion { found, span }) => {
                keymap.push(
                    KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::InvalidVersion,
                        LAYER,
                        format!(
                            "`version` must be a whole number from 1 up, found {found}; \
                             the file is read as version {target}"
                        ),
                    )
                    .key("version")
                    .at(&text, span.or_else(|| version_span(&text))),
                );
            }
            Err(MigrationError::Newer { found, supported }) => {
                keymap.version = found;
                keymap.read_only = true;
                keymap.push(
                    KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::NewerVersion,
                        LAYER,
                        format!(
                            "this file was written by a newer version of Glyphvane (keymap \
                             version {found}; this version reads up to {supported}). It is \
                             read as far as possible but not changed"
                        ),
                    )
                    .key("version")
                    .at(&text, version_span(&text)),
                );
            }
            Err(err @ (MigrationError::MissingStep { .. } | MigrationError::Step { .. })) => {
                let message = match err {
                    MigrationError::MissingStep { from } => {
                        format!("no migration from keymap version {from}")
                    }
                    MigrationError::Step { from, message } => {
                        format!("upgrading from keymap version {from} failed: {message}")
                    }
                    _ => String::new(),
                };
                keymap.push(KeymapDiagnostic::new(
                    Severity::Error,
                    KeymapIssue::MigrationFailed,
                    LAYER,
                    format!("{message}, so no binding in this file is used"),
                ));
                keymap.text = text;
                keymap.read_only = true;
                return keymap;
            }
        }
        keymap.read_entries(&text);
        keymap.text = text;
        keymap
    }

    /// Loads `keymap.toml` for `platform`. A missing file has no bindings.
    /// A file from an older version is upgraded in memory and on disk, with
    /// the old file kept as `keymap.toml.v{N}.bak`.
    pub fn load(path: impl Into<PathBuf>, platform: Platform) -> Result<Self, SettingsError> {
        Self::load_with(path, platform, keymap_migrations(), KEYMAP_VERSION)
    }

    /// [`UserKeymap::load`] with an explicit migration chain and target
    /// version.
    pub fn load_with(
        path: impl Into<PathBuf>,
        platform: Platform,
        chain: &[Migration],
        target: u32,
    ) -> Result<Self, SettingsError> {
        let path = path.into();
        let text = match fs::read(&path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    let mut keymap = Self::parse_with("", Some(&path), platform, chain, target);
                    keymap.read_only = true;
                    keymap.push(KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::Syntax,
                        LAYER,
                        "the file is not valid UTF-8, so no binding in it is used",
                    ));
                    return Ok(keymap);
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(SettingsError::Io { path, source }),
        };
        let mut keymap = Self::parse_with(&text, Some(&path), platform, chain, target);
        if let Some(from) = keymap.migrated_from {
            let upgraded = atomic_write(&backup_path(&path, from), text.as_bytes())
                .and_then(|()| atomic_write(&path, keymap.text.as_bytes()));
            if let Err(e) = upgraded {
                keymap.read_only = true;
                keymap.push(KeymapDiagnostic::new(
                    Severity::Error,
                    KeymapIssue::MigrationFailed,
                    LAYER,
                    format!("the upgraded file could not be saved, so it is not changed: {e}"),
                ));
            }
        }
        Ok(keymap)
    }

    /// The platform the file was read for.
    pub fn platform(&self) -> Platform {
        self.platform
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The text the entries were read from (after any in-memory migration).
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The keymap version of the file (after any migration).
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The version the file had before an in-memory migration.
    pub fn migrated_from(&self) -> Option<u32> {
        self.migrated_from
    }

    /// Whether the file must not be written.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Every valid entry, of every platform section: top-level entries
    /// first, then the `macos`, `linux` and `windows` sections, each in file
    /// order.
    pub fn entries(&self) -> &[UserEntry] {
        &self.entries
    }

    /// The entries that apply on [`UserKeymap::platform`], in the order they
    /// are applied (later binds win over earlier ones).
    pub fn active_entries(&self) -> impl Iterator<Item = &UserEntry> {
        self.entries
            .iter()
            .filter(|e| e.platform.is_none_or(|p| p == self.platform))
    }

    /// Problems found in the file itself. [`super::Keymap::diagnostics`]
    /// adds those found when combining it with the built-in bindings.
    pub fn diagnostics(&self) -> &[KeymapDiagnostic] {
        &self.diagnostics
    }

    fn push(&mut self, diagnostic: KeymapDiagnostic) {
        self.diagnostics.push(diagnostic.file(self.path.clone()));
    }

    fn read_entries(&mut self, text: &str) {
        let doc = match toml_edit::Document::parse(text) {
            Ok(doc) => doc,
            Err(e) => {
                // `migrate` parsed the same text, so this is unreachable in
                // practice; report it like any syntax error.
                self.push(
                    KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::Syntax,
                        LAYER,
                        e.message().to_owned(),
                    )
                    .at(text, e.span()),
                );
                return;
            }
        };
        let root = doc.as_table();
        let mut top = Vec::new();
        let mut sections: [Vec<UserEntry>; 3] = Default::default();
        for (name, item) in root.iter() {
            let key_span = root.key(name).and_then(toml_edit::Key::span);
            match name {
                "version" => {}
                "bind" | "unbind" => {
                    let list = self.read_list(text, item, name, None);
                    top.extend(list);
                }
                _ => {
                    let Some(platform) = Platform::from_name(name) else {
                        self.push(
                            unknown_key(
                                "unknown key; expected `version`, `bind`, `unbind`, `macos`, \
                                 `linux` or `windows`",
                            )
                            .key(name)
                            .at(text, key_span),
                        );
                        continue;
                    };
                    let Some(section) = item.as_table_like() else {
                        self.push(
                            KeymapDiagnostic::new(
                                Severity::Error,
                                KeymapIssue::NotATable,
                                LAYER,
                                format!(
                                    "expected a table with `bind` and `unbind` lists, found {}",
                                    describe_item(item)
                                ),
                            )
                            .key(name)
                            .at(text, item.span().or(key_span)),
                        );
                        continue;
                    };
                    let index = Platform::ALL
                        .iter()
                        .position(|p| *p == platform)
                        .unwrap_or(0);
                    for (list_name, list) in section.iter() {
                        let path = format!("{name}.{list_name}");
                        if list_name == "bind" || list_name == "unbind" {
                            let entries = self.read_list(text, list, &path, Some(platform));
                            sections[index].extend(entries);
                        } else {
                            let span = section
                                .get_key_value(list_name)
                                .and_then(|(k, _)| k.span())
                                .or_else(|| list.span());
                            self.push(
                                unknown_key("unknown key; expected `bind` or `unbind`")
                                    .key(path)
                                    .at(text, span),
                            );
                        }
                    }
                }
            }
        }
        self.entries = top;
        for section in sections {
            self.entries.extend(section);
        }
    }

    /// Reads a `bind` or `unbind` list: an array of tables, or an array of
    /// inline tables.
    fn read_list(
        &mut self,
        text: &str,
        item: &Item,
        path: &str,
        platform: Option<Platform>,
    ) -> Vec<UserEntry> {
        let unbind = path.ends_with("unbind");
        let mut out = Vec::new();
        match item {
            Item::ArrayOfTables(tables) => {
                for (i, table) in tables.iter().enumerate() {
                    let key = format!("{path}[{i}]");
                    if let Some(e) =
                        self.read_entry(text, table, table.span(), key, unbind, platform)
                    {
                        out.push(e);
                    }
                }
            }
            Item::Value(Value::Array(array)) => {
                for (i, value) in array.iter().enumerate() {
                    let key = format!("{path}[{i}]");
                    match value {
                        Value::InlineTable(table) => {
                            if let Some(e) =
                                self.read_entry(text, table, table.span(), key, unbind, platform)
                            {
                                out.push(e);
                            }
                        }
                        other => self.push(
                            KeymapDiagnostic::new(
                                Severity::Error,
                                KeymapIssue::NotATable,
                                LAYER,
                                format!(
                                    "expected a table such as {{ keys = \"…\", command = \"…\" }}, \
                                     found {}",
                                    crate::spec::describe(other)
                                ),
                            )
                            .key(key)
                            .at(text, other.span()),
                        ),
                    }
                }
            }
            other => {
                let header = if unbind { "[[unbind]]" } else { "[[bind]]" };
                self.push(
                    KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::NotATable,
                        LAYER,
                        format!(
                            "expected a list of tables (`{header}` sections), found {}",
                            describe_item(other)
                        ),
                    )
                    .key(path)
                    .at(text, other.span()),
                );
            }
        }
        out
    }

    fn read_entry(
        &mut self,
        text: &str,
        table: &dyn TableLike,
        span: Option<Range<usize>>,
        key: String,
        unbind: bool,
        platform: Option<Platform>,
    ) -> Option<UserEntry> {
        let parse_platform = platform.unwrap_or(self.platform);
        let mut valid = true;
        let mut keys = None;
        let mut command = None;
        let mut command_span = None;
        let mut context = EntryContext::Command;
        let mut context_span = None;
        let mut args = None;
        for (field, item) in table.iter() {
            let field_path = format!("{key}.{field}");
            let value_span = item.span();
            match (field, item) {
                ("keys", Item::Value(Value::String(s))) => {
                    match KeySequence::parse(s.value(), parse_platform) {
                        Ok(parsed) => keys = Some(parsed),
                        Err(e) => {
                            valid = false;
                            self.push(
                                KeymapDiagnostic::new(
                                    Severity::Error,
                                    KeymapIssue::InvalidKeys,
                                    LAYER,
                                    format!("{e}; the entry is ignored"),
                                )
                                .key(field_path)
                                .at(text, inner_span(text, value_span, s.value(), e.range)),
                            );
                        }
                    }
                }
                ("command", Item::Value(Value::String(s))) => {
                    command_span = value_span.clone();
                    match CommandId::new(s.value().clone()) {
                        Ok(id) => command = Some(id),
                        Err(e) => {
                            valid = false;
                            self.push(
                                KeymapDiagnostic::new(
                                    Severity::Error,
                                    KeymapIssue::InvalidEntry,
                                    LAYER,
                                    format!("{e}; the entry is ignored"),
                                )
                                .key(field_path)
                                .at(text, value_span),
                            );
                        }
                    }
                }
                ("context", Item::Value(Value::String(s))) => {
                    context_span = value_span.clone();
                    if s.value().trim().is_empty() {
                        context = EntryContext::Anywhere;
                    } else {
                        match ContextExpr::parse(s.value()) {
                            Ok(expr) => context = EntryContext::Expr(expr),
                            Err(e) => {
                                valid = false;
                                self.push(
                                    KeymapDiagnostic::new(
                                        Severity::Error,
                                        KeymapIssue::InvalidContext,
                                        LAYER,
                                        format!("{e}; the entry is ignored"),
                                    )
                                    .key(field_path)
                                    .at(text, inner_span(text, value_span, s.value(), e.range)),
                                );
                            }
                        }
                    }
                }
                ("args", Item::Value(v)) if !unbind => match to_toml(v) {
                    Some(value) => args = Some(value),
                    None => {
                        valid = false;
                        self.push(
                            KeymapDiagnostic::new(
                                Severity::Error,
                                KeymapIssue::InvalidEntry,
                                LAYER,
                                "these arguments could not be read; the entry is ignored",
                            )
                            .key(field_path)
                            .at(text, value_span),
                        );
                    }
                },
                ("args", Item::Table(_)) if !unbind => {
                    valid = false;
                    self.push(
                        KeymapDiagnostic::new(
                            Severity::Error,
                            KeymapIssue::InvalidEntry,
                            LAYER,
                            "write the arguments inline, as `args = { … }`; the entry is ignored",
                        )
                        .key(field_path)
                        .at(text, value_span),
                    );
                }
                ("keys" | "command" | "context", _) => {
                    valid = false;
                    self.push(
                        KeymapDiagnostic::new(
                            Severity::Error,
                            KeymapIssue::InvalidEntry,
                            LAYER,
                            format!(
                                "expected a string, found {}; the entry is ignored",
                                describe_item(item)
                            ),
                        )
                        .key(field_path)
                        .at(text, value_span),
                    );
                }
                _ => {
                    let span = table
                        .get_key_value(field)
                        .and_then(|(k, _)| k.span())
                        .or(value_span);
                    let expected = if unbind {
                        "unknown key; an `unbind` entry has `keys`, `command` and `context`"
                    } else {
                        "unknown key; a `bind` entry has `keys`, `command`, `context` and `args`"
                    };
                    self.push(unknown_key(expected).key(field_path).at(text, span));
                }
            }
        }
        let action = if unbind {
            if keys.is_none() && command.is_none() && valid {
                self.push(
                    KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::InvalidEntry,
                        LAYER,
                        "an `unbind` entry needs `keys`, `command` or both; the entry is ignored",
                    )
                    .key(key.clone())
                    .at(text, span.clone()),
                );
                valid = false;
            }
            EntryAction::Unbind { command }
        } else {
            let mut missing = Vec::new();
            if keys.is_none() && valid {
                missing.push("`keys`");
            }
            if command.is_none() && valid {
                missing.push("`command`");
            }
            if !missing.is_empty() {
                self.push(
                    KeymapDiagnostic::new(
                        Severity::Error,
                        KeymapIssue::InvalidEntry,
                        LAYER,
                        format!(
                            "a `bind` entry needs {}; the entry is ignored",
                            missing.join(" and ")
                        ),
                    )
                    .key(key.clone())
                    .at(text, span.clone()),
                );
                valid = false;
            }
            EntryAction::Bind {
                command: command?,
                args,
            }
        };
        if !valid {
            return None;
        }
        let position = span.as_ref().map(|s| Position::of(text, s.start));
        Some(UserEntry {
            action,
            keys,
            context,
            platform,
            origin: BindingOrigin {
                layer: LAYER,
                file: self.path.clone(),
                span,
                position,
                key: Some(key),
            },
            command_span,
            context_span,
        })
    }
}

/// The byte range of the `version` value.
fn version_span(text: &str) -> Option<Range<usize>> {
    let doc = toml_edit::Document::parse(text).ok()?;
    doc.as_table().get("version")?.span()
}

fn unknown_key(message: &str) -> KeymapDiagnostic {
    KeymapDiagnostic::new(Severity::Warning, KeymapIssue::UnknownKey, LAYER, message)
}

/// The byte range of `range` (relative to the string's value) in the file,
/// when the string is written without escapes; otherwise the whole value.
fn inner_span(
    text: &str,
    value_span: Option<Range<usize>>,
    value: &str,
    range: Range<usize>,
) -> Option<Range<usize>> {
    let span = value_span?;
    let raw = text.get(span.clone())?;
    let plain = raw.len() == value.len() + 2
        && (raw.starts_with('"') || raw.starts_with('\''))
        && raw.get(1..raw.len() - 1) == Some(value);
    if plain && range.end <= value.len() {
        Some(span.start + 1 + range.start..span.start + 1 + range.end)
    } else {
        Some(span)
    }
}

/// Converts a parsed value to a plain TOML value.
fn to_toml(value: &Value) -> Option<toml::Value> {
    let mut value = value.clone();
    value.decor_mut().clear();
    let mut table: toml::Table = toml::from_str(&format!("v = {value}")).ok()?;
    table.remove("v")
}

fn describe_item(item: &Item) -> String {
    match item {
        Item::Value(v) => crate::spec::describe(v),
        Item::Table(_) => "a table".to_owned(),
        Item::ArrayOfTables(_) => "an array of tables".to_owned(),
        Item::None => "nothing".to_owned(),
    }
}

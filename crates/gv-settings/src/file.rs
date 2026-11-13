// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Settings files on disk: loading with the on-disk upgrade, programmatic
//! edits, and atomic writes.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::edit::{self, EditError};
use crate::layer::{Layer, LayerKind};
use crate::migrate::{self, CURRENT_VERSION, Migration};
use crate::spec::{self, Scope};

/// Why a settings file operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// Reading or writing failed.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// No setting has this path.
    #[error("`{0}` is not a setting")]
    UnknownKey(String),
    /// The value does not fit the setting.
    #[error("{key}: {message}")]
    InvalidValue { key: String, message: String },
    /// The setting can only be set in the global file.
    #[error("{0}: this setting applies to all vaults and can only be set globally")]
    GlobalOnly(String),
    /// The file must not be written (newer version, invalid TOML, failed
    /// migration).
    #[error("{}: not changed: {reason}", path.display())]
    ReadOnly { path: PathBuf, reason: String },
    /// The text edit was refused.
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// A settings file and the layer read from it.
#[derive(Debug, Clone)]
pub struct SettingsFile {
    path: PathBuf,
    kind: LayerKind,
    text: String,
    layer: Layer,
    chain: &'static [Migration],
    target: u32,
}

impl SettingsFile {
    /// Loads the file at `path` as a layer of `kind`. A missing file is an
    /// empty layer. A file from an older version is upgraded: for files the
    /// app owns (global and vault overlays) the old file is kept as
    /// `settings.toml.v{N}.bak` and the upgraded one is written; in-vault
    /// files are upgraded in memory only, because nothing is written into a
    /// vault unasked.
    pub fn load(path: impl Into<PathBuf>, kind: LayerKind) -> Result<Self, SettingsError> {
        Self::load_with(path, kind, migrate::migrations(), CURRENT_VERSION)
    }

    /// [`SettingsFile::load`] with an explicit migration chain and target
    /// version.
    pub fn load_with(
        path: impl Into<PathBuf>,
        kind: LayerKind,
        chain: &'static [Migration],
        target: u32,
    ) -> Result<Self, SettingsError> {
        let path = path.into();
        let text = match fs::read(&path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => {
                    let mut layer = Layer::parse_with(kind, Some(&path), "", chain, target);
                    layer.mark_read_only(Diagnostic::new(
                        Severity::Error,
                        DiagnosticKind::Syntax,
                        kind,
                        "the file is not valid UTF-8, so nothing in it is used",
                    ));
                    return Ok(Self {
                        path,
                        kind,
                        text: String::new(),
                        layer,
                        chain,
                        target,
                    });
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(SettingsError::Io { path, source }),
        };
        let mut layer = Layer::parse_with(kind, Some(&path), &text, chain, target);
        let mut on_disk = text;
        if let Some(from) = layer.migrated_from()
            && !kind.is_in_vault()
        {
            match upgrade_on_disk(&path, &on_disk, layer.text(), from) {
                Ok(()) => on_disk = layer.text().to_owned(),
                Err(e) => layer.mark_read_only(Diagnostic::new(
                    Severity::Error,
                    DiagnosticKind::MigrationFailed,
                    kind,
                    format!("the upgraded file could not be saved, so it is not changed: {e}"),
                )),
            }
        }
        Ok(Self {
            path,
            kind,
            text: on_disk,
            layer,
            chain,
            target,
        })
    }

    /// The file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The layer kind.
    pub fn kind(&self) -> LayerKind {
        self.kind
    }

    /// The file contents as last read or written.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The layer read from the file.
    pub fn layer(&self) -> &Layer {
        &self.layer
    }

    /// Sets `key` to `value` and writes the file. Only that value's bytes
    /// change (or one line is added); comments and unknown keys stay.
    pub fn set<T: Serialize + ?Sized>(
        &mut self,
        key: &str,
        value: &T,
    ) -> Result<(), SettingsError> {
        let value = toml::Value::try_from(value).map_err(|e| SettingsError::InvalidValue {
            key: key.to_owned(),
            message: e.to_string(),
        })?;
        self.set_value(key, &value)
    }

    /// [`SettingsFile::set`] with a TOML value.
    pub fn set_value(&mut self, key: &str, value: &toml::Value) -> Result<(), SettingsError> {
        let spec = spec::spec(key).ok_or_else(|| SettingsError::UnknownKey(key.to_owned()))?;
        if spec.scope == Scope::GlobalOnly && self.kind.is_vault_level() {
            return Err(SettingsError::GlobalOnly(key.to_owned()));
        }
        let value = spec
            .check_owned(value)
            .map_err(|e| SettingsError::InvalidValue {
                key: key.to_owned(),
                message: e.message,
            })?;
        self.ensure_writable()?;
        let mut base = self.base_text()?;
        if base.trim().is_empty() {
            base = format!("version = {}\n", self.target);
        }
        let text = edit::set_value(&base, key, &value)?;
        self.write(text)
    }

    /// The text to edit. An in-vault file upgraded only in memory is written
    /// upgraded (with a backup) before its first edit.
    fn base_text(&mut self) -> Result<String, SettingsError> {
        if let Some(from) = self.layer.migrated_from()
            && self.text != self.layer.text()
        {
            self.ensure_writable()?;
            upgrade_on_disk(&self.path, &self.text, self.layer.text(), from).map_err(|source| {
                SettingsError::Io {
                    path: self.path.clone(),
                    source,
                }
            })?;
            self.text = self.layer.text().to_owned();
        }
        Ok(self.text.clone())
    }

    /// Removes `key` from the file, so the layer below applies again. The
    /// comments on its line go with it.
    pub fn remove(&mut self, key: &str) -> Result<(), SettingsError> {
        let base = self.base_text()?;
        let text = edit::remove_value(&base, key)?;
        if text == self.text {
            return Ok(());
        }
        self.ensure_writable()?;
        self.write(text)
    }

    /// Re-reads the file from disk.
    pub fn reload(&mut self) -> Result<(), SettingsError> {
        *self = Self::load_with(self.path.clone(), self.kind, self.chain, self.target)?;
        Ok(())
    }

    /// Live reload with contents the watcher already read.
    pub fn reload_from_text(&mut self, text: String) {
        self.layer = self.parse(&text);
        self.text = text;
    }

    fn parse(&self, text: &str) -> Layer {
        Layer::parse_with(self.kind, Some(&self.path), text, self.chain, self.target)
    }

    fn ensure_writable(&self) -> Result<(), SettingsError> {
        if !self.layer.is_read_only() {
            return Ok(());
        }
        let reason = self
            .layer
            .diagnostics()
            .iter()
            .find(|d| d.severity == Severity::Error)
            .map_or_else(
                || "the file cannot be used".to_owned(),
                |d| d.message.clone(),
            );
        Err(SettingsError::ReadOnly {
            path: self.path.clone(),
            reason,
        })
    }

    fn write(&mut self, text: String) -> Result<(), SettingsError> {
        atomic_write(&self.path, text.as_bytes()).map_err(|source| SettingsError::Io {
            path: self.path.clone(),
            source,
        })?;
        self.layer = self.parse(&text);
        self.text = text;
        Ok(())
    }
}

fn upgrade_on_disk(path: &Path, old: &str, new: &str, from: u32) -> io::Result<()> {
    let backup = backup_path(path, from);
    atomic_write(&backup, old.as_bytes())?;
    atomic_write(path, new.as_bytes())
}

/// The first free backup name for a file of settings version `version`:
/// `settings.toml.v1.bak`, then `settings.toml.v1-2.bak`, and so on.
pub fn backup_path(path: &Path, version: u32) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.toml".to_owned());
    let mut candidate = path.with_file_name(format!("{name}.v{version}.bak"));
    let mut n = 2;
    while candidate.exists() {
        candidate = path.with_file_name(format!("{name}.v{version}-{n}.bak"));
        n += 1;
    }
    candidate
}

/// Writes `contents` to `path` atomically: a temporary file in the same
/// directory, `fsync`, rename over the original, then `fsync` of the
/// directory where the platform supports it. A symlink is followed and its
/// target replaced; the link itself stays. The original's permissions are
/// kept. Missing parent directories are created.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let target = resolve_symlink(path)?;
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{name}.gv-tmp-"))
        .tempfile_in(&dir)?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    if let Ok(meta) = fs::metadata(&target) {
        fs::set_permissions(temp.path(), meta.permissions())?;
    }
    persist(temp, &target)?;
    sync_dir(&dir);
    Ok(())
}

fn resolve_symlink(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}

#[cfg(not(windows))]
fn persist(temp: tempfile::NamedTempFile, target: &Path) -> io::Result<()> {
    temp.persist(target).map(drop).map_err(|e| e.error)
}

/// Windows: retry on sharing violations (antivirus, indexers, sync clients)
/// with backoff for up to about 2 s.
#[cfg(windows)]
fn persist(mut temp: tempfile::NamedTempFile, target: &Path) -> io::Result<()> {
    let mut delay = std::time::Duration::from_millis(20);
    let mut waited = std::time::Duration::ZERO;
    loop {
        match temp.persist(target) {
            Ok(_) => return Ok(()),
            Err(e)
                if e.error.kind() == io::ErrorKind::PermissionDenied
                    && waited < std::time::Duration::from_secs(2) =>
            {
                temp = e.file;
                std::thread::sleep(delay);
                waited += delay;
                delay = (delay * 2).min(std::time::Duration::from_millis(400));
            }
            Err(e) => return Err(e.error),
        }
    }
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    // Best effort: the rename already happened; a failed directory fsync
    // only weakens durability across a power cut.
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

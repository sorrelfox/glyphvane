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
    /// `text` has edits not written yet ([`SettingsFile::set_value_unsaved`]).
    unsaved: bool,
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
                        unsaved: false,
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
            unsaved: false,
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

    /// The file contents as last read or written, with the edits not
    /// written yet.
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
        let text = self.edited(key, value)?;
        self.write(text)
    }

    /// [`SettingsFile::set_value`] in memory only: the layer has the new
    /// value at once and [`SettingsFile::save`] writes it later, for values
    /// that change many times a second (the zoom under the mouse wheel).
    /// The next write of any edit writes this one too.
    pub fn set_value_unsaved(
        &mut self,
        key: &str,
        value: &toml::Value,
    ) -> Result<(), SettingsError> {
        let text = self.edited(key, value)?;
        if text != self.text {
            self.layer = self.parse(&text);
            self.text = text;
            self.unsaved = true;
        }
        Ok(())
    }

    /// Whether edits wait to be written ([`SettingsFile::set_value_unsaved`]).
    pub fn has_unsaved(&self) -> bool {
        self.unsaved
    }

    /// Writes the edits made with [`SettingsFile::set_value_unsaved`], if
    /// any.
    pub fn save(&mut self) -> Result<(), SettingsError> {
        if !self.unsaved {
            return Ok(());
        }
        self.ensure_writable()?;
        let text = self.text.clone();
        self.write(text)
    }

    /// The text with `key` set to `value`, checked against the schema.
    fn edited(&mut self, key: &str, value: &toml::Value) -> Result<String, SettingsError> {
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
        Ok(edit::set_value(&base, key, &value)?)
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
        self.unsaved = false;
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
        self.unsaved = false;
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
    atomic_write_with(path, contents, &mut |_, _| Ok(()))
}

/// [`atomic_write`] with a hook for the metadata it does not copy itself.
///
/// The temporary file is named `.<name>.gv-tmp-<random>` and lives next to
/// the target. The steps are: write the contents; copy the original's
/// permissions, and its file attributes and creation time where the platform
/// has them (Windows: hidden, system, archive and not-indexed attributes and
/// the creation time; macOS: the creation time); call `before_rename` with
/// the temporary file and the original's metadata (`None` for a new file);
/// `fsync`; rename over the target; `fsync` the directory. On Windows the
/// rename is `ReplaceFileW` when the target exists, which also keeps the
/// original's ACL and alternate data streams (a plain rename where it does
/// not apply), retried with backoff for up to about 2 s while another
/// process holds the file open, as antivirus scanners, indexers and sync
/// clients do.
///
/// `before_rename` copies what only the caller knows how to copy (owner,
/// extended attributes) or checks a last condition. If it or any step before
/// the rename fails, the temporary file is removed and the original is left
/// untouched.
pub fn atomic_write_with(
    path: &Path,
    contents: &[u8],
    before_rename: &mut dyn FnMut(&fs::File, Option<&fs::Metadata>) -> io::Result<()>,
) -> io::Result<()> {
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
    let original = fs::metadata(&target).ok();
    let mut temp = create_temp(&dir, &name, original.as_ref())?;
    temp.write_all(contents)?;
    if let Some(meta) = &original {
        temp.as_file().set_permissions(meta.permissions())?;
        copy_creation_time(temp.as_file(), meta);
    }
    before_rename(temp.as_file(), original.as_ref())?;
    temp.as_file().sync_all()?;
    persist(temp, &target)?;
    sync_dir(&dir);
    Ok(())
}

#[cfg(not(windows))]
fn create_temp(
    dir: &Path,
    name: &str,
    _original: Option<&fs::Metadata>,
) -> io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(&format!(".{name}.gv-tmp-"))
        .tempfile_in(dir)
}

/// Windows: the temporary file is created with the original's hidden,
/// system, archive and not-content-indexed attributes, which can only be set
/// when a file is created. The read-only attribute is not copied: the
/// temporary file must stay writable and removable.
#[cfg(windows)]
fn create_temp(
    dir: &Path,
    name: &str,
    original: Option<&fs::Metadata>,
) -> io::Result<tempfile::NamedTempFile> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    const ARCHIVE: u32 = 0x20;
    const NOT_CONTENT_INDEXED: u32 = 0x2000;
    let attributes = original.map_or(0, |m| {
        m.file_attributes() & (HIDDEN | SYSTEM | ARCHIVE | NOT_CONTENT_INDEXED)
    });
    tempfile::Builder::new()
        .prefix(&format!(".{name}.gv-tmp-"))
        .make_in(dir, |path| {
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .attributes(attributes)
                .open(path)
        })
}

/// Best effort: a file system without creation times keeps the new file's.
#[cfg(any(windows, target_os = "macos"))]
fn copy_creation_time(temp: &fs::File, original: &fs::Metadata) {
    #[cfg(target_os = "macos")]
    use std::os::macos::fs::FileTimesExt;
    #[cfg(windows)]
    use std::os::windows::fs::FileTimesExt;
    if let Ok(created) = original.created() {
        let _ = temp.set_times(fs::FileTimes::new().set_created(created));
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn copy_creation_time(_temp: &fs::File, _original: &fs::Metadata) {}

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

/// Windows: `ReplaceFileW` swaps the temporary file in. Unlike a rename, it
/// keeps what the original carries beyond its attributes: its ACL, creation
/// time, short name, object id, encryption, compression and alternate data
/// streams. Where it does not apply (a new file, or a file system that does
/// not support it), the temporary file is renamed over the target instead.
/// Both are retried with backoff for up to about 2 s while another process
/// holds the file open (antivirus scanners, indexers, sync clients).
#[cfg(windows)]
fn persist(temp: tempfile::NamedTempFile, target: &Path) -> io::Result<()> {
    use std::time::Duration;

    // `ReplaceFileW` opens the replacement itself: close it first.
    let mut temp = temp.into_temp_path();
    let mut replace = fs::symlink_metadata(target).is_ok();
    // Set when a failed `ReplaceFileW` removed the original: the new
    // contents then exist only in the temporary file, which is kept.
    let mut original_gone = false;
    let mut delay = Duration::from_millis(20);
    let mut waited = Duration::ZERO;
    loop {
        let result = if replace {
            replace_file::replace(target, &temp)
        } else {
            fs::rename(&temp, target)
        };
        let error = match result {
            Ok(()) => {
                // The temporary name is gone (renamed or replaced).
                temp.disable_cleanup(true);
                return Ok(());
            }
            Err(error) => error,
        };
        if replace {
            match replace_file::failure(&error) {
                replace_file::Failure::NotApplicable => {
                    replace = false;
                    continue;
                }
                replace_file::Failure::OriginalRemoved => {
                    original_gone = true;
                    replace = false;
                    continue;
                }
                replace_file::Failure::Other => {}
            }
        }
        if is_sharing_violation(&error) && waited < Duration::from_secs(2) {
            std::thread::sleep(delay);
            waited += delay;
            delay = (delay * 2).min(Duration::from_millis(400));
            continue;
        }
        if original_gone {
            temp.disable_cleanup(true);
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; the new contents are in {}",
                    temp.to_path_buf().display()
                ),
            ));
        }
        return Err(error);
    }
}

/// `ReplaceFileW`, the one call in this crate that needs `unsafe`: the
/// standard library has no safe way to replace a file while keeping its
/// ACL and streams.
#[cfg(windows)]
mod replace_file {
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;

    use windows_sys::Win32::Foundation::{
        ERROR_CALL_NOT_IMPLEMENTED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION,
        ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_UNABLE_TO_MOVE_REPLACEMENT,
        ERROR_UNABLE_TO_REMOVE_REPLACED,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        REPLACEFILE_IGNORE_ACL_ERRORS, REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW,
    };

    /// How a failed [`replace`] left the two files.
    pub(super) enum Failure {
        /// Nothing changed and the call cannot work here (the file system
        /// does not support it, or the original vanished): rename instead.
        NotApplicable,
        /// The original was removed but the replacement could not take its
        /// name (`ERROR_UNABLE_TO_MOVE_REPLACEMENT` without a backup file):
        /// a rename finishes the job.
        OriginalRemoved,
        /// Nothing changed: a sharing violation to retry, or an error to
        /// report.
        Other,
    }

    pub(super) fn failure(error: &io::Error) -> Failure {
        let Some(code) = error.raw_os_error().and_then(|c| u32::try_from(c).ok()) else {
            return Failure::Other;
        };
        match code {
            ERROR_UNABLE_TO_MOVE_REPLACEMENT => Failure::OriginalRemoved,
            ERROR_INVALID_FUNCTION
            | ERROR_NOT_SUPPORTED
            | ERROR_INVALID_PARAMETER
            | ERROR_CALL_NOT_IMPLEMENTED
            | ERROR_FILE_NOT_FOUND => Failure::NotApplicable,
            // Including ERROR_UNABLE_TO_REMOVE_REPLACED: both files keep
            // their names (see `is_busy`).
            _ => Failure::Other,
        }
    }

    /// Whether `error` is `ReplaceFileW`'s "the original could not be
    /// removed", which another process holding the file open causes.
    pub(super) fn is_busy(error: &io::Error) -> bool {
        error
            .raw_os_error()
            .and_then(|c| u32::try_from(c).ok())
            .is_some_and(|c| c == ERROR_UNABLE_TO_REMOVE_REPLACED)
    }

    /// Replaces `target` with `replacement` (both closed), keeping the
    /// target's ACL, creation time and streams. Errors in merging
    /// attributes or the ACL do not fail the call: the result then has the
    /// replacement's own, as a rename would give it.
    #[expect(
        unsafe_code,
        reason = "FFI call to ReplaceFileW; see the SAFETY comment"
    )]
    pub(super) fn replace(target: &Path, replacement: &Path) -> io::Result<()> {
        let target = wide(target)?;
        let replacement = wide(replacement)?;
        // SAFETY: both names are valid, NUL-terminated UTF-16 strings that
        // outlive the call; the backup name, exclude and reserved arguments
        // are null, which the API allows. The call only reads its arguments.
        let ok = unsafe {
            ReplaceFileW(
                target.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// `path` as an absolute, NUL-terminated wide string in the `\\?\`
    /// form, so paths longer than `MAX_PATH` work.
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let absolute = std::path::absolute(path)?;
        let units: Vec<u16> = absolute.as_os_str().encode_wide().collect();
        let starts = |prefix: &str| {
            let prefix: Vec<u16> = prefix.encode_utf16().collect();
            units.starts_with(&prefix)
        };
        let mut out: Vec<u16> = if starts(r"\\?\") || starts(r"\\.\") {
            units.clone()
        } else if let Some(share) = units.strip_prefix(&[u16::from(b'\\'), u16::from(b'\\')][..]) {
            r"\\?\UNC\"
                .encode_utf16()
                .chain(share.iter().copied())
                .collect()
        } else {
            r"\\?\"
                .encode_utf16()
                .chain(units.iter().copied())
                .collect()
        };
        if out.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{}: the path contains a NUL character",
                    OsString::from_wide(&units).to_string_lossy()
                ),
            ));
        }
        out.push(0);
        Ok(out)
    }
}

/// Access denied (a file open without delete sharing), sharing violation
/// (`ERROR_SHARING_VIOLATION`, 32) or lock violation (`ERROR_LOCK_VIOLATION`,
/// 33).
/// `ReplaceFileW`'s "the original could not be removed"
/// (`ERROR_UNABLE_TO_REMOVE_REPLACED`) counts as well.
#[cfg(windows)]
fn is_sharing_violation(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(32 | 33))
        || replace_file::is_busy(error)
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

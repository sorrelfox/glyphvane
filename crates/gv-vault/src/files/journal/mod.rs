// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The crash journal: unsaved edits survive a crash, a power cut or
//! `kill -9`.
//!
//! # Layout
//!
//! A [`JournalStore`] is one directory, by default
//! `<data dir>/journal/<vault-id>/` ([`JournalStore::for_vault`]). Per
//! document it holds:
//!
//! - `<file-id>`: the journal, an append-only log (format in [`format`]);
//! - `<file-id>.backup`: the original bytes while a hardlinked file is
//!   overwritten in place;
//! - `.<file-id>.lock`: an empty, hidden file that carries the journal's
//!   lock (see [Several processes](#several-processes)).
//!
//! The file id is 32 hexadecimal digits of the SHA-256 of the document's
//! path, or `untitled-<n>` for a document that was never saved (untitled
//! documents live in the journal until their first save). Files are created
//! with mode 0600 and the directory with 0700 on Unix: they hold document
//! text.
//!
//! # Lifecycle
//!
//! 1. Opening a document: [`JournalStore::begin`] with the loaded text as
//!    the *base*. Nothing is written yet.
//! 2. Each applied transaction: [`DocumentJournal::record`]. The first one
//!    writes the journal's header and the base text (atomically), later
//!    ones append one edit record each.
//! 3. A save: [`DocumentJournal::saved`] deletes the journal; the saved text
//!    becomes the new base.
//! 4. Closing without saving: [`DocumentJournal::discard`].
//! 5. Start-up after a crash: every journal still present belongs to a
//!    document with unsaved edits. [`JournalStore::recoverable`] lists them;
//!    [`JournalStore::recover`] rebuilds the text (Restore, Compare);
//!    [`JournalStore::discard`] drops it.
//!
//! # Several processes
//!
//! Two app processes can share a data dir: a safe-mode instance beside a
//! normal one, or two instances with different config dirs. Each journal
//! therefore has an exclusive OS advisory lock, held by its
//! [`DocumentJournal`] for as long as it exists:
//!
//! - [`JournalStore::begin`] (and [`DocumentJournal::saved_as`] onto
//!   another document) fails with [`JournalError::Busy`] while another
//!   handle holds the document's lock, in another process or in this one;
//! - [`JournalStore::scan`] reports such journals as in use rather than
//!   recoverable, and [`JournalStore::recoverable`] leaves them out: they
//!   belong to a running editor, not to a crash;
//! - [`JournalStore::recover`], [`JournalStore::discard`] and
//!   [`JournalStore::restore_backup`] take the lock for their duration and
//!   fail with [`JournalError::Busy`] on a journal in use;
//! - [`JournalStore::remove_stale_temp_files`] keeps the temporary files of
//!   journals in use (another process may be writing them).
//!
//! The OS releases the lock when its file handle closes, which happens
//! however the process ends (exit, panic, `kill -9`, a crash). A journal
//! left by a dead process is therefore never locked, and there are no stale
//! locks to detect or break. The lock is advisory: it binds only processes
//! that take it. The journal's format is not affected.
//!
//! # Durability
//!
//! Each record is handed to the OS with a single write as soon as it is
//! recorded, so a killed process loses nothing it recorded. `fsync` runs on
//! a record when the last one is older than the sync interval (2 s by
//! default), and from [`DocumentJournal::sync_if_due`], which the app calls
//! at [`DocumentJournal::next_sync_deadline`]; a power cut loses at most the
//! interval (NFR-SAFE-02). The header and base, compactions and in-place
//! backups are written atomically and synced at once.
//!
//! # Compaction
//!
//! When the edit records since the last snapshot outgrow
//! `max(compact_min_bytes, 2 × text length)`, the journal is rewritten
//! atomically as header, base and one snapshot of the current text. A crash
//! during compaction leaves either the old or the new journal, and both
//! rebuild the same text.
//!
//! # Merge base
//!
//! [`DocumentJournal::base`] (while running) and [`Recovery::base`] (after a
//! crash) are the text as last loaded or saved: the common ancestor for a
//! 3-way merge with a version changed on disk
//! ([`merge3`](crate::files::merge3)).

pub mod format;
mod lock;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use gv_settings::{InvalidVaultId, Locations};
use gv_text::encoding::decode_with;
use gv_text::{Buffer, TextFormat, TextSnapshot, Transaction};

pub use self::format::{DocumentKey, JournalTail};
use self::format::{Header, Record};
use self::lock::JournalLock;
use super::clock::{Clock, SystemClock};
use super::hash::ContentHash;
use super::write::{Backup, sync_dir};

/// The name of the folder below the data dir that holds the journals.
pub const JOURNAL_FOLDER: &str = "journal";

/// [`DocumentJournal::record_all`] writes its records in pieces of about
/// this size, so a long burst does not build one huge buffer.
const RECORD_BATCH_BYTES: usize = 1024 * 1024;

/// The store id for documents outside any vault.
pub const NO_VAULT_ID: &str = "no-vault";

/// A journal's file name: 32 hex digits, or `untitled-<n>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JournalId(String);

impl JournalId {
    /// The id of `key`'s journal.
    pub fn of(key: &DocumentKey) -> Self {
        match key {
            DocumentKey::Path(path) => {
                let mut bytes = Vec::new();
                format::put_path(&mut bytes, path);
                Self(ContentHash::of(&bytes).to_hex()[..32].to_owned())
            }
            DocumentKey::Untitled(n) => Self(format!("untitled-{n}")),
        }
    }

    /// The id as a file name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses a journal file name.
    pub fn parse(name: &str) -> Option<Self> {
        let hex = name.len() == 32 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        let untitled = name.strip_prefix("untitled-").is_some_and(|n| {
            !n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit())
        });
        (hex || untitled).then(|| Self(name.to_owned()))
    }
}

impl std::fmt::Display for JournalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Tuning of a [`DocumentJournal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalPolicy {
    /// The longest time a recorded edit stays without `fsync` (the power-cut
    /// loss window). Zero syncs every record.
    pub sync_interval: Duration,
    /// Edit records may grow to `max(this, 2 × text length)` bytes before
    /// the journal is compacted into a snapshot.
    pub compact_min_bytes: u64,
    /// A base larger than this is not copied into the journal; recovery then
    /// needs the file on disk (or an in-place backup) to still hold it,
    /// unless a snapshot was written since.
    pub inline_base_limit: usize,
}

impl Default for JournalPolicy {
    /// 2 s, 256 KiB, 4 MiB.
    fn default() -> Self {
        Self {
            sync_interval: Duration::from_secs(2),
            compact_min_bytes: 256 * 1024,
            inline_base_limit: 4 * 1024 * 1024,
        }
    }
}

/// Why a journal operation failed.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// Reading or writing failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The vault id is not a plain path component.
    #[error(transparent)]
    VaultId(#[from] InvalidVaultId),
    /// No journal with this id exists.
    #[error("{0}: no such journal")]
    NotFound(JournalId),
    /// The file is not a journal, or its header is damaged.
    #[error("{0}: not a readable journal")]
    Unreadable(JournalId),
    /// A journal (or an in-place backup) from an earlier session exists for
    /// this document; recover or discard it before starting a new one.
    #[error("{0}: a journal from an earlier session is waiting to be recovered or discarded")]
    PendingRecovery(JournalId),
    /// The edits cannot be replayed: the journal holds no copy of their
    /// base text and the file on disk no longer matches it.
    #[error("{0}: the text the journaled edits apply to is no longer available")]
    BaseUnavailable(JournalId),
    /// The journal is in use: another open [`DocumentJournal`] (in another
    /// process, or in this one) holds its lock. It is not a crashed journal
    /// and must not be written, recovered or discarded from here.
    #[error("{}: the journal is in use by another window or process", path.display())]
    Busy {
        /// The journal file (which may not exist yet).
        path: PathBuf,
    },
}

/// The journals of a store, as [`JournalStore::scan`] found them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalScan {
    /// Journals left from an earlier session (no live process holds them),
    /// by id.
    pub recoverable: Vec<RecoverableDocument>,
    /// Journals a live process (or this one) has open, by id: not crashed,
    /// so not offered for recovery.
    pub in_use: Vec<RecoverableDocument>,
}

/// A document with a journal left from an earlier session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverableDocument {
    /// The journal's id.
    pub id: JournalId,
    /// The document, if the header could be read.
    pub key: Option<DocumentKey>,
    /// When the journal (or backup) was last written.
    pub modified: Option<SystemTime>,
    /// Whether a journal file exists (it may be only a backup).
    pub has_journal: bool,
    /// Whether an in-place write was interrupted: the file may be damaged,
    /// and its original bytes are in the backup.
    pub has_backup: bool,
}

/// How the file on disk relates to a recovered journal's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskComparison {
    /// The file still holds the base: restoring is a plain replay.
    MatchesBase,
    /// The file changed since the base (another program, a sync client):
    /// offer Compare or a 3-way merge.
    Changed,
    /// The file is gone.
    Missing,
    /// An untitled document has no file.
    NoFile,
    /// The file could not be read.
    Unreadable,
}

/// A document rebuilt from its journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// The journal's id.
    pub id: JournalId,
    /// The document.
    pub key: DocumentKey,
    /// The unsaved text.
    pub text: String,
    /// The on-disk format to save it with.
    pub format: TextFormat,
    /// The text as last loaded or saved, when available: the left side of
    /// Compare and the base of a 3-way merge.
    pub base: Option<String>,
    /// The hash of the base's bytes on disk (`None` for untitled).
    pub base_hash: Option<ContentHash>,
    /// How many edit and snapshot records were replayed.
    pub records: u64,
    /// How reading the journal ended.
    pub tail: JournalTail,
    /// The file on disk compared with the base.
    pub disk: DiskComparison,
    /// The original bytes of an interrupted in-place write. When present,
    /// the file itself may be half written; see
    /// [`JournalStore::restore_backup`].
    pub backup: Option<Vec<u8>>,
}

/// The directory of journals for one vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalStore {
    dir: PathBuf,
}

impl JournalStore {
    /// A store in `dir`, created (mode 0700 on Unix) if missing.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        create_private_dir(&dir)?;
        Ok(Self { dir })
    }

    /// The store of vault `vault_id` in the app's data dir:
    /// `<data>/journal/<vault-id>/`. Use [`NO_VAULT_ID`] for files outside a
    /// vault.
    pub fn for_vault(locations: &Locations, vault_id: &str) -> Result<Self, JournalError> {
        // Same rules as the vault's settings folder: one plain component.
        locations.vault_data_dir(vault_id)?;
        Ok(Self::open(
            locations.data_dir().join(JOURNAL_FOLDER).join(vault_id),
        )?)
    }

    /// The store's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn journal_path(&self, id: &JournalId) -> PathBuf {
        self.dir.join(id.as_str())
    }

    fn backup_path(&self, id: &JournalId) -> PathBuf {
        self.dir.join(format!("{id}.backup"))
    }

    fn lock(&self, id: &JournalId) -> Result<JournalLock, JournalError> {
        lock_journal(&self.dir, id)
    }

    /// Starts journaling a document whose current text is `base` (as
    /// loaded or last saved; empty for a new untitled document) in
    /// `format`, with `base_hash` the hash of its bytes on disk (`None` if
    /// it has no file). Nothing is written until the first edit.
    ///
    /// Takes the document's lock, held until the returned journal is
    /// dropped. Fails with [`JournalError::Busy`] while another journal
    /// handle (in any process) has the same document open, and with
    /// [`JournalError::PendingRecovery`] while a journal from an earlier
    /// session exists for it.
    pub fn begin<C: Clock>(
        &self,
        key: DocumentKey,
        base: TextSnapshot,
        format: TextFormat,
        base_hash: Option<ContentHash>,
        policy: JournalPolicy,
        clock: C,
    ) -> Result<DocumentJournal<C>, JournalError> {
        let id = JournalId::of(&key);
        let lock = self.lock(&id)?;
        if exists(&self.journal_path(&id))? || exists(&self.backup_path(&id))? {
            return Err(JournalError::PendingRecovery(id));
        }
        Ok(DocumentJournal {
            dir: self.dir.clone(),
            id,
            _lock: lock,
            key,
            base,
            base_format: format,
            format,
            base_hash,
            policy,
            clock,
            file: None,
            edit_bytes: 0,
            seq: 0,
            last_sync: None,
            unsynced: false,
        })
    }

    /// [`JournalStore::begin`] with the default policy and the system clock.
    pub fn begin_default(
        &self,
        key: DocumentKey,
        base: TextSnapshot,
        format: TextFormat,
        base_hash: Option<ContentHash>,
    ) -> Result<DocumentJournal<SystemClock>, JournalError> {
        self.begin(
            key,
            base,
            format,
            base_hash,
            JournalPolicy::default(),
            SystemClock,
        )
    }

    /// The documents with a journal or backup left from an earlier session,
    /// by id: [`JournalScan::recoverable`] of [`JournalStore::scan`].
    /// Journals in use by a live process are left out.
    pub fn recoverable(&self) -> io::Result<Vec<RecoverableDocument>> {
        Ok(self.scan()?.recoverable)
    }

    /// The documents with a journal or backup in this store, split into
    /// those left from an earlier session and those a live process holds
    /// open (see [Several processes](self#several-processes)).
    pub fn scan(&self) -> io::Result<JournalScan> {
        let mut found: std::collections::BTreeMap<JournalId, RecoverableDocument> =
            std::collections::BTreeMap::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let (id, is_backup) = match name.strip_suffix(".backup") {
                Some(stem) => (JournalId::parse(stem), true),
                None => (JournalId::parse(&name), false),
            };
            let Some(id) = id else {
                continue;
            };
            let modified = entry.metadata().ok().and_then(|m| m.modified().ok());
            let doc = found
                .entry(id.clone())
                .or_insert_with(|| RecoverableDocument {
                    id,
                    key: None,
                    modified: None,
                    has_journal: false,
                    has_backup: false,
                });
            doc.modified = doc.modified.max(modified);
            if is_backup {
                doc.has_backup = true;
            } else {
                doc.has_journal = true;
            }
        }
        let mut scan = JournalScan::default();
        for mut doc in found.into_values() {
            // Held while the header is read.
            let lock = JournalLock::try_acquire(self.lock_path(&doc.id))?;
            doc.key = if doc.has_journal {
                fs::read(self.journal_path(&doc.id))
                    .ok()
                    .and_then(|b| format::parse(&b))
                    .and_then(|p| p.header)
                    .map(|h| h.key)
            } else {
                self.read_backup(&doc.id)
                    .ok()
                    .flatten()
                    .map(|(path, _)| DocumentKey::Path(path))
            };
            match lock {
                Some(_) => scan.recoverable.push(doc),
                None => scan.in_use.push(doc),
            }
        }
        Ok(scan)
    }

    fn lock_path(&self, id: &JournalId) -> PathBuf {
        lock_path(&self.dir, id)
    }

    /// Rebuilds the document of journal `id`.
    ///
    /// The base comes from the journal itself, else from the in-place
    /// backup, else from the file on disk; the last two only when their
    /// hash matches the journal's. Edits are replayed until the first
    /// record that is cut short or does not apply ([`Recovery::tail`]).
    ///
    /// Fails with [`JournalError::Busy`] while the journal is in use.
    pub fn recover(&self, id: &JournalId) -> Result<Recovery, JournalError> {
        let _lock = self.lock(id)?;
        let backup = self.read_backup(id)?;
        let bytes = match fs::read(self.journal_path(id)) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let Some(bytes) = bytes else {
            // Only a backup: an in-place write was interrupted with no
            // unsaved edits journaled.
            let Some((path, original)) = backup else {
                return Err(JournalError::NotFound(id.clone()));
            };
            let decoded = gv_text::decode(&original).ok();
            return Ok(Recovery {
                id: id.clone(),
                disk: compare_disk(&path, Some(ContentHash::of(&original))),
                key: DocumentKey::Path(path),
                text: decoded.as_ref().map(|d| d.text.clone()).unwrap_or_default(),
                format: decoded.map(|d| d.format).unwrap_or_default(),
                base: None,
                base_hash: Some(ContentHash::of(&original)),
                records: 0,
                tail: JournalTail::Complete,
                backup: Some(original),
            });
        };
        let parsed = format::parse(&bytes).ok_or_else(|| JournalError::Unreadable(id.clone()))?;
        let header = parsed
            .header
            .clone()
            .ok_or_else(|| JournalError::Unreadable(id.clone()))?;
        let disk = match &header.key {
            DocumentKey::Path(path) => compare_disk(path, header.base_hash),
            DocumentKey::Untitled(_) => DiskComparison::NoFile,
        };
        let base = self.resolve_base(&header, &parsed.records, backup.as_ref());
        let starts_with_snapshot = parsed
            .records
            .iter()
            .find(|(_, r)| matches!(r, Record::Snapshot { .. } | Record::Edit { .. }))
            .is_some_and(|(_, r)| matches!(r, Record::Snapshot { .. }));
        let start = match &base {
            Some(text) => text.clone(),
            None if starts_with_snapshot => String::new(),
            None => return Err(JournalError::BaseUnavailable(id.clone())),
        };
        let mut buffer = Buffer::new(&start);
        let mut format = header.format;
        let mut records = 0;
        let mut tail = parsed.tail;
        for (at, record) in parsed.records {
            match record {
                Record::Base(_) => {}
                Record::Format(f) => format = f,
                Record::Snapshot { text, .. } => {
                    buffer = Buffer::new(&text);
                    records += 1;
                }
                Record::Edit { replacements, .. } => {
                    let mut transaction = Transaction::new();
                    let mut valid = true;
                    for (start, end, text) in replacements {
                        match (usize::try_from(start), usize::try_from(end)) {
                            (Ok(start), Ok(end)) => transaction.push(start..end, text),
                            _ => valid = false,
                        }
                    }
                    if !valid || buffer.apply(&transaction).is_err() {
                        tail = JournalTail::Corrupt { at };
                        break;
                    }
                    records += 1;
                }
            }
        }
        Ok(Recovery {
            id: id.clone(),
            key: header.key,
            text: buffer.to_string(),
            format,
            base,
            base_hash: header.base_hash,
            records,
            tail,
            disk,
            backup: backup.map(|(_, bytes)| bytes),
        })
    }

    /// The base text of a journal: its Base record, or the backup or disk
    /// file when their hash matches.
    fn resolve_base(
        &self,
        header: &Header,
        records: &[(u64, Record)],
        backup: Option<&(PathBuf, Vec<u8>)>,
    ) -> Option<String> {
        if let Some(text) = records.iter().find_map(|(_, r)| match r {
            Record::Base(text) => Some(text.clone()),
            _ => None,
        }) {
            return Some(text);
        }
        if header.base_hash.is_none() && header.base_len == 0 {
            return Some(String::new());
        }
        let decode = |bytes: &[u8]| {
            (Some(ContentHash::of(bytes)) == header.base_hash)
                .then(|| decode_with(bytes, header.format.encoding).text)
        };
        if let Some(text) = backup.and_then(|(_, bytes)| decode(bytes)) {
            return Some(text);
        }
        match &header.key {
            DocumentKey::Path(path) => fs::read(path).ok().and_then(|b| decode(&b)),
            DocumentKey::Untitled(_) => None,
        }
    }

    /// Deletes journal `id` and its backup (the user chose Discard, or the
    /// recovered text was taken over into a new journal).
    ///
    /// Fails with [`JournalError::Busy`] while the journal is in use.
    pub fn discard(&self, id: &JournalId) -> Result<(), JournalError> {
        let _lock = self.lock(id)?;
        remove_if_exists(&self.journal_path(id))?;
        remove_if_exists(&self.backup_path(id))?;
        sync_dir(&self.dir);
        Ok(())
    }

    /// Writes the original bytes of an interrupted in-place write back into
    /// the file (in place, so hard links stay joined), then deletes the
    /// backup. Returns the file's path.
    ///
    /// Fails with [`JournalError::Busy`] while the journal is in use.
    pub fn restore_backup(&self, id: &JournalId) -> Result<PathBuf, JournalError> {
        let _lock = self.lock(id)?;
        let Some((path, original)) = self.read_backup(id)? else {
            return Err(JournalError::NotFound(id.clone()));
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.write_all(&original)?;
        file.set_len(original.len() as u64)?;
        file.sync_all()?;
        remove_if_exists(&self.backup_path(id))?;
        Ok(path)
    }

    /// Removes temporary files left by a crash during an atomic write in
    /// this store (`.<name>.gv-tmp-*`). Call once at start-up, before any
    /// journal is opened. The files of journals in use (by another process)
    /// are kept. Returns how many were removed.
    pub fn remove_stale_temp_files(&self) -> io::Result<usize> {
        let mut removed = 0;
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(target) = name
                .strip_prefix('.')
                .and_then(|rest| rest.split_once(".gv-tmp-"))
                .map(|(target, _)| target)
            else {
                continue;
            };
            let id = JournalId::parse(target.strip_suffix(".backup").unwrap_or(target));
            let _lock = match id {
                Some(id) => match JournalLock::try_acquire(self.lock_path(&id))? {
                    Some(lock) => Some(lock),
                    None => continue,
                },
                None => None,
            };
            remove_if_exists(&entry.path())?;
            removed += 1;
        }
        Ok(removed)
    }

    fn read_backup(&self, id: &JournalId) -> io::Result<Option<(PathBuf, Vec<u8>)>> {
        let bytes = match fs::read(self.backup_path(id)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let parsed = bytes
            .strip_prefix(format::BACKUP_MAGIC.as_slice())
            .and_then(|rest| {
                let (path, used) = format::read_path_from(rest)?;
                Some((path, rest[used..].to_vec()))
            });
        match parsed {
            Some(parsed) => Ok(Some(parsed)),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{id}: damaged backup file"),
            )),
        }
    }
}

/// The journal of one open document. Owned by the document's editor.
#[derive(Debug)]
pub struct DocumentJournal<C: Clock = SystemClock> {
    dir: PathBuf,
    id: JournalId,
    /// The lock on `id`, held for the journal's lifetime.
    _lock: JournalLock,
    key: DocumentKey,
    base: TextSnapshot,
    base_format: TextFormat,
    format: TextFormat,
    base_hash: Option<ContentHash>,
    policy: JournalPolicy,
    clock: C,
    /// The open journal file; `None` while nothing was recorded since the
    /// base.
    file: Option<fs::File>,
    /// Bytes of edit records since the last snapshot.
    edit_bytes: u64,
    seq: u64,
    last_sync: Option<Instant>,
    unsynced: bool,
}

impl<C: Clock> DocumentJournal<C> {
    /// The journal's id.
    pub fn id(&self) -> &JournalId {
        &self.id
    }

    /// The document.
    pub fn key(&self) -> &DocumentKey {
        &self.key
    }

    /// The text as last loaded or saved: the base of a 3-way merge.
    pub fn base(&self) -> &TextSnapshot {
        &self.base
    }

    /// The hash of the base's bytes on disk.
    pub fn base_hash(&self) -> Option<ContentHash> {
        self.base_hash
    }

    /// Whether a journal file exists (something was recorded since the
    /// base).
    pub fn is_active(&self) -> bool {
        self.file.is_some()
    }

    fn path(&self) -> PathBuf {
        self.dir.join(self.id.as_str())
    }

    fn backup_path(&self) -> PathBuf {
        self.dir.join(format!("{}.backup", self.id))
    }

    /// Records an applied transaction; `after` is the text it produced.
    /// Starts the journal on the first record and compacts it when it has
    /// grown.
    pub fn record(&mut self, transaction: &Transaction, after: &TextSnapshot) -> io::Result<()> {
        self.record_all([(transaction, after)])
    }

    /// Records several applied transactions in order, each with the text
    /// it produced, as [`DocumentJournal::record`] would one by one, but
    /// with their records handed to the OS in one write (a burst of edits
    /// that queued up while the journal was busy). A transaction too large
    /// for one record, and compaction, are handled as in
    /// [`DocumentJournal::record`]. On an error, the records before the
    /// failing write may or may not be in the journal.
    pub fn record_all<'a, I>(&mut self, edits: I) -> io::Result<()>
    where
        I: IntoIterator<Item = (&'a Transaction, &'a TextSnapshot)>,
    {
        let mut frames = Vec::new();
        let mut last: Option<&TextSnapshot> = None;
        for (transaction, after) in edits {
            if transaction.is_empty() {
                continue;
            }
            let inserted: usize = transaction
                .replacements()
                .iter()
                .map(|r| r.text.len())
                .sum();
            if inserted > format::MAX_RUN / 2 {
                // Too large for one record: the snapshot carries the result
                // (and everything before it).
                frames.clear();
                last = None;
                self.compact(after)?;
                continue;
            }
            // Writes the prologue when the journal does not exist yet;
            // `frames` is empty then, since every frame follows a start.
            self.ensure_started()?;
            self.seq += 1;
            format::edit(
                &mut frames,
                self.seq,
                transaction
                    .replacements()
                    .iter()
                    .map(|r| (r.range.start as u64, r.range.end as u64, r.text.as_str())),
            );
            last = Some(after);
            if frames.len() >= RECORD_BATCH_BYTES {
                self.append_edits(&frames, after)?;
                frames.clear();
                last = None;
            }
        }
        match last {
            Some(after) if !frames.is_empty() => self.append_edits(&frames, after),
            _ => Ok(()),
        }
    }

    /// Appends edit records with one write, then compacts at `after` (the
    /// text after the last of them) if the records have outgrown the text.
    fn append_edits(&mut self, frames: &[u8], after: &TextSnapshot) -> io::Result<()> {
        self.append(frames)?;
        self.edit_bytes += frames.len() as u64;
        let limit = self.policy.compact_min_bytes.max(2 * after.len() as u64);
        if self.edit_bytes > limit {
            self.compact(after)?;
        }
        Ok(())
    }

    /// Records the whole current text, for example after restoring a
    /// recovered text into a new journal, or after edits made while a save
    /// was running.
    pub fn record_snapshot(&mut self, current: &TextSnapshot) -> io::Result<()> {
        self.compact(current)
    }

    /// Records a change of the on-disk format ("Save with encoding…").
    pub fn record_format(&mut self, format: TextFormat) -> io::Result<()> {
        if format == self.format {
            return Ok(());
        }
        self.format = format;
        if self.file.is_none() {
            // The new journal's prologue records the format.
            return self.ensure_started();
        }
        let mut frame = Vec::new();
        format::format(&mut frame, &format);
        self.append(&frame)
    }

    /// Syncs recorded data if the sync interval has passed since the last
    /// sync. Returns whether it synced.
    pub fn sync_if_due(&mut self) -> io::Result<bool> {
        if self.unsynced
            && self
                .next_sync_deadline()
                .is_some_and(|d| self.clock.now() >= d)
        {
            self.sync()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Syncs recorded data now (on focus loss, before quitting).
    pub fn sync(&mut self) -> io::Result<()> {
        if self.unsynced
            && let Some(file) = &self.file
        {
            file.sync_data()?;
        }
        self.unsynced = false;
        self.last_sync = Some(self.clock.now());
        Ok(())
    }

    /// When [`DocumentJournal::sync_if_due`] should run next, if data is
    /// waiting.
    pub fn next_sync_deadline(&self) -> Option<Instant> {
        self.unsynced.then(|| {
            self.last_sync
                .map_or_else(|| self.clock.now(), |t| t + self.policy.sync_interval)
        })
    }

    /// The document was saved: `base` (in `format`, with on-disk hash
    /// `base_hash`) is the new base and the journal is deleted. If edits
    /// were made after the saved text, record them again with
    /// [`DocumentJournal::record_snapshot`].
    pub fn saved(
        &mut self,
        base: TextSnapshot,
        format: TextFormat,
        base_hash: ContentHash,
    ) -> io::Result<()> {
        self.close_and_delete()?;
        self.base = base;
        self.base_format = format;
        self.format = format;
        self.base_hash = Some(base_hash);
        Ok(())
    }

    /// Like [`DocumentJournal::saved`], when the document now lives under
    /// another key (an untitled document's first save, "Save As"). Takes
    /// the new key's lock and releases the old one; fails with
    /// [`JournalError::Busy`] while another handle has the new key open.
    pub fn saved_as(
        &mut self,
        key: DocumentKey,
        base: TextSnapshot,
        format: TextFormat,
        base_hash: ContentHash,
    ) -> Result<(), JournalError> {
        let id = JournalId::of(&key);
        let lock = if id == self.id {
            None
        } else {
            let lock = lock_journal(&self.dir, &id)?;
            if exists(&self.dir.join(id.as_str()))?
                || exists(&self.dir.join(format!("{id}.backup")))?
            {
                return Err(JournalError::PendingRecovery(id));
            }
            Some(lock)
        };
        self.saved(base, format, base_hash)?;
        if let Some(lock) = lock {
            // The old key's lock is released here.
            self._lock = lock;
        }
        self.id = id;
        self.key = key;
        Ok(())
    }

    /// Deletes the journal without saving (the user closed the document and
    /// chose not to save).
    pub fn discard(mut self) -> io::Result<()> {
        self.close_and_delete()
    }

    fn close_and_delete(&mut self) -> io::Result<()> {
        let existed = self.file.take().is_some();
        self.edit_bytes = 0;
        self.unsynced = false;
        remove_if_exists(&self.path())?;
        if existed {
            // Otherwise a power cut could bring back a journal of edits that
            // are already saved.
            sync_dir(&self.dir);
        }
        Ok(())
    }

    /// Writes header and base (atomically) if the journal does not exist
    /// yet.
    fn ensure_started(&mut self) -> io::Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        let mut bytes = self.prologue();
        if self.format != self.base_format {
            format::format(&mut bytes, &self.format);
        }
        self.replace_file(&bytes)
    }

    /// Magic, header and (if small enough) the base.
    fn prologue(&self) -> Vec<u8> {
        let mut bytes = format::MAGIC.to_vec();
        format::header(
            &mut bytes,
            &Header {
                key: self.key.clone(),
                format: self.base_format,
                base_hash: self.base_hash,
                base_len: self.base.len() as u64,
            },
        );
        if self.base.len() <= self.policy.inline_base_limit.min(format::MAX_RUN) {
            format::base(&mut bytes, &self.base.to_string());
        }
        bytes
    }

    /// Rewrites the journal as prologue plus one snapshot of `current`.
    fn compact(&mut self, current: &TextSnapshot) -> io::Result<()> {
        if current.len() > format::MAX_RUN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the text is too large to journal",
            ));
        }
        let mut bytes = self.prologue();
        if self.format != self.base_format {
            format::format(&mut bytes, &self.format);
        }
        self.seq += 1;
        format::snapshot(&mut bytes, self.seq, current.len(), current.all_chunks());
        self.replace_file(&bytes)?;
        self.edit_bytes = 0;
        Ok(())
    }

    /// Atomically replaces the journal file with `bytes` and reopens it for
    /// appending.
    fn replace_file(&mut self, bytes: &[u8]) -> io::Result<()> {
        let path = self.path();
        self.file = None;
        gv_settings::atomic_write(&path, bytes)?;
        self.file = Some(fs::OpenOptions::new().append(true).open(&path)?);
        self.unsynced = false;
        self.last_sync = Some(self.clock.now());
        Ok(())
    }

    /// Appends one record with a single write, then syncs if due.
    fn append(&mut self, frame: &[u8]) -> io::Result<()> {
        let Some(file) = &mut self.file else {
            return Err(io::Error::other("the journal is not open"));
        };
        file.write_all(frame)?;
        self.unsynced = true;
        let due = self.policy.sync_interval.is_zero()
            || self
                .last_sync
                .is_none_or(|t| self.clock.now().duration_since(t) >= self.policy.sync_interval);
        if due {
            self.sync()?;
        }
        Ok(())
    }
}

impl<C: Clock> Backup for DocumentJournal<C> {
    fn store(&mut self, target: &Path, original: &[u8]) -> io::Result<()> {
        let mut bytes = format::BACKUP_MAGIC.to_vec();
        format::put_path(&mut bytes, target);
        bytes.extend_from_slice(original);
        gv_settings::atomic_write(&self.backup_path(), &bytes)
    }

    fn clear(&mut self, _target: &Path) -> io::Result<()> {
        remove_if_exists(&self.backup_path())
    }
}

fn lock_path(dir: &Path, id: &JournalId) -> PathBuf {
    dir.join(format!(".{id}.lock"))
}

/// Takes the lock of journal `id` in `dir`, or fails with
/// [`JournalError::Busy`].
fn lock_journal(dir: &Path, id: &JournalId) -> Result<JournalLock, JournalError> {
    JournalLock::try_acquire(lock_path(dir, id))?.ok_or_else(|| JournalError::Busy {
        path: dir.join(id.as_str()),
    })
}

fn compare_disk(path: &Path, base_hash: Option<ContentHash>) -> DiskComparison {
    match fs::read(path) {
        Ok(bytes) if Some(ContentHash::of(&bytes)) == base_hash => DiskComparison::MatchesBase,
        Ok(_) => DiskComparison::Changed,
        Err(e) if e.kind() == io::ErrorKind::NotFound => DiskComparison::Missing,
        Err(_) => DiskComparison::Unreadable,
    }
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.is_dir() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let mode = fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

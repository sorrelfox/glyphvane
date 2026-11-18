// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Local version history (FR-FILE-10): a snapshot of a note each time it is
//! saved with new content, kept in the app's data dir. Nothing is written
//! to the vault.
//!
//! # Layout
//!
//! A [`HistoryStore`] is one directory, by default
//! `<data dir>/history/<vault-id>/` ([`HistoryStore::for_vault`]):
//!
//! - `objects/<2 hex>/<64 hex>`: one file per distinct content, named by
//!   the SHA-256 of the text, so equal versions (of any note) are stored
//!   once. A 4-byte magic, a codec byte (0 stored as is, 1 deflate) and the
//!   payload. Deflate (`miniz_oxide`, pure Rust) rather than zstd keeps C
//!   code out of the build; the codec byte leaves room for another one.
//! - `logs/<32 hex>.log`: the versions of one document, keyed by the
//!   SHA-256 of its path: a header line, then one line per version,
//!   `<unix ms> <content hash> <size>`, oldest first.
//!
//! Files are created with mode 0600 and folders with 0700 on Unix: they
//! hold note text.
//!
//! # Crash safety
//!
//! Every file is written to a temporary file in its folder, synced, and
//! renamed over the old one; the folder is synced after. A snapshot writes
//! its object first and the log second, so a crash leaves either the old
//! log (and maybe an object nothing refers to yet, which
//! [`HistoryStore::collect_garbage`] removes) or the new one, never a log
//! that refers to a missing object. Objects are deleted only after the
//! logs that referred to them were rewritten. A log line that cannot be
//! read is skipped, and every object is checked against its hash when
//! read.
//!
//! # Retention
//!
//! [`HistoryPolicy`]: the last 20 versions of each note, then the newest
//! version of each hour for 2 days and of each day (UTC) for 30 days;
//! older ones go when the note is next saved. Above a size cap for the
//! whole store, the oldest versions of any note go first, but the newest
//! version of each note stays. Texts above a size limit are not kept.
//!
//! # Several processes
//!
//! One store serialises its own writers. Two processes writing the same
//! note's log at the same moment can lose one of the two new versions;
//! neither log nor objects are damaged.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gv_settings::{InvalidVaultId, Locations};

use super::hash::ContentHash;
use super::write::sync_dir;

/// The name of the folder below the data dir that holds the stores.
pub const HISTORY_FOLDER: &str = "history";

const OBJECTS: &str = "objects";
const LOGS: &str = "logs";
const LOG_HEADER: &str = "glyphvane-history 1";
const MAGIC: &[u8; 4] = b"GVH1";
const CODEC_STORED: u8 = 0;
const CODEC_DEFLATE: u8 = 1;
const TEMP_PREFIX: &str = ".tmp-";

/// Objects and temporary files younger than this are never collected:
/// another process may be between writing one and its log.
const GARBAGE_GRACE: Duration = Duration::from_secs(60 * 60);

/// How long versions are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryPolicy {
    /// The newest versions of each note that are always kept.
    pub keep_last: usize,
    /// For this long, the newest version of each hour is kept.
    pub hourly_for: Duration,
    /// For this long, the newest version of each day is kept.
    pub daily_for: Duration,
    /// The most bytes the store's objects may take on disk; the oldest
    /// versions go first above it.
    pub max_bytes: u64,
    /// Texts larger than this are not kept.
    pub max_file_bytes: u64,
}

impl Default for HistoryPolicy {
    fn default() -> Self {
        Self {
            keep_last: 20,
            hourly_for: Duration::from_secs(2 * 24 * 60 * 60),
            daily_for: Duration::from_secs(30 * 24 * 60 * 60),
            max_bytes: 256 * 1024 * 1024,
            max_file_bytes: 8 * 1024 * 1024,
        }
    }
}

/// One kept version of a document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    /// When it was saved.
    pub time: SystemTime,
    /// The hash of its text.
    pub hash: ContentHash,
    /// The text's length in bytes.
    pub size: u64,
}

/// What [`HistoryStore::record`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// A new version was kept.
    Added(Version),
    /// The newest version already has this text.
    Unchanged,
    /// The text is larger than [`HistoryPolicy::max_file_bytes`].
    TooLarge,
}

/// Why a version could not be read.
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("this version is no longer stored")]
    Missing,
    #[error("this version is damaged: {0}")]
    Damaged(&'static str),
    #[error(transparent)]
    InvalidVaultId(#[from] InvalidVaultId),
}

/// The version history of the documents of one vault.
#[derive(Debug)]
pub struct HistoryStore {
    dir: PathBuf,
    policy: HistoryPolicy,
    /// Serialises the writers of this store.
    lock: Mutex<()>,
    /// The objects' size on disk, once counted.
    stored: Mutex<Option<u64>>,
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl HistoryStore {
    /// A store in `dir`, created (mode 0700 on Unix) if missing.
    pub fn open(dir: impl Into<PathBuf>, policy: HistoryPolicy) -> io::Result<Self> {
        let dir = dir.into();
        create_private_dir(&dir.join(OBJECTS))?;
        create_private_dir(&dir.join(LOGS))?;
        Ok(Self {
            dir,
            policy,
            lock: Mutex::new(()),
            stored: Mutex::new(None),
        })
    }

    /// The store of vault `vault_id` in the app's data dir:
    /// `<data>/history/<vault-id>/`.
    pub fn for_vault(
        locations: &Locations,
        vault_id: &str,
        policy: HistoryPolicy,
    ) -> Result<Self, HistoryError> {
        locations.vault_data_dir(vault_id)?;
        Ok(Self::open(
            locations.data_dir().join(HISTORY_FOLDER).join(vault_id),
            policy,
        )?)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn policy(&self) -> HistoryPolicy {
        self.policy
    }

    /// Keeps `text` as the version of `path` saved at `now`, unless the
    /// newest version already has it, then applies the retention policy to
    /// the document and the store's size cap.
    pub fn record(&self, path: &Path, text: &[u8], now: SystemTime) -> io::Result<Recorded> {
        if text.len() as u64 > self.policy.max_file_bytes {
            return Ok(Recorded::TooLarge);
        }
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let hash = ContentHash::of(text);
        let log = self.log_path(path);
        let mut versions = read_log(&log)?;
        if versions.last().is_some_and(|v| v.hash == hash) {
            return Ok(Recorded::Unchanged);
        }
        let added = self.write_object(&hash, text)?;
        let version = Version {
            time: now,
            hash,
            size: text.len() as u64,
        };
        versions.push(version.clone());
        // A clock set back must not reorder the log.
        versions.sort_by_key(|v| v.time);
        let keep = retained(&versions, now, &self.policy);
        let (kept, dropped): (Vec<_>, Vec<_>) =
            versions.into_iter().zip(keep).partition(|(_, keep)| *keep);
        let kept: Vec<Version> = kept.into_iter().map(|(v, _)| v).collect();
        write_log(&log, &kept)?;
        self.add_stored(added);
        if !dropped.is_empty() {
            let candidates: HashSet<ContentHash> = dropped.iter().map(|(v, _)| v.hash).collect();
            self.delete_unreferenced(candidates)?;
        }
        self.enforce_cap()?;
        Ok(Recorded::Added(version))
    }

    /// The versions of `path`, newest first.
    pub fn versions(&self, path: &Path) -> io::Result<Vec<Version>> {
        let mut versions = read_log(&self.log_path(path))?;
        versions.reverse();
        Ok(versions)
    }

    /// The text of `version`, checked against its hash.
    pub fn read(&self, version: &Version) -> Result<Vec<u8>, HistoryError> {
        let bytes = match fs::read(self.object_path(&version.hash)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(HistoryError::Missing),
            Err(e) => return Err(e.into()),
        };
        let (head, payload) = bytes
            .split_at_checked(MAGIC.len() + 1)
            .ok_or(HistoryError::Damaged("too short"))?;
        if &head[..MAGIC.len()] != MAGIC {
            return Err(HistoryError::Damaged("unknown format"));
        }
        let limit = usize::try_from(self.policy.max_file_bytes.max(version.size))
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        let text = match head[MAGIC.len()] {
            CODEC_STORED => payload.to_vec(),
            CODEC_DEFLATE => miniz_oxide::inflate::decompress_to_vec_with_limit(payload, limit)
                .map_err(|_| HistoryError::Damaged("cannot be decompressed"))?,
            _ => return Err(HistoryError::Damaged("unknown codec")),
        };
        if ContentHash::of(&text) != version.hash {
            return Err(HistoryError::Damaged("its content does not match"));
        }
        Ok(text)
    }

    /// Moves the history of `from` to `to` (a rename or move). Versions
    /// `to` already had are kept beside them, in time order.
    pub fn moved(&self, from: &Path, to: &Path) -> io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (old, new) = (self.log_path(from), self.log_path(to));
        if old == new {
            return Ok(());
        }
        let mut versions = read_log(&old)?;
        if versions.is_empty() {
            return Ok(());
        }
        versions.extend(read_log(&new)?);
        versions.sort_by_key(|v| v.time);
        versions.dedup_by(|a, b| a.time == b.time && a.hash == b.hash);
        write_log(&new, &versions)?;
        remove_file(&old)?;
        sync_dir(&self.dir.join(LOGS));
        Ok(())
    }

    /// Forgets every version of `path`.
    pub fn forget(&self, path: &Path) -> io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let log = self.log_path(path);
        let versions = read_log(&log)?;
        remove_file(&log)?;
        sync_dir(&self.dir.join(LOGS));
        self.delete_unreferenced(versions.into_iter().map(|v| v.hash).collect())
    }

    /// The bytes the store's objects take on disk.
    pub fn stored_bytes(&self) -> io::Result<u64> {
        let mut stored = self.stored.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(total) = *stored {
            return Ok(total);
        }
        let total = self.objects()?.iter().map(|o| o.size).sum();
        *stored = Some(total);
        Ok(total)
    }

    /// Removes objects no log refers to and temporary files left by a
    /// crash, both only when older than an hour (`now` is the current
    /// time). Returns how many files were removed.
    pub fn collect_garbage(&self, now: SystemTime) -> io::Result<usize> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let referenced = self.referenced()?;
        let old = |modified: Option<SystemTime>| {
            modified.is_some_and(|m| now.duration_since(m).unwrap_or_default() >= GARBAGE_GRACE)
        };
        let mut removed = 0;
        for object in self.objects()? {
            if object.hash.is_none_or(|h| !referenced.contains(&h)) && old(object.modified) {
                remove_file(&object.path)?;
                self.add_stored(-i64::try_from(object.size).unwrap_or(i64::MAX));
                removed += 1;
            }
        }
        for dir in self.folders()? {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let temp = entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX);
                if temp && old(entry.metadata().ok().and_then(|m| m.modified().ok())) {
                    remove_file(&entry.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    // ----- inside ---------------------------------------------------------------

    fn log_path(&self, path: &Path) -> PathBuf {
        // Rebuilt from its components, so `a/./b`, `a//b` and (on Windows)
        // `C:\a/b` share the log of `a/b` and `C:\a\b`.
        let path: PathBuf = path.components().collect();
        let key = ContentHash::of(path.as_os_str().as_encoded_bytes()).to_hex();
        self.dir.join(LOGS).join(format!("{}.log", &key[..32]))
    }

    fn object_path(&self, hash: &ContentHash) -> PathBuf {
        let hex = hash.to_hex();
        self.dir.join(OBJECTS).join(&hex[..2]).join(hex)
    }

    /// Writes the object of `text` unless it exists. Returns the bytes
    /// added on disk.
    fn write_object(&self, hash: &ContentHash, text: &[u8]) -> io::Result<i64> {
        let path = self.object_path(hash);
        if path.is_file() {
            return Ok(0);
        }
        let compressed = miniz_oxide::deflate::compress_to_vec(text, 6);
        let mut bytes = Vec::with_capacity(MAGIC.len() + 1 + compressed.len().min(text.len()));
        bytes.extend_from_slice(MAGIC);
        if compressed.len() < text.len() {
            bytes.push(CODEC_DEFLATE);
            bytes.extend_from_slice(&compressed);
        } else {
            bytes.push(CODEC_STORED);
            bytes.extend_from_slice(text);
        }
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        write_atomic(&path, &bytes)?;
        Ok(i64::try_from(bytes.len()).unwrap_or(i64::MAX))
    }

    fn add_stored(&self, delta: i64) {
        let mut stored = self.stored.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(total) = stored.as_mut() {
            *total = total.saturating_add_signed(delta);
        }
    }

    /// Every log file.
    fn logs(&self) -> io::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.dir.join(LOGS))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "log") {
                out.push(path);
            }
        }
        out.sort();
        Ok(out)
    }

    /// The object folders.
    fn folders(&self) -> io::Result<Vec<PathBuf>> {
        let mut out = vec![self.dir.join(LOGS)];
        for entry in fs::read_dir(self.dir.join(OBJECTS))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.path());
            }
        }
        Ok(out)
    }

    /// Every object file.
    fn objects(&self) -> io::Result<Vec<ObjectFile>> {
        let mut out = Vec::new();
        for dir in self.folders()?.into_iter().skip(1) {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(TEMP_PREFIX) {
                    continue;
                }
                let meta = entry.metadata()?;
                out.push(ObjectFile {
                    path: entry.path(),
                    hash: parse_hash(&name),
                    size: meta.len(),
                    modified: meta.modified().ok(),
                });
            }
        }
        Ok(out)
    }

    /// The hashes every log refers to.
    fn referenced(&self) -> io::Result<HashSet<ContentHash>> {
        let mut out = HashSet::new();
        for log in self.logs()? {
            out.extend(read_log(&log)?.into_iter().map(|v| v.hash));
        }
        Ok(out)
    }

    /// Deletes the objects of `candidates` that no log refers to any more.
    fn delete_unreferenced(&self, mut candidates: HashSet<ContentHash>) -> io::Result<()> {
        if candidates.is_empty() {
            return Ok(());
        }
        let referenced = self.referenced()?;
        candidates.retain(|h| !referenced.contains(h));
        for hash in candidates {
            let path = self.object_path(&hash);
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            remove_file(&path)?;
            self.add_stored(-i64::try_from(size).unwrap_or(i64::MAX));
        }
        Ok(())
    }

    /// Drops the oldest versions of any document (never a document's
    /// newest) until the objects fit [`HistoryPolicy::max_bytes`].
    fn enforce_cap(&self) -> io::Result<()> {
        let mut total = self.stored_bytes()?;
        if total <= self.policy.max_bytes {
            return Ok(());
        }
        let logs: Vec<(PathBuf, Vec<Version>)> = self
            .logs()?
            .into_iter()
            .map(|log| read_log(&log).map(|v| (log, v)))
            .collect::<io::Result<_>>()?;
        let mut refs: HashMap<ContentHash, usize> = HashMap::new();
        for (_, versions) in &logs {
            for v in versions {
                *refs.entry(v.hash).or_default() += 1;
            }
        }
        // Every version but each log's newest, oldest first.
        let mut candidates: Vec<(SystemTime, usize, usize)> = logs
            .iter()
            .enumerate()
            .flat_map(|(l, (_, versions))| {
                let last = versions.len().saturating_sub(1);
                versions[..last]
                    .iter()
                    .enumerate()
                    .map(move |(i, v)| (v.time, l, i))
            })
            .collect();
        candidates.sort();
        let mut drop: HashSet<(usize, usize)> = HashSet::new();
        let mut freed: Vec<ContentHash> = Vec::new();
        for (_, l, i) in candidates {
            if total <= self.policy.max_bytes {
                break;
            }
            let hash = logs[l].1[i].hash;
            drop.insert((l, i));
            let count = refs.entry(hash).or_default();
            *count = count.saturating_sub(1);
            if *count == 0 {
                let size = fs::metadata(self.object_path(&hash))
                    .map(|m| m.len())
                    .unwrap_or(0);
                total = total.saturating_sub(size);
                freed.push(hash);
            }
        }
        for (l, (log, versions)) in logs.iter().enumerate() {
            if !(0..versions.len()).any(|i| drop.contains(&(l, i))) {
                continue;
            }
            let kept: Vec<Version> = versions
                .iter()
                .enumerate()
                .filter(|(i, _)| !drop.contains(&(l, *i)))
                .map(|(_, v)| v.clone())
                .collect();
            write_log(log, &kept)?;
        }
        for hash in freed {
            let path = self.object_path(&hash);
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            remove_file(&path)?;
            self.add_stored(-i64::try_from(size).unwrap_or(i64::MAX));
        }
        Ok(())
    }
}

/// An object file: path, hash (when the name is one), size and
/// modification time.
struct ObjectFile {
    path: PathBuf,
    hash: Option<ContentHash>,
    size: u64,
    modified: Option<SystemTime>,
}

/// Which of `versions` (oldest first) the policy keeps at `now`.
fn retained(versions: &[Version], now: SystemTime, policy: &HistoryPolicy) -> Vec<bool> {
    let mut keep = vec![false; versions.len()];
    let mut hours = HashSet::new();
    let mut days = HashSet::new();
    for (rank, (i, v)) in versions.iter().enumerate().rev().enumerate() {
        let age = now.duration_since(v.time).unwrap_or_default();
        let secs = millis(v.time) / 1000;
        // Newest first: the first version seen in a bucket is its newest.
        let newest_of_hour = hours.insert(secs / 3600);
        let newest_of_day = days.insert(secs / 86_400);
        keep[i] = rank < policy.keep_last
            || (age <= policy.hourly_for && newest_of_hour)
            || (age <= policy.daily_for && newest_of_day);
    }
    keep
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn parse_hash(hex: &str) -> Option<ContentHash> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(ContentHash::from_bytes(bytes))
}

/// The versions in a log, oldest first. A missing log has none; lines that
/// cannot be read are skipped.
fn read_log(path: &Path) -> io::Result<Vec<Version>> {
    let text = match fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut lines = text.lines();
    if lines.next() != Some(LOG_HEADER) {
        return Ok(Vec::new());
    }
    let mut out: Vec<Version> = lines
        .filter_map(|line| {
            let mut parts = line.split(' ');
            let ms: u64 = parts.next()?.parse().ok()?;
            let hash = parse_hash(parts.next()?)?;
            let size = parts.next()?.parse().ok()?;
            parts.next().is_none().then(|| Version {
                time: UNIX_EPOCH + Duration::from_millis(ms),
                hash,
                size,
            })
        })
        .collect();
    out.sort_by_key(|v| v.time);
    Ok(out)
}

/// Replaces a log; an empty one is removed.
fn write_log(path: &Path, versions: &[Version]) -> io::Result<()> {
    if versions.is_empty() {
        remove_file(path)?;
        if let Some(dir) = path.parent() {
            sync_dir(dir);
        }
        return Ok(());
    }
    let mut text = String::from(LOG_HEADER);
    text.push('\n');
    for v in versions {
        text.push_str(&format!(
            "{} {} {}\n",
            millis(v.time),
            v.hash.to_hex(),
            v.size
        ));
    }
    write_atomic(path, text.as_bytes())
}

/// Writes `bytes` to a temporary file beside `path`, syncs it and renames
/// it over `path`, then syncs the folder.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent folder"))?;
    let temp = dir.join(format!(
        "{TEMP_PREFIX}{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = private_file(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    sync_dir(dir);
    Ok(())
}

fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        if !dir.is_dir() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);
    const DAY: Duration = Duration::from_secs(86_400);

    fn t0() -> SystemTime {
        // 2026-01-01T00:00:00Z
        UNIX_EPOCH + Duration::from_secs(1_767_225_600)
    }

    fn store(policy: HistoryPolicy) -> (tempfile::TempDir, HistoryStore) {
        let dir = tempfile::tempdir().expect("temporary folder");
        let store = HistoryStore::open(dir.path().join("h"), policy).expect("store opens");
        (dir, store)
    }

    fn texts(store: &HistoryStore, path: &Path) -> Vec<String> {
        store
            .versions(path)
            .expect("versions")
            .iter()
            .map(|v| String::from_utf8(store.read(v).expect("read")).expect("utf-8"))
            .collect()
    }

    #[test]
    fn records_reads_and_dedupes() {
        let (_dir, store) = store(HistoryPolicy::default());
        let note = Path::new("/vault/a.md");
        let long = "Some text that repeats. ".repeat(200);
        assert!(matches!(
            store.record(note, long.as_bytes(), t0()),
            Ok(Recorded::Added(_))
        ));
        // The same text again: nothing new.
        assert_eq!(
            store.record(note, long.as_bytes(), t0() + HOUR).ok(),
            Some(Recorded::Unchanged)
        );
        store
            .record(note, b"second", t0() + 2 * HOUR)
            .expect("record");
        // Back to the first text: a new version, the same object.
        store
            .record(note, long.as_bytes(), t0() + 3 * HOUR)
            .expect("record");
        assert_eq!(
            texts(&store, note),
            vec![long.clone(), "second".to_owned(), long.clone()]
        );
        let versions = store.versions(note).expect("versions");
        assert_eq!(versions[0].time, t0() + 3 * HOUR);
        assert_eq!(versions[0].size, long.len() as u64);
        // Two objects: compressed text and "second" stored as is.
        let stored = store.stored_bytes().expect("size");
        assert!(stored < long.len() as u64, "{stored}");
        // Another note with the same text shares the object.
        store
            .record(Path::new("/vault/b.md"), long.as_bytes(), t0())
            .expect("record");
        assert_eq!(store.stored_bytes().expect("size"), stored);
        assert!(texts(&store, Path::new("/vault/c.md")).is_empty());
    }

    #[test]
    fn retention_keeps_last_hourly_and_daily() {
        let policy = HistoryPolicy {
            keep_last: 3,
            ..HistoryPolicy::default()
        };
        let (_dir, store) = store(policy);
        let note = Path::new("/vault/a.md");
        // Two saves an hour for 35 days.
        let quarter = Duration::from_secs(30 * 60);
        let saves = 35 * 24 * 2;
        let mut now = t0();
        for i in 0..saves {
            now = t0() + quarter * i;
            store
                .record(note, format!("text {i}").as_bytes(), now)
                .expect("record");
        }
        let versions = store.versions(note).expect("versions");
        // 3 newest; within 2 days one per hour (the newest hour's version is
        // among the 3), within 30 days one per day.
        let recent = versions
            .iter()
            .filter(|v| now.duration_since(v.time).unwrap_or_default() <= 2 * DAY)
            .count();
        assert!((48..=52).contains(&recent), "{recent}");
        assert!(
            versions
                .iter()
                .all(|v| now.duration_since(v.time).unwrap_or_default() <= 30 * DAY)
        );
        assert!(versions.len() <= 3 + 49 + 31, "{}", versions.len());
        assert!(versions.len() >= 48 + 27, "{}", versions.len());
        // Dropped versions' objects are gone: one per kept version.
        let objects = store.objects().expect("objects").len();
        assert_eq!(objects, versions.len());
        assert_eq!(store.collect_garbage(now + DAY).expect("gc"), 0);
    }

    #[test]
    fn the_size_cap_drops_the_oldest_but_keeps_each_newest() {
        let policy = HistoryPolicy {
            max_bytes: 4_000,
            ..HistoryPolicy::default()
        };
        let (_dir, store) = store(policy);
        // Text that does not compress: 1,000 bytes stored each.
        let text = |seed: u64| -> Vec<u8> {
            let mut x = seed.wrapping_mul(6_364_136_223_846_793_005) | 1;
            (0..1_000)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x & 0xff) as u8
                })
                .collect()
        };
        let (a, b) = (Path::new("/v/a.md"), Path::new("/v/b.md"));
        for i in 0..6 {
            store
                .record(a, &text(i), t0() + HOUR * u32::try_from(i).unwrap_or(0))
                .expect("record");
        }
        store.record(b, &text(100), t0()).expect("record");
        assert!(store.stored_bytes().expect("size") <= 4_000);
        let kept_a = store.versions(a).expect("versions");
        assert_eq!(kept_a[0].time, t0() + HOUR * 5, "the newest stays");
        assert!(kept_a.len() < 6);
        assert_eq!(store.versions(b).expect("versions").len(), 1);
        // The counted size matches the disk.
        let on_disk: u64 = store
            .objects()
            .expect("objects")
            .iter()
            .map(|o| o.size)
            .sum();
        assert_eq!(on_disk, store.stored_bytes().expect("size"));
    }

    #[test]
    fn too_large_texts_are_not_kept() {
        let policy = HistoryPolicy {
            max_file_bytes: 10,
            ..HistoryPolicy::default()
        };
        let (_dir, store) = store(policy);
        let note = Path::new("/v/a.md");
        assert_eq!(
            store.record(note, b"0123456789ab", t0()).ok(),
            Some(Recorded::TooLarge)
        );
        assert!(store.versions(note).expect("versions").is_empty());
    }

    #[test]
    fn crash_leftovers_are_harmless() {
        let (_dir, store) = store(HistoryPolicy::default());
        let note = Path::new("/v/a.md");
        store.record(note, b"one", t0()).expect("record");
        store.record(note, b"two", t0() + HOUR).expect("record");
        let log = store.log_path(note);

        // A crash between the object and the log: an object nothing refers
        // to, and a half-written temporary file.
        store
            .write_object(&ContentHash::of(b"orphan"), b"orphan")
            .expect("object");
        fs::write(store.dir.join(LOGS).join(".tmp-1-1"), "glyphvane-hi").expect("temp");
        assert_eq!(texts(&store, note), vec!["two", "one"]);
        // Young leftovers stay (another process may be writing them).
        assert_eq!(store.collect_garbage(SystemTime::now()).expect("gc"), 0);
        let later = SystemTime::now() + 2 * GARBAGE_GRACE;
        assert_eq!(store.collect_garbage(later).expect("gc"), 2);
        assert_eq!(texts(&store, note), vec!["two", "one"]);

        // A torn or garbled line is skipped; the others still read.
        let mut text = fs::read_to_string(&log).expect("log");
        text.push_str("1767229200000 abc");
        fs::write(&log, &text).expect("log");
        assert_eq!(texts(&store, note), vec!["two", "one"]);
        store
            .record(note, b"three", t0() + 2 * HOUR)
            .expect("record");
        assert_eq!(texts(&store, note), vec!["three", "two", "one"]);

        // A damaged object is reported, not returned.
        let newest = store.versions(note).expect("versions")[0].clone();
        let object = store.object_path(&newest.hash);
        let mut bytes = fs::read(&object).expect("object");
        if let Some(last) = bytes.last_mut() {
            *last ^= 0xff;
        }
        fs::write(&object, bytes).expect("object");
        assert!(matches!(store.read(&newest), Err(HistoryError::Damaged(_))));
        fs::remove_file(&object).expect("removed");
        assert!(matches!(store.read(&newest), Err(HistoryError::Missing)));
    }

    #[test]
    fn spellings_of_one_path_share_a_history() {
        let (_dir, store) = store(HistoryPolicy::default());
        store
            .record(Path::new("/v/dir/./a.md"), b"one", t0())
            .expect("record");
        assert_eq!(texts(&store, Path::new("/v/dir/a.md")), vec!["one"]);
        assert_eq!(texts(&store, Path::new("/v//dir/a.md")), vec!["one"]);
        if cfg!(windows) {
            // Either separator, as Windows itself accepts.
            assert_eq!(texts(&store, Path::new("/v\\dir/a.md")), vec!["one"]);
        }
    }

    #[test]
    fn a_new_store_counts_the_objects_already_there() {
        let dir = tempfile::tempdir().expect("temporary folder");
        let first = HistoryStore::open(dir.path(), HistoryPolicy::default()).expect("store");
        first
            .record(Path::new("/v/a.md"), b"text", t0())
            .expect("record");
        let size = first.stored_bytes().expect("size");
        drop(first);
        let again = HistoryStore::open(dir.path(), HistoryPolicy::default()).expect("store");
        assert_eq!(again.stored_bytes().expect("size"), size);
        assert_eq!(texts(&again, Path::new("/v/a.md")), vec!["text"]);
    }

    #[test]
    fn moves_and_forgets() {
        let (_dir, store) = store(HistoryPolicy::default());
        let (a, b) = (Path::new("/v/a.md"), Path::new("/v/b.md"));
        store.record(a, b"one", t0()).expect("record");
        store.record(b, b"other", t0() + HOUR).expect("record");
        store.record(a, b"two", t0() + 2 * HOUR).expect("record");
        store.moved(a, b).expect("moved");
        assert!(store.versions(a).expect("versions").is_empty());
        assert_eq!(texts(&store, b), vec!["two", "other", "one"]);
        store.forget(b).expect("forget");
        assert!(store.versions(b).expect("versions").is_empty());
        assert_eq!(store.stored_bytes().expect("size"), 0);
    }

    #[test]
    fn locations_place_the_store_in_the_data_dir() {
        let dir = tempfile::tempdir().expect("temporary folder");
        let locations = Locations::with_root(dir.path());
        let store = HistoryStore::for_vault(&locations, "no-vault", HistoryPolicy::default())
            .expect("store");
        assert!(store.dir().starts_with(locations.data_dir()));
        assert!(store.dir().ends_with("history/no-vault"));
        assert!(HistoryStore::for_vault(&locations, "../x", HistoryPolicy::default()).is_err());
    }
}

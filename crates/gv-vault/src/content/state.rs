// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The content index's state and its incremental update.
//!
//! [`State`] is a value: cloning it is cheap (copy-on-write shards, as in
//! the path index), so every update publishes a new immutable snapshot.
//!
//! # Update
//!
//! [`State::apply`] takes a newer path index snapshot and overlay changes:
//!
//! 1. **Content diff.** Every note file in the new path index is compared
//!    with its record by size and modification time; records in doubt
//!    ([`FileStamp::doubt`]) are compared by CRC after reading. New and
//!    changed files are read in parallel and parsed; a file whose bytes
//!    are unchanged (same CRC) keeps its parsed data. Records of files
//!    that left the index are dropped.
//! 2. **Overlay.** An unsaved buffer replaces the disk record of its path
//!    until it is cleared.
//! 3. **Re-resolution.** Every link stores the lookup key it resolves
//!    through: its last path segment, case-folded, up to the first dot.
//!    Links of notes whose records changed are resolved again, and so are
//!    the links whose key matches the name of an entry that was added or
//!    removed (or of its folder, for index files and folder targets, or a
//!    sibling it makes a sync-conflict copy). Everything is resolved again
//!    when the path index finishes building or the root changes.
//!
//! The backlink map (target → sources) and the key map (key → sources)
//! are maintained from the old and new resolutions of each re-resolved
//! note.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::Hash;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::extract::extract;
use super::record::{FileStamp, NoteData, NoteRecord, RecordKind};
use super::{ContentOptions, FileSource};
use crate::index::cow::CowMap;
use crate::index::{EntryKind, FileMeta, PathIndex};
use crate::keys::fold_key;
use crate::link::{DecodedTarget, LinkRef, decode};
use crate::path::VaultPath;
use crate::resolve::{Status, resolve};

/// A modification time closer than this to the moment a file was read is
/// not trusted ([`FileStamp::doubt`]).
pub(crate) const RACY_WINDOW: Duration = Duration::from_secs(2);

/// Where one link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget {
    /// The resolver's status.
    pub status: Status,
    /// The target file or folder, when resolved.
    pub target: Option<VaultPath>,
}

/// One link's resolution, packed: the status and an index into
/// [`Resolved::paths`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Packed {
    pub(crate) status: u8,
    /// The candidate count of [`Status::Ambiguous`].
    pub(crate) count: u32,
    /// Index into [`Resolved::paths`], or [`Packed::NONE`].
    pub(crate) target: u32,
}

impl Packed {
    pub(crate) const NONE: u32 = u32::MAX;

    fn new(status: Status, target: Option<u32>) -> Self {
        let (status, count) = match status {
            Status::Exact => (0, 0),
            Status::CaseFold => (1, 0),
            Status::Loose => (2, 0),
            Status::Ambiguous(n) => (3, u32::try_from(n).unwrap_or(u32::MAX)),
            Status::Pending => (4, 0),
            Status::Unresolved => (5, 0),
            Status::OutsideRoot => (6, 0),
            Status::External => (7, 0),
            Status::Blocked => (8, 0),
        };
        Self {
            status,
            count,
            target: target.unwrap_or(Self::NONE),
        }
    }

    fn status(self) -> Status {
        match self.status {
            0 => Status::Exact,
            1 => Status::CaseFold,
            2 => Status::Loose,
            3 => Status::Ambiguous(self.count as usize),
            4 => Status::Pending,
            5 => Status::Unresolved,
            6 => Status::OutsideRoot,
            7 => Status::External,
            _ => Status::Blocked,
        }
    }
}

/// The resolved links of one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// One per link, in order.
    pub targets: Box<[Packed]>,
    /// The distinct targets, sorted (the note itself included when it
    /// links to itself). The paths are the path index's own.
    pub paths: Box<[VaultPath]>,
    /// The distinct lookup keys its links resolve through (URLs and links
    /// to the note itself have none), sorted.
    pub keys: Box<[Arc<str>]>,
}

impl Resolved {
    /// The `i`-th link's target.
    pub(crate) fn get(&self, i: usize) -> Option<LinkTarget> {
        let p = *self.targets.get(i)?;
        Some(LinkTarget {
            status: p.status(),
            target: self.paths.get(p.target as usize).cloned(),
        })
    }

    /// Distinct targets other than `source`.
    pub(crate) fn out<'a>(&'a self, source: &'a VaultPath) -> impl Iterator<Item = &'a VaultPath> {
        self.paths.iter().filter(move |p| *p != source)
    }
}

/// Shares equal strings (link destinations, tag names, lookup keys)
/// between notes.
#[derive(Debug, Default)]
pub(crate) struct Interner(HashSet<Arc<str>>);

impl Interner {
    pub(crate) fn intern(&mut self, s: &mut Arc<str>) {
        match self.0.get(&**s) {
            Some(shared) => *s = shared.clone(),
            None => {
                self.0.insert(s.clone());
            }
        }
    }

    /// The heap the strings take.
    pub(crate) fn bytes(&self) -> usize {
        self.0.iter().map(|s| s.len() + 16 + 24).sum()
    }

    /// Drops strings nothing uses any more.
    pub(crate) fn purge(&mut self) {
        self.0.retain(|s| Arc::strong_count(s) > 1);
    }

    fn data(&mut self, d: &mut NoteData) {
        for l in d.links.iter_mut().chain(&mut d.html_links) {
            self.intern(&mut l.raw);
        }
        for t in &mut d.tags {
            self.intern(&mut t.name);
        }
    }
}

/// A map from keys to sets of values, copy-on-write per key.
#[derive(Clone)]
pub(crate) struct SetMap<K, V> {
    map: CowMap<K, Arc<HashSet<V>>>,
}

impl<K: Hash + Eq + Clone, V: Hash + Eq + Clone> SetMap<K, V> {
    pub(crate) fn new() -> Self {
        Self { map: CowMap::new() }
    }

    pub(crate) fn from_map(map: HashMap<K, HashSet<V>>) -> Self {
        let mut out = Self::new();
        for (k, set) in map {
            out.map.insert(k, Arc::new(set));
        }
        out
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> impl Iterator<Item = &V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get(key).into_iter().flat_map(|s| s.iter())
    }

    pub(crate) fn add(&mut self, key: K, value: V) {
        match self.map.get_mut(&key) {
            Some(set) => {
                Arc::make_mut(set).insert(value);
            }
            None => {
                self.map.insert(key, Arc::new(HashSet::from([value])));
            }
        }
    }

    /// The number of keys and of values.
    pub(crate) fn stats(&self) -> (usize, usize) {
        (self.map.len(), self.map.iter().map(|(_, s)| s.len()).sum())
    }

    pub(crate) fn remove(&mut self, key: &K, value: &V) {
        let empty = match self.map.get_mut(key) {
            Some(set) => {
                let set = Arc::make_mut(set);
                set.remove(value);
                set.is_empty()
            }
            None => false,
        };
        if empty {
            self.map.remove(key);
        }
    }
}

/// The work was cancelled (the vault was closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// What one update did.
#[derive(Debug, Clone, Default)]
pub struct UpdateReport {
    /// Files read from disk.
    pub read: usize,
    /// Notes parsed (from disk or overlays).
    pub parsed: usize,
    /// Records dropped.
    pub removed: usize,
    /// Notes whose links were resolved again.
    pub resolved: usize,
    /// Notes whose effective record changed, sorted.
    pub changed: Vec<VaultPath>,
    /// Time spent comparing, reading and parsing files.
    pub read_time: Duration,
    /// Time spent resolving links and updating the link maps.
    pub resolve_time: Duration,
}

/// An overlay change: the text of an unsaved buffer, or `None` when the
/// buffer is clean again (or closed).
pub(crate) type OverlayChange = (VaultPath, Option<String>);

/// The content index state. See the [module docs](self).
#[derive(Clone)]
pub(crate) struct State {
    pub(crate) options: Arc<ContentOptions>,
    /// The path index the links were resolved against.
    pub(crate) paths: Option<PathIndex>,
    pub(crate) disk: CowMap<VaultPath, Arc<NoteRecord>>,
    pub(crate) overlay: CowMap<VaultPath, Arc<NoteRecord>>,
    /// The texts of the overlays (unlinked mentions read them).
    pub(crate) overlay_text: CowMap<VaultPath, Arc<str>>,
    pub(crate) resolved: CowMap<VaultPath, Arc<Resolved>>,
    /// Target → the notes linking to it.
    pub(crate) incoming: SetMap<VaultPath, VaultPath>,
    /// Lookup key → the notes with links through it.
    pub(crate) by_key: SetMap<Arc<str>, VaultPath>,
    /// Shared strings (the indexer thread is the only writer).
    pub(crate) strings: Arc<Mutex<Interner>>,
    /// The path index came from the cache: compare it with the next one
    /// entry by entry, and resolve the notes the cache had no links for.
    pub(crate) from_cache: bool,
    pub(crate) generation: u64,
}

impl State {
    pub(crate) fn new(options: Arc<ContentOptions>) -> Self {
        Self {
            options,
            paths: None,
            disk: CowMap::new(),
            overlay: CowMap::new(),
            overlay_text: CowMap::new(),
            resolved: CowMap::new(),
            incoming: SetMap::new(),
            by_key: SetMap::new(),
            strings: Arc::default(),
            from_cache: false,
            generation: 0,
        }
    }

    /// A state holding records loaded from the cache; nothing is resolved
    /// until the first [`apply`](Self::apply).
    pub(crate) fn with_records(options: Arc<ContentOptions>, records: Vec<NoteRecord>) -> Self {
        let mut state = Self::new(options);
        let mut strings = Interner::default();
        for mut r in records {
            strings.data(&mut r.data);
            state.disk.insert(r.path.clone(), Arc::new(r));
        }
        state.strings = Arc::new(Mutex::new(strings));
        state
    }

    /// Takes over a stored link graph: `previous` is the path index it was
    /// resolved against. The next [`apply`](Self::apply) compares the
    /// current path index with it, as after any change.
    pub(crate) fn adopt_graph(
        &mut self,
        previous: PathIndex,
        resolved: Vec<(VaultPath, Resolved)>,
    ) {
        let mut incoming: HashMap<VaultPath, HashSet<VaultPath>> = HashMap::new();
        let mut by_key: HashMap<Arc<str>, HashSet<VaultPath>> = HashMap::new();
        let mut strings = self.strings.lock().unwrap_or_else(PoisonError::into_inner);
        for (source, mut r) in resolved {
            // A resolution is only good for the record it was made from.
            let links = self.disk.get(&source).map(|d| d.data.links.len());
            if links != Some(r.targets.len()) {
                continue;
            }
            for k in &mut r.keys {
                strings.intern(k);
                by_key.entry(k.clone()).or_default().insert(source.clone());
            }
            for t in r.out(&source) {
                incoming
                    .entry(t.clone())
                    .or_default()
                    .insert(source.clone());
            }
            self.resolved.insert(source, Arc::new(r));
        }
        drop(strings);
        self.incoming = SetMap::from_map(incoming);
        self.by_key = SetMap::from_map(by_key);
        self.paths = Some(previous);
        self.from_cache = true;
    }

    /// The record that counts for `path`: the overlay's, else the disk's.
    pub(crate) fn effective(&self, path: &VaultPath) -> Option<&Arc<NoteRecord>> {
        self.overlay.get(path).or_else(|| self.disk.get(path))
    }

    /// Whether `path` names a note file (by extension).
    pub(crate) fn is_note(&self, path: &VaultPath) -> bool {
        is_note(&self.options, path)
    }

    /// Applies a newer path index and overlay changes.
    pub(crate) fn apply(
        &mut self,
        paths: Option<PathIndex>,
        overlays: Vec<OverlayChange>,
        source: &dyn FileSource,
        cancel: &AtomicBool,
    ) -> Result<UpdateReport, Cancelled> {
        let mut report = UpdateReport::default();
        let mut affected: BTreeSet<VaultPath> = BTreeSet::new();
        let mut keys: HashSet<Arc<str>> = HashSet::new();
        let mut full = false;

        if let Some(new) = paths {
            // 1. What the path change invalidates.
            match &self.paths {
                None => full = true,
                Some(old) if old.root() != new.root() => full = true,
                Some(old) if old.state() != new.state() => full = true,
                Some(old) if self.from_cache || old.generation() != new.generation() => {
                    invalidated_keys(old, &new, &mut keys);
                }
                Some(_) => {}
            }

            // 2. The content diff.
            let started = Instant::now();
            let mut jobs = Vec::new();
            for entry in new.iter() {
                if entry.kind != EntryKind::File || !self.is_note(&entry.path) {
                    continue;
                }
                let old = self.disk.get(&entry.path);
                let stale = match old {
                    None => true,
                    Some(r) => {
                        r.stamp.doubt
                            || r.stamp.size != entry.meta.size
                            || r.stamp.mtime_ns != mtime_ns(&entry.meta)
                    }
                };
                if stale {
                    jobs.push(Job {
                        path: entry.path.clone(),
                        meta: entry.meta.clone(),
                        old: old.cloned(),
                    });
                }
            }
            let gone: Vec<VaultPath> = self
                .disk
                .iter()
                .filter(|(path, _)| {
                    new.entry_by_path(path.as_str())
                        .is_none_or(|e| e.kind != EntryKind::File)
                })
                .map(|(path, _)| path.clone())
                .collect();

            let mut records = read_all(jobs, source, &self.options, cancel)?;
            report.read = records.len();
            {
                let mut strings = self.strings.lock().unwrap_or_else(PoisonError::into_inner);
                for (record, parsed) in &mut records {
                    if *parsed {
                        strings.data(&mut record.data);
                    }
                }
            }
            for (record, parsed) in records {
                report.parsed += usize::from(parsed);
                let unchanged = self
                    .disk
                    .get(&record.path)
                    .is_some_and(|old| old.kind == record.kind && old.data == record.data);
                if !unchanged && self.overlay.get(&record.path).is_none() {
                    affected.insert(record.path.clone());
                }
                self.disk.insert(record.path.clone(), Arc::new(record));
            }
            for path in gone {
                self.disk.remove(&path);
                report.removed += 1;
                if self.overlay.get(&path).is_none() {
                    affected.insert(path);
                }
            }
            self.paths = Some(new);
            report.read_time = started.elapsed();
        }

        // 3. Overlays.
        for (path, text) in overlays {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled);
            }
            match text {
                Some(text) => {
                    let mut data = extract(&text, self.options.profile);
                    self.strings
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .data(&mut data);
                    report.parsed += 1;
                    let same = self.effective(&path).is_some_and(|r| r.data == data);
                    let record = NoteRecord {
                        path: path.clone(),
                        stamp: FileStamp {
                            size: text.len() as u64,
                            ..FileStamp::default()
                        },
                        kind: RecordKind::Overlay,
                        data,
                    };
                    self.overlay.insert(path.clone(), Arc::new(record));
                    self.overlay_text.insert(path.clone(), Arc::from(text));
                    if !same {
                        affected.insert(path);
                    }
                }
                None => {
                    self.overlay_text.remove(&path);
                    if let Some(old) = self.overlay.remove(&path) {
                        let same = self.disk.get(&path).is_some_and(|d| d.data == old.data);
                        if !same {
                            affected.insert(path);
                        }
                    }
                }
            }
        }

        // 4. Re-resolution.
        let Some(paths) = self.paths.clone() else {
            report.changed = affected.into_iter().collect();
            return Ok(report);
        };
        let started = Instant::now();
        let mut todo: BTreeSet<VaultPath> = affected.clone();
        if full {
            todo.extend(self.disk.iter().map(|(p, _)| p.clone()));
            todo.extend(self.overlay.iter().map(|(p, _)| p.clone()));
            todo.extend(self.resolved.iter().map(|(p, _)| p.clone()));
        } else {
            for key in &keys {
                todo.extend(self.by_key.get(key).cloned());
            }
            if self.from_cache {
                // Notes the stored graph had no links for.
                todo.extend(
                    self.disk
                        .iter()
                        .map(|(p, _)| p)
                        .chain(self.overlay.iter().map(|(p, _)| p))
                        .filter(|p| self.resolved.get(*p).is_none())
                        .cloned(),
                );
            }
        }
        self.from_cache = false;
        let todo: Vec<VaultPath> = todo.into_iter().collect();
        let fresh = resolve_all(self, &paths, &todo, cancel)?;
        report.resolved = todo.len();
        let mut strings = self.strings.lock().unwrap_or_else(PoisonError::into_inner);
        if full {
            // Every note was resolved again: build the maps in one pass.
            let mut resolved = CowMap::new();
            let mut incoming: HashMap<VaultPath, HashSet<VaultPath>> = HashMap::new();
            let mut by_key: HashMap<Arc<str>, HashSet<VaultPath>> = HashMap::new();
            for (source, targets) in todo.into_iter().zip(fresh) {
                let Some(mut new) = targets else {
                    continue;
                };
                for k in &mut new.keys {
                    strings.intern(k);
                    by_key.entry(k.clone()).or_default().insert(source.clone());
                }
                for t in new.out(&source) {
                    incoming
                        .entry(t.clone())
                        .or_default()
                        .insert(source.clone());
                }
                resolved.insert(source, Arc::new(new));
            }
            self.resolved = resolved;
            self.incoming = SetMap::from_map(incoming);
            self.by_key = SetMap::from_map(by_key);
        } else {
            for (source, targets) in todo.into_iter().zip(fresh) {
                if let Some(old) = self.resolved.remove(&source) {
                    for t in old.out(&source) {
                        self.incoming.remove(t, &source);
                    }
                    for k in &old.keys {
                        self.by_key.remove(k, &source);
                    }
                }
                if let Some(mut new) = targets {
                    for k in &mut new.keys {
                        strings.intern(k);
                    }
                    for t in new.out(&source) {
                        self.incoming.add(t.clone(), source.clone());
                    }
                    for k in &new.keys {
                        self.by_key.add(k.clone(), source.clone());
                    }
                    self.resolved.insert(source, Arc::new(new));
                }
            }
        }
        drop(strings);
        self.generation += 1;
        report.resolve_time = started.elapsed();
        report.changed = affected.into_iter().collect();
        Ok(report)
    }
}

/// Whether `path` names a note file under `options`.
pub(crate) fn is_note(options: &ContentOptions, path: &VaultPath) -> bool {
    path.extension().is_some_and(|ext| {
        options
            .note_extensions
            .iter()
            .any(|n| n.eq_ignore_ascii_case(ext))
    })
}

/// The modification time in nanoseconds since the epoch.
pub(crate) fn mtime_ns(meta: &FileMeta) -> Option<i64> {
    meta.mtime.map(system_time_ns)
}

pub(crate) fn system_time_ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

/// The lookup key of a file or folder name, or of a link's last segment:
/// case-folded, up to the first dot.
pub(crate) fn name_key(name: &str) -> Arc<str> {
    let fold = fold_key(name);
    match fold.find('.') {
        Some(dot) => fold[..dot].into(),
        None => fold.into(),
    }
}

/// Keys whose links may resolve differently after the path index went
/// from `old` to `new`.
fn invalidated_keys(old: &PathIndex, new: &PathIndex, keys: &mut HashSet<Arc<str>>) {
    let touched = |index: &PathIndex, path: &VaultPath, keys: &mut HashSet<Arc<str>>| {
        let Some(name) = path.file_name() else {
            return;
        };
        keys.insert(name_key(name));
        let Some(parent) = path.parent() else {
            return;
        };
        // Folder targets and index files.
        if let Some(dir) = parent.file_name() {
            keys.insert(name_key(dir));
        }
        // Sync-conflict copies of this name next to it become (or stop
        // being) copies, which the basename lookup leaves out.
        let stem = fold_key(crate::path::stem_of(name));
        if let Some(dir) = index.id_of(parent.as_str()) {
            for child in index.children(dir) {
                if let Some(sibling) = index.get(*child).and_then(|e| e.path.file_name())
                    && sibling != name
                    && fold_key(sibling).starts_with(&stem)
                {
                    keys.insert(name_key(sibling));
                }
            }
        }
    };
    for e in new.iter() {
        let changed = old
            .entry_by_path(e.path.as_str())
            .is_none_or(|o| o.kind != e.kind || o.user_ignored != e.user_ignored);
        if changed {
            touched(new, &e.path, keys);
        }
    }
    for e in old.iter() {
        if new.entry_by_path(e.path.as_str()).is_none() {
            touched(old, &e.path, keys);
        }
    }
}

/// A file to read.
struct Job {
    path: VaultPath,
    meta: FileMeta,
    old: Option<Arc<NoteRecord>>,
}

/// Reads and parses `jobs` on the options' worker threads. Returns each
/// record and whether it was parsed (`false`: unchanged bytes, or not
/// read).
fn read_all(
    jobs: Vec<Job>,
    source: &dyn FileSource,
    options: &ContentOptions,
    cancel: &AtomicBool,
) -> Result<Vec<(NoteRecord, bool)>, Cancelled> {
    let now = system_time_ns(SystemTime::now());
    let run = |job: &Job| read_one(job, source, options, now);
    parallel_map(&jobs, options.threads, cancel, run)
}

fn read_one(
    job: &Job,
    source: &dyn FileSource,
    options: &ContentOptions,
    now: i64,
) -> (NoteRecord, bool) {
    let mtime = mtime_ns(&job.meta);
    let racy = i64::try_from(RACY_WINDOW.as_nanos()).unwrap_or(i64::MAX);
    let mut stamp = FileStamp {
        size: job.meta.size,
        mtime_ns: mtime,
        crc: 0,
        doubt: mtime.is_none_or(|m| now.saturating_sub(m) < racy),
    };
    let path_only = |kind: RecordKind, stamp: FileStamp| {
        (
            NoteRecord {
                path: job.path.clone(),
                stamp,
                kind,
                data: NoteData::default(),
            },
            false,
        )
    };
    match source.is_placeholder(&job.path) {
        Ok(true) => {
            return path_only(
                RecordKind::Placeholder,
                FileStamp {
                    doubt: false,
                    ..stamp
                },
            );
        }
        Ok(false) => {}
        Err(_) => return path_only(RecordKind::Unreadable, stamp),
    }
    if job.meta.size > options.max_file_bytes {
        return path_only(
            RecordKind::TooLarge,
            FileStamp {
                doubt: false,
                ..stamp
            },
        );
    }
    let bytes = match source.read(&job.path) {
        Ok(bytes) => bytes,
        Err(_) => return path_only(RecordKind::Unreadable, stamp),
    };
    if bytes.len() as u64 > options.max_file_bytes {
        return path_only(
            RecordKind::TooLarge,
            FileStamp {
                doubt: false,
                ..stamp
            },
        );
    }
    stamp.crc = crc32fast::hash(&bytes);
    if let Some(old) = &job.old
        && old.kind == RecordKind::Indexed
        && old.stamp.crc == stamp.crc
        && old.stamp.size == bytes.len() as u64
    {
        return (
            NoteRecord {
                path: job.path.clone(),
                stamp,
                kind: RecordKind::Indexed,
                data: old.data.clone(),
            },
            false,
        );
    }
    let text = String::from_utf8_lossy(&bytes);
    let data = extract(&text, options.profile);
    (
        NoteRecord {
            path: job.path.clone(),
            stamp,
            kind: RecordKind::Indexed,
            data,
        },
        true,
    )
}

/// Resolves the links of each of `sources` (`None`: the note is gone).
fn resolve_all(
    state: &State,
    paths: &PathIndex,
    sources: &[VaultPath],
    cancel: &AtomicBool,
) -> Result<Vec<Option<Resolved>>, Cancelled> {
    let params = &state.options.params;
    let run = |source: &VaultPath| {
        let record = state.effective(source)?;
        let mut keys: BTreeSet<Arc<str>> = BTreeSet::new();
        let resolutions: Vec<(Status, Option<VaultPath>)> = record
            .data
            .links
            .iter()
            .map(|link| {
                let lref = LinkRef::new(link.kind, &*link.raw, source.clone(), params.profile);
                if let DecodedTarget::Path(p) = decode(&lref, params).target
                    && let Some(last) = p.segments.iter().rev().find(|s| *s != "." && *s != "..")
                {
                    keys.insert(name_key(last));
                }
                let resolution = resolve(paths, &lref, params);
                // The index's own path, shared with it.
                let target = resolution
                    .target
                    .and_then(|id| paths.get(id))
                    .map(|e| e.path.clone())
                    .or(resolution.target_path);
                (resolution.status, target)
            })
            .collect();
        let distinct: BTreeSet<&VaultPath> =
            resolutions.iter().filter_map(|(_, t)| t.as_ref()).collect();
        let distinct: Vec<VaultPath> = distinct.into_iter().cloned().collect();
        let targets = resolutions
            .iter()
            .map(|(status, target)| {
                let index = target.as_ref().and_then(|t| {
                    distinct
                        .binary_search(t)
                        .ok()
                        .and_then(|i| u32::try_from(i).ok())
                });
                Packed::new(*status, index)
            })
            .collect();
        Some(Resolved {
            targets,
            paths: distinct.into_boxed_slice(),
            keys: keys.into_iter().collect(),
        })
    };
    parallel_map(sources, state.options.threads, cancel, run)
}

/// Maps `items` with `f` on up to `threads` scoped threads, in order.
/// Checks `cancel` between items.
pub(crate) fn parallel_map<T: Sync, R: Send>(
    items: &[T],
    threads: usize,
    cancel: &AtomicBool,
    f: impl Fn(&T) -> R + Sync,
) -> Result<Vec<R>, Cancelled> {
    const CHUNK: usize = 16;
    let threads = threads.max(1).min(items.len().div_ceil(CHUNK).max(1));
    if threads == 1 {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled);
            }
            out.push(f(item));
        }
        return Ok(out);
    }
    let next = AtomicUsize::new(0);
    let parts: Vec<Vec<(usize, R)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let start = next.fetch_add(CHUNK, Ordering::Relaxed);
                        if start >= items.len() {
                            break;
                        }
                        let end = (start + CHUNK).min(items.len());
                        for (i, item) in items[start..end].iter().enumerate() {
                            out.push((start + i, f(item)));
                        }
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    let mut slots: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    for (i, r) in parts.into_iter().flatten() {
        slots[i] = Some(r);
    }
    Ok(slots.into_iter().flatten().collect())
}

/// Reads a vault file for the index.
pub(crate) fn read_capped(path: &std::path::Path, limit: u64) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_cut_at_the_first_dot() {
        assert_eq!(&*name_key("My Note.md"), "my note");
        assert_eq!(&*name_key("v1.2 notes.md"), "v1");
        assert_eq!(&*name_key("v1.2 notes"), "v1");
        assert_eq!(&*name_key("Straße"), &*name_key("STRASSE"));
    }

    #[test]
    fn parallel_map_keeps_order_and_cancels() {
        let items: Vec<usize> = (0..1000).collect();
        let cancel = AtomicBool::new(false);
        let out = parallel_map(&items, 4, &cancel, |x| x * 2).expect("not cancelled");
        assert_eq!(out, items.iter().map(|x| x * 2).collect::<Vec<_>>());
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(parallel_map(&items, 4, &cancel, |x| *x), Err(Cancelled));
    }

    #[test]
    fn index_state_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<State>();
    }
}

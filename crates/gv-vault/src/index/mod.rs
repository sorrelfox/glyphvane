// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The path index: file names to [`FileId`]s, with NFC, case-fold and
//! basename keys.
//!
//! A [`PathIndex`] is an **immutable snapshot**. Cloning it is O(1), it is
//! `Send + Sync`, and readers never lock. Changes produce a new snapshot
//! with [`PathIndex::apply`], which shares everything it does not touch
//! with the old one (copy-on-write shards), so an incremental add, remove or
//! rename costs a small fraction of a rebuild. [`SharedPathIndex`] publishes
//! the current snapshot to other threads.
//!
//! # Maps
//!
//! | Map | Key | Value |
//! |---|---|---|
//! | `by_path` | exact path bytes | the entry |
//! | `by_nfc` | NFC of the path | entries (NFC/NFD twins share a key) |
//! | `by_fold` | canonical caseless key of the path ([`fold_key`]) | entries |
//! | `by_name` | caseless key of the **file name** (with extension) | files only |
//! | `children` | directory | its entries |
//!
//! Suffix lookups (`[[b/c]]` matching `a/b/c.md` but not `ab/c.md`) go
//! through `by_name` and then compare the trailing segments, so they cost
//! O(entries sharing the name), not O(vault).
//!
//! # States
//!
//! While the initial walk runs, the index is [`IndexState::Building`]: the
//! resolver answers `Pending` for anything it cannot decide from exact
//! paths ([`crate::resolve()`]).

pub mod conflict;
pub(crate) mod cow;
mod shared;
mod walk;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::keys::{fold_key, nfc_key};
use crate::path::{PathError, VaultPath};

use cow::{CowMap, CowMultiMap, CowTable};

pub use shared::SharedPathIndex;
pub use walk::WalkStats;
pub(crate) use walk::entry_of;

/// A session-stable handle for one indexed entry. It survives renames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
pub struct FileId(u32);

impl FileId {
    /// The vault root directory.
    pub const ROOT: FileId = FileId(0);

    /// The raw number.
    #[must_use]
    pub fn as_u32(self) -> u32 {
        self.0
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// What an entry is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link (never followed by the walk).
    Symlink(SymlinkTarget),
}

impl EntryKind {
    /// `true` for directories.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Dir)
    }
}

/// Where a symbolic link points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymlinkTarget {
    /// Inside the root; resolution follows it to this path.
    Inside(VaultPath),
    /// Outside the root; resolution reports `OutsideRoot`.
    Outside(PathBuf),
    /// Dangling, a loop, or unreadable.
    Broken,
}

/// The file identity the OS reports (`(dev, inode)` on Unix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    /// Device number.
    pub dev: u64,
    /// Inode number.
    pub ino: u64,
}

/// File metadata gathered by the walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMeta {
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// Modification time, if the OS reports one.
    pub mtime: Option<SystemTime>,
    /// OS file identity, if available.
    pub identity: Option<FileIdentity>,
}

/// Precomputed matching keys of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathKeys {
    pub(crate) nfc: Arc<str>,
    pub(crate) fold: Arc<str>,
    pub(crate) name_fold: Arc<str>,
}

impl PathKeys {
    pub(crate) fn of(path: &VaultPath) -> Self {
        let fold = fold_key(path.as_str());
        let name_fold = match fold.rfind('/') {
            Some(i) => fold[i + 1..].into(),
            None => fold.as_str().into(),
        };
        Self {
            nfc: nfc_key(path.as_str()).into(),
            fold: fold.into(),
            name_fold,
        }
    }
}

/// One indexed file, directory or symlink.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Its handle.
    pub id: FileId,
    /// Its path, exactly as on disk.
    pub path: VaultPath,
    /// File, directory or symlink.
    pub kind: EntryKind,
    /// Size, mtime, identity.
    pub meta: FileMeta,
    /// Matched by one of Obsidian's `userIgnoreFilters` (ranked last in
    /// basename lookups; excluded from search).
    pub user_ignored: bool,
    pub(crate) keys: PathKeys,
}

impl Entry {
    /// NFC form of the path.
    #[must_use]
    pub fn nfc_key(&self) -> &str {
        &self.keys.nfc
    }

    /// Canonical caseless key of the path.
    #[must_use]
    pub fn fold_key(&self) -> &str {
        &self.keys.fold
    }
}

/// Whether the initial walk has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    /// The walk is still running: the maps are incomplete.
    Building,
    /// Every entry under the root is indexed.
    Complete,
}

/// Directory names never indexed. `.obsidian` is configuration, read
/// separately.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    "node_modules",
    ".obsidian",
    ".trash",
    ".glyphvane",
    "target",
    ".venv",
];

/// Entry count above which indexing must not start without asking.
pub const AUTO_INDEX_LIMIT: usize = 50_000;

/// What the walk indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexConfig {
    /// Directory names skipped at any depth.
    pub exclude_names: Vec<String>,
    /// Index dot-files and dot-folders (off for Obsidian vaults, which hide
    /// them; on for repositories, where `.github/` files are link targets).
    pub include_hidden: bool,
    /// Honour `.gitignore` files (off for Obsidian vaults, on for GFM repos).
    pub respect_gitignore: bool,
    /// Obsidian `userIgnoreFilters`: path prefixes. Matching entries stay
    /// indexed but are flagged [`Entry::user_ignored`]. Regex filters
    /// (`/…/`) are not supported yet and are ignored.
    pub user_ignore_filters: Vec<String>,
    /// Walker threads; 0 picks `min(available, 8)`.
    pub threads: usize,
    /// Stop with [`IndexError::TooManyEntries`] past this many entries.
    pub entry_limit: Option<usize>,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            exclude_names: DEFAULT_EXCLUDES.iter().map(|s| (*s).to_owned()).collect(),
            include_hidden: false,
            respect_gitignore: false,
            user_ignore_filters: Vec::new(),
            threads: 0,
            entry_limit: Some(AUTO_INDEX_LIMIT),
        }
    }
}

impl IndexConfig {
    /// An Obsidian vault: no hidden files, no `.gitignore`, the vault's
    /// `userIgnoreFilters`.
    #[must_use]
    pub fn obsidian(user_ignore_filters: Vec<String>) -> Self {
        Self {
            user_ignore_filters,
            ..Self::default()
        }
    }

    /// A git repository: hidden files (except the default excludes) and
    /// `.gitignore` rules.
    #[must_use]
    pub fn git_repo() -> Self {
        Self {
            include_hidden: true,
            respect_gitignore: true,
            ..Self::default()
        }
    }

    /// `true` if `path` matches one of the `userIgnoreFilters`.
    ///
    /// ORACLE-PENDING: a filter is a plain prefix of the vault path
    /// (`Archive/`), compared case-sensitively; `/regex/` filters are not
    /// supported yet.
    #[must_use]
    pub fn is_user_ignored(&self, path: &VaultPath) -> bool {
        self.user_ignore_filters.iter().any(|f| {
            let regex = f.len() > 2 && f.starts_with('/') && f.ends_with('/');
            !f.is_empty() && !regex && path.as_str().starts_with(f.as_str())
        })
    }
}

/// One change for [`PathIndex::apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Adds an entry, or updates its metadata. Missing parent directories
    /// are added. Changing between file and directory replaces the entry
    /// (new [`FileId`]).
    Upsert {
        /// The path.
        path: VaultPath,
        /// File, directory or symlink.
        kind: EntryKind,
        /// Metadata.
        meta: FileMeta,
    },
    /// Removes an entry and, for a directory, everything below it.
    Remove {
        /// The path.
        path: VaultPath,
    },
    /// Moves an entry (and its subtree), keeping every [`FileId`]. An
    /// existing entry at `to` is replaced, as `rename(2)` does.
    Rename {
        /// Old path.
        from: VaultPath,
        /// New path.
        to: VaultPath,
    },
}

/// Errors from building or changing an index.
#[derive(Debug)]
pub enum IndexError {
    /// The path is not in the index.
    NotFound(VaultPath),
    /// A parent of the path is a file.
    NotADirectory(VaultPath),
    /// The root cannot be removed, renamed or replaced.
    Root,
    /// A rename into its own subtree.
    IntoItself(VaultPath),
    /// The walk found more entries than [`IndexConfig::entry_limit`].
    TooManyEntries {
        /// The limit that was hit.
        limit: usize,
    },
    /// A path that cannot be a vault path.
    Path(PathError),
    /// The root could not be read.
    Io(std::io::Error),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "not in the index: {p}"),
            Self::NotADirectory(p) => write!(f, "not a directory: {p}"),
            Self::Root => f.write_str("the root cannot be changed"),
            Self::IntoItself(p) => write!(f, "cannot move a directory into itself: {p}"),
            Self::TooManyEntries { limit } => write!(f, "more than {limit} entries"),
            Self::Path(e) => write!(f, "invalid path: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Path(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for IndexError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// An immutable snapshot of the path index. See the [module docs](self).
#[derive(Clone)]
pub struct PathIndex {
    inner: Arc<Inner>,
}

#[derive(Clone)]
pub(crate) struct Inner {
    root: Arc<Path>,
    config: Arc<IndexConfig>,
    state: IndexState,
    generation: u64,
    next_id: u32,
    entries: CowTable<Arc<Entry>>,
    by_path: CowMap<Arc<str>, FileId>,
    by_nfc: CowMultiMap<Arc<str>, FileId>,
    by_fold: CowMultiMap<Arc<str>, FileId>,
    by_name: CowMultiMap<Arc<str>, FileId>,
    children: CowMultiMap<FileId, FileId>,
    files: usize,
    dirs: usize,
    symlinks: usize,
}

impl fmt::Debug for PathIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PathIndex")
            .field("root", &self.inner.root)
            .field("state", &self.inner.state)
            .field("generation", &self.inner.generation)
            .field("files", &self.inner.files)
            .field("dirs", &self.inner.dirs)
            .field("symlinks", &self.inner.symlinks)
            .finish_non_exhaustive()
    }
}

impl PathIndex {
    /// An empty index in the [`IndexState::Building`] state: what readers
    /// see before the walk has produced anything.
    #[must_use]
    pub fn new_building(root: impl Into<PathBuf>, config: IndexConfig) -> Self {
        Self {
            inner: Arc::new(Inner::new(root.into(), config, IndexState::Building)),
        }
    }

    /// An index from a list of entries (tests, caches, synthetic vaults).
    /// Missing parent directories are added.
    pub fn from_entries<I>(
        root: impl Into<PathBuf>,
        config: IndexConfig,
        state: IndexState,
        entries: I,
    ) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = (VaultPath, EntryKind, FileMeta)>,
    {
        let mut inner = Inner::new(root.into(), config, state);
        let mut list: Vec<_> = entries.into_iter().collect();
        list.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, kind, meta) in list {
            inner.upsert(path, kind, meta, None)?;
        }
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Convenience for tests: files at the given paths (directories are
    /// implied), complete state, root `/vault`.
    pub fn from_paths<I, S>(paths: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut list = Vec::new();
        for p in paths {
            let p = p.as_ref();
            let (p, kind) = match p.strip_suffix('/') {
                Some(dir) => (dir, EntryKind::Dir),
                None => (p, EntryKind::File),
            };
            list.push((
                VaultPath::new(p).map_err(IndexError::Path)?,
                kind,
                FileMeta::default(),
            ));
        }
        Self::from_entries("/vault", IndexConfig::default(), IndexState::Complete, list)
    }

    pub(crate) fn from_inner(inner: Inner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    pub(crate) fn inner_arc(&self) -> &Arc<Inner> {
        &self.inner
    }

    pub(crate) fn from_arc(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    /// The absolute root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The configuration the index was built with.
    #[must_use]
    pub fn config(&self) -> &IndexConfig {
        &self.inner.config
    }

    /// Building or complete.
    #[must_use]
    pub fn state(&self) -> IndexState {
        self.inner.state
    }

    /// `true` once the initial walk is done.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.inner.state == IndexState::Complete
    }

    /// Increases with every [`apply`](Self::apply) and state change.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.generation
    }

    /// Indexed files (and symlinks), without directories.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.inner.files + self.inner.symlinks
    }

    /// Indexed directories, including the root.
    #[must_use]
    pub fn dir_count(&self) -> usize {
        self.inner.dirs
    }

    /// All entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.by_path.len()
    }

    /// `true` if only the root is indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }

    /// `true` if any symlink is indexed.
    #[must_use]
    pub fn has_symlinks(&self) -> bool {
        self.inner.symlinks > 0
    }

    /// The entry with this id.
    #[must_use]
    pub fn get(&self, id: FileId) -> Option<&Entry> {
        self.inner.entries.get(id.index()).map(|e| &**e)
    }

    /// The entry at exactly this path (byte-exact).
    #[must_use]
    pub fn entry_by_path(&self, path: &str) -> Option<&Entry> {
        self.get(*self.inner.by_path.get(path)?)
    }

    /// The id at exactly this path (byte-exact).
    #[must_use]
    pub fn id_of(&self, path: &str) -> Option<FileId> {
        self.inner.by_path.get(path).copied()
    }

    /// Entries whose path is NFC-equal to `nfc` (which must be NFC).
    #[must_use]
    pub fn by_nfc(&self, nfc: &str) -> &[FileId] {
        self.inner.by_nfc.get(nfc)
    }

    /// Entries whose path has this canonical caseless key.
    #[must_use]
    pub fn by_fold(&self, fold: &str) -> &[FileId] {
        self.inner.by_fold.get(fold)
    }

    /// Files whose **name** has this canonical caseless key.
    #[must_use]
    pub fn by_name_fold(&self, name_fold: &str) -> &[FileId] {
        self.inner.by_name.get(name_fold)
    }

    /// The entries directly inside a directory.
    #[must_use]
    pub fn children(&self, dir: FileId) -> &[FileId] {
        self.inner.children.get(&dir)
    }

    /// Every entry, in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Entry> {
        self.inner.entries.iter().map(|e| &**e)
    }

    /// A snapshot with `changes` applied. `self` is not modified; the new
    /// snapshot shares all untouched data with it. All-or-nothing: on an
    /// error, no change is applied.
    pub fn apply(&self, changes: &[Change]) -> Result<PathIndex, IndexError> {
        let mut inner = (*self.inner).clone();
        for change in changes {
            match change {
                Change::Upsert { path, kind, meta } => {
                    inner.upsert(path.clone(), kind.clone(), meta.clone(), None)?;
                }
                Change::Remove { path } => inner.remove(path)?,
                Change::Rename { from, to } => inner.rename(from, to)?,
            }
        }
        inner.generation += 1;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// A snapshot in another state (the walk marks the index complete).
    #[must_use]
    pub fn with_state(&self, state: IndexState) -> PathIndex {
        let mut inner = (*self.inner).clone();
        inner.state = state;
        inner.generation += 1;
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Checks the internal maps against each other (tests and debugging).
    /// Returns a description of the first inconsistency.
    pub fn check_consistency(&self) -> Result<(), String> {
        let inner = &*self.inner;
        let mut count = 0;
        for e in inner.entries.iter() {
            count += 1;
            let keys = PathKeys::of(&e.path);
            if keys != e.keys {
                return Err(format!("stale keys for {}", e.path));
            }
            if inner.by_path.get(e.path.as_str()) != Some(&e.id) {
                return Err(format!("by_path misses {}", e.path));
            }
            if !inner.by_nfc.get(&*keys.nfc).contains(&e.id) {
                return Err(format!("by_nfc misses {}", e.path));
            }
            if !inner.by_fold.get(&*keys.fold).contains(&e.id) {
                return Err(format!("by_fold misses {}", e.path));
            }
            let named = inner.by_name.get(&*keys.name_fold).contains(&e.id);
            if named == e.kind.is_dir() {
                return Err(format!("by_name wrong for {}", e.path));
            }
            if let Some(parent) = e.path.parent() {
                let pid = inner.by_path.get(parent.as_str()).ok_or("missing parent")?;
                if !inner.children.get(pid).contains(&e.id) {
                    return Err(format!("children of {parent} miss {}", e.path));
                }
            }
            let ignored = inner.config.is_user_ignored(&e.path);
            if ignored != e.user_ignored {
                return Err(format!("user_ignored stale for {}", e.path));
            }
        }
        if count != inner.by_path.len() {
            return Err(format!("{count} entries, {} paths", inner.by_path.len()));
        }
        let dirs = inner.entries.iter().filter(|e| e.kind.is_dir()).count();
        if dirs != inner.dirs {
            return Err(format!("dir count {} vs {dirs}", inner.dirs));
        }
        Ok(())
    }
}

impl Inner {
    fn new(root: PathBuf, config: IndexConfig, state: IndexState) -> Self {
        let mut inner = Self {
            root: Arc::from(root.as_path()),
            config: Arc::new(config),
            state,
            generation: 0,
            next_id: 0,
            entries: CowTable::new(),
            by_path: CowMap::new(),
            by_nfc: CowMultiMap::new(),
            by_fold: CowMultiMap::new(),
            by_name: CowMultiMap::new(),
            children: CowMultiMap::new(),
            files: 0,
            dirs: 0,
            symlinks: 0,
        };
        let root_path = VaultPath::root();
        let keys = PathKeys::of(&root_path);
        inner.insert_new(root_path, EntryKind::Dir, FileMeta::default(), keys);
        inner
    }

    pub(crate) fn set_state(&mut self, state: IndexState) {
        self.state = state;
    }

    fn count(&mut self, kind: &EntryKind, delta: isize) {
        let slot = match kind {
            EntryKind::File => &mut self.files,
            EntryKind::Dir => &mut self.dirs,
            EntryKind::Symlink(_) => &mut self.symlinks,
        };
        *slot = slot.saturating_add_signed(delta);
    }

    /// Inserts a path that is not indexed yet; its parent must exist.
    fn insert_new(
        &mut self,
        path: VaultPath,
        kind: EntryKind,
        meta: FileMeta,
        keys: PathKeys,
    ) -> FileId {
        let id = FileId(self.next_id);
        self.next_id += 1;
        self.link_keys(id, &path, &kind, &keys);
        self.count(&kind, 1);
        let user_ignored = !path.is_root() && self.config.is_user_ignored(&path);
        self.entries.set(
            id.index(),
            Some(Arc::new(Entry {
                id,
                path,
                kind,
                meta,
                user_ignored,
                keys,
            })),
        );
        id
    }

    fn link_keys(&mut self, id: FileId, path: &VaultPath, kind: &EntryKind, keys: &PathKeys) {
        self.by_path.insert(path.shared_str(), id);
        self.by_nfc.add(keys.nfc.clone(), id);
        self.by_fold.add(keys.fold.clone(), id);
        if !kind.is_dir() {
            self.by_name.add(keys.name_fold.clone(), id);
        }
        if let Some(parent) = path.parent()
            && let Some(&pid) = self.by_path.get(parent.as_str())
        {
            self.children.add(pid, id);
        }
    }

    fn unlink_keys(&mut self, entry: &Entry) {
        let id = entry.id;
        self.by_path.remove(entry.path.as_str());
        self.by_nfc.remove(&entry.keys.nfc, &id);
        self.by_fold.remove(&entry.keys.fold, &id);
        if !entry.kind.is_dir() {
            self.by_name.remove(&entry.keys.name_fold, &id);
        }
        if let Some(parent) = entry.path.parent()
            && let Some(&pid) = self.by_path.get(parent.as_str())
        {
            self.children.remove(&pid, &id);
        }
    }

    fn entry(&self, id: FileId) -> Option<Arc<Entry>> {
        self.entries.get(id.index()).cloned()
    }

    /// Makes sure `dir` exists as a directory, adding missing ancestors.
    fn ensure_dir(&mut self, dir: &VaultPath) -> Result<FileId, IndexError> {
        if let Some(&id) = self.by_path.get(dir.as_str()) {
            let e = self
                .entry(id)
                .ok_or_else(|| IndexError::NotFound(dir.clone()))?;
            return if e.kind.is_dir() {
                Ok(id)
            } else {
                Err(IndexError::NotADirectory(dir.clone()))
            };
        }
        if let Some(parent) = dir.parent() {
            self.ensure_dir(&parent)?;
        }
        let keys = PathKeys::of(dir);
        Ok(self.insert_new(dir.clone(), EntryKind::Dir, FileMeta::default(), keys))
    }

    pub(crate) fn upsert(
        &mut self,
        path: VaultPath,
        kind: EntryKind,
        meta: FileMeta,
        keys: Option<PathKeys>,
    ) -> Result<FileId, IndexError> {
        if path.is_root() {
            return if kind.is_dir() {
                Ok(FileId::ROOT)
            } else {
                Err(IndexError::Root)
            };
        }
        if let Some(&id) = self.by_path.get(path.as_str()) {
            let old = self
                .entry(id)
                .ok_or_else(|| IndexError::NotFound(path.clone()))?;
            if old.kind.is_dir() == kind.is_dir() {
                if old.kind != kind {
                    self.count(&old.kind, -1);
                    self.count(&kind, 1);
                }
                let mut e = (*old).clone();
                e.kind = kind;
                e.meta = meta;
                self.entries.set(id.index(), Some(Arc::new(e)));
                return Ok(id);
            }
            self.remove(&path)?;
        }
        if let Some(parent) = path.parent() {
            self.ensure_dir(&parent)?;
        }
        let keys = keys.unwrap_or_else(|| PathKeys::of(&path));
        Ok(self.insert_new(path, kind, meta, keys))
    }

    fn remove(&mut self, path: &VaultPath) -> Result<(), IndexError> {
        if path.is_root() {
            return Err(IndexError::Root);
        }
        let id = *self
            .by_path
            .get(path.as_str())
            .ok_or_else(|| IndexError::NotFound(path.clone()))?;
        self.remove_subtree(id);
        Ok(())
    }

    fn remove_subtree(&mut self, id: FileId) {
        let kids: Vec<FileId> = self.children.get(&id).to_vec();
        for kid in kids {
            self.remove_subtree(kid);
        }
        if let Some(e) = self.entry(id) {
            self.unlink_keys(&e);
            self.count(&e.kind, -1);
            self.entries.set(id.index(), None);
        }
    }

    fn rename(&mut self, from: &VaultPath, to: &VaultPath) -> Result<(), IndexError> {
        if from.is_root() || to.is_root() {
            return Err(IndexError::Root);
        }
        if from == to {
            return Ok(());
        }
        let id = *self
            .by_path
            .get(from.as_str())
            .ok_or_else(|| IndexError::NotFound(from.clone()))?;
        if to.starts_with(from) || from.starts_with(to) {
            return Err(IndexError::IntoItself(to.clone()));
        }
        if self.by_path.get(to.as_str()).is_some() {
            self.remove(to)?;
        }
        if let Some(parent) = to.parent() {
            self.ensure_dir(&parent)?;
        }
        // Collect the subtree (pre-order), then re-key every entry.
        let mut subtree = Vec::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            subtree.push(cur);
            stack.extend_from_slice(self.children.get(&cur));
        }
        let old: Vec<Arc<Entry>> = subtree.iter().filter_map(|&i| self.entry(i)).collect();
        for e in &old {
            self.unlink_keys(e);
        }
        for e in &old {
            let rest = e.path.strip_prefix(from).unwrap_or("");
            let new_path = if rest.is_empty() {
                to.clone()
            } else {
                VaultPath::new(&format!("{}/{rest}", to.as_str())).map_err(IndexError::Path)?
            };
            let keys = PathKeys::of(&new_path);
            let mut moved = (**e).clone();
            moved.user_ignored = self.config.is_user_ignored(&new_path);
            moved.path = new_path;
            moved.keys = keys;
            self.entries.set(e.id.index(), Some(Arc::new(moved)));
        }
        // Parents are linked before children because `old` is pre-order.
        for e in &old {
            if let Some(moved) = self.entry(e.id) {
                self.link_keys(moved.id, &moved.path, &moved.kind, &moved.keys);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VaultPath {
        VaultPath::new(s).unwrap()
    }

    #[test]
    fn from_paths_adds_parents() {
        let idx = PathIndex::from_paths(["a/b/c.md", "d.md"]).unwrap();
        assert_eq!(idx.file_count(), 2);
        assert_eq!(idx.dir_count(), 3); // root, a, a/b
        assert!(idx.entry_by_path("a/b").unwrap().kind.is_dir());
        idx.check_consistency().unwrap();
    }

    #[test]
    fn rename_keeps_ids_and_moves_subtree() {
        let idx = PathIndex::from_paths(["a/b/c.md", "a/x.md"]).unwrap();
        let c = idx.id_of("a/b/c.md").unwrap();
        let next = idx
            .apply(&[Change::Rename {
                from: vp("a"),
                to: vp("z/a2"),
            }])
            .unwrap();
        assert_eq!(next.id_of("z/a2/b/c.md"), Some(c));
        assert_eq!(next.id_of("a/b/c.md"), None);
        next.check_consistency().unwrap();
        // The old snapshot is untouched.
        assert_eq!(idx.id_of("a/b/c.md"), Some(c));
        idx.check_consistency().unwrap();
    }

    #[test]
    fn case_only_rename() {
        let idx = PathIndex::from_paths(["Note.md"]).unwrap();
        let id = idx.id_of("Note.md").unwrap();
        let next = idx
            .apply(&[Change::Rename {
                from: vp("Note.md"),
                to: vp("note.md"),
            }])
            .unwrap();
        assert_eq!(next.id_of("note.md"), Some(id));
        assert_eq!(next.by_fold("note.md"), &[id]);
        next.check_consistency().unwrap();
    }

    #[test]
    fn remove_and_errors() {
        let idx = PathIndex::from_paths(["a/b.md", "a/c.md", "d.md"]).unwrap();
        let next = idx.apply(&[Change::Remove { path: vp("a") }]).unwrap();
        assert_eq!(next.len(), 2); // root + d.md
        next.check_consistency().unwrap();
        assert!(matches!(
            next.apply(&[Change::Remove { path: vp("a") }]),
            Err(IndexError::NotFound(_))
        ));
        assert!(matches!(
            idx.apply(&[Change::Upsert {
                path: vp("d.md/x"),
                kind: EntryKind::File,
                meta: FileMeta::default()
            }]),
            Err(IndexError::NotADirectory(_))
        ));
        assert!(matches!(
            idx.apply(&[Change::Rename {
                from: vp("a"),
                to: vp("a/b2")
            }]),
            Err(IndexError::IntoItself(_))
        ));
    }

    #[test]
    fn user_ignore_filters_flag_entries() {
        let cfg = IndexConfig::obsidian(vec!["Archive/".into(), "/re/".into()]);
        let idx = PathIndex::from_entries(
            "/v",
            cfg,
            IndexState::Complete,
            [
                (vp("Archive/x.md"), EntryKind::File, FileMeta::default()),
                (vp("Archived.md"), EntryKind::File, FileMeta::default()),
            ],
        )
        .unwrap();
        assert!(idx.entry_by_path("Archive/x.md").unwrap().user_ignored);
        assert!(!idx.entry_by_path("Archived.md").unwrap().user_ignored);
        let moved = idx
            .apply(&[Change::Rename {
                from: vp("Archived.md"),
                to: vp("Archive/y.md"),
            }])
            .unwrap();
        assert!(moved.entry_by_path("Archive/y.md").unwrap().user_ignored);
        moved.check_consistency().unwrap();
    }
}

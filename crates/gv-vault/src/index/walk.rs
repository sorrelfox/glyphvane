// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The initial directory walk.
//!
//! A parallel walk (the `ignore` crate) gathers path, kind, size, mtime and
//! file identity, and computes the matching keys on the walker threads.
//! The entries are then sorted by path, so [`FileId`]s are assigned in a
//! deterministic order, and inserted into a fresh index.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};

use super::{
    EntryKind, FileIdentity, FileMeta, IndexConfig, IndexError, IndexState, Inner, PathIndex,
    PathKeys, SymlinkTarget,
};
use crate::path::{PathError, VaultPath};

/// What the walk saw.
#[derive(Debug, Clone, Default)]
pub struct WalkStats {
    /// Entries indexed (files, directories and symlinks, without the root).
    pub entries: usize,
    /// Names skipped because they are not valid UTF-8.
    pub skipped_non_utf8: usize,
    /// Entries the walk could not read (permissions, races).
    pub errors: usize,
    /// The first few error messages.
    pub error_samples: Vec<String>,
    /// Time spent walking (parallel part).
    pub walk_time: Duration,
    /// Time spent building the maps.
    pub index_time: Duration,
}

struct RawEntry {
    path: VaultPath,
    kind: EntryKind,
    meta: FileMeta,
    keys: PathKeys,
}

#[derive(Default)]
struct Shared {
    batches: Mutex<Vec<Vec<RawEntry>>>,
    errors: Mutex<Vec<String>>,
    error_count: AtomicUsize,
    non_utf8: AtomicUsize,
    count: AtomicUsize,
    over_limit: AtomicBool,
}

struct Collector<'a> {
    root: &'a Path,
    canonical_root: Option<&'a Path>,
    limit: Option<usize>,
    shared: &'a Shared,
    out: Vec<RawEntry>,
}

impl Collector<'_> {
    fn error(&self, msg: String) {
        if self.shared.error_count.fetch_add(1, Ordering::Relaxed) < 10 {
            self.shared
                .errors
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(msg);
        }
    }

    fn take(&mut self, entry: &DirEntry) -> Option<RawEntry> {
        let rel = entry.path().strip_prefix(self.root).ok()?;
        let path = match VaultPath::from_relative_fs_path(rel) {
            Ok(p) if !p.is_root() => p,
            Ok(_) => return None,
            Err(PathError::NotUtf8) => {
                self.shared.non_utf8.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            Err(e) => {
                self.error(format!("{}: {e}", entry.path().display()));
                return None;
            }
        };
        let ft = entry.file_type()?;
        let kind = if ft.is_symlink() {
            EntryKind::Symlink(symlink_target(entry.path(), self.canonical_root))
        } else if ft.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        let meta = match entry.metadata() {
            Ok(m) => FileMeta {
                size: if ft.is_file() { m.len() } else { 0 },
                mtime: m.modified().ok(),
                identity: identity(&m),
            },
            Err(e) => {
                self.error(format!("{}: {e}", entry.path().display()));
                FileMeta::default()
            }
        };
        let keys = PathKeys::of(&path);
        Some(RawEntry {
            path,
            kind,
            meta,
            keys,
        })
    }
}

impl ParallelVisitor for Collector<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        match entry {
            Ok(entry) => {
                if entry.depth() == 0 {
                    return WalkState::Continue;
                }
                if let Some(limit) = self.limit
                    && self.shared.count.fetch_add(1, Ordering::Relaxed) >= limit
                {
                    self.shared.over_limit.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
                if let Some(raw) = self.take(&entry) {
                    self.out.push(raw);
                }
            }
            Err(e) => self.error(e.to_string()),
        }
        WalkState::Continue
    }
}

impl Drop for Collector<'_> {
    fn drop(&mut self) {
        let batch = std::mem::take(&mut self.out);
        self.shared
            .batches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(batch);
    }
}

struct Builder<'a> {
    root: &'a Path,
    canonical_root: Option<&'a Path>,
    limit: Option<usize>,
    shared: &'a Shared,
}

impl<'s> ParallelVisitorBuilder<'s> for Builder<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Collector {
            root: self.root,
            canonical_root: self.canonical_root,
            limit: self.limit,
            shared: self.shared,
            out: Vec::new(),
        })
    }
}

/// Where the symbolic link at `path` points, relative to the vault whose
/// canonical root is `canonical_root`.
pub(crate) fn symlink_target(path: &Path, canonical_root: Option<&Path>) -> SymlinkTarget {
    let (Ok(target), Some(croot)) = (fs::canonicalize(path), canonical_root) else {
        return SymlinkTarget::Broken;
    };
    match target.strip_prefix(croot) {
        Ok(rel) => match VaultPath::from_relative_fs_path(rel) {
            Ok(vp) => SymlinkTarget::Inside(vp),
            Err(_) => SymlinkTarget::Broken,
        },
        Err(_) => SymlinkTarget::Outside(target),
    }
}

/// The kind and metadata of the entry at `path`, without following a final
/// symbolic link: the same values the walk records.
pub(crate) fn entry_of(
    path: &Path,
    canonical_root: Option<&Path>,
) -> std::io::Result<(EntryKind, FileMeta)> {
    let m = fs::symlink_metadata(path)?;
    let ft = m.file_type();
    let kind = if ft.is_symlink() {
        EntryKind::Symlink(symlink_target(path, canonical_root))
    } else if ft.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let meta = FileMeta {
        size: if ft.is_file() { m.len() } else { 0 },
        mtime: m.modified().ok(),
        identity: identity(&m),
    };
    Ok((kind, meta))
}

#[cfg(unix)]
fn identity(m: &fs::Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        dev: m.dev(),
        ino: m.ino(),
    })
}

#[cfg(not(unix))]
fn identity(_m: &fs::Metadata) -> Option<FileIdentity> {
    // The NTFS file id needs a handle-based query; added with the Windows
    // watcher work.
    None
}

fn thread_count(config: &IndexConfig) -> usize {
    if config.threads > 0 {
        config.threads
    } else {
        std::thread::available_parallelism().map_or(4, |n| n.get().min(8))
    }
}

impl PathIndex {
    /// Walks `root` and builds a complete index. Blocking and parallel; run
    /// it on a background thread. See [`SharedPathIndex`](super::SharedPathIndex)
    /// for publishing.
    pub fn build(root: &Path, config: IndexConfig) -> Result<(PathIndex, WalkStats), IndexError> {
        let root: PathBuf = std::path::absolute(root)?;
        if !fs::metadata(&root)?.is_dir() {
            return Err(IndexError::Io(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "index root is not a directory",
            )));
        }
        let canonical_root = fs::canonicalize(&root).ok();
        let started = Instant::now();

        let mut wb = WalkBuilder::new(&root);
        wb.standard_filters(false)
            .hidden(!config.include_hidden)
            .git_ignore(config.respect_gitignore)
            .git_exclude(config.respect_gitignore)
            .require_git(false)
            .follow_links(false)
            .threads(thread_count(&config));
        let excludes: Vec<String> = config.exclude_names.clone();
        wb.filter_entry(move |e| {
            let name = e.file_name();
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            e.depth() == 0
                || !(excludes.iter().any(|x| name == OsStr::new(x)) && (is_dir || name == ".git"))
        });

        let shared = Shared::default();
        wb.build_parallel().visit(&mut Builder {
            root: &root,
            canonical_root: canonical_root.as_deref(),
            limit: config.entry_limit,
            shared: &shared,
        });
        if shared.over_limit.load(Ordering::Relaxed)
            && let Some(limit) = config.entry_limit
        {
            return Err(IndexError::TooManyEntries { limit });
        }
        let walk_time = started.elapsed();

        let indexing = Instant::now();
        let mut raw: Vec<RawEntry> = shared
            .batches
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .into_iter()
            .flatten()
            .collect();
        raw.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let entries = raw.len();
        let mut inner = Inner::new(root, config, IndexState::Building);
        for r in raw {
            inner.upsert(r.path, r.kind, r.meta, Some(r.keys))?;
        }
        inner.set_state(IndexState::Complete);
        let stats = WalkStats {
            entries,
            skipped_non_utf8: shared.non_utf8.load(Ordering::Relaxed),
            errors: shared.error_count.load(Ordering::Relaxed),
            error_samples: shared
                .errors
                .into_inner()
                .unwrap_or_else(PoisonError::into_inner),
            walk_time,
            index_time: indexing.elapsed(),
        };
        Ok((PathIndex::from_inner(inner), stats))
    }
}

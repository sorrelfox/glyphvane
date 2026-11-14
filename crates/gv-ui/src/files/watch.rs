// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Which open files changed on disk, per window.
//!
//! - **Open documents** are watched lightly in every window: the folder of
//!   each open file, not recursively (`notify`, debounced). An event for an
//!   open file compares the file with its baseline
//!   ([`check_external_change`]) and reports a
//!   [`WatchEvent::DocumentChanged`]; a new file next to it named like a
//!   sync client's conflict copy of it reports a
//!   [`WatchEvent::SyncConflict`]. A window opened for a file indexes
//!   nothing (FR-VIEW-16): watching one folder costs one OS watch.
//! - **The vault**, once the window has one and its index is built, gets a
//!   [`VaultWatcher`], which keeps the path index in step with the disk
//!   ([`WatchEvent::IndexUpdated`]) and reports sync-conflict copies
//!   anywhere in it.
//!
//! **Own saves** are announced around every write
//! ([`WatchHandle::begin_write`], then [`WatchHandle::expect_write`] or
//! [`WatchHandle::cancel_write`]): events for the file wait meanwhile, and
//! the saved state becomes the baseline, so the save is not reported back
//! as an external change. Both watchers are told.
//!
//! All checks read files on the watcher's thread or the background
//! executor, never on the UI thread.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use gpui::BackgroundExecutor;
use gv_vault::files::{
    CheckMode, DiskState, ExternalChange, THOROUGH_CHECK_BYTES, check_external_change,
};
use gv_vault::index::conflict::detect_sync_conflict;
use gv_vault::{SharedPathIndex, VaultWatcher, WatchEvent, WatchOptions};
use notify_debouncer_full::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

/// How long OS events are collected before the files are compared.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// How long an announced own write holds back events for its file, at most.
const WRITE_TTL: Duration = Duration::from_secs(30);

type FolderWatcher = Debouncer<RecommendedWatcher, RecommendedCache>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An open file being watched.
#[derive(Debug, Default)]
struct Tracked {
    /// What the file was when last loaded, saved or reported; `None` until
    /// it was read.
    baseline: Option<DiskState>,
    /// The path with symlinks in its folder resolved, for platforms whose
    /// events name canonical paths.
    canonical: Option<PathBuf>,
    /// Reported as deleted and not back yet.
    deleted: bool,
    /// An own write is running since then.
    writing: Option<Instant>,
    /// An event came while the write ran.
    missed: bool,
}

/// What the folder watcher's thread shares with the window.
#[derive(Debug)]
struct Files {
    docs: HashMap<PathBuf, Tracked>,
    /// Watched folders, with the number of open files in each.
    folders: HashMap<PathBuf, usize>,
    /// Sync-conflict copies already reported.
    copies: HashSet<PathBuf>,
    events: mpsc::UnboundedSender<WatchEvent>,
}

impl Files {
    fn send(&self, event: WatchEvent) {
        // The receiver is gone only when the window closed.
        let _ = self.events.unbounded_send(event);
    }

    /// The tracked path an event path names, if any.
    fn key_of(&self, path: &Path) -> Option<PathBuf> {
        if self.docs.contains_key(path) {
            return Some(path.to_path_buf());
        }
        self.docs
            .iter()
            .find(|(_, t)| t.canonical.as_deref() == Some(path))
            .map(|(p, _)| p.clone())
    }
}

struct Inner {
    files: Arc<Mutex<Files>>,
    folder_watcher: Arc<Mutex<Option<FolderWatcher>>>,
    vault: Arc<Mutex<Option<VaultWatcher>>>,
    executor: BackgroundExecutor,
}

/// The watchers of one window. Clones share them; they stop when the last
/// clone is dropped.
#[derive(Clone)]
pub(crate) struct WatchHandle(Arc<Inner>);

impl std::fmt::Debug for WatchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchHandle")
            .field("files", &lock(&self.0.files).docs.len())
            .finish_non_exhaustive()
    }
}

impl WatchHandle {
    /// Watchers for a window; their events arrive on the receiver.
    pub fn new(executor: BackgroundExecutor) -> (Self, mpsc::UnboundedReceiver<WatchEvent>) {
        let (events, rx) = mpsc::unbounded();
        let files = Files {
            docs: HashMap::new(),
            folders: HashMap::new(),
            copies: HashSet::new(),
            events,
        };
        let inner = Inner {
            files: Arc::new(Mutex::new(files)),
            folder_watcher: Arc::new(Mutex::new(None)),
            vault: Arc::new(Mutex::new(None)),
            executor,
        };
        (Self(Arc::new(inner)), rx)
    }

    /// Watches the open file at `path` (absolute), whose state on disk is
    /// `baseline`, or is read now (off the UI thread) when not given.
    /// Tracking a file again replaces its baseline.
    pub fn track(&self, path: &Path, baseline: Option<DiskState>) {
        let folder = path.parent().map(Path::to_path_buf);
        let new_folder = {
            let mut files = lock(&self.0.files);
            let known = files.docs.contains_key(path);
            let tracked = files.docs.entry(path.to_path_buf()).or_default();
            if baseline.is_some() {
                tracked.baseline = baseline;
                tracked.deleted = false;
            } else if known {
                return;
            }
            match &folder {
                Some(folder) if !known => {
                    let count = files.folders.entry(folder.clone()).or_insert(0);
                    *count += 1;
                    *count == 1
                }
                _ => false,
            }
        };
        let files = self.0.files.clone();
        let watcher = self.0.folder_watcher.clone();
        let path = path.to_path_buf();
        self.0
            .executor
            .spawn(async move {
                let canonical = canonical(&path);
                let read = DiskState::read(&path).ok().map(|(_, state)| state);
                {
                    let mut files = lock(&files);
                    if let Some(tracked) = files.docs.get_mut(&path) {
                        tracked.canonical = canonical;
                        if tracked.baseline.is_none() {
                            tracked.baseline = read;
                        }
                    }
                }
                // Unless every file in the folder closed meanwhile.
                if new_folder
                    && let Some(folder) = folder
                    && lock(&files).folders.contains_key(&folder)
                {
                    watch_folder(&watcher, &files, &folder);
                }
            })
            .detach();
    }

    /// Stops watching the file at `path`.
    pub fn untrack(&self, path: &Path) {
        let folder = path.parent().map(Path::to_path_buf);
        let unwatch = {
            let mut files = lock(&self.0.files);
            if files.docs.remove(path).is_none() {
                return;
            }
            match folder.as_ref().and_then(|f| files.folders.get_mut(f)) {
                Some(count) => {
                    *count -= 1;
                    *count == 0
                }
                None => false,
            }
        };
        if unwatch && let Some(folder) = folder {
            lock(&self.0.files).folders.remove(&folder);
            let watcher = self.0.folder_watcher.clone();
            self.0
                .executor
                .spawn(async move {
                    if let Some(watcher) = lock(&watcher).as_mut() {
                        // Fails when the folder is gone already.
                        let _ = watcher.unwatch(&folder);
                    }
                })
                .detach();
        }
    }

    /// The app is about to write `path`.
    pub fn begin_write(&self, path: &Path) {
        if let Some(tracked) = lock(&self.0.files).docs.get_mut(path) {
            tracked.writing = Some(Instant::now());
        }
        if let Some(vault) = lock(&self.0.vault).as_ref() {
            vault.begin_write(path);
        }
    }

    /// The app wrote `path`, which is now in `state`.
    pub fn expect_write(&self, path: &Path, state: DiskState) {
        if let Some(vault) = lock(&self.0.vault).as_ref() {
            vault.expect_write(path, state.clone());
        }
        let missed = {
            let mut files = lock(&self.0.files);
            let Some(tracked) = files.docs.get_mut(path) else {
                return;
            };
            tracked.baseline = Some(state);
            tracked.deleted = false;
            tracked.writing = None;
            std::mem::take(&mut tracked.missed)
        };
        if missed {
            self.recheck(path);
        }
    }

    /// The write announced for `path` did not happen.
    pub fn cancel_write(&self, path: &Path) {
        if let Some(vault) = lock(&self.0.vault).as_ref() {
            vault.cancel_write(path);
        }
        let missed = {
            let mut files = lock(&self.0.files);
            let Some(tracked) = files.docs.get_mut(path) else {
                return;
            };
            tracked.writing = None;
            std::mem::take(&mut tracked.missed)
        };
        if missed {
            self.recheck(path);
        }
    }

    /// Compares `path` with its baseline again, off the UI thread.
    fn recheck(&self, path: &Path) {
        let files = self.0.files.clone();
        let path = path.to_path_buf();
        self.0
            .executor
            .spawn(async move { check(&files, &path) })
            .detach();
    }

    /// Starts the vault watcher for `index` (built), off the UI thread: it
    /// sets up its OS watches before it returns.
    pub fn start_vault(&self, index: Arc<SharedPathIndex>) {
        let events = lock(&self.0.files).events.clone();
        let vault = self.0.vault.clone();
        self.0
            .executor
            .spawn(async move {
                let sink = move |event: WatchEvent| match event {
                    // Open documents are watched by the folder watcher.
                    WatchEvent::DocumentChanged { .. } | WatchEvent::DocumentMoved { .. } => {}
                    event => {
                        let _ = events.unbounded_send(event);
                    }
                };
                match VaultWatcher::start(index, WatchOptions::default(), sink) {
                    Ok(watcher) => *lock(&vault) = Some(watcher),
                    Err(err) => tracing::warn!("vault watcher: {err}"),
                }
            })
            .detach();
    }
}

/// `path` with its folder's symlinks resolved; `None` when that changes
/// nothing or fails.
fn canonical(path: &Path) -> Option<PathBuf> {
    let folder = fs::canonicalize(path.parent()?).ok()?;
    let canonical = folder.join(path.file_name()?);
    (canonical != path).then_some(canonical)
}

/// Adds `folder` to the folder watcher, creating the watcher first.
fn watch_folder(watcher: &Mutex<Option<FolderWatcher>>, files: &Arc<Mutex<Files>>, folder: &Path) {
    let mut watcher = lock(watcher);
    if watcher.is_none() {
        let shared = files.clone();
        let made = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
            match result {
                Ok(events) => {
                    // Opening and reading a file are events too (inotify):
                    // only changes count, or every check would trigger the
                    // next.
                    let paths: BTreeSet<PathBuf> = events
                        .iter()
                        .filter(|e| !e.kind.is_access())
                        .flat_map(|e| e.paths.iter().cloned())
                        .collect();
                    for path in paths {
                        check(&shared, &path);
                    }
                }
                Err(errors) => {
                    for err in errors {
                        tracing::warn!("file watcher: {err}");
                    }
                }
            }
        });
        match made {
            Ok(made) => *watcher = Some(made),
            Err(err) => {
                tracing::warn!(
                    "file watcher: cannot start, so changes on disk are not seen: {err}"
                );
                return;
            }
        }
    }
    if let Some(watcher) = watcher.as_mut()
        && let Err(err) = watcher.watch(folder, RecursiveMode::NonRecursive)
    {
        tracing::warn!(
            "file watcher: cannot watch {}, so changes on disk are not seen: {err}",
            folder.display()
        );
    }
}

/// An event named `path`: compares an open file with its baseline, or
/// reports a sync-conflict copy of one.
fn check(files: &Mutex<Files>, path: &Path) {
    let (key, baseline) = {
        let mut guard = lock(files);
        let Some(key) = guard.key_of(path) else {
            drop(guard);
            sync_copy(files, path);
            return;
        };
        let Some(tracked) = guard.docs.get_mut(&key) else {
            return;
        };
        if tracked.writing.is_some_and(|at| at.elapsed() < WRITE_TTL) {
            tracked.missed = true;
            return;
        }
        let Some(baseline) = tracked.baseline.clone() else {
            return;
        };
        (key, baseline)
    };
    let mode = if baseline.len > THOROUGH_CHECK_BYTES {
        CheckMode::Quick
    } else {
        CheckMode::Thorough
    };
    let change = match check_external_change(&key, &baseline, mode) {
        Ok(change) => change,
        Err(err) => {
            tracing::warn!("cannot check {}: {err}", key.display());
            return;
        }
    };
    let mut guard = lock(files);
    let Some(tracked) = guard.docs.get_mut(&key) else {
        return;
    };
    // A save or another check moved the baseline meanwhile: this result
    // is stale.
    if tracked.baseline.as_ref() != Some(&baseline) || tracked.writing.is_some() {
        return;
    }
    let report = match &change {
        ExternalChange::Unchanged => false,
        ExternalChange::Deleted => !std::mem::replace(&mut tracked.deleted, true),
        ExternalChange::Touched(state) | ExternalChange::Modified { state, .. } => {
            tracked.baseline = Some(state.clone());
            tracked.deleted = false;
            true
        }
    };
    if report {
        guard.send(WatchEvent::DocumentChanged { path: key, change });
    }
}

/// Reports `path` if it is a new sync-conflict copy of an open file.
fn sync_copy(files: &Mutex<Files>, path: &Path) {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let Some(conflict) = detect_sync_conflict(name, |original| folder.join(original).is_file())
    else {
        return;
    };
    let original = folder.join(&conflict.original);
    if !path.is_file() {
        return;
    }
    let mut guard = lock(files);
    let Some(key) = guard.key_of(&original) else {
        return;
    };
    if !guard.copies.insert(path.to_path_buf()) {
        return;
    }
    guard.send(WatchEvent::SyncConflict {
        copy: path.to_path_buf(),
        original: key,
        service: conflict.service,
    });
}

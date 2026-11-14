// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The vault watcher: keeps a [`SharedPathIndex`] in step with the disk and
//! reports changes to open documents.
//!
//! [`VaultWatcher::start`] watches the index's root on a background thread
//! and delivers [`WatchEvent`]s to a callback (or a channel, with
//! [`VaultWatcher::start_channel`]). Dropping the watcher stops it and
//! joins its threads.
//!
//! # Sources of changes
//!
//! | Mode | Used for | How |
//! |---|---|---|
//! | Native | Local disks | OS notifications (inotify, FSEvents, `ReadDirectoryChangesW`) through `notify`, debounced (100 ms by default) |
//! | Polling | Network and user-space (FUSE) file systems, or when the OS watcher cannot start or runs out of watches | A walk every [`WatchOptions::poll_interval`] (the `vault.network_poll_interval_secs` setting), compared with the index by size, modification time and file identity |
//!
//! Events are only hints. Every path an event names is looked up on disk
//! again and compared with the index, so a burst of events, events in an
//! unexpected order, or an editor's save dance end up as the minimal set of
//! [`Change`]s:
//!
//! | On disk | Index change |
//! |---|---|
//! | A new file or folder | [`Change::Upsert`]; a new folder's contents are read too |
//! | Different size, modification time, identity or kind | [`Change::Upsert`] (same [`FileId`](crate::FileId)) |
//! | Gone | [`Change::Remove`] (a folder takes its subtree) |
//! | A matched rename event, or a removed and a new entry with the same file identity | [`Change::Rename`] (ids kept, folders move their subtree) |
//! | A rename onto an existing file (atomic save: temporary file renamed over the original) | [`Change::Remove`] of the temporary file and [`Change::Upsert`] of the target, which keeps its id: one modification |
//!
//! Access events (opening or reading a file: inotify reports them) are
//! always ignored, as are paths the walk excludes ([`IndexConfig`](crate::IndexConfig):
//! excluded folder names, hidden files, `.gitignore`) and the save
//! pipeline's own temporary files.
//!
//! When events may have been lost (the OS queue overflowed, the watcher
//! reported an error) the watcher rescans: a new walk compared with the
//! index, which keeps the ids of unchanged and moved entries. Rescans are
//! cheap and coalesced.
//!
//! # Open documents
//!
//! [`VaultWatcher::track`] registers an open document with its baseline
//! [`DiskState`]. Whenever the document's path is touched, the watcher
//! compares the file with the baseline ([`check_external_change`](crate::files::check_external_change),
//! hashing the bytes) and reports a [`WatchEvent::DocumentChanged`] if it
//! differs, or [`WatchEvent::DocumentMoved`] when it was moved.
//!
//! **Own saves are not external changes.** Around a save, call
//! [`VaultWatcher::begin_write`] before writing and
//! [`VaultWatcher::expect_write`] with [`WriteReport::state`](crate::files::WriteReport::state)
//! after it (or [`VaultWatcher::cancel_write`] if it failed). Changes to
//! the path that arrive in between wait; afterwards a file that still
//! matches the expected state is ours. Expectations lapse after
//! [`WatchOptions::own_write_ttl`].
//!
//! # Sync-conflict copies
//!
//! A new file whose name is a sync client's conflict copy of a file next
//! to it (Dropbox, Syncthing, OneDrive, iCloud patterns, as in
//! [`sync_conflict_copies`](crate::files::sync_conflict_copies)) is reported
//! as [`WatchEvent::SyncConflict`].

mod filter;
mod reconcile;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::files::{DiskState, ExternalChange};
use crate::index::conflict::SyncService;
use crate::index::{Change, PathIndex, SharedPathIndex, WalkStats};
use crate::path::VaultPath;

use worker::{Msg, Worker};

/// The default debounce of OS events.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(100);

/// The default polling interval (the `vault.network_poll_interval_secs`
/// setting's default).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long an expected own write is remembered by default.
pub const DEFAULT_OWN_WRITE_TTL: Duration = Duration::from_secs(5);

/// Which source of changes to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatchMode {
    /// OS notifications on local disks, polling on network and user-space
    /// file systems.
    #[default]
    Auto,
    /// OS notifications; polling only if they cannot start.
    Native,
    /// Always poll.
    Polling,
}

/// The source of changes in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveMode {
    /// OS notifications.
    Native,
    /// Polling, and why.
    Polling(PollReason),
}

/// Why the watcher polls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollReason {
    /// [`WatchMode::Polling`] was asked for.
    Forced,
    /// The root is on a network file system (its name).
    NetworkFs(&'static str),
    /// The root is on a user-space (FUSE) file system.
    UserSpaceFs,
    /// The OS watcher ran out of watches (inotify `max_user_watches`).
    WatchLimit,
    /// The OS watcher could not start (the error).
    WatcherUnavailable(String),
}

/// Watcher settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchOptions {
    /// Which source of changes to use.
    pub mode: WatchMode,
    /// How long OS events are collected before they are processed.
    pub debounce: Duration,
    /// The interval between polls in polling mode.
    pub poll_interval: Duration,
    /// How long an expected own write ([`VaultWatcher::expect_write`]) or
    /// a pending one ([`VaultWatcher::begin_write`]) is remembered.
    pub own_write_ttl: Duration,
    /// Compare the index with the disk once the watch is in place, so
    /// nothing that changed before it is missed. An index that is still
    /// [`Building`](crate::IndexState::Building) is always walked instead.
    pub startup_scan: bool,
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self {
            mode: WatchMode::Auto,
            debounce: DEFAULT_DEBOUNCE,
            poll_interval: DEFAULT_POLL_INTERVAL,
            own_write_ttl: DEFAULT_OWN_WRITE_TTL,
            startup_scan: true,
        }
    }
}

impl WatchOptions {
    /// The defaults with the polling interval of the
    /// `vault.network_poll_interval_secs` setting (at least one second).
    #[must_use]
    pub fn with_poll_interval_secs(secs: u32) -> Self {
        Self {
            poll_interval: Duration::from_secs(u64::from(secs.max(1))),
            ..Self::default()
        }
    }
}

/// What caused an index update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCause {
    /// OS events.
    Events,
    /// A poll (polling mode).
    Poll,
    /// A rescan, and why.
    Rescan(RescanReason),
    /// The comparison when the watcher started.
    Startup,
}

/// Why the watcher rescanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RescanReason {
    /// The OS dropped events (inotify queue overflow, FSEvents
    /// `MustScanSubDirs`, a `ReadDirectoryChangesW` buffer overflow).
    Overflow,
    /// The OS watcher reported an error (its message).
    WatcherError(String),
    /// [`VaultWatcher::rescan`].
    Requested,
    /// An ignore file changed.
    RulesChanged,
    /// The index changed under the watcher and the update did not apply.
    Conflict,
}

/// Something the user may need to know, reported once per watcher (except
/// [`WatchNotice::ScanFailed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchNotice {
    /// The OS dropped events; a rescan follows. Raising the queue size
    /// (`fs.inotify.max_queued_events`) avoids it.
    EventsLost,
    /// The OS watcher ran out of watches; the watcher polls from now on.
    /// Raising `fs.inotify.max_user_watches` avoids it.
    WatchLimit,
    /// The OS watcher reported an error (its message); a rescan follows.
    WatcherError(String),
    /// The OS watcher could not start (its message); the watcher polls.
    PollingFallback(String),
    /// A walk of the root failed (its message); the index was not updated.
    ScanFailed(String),
}

/// A notification from the watcher.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// The OS watch (or polling) is in place and the startup scan is done:
    /// every later change is reported.
    Ready {
        /// The source of changes.
        mode: ActiveMode,
    },
    /// The watcher switched to another source of changes.
    ModeChanged {
        /// The new source.
        mode: ActiveMode,
    },
    /// The index was still building at start and the watcher walked the
    /// root: `index` replaces it.
    IndexBuilt {
        /// The complete index, already published.
        index: PathIndex,
        /// What the walk saw.
        stats: WalkStats,
    },
    /// A new snapshot was published.
    IndexUpdated {
        /// The snapshot, already published.
        index: PathIndex,
        /// The changes from the previous snapshot, in the order applied.
        changes: Vec<Change>,
        /// What caused them.
        cause: UpdateCause,
        /// Paths among the changes that hold an expected own write.
        own_writes: Vec<VaultPath>,
    },
    /// A tracked document changed on disk. For [`ExternalChange::Touched`]
    /// adopt the new state as the baseline; it is also reported when a
    /// deleted document reappears with the same bytes.
    DocumentChanged {
        /// The document's path, as tracked.
        path: PathBuf,
        /// What changed; the watcher's baseline is now the new state.
        change: ExternalChange,
    },
    /// A tracked document was moved (by identity or a rename event); it
    /// stays tracked under its new path.
    DocumentMoved {
        /// The old path.
        from: PathBuf,
        /// The new path.
        to: PathBuf,
    },
    /// A sync client's conflict copy appeared.
    SyncConflict {
        /// The copy.
        copy: PathBuf,
        /// The file it is a copy of.
        original: PathBuf,
        /// Whose naming pattern it matches.
        service: SyncService,
    },
    /// See [`WatchNotice`].
    Notice(WatchNotice),
}

/// Why the watcher did not start.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    /// The root is not a directory, or cannot be read.
    #[error("cannot watch {}: {source}", path.display())]
    Root {
        /// The root.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
    /// The watcher thread could not be started.
    #[error("cannot start the watcher thread: {0}")]
    Thread(std::io::Error),
}

/// Watches a vault root. See the [module docs](self).
pub struct VaultWatcher {
    tx: Sender<Msg>,
    worker: Option<JoinHandle<()>>,
    mode: Arc<Mutex<ActiveMode>>,
    root: PathBuf,
}

impl std::fmt::Debug for VaultWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultWatcher")
            .field("root", &self.root)
            .field("mode", &self.mode())
            .finish_non_exhaustive()
    }
}

impl VaultWatcher {
    /// Watches the root of `index` and calls `sink` with every
    /// [`WatchEvent`], on the watcher thread. Returns once the OS watch is
    /// in place (or polling chosen); the startup scan runs afterwards and
    /// ends with [`WatchEvent::Ready`].
    pub fn start<F>(
        index: Arc<SharedPathIndex>,
        options: WatchOptions,
        sink: F,
    ) -> Result<Self, WatchError>
    where
        F: FnMut(WatchEvent) + Send + 'static,
    {
        let snapshot = index.load();
        let root = std::path::absolute(snapshot.root()).map_err(|source| WatchError::Root {
            path: snapshot.root().to_path_buf(),
            source,
        })?;
        match std::fs::metadata(&root) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                return Err(WatchError::Root {
                    path: root,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "not a directory",
                    ),
                });
            }
            Err(source) => return Err(WatchError::Root { path: root, source }),
        }
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let mode = Arc::new(Mutex::new(ActiveMode::Polling(PollReason::Forced)));
        let worker = Worker::new(
            index,
            root.clone(),
            options,
            Box::new(sink),
            rx,
            tx.clone(),
            mode.clone(),
        );
        let handle = std::thread::Builder::new()
            .name("gv-vault watcher".to_owned())
            .spawn(move || worker.run(ready_tx))
            .map_err(WatchError::Thread)?;
        // The worker reports once the watch is in place; if it died, the
        // channel is closed and there is nothing to wait for.
        let _ = ready_rx.recv();
        Ok(Self {
            tx,
            worker: Some(handle),
            mode,
            root,
        })
    }

    /// [`VaultWatcher::start`] with the events delivered to a channel.
    pub fn start_channel(
        index: Arc<SharedPathIndex>,
        options: WatchOptions,
    ) -> Result<(Self, Receiver<WatchEvent>), WatchError> {
        let (tx, rx) = mpsc::channel();
        let watcher = Self::start(index, options, move |event| {
            // The receiver is gone only when nobody listens any more.
            let _ = tx.send(event);
        })?;
        Ok((watcher, rx))
    }

    /// The watched root (absolute).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The source of changes in use.
    #[must_use]
    pub fn mode(&self) -> ActiveMode {
        self.mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Tracks an open document whose file is `path` (inside the root),
    /// with `baseline` as its state on disk. Tracking it again replaces the
    /// baseline (after a reload, or when adopting a touched state).
    pub fn track(&self, path: impl Into<PathBuf>, baseline: DiskState) {
        self.send(Msg::Track {
            path: path.into(),
            baseline,
        });
    }

    /// Stops tracking the document at `path`.
    pub fn untrack(&self, path: impl Into<PathBuf>) {
        self.send(Msg::Untrack(path.into()));
    }

    /// Announces that the app is about to write `path`: changes to it wait
    /// until [`expect_write`](Self::expect_write) or
    /// [`cancel_write`](Self::cancel_write) (or the TTL).
    pub fn begin_write(&self, path: impl Into<PathBuf>) {
        self.send(Msg::BeginWrite(path.into()));
    }

    /// Records the app's own write of `path`, which left it in `state`
    /// ([`WriteReport::state`](crate::files::WriteReport::state)). Until the
    /// TTL, events that find the file in that state are not external
    /// changes, and a tracked document's baseline becomes `state`.
    pub fn expect_write(&self, path: impl Into<PathBuf>, state: DiskState) {
        self.send(Msg::ExpectWrite {
            path: path.into(),
            state,
        });
    }

    /// The write announced by [`begin_write`](Self::begin_write) did not
    /// happen (it failed).
    pub fn cancel_write(&self, path: impl Into<PathBuf>) {
        self.send(Msg::CancelWrite(path.into()));
    }

    /// Compares the whole root with the index (for example when the app
    /// regains focus after the machine slept).
    pub fn rescan(&self) {
        self.send(Msg::Rescan(RescanReason::Requested));
    }

    /// Feeds an OS queue overflow through the event path, as the OS would
    /// report it. For tests.
    #[doc(hidden)]
    pub fn inject_overflow(&self) {
        self.send(Msg::InjectOverflow);
    }

    /// Feeds an OS watcher error through the event path. For tests.
    #[doc(hidden)]
    pub fn inject_error(&self, message: &str) {
        self.send(Msg::InjectError(message.to_owned()));
    }

    fn send(&self, msg: Msg) {
        // The worker only exits when the watcher is dropped.
        let _ = self.tx.send(msg);
    }
}

impl Drop for VaultWatcher {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

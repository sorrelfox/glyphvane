// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Keeping a vault's Git status current on a background thread.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::{
    self, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _,
};

use super::{GitStatus, read_status};

/// What a [`GitWatcher`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitEvent {
    /// A new snapshot, different from the last one.
    Updated(Arc<GitStatus>),
    /// The vault is no longer in a work tree (the repository was removed).
    Gone,
    /// Reading the status failed; the last snapshot stays.
    Failed(String),
}

/// How a [`GitWatcher`] waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitWatchOptions {
    /// How long a burst of changes must be quiet before the status is
    /// read again.
    pub debounce: Duration,
    /// The longest a burst may delay a read.
    pub max_delay: Duration,
    /// Watch the repository's folder (`HEAD`, the index) for commits,
    /// checkouts and staging done outside the app.
    pub watch_git_dir: bool,
}

impl Default for GitWatchOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(400),
            max_delay: Duration::from_secs(3),
            watch_git_dir: true,
        }
    }
}

enum Msg {
    Refresh,
    Stop,
}

/// Reads a vault's Git status on a background thread, again after each
/// burst of changes. Dropping it stops the thread.
pub struct GitWatcher {
    tx: Sender<Msg>,
    latest: Arc<Mutex<Option<Arc<GitStatus>>>>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    _git_dir: Option<RecommendedWatcher>,
}

impl std::fmt::Debug for GitWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitWatcher").finish_non_exhaustive()
    }
}

impl GitWatcher {
    /// Starts watching the Git status of the vault at `vault_root`,
    /// calling `on_event` on the watcher's thread. `None` when the vault
    /// is not in a Git work tree (nothing starts).
    pub fn start<F>(vault_root: &Path, options: GitWatchOptions, on_event: F) -> Option<Self>
    where
        F: Fn(GitEvent) + Send + 'static,
    {
        let (work_tree, _) = super::find_work_tree(vault_root)?;
        let git_dir = gix::discover(&work_tree)
            .ok()
            .map(|r| r.git_dir().to_path_buf());
        let (tx, rx) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let watcher = git_dir
            .filter(|_| options.watch_git_dir)
            .and_then(|dir| watch_git_dir(&dir, tx.clone()));
        let thread = std::thread::Builder::new()
            .name("gv-git-status".into())
            .spawn({
                let root = vault_root.to_path_buf();
                let latest = latest.clone();
                let cancel = cancel.clone();
                move || run(&root, options, &rx, &latest, &cancel, &on_event)
            })
            .ok()?;
        Some(Self {
            tx,
            latest,
            cancel,
            thread: Some(thread),
            _git_dir: watcher,
        })
    }

    /// Like [`Self::start`], delivering the events to a channel.
    #[must_use]
    pub fn start_channel(
        vault_root: &Path,
        options: GitWatchOptions,
    ) -> Option<(Self, Receiver<GitEvent>)> {
        let (tx, rx) = mpsc::channel();
        let watcher = Self::start(vault_root, options, move |e| {
            let _ = tx.send(e);
        })?;
        Some((watcher, rx))
    }

    /// Something in the vault changed: read the status again once changes
    /// settle. Cheap; call it for every vault watcher event.
    pub fn refresh(&self) {
        let _ = self.tx.send(Msg::Refresh);
    }

    /// The last snapshot read, if any.
    #[must_use]
    pub fn snapshot(&self) -> Option<Arc<GitStatus>> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for GitWatcher {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Watches the repository's own folder (not recursively): `HEAD`, the
/// index and the like change on commits, checkouts and staging.
fn watch_git_dir(dir: &Path, tx: Sender<Msg>) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let relevant = match res {
            Ok(event) => !matches!(event.kind, EventKind::Access(_)),
            Err(_) => true,
        };
        if relevant {
            let _ = tx.send(Msg::Refresh);
        }
    })
    .ok()?;
    watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(watcher)
}

fn run(
    root: &Path,
    options: GitWatchOptions,
    rx: &Receiver<Msg>,
    latest: &Mutex<Option<Arc<GitStatus>>>,
    cancel: &Arc<AtomicBool>,
    on_event: &dyn Fn(GitEvent),
) {
    let mut last: Option<Arc<GitStatus>> = None;
    let mut gone = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        match read_status(root, cancel) {
            Ok(Some(status)) => {
                gone = false;
                if last.as_deref() != Some(&status) {
                    let status = Arc::new(status);
                    *latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(status.clone());
                    last = Some(status.clone());
                    on_event(GitEvent::Updated(status));
                }
            }
            Ok(None) => {
                if !gone {
                    gone = true;
                    last = None;
                    *latest.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    on_event(GitEvent::Gone);
                }
            }
            Err(e) => {
                if !cancel.load(Ordering::Acquire) {
                    on_event(GitEvent::Failed(e.0));
                }
            }
        }
        // Wait for a change, then for the burst to settle.
        match rx.recv() {
            Ok(Msg::Refresh) => {}
            Ok(Msg::Stop) | Err(_) => return,
        }
        let started = Instant::now();
        loop {
            let left = options.max_delay.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(options.debounce.min(left)) {
                Ok(Msg::Refresh) => {}
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
    }
}

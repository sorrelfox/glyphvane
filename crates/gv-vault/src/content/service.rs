// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The content index service: one background thread per vault.
//!
//! [`ContentIndexer::start`] spawns the thread. It waits for a complete
//! path index, loads the cache (when it is valid for this profile and
//! root), reads and parses what changed since, resolves the links and
//! publishes the first [`ContentSnapshot`] ([`ContentEvent::Ready`]).
//! From then on it applies, in order:
//!
//! - newer path-index snapshots ([`ContentIndexer::update_paths`]): only
//!   the newest of those queued is used;
//! - overlay changes ([`ContentIndexer::set_overlay`],
//!   [`ContentIndexer::clear_overlay`]), collected for
//!   [`ContentOptions::overlay_debounce`] so typing does not reparse on
//!   every keystroke.
//!
//! Each update publishes a new snapshot ([`ContentIndexer::snapshot`],
//! lock-free) and reports [`ContentEvent::Updated`]. The cache is written
//! after the first build and then at most every
//! [`ContentOptions::persist_interval`] while records change.
//!
//! **Cancellation.** Dropping the indexer (the window's vault closed or
//! switched) cancels the work in progress at the next file and ends the
//! thread without waiting for it; the cache keeps its last written state,
//! which the next start validates file by file. [`ContentIndexer::stop`]
//! instead finishes the queue, writes the cache and waits.

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use arc_swap::ArcSwapOption;
use gv_text::TextSnapshot;

use super::query::ContentSnapshot;
use super::state::{Cancelled, RACY_WINDOW, State, UpdateReport, system_time_ns};
use super::store::{self, Header, LoadError};
use super::{ContentOptions, DiskSource, FileSource};
use crate::index::{IndexState, PathIndex};
use crate::path::VaultPath;

/// Whether the cache was used for the first build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheUse {
    /// No cache file is configured.
    Disabled,
    /// Loaded with this many records.
    Loaded(usize),
    /// Not used (missing, damaged, another version, stale), and why.
    NotUsed(String),
}

/// What the first build did.
#[derive(Debug, Clone)]
pub struct ColdStats {
    /// The cache.
    pub cache: CacheUse,
    /// Notes in the index.
    pub notes: usize,
    /// Files read.
    pub read: usize,
    /// Notes parsed.
    pub parsed: usize,
    /// Loading and decoding the cache.
    pub load: Duration,
    /// Comparing, reading and parsing files.
    pub read_time: Duration,
    /// Resolving every link.
    pub resolve: Duration,
    /// From the complete path index to the published snapshot.
    pub elapsed: Duration,
}

/// What the indexer reports (on its own thread).
#[derive(Debug, Clone)]
pub enum ContentEvent {
    /// The first snapshot is published.
    Ready {
        /// The snapshot.
        snapshot: ContentSnapshot,
        /// What the build did.
        stats: ColdStats,
    },
    /// A newer snapshot is published.
    Updated {
        /// The snapshot.
        snapshot: ContentSnapshot,
        /// What changed.
        report: UpdateReport,
        /// How long the update took.
        elapsed: Duration,
    },
    /// The cache was written.
    Persisted {
        /// Its size.
        bytes: u64,
        /// How long writing took (encoding included).
        elapsed: Duration,
    },
    /// The cache could not be written (the error).
    PersistFailed(String),
}

enum Cmd {
    Paths(PathIndex),
    Overlay(VaultPath, Option<TextSnapshot>),
    Flush(Sender<()>),
    Persist,
    Stop,
}

struct Shared {
    snapshot: ArcSwapOption<State>,
    cancel: AtomicBool,
}

/// A cheap, cloneable sender to a running indexer (for whoever produces
/// path-index snapshots or overlays). Sending to a stopped indexer does
/// nothing.
#[derive(Clone)]
pub struct ContentHandle {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ContentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentHandle")
            .field("cancelled", &self.shared.cancel.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ContentHandle {
    /// A newer path-index snapshot.
    pub fn update_paths(&self, paths: PathIndex) {
        let _ = self.tx.send(Cmd::Paths(paths));
    }

    /// The unsaved text of the note at `path`.
    pub fn set_overlay(&self, path: VaultPath, text: TextSnapshot) {
        let _ = self.tx.send(Cmd::Overlay(path, Some(text)));
    }

    /// The note at `path` has no unsaved changes any more (saved,
    /// reverted or closed).
    pub fn clear_overlay(&self, path: VaultPath) {
        let _ = self.tx.send(Cmd::Overlay(path, None));
    }

    /// The current snapshot, once the first build is done.
    #[must_use]
    pub fn snapshot(&self) -> Option<ContentSnapshot> {
        self.shared.snapshot.load_full().map(ContentSnapshot)
    }

    /// Whether the indexer was stopped or cancelled.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }
}

/// The content index of one vault. See the [module docs](self).
pub struct ContentIndexer {
    handle: ContentHandle,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ContentIndexer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentIndexer")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl ContentIndexer {
    /// Starts indexing the notes of `paths`' root, read from disk.
    /// `paths` may still be building: the index waits for a complete one.
    pub fn start(
        paths: PathIndex,
        options: ContentOptions,
        on_event: impl Fn(ContentEvent) + Send + 'static,
    ) -> io::Result<Self> {
        let source = DiskSource::new(paths.root(), &options);
        Self::start_with_source(paths, options, Arc::new(source), on_event)
    }

    /// [`start`](Self::start) with another file source (tests).
    pub fn start_with_source(
        paths: PathIndex,
        options: ContentOptions,
        source: Arc<dyn FileSource>,
        on_event: impl Fn(ContentEvent) + Send + 'static,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            snapshot: ArcSwapOption::empty(),
            cancel: AtomicBool::new(false),
        });
        let worker = Worker {
            rx,
            shared: shared.clone(),
            options: Arc::new(options),
            source,
            on_event: Box::new(on_event),
        };
        let thread = std::thread::Builder::new()
            .name("content-index".into())
            .spawn(move || worker.run(paths))?;
        Ok(Self {
            handle: ContentHandle { tx, shared },
            thread: Some(thread),
        })
    }

    /// A sender for producers on other threads.
    #[must_use]
    pub fn handle(&self) -> ContentHandle {
        self.handle.clone()
    }

    /// See [`ContentHandle::update_paths`].
    pub fn update_paths(&self, paths: PathIndex) {
        self.handle.update_paths(paths);
    }

    /// See [`ContentHandle::set_overlay`].
    pub fn set_overlay(&self, path: VaultPath, text: TextSnapshot) {
        self.handle.set_overlay(path, text);
    }

    /// See [`ContentHandle::clear_overlay`].
    pub fn clear_overlay(&self, path: VaultPath) {
        self.handle.clear_overlay(path);
    }

    /// See [`ContentHandle::snapshot`].
    #[must_use]
    pub fn snapshot(&self) -> Option<ContentSnapshot> {
        self.handle.snapshot()
    }

    /// Writes the cache soon, if anything changed since it was written.
    pub fn persist(&self) {
        let _ = self.handle.tx.send(Cmd::Persist);
    }

    /// Waits until everything sent before is indexed (pending overlays
    /// included), or `timeout` passes. Returns `false` on timeout or when
    /// the indexer is gone. For tests and orderly shutdown; never call it
    /// on the UI thread.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (tx, rx) = mpsc::channel();
        if self.handle.tx.send(Cmd::Flush(tx)).is_err() {
            return false;
        }
        rx.recv_timeout(timeout).is_ok()
    }

    /// Finishes the queue, writes the cache if it changed, and waits for
    /// the thread.
    pub fn stop(mut self) {
        let _ = self.handle.tx.send(Cmd::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ContentIndexer {
    fn drop(&mut self) {
        if self.thread.take().is_some() {
            // Cancel and let the thread end on its own.
            self.handle.shared.cancel.store(true, Ordering::Relaxed);
            let _ = self.handle.tx.send(Cmd::Stop);
        }
    }
}

struct Worker {
    rx: Receiver<Cmd>,
    shared: Arc<Shared>,
    options: Arc<ContentOptions>,
    source: Arc<dyn FileSource>,
    on_event: Box<dyn Fn(ContentEvent) + Send>,
}

#[derive(Default)]
struct Pending {
    paths: Option<PathIndex>,
    overlays: BTreeMap<VaultPath, Option<TextSnapshot>>,
    overlay_due: Option<Instant>,
    flushes: Vec<Sender<()>>,
    persist: bool,
    stop: bool,
}

impl Pending {
    fn take(&mut self, cmd: Cmd, debounce: Duration) {
        match cmd {
            Cmd::Paths(p) => {
                let newer = self
                    .paths
                    .as_ref()
                    .is_none_or(|old| p.generation() >= old.generation() || p.root() != old.root());
                if newer {
                    self.paths = Some(p);
                }
            }
            Cmd::Overlay(path, text) => {
                self.overlays.insert(path, text);
                self.overlay_due = Some(Instant::now() + debounce);
            }
            Cmd::Flush(tx) => self.flushes.push(tx),
            Cmd::Persist => self.persist = true,
            Cmd::Stop => self.stop = true,
        }
    }
}

impl Worker {
    fn cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }

    fn run(self, initial: PathIndex) {
        let debounce = self.options.overlay_debounce;
        let mut pending = Pending {
            paths: Some(initial),
            ..Pending::default()
        };
        let mut state: Option<State> = None;
        // Records changed since the cache was written.
        let mut dirty_since: Option<Instant> = None;
        let mut first = true;
        loop {
            if !first {
                let deadline = [
                    pending.overlay_due,
                    dirty_since.map(|t| t + self.options.persist_interval),
                ]
                .into_iter()
                .flatten()
                .min();
                let msg = match deadline {
                    Some(d) => match self
                        .rx
                        .recv_timeout(d.saturating_duration_since(Instant::now()))
                    {
                        Ok(m) => Some(m),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => Some(Cmd::Stop),
                    },
                    None => Some(self.rx.recv().unwrap_or(Cmd::Stop)),
                };
                if let Some(m) = msg {
                    pending.take(m, debounce);
                }
            }
            first = false;
            while let Ok(m) = self.rx.try_recv() {
                pending.take(m, debounce);
            }
            if self.cancelled() {
                self.shared.cancel.store(true, Ordering::Relaxed);
                return;
            }

            let urgent = !pending.flushes.is_empty() || pending.stop;
            let overlays_due = !pending.overlays.is_empty()
                && (urgent || pending.overlay_due.is_some_and(|d| d <= Instant::now()));
            let result = match &mut state {
                None => match pending.paths.take() {
                    Some(paths) if paths.state() == IndexState::Complete => {
                        let overlays = take_overlays(&mut pending);
                        match self.cold(paths, overlays) {
                            Ok((s, changed)) => {
                                state = Some(s);
                                if changed {
                                    dirty_since = Some(Instant::now());
                                    pending.persist = true;
                                }
                                Ok(())
                            }
                            Err(c) => Err(c),
                        }
                    }
                    other => {
                        pending.paths = other;
                        Ok(())
                    }
                },
                Some(s) => {
                    let paths = pending.paths.take();
                    if paths.is_some() || overlays_due {
                        let overlays = if overlays_due {
                            take_overlays(&mut pending)
                        } else {
                            Vec::new()
                        };
                        let started = Instant::now();
                        match s.apply(paths, overlays, &*self.source, &self.shared.cancel) {
                            Ok(report) => {
                                if report.read > 0 || report.removed > 0 {
                                    dirty_since.get_or_insert_with(Instant::now);
                                }
                                self.publish(s);
                                (self.on_event)(ContentEvent::Updated {
                                    snapshot: ContentSnapshot(Arc::new(s.clone())),
                                    report,
                                    elapsed: started.elapsed(),
                                });
                                Ok(())
                            }
                            Err(c) => Err(c),
                        }
                    } else {
                        Ok(())
                    }
                }
            };
            if result.is_err() || self.cancelled() {
                return;
            }
            for tx in pending.flushes.drain(..) {
                let _ = tx.send(());
            }
            if let Some(s) = &state {
                let due = dirty_since.is_some_and(|t| t.elapsed() >= self.options.persist_interval);
                if dirty_since.is_some() && (pending.persist || due || pending.stop) {
                    self.persist(s);
                    dirty_since = None;
                }
            }
            pending.persist = false;
            if pending.stop {
                // Graceful stop: the queue is done.
                self.shared.cancel.store(true, Ordering::Relaxed);
                return;
            }
        }
    }

    /// The first build. Returns the state and whether it differs from the
    /// cache.
    fn cold(
        &self,
        paths: PathIndex,
        overlays: Vec<(VaultPath, Option<String>)>,
    ) -> Result<(State, bool), Cancelled> {
        let started = Instant::now();
        let root = paths.root().to_string_lossy().into_owned();
        let load_started = Instant::now();
        let fingerprint = store::graph_fingerprint(&self.options.params, paths.config());
        let (mut state, cache) = match &self.options.cache_file {
            None => (State::new(self.options.clone()), CacheUse::Disabled),
            Some(file) => match store::load(file, self.options.profile, &root, fingerprint) {
                Ok(store::Loaded {
                    header,
                    mut records,
                    graph,
                }) => {
                    // Files written in the same clock tick as the cache
                    // may have changed after it: compare their bytes.
                    let racy = i64::try_from(RACY_WINDOW.as_nanos()).unwrap_or(i64::MAX);
                    for r in &mut records {
                        if r.stamp
                            .mtime_ns
                            .is_none_or(|m| m >= header.written_at - racy)
                        {
                            r.stamp.doubt = true;
                        }
                    }
                    let n = records.len();
                    let mut state = State::with_records(self.options.clone(), records);
                    if let Some(graph) = graph
                        && let Some(previous) =
                            store::previous_index(paths.root(), paths.config(), &graph.entries)
                    {
                        state.adopt_graph(previous, graph.resolved);
                    }
                    (state, CacheUse::Loaded(n))
                }
                Err(e) => {
                    if !matches!(e, LoadError::Missing) {
                        let _ = std::fs::remove_file(file);
                    }
                    (
                        State::new(self.options.clone()),
                        CacheUse::NotUsed(e.to_string()),
                    )
                }
            },
        };
        let load = load_started.elapsed();
        let report = state.apply(Some(paths), overlays, &*self.source, &self.shared.cancel)?;
        let changed =
            !matches!(cache, CacheUse::Loaded(_)) || report.read > 0 || report.removed > 0;
        self.publish(&state);
        (self.on_event)(ContentEvent::Ready {
            snapshot: ContentSnapshot(Arc::new(state.clone())),
            stats: ColdStats {
                cache,
                notes: ContentSnapshot(Arc::new(state.clone())).note_count(),
                read: report.read,
                parsed: report.parsed,
                load,
                read_time: report.read_time,
                resolve: report.resolve_time,
                elapsed: started.elapsed(),
            },
        });
        Ok((state, changed))
    }

    fn publish(&self, state: &State) {
        self.shared.snapshot.store(Some(Arc::new(state.clone())));
    }

    fn persist(&self, state: &State) {
        let Some(file) = &self.options.cache_file else {
            return;
        };
        let Some(paths) = &state.paths else {
            return;
        };
        let started = Instant::now();
        state
            .strings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .purge();
        let header = Header {
            profile: self.options.profile,
            root: paths.root().to_string_lossy().into_owned(),
            written_at: system_time_ns(SystemTime::now()),
            fingerprint: store::graph_fingerprint(&self.options.params, paths.config()),
        };
        match store::save(file, &header, state) {
            Ok(bytes) => (self.on_event)(ContentEvent::Persisted {
                bytes,
                elapsed: started.elapsed(),
            }),
            Err(e) => (self.on_event)(ContentEvent::PersistFailed(e.to_string())),
        }
    }
}

fn take_overlays(pending: &mut Pending) -> Vec<(VaultPath, Option<String>)> {
    pending.overlay_due = None;
    std::mem::take(&mut pending.overlays)
        .into_iter()
        .map(|(p, t)| (p, t.map(|t| t.to_string())))
        .collect()
}

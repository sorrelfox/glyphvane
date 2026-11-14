// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The watcher thread: owns the OS watcher (or the poll timer), the tracked
//! documents and the expected own writes, and publishes index snapshots.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::mem::{Discriminant, discriminant};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use notify_debouncer_full::notify::event::{Flag, ModifyKind, RenameMode};
use notify_debouncer_full::notify::{
    self, ErrorKind, Event, EventKind, RecommendedWatcher, RecursiveMode,
};
use notify_debouncer_full::{
    DebounceEventResult, DebouncedEvent, Debouncer, RecommendedCache, new_debouncer_opt,
};

use super::filter::PathFilter;
use super::reconcile::{self, Disk};
use super::{
    ActiveMode, PollReason, RescanReason, UpdateCause, WatchEvent, WatchMode, WatchNotice,
    WatchOptions,
};
use crate::files::{
    CheckMode, DiskState, ExternalChange, FileIdentity, FsKind, check_external_change,
    filesystem_kind,
};
use crate::index::conflict::detect_sync_conflict;
use crate::index::{Change, FileMeta, IndexState, PathIndex, SharedPathIndex, entry_of};
use crate::path::VaultPath;

/// How long rescan requests are collected before the rescan runs.
const RESCAN_DELAY: Duration = Duration::from_millis(50);

/// Messages to the watcher thread.
pub(super) enum Msg {
    Notify(DebounceEventResult),
    Track { path: PathBuf, baseline: DiskState },
    Untrack(PathBuf),
    BeginWrite(PathBuf),
    ExpectWrite { path: PathBuf, state: DiskState },
    CancelWrite(PathBuf),
    Rescan(RescanReason),
    InjectOverflow,
    InjectError(String),
    Shutdown,
}

/// An open document.
struct Tracked {
    /// The path it was tracked with.
    path: PathBuf,
    /// Its state on disk as last seen.
    baseline: DiskState,
    /// Reported deleted and not seen since.
    deleted: bool,
}

/// An own write that was announced.
struct Expected {
    state: DiskState,
    until: Instant,
}

type Sink = Box<dyn FnMut(WatchEvent) + Send>;

pub(super) struct Worker {
    shared: Arc<SharedPathIndex>,
    root: PathBuf,
    canonical_root: Option<PathBuf>,
    options: WatchOptions,
    sink: Sink,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    mode: Arc<Mutex<ActiveMode>>,
    filter: PathFilter,
    native: Option<Debouncer<RecommendedWatcher, RecommendedCache>>,
    polling: bool,
    next_poll: Option<Instant>,
    rescan: Option<(Instant, RescanReason)>,
    docs: HashMap<VaultPath, Tracked>,
    expected: HashMap<VaultPath, Expected>,
    pending: HashMap<VaultPath, Instant>,
    deferred: HashSet<VaultPath>,
    notices: HashSet<Discriminant<WatchNotice>>,
}

impl Worker {
    pub(super) fn new(
        shared: Arc<SharedPathIndex>,
        root: PathBuf,
        options: WatchOptions,
        sink: Sink,
        rx: Receiver<Msg>,
        tx: Sender<Msg>,
        mode: Arc<Mutex<ActiveMode>>,
    ) -> Self {
        let filter = PathFilter::new(&root, shared.load().config());
        Self {
            canonical_root: fs::canonicalize(&root).ok(),
            shared,
            root,
            options,
            sink,
            rx,
            tx,
            mode,
            filter,
            native: None,
            polling: false,
            next_poll: None,
            rescan: None,
            docs: HashMap::new(),
            expected: HashMap::new(),
            pending: HashMap::new(),
            deferred: HashSet::new(),
            notices: HashSet::new(),
        }
    }

    /// The thread body. `ready` is signalled once the watch is in place.
    pub(super) fn run(mut self, ready: SyncSender<()>) {
        self.choose_mode();
        let _ = ready.send(());
        drop(ready);
        self.startup();
        let mode = self.current_mode();
        self.emit(WatchEvent::Ready { mode });
        loop {
            let msg = match self.next_deadline() {
                Some(at) => self
                    .rx
                    .recv_timeout(at.saturating_duration_since(Instant::now())),
                None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match msg {
                Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.run_due();
        }
        if let Some(debouncer) = self.native.take() {
            // Joins the debouncer thread.
            debouncer.stop();
        }
    }

    // ----- Modes -----

    fn choose_mode(&mut self) {
        let poll = match self.options.mode {
            WatchMode::Polling => Some(PollReason::Forced),
            WatchMode::Native => None,
            WatchMode::Auto => match filesystem_kind(&self.root) {
                FsKind::Network(name) => Some(PollReason::NetworkFs(name)),
                FsKind::UserSpace => Some(PollReason::UserSpaceFs),
                FsKind::Local | FsKind::Unknown => None,
            },
        };
        if let Some(reason) = poll {
            self.set_polling(reason, false);
            return;
        }
        match self.start_native() {
            Ok(debouncer) => {
                self.native = Some(debouncer);
                self.set_mode(ActiveMode::Native);
            }
            Err(err) if matches!(err.kind, ErrorKind::MaxFilesWatch) => {
                self.notice_once(WatchNotice::WatchLimit);
                self.set_polling(PollReason::WatchLimit, false);
            }
            Err(err) => {
                let message = err.to_string();
                self.notice_once(WatchNotice::PollingFallback(message.clone()));
                self.set_polling(PollReason::WatcherUnavailable(message), false);
            }
        }
    }

    fn start_native(&self) -> notify::Result<Debouncer<RecommendedWatcher, RecommendedCache>> {
        let tx = self.tx.clone();
        let config = notify::Config::default().with_follow_symlinks(false);
        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, RecommendedCache>(
            self.options.debounce,
            None,
            move |result: DebounceEventResult| {
                // Closed only while the watcher shuts down.
                let _ = tx.send(Msg::Notify(result));
            },
            RecommendedCache::new(),
            config,
        )?;
        debouncer.watch(&self.root, RecursiveMode::Recursive)?;
        Ok(debouncer)
    }

    fn set_polling(&mut self, reason: PollReason, announce: bool) {
        if let Some(debouncer) = self.native.take() {
            debouncer.stop();
        }
        self.polling = true;
        self.next_poll = Some(Instant::now() + self.options.poll_interval);
        let mode = ActiveMode::Polling(reason);
        self.set_mode(mode.clone());
        if announce {
            self.emit(WatchEvent::ModeChanged { mode });
        }
    }

    fn set_mode(&self, mode: ActiveMode) {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    fn current_mode(&self) -> ActiveMode {
        self.mode
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    // ----- Loop -----

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.rescan.as_ref().map(|(at, _)| *at),
            self.next_poll,
            self.pending.values().min().copied(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn run_due(&mut self) {
        let now = Instant::now();
        let expired: Vec<VaultPath> = self
            .pending
            .iter()
            .filter(|(_, until)| **until <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for key in expired {
            self.pending.remove(&key);
            if self.deferred.remove(&key) {
                self.check_doc(&key);
            }
        }
        self.expected.retain(|_, e| e.until > now);
        if let Some((at, _)) = &self.rescan
            && *at <= now
            && let Some((_, reason)) = self.rescan.take()
        {
            self.rescan_now(UpdateCause::Rescan(reason));
            if self.polling {
                self.next_poll = Some(Instant::now() + self.options.poll_interval);
            }
        }
        if self.next_poll.is_some_and(|at| at <= Instant::now()) {
            self.rescan_now(UpdateCause::Poll);
            self.next_poll = Some(Instant::now() + self.options.poll_interval);
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Notify(result) => self.on_notify(result),
            Msg::Track { path, baseline } => {
                let Some(key) = self.key_of(&path) else {
                    return;
                };
                self.docs.insert(
                    key.clone(),
                    Tracked {
                        path,
                        baseline,
                        deleted: false,
                    },
                );
                // Catches a change between loading the file and tracking it.
                // A missing file is left to the events: it may have been
                // moved, and the move is reported once its event arrives.
                self.check_doc_with(&key, false);
            }
            Msg::Untrack(path) => {
                if let Some(key) = self.key_of(&path) {
                    self.docs.remove(&key);
                    self.deferred.remove(&key);
                }
            }
            Msg::BeginWrite(path) => {
                if let Some(key) = self.key_of(&path) {
                    self.pending
                        .insert(key, Instant::now() + self.options.own_write_ttl);
                }
            }
            Msg::ExpectWrite { path, state } => {
                let Some(key) = self.key_of(&path) else {
                    return;
                };
                self.pending.remove(&key);
                if let Some(doc) = self.docs.get_mut(&key) {
                    doc.baseline = state.clone();
                    doc.deleted = false;
                }
                self.expected.insert(
                    key.clone(),
                    Expected {
                        state,
                        until: Instant::now() + self.options.own_write_ttl,
                    },
                );
                if self.deferred.remove(&key) {
                    self.check_doc(&key);
                }
            }
            Msg::CancelWrite(path) => {
                if let Some(key) = self.key_of(&path) {
                    self.pending.remove(&key);
                    if self.deferred.remove(&key) {
                        self.check_doc(&key);
                    }
                }
            }
            Msg::Rescan(reason) => self.schedule_rescan(reason),
            Msg::InjectOverflow => {
                let event = Event::new(EventKind::Other).set_flag(Flag::Rescan);
                self.on_notify(Ok(vec![DebouncedEvent::new(event, Instant::now())]));
            }
            Msg::InjectError(message) => {
                self.on_notify(Err(vec![notify::Error::generic(&message)]));
            }
            Msg::Shutdown => {}
        }
    }

    fn schedule_rescan(&mut self, reason: RescanReason) {
        if self.rescan.is_none() {
            self.rescan = Some((Instant::now() + RESCAN_DELAY, reason));
        }
    }

    fn emit(&mut self, event: WatchEvent) {
        (self.sink)(event);
    }

    fn notice_once(&mut self, notice: WatchNotice) {
        if self.notices.insert(discriminant(&notice)) {
            self.emit(WatchEvent::Notice(notice));
        }
    }

    // ----- Events -----

    fn on_notify(&mut self, result: DebounceEventResult) {
        match result {
            Ok(events) => {
                let mut renames = Vec::new();
                let mut hints = Vec::new();
                for event in events {
                    // Opening or reading a file is an event too (inotify
                    // reports opens); only changes count.
                    if event.kind.is_access() {
                        continue;
                    }
                    if event.need_rescan() {
                        self.notice_once(WatchNotice::EventsLost);
                        self.schedule_rescan(RescanReason::Overflow);
                        continue;
                    }
                    match event.kind {
                        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
                            if event.paths.len() == 2 =>
                        {
                            renames.push((event.paths[0].clone(), event.paths[1].clone()));
                        }
                        EventKind::Other => {}
                        _ => hints.extend(event.event.paths),
                    }
                }
                if !self.polling && (!renames.is_empty() || !hints.is_empty()) {
                    self.apply_hints(&renames, &hints);
                }
            }
            Err(errors) => {
                for err in errors {
                    if matches!(err.kind, ErrorKind::MaxFilesWatch) {
                        self.notice_once(WatchNotice::WatchLimit);
                        if !self.polling {
                            self.set_polling(PollReason::WatchLimit, true);
                            self.schedule_rescan(RescanReason::WatcherError(err.to_string()));
                        }
                    } else {
                        let message = err.to_string();
                        self.notice_once(WatchNotice::WatcherError(message.clone()));
                        self.schedule_rescan(RescanReason::WatcherError(message));
                    }
                }
            }
        }
    }

    /// Looks up every path the events name and publishes the differences.
    fn apply_hints(&mut self, renames: &[(PathBuf, PathBuf)], hints: &[PathBuf]) {
        let base = self.shared.load();
        let mut disk = Disk::new();
        let mut confirmed = Vec::new();
        let mut todo = Vec::new();
        for (from, to) in renames {
            let (from, to) = (self.rel(from), self.rel(to));
            if let (Some(f), Some(t)) = (&from, &to)
                && self.is_rename(&base, f, t)
            {
                confirmed.push((f.clone(), t.clone()));
            }
            todo.extend(from);
            todo.extend(to);
        }
        todo.extend(hints.iter().filter_map(|h| self.rel(h)));
        let mut rules_changed = false;
        for path in &todo {
            if self.filter.uses_gitignore() && path.file_name() == Some(".gitignore") {
                rules_changed = true;
            }
            self.look(&base, path, true, &mut disk);
        }
        if rules_changed {
            self.filter.reload_rules();
            self.schedule_rescan(RescanReason::RulesChanged);
        }
        self.publish(&base, &confirmed, &disk, UpdateCause::Events);
    }

    /// Whether a rename event from `from` to `to` is a move of the indexed
    /// entry: the target exists, and the source is gone or is now another
    /// file (an editor recreated it).
    fn is_rename(&mut self, base: &PathIndex, from: &VaultPath, to: &VaultPath) -> bool {
        let Some(entry) = base.entry_by_path(from.as_str()) else {
            return false;
        };
        let Ok((to_kind, to_meta)) = entry_of(&to.to_fs_path(&self.root), self.canonical()) else {
            return false;
        };
        if self.filter.excluded(to, to_kind.is_dir()) {
            return false;
        }
        match entry_of(&from.to_fs_path(&self.root), self.canonical()) {
            Err(e) => e.kind() == io::ErrorKind::NotFound,
            Ok((_, from_meta)) => {
                entry.meta.identity.is_some()
                    && from_meta.identity != entry.meta.identity
                    && to_meta.identity == entry.meta.identity
            }
        }
    }

    /// Records the disk state of `path` in `disk`: its entry (and, for a
    /// folder new to the index, everything below it), missing ancestors,
    /// and with `parent` its parent folder (whose modification time
    /// changes with its entries).
    fn look(&mut self, base: &PathIndex, path: &VaultPath, parent: bool, disk: &mut Disk) {
        if path.is_root() || disk.contains_key(path) {
            return;
        }
        let abs = path.to_fs_path(&self.root);
        match entry_of(&abs, self.canonical()) {
            Ok((kind, meta)) => {
                if self.filter.excluded(path, kind.is_dir()) {
                    return;
                }
                let new_dir = kind.is_dir()
                    && !base
                        .entry_by_path(path.as_str())
                        .is_some_and(|e| e.kind.is_dir());
                disk.insert(path.clone(), Some((kind, meta)));
                let mut ancestor = path.parent();
                while let Some(a) = ancestor {
                    if a.is_root()
                        || base
                            .entry_by_path(a.as_str())
                            .is_some_and(|e| e.kind.is_dir())
                    {
                        break;
                    }
                    self.look(base, &a, false, disk);
                    ancestor = a.parent();
                }
                if new_dir {
                    self.scan(path, disk);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(entry) = base.entry_by_path(path.as_str())
                    && !self.filter.excluded(path, entry.kind.is_dir())
                {
                    disk.insert(path.clone(), None);
                }
            }
            // Unreadable (permissions): the index keeps what it had.
            Err(_) => return,
        }
        if parent && let Some(p) = path.parent() {
            self.look(base, &p, false, disk);
        }
    }

    /// Reads the folder `dir` recursively into `disk`, applying the
    /// exclusion rules. Symbolic links are not followed.
    fn scan(&mut self, dir: &VaultPath, disk: &mut Disk) {
        let mut stack = vec![dir.clone()];
        while let Some(cur) = stack.pop() {
            let Ok(entries) = fs::read_dir(cur.to_fs_path(&self.root)) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Ok(child) = cur.join(&name) else { continue };
                if disk.contains_key(&child) {
                    continue;
                }
                let Ok((kind, meta)) = entry_of(&entry.path(), self.canonical()) else {
                    continue;
                };
                if self.filter.excluded(&child, kind.is_dir()) {
                    continue;
                }
                if kind.is_dir() {
                    stack.push(child.clone());
                }
                disk.insert(child, Some((kind, meta)));
            }
        }
    }

    /// Walks the whole root and publishes the differences.
    fn rescan_now(&mut self, cause: UpdateCause) {
        let base = self.shared.load();
        let fresh = match PathIndex::build(&self.root, base.config().clone()) {
            Ok((fresh, _)) => fresh,
            Err(e) => {
                self.emit(WatchEvent::Notice(WatchNotice::ScanFailed(e.to_string())));
                return;
            }
        };
        let disk = reconcile::rescan_disk(&base, &fresh);
        self.publish(&base, &[], &disk, cause);
    }

    fn startup(&mut self) {
        let base = self.shared.load();
        if base.state() == IndexState::Building {
            match PathIndex::build(&self.root, base.config().clone()) {
                Ok((index, stats)) => {
                    if self.shared.compare_and_store(&base, &index) {
                        self.emit(WatchEvent::IndexBuilt { index, stats });
                    } else {
                        // Someone else published meanwhile: compare instead.
                        self.rescan_now(UpdateCause::Startup);
                    }
                }
                Err(e) => self.emit(WatchEvent::Notice(WatchNotice::ScanFailed(e.to_string()))),
            }
        } else if self.options.startup_scan {
            self.rescan_now(UpdateCause::Startup);
        }
    }

    /// Plans the changes from `base` to `disk`, publishes them and reports
    /// what they mean for tracked documents.
    fn publish(
        &mut self,
        base: &PathIndex,
        renames: &[(VaultPath, VaultPath)],
        disk: &Disk,
        cause: UpdateCause,
    ) {
        let plan = match reconcile::plan(base, renames, disk) {
            Ok(plan) => plan,
            Err(e) => {
                self.conflict(&cause, e.to_string());
                return;
            }
        };
        let mut published = plan.index.clone();
        if !plan.changes.is_empty() {
            if !self.shared.compare_and_store(base, &plan.index) {
                // Another writer published meanwhile: apply on top of it.
                match self.shared.apply(&plan.changes) {
                    Ok(index) => published = index,
                    Err(e) => {
                        self.conflict(&cause, e.to_string());
                        return;
                    }
                }
            }
            let own_writes = self.own_writes(&plan.changes);
            self.emit(WatchEvent::IndexUpdated {
                index: published.clone(),
                changes: plan.changes.clone(),
                cause,
                own_writes,
            });
        }
        self.follow_moves(&plan.moves);
        let touched: Vec<VaultPath> = self
            .docs
            .keys()
            .filter(|key| {
                disk.contains_key(*key)
                    || plan.moves.iter().any(|(_, to)| key.starts_with(to))
                    || ancestors(key).any(|a| matches!(disk.get(&a), Some(None)))
            })
            .cloned()
            .collect();
        for key in touched {
            self.check_doc(&key);
        }
        self.report_conflict_copies(&published, &plan.new_files, &plan.moves);
    }

    fn conflict(&mut self, cause: &UpdateCause, message: String) {
        if matches!(cause, UpdateCause::Rescan(RescanReason::Conflict)) {
            self.emit(WatchEvent::Notice(WatchNotice::ScanFailed(message)));
        } else {
            self.schedule_rescan(RescanReason::Conflict);
        }
    }

    fn own_writes(&self, changes: &[Change]) -> Vec<VaultPath> {
        let now = Instant::now();
        changes
            .iter()
            .filter_map(|c| match c {
                Change::Upsert { path, meta, .. } => {
                    let e = self.expected.get(path)?;
                    (e.until > now && meta_matches(meta, &e.state)).then(|| path.clone())
                }
                _ => None,
            })
            .collect()
    }

    // ----- Documents -----

    fn follow_moves(&mut self, moves: &[(VaultPath, VaultPath)]) {
        for (from, to) in moves {
            let moved: Vec<VaultPath> = self
                .docs
                .keys()
                .filter(|k| k.starts_with(from))
                .cloned()
                .collect();
            for old in moved {
                let Some(rest) = old.strip_prefix(from) else {
                    continue;
                };
                let new = if rest.is_empty() {
                    to.clone()
                } else {
                    match VaultPath::new(&format!("{}/{rest}", to.as_str())) {
                        Ok(p) => p,
                        Err(_) => continue,
                    }
                };
                let Some(mut doc) = self.docs.remove(&old) else {
                    continue;
                };
                let old_path = doc.path.clone();
                let by_name = self.rel(&doc.path).as_ref() == Some(&old);
                if by_name {
                    doc.path = new.to_fs_path(&self.root);
                }
                let new_path = doc.path.clone();
                self.docs.insert(new, doc);
                if by_name {
                    self.emit(WatchEvent::DocumentMoved {
                        from: old_path,
                        to: new_path,
                    });
                }
            }
        }
    }

    /// Compares a tracked document with its baseline and reports a change.
    fn check_doc(&mut self, key: &VaultPath) {
        self.check_doc_with(key, true);
    }

    fn check_doc_with(&mut self, key: &VaultPath, report_deleted: bool) {
        let now = Instant::now();
        if self.pending.get(key).is_some_and(|until| *until > now) {
            self.deferred.insert(key.clone());
            return;
        }
        let expected = self
            .expected
            .get(key)
            .filter(|e| e.until > now)
            .map(|e| e.state.clone());
        let Some(doc) = self.docs.get_mut(key) else {
            return;
        };
        if let Some(state) = expected
            && stat_matches(&doc.path, &state)
        {
            doc.baseline = state;
            doc.deleted = false;
            return;
        }
        let change = match check_external_change(&doc.path, &doc.baseline, CheckMode::Thorough) {
            Ok(ExternalChange::Unchanged) if doc.deleted => match DiskState::read(&doc.path) {
                Ok((_, state)) => ExternalChange::Touched(state),
                Err(_) => return,
            },
            Ok(ExternalChange::Unchanged) | Err(_) => return,
            Ok(ExternalChange::Deleted) if doc.deleted || !report_deleted => return,
            // The write-access check runs after the read; a file moved
            // away in between looks read-only. The move is reported next.
            Ok(ExternalChange::Touched(_)) if fs::metadata(&doc.path).is_err() => return,
            Ok(change) => change,
        };
        match &change {
            ExternalChange::Touched(state) | ExternalChange::Modified { state, .. } => {
                doc.baseline = state.clone();
                doc.deleted = false;
            }
            ExternalChange::Deleted => doc.deleted = true,
            ExternalChange::Unchanged => {}
        }
        let path = doc.path.clone();
        self.emit(WatchEvent::DocumentChanged { path, change });
    }

    fn report_conflict_copies(
        &mut self,
        index: &PathIndex,
        new_files: &[VaultPath],
        moves: &[(VaultPath, VaultPath)],
    ) {
        let renamed = moves.iter().filter(|(from, to)| {
            from.file_name() != to.file_name()
                && from
                    .file_name()
                    .is_none_or(|n| detect_sync_conflict(n, |_| false).is_none())
        });
        let candidates: Vec<VaultPath> = new_files
            .iter()
            .cloned()
            .chain(renamed.map(|(_, to)| to.clone()))
            .collect();
        for copy in candidates {
            let (Some(name), Some(dir)) = (copy.file_name(), copy.parent()) else {
                continue;
            };
            if index
                .entry_by_path(copy.as_str())
                .is_none_or(|e| e.kind.is_dir())
            {
                continue;
            }
            let dir_fs = dir.to_fs_path(&self.root);
            let Some(conflict) = detect_sync_conflict(name, |original| {
                original != name && dir_fs.join(original).is_file()
            }) else {
                continue;
            };
            self.emit(WatchEvent::SyncConflict {
                copy: copy.to_fs_path(&self.root),
                original: dir_fs.join(&conflict.original),
                service: conflict.service,
            });
        }
    }

    // ----- Paths -----

    fn canonical(&self) -> Option<&Path> {
        self.canonical_root.as_deref()
    }

    /// The vault path of an absolute path below the root (as given, or
    /// canonical: FSEvents reports canonical paths).
    fn rel(&self, abs: &Path) -> Option<VaultPath> {
        let rel = abs.strip_prefix(&self.root).ok().or_else(|| {
            self.canonical_root
                .as_deref()
                .and_then(|c| abs.strip_prefix(c).ok())
        })?;
        VaultPath::from_relative_fs_path(rel).ok()
    }

    /// The key a document is tracked under: the vault path of the file
    /// itself (a symbolic link's target), so events on it find it.
    fn key_of(&self, path: &Path) -> Option<VaultPath> {
        let abs = std::path::absolute(path).ok()?;
        if let (Ok(canonical), Some(croot)) = (fs::canonicalize(&abs), &self.canonical_root)
            && let Ok(rel) = canonical.strip_prefix(croot)
        {
            return VaultPath::from_relative_fs_path(rel).ok();
        }
        self.rel(&abs)
    }
}

/// The ancestors of `path`, without the root.
fn ancestors(path: &VaultPath) -> impl Iterator<Item = VaultPath> {
    std::iter::successors(path.parent(), VaultPath::parent).filter(|p| !p.is_root())
}

/// Whether the file at `path` has the length, modification time and
/// identity of `state`.
fn stat_matches(path: &Path, state: &DiskState) -> bool {
    fs::metadata(path).is_ok_and(|m| {
        m.len() == state.len
            && m.modified().ok() == state.modified
            && FileIdentity::of(&m) == state.identity
    })
}

/// Whether index metadata agrees with a disk state.
fn meta_matches(meta: &FileMeta, state: &DiskState) -> bool {
    meta.size == state.len
        && meta.mtime == state.modified
        && meta.identity.map(|i| (i.dev, i.ino)) == state.identity.map(|i| (i.device, i.inode))
}

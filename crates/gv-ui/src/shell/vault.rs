// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A window's vault: its root folder and the path index of that folder.
//!
//! Nothing is indexed until something needs the index. A window opened for a
//! file creates its vault when the user first shows the sidebar, so starting
//! the application does no indexing at all. *Open folder* creates it at
//! once, rooted at that folder.
//!
//! The home folder, a filesystem root and a folder with more than
//! [`gv_vault::index::AUTO_INDEX_LIMIT`] entries are never indexed without asking
//! (FR-VAULT-02): the vault stays [`VaultStatus::NotIndexed`] with an
//! [`IndexQuestion`], which the window asks as an infobar. *Index* builds
//! the index without the limit ([`WindowVault::index_anyway`]).
//!
//! Once the path index is built, the vault starts its **content index**
//! ([`ContentIndexer`]) on a background thread, cached in the app's cache
//! folder. Every path-index snapshot the vault publishes (from the
//! watcher) goes to it, and so do the unsaved buffers of files in the
//! vault ([`crate::files::overlay`]). Dropping the vault (closing the
//! window, opening another folder) cancels it. Each snapshot it publishes
//! is announced with a [`ContentUpdated`] event (the quick switcher
//! follows them).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::{App, AppContext as _, Context, EventEmitter, Global, SharedString, Task};
use gv_vault::content::{ContentEvent, ContentIndexer, ContentOptions, ContentSnapshot};
use gv_vault::root::{self, RootDetection, RootMarker, RootReason};
use gv_vault::{IndexConfig, IndexError, PathIndex, ProfileId, ProfileParams, SharedPathIndex};

/// Where the path index is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VaultStatus {
    /// The initial walk is running.
    Building,
    /// The index is complete.
    Ready,
    /// The root is not indexed without asking: the home folder, a filesystem
    /// root, or more entries than the automatic limit.
    NotIndexed(SharedString),
    /// The root could not be read.
    Failed(SharedString),
}

/// Why a folder waits for the user before it is indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexQuestion {
    /// How the question describes the folder: "home folder", "more than
    /// 50,000 files".
    pub size: String,
}

/// The entry limit of automatic indexing, when not the default (tests).
pub(crate) struct AutoIndexLimit(pub usize);

impl Global for AutoIndexLimit {}

fn auto_index_limit(cx: &App) -> Option<usize> {
    Some(
        cx.try_global::<AutoIndexLimit>()
            .map_or(gv_vault::index::AUTO_INDEX_LIMIT, |l| l.0),
    )
}

/// The content index published a new snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContentUpdated;

/// The vault of one window. Views observe it: it notifies whenever it
/// publishes a new snapshot of the index.
pub(crate) struct WindowVault {
    root: PathBuf,
    name: SharedString,
    reason: RootReason,
    index: Arc<SharedPathIndex>,
    status: VaultStatus,
    /// Indexing waits for the user.
    question: Option<IndexQuestion>,
    build_time: Option<Duration>,
    _build: Option<Task<()>>,
    /// The content index, once the path index is built.
    content: Option<ContentIndexer>,
    /// Turns the content index's snapshots into [`ContentUpdated`] events.
    _content_events: Option<Task<()>>,
    /// The content index published a snapshot not announced yet.
    content_pending: Arc<AtomicBool>,
}

impl EventEmitter<ContentUpdated> for WindowVault {}

impl fmt::Debug for WindowVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowVault")
            .field("root", &self.root)
            .field("reason", &self.reason)
            .field("status", &self.status)
            .field("build_time", &self.build_time)
            .finish_non_exhaustive()
    }
}

impl WindowVault {
    /// The vault of the file at `path`: root detection, then the path index
    /// built on a background thread.
    pub fn for_file(path: &Path, cx: &mut Context<'_, Self>) -> Self {
        Self::open(root::detect_root(path), cx)
    }

    /// The vault of a folder the user opened: rooted at that folder.
    pub fn for_folder(folder: &Path, cx: &mut Context<'_, Self>) -> Self {
        let root = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
        Self::open(
            RootDetection {
                root,
                reason: RootReason::FileFolder,
            },
            cx,
        )
    }

    fn open(detected: RootDetection, cx: &mut Context<'_, Self>) -> Self {
        // Per-profile settings (gitignore for repositories, the vault's
        // userIgnoreFilters) come with profile detection.
        let config = IndexConfig {
            entry_limit: auto_index_limit(cx),
            ..IndexConfig::default()
        };
        let name = detected.root.file_name().map_or_else(
            || detected.root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let index = Arc::new(SharedPathIndex::building(&detected.root, config.clone()));
        let mut this = Self {
            root: detected.root.clone(),
            name: name.into(),
            reason: detected.reason,
            index: index.clone(),
            status: VaultStatus::Building,
            question: None,
            build_time: None,
            _build: None,
            content: None,
            _content_events: None,
            content_pending: Arc::default(),
        };
        if let Some(reason) = root::is_refused_root(&detected.root, root::home_dir().as_deref()) {
            this.status = VaultStatus::NotIndexed(format!("Not indexed: {reason}").into());
            this.question = Some(IndexQuestion {
                size: format!("your {reason}"),
            });
            return this;
        }
        this.build(config, cx);
        this
    }

    /// Builds the index on a background thread, after rolling back
    /// refactors of this vault that a crash interrupted.
    fn build(&mut self, config: IndexConfig, cx: &mut Context<'_, Self>) {
        let index = self.index.clone();
        let path = self.root.clone();
        let journal = refactor_journal_dir(cx);
        self.status = VaultStatus::Building;
        self._build = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = cx
                .background_spawn(async move {
                    if let Some(journal) = journal {
                        for done in gv_vault::refactor::recover(&path, &journal) {
                            tracing::warn!(
                                root = %path.display(),
                                id = done.id,
                                problems = ?done.problems,
                                "rolled back an interrupted file operation"
                            );
                        }
                    }
                    index.build(&path, config)
                })
                .await;
            let elapsed = started.elapsed();
            this.update(cx, |vault, cx| vault.finish_build(result, elapsed, cx))
                .ok();
        }));
    }

    /// The user answered *Index*: builds the index without the entry
    /// limit.
    pub fn index_anyway(&mut self, cx: &mut Context<'_, Self>) {
        if self.question.take().is_none() {
            return;
        }
        let config = IndexConfig {
            entry_limit: None,
            ..IndexConfig::default()
        };
        self.index = Arc::new(SharedPathIndex::building(&self.root, config.clone()));
        self.build(config, cx);
        cx.notify();
    }

    /// The user answered *Cancel*: the folder stays unindexed and is not
    /// asked about again in this window.
    pub fn decline(&mut self, cx: &mut Context<'_, Self>) {
        if self.question.take().is_some() {
            cx.notify();
        }
    }

    /// What indexing waits for, if it waits for the user.
    pub fn question(&self) -> Option<&IndexQuestion> {
        self.question.as_ref()
    }

    fn finish_build(
        &mut self,
        result: Result<gv_vault::WalkStats, IndexError>,
        elapsed: Duration,
        cx: &mut Context<'_, Self>,
    ) {
        self.build_time = Some(elapsed);
        self.status = match result {
            Ok(stats) => {
                tracing::info!(
                    root = %self.root.display(),
                    entries = stats.entries,
                    errors = stats.errors,
                    ?elapsed,
                    "path index built"
                );
                self.start_content(cx);
                VaultStatus::Ready
            }
            Err(IndexError::TooManyEntries { limit }) => {
                self.question = Some(IndexQuestion {
                    size: format!("more than {} files", thousands(limit)),
                });
                VaultStatus::NotIndexed(format!("Not indexed: more than {limit} entries").into())
            }
            Err(err) => {
                tracing::warn!(root = %self.root.display(), "cannot index: {err}");
                VaultStatus::Failed("Cannot read this folder".into())
            }
        };
        self._build = None;
        cx.notify();
    }

    /// Publishes a newer snapshot (from the file watcher, or after a file
    /// operation) and tells the views.
    pub fn publish(&mut self, index: PathIndex, cx: &mut Context<'_, Self>) {
        if let Some(content) = &self.content {
            content.update_paths(index.clone());
        }
        self.index.store(index);
        cx.notify();
    }

    /// The profile the vault's links resolve with.
    pub fn profile(&self) -> ProfileId {
        match self.reason {
            RootReason::Marker(RootMarker::Obsidian) => ProfileId::Obsidian,
            RootReason::Marker(RootMarker::Git) => ProfileId::GfmFile,
            RootReason::Marker(RootMarker::Glyphvane) | RootReason::FileFolder => {
                ProfileId::Extended
            }
        }
    }

    /// The link resolution parameters: the content index's, else the
    /// profile's defaults.
    pub fn params(&self) -> ProfileParams {
        self.content().map_or_else(
            || ProfileParams::for_profile(self.profile()),
            |c| c.options().params.clone(),
        )
    }

    /// Starts the content index on the built path index, replacing (and
    /// so cancelling) a previous one.
    fn start_content(&mut self, cx: &mut Context<'_, Self>) {
        let profile = self.profile();
        let mut options = ContentOptions::for_profile(profile);
        if let Some(locations) = cx
            .try_global::<crate::settings::AppSettings>()
            .and_then(|s| s.locations())
        {
            options = options.with_cache(locations.cache_dir(), &self.root);
        }
        let root = self.root.clone();
        // The indexer's thread marks each snapshot and wakes the window's
        // thread, which announces it. GPUI's test scheduler forbids waking
        // from another thread: there, tests call `poll_content`.
        let pending = Arc::new(AtomicBool::new(false));
        self.content_pending = pending.clone();
        let (published, mut snapshots) = futures::channel::mpsc::unbounded::<()>();
        let published = (!cx.background_executor().is_test()).then_some(published);
        let announce = move || {
            pending.store(true, Ordering::Release);
            if let Some(published) = &published {
                // Fails only when the vault is gone.
                let _ = published.unbounded_send(());
            }
        };
        let on_event = move |event: ContentEvent| match event {
            ContentEvent::Ready { stats, .. } => {
                tracing::info!(
                    root = %root.display(),
                    notes = stats.notes,
                    parsed = stats.parsed,
                    cache = ?stats.cache,
                    elapsed = ?stats.elapsed,
                    "content index built"
                );
                announce();
            }
            ContentEvent::Updated { .. } => announce(),
            ContentEvent::PersistFailed(err) => {
                tracing::warn!(root = %root.display(), "content index cache: {err}");
            }
            ContentEvent::Persisted { .. } => {}
        };
        self._content_events = Some(cx.spawn(async move |this, cx| {
            while snapshots.next().await.is_some() {
                // One event for a burst of snapshots.
                while snapshots.try_recv().is_ok() {}
                if this.update(cx, |v, cx| v.poll_content(cx)).is_err() {
                    break;
                }
            }
        }));
        self.content = None;
        match ContentIndexer::start(self.index.load(), options, on_event) {
            Ok(content) => {
                crate::files::overlay::register(&self.root, content.handle());
                self.content = Some(content);
            }
            Err(err) => tracing::warn!("cannot start the content index: {err}"),
        }
    }

    /// Announces a snapshot the content index published since the last
    /// call, with [`ContentUpdated`]. Returns whether there was one.
    pub fn poll_content(&mut self, cx: &mut Context<'_, Self>) -> bool {
        let fresh = self.content_pending.swap(false, Ordering::AcqRel);
        if fresh {
            cx.emit(ContentUpdated);
        }
        fresh
    }

    /// The content index's current snapshot, once it is built (for the
    /// backlinks, tags, switcher and link-checker panels).
    pub fn content(&self) -> Option<ContentSnapshot> {
        self.content.as_ref().and_then(ContentIndexer::snapshot)
    }

    /// The root folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The shared index, for the vault watcher.
    pub fn shared_index(&self) -> Arc<SharedPathIndex> {
        self.index.clone()
    }

    /// The root folder's name.
    pub fn name(&self) -> SharedString {
        self.name.clone()
    }

    pub fn status(&self) -> &VaultStatus {
        &self.status
    }

    /// The current snapshot of the index.
    pub fn snapshot(&self) -> PathIndex {
        self.index.load()
    }

    /// How long the initial walk took, once it is done.
    #[cfg(test)]
    pub fn build_time(&self) -> Option<Duration> {
        self.build_time
    }
}

/// Where refactors (rename and move with their link updates) are
/// journaled: `refactor/` in the app's data folder, when it is known.
pub(crate) fn refactor_journal_dir(cx: &App) -> Option<PathBuf> {
    cx.try_global::<crate::settings::AppSettings>()
        .and_then(|s| s.locations())
        .map(|l| l.data_dir().join("refactor"))
}

/// `n` with thousands separators: 50000 becomes "50,000".
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui::{AppContext as _, Entity, TestAppContext};
    use gv_vault::content::ContentSnapshot;
    use gv_vault::{IndexConfig, PathIndex, VaultPath};

    use super::WindowVault;

    /// Waits (in real time: the index has its own thread) until the
    /// vault's content index satisfies `f`.
    fn until(
        vault: &Entity<WindowVault>,
        cx: &mut TestAppContext,
        f: impl Fn(&ContentSnapshot) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            cx.run_until_parked();
            if vault
                .read_with(cx, |v, _| v.content())
                .is_some_and(|s| f(&s))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the content index did not get there in time");
    }

    #[gpui::test]
    fn the_content_index_follows_the_vault(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        std::fs::write(root.join("A.md"), "[[B]]\n").expect("write");
        std::fs::write(root.join("B.md"), "b\n").expect("write");
        let vault = cx.new(|cx| WindowVault::for_folder(root, cx));
        let b = VaultPath::new("B.md").expect("path");
        until(&vault, cx, |s| s.backlink_sources(&b).len() == 1);

        // A newer path index (as the watcher publishes it).
        std::fs::write(root.join("C.md"), "also [[B]]\n").expect("write");
        let index = PathIndex::build(root, IndexConfig::default())
            .expect("index")
            .0;
        vault.update(cx, |v, cx| v.publish(index, cx));
        until(&vault, cx, |s| s.backlink_sources(&b).len() == 2);

        // Unsaved text of a file in the vault.
        crate::files::overlay::dirty(&root.join("A.md"), &gv_text::TextSnapshot::new("none\n"));
        until(&vault, cx, |s| s.backlink_sources(&b).len() == 1);
        crate::files::overlay::clean(&root.join("A.md"));
        until(&vault, cx, |s| s.backlink_sources(&b).len() == 2);
    }

    #[test]
    fn counts_have_thousands_separators() {
        assert_eq!(super::thousands(7), "7");
        assert_eq!(super::thousands(50_000), "50,000");
        assert_eq!(super::thousands(1_234_567), "1,234,567");
    }
}

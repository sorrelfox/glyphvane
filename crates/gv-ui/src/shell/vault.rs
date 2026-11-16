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

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Global, SharedString, Task};
use gv_vault::root::{self, RootDetection, RootReason};
use gv_vault::{IndexConfig, IndexError, PathIndex, SharedPathIndex};

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
}

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

    /// Builds the index on a background thread.
    fn build(&mut self, config: IndexConfig, cx: &mut Context<'_, Self>) {
        let index = self.index.clone();
        let path = self.root.clone();
        self.status = VaultStatus::Building;
        self._build = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = cx
                .background_spawn(async move { index.build(&path, config) })
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
        self.index.store(index);
        cx.notify();
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
    #[test]
    fn counts_have_thousands_separators() {
        assert_eq!(super::thousands(7), "7");
        assert_eq!(super::thousands(50_000), "50,000");
        assert_eq!(super::thousands(1_234_567), "1,234,567");
    }
}

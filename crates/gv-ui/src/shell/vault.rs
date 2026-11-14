// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A window's vault: its root folder and the path index of that folder.
//!
//! Nothing is indexed until something needs the index. A window opened for a
//! file creates its vault when the user first shows the sidebar, so starting
//! the application does no indexing at all.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, SharedString, Task};
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

/// The vault of one window. Views observe it: it notifies whenever it
/// publishes a new snapshot of the index.
pub(crate) struct WindowVault {
    root: PathBuf,
    name: SharedString,
    reason: RootReason,
    index: Arc<SharedPathIndex>,
    status: VaultStatus,
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

    fn open(detected: RootDetection, cx: &mut Context<'_, Self>) -> Self {
        // Per-profile settings (gitignore for repositories, the vault's
        // userIgnoreFilters) come with profile detection.
        let config = IndexConfig::default();
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
            build_time: None,
            _build: None,
        };
        // TODO(vault): ask before indexing these ("Index ~/ (≈ N files)?").
        if let Some(reason) = root::is_refused_root(&detected.root, root::home_dir().as_deref()) {
            this.status = VaultStatus::NotIndexed(format!("Not indexed: {reason}").into());
            return this;
        }
        let path = detected.root;
        this._build = Some(cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let result = cx
                .background_spawn(async move { index.build(&path, config) })
                .await;
            let elapsed = started.elapsed();
            this.update(cx, |vault, cx| vault.finish_build(result, elapsed, cx))
                .ok();
        }));
        this
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
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by the watcher, coming next")
    )]
    pub fn publish(&mut self, index: PathIndex, cx: &mut Context<'_, Self>) {
        self.index.store(index);
        cx.notify();
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
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

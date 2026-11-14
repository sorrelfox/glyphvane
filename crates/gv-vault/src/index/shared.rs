// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Publishing index snapshots to other threads.

use std::fmt;
use std::path::Path;

use arc_swap::ArcSwap;

use super::{Change, IndexConfig, IndexError, PathIndex, WalkStats};

/// The current [`PathIndex`] of a vault, readable from any thread without
/// locks. Writers replace the whole snapshot; readers keep whatever
/// snapshot they loaded for as long as they need it.
pub struct SharedPathIndex {
    current: ArcSwap<super::Inner>,
}

impl fmt::Debug for SharedPathIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SharedPathIndex")
            .field(&self.load())
            .finish()
    }
}

impl SharedPathIndex {
    /// Starts with `index` as the current snapshot.
    #[must_use]
    pub fn new(index: PathIndex) -> Self {
        Self {
            current: ArcSwap::new(index.inner_arc().clone()),
        }
    }

    /// Starts with an empty, building index (before the walk).
    #[must_use]
    pub fn building(root: impl Into<std::path::PathBuf>, config: IndexConfig) -> Self {
        Self::new(PathIndex::new_building(root, config))
    }

    /// The current snapshot. O(1), lock-free.
    #[must_use]
    pub fn load(&self) -> PathIndex {
        PathIndex::from_arc(self.current.load_full())
    }

    /// Replaces the current snapshot.
    pub fn store(&self, index: PathIndex) {
        self.current.store(index.inner_arc().clone());
    }

    /// Applies `changes` to the current snapshot and publishes the result.
    /// Concurrent writers are serialized by retrying (read-copy-update).
    pub fn apply(&self, changes: &[Change]) -> Result<PathIndex, IndexError> {
        let mut result: Result<Option<PathIndex>, IndexError> = Ok(None);
        self.current.rcu(
            |cur| match PathIndex::from_arc(cur.clone()).apply(changes) {
                Ok(next) => {
                    let arc = next.inner_arc().clone();
                    result = Ok(Some(next));
                    arc
                }
                Err(e) => {
                    result = Err(e);
                    cur.clone()
                }
            },
        );
        result.map(|next| next.unwrap_or_else(|| self.load()))
    }

    /// Publishes `next` only if the current snapshot is still `expected`
    /// (the one `next` was derived from). Returns whether it was published.
    pub(crate) fn compare_and_store(&self, expected: &PathIndex, next: &PathIndex) -> bool {
        let prev = self
            .current
            .compare_and_swap(expected.inner_arc(), next.inner_arc().clone());
        std::sync::Arc::ptr_eq(&*prev, expected.inner_arc())
    }

    /// Runs the initial walk of `root` and publishes the complete index.
    /// Until it returns, readers see the building snapshot this was created
    /// with. Blocking: run it on a background thread.
    pub fn build(&self, root: &Path, config: IndexConfig) -> Result<WalkStats, IndexError> {
        let (index, stats) = PathIndex::build(root, config)?;
        self.store(index);
        Ok(stats)
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Local version history in the views (FR-FILE-10, Should; plan 11 § Local
//! version history). The store is `gv_vault::files::history`; this module
//! keeps it for the application and shows it.
//!
//! - [`VersionHistory`]: a global with the store, installed by [`install`].
//!   Without it nothing is kept.
//! - **Snapshots on save:** the Source view calls [`record_saved`] after
//!   every successful save (and Save As) with the saved text. The snapshot
//!   is hashed, compressed and written off the UI thread; a text equal to
//!   the newest version is not kept again.
//! - [`HistoryPanel`]: the versions of one file, newest first, with their
//!   time and size; the selected version's text read-only; *Compare with
//!   current* and *Restore*, which the window hands to the file's Source
//!   view ([`crate::editor::EditorView::compare_with_version`],
//!   [`crate::editor::EditorView::restore_version`]): the compare and merge
//!   view, or the whole text replaced as one undoable edit; and *Copy*.
//!
//! Like the crash journal, the store is `<data dir>/history/no-vault/`,
//! keyed by the file's full path: documents are not tied to vault ids yet.

mod panel;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use gpui::{App, AppContext as _, BorrowAppContext as _, Global};
use gv_settings::Locations;
use gv_text::TextSnapshot;
use gv_vault::files::history::{HistoryPolicy, HistoryStore, Recorded};
use gv_vault::files::journal::NO_VAULT_ID;

pub use panel::{HistoryEvent, HistoryPanel, KEY_CONTEXT, key_bindings};

/// The application's version history. `generation` grows with every kept
/// snapshot, so open panels know to read the list again.
#[derive(Debug, Default)]
pub struct VersionHistory {
    store: Option<Arc<HistoryStore>>,
    generation: u64,
}

impl Global for VersionHistory {}

impl VersionHistory {
    pub fn store(&self) -> Option<&Arc<HistoryStore>> {
        self.store.as_ref()
    }

    /// Grows each time a snapshot is kept.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// The widgets' keys.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
}

/// Opens the store in the data dir of `locations`, and removes what a
/// crash left there, off the UI thread. Without locations (no home folder)
/// or in safe mode nothing is kept. Call once at start-up, after
/// [`crate::init`].
pub fn install(cx: &mut App, locations: Option<&Locations>) {
    let safe_mode = cx
        .try_global::<crate::settings::AppSettings>()
        .is_some_and(crate::settings::AppSettings::safe_mode);
    let store = locations.filter(|_| !safe_mode).and_then(
        |locations| match HistoryStore::for_vault(locations, NO_VAULT_ID, HistoryPolicy::default())
        {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!(
                    "version history: cannot use the data folder, so no versions are kept: {err}"
                );
                None
            }
        },
    );
    install_store(cx, store);
    if let Some(store) = cx.global::<VersionHistory>().store.clone() {
        cx.background_spawn(async move {
            if let Err(err) = store.collect_garbage(SystemTime::now()) {
                tracing::warn!("version history: cleaning up: {err}");
            }
        })
        .detach();
    }
}

/// [`install`] with a store of its own, for tests and portable setups.
pub fn install_store(cx: &mut App, store: Option<HistoryStore>) {
    cx.set_global(VersionHistory {
        store: store.map(Arc::new),
        generation: 0,
    });
}

/// The store, if versions are kept.
pub fn store(cx: &App) -> Option<Arc<HistoryStore>> {
    cx.try_global::<VersionHistory>()?.store.clone()
}

/// Keeps `text`, just saved to `path`, as a version (off the UI thread).
/// Does nothing without a store.
pub fn record_saved(path: PathBuf, text: TextSnapshot, cx: &mut App) {
    let Some(store) = store(cx) else {
        return;
    };
    let task = cx.background_spawn(async move {
        let text = text.to_string();
        store.record(&path, text.as_bytes(), SystemTime::now())
    });
    cx.spawn(async move |cx| match task.await {
        Ok(Recorded::Added(_)) => {
            cx.update(|cx| {
                if cx.has_global::<VersionHistory>() {
                    cx.update_global::<VersionHistory, _>(|h, _| h.generation += 1);
                }
            });
        }
        Ok(Recorded::Unchanged | Recorded::TooLarge) => {}
        Err(err) => tracing::warn!("version history: cannot keep a version: {err}"),
    })
    .detach();
}

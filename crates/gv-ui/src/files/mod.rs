// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Data safety in the views: saving, the crash journal, autosave, crash
//! recovery and changes made on disk by other programs. The pipeline
//! itself is `gv_vault::files`; this module wires it to the windows.
//!
//! # Parts
//!
//! | Part | What |
//! |---|---|
//! | [`DataSafety`] | A global: the journal store, the journals in use, untitled numbers |
//! | [`worker`] | One background task per edited document: it records every edit in the journal and runs the saves, in order |
//! | [`watch`] | Per window: open files changed on disk, the vault watcher |
//! | [`notice`] | What the infobar says |
//! | [`save_as`] | Where Save As writes: the save dialog, the suggested name, the `.md` extension |
//! | [`open_dialog`] | Open file and Open folder: the open dialog |
//! | The Source view ([`crate::editor`]) | A document's file, journal, autosave state and conflicts |
//! | The window ([`crate::shell`]) | Save and close commands, the infobar, the unsaved dot, watcher events |
//!
//! # Rules
//!
//! - **No file I/O on the UI thread.** Loading, journaling, saving, change
//!   checks, decoding and the reload diff run on the background executor.
//! - **Every edit reaches the journal** as soon as the editor applies it,
//!   through the document's worker; a successful save clears it.
//! - **Autosave** follows `files.autosave` with the policy of
//!   [`gv_vault::files::Autosave`]: 1 s after the last edit, on focus loss,
//!   on tab switch and on quit; never during an input-method composition
//!   or while a conflict, deleted-file or recovery infobar is open.
//! - **Closing with unsaved changes** (a tab, a window, the application):
//!   with autosave on, the document is saved first (the window waits for
//!   it). A document that would not be saved on its own (untitled,
//!   autosave off, a conflict or recovery question open, a file that may
//!   not be written, a failed save) is asked about first: *Save*, *Don't
//!   save* or *Cancel* (not designed: a warning infobar; see
//!   [`crate::shell`]'s `saving`). Cancel, a cancelled save dialog or a
//!   failed save stops the whole close. Text the close could not save or
//!   ask about (the application ended another way) stays in the journal
//!   and comes back with the recovery infobar.
//! - **Untitled documents** live in the journal until saved. Save on one
//!   is Save As; after Save As (of any document) its tab, journal, watcher
//!   and recent-files entry follow the new path.
//! - **Crash recovery:** at start-up every journal left in the store
//!   reopens its document in Edit mode with the recovered text, marked
//!   modified, and the recovery infobar (Restore, Compare, Discard). A
//!   journal found when a file is opened is recovered the same way.
//! - **Compare** ([`crate::compare`]): the conflict, recovery and
//!   sync-conflict infobars open a compare and merge view; its result
//!   replaces the text as one undoable edit, and nothing is written to
//!   disk until the document is saved.
//! - **Large and read-only files:** large-file mode shows its infobar; a
//!   file that may not be written (permissions, also when they change on
//!   disk), is too large to edit or is not valid in its encoding opens
//!   read-only with an infobar.
//!
//! Journals live in `<data dir>/journal/no-vault/`: the documents are not
//! tied to vault ids yet.

pub mod notice;
pub mod open_dialog;
#[cfg(test)]
mod safety_tests;
pub mod save_as;
#[cfg(test)]
mod tests;
pub(crate) mod watch;
pub(crate) mod worker;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::{App, AppContext as _, AsyncApp, BackgroundExecutor, Global};
use gv_settings::Locations;
use gv_vault::files::AutosaveSettings;
use gv_vault::files::journal::{DocumentKey, JournalId, JournalStore, NO_VAULT_ID};

use crate::settings::AppSettings;
use crate::shell::{self, Workspace};

/// How long quitting or closing a window waits for its saves before it
/// goes ahead; a save still running then is finished on the next start
/// from the journal.
pub const CLOSE_WAIT: Duration = Duration::from_secs(10);

/// The application's data-safety state. One per application, installed by
/// [`install`]; without it nothing is journaled and no file is watched
/// (saving still works).
pub struct DataSafety {
    store: Option<JournalStore>,
    /// Whether windows watch their files and vaults.
    watch_files: bool,
    /// Journals open in this process, so two views of one file never share
    /// one.
    active: Arc<Mutex<HashSet<JournalId>>>,
    next_untitled: u64,
    autosave: Option<bool>,
}

impl Global for DataSafety {}

impl std::fmt::Debug for DataSafety {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataSafety")
            .field("store", &self.store.as_ref().map(JournalStore::dir))
            .field("watch_files", &self.watch_files)
            .finish_non_exhaustive()
    }
}

/// Sets up the journal store in the data dir of `locations` and turns on
/// file watching. Without locations (no home directory) nothing is
/// journaled. Call once at start-up, after [`crate::init`].
pub fn install(cx: &mut App, locations: Option<&Locations>) {
    let store = locations.and_then(|locations| {
        match JournalStore::for_vault(locations, NO_VAULT_ID) {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::warn!("recovery journal: cannot use the data folder, so edits are not journaled: {err}");
                None
            }
        }
    });
    install_store(cx, store, true);
}

/// [`install`] with a store of its own; `watch_files` turns the file
/// watchers on. For tests and portable setups.
pub fn install_store(cx: &mut App, store: Option<JournalStore>, watch_files: bool) {
    cx.set_global(DataSafety {
        store,
        watch_files,
        active: Arc::default(),
        next_untitled: 1,
        autosave: None,
    });
}

/// Turns autosave on or off whatever the settings say (tests).
pub fn override_autosave(cx: &mut App, enabled: Option<bool>) {
    if cx.has_global::<DataSafety>() {
        cx.global_mut::<DataSafety>().autosave = enabled;
    }
}

/// The journal store, if documents are journaled.
pub(crate) fn store(cx: &App) -> Option<JournalStore> {
    cx.try_global::<DataSafety>()?.store.clone()
}

/// Whether windows watch their files.
pub(crate) fn watches_files(cx: &App) -> bool {
    cx.try_global::<DataSafety>().is_some_and(|s| s.watch_files)
}

/// The autosave settings in effect.
pub(crate) fn autosave_settings(cx: &App) -> AutosaveSettings {
    let configured = cx
        .try_global::<AppSettings>()
        .is_none_or(|s| s.settings().files.autosave);
    let enabled = cx
        .try_global::<DataSafety>()
        .and_then(|s| s.autosave)
        .unwrap_or(configured);
    AutosaveSettings {
        enabled,
        ..AutosaveSettings::default()
    }
}

/// A new untitled document's number.
pub(crate) fn next_untitled(cx: &mut App) -> u64 {
    if !cx.has_global::<DataSafety>() {
        return 1;
    }
    let safety = cx.global_mut::<DataSafety>();
    let n = safety.next_untitled;
    safety.next_untitled += 1;
    n
}

/// Untitled number `n` is taken (a recovered journal).
fn note_untitled(cx: &mut App, n: u64) {
    if cx.has_global::<DataSafety>() {
        let safety = cx.global_mut::<DataSafety>();
        safety.next_untitled = safety.next_untitled.max(n.saturating_add(1));
    }
}

/// The right to use the journal of one document in this process. Held by
/// the document's worker; released when it ends.
#[derive(Debug)]
pub(crate) struct Claim {
    id: JournalId,
    active: Arc<Mutex<HashSet<JournalId>>>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// Claims the journal of `key`; `None` when another view in this process
/// has it (that view journals the document).
pub(crate) fn claim(cx: &App, key: &DocumentKey) -> Option<Claim> {
    let safety = cx.try_global::<DataSafety>()?;
    safety.store.as_ref()?;
    let id = JournalId::of(key);
    let fresh = safety
        .active
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id.clone());
    fresh.then(|| Claim {
        id,
        active: safety.active.clone(),
    })
}

/// The autosave policy's clock: the executor's, so tests can move it.
#[derive(Clone)]
pub(crate) struct ExecutorClock(pub BackgroundExecutor);

impl std::fmt::Debug for ExecutorClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExecutorClock")
    }
}

impl gv_vault::files::Clock for ExecutorClock {
    fn now(&self) -> Instant {
        self.0.now()
    }
}

/// Reopens the documents whose journals an earlier session left behind (a
/// crash, or a close with unsaved changes): in Edit mode, with the
/// recovered text and the recovery infobar, as tabs of the window focused
/// last (a new window when none is open). Call once at start-up, after the
/// first window opened.
pub fn recover_unsaved(cx: &mut App) {
    let Some(store) = store(cx) else {
        return;
    };
    if cx
        .try_global::<AppSettings>()
        .is_some_and(AppSettings::safe_mode)
    {
        // Safe mode runs beside a normal instance, whose journals these may
        // be; the next normal start recovers them.
        tracing::info!("safe mode: journals from earlier sessions are left for a normal start");
        return;
    }
    cx.spawn(async move |cx| {
        let found = cx
            .background_spawn(async move {
                if let Err(err) = store.remove_stale_temp_files() {
                    tracing::warn!("recovery journal: {err}");
                }
                store.recoverable()
            })
            .await;
        match found {
            Ok(found) if !found.is_empty() => {
                cx.update(|cx| reopen(found, cx));
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("recovery journal: cannot list the journals: {err}"),
        }
    })
    .detach();
}

fn reopen(found: Vec<gv_vault::files::journal::RecoverableDocument>, cx: &mut App) {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut untitled: Vec<u64> = Vec::new();
    for doc in found {
        match doc.key {
            Some(DocumentKey::Path(path)) => paths.push(path),
            Some(DocumentKey::Untitled(n)) => {
                note_untitled(cx, n);
                untitled.push(n);
            }
            None => tracing::warn!("recovery journal {}: unreadable, left in place", doc.id),
        }
    }
    if paths.is_empty() && untitled.is_empty() {
        return;
    }
    tracing::info!(
        files = paths.len(),
        untitled = untitled.len(),
        "recovering unsaved changes"
    );
    let window = match shell::most_recent_window(cx) {
        Some(window) => window,
        None => match crate::open(cx, Vec::new(), crate::Placement::RecentWindow)
            .ok()
            .and_then(|_| shell::most_recent_window(cx))
        {
            Some(window) => window,
            None => return,
        },
    };
    let workspace = window.workspace.clone();
    // Fails only when the window has closed.
    let _ = cx.update_window(window.window, |_, window, cx| {
        workspace.update(cx, |ws: &mut Workspace, cx| {
            ws.recover(paths, untitled, window, cx);
        })
    });
}

/// The `app.quit` command: first asks, window by window, about documents
/// that would not be saved on their own (Save, Don't save, Cancel), then
/// saves what autosave may save, syncs every journal, waits for both (at
/// most [`CLOSE_WAIT`]) and quits. A save that fails stops quitting and
/// asks about it.
pub fn quit(cx: &mut App) {
    if shell::ask_before_quit(cx) {
        return;
    }
    let waits = shell::prepare_quit(cx);
    if waits.is_empty() {
        finish_quit(cx);
        return;
    }
    cx.spawn(async move |cx| {
        wait_for(waits, cx).await;
        cx.update(|cx| {
            if !shell::ask_before_quit(cx) {
                finish_quit(cx);
            }
        });
    })
    .detach();
}

/// Drops the unsaved text answered with Don't save, then quits.
fn finish_quit(cx: &mut App) {
    let drops = shell::discard_for_quit(cx);
    if drops.is_empty() {
        cx.quit();
        return;
    }
    cx.spawn(async move |cx| {
        wait_for(drops, cx).await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// Waits until every receiver fired or was dropped, or [`CLOSE_WAIT`]
/// passed.
pub(crate) async fn wait_for(waits: Vec<oneshot::Receiver<()>>, cx: &AsyncApp) {
    let all = futures::future::join_all(waits);
    let timer = cx.background_executor().timer(CLOSE_WAIT);
    let _ = futures::future::select(std::pin::pin!(all), std::pin::pin!(timer)).await;
}

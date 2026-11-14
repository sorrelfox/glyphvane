// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The document worker: one background task per edited document that owns
//! its crash journal and runs its saves, in the order the UI asked for
//! them.
//!
//! The UI thread only sends messages ([`Msg`]): an edit is recorded the
//! moment the editor applies it, without waiting, and a save runs off the
//! UI thread with its reply on a oneshot channel. Because one queue
//! carries both, the journal always matches what was saved: a save that
//! succeeds clears the journal before the edits made after it are
//! recorded against the saved text.
//!
//! Each record is written at once (bounded latency); `fsync` follows the
//! journal's own cadence ([`DocumentJournal::next_sync_deadline`]), which
//! the worker waits for between messages. When the last sender is dropped
//! (the tab closed) the worker finishes the queue, syncs and ends; a
//! journal with unsaved edits stays on disk for recovery.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::{mpsc, oneshot};
use futures::future::Either;
use gpui::BackgroundExecutor;
use gv_text::{EditSummary, TextFormat, TextSnapshot, Transaction};
use gv_vault::files::journal::DocumentJournal;
use gv_vault::files::{Backup, ContentHash, DocumentFile, SaveError, SaveOptions, SaveOutcome};

use super::Claim;

/// How long the worker waits after a failed `fsync` before it tries again.
const SYNC_RETRY: Duration = Duration::from_secs(1);

/// What the UI asks of a document's worker.
pub(crate) enum Msg {
    /// An edit the editor applied: what changed, and the text after it.
    Record {
        summary: EditSummary,
        text: TextSnapshot,
    },
    /// The buffer now holds the file on disk again (a reload, Take theirs,
    /// Discard): `base` is the new base and the journal is cleared.
    Rebase {
        base: TextSnapshot,
        format: TextFormat,
        hash: ContentHash,
    },
    /// Save `text` to `file`; the reply carries the file with its new
    /// baseline.
    Save {
        file: DocumentFile,
        text: TextSnapshot,
        options: SaveOptions,
        reply: oneshot::Sender<SaveReply>,
    },
    /// Sync the journal now (focus loss, closing, quitting). The reply, if
    /// any, fires once everything sent before it is done.
    Sync(Option<oneshot::Sender<()>>),
    /// Delete the journal: the user dropped the unsaved text.
    Discard,
}

/// The outcome of [`Msg::Save`].
#[derive(Debug)]
pub(crate) struct SaveReply {
    /// On success, the file with its new baseline.
    pub result: Result<(DocumentFile, SaveOutcome), SaveError>,
}

/// The sending side of a document's worker. Clones share the worker.
#[derive(Clone, Debug)]
pub(crate) struct Worker {
    tx: mpsc::UnboundedSender<Msg>,
}

impl Worker {
    /// Starts the worker of a document on the background executor. With
    /// `journal` `None` (no data dir, or another view edits the same file)
    /// the worker only saves.
    pub fn spawn(
        journal: Option<DocumentJournal>,
        claim: Option<Claim>,
        executor: &BackgroundExecutor,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded();
        let timers = executor.clone();
        executor
            .spawn(async move {
                run(journal, rx, timers).await;
                drop(claim);
            })
            .detach();
        Self { tx }
    }

    /// The sender, for the editor's observer.
    pub fn sender(&self) -> mpsc::UnboundedSender<Msg> {
        self.tx.clone()
    }

    pub fn send(&self, msg: Msg) {
        // The worker ends only when every sender is gone.
        let _ = self.tx.unbounded_send(msg);
    }

    /// Asks for a sync and returns a receiver that fires once the queue up
    /// to here is done (including a save sent before).
    pub fn flush(&self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.send(Msg::Sync(Some(tx)));
        rx
    }
}

async fn run(
    mut journal: Option<DocumentJournal>,
    mut rx: mpsc::UnboundedReceiver<Msg>,
    executor: BackgroundExecutor,
) {
    let mut retry_sync_at: Option<Instant> = None;
    loop {
        let deadline = journal
            .as_ref()
            .and_then(DocumentJournal::next_sync_deadline)
            .map(|d| retry_sync_at.map_or(d, |r| r.max(d)));
        let msg = match deadline {
            None => rx.next().await,
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                let timer = std::pin::pin!(executor.timer(wait));
                match futures::future::select(rx.next(), timer).await {
                    Either::Left((msg, _)) => msg,
                    Either::Right(((), _)) => {
                        if let Some(journal) = journal.as_mut() {
                            retry_sync_at = sync_due(journal);
                        }
                        continue;
                    }
                }
            }
        };
        let Some(msg) = msg else {
            break;
        };
        handle(&mut journal, msg);
    }
    if let Some(journal) = journal.as_mut()
        && let Err(err) = journal.sync()
    {
        tracing::warn!("journal {}: cannot sync: {err}", journal.id());
    }
}

/// Syncs a journal whose sync is due; returns when to try again after a
/// failure.
fn sync_due(journal: &mut DocumentJournal) -> Option<Instant> {
    match journal.sync_if_due() {
        Ok(_) => None,
        Err(err) => {
            tracing::warn!("journal {}: cannot sync: {err}", journal.id());
            Some(Instant::now() + SYNC_RETRY)
        }
    }
}

fn handle(journal: &mut Option<DocumentJournal>, msg: Msg) {
    match msg {
        Msg::Record { summary, text } => {
            if let Some(journal) = journal.as_mut() {
                record(journal, &summary, &text);
            }
        }
        Msg::Rebase { base, format, hash } => {
            if let Some(journal) = journal.as_mut()
                && let Err(err) = journal.saved(base, format, hash)
            {
                tracing::warn!("journal {}: cannot clear: {err}", journal.id());
            }
        }
        Msg::Save {
            mut file,
            text,
            options,
            reply,
        } => {
            let result = match journal.as_mut() {
                Some(journal) => file.save(&text, &options, journal),
                None => file.save(&text, &options, &mut NoBackup),
            };
            let result = result.map(|outcome| {
                if let (Some(journal), Some(baseline)) = (journal.as_mut(), file.baseline())
                    && let Err(err) = journal.saved(text, file.format(), baseline.hash)
                {
                    tracing::warn!("journal {}: cannot clear after a save: {err}", journal.id());
                }
                (file, outcome)
            });
            if let Err(err) = &result {
                tracing::info!("not saved: {err}");
            }
            // The view may be gone (a tab closed while its last save ran).
            let _ = reply.send(SaveReply { result });
        }
        Msg::Sync(reply) => {
            if let Some(journal) = journal.as_mut()
                && let Err(err) = journal.sync()
            {
                tracing::warn!("journal {}: cannot sync: {err}", journal.id());
            }
            if let Some(reply) = reply {
                let _ = reply.send(());
            }
        }
        Msg::Discard => {
            if let Some(journal) = journal.take() {
                let id = journal.id().clone();
                if let Err(err) = journal.discard() {
                    tracing::warn!("journal {id}: cannot delete: {err}");
                }
            }
        }
    }
}

/// Records one edit. The editor's summary gives the replaced ranges in the
/// old text and the inserted ranges in `text`, which is all a transaction
/// needs; if the text cannot be read back, the whole text is recorded.
fn record(journal: &mut DocumentJournal, summary: &EditSummary, text: &TextSnapshot) {
    let mut transaction = Transaction::new();
    for edit in summary.edits() {
        match text.text_for_range(edit.new.clone()) {
            Ok(inserted) => transaction.push(edit.old.clone(), inserted),
            Err(_) => {
                if let Err(err) = journal.record_snapshot(text) {
                    tracing::warn!("journal {}: cannot record: {err}", journal.id());
                }
                return;
            }
        }
    }
    if let Err(err) = journal.record(&transaction, text) {
        tracing::warn!("journal {}: cannot record: {err}", journal.id());
    }
}

/// The backup of an in-place write when there is no journal to hold it:
/// refusing keeps a hardlinked file from being overwritten without a copy.
struct NoBackup;

impl Backup for NoBackup {
    fn store(&mut self, _target: &Path, _original: &[u8]) -> io::Result<()> {
        Err(io::Error::other(
            "no recovery journal to hold the original while writing in place",
        ))
    }

    fn clear(&mut self, _target: &Path) -> io::Result<()> {
        Ok(())
    }
}

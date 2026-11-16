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
//! Each record is written as soon as the worker gets to it (bounded
//! latency). Edits that queued up meanwhile (while a save ran, or the disk
//! was slow) are written together, in order, with one write. `fsync`
//! follows the journal's own cadence
//! ([`DocumentJournal::next_sync_deadline`]), which the worker waits for
//! between messages. When the last sender is dropped (the tab closed) the
//! worker finishes the queue, syncs and ends; a journal with unsaved edits
//! stays on disk for recovery.

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

/// At most this many queued edits go to the journal in one write.
const MAX_BATCH: usize = 1024;

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
        let mut next = Some(msg);
        while let Some(msg) = next.take() {
            match msg {
                Msg::Record { summary, text } => {
                    // Edits that queued up while the worker was busy (a
                    // save, a slow disk) go to the journal in one write. A
                    // message of another kind ends the batch and runs after
                    // it, so the order stays the UI's.
                    let mut batch = vec![(summary, text)];
                    while batch.len() < MAX_BATCH {
                        match rx.try_recv() {
                            Ok(Msg::Record { summary, text }) => batch.push((summary, text)),
                            Ok(other) => {
                                next = Some(other);
                                break;
                            }
                            // Empty, or closed: the next wait sees the end.
                            Err(_) => break,
                        }
                    }
                    if let Some(journal) = journal.as_mut() {
                        record(journal, &batch);
                    }
                }
                other => handle(&mut journal, other),
            }
        }
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
                record(journal, &[(summary, text)]);
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

/// Records edits in order. The editor's summary gives the replaced ranges
/// in the old text and the inserted ranges in the new one, which is all a
/// transaction needs; an edit whose text cannot be read back is recorded
/// as a snapshot of the whole text. Consecutive edits go out in one write.
fn record(journal: &mut DocumentJournal, edits: &[(EditSummary, TextSnapshot)]) {
    let mut run: Vec<(Transaction, &TextSnapshot)> = Vec::new();
    for (summary, text) in edits {
        match transaction_of(summary, text) {
            Some(transaction) => run.push((transaction, text)),
            None => {
                write_run(journal, &mut run);
                if let Err(err) = journal.record_snapshot(text) {
                    tracing::warn!("journal {}: cannot record: {err}", journal.id());
                }
            }
        }
    }
    write_run(journal, &mut run);
}

/// The transaction that `summary` describes, read from `text`, the text
/// after it.
fn transaction_of(summary: &EditSummary, text: &TextSnapshot) -> Option<Transaction> {
    let mut transaction = Transaction::new();
    for edit in summary.edits() {
        let inserted = text.text_for_range(edit.new.clone()).ok()?;
        transaction.push(edit.old.clone(), inserted);
    }
    Some(transaction)
}

/// Writes `run` and empties it. If the write fails, a snapshot of the last
/// text is tried, so later records do not apply to a journal that misses
/// some of these.
fn write_run(journal: &mut DocumentJournal, run: &mut Vec<(Transaction, &TextSnapshot)>) {
    let Some(&(_, last)) = run.last() else {
        return;
    };
    if let Err(err) = journal.record_all(run.iter().map(|(t, text)| (t, *text))) {
        tracing::warn!("journal {}: cannot record: {err}", journal.id());
        if let Err(err) = journal.record_snapshot(last) {
            tracing::warn!("journal {}: cannot record a snapshot: {err}", journal.id());
        }
    }
    run.clear();
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

#[cfg(test)]
mod tests {
    //! The worker over a real journal in a temporary folder: queued edits
    //! are written in order around saves, and a flush waits for them.

    use std::fs;
    use std::path::PathBuf;

    use gpui::TestAppContext;
    use gv_text::{Buffer, Transaction};
    use gv_vault::files::journal::{DocumentKey, JournalStore};
    use gv_vault::files::{DocumentFile, SaveOptions};

    use super::{Msg, SaveReply, Worker};

    struct Doc {
        _dir: tempfile::TempDir,
        path: PathBuf,
        store: JournalStore,
        file: DocumentFile,
        buffer: Buffer,
    }

    fn doc(text: &str) -> Doc {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("note.md");
        fs::write(&path, text).expect("write");
        let store = JournalStore::open(dir.path().join("journal")).expect("store");
        let (file, decoded) = DocumentFile::load(&path).expect("load");
        let buffer = Buffer::from_decoded(&decoded);
        Doc {
            _dir: dir,
            path,
            store,
            file,
            buffer,
        }
    }

    impl Doc {
        fn worker(&self, cx: &TestAppContext) -> Worker {
            let journal = self
                .store
                .begin_default(
                    DocumentKey::Path(self.path.clone()),
                    self.buffer.snapshot(),
                    self.file.format(),
                    self.file.baseline().map(|b| b.hash),
                )
                .expect("journal");
            Worker::spawn(Some(journal), None, &cx.executor())
        }

        /// Applies edit `step` and sends it, as the editor's observer does.
        fn edit(&mut self, worker: &Worker, step: usize) {
            let at = self
                .buffer
                .clip_offset((step * 7) % (self.buffer.len() + 1), gv_text::Bias::Left);
            let transaction = if step % 4 == 3 && self.buffer.len() > at + 2 {
                let end = self.buffer.clip_offset(at + 2, gv_text::Bias::Right);
                Transaction::new().delete(at..end)
            } else {
                Transaction::new().insert(at, format!("<{step}é>"))
            };
            let summary = self.buffer.apply(&transaction).expect("edit");
            worker.send(Msg::Record {
                summary,
                text: self.buffer.snapshot(),
            });
        }

        fn save(&self, worker: &Worker) -> futures::channel::oneshot::Receiver<SaveReply> {
            let (reply, answer) = futures::channel::oneshot::channel();
            worker.send(Msg::Save {
                file: self.file.clone(),
                text: self.buffer.snapshot(),
                options: SaveOptions::default(),
                reply,
            });
            answer
        }

        fn journal_file(&self) -> Option<PathBuf> {
            let mut journals = self.store.scan().expect("scan");
            journals.recoverable.append(&mut journals.in_use);
            journals
                .recoverable
                .first()
                .map(|d| self.store.dir().join(d.id.as_str()))
        }
    }

    #[gpui::test]
    fn queued_edits_are_journaled_in_order_around_a_save(cx: &mut TestAppContext) {
        let mut doc = doc("# Note\r\n\r\nText ünïcödé.\r\n");
        let worker = doc.worker(cx);
        // Nothing runs until the test parks, so every message below is
        // queued when the worker starts: the edits arrive as bursts.
        for step in 0..60 {
            doc.edit(&worker, step);
        }
        let saved_text = doc.buffer.to_string();
        let mut answer = doc.save(&worker);
        for step in 60..85 {
            doc.edit(&worker, step);
        }
        let mut flushed = worker.flush();
        cx.run_until_parked();

        let reply = answer.try_recv().expect("reply").expect("saved");
        assert!(reply.result.is_ok(), "the save succeeded");
        assert!(
            flushed.try_recv().expect("flush").is_some(),
            "the flush answers once the queue before it is done"
        );
        assert_eq!(
            fs::read_to_string(&doc.path).expect("read"),
            saved_text,
            "the save wrote the text of the edits sent before it"
        );

        // A crash: the worker and its journal go away unsaved.
        drop(worker);
        cx.run_until_parked();
        let listed = doc.store.recoverable().expect("list");
        assert_eq!(listed.len(), 1);
        let recovery = doc.store.recover(&listed[0].id).expect("recover");
        assert_eq!(recovery.text, doc.buffer.to_string());
        assert_eq!(
            recovery.base.as_deref(),
            Some(saved_text.as_str()),
            "the edits after the save apply to the saved text"
        );
        assert_eq!(recovery.records, 25, "only the edits after the save");
    }

    #[gpui::test]
    fn a_save_after_a_burst_leaves_no_journal(cx: &mut TestAppContext) {
        let mut doc = doc("start\n");
        let worker = doc.worker(cx);
        for step in 0..200 {
            doc.edit(&worker, step);
        }
        let mut answer = doc.save(&worker);
        let mut flushed = worker.flush();
        cx.run_until_parked();

        assert!(
            answer
                .try_recv()
                .expect("reply")
                .expect("saved")
                .result
                .is_ok()
        );
        assert!(flushed.try_recv().expect("flush").is_some());
        assert_eq!(
            fs::read_to_string(&doc.path).expect("read"),
            doc.buffer.to_string()
        );
        assert_eq!(doc.journal_file(), None, "the save cleared the journal");
        drop(worker);
        cx.run_until_parked();
        assert!(doc.store.recoverable().expect("list").is_empty());
    }

    #[gpui::test]
    fn a_flush_waits_for_the_edits_sent_before_it(cx: &mut TestAppContext) {
        let mut doc = doc("x\n");
        let worker = doc.worker(cx);
        for step in 0..30 {
            doc.edit(&worker, step);
        }
        let mut flushed = worker.flush();
        assert_eq!(doc.journal_file(), None, "nothing written before it runs");
        cx.run_until_parked();
        assert!(flushed.try_recv().expect("flush").is_some());
        let journal = doc.journal_file().expect("the journal exists");
        assert!(fs::metadata(&journal).expect("journal").len() > 0);

        drop(worker);
        cx.run_until_parked();
        let listed = doc.store.recoverable().expect("list");
        let recovery = doc.store.recover(&listed[0].id).expect("recover");
        assert_eq!(recovery.text, doc.buffer.to_string());
        assert_eq!(recovery.records, 30);
    }
}

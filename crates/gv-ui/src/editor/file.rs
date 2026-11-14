// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view's document file: loading with its crash journal,
//! saving, autosave, changes on disk and crash recovery (see
//! [`crate::files`]).
//!
//! # Life of a document
//!
//! 1. **Load** (background): [`DocumentFile::load`], then the journal. A
//!    journal left by an earlier session is recovered: its text replaces
//!    the old journal with a new one (base: the file on disk, then a
//!    snapshot of the recovered text) before the view sees it, and the
//!    view applies it as one undoable edit, marked modified, with the
//!    recovery infobar.
//! 2. **Edits:** the editor's observer hands each one to the document's
//!    worker, which journals it; autosave counts it.
//! 3. **Save:** a save point, the watcher told, the worker saves; on
//!    success the editor is marked saved at that point (undo stops there)
//!    and the journal is cleared. Failures show an error infobar and keep
//!    the text modified.
//! 4. **Changes on disk:** a clean buffer reloads the file as a minimal
//!    diff ([`reload_transaction`]), one undoable edit, so anchors,
//!    selections and the scroll position survive; a modified one gets the
//!    conflict infobar (Keep mine, Take theirs; Compare is not designed).
//!    A deleted file gets an infobar with Save and Close.

use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::channel::oneshot;
use gpui::{AppContext as _, Context, Task};
use gv_editor::{EditKind, SavePoint};
use gv_syntax::{ParseOptions, SyntaxTree};
use gv_text::{
    Bom, Buffer, DecodeError, DecodeStatus, Decoded, Encoding, Revision, TextFormat, TextSnapshot,
    Transaction,
};
use gv_vault::files::journal::{
    DiskComparison, DocumentJournal, DocumentKey, JournalError, JournalId, JournalPolicy,
    JournalStore,
};
use gv_vault::files::{
    Autosave, DiskState, DocumentFile, ExternalChange, LoadError, OnExternalChange, QuitDecision,
    ReadOnlyReason, SaveError, SaveOptions, SaveReason, SaveRequest, SystemClock, WriteError,
    reload_transaction,
};

use super::EditorView;
use crate::files::notice::{Notice, NoticeAction, Topic};
use crate::files::worker::{Msg, SaveReply, Worker};
use crate::files::{self, ExecutorClock};

/// What the window hears from a Source view about its document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocEvent {
    /// The document loaded: its file can be watched.
    Loaded,
    /// The text went from saved to modified, or back.
    ModifiedChanged,
    /// The infobar changed.
    NoticeChanged,
    /// The text was replaced by the file on disk: the Reading view
    /// follows.
    Reloaded,
    /// A save finished.
    Saved,
    /// The user closed the document from its infobar.
    CloseRequested,
}

/// A loaded document, before it gets an editor.
pub(super) struct Loaded {
    pub buffer: Buffer,
    pub tree: SyntaxTree,
    pub lossy: bool,
    pub file: Option<DocumentFile>,
    pub journal: Option<DocumentJournal>,
    pub recovered: Option<Recovered>,
}

/// Text rebuilt from a journal an earlier session left: the edit from the
/// file as loaded to it (computed off the UI thread).
pub(super) struct Recovered {
    pub transaction: Transaction,
    pub disk: DiskComparison,
}

/// Reads, decodes and parses `path` and opens its journal, recovering one
/// left behind. Runs on the background executor.
pub(super) fn load_file(
    path: &Path,
    options: ParseOptions,
    store: Option<&JournalStore>,
) -> Result<Loaded, String> {
    let key = DocumentKey::Path(path.to_path_buf());
    let (file, buffer) = match DocumentFile::load(path) {
        Ok((file, decoded)) => (file, Buffer::from_decoded(&decoded)),
        // Deleted with unsaved edits in a journal: they come back, and the
        // first save writes the file again.
        Err(LoadError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound
                && store.is_some_and(|s| has_journal(s, &key)) =>
        {
            (
                DocumentFile::new_file(path, TextFormat::default()),
                Buffer::new(""),
            )
        }
        Err(err) => return Err(err.to_string()),
    };
    let lossy = matches!(file.status(), DecodeStatus::Lossy(_));
    let (file, buffer, journal, recovered) = with_journal(store, key, Some(file), buffer);
    let tree = SyntaxTree::parse_with(buffer.text(), options);
    Ok(Loaded {
        buffer,
        tree,
        lossy,
        file,
        journal,
        recovered,
    })
}

/// An untitled document: empty, or the text its journal holds.
pub(super) fn load_untitled(n: u64, options: ParseOptions, store: Option<&JournalStore>) -> Loaded {
    let (file, buffer, journal, recovered) =
        with_journal(store, DocumentKey::Untitled(n), None, Buffer::new(""));
    let tree = SyntaxTree::parse_with(buffer.text(), options);
    Loaded {
        buffer,
        tree,
        lossy: false,
        file,
        journal,
        recovered,
    }
}

fn has_journal(store: &JournalStore, key: &DocumentKey) -> bool {
    let id = JournalId::of(key);
    store.dir().join(id.as_str()).exists() || store.dir().join(format!("{id}.backup")).exists()
}

type WithJournal = (
    Option<DocumentFile>,
    Buffer,
    Option<DocumentJournal>,
    Option<Recovered>,
);

/// Opens the journal of `key` over `buffer` (the file as loaded). A
/// journal from an earlier session is recovered and replaced by a new one
/// holding the same text.
fn with_journal(
    store: Option<&JournalStore>,
    key: DocumentKey,
    mut file: Option<DocumentFile>,
    mut buffer: Buffer,
) -> WithJournal {
    let Some(store) = store else {
        return (file, buffer, None, None);
    };
    let begin = |file: &Option<DocumentFile>, buffer: &Buffer| {
        let format = file
            .as_ref()
            .map_or_else(|| buffer.format(), DocumentFile::format);
        let hash = file
            .as_ref()
            .and_then(DocumentFile::baseline)
            .map(|b| b.hash);
        store.begin(
            key.clone(),
            buffer.snapshot(),
            format,
            hash,
            JournalPolicy::default(),
            SystemClock,
        )
    };
    let id = match begin(&file, &buffer) {
        Ok(journal) => return (file, buffer, Some(journal), None),
        Err(JournalError::PendingRecovery(id)) => id,
        Err(err) => {
            tracing::warn!("recovery journal: {err}; edits to this document are not journaled");
            return (file, buffer, None, None);
        }
    };
    let recovery = match store.recover(&id) {
        Ok(recovery) => recovery,
        Err(err) => {
            tracing::warn!("recovery journal {id}: cannot recover ({err}); it is left in place");
            return (file, buffer, None, None);
        }
    };
    if !matches!(
        recovery.tail,
        gv_vault::files::journal::JournalTail::Complete
    ) {
        tracing::warn!(
            "recovery journal {id}: ends early ({:?}); {} records restored",
            recovery.tail,
            recovery.records
        );
    }
    if recovery.backup.is_some() {
        // An in-place write was cut short: the original goes back first.
        match store.restore_backup(&id) {
            Ok(path) => {
                tracing::warn!(
                    "{}: an interrupted save was undone from the recovery journal",
                    path.display()
                );
                if let Ok((restored, decoded)) = DocumentFile::load(&path) {
                    file = Some(restored);
                    buffer = Buffer::from_decoded(&decoded);
                }
            }
            Err(err) => {
                tracing::warn!(
                    "recovery journal {id}: cannot restore the backup ({err}); it is left in place"
                );
                return (file, buffer, None, None);
            }
        }
    }
    let loaded = buffer.text().to_string();
    let recovered_text = (recovery.text != loaded).then_some(recovery.text);
    if let Err(err) = store.discard(&id) {
        // The old journal stays; this session edits without one.
        tracing::warn!("recovery journal {id}: {err}; edits to this document are not journaled");
        let recovered = recovered_text.map(|text| Recovered {
            transaction: reload_transaction(&loaded, &text),
            disk: recovery.disk,
        });
        return (file, buffer, None, recovered);
    }
    let mut journal = match begin(&file, &buffer) {
        Ok(journal) => Some(journal),
        Err(err) => {
            tracing::warn!("recovery journal: {err}; edits to this document are not journaled");
            None
        }
    };
    if let (Some(journal), Some(text)) = (journal.as_mut(), &recovered_text)
        && let Err(err) = journal.record_snapshot(&Buffer::new(text).snapshot())
    {
        tracing::warn!("recovery journal {id}: cannot record the recovered text: {err}");
    }
    let recovered = recovered_text.map(|text| Recovered {
        transaction: reload_transaction(&loaded, &text),
        disk: recovery.disk,
    });
    (file, buffer, journal, recovered)
}

/// The version on disk waiting while the buffer has unsaved edits.
#[derive(Debug)]
struct Theirs {
    decoded: Decoded,
    state: DiskState,
}

/// A save on its way.
#[derive(Debug)]
struct Saving {
    request: SaveRequest,
    point: SavePoint,
    path: PathBuf,
}

/// The file side of a document in a Source view.
pub(crate) struct DocState {
    /// `None` for a text given directly (tests, scratch): never saved.
    key: Option<DocumentKey>,
    file: Option<DocumentFile>,
    worker: Option<Worker>,
    autosave: Autosave<ExecutorClock>,
    /// While set, edits are not journaled (a reload: the journal is
    /// rebased instead; the recovered text: the journal holds it already).
    pub(super) pause: Rc<Cell<bool>>,
    /// Set by the editor's observer on every edit; taken after each change.
    pub(super) edited: Rc<Cell<bool>>,
    saving: Option<Saving>,
    /// A manual save asked for while one ran.
    save_again: bool,
    /// A change on disk that arrived while a save ran.
    deferred: Option<ExternalChange>,
    conflict: Option<Theirs>,
    deleted: bool,
    /// The recovery infobar is open.
    recovering: bool,
    /// The text as loaded (the file on disk), for Discard.
    base: TextSnapshot,
    notice: Option<Notice>,
    /// The last save error shown, so a failing autosave does not show it
    /// again and again.
    last_error: Option<Notice>,
    /// Whether the text was modified when last reported.
    modified: bool,
    timer: Option<Task<()>>,
    save_task: Option<Task<()>>,
    disk_task: Option<Task<()>>,
}

impl std::fmt::Debug for DocState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocState")
            .field("key", &self.key)
            .field("file", &self.file.as_ref().map(DocumentFile::path))
            .field("saving", &self.saving.is_some())
            .field("conflict", &self.conflict.is_some())
            .field("deleted", &self.deleted)
            .field("notice", &self.notice.as_ref().map(|n| n.topic))
            .finish_non_exhaustive()
    }
}

impl DocState {
    pub(super) fn new(
        key: Option<DocumentKey>,
        file: Option<DocumentFile>,
        worker: Option<Worker>,
        base: TextSnapshot,
        clock: ExecutorClock,
    ) -> Self {
        Self {
            key,
            file,
            worker,
            autosave: Autosave::new(Default::default(), clock),
            pause: Rc::default(),
            edited: Rc::default(),
            saving: None,
            save_again: false,
            deferred: None,
            conflict: None,
            deleted: false,
            recovering: false,
            base,
            notice: None,
            last_error: None,
            modified: false,
            timer: None,
            save_task: None,
            disk_task: None,
        }
    }

    pub(super) fn worker(&self) -> Option<&Worker> {
        self.worker.as_ref()
    }

    fn untitled(&self) -> bool {
        matches!(self.key, Some(DocumentKey::Untitled(_)))
    }

    /// Whether an infobar that holds autosave back is open.
    fn held(&self) -> bool {
        self.conflict.is_some() || self.deleted || self.recovering
    }

    /// Shows `notice`. A newer question replaces the recovery one, which
    /// then counts as answered with Keep (the text stays).
    fn set_notice(&mut self, notice: Option<Notice>) {
        let recovery = |n: Option<&Notice>| n.is_some_and(|n| n.topic == Topic::Recovered);
        if recovery(self.notice.as_ref()) && !recovery(notice.as_ref()) {
            self.recovering = false;
        }
        self.notice = notice;
    }

    fn shows(&self, topic: Topic) -> bool {
        self.notice.as_ref().is_some_and(|n| n.topic == topic)
    }

    fn clear_notice(&mut self, topics: &[Topic]) {
        if self
            .notice
            .as_ref()
            .is_some_and(|n| topics.contains(&n.topic))
        {
            self.notice = None;
        }
    }
}

/// The text of `snapshot`.
fn text_of(snapshot: &TextSnapshot) -> String {
    snapshot.to_string()
}

/// The file on disk decoded, and the edit that turns `current` into it.
struct Prepared {
    decoded: Result<Decoded, DecodeError>,
    revision: Revision,
    transaction: Transaction,
}

fn prepare_bytes(bytes: &[u8], current: &TextSnapshot) -> Prepared {
    match gv_text::decode(bytes) {
        Ok(decoded) => prepare_text(decoded, current),
        Err(err) => Prepared {
            decoded: Err(err),
            revision: current.revision(),
            transaction: Transaction::new(),
        },
    }
}

fn prepare_text(decoded: Decoded, current: &TextSnapshot) -> Prepared {
    let transaction = reload_transaction(&text_of(current), &decoded.text);
    Prepared {
        decoded: Ok(decoded),
        revision: current.revision(),
        transaction,
    }
}

impl EditorView {
    // ----- state the window reads ----------------------------------------------

    /// Whether the text differs from its saved state (for an untitled
    /// document: from empty): the tab's unsaved dot.
    pub fn is_modified(&self) -> bool {
        self.doc.as_ref().is_some_and(|d| d.editor.is_modified())
    }

    /// The infobar to show for this document.
    pub fn notice(&self) -> Option<&crate::files::notice::Notice> {
        self.doc.as_ref()?.state.notice.as_ref()
    }

    /// The file's path and the state it had when loaded or last saved, to
    /// watch it.
    pub fn disk_baseline(&self) -> Option<(PathBuf, DiskState)> {
        let file = self.doc.as_ref()?.state.file.as_ref()?;
        Some((file.path().to_path_buf(), file.baseline()?.clone()))
    }

    /// Whether a save is running.
    pub fn is_saving(&self) -> bool {
        self.doc.as_ref().is_some_and(|d| d.state.saving.is_some())
    }

    /// Whether closing would save first: modified, with a file, and
    /// autosave allowed to write it.
    pub fn saves_on_close(&self) -> bool {
        self.doc.as_ref().is_some_and(|d| {
            d.editor.is_modified()
                && d.state.file.is_some()
                && d.state.worker.is_some()
                && !d.state.untitled()
                && !d.state.held()
                && d.state.autosave.settings().enabled
        })
    }

    // ----- after every change ----------------------------------------------------

    /// Bookkeeping after anything changed the editor: autosave counts an
    /// edit and follows the composition; the unsaved dot follows the text.
    pub(super) fn after_edit(&mut self, cx: &mut Context<'_, Self>) {
        let mut settings = files::autosave_settings(cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let state = &mut doc.state;
        if state.key.is_none() || state.untitled() || state.worker.is_none() {
            // Untitled documents live in the journal until Save As exists.
            settings.enabled = false;
        }
        state.autosave.set_settings(settings);
        state.autosave.set_composing(doc.editor.is_composing());
        let held = state.held();
        state.autosave.set_conflict(held);
        if state.edited.replace(false) {
            state.autosave.edited();
        }
        self.schedule_autosave(cx);
        self.report_modified(cx);
        self.find_after_edit(cx);
        self.schedule_word_count(cx);
    }

    /// Tells the window when the text went from saved to modified or back.
    fn report_modified(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let modified = doc.editor.is_modified();
        if modified != doc.state.modified {
            doc.state.modified = modified;
            cx.emit(DocEvent::ModifiedChanged);
        }
    }

    fn notice_changed(&mut self, cx: &mut Context<'_, Self>) {
        cx.emit(DocEvent::NoticeChanged);
        cx.notify();
    }

    // ----- autosave ----------------------------------------------------------------

    /// Arms the idle timer for autosave's next deadline.
    fn schedule_autosave(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(deadline) = doc.state.autosave.next_deadline() else {
            doc.state.timer = None;
            return;
        };
        let executor = cx.background_executor().clone();
        let wait = deadline.saturating_duration_since(executor.now());
        doc.state.timer = Some(cx.spawn(async move |this, cx| {
            executor.timer(wait).await;
            this.update(cx, |view, cx| view.autosave_due(cx)).ok();
        }));
    }

    fn autosave_due(&mut self, cx: &mut Context<'_, Self>) {
        let request = self.doc.as_mut().and_then(|doc| doc.state.autosave.poll());
        match request {
            Some(request) => self.start_save(request, SaveOptions::default(), cx),
            None => self.schedule_autosave(cx),
        }
    }

    /// The window lost focus: autosave, and sync the journal.
    pub fn focus_lost(&mut self, cx: &mut Context<'_, Self>) {
        self.after_edit(cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if let Some(worker) = &doc.state.worker {
            worker.send(Msg::Sync(None));
        }
        if let Some(request) = doc.state.autosave.focus_lost() {
            self.start_save(request, SaveOptions::default(), cx);
        }
    }

    /// The user switched to another tab: autosave.
    pub fn tab_switched(&mut self, cx: &mut Context<'_, Self>) {
        self.after_edit(cx);
        let request = self
            .doc
            .as_mut()
            .and_then(|doc| doc.state.autosave.tab_switched());
        if let Some(request) = request {
            self.start_save(request, SaveOptions::default(), cx);
        }
    }

    // ----- saving --------------------------------------------------------------------

    /// Mod+S.
    pub fn save(&mut self, cx: &mut Context<'_, Self>) {
        self.after_edit(cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.state.untitled() {
            // Not designed: there is no Save As dialog yet.
            doc.state.set_notice(Some(Notice::untitled()));
            self.notice_changed(cx);
            return;
        }
        if doc.state.saving.is_some() {
            doc.state.save_again = true;
            return;
        }
        if let Some(request) = doc.state.autosave.manual() {
            self.start_save(request, SaveOptions::default(), cx);
        }
    }

    /// A save the user asked for whether or not autosave sees edits.
    fn save_anyway(&mut self, options: SaveOptions, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.state.saving.is_some() {
            return;
        }
        let request = doc.state.autosave.manual().unwrap_or(SaveRequest {
            reason: SaveReason::Manual,
            generation: 0,
        });
        self.start_save(request, options, cx);
    }

    fn start_save(
        &mut self,
        request: SaveRequest,
        options: SaveOptions,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let (Some(file), Some(worker)) = (doc.state.file.clone(), doc.state.worker.clone()) else {
            doc.state.autosave.save_failed(request);
            return;
        };
        let point = doc.editor.save_point();
        let text = doc.editor.snapshot();
        let path = file.path().to_path_buf();
        if let Some(watch) = &self.watch {
            watch.begin_write(&path);
        }
        let (reply, answer) = oneshot::channel();
        worker.send(Msg::Save {
            file,
            text,
            options,
            reply,
        });
        doc.state.saving = Some(Saving {
            request,
            point,
            path,
        });
        doc.state.save_task = Some(cx.spawn(async move |this, cx| {
            let reply = answer.await.ok();
            this.update(cx, |view, cx| view.save_finished(reply, cx))
                .ok();
        }));
    }

    fn save_finished(&mut self, reply: Option<SaveReply>, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(saving) = doc.state.saving.take() else {
            return;
        };
        doc.state.save_task = None;
        match reply.map(|r| r.result) {
            Some(Ok((file, _))) => {
                let baseline = file.baseline().cloned();
                doc.state.file = Some(file);
                doc.editor.mark_saved_at(saving.point);
                doc.state.autosave.save_succeeded(saving.request);
                if let Some(watch) = &self.watch {
                    match baseline {
                        Some(state) => watch.expect_write(&saving.path, state),
                        None => watch.cancel_write(&saving.path),
                    }
                }
                doc.state.deleted = false;
                doc.state.last_error = None;
                doc.state.clear_notice(&[Topic::SaveFailed, Topic::Deleted]);
                cx.emit(DocEvent::Saved);
                self.notice_changed(cx);
            }
            Some(Err(err)) => {
                doc.state.autosave.save_failed(saving.request);
                if let Some(watch) = &self.watch {
                    watch.cancel_write(&saving.path);
                }
                self.save_failed(err, saving.request.reason, cx);
            }
            None => {
                doc.state.autosave.save_failed(saving.request);
                if let Some(watch) = &self.watch {
                    watch.cancel_write(&saving.path);
                }
                tracing::warn!("{}: the save did not finish", saving.path.display());
            }
        }
        if let Some(change) = self.doc.as_mut().and_then(|d| d.state.deferred.take()) {
            self.disk_changed(change, cx);
        }
        if self
            .doc
            .as_mut()
            .is_some_and(|d| std::mem::take(&mut d.state.save_again))
        {
            self.save(cx);
        }
        self.after_edit(cx);
        cx.notify();
    }

    fn save_failed(&mut self, err: SaveError, reason: SaveReason, cx: &mut Context<'_, Self>) {
        let notice = match err {
            SaveError::ExternalChange { change, .. } => {
                self.disk_changed(*change, cx);
                return;
            }
            SaveError::Unmappable {
                line,
                column,
                ch,
                encoding,
                ..
            } => Notice::unmappable(line, column, ch, encoding),
            SaveError::ReadOnly(ReadOnlyReason::Lossy(_)) => {
                let encoding = self
                    .doc
                    .as_ref()
                    .and_then(|d| d.state.file.as_ref())
                    .map_or(Encoding::Utf8, |f| f.format().encoding);
                Notice::lossy(encoding)
            }
            SaveError::ReadOnly(reason) => Notice::save_failed(sentence(&reason.to_string())),
            SaveError::Write(WriteError::ReadOnly { .. }) => {
                Notice::save_failed("The file is read-only.".to_owned())
            }
            other => Notice::save_failed(sentence(&other.to_string())),
        };
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        // An autosave that fails the same way again stays quiet.
        let repeat = reason != SaveReason::Manual && doc.state.last_error.as_ref() == Some(&notice);
        doc.state.last_error = Some(notice.clone());
        if !repeat {
            doc.state.set_notice(Some(notice));
            self.notice_changed(cx);
        }
    }

    // ----- changes on disk -------------------------------------------------------

    /// The file changed on disk (the watcher, or a refused save).
    pub fn disk_changed(&mut self, change: ExternalChange, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            // Still loading: the load reads the current file.
            return;
        };
        let state = &mut doc.state;
        let Some(file) = state.file.as_mut() else {
            return;
        };
        if state.saving.is_some() {
            // Decided once the save is done: it may be the save itself.
            state.deferred = Some(change);
            return;
        }
        match change {
            ExternalChange::Unchanged => {}
            ExternalChange::Touched(disk) => {
                file.accept_metadata(disk);
                if std::mem::take(&mut state.deleted) {
                    state.clear_notice(&[Topic::Deleted]);
                    self.notice_changed(cx);
                }
            }
            ExternalChange::Deleted => {
                if state.deleted && state.shows(Topic::Deleted) {
                    return;
                }
                state.deleted = true;
                state.set_notice(Some(Notice::deleted(true)));
                self.notice_changed(cx);
            }
            ExternalChange::Modified { state: disk, bytes } => {
                if file.baseline().is_some_and(|b| b.hash == disk.hash) {
                    file.accept_metadata(disk);
                    return;
                }
                if state
                    .conflict
                    .as_ref()
                    .is_some_and(|t| t.state.hash == disk.hash)
                {
                    // Known already (a refused save): show the question
                    // again if it was dismissed.
                    if !state.shows(Topic::Conflict) {
                        state.set_notice(Some(Notice::conflict()));
                        self.notice_changed(cx);
                    }
                    return;
                }
                if std::mem::take(&mut state.deleted) {
                    state.clear_notice(&[Topic::Deleted]);
                }
                let current = doc.editor.snapshot();
                state.disk_task = Some(cx.spawn(async move |this, cx| {
                    let prepared = cx
                        .background_spawn(async move { prepare_bytes(&bytes, &current) })
                        .await;
                    this.update(cx, |view, cx| view.disk_version(prepared, disk, cx))
                        .ok();
                }));
            }
        }
        self.after_edit(cx);
    }

    /// The decoded file on disk is ready: reload a clean buffer, or open
    /// the conflict infobar.
    fn disk_version(&mut self, prepared: Prepared, disk: DiskState, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.state.disk_task = None;
        let decoded = match prepared.decoded {
            Ok(decoded) => decoded,
            Err(_) => {
                doc.state.set_notice(Some(Notice::not_text()));
                self.notice_changed(cx);
                return;
            }
        };
        if doc.editor.is_modified() || doc.state.saving.is_some() {
            doc.state.conflict = Some(Theirs {
                decoded,
                state: disk,
            });
            doc.state.set_notice(Some(Notice::conflict()));
            self.notice_changed(cx);
            self.after_edit(cx);
            return;
        }
        if doc.editor.text().revision() != prepared.revision {
            // Edited and undone meanwhile: diff again.
            self.prepare_reload(decoded, disk, cx);
            return;
        }
        self.take_disk_version(decoded, disk, prepared.transaction, cx);
    }

    /// Diffs the buffer against `decoded` off the UI thread, then takes it.
    fn prepare_reload(&mut self, decoded: Decoded, disk: DiskState, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let current = doc.editor.snapshot();
        doc.state.disk_task = Some(cx.spawn(async move |this, cx| {
            let prepared = cx
                .background_spawn(async move { prepare_text(decoded, &current) })
                .await;
            this.update(cx, |view, cx| {
                let Some(doc) = view.doc.as_mut() else {
                    return;
                };
                doc.state.disk_task = None;
                let Ok(decoded) = prepared.decoded else {
                    return;
                };
                if doc.editor.text().revision() == prepared.revision {
                    view.take_disk_version(decoded, disk, prepared.transaction, cx);
                } else {
                    view.prepare_reload(decoded, disk, cx);
                }
            })
            .ok();
        }));
    }

    /// Replaces the text with the file on disk as one undoable edit and
    /// makes it the saved state.
    fn take_disk_version(
        &mut self,
        decoded: Decoded,
        disk: DiskState,
        transaction: Transaction,
        cx: &mut Context<'_, Self>,
    ) {
        self.replace_unjournaled(&transaction, cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.editor.mark_saved();
        if let Some(file) = doc.state.file.as_mut() {
            file.accept_disk_version(&decoded, disk.clone());
        }
        if let Some(worker) = &doc.state.worker {
            worker.send(Msg::Rebase {
                base: doc.editor.snapshot(),
                format: decoded.format,
                hash: disk.hash,
            });
        }
        doc.state.base = doc.editor.snapshot();
        doc.state.autosave.mark_clean();
        doc.state.conflict = None;
        doc.state.deleted = false;
        doc.state.recovering = false;
        doc.state.clear_notice(&[
            Topic::Conflict,
            Topic::Deleted,
            Topic::Recovered,
            Topic::NotText,
        ]);
        cx.emit(DocEvent::Reloaded);
        self.notice_changed(cx);
        self.after_edit(cx);
    }

    /// Applies `transaction` as one undoable command edit that the journal
    /// does not record and autosave does not count.
    fn replace_unjournaled(&mut self, transaction: &Transaction, cx: &mut Context<'_, Self>) {
        if transaction.is_empty() {
            return;
        }
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let edits: Vec<_> = transaction
            .replacements()
            .iter()
            .map(|r| (r.range.clone(), r.text.clone()))
            .collect();
        doc.state.pause.set(true);
        let result = doc.editor.edit(edits, EditKind::Command);
        doc.state.pause.set(false);
        doc.state.edited.set(false);
        if let Err(err) = result {
            tracing::warn!("cannot apply the text from disk: {err}");
        }
        self.pump_syntax(cx);
    }

    // ----- infobar actions ---------------------------------------------------------

    /// Runs an infobar button of this document's notice.
    pub fn notice_action(&mut self, action: &NoticeAction, cx: &mut Context<'_, Self>) {
        match action {
            NoticeAction::SaveAsUtf8 => self.save_as_utf8(cx),
            NoticeAction::KeepMine => self.keep_mine(cx),
            NoticeAction::TakeTheirs => self.take_theirs(cx),
            NoticeAction::KeepRecovered => self.keep_recovered(cx),
            NoticeAction::DiscardRecovered => self.discard_recovered(cx),
            NoticeAction::Recreate => self.save_anyway(
                SaveOptions {
                    on_external_change: OnExternalChange::Overwrite,
                    ..SaveOptions::default()
                },
                cx,
            ),
            NoticeAction::Close => cx.emit(DocEvent::CloseRequested),
            // Compare is not designed yet (its button is disabled); opening
            // a sync-conflict copy is the window's.
            NoticeAction::Compare | NoticeAction::OpenCopy(_) => {}
        }
    }

    /// The infobar's dismiss button.
    pub fn dismiss_notice(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        // Dismissing the recovery question keeps the text; a dismissed save
        // failure stays quiet until it changes; a dismissed conflict or
        // deleted file still holds autosave back.
        doc.state.set_notice(None);
        self.notice_changed(cx);
        self.after_edit(cx);
    }

    fn save_as_utf8(&mut self, cx: &mut Context<'_, Self>) {
        let Some(format) = self
            .doc
            .as_ref()
            .and_then(|d| d.state.file.as_ref())
            .map(DocumentFile::format)
        else {
            return;
        };
        let convert = TextFormat {
            encoding: Encoding::Utf8,
            bom: Bom::Absent,
            line_ending: format.line_ending,
        };
        self.save_anyway(
            SaveOptions {
                convert: Some(convert),
                ..SaveOptions::default()
            },
            cx,
        );
    }

    /// Keeps the edits: the version on disk becomes the base, so the next
    /// save overwrites it deliberately (a further change on disk is still
    /// noticed).
    fn keep_mine(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(theirs) = doc.state.conflict.take() else {
            return;
        };
        if let Some(file) = doc.state.file.as_mut() {
            file.accept_metadata(theirs.state);
        }
        doc.state.clear_notice(&[Topic::Conflict]);
        self.notice_changed(cx);
        self.after_edit(cx);
    }

    /// Takes the version on disk; the edits stay in the undo history.
    fn take_theirs(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(theirs) = doc.state.conflict.take() else {
            return;
        };
        self.prepare_reload(theirs.decoded, theirs.state, cx);
    }

    fn keep_recovered(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.state.recovering = false;
        doc.state.clear_notice(&[Topic::Recovered]);
        self.notice_changed(cx);
        self.after_edit(cx);
    }

    /// Drops the recovered text: back to the file on disk, or, for an
    /// untitled document or a file that no longer exists, closes the tab.
    fn discard_recovered(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let on_disk = doc
            .state
            .file
            .as_ref()
            .and_then(|f| f.baseline().map(|b| (f.format(), b.clone())));
        let Some((format, disk)) = on_disk else {
            if let Some(worker) = &doc.state.worker {
                worker.send(Msg::Discard);
            }
            doc.state.recovering = false;
            doc.state.notice = None;
            doc.editor.mark_saved();
            cx.emit(DocEvent::CloseRequested);
            return;
        };
        let base = doc.state.base.clone();
        let decoded = Decoded {
            text: text_of(&base),
            format,
            line_endings: Default::default(),
            status: DecodeStatus::Exact,
        };
        self.prepare_reload(decoded, disk, cx);
    }

    // ----- recovery ---------------------------------------------------------------------

    /// Applies text recovered from the journal (already journaled again)
    /// as one undoable edit, and opens the recovery infobar.
    pub(super) fn apply_recovered(&mut self, recovered: Recovered, cx: &mut Context<'_, Self>) {
        self.replace_unjournaled(&recovered.transaction, cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.state.autosave.edited();
        doc.state.recovering = true;
        doc.state
            .set_notice(Some(Notice::recovered(recovered.disk)));
        self.notice_changed(cx);
    }

    // ----- closing ------------------------------------------------------------------------

    /// The tab or window is closing, or the app quitting: saves what
    /// autosave may save and syncs the journal. Unsaved text that may not
    /// be saved stays in the journal for the next start (closing with
    /// unsaved changes has no designed dialog). The receiver fires once
    /// the document's queue is done.
    pub fn prepare_close(&mut self, cx: &mut Context<'_, Self>) -> Option<oneshot::Receiver<()>> {
        self.after_edit(cx);
        let doc = self.doc.as_mut()?;
        let worker = doc.state.worker.clone()?;
        if doc.editor.is_modified() {
            match doc.state.autosave.quitting() {
                QuitDecision::Save(request) => {
                    self.start_save(request, SaveOptions::default(), cx);
                }
                QuitDecision::Clean | QuitDecision::WaitForSave => {}
                QuitDecision::Ask(why) => tracing::info!(
                    "closing with unsaved changes ({why:?}): they stay in the recovery journal"
                ),
            }
        }
        Some(worker.flush())
    }
}

/// `text` as a sentence: capitalised, ending with a full stop.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

#[cfg(test)]
impl EditorView {
    /// Puts one caret at `offset`.
    pub(crate) fn set_cursor_for_tests(&mut self, offset: usize, cx: &mut Context<'_, Self>) {
        if let Some(doc) = self.doc.as_mut() {
            doc.editor.set_cursor(offset);
        }
        cx.notify();
    }

    /// The encoding the next save writes.
    pub(crate) fn disk_encoding_for_tests(&self) -> Option<Encoding> {
        Some(self.doc.as_ref()?.state.file.as_ref()?.format().encoding)
    }
}

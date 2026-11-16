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
//!    conflict infobar (Compare, Keep mine, Take theirs). A deleted file
//!    gets an infobar with Save, Save as and Close. A file whose
//!    permissions turn read-only gets the read-only infobar, and editing
//!    is off until they allow writing again.
//! 6. **Compare** ([`crate::compare`]): the conflict's, the recovery's or
//!    a sync-conflict copy's compare view replaces the document on screen
//!    until it is applied (one undoable, journaled edit) or cancelled.
//! 7. **Closing:** the window asks before closing a document that would
//!    not be saved on its own ([`EditorView::needs_close_answer`]); *Don't
//!    save* drops its journal ([`EditorView::discard_changes`]).
//! 5. **Save As** (and Save on an untitled document): the window asks for
//!    a path ([`crate::files::save_as`]) and hands it to
//!    [`EditorView::save_as`]. The worker writes the new file and moves the
//!    journal to the new path's key; on success the view follows the new
//!    path and tells the window ([`DocEvent::SavedAs`]), which re-points
//!    the tab.

use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::channel::oneshot;
use gpui::{AppContext as _, Context, Entity, SharedString, Task};
use gv_editor::{EditKind, SavePoint};
use gv_syntax::{ParseOptions, SyntaxTree};
use gv_text::{
    Bom, Buffer, DecodeError, DecodeStatus, Decoded, Encoding, LineEnding, Revision, TextFormat,
    TextSnapshot, Transaction,
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
use crate::compare::{CompareView, Comparison, Sides};
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
    /// The document should be saved under a path the user chooses: Save
    /// on an untitled document, or an infobar's Save as.
    SaveAsRequested,
    /// Save As succeeded: the document now lives at [`EditorView::path`].
    SavedAs,
    /// The user closed the document from its infobar.
    CloseRequested,
    /// The file stopped blocking edits (its permissions allow writing
    /// again, or it was converted): the window decides again whether the
    /// view may be edited (viewer mode).
    EditableChanged,
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
    /// Where it writes.
    path: PathBuf,
    /// The text it writes.
    text: TextSnapshot,
    /// A Save As: the document moves to `path` once written.
    save_as: Option<SaveAsTarget>,
}

/// Where Save As writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SaveAsTarget {
    /// The absolute path.
    pub path: PathBuf,
    /// The user confirmed replacing a file there (the save dialog asks);
    /// otherwise an existing file is not replaced.
    pub overwrite: bool,
}

/// What a compare view compares the buffer with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComparePurpose {
    /// The file on disk that changed while the buffer had unsaved edits.
    Conflict,
    /// The file on disk, against the text recovered from the journal.
    Recovered,
    /// A sync-conflict copy of the file.
    Copy(PathBuf),
}

/// A compare view on screen.
struct OpenCompare {
    view: Entity<CompareView>,
    purpose: ComparePurpose,
    /// The text it was made from.
    revision: Revision,
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
    /// A Save As asked for while a save ran.
    save_as_again: Option<SaveAsTarget>,
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
    /// The file may not be edited (permissions, size, encoding): the view
    /// is read-only whatever viewer mode says.
    blocked: bool,
    /// The compare view on screen.
    compare: Option<OpenCompare>,
    compare_task: Option<Task<()>>,
    /// Told when no save is running any more.
    save_waiters: Vec<oneshot::Sender<()>>,
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
            save_as_again: None,
            deferred: None,
            conflict: None,
            deleted: false,
            recovering: false,
            base,
            notice: None,
            last_error: None,
            modified: false,
            blocked: false,
            compare: None,
            compare_task: None,
            save_waiters: Vec::new(),
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
                && !d.state.blocked
                && d.state.autosave.settings().enabled
        })
    }

    /// Whether closing must ask first: the text is modified and would not
    /// be saved on its own (untitled, autosave off or held back, a file
    /// that may not be written, or a save that failed last time).
    pub fn needs_close_answer(&self) -> bool {
        self.doc.as_ref().is_some_and(|d| {
            d.editor.is_modified() && (!self.saves_on_close() || d.state.last_error.is_some())
        })
    }

    /// Fires once no save is running (at once when none is).
    pub fn when_saved(&mut self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        match self.doc.as_mut() {
            Some(doc) if doc.state.saving.is_some() => doc.state.save_waiters.push(tx),
            _ => {
                let _ = tx.send(());
            }
        }
        rx
    }

    /// *Don't save* before closing: the unsaved text is dropped with its
    /// journal and the text counts as saved, so closing writes nothing.
    /// The receiver fires once the journal is gone.
    pub fn discard_changes(&mut self, cx: &mut Context<'_, Self>) -> Option<oneshot::Receiver<()>> {
        let doc = self.doc.as_mut()?;
        doc.state.recovering = false;
        doc.state.conflict = None;
        doc.state.compare = None;
        doc.editor.mark_saved();
        doc.state.autosave.mark_clean();
        let wait = doc.state.worker.as_ref().map(|worker| {
            worker.send(Msg::Discard);
            worker.flush()
        });
        self.report_modified(cx);
        cx.notify();
        wait
    }

    /// Whether the file blocks edits (permissions, size, encoding).
    pub fn file_blocks_edits(&self) -> bool {
        self.doc.as_ref().is_some_and(|d| d.state.blocked)
    }

    /// The compare view on screen, if one is open.
    pub fn compare_view(&self) -> Option<Entity<CompareView>> {
        Some(self.doc.as_ref()?.state.compare.as_ref()?.view.clone())
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
            // Untitled documents live in the journal until saved as.
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

    /// Mod+S. An untitled document has no file yet: the window is asked
    /// for a Save As.
    pub fn save(&mut self, cx: &mut Context<'_, Self>) {
        self.after_edit(cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.state.untitled() {
            cx.emit(DocEvent::SaveAsRequested);
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
            text: text.clone(),
            options,
            reply,
        });
        doc.state.saving = Some(Saving {
            request,
            point,
            path,
            text,
            save_as: None,
        });
        self.await_save(answer, cx);
    }

    fn await_save(&mut self, answer: oneshot::Receiver<SaveReply>, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.state.save_task = Some(cx.spawn(async move |this, cx| {
            let reply = answer.await.ok();
            this.update(cx, |view, cx| view.save_finished(reply, cx))
                .ok();
        }));
    }

    // ----- Save As --------------------------------------------------------------

    /// Saves the text to `target` (Save As, or an untitled document's first
    /// save) and, once written, makes it this document's file. A view still
    /// loading saves once loaded; a save running finishes first. Fails with
    /// an error infobar when another tab edits the file at `target`.
    pub(crate) fn save_as(&mut self, target: SaveAsTarget, cx: &mut Context<'_, Self>) {
        let target = SaveAsTarget {
            path: std::path::absolute(&target.path).unwrap_or(target.path),
            ..target
        };
        if self.doc.is_none() {
            self.save_as_after_load = Some(target);
            return;
        }
        self.after_edit(cx);
        // Both absolute.
        let same = self.path.as_deref() == Some(target.path.as_path());
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.state.key.is_none() || doc.state.worker.is_none() {
            return;
        }
        if doc.state.saving.is_some() {
            doc.state.save_as_again = Some(target);
            return;
        }
        if same {
            // Its own file: a plain save, even of unchanged text.
            self.save_anyway(SaveOptions::default(), cx);
            return;
        }
        let key = DocumentKey::Path(target.path.clone());
        let claim = files::claim(cx, &key);
        if claim.is_none() && files::store(cx).is_some() {
            self.show_save_as_error(Notice::save_as_open_elsewhere(&file_name(&target.path)), cx);
            return;
        }
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(worker) = doc.state.worker.clone() else {
            return;
        };
        let request = doc.state.autosave.manual().unwrap_or(SaveRequest {
            reason: SaveReason::Manual,
            generation: 0,
        });
        let point = doc.editor.save_point();
        let text = doc.editor.snapshot();
        let file = doc.state.file.clone();
        let format = file
            .as_ref()
            .map_or_else(TextFormat::default, DocumentFile::format);
        if let Some(watch) = &self.watch {
            watch.begin_write(&target.path);
        }
        let options = SaveOptions {
            on_external_change: if target.overwrite {
                OnExternalChange::Overwrite
            } else {
                OnExternalChange::Refuse
            },
            ..SaveOptions::default()
        };
        let (reply, answer) = oneshot::channel();
        worker.send(Msg::SaveAs {
            file,
            path: target.path.clone(),
            format,
            text: text.clone(),
            options,
            claim,
            reply,
        });
        doc.state.saving = Some(Saving {
            request,
            point,
            path: target.path.clone(),
            text,
            save_as: Some(target),
        });
        self.await_save(answer, cx);
    }

    /// Shows a Save As failure in the error infobar (also the window's: the
    /// dialog could not be shown, the file is open in another tab).
    pub(crate) fn show_save_as_error(&mut self, notice: Notice, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.state.last_error = Some(notice.clone());
        doc.state.set_notice(Some(notice));
        self.notice_changed(cx);
    }

    /// Save As wrote the file: the document lives there from now on.
    fn saved_as(&mut self, file: DocumentFile, saving: Saving, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let path = saving.path;
        let baseline = file.baseline().cloned();
        doc.state.key = Some(DocumentKey::Path(path.clone()));
        doc.state.file = Some(file);
        doc.editor.mark_saved_at(saving.point);
        doc.state.autosave.save_succeeded(saving.request);
        doc.state.base = saving.text;
        // What was pending concerned the old file.
        doc.state.deferred = None;
        doc.state.conflict = None;
        doc.state.deleted = false;
        doc.state.recovering = false;
        doc.state.last_error = None;
        doc.state.set_notice(None);
        if let Some(watch) = &self.watch {
            match baseline {
                Some(state) => watch.expect_write(&path, state),
                None => watch.cancel_write(&path),
            }
        }
        self.links = super::LinkStatuses::new(Some(&path), None, self.links.profile());
        self.path = Some(path);
        self.untitled = None;
        cx.emit(DocEvent::Saved);
        cx.emit(DocEvent::SavedAs);
        self.notice_changed(cx);
    }

    fn save_finished(&mut self, reply: Option<SaveReply>, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(saving) = doc.state.saving.take() else {
            return;
        };
        doc.state.save_task = None;
        let save_as = saving.save_as.clone();
        match reply.map(|r| r.result) {
            Some(Ok((file, _))) if save_as.is_some() => self.saved_as(file, saving, cx),
            Some(Err(err)) if save_as.is_some() => {
                doc.state.autosave.save_failed(saving.request);
                if let Some(watch) = &self.watch {
                    watch.cancel_write(&saving.path);
                }
                match err {
                    // Only when not told to overwrite: `.md` was added to
                    // the name the dialog asked about.
                    SaveError::ExternalChange { .. } => self
                        .show_save_as_error(Notice::save_as_exists(&file_name(&saving.path)), cx),
                    err => self.save_failed(err, SaveReason::Manual, cx),
                }
            }
            Some(Ok((file, _))) => {
                let baseline = file.baseline().cloned();
                doc.state.file = Some(file);
                doc.editor.mark_saved_at(saving.point);
                doc.state.autosave.save_succeeded(saving.request);
                // The base of a later 3-way merge.
                doc.state.base = saving.text;
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
                // Saved as UTF-8, a file that was not valid in its
                // encoding may be edited now.
                self.check_file_blocks(cx);
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
        if let Some(target) = self.doc.as_mut().and_then(|d| d.state.save_as_again.take()) {
            self.save_as(target, cx);
        }
        if let Some(doc) = self.doc.as_mut()
            && doc.state.saving.is_none()
        {
            for waiter in std::mem::take(&mut doc.state.save_waiters) {
                let _ = waiter.send(());
            }
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
            SaveError::ReadOnly(reason @ ReadOnlyReason::Permissions) => {
                Notice::read_only(sentence(&reason.to_string()))
            }
            SaveError::ReadOnly(reason) => Notice::save_failed(sentence(&reason.to_string())),
            SaveError::Write(WriteError::ReadOnly { .. }) => {
                Notice::read_only("The file is read-only.".to_owned())
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
        let writable = match &change {
            ExternalChange::Touched(disk) | ExternalChange::Modified { state: disk, .. } => {
                Some(!disk.read_only)
            }
            ExternalChange::Deleted | ExternalChange::Unchanged => None,
        };
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
        if let Some(writable) = writable {
            self.permissions_changed(writable, cx);
        }
        self.after_edit(cx);
    }

    /// The file's permissions as last seen on disk: read-only blocks
    /// editing (plan 11 § External changes) until they allow writing
    /// again.
    fn permissions_changed(&mut self, writable: bool, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let reason = doc.state.file.as_ref().and_then(|f| {
            let mut reason = f.read_only_reason();
            if matches!(reason, Some(ReadOnlyReason::Permissions)) && writable {
                reason = None;
            }
            if reason.is_none() && !writable {
                reason = Some(ReadOnlyReason::Permissions);
            }
            reason
        });
        self.set_blocked(reason, cx);
    }

    /// Blocks editing for `reason`, with the read-only infobar, or stops
    /// blocking it.
    fn set_blocked(&mut self, reason: Option<ReadOnlyReason>, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let blocked = reason.is_some();
        if doc.state.blocked == blocked {
            return;
        }
        doc.state.blocked = blocked;
        match reason {
            Some(reason) => {
                self.read_only = true;
                let encoding = doc
                    .state
                    .file
                    .as_ref()
                    .map_or(Encoding::Utf8, |f| f.format().encoding);
                // A question already open (recovery, a conflict) stays.
                if doc.state.notice.is_none()
                    || doc.state.shows(Topic::LargeFile)
                    || doc.state.shows(Topic::SaveFailed)
                {
                    doc.state
                        .set_notice(Some(Notice::read_only_file(&reason, encoding)));
                }
            }
            None => {
                doc.state.clear_notice(&[Topic::ReadOnly]);
                // The window decides again (viewer mode).
                self.read_only = false;
                cx.emit(DocEvent::EditableChanged);
            }
        }
        self.notice_changed(cx);
    }

    /// Blocks or unblocks editing for what the file is now.
    fn check_file_blocks(&mut self, cx: &mut Context<'_, Self>) {
        let reason = self
            .doc
            .as_ref()
            .and_then(|d| d.state.file.as_ref())
            .and_then(DocumentFile::read_only_reason);
        self.set_blocked(reason, cx);
    }

    /// The document finished loading: a file that may not be edited gets
    /// the read-only infobar and blocks editing; a large one gets the
    /// large-file infobar. A recovery question comes first: these follow
    /// once it is answered.
    pub fn show_file_state(&mut self, cx: &mut Context<'_, Self>) {
        self.check_file_blocks(cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.large_file && doc.state.notice.is_none() {
            doc.state.set_notice(Some(Notice::large_file()));
            self.notice_changed(cx);
        }
    }

    /// Shows the read-only or large-file infobar again after a question
    /// that hid it was answered.
    fn restore_file_state_notice(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.state.notice.is_some() {
            return;
        }
        let reason = doc
            .state
            .file
            .as_ref()
            .and_then(DocumentFile::read_only_reason);
        let notice = match reason {
            Some(reason) if doc.state.blocked => {
                let encoding = doc
                    .state
                    .file
                    .as_ref()
                    .map_or(Encoding::Utf8, |f| f.format().encoding);
                Some(Notice::read_only_file(&reason, encoding))
            }
            _ => doc.large_file.then(Notice::large_file),
        };
        if notice.is_some() {
            doc.state.set_notice(notice);
            self.notice_changed(cx);
        }
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
            // A compare view of the older version is out of date.
            if doc
                .state
                .compare
                .as_ref()
                .is_some_and(|c| c.purpose == ComparePurpose::Conflict)
            {
                doc.state.compare = None;
            }
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
        let was_recovering = std::mem::take(&mut doc.state.recovering);
        doc.state.clear_notice(&[
            Topic::Conflict,
            Topic::Deleted,
            Topic::Recovered,
            Topic::NotText,
        ]);
        if was_recovering {
            self.restore_file_state_notice(cx);
        }
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
            NoticeAction::SaveAs => cx.emit(DocEvent::SaveAsRequested),
            NoticeAction::Close => cx.emit(DocEvent::CloseRequested),
            NoticeAction::Compare => self.compare_with_disk(cx),
            NoticeAction::Details => {
                if let Some(doc) = self.doc.as_mut() {
                    doc.state.set_notice(Some(Notice::large_file_details()));
                }
                self.notice_changed(cx);
            }
            // Opening or comparing a sync-conflict copy and switching the
            // profile are the window's.
            NoticeAction::CompareCopy(_)
            | NoticeAction::OpenCopy(_)
            | NoticeAction::SwitchProfile => {}
            NoticeAction::Revert => self.revert(cx),
            // The window's questions.
            NoticeAction::OpenUrl(_)
            | NoticeAction::OpenExternally(_)
            | NoticeAction::Reveal(_)
            | NoticeAction::IndexAnyway
            | NoticeAction::IndexFolder(_)
            | NoticeAction::SaveAndClose
            | NoticeAction::DiscardAndClose
            | NoticeAction::Cancel => {}
        }
    }

    // ----- compare --------------------------------------------------------------------

    /// The infobar's Compare: the conflict's version on disk (3-way,
    /// against the text last loaded or saved), or, while the recovery
    /// question is open, the file on disk against the recovered text.
    fn compare_with_disk(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let sides = |left: &str, right: &str| Sides {
            left: SharedString::from(left.to_owned()),
            right: SharedString::from(right.to_owned()),
        };
        let mine = doc.editor.snapshot();
        let ending = line_ending_of(doc.state.file.as_ref());
        if let Some(theirs) = &doc.state.conflict {
            let base = doc.state.base.clone();
            let theirs = theirs.decoded.text.clone();
            let sides = sides("Your version", "On disk");
            self.open_compare(ComparePurpose::Conflict, cx, move || {
                Comparison::three_way(&text_of(&base), &text_of(&mine), &theirs, sides, ending)
            });
        } else if doc.state.recovering {
            let disk = doc.state.base.clone();
            let sides = sides("Recovered", "On disk");
            self.open_compare(ComparePurpose::Recovered, cx, move || {
                Comparison::two_way(&text_of(&mine), &text_of(&disk), sides, ending)
            });
        }
    }

    /// Compares the buffer with the sync-conflict copy at `copy`, read off
    /// the UI thread. The copy is only read.
    pub fn compare_with_copy(&mut self, copy: PathBuf, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let mine = doc.editor.snapshot();
        let ending = line_ending_of(doc.state.file.as_ref());
        let sides = Sides {
            left: "This file".into(),
            right: file_name(&copy).into(),
        };
        let revision = mine.revision();
        let purpose = ComparePurpose::Copy(copy.clone());
        doc.state.compare_task = Some(cx.spawn(async move |this, cx| {
            let made = cx
                .background_spawn(async move {
                    let (_, decoded) = DocumentFile::load(&copy).map_err(|e| e.to_string())?;
                    Ok::<_, String>(Comparison::two_way(
                        &text_of(&mine),
                        &decoded.text,
                        sides,
                        ending,
                    ))
                })
                .await;
            this.update(cx, |view, cx| match made {
                Ok(comparison) => view.show_compare(purpose, revision, comparison, cx),
                Err(reason) => {
                    if let Some(doc) = view.doc.as_mut() {
                        doc.state.compare_task = None;
                        doc.state.set_notice(Some(Notice::compare_failed(&reason)));
                    }
                    view.notice_changed(cx);
                }
            })
            .ok();
        }));
    }

    /// Makes the comparison off the UI thread, then shows it.
    fn open_compare(
        &mut self,
        purpose: ComparePurpose,
        cx: &mut Context<'_, Self>,
        make: impl FnOnce() -> Comparison + Send + 'static,
    ) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let revision = doc.editor.text().revision();
        doc.state.compare_task = Some(cx.spawn(async move |this, cx| {
            let comparison = cx.background_spawn(async move { make() }).await;
            this.update(cx, |view, cx| {
                view.show_compare(purpose, revision, comparison, cx);
            })
            .ok();
        }));
    }

    fn show_compare(
        &mut self,
        purpose: ComparePurpose,
        revision: Revision,
        comparison: Comparison,
        cx: &mut Context<'_, Self>,
    ) {
        let this = cx.weak_entity();
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.state.compare_task = None;
        let title = match &purpose {
            ComparePurpose::Conflict => "Compare with the file on disk".to_owned(),
            ComparePurpose::Recovered => "Compare the recovered text with the file".to_owned(),
            ComparePurpose::Copy(copy) => format!("Compare with “{}”", file_name(copy)),
        };
        let view = cx.new(|_| CompareView::new(comparison, title.into(), this));
        doc.state.compare = Some(OpenCompare {
            view,
            purpose,
            revision,
        });
        self.notice_changed(cx);
    }

    /// The compare view closed: with `result`, the buffer takes it as one
    /// undoable edit and the question that opened it is answered (a
    /// conflict as with *Keep mine*: the next save overwrites the version
    /// on disk; the recovery as with *Restore*). Without, nothing changes.
    pub(crate) fn finish_compare(&mut self, result: Option<String>, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(open) = doc.state.compare.take() else {
            return;
        };
        let Some(result) = result else {
            self.notice_changed(cx);
            return;
        };
        if doc.editor.text().revision() != open.revision {
            // The text changed under the view: compare again rather than
            // drop that change.
            match open.purpose {
                ComparePurpose::Copy(copy) => self.compare_with_copy(copy, cx),
                ComparePurpose::Conflict | ComparePurpose::Recovered => {
                    self.compare_with_disk(cx);
                }
            }
            return;
        }
        let current = text_of(&doc.editor.snapshot());
        let transaction = reload_transaction(&current, &result);
        self.apply_edit(&transaction, cx);
        match open.purpose {
            ComparePurpose::Conflict => self.keep_mine(cx),
            ComparePurpose::Recovered => self.keep_recovered(cx),
            ComparePurpose::Copy(_) => {}
        }
        self.notice_changed(cx);
        self.after_edit(cx);
    }

    /// Applies `transaction` as one undoable, journaled command edit.
    fn apply_edit(&mut self, transaction: &Transaction, cx: &mut Context<'_, Self>) {
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
        if let Err(err) = doc.editor.edit(edits, EditKind::Command) {
            tracing::warn!("cannot apply the compared text: {err}");
        }
        self.pump_syntax(cx);
    }

    /// `file.revert`: reads the file again (off the UI thread) and takes it
    /// as one undoable edit, so Undo brings the dropped edits back. The
    /// window asks first when there are unsaved edits. An untitled
    /// document has nothing to revert to.
    pub fn revert(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let Some(file) = doc.state.file.as_ref() else {
            return;
        };
        let path = file.path().to_path_buf();
        let encoding = file.format().encoding;
        doc.state.disk_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move { DocumentFile::load_with_encoding(path, encoding) })
                .await;
            this.update(cx, |view, cx| view.reverted(loaded, cx)).ok();
        }));
    }

    fn reverted(
        &mut self,
        loaded: Result<(DocumentFile, Decoded), LoadError>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.state.disk_task = None;
        let (file, decoded) = match loaded {
            Ok(loaded) => loaded,
            Err(err) => {
                doc.state
                    .set_notice(Some(Notice::revert_failed(&err.to_string())));
                self.notice_changed(cx);
                return;
            }
        };
        let Some(disk) = file.baseline().cloned() else {
            return;
        };
        doc.state.conflict = None;
        self.prepare_reload(decoded, disk, cx);
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
        // The version on disk is what both sides share from now on.
        doc.state.base = Buffer::new(&theirs.decoded.text).snapshot();
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
        self.restore_file_state_notice(cx);
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
    /// autosave may save and syncs the journal. The window asked about
    /// unsaved text that would not be saved first
    /// ([`Self::needs_close_answer`]); what is left stays in the journal
    /// for the next start. The receiver fires once the document's queue
    /// is done.
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

/// The line ending a compare view writes for its markers: the file's.
fn line_ending_of(file: Option<&DocumentFile>) -> LineEnding {
    file.map_or(LineEnding::Lf, |f| f.format().line_ending)
}

/// The file name of `path`, for messages.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
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

    /// Undoes the last edit, as Mod+Z does.
    pub(crate) fn undo_for_tests(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(doc) = self.doc.as_mut() {
            doc.editor.undo();
        }
        self.pump_syntax(cx);
        self.after_edit(cx);
        cx.notify();
    }
}

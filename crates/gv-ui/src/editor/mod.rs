// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view (Edit mode): the text editor of one tab.
//!
//! # Pipeline
//!
//! 1. [`EditorView::open`] reads, decodes and parses the file on the
//!    background executor; the text keeps its [`gv_text::TextFormat`]. The
//!    view then owns a [`gv_editor::Editor`] over it.
//! 2. Every edit is one `gv-editor` transaction. An editor observer queues
//!    its summary, and [`syntax`] reparses incrementally: about 1 ms on the
//!    UI thread, the rest of a long reparse on the background executor.
//! 3. [`highlight`] colours the lines around the rows on screen from the
//!    syntax tree (SPEC § Editor); link targets take their resolution
//!    status from the window's path index.
//! 4. One element ([`element`]) covers the view. It shapes only the visible
//!    rows ([`rows`], one GPUI shaped line per buffer line, kept while the
//!    line and its highlighting stay the same) and draws the gutter, the
//!    current line, selections, carets, the composition underline, link
//!    underlines and the scrollbar.
//!
//! Display rows are buffer rows: soft wrap waits for the display map's
//! wrap layer, so long lines scroll sideways instead. With or without side
//! panels the text uses the Reading view's centred column, so text does
//! not move sideways when the mode changes; split view, which starts at
//! the pane's edge, is not built yet.
//!
//! # Input
//!
//! - Text and input-method composition come through GPUI's input handler
//!   ([`input`]), which answers every query from the current buffer, never
//!   from the last paint (the IME spike's rule).
//! - Caret motion, deletion and the like are the view's own keys
//!   ([`key_bindings`]); undo, clipboard and select all are keymap
//!   commands. Bare printable keys are never bound here.
//! - Enter, Shift+Enter, Tab, Shift+Tab and the formatting commands are
//!   the Markdown commands of the tab's profile ([`markdown`]): list and
//!   quote continuation, indentation, emphasis, headings, links. A command
//!   that cannot run (strikethrough in CommonMark, formatting in a code
//!   block) has no handler, so its menu item is disabled and its key falls
//!   through.
//! - Mouse: click places the caret, drag selects, Shift-click extends,
//!   double click selects a word and triple click a line (dragging after
//!   either extends by words or lines), Alt-click adds a caret and
//!   Alt-drag selects a column (07 § Buffer, selections and undo); the
//!   wheel scrolls, sideways with Shift.
//!
//! # The file
//!
//! [`file`] holds the document's file side: the crash journal every
//! edit goes to, saving (Mod+S and autosave), changes made on disk by other
//! programs and crash recovery ([`crate::files`]). When the tab goes back
//! to Preview, the Reading view is rebuilt from the editor's text
//! ([`EditorView::take_preview_text`]), saved or not.

mod element;
mod file;
mod highlight;
mod input;
mod layout;
mod markdown;
mod rows;
mod syntax;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem, Context, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, Task, Window, div, px,
};
use gv_editor::{DeleteUnit, Editor, EditorEvent, MarkdownCommands, Motion};
use gv_syntax::{ParseOptions, SyntaxTree};
use gv_text::{Buffer, Revision};
use gv_vault::PathIndex;
use gv_vault::files::journal::DocumentKey;

use crate::appearance::Appearance;
use crate::files::watch::WatchHandle;
use crate::files::worker::{Msg, Worker};
use crate::files::{self, ExecutorClock};
use crate::inline::FontFamilies;
use crate::keymap::actions::editor as commands;
use crate::metrics::size::document;
use crate::metrics::spacing::document as spacing;
use crate::metrics::text;
use element::{EditorElement, Press, Scrollbar};
pub use file::DocEvent;
use file::{DocState, Loaded};
use highlight::{HighlightCache, LinkStatuses};
use markdown::Availabilities;
use rows::{Fonts, RowCache};
use syntax::{Background, EditQueue, Syntax, UI_BUDGET};

gpui::actions!(
    source,
    [
        /// Move the carets one character left.
        MoveLeft,
        /// Move the carets one character right.
        MoveRight,
        /// Move the carets one row up.
        MoveUp,
        /// Move the carets one row down.
        MoveDown,
        /// Move the carets to the previous word start.
        MoveWordLeft,
        /// Move the carets to the next word end.
        MoveWordRight,
        /// Move the carets to the first non-blank character, then the line
        /// start.
        MoveLineStart,
        /// Move the carets to the line end.
        MoveLineEnd,
        /// Move the carets to the start of the text.
        MoveDocumentStart,
        /// Move the carets to the end of the text.
        MoveDocumentEnd,
        /// Move the carets and the view up by a page.
        MovePageUp,
        /// Move the carets and the view down by a page.
        MovePageDown,
        /// Extend the selections one character left.
        SelectLeft,
        /// Extend the selections one character right.
        SelectRight,
        /// Extend the selections one row up.
        SelectUp,
        /// Extend the selections one row down.
        SelectDown,
        /// Extend the selections to the previous word start.
        SelectWordLeft,
        /// Extend the selections to the next word end.
        SelectWordRight,
        /// Extend the selections to the line start.
        SelectLineStart,
        /// Extend the selections to the line end.
        SelectLineEnd,
        /// Extend the selections to the start of the text.
        SelectDocumentStart,
        /// Extend the selections to the end of the text.
        SelectDocumentEnd,
        /// Extend the selections up by a page.
        SelectPageUp,
        /// Extend the selections down by a page.
        SelectPageDown,
        /// Delete the selections, or the character before each caret.
        Backspace,
        /// Delete the selections, or the character after each caret.
        Delete,
        /// Delete to the previous word start.
        DeleteWordLeft,
        /// Delete to the next word end.
        DeleteWordRight,
        /// Delete to the line start.
        DeleteLineLeft,
        /// Delete to the line end.
        DeleteLineRight,
        /// Add a caret one row above the topmost one.
        AddCursorAbove,
        /// Add a caret one row below the bottommost one.
        AddCursorBelow,
        /// Cancel a composition, else keep only the primary selection, else
        /// collapse it.
        Cancel,
    ]
);

/// The key context of the Source view (the keymap's `editor` context).
pub(crate) const KEY_CONTEXT: &str = "Editor";

/// The Source view's own keys: caret motion, selection, deletion, extra
/// carets. Undo, the clipboard, select all, Enter, Tab and the Markdown
/// commands are commands of the keymap ([`markdown`]).
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let context = Some(KEY_CONTEXT);
    let mut keys = vec![
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("home", MoveLineStart, context),
        KeyBinding::new("end", MoveLineEnd, context),
        KeyBinding::new("pageup", MovePageUp, context),
        KeyBinding::new("pagedown", MovePageDown, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("shift-home", SelectLineStart, context),
        KeyBinding::new("shift-end", SelectLineEnd, context),
        KeyBinding::new("shift-pageup", SelectPageUp, context),
        KeyBinding::new("shift-pagedown", SelectPageDown, context),
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("escape", Cancel, context),
    ];
    if cfg!(target_os = "macos") {
        keys.extend([
            KeyBinding::new("alt-left", MoveWordLeft, context),
            KeyBinding::new("alt-right", MoveWordRight, context),
            KeyBinding::new("cmd-left", MoveLineStart, context),
            KeyBinding::new("cmd-right", MoveLineEnd, context),
            KeyBinding::new("cmd-up", MoveDocumentStart, context),
            KeyBinding::new("cmd-down", MoveDocumentEnd, context),
            KeyBinding::new("alt-shift-left", SelectWordLeft, context),
            KeyBinding::new("alt-shift-right", SelectWordRight, context),
            KeyBinding::new("cmd-shift-left", SelectLineStart, context),
            KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
            KeyBinding::new("cmd-shift-up", SelectDocumentStart, context),
            KeyBinding::new("cmd-shift-down", SelectDocumentEnd, context),
            KeyBinding::new("alt-backspace", DeleteWordLeft, context),
            KeyBinding::new("alt-delete", DeleteWordRight, context),
            KeyBinding::new("cmd-backspace", DeleteLineLeft, context),
            KeyBinding::new("cmd-delete", DeleteLineRight, context),
            KeyBinding::new("cmd-alt-up", AddCursorAbove, context),
            KeyBinding::new("cmd-alt-down", AddCursorBelow, context),
        ]);
    } else {
        keys.extend([
            KeyBinding::new("ctrl-left", MoveWordLeft, context),
            KeyBinding::new("ctrl-right", MoveWordRight, context),
            KeyBinding::new("ctrl-home", MoveDocumentStart, context),
            KeyBinding::new("ctrl-end", MoveDocumentEnd, context),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, context),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, context),
            KeyBinding::new("ctrl-shift-home", SelectDocumentStart, context),
            KeyBinding::new("ctrl-shift-end", SelectDocumentEnd, context),
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, context),
            KeyBinding::new("ctrl-delete", DeleteWordRight, context),
        ]);
        // Linux desktops take Ctrl+Alt+↑/↓ for workspaces.
        let (above, below) = if cfg!(target_os = "windows") {
            ("ctrl-alt-up", "ctrl-alt-down")
        } else {
            ("shift-alt-up", "shift-alt-down")
        };
        keys.extend([
            KeyBinding::new(above, AddCursorAbove, context),
            KeyBinding::new(below, AddCursorBelow, context),
        ]);
    }
    keys
}

/// Where the view is with its document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorStatus {
    Loading,
    Ready,
    /// The file could not be opened; why.
    Failed(SharedString),
}

/// A document being edited.
pub(crate) struct Doc {
    editor: Editor,
    /// Edits waiting for the parser.
    queue: EditQueue,
    syntax: Syntax,
    highlights: HighlightCache,
    rows: RowCache,
    /// The file, journal and autosave side.
    state: DocState,
}

impl std::fmt::Debug for Doc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Doc")
            .field("editor", &self.editor)
            .field("syntax", &self.syntax)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// A line to scroll to the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Top {
    /// The line holding this source offset.
    Offset(usize),
    /// A 0-based line.
    Line(usize),
}

/// The Source view of one tab.
pub struct EditorView {
    focus: FocusHandle,
    path: Option<PathBuf>,
    /// The number of an untitled document.
    untitled: Option<u64>,
    /// The window's watchers, told about every save.
    watch: Option<WatchHandle>,
    status: EditorStatus,
    doc: Option<Doc>,
    options: ParseOptions,
    /// The Markdown commands of the profile.
    commands: MarkdownCommands,
    /// Which of them can run now.
    availabilities: Availabilities,
    /// Vertical scroll in px from the top of the content (f64: a long
    /// file is millions of px tall).
    scroll_y: f64,
    /// Sideways scroll of the text column, in px.
    scroll_x: f32,
    /// The line to put at the top once the text is there.
    pending_top: Option<Top>,
    /// Bring the primary caret into view in the next frame.
    autoscroll: bool,
    /// The bounds of the last layout; geometry queries between frames
    /// compute from them and the current text.
    bounds: Option<Bounds<Pixels>>,
    /// The widest row shaped, for the sideways scroll limit.
    widest: f32,
    show_whitespace: bool,
    links: LinkStatuses,
    fonts: Fonts,
    /// The advance of a digit at the line number size and of a space at
    /// the text size, measured once.
    advances: Option<(f32, f32)>,
    press: Press,
    /// The scrollbar as last drawn.
    scrollbar: Option<Scrollbar>,
    /// The text revision the Reading view was last given.
    handed_off: Option<Revision>,
    #[cfg(test)]
    probe: element::Probe,
    _load: Option<Task<()>>,
    _reparse: Option<Task<()>>,
}

impl std::fmt::Debug for EditorView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorView")
            .field("path", &self.path)
            .field("status", &self.status)
            .field("doc", &self.doc)
            .finish_non_exhaustive()
    }
}

impl EditorView {
    /// A view with nothing loaded.
    pub fn new(options: ParseOptions, cx: &mut Context<'_, Self>) -> Self {
        let families = FontFamilies::from_app(cx);
        Self {
            focus: cx.focus_handle(),
            path: None,
            untitled: None,
            watch: None,
            status: EditorStatus::Loading,
            doc: None,
            options,
            commands: MarkdownCommands::for_profile(crate::reader::PROFILE.as_str()),
            availabilities: Availabilities::default(),
            scroll_y: 0.0,
            scroll_x: 0.0,
            pending_top: None,
            autoscroll: false,
            bounds: None,
            widest: 0.0,
            show_whitespace: false,
            links: LinkStatuses::new(None, None),
            fonts: Fonts::new(&families),
            advances: None,
            press: Press::None,
            scrollbar: None,
            handed_off: None,
            #[cfg(test)]
            probe: element::Probe::default(),
            _load: None,
            _reparse: None,
        }
    }

    /// The parse options of the Reading view's profile.
    pub fn profile_options() -> ParseOptions {
        ParseOptions::for_profile(crate::reader::PROFILE.as_str()).unwrap_or_default()
    }

    /// Sets the flavor profile (`commonmark`, `gfm-file`, …): the
    /// Markdown commands follow it at once, the parser from the next
    /// document loaded on.
    pub fn set_profile(&mut self, id: &str, cx: &mut Context<'_, Self>) {
        self.commands = MarkdownCommands::for_profile(id);
        self.options = ParseOptions::for_profile(id).unwrap_or(self.options);
        self.availabilities.reset();
        cx.notify();
    }

    /// The Markdown commands of the view's profile.
    pub fn markdown_commands(&self) -> MarkdownCommands {
        self.commands
    }

    /// Tells the view the window's watchers, which hear about its saves.
    pub(crate) fn set_watch(&mut self, watch: Option<WatchHandle>) {
        self.watch = watch;
    }

    /// Loads `path` for editing, with link statuses from `index`. The file
    /// is read, decoded and parsed off the UI thread, with its crash
    /// journal (recovering one an earlier session left).
    pub fn open(&mut self, path: PathBuf, index: Option<PathIndex>, cx: &mut Context<'_, Self>) {
        // The journal and the watcher know a file by its absolute path.
        let path = std::path::absolute(&path).unwrap_or(path);
        self.links = LinkStatuses::new(Some(&path), index.as_ref());
        self.path = Some(path.clone());
        self.untitled = None;
        self.status = EditorStatus::Loading;
        self.doc = None;
        let key = DocumentKey::Path(path.clone());
        let claim = files::claim(cx, &key);
        let store = match (&claim, files::store(cx)) {
            (Some(_), store) => store,
            (None, Some(_)) => {
                tracing::warn!(
                    "{} is edited in another tab, which journals it",
                    path.display()
                );
                None
            }
            (None, None) => None,
        };
        let options = self.options;
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { file::load_file(&path, options, store.as_ref()) })
                .await;
            this.update(cx, |view, cx| view.finish_load(key, result, claim, cx))
                .ok();
        }));
        cx.notify();
    }

    /// Opens untitled document `n`: empty, or the text its journal holds.
    pub fn open_untitled(&mut self, n: u64, cx: &mut Context<'_, Self>) {
        self.links = LinkStatuses::new(None, None);
        self.path = None;
        self.untitled = Some(n);
        self.status = EditorStatus::Loading;
        self.doc = None;
        let key = DocumentKey::Untitled(n);
        let claim = files::claim(cx, &key);
        let store = claim.as_ref().and_then(|_| files::store(cx));
        let options = self.options;
        self._load = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move { file::load_untitled(n, options, store.as_ref()) })
                .await;
            this.update(cx, |view, cx| view.finish_load(key, Ok(loaded), claim, cx))
                .ok();
        }));
        cx.notify();
    }

    /// The number of an untitled document.
    pub fn untitled(&self) -> Option<u64> {
        self.untitled
    }

    /// Edits `text` (not a file), for tests and scratch documents. It is
    /// never journaled or saved.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<'_, Self>) {
        let buffer = Buffer::new(text);
        let tree = SyntaxTree::parse_with(buffer.text(), self.options);
        let clock = ExecutorClock(cx.background_executor().clone());
        let state = DocState::new(None, None, None, buffer.snapshot(), clock);
        self.install(buffer, tree, state);
        cx.notify();
    }

    fn finish_load(
        &mut self,
        key: DocumentKey,
        result: Result<Loaded, String>,
        claim: Option<files::Claim>,
        cx: &mut Context<'_, Self>,
    ) {
        self._load = None;
        match result {
            Ok(loaded) => {
                if loaded.lossy {
                    let name = self.path.as_deref().map(Path::display);
                    tracing::warn!(
                        "{} is not valid in its encoding: the editor shows U+FFFD",
                        name.map(|n| n.to_string()).unwrap_or_default()
                    );
                }
                let executor = cx.background_executor().clone();
                let worker = Worker::spawn(loaded.journal, claim, &executor);
                let state = DocState::new(
                    Some(key),
                    loaded.file,
                    Some(worker),
                    loaded.buffer.snapshot(),
                    ExecutorClock(executor),
                );
                self.install(loaded.buffer, loaded.tree, state);
                if let Some(recovered) = loaded.recovered {
                    self.apply_recovered(recovered, cx);
                }
                self.after_edit(cx);
                cx.emit(DocEvent::Loaded);
            }
            Err(err) => {
                tracing::warn!("cannot edit {:?}: {err}", self.path);
                self.status = EditorStatus::Failed(err.into());
            }
        }
        cx.notify();
    }

    fn install(&mut self, buffer: Buffer, tree: SyntaxTree, state: DocState) {
        let mut editor = Editor::new(buffer);
        let queue = EditQueue::default();
        let sink = queue.clone();
        let journal = state.worker().map(Worker::sender);
        let pause = state.pause.clone();
        let edited = state.edited.clone();
        editor.observe(move |event| {
            if let EditorEvent::Edited(edit) = event {
                sink.borrow_mut()
                    .push_back((edit.text.clone(), edit.summary.clone()));
                edited.set(true);
                if let Some(journal) = &journal
                    && !pause.get()
                {
                    // The worker is gone only when the view is.
                    let _ = journal.unbounded_send(Msg::Record {
                        summary: edit.summary.clone(),
                        text: edit.text.clone(),
                    });
                }
            }
        });
        let syntax = Syntax::new(tree, editor.snapshot());
        self.handed_off = Some(editor.text().revision());
        self.doc = Some(Doc {
            editor,
            queue,
            syntax,
            highlights: HighlightCache::default(),
            rows: RowCache::default(),
            state,
        });
        self.availabilities.reset();
        self.status = EditorStatus::Ready;
        self.scroll_y = 0.0;
        self.scroll_x = 0.0;
        self.widest = 0.0;
    }

    pub fn status(&self) -> &EditorStatus {
        &self.status
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The editing model, once loaded.
    pub fn editor(&self) -> Option<&Editor> {
        self.doc.as_ref().map(|d| &d.editor)
    }

    /// The window's path index changed: link statuses are asked again.
    pub fn set_index(&mut self, index: Option<PathIndex>, cx: &mut Context<'_, Self>) {
        self.links.reset(self.path.as_deref(), index.as_ref());
        cx.notify();
    }

    /// Whether whitespace is shown.
    pub fn shows_whitespace(&self) -> bool {
        self.show_whitespace
    }

    // ----- the Preview hand-off ------------------------------------------------

    /// The text to show in Preview, if it changed since the Reading view
    /// last got it (the file as loaded, at first). The text is not saved:
    /// the Reading view is rebuilt from it.
    pub fn take_preview_text(&mut self) -> Option<Arc<str>> {
        let doc = self.doc.as_ref()?;
        let text = doc.editor.text();
        if self.handed_off == Some(text.revision()) {
            return None;
        }
        self.handed_off = Some(text.revision());
        let string = text.text_for_range(0..text.len()).ok()?;
        Some(Arc::from(string))
    }

    /// The start of the line at the top of the view.
    pub fn top_offset(&self) -> Option<usize> {
        if let Some(Top::Offset(offset)) = self.pending_top {
            return Some(offset);
        }
        let text = self.doc.as_ref()?.editor.text();
        let line = match self.pending_top {
            Some(Top::Line(line)) => line,
            _ => self.top_line(),
        };
        text.line_start(line.min(text.line_count().saturating_sub(1)))
            .ok()
    }

    /// Scrolls so that the line holding `offset` is at the top (once the
    /// text is loaded, if it is not yet).
    pub fn scroll_to_offset(&mut self, offset: usize, cx: &mut Context<'_, Self>) {
        self.pending_top = Some(Top::Offset(offset));
        cx.notify();
    }

    /// Scrolls so that 0-based `line` is at the top.
    pub fn scroll_to_line(&mut self, line: usize, cx: &mut Context<'_, Self>) {
        self.pending_top = Some(Top::Line(line));
        cx.notify();
    }

    /// The first line whose row is at least half visible.
    fn top_line(&self) -> usize {
        let line = (self.scroll_y - f64::from(document::PAD_TOP)) / f64::from(LINE);
        line.round().max(0.0) as usize
    }

    // ----- editing ---------------------------------------------------------------

    /// After a change made by the view (keys, mouse, commands): reparse,
    /// bring the caret into view, tell the input method, redraw.
    fn changed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input_changed(cx);
        window.invalidate_character_coordinates();
    }

    /// After a change the platform's input handler made. The platform
    /// moves its candidate window itself; asking it to (a cursor rectangle
    /// commit on Wayland) can make the compositor drop the composition
    /// (IME spike, Problem 2).
    fn input_changed(&mut self, cx: &mut Context<'_, Self>) {
        self.pump_syntax(cx);
        self.after_edit(cx);
        self.autoscroll = true;
        cx.notify();
    }

    /// Reparses the queued edits within the UI-thread budget; a reparse
    /// that does not finish continues in the background.
    fn pump_syntax(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let deadline = std::time::Instant::now() + UI_BUDGET;
        let background = doc.syntax.advance(&mut doc.queue.borrow_mut(), deadline);
        if let Some(job) = background {
            self._reparse = Some(cx.spawn(async move |this, cx| {
                let (tree, text) = cx
                    .background_spawn(async move { Background::finish(job) })
                    .await;
                this.update(cx, |view, cx| {
                    if let Some(doc) = view.doc.as_mut() {
                        doc.syntax.finished(tree, text);
                    }
                    view.pump_syntax(cx);
                    cx.notify();
                })
                .ok();
            }));
        }
    }

    fn with_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        f: impl FnOnce(&mut Editor, &mut Extra),
    ) {
        let page = self.page_rows();
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let mut extra = Extra { page, scroll: 0.0 };
        f(&mut doc.editor, &mut extra);
        self.scroll_y += extra.scroll;
        self.changed(window, cx);
    }

    /// Rows in a page: the view's height less one row.
    fn page_rows(&self) -> u32 {
        let height = self.bounds.map_or(0.0, |b| f32::from(b.size.height));
        ((height / LINE).floor() as u32).saturating_sub(1).max(1)
    }

    fn motion(
        &mut self,
        motion: Motion,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.with_editor(window, cx, |editor, _| {
            editor.move_selections(motion, extend)
        });
    }

    fn page(&mut self, down: bool, extend: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.with_editor(window, cx, |editor, extra| {
            let rows = extra.page;
            let motion = if down {
                Motion::DownRows(rows)
            } else {
                Motion::UpRows(rows)
            };
            editor.move_selections(motion, extend);
            let px = f64::from(rows) * f64::from(LINE);
            extra.scroll = if down { px } else { -px };
        });
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.with_editor(window, cx, |editor, _| {
            if editor.is_composing() {
                // The input method did not take Escape: drop the marked text.
                editor.replace_and_mark_text_in_range_utf16(None, "", None);
            } else if editor.selections().len() > 1 {
                editor.collapse_to_primary();
            } else {
                editor.collapse_selections();
            }
        });
    }

    fn undo(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.with_editor(window, cx, |editor, _| {
            editor.undo();
        });
    }

    fn redo(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.with_editor(window, cx, |editor, _| {
            editor.redo();
        });
    }

    // ----- clipboard ----------------------------------------------------------------

    /// The selected texts, one per selection, and their join with line
    /// breaks; `None` when every selection is empty.
    fn selected_texts(&self) -> Option<(String, Vec<usize>)> {
        let editor = self.editor()?;
        let text = editor.text();
        if editor.selections().iter().all(|s| s.is_empty()) {
            return None;
        }
        let parts: Vec<String> = editor
            .selections()
            .iter()
            .map(|s| text.text_for_range(s.range()).unwrap_or_default())
            .collect();
        let lengths = parts.iter().map(String::len).collect();
        Some((parts.join("\n"), lengths))
    }

    fn copy(&mut self, cx: &mut Context<'_, Self>) -> bool {
        let Some((joined, lengths)) = self.selected_texts() else {
            return false;
        };
        let item = if lengths.len() > 1 {
            ClipboardItem::new_string_with_json_metadata(joined, lengths)
        } else {
            ClipboardItem::new_string(joined)
        };
        cx.write_to_clipboard(item);
        true
    }

    fn cut(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.copy(cx) {
            self.with_editor(window, cx, |editor, _| {
                editor.delete_backward(DeleteUnit::Grapheme);
            });
        }
    }

    /// Pastes the clipboard's text. With several carets and as many pieces
    /// on the clipboard (the selections of a multi-caret copy, or lines),
    /// each caret gets one; otherwise every caret gets the whole text.
    fn paste(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else {
            return;
        };
        let lengths = match item.entries().first() {
            Some(ClipboardEntry::String(s)) => s.metadata_json::<Vec<usize>>(),
            _ => None,
        };
        self.with_editor(window, cx, |editor, _| {
            let count = editor.selections().len();
            let pieces = split_for_carets(&text, lengths.as_deref(), count);
            match pieces {
                Some(pieces) => {
                    editor.insert_per_selection(&pieces, gv_editor::EditKind::Paste);
                }
                None => {
                    editor.paste(&text);
                }
            }
        });
    }

    fn select_all(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.with_editor(window, cx, |editor, _| editor.select_all());
        // Select all does not jump to the end.
        self.autoscroll = false;
    }

    fn toggle_whitespace(&mut self, cx: &mut Context<'_, Self>) {
        self.show_whitespace = !self.show_whitespace;
        cx.notify();
    }

    /// The advances of a line-number digit and of a text space.
    fn advances(&mut self, window: &Window) -> (f32, f32) {
        if let Some(advances) = self.advances {
            return advances;
        }
        let width = |s: &str, size: f32| {
            let run = gpui::TextRun {
                len: s.len(),
                font: self.fonts.regular().clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .shape_line(s.to_owned().into(), px(size), &[run], None)
                    .width,
            )
        };
        let advances = (
            width("0", text::EDITOR_LINE_NUMBER.size),
            width(" ", text::EDITOR.size),
        );
        self.advances = Some(advances);
        advances
    }
}

/// What an editing closure may ask of the view.
pub(crate) struct Extra {
    /// Rows in a page.
    page: u32,
    /// Scroll the view by this many px.
    scroll: f64,
}

/// The text for each of `count` carets, if it splits into one piece per
/// caret: by the lengths a multi-caret copy recorded, else by lines.
fn split_for_carets(text: &str, lengths: Option<&[usize]>, count: usize) -> Option<Vec<String>> {
    if count < 2 {
        return None;
    }
    if let Some(lengths) = lengths.filter(|l| l.len() == count)
        && lengths.iter().sum::<usize>() + count - 1 == text.len()
    {
        let mut pieces = Vec::with_capacity(count);
        let mut at = 0;
        for &len in lengths {
            pieces.push(text.get(at..at + len)?.to_owned());
            at += len + 1;
        }
        return Some(pieces);
    }
    let lines: Vec<&str> = text
        .strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    (lines.len() == count).then(|| lines.into_iter().map(str::to_owned).collect())
}

/// The row height.
const LINE: f32 = text::EDITOR.line;

impl EventEmitter<DocEvent> for EditorView {}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A message in the text column (a file that cannot be edited).
fn notice(title: SharedString, message: SharedString, cx: &App) -> impl IntoElement {
    let theme = Appearance::theme(cx);
    let column_width = document::COLUMN_MAX + 2.0 * document::PAD_X;
    div().size_full().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(column_width))
            .px(px(document::PAD_X))
            .pt(px(document::PAD_TOP))
            .flex()
            .flex_col()
            .child(
                div()
                    .mb(px(spacing::HEADING_GAP))
                    .text_size(px(text::PREVIEW_H1.size))
                    .line_height(px(text::PREVIEW_H1.line))
                    .font_weight(FontWeight(text::weight::BOLD))
                    .text_color(theme.text.primary)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(text::PREVIEW_BODY.size))
                    .line_height(px(text::PREVIEW_BODY.line))
                    .text_color(theme.text.secondary)
                    .child(message),
            ),
    )
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let root = div()
            .id("editor")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &MoveLeft, w, cx| this.motion(Motion::Left, false, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &MoveRight, w, cx| this.motion(Motion::Right, false, w, cx)),
            )
            .on_action(cx.listener(|this, _: &MoveUp, w, cx| this.motion(Motion::Up, false, w, cx)))
            .on_action(
                cx.listener(|this, _: &MoveDown, w, cx| this.motion(Motion::Down, false, w, cx)),
            )
            .on_action(cx.listener(|this, _: &MoveWordLeft, w, cx| {
                this.motion(Motion::WordLeft, false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveWordRight, w, cx| {
                this.motion(Motion::WordRight, false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveLineStart, w, cx| {
                this.motion(Motion::SmartLineStart, false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveLineEnd, w, cx| {
                this.motion(Motion::LineEnd, false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveDocumentStart, w, cx| {
                this.motion(Motion::DocumentStart, false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveDocumentEnd, w, cx| {
                this.motion(Motion::DocumentEnd, false, w, cx)
            }))
            .on_action(cx.listener(|this, _: &MovePageUp, w, cx| this.page(false, false, w, cx)))
            .on_action(cx.listener(|this, _: &MovePageDown, w, cx| this.page(true, false, w, cx)))
            .on_action(
                cx.listener(|this, _: &SelectLeft, w, cx| this.motion(Motion::Left, true, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectRight, w, cx| this.motion(Motion::Right, true, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectUp, w, cx| this.motion(Motion::Up, true, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectDown, w, cx| this.motion(Motion::Down, true, w, cx)),
            )
            .on_action(cx.listener(|this, _: &SelectWordLeft, w, cx| {
                this.motion(Motion::WordLeft, true, w, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, w, cx| {
                this.motion(Motion::WordRight, true, w, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLineStart, w, cx| {
                this.motion(Motion::SmartLineStart, true, w, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLineEnd, w, cx| {
                this.motion(Motion::LineEnd, true, w, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentStart, w, cx| {
                this.motion(Motion::DocumentStart, true, w, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectDocumentEnd, w, cx| {
                this.motion(Motion::DocumentEnd, true, w, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPageUp, w, cx| this.page(false, true, w, cx)))
            .on_action(cx.listener(|this, _: &SelectPageDown, w, cx| this.page(true, true, w, cx)))
            .on_action(cx.listener(|this, _: &Backspace, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.delete_backward(DeleteUnit::Grapheme);
                });
            }))
            .on_action(cx.listener(|this, _: &Delete, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.delete_forward(DeleteUnit::Grapheme);
                });
            }))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.delete_backward(DeleteUnit::Word);
                });
            }))
            .on_action(cx.listener(|this, _: &DeleteWordRight, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.delete_forward(DeleteUnit::Word);
                });
            }))
            .on_action(cx.listener(|this, _: &DeleteLineLeft, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.delete_backward(DeleteUnit::Line);
                });
            }))
            .on_action(cx.listener(|this, _: &DeleteLineRight, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.delete_forward(DeleteUnit::Line);
                });
            }))
            .on_action(cx.listener(|this, _: &AddCursorAbove, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.add_cursor_above();
                });
            }))
            .on_action(cx.listener(|this, _: &AddCursorBelow, w, cx| {
                this.with_editor(w, cx, |e, _| {
                    e.add_cursor_below();
                });
            }))
            .on_action(cx.listener(|this, _: &Cancel, w, cx| this.cancel(w, cx)))
            .on_action(cx.listener(|this, _: &commands::Undo, w, cx| this.undo(w, cx)))
            .on_action(cx.listener(|this, _: &commands::Redo, w, cx| this.redo(w, cx)))
            .on_action(cx.listener(|this, _: &commands::Copy, _, cx| {
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &commands::CopyAsMarkdown, _, cx| {
                // The source is Markdown already.
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &commands::Cut, w, cx| this.cut(w, cx)))
            .on_action(cx.listener(|this, _: &commands::Paste, w, cx| this.paste(w, cx)))
            .on_action(cx.listener(|this, _: &commands::PastePlain, w, cx| this.paste(w, cx)))
            .on_action(cx.listener(|this, _: &commands::SelectAll, w, cx| this.select_all(w, cx)))
            .on_action(cx.listener(|this, _: &commands::ToggleWhitespace, _, cx| {
                this.toggle_whitespace(cx);
            }));
        let root = self
            .markdown_handlers(root, cx)
            .relative()
            .size_full()
            .bg(theme.surface.page);
        match &self.status {
            EditorStatus::Ready => root.child(EditorElement::new(cx.entity())),
            EditorStatus::Failed(err) => {
                let name: SharedString = self
                    .path
                    .as_deref()
                    .and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into();
                root.child(notice(
                    name,
                    format!("Cannot edit this file: {err}").into(),
                    cx,
                ))
            }
            EditorStatus::Loading => root,
        }
    }
}

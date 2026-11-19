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
//!    rows ([`rows`]: a line's text and styles from the display map's
//!    chunks, one GPUI shaped line per wrap row, kept while the line, its
//!    highlighting and its wraps stay the same)
//!    and draws the gutter, the current line, selections, carets, the
//!    composition underline, link underlines and the scrollbar.
//!
//! Display rows are the wrap rows of the editor's wrap layer ([`wrap`]):
//! lines soft-wrap at the text column's width, with a hanging indent for
//! list items and quotes. In large-file mode, or with soft wrap turned
//! off, display rows are buffer lines and long lines scroll sideways. With
//! or without side panels the text uses the Reading view's centred column,
//! so text does not move sideways when the mode changes
//! ([`EditorLayout::Column`]); in the Split view it starts at the pane's
//! left edge and wraps at the pane's width ([`EditorLayout::Pane`]).
//!
//! # The Split view
//!
//! The Split view ([`crate::split`]) shows this view beside a Reading view
//! that follows its text: [`EditorView::take_handoff`] hands out the syntax
//! tree after each reparse with the reparses since the last hand-off, so
//! the Reading view updates incrementally. Scroll sync reads and sets the
//! top of the view as a fractional source line
//! ([`EditorView::top_position`], [`EditorView::scroll_to_position`]).
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
//! - Paste is smart paste ([`paste`]): a URL over a selection makes a
//!   link, clipboard HTML becomes Markdown; Mod+Shift+V pastes the text.
//! - Viewer mode ([`EditorView::set_read_only`]): the carets move and copy
//!   works, but nothing changes the text.
//! - Mouse: click places the caret, drag selects, Shift-click extends,
//!   double click selects a word and triple click a line (dragging after
//!   either extends by words or lines), Alt-click adds a caret and
//!   Alt-drag selects a column (07 § Buffer, selections and undo); the
//!   wheel scrolls, sideways with Shift.
//!
//! # Line commands and Go
//!
//! Move line up and down (Alt+↑ / Alt+↓), duplicate, join and sort lines,
//! change case and toggle comment (Mod+/) are Markdown commands like the
//! others ([`markdown`]). [`go`]: Go to line (Ctrl+G) and Go to heading
//! (Mod+Shift+O) open the Go popover ([`crate::goto`]), whose choice the
//! caret follows until Esc puts it back; Go to footnote or back; the
//! callout type picker; Expand and Shrink selection (Shift+Alt+→ / ←).
//!
//! # Find and replace
//!
//! [`find`]: Mod+F and Ctrl+H (Cmd+Alt+F) open the find popover with the
//! replace row over the text; matches are highlighted on the rows on
//! screen, the current one is selected, Replace all is one undo step.
//!
//! # Folding
//!
//! [`fold`]: headings, list items, code, quotes, front matter and HTML
//! blocks fold to their first line and a `⋯` chip. In the Split view the
//! gutter has a chevron on each line that folds; the View menu and the
//! palette have Fold, Unfold, Fold all, Unfold all and Fold to heading
//! level 1 to 3. Rows are visual lines: a folded line and the rest of the
//! line its fold ends on.
//!
//! # Sizes
//!
//! [`metrics`]: the text size, the row height and the Edit mode column
//! follow the editor font settings, the readable line width and the zoom
//! ([`crate::zoom`]), live.
//!
//! # The status bar
//!
//! [`stats`]: the primary caret's line and column, the characters
//! selected, the word count (counted as Preview counts, off the UI thread),
//! the encoding and the line endings ([`EditorView::status_facts`]).
//!
//! # Spell check
//!
//! [`spell`]: misspelled prose gets a wavy underline, checked on the
//! background executor after each reparse, block by block with a cache;
//! the context menu offers suggestions, Add to dictionary and Ignore, and
//! Next misspelling selects the next one.
//!
//! # The file
//!
//! [`file`] holds the document's file side: the crash journal every
//! edit goes to, saving (Mod+S and autosave), changes made on disk by other
//! programs and crash recovery ([`crate::files`]). When the tab goes back
//! to Preview, the Reading view is rebuilt from the editor's text
//! ([`EditorView::take_preview_text`]), saved or not.

mod autocomplete;
mod element;
mod file;
mod find;
mod fold;
mod go;
mod highlight;
mod input;
mod insert;
mod layout;
mod links;
mod live_blocks;
mod live_context;
mod live_preview;
mod live_widgets;
mod markdown;
mod metrics;
mod paste;
mod plain;
mod rename;
mod rows;
mod source_a11y;
mod spell;
mod stats;
mod syntax;
mod task_toggle;
#[cfg(test)]
mod tests;
mod wrap;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem, Context, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Pixels, Render, SharedString, Styled as _, Subscription, Task, Window, div,
    px,
};
use gv_editor::display_map::MarkdownHangingIndent;
use gv_editor::{DeleteUnit, Editor, EditorEvent, MarkdownCommands, Motion};
use gv_syntax::{ParseOptions, SyntaxTree};
use gv_text::{Buffer, Revision};
use gv_vault::PathIndex;
use gv_vault::files::journal::DocumentKey;

use crate::appearance::Appearance;
use crate::files::watch::WatchHandle;
use crate::files::worker::{Msg, Worker};
use crate::files::{self, ExecutorClock};
use crate::keymap::actions::editor as commands;
use crate::keymap::actions::view as view_commands;
use crate::metrics::size::{document, split};
use crate::metrics::spacing::document as spacing;
use crate::metrics::text;
use crate::settings::AppSettings;
use element::{EditorElement, Press, Scrollbar};
pub use file::DocEvent;
pub(crate) use file::SaveAsTarget;
use file::{DocState, Loaded};
pub use find::DEBUG_REPLACE_ENV;
pub use fold::DEBUG_FOLD_ENV;
use highlight::{HighlightCache, LinkStatuses};
pub(crate) use insert::LinkTarget;
use markdown::Availabilities;
use metrics::EditorMetrics;
pub use paste::{ClipboardHtml, HtmlClipboard, set_html_clipboard};
use rows::{Fonts, RowCache};
pub use stats::EditorFacts;
use syntax::{Background, EditQueue, Syntax, UI_BUDGET};
pub(crate) use syntax::{SyntaxHandoff, TextStamp};
use wrap::{RowMap, SoftWrap};

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
    /// Large-file mode: soft wrap stays off.
    large_file: bool,
    /// The text revision the foldable regions were found in.
    foldable_at: Option<Revision>,
}

impl std::fmt::Debug for Doc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Doc")
            .field("editor", &self.editor)
            .field("syntax", &self.syntax)
            .field("state", &self.state)
            .field("large_file", &self.large_file)
            .finish_non_exhaustive()
    }
}

/// Where the view puts its text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EditorLayout {
    /// The Reading view's centred text column (Edit mode).
    #[default]
    Column,
    /// From the pane's left edge, wrapping at the pane's width (the Split
    /// view's editor pane).
    Pane,
}

impl EditorLayout {
    /// Px above the first row, and below the last.
    pub(crate) fn pad_top(self) -> f32 {
        match self {
            Self::Column => document::PAD_TOP,
            Self::Pane => split::EDITOR_PAD_TOP,
        }
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
    /// The selections before each Expand selection, for Shrink.
    selection_history: gv_editor::commands::SelectionHistory,
    /// The profile smart paste writes HTML for.
    paste_profile: gv_html::Profile,
    /// Viewer mode: the text cannot be changed.
    read_only: bool,
    /// Plain-text mode: the file is not Markdown ([`plain`]).
    plain: Option<plain::Plain>,
    /// Live Preview ([`live_preview`]).
    live: live_preview::LivePreview,
    /// The caret's place and the word count, for the status bar.
    stats: stats::Stats,
    /// Misspellings and the spelling menu.
    spell: spell::SpellState,
    /// Vertical scroll in px from the top of the content (f64: a long
    /// file is millions of px tall).
    scroll_y: f64,
    /// Sideways scroll of the text column, in px (0 while wrapping).
    scroll_x: f32,
    /// Soft wrap.
    wrap: SoftWrap,
    /// The centred column, or the pane's edge.
    layout: EditorLayout,
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
    /// The link a Mod+click or a middle click asked the window to follow.
    clicked_link: Option<crate::reader::link_info::LinkAt>,
    /// The link under the pointer, for its tooltip.
    link_hover: links::LinkHover,
    /// The sizes from the Appearance settings and the zoom ([`metrics`]).
    metrics: EditorMetrics,
    fonts: Fonts,
    /// The advance of a digit at the line number size and of a space at
    /// the text size, measured once per size.
    advances: Option<(f32, f32)>,
    press: Press,
    /// The scrollbar as last drawn.
    scrollbar: Option<Scrollbar>,
    /// The text revision the Reading view was last given.
    handed_off: Option<Revision>,
    /// Find and replace.
    find: find::FindState,
    /// Autocomplete ([`crate::autocomplete`]).
    autocomplete: crate::autocomplete::State,
    /// The document's index record, for autocomplete.
    note_record: autocomplete::CurrentNote,
    /// Where the autocomplete popover was last placed.
    autocomplete_placed: Option<Bounds<Pixels>>,
    /// Rename heading or block ID: the last rename, for its Undo.
    rename: rename::RenameState,
    /// A Save As asked for while the document loaded.
    save_as_after_load: Option<file::SaveAsTarget>,
    /// The text for assistive technology, as the last frame laid it out.
    a11y: source_a11y::SharedSource,
    #[cfg(test)]
    probe: element::Probe,
    _load: Option<Task<()>>,
    _reparse: Option<Task<()>>,
    /// The background re-wrap of a large document.
    _rewrap: Option<wrap::Rewrap>,
    /// The wait for a large document's new width to settle.
    _rewrap_wait: Option<Task<()>>,
    /// The wait before the foldable regions are found again.
    _foldable: Option<Task<()>>,
    /// Folds of an earlier session waiting for the document, with the text
    /// revision first seen while waiting ([`fold`]).
    restored_folds: Option<(Vec<std::ops::Range<usize>>, Option<gv_text::Revision>)>,
    /// Follows the Appearance settings and the zoom.
    _settings: Subscription,
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
        let metrics = EditorMetrics::from_app(cx);
        let families = metrics.editor_families(cx);
        let fonts = Fonts::new(&families, metrics.text_size);
        Self {
            focus: cx.focus_handle(),
            path: None,
            untitled: None,
            watch: None,
            status: EditorStatus::Loading,
            doc: None,
            options,
            commands: MarkdownCommands::for_profile(gv_profile::ProfileId::CommonMark.as_str()),
            availabilities: Availabilities::default(),
            selection_history: gv_editor::commands::SelectionHistory::default(),
            paste_profile: paste_profile(gv_profile::ProfileId::CommonMark.as_str()),
            read_only: false,
            plain: None,
            live: live_preview::LivePreview::default(),
            stats: stats::Stats::default(),
            spell: spell::SpellState::default(),
            scroll_y: 0.0,
            scroll_x: 0.0,
            wrap: SoftWrap::default(),
            layout: EditorLayout::default(),
            pending_top: None,
            autoscroll: false,
            bounds: None,
            widest: 0.0,
            show_whitespace: false,
            links: LinkStatuses::new(None, None, gv_profile::ProfileId::CommonMark),
            clicked_link: None,
            link_hover: links::LinkHover::default(),
            metrics,
            fonts,
            advances: None,
            press: Press::None,
            scrollbar: None,
            handed_off: None,
            find: find::FindState::default(),
            autocomplete: autocomplete_state(gv_profile::ProfileId::CommonMark),
            note_record: autocomplete::CurrentNote::default(),
            autocomplete_placed: None,
            rename: rename::RenameState::default(),
            save_as_after_load: None,
            a11y: source_a11y::SharedSource::default(),
            #[cfg(test)]
            probe: element::Probe::default(),
            _load: None,
            _reparse: None,
            _rewrap: None,
            _rewrap_wait: None,
            _foldable: None,
            restored_folds: None,
            _settings: cx.observe_global::<AppSettings>(|this, cx| this.sync_metrics(cx)),
        }
    }

    /// Sets the flavor profile (`commonmark`, `gfm-file`, …): the
    /// Markdown commands follow it at once, the parser from the next
    /// document loaded on.
    pub fn set_profile(&mut self, id: &str, cx: &mut Context<'_, Self>) {
        self.commands = MarkdownCommands::for_profile(id);
        self.paste_profile = paste_profile(id);
        if let Some(profile) = gv_profile::ProfileId::from_id(id) {
            let switches = &gv_profile::definition(profile).switches;
            self.autocomplete.set_switches(switches);
            self.set_live_switches(switches);
            self.set_live_profile(Arc::new(gv_profile::compile_builtin(profile)));
        }
        self.options =
            gv_profile::ProfileId::from_id(id).map_or(self.options, ParseOptions::builtin);
        self.availabilities.reset();
        cx.notify();
    }

    /// Sets the compiled profile of the tab's document: the Markdown
    /// commands, smart paste and link statuses follow it at once; a loaded
    /// document is parsed again when the parse options differ (and no edit
    /// is still waiting to be parsed). A Reading view that follows the
    /// view gets the whole tree again at the next hand-off.
    pub fn set_compiled_profile(
        &mut self,
        profile: &gv_profile::CompiledProfile,
        cx: &mut Context<'_, Self>,
    ) {
        let id = profile.id.as_str();
        self.commands = MarkdownCommands::for_profile(id);
        self.paste_profile = paste_profile(id);
        self.autocomplete.set_switches(&profile.switches);
        self.set_live_switches(&profile.switches);
        self.set_live_profile(Arc::new(profile.clone()));
        self.availabilities.reset();
        if self.links.profile() != profile.id {
            self.links.set_profile(profile.id);
        }
        let options = ParseOptions::from_profile(profile);
        if options != self.options {
            self.options = options;
            if let Some(doc) = self.doc.as_mut()
                && doc.queue.borrow().is_empty()
                && doc.syntax.reparse_all(options)
            {
                self.schedule_foldable(cx);
            }
        }
        if let Some(doc) = self.doc.as_mut() {
            doc.syntax.restart_handoff();
        }
        cx.notify();
    }

    /// The Markdown commands of the view's profile.
    pub fn markdown_commands(&self) -> MarkdownCommands {
        self.commands
    }

    /// Viewer mode: keys, input methods, the clipboard and the Markdown
    /// commands cannot change the text (the carets still move, and copy
    /// still works). Edits made before stay and are saved as usual.
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<'_, Self>) {
        let read_only = read_only || self.file_blocks_edits();
        if self.read_only != read_only {
            self.read_only = read_only;
            cx.notify();
        }
    }

    /// Whether the text cannot be changed (viewer mode).
    pub fn is_read_only(&self) -> bool {
        self.read_only
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
        self.links = LinkStatuses::new(Some(&path), index.as_ref(), self.links.profile());
        self.autocomplete.index = index;
        self.path = Some(path.clone());
        self.plain_for_path(cx);
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
        self.links = LinkStatuses::new(None, None, self.links.profile());
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
        let limits = wrap::large_file_limits(cx);
        self.install(buffer, tree, state, &limits);
        self.schedule_word_count(cx);
        self.schedule_spell(cx);
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
                let limits = wrap::large_file_limits(cx);
                self.install(loaded.buffer, loaded.tree, state, &limits);
                if let Some(recovered) = loaded.recovered {
                    self.apply_recovered(recovered, cx);
                }
                self.after_edit(cx);
                cx.emit(DocEvent::Loaded);
                if let Some(target) = self.save_as_after_load.take() {
                    self.save_as(target, cx);
                }
            }
            Err(err) => {
                tracing::warn!("cannot edit {:?}: {err}", self.path);
                self.status = EditorStatus::Failed(err.into());
            }
        }
        cx.notify();
    }

    fn install(
        &mut self,
        buffer: Buffer,
        tree: SyntaxTree,
        state: DocState,
        limits: &gv_settings::Editor,
    ) {
        let mut editor = Editor::new(buffer);
        // Soft wrap starts with the first frame, which knows the width.
        editor.set_wrap_indent(wrap_indent(self.plain.is_some()));
        let large_file = wrap::is_large_file(editor.text(), limits);
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
        let mut highlights = HighlightCache::default();
        highlights.set_plain(self.plain_language());
        self.doc = Some(Doc {
            editor,
            queue,
            syntax,
            highlights,
            rows: RowCache::default(),
            state,
            large_file,
            foldable_at: None,
        });
        self._foldable = None;
        self.refresh_foldable();
        self.availabilities.reset();
        self.stats.reset();
        self.spell.reset();
        self.reset_find();
        self.autocomplete.clear();
        self.status = EditorStatus::Ready;
        self.scroll_y = 0.0;
        self.scroll_x = 0.0;
        self.widest = 0.0;
        self.wrap.ready = false;
        self.wrap.pending = None;
        self._rewrap = None;
        self._rewrap_wait = None;
        self.live_settings_changed();
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
        self.autocomplete.index = index;
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

    /// The line of the first row that is at least half visible.
    fn top_line(&self) -> usize {
        let row = self
            .geo()
            .fractional_row(self.scroll_y - f64::from(self.layout.pad_top()));
        let row = row.round().max(0.0) as usize;
        self.row_map().map_or(row, |map| map.line_of_row(row))
    }

    // ----- layout and the Split view ---------------------------------------------

    /// Where the view puts its text.
    pub fn layout(&self) -> EditorLayout {
        self.layout
    }

    /// Puts the text in the centred column or at the pane's edge. The line
    /// at the top stays at the top.
    pub fn set_layout(&mut self, layout: EditorLayout, cx: &mut Context<'_, Self>) {
        if self.layout == layout {
            return;
        }
        let top = self.top_offset();
        self.layout = layout;
        if let Some(top) = top {
            self.pending_top = Some(Top::Offset(top));
        }
        cx.notify();
    }

    /// The top of the view as a fractional source line: line `n` plus the
    /// part of its rows above the view's top edge. Negative while the top
    /// padding shows: -1 at the very top, up to 0 where the first row
    /// starts. As of the last frame.
    pub(crate) fn top_position(&self) -> Option<f64> {
        if let Some(top) = self.pending_top {
            let text = self.doc.as_ref()?.editor.text();
            let line = match top {
                Top::Offset(offset) => text.line_of_offset(offset.min(text.len())).ok()?,
                Top::Line(line) => line.min(text.line_count().saturating_sub(1)),
            };
            return Some(line as f64);
        }
        let map = self.row_map()?;
        let pad = f64::from(self.layout.pad_top());
        if self.scroll_y < pad {
            return Some(self.scroll_y / pad.max(1.0) - 1.0);
        }
        let row = self.geo().fractional_row(self.scroll_y - pad);
        let line = map.line_of_row(row.floor() as usize);
        let (first, rows) = line_rows(map, line);
        Some(line as f64 + ((row - first as f64) / rows as f64).clamp(0.0, 1.0))
    }

    /// Scrolls so that the top of the view is at fractional source line
    /// `position` (as [`Self::top_position`] gives it).
    pub(crate) fn scroll_to_position(&mut self, position: f64, cx: &mut Context<'_, Self>) {
        let Some(lines) = self.doc.as_ref().map(|d| d.editor.text().line_count()) else {
            return;
        };
        let Some(map) = self.row_map() else {
            return;
        };
        let pad = f64::from(self.layout.pad_top());
        let y = if position < 0.0 {
            (position + 1.0).max(0.0) * pad
        } else {
            let line = (position.floor() as usize).min(lines.saturating_sub(1));
            let (first, rows) = line_rows(map, line);
            let frac = (position - line as f64).clamp(0.0, 1.0);
            pad + self
                .geo()
                .y_of_fractional_row(first as f64 + frac * rows as f64)
        };
        self.pending_top = None;
        if (y - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = y;
            cx.notify();
        }
    }

    /// Scrolls so that the top of the view is at fractional source line
    /// `position` ([`Self::scroll_to_position`]) once the view is laid
    /// out; before that, so that the line holding it is at the top.
    pub(crate) fn scroll_to_position_soon(&mut self, position: f64, cx: &mut Context<'_, Self>) {
        if self.doc.is_some() && self.row_map().is_some() {
            self.scroll_to_position(position, cx);
        } else {
            self.scroll_to_line(position.max(0.0).floor() as usize, cx);
        }
    }

    /// The primary caret's offset and the text's revision.
    pub(crate) fn caret(&self) -> Option<(usize, Revision)> {
        let editor = &self.doc.as_ref()?.editor;
        Some((
            editor.selections().primary().head(),
            editor.text().revision(),
        ))
    }

    /// The focused element as a multi-line text input named "Source" for
    /// assistive technology: the rows as text runs with the selection
    /// ([`source_a11y`]); a screen reader's `SetTextSelection` moves the
    /// selection, its `Focus` focuses the view (GPUI's own handling of the
    /// focusable element), its `ScrollUp`/`ScrollDown` scroll by a line or
    /// a page, `ScrollLeft`/`ScrollRight` sideways (without soft wrap), and
    /// `SetScrollOffset` to a point. Runs take `ScrollIntoView` and
    /// `ScrollToPoint` (registered by the element when it paints, by their
    /// node ids).
    fn a11y_node(
        &self,
        root: gpui::Stateful<gpui::Div>,
        cx: &mut Context<'_, Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::StatefulInteractiveElement as _;
        let shared = self.a11y.clone();
        let source = self.a11y.clone();
        let view = cx.entity().downgrade();
        let up = view.clone();
        let down = view.clone();
        let left = view.clone();
        let right = view.clone();
        let offset = view.clone();
        root.role(gpui::Role::MultilineTextInput)
            .aria_label("Source")
            .a11y_synthetic_children(move |builder| source_a11y::push(&shared, builder))
            .on_a11y_action(
                gpui::accesskit::Action::SetTextSelection,
                move |data, window, cx| {
                    let Some(gpui::accesskit::ActionData::SetTextSelection(selection)) = data
                    else {
                        return;
                    };
                    let (anchor, focus) = {
                        let source = source.borrow();
                        (
                            source.offset_of(selection.anchor),
                            source.offset_of(selection.focus),
                        )
                    };
                    if let (Some(anchor), Some(focus)) = (anchor, focus) {
                        let _ = view.update(cx, |view, cx| {
                            view.select_from_a11y(anchor, focus, window, cx);
                        });
                    }
                },
            )
            .on_a11y_action(gpui::accesskit::Action::ScrollUp, move |data, _, cx| {
                let _ = up.update(cx, |view, cx| view.scroll_from_a11y(true, data, cx));
            })
            .on_a11y_action(gpui::accesskit::Action::ScrollDown, move |data, _, cx| {
                let _ = down.update(cx, |view, cx| view.scroll_from_a11y(false, data, cx));
            })
            .on_a11y_action(gpui::accesskit::Action::ScrollLeft, move |data, _, cx| {
                let _ = left.update(cx, |view, cx| {
                    view.scroll_sideways_from_a11y(true, data, cx)
                });
            })
            .on_a11y_action(gpui::accesskit::Action::ScrollRight, move |data, _, cx| {
                let _ = right.update(cx, |view, cx| {
                    view.scroll_sideways_from_a11y(false, data, cx);
                });
            })
            .on_a11y_action(
                gpui::accesskit::Action::SetScrollOffset,
                move |data, _, cx| {
                    if let Some(gpui::accesskit::ActionData::SetScrollOffset(to)) = data {
                        let to = *to;
                        let _ = offset.update(cx, |view, cx| view.set_scroll_from_a11y(to, cx));
                    }
                },
            )
    }

    /// Scrolls sideways (left when `left`) by a line height or
    /// (`ScrollUnit::Page`) the text column's width less a line, as a
    /// screen reader asks. Nothing moves while lines wrap.
    pub(crate) fn scroll_sideways_from_a11y(
        &mut self,
        left: bool,
        data: Option<&gpui::accesskit::ActionData>,
        cx: &mut Context<'_, Self>,
    ) {
        let line = self.metrics.line;
        let step = match data {
            Some(gpui::accesskit::ActionData::ScrollUnit(gpui::accesskit::ScrollUnit::Page)) => {
                self.bounds
                    .map_or(line, |b| (f32::from(b.size.width) - line).max(line))
            }
            _ => line,
        };
        // The next frame keeps it within the widest line.
        self.scroll_x = (self.scroll_x + if left { -step } else { step }).max(0.0);
        cx.notify();
    }

    /// Scrolls to `to` (px from the content's top left), as a screen
    /// reader asks; the next frame keeps it within the content.
    pub(crate) fn set_scroll_from_a11y(
        &mut self,
        to: gpui::accesskit::Point,
        cx: &mut Context<'_, Self>,
    ) {
        self.scroll_y = to.y.max(0.0);
        #[allow(clippy::cast_possible_truncation)]
        let x = to.x.max(0.0) as f32;
        self.scroll_x = x;
        cx.notify();
    }

    /// Scrolls up or down by a line or (`ScrollUnit::Page`) a screen, as a
    /// screen reader asks.
    pub(crate) fn scroll_from_a11y(
        &mut self,
        up: bool,
        data: Option<&gpui::accesskit::ActionData>,
        cx: &mut Context<'_, Self>,
    ) {
        let line = f64::from(self.metrics.line);
        let step = match data {
            Some(gpui::accesskit::ActionData::ScrollUnit(gpui::accesskit::ScrollUnit::Page)) => {
                self.bounds.map_or(line, |b| {
                    (f64::from(f32::from(b.size.height)) - line).max(line)
                })
            }
            _ => line,
        };
        self.scroll_y += if up { -step } else { step };
        cx.notify();
    }

    /// Scrolls run `run` of the last frame's description into view, or
    /// its top to `y` (window px), as a screen reader asks.
    pub(crate) fn scroll_run_from_a11y(
        &mut self,
        run: usize,
        y: Option<f32>,
        cx: &mut Context<'_, Self>,
    ) {
        let target = {
            let source = self.a11y.borrow();
            match y {
                Some(y) => source.scroll_to_point(run, y),
                None => source.scroll_to_show(run),
            }
        };
        if let Some(target) = target {
            self.scroll_y = target.max(0.0);
            cx.notify();
        }
    }

    /// Selects from `anchor` to `focus` (buffer offsets), as a screen
    /// reader asks.
    pub(crate) fn select_from_a11y(
        &mut self,
        anchor: usize,
        focus: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let len = doc.editor.text().len();
        doc.editor.unmark_text();
        doc.editor.set_selections(
            &[gv_editor::SelectionSpec::new(
                anchor.min(len),
                focus.min(len),
            )],
            0,
        );
        self.focus.focus(window, cx);
        self.changed(window, cx);
    }

    /// The last frame's description for assistive technology.
    #[cfg(test)]
    pub(crate) fn a11y_source(&self) -> std::cell::Ref<'_, source_a11y::SourceA11y> {
        self.a11y.borrow()
    }

    /// Puts the caret at `offset` and brings it into view.
    #[cfg(test)]
    pub(crate) fn set_cursor(&mut self, offset: usize, cx: &mut Context<'_, Self>) {
        if let Some(doc) = self.doc.as_mut() {
            doc.editor.set_cursor(offset);
            self.autoscroll = true;
            cx.notify();
        }
    }

    /// The syntax tree and its text for a Reading view that follows this
    /// view, when they changed since the last hand-off: the latest complete
    /// tree (a reparse still running in the background hands off when it
    /// lands).
    pub(crate) fn take_handoff(&mut self) -> Option<SyntaxHandoff> {
        self.doc.as_mut()?.syntax.take_handoff()
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
        crate::latency::handled();
        self.spell_after_change();
        self.pump_syntax(cx);
        self.update_autocomplete();
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
                let (tree, text, changes) = cx
                    .background_spawn(async move { Background::finish(job) })
                    .await;
                this.update(cx, |view, cx| {
                    if let Some(doc) = view.doc.as_mut() {
                        doc.syntax.finished(tree, text, &changes);
                    }
                    view.pump_syntax(cx);
                    cx.notify();
                })
                .ok();
            }));
        }
        self.schedule_foldable(cx);
        self.schedule_spell(cx);
        self.schedule_markers(cx);
    }

    fn with_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        f: impl FnOnce(&mut Editor, &mut Extra),
    ) {
        let page = self.page_rows();
        let line = self.metrics.line;
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let mut extra = Extra {
            page,
            line,
            scroll: 0.0,
        };
        f(&mut doc.editor, &mut extra);
        self.scroll_y += extra.scroll;
        self.changed(window, cx);
    }

    /// Rows in a page: the view's height less one row.
    fn page_rows(&self) -> u32 {
        let height = self.bounds.map_or(0.0, |b| f32::from(b.size.height));
        ((height / self.metrics.line).floor() as u32)
            .saturating_sub(1)
            .max(1)
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
            let px = f64::from(rows) * f64::from(extra.line);
            if editor.blocks().block_count() > 0 {
                // Live Preview blocks: a page is the view's height in
                // pixels, through the blocks' heights.
                let dy = if down { px } else { -px };
                editor.move_selections_by_pixels(dy as f32, extend);
            } else {
                editor.move_selections(motion, extend);
            }
            extra.scroll = if down { px } else { -px };
        });
    }

    /// Esc: closes the find popover first.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.close_find(window, cx) {
            return;
        }
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
        if !self.read_only && self.copy(cx) {
            self.with_editor(window, cx, |editor, _| {
                editor.delete_backward(DeleteUnit::Grapheme);
            });
        }
    }

    /// Pastes the clipboard ([`paste`]): smart paste with one caret unless
    /// `plain`; with several carets and as many pieces on the clipboard
    /// (the selections of a multi-caret copy, or lines) each caret gets
    /// one, otherwise every caret gets the whole text. One undo step.
    fn paste(&mut self, plain: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.read_only {
            return;
        }
        // A file that is not Markdown gets the clipboard's text as it is.
        let plain = plain || self.plain.is_some();
        let item = cx.read_from_clipboard();
        // An image (a screenshot, an image copied in a browser) is saved
        // as an attachment and embedded ([`insert`]).
        if !plain && let Some(image) = item.as_ref().and_then(insert::clipboard_image) {
            self.paste_image(image, window, cx);
            return;
        }
        let text = item.as_ref().and_then(ClipboardItem::text);
        let lengths = match item.as_ref().and_then(|i| i.entries().first()) {
            Some(ClipboardEntry::String(s)) => s.metadata_json::<Vec<usize>>(),
            _ => None,
        };
        let single = self.editor().is_some_and(|e| e.selections().len() == 1);
        // Pieces copied from several carets here are pasted as they are.
        let smart = (!plain && single && lengths.is_none())
            .then(|| {
                let selection = self.primary_selected_text().unwrap_or_default();
                let html = paste::clipboard_html(cx);
                paste::smart_text(
                    text.as_deref(),
                    html.as_ref(),
                    &selection,
                    self.paste_profile,
                )
            })
            .flatten();
        let Some(text) = smart.or(text) else {
            return;
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

    /// The text of the primary selection.
    fn primary_selected_text(&self) -> Option<String> {
        let editor = self.editor()?;
        let range = editor.selections().primary().range();
        editor.text().text_for_range(range).ok()
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
            width("0", self.metrics.number_size),
            width(" ", self.metrics.text_size),
        );
        self.advances = Some(advances);
        advances
    }
}

/// Registers the fold commands on `root`.
fn fold_handlers(
    root: gpui::Stateful<gpui::Div>,
    cx: &mut Context<'_, EditorView>,
) -> gpui::Stateful<gpui::Div> {
    use fold::FoldCommand;
    root.on_action(cx.listener(|this, _: &view_commands::Fold, w, cx| {
        this.fold_command(FoldCommand::Fold, w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::Unfold, w, cx| {
        this.fold_command(FoldCommand::Unfold, w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::FoldAll, w, cx| {
        this.fold_command(FoldCommand::FoldAll, w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::UnfoldAll, w, cx| {
        this.fold_command(FoldCommand::UnfoldAll, w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::FoldLevel1, w, cx| {
        this.fold_command(FoldCommand::Level(1), w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::FoldLevel2, w, cx| {
        this.fold_command(FoldCommand::Level(2), w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::FoldLevel3, w, cx| {
        this.fold_command(FoldCommand::Level(3), w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::FoldLevel4, w, cx| {
        this.fold_command(FoldCommand::Level(4), w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::FoldLevel5, w, cx| {
        this.fold_command(FoldCommand::Level(5), w, cx);
    }))
    .on_action(cx.listener(|this, _: &view_commands::FoldLevel6, w, cx| {
        this.fold_command(FoldCommand::Level(6), w, cx);
    }))
}

/// What an editing closure may ask of the view.
pub(crate) struct Extra {
    /// Rows in a page.
    page: u32,
    /// The row height.
    line: f32,
    /// Scroll the view by this many px.
    scroll: f64,
}

/// How wrapped rows are indented: as the line, plus a hanging indent for
/// list items and quotes in Markdown.
fn wrap_indent(plain: bool) -> Arc<dyn gv_editor::display_map::WrapIndent> {
    if plain {
        Arc::new(gv_editor::display_map::IndentAdjustment::SameIndent)
    } else {
        Arc::new(MarkdownHangingIndent::default())
    }
}

/// Autocomplete for built-in profile `id`.
fn autocomplete_state(id: gv_profile::ProfileId) -> crate::autocomplete::State {
    let mut state = crate::autocomplete::State::default();
    state.set_switches(&gv_profile::definition(id).switches);
    state
}

/// The paste profile of flavor profile `id`.
fn paste_profile(id: &str) -> gv_html::Profile {
    gv_html::Profile::from_id(id).unwrap_or_default()
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

/// The row height at the design's sizes (100 %, the default editor
/// font size).
#[cfg(test)]
const LINE: f32 = text::EDITOR.line;

/// The first display row of the visual line of `line` and its number of
/// rows.
fn line_rows(map: RowMap<'_>, line: usize) -> (usize, usize) {
    let first = map.first_row(line);
    (first, map.row_end(line).saturating_sub(first).max(1))
}

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
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        if self.status == EditorStatus::Ready {
            self.debug_replace(window, cx);
            self.debug_fold(window, cx);
            self.apply_restored_folds(window, cx);
        }
        // The key context is on the element around the editor and the find
        // popover, so the commands bound to it also work from the
        // popover's fields; the editor's own handlers are on the element
        // with its focus, which the fields are not inside.
        let host = div().id("editor-host").relative().size_full();
        let host = self.autocomplete_handlers(host, cx);
        let host = self.find_handlers(host, cx);
        let host = if self.plain.is_some() {
            host
        } else {
            self.rename_handlers(host, cx)
        };
        let popover = self
            .find_popover()
            .filter(|_| self.status == EditorStatus::Ready);
        let root = div()
            .id("editor")
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
            .on_action(cx.listener(|this, _: &SelectPageDown, w, cx| this.page(true, true, w, cx)));
        let root = root
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
            .on_action(cx.listener(|this, _: &commands::Copy, _, cx| {
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &commands::CopyAsMarkdown, _, cx| {
                // The source is Markdown already.
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &commands::SelectAll, w, cx| this.select_all(w, cx)))
            .on_action(cx.listener(|this, _: &commands::ToggleWhitespace, _, cx| {
                this.toggle_whitespace(cx);
            }))
            .on_action(
                cx.listener(|this, _: &view_commands::ToggleSoftWrap, _, cx| {
                    this.toggle_soft_wrap(cx);
                }),
            );
        // Plain text has no folds.
        let root = if self.plain.is_some() {
            root
        } else {
            fold_handlers(root, cx)
        };
        let root = self.go_handlers(root, cx);
        let root = self.spell_handlers(root, cx);
        // Viewer mode: nothing that changes the text has a handler, so its
        // menu items are disabled and its keys do nothing.
        let root = if self.read_only {
            root
        } else {
            root.on_action(cx.listener(|this, _: &Backspace, w, cx| {
                this.backspace(w, cx);
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
            .on_action(cx.listener(|this, _: &commands::Undo, w, cx| this.undo(w, cx)))
            .on_action(cx.listener(|this, _: &commands::Redo, w, cx| this.redo(w, cx)))
            .on_action(cx.listener(|this, _: &commands::Cut, w, cx| this.cut(w, cx)))
            .on_action(cx.listener(|this, _: &commands::Paste, w, cx| {
                this.paste(false, w, cx);
            }))
            .on_action(cx.listener(|this, _: &commands::PastePlain, w, cx| {
                this.paste(true, w, cx);
            }))
        };
        let root = if self.plain.is_some() {
            self.plain_handlers(root, cx)
        } else {
            self.markdown_handlers(root, cx)
        };
        let root = root.relative().size_full().bg(theme.surface.page);
        let root = match &self.status {
            EditorStatus::Ready => self
                .a11y_node(root, cx)
                .child(EditorElement::new(cx.entity())),
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
        };
        let link_tip = (self.status == EditorStatus::Ready)
            .then(|| self.link_tooltip(window, cx))
            .flatten();
        let completions = (self.status == EditorStatus::Ready)
            .then(|| self.autocomplete_popover(window, cx))
            .flatten();
        host.child(root)
            .children(popover)
            .children(link_tip)
            .children(completions)
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`Editor`]: the GUI-free editing model of one document.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::Arc;

use gv_syntax::SyntaxTree;

use gv_text::{
    Anchor, Bias, Buffer, EditError, EditSummary, OffsetUtf16, Revision, TextSnapshot, Transaction,
};

use crate::boundaries::{
    clip_caret, indent_end, line_end, line_start, next_grapheme_boundary, next_word_end,
    prev_grapheme_boundary, prev_word_start, snap_to_grapheme,
};
use crate::clock::{Clock, SystemClock};
use crate::commands::SyntaxRef;
use crate::display_map::{
    Block, BlockChanges, BlockId, BlockInput, BlockItem, BlockKind, BlockMap, BlockProperties,
    BlockSnapshot, DisplayItem, Fold, FoldId, Height, IndentAdjustment, MonospaceMeasure, Removal,
    ReplaceEdit, ReplaceMap, ReplacePoint, ReplaceSnapshot, Reveal, RewrapJob, RowEdit, TabEdit,
    TabMap, TabPoint, TabSnapshot, TextMeasure, WrapEdit, WrapIndent, WrapMap, WrapPoint, WrapRow,
    WrapSnapshot, Y,
};
use crate::folds::{FoldCandidate, FoldKind, foldable_ranges};
use crate::history::{
    EditKind, History, HistoryConfig, Record, SelectionState, StateId, UndoOutcome, UndoPlan,
    patch_of,
};
use crate::movement::{
    Motion, display_column, display_to_offset, move_caret, offset_to_display, point_at_column,
    smart_line_start,
};
use crate::selection::{Selection, SelectionGoal, SelectionId, SelectionSpec, Selections};

/// Settings of an [`Editor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorConfig {
    /// Columns per tab stop in the display map.
    pub tab_size: NonZeroU32,
    /// Undo grouping and limits.
    pub history: HistoryConfig,
}

impl Default for EditorConfig {
    /// Tab size 4 (CommonMark's tab stop), the default history.
    fn default() -> Self {
        Self {
            tab_size: NonZeroU32::new(4).unwrap_or(NonZeroU32::MIN),
            history: HistoryConfig::default(),
        }
    }
}

/// How much a delete command removes from a caret. A non-empty selection
/// is always deleted as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeleteUnit {
    /// One grapheme cluster (a CRLF is one cluster).
    Grapheme,
    /// To the start of the previous word or the end of the next one (see
    /// [`Motion::WordLeft`] and [`Motion::WordRight`]).
    Word,
    /// To the start or the end of the line; at the start (backwards) or the
    /// end (forwards) of a line, the line break, joining the lines.
    Line,
}

/// Why the text changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditCause {
    /// An edit of this kind.
    Edit(EditKind),
    /// An undo step.
    Undo,
    /// A redo step.
    Redo,
}

/// A change of the text, as observers see it. Everything is already
/// updated: the text, the display map and the selections.
#[derive(Debug)]
pub struct EditEvent<'a> {
    /// Why the text changed.
    pub cause: EditCause,
    /// The text before the change.
    pub old_text: &'a TextSnapshot,
    /// The text after the change.
    pub text: &'a TextSnapshot,
    /// What changed, in buffer bytes: feed it to the incremental parser and
    /// to anchors.
    pub summary: &'a EditSummary,
    /// The display map after the change.
    pub display: &'a TabSnapshot,
    /// What changed in the display map.
    pub display_edits: &'a [TabEdit],
    /// The wrap layer after the change, if soft wrap is on.
    pub wrap: Option<&'a WrapSnapshot>,
    /// What changed in the wrap layer, in wrap rows (empty when soft wrap
    /// is off).
    pub wrap_edits: &'a [WrapEdit],
    /// The selections after the change.
    pub selections: &'a Selections,
}

/// What an observer is told.
#[derive(Debug)]
#[non_exhaustive]
pub enum EditorEvent<'a> {
    /// The text changed (selections may have changed too).
    Edited(EditEvent<'a>),
    /// Only the selections changed.
    SelectionsChanged(&'a Selections),
    /// An input-method composition started, changed or ended without a
    /// text change.
    CompositionChanged,
    /// The display map changed without a text change: the tab size, or
    /// folds were added or removed.
    DisplayChanged {
        /// The display map after the change.
        display: &'a TabSnapshot,
        /// What changed in it.
        edits: &'a [TabEdit],
    },
    /// The wrap layer changed without a text change: soft wrap was turned
    /// on or off, or its width, measure or indent changed, or the tab size
    /// did (after [`EditorEvent::DisplayChanged`]).
    WrapChanged {
        /// The wrap layer after the change; `None` when soft wrap was
        /// turned off, so the view shows the rows of
        /// [`Editor::display`] again.
        wrap: Option<&'a WrapSnapshot>,
        /// What changed, in display rows: wrap rows, or tab rows on the
        /// side where soft wrap is off.
        edits: &'a [WrapEdit],
    },
    /// Blocks were inserted, removed (also revealed, or by an edit of their
    /// anchors' text), resized or displaced: after the event of the change
    /// that did it, if any. Rows that only moved under the blocks are not
    /// reported here; the other events cover them.
    BlocksChanged {
        /// The block layer after the change.
        blocks: &'a BlockSnapshot,
        /// What happened to which blocks.
        changes: &'a BlockChanges,
    },
}

/// Identifies an observer, to remove it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObserverId(u64);

type Observer = Box<dyn FnMut(&EditorEvent<'_>)>;

/// A selection range in UTF-16 code units, as input-method APIs use it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Utf16Selection {
    /// The range, from the start of the document.
    pub range: Range<usize>,
    /// Whether the caret (head) is at the start of the range.
    pub reversed: bool,
}

/// A fold placeholder on the display, for the view to paint and hit-test
/// (see [`Editor::fold_placeholders`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FoldPlaceholder {
    /// The first fold of the placeholder's run (overlapping folds share one
    /// placeholder).
    pub fold: FoldId,
    /// The buffer bytes it hides.
    pub buffer_range: Range<usize>,
    /// Where it is in the tab layer ([`Editor::display`]): one
    /// [`PLACEHOLDER`](crate::display_map::PLACEHOLDER) on one row.
    pub tab_range: Range<TabPoint>,
    /// Where it is in the wrap layer, while soft wrap is on.
    pub wrap_range: Option<Range<WrapPoint>>,
}

/// A foldable region from [`Editor::update_foldable`], moved through edits
/// since.
#[derive(Clone, Debug)]
struct Foldable {
    /// The start of the line its marker is on.
    header: usize,
    range: Range<usize>,
    kind: FoldKind,
}

/// Folds that the step from text state `from` to `to` removed, with their
/// ranges in the text of `from`: brought back when undo or redo returns
/// from `to` to `from`.
#[derive(Clone, Debug)]
struct FoldRestore {
    from: StateId,
    to: StateId,
    ranges: Vec<Range<usize>>,
}

/// How many [`FoldRestore`]s the editor keeps.
const MAX_FOLD_RESTORES: usize = 64;

/// A state to record as saved once a save of `revision` completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SavePoint {
    /// The revision of the text being saved.
    pub revision: Revision,
    /// The history state of that text.
    pub state: StateId,
}

/// The replace and tab layers, and the wrap layer while soft wrap is on,
/// kept in step with the buffer.
struct DisplayLayers {
    replace_map: ReplaceMap,
    tab_map: TabMap,
    snapshot: TabSnapshot,
    tab_size: NonZeroU32,
    wrap: Option<WrapLayer>,
    /// The wrap map while soft wrap is off, once it has been on: synced to
    /// an empty text, so it costs nothing to keep. Keeping one map for the
    /// editor's life keeps its re-wrap generations counting up, so a job
    /// handed out before soft wrap was turned off stays stale.
    parked: Option<WrapMap>,
    /// The measure until the wrap map exists (it owns it from then on).
    measure: Option<Box<dyn TextMeasure>>,
    indent: Arc<dyn WrapIndent>,
}

#[derive(Debug)]
struct WrapLayer {
    map: WrapMap,
    snapshot: WrapSnapshot,
}

impl fmt::Debug for DisplayLayers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplayLayers")
            .field("replace_map", &self.replace_map)
            .field("tab_map", &self.tab_map)
            .field("tab_size", &self.tab_size)
            .field("wrap", &self.wrap)
            .field("parked", &self.parked)
            .finish_non_exhaustive()
    }
}

/// A tab edit from the start of the text to `old_end` and `new_end`.
fn whole_text_edit(old_end: TabPoint, new_end: TabPoint) -> TabEdit {
    let zero = TabPoint::new(0, 0);
    TabEdit {
        old: zero..old_end,
        new: zero..new_end,
    }
}

impl DisplayLayers {
    fn new(text: TextSnapshot, tab_size: NonZeroU32) -> Self {
        let (replace_map, replace) = ReplaceMap::new(text);
        let (tab_map, snapshot) = TabMap::new(replace, tab_size);
        Self {
            replace_map,
            tab_map,
            snapshot,
            tab_size,
            wrap: None,
            parked: None,
            measure: None,
            indent: Arc::new(IndentAdjustment::SameIndent),
        }
    }

    /// The rows vertical motion runs on.
    fn rows(&self) -> DisplayRows<'_> {
        match &self.wrap {
            Some(wrap) => DisplayRows::Wrap(&wrap.snapshot),
            None => DisplayRows::Tab(&self.snapshot),
        }
    }

    fn wrap_snapshot(&self) -> Option<&WrapSnapshot> {
        self.wrap.as_ref().map(|wrap| &wrap.snapshot)
    }

    /// The rows the block layer sits on: the display rows.
    fn block_input(&self) -> BlockInput {
        match &self.wrap {
            Some(wrap) => BlockInput::Wrap(wrap.snapshot.clone()),
            None => BlockInput::Tab(self.snapshot.clone()),
        }
    }

    fn replace(&self) -> &ReplaceSnapshot {
        self.snapshot.replace_snapshot()
    }

    /// Syncs every layer to `text`. Also returns the folds the edits
    /// removed (see [`ReplaceMap::sync_with_unfolded`]).
    fn sync(
        &mut self,
        text: TextSnapshot,
        summary: &EditSummary,
    ) -> (Vec<TabEdit>, Vec<WrapEdit>, Vec<Fold>) {
        let (replace, replace_edits, unfolded) = self.replace_map.sync_with_unfolded(text, summary);
        let (edits, wrap_edits) = self.apply_replace(replace, replace_edits);
        (edits, wrap_edits, unfolded)
    }

    /// Changes the folds with `change` and passes the edits up the layers.
    fn change_folds(
        &mut self,
        change: impl FnOnce(&mut ReplaceMap) -> (ReplaceSnapshot, Vec<ReplaceEdit>),
    ) -> (Vec<TabEdit>, Vec<WrapEdit>) {
        let (replace, replace_edits) = change(&mut self.replace_map);
        self.apply_replace(replace, replace_edits)
    }

    /// Passes a change of the replace layer up the tab and wrap layers.
    fn apply_replace(
        &mut self,
        replace: ReplaceSnapshot,
        replace_edits: Vec<ReplaceEdit>,
    ) -> (Vec<TabEdit>, Vec<WrapEdit>) {
        let (snapshot, edits) = self.tab_map.sync(replace, replace_edits, self.tab_size);
        self.snapshot = snapshot;
        let wrap_edits = self.sync_wrap(&edits);
        (edits, wrap_edits)
    }

    fn sync_wrap(&mut self, edits: &[TabEdit]) -> Vec<WrapEdit> {
        match &mut self.wrap {
            Some(wrap) => {
                let (snapshot, wrap_edits) = wrap.map.sync(self.snapshot.clone(), edits.to_vec());
                wrap.snapshot = snapshot;
                wrap_edits
            }
            None => Vec::new(),
        }
    }

    fn set_tab_size(&mut self, tab_size: NonZeroU32) -> (Vec<TabEdit>, Vec<WrapEdit>) {
        self.tab_size = tab_size;
        let replace = self.replace_map.snapshot();
        let (snapshot, edits) = self.tab_map.sync(replace, Vec::new(), tab_size);
        self.snapshot = snapshot;
        let wrap_edits = self.sync_wrap(&edits);
        (edits, wrap_edits)
    }

    /// Turns soft wrap on or off or changes its width. Returns the edits
    /// in display rows, or `None` if nothing changed.
    fn set_wrap_width(&mut self, width: Option<f32>) -> Option<Vec<WrapEdit>> {
        let width = width.filter(|width| !width.is_nan());
        match (self.wrap.as_mut(), width) {
            (None, None) => None,
            (Some(wrap), Some(width)) => {
                let (snapshot, edits) = wrap.map.set_wrap_width(Some(width));
                wrap.snapshot = snapshot;
                (!edits.is_empty()).then_some(edits)
            }
            (Some(_), None) => Some(self.wrap_off()),
            (None, Some(width)) => Some(self.wrap_on(Some(width))),
        }
    }

    /// Like [`set_wrap_width`](Self::set_wrap_width), except that a
    /// re-wrap of the whole text at a new width is handed out as a job
    /// instead of done: the rows are interpolated until it is finished.
    /// Turning soft wrap on shows the lines unwrapped meanwhile.
    fn defer_wrap_width(
        &mut self,
        width: Option<f32>,
    ) -> (Option<RewrapJob>, Option<Vec<WrapEdit>>) {
        let width = width.filter(|width| !width.is_nan());
        match (self.wrap.as_mut(), width) {
            (Some(wrap), Some(width)) => {
                let job = wrap.map.defer_wrap_width(Some(width));
                if job.is_some() {
                    // The same rows, now interpolated.
                    wrap.snapshot = wrap.map.snapshot();
                }
                (job, None)
            }
            (None, Some(width)) => {
                let edits = self.wrap_on(None);
                let job = self.wrap.as_mut().and_then(|wrap| {
                    let job = wrap.map.defer_wrap_width(Some(width));
                    wrap.snapshot = wrap.map.snapshot();
                    job
                });
                (job, Some(edits))
            }
            (_, None) => (None, self.set_wrap_width(None)),
        }
    }

    /// Installs a job from [`defer_wrap_width`](Self::defer_wrap_width).
    /// Returns the edits, or `None` for a stale job or with soft wrap off.
    fn finish_rewrap(&mut self, job: RewrapJob) -> Option<Vec<WrapEdit>> {
        let wrap = self.wrap.as_mut()?;
        let (snapshot, edits) = wrap.map.finish_rewrap(job)?;
        wrap.snapshot = snapshot;
        Some(edits)
    }

    /// Turns soft wrap on, wrapping the text at `width` (`None`: not yet).
    /// Returns the edit from tab rows to wrap rows.
    fn wrap_on(&mut self, width: Option<f32>) -> Vec<WrapEdit> {
        let tab_rows = self.snapshot.max_point().row() + 1;
        let map = match self.parked.take() {
            Some(mut map) => {
                // Parked at an empty text: the width costs nothing to set
                // there, and the text comes back wrapped at it.
                map.set_wrap_width(width);
                let edit = whole_text_edit(TabPoint::new(0, 0), self.snapshot.max_point());
                map.sync(self.snapshot.clone(), vec![edit]);
                map
            }
            None => {
                let measure = self
                    .measure
                    .take()
                    .unwrap_or_else(|| Box::new(MonospaceMeasure::new(1.0)));
                WrapMap::with_indent(
                    self.snapshot.clone(),
                    measure,
                    Arc::clone(&self.indent),
                    width,
                )
                .0
            }
        };
        let snapshot = map.snapshot();
        let wrap_rows = snapshot.max_point().row().0 + 1;
        self.wrap = Some(WrapLayer { map, snapshot });
        vec![WrapEdit {
            old: WrapRow(0)..WrapRow(tab_rows),
            new: WrapRow(0)..WrapRow(wrap_rows),
        }]
    }

    /// Turns soft wrap off, parking the wrap map. Returns the edit from
    /// wrap rows to tab rows.
    fn wrap_off(&mut self) -> Vec<WrapEdit> {
        let tab_rows = self.snapshot.max_point().row() + 1;
        let Some(WrapLayer { mut map, snapshot }) = self.wrap.take() else {
            return Vec::new();
        };
        let wrap_rows = snapshot.max_point().row().0 + 1;
        // Down to an empty text (cheap: no row is left to wrap), then a
        // re-wrap of that text settles a job still out and makes it stale.
        let (_, replace) = ReplaceMap::new(TextSnapshot::new(""));
        let (_, empty) = TabMap::new(replace, self.tab_size);
        let edit = whole_text_edit(snapshot.tab_snapshot().max_point(), TabPoint::new(0, 0));
        map.sync(empty, vec![edit]);
        let job = map.begin_rewrap();
        map.finish_rewrap(job);
        self.parked = Some(map);
        vec![WrapEdit {
            old: WrapRow(0)..WrapRow(wrap_rows),
            new: WrapRow(0)..WrapRow(tab_rows),
        }]
    }

    /// Like [`set_measure`](Self::set_measure), except that while soft
    /// wrap is on the re-wrap is handed out as a job instead of done: the
    /// rows are interpolated until it is finished.
    fn defer_measure(&mut self, measure: Box<dyn TextMeasure>) -> Option<RewrapJob> {
        match self.wrap.as_mut() {
            Some(wrap) => {
                let job = wrap.map.defer_measure(measure);
                // The same rows, now interpolated.
                wrap.snapshot = wrap.map.snapshot();
                Some(job)
            }
            None => {
                self.set_measure(measure);
                None
            }
        }
    }

    /// Returns the wrap edits if soft wrap is on.
    fn set_measure(&mut self, measure: Box<dyn TextMeasure>) -> Option<Vec<WrapEdit>> {
        match (&mut self.wrap, &mut self.parked) {
            (Some(wrap), _) => {
                let (snapshot, edits) = wrap.map.set_measure(measure);
                wrap.snapshot = snapshot;
                Some(edits)
            }
            (None, Some(parked)) => {
                parked.set_measure(measure);
                None
            }
            (None, None) => {
                self.measure = Some(measure);
                None
            }
        }
    }

    /// Returns the wrap edits if soft wrap is on.
    fn set_wrap_indent(&mut self, indent: Arc<dyn WrapIndent>) -> Option<Vec<WrapEdit>> {
        self.indent = Arc::clone(&indent);
        if let Some(parked) = &mut self.parked {
            parked.set_indent(Arc::clone(&indent));
        }
        let wrap = self.wrap.as_mut()?;
        let (snapshot, edits) = wrap.map.set_indent(indent);
        wrap.snapshot = snapshot;
        Some(edits)
    }
}

/// The height of a text row until the view sets one: one pixel, so that
/// pixels count display rows.
const DEFAULT_ROW_HEIGHT: Height = Height(crate::display_map::SUBPIXELS_PER_PX);

/// The marked text of an input-method composition: one range per caret.
#[derive(Clone, Debug)]
struct Composition {
    ranges: Vec<(Anchor, Anchor)>,
    /// The range the platform sees (the primary caret's).
    primary: usize,
}

/// The editing model of one document: the buffer, selections, undo
/// history, display map and input-method state. It has no GUI types; the
/// editor view drives it and listens to it through observers.
///
/// # Edits
///
/// Every edit is one [`Transaction`] (all cursors together), producing one
/// [`EditSummary`] that moves the selections, syncs the display map, is
/// recorded in the history and is passed to the observers
/// ([`Editor::observe`]), which feed the parser and the render model.
///
/// # Positions
///
/// Byte offsets passed in are clipped (to the text, out of multi-byte
/// characters and out of a CRLF), never trusted. UTF-16 positions from an
/// input method are clipped the same way and never split a surrogate pair.
///
/// # Folds
///
/// Folds live in the display map's replace layer (see
/// [`ReplaceMap`]): each shows as one placeholder at the end of its first
/// row. The fold commands that take a line ([`fold_at`](Self::fold_at),
/// [`toggle_fold`](Self::toggle_fold), …) work on the foldable regions of
/// [`update_foldable`](Self::update_foldable); [`fold_ranges`](Self::fold_ranges)
/// folds any ranges. Folds are display state, not undo steps. No selection
/// end is ever hidden in a fold:
///
/// - motions skip folded text as a whole (Left and Right jump over it,
///   vertical motion skips folded rows, Home and End work on the display
///   line);
/// - folding moves carets inside the new fold to its start;
/// - a caret or selection end placed inside a fold (a click, a search
///   result) unfolds it, and so does Backspace after a placeholder or
///   Delete before one, instead of deleting hidden text;
/// - an edit that changes text inside a fold unfolds it; undoing the edit
///   folds it again, and redoing it unfolds it again. Such an edit is an
///   undo step of its own.
///
/// # Blocks
///
/// The display map's block layer ([`BlockMap`]) holds block widgets above,
/// below or instead of display lines, with heights in pixels
/// ([`insert_blocks`](Self::insert_blocks), [`blocks`](Self::blocks) for
/// y-based queries). It follows every change of the text and the display
/// rows. No selection end is left on a row a replace block hides: motions
/// skip the rows of a block that is never revealed ([`Reveal::Never`]),
/// and a selection end put there otherwise (a click, an edit, undo) moves
/// to the start of the row after the block (or the end of the row before
/// it at the end of the text). A block revealed on selection
/// ([`Reveal::OnSelection`]) lets carets onto its rows and is removed when
/// one gets there. Which blocks are revealed when is the caller's policy:
/// it picks the rule per block and inserts the block again when it wants.
pub struct Editor {
    buffer: Buffer,
    selections: Selections,
    history: History,
    clock: Box<dyn Clock>,
    display: DisplayLayers,
    composition: Option<Composition>,
    /// Closing characters inserted by smart pairs, with the character:
    /// typing it there moves over it, and Backspace between it and its
    /// opener deletes both. Forgotten when the selections are moved.
    auto_closers: Vec<(Anchor, char)>,
    observers: Vec<(ObserverId, Observer)>,
    next_observer: u64,
    /// Sorted by header.
    foldable: Vec<Foldable>,
    fold_restores: VecDeque<FoldRestore>,
    /// The folds the last undo or redo transaction removed, in the text
    /// before it.
    step_unfolded: Vec<Fold>,
    blocks: BlockMap,
}

impl fmt::Debug for Editor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Editor")
            .field("buffer", &self.buffer)
            .field("selections", &self.selections)
            .field("history", &self.history)
            .field("composition", &self.composition)
            .field("observers", &self.observers.len())
            .finish_non_exhaustive()
    }
}

fn emit(observers: &mut [(ObserverId, Observer)], event: &EditorEvent<'_>) {
    for (_, observer) in observers.iter_mut() {
        observer(event);
    }
}

impl Editor {
    /// An editor for `buffer`, with the default configuration and the
    /// system clock. The caret is at the start.
    pub fn new(buffer: Buffer) -> Self {
        Self::with_config(
            buffer,
            EditorConfig::default(),
            Box::new(SystemClock::new()),
        )
    }

    /// An editor for `buffer` with `config`, taking edit times from
    /// `clock`.
    pub fn with_config(buffer: Buffer, config: EditorConfig, clock: Box<dyn Clock>) -> Self {
        let display = DisplayLayers::new(buffer.snapshot(), config.tab_size);
        let blocks = BlockMap::new(display.block_input(), DEFAULT_ROW_HEIGHT);
        Self {
            buffer,
            selections: Selections::new(),
            history: History::new(config.history),
            clock,
            display,
            composition: None,
            auto_closers: Vec::new(),
            observers: Vec::new(),
            next_observer: 0,
            foldable: Vec::new(),
            fold_restores: VecDeque::new(),
            step_unfolded: Vec::new(),
            blocks,
        }
    }

    // ----- state ------------------------------------------------------------

    /// The buffer (read only: every change goes through the editor).
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// The current text.
    pub fn text(&self) -> &TextSnapshot {
        self.buffer.text()
    }

    /// A copy of the current text (O(1)).
    pub fn snapshot(&self) -> TextSnapshot {
        self.buffer.snapshot()
    }

    /// The buffer, giving up the editor.
    pub fn into_buffer(self) -> Buffer {
        self.buffer
    }

    /// The selections.
    pub fn selections(&self) -> &Selections {
        &self.selections
    }

    /// The display map's tab layer, at the current revision.
    pub fn display(&self) -> &TabSnapshot {
        &self.display.snapshot
    }

    /// The undo history.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Changes the history limits and timing (the steps over the new
    /// limits are dropped).
    pub fn set_history_config(&mut self, config: HistoryConfig) {
        self.history.set_config(config);
    }

    /// Changes the tab size of the display map.
    pub fn set_tab_size(&mut self, tab_size: NonZeroU32) {
        let (edits, wrap_edits) = self.display.set_tab_size(tab_size);
        let block_changes = self.sync_blocks(&edits, &wrap_edits, None);
        if !edits.is_empty() {
            let event = EditorEvent::DisplayChanged {
                display: &self.display.snapshot,
                edits: &edits,
            };
            emit(&mut self.observers, &event);
        }
        if !wrap_edits.is_empty() {
            self.wrap_changed(&wrap_edits);
        }
        self.after_blocks_changed(block_changes);
    }

    // ----- soft wrap ----------------------------------------------------------

    /// The wrap layer over [`display`](Self::display), if soft wrap is on.
    pub fn wrap(&self) -> Option<&WrapSnapshot> {
        self.display.wrap_snapshot()
    }

    /// The soft-wrap width, or `None` when soft wrap is off.
    pub fn wrap_width(&self) -> Option<f32> {
        self.display
            .wrap
            .as_ref()
            .and_then(|wrap| wrap.map.wrap_width())
    }

    /// Turns soft wrap on at `width`, changes the width, or turns soft wrap
    /// off (`None`). The width is in the units of the measure
    /// ([`set_measure`](Self::set_measure)): text-column pixels in the view,
    /// columns with the default measure (every character one unit wide).
    ///
    /// The whole text is re-wrapped at once. While soft wrap is on,
    /// vertical motion moves by wrap rows, and Home and End stop at the
    /// display row first (see [`Editor::move_selections`]). Observers get
    /// [`EditorEvent::WrapChanged`].
    pub fn set_wrap_width(&mut self, width: Option<f32>) {
        if let Some(edits) = self.display.set_wrap_width(width) {
            let block_changes = self.sync_blocks(&[], &edits, None);
            self.wrap_changed(&edits);
            self.after_blocks_changed(block_changes);
        }
    }

    /// Like [`set_wrap_width`](Self::set_wrap_width), but a re-wrap of the
    /// whole text is handed out as a [`RewrapJob`] instead of done, for a
    /// view to run on another thread or in steps: soft wrap turned on, or
    /// a new width. Returns `None` when there is nothing to wrap (the same
    /// width, or soft wrap turned off, which is done at once).
    ///
    /// Until the job is handed back to
    /// [`finish_rewrap`](Self::finish_rewrap), the wrap layer keeps the
    /// old wraps of unchanged text and leaves edited text unwrapped (it
    /// [is interpolated](WrapSnapshot::is_interpolated)); turning soft
    /// wrap on shows the lines unwrapped. Edits keep syncing meanwhile.
    /// [`wrap_width`](Self::wrap_width) is the new width at once.
    ///
    /// Run the job with [`RewrapJob::run`] and a
    /// [`LineWrapper`](crate::display_map::LineWrapper) whose measure
    /// measures like the editor's ([`set_measure`](Self::set_measure)).
    /// A later re-wrap of the whole text (another width, measure or
    /// indent, soft wrap turned off) makes the job stale; edits and a new
    /// tab size do not. Turning soft wrap on emits
    /// [`EditorEvent::WrapChanged`] at once, with the rows unchanged.
    pub fn defer_wrap_width(&mut self, width: Option<f32>) -> Option<RewrapJob> {
        let (job, edits) = self.display.defer_wrap_width(width);
        // The rows may be the same but interpolated now: the block layer
        // takes the new snapshot either way.
        let block_changes = self.sync_blocks(&[], edits.as_deref().unwrap_or(&[]), None);
        if let Some(edits) = edits {
            self.wrap_changed(&edits);
        }
        self.after_blocks_changed(block_changes);
        job
    }

    /// Installs a job from [`defer_wrap_width`](Self::defer_wrap_width):
    /// wraps any rows it has not done, re-applies the edits made since it
    /// began, and emits [`EditorEvent::WrapChanged`] with one edit over
    /// the whole text. Returns `false`, changing nothing, for a stale job.
    pub fn finish_rewrap(&mut self, job: RewrapJob) -> bool {
        match self.display.finish_rewrap(job) {
            Some(edits) => {
                let block_changes = self.sync_blocks(&[], &edits, None);
                self.wrap_changed(&edits);
                self.after_blocks_changed(block_changes);
                true
            }
            None => false,
        }
    }

    /// Whether a job from [`defer_wrap_width`](Self::defer_wrap_width) is
    /// out and not stale.
    pub fn is_rewrapping(&self) -> bool {
        self.display
            .wrap
            .as_ref()
            .is_some_and(|wrap| wrap.map.is_rewrapping())
    }

    /// Measures text for soft wrap with `measure` from now on (a new font
    /// or size), re-wrapping at once if soft wrap is on.
    pub fn set_measure(&mut self, measure: Box<dyn TextMeasure>) {
        if let Some(edits) = self.display.set_measure(measure) {
            let block_changes = self.sync_blocks(&[], &edits, None);
            self.wrap_changed(&edits);
            self.after_blocks_changed(block_changes);
        }
    }

    /// Like [`set_measure`](Self::set_measure), but while soft wrap is on
    /// the re-wrap is handed out as a [`RewrapJob`] instead of done, as
    /// [`defer_wrap_width`](Self::defer_wrap_width) does: the old wraps
    /// show (interpolated) until the job is handed back to
    /// [`finish_rewrap`](Self::finish_rewrap). Run the job with a
    /// [`LineWrapper`](crate::display_map::LineWrapper) that measures like
    /// `measure`. Returns `None`, with the measure in place, while soft
    /// wrap is off.
    pub fn defer_measure(&mut self, measure: Box<dyn TextMeasure>) -> Option<RewrapJob> {
        let job = self.display.defer_measure(measure);
        if job.is_some() {
            // The rows are the same but interpolated now: the block layer
            // takes the new snapshot.
            let block_changes = self.sync_blocks(&[], &[], None);
            self.after_blocks_changed(block_changes);
        }
        job
    }

    /// Chooses the indent of soft-wrapped continuation rows with `indent`
    /// (by default, their row's leading whitespace; see
    /// [`MarkdownHangingIndent`](crate::display_map::MarkdownHangingIndent)
    /// for a list and quote hanging indent), re-wrapping at once if soft
    /// wrap is on.
    pub fn set_wrap_indent(&mut self, indent: Arc<dyn WrapIndent>) {
        if let Some(edits) = self.display.set_wrap_indent(indent) {
            let block_changes = self.sync_blocks(&[], &edits, None);
            self.wrap_changed(&edits);
            self.after_blocks_changed(block_changes);
        }
    }

    fn wrap_changed(&mut self, edits: &[WrapEdit]) {
        let event = EditorEvent::WrapChanged {
            wrap: self.display.wrap_snapshot(),
            edits,
        };
        emit(&mut self.observers, &event);
    }

    // ----- observers ----------------------------------------------------------

    /// Calls `observer` after every change of the text, the selections or
    /// the composition. Observers run in the order they were added, after
    /// the editor is fully updated.
    pub fn observe(&mut self, observer: impl FnMut(&EditorEvent<'_>) + 'static) -> ObserverId {
        let id = ObserverId(self.next_observer);
        self.next_observer += 1;
        self.observers.push((id, Box::new(observer)));
        id
    }

    /// Removes an observer. Returns whether it existed.
    pub fn unobserve(&mut self, id: ObserverId) -> bool {
        let before = self.observers.len();
        self.observers.retain(|(other, _)| *other != id);
        before != self.observers.len()
    }

    fn selections_changed(&mut self) {
        emit(
            &mut self.observers,
            &EditorEvent::SelectionsChanged(&self.selections),
        );
    }

    // ----- the edit path --------------------------------------------------------

    /// Applies `transaction` and runs the edit path: `after` updates the
    /// selections from the summary, then the history records the change
    /// (for [`EditCause::Edit`]), the display map and the composition
    /// follow, and observers are told. `after` runs even if the text did
    /// not change. Returns the summary if the text changed.
    fn commit<R>(
        &mut self,
        transaction: &Transaction,
        cause: EditCause,
        force_group: bool,
        after: impl FnOnce(&mut Selections, &TextSnapshot, &EditSummary) -> R,
    ) -> Result<(Option<EditSummary>, R), EditError> {
        let old = self.buffer.snapshot();
        let summary = self.buffer.apply(transaction)?;
        let selections_before = self.selections.specs();
        let result = after(&mut self.selections, self.buffer.text(), &summary);
        if summary.is_empty() {
            if self.selections.specs() != selections_before {
                self.history.seal();
                let mut block_changes = BlockChanges::default();
                self.settle_block_selections(&mut block_changes);
                self.blocks_changed(&block_changes);
                self.selections_changed();
            }
            return Ok((None, result));
        }
        let (display_edits, wrap_edits, unfolded) =
            self.display.sync(self.buffer.snapshot(), &summary);
        let mut block_changes = self.sync_blocks(&display_edits, &wrap_edits, Some(&summary));
        self.settle_block_selections(&mut block_changes);
        self.move_foldable(&summary);
        if let EditCause::Edit(kind) = cause {
            // Replacing text with the same text changes no byte: nothing to
            // undo.
            let patch = patch_of(&old, transaction);
            if !patch.is_empty() {
                // An edit that removes folds is an undo step of its own, so
                // undoing it returns to exactly the text the folds were in.
                if !unfolded.is_empty() {
                    self.history.seal();
                }
                let from = self.history.state();
                self.history.record(Record {
                    kind,
                    patch,
                    selections_before,
                    selections_after: self.selections.specs(),
                    now: self.clock.now(),
                    force_group,
                });
                if !unfolded.is_empty() {
                    self.history.seal();
                    let to = self.history.state();
                    self.remember_unfolded(from, to, &unfolded);
                }
            }
        } else {
            self.step_unfolded = unfolded;
        }
        for (anchor, _) in &mut self.auto_closers {
            *anchor = anchor.transform(&summary);
        }
        if let Some(composition) = &mut self.composition {
            for (start, end) in &mut composition.ranges {
                *start = start.transform(&summary);
                *end = end.transform(&summary);
            }
        }
        let event = EditorEvent::Edited(EditEvent {
            cause,
            old_text: &old,
            text: self.buffer.text(),
            summary: &summary,
            display: &self.display.snapshot,
            display_edits: &display_edits,
            wrap: self.display.wrap_snapshot(),
            wrap_edits: &wrap_edits,
            selections: &self.selections,
        });
        emit(&mut self.observers, &event);
        self.blocks_changed(&block_changes);
        Ok((Some(summary), result))
    }

    /// Applies replacements given as `(range, text)` in any order (they are
    /// sorted; they must not overlap and must lie on character boundaries).
    /// Selections move through the edit by their anchors. Returns the
    /// summary, or `None` if nothing changed. A composition in progress is
    /// committed first.
    pub fn edit<I, S>(&mut self, edits: I, kind: EditKind) -> Result<Option<EditSummary>, EditError>
    where
        I: IntoIterator<Item = (Range<usize>, S)>,
        S: Into<String>,
    {
        self.end_composition();
        let mut edits: Vec<(Range<usize>, String)> = edits
            .into_iter()
            .map(|(range, text)| (range, text.into()))
            .collect();
        edits.sort_by_key(|(range, _)| (range.start, range.end));
        let mut transaction = Transaction::new();
        for (range, text) in edits {
            transaction.push(range, text);
        }
        let (summary, ()) = self.commit(
            &transaction,
            EditCause::Edit(kind),
            false,
            |selections, text, summary| selections.transform(text, summary),
        )?;
        Ok(summary)
    }

    /// Replaces every selection with `text`, as typing: one transaction
    /// for all cursors, carets after the inserted text. Returns whether the
    /// text changed.
    pub fn insert_at_selections(&mut self, text: &str) -> bool {
        self.end_composition();
        self.insert(text, EditKind::Typing)
    }

    /// Like [`Editor::insert_at_selections`], as a paste (its own undo
    /// step).
    pub fn paste(&mut self, text: &str) -> bool {
        self.end_composition();
        self.insert(text, EditKind::Paste)
    }

    /// Replaces selection `i` with `texts[i]` (for a multi-cursor paste of
    /// one line per cursor). Returns false and changes nothing if the
    /// counts differ.
    pub fn insert_per_selection<S: AsRef<str>>(&mut self, texts: &[S], kind: EditKind) -> bool {
        if texts.len() != self.selections.len() {
            return false;
        }
        self.end_composition();
        let mut transaction = Transaction::new();
        for (selection, text) in self.selections.iter().zip(texts) {
            transaction.push(selection.range(), text.as_ref());
        }
        self.replace_selections(&transaction, kind)
    }

    /// Inserts the buffer's line ending (its dominant one, see
    /// [`gv_text::TextFormat::line_ending`]) at every selection.
    pub fn newline(&mut self) -> bool {
        self.end_composition();
        let line_ending = self.buffer.format().line_ending.as_str();
        self.insert(line_ending, EditKind::Typing)
    }

    fn insert(&mut self, text: &str, kind: EditKind) -> bool {
        let mut transaction = Transaction::new();
        for selection in self.selections.iter() {
            transaction.push(selection.range(), text);
        }
        self.replace_selections(&transaction, kind)
    }

    /// Applies a transaction with one replacement per selection, in order,
    /// and puts each caret after its replacement.
    fn replace_selections(&mut self, transaction: &Transaction, kind: EditKind) -> bool {
        let result = self.commit(
            transaction,
            EditCause::Edit(kind),
            false,
            |selections, text, summary| {
                selections.update(text, |selection| {
                    SelectionSpec::caret(summary.transform_offset(selection.end(), Bias::Right))
                });
            },
        );
        debug_assert!(result.is_ok(), "selection edit rejected: {result:?}");
        matches!(result, Ok((Some(_), ())))
    }

    /// Deletes backwards from every caret by `unit` (and every non-empty
    /// selection). One transaction for all cursors; ranges that overlap
    /// are merged.
    pub fn delete_backward(&mut self, unit: DeleteUnit) -> bool {
        self.end_composition();
        let text = self.buffer.text();
        let ranges: Vec<Range<usize>> = self
            .selections
            .iter()
            .map(|selection| {
                if !selection.is_empty() {
                    return selection.range();
                }
                let head = selection.head();
                let start = match unit {
                    DeleteUnit::Grapheme => {
                        // Between an opener and the closer smart pairs
                        // inserted for it: both go.
                        let closer = self.auto_closer_at(head);
                        if let Some(closer) = closer
                            && head > 0
                            && char_before(text, head) == opener_of(closer)
                        {
                            return head - 1..head + closer.len_utf8();
                        }
                        prev_grapheme_boundary(text, head)
                    }
                    DeleteUnit::Word => prev_word_start(text, head),
                    DeleteUnit::Line => {
                        let start = line_start(text, head);
                        if start == head {
                            prev_grapheme_boundary(text, head)
                        } else {
                            start
                        }
                    }
                };
                start..head
            })
            .collect();
        let ranges = self.unfold_instead_of_deleting(ranges);
        self.delete_ranges(ranges, EditKind::DeleteBackward)
    }

    /// Deletes forwards from every caret by `unit` (and every non-empty
    /// selection).
    pub fn delete_forward(&mut self, unit: DeleteUnit) -> bool {
        self.end_composition();
        let text = self.buffer.text();
        let ranges: Vec<Range<usize>> = self
            .selections
            .iter()
            .map(|selection| {
                if !selection.is_empty() {
                    return selection.range();
                }
                let head = selection.head();
                let end = match unit {
                    DeleteUnit::Grapheme => next_grapheme_boundary(text, head),
                    DeleteUnit::Word => next_word_end(text, head),
                    DeleteUnit::Line => {
                        let end = line_end(text, head);
                        if end == head {
                            next_grapheme_boundary(text, head)
                        } else {
                            end
                        }
                    }
                };
                head..end
            })
            .collect();
        let ranges = self.unfold_instead_of_deleting(ranges);
        self.delete_ranges(ranges, EditKind::DeleteForward)
    }

    /// Deletes one range per selection (in selection order), merging
    /// overlaps; each caret goes to the start of its range.
    fn delete_ranges(&mut self, ranges: Vec<Range<usize>>, kind: EditKind) -> bool {
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in &ranges {
            if range.is_empty() {
                continue;
            }
            match merged.last_mut() {
                Some(last) if range.start < last.end => last.end = last.end.max(range.end),
                _ => merged.push(range.clone()),
            }
        }
        let mut transaction = Transaction::new();
        for range in merged {
            transaction.push(range, "");
        }
        let starts: Vec<usize> = ranges.iter().map(|range| range.start).collect();
        let result = self.commit(
            &transaction,
            EditCause::Edit(kind),
            false,
            |selections, text, summary| {
                let mut index = 0;
                selections.update(text, |_| {
                    let start = starts.get(index).copied().unwrap_or(0);
                    index += 1;
                    SelectionSpec::caret(summary.transform_offset(start, Bias::Left))
                });
            },
        );
        debug_assert!(result.is_ok(), "delete rejected: {result:?}");
        matches!(result, Ok((Some(_), ())))
    }

    // ----- smart pairs -----------------------------------------------------------

    /// The closer smart pairs inserted at `offset`, if it is still there.
    pub(crate) fn auto_closer_at(&self, offset: usize) -> Option<char> {
        let text = self.buffer.text();
        self.auto_closers
            .iter()
            .find(|(anchor, closer)| {
                anchor.offset == offset && char_after(text, offset) == Some(*closer)
            })
            .map(|&(_, closer)| closer)
    }

    /// Applies typed text with one [`TypedEdit`] per selection (in
    /// selection order) as one typing transaction, sets the selections the
    /// edits ask for and remembers the closers they insert. Returns
    /// whether the text or the selections changed.
    pub(crate) fn type_edits(&mut self, edits: Vec<TypedEdit>) -> bool {
        if edits.len() != self.selections.len() {
            return false;
        }
        self.end_composition();
        let primary = self.selections.primary_index();
        let mut order: Vec<usize> = (0..edits.len()).collect();
        order.sort_by_key(|&i| (edits[i].range.start, edits[i].range.end));
        let mut transaction = Transaction::new();
        for &i in &order {
            transaction.push(edits[i].range.clone(), edits[i].text.as_str());
        }
        let before = self.selections.specs();
        let result = self.commit(
            &transaction,
            EditCause::Edit(EditKind::Typing),
            false,
            |selections, text, summary| {
                let starts: Vec<usize> = edits
                    .iter()
                    .map(|edit| summary.transform_offset(edit.range.start, Bias::Left))
                    .collect();
                let specs: Vec<SelectionSpec> = edits
                    .iter()
                    .zip(&starts)
                    .map(|(edit, start)| {
                        SelectionSpec::new(start + edit.select.start, start + edit.select.end)
                    })
                    .collect();
                selections.set(text, &specs, primary);
                let mut added = Vec::new();
                let mut consumed = Vec::new();
                for (edit, start) in edits.iter().zip(&starts) {
                    if let Some(at) = edit.closer
                        && let Some(closer) = edit.text.get(at..).and_then(|t| t.chars().next())
                    {
                        added.push((Anchor::after(start + at), closer));
                    }
                    if edit.over {
                        consumed.push(*start);
                    }
                }
                (added, consumed)
            },
        );
        debug_assert!(result.is_ok(), "typed edit rejected: {result:?}");
        let Ok((summary, (added, consumed))) = result else {
            return false;
        };
        let text = self.buffer.text();
        self.auto_closers.retain(|(anchor, closer)| {
            !consumed.contains(&anchor.offset) && char_after(text, anchor.offset) == Some(*closer)
        });
        self.auto_closers.extend(added);
        if summary.is_none() && self.selections.specs() != before {
            self.selections_changed();
        }
        summary.is_some() || self.selections.specs() != before
    }

    // ----- undo ---------------------------------------------------------------

    /// Undoes the newest step and restores the selections from before it.
    /// A composition in progress is committed first.
    pub fn undo(&mut self) -> UndoOutcome {
        self.end_composition();
        self.auto_closers.clear();
        match self.history.begin_undo() {
            UndoPlan::Nothing => UndoOutcome::NothingToUndo,
            UndoPlan::StopAtSavePoint => UndoOutcome::StoppedAtSavePoint,
            UndoPlan::Apply(transaction, selections) => {
                let from = self.history.state();
                if self.apply_history_step(&transaction, EditCause::Undo, selections) {
                    self.history.commit_undo();
                    self.after_history_step(from);
                    UndoOutcome::Undone
                } else {
                    UndoOutcome::NothingToUndo
                }
            }
        }
    }

    /// Redoes the next step and restores the selections from after it.
    /// Returns whether anything was redone.
    pub fn redo(&mut self) -> bool {
        self.end_composition();
        self.auto_closers.clear();
        let Some((transaction, selections)) = self.history.begin_redo() else {
            return false;
        };
        let from = self.history.state();
        if self.apply_history_step(&transaction, EditCause::Redo, selections) {
            self.history.commit_redo();
            self.after_history_step(from);
            true
        } else {
            false
        }
    }

    fn apply_history_step(
        &mut self,
        transaction: &Transaction,
        cause: EditCause,
        (specs, primary): SelectionState,
    ) -> bool {
        let result = self.commit(transaction, cause, false, |selections, text, _| {
            selections.set(text, &specs, primary);
        });
        if result.is_err() {
            // The history no longer fits the text: a bug. Keep the text
            // safe and drop the history rather than apply a wrong patch.
            debug_assert!(false, "history step rejected: {result:?}");
            self.history.clear();
            return false;
        }
        true
    }

    /// Ends the open undo step: the next edit starts a new one.
    pub fn break_undo_group(&mut self) {
        self.history.seal();
    }

    /// Records the current text as saved. Undo stops between the text
    /// before and after this point (the open step is sealed).
    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
    }

    /// For a save that completes later: seals the open step and returns the
    /// state to pass to [`Editor::mark_saved_at`] once the text of
    /// `revision` is on disk.
    pub fn save_point(&mut self) -> SavePoint {
        SavePoint {
            revision: self.buffer.revision(),
            state: self.history.save_point(),
        }
    }

    /// Records a completed save. If the user edited in the meantime, the
    /// text stays modified until undo returns to the saved state.
    pub fn mark_saved_at(&mut self, point: SavePoint) {
        self.history.mark_saved_state(point.state);
    }

    /// Whether the text differs from the last saved state.
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    // ----- selections -----------------------------------------------------------

    /// After a user's selection change: commit a composition, close the
    /// undo step, tell observers.
    fn user_selection_change(&mut self) {
        self.history.seal();
        self.auto_closers.clear();
        self.unfold_at_selections();
        let mut block_changes = BlockChanges::default();
        self.settle_block_selections(&mut block_changes);
        self.blocks_changed(&block_changes);
        self.selections_changed();
    }

    /// Replaces the selections; `primary` indexes `specs`. Offsets are
    /// clipped.
    pub fn set_selections(&mut self, specs: &[SelectionSpec], primary: usize) {
        self.end_composition();
        self.selections.set(self.buffer.text(), specs, primary);
        self.user_selection_change();
    }

    /// Selects `ranges` (forwards); the last one becomes primary.
    pub fn select_ranges<I: IntoIterator<Item = Range<usize>>>(&mut self, ranges: I) {
        let specs: Vec<SelectionSpec> = ranges.into_iter().map(SelectionSpec::range).collect();
        let primary = specs.len().saturating_sub(1);
        self.set_selections(&specs, primary);
    }

    /// One caret at `offset` (clipped).
    pub fn set_cursor(&mut self, offset: usize) {
        self.set_selections(&[SelectionSpec::caret(offset)], 0);
    }

    /// Adds a caret, which becomes primary.
    pub fn add_cursor(&mut self, offset: usize) -> SelectionId {
        self.add_selection(SelectionSpec::caret(offset))
    }

    /// Adds a selection, which becomes primary (merged with any selection
    /// it overlaps).
    pub fn add_selection(&mut self, spec: SelectionSpec) -> SelectionId {
        self.end_composition();
        let id = self.selections.add(self.buffer.text(), spec);
        self.user_selection_change();
        id
    }

    /// Removes a selection, unless it is the last one.
    pub fn remove_selection(&mut self, id: SelectionId) -> bool {
        self.end_composition();
        let removed = self.selections.remove(id);
        if removed {
            self.user_selection_change();
        }
        removed
    }

    /// Selects the whole text.
    pub fn select_all(&mut self) {
        let len = self.buffer.len();
        self.set_selections(&[SelectionSpec::new(0, len)], 0);
    }

    /// Collapses every selection to its caret (head).
    pub fn collapse_selections(&mut self) {
        self.end_composition();
        self.selections.update(self.buffer.text(), |selection| {
            SelectionSpec::caret(selection.head())
        });
        self.user_selection_change();
    }

    /// Keeps only the primary selection.
    pub fn collapse_to_primary(&mut self) {
        let primary = self.selections.primary();
        let spec = SelectionSpec {
            tail: primary.tail(),
            head: primary.head(),
            goal: primary.goal(),
        };
        self.set_selections(&[spec], 0);
    }

    /// Moves every caret by `motion`. With `extend`, the heads move and the
    /// tails stay (shift-motion); without, a non-empty selection collapses:
    /// left and up from its start, right and down from its end (a plain
    /// left or right only collapses it).
    ///
    /// While soft wrap is on, vertical motions move by wrap rows, with the
    /// goal column counted in characters of the wrap row (its indent
    /// included). [`Motion::LineStart`] and [`Motion::SmartLineStart`] on a
    /// continuation row go to the start of that row's text, and
    /// [`Motion::LineEnd`] on a row that continues goes before its last
    /// character (the end of a soft-wrapped row is the same text position
    /// as the start of the next one); pressed again there, they do what
    /// they do without soft wrap, on the buffer line.
    ///
    /// Motions skip the rows a block hides unless the block is revealed on
    /// selection (see [`Reveal`]): out of them, a caret moving down or
    /// forwards goes to the first row after the block (at its goal column
    /// for vertical motions), one moving up or backwards to the last row
    /// before it, and the other way when there is no such row.
    pub fn move_selections(&mut self, motion: Motion, extend: bool) {
        self.end_composition();
        let rows = self.display.rows();
        let blocks = self.blocks.snapshot();
        self.selections.update(self.buffer.text(), |selection| {
            move_selection(rows, blocks, selection, motion, extend)
        });
        self.user_selection_change();
    }

    /// Column (box) selection, as with Alt-drag: one selection per display
    /// row (wrap row while soft wrap is on) from `tail`'s row to `head`'s,
    /// each from `tail`'s display column to `head`'s (clamped to the row).
    /// Rows that end before the left column get none. The selection on
    /// `head`'s row is primary.
    pub fn select_columns(&mut self, tail: usize, head: usize) {
        let display = self.display.rows();
        let (tail_row, tail_column) = display.row_and_column(tail);
        let (head_row, head_column) = display.row_and_column(head);
        let left = tail_column.min(head_column);
        let rows: Vec<u32> = if tail_row <= head_row {
            (tail_row..=head_row).collect()
        } else {
            (head_row..=tail_row).rev().collect()
        };
        let specs: Vec<SelectionSpec> = rows
            .into_iter()
            .filter(|&row| left <= display.row_end_column(row))
            .map(|row| SelectionSpec {
                tail: display.offset_at(row, tail_column, false),
                head: display.offset_at(row, head_column, false),
                goal: SelectionGoal::Column(head_column),
            })
            .collect();
        if specs.is_empty() {
            self.set_cursor(head);
        } else {
            let primary = specs.len() - 1;
            self.set_selections(&specs, primary);
        }
    }

    /// Adds a caret one display row above the topmost selection's head.
    /// Returns false at the first row.
    pub fn add_cursor_above(&mut self) -> bool {
        self.add_cursor_vertically(Motion::Up)
    }

    /// Adds a caret one display row below the bottommost selection's head.
    /// Returns false at the last row.
    pub fn add_cursor_below(&mut self) -> bool {
        self.add_cursor_vertically(Motion::Down)
    }

    fn add_cursor_vertically(&mut self, motion: Motion) -> bool {
        let display = self.display.rows();
        let from = match motion {
            Motion::Up => self.selections.first(),
            _ => self.selections.last(),
        };
        let row = display.row_of(from.head());
        let (to, goal) = display.move_caret(from.head(), from.goal(), motion);
        let column = match goal {
            SelectionGoal::Column(column) => Some(column),
            SelectionGoal::None => None,
        };
        let to = out_of_hidden_rows(
            display,
            self.blocks.snapshot(),
            to,
            motion == Motion::Down,
            column,
            false,
        );
        if display.row_of(to) == row {
            return false;
        }
        self.add_selection(SelectionSpec {
            tail: to,
            head: to,
            goal,
        });
        true
    }

    // ----- folding ------------------------------------------------------------

    /// Recomputes the foldable regions (see [`crate::folds`]) from
    /// `syntax`. A tree that is not the parse of the current text is
    /// replaced by a fresh parse with the tree's options.
    ///
    /// The regions move with edits until the next update, so call this
    /// again after a reparse. The fold commands ([`fold_at`](Self::fold_at)
    /// and the others that take a line) and [`is_foldable`](Self::is_foldable)
    /// use them; existing folds do not depend on them.
    pub fn update_foldable(&mut self, syntax: SyntaxRef<'_>) {
        let text = self.buffer.text();
        let candidates = if syntax.is_parse_of(text) {
            foldable_ranges(syntax.tree, text)
        } else {
            let tree = SyntaxTree::parse_with(text, syntax.tree.options());
            foldable_ranges(&tree, text)
        };
        self.foldable = candidates
            .into_iter()
            .map(|candidate| Foldable {
                header: text
                    .line_start(candidate.line)
                    .unwrap_or(candidate.range.start),
                range: candidate.range,
                kind: candidate.kind,
            })
            .collect();
    }

    /// The foldable regions, as [`update_foldable`](Self::update_foldable)
    /// found them and moved through the edits since.
    pub fn foldable(&self) -> Vec<FoldCandidate> {
        let text = self.buffer.text();
        self.foldable
            .iter()
            .map(|foldable| FoldCandidate {
                line: text.line_of_offset(foldable.header).unwrap_or(0),
                range: foldable.range.clone(),
                kind: foldable.kind,
            })
            .collect()
    }

    /// The foldable regions whose marker is on buffer line `line`.
    fn foldable_on(&self, line: usize) -> &[Foldable] {
        let Ok(range) = self.buffer.text().line_range(line) else {
            return &[];
        };
        let first = self
            .foldable
            .partition_point(|foldable| foldable.header < range.start);
        let end = self
            .foldable
            .partition_point(|foldable| foldable.header <= range.end);
        &self.foldable[first..end.max(first)]
    }

    /// Whether a foldable region starts on buffer line `line` (where a
    /// fold marker goes).
    pub fn is_foldable(&self, line: usize) -> bool {
        !self.foldable_on(line).is_empty()
    }

    /// Whether the display row of buffer line `line` is folded: a fold
    /// starts on the line, or at the region whose marker is on it (a setext
    /// heading folds after its underline).
    pub fn is_row_folded(&self, line: usize) -> bool {
        let text = self.buffer.text();
        let Ok(range) = text.line_range(line) else {
            return false;
        };
        let folds = self.folds();
        let starts_at = |start: usize, end: usize| {
            let first = folds.partition_point(|fold| fold.range.start < start);
            folds.get(first).is_some_and(|fold| fold.range.start <= end)
        };
        starts_at(range.start, range.end)
            || self
                .foldable_on(line)
                .iter()
                .any(|foldable| starts_at(foldable.range.start, foldable.range.start))
    }

    /// Whether buffer line `line` shows a placeholder or is hidden (see
    /// [`ReplaceSnapshot::is_line_folded`]).
    pub fn is_line_folded(&self, line: usize) -> bool {
        self.display.replace().is_line_folded(line)
    }

    /// The folds, sorted by start (see [`Fold`]).
    pub fn folds(&self) -> &[Fold] {
        self.display.replace().folds()
    }

    /// The folded byte ranges, sorted: what to store to fold them again
    /// later with [`fold_ranges`](Self::fold_ranges).
    pub fn folded_ranges(&self) -> Vec<Range<usize>> {
        self.folds().iter().map(|fold| fold.range.clone()).collect()
    }

    /// Folds the region whose marker is on buffer line `line` (the largest
    /// one not folded yet), or else the smallest unfolded region that
    /// contains the line. Returns whether anything was folded.
    pub fn fold_at(&mut self, line: usize) -> bool {
        let folded: HashSet<Range<usize>> = self.folded_ranges().into_iter().collect();
        let unfolded = |foldable: &&Foldable| !folded.contains(&foldable.range);
        let on_line = self
            .foldable_on(line)
            .iter()
            .filter(unfolded)
            .max_by_key(|foldable| foldable.range.len());
        let target = on_line.or_else(|| {
            let start = self.buffer.text().line_start(line).ok()?;
            self.foldable
                .iter()
                .filter(unfolded)
                .filter(|foldable| foldable.range.start < start && start <= foldable.range.end)
                .min_by_key(|foldable| foldable.range.len())
        });
        match target.map(|foldable| foldable.range.clone()) {
            Some(range) => self.fold_ranges([range]),
            None => false,
        }
    }

    /// Unfolds the folds on the display row of buffer line `line`: those
    /// that touch the line or start at a region whose marker is on it.
    /// Returns whether anything was unfolded.
    pub fn unfold_at(&mut self, line: usize) -> bool {
        let Ok(range) = self.buffer.text().line_range(line) else {
            return false;
        };
        let mut ranges = vec![range];
        ranges.extend(
            self.foldable_on(line)
                .iter()
                .map(|foldable| foldable.range.start..foldable.range.start),
        );
        self.unfold_ranges(ranges, true)
    }

    /// Unfolds the row of `line` if it is folded
    /// ([`is_row_folded`](Self::is_row_folded)), else folds at it. Returns
    /// whether anything changed.
    pub fn toggle_fold(&mut self, line: usize) -> bool {
        if self.is_row_folded(line) {
            self.unfold_at(line)
        } else {
            self.fold_at(line)
        }
    }

    /// Folds every foldable region.
    pub fn fold_all(&mut self) -> bool {
        let ranges: Vec<Range<usize>> = self
            .foldable
            .iter()
            .map(|foldable| foldable.range.clone())
            .collect();
        self.fold_ranges(ranges)
    }

    /// Folds the sections of the headings of level `level` or deeper: the
    /// outline stays visible down to level `level` (1 folds every heading).
    pub fn fold_headings(&mut self, level: u8) -> bool {
        let ranges: Vec<Range<usize>> = self
            .foldable
            .iter()
            .filter(
                |foldable| matches!(foldable.kind, FoldKind::Heading { level: l } if l >= level),
            )
            .map(|foldable| foldable.range.clone())
            .collect();
        self.fold_ranges(ranges)
    }

    /// Unfolds the folded sections of the headings of level `level` or
    /// deeper.
    pub fn unfold_headings(&mut self, level: u8) -> bool {
        let sections: HashSet<Range<usize>> = self
            .foldable
            .iter()
            .filter(
                |foldable| matches!(foldable.kind, FoldKind::Heading { level: l } if l >= level),
            )
            .map(|foldable| foldable.range.clone())
            .collect();
        let ids: Vec<FoldId> = self
            .folds()
            .iter()
            .filter(|fold| sections.contains(&fold.range))
            .map(|fold| fold.id)
            .collect();
        if ids.is_empty() {
            return false;
        }
        self.change_folds(|map| map.remove_folds(ids))
    }

    /// Removes every fold.
    pub fn unfold_all(&mut self) -> bool {
        self.change_folds(ReplaceMap::unfold_all)
    }

    /// Folds byte ranges of the current text (any ranges: regions, a
    /// selection, ranges stored with the session). Ranges are clipped;
    /// empty ones and ranges folded already are skipped. Carets inside a
    /// new fold move to its start. Returns whether anything was folded.
    pub fn fold_ranges(&mut self, ranges: impl IntoIterator<Item = Range<usize>>) -> bool {
        let changed = self.change_folds(|map| map.fold(ranges));
        if changed {
            self.move_selections_out_of_folds();
        }
        changed
    }

    /// Removes the folds that intersect `ranges` (see
    /// [`ReplaceMap::unfold_intersecting`]). Returns whether any was.
    pub fn unfold_ranges(
        &mut self,
        ranges: impl IntoIterator<Item = Range<usize>>,
        inclusive: bool,
    ) -> bool {
        self.change_folds(|map| map.unfold_intersecting(ranges, inclusive))
    }

    /// The placeholders on display rows `rows` (wrap rows while soft wrap
    /// is on, tab rows otherwise), in order.
    pub fn fold_placeholders(&self, rows: Range<u32>) -> Vec<FoldPlaceholder> {
        let tabs = &self.display.snapshot;
        let wrap = self.display.wrap_snapshot();
        let tab_rows = match wrap {
            Some(wrap) => {
                let start = wrap.to_tab_point(WrapPoint::new(WrapRow(rows.start), 0));
                let end = wrap.to_tab_point(WrapPoint::new(WrapRow(rows.end), 0));
                start.row()..end.row().saturating_add(1)
            }
            None => rows.clone(),
        };
        tabs.replace_snapshot()
            .placeholders(tab_rows)
            .into_iter()
            .filter_map(|placeholder| {
                let tab_range = tabs.replace_point_to_tab_point(placeholder.range.start)
                    ..tabs.replace_point_to_tab_point(placeholder.range.end);
                let wrap_range = wrap.map(|wrap| {
                    wrap.tab_point_to_wrap_point(tab_range.start)
                        ..wrap.tab_point_to_wrap_point(tab_range.end)
                });
                let row = wrap_range
                    .as_ref()
                    .map_or(tab_range.start.row(), |range| range.start.row().0);
                rows.contains(&row).then_some(FoldPlaceholder {
                    fold: placeholder.fold,
                    buffer_range: placeholder.buffer_range,
                    tab_range,
                    wrap_range,
                })
            })
            .collect()
    }

    /// Changes the folds and tells observers. Returns whether the display
    /// changed.
    fn change_folds(
        &mut self,
        change: impl FnOnce(&mut ReplaceMap) -> (ReplaceSnapshot, Vec<ReplaceEdit>),
    ) -> bool {
        let (edits, wrap_edits) = self.display.change_folds(change);
        let block_changes = self.sync_blocks(&edits, &wrap_edits, None);
        if !edits.is_empty() {
            let event = EditorEvent::DisplayChanged {
                display: &self.display.snapshot,
                edits: &edits,
            };
            emit(&mut self.observers, &event);
        }
        if !wrap_edits.is_empty() {
            self.wrap_changed(&wrap_edits);
        }
        self.after_blocks_changed(block_changes);
        !edits.is_empty()
    }

    /// Moves selection ends inside a fold to its start.
    fn move_selections_out_of_folds(&mut self) {
        let replace = self.display.replace().clone();
        let out = |offset: usize| {
            replace
                .folded_range_at(offset)
                .map_or(offset, |range| range.start)
        };
        let moved = self.selections.iter().any(|selection| {
            out(selection.tail()) != selection.tail() || out(selection.head()) != selection.head()
        });
        if !moved {
            return;
        }
        self.selections
            .update(self.buffer.text(), |selection| SelectionSpec {
                tail: out(selection.tail()),
                head: out(selection.head()),
                goal: selection.goal(),
            });
        self.history.seal();
        // A fold's start can be on rows a block hides.
        let mut block_changes = BlockChanges::default();
        self.settle_block_selections(&mut block_changes);
        self.blocks_changed(&block_changes);
        self.selections_changed();
    }

    /// Unfolds the folds a selection end is inside of (a caret placed in
    /// folded text reveals it). Returns whether any was.
    fn unfold_at_selections(&mut self) -> bool {
        let replace = self.display.replace();
        if !replace.has_folds() {
            return false;
        }
        let runs: Vec<Range<usize>> = self
            .selections
            .iter()
            .flat_map(|selection| [selection.tail(), selection.head()])
            .filter_map(|offset| replace.folded_range_at(offset))
            .collect();
        if runs.is_empty() {
            return false;
        }
        self.change_folds(|map| map.unfold_intersecting(runs, false))
    }

    /// For deletions from carets: a range that would reach into a fold
    /// (Backspace after a placeholder, Delete before one) unfolds the fold
    /// instead and deletes nothing there.
    fn unfold_instead_of_deleting(&mut self, mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
        let replace = self.display.replace();
        if !replace.has_folds() {
            return ranges;
        }
        let mut runs = Vec::new();
        for range in &mut ranges {
            if let Some(run) = replace.folded_range_at(range.start) {
                runs.push(run);
                *range = range.end..range.end;
            } else if let Some(run) = replace.folded_range_at(range.end) {
                runs.push(run);
                *range = range.start..range.start;
            }
        }
        if !runs.is_empty() {
            self.change_folds(|map| map.unfold_intersecting(runs, false));
        }
        ranges
    }

    /// Moves the foldable regions through an edit.
    fn move_foldable(&mut self, summary: &EditSummary) {
        let (Some(first), Some(last)) = (summary.edits().first(), summary.edits().last()) else {
            return;
        };
        let (before, after) = (first.old.start, last.old.end);
        let shift = |offset: usize| (offset + last.new.end) - last.old.end;
        self.foldable.retain_mut(|foldable| {
            // Regions wholly before the first edit or after the last one
            // only stay or shift.
            if foldable.range.end < before {
                return true;
            }
            if foldable.header > after {
                foldable.header = shift(foldable.header);
                foldable.range = shift(foldable.range.start)..shift(foldable.range.end);
                return true;
            }
            // A line start stays with the text after it.
            foldable.header = summary.transform_offset(foldable.header, Bias::Right);
            let start = summary.transform_offset(foldable.range.start, Bias::Right);
            let end = summary.transform_offset(foldable.range.end, Bias::Left);
            foldable.range = start..end;
            start < end
        });
    }

    /// Remembers folds that the step from state `from` to `to` removed.
    fn remember_unfolded(&mut self, from: StateId, to: StateId, unfolded: &[Fold]) {
        if self.fold_restores.len() == MAX_FOLD_RESTORES {
            self.fold_restores.pop_front();
        }
        self.fold_restores.push_back(FoldRestore {
            from,
            to,
            ranges: unfolded.iter().map(|fold| fold.range.clone()).collect(),
        });
    }

    /// After an undo or redo from state `from`: brings back the folds that
    /// the step into `from` removed from the text now shown, remembers the
    /// folds this step removed, and reveals folds the restored selections
    /// are in.
    fn after_history_step(&mut self, from: StateId) {
        let to = self.history.state();
        let unfolded = std::mem::take(&mut self.step_unfolded);
        let restore = self
            .fold_restores
            .iter()
            .rposition(|restore| restore.from == to && restore.to == from)
            .and_then(|index| self.fold_restores.remove(index));
        if let Some(restore) = restore {
            self.change_folds(|map| map.fold(restore.ranges));
        }
        if !unfolded.is_empty() {
            self.remember_unfolded(from, to, &unfolded);
        }
        self.unfold_at_selections();
    }

    // ----- blocks ---------------------------------------------------------------

    /// The block layer: display rows and blocks with their heights, for
    /// painting and hit-testing in pixels ([`BlockSnapshot::items`],
    /// [`BlockSnapshot::item_at`], [`BlockSnapshot::row_top`]). Its input
    /// rows are the display rows: wrap rows while soft wrap is on, tab rows
    /// otherwise.
    pub fn blocks(&self) -> &BlockSnapshot {
        self.blocks.snapshot()
    }

    /// The block layer with its blocks, shown or not.
    pub fn block_map(&self) -> &BlockMap {
        &self.blocks
    }

    /// Block `id`, its anchors in the current text.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.block(id)
    }

    /// Where block `id` is shown; `None` if it is not (see
    /// [`BlockMap::bounds`]).
    pub fn block_bounds(&self, id: BlockId) -> Option<BlockItem> {
        self.blocks.bounds(id)
    }

    /// The height of a text row (one pixel until set).
    pub fn row_height(&self) -> Height {
        self.blocks.snapshot().row_height()
    }

    /// Sets the height of a text row (the line height of the view's font).
    /// O(1); no event, the caller knows.
    pub fn set_row_height(&mut self, height: Height) {
        self.blocks.set_row_height(height);
    }

    /// Inserts blocks (see [`BlockProperties`]); returns their ids, in
    /// order. Selection ends on rows a new replace block hides move out of
    /// them, or reveal it (see [`Reveal`]). Observers get
    /// [`EditorEvent::BlocksChanged`].
    pub fn insert_blocks(
        &mut self,
        blocks: impl IntoIterator<Item = BlockProperties>,
    ) -> Vec<BlockId> {
        let ids = self.blocks.insert(blocks);
        if !ids.is_empty() {
            self.after_blocks_changed(BlockChanges {
                inserted: ids.clone(),
                ..BlockChanges::default()
            });
        }
        ids
    }

    /// Removes blocks; returns how many there were.
    pub fn remove_blocks(&mut self, ids: impl IntoIterator<Item = BlockId>) -> usize {
        let removed = self.blocks.remove(ids);
        let count = removed.len();
        if count > 0 {
            self.after_blocks_changed(BlockChanges {
                removed: removed
                    .into_iter()
                    .map(|id| (id, Removal::Requested))
                    .collect(),
                ..BlockChanges::default()
            });
        }
        count
    }

    /// Changes the height of block `id` (an image loaded), in place.
    /// Returns whether it changed.
    pub fn resize_block(&mut self, id: BlockId, height: Height) -> bool {
        self.resize_blocks([(id, height)]) > 0
    }

    /// Changes the height of blocks; returns how many changed.
    pub fn resize_blocks(&mut self, heights: impl IntoIterator<Item = (BlockId, Height)>) -> usize {
        let resized = self.blocks.resize(heights);
        let count = resized.len();
        if count > 0 {
            self.blocks_changed(&BlockChanges {
                resized,
                ..BlockChanges::default()
            });
        }
        count
    }

    /// The top of the display row of a caret at `offset` (clipped), or of
    /// the block that hides the row.
    pub fn y_for_offset(&self, offset: usize) -> Y {
        let row = self.display.rows().row_of(offset);
        self.blocks.snapshot().row_top(row)
    }

    /// The replace block that hides the display row of a caret at `offset`,
    /// if any: for a reveal policy of the caller's own.
    pub fn block_hiding(&self, offset: usize) -> Option<BlockItem> {
        let row = self.display.rows().row_of(offset);
        self.blocks.snapshot().replacement_at(row)
    }

    /// Moves every caret `dy` pixels down (negative: up), as Page Down and
    /// Page Up do, through the block layer's heights: to the row at the
    /// caret row's top plus `dy` (clamped to the document), at the goal
    /// column. A block there is passed (to the row after it moving down,
    /// before it moving up) unless it hides rows and is revealed on
    /// selection, which the caret then enters. With `extend`, the heads
    /// move and the tails stay; without, a selection collapses from its
    /// end (down) or start (up).
    pub fn move_selections_by_pixels(&mut self, dy: f32, extend: bool) {
        self.end_composition();
        let rows = self.display.rows();
        let blocks = self.blocks.snapshot();
        self.selections.update(self.buffer.text(), |selection| {
            move_selection_by_pixels(rows, blocks, selection, dy, extend)
        });
        self.user_selection_change();
    }

    /// Brings the block layer up to the display layers after they changed:
    /// `tab_edits` and `wrap_edits` as the layers returned them, `text` the
    /// buffer edit if the text changed.
    fn sync_blocks(
        &mut self,
        tab_edits: &[TabEdit],
        wrap_edits: &[WrapEdit],
        text: Option<&EditSummary>,
    ) -> BlockChanges {
        // Wrap edits are in display rows, also when soft wrap was just
        // turned on or off; without them (soft wrap off) the tab edits are.
        if wrap_edits.is_empty() && self.display.wrap.is_none() {
            return self
                .blocks
                .sync_tab(self.display.snapshot.clone(), tab_edits, text);
        }
        let edits: Vec<RowEdit> = wrap_edits
            .iter()
            .map(|edit| RowEdit {
                old: edit.old.start.0..edit.old.end.0,
                new: edit.new.start.0..edit.new.end.0,
            })
            .collect();
        self.blocks.sync(self.display.block_input(), &edits, text)
    }

    /// After a change of the block layer outside an edit: keeps selection
    /// ends off hidden rows and tells observers.
    fn after_blocks_changed(&mut self, mut changes: BlockChanges) {
        let moved = self.settle_block_selections(&mut changes);
        self.blocks_changed(&changes);
        if moved {
            self.history.seal();
            self.selections_changed();
        }
    }

    fn blocks_changed(&mut self, changes: &BlockChanges) {
        if changes.is_empty() {
            return;
        }
        let event = EditorEvent::BlocksChanged {
            blocks: self.blocks.snapshot(),
            changes,
        };
        emit(&mut self.observers, &event);
    }

    /// Keeps selection ends off the rows replace blocks hide: removes the
    /// blocks revealed on selection that have one, then moves ends out of
    /// the others (see [`Reveal`]). Returns whether a selection moved; the
    /// removals go to `changes`. Tells no observer.
    fn settle_block_selections(&mut self, changes: &mut BlockChanges) -> bool {
        loop {
            let blocks = self.blocks.snapshot();
            if !blocks.has_replaced_rows() {
                return false;
            }
            let display = self.display.rows();
            let mut revealed: Vec<BlockId> = self
                .selections
                .iter()
                .flat_map(|selection| [selection.tail(), selection.head()])
                .filter_map(|offset| blocks.replacement_at(display.row_of(offset)))
                .filter(|item| item.reveal == Reveal::OnSelection)
                .map(|item| item.id)
                .collect();
            if revealed.is_empty() {
                break;
            }
            revealed.sort_unstable();
            revealed.dedup();
            // Another block merged into a revealed one may show and hide
            // the end again: go round.
            let removed = self.blocks.remove(revealed);
            changes
                .removed
                .extend(removed.into_iter().map(|id| (id, Removal::Revealed)));
        }
        let display = self.display.rows();
        let blocks = self.blocks.snapshot();
        let out = |offset: usize| out_of_hidden_rows(display, blocks, offset, true, None, true);
        let moved = self.selections.iter().any(|selection| {
            out(selection.tail()) != selection.tail() || out(selection.head()) != selection.head()
        });
        if moved {
            self.selections
                .update(self.buffer.text(), |selection| SelectionSpec {
                    tail: out(selection.tail()),
                    head: out(selection.head()),
                    goal: selection.goal(),
                });
        }
        moved
    }

    // ----- input method -----------------------------------------------------------

    fn utf16(&self, offset: usize) -> usize {
        self.buffer
            .offset_to_utf16(offset)
            .unwrap_or_else(|_| self.buffer.len_utf16())
            .0
    }

    fn utf16_range(&self, range: Range<usize>) -> Range<usize> {
        self.utf16(range.start)..self.utf16(range.end)
    }

    /// Clips an untrusted UTF-16 range to bytes: clamped and reordered; a
    /// non-empty range grows to whole characters (never splitting a
    /// surrogate pair) and whole CRLFs; an empty range (an insertion point)
    /// stays empty and moves before the character or CRLF it splits.
    pub fn clip_utf16_range(&self, range: Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        if range.start == range.end {
            let offset = text.clip_utf16_to_offset(OffsetUtf16(range.start), Bias::Left);
            let offset = clip_caret(text, offset, Bias::Left);
            return offset..offset;
        }
        let mut bytes = text.clip_utf16_range(OffsetUtf16(range.start)..OffsetUtf16(range.end));
        if text.is_inside_crlf(bytes.start) {
            bytes.start -= 1;
        }
        if text.is_inside_crlf(bytes.end) {
            bytes.end += 1;
        }
        bytes
    }

    /// The primary selection in UTF-16 (`selected_text_range`).
    pub fn selected_text_range_utf16(&self) -> Utf16Selection {
        let primary = self.selections.primary();
        Utf16Selection {
            range: self.utf16_range(primary.range()),
            reversed: primary.is_reversed(),
        }
    }

    /// The primary marked range in UTF-16, while composing
    /// (`marked_text_range`).
    pub fn marked_text_range_utf16(&self) -> Option<Range<usize>> {
        let composition = self.composition.as_ref()?;
        let (start, end) = composition.ranges.get(composition.primary)?;
        Some(self.utf16_range(start.offset..end.offset.max(start.offset)))
    }

    /// Every marked range in bytes (one per caret), for underlining.
    pub fn marked_ranges(&self) -> Vec<Range<usize>> {
        self.composition
            .as_ref()
            .map(|composition| {
                composition
                    .ranges
                    .iter()
                    .map(|(start, end)| start.offset..end.offset.max(start.offset))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether an input-method composition is in progress.
    pub fn is_composing(&self) -> bool {
        self.composition.is_some()
    }

    /// The text of a UTF-16 range (`text_for_range`), clipped, with the
    /// range actually returned (in UTF-16).
    pub fn text_for_range_utf16(&self, range: Range<usize>) -> (String, Range<usize>) {
        let text = self.buffer.text();
        let bytes = text.clip_utf16_range(OffsetUtf16(range.start)..OffsetUtf16(range.end));
        let string = text.text_for_range(bytes.clone()).unwrap_or_default();
        (string, self.utf16_range(bytes))
    }

    /// The byte ranges an input-method replacement applies to, one per
    /// selection, sorted and merged, with the index of the primary one.
    ///
    /// With a range, it is the primary selection's range moved by the same
    /// UTF-16 distances as the given range is from the primary selection,
    /// for every selection (so a multi-cursor composition stays aligned).
    /// Without, the marked ranges while composing, else the selections.
    fn replacement_ranges(&self, range: Option<Range<usize>>) -> (Vec<Range<usize>>, usize) {
        let primary_index = self.selections.primary_index();
        let (ranges, primary) = match range {
            Some(range) => {
                let primary = self.utf16_range(self.selections.primary().range());
                let start_delta = range.start as i128 - primary.start as i128;
                let end_delta = range.end as i128 - primary.end as i128;
                let shift = |offset: usize, delta: i128| -> usize {
                    usize::try_from((offset as i128 + delta).max(0)).unwrap_or(usize::MAX)
                };
                let ranges: Vec<Range<usize>> = self
                    .selections
                    .iter()
                    .map(|selection| {
                        let utf16 = self.utf16_range(selection.range());
                        self.clip_utf16_range(
                            shift(utf16.start, start_delta)..shift(utf16.end, end_delta),
                        )
                    })
                    .collect();
                (ranges, primary_index)
            }
            None => match &self.composition {
                Some(composition) => (
                    composition
                        .ranges
                        .iter()
                        .map(|(start, end)| start.offset..end.offset.max(start.offset))
                        .collect(),
                    composition.primary,
                ),
                None => (self.selections.ranges().collect(), primary_index),
            },
        };
        let primary_range = ranges.get(primary).cloned().unwrap_or(0..0);
        let mut sorted = ranges;
        sorted.sort_by_key(|range| (range.start, range.end));
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(sorted.len());
        for range in sorted {
            match merged.last_mut() {
                Some(last) if range.start < last.end || range == *last => {
                    last.end = last.end.max(range.end);
                }
                _ => merged.push(range),
            }
        }
        let primary = merged
            .iter()
            .position(|range| range.start <= primary_range.start && primary_range.end <= range.end)
            .unwrap_or(0);
        (merged, primary)
    }

    /// Replaces `ranges` with `text` in one transaction; returns the
    /// inserted ranges in the new text and puts a caret after each.
    fn replace_ranges(
        &mut self,
        ranges: &[Range<usize>],
        primary: usize,
        text: &str,
        kind: EditKind,
        force_group: bool,
    ) -> Vec<Range<usize>> {
        let mut transaction = Transaction::new();
        for range in ranges {
            transaction.push(range.clone(), text);
        }
        let ranges = ranges.to_vec();
        let result = self.commit(
            &transaction,
            EditCause::Edit(kind),
            force_group,
            move |selections, new_text, summary| {
                let inserted: Vec<Range<usize>> = ranges
                    .iter()
                    .map(|range| {
                        let start = summary.transform_offset(range.start, Bias::Left);
                        let end = summary.transform_offset(range.end, Bias::Right);
                        start..end.max(start)
                    })
                    .collect();
                let specs: Vec<SelectionSpec> = inserted
                    .iter()
                    .map(|range| SelectionSpec::caret(range.end))
                    .collect();
                selections.set(new_text, &specs, primary);
                inserted
            },
        );
        debug_assert!(result.is_ok(), "input-method edit rejected: {result:?}");
        result.map(|(_, inserted)| inserted).unwrap_or_default()
    }

    /// Inserts committed input-method text (`replace_text_in_range`).
    /// `range` is in UTF-16; `None` means the marked text while composing,
    /// else the selections. Ends the composition: the whole composition,
    /// including this commit, is one undo step.
    pub fn replace_text_in_range_utf16(&mut self, range: Option<Range<usize>>, text: &str) {
        let composing = self.composition.is_some();
        let (ranges, primary) = self.replacement_ranges(range);
        let kind = if composing {
            EditKind::Ime
        } else {
            EditKind::Typing
        };
        self.replace_ranges(&ranges, primary, text, kind, composing);
        if composing {
            self.end_composition();
        }
    }

    /// Replaces a range with marked (composing) text
    /// (`replace_and_mark_text_in_range`). `range` is in UTF-16 (`None`:
    /// the marked text, else the selections). `selected` is the caret or
    /// selection inside the new text, in UTF-16 relative to its start and
    /// clamped to it (`None`: a caret at its end). Empty text ends the
    /// composition.
    pub fn replace_and_mark_text_in_range_utf16(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let composing = self.composition.is_some();
        let (ranges, primary) = self.replacement_ranges(range);
        let marked = self.replace_ranges(&ranges, primary, text, EditKind::Ime, composing);
        if text.is_empty() || marked.is_empty() {
            if composing {
                self.end_composition();
            } else {
                // A composition that never started leaves no undo stop.
                self.history.seal();
            }
            return;
        }
        let (relative_start, relative_end) = match selected {
            Some(range) if range.start == range.end => {
                let caret = utf16_to_byte_in(text, range.start, Bias::Left);
                (caret, caret)
            }
            Some(range) => {
                let (start, end) = (range.start.min(range.end), range.start.max(range.end));
                (
                    utf16_to_byte_in(text, start, Bias::Left),
                    utf16_to_byte_in(text, end, Bias::Right),
                )
            }
            None => (text.len(), text.len()),
        };
        let specs: Vec<SelectionSpec> = marked
            .iter()
            .map(|range| {
                SelectionSpec::new(range.start + relative_start, range.start + relative_end)
            })
            .collect();
        self.selections.set(self.buffer.text(), &specs, primary);
        self.composition = Some(Composition {
            ranges: marked
                .iter()
                .map(|range| (Anchor::after(range.start), Anchor::before(range.end)))
                .collect(),
            primary,
        });
        emit(&mut self.observers, &EditorEvent::CompositionChanged);
        self.selections_changed();
    }

    /// Ends the composition, keeping its text (`unmark_text`). The
    /// composition becomes one undo step.
    pub fn unmark_text(&mut self) {
        self.end_composition();
    }

    fn end_composition(&mut self) {
        if self.composition.take().is_some() {
            self.history.seal();
            emit(&mut self.observers, &EditorEvent::CompositionChanged);
        }
    }
}

/// Where a selection goes under a motion (see [`Editor::move_selections`]).
fn move_selection(
    display: DisplayRows<'_>,
    blocks: &BlockSnapshot,
    selection: &Selection,
    motion: Motion,
    extend: bool,
) -> SelectionSpec {
    if !extend && !selection.is_empty() {
        match motion {
            Motion::Left => return SelectionSpec::caret(selection.start()),
            Motion::Right => return SelectionSpec::caret(selection.end()),
            _ => {}
        }
    }
    let from = if extend || selection.is_empty() {
        selection.head()
    } else {
        match motion {
            Motion::Up | Motion::UpRows(_) => selection.start(),
            Motion::Down | Motion::DownRows(_) => selection.end(),
            _ => selection.head(),
        }
    };
    let goal = if motion.is_vertical() {
        selection.goal()
    } else {
        SelectionGoal::None
    };
    let (to, goal) = display.move_caret(from, goal, motion);
    // Folded text is skipped as a whole.
    let to = match display.replace().folded_range_at(to) {
        Some(range) if to > from => range.end,
        Some(range) => range.start,
        None => to,
    };
    // So are the rows a block hides.
    let forward = match motion {
        Motion::Up | Motion::UpRows(_) => false,
        Motion::Down | Motion::DownRows(_) => true,
        _ => to >= from,
    };
    let column = match goal {
        SelectionGoal::Column(column) if motion.is_vertical() => Some(column),
        _ => None,
    };
    let to = out_of_hidden_rows(display, blocks, to, forward, column, false);
    let tail = if extend { selection.tail() } else { to };
    SelectionSpec {
        tail,
        head: to,
        goal,
    }
}

/// The first display row from `row` on (`forward`) or before `row` that no
/// block hides, passing the blocks that skip carets (every block with
/// `all`).
fn visible_row(blocks: &BlockSnapshot, mut row: u32, forward: bool, all: bool) -> Option<u32> {
    let passes = |item: &BlockItem| all || item.reveal == Reveal::Never;
    if forward {
        while row < blocks.row_count() {
            match blocks.replacement_at(row) {
                Some(item) if passes(&item) => row = item.rows.end,
                _ => return Some(row),
            }
        }
        None
    } else {
        loop {
            let previous = row.checked_sub(1)?;
            match blocks.replacement_at(previous) {
                Some(item) if passes(&item) => row = item.rows.start,
                _ => return Some(previous),
            }
        }
    }
}

/// A caret offset out of the rows a block hides, if it is on one (of a
/// block that skips carets, or of any block with `all`): `forward`, the
/// start of the first row after the block, else the end of the last row
/// before it, or the other one when there is none; at `column` instead,
/// for vertical motion.
fn out_of_hidden_rows(
    display: DisplayRows<'_>,
    blocks: &BlockSnapshot,
    offset: usize,
    forward: bool,
    column: Option<u32>,
    all: bool,
) -> usize {
    if !blocks.has_replaced_rows() {
        return offset;
    }
    let Some(item) = blocks
        .replacement_at(display.row_of(offset))
        .filter(|item| all || item.reveal == Reveal::Never)
    else {
        return offset;
    };
    let after = || {
        visible_row(blocks, item.rows.end, true, all)
            .map(|row| display.offset_at(row, column.unwrap_or(0), false))
    };
    let before = || {
        visible_row(blocks, item.rows.start, false, all)
            .map(|row| display.offset_at(row, column.unwrap_or(u32::MAX), true))
    };
    let target = if forward {
        after().or_else(before)
    } else {
        before().or_else(after)
    };
    target.unwrap_or(offset)
}

/// Where a selection goes when its caret moves `dy` pixels (see
/// [`Editor::move_selections_by_pixels`]).
fn move_selection_by_pixels(
    display: DisplayRows<'_>,
    blocks: &BlockSnapshot,
    selection: &Selection,
    dy: f32,
    extend: bool,
) -> SelectionSpec {
    let down = dy >= 0.0 || dy.is_nan();
    let from = match (extend || selection.is_empty(), down) {
        (true, _) => selection.head(),
        (false, true) => selection.end(),
        (false, false) => selection.start(),
    };
    let (row, column) = display.row_and_column(from);
    let column = match selection.goal() {
        SelectionGoal::Column(column) => column,
        SelectionGoal::None => column,
    };
    let distance = Y::from_px(f64::from(dy.abs())).0;
    let top = blocks.row_top(row).0;
    let y = if down {
        top.saturating_add(distance)
    } else {
        top.saturating_sub(distance)
    };
    let last = blocks.total_height().0.saturating_sub(1);
    let target = match blocks.item_at(Y(y.min(last))) {
        Some(DisplayItem::Row { row, .. }) => Some(row),
        Some(DisplayItem::Block(item))
            if item.kind == BlockKind::Replace && item.reveal == Reveal::OnSelection =>
        {
            Some(if down {
                item.rows.start
            } else {
                item.rows.end - 1
            })
        }
        Some(DisplayItem::Block(item)) => {
            let after = || visible_row(blocks, item.rows.end, true, false);
            let before = || visible_row(blocks, item.rows.start, false, false);
            if down {
                after().or_else(before)
            } else {
                before().or_else(after)
            }
        }
        None => None,
    };
    let to = target.map_or(from, |row| display.offset_at(row, column, !down));
    SelectionSpec {
        tail: if extend { selection.tail() } else { to },
        head: to,
        goal: SelectionGoal::Column(column),
    }
}

/// The rows vertical motion and column selection run on: wrap rows while
/// soft wrap is on, tab rows otherwise. Columns are counted in characters
/// of the display row.
#[derive(Clone, Copy, Debug)]
enum DisplayRows<'a> {
    Tab(&'a TabSnapshot),
    Wrap(&'a WrapSnapshot),
}

impl<'a> DisplayRows<'a> {
    fn replace(self) -> &'a ReplaceSnapshot {
        match self {
            Self::Tab(tabs) => tabs.replace_snapshot(),
            Self::Wrap(wrap) => wrap.tab_snapshot().replace_snapshot(),
        }
    }

    fn text(self) -> &'a TextSnapshot {
        match self {
            Self::Tab(tabs) => tabs.buffer(),
            Self::Wrap(wrap) => wrap.buffer(),
        }
    }

    /// The display row of a caret offset.
    fn row_of(self, offset: usize) -> u32 {
        match self {
            Self::Tab(tabs) => offset_to_display(tabs, offset).row(),
            Self::Wrap(wrap) => offset_to_wrap_point(wrap, offset).row().0,
        }
    }

    /// The display row of a caret offset and its column.
    fn row_and_column(self, offset: usize) -> (u32, u32) {
        match self {
            Self::Tab(tabs) => {
                let point = offset_to_display(tabs, offset);
                (point.row(), display_column(tabs, point))
            }
            Self::Wrap(wrap) => {
                let point = offset_to_wrap_point(wrap, offset);
                (point.row().0, wrap_column(wrap, point))
            }
        }
    }

    /// The columns of display row `row`.
    fn row_end_column(self, row: u32) -> u32 {
        match self {
            Self::Tab(tabs) => display_column(tabs, TabPoint::new(row, tabs.line_len(row))),
            Self::Wrap(wrap) => {
                let row = WrapRow(row);
                wrap_column(wrap, WrapPoint::new(row, wrap.line_len(row)))
            }
        }
    }

    /// The caret offset at `column` of display row `row`, or at the end of
    /// the row if it is shorter. A wrap row with no caret position (all
    /// inside an expanded tab) gives the nearest one before it with
    /// `earlier`, after it otherwise.
    fn offset_at(self, row: u32, column: u32, earlier: bool) -> usize {
        match self {
            Self::Tab(tabs) => snap_to_grapheme(
                tabs.buffer(),
                display_to_offset(tabs, point_at_column(tabs, row, column)),
            ),
            Self::Wrap(wrap) => wrap_offset_at(wrap, WrapRow(row), column, earlier),
        }
    }

    fn move_caret(
        self,
        offset: usize,
        goal: SelectionGoal,
        motion: Motion,
    ) -> (usize, SelectionGoal) {
        let wrap = match self {
            Self::Tab(tabs) => {
                let replace = tabs.replace_snapshot();
                if replace.has_folds() {
                    let text = tabs.buffer();
                    let offset = clip_caret(text, offset, Bias::Left);
                    let target = match motion {
                        Motion::LineStart => Some(row_start(replace, offset)),
                        Motion::SmartLineStart => Some(smart_row_start(replace, offset)),
                        Motion::LineEnd => Some(row_end(replace, offset)),
                        _ => None,
                    };
                    if let Some(target) = target {
                        return (clip_caret(text, target, Bias::Left), SelectionGoal::None);
                    }
                }
                return move_caret(tabs, offset, goal, motion);
            }
            Self::Wrap(wrap) => wrap,
        };
        let text = wrap.buffer();
        let offset = clip_caret(text, offset, Bias::Left);
        let target = match motion {
            Motion::Up => return self.move_rows(offset, goal, -1),
            Motion::Down => return self.move_rows(offset, goal, 1),
            Motion::UpRows(rows) => return self.move_rows(offset, goal, -i64::from(rows)),
            Motion::DownRows(rows) => return self.move_rows(offset, goal, i64::from(rows)),
            Motion::LineStart => wrap_line_start(wrap, offset, false),
            Motion::SmartLineStart => wrap_line_start(wrap, offset, true),
            Motion::LineEnd => wrap_line_end(wrap, offset),
            _ => return move_caret(wrap.tab_snapshot(), offset, goal, motion),
        };
        (clip_caret(text, target, Bias::Left), SelectionGoal::None)
    }

    /// Moves `rows` display rows (negative: up), as
    /// [`movement::move_caret`](crate::movement::move_caret) does on tab
    /// rows: past the first row the caret goes to the start of the text,
    /// past the last to its end; the goal is kept either way.
    fn move_rows(self, offset: usize, goal: SelectionGoal, rows: i64) -> (usize, SelectionGoal) {
        let (row, column) = self.row_and_column(offset);
        let column = match goal {
            SelectionGoal::Column(column) => column,
            SelectionGoal::None => column,
        };
        let goal = SelectionGoal::Column(column);
        let max_row = match self {
            Self::Tab(tabs) => tabs.max_point().row(),
            Self::Wrap(wrap) => wrap.max_point().row().0,
        };
        let target = i64::from(row) + rows;
        if target < 0 {
            return (0, goal);
        }
        if target > i64::from(max_row) {
            return (self.text().len(), goal);
        }
        let row = u32::try_from(target).unwrap_or(0);
        (self.offset_at(row, column, rows < 0), goal)
    }
}

/// The wrap point of a caret offset (clipped first). The offset of a soft
/// wrap is shown at the start of the continuation row.
fn offset_to_wrap_point(wrap: &WrapSnapshot, offset: usize) -> WrapPoint {
    let text = wrap.buffer();
    let offset = clip_caret(text, offset, Bias::Left);
    let point = text
        .offset_to_point(offset)
        .unwrap_or_else(|_| text.max_point());
    wrap.buffer_point_to_wrap_point(point, Bias::Left)
}

/// The caret offset of a wrap point; inside an expanded tab it goes to the
/// tab ([`Bias::Left`]) or after it ([`Bias::Right`]).
fn wrap_point_to_offset(wrap: &WrapSnapshot, point: WrapPoint, bias: Bias) -> usize {
    let text = wrap.buffer();
    let point = wrap.wrap_point_to_buffer_point(point, bias);
    let offset = text.point_to_offset(point).unwrap_or_else(|_| {
        text.point_to_offset(text.clip_point(point, Bias::Left))
            .unwrap_or(0)
    });
    clip_caret(text, offset, bias)
}

/// The column of `point` in characters of its wrap row, the indent
/// included.
fn wrap_column(wrap: &WrapSnapshot, point: WrapPoint) -> u32 {
    let row = point.row();
    let mut bytes = 0;
    let mut chars = 0u32;
    for chunk in wrap.chunks(row..row + WrapRow(1)) {
        for c in chunk.text.chars() {
            if bytes >= point.column() || c == '\n' {
                return chars;
            }
            bytes += c.len_utf8() as u32;
            chars += 1;
        }
    }
    chars
}

/// The caret position at character `column` of wrap row `row`: not inside
/// the continuation indent, and not at the end of a row that continues
/// (that position belongs to the next row).
fn wrap_point_at_column(wrap: &WrapSnapshot, row: WrapRow, column: u32) -> WrapPoint {
    let mut bytes = 0;
    let mut chars = 0u32;
    'row: for chunk in wrap.chunks(row..row + WrapRow(1)) {
        for c in chunk.text.chars() {
            if c == '\n' || chars == column {
                break 'row;
            }
            bytes += c.len_utf8() as u32;
            chars += 1;
        }
    }
    let indent = match row.0.checked_sub(1) {
        Some(previous) => wrap.soft_wrap_indent(WrapRow(previous)).unwrap_or(0),
        None => 0,
    };
    let point = WrapPoint::new(row, bytes.max(indent));
    if wrap.soft_wrap_indent(row).is_some() && point.column() >= wrap.line_len(row) {
        wrap.clip_point(point, Bias::Left)
    } else {
        point
    }
}

/// The caret offset at character `column` of wrap row `row`, on that row.
///
/// A tab expanded across a soft wrap has its caret positions on the rows
/// where it starts and ends, so a column inside it on another row goes
/// after it, or before it if that is on the row. A row all inside a tab
/// has no caret position: then the position before the tab (`earlier`) or
/// after it, so that moving up or down never stops on the row it started
/// from.
fn wrap_offset_at(wrap: &WrapSnapshot, row: WrapRow, column: u32, earlier: bool) -> usize {
    let text = wrap.buffer();
    let point = wrap_point_at_column(wrap, row, column);
    let before = snap_to_grapheme(text, wrap_point_to_offset(wrap, point, Bias::Left));
    if offset_to_wrap_point(wrap, before).row() == row {
        return before;
    }
    let after = snap_to_grapheme(text, wrap_point_to_offset(wrap, point, Bias::Right));
    if earlier && offset_to_wrap_point(wrap, after).row() != row {
        before
    } else {
        after
    }
}

/// Home on wrap rows: the start of a continuation row's text, then (pressed
/// there, or on the first row of a line) the line start, or the smart line
/// start with `smart` (of the display line: see [`row_start`]).
fn wrap_line_start(wrap: &WrapSnapshot, offset: usize, smart: bool) -> usize {
    let row = offset_to_wrap_point(wrap, offset).row();
    let continuation = row
        .0
        .checked_sub(1)
        .is_some_and(|previous| wrap.soft_wrap_indent(WrapRow(previous)).is_some());
    if continuation {
        let wrap_row_start = wrap_point_to_offset(wrap, WrapPoint::new(row, 0), Bias::Right);
        if offset > wrap_row_start && offset_to_wrap_point(wrap, wrap_row_start).row() == row {
            return wrap_row_start;
        }
    }
    let replace = wrap.tab_snapshot().replace_snapshot();
    if smart {
        smart_row_start(replace, offset)
    } else {
        row_start(replace, offset)
    }
}

/// End on wrap rows: before the last character of a row that continues,
/// then (pressed there, or on the last row of a line) the line end (of the
/// display line: see [`row_end`]).
fn wrap_line_end(wrap: &WrapSnapshot, offset: usize) -> usize {
    let row = offset_to_wrap_point(wrap, offset).row();
    if wrap.soft_wrap_indent(row).is_some() {
        let wrap_row_end = wrap_offset_at(wrap, row, u32::MAX, true);
        if offset < wrap_row_end {
            return wrap_row_end;
        }
    }
    row_end(wrap.tab_snapshot().replace_snapshot(), offset)
}

/// The replace row of a caret offset: its buffer line, unless a fold puts
/// it on the row of an earlier line.
fn caret_row(replace: &ReplaceSnapshot, offset: usize) -> u32 {
    let text = replace.buffer();
    let point = text
        .offset_to_point(offset)
        .unwrap_or_else(|_| text.max_point());
    replace.to_replace_point(point, Bias::Left).row()
}

fn replace_point_to_offset(replace: &ReplaceSnapshot, point: ReplacePoint, bias: Bias) -> usize {
    let text = replace.buffer();
    text.point_to_offset(replace.to_buffer_point(point, bias))
        .unwrap_or(text.len())
}

/// Home on a display line: the start of the caret's line, or with folds
/// the start of the first line its row shows.
fn row_start(replace: &ReplaceSnapshot, offset: usize) -> usize {
    if !replace.has_folds() {
        return line_start(replace.buffer(), offset);
    }
    let row = caret_row(replace, offset);
    replace_point_to_offset(replace, ReplacePoint::new(row, 0), Bias::Left)
}

/// Smart home on a display line: the first non-blank character of the
/// row's first line, or the row start if the caret is there.
fn smart_row_start(replace: &ReplaceSnapshot, offset: usize) -> usize {
    let text = replace.buffer();
    if !replace.has_folds() {
        return smart_line_start(text, offset);
    }
    let start = row_start(replace, offset);
    let indent = indent_end(text, start);
    if offset == indent { start } else { indent }
}

/// End on a display line: the end of the caret's line, or with folds the
/// end of the last line its row shows (after a placeholder that ends it).
fn row_end(replace: &ReplaceSnapshot, offset: usize) -> usize {
    if !replace.has_folds() {
        return line_end(replace.buffer(), offset);
    }
    let row = caret_row(replace, offset);
    replace_point_to_offset(
        replace,
        ReplacePoint::new(row, replace.line_len(row)),
        Bias::Right,
    )
}

/// The byte offset in `text` of a UTF-16 offset, clamped to the text and
/// moved out of a surrogate pair by `bias`.
fn utf16_to_byte_in(text: &str, utf16: usize, bias: Bias) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        if units == utf16 {
            return index;
        }
        let next = units + c.len_utf16();
        if next > utf16 {
            return match bias {
                Bias::Left => index,
                Bias::Right => index + c.len_utf8(),
            };
        }
        units = next;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets_in_a_string() {
        let text = "a😀b";
        assert_eq!(utf16_to_byte_in(text, 0, Bias::Left), 0);
        assert_eq!(utf16_to_byte_in(text, 1, Bias::Left), 1);
        assert_eq!(utf16_to_byte_in(text, 2, Bias::Left), 1);
        assert_eq!(utf16_to_byte_in(text, 2, Bias::Right), 5);
        assert_eq!(utf16_to_byte_in(text, 3, Bias::Left), 5);
        assert_eq!(utf16_to_byte_in(text, 4, Bias::Left), 6);
        assert_eq!(utf16_to_byte_in(text, 99, Bias::Right), 6);
    }

    // ----- deferred re-wraps --------------------------------------------------

    use std::cell::RefCell;
    use std::rc::Rc;

    use proptest::prelude::*;

    use crate::display_map::{LineWrapper, MarkdownHangingIndent};

    /// Lines long enough to wrap at a few columns, with list items for the
    /// hanging indent.
    fn sample() -> String {
        let mut text = String::new();
        for i in 0..60 {
            match i % 3 {
                0 => text.push_str(&format!("line {i} with some words to wrap here\n")),
                1 => text.push_str(&format!("- item {i} of a list that wraps too\n")),
                _ => text.push('\n'),
            }
        }
        text
    }

    fn editor_of(text: &str) -> Editor {
        let mut editor = Editor::new(Buffer::new(text));
        editor.set_wrap_indent(Arc::new(MarkdownHangingIndent::default()));
        editor
    }

    /// What `editor` shows: the wrap layer's text.
    fn shown(editor: &Editor) -> String {
        editor.wrap().map(WrapSnapshot::text).unwrap_or_default()
    }

    /// Runs `job` on another thread, in steps, with its own wrapper (the
    /// default measure: one unit a character).
    fn run_elsewhere(mut job: RewrapJob) -> RewrapJob {
        std::thread::spawn(move || {
            let mut wrapper = LineWrapper::new(Box::new(MonospaceMeasure::new(1.0)));
            while !job.run(&mut wrapper, 7) {}
            job
        })
        .join()
        .expect("the wrap thread")
    }

    /// The whole-text wrap edits `editor` emits.
    fn wrap_events(editor: &mut Editor) -> Rc<RefCell<Vec<Vec<WrapEdit>>>> {
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&events);
        editor.observe(move |event| {
            if let EditorEvent::WrapChanged { edits, .. } = event {
                sink.borrow_mut().push(edits.to_vec());
            }
        });
        events
    }

    #[test]
    fn a_deferred_measure_equals_a_synchronous_one() {
        let text = sample();
        let mut sync = editor_of(&text);
        let mut deferred = editor_of(&text);
        sync.set_wrap_width(Some(24.0));
        deferred.set_wrap_width(Some(24.0));
        let before = shown(&deferred);

        // Characters twice as wide (a larger font): the old wraps show
        // until the job lands.
        sync.set_measure(Box::new(MonospaceMeasure::new(2.0)));
        let mut job = deferred
            .defer_measure(Box::new(MonospaceMeasure::new(2.0)))
            .expect("a job while soft wrap is on");
        assert!(deferred.is_rewrapping());
        assert!(deferred.wrap().is_some_and(WrapSnapshot::is_interpolated));
        assert_eq!(shown(&deferred), before);
        let mut wrapper = LineWrapper::new(Box::new(MonospaceMeasure::new(2.0)));
        while !job.run(&mut wrapper, 7) {}
        assert!(deferred.finish_rewrap(job));
        assert_ne!(shown(&sync), before);
        assert_eq!(shown(&deferred), shown(&sync));

        // Soft wrap off: the measure is taken at once, for when it is on
        // again.
        deferred.set_wrap_width(None);
        assert!(
            deferred
                .defer_measure(Box::new(MonospaceMeasure::new(1.0)))
                .is_none()
        );
        deferred.set_wrap_width(Some(24.0));
        assert_eq!(shown(&deferred), before);
    }

    #[test]
    fn a_job_run_on_another_thread_equals_a_synchronous_rewrap() {
        let text = sample();
        let mut sync = editor_of(&text);
        let mut deferred = editor_of(&text);
        let events = wrap_events(&mut deferred);

        // Soft wrap turned on: the lines show unwrapped until the job lands.
        sync.set_wrap_width(Some(12.0));
        let job = deferred.defer_wrap_width(Some(12.0)).expect("a job");
        assert_eq!(deferred.wrap_width(), Some(12.0));
        assert!(deferred.is_rewrapping());
        assert!(deferred.wrap().is_some_and(WrapSnapshot::is_interpolated));
        assert_eq!(shown(&deferred), text);
        let unwrapped_rows = deferred.display().max_point().row() + 1;
        assert_eq!(
            events.borrow().as_slice(),
            [vec![WrapEdit {
                old: WrapRow(0)..WrapRow(unwrapped_rows),
                new: WrapRow(0)..WrapRow(unwrapped_rows),
            }]]
        );
        assert!(deferred.finish_rewrap(run_elsewhere(job)));
        assert!(!deferred.is_rewrapping());
        assert!(!deferred.wrap().is_some_and(WrapSnapshot::is_interpolated));
        assert_eq!(shown(&deferred), shown(&sync));
        let wrap_rows = deferred.wrap().unwrap().max_point().row().0 + 1;
        assert_eq!(
            events.borrow().last().unwrap(),
            &[WrapEdit {
                old: WrapRow(0)..WrapRow(unwrapped_rows),
                new: WrapRow(0)..WrapRow(wrap_rows),
            }]
        );

        // A new width: the old wraps show meanwhile.
        sync.set_wrap_width(Some(20.0));
        let before = shown(&deferred);
        let job = deferred.defer_wrap_width(Some(20.0)).expect("a job");
        assert_eq!(shown(&deferred), before);
        assert!(deferred.finish_rewrap(run_elsewhere(job)));
        assert_eq!(shown(&deferred), shown(&sync));

        // The same width: nothing to do. Off: at once.
        assert!(deferred.defer_wrap_width(Some(20.0)).is_none());
        assert!(deferred.defer_wrap_width(None).is_none());
        assert!(deferred.wrap().is_none());

        // On again, from the parked map.
        sync.set_wrap_width(None);
        sync.set_wrap_width(Some(9.0));
        let job = deferred.defer_wrap_width(Some(9.0)).expect("a job");
        assert_eq!(shown(&deferred), text);
        assert!(deferred.finish_rewrap(run_elsewhere(job)));
        assert_eq!(shown(&deferred), shown(&sync));
    }

    #[test]
    fn a_job_finished_part_way_wraps_the_rest_itself() {
        let text = sample();
        let mut sync = editor_of(&text);
        sync.set_wrap_width(Some(10.0));
        let mut deferred = editor_of(&text);
        let mut job = deferred.defer_wrap_width(Some(10.0)).expect("a job");
        let mut wrapper = LineWrapper::new(Box::new(MonospaceMeasure::new(1.0)));
        assert!(!job.run(&mut wrapper, 5));
        assert_eq!(job.rows_done(), 5);
        assert!(deferred.finish_rewrap(job));
        assert_eq!(shown(&deferred), shown(&sync));
    }

    #[test]
    fn edits_made_during_a_job_are_kept() {
        let text = sample();
        let edits = [
            (0..0, "new words at the start "),
            (40..45, ""),
            (70..70, "\n- x\n"),
        ];
        let mut sync = editor_of(&text);
        sync.set_wrap_width(Some(15.0));
        let mut deferred = editor_of(&text);
        deferred.set_wrap_width(Some(30.0));
        let job = deferred.defer_wrap_width(Some(15.0)).expect("a job");
        for (range, insert) in edits.iter().cloned() {
            sync.edit([(range.clone(), insert)], EditKind::Typing)
                .unwrap();
            deferred.edit([(range, insert)], EditKind::Typing).unwrap();
            assert!(deferred.wrap().is_some_and(WrapSnapshot::is_interpolated));
            // The interpolated layer shows the edited text.
            assert_eq!(shown(&deferred).replace(['\n', ' '], ""), {
                let text = deferred.text();
                text.text_for_range(0..text.len())
                    .unwrap()
                    .replace(['\n', ' '], "")
            });
        }
        // The job wrapped the text from before the edits.
        assert!(deferred.finish_rewrap(run_elsewhere(job)));
        assert_eq!(shown(&deferred), shown(&sync));
        // And later edits wrap at once again.
        sync.edit([(0..0, "more more more ")], EditKind::Typing)
            .unwrap();
        deferred
            .edit([(0..0, "more more more ")], EditKind::Typing)
            .unwrap();
        assert!(!deferred.wrap().is_some_and(WrapSnapshot::is_interpolated));
        assert_eq!(shown(&deferred), shown(&sync));
    }

    #[test]
    fn a_stale_job_is_rejected() {
        let text = sample();
        let mut sync = editor_of(&text);
        sync.set_wrap_width(Some(25.0));
        let mut editor = editor_of(&text);
        let events = wrap_events(&mut editor);

        // A newer width.
        let first = editor.defer_wrap_width(Some(11.0)).expect("a job");
        let second = editor.defer_wrap_width(Some(25.0)).expect("a job");
        let count = events.borrow().len();
        assert!(!editor.finish_rewrap(run_elsewhere(first)));
        assert_eq!(events.borrow().len(), count);
        assert!(editor.is_rewrapping());
        assert!(editor.finish_rewrap(run_elsewhere(second)));
        assert_eq!(shown(&editor), shown(&sync));

        // A synchronous re-wrap, a new measure or indent.
        let job = editor.defer_wrap_width(Some(11.0)).expect("a job");
        editor.set_wrap_width(Some(25.0));
        assert!(!editor.finish_rewrap(job));
        assert_eq!(shown(&editor), shown(&sync));
        let job = editor.defer_wrap_width(Some(11.0)).expect("a job");
        editor.set_measure(Box::new(MonospaceMeasure::new(1.0)));
        assert!(!editor.finish_rewrap(job));
        let job = editor.defer_wrap_width(Some(25.0)).expect("a job");
        editor.set_wrap_indent(Arc::new(MarkdownHangingIndent::default()));
        assert!(!editor.finish_rewrap(job));
        assert_eq!(shown(&editor), shown(&sync));

        // Soft wrap turned off, then on again: the job from before stays
        // stale, however many re-wraps the new layer has had.
        let old = editor.defer_wrap_width(Some(11.0)).expect("a job");
        editor.set_wrap_width(None);
        assert!(!editor.is_rewrapping());
        let new = editor.defer_wrap_width(Some(25.0)).expect("a job");
        let old = run_elsewhere(old);
        assert!(!editor.finish_rewrap(old));
        assert!(editor.wrap().is_some_and(WrapSnapshot::is_interpolated));
        assert!(editor.finish_rewrap(run_elsewhere(new)));
        assert_eq!(shown(&editor), shown(&sync));
        // With soft wrap off, every job is stale.
        let job = editor.defer_wrap_width(Some(11.0)).expect("a job");
        editor.set_wrap_width(None);
        assert!(!editor.finish_rewrap(job));
        assert!(editor.wrap().is_none());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Edits, widths and jobs in any order end where synchronous
        /// re-wraps do.
        #[test]
        fn deferred_rewraps_match_synchronous_ones(
            text in "[a-c \n-]{0,120}",
            steps in prop::collection::vec(
                (0u8..4, 0usize..200, "[a-c \n]{0,6}", 0u8..4, 3u32..20),
                1..12,
            ),
        ) {
            let mut sync = editor_of(&text);
            let mut deferred = editor_of(&text);
            // The newest job, and the jobs it made stale.
            let mut current: Option<RewrapJob> = None;
            let mut stale: Vec<RewrapJob> = Vec::new();
            for (kind, at, insert, len, width) in steps {
                match kind {
                    0 => {
                        let text = sync.text().clone();
                        let start = text.clip_offset(at.min(text.len()), Bias::Left);
                        let stop = text
                            .clip_offset((start + len as usize).min(text.len()), Bias::Right)
                            .max(start);
                        sync.edit([(start..stop, insert.as_str())], EditKind::Typing).unwrap();
                        deferred.edit([(start..stop, insert.as_str())], EditKind::Typing).unwrap();
                    }
                    1 => {
                        sync.set_wrap_width(Some(width as f32));
                        if let Some(job) = deferred.defer_wrap_width(Some(width as f32)) {
                            stale.extend(current.replace(job));
                        }
                    }
                    2 => {
                        sync.set_wrap_width(None);
                        prop_assert!(deferred.defer_wrap_width(None).is_none());
                        stale.extend(current.take());
                    }
                    _ => {
                        for job in stale.drain(..) {
                            prop_assert!(!deferred.finish_rewrap(run_elsewhere(job)));
                        }
                        if let Some(job) = current.take() {
                            prop_assert!(deferred.finish_rewrap(run_elsewhere(job)));
                        }
                    }
                }
            }
            for job in stale {
                prop_assert!(!deferred.finish_rewrap(job));
            }
            if let Some(job) = current {
                prop_assert!(deferred.finish_rewrap(job));
            }
            prop_assert!(!deferred.is_rewrapping());
            prop_assert_eq!(sync.wrap_width(), deferred.wrap_width());
            prop_assert_eq!(shown(&sync), shown(&deferred));
        }
    }
}

/// One selection's part of a typed edit ([`Editor::type_edits`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TypedEdit {
    /// The range replaced (the selection, or an empty range at the caret).
    pub range: Range<usize>,
    /// The text inserted there.
    pub text: String,
    /// The selection afterwards (tail..head), relative to the start of the
    /// inserted text; it may reach past it (moving over a closer).
    pub select: Range<usize>,
    /// The byte offset in `text` of a closer inserted automatically.
    pub closer: Option<usize>,
    /// Whether the caret moves over a remembered closer at `range.start`.
    pub over: bool,
}

/// The character after `offset`, if any.
pub(crate) fn char_after(text: &TextSnapshot, offset: usize) -> Option<char> {
    let end = next_grapheme_boundary(text, offset);
    if end <= offset {
        return None;
    }
    text.text_for_range(offset..end)
        .ok()
        .and_then(|s| s.chars().next())
}

/// The character before `offset`, if any.
pub(crate) fn char_before(text: &TextSnapshot, offset: usize) -> Option<char> {
    let start = prev_grapheme_boundary(text, offset);
    if start >= offset {
        return None;
    }
    text.text_for_range(start..offset)
        .ok()
        .and_then(|s| s.chars().next_back())
}

/// The opener smart pairs close with `closer`.
pub(crate) fn opener_of(closer: char) -> Option<char> {
    match closer {
        ')' => Some('('),
        ']' => Some('['),
        '}' => Some('{'),
        other => Some(other),
    }
}

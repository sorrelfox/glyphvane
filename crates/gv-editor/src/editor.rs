// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`Editor`]: the GUI-free editing model of one document.

use std::fmt;
use std::num::NonZeroU32;
use std::ops::Range;

use gv_text::{
    Anchor, Bias, Buffer, EditError, EditSummary, OffsetUtf16, Revision, TextSnapshot, Transaction,
};

use crate::boundaries::{
    clip_caret, line_end, line_start, next_grapheme_boundary, next_word_end,
    prev_grapheme_boundary, prev_word_start, snap_to_grapheme,
};
use crate::clock::{Clock, SystemClock};
use crate::display_map::{ReplaceMap, TabEdit, TabMap, TabPoint, TabSnapshot};
use crate::history::{
    EditKind, History, HistoryConfig, Record, SelectionState, StateId, UndoOutcome, UndoPlan,
    patch_of,
};
use crate::movement::{
    Motion, display_column, display_to_offset, move_caret, offset_to_display, point_at_column,
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
    /// The display map changed without a text change (the tab size).
    DisplayChanged {
        /// The display map after the change.
        display: &'a TabSnapshot,
        /// What changed in it.
        edits: &'a [TabEdit],
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

/// A state to record as saved once a save of `revision` completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SavePoint {
    /// The revision of the text being saved.
    pub revision: Revision,
    /// The history state of that text.
    pub state: StateId,
}

/// The replace and tab layers, kept in step with the buffer.
#[derive(Debug)]
struct DisplayLayers {
    replace_map: ReplaceMap,
    tab_map: TabMap,
    snapshot: TabSnapshot,
    tab_size: NonZeroU32,
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
        }
    }

    fn sync(&mut self, text: TextSnapshot, summary: &EditSummary) -> Vec<TabEdit> {
        let (replace, replace_edits) = self.replace_map.sync(text, summary);
        let (snapshot, edits) = self.tab_map.sync(replace, replace_edits, self.tab_size);
        self.snapshot = snapshot;
        edits
    }

    fn set_tab_size(&mut self, tab_size: NonZeroU32) -> Vec<TabEdit> {
        self.tab_size = tab_size;
        let replace = self.replace_map.snapshot();
        let (snapshot, edits) = self.tab_map.sync(replace, Vec::new(), tab_size);
        self.snapshot = snapshot;
        edits
    }
}

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
pub struct Editor {
    buffer: Buffer,
    selections: Selections,
    history: History,
    clock: Box<dyn Clock>,
    display: DisplayLayers,
    composition: Option<Composition>,
    observers: Vec<(ObserverId, Observer)>,
    next_observer: u64,
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
        Self {
            buffer,
            selections: Selections::new(),
            history: History::new(config.history),
            clock,
            display,
            composition: None,
            observers: Vec::new(),
            next_observer: 0,
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
        let edits = self.display.set_tab_size(tab_size);
        if !edits.is_empty() {
            let event = EditorEvent::DisplayChanged {
                display: &self.display.snapshot,
                edits: &edits,
            };
            emit(&mut self.observers, &event);
        }
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
                self.selections_changed();
            }
            return Ok((None, result));
        }
        if let EditCause::Edit(kind) = cause {
            // Replacing text with the same text changes no byte: nothing to
            // undo.
            let patch = patch_of(&old, transaction);
            if !patch.is_empty() {
                self.history.record(Record {
                    kind,
                    patch,
                    selections_before,
                    selections_after: self.selections.specs(),
                    now: self.clock.now(),
                    force_group,
                });
            }
        }
        let display_edits = self.display.sync(self.buffer.snapshot(), &summary);
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
            selections: &self.selections,
        });
        emit(&mut self.observers, &event);
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
                    DeleteUnit::Grapheme => prev_grapheme_boundary(text, head),
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

    // ----- undo ---------------------------------------------------------------

    /// Undoes the newest step and restores the selections from before it.
    /// A composition in progress is committed first.
    pub fn undo(&mut self) -> UndoOutcome {
        self.end_composition();
        match self.history.begin_undo() {
            UndoPlan::Nothing => UndoOutcome::NothingToUndo,
            UndoPlan::StopAtSavePoint => UndoOutcome::StoppedAtSavePoint,
            UndoPlan::Apply(transaction, selections) => {
                if self.apply_history_step(&transaction, EditCause::Undo, selections) {
                    self.history.commit_undo();
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
        let Some((transaction, selections)) = self.history.begin_redo() else {
            return false;
        };
        if self.apply_history_step(&transaction, EditCause::Redo, selections) {
            self.history.commit_redo();
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
    pub fn move_selections(&mut self, motion: Motion, extend: bool) {
        self.end_composition();
        let display = &self.display.snapshot;
        self.selections.update(self.buffer.text(), |selection| {
            move_selection(display, selection, motion, extend)
        });
        self.user_selection_change();
    }

    /// Column (box) selection, as with Alt-drag: one selection per display
    /// row from `tail`'s row to `head`'s, each from `tail`'s display column
    /// to `head`'s (clamped to the row). Rows that end before the left
    /// column get none. The selection on `head`'s row is primary.
    pub fn select_columns(&mut self, tail: usize, head: usize) {
        let display = &self.display.snapshot;
        let text = self.buffer.text();
        let tail_point = offset_to_display(display, tail);
        let head_point = offset_to_display(display, head);
        let tail_column = display_column(display, tail_point);
        let head_column = display_column(display, head_point);
        let left = tail_column.min(head_column);
        let rows: Vec<u32> = if tail_point.row() <= head_point.row() {
            (tail_point.row()..=head_point.row()).collect()
        } else {
            (head_point.row()..=tail_point.row()).rev().collect()
        };
        let specs: Vec<SelectionSpec> = rows
            .into_iter()
            .filter(|&row| {
                let end = TabPoint::new(row, display.line_len(row));
                left <= display_column(display, end)
            })
            .map(|row| {
                let at = |column| {
                    snap_to_grapheme(
                        text,
                        display_to_offset(display, point_at_column(display, row, column)),
                    )
                };
                SelectionSpec {
                    tail: at(tail_column),
                    head: at(head_column),
                    goal: SelectionGoal::Column(head_column),
                }
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
        let display = &self.display.snapshot;
        let from = match motion {
            Motion::Up => self.selections.first(),
            _ => self.selections.last(),
        };
        let row = offset_to_display(display, from.head()).row();
        let (to, goal) = move_caret(display, from.head(), from.goal(), motion);
        if offset_to_display(display, to).row() == row {
            return false;
        }
        self.add_selection(SelectionSpec {
            tail: to,
            head: to,
            goal,
        });
        true
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
    display: &TabSnapshot,
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
    let (to, goal) = move_caret(display, from, goal, motion);
    let tail = if extend { selection.tail() } else { to };
    SelectionSpec {
        tail,
        head: to,
        goal,
    }
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
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The undo history: transactions grouped into undo steps.
//!
//! # Entries
//!
//! Every undo step is a **patch**: a sorted list of replacements, each with
//! its range and text before and after, plus the selections before and
//! after. Undo applies the patch backwards and redo forwards, each as one
//! transaction (one pass over the rope, one [`EditSummary`] for the parser
//! and the display map), however many edits were grouped into the step.
//!
//! When a transaction joins the open step, its replacements are
//! **composed** with the step's patch (the patch composition of Zed's
//! `text::Patch`, carrying the texts along), so a step never stores more
//! than its net effect. Composition reads no buffer text: every byte it
//! needs is in one of the two patches.
//!
//! # Grouping
//!
//! See [`EditKind`] for which edits join the open step.
//!
//! # Save points
//!
//! Every text state the history can return to has a [`StateId`]. Saving
//! seals the open step and records the current state as saved, so there is
//! always an undo stop exactly at the saved text and
//! [`History::is_modified`] is exact after undo and redo. With
//! [`HistoryConfig::stop_at_save_point`], the first undo at the saved state
//! does nothing but report [`UndoOutcome::StoppedAtSavePoint`]; the next
//! one goes past it.
//!
//! [`EditSummary`]: gv_text::EditSummary

use std::collections::VecDeque;
use std::mem;
use std::ops::Range;
use std::time::Duration;

use gv_text::{TextSnapshot, Transaction};

use crate::selection::SelectionSpec;

/// What kind of edit a transaction is, which decides undo grouping.
///
/// A transaction joins the open undo step when all of these hold:
///
/// - the open step has the same kind, and the kind groups (`Typing`,
///   `DeleteBackward`, `DeleteForward`, `Ime`);
/// - no undo, redo, save, explicit break
///   ([`crate::Editor::break_undo_group`]) or selection change by the user
///   happened in between;
/// - for `Ime`: the composition is still going on (the whole composition is
///   one step, however long it takes);
/// - otherwise: at most [`HistoryConfig::group_interval`] has passed since
///   the step's last edit;
/// - for `Typing`: the text has no line break (a new line starts a new step),
///   and it does not start a word right after whitespace (so typing
///   `hello world` gives the steps `hello ` and `world`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EditKind {
    /// Typed text, including new lines.
    Typing,
    /// Backspace.
    DeleteBackward,
    /// Forward delete.
    DeleteForward,
    /// Input-method composition.
    Ime,
    /// A paste or drop: always its own step.
    Paste,
    /// A command (formatting, replace all, external change, …): always its
    /// own step.
    Command,
}

impl EditKind {
    /// Whether consecutive edits of this kind can form one undo step.
    pub fn groups(self) -> bool {
        matches!(
            self,
            Self::Typing | Self::DeleteBackward | Self::DeleteForward | Self::Ime
        )
    }
}

/// Limits and timing of the undo history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryConfig {
    /// The longest pause between two edits of one undo step.
    pub group_interval: Duration,
    /// The most undo and redo steps kept. The oldest are dropped first.
    pub max_entries: usize,
    /// The most bytes of text (inserted and deleted) the steps may hold,
    /// plus a fixed cost per step and per edit. The oldest steps are dropped
    /// first; the newest step is always kept, however large.
    pub max_bytes: usize,
    /// Whether undo stops once at the saved state (see
    /// [`UndoOutcome::StoppedAtSavePoint`]).
    pub stop_at_save_point: bool,
}

impl Default for HistoryConfig {
    /// One second, 10 000 steps, 64 MiB, no stop at the save point.
    fn default() -> Self {
        Self {
            group_interval: Duration::from_secs(1),
            max_entries: 10_000,
            max_bytes: 64 << 20,
            stop_at_save_point: false,
        }
    }
}

/// A text state the history can return to. Two equal ids mean the same
/// text; different ids may still hold equal texts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateId(pub u64);

/// What [`crate::Editor::undo`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UndoOutcome {
    /// One step was undone.
    Undone,
    /// There was nothing to undo.
    NothingToUndo,
    /// The text is at its saved state and
    /// [`HistoryConfig::stop_at_save_point`] is set: nothing changed this
    /// time, the next undo goes past the save point.
    StoppedAtSavePoint,
}

/// One replacement of a patch, with the texts on both sides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PatchEdit {
    /// The range in the text before the patch.
    pub old: Range<usize>,
    /// The range in the text after the patch.
    pub new: Range<usize>,
    /// The text `old` held.
    pub old_text: String,
    /// The text `new` holds.
    pub new_text: String,
}

impl PatchEdit {
    fn delta(&self) -> isize {
        self.new_text.len() as isize - self.old_text.len() as isize
    }
}

/// The patch of one transaction: its replacements with the replaced text
/// read from `old` (the text before), touching edits joined, and edits
/// that put back the same text dropped (so a transaction that changes no
/// byte has an empty patch).
pub(crate) fn patch_of(old: &TextSnapshot, transaction: &Transaction) -> Vec<PatchEdit> {
    let mut patch: Vec<PatchEdit> = Vec::with_capacity(transaction.replacements().len());
    let mut delta: isize = 0;
    for replacement in transaction.replacements() {
        let range = replacement.range.clone();
        if range.is_empty() && replacement.text.is_empty() {
            continue;
        }
        let old_text = if range.is_empty() {
            String::new()
        } else {
            old.text_for_range(range.clone()).unwrap_or_default()
        };
        let start = range.start.saturating_add_signed(delta);
        let new = start..start + replacement.text.len();
        delta += replacement.text.len() as isize - range.len() as isize;
        let edit = PatchEdit {
            old: range,
            new,
            old_text,
            new_text: replacement.text.clone(),
        };
        match patch.last_mut() {
            Some(last) if last.old.end == edit.old.start => {
                last.old.end = edit.old.end;
                last.new.end = edit.new.end;
                last.old_text.push_str(&edit.old_text);
                last.new_text.push_str(&edit.new_text);
            }
            _ => patch.push(edit),
        }
    }
    patch.retain(|edit| edit.old_text != edit.new_text);
    patch
}

/// The patch `a` followed by `b` (whose old ranges are in the coordinates
/// of `a`'s new text), as one patch from `a`'s old text to `b`'s new text.
/// Neither patch may have touching edits; neither has the result. Edits
/// that turn out to change nothing are dropped.
pub(crate) fn compose(a: Vec<PatchEdit>, b: Vec<PatchEdit>) -> Vec<PatchEdit> {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let mut out = Vec::with_capacity(a.len() + b.len());
    let mut a = a.into_iter().peekable();
    let mut b = b.into_iter().peekable();
    // Length changes of the edits of each patch before the current cluster.
    let mut delta_a: isize = 0;
    let mut delta_b: isize = 0;
    let mut cluster_a: Vec<PatchEdit> = Vec::new();
    let mut cluster_b: Vec<PatchEdit> = Vec::new();
    loop {
        // A cluster: edits of either patch whose ranges in the middle text
        // overlap or touch, transitively.
        let start = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(x), None) => x.new.start,
            (None, Some(y)) => y.old.start,
            (Some(x), Some(y)) => x.new.start.min(y.old.start),
        };
        let mut end = start;
        loop {
            if let Some(edit) = a.next_if(|edit| edit.new.start <= end) {
                end = end.max(edit.new.end);
                cluster_a.push(edit);
            } else if let Some(edit) = b.next_if(|edit| edit.old.start <= end) {
                end = end.max(edit.old.end);
                cluster_b.push(edit);
            } else {
                break;
            }
        }
        let cluster_delta_a: isize = cluster_a.iter().map(PatchEdit::delta).sum();
        let cluster_delta_b: isize = cluster_b.iter().map(PatchEdit::delta).sum();
        let old = start.saturating_add_signed(-delta_a)
            ..end.saturating_add_signed(-delta_a - cluster_delta_a);
        let new = start.saturating_add_signed(delta_b)
            ..end.saturating_add_signed(delta_b + cluster_delta_b);
        let edit = if cluster_b.is_empty() && cluster_a.len() == 1 {
            let mut edit = cluster_a.pop().unwrap_or_else(|| unreachable!());
            edit.new = new;
            edit
        } else if cluster_a.is_empty() && cluster_b.len() == 1 {
            let mut edit = cluster_b.pop().unwrap_or_else(|| unreachable!());
            edit.old = old;
            edit
        } else if cluster_a.len() == 1
            && cluster_b.len() == 1
            && cluster_a[0].new.start <= cluster_b[0].old.start
            && cluster_b[0].old.end <= cluster_a[0].new.end
        {
            // The common case while typing or deleting at one place: the new
            // edit lies inside (or at the end of) the step's edit, so it
            // changes that edit's new text in place.
            let b = cluster_b.pop().unwrap_or_else(|| unreachable!());
            let mut edit = cluster_a.pop().unwrap_or_else(|| unreachable!());
            let at = edit.new.start;
            edit.new_text
                .replace_range(b.old.start - at..b.old.end - at, &b.new_text);
            edit.new = new;
            edit
        } else {
            let middle = Middle {
                a: &cluster_a,
                b: &cluster_b,
            };
            let mut old_text = String::new();
            let mut pos = start;
            for edit in &cluster_a {
                middle.push(pos..edit.new.start, &mut old_text);
                old_text.push_str(&edit.old_text);
                pos = edit.new.end;
            }
            middle.push(pos..end, &mut old_text);
            let mut new_text = String::new();
            let mut pos = start;
            for edit in &cluster_b {
                middle.push(pos..edit.old.start, &mut new_text);
                new_text.push_str(&edit.new_text);
                pos = edit.old.end;
            }
            middle.push(pos..end, &mut new_text);
            cluster_a.clear();
            cluster_b.clear();
            PatchEdit {
                old,
                new,
                old_text,
                new_text,
            }
        };
        debug_assert_eq!(edit.old.len(), edit.old_text.len());
        debug_assert_eq!(edit.new.len(), edit.new_text.len());
        delta_a += cluster_delta_a;
        delta_b += cluster_delta_b;
        if edit.old_text != edit.new_text {
            out.push(edit);
        }
    }
    out
}

/// The middle text (after `a`, before `b`) of one cluster, pieced together
/// from the two patches: every byte of the cluster lies in an edit of
/// either.
struct Middle<'a> {
    a: &'a [PatchEdit],
    b: &'a [PatchEdit],
}

impl Middle<'_> {
    fn push(&self, range: Range<usize>, out: &mut String) {
        let mut pos = range.start;
        while pos < range.end {
            let a = self.a.partition_point(|edit| edit.new.end <= pos);
            if let Some(edit) = self.a.get(a).filter(|edit| edit.new.start <= pos) {
                let end = edit.new.end.min(range.end);
                out.push_str(&edit.new_text[pos - edit.new.start..end - edit.new.start]);
                pos = end;
                continue;
            }
            let b = self.b.partition_point(|edit| edit.old.end <= pos);
            if let Some(edit) = self.b.get(b).filter(|edit| edit.old.start <= pos) {
                let end = edit.old.end.min(range.end);
                out.push_str(&edit.old_text[pos - edit.old.start..end - edit.old.start]);
                pos = end;
                continue;
            }
            debug_assert!(false, "byte {pos} of a cluster is in no edit");
            break;
        }
    }
}

/// Selections in the text of one state, and the index of the primary one.
pub(crate) type SelectionState = (Vec<SelectionSpec>, usize);

/// What [`History::begin_undo`] asks the editor to do.
#[derive(Debug)]
pub(crate) enum UndoPlan {
    /// Nothing to undo.
    Nothing,
    /// At the save point, with the stop enabled: report it, change nothing.
    StopAtSavePoint,
    /// Apply the transaction, then restore the selections.
    Apply(Transaction, SelectionState),
}

#[derive(Debug)]
struct Entry {
    patch: Vec<PatchEdit>,
    kind: EditKind,
    selections_before: SelectionState,
    selections_after: SelectionState,
    last_edit_at: Duration,
    state_before: StateId,
    state_after: StateId,
    /// The last character typed, for the word-boundary rule.
    last_char: Option<char>,
    bytes: usize,
}

impl Entry {
    fn measure(&mut self) {
        let text: usize = self
            .patch
            .iter()
            .map(|edit| edit.old_text.len() + edit.new_text.len())
            .sum();
        self.bytes = mem::size_of::<Self>()
            + text
            + self.patch.len() * mem::size_of::<PatchEdit>()
            + (self.selections_before.0.len() + self.selections_after.0.len())
                * mem::size_of::<SelectionSpec>();
    }
}

/// One transaction to record.
#[derive(Debug)]
pub(crate) struct Record {
    pub kind: EditKind,
    pub patch: Vec<PatchEdit>,
    pub selections_before: SelectionState,
    pub selections_after: SelectionState,
    pub now: Duration,
    /// Join the open step regardless of time and text (IME composition).
    pub force_group: bool,
}

/// Undo and redo steps, the save point, and the grouping state.
#[derive(Debug)]
pub struct History {
    config: HistoryConfig,
    undo: VecDeque<Entry>,
    redo: Vec<Entry>,
    /// Whether the newest undo step may take more edits.
    open: bool,
    bytes: usize,
    current: StateId,
    saved: Option<StateId>,
    next_state: u64,
    /// The save-point stop was reported; the next undo goes past it.
    save_stop_reported: bool,
}

impl History {
    pub(crate) fn new(config: HistoryConfig) -> Self {
        Self {
            config,
            undo: VecDeque::new(),
            redo: Vec::new(),
            open: false,
            bytes: 0,
            current: StateId(0),
            saved: Some(StateId(0)),
            next_state: 1,
            save_stop_reported: false,
        }
    }

    /// The configuration.
    pub fn config(&self) -> HistoryConfig {
        self.config
    }

    pub(crate) fn set_config(&mut self, config: HistoryConfig) {
        self.config = config;
        self.enforce_limits();
    }

    /// Number of steps that can be undone.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Number of steps that can be redone.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Whether undo would change the text.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether redo would change the text.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The bytes the steps hold, as counted against
    /// [`HistoryConfig::max_bytes`].
    pub fn memory_bytes(&self) -> usize {
        self.bytes
    }

    /// Whether the newest step may still take more edits.
    pub fn is_group_open(&self) -> bool {
        self.open
    }

    /// The current text state.
    pub fn state(&self) -> StateId {
        self.current
    }

    /// The saved text state, if the history still knows it.
    pub fn saved_state(&self) -> Option<StateId> {
        self.saved
    }

    /// Whether the text differs from the saved state.
    pub fn is_modified(&self) -> bool {
        self.saved != Some(self.current)
    }

    fn fresh_state(&mut self) -> StateId {
        let state = StateId(self.next_state);
        self.next_state += 1;
        state
    }

    /// Closes the open step: the next edit starts a new one. A step whose
    /// edits cancelled out (a composition typed and then cancelled) is
    /// dropped.
    pub(crate) fn seal(&mut self) {
        self.open = false;
        if self.undo.back().is_some_and(|entry| entry.patch.is_empty())
            && let Some(entry) = self.undo.pop_back()
        {
            self.bytes -= entry.bytes;
            self.current = entry.state_before;
        }
    }

    /// Records the current state as saved (sealing the open step) and
    /// returns it.
    pub(crate) fn mark_saved(&mut self) -> StateId {
        self.seal();
        self.saved = Some(self.current);
        self.save_stop_reported = false;
        self.current
    }

    /// Records `state` as saved. Use the state returned by
    /// [`History::save_point`] when the save started; if the user has
    /// edited since, the text is modified.
    pub(crate) fn mark_saved_state(&mut self, state: StateId) {
        self.saved = Some(state);
        self.save_stop_reported = false;
    }

    /// Seals the open step and returns the current state, for a save that
    /// finishes later.
    pub(crate) fn save_point(&mut self) -> StateId {
        self.seal();
        self.current
    }

    /// Drops every step. The current text stays saved if it was.
    pub(crate) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
        self.open = false;
    }

    fn can_group(&self, record: &Record, first_char: Option<char>, line_break: bool) -> bool {
        let Some(top) = self.undo.back() else {
            return false;
        };
        if !self.open || top.kind != record.kind {
            return false;
        }
        if record.force_group {
            return true;
        }
        if !record.kind.groups() || record.kind == EditKind::Ime {
            return false;
        }
        if record.now.saturating_sub(top.last_edit_at) > self.config.group_interval {
            return false;
        }
        if record.kind == EditKind::Typing {
            if line_break {
                return false;
            }
            let after_space = top.last_char.is_some_and(char::is_whitespace);
            if after_space && first_char.is_some_and(|c| !c.is_whitespace()) {
                return false;
            }
        }
        true
    }

    /// Records a transaction: it joins the open step or starts a new one.
    /// Clears the redo steps.
    pub(crate) fn record(&mut self, record: Record) {
        let inserted = record.patch.first().map(|edit| edit.new_text.as_str());
        let first_char = inserted.and_then(|text| text.chars().next());
        let last_char = inserted.and_then(|text| text.chars().next_back());
        let line_break = inserted.is_some_and(|text| text.contains(['\n', '\r']));

        for entry in self.redo.drain(..) {
            self.bytes -= entry.bytes;
        }
        self.save_stop_reported = false;
        let state = self.fresh_state();
        let group = self.can_group(&record, first_char, line_break);
        if group && let Some(top) = self.undo.back_mut() {
            self.bytes -= top.bytes;
            top.patch = compose(mem::take(&mut top.patch), record.patch);
            top.selections_after = record.selections_after;
            top.last_edit_at = record.now;
            top.state_after = state;
            if last_char.is_some() {
                top.last_char = last_char;
            }
            top.measure();
            self.bytes += top.bytes;
        } else {
            self.seal();
            let mut entry = Entry {
                patch: record.patch,
                kind: record.kind,
                selections_before: record.selections_before,
                selections_after: record.selections_after,
                last_edit_at: record.now,
                state_before: self.current,
                state_after: state,
                last_char,
                bytes: 0,
            };
            entry.measure();
            self.bytes += entry.bytes;
            self.undo.push_back(entry);
        }
        self.current = state;
        self.open = record.kind.groups() || record.force_group;
        self.enforce_limits();
    }

    fn enforce_limits(&mut self) {
        let max_entries = self.config.max_entries.max(1);
        while self.undo.len() + self.redo.len() > max_entries || self.bytes > self.config.max_bytes
        {
            if self.undo.len() > 1 {
                if let Some(entry) = self.undo.pop_front() {
                    self.bytes -= entry.bytes;
                }
            } else if !self.redo.is_empty() {
                let entry = self.redo.remove(0);
                self.bytes -= entry.bytes;
            } else {
                break;
            }
        }
    }

    /// The transaction that undoes the newest step, and the selections to
    /// restore, without changing the history; [`History::commit_undo`]
    /// moves the step once the transaction is applied. Seals the open step
    /// first.
    pub(crate) fn begin_undo(&mut self) -> UndoPlan {
        self.seal();
        let Some(entry) = self.undo.back() else {
            return UndoPlan::Nothing;
        };
        if self.config.stop_at_save_point
            && self.saved == Some(self.current)
            && !self.save_stop_reported
        {
            self.save_stop_reported = true;
            return UndoPlan::StopAtSavePoint;
        }
        let mut transaction = Transaction::new();
        for edit in &entry.patch {
            transaction.push(edit.new.clone(), edit.old_text.clone());
        }
        UndoPlan::Apply(transaction, entry.selections_before.clone())
    }

    /// Moves the newest undo step to the redo steps.
    pub(crate) fn commit_undo(&mut self) {
        if let Some(entry) = self.undo.pop_back() {
            self.current = entry.state_before;
            self.redo.push(entry);
        }
        self.save_stop_reported = false;
    }

    /// The transaction that redoes the next step, and the selections to
    /// restore; [`History::commit_redo`] moves the step.
    pub(crate) fn begin_redo(&mut self) -> Option<(Transaction, SelectionState)> {
        self.seal();
        let entry = self.redo.last()?;
        let mut transaction = Transaction::new();
        for edit in &entry.patch {
            transaction.push(edit.old.clone(), edit.new_text.clone());
        }
        Some((transaction, entry.selections_after.clone()))
    }

    /// Moves the next redo step back to the undo steps.
    pub(crate) fn commit_redo(&mut self) {
        if let Some(entry) = self.redo.pop() {
            self.current = entry.state_after;
            self.undo.push_back(entry);
        }
        self.save_stop_reported = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn apply(text: &str, patch: &[PatchEdit]) -> String {
        let mut out = String::new();
        let mut pos = 0;
        for edit in patch {
            assert_eq!(
                &text[edit.old.clone()],
                edit.old_text,
                "old text of {edit:?}"
            );
            out.push_str(&text[pos..edit.old.start]);
            assert_eq!(out.len(), edit.new.start, "new start of {edit:?}");
            out.push_str(&edit.new_text);
            pos = edit.old.end;
        }
        out.push_str(&text[pos..]);
        out
    }

    fn invert(patch: &[PatchEdit]) -> Vec<PatchEdit> {
        patch
            .iter()
            .map(|edit| PatchEdit {
                old: edit.new.clone(),
                new: edit.old.clone(),
                old_text: edit.new_text.clone(),
                new_text: edit.old_text.clone(),
            })
            .collect()
    }

    fn patch(text: &str, edits: &[(Range<usize>, &str)]) -> Vec<PatchEdit> {
        let snapshot = TextSnapshot::new(text);
        let mut transaction = Transaction::new();
        for (range, new) in edits {
            transaction.push(range.clone(), *new);
        }
        patch_of(&snapshot, &transaction)
    }

    #[test]
    fn typing_composes_into_one_edit() {
        let a = patch("hello", &[(5..5, " ")]);
        let b = patch("hello ", &[(6..6, "w")]);
        let c = compose(a, b);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].old, 5..5);
        assert_eq!(c[0].new, 5..7);
        assert_eq!(c[0].new_text, " w");
    }

    #[test]
    fn deletes_compose() {
        // Backspace twice from the end of "abcd".
        let a = patch("abcd", &[(3..4, "")]);
        let b = patch("abc", &[(2..3, "")]);
        let c = compose(a, b);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].old, 2..4);
        assert_eq!(c[0].old_text, "cd");
        assert_eq!(apply("abcd", &c), "ab");
    }

    #[test]
    fn multi_cursor_typing_keeps_one_edit_per_cursor() {
        let a = patch("a\nb\nc", &[(1..1, "x"), (3..3, "x"), (5..5, "x")]);
        let b = patch("ax\nbx\ncx", &[(2..2, "y"), (5..5, "y"), (8..8, "y")]);
        let c = compose(a, b);
        assert_eq!(c.len(), 3);
        assert_eq!(apply("a\nb\nc", &c), "axy\nbxy\ncxy");
    }

    #[test]
    fn cancelled_edits_vanish() {
        let a = patch("abc", &[(1..1, "xy")]);
        let b = patch("axybc", &[(1..3, "")]);
        assert!(compose(a, b).is_empty());
    }

    #[test]
    fn touching_edits_join() {
        let p = patch("abcdef", &[(1..2, "X"), (2..2, "Y"), (2..4, "")]);
        assert_eq!(p.len(), 1);
        assert_eq!(apply("abcdef", &p), "aXYef");
    }

    fn arb_patch(text: String) -> impl Strategy<Value = (String, Vec<(Range<usize>, String)>)> {
        let len = text.len();
        prop::collection::vec((0..=len, 0..=len, "[a-c\u{e9}]{0,3}"), 0..5).prop_map(move |raw| {
            // Sorted, non-overlapping ranges on char boundaries.
            let mut points: Vec<(usize, usize, String)> = raw
                .into_iter()
                .map(|(x, y, s)| {
                    let (x, y) = (x.min(y), x.max(y));
                    let x = (0..=x)
                        .rev()
                        .find(|&i| text.is_char_boundary(i))
                        .unwrap_or(0);
                    let y = (y..=len).find(|&i| text.is_char_boundary(i)).unwrap_or(len);
                    (x, y, s)
                })
                .collect();
            points.sort_by_key(|(x, y, _)| (*x, *y));
            let mut edits: Vec<(Range<usize>, String)> = Vec::new();
            let mut last_end = 0;
            for (x, y, s) in points {
                if x < last_end {
                    continue;
                }
                last_end = y;
                edits.push((x..y, s));
            }
            (text.clone(), edits)
        })
    }

    fn arb_text() -> impl Strategy<Value = String> {
        "[a-d\u{e9}\n]{0,24}"
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, failure_persistence: None, ..ProptestConfig::default() })]

        /// Composing two patches equals applying them in turn, and the
        /// inverse of the composition goes back.
        #[test]
        fn compose_equals_sequential(
            (base, first) in arb_text().prop_flat_map(arb_patch),
            seed in any::<u64>(),
        ) {
            let first_refs: Vec<(Range<usize>, &str)> =
                first.iter().map(|(r, s)| (r.clone(), s.as_str())).collect();
            let a = patch(&base, &first_refs);
            let middle = apply(&base, &a);
            // A second patch on the middle text, derived from the seed.
            let mut rng = seed;
            let mut next = || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
            let mut second: Vec<(Range<usize>, String)> = Vec::new();
            let mut pos = 0;
            while pos <= middle.len() && second.len() < 4 {
                let start = pos + (next() as usize % (middle.len() - pos + 1));
                let len = next() as usize % 3;
                let mut end = (start + len).min(middle.len());
                let start = (0..=start).rev().find(|&i| middle.is_char_boundary(i)).unwrap_or(0);
                if start < pos { break; }
                while !middle.is_char_boundary(end) { end += 1; }
                let text = ["", "z", "\u{e9}q", "\n"][next() as usize % 4].to_string();
                second.push((start..end, text));
                pos = end + 1;
            }
            let second_refs: Vec<(Range<usize>, &str)> =
                second.iter().map(|(r, s)| (r.clone(), s.as_str())).collect();
            let b = patch(&middle, &second_refs);
            let expected = apply(&middle, &b);
            let c = compose(a, b);
            prop_assert_eq!(&apply(&base, &c), &expected);
            prop_assert_eq!(apply(&expected, &invert(&c)), base);
            for pair in c.windows(2) {
                prop_assert!(pair[0].old.end < pair[1].old.start);
                prop_assert!(pair[0].new.end < pair[1].new.start);
            }
        }
    }
}

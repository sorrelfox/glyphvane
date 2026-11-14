// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Selections: one or more ranges, each with a tail (where it started) and
//! a head (the caret).

use std::ops::Range;

use gv_text::{Anchor, Bias, EditSummary, TextSnapshot};

use crate::boundaries::clip_caret;

/// Identifies a selection for as long as it exists. Newer selections have
/// larger ids; the newest one is the primary selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SelectionId(pub u64);

/// Where vertical movement wants the caret to be, so that moving through a
/// short line and back returns to the original column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SelectionGoal {
    /// No goal: the next vertical move starts from the caret's column.
    #[default]
    None,
    /// A display column, counted in characters of the tab-expanded row.
    Column(u32),
}

/// One selection. `tail..head` may run backwards; an empty selection is a
/// caret.
///
/// Both ends are [`Anchor`]s in the buffer's current revision. The editor
/// moves them through every edit. Their biases follow the shape of the
/// selection: a caret sticks to the right of text inserted at it, and a
/// non-empty selection does not grow when text is inserted at either end
/// (its start sticks right, its end left).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Selection {
    id: SelectionId,
    tail: Anchor,
    head: Anchor,
    goal: SelectionGoal,
}

impl Selection {
    pub(crate) fn new(id: SelectionId, tail: usize, head: usize, goal: SelectionGoal) -> Self {
        let mut selection = Self {
            id,
            tail: Anchor::before(tail),
            head: Anchor::before(head),
            goal,
        };
        selection.rebias();
        selection
    }

    fn rebias(&mut self) {
        let (tail, head) = (self.tail.offset, self.head.offset);
        let (tail_bias, head_bias) = match tail.cmp(&head) {
            std::cmp::Ordering::Equal => (Bias::Right, Bias::Right),
            std::cmp::Ordering::Less => (Bias::Right, Bias::Left),
            std::cmp::Ordering::Greater => (Bias::Left, Bias::Right),
        };
        self.tail = Anchor::new(tail, tail_bias);
        self.head = Anchor::new(head, head_bias);
    }

    /// The selection's id.
    pub fn id(&self) -> SelectionId {
        self.id
    }

    /// Where the selection started (byte offset).
    pub fn tail(&self) -> usize {
        self.tail.offset
    }

    /// The caret end of the selection (byte offset).
    pub fn head(&self) -> usize {
        self.head.offset
    }

    /// The tail as an anchor.
    pub fn tail_anchor(&self) -> Anchor {
        self.tail
    }

    /// The head as an anchor.
    pub fn head_anchor(&self) -> Anchor {
        self.head
    }

    /// The vertical-movement goal.
    pub fn goal(&self) -> SelectionGoal {
        self.goal
    }

    /// The smaller end.
    pub fn start(&self) -> usize {
        self.tail.offset.min(self.head.offset)
    }

    /// The larger end.
    pub fn end(&self) -> usize {
        self.tail.offset.max(self.head.offset)
    }

    /// `start..end`.
    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    /// Whether this is a caret.
    pub fn is_empty(&self) -> bool {
        self.tail.offset == self.head.offset
    }

    /// Whether the head is before the tail.
    pub fn is_reversed(&self) -> bool {
        self.head.offset < self.tail.offset
    }

    fn transformed(&self, edits: &EditSummary) -> Self {
        let mut selection = Self {
            id: self.id,
            tail: self.tail.transform(edits),
            head: self.head.transform(edits),
            goal: SelectionGoal::None,
        };
        selection.rebias();
        selection
    }
}

/// A plain description of a selection, for setting selections and for the
/// undo history: offsets in one particular text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SelectionSpec {
    /// Where the selection starts.
    pub tail: usize,
    /// The caret.
    pub head: usize,
    /// The vertical-movement goal.
    pub goal: SelectionGoal,
}

impl SelectionSpec {
    /// A caret at `offset`.
    pub const fn caret(offset: usize) -> Self {
        Self {
            tail: offset,
            head: offset,
            goal: SelectionGoal::None,
        }
    }

    /// A selection from `tail` to `head`.
    pub const fn new(tail: usize, head: usize) -> Self {
        Self {
            tail,
            head,
            goal: SelectionGoal::None,
        }
    }

    /// A forward selection of `range`.
    pub const fn range(range: Range<usize>) -> Self {
        Self::new(range.start, range.end)
    }
}

/// The selections of an editor: never empty, sorted, and merged.
///
/// # Invariants
///
/// After every editor operation:
///
/// - there is at least one selection;
/// - selections are sorted by position;
/// - selections do not overlap: each one ends at or before the next one's
///   start, and a caret never touches another selection (two carets at one
///   offset, or a caret at the edge of a selection, are merged into one);
/// - every end is a character boundary inside the text and never between
///   the CR and the LF of a CRLF.
///
/// [`Selections::check`] verifies them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selections {
    list: Vec<Selection>,
    next_id: u64,
}

impl Selections {
    /// A single caret at the start of the text.
    pub(crate) fn new() -> Self {
        Self {
            list: vec![Selection::new(SelectionId(0), 0, 0, SelectionGoal::None)],
            next_id: 1,
        }
    }

    /// The selections in order.
    pub fn as_slice(&self) -> &[Selection] {
        &self.list
    }

    /// Iterates the selections in order.
    pub fn iter(&self) -> std::slice::Iter<'_, Selection> {
        self.list.iter()
    }

    /// The number of selections (at least 1).
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Always false: there is at least one selection.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The index of the primary (newest) selection.
    pub fn primary_index(&self) -> usize {
        self.list
            .iter()
            .enumerate()
            .max_by_key(|(_, selection)| selection.id)
            .map_or(0, |(index, _)| index)
    }

    /// The primary (newest) selection: the one IME and scrolling follow.
    pub fn primary(&self) -> &Selection {
        &self.list[self.primary_index()]
    }

    /// The first selection in the text.
    pub fn first(&self) -> &Selection {
        &self.list[0]
    }

    /// The last selection in the text.
    pub fn last(&self) -> &Selection {
        &self.list[self.list.len() - 1]
    }

    /// The selection with `id`, if it still exists.
    pub fn get(&self, id: SelectionId) -> Option<&Selection> {
        self.list.iter().find(|selection| selection.id == id)
    }

    /// The ranges of all selections, in order.
    pub fn ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.list.iter().map(Selection::range)
    }

    /// The selections as plain specs, in order, with the index of the
    /// primary one.
    pub fn specs(&self) -> (Vec<SelectionSpec>, usize) {
        let specs = self
            .list
            .iter()
            .map(|selection| SelectionSpec {
                tail: selection.tail(),
                head: selection.head(),
                goal: selection.goal,
            })
            .collect();
        (specs, self.primary_index())
    }

    /// Checks the invariants against `text`.
    pub fn check(&self, text: &TextSnapshot) -> Result<(), String> {
        if self.list.is_empty() {
            return Err("no selections".into());
        }
        for (index, selection) in self.list.iter().enumerate() {
            for offset in [selection.tail(), selection.head()] {
                if !text.is_char_boundary(offset) {
                    return Err(format!(
                        "selection {index}: {offset} is not a char boundary"
                    ));
                }
                if text.is_inside_crlf(offset) {
                    return Err(format!("selection {index}: {offset} is inside a CRLF"));
                }
            }
            if let Some(next) = self.list.get(index + 1) {
                let touching = selection.end() == next.start();
                if selection.end() > next.start()
                    || (touching && (selection.is_empty() || next.is_empty()))
                {
                    return Err(format!(
                        "selections {index} ({:?}) and {} ({:?}) overlap",
                        selection.range(),
                        index + 1,
                        next.range()
                    ));
                }
            }
        }
        Ok(())
    }

    fn fresh_id(&mut self) -> SelectionId {
        let id = SelectionId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Replaces all selections. `primary` is the index in `specs` of the
    /// selection that becomes primary (it gets the newest id). Specs are
    /// clipped to `text`, then sorted and merged.
    pub(crate) fn set(&mut self, text: &TextSnapshot, specs: &[SelectionSpec], primary: usize) {
        let primary = primary.min(specs.len().saturating_sub(1));
        let mut list = Vec::with_capacity(specs.len().max(1));
        for (index, spec) in specs.iter().enumerate() {
            if index == primary {
                continue;
            }
            list.push(Selection::new(
                self.fresh_id(),
                spec.tail,
                spec.head,
                spec.goal,
            ));
        }
        // The primary selection is created last, so it has the newest id.
        if let Some(spec) = specs.get(primary) {
            let id = self.fresh_id();
            list.push(Selection::new(id, spec.tail, spec.head, spec.goal));
        }
        if list.is_empty() {
            list.push(Selection::new(self.fresh_id(), 0, 0, SelectionGoal::None));
        }
        self.list = list;
        self.normalize(text);
    }

    /// Adds a selection, which becomes primary.
    pub(crate) fn add(&mut self, text: &TextSnapshot, spec: SelectionSpec) -> SelectionId {
        let id = self.fresh_id();
        self.list
            .push(Selection::new(id, spec.tail, spec.head, spec.goal));
        self.normalize(text);
        // The new selection may have been merged into an older one; the
        // merged selection keeps the newest id, so `id` still names it.
        id
    }

    /// Removes the selection with `id` unless it is the only one.
    pub(crate) fn remove(&mut self, id: SelectionId) -> bool {
        if self.list.len() <= 1 {
            return false;
        }
        let before = self.list.len();
        self.list.retain(|selection| selection.id != id);
        before != self.list.len()
    }

    /// Changes each selection with `f` (which gets the selection and returns
    /// its new tail, head and goal), then clips, sorts and merges. Ids are
    /// kept.
    pub(crate) fn update(
        &mut self,
        text: &TextSnapshot,
        mut f: impl FnMut(&Selection) -> SelectionSpec,
    ) {
        for selection in &mut self.list {
            let spec = f(selection);
            *selection = Selection::new(selection.id, spec.tail, spec.head, spec.goal);
        }
        self.normalize(text);
    }

    /// Moves every selection through `edits`, the summary of a transaction
    /// whose result is `text`. Goals are dropped.
    pub(crate) fn transform(&mut self, text: &TextSnapshot, edits: &EditSummary) {
        for selection in &mut self.list {
            *selection = selection.transformed(edits);
        }
        self.normalize(text);
    }

    /// Clips every end to a caret position, then sorts and merges.
    fn normalize(&mut self, text: &TextSnapshot) {
        for selection in &mut self.list {
            let tail = clip_caret(text, selection.tail(), Bias::Left);
            let head = clip_caret(text, selection.head(), Bias::Left);
            if tail != selection.tail() || head != selection.head() {
                *selection = Selection::new(selection.id, tail, head, selection.goal);
            }
        }
        let sorted = self
            .list
            .windows(2)
            .all(|pair| (pair[0].start(), pair[0].end()) <= (pair[1].start(), pair[1].end()));
        if !sorted {
            self.list
                .sort_by_key(|selection| (selection.start(), selection.end()));
        }
        let needs_merge = self
            .list
            .windows(2)
            .any(|pair| overlaps(&pair[0], &pair[1]));
        if !needs_merge {
            return;
        }
        let mut merged: Vec<Selection> = Vec::with_capacity(self.list.len());
        for selection in self.list.drain(..) {
            match merged.last_mut() {
                Some(last) if overlaps(last, &selection) => *last = merge(last, &selection),
                _ => merged.push(selection),
            }
        }
        self.list = merged;
    }
}

/// Whether `b` (which starts at or after `a`) must be merged into `a`.
fn overlaps(a: &Selection, b: &Selection) -> bool {
    b.start() < a.end() || (b.start() == a.end() && (a.is_empty() || b.is_empty()))
}

/// One selection covering both. It keeps the newer id and goal, and the
/// direction of the newer one if it is not a caret.
fn merge(a: &Selection, b: &Selection) -> Selection {
    let (newer, older) = if a.id > b.id { (a, b) } else { (b, a) };
    let start = a.start().min(b.start());
    let end = a.end().max(b.end());
    let reversed = if newer.is_empty() {
        older.is_reversed()
    } else {
        newer.is_reversed()
    };
    let (tail, head) = if reversed { (end, start) } else { (start, end) };
    Selection::new(newer.id, tail, head, newer.goal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(ranges: &[(usize, usize)]) -> Vec<SelectionSpec> {
        ranges
            .iter()
            .map(|&(tail, head)| SelectionSpec::new(tail, head))
            .collect()
    }

    fn ranges(selections: &Selections) -> Vec<(usize, usize)> {
        selections
            .iter()
            .map(|selection| (selection.tail(), selection.head()))
            .collect()
    }

    #[test]
    fn sorted_and_merged() {
        let text = TextSnapshot::new("0123456789abcdef");
        let mut selections = Selections::new();
        selections.set(
            &text,
            &specs(&[
                (9, 7),
                (1, 1),
                (1, 1),
                (2, 4),
                (4, 6),
                (6, 6),
                (12, 12),
                (11, 13),
            ]),
            0,
        );
        // (1,1) twice merges; (2,4) and (4,6) touch and stay apart, but the
        // caret at 6 joins (4,6); (9,7) stays reversed; the caret at 12
        // joins (11,13).
        assert_eq!(
            ranges(&selections),
            [(1, 1), (2, 4), (4, 6), (9, 7), (11, 13)]
        );
        selections.check(&text).unwrap();
        // The primary was spec 0, (9,7).
        assert_eq!(selections.primary().range(), 7..9);
        assert!(selections.primary().is_reversed());
    }

    #[test]
    fn clipped_into_the_text() {
        let text = TextSnapshot::new("aé\r\nb");
        let mut selections = Selections::new();
        selections.set(&text, &specs(&[(2, 2), (4, 4), (99, 3)]), 1);
        // 2 is inside `é` (left: 1); 4 is inside the CRLF (before the CR,
        // 3), merging with the last selection, clipped to (6, 3).
        assert_eq!(ranges(&selections), [(1, 1), (6, 3)]);
        selections.check(&text).unwrap();
    }

    #[test]
    fn biases_follow_the_shape() {
        let caret = Selection::new(SelectionId(0), 3, 3, SelectionGoal::None);
        assert_eq!(caret.head_anchor().bias, Bias::Right);
        let forward = Selection::new(SelectionId(0), 3, 5, SelectionGoal::None);
        assert_eq!(forward.tail_anchor().bias, Bias::Right);
        assert_eq!(forward.head_anchor().bias, Bias::Left);
        let backward = Selection::new(SelectionId(0), 5, 3, SelectionGoal::None);
        assert_eq!(backward.tail_anchor().bias, Bias::Left);
        assert_eq!(backward.head_anchor().bias, Bias::Right);
    }

    #[test]
    fn transform_through_edits() {
        let mut buffer = gv_text::Buffer::new("0123456789");
        let mut selections = Selections::new();
        selections.set(buffer.text(), &specs(&[(2, 2), (4, 6)]), 0);
        // Insertions at the caret and at both ends of the selection.
        let summary = buffer
            .edit([(2..2, "ab"), (4..4, "cd"), (6..6, "ef")])
            .unwrap();
        selections.transform(buffer.text(), &summary);
        assert_eq!(buffer.to_string(), "01ab23cd45ef6789");
        assert_eq!(ranges(&selections), [(4, 4), (8, 10)]);
    }

    #[test]
    fn remove_keeps_one() {
        let text = TextSnapshot::new("abc");
        let mut selections = Selections::new();
        selections.set(&text, &specs(&[(0, 0), (2, 2)]), 0);
        let first = selections.first().id();
        let last = selections.last().id();
        assert!(selections.remove(first));
        assert!(!selections.remove(last));
        assert_eq!(selections.len(), 1);
    }
}

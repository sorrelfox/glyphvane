// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The replacements other than folds, in a sum tree.
//!
//! Each item stores the bytes from the end of the item before it (its
//! *gap*) and its own length, never an absolute offset. An edit therefore
//! changes only the items it touches (they go) and the gap of the first
//! item after it; splicing a range and moving through edits cost
//! O(changes · log n), not O(n), which matters for Live Preview's tens of
//! thousands of hidden markers per megabyte.

use std::ops::Range;
use std::sync::Arc;

use gv_text::EditSummary;
use sum_tree::{Cursor, SumTree};

use super::{Replacement, Widget};

#[derive(Clone, Debug)]
struct Item {
    /// Bytes from the end of the item before (or from 0) to its start.
    gap: usize,
    len: usize,
    widget: Option<Arc<Widget>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ItemSummary {
    /// Gaps and lengths: the end of the last item covered.
    span: usize,
    count: usize,
}

impl sum_tree::ContextLessSummary for ItemSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, other: &Self) {
        self.span += other.span;
        self.count += other.count;
    }
}

impl sum_tree::Item for Item {
    type Summary = ItemSummary;

    fn summary(&self, _cx: ()) -> ItemSummary {
        ItemSummary {
            span: self.gap + self.len,
            count: 1,
        }
    }
}

/// The end offset of the items before the cursor, as a dimension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct End(usize);

impl<'a> sum_tree::Dimension<'a, ItemSummary> for End {
    fn zero(_cx: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a ItemSummary, _cx: ()) {
        self.0 += summary.span;
    }
}

type ItemCursor<'a> = Cursor<'a, 'static, Item, End>;

/// The item at the cursor, with its absolute range.
fn current<'a>(cursor: &ItemCursor<'a>) -> Option<(Range<usize>, &'a Item)> {
    let item = cursor.item()?;
    let start = cursor.start().0 + item.gap;
    Some((start..start + item.len, item))
}

fn replacement(range: Range<usize>, item: &Item) -> Replacement {
    Replacement {
        range,
        widget: item.widget.clone(),
    }
}

fn shift(offset: usize, delta: isize) -> usize {
    (offset as isize + delta) as usize
}

/// Builds a tree from left to right.
struct Builder {
    tree: SumTree<Item>,
    /// The end of the last item pushed.
    end: usize,
}

impl Builder {
    fn new() -> Self {
        Self {
            tree: SumTree::default(),
            end: 0,
        }
    }

    fn push(&mut self, replacement: Replacement) {
        debug_assert!(
            replacement.range.start >= self.end,
            "{:?} before {}",
            replacement.range,
            self.end
        );
        let start = replacement.range.start.max(self.end);
        self.tree.push(
            Item {
                gap: start - self.end,
                len: replacement.range.len(),
                widget: replacement.widget,
            },
            (),
        );
        self.end = start + replacement.range.len();
    }

    /// Moves the items that end before `target` from `cursor`, shifted by
    /// `delta` (`None`: all the rest).
    fn take(&mut self, cursor: &mut ItemCursor<'_>, target: Option<usize>, delta: isize) {
        // The first item's gap is relative to an item that may be gone or
        // moved: push it again. The rest keep their gaps.
        let Some((range, item)) = current(cursor) else {
            return;
        };
        if target.is_some_and(|target| range.end >= target) {
            return;
        }
        self.push(Replacement {
            range: shift(range.start, delta)..shift(range.end, delta),
            widget: item.widget.clone(),
        });
        cursor.next();
        let rest = match target {
            Some(target) => cursor.slice(&End(target), sum_tree::Bias::Left),
            None => cursor.suffix(),
        };
        if !rest.is_empty() {
            self.end = shift(cursor.start().0, delta);
            self.tree.append(rest, ());
        }
    }
}

/// The replacements other than folds, sorted by start (an empty one before
/// a non-empty one at the same offset) and not overlapping. Cloning is
/// O(1).
#[derive(Clone, Default)]
pub(super) struct ReplacementTree {
    tree: SumTree<Item>,
}

impl std::fmt::Debug for ReplacementTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl ReplacementTree {
    pub fn len(&self) -> usize {
        self.tree.summary().count
    }

    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    fn cursor(&self) -> ItemCursor<'_> {
        let mut cursor = self.tree.cursor::<End>(());
        cursor.seek(&End(0), sum_tree::Bias::Left);
        cursor
    }

    /// Every replacement, in order.
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            cursor: self.cursor(),
        }
    }

    /// The replacements that start at `offset` or after it, in order.
    pub fn iter_from(&self, offset: usize) -> Iter<'_> {
        let mut cursor = self.tree.cursor::<End>(());
        // The first that ends at `offset` or after; one that starts before
        // it ends there or has it inside, and the next starts after it.
        cursor.seek(&End(offset), sum_tree::Bias::Left);
        if current(&cursor).is_some_and(|(range, _)| range.start < offset) {
            cursor.next();
        }
        Iter { cursor }
    }

    /// The replacements that share bytes with `range` or touch it (an
    /// empty one inside or at its ends counts), in order.
    pub fn touching(&self, range: Range<usize>) -> impl Iterator<Item = Replacement> + '_ {
        let mut cursor = self.tree.cursor::<End>(());
        cursor.seek(&End(range.start), sum_tree::Bias::Left);
        Iter { cursor }.take_while(move |replacement| replacement.range.start <= range.end)
    }

    /// The replacement that has `offset` strictly inside, if any.
    pub fn across(&self, offset: usize) -> Option<Replacement> {
        let mut cursor = self.tree.cursor::<End>(());
        cursor.seek(&End(offset), sum_tree::Bias::Right);
        let (range, item) = current(&cursor)?;
        (range.start < offset).then(|| replacement(range, item))
    }

    /// The run of replacements that follow each other without a gap (one's
    /// end is the next one's start) and hold `offset`, ends included: its
    /// buffer range, or `None` if no replacement touches `offset`.
    pub fn run_around(&self, offset: usize) -> Option<Range<usize>> {
        let mut cursor = self.tree.cursor::<End>(());
        cursor.seek(&End(offset), sum_tree::Bias::Left);
        let (first, _) = current(&cursor)?;
        if first.start > offset {
            return None;
        }
        let mut run = first;
        let mut back = cursor.clone();
        loop {
            back.prev();
            match current(&back) {
                Some((range, _)) if range.end == run.start => run.start = range.start,
                _ => break,
            }
        }
        loop {
            cursor.next();
            match current(&cursor) {
                Some((range, _)) if range.start == run.end => run.end = range.end,
                _ => break,
            }
        }
        Some(run)
    }

    /// The tree after a buffer edit `summary`: replacements an edit
    /// touches (their ends included) are removed, the others move with the
    /// text.
    pub fn moved(&self, summary: &EditSummary) -> Self {
        let edits = summary.edits();
        if edits.is_empty() || self.is_empty() {
            return self.clone();
        }
        let mut builder = Builder::new();
        let mut cursor = self.cursor();
        let mut delta = 0isize;
        for edit in edits {
            builder.take(&mut cursor, Some(edit.old.start), delta);
            while let Some((range, _)) = current(&cursor) {
                if range.start > edit.old.end {
                    break;
                }
                cursor.next();
            }
            delta = edit.new.end as isize - edit.old.end as isize;
        }
        builder.take(&mut cursor, None, delta);
        Self { tree: builder.tree }
    }

    /// Replaces the replacements of each range in `splices` (sorted by
    /// start, not overlapping): those that overlap the range or lie in it
    /// (ends included; a non-empty one that only touches it stays) go, the
    /// range's new ones (sorted, not overlapping, inside the range) come.
    /// Returns the new tree and the buffer ranges of the replacements that
    /// differ, removed or added.
    pub fn spliced(
        &self,
        splices: impl IntoIterator<Item = (Range<usize>, Vec<Replacement>)>,
    ) -> (Self, Vec<Range<usize>>) {
        let mut builder = Builder::new();
        let mut cursor = self.cursor();
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        for (range, new) in splices {
            builder.take(&mut cursor, Some(range.start), 0);
            // A non-empty one that ends where the range starts stays.
            if let Some((r, item)) = current(&cursor)
                && r.end == range.start
                && r.start < r.end
            {
                builder.push(replacement(r, item));
                cursor.next();
            }
            removed.clear();
            while let Some((r, item)) = current(&cursor) {
                let gone = r.start < range.end || (r.start == range.end && r.is_empty());
                if !gone {
                    break;
                }
                removed.push(replacement(r, item));
                cursor.next();
            }
            diff(&removed, &new, &mut changed);
            for replacement in new {
                if replacement.range.start >= builder.end {
                    builder.push(replacement);
                }
            }
        }
        builder.take(&mut cursor, None, 0);
        (Self { tree: builder.tree }, changed)
    }
}

/// Pushes the ranges of the replacements in `old` and `new` (both sorted)
/// that are not in the other.
fn diff(old: &[Replacement], new: &[Replacement], changed: &mut Vec<Range<usize>>) {
    let (mut i, mut j) = (0, 0);
    let key = |r: &Replacement| (r.range.start, r.range.end);
    loop {
        match (old.get(i), new.get(j)) {
            (None, None) => break,
            (Some(a), Some(b)) if key(a) == key(b) => {
                i += 1;
                j += 1;
                if a != b {
                    changed.push(a.range.clone());
                }
            }
            (Some(a), Some(b)) if key(a) < key(b) => {
                i += 1;
                changed.push(a.range.clone());
            }
            (Some(a), None) => {
                i += 1;
                changed.push(a.range.clone());
            }
            (_, Some(b)) => {
                j += 1;
                changed.push(b.range.clone());
            }
        }
    }
}

/// Replacements in order, from a cursor.
pub(super) struct Iter<'a> {
    cursor: ItemCursor<'a>,
}

impl Iterator for Iter<'_> {
    type Item = Replacement;

    fn next(&mut self) -> Option<Replacement> {
        let (range, item) = current(&self.cursor)?;
        let replacement = replacement(range, item);
        self.cursor.next();
        Some(replacement)
    }
}

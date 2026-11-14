// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors
//
// The fold transforms are ported from Zed (https://github.com/zed-industries/zed),
// crates/editor/src/display_map/fold_map.rs at commit
// d2a4fb6ded7c470be25d1541281da3882495b65b.
// Copyright (C) Zed Industries, Inc. and the Zed contributors.
// Licensed under the GNU General Public License, version 3 or later.
//
// Modified by the Glyphvane contributors (2026-11-14): the layer sits
// directly on gv-text (no inlay map) and turns every line terminator into
// one `\n`; the transform summaries are buffer byte lengths and lines
// instead of rope text summaries; folds are byte ranges moved through edit
// summaries instead of multi-buffer anchors, and an edit inside a fold
// removes it; placeholders are a fold id instead of a render closure; the
// public API takes points instead of fold offsets; highlighting, row infos,
// fold widths and tracing are removed, and the tests run without GPUI. The
// full list is in third_party/zed/PROVENANCE.md.

//! The replace layer: the first layer above the buffer.
//!
//! # Text
//!
//! The layer's text is the buffer text with
//!
//! - **every line terminator** (LF, CRLF or a lone CR) turned into one
//!   `\n`, so a trailing `\r` belongs to the line ending and never takes up
//!   a column;
//! - **every fold** replaced by one placeholder character,
//!   [`PLACEHOLDER`] (`⋯`). Folds that overlap share one placeholder;
//!   folds that only touch keep one each, so the position between them
//!   stays visible.
//!
//! A [`ReplacePoint`] is a row and a byte column in that text. There is no
//! offset coordinate: offsets in this text differ from buffer offsets
//! wherever CRLF occurs or a fold is.
//!
//! Later (Live Preview) the same layer will hide Markdown markers and put
//! inline widgets in place of ranges; they are replacements like folds.
//!
//! # Folds
//!
//! A [`Fold`] is a non-empty byte range of the buffer, never inside a CRLF,
//! with a stable [`FoldId`]. Its start sticks to the text after it and its
//! end to the text before it, so text typed at either edge stays outside
//! the fold. An edit that changes text **inside** a fold removes the fold
//! ([`ReplaceMap::sync_with_unfolded`] reports which); edits next to it
//! keep it.
//!
//! The layer is a sum tree of transforms, as in Zed's fold map: isomorphic
//! runs of buffer text and fold placeholders, each summarized by its
//! buffer length and lines on the input side and its lines on the output
//! side, so conversions are O(log n). Positions inside a placeholder do not
//! exist: they clip to its start ([`Bias::Left`]) or end
//! ([`Bias::Right`]), and buffer positions inside a fold map to the
//! placeholder's start or end the same way.

use std::cmp::{self, Ordering};
use std::collections::HashSet;
use std::iter::Peekable;
use std::ops::Range;
use std::sync::Arc;

use gv_text::{Bias, EditSummary, TextSnapshot};
use sum_tree::{Cursor, Dimensions, SumTree};

use super::BufferPoint;
use super::primitives::{Chunk, Edit, MAX_CHUNK_LEN, Point, TextSummary};

/// The text a fold shows in place of its range: one `⋯` (U+22EF, three
/// bytes), as in Zed.
pub const PLACEHOLDER: &str = "\u{22ef}";

/// [`PLACEHOLDER`]'s length in bytes.
const PLACEHOLDER_LEN: u32 = PLACEHOLDER.len() as u32;

/// A position in the replace layer's text: a row and a byte column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplacePoint(pub Point);

impl ReplacePoint {
    /// Creates a point.
    pub const fn new(row: u32, column: u32) -> Self {
        Self(Point::new(row, column))
    }

    /// The start of the text.
    pub const fn zero() -> Self {
        Self::new(0, 0)
    }

    /// The row.
    pub const fn row(self) -> u32 {
        self.0.row
    }

    /// The byte column.
    pub const fn column(self) -> u32 {
        self.0.column
    }
}

impl<'a> sum_tree::Dimension<'a, TransformSummary> for ReplacePoint {
    fn zero(_cx: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a TransformSummary, _cx: ()) {
        self.0 += summary.output;
    }
}

/// An edit of the replace layer's text.
pub type ReplaceEdit = Edit<ReplacePoint>;

/// Identifies a fold for as long as it exists. Ids are never reused by one
/// [`ReplaceMap`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FoldId(pub u64);

/// A folded range of the buffer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Fold {
    /// Its identity.
    pub id: FoldId,
    /// The folded bytes, in the snapshot's buffer: non-empty, on character
    /// boundaries, and neither end inside a CRLF.
    pub range: Range<usize>,
}

/// Where a placeholder is: one per run of overlapping folds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Placeholder {
    /// The first fold of the run (the UI's handle for it).
    pub fold: FoldId,
    /// The placeholder's text in the layer: [`PLACEHOLDER`], on one row.
    pub range: Range<ReplacePoint>,
    /// The buffer bytes it hides (the union of the run).
    pub buffer_range: Range<usize>,
}

// ----- transforms -----------------------------------------------------------

/// Buffer bytes and lines of the text a transform covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InputSummary {
    /// Buffer bytes, line terminators included.
    len: usize,
    /// Lines as the layer counts them (a CRLF is one line break).
    lines: Point,
}

impl InputSummary {
    fn add(&mut self, other: &Self) {
        self.len += other.len;
        self.lines += other.lines;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TransformSummary {
    input: InputSummary,
    output: Point,
}

impl TransformSummary {
    fn add(&mut self, other: &Self) {
        self.input.add(&other.input);
        self.output += other.output;
    }
}

impl sum_tree::ContextLessSummary for TransformSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, other: &Self) {
        self.add(other);
    }
}

/// An isomorphic run of buffer text, or a placeholder for folded text.
#[derive(Clone, Debug, Default)]
struct Transform {
    summary: TransformSummary,
    /// For a placeholder: the first fold of its run.
    placeholder: Option<FoldId>,
}

impl Transform {
    fn isomorphic(input: InputSummary) -> Self {
        Self {
            summary: TransformSummary {
                input,
                output: input.lines,
            },
            placeholder: None,
        }
    }

    fn fold(id: FoldId, input: InputSummary) -> Self {
        Self {
            summary: TransformSummary {
                input,
                output: Point::new(0, PLACEHOLDER_LEN),
            },
            placeholder: Some(id),
        }
    }

    fn is_fold(&self) -> bool {
        self.placeholder.is_some()
    }
}

impl sum_tree::Item for Transform {
    type Summary = TransformSummary;

    fn summary(&self, _cx: ()) -> TransformSummary {
        self.summary
    }
}

/// A buffer byte offset, as a transform-tree dimension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct InputOffset(usize);

impl<'a> sum_tree::Dimension<'a, TransformSummary> for InputOffset {
    fn zero(_cx: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a TransformSummary, _cx: ()) {
        self.0 += summary.input.len;
    }
}

/// A buffer position as the layer counts lines, as a dimension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct InputPoint(Point);

impl<'a> sum_tree::Dimension<'a, TransformSummary> for InputPoint {
    fn zero(_cx: ()) -> Self {
        Self::default()
    }

    fn add_summary(&mut self, summary: &'a TransformSummary, _cx: ()) {
        self.0 += summary.input.lines;
    }
}

/// `base` moved by the distance `by` (see [`Point`]'s `Add`), saturating.
fn advance(base: Point, by: Point) -> Point {
    if by.row == 0 {
        Point::new(base.row, base.column.saturating_add(by.column))
    } else {
        Point::new(base.row.saturating_add(by.row), by.column)
    }
}

/// Saturates: the display map handles texts up to 4 GiB.
fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The layer point of a buffer offset that is not inside a CRLF.
fn point_of(buffer: &TextSnapshot, offset: usize) -> Point {
    debug_assert!(
        !buffer.is_inside_crlf(offset),
        "offset {offset} splits a CRLF"
    );
    let point = buffer
        .offset_to_point(offset)
        .unwrap_or_else(|_| buffer.max_point());
    Point::new(to_u32(point.line), to_u32(point.column))
}

fn input_summary(buffer: &TextSnapshot, range: Range<usize>) -> InputSummary {
    InputSummary {
        len: range.len(),
        lines: point_of(buffer, range.end) - point_of(buffer, range.start),
    }
}

/// Appends an isomorphic run, joining it to an isomorphic last transform.
fn push_isomorphic(transforms: &mut SumTree<Transform>, input: InputSummary) {
    let mut did_merge = false;
    transforms.update_last(
        |last| {
            if !last.is_fold() {
                last.summary.input.add(&input);
                last.summary.output += input.lines;
                did_merge = true;
            }
        },
        (),
    );
    if !did_merge {
        transforms.push(Transform::isomorphic(input), ());
    }
}

/// If the cursor is at an isomorphic transform and `transforms` ends with
/// one, joins the first to the second and moves the cursor past it.
fn join_isomorphic(
    transforms: &mut SumTree<Transform>,
    cursor: &mut Cursor<'_, 'static, Transform, InputOffset>,
) {
    let Some(item) = cursor.item().filter(|item| !item.is_fold()) else {
        return;
    };
    let summary = item.summary;
    let mut did_merge = false;
    transforms.update_last(
        |last| {
            if !last.is_fold() {
                last.summary.add(&summary);
                did_merge = true;
            }
        },
        (),
    );
    if did_merge {
        cursor.next();
    }
}

/// Moves `offset` out of a CRLF: before the CR ([`Bias::Left`]) or after
/// the LF ([`Bias::Right`]).
fn clip_out_of_crlf(buffer: &TextSnapshot, offset: usize, bias: Bias) -> usize {
    if buffer.is_inside_crlf(offset) {
        match bias {
            Bias::Left => offset - 1,
            Bias::Right => offset + 1,
        }
    } else {
        offset
    }
}

/// Folds sorted by start, the longer of two with the same start first.
fn fold_order(a: &Fold, b: &Fold) -> Ordering {
    a.range
        .start
        .cmp(&b.range.start)
        .then_with(|| b.range.end.cmp(&a.range.end))
}

/// The runs of overlapping folds: their unions, sorted, each with the id
/// of its first fold.
#[cfg(test)]
fn merged_folds(folds: &[Fold]) -> Vec<(FoldId, Range<usize>)> {
    let mut merged: Vec<(FoldId, Range<usize>)> = Vec::new();
    for fold in folds {
        match merged.last_mut() {
            Some((_, last)) if fold.range.start < last.end => {
                last.end = last.end.max(fold.range.end);
            }
            _ => merged.push((fold.id, fold.range.clone())),
        }
    }
    merged
}

// ----- the map ----------------------------------------------------------------

/// A buffer edit in byte offsets: old range in the old buffer, new range
/// in the new one.
type InputEdit = Edit<usize>;

/// Keeps the replace layer in step with the buffer and holds the folds.
#[derive(Debug)]
pub struct ReplaceMap {
    snapshot: ReplaceSnapshot,
    next_fold_id: u64,
}

impl ReplaceMap {
    /// A replace layer over `buffer` without folds, and its first snapshot.
    pub fn new(buffer: TextSnapshot) -> (Self, ReplaceSnapshot) {
        let input = input_summary(&buffer, 0..buffer.len());
        let snapshot = ReplaceSnapshot {
            buffer,
            transforms: SumTree::from_item(Transform::isomorphic(input), ()),
            folds: Arc::new(Vec::new()),
            version: 0,
        };
        (
            Self {
                snapshot: snapshot.clone(),
                next_fold_id: 0,
            },
            snapshot,
        )
    }

    /// The current snapshot.
    pub fn snapshot(&self) -> ReplaceSnapshot {
        self.snapshot.clone()
    }

    /// Moves the layer to `buffer`, which `edits` produced from the buffer
    /// of the current snapshot, and returns the new snapshot with the edits
    /// in replace coordinates (sorted, non-overlapping, old ranges in the
    /// previous snapshot and new ranges in the returned one).
    ///
    /// Folds move with the text; a fold whose inside is edited is removed
    /// (see [`sync_with_unfolded`](Self::sync_with_unfolded)). Edits are
    /// widened by a byte where a CR before them or an LF after them may
    /// join or split a CRLF, and to whole placeholders. If the revisions of
    /// `edits` do not match the two buffers, every fold is removed and the
    /// whole text is reported as changed.
    pub fn sync(
        &mut self,
        buffer: TextSnapshot,
        edits: &EditSummary,
    ) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
        let (snapshot, edits, _) = self.sync_with_unfolded(buffer, edits);
        (snapshot, edits)
    }

    /// Like [`sync`](Self::sync), and also returns the folds the edits
    /// removed, with their ranges in the **old** buffer.
    ///
    /// A fold is removed when an edit replaces or inserts text strictly
    /// inside it or replaces text that overlaps it (including all of it);
    /// a replacement by the same text does not count. Insertions at its
    /// start or end and edits that only touch it keep it.
    pub fn sync_with_unfolded(
        &mut self,
        buffer: TextSnapshot,
        edits: &EditSummary,
    ) -> (ReplaceSnapshot, Vec<ReplaceEdit>, Vec<Fold>) {
        let old_buffer = self.snapshot.buffer.clone();
        let lined_up = edits.old_revision() == old_buffer.revision()
            && edits.new_revision() == buffer.revision();
        if lined_up && edits.is_empty() {
            self.snapshot.buffer = buffer;
            return (self.snapshot.clone(), Vec::new(), Vec::new());
        }

        let input_edits = lined_up
            .then(|| input_edits(&old_buffer, &buffer, edits.edits()))
            .flatten();
        let Some(input_edits) = input_edits else {
            let old_max = self.snapshot.max_point();
            let unfolded = std::mem::take(Arc::make_mut(&mut self.snapshot.folds));
            let input = input_summary(&buffer, 0..buffer.len());
            self.snapshot.transforms = SumTree::from_item(Transform::isomorphic(input), ());
            self.snapshot.buffer = buffer;
            self.snapshot.version += 1;
            let edit = Edit {
                old: ReplacePoint::zero()..old_max,
                new: ReplacePoint::zero()..self.snapshot.max_point(),
            };
            return (self.snapshot.clone(), vec![edit], unfolded);
        };

        let unfolded = self.move_folds(&old_buffer, &buffer, edits);
        let replace_edits = self.sync_transforms(buffer, input_edits);
        self.check_invariants();
        (self.snapshot.clone(), replace_edits, unfolded)
    }

    /// Moves the folds through `edits` into `buffer`, removing those edited
    /// inside. Returns the removed ones, as they were.
    fn move_folds(
        &mut self,
        old_buffer: &TextSnapshot,
        buffer: &TextSnapshot,
        summary: &EditSummary,
    ) -> Vec<Fold> {
        if self.snapshot.folds.is_empty() {
            return Vec::new();
        }
        let edits = summary.edits();
        // Whether edit `index` puts back the same text (a no-op for folds).
        let same_text = |index: usize| {
            edits.get(index).is_some_and(|edit| {
                edit.old.len() == edit.new.len()
                    && old_buffer.text_for_range(edit.old.clone()).ok()
                        == buffer.text_for_range(edit.new.clone()).ok()
            })
        };
        let map = |offset: usize, bias: Bias| {
            // Inside a replacement by the same text, offsets keep their
            // place; elsewhere they move as anchors do.
            let index = edits.partition_point(|edit| edit.old.end <= offset);
            match edits.get(index) {
                Some(edit) if edit.old.start < offset && same_text(index) => {
                    edit.new.start + (offset - edit.old.start)
                }
                _ => summary.transform_offset(offset, bias),
            }
        };

        // Only an endpoint an edit starts or ends at can come to lie inside
        // a CRLF (elsewhere the characters around it stay the same).
        let touched = |offset: usize| {
            let index = edits.partition_point(|edit| edit.old.end < offset);
            edits
                .get(index)
                .is_some_and(|edit| edit.old.start == offset || edit.old.end == offset)
        };
        let moved = |offset: usize, bias: Bias| {
            let new = map(offset, bias);
            if touched(offset) {
                clip_out_of_crlf(buffer, new, bias)
            } else {
                new
            }
        };

        // Folds wholly before the first edit or after the last one only
        // stay or shift.
        let (Some(first_edit), Some(last_edit)) = (edits.first(), edits.last()) else {
            return Vec::new();
        };
        let (before, after) = (first_edit.old.start, last_edit.old.end);
        let shift = |offset: usize| (offset + last_edit.new.end) - last_edit.old.end;

        let mut folds: Vec<Fold> = Vec::with_capacity(self.snapshot.folds.len());
        let mut removed = Vec::new();
        for fold in self.snapshot.folds.iter() {
            let range = &fold.range;
            if range.end < before {
                folds.push(fold.clone());
                continue;
            }
            if range.start > after {
                folds.push(Fold {
                    id: fold.id,
                    range: shift(range.start)..shift(range.end),
                });
                continue;
            }
            let first = edits.partition_point(|edit| edit.old.end <= range.start);
            let edited_inside = edits[first..]
                .iter()
                .enumerate()
                .take_while(|(_, edit)| edit.old.start < range.end)
                .any(|(index, _)| !same_text(first + index));
            if edited_inside {
                removed.push(fold.clone());
                continue;
            }
            let start = moved(range.start, Bias::Right);
            let end = moved(range.end, Bias::Left);
            if start < end {
                folds.push(Fold {
                    id: fold.id,
                    range: start..end,
                });
            } else {
                removed.push(fold.clone());
            }
        }
        if !folds.is_sorted_by(|a, b| fold_order(a, b).is_le()) {
            folds.sort_by(fold_order);
        }
        // Folds that the edits brought to the same range become one; the
        // others count as removed.
        if folds.windows(2).any(|pair| pair[0].range == pair[1].range) {
            let mut unique: Vec<Fold> = Vec::with_capacity(folds.len());
            for fold in folds {
                match unique.last() {
                    Some(last) if last.range == fold.range => {
                        if let Some(old) = self.snapshot.folds.iter().find(|old| old.id == fold.id)
                        {
                            removed.push(old.clone());
                        }
                    }
                    _ => unique.push(fold),
                }
            }
            folds = unique;
        }
        self.snapshot.folds = Arc::new(folds);
        removed
    }

    /// Folds `ranges` (byte ranges of the current buffer, clipped to
    /// character boundaries and moved out of CRLFs; empty ones and ranges
    /// already folded exactly are skipped). Returns the new snapshot and
    /// the edits.
    pub fn fold(
        &mut self,
        ranges: impl IntoIterator<Item = Range<usize>>,
    ) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
        let buffer = self.snapshot.buffer.clone();
        let mut edits = Vec::new();
        let mut folds = (*self.snapshot.folds).clone();
        let mut folded: HashSet<Range<usize>> =
            folds.iter().map(|fold| fold.range.clone()).collect();
        for range in ranges {
            let start = buffer.clip_offset(range.start, Bias::Right);
            let start = clip_out_of_crlf(&buffer, start, Bias::Right);
            let end = buffer.clip_offset(range.end, Bias::Left);
            let end = clip_out_of_crlf(&buffer, end, Bias::Left);
            if start >= end || !folded.insert(start..end) {
                continue;
            }
            let id = FoldId(self.next_fold_id);
            self.next_fold_id += 1;
            folds.push(Fold {
                id,
                range: start..end,
            });
            edits.push(Edit {
                old: start..end,
                new: start..end,
            });
        }
        if edits.is_empty() {
            return (self.snapshot.clone(), Vec::new());
        }
        folds.sort_by(fold_order);
        self.snapshot.folds = Arc::new(folds);
        let edits = consolidate_edits(edits);
        let replace_edits = self.sync_transforms(buffer, edits);
        self.check_invariants();
        (self.snapshot.clone(), replace_edits)
    }

    /// Removes the folds that intersect any of `ranges` (byte ranges of the
    /// current buffer). With `inclusive`, folds that only touch a range
    /// count too; otherwise they must overlap it (for an empty range: have
    /// it strictly inside).
    pub fn unfold_intersecting(
        &mut self,
        ranges: impl IntoIterator<Item = Range<usize>>,
        inclusive: bool,
    ) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
        let ranges: Vec<Range<usize>> = ranges.into_iter().collect();
        self.remove_folds_with(|fold| {
            ranges
                .iter()
                .any(|range| intersects(&fold.range, range, inclusive))
        })
    }

    /// Removes the folds with the given ids.
    pub fn remove_folds(
        &mut self,
        ids: impl IntoIterator<Item = FoldId>,
    ) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
        let ids: HashSet<FoldId> = ids.into_iter().collect();
        self.remove_folds_with(|fold| ids.contains(&fold.id))
    }

    /// Removes every fold.
    pub fn unfold_all(&mut self) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
        self.remove_folds_with(|_| true)
    }

    fn remove_folds_with(
        &mut self,
        mut should_unfold: impl FnMut(&Fold) -> bool,
    ) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
        let mut edits = Vec::new();
        let mut kept = Vec::with_capacity(self.snapshot.folds.len());
        for fold in self.snapshot.folds.iter() {
            if should_unfold(fold) {
                edits.push(Edit {
                    old: fold.range.clone(),
                    new: fold.range.clone(),
                });
            } else {
                kept.push(fold.clone());
            }
        }
        if edits.is_empty() {
            return (self.snapshot.clone(), Vec::new());
        }
        self.snapshot.folds = Arc::new(kept);
        let buffer = self.snapshot.buffer.clone();
        let edits = consolidate_edits(edits);
        let replace_edits = self.sync_transforms(buffer, edits);
        self.check_invariants();
        (self.snapshot.clone(), replace_edits)
    }

    /// Rebuilds the transforms where `input_edits` changed the text or the
    /// folds (the folds are already in `buffer`'s coordinates) and returns
    /// the edits of the layer's text. Ported from Zed's `FoldMap::sync`.
    fn sync_transforms(
        &mut self,
        buffer: TextSnapshot,
        input_edits: Vec<InputEdit>,
    ) -> Vec<ReplaceEdit> {
        if input_edits.is_empty() {
            if self.snapshot.buffer.revision() != buffer.revision() {
                self.snapshot.version += 1;
            }
            self.snapshot.buffer = buffer;
            return Vec::new();
        }

        let old = &self.snapshot;
        let folds = &old.folds;
        let mut new_transforms = SumTree::<Transform>::default();
        let mut cursor = old.transforms.cursor::<InputOffset>(());
        cursor.seek(&InputOffset(0), sum_tree::Bias::Right);

        let mut edits_iter = input_edits.iter().cloned().peekable();
        while let Some(mut edit) = edits_iter.next() {
            // An isomorphic transform wholly before the edit joins an
            // isomorphic last transform, so isomorphic transforms never
            // follow each other. (Upstream joins it whenever it is
            // isomorphic: there no edit starts inside it, but here the
            // previous region can end with the text of a fold an edit
            // removed, and the next edit can start in the text after it.)
            if cursor.end().0 <= edit.old.start {
                join_isomorphic(&mut new_transforms, &mut cursor);
            }
            new_transforms.append(
                cursor.slice(&InputOffset(edit.old.start), sum_tree::Bias::Left),
                (),
            );
            let pulled = edit.old.start - cursor.start().0;
            edit.old.start -= pulled;
            edit.new.start -= pulled;

            cursor.seek(&InputOffset(edit.old.end), sum_tree::Bias::Right);
            cursor.next();

            let mut delta = edit.new.len() as isize - edit.old.len() as isize;
            loop {
                edit.old.end = cursor.start().0;
                match edits_iter.peek() {
                    Some(next_edit) if next_edit.old.start <= edit.old.end => {
                        delta += next_edit.new.len() as isize - next_edit.old.len() as isize;
                        if next_edit.old.end >= edit.old.end {
                            edit.old.end = next_edit.old.end;
                            cursor.seek(&InputOffset(edit.old.end), sum_tree::Bias::Right);
                            cursor.next();
                        }
                        edits_iter.next();
                    }
                    _ => break,
                }
            }
            edit.new.end = ((edit.new.start + edit.old.len()) as isize + delta) as usize;

            let mut fold_index = folds.partition_point(|fold| fold.range.start < edit.new.start);
            while let Some(fold) = folds.get(fold_index)
                && fold.range.start < edit.new.end
            {
                let id = fold.id;
                let mut range = fold.range.clone();
                fold_index += 1;
                // Upstream also joins folds that touch (`merge_adjacent`).
                while let Some(next) = folds.get(fold_index)
                    && next.range.start < range.end
                {
                    range.end = range.end.max(next.range.end);
                    fold_index += 1;
                }

                let sum = new_transforms.summary().input.len;
                debug_assert!(range.start >= sum, "fold {range:?} starts before {sum}");
                if range.start > sum {
                    push_isomorphic(
                        &mut new_transforms,
                        input_summary(&buffer, sum..range.start),
                    );
                }
                if range.end > range.start {
                    let start = range.start.max(sum);
                    new_transforms.push(
                        Transform::fold(id, input_summary(&buffer, start..range.end)),
                        (),
                    );
                }
            }

            let sum = new_transforms.summary().input.len;
            debug_assert!(sum <= edit.new.end, "transforms past the edit");
            if sum < edit.new.end {
                push_isomorphic(
                    &mut new_transforms,
                    input_summary(&buffer, sum..edit.new.end),
                );
            }
        }

        // The rest, joined like the transforms before an edit (the last
        // region can end with the text of a fold an edit removed).
        join_isomorphic(&mut new_transforms, &mut cursor);
        new_transforms.append(cursor.suffix(), ());
        if new_transforms.is_empty() {
            push_isomorphic(&mut new_transforms, input_summary(&buffer, 0..buffer.len()));
        }
        drop(cursor);

        let replace_edits = layer_edits(
            &old.transforms,
            &old.buffer,
            &new_transforms,
            &buffer,
            input_edits,
        );

        self.snapshot.transforms = new_transforms;
        self.snapshot.buffer = buffer;
        self.snapshot.version += 1;
        replace_edits
    }

    #[cfg(not(test))]
    fn check_invariants(&self) {}

    #[cfg(test)]
    fn check_invariants(&self) {
        let snapshot = &self.snapshot;
        let summary = snapshot.transforms.summary();
        assert_eq!(
            summary.input.len,
            snapshot.buffer.len(),
            "transforms do not cover the buffer"
        );
        assert_eq!(
            summary.input.lines,
            point_of(&snapshot.buffer, snapshot.buffer.len()),
            "transform lines do not match the buffer"
        );
        let mut previous_isomorphic = false;
        let mut runs = Vec::new();
        let mut offset = 0;
        for transform in snapshot.transforms.iter() {
            assert!(
                transform.is_fold() || !previous_isomorphic,
                "adjacent isomorphic transforms"
            );
            previous_isomorphic = !transform.is_fold();
            let end = offset + transform.summary.input.len;
            if let Some(id) = transform.placeholder {
                runs.push((id, offset..end));
            }
            offset = end;
        }
        assert!(
            snapshot.folds.is_sorted_by(|a, b| fold_order(a, b).is_le()),
            "folds out of order: {:?}",
            snapshot.folds
        );
        for fold in snapshot.folds.iter() {
            assert!(fold.range.start < fold.range.end, "empty fold {fold:?}");
            assert!(!snapshot.buffer.is_inside_crlf(fold.range.start));
            assert!(!snapshot.buffer.is_inside_crlf(fold.range.end));
        }
        assert_eq!(runs, merged_folds(&snapshot.folds), "placeholders");
    }
}

/// Whether `fold` intersects `range`: shares bytes with it or, for an
/// empty range, has it strictly inside; with `inclusive`, touching counts.
fn intersects(fold: &Range<usize>, range: &Range<usize>, inclusive: bool) -> bool {
    if inclusive {
        range.start <= fold.end && range.end >= fold.start
    } else {
        range.start < fold.end && range.end > fold.start
    }
}

/// Sorts edits and joins the ones that overlap (Zed's
/// `consolidate_inlay_edits`).
fn consolidate_edits(mut edits: Vec<InputEdit>) -> Vec<InputEdit> {
    edits.sort_unstable_by(|a, b| {
        a.old
            .start
            .cmp(&b.old.start)
            .then_with(|| b.old.end.cmp(&a.old.end))
    });
    let mut consolidated: Vec<InputEdit> = Vec::with_capacity(edits.len());
    for edit in edits {
        match consolidated.last_mut() {
            Some(last) if last.old.end >= edit.old.start => {
                last.old.end = last.old.end.max(edit.old.end);
                last.new.start = last.new.start.min(edit.new.start);
                last.new.end = last.new.end.max(edit.new.end);
            }
            _ => consolidated.push(edit),
        }
    }
    consolidated
}

/// The layer edits for `input_edits`: each widened to whole placeholders
/// on both sides by the same amount of unchanged text (so text between
/// edits is the same in both snapshots), then converted to points and
/// joined where they overlap. Ported from the second half of Zed's
/// `FoldMap::sync`.
fn layer_edits(
    old_transforms: &SumTree<Transform>,
    old_buffer: &TextSnapshot,
    new_transforms: &SumTree<Transform>,
    new_buffer: &TextSnapshot,
    input_edits: Vec<InputEdit>,
) -> Vec<ReplaceEdit> {
    type Dims = Dimensions<InputOffset, InputPoint, ReplacePoint>;
    let old_len = old_transforms.summary().input.len;
    let new_len = new_transforms.summary().input.len;
    let mut old_cursor = old_transforms.cursor::<Dims>(());
    let mut new_cursor = new_transforms.cursor::<Dims>(());
    let is_fold = |cursor: &Cursor<'_, 'static, Transform, Dims>| {
        cursor.item().is_some_and(Transform::is_fold)
    };

    let mut widened: Vec<InputEdit> = Vec::with_capacity(input_edits.len());
    let mut edits_iter = input_edits.into_iter().peekable();
    while let Some(mut edit) = edits_iter.next() {
        let previous = widened.last();
        let mut merge_with_previous =
            previous.is_some_and(|previous| previous.old.end >= edit.old.start);
        if !merge_with_previous {
            let mut limit = match previous {
                Some(previous) => {
                    (edit.old.start - previous.old.end).min(edit.new.start - previous.new.end)
                }
                None => edit.old.start.min(edit.new.start),
            };
            loop {
                old_cursor.seek(&InputOffset(edit.old.start), sum_tree::Bias::Left);
                new_cursor.seek(&InputOffset(edit.new.start), sum_tree::Bias::Left);
                let old_pull = if is_fold(&old_cursor) {
                    edit.old.start - old_cursor.start().0.0
                } else {
                    0
                };
                let new_pull = if is_fold(&new_cursor) {
                    edit.new.start - new_cursor.start().0.0
                } else {
                    0
                };
                let pull = old_pull.max(new_pull).min(limit);
                if pull == 0 {
                    break;
                }
                edit.old.start -= pull;
                edit.new.start -= pull;
                limit -= pull;
            }
            merge_with_previous = previous.is_some() && limit == 0;
        }

        let mut limit = match edits_iter.peek() {
            Some(next) => (next.old.start - edit.old.end).min(next.new.start - edit.new.end),
            None => (old_len - edit.old.end).min(new_len - edit.new.end),
        };
        loop {
            old_cursor.seek(&InputOffset(edit.old.end), sum_tree::Bias::Right);
            new_cursor.seek(&InputOffset(edit.new.end), sum_tree::Bias::Right);
            let old_extension = if is_fold(&old_cursor) {
                old_cursor.end().0.0 - edit.old.end
            } else {
                0
            };
            let new_extension = if is_fold(&new_cursor) {
                new_cursor.end().0.0 - edit.new.end
            } else {
                0
            };
            let extension = old_extension.max(new_extension).min(limit);
            if extension == 0 {
                break;
            }
            edit.old.end += extension;
            edit.new.end += extension;
            limit -= extension;
        }

        match widened.last_mut() {
            Some(previous) if merge_with_previous => {
                previous.old.end = edit.old.end;
                previous.new.end = edit.new.end;
            }
            _ => widened.push(edit),
        }
    }

    let output_at =
        |cursor: &Cursor<'_, 'static, Transform, Dims>, buffer: &TextSnapshot, offset| {
            let start = cursor.start();
            ReplacePoint(advance(start.2.0, point_of(buffer, offset) - start.1.0))
        };
    let mut replace_edits: Vec<ReplaceEdit> = Vec::with_capacity(widened.len());
    for edit in widened {
        old_cursor.seek(&InputOffset(edit.old.start), sum_tree::Bias::Left);
        let old_start = output_at(&old_cursor, old_buffer, edit.old.start);
        old_cursor.seek(&InputOffset(edit.old.end), sum_tree::Bias::Right);
        let old_end = output_at(&old_cursor, old_buffer, edit.old.end);
        new_cursor.seek(&InputOffset(edit.new.start), sum_tree::Bias::Left);
        let new_start = output_at(&new_cursor, new_buffer, edit.new.start);
        new_cursor.seek(&InputOffset(edit.new.end), sum_tree::Bias::Right);
        let new_end = output_at(&new_cursor, new_buffer, edit.new.end);
        replace_edits.push(Edit {
            old: old_start..old_end,
            new: new_start..new_end,
        });
    }
    consolidate_replace_edits(replace_edits)
}

/// Sorts edits and joins the ones that overlap or touch (Zed's
/// `consolidate_fold_edits`).
fn consolidate_replace_edits(mut edits: Vec<ReplaceEdit>) -> Vec<ReplaceEdit> {
    edits.sort_unstable_by(|a, b| {
        a.old
            .start
            .cmp(&b.old.start)
            .then_with(|| b.old.end.cmp(&a.old.end))
    });
    let mut consolidated: Vec<ReplaceEdit> = Vec::with_capacity(edits.len());
    for edit in edits {
        match consolidated.last_mut() {
            Some(last) if last.old.end >= edit.old.start => {
                last.old.end = last.old.end.max(edit.old.end);
                last.new.start = last.new.start.min(edit.new.start);
                last.new.end = last.new.end.max(edit.new.end);
            }
            _ => consolidated.push(edit),
        }
    }
    consolidated
}

/// Converts buffer edits to input edits (byte offsets) that never end
/// inside a CRLF. `None` if the edits do not fit the snapshots.
fn input_edits(
    old: &TextSnapshot,
    new: &TextSnapshot,
    edits: &[gv_text::Edit],
) -> Option<Vec<InputEdit>> {
    // Join edits that touch, so the byte before and the byte after each one
    // is unchanged text, the same in both snapshots.
    let mut joined = Vec::with_capacity(edits.len());
    for edit in edits {
        push_joined(&mut joined, edit.old.clone(), edit.new.clone());
    }

    // An edit end lies inside a CRLF when a CR just before it and an LF
    // just after it pair up, in the old text or in the new one. Taking that
    // CR or LF (unchanged text) into the edit moves the end outside the
    // terminator. For an empty side, the "first" and "last" characters are
    // the unchanged ones after and before it, which covers an insertion
    // between CR and LF and a deletion that brings them together.
    let first_is = |snapshot: &TextSnapshot, range: &Range<usize>, c: char| {
        Some(snapshot.chars_at(range.start).ok()?.next() == Some(c))
    };
    let last_is = |snapshot: &TextSnapshot, range: &Range<usize>, c: char| {
        Some(snapshot.reversed_chars_at(range.end).ok()?.next() == Some(c))
    };
    let mut widened = Vec::with_capacity(joined.len());
    for (mut old_range, mut new_range) in joined {
        if old_range.end > old.len() || new_range.end > new.len() {
            return None;
        }
        let cr_before = old.reversed_chars_at(old_range.start).ok()?.next() == Some('\r');
        if cr_before && (first_is(old, &old_range, '\n')? || first_is(new, &new_range, '\n')?) {
            old_range.start -= 1;
            new_range.start -= 1;
        }
        let lf_after = old.chars_at(old_range.end).ok()?.next() == Some('\n');
        if lf_after && (last_is(old, &old_range, '\r')? || last_is(new, &new_range, '\r')?) {
            old_range.end += 1;
            new_range.end += 1;
        }
        push_joined(&mut widened, old_range, new_range);
    }
    Some(
        widened
            .into_iter()
            .map(|(old, new)| Edit { old, new })
            .collect(),
    )
}

/// Appends an edit, joining it with the previous one if they touch.
fn push_joined(
    edits: &mut Vec<(Range<usize>, Range<usize>)>,
    old: Range<usize>,
    new: Range<usize>,
) {
    match edits.last_mut() {
        Some((last_old, last_new)) if last_old.end >= old.start => {
            last_old.end = old.end;
            last_new.end = new.end;
        }
        _ => edits.push((old, new)),
    }
}

// ----- the snapshot -------------------------------------------------------------

/// An immutable view of the replace layer. Cloning is O(1).
#[derive(Clone)]
pub struct ReplaceSnapshot {
    buffer: TextSnapshot,
    transforms: SumTree<Transform>,
    /// Sorted by [`fold_order`], in `buffer`'s coordinates.
    folds: Arc<Vec<Fold>>,
    version: usize,
}

impl std::fmt::Debug for ReplaceSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplaceSnapshot")
            .field("buffer", &self.buffer)
            .field("folds", &self.folds)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl ReplaceSnapshot {
    /// The buffer snapshot this layer shows.
    pub fn buffer(&self) -> &TextSnapshot {
        &self.buffer
    }

    /// Increases with every change of the text or the folds.
    pub fn version(&self) -> usize {
        self.version
    }

    /// The end of the text.
    pub fn max_point(&self) -> ReplacePoint {
        ReplacePoint(self.transforms.summary().output)
    }

    /// Length of `row` in bytes, without its terminator; 0 past the last
    /// row.
    pub fn line_len(&self, row: u32) -> u32 {
        let max = self.max_point();
        if row > max.row() {
            return 0;
        }
        if row == max.row() {
            return max.column();
        }
        // The `\n` that ends the row is in an isomorphic transform: the one
        // that contains the next row's start or ends there.
        let next = ReplacePoint::new(row + 1, 0);
        let (start, _, _) = self
            .transforms
            .find::<Dimensions<ReplacePoint, InputPoint>, _>((), &next, sum_tree::Bias::Left);
        let next_input = advance(start.1.0, next.0 - start.0.0);
        let line = next_input.row.saturating_sub(1);
        let line_end = Point::new(line, to_u32(self.content_len(line as usize)));
        advance(start.0.0, line_end - start.1.0).column
    }

    /// Bytes of buffer line `line` without its terminator; 0 past the end.
    fn content_len(&self, line: usize) -> usize {
        self.buffer.line_range(line).map_or(0, |range| range.len())
    }

    /// Clips a buffer position (as a layer point) to a valid one: a line
    /// past the end goes to the end of the text, a column past the end of
    /// its line (or between CR and LF) to the end of the line, and a column
    /// inside a character moves by `bias`.
    fn clip_input(&self, point: Point, bias: Bias) -> Point {
        let clipped = self.buffer.clip_point(
            BufferPoint::new(point.row as usize, point.column as usize),
            bias,
        );
        let column = clipped.column.min(self.content_len(clipped.line));
        Point::new(to_u32(clipped.line), to_u32(column))
    }

    /// Clips a point to a valid one: a row past the end goes to the end of
    /// the text, a column past the end of its row to the end of the row, a
    /// column inside a character moves by `bias`, and a position inside a
    /// placeholder goes to its start ([`Bias::Left`]) or end
    /// ([`Bias::Right`]).
    pub fn clip_point(&self, point: ReplacePoint, bias: Bias) -> ReplacePoint {
        let (start, end, item) = self
            .transforms
            .find::<Dimensions<ReplacePoint, InputPoint>, _>((), &point, sum_tree::Bias::Right);
        match item {
            Some(transform) if transform.is_fold() => {
                if point == start.0 || bias == Bias::Left {
                    start.0
                } else {
                    end.0
                }
            }
            Some(_) => {
                let input = advance(start.1.0, point.0 - start.0.0);
                let clipped = self.clip_input(input, bias).max(start.1.0).min(end.1.0);
                ReplacePoint(advance(start.0.0, clipped - start.1.0))
            }
            None => self.max_point(),
        }
    }

    /// The replace point of a buffer point, clipped first. The position
    /// between the CR and the LF of a CRLF goes to the end of its line
    /// ([`Bias::Left`]) or the start of the next ([`Bias::Right`]); a
    /// position inside a fold to the start ([`Bias::Left`]) or end
    /// ([`Bias::Right`]) of its placeholder.
    pub fn to_replace_point(&self, point: BufferPoint, bias: Bias) -> ReplacePoint {
        let clipped = self.buffer.clip_point(point, bias);
        let content = self.content_len(clipped.line);
        let input = if clipped.column <= content {
            Point::new(to_u32(clipped.line), to_u32(clipped.column))
        } else {
            match bias {
                Bias::Left => Point::new(to_u32(clipped.line), to_u32(content)),
                Bias::Right => Point::new(to_u32(clipped.line + 1), 0),
            }
        };
        let (start, end, item) = self
            .transforms
            .find::<Dimensions<InputPoint, ReplacePoint>, _>(
                (),
                &InputPoint(input),
                sum_tree::Bias::Right,
            );
        match item {
            Some(transform) if transform.is_fold() => {
                if bias == Bias::Left || input == start.0.0 {
                    start.1
                } else {
                    end.1
                }
            }
            Some(_) => ReplacePoint(cmp::min(advance(start.1.0, input - start.0.0), end.1.0)),
            None => end.1,
        }
    }

    /// The buffer point of a replace point, clipped first: a placeholder's
    /// start is its fold's start and its end the fold's end.
    pub fn to_buffer_point(&self, point: ReplacePoint, bias: Bias) -> BufferPoint {
        let clipped = self.clip_point(point, bias);
        let (start, _, _) = self
            .transforms
            .find::<Dimensions<ReplacePoint, InputPoint>, _>((), &clipped, sum_tree::Bias::Right);
        let input = advance(start.1.0, clipped.0 - start.0.0);
        BufferPoint::new(input.row as usize, input.column as usize)
    }

    /// The buffer offset of a replace point, clipped first.
    fn buffer_offset(&self, point: ReplacePoint, bias: Bias) -> usize {
        self.buffer
            .point_to_offset(self.to_buffer_point(point, bias))
            .unwrap_or(self.buffer.len())
    }

    /// The text in `range`, in chunks; a placeholder is one chunk. The ends
    /// are clipped, the start with [`Bias::Left`] and the end with
    /// [`Bias::Right`].
    pub fn chunks(&self, range: Range<ReplacePoint>) -> ReplaceChunks<'_> {
        let start = self.clip_point(range.start, Bias::Left);
        let end = self.clip_point(range.end, Bias::Right).max(start);
        let start_offset = self.buffer_offset(start, Bias::Left);
        let end_offset = self.buffer_offset(end, Bias::Right).max(start_offset);
        let mut transforms = self
            .transforms
            .cursor::<Dimensions<ReplacePoint, InputOffset>>(());
        transforms.seek(&start, sum_tree::Bias::Right);
        ReplaceChunks {
            snapshot: self,
            transforms,
            text: None,
            start: start_offset,
            end: end_offset,
            end_point: end,
        }
    }

    /// The text from `start` to the end, in chunks.
    pub fn chunks_at(&self, start: ReplacePoint) -> ReplaceChunks<'_> {
        self.chunks(start..self.max_point())
    }

    /// The characters from `start` to the end.
    pub fn chars_at(&self, start: ReplacePoint) -> impl Iterator<Item = char> + '_ {
        self.chunks_at(start).flat_map(|chunk| chunk.text.chars())
    }

    /// The whole text.
    pub fn text(&self) -> String {
        self.chunks_at(ReplacePoint::zero())
            .map(|chunk| chunk.text)
            .collect()
    }

    /// The summary of the text in `range`.
    ///
    /// The transforms count lines but not characters, so this scans the
    /// range: O(length of the range).
    pub fn text_summary_for_range(&self, range: Range<ReplacePoint>) -> TextSummary {
        let mut summary = TextSummary::default();
        for chunk in self.chunks(range) {
            summary += &TextSummary::from(chunk.text);
        }
        summary
    }

    /// The summary of the whole text. O(n), see
    /// [`text_summary_for_range`](Self::text_summary_for_range).
    pub fn text_summary(&self) -> TextSummary {
        self.text_summary_for_range(ReplacePoint::zero()..self.max_point())
    }

    // ----- folds --------------------------------------------------------------

    /// The folds, sorted by start (the longer first on a tie).
    pub fn folds(&self) -> &[Fold] {
        &self.folds
    }

    /// Whether there is any fold.
    pub fn has_folds(&self) -> bool {
        !self.folds.is_empty()
    }

    /// The number of folds.
    pub fn fold_count(&self) -> usize {
        self.folds.len()
    }

    /// The folds that share bytes with `range` (byte offsets); for an empty
    /// range, the folds that have it strictly inside.
    pub fn folds_in_range(&self, range: Range<usize>) -> impl Iterator<Item = &Fold> + '_ {
        self.folds
            .iter()
            .take_while(move |fold| fold.range.start < range.end.max(range.start + 1))
            .filter(move |fold| intersects(&fold.range, &range, false))
    }

    /// The hidden bytes of the placeholder whose run of folds has `offset`
    /// strictly inside, if any: a caret there is not visible. (Positions at
    /// a fold's ends, also between two folds that touch, are visible.)
    pub fn folded_range_at(&self, offset: usize) -> Option<Range<usize>> {
        let (start, end, item) =
            self.transforms
                .find::<InputOffset, _>((), &InputOffset(offset), sum_tree::Bias::Right);
        item.filter(|transform| transform.is_fold() && offset > start.0)
            .map(|_| start.0..end.0)
    }

    /// Whether buffer line `line` shows a placeholder or is hidden: some
    /// fold covers part of it (its start included, its end before the
    /// line's start).
    pub fn is_line_folded(&self, line: usize) -> bool {
        let Ok(range) = self.buffer.line_range(line) else {
            return false;
        };
        let mut cursor = self.transforms.cursor::<InputOffset>(());
        cursor.seek(&InputOffset(range.start), sum_tree::Bias::Right);
        while let Some(transform) = cursor.item() {
            if cursor.start().0 > range.end {
                return false;
            }
            if transform.is_fold() {
                return true;
            }
            cursor.next();
        }
        false
    }

    /// The placeholders on rows `rows` (end excluded), in order.
    pub fn placeholders(&self, rows: Range<u32>) -> Vec<Placeholder> {
        let mut placeholders = Vec::new();
        let start = ReplacePoint::new(rows.start, 0);
        let end = ReplacePoint::new(rows.end, 0);
        let mut cursor = self
            .transforms
            .cursor::<Dimensions<ReplacePoint, InputOffset>>(());
        cursor.seek(&start, sum_tree::Bias::Right);
        while let Some(transform) = cursor.item() {
            if cursor.start().0 >= end {
                break;
            }
            if let Some(fold) = transform.placeholder {
                placeholders.push(Placeholder {
                    fold,
                    range: cursor.start().0..cursor.end().0,
                    buffer_range: cursor.start().1.0..cursor.end().1.0,
                });
            }
            cursor.next();
        }
        placeholders
    }

    /// The placeholder that starts at `point` or has it inside.
    pub fn placeholder_at(&self, point: ReplacePoint) -> Option<Placeholder> {
        let (start, end, item) = self
            .transforms
            .find::<Dimensions<ReplacePoint, InputOffset>, _>((), &point, sum_tree::Bias::Right);
        let fold = item?.placeholder?;
        Some(Placeholder {
            fold,
            range: start.0..end.0,
            buffer_range: start.1.0..end.1.0,
        })
    }

    /// The buffer line at the start of row `row` (the last line for a row
    /// past the end).
    pub fn buffer_line(&self, row: u32) -> usize {
        self.to_buffer_point(ReplacePoint::new(row, 0), Bias::Left)
            .line
    }

    /// The buffer line at the start of every row from `start_row` on.
    pub fn buffer_lines(&self, start_row: u32) -> impl Iterator<Item = usize> + '_ {
        (start_row..=self.max_point().row()).map(|row| self.buffer_line(row))
    }
}

/// The replace layer's text in chunks of at most [`MAX_CHUNK_LEN`] bytes.
/// Never yields an empty chunk; a placeholder is a chunk of its own.
#[derive(Debug)]
pub struct ReplaceChunks<'a> {
    snapshot: &'a ReplaceSnapshot,
    transforms: Cursor<'a, 'static, Transform, Dimensions<ReplacePoint, InputOffset>>,
    /// The text of the isomorphic transform being read.
    text: Option<BufferChunks<'a>>,
    /// Buffer offsets of the range.
    start: usize,
    end: usize,
    end_point: ReplacePoint,
}

impl<'a> ReplaceChunks<'a> {
    /// Restarts the iterator on another range of the same snapshot.
    pub fn seek(&mut self, range: Range<ReplacePoint>) {
        *self = self.snapshot.chunks(range);
    }
}

impl<'a> Iterator for ReplaceChunks<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Chunk<'a>> {
        loop {
            if let Some(text) = &mut self.text {
                if let Some(chunk) = text.next() {
                    return Some(chunk);
                }
                self.text = None;
                self.transforms.next();
            }
            let transform = self.transforms.item()?;
            if self.transforms.start().0 >= self.end_point {
                return None;
            }
            if transform.is_fold() {
                self.transforms.next();
                return Some(Chunk::new(PLACEHOLDER));
            }
            let input_start = self.transforms.start().1.0;
            let from = self.start.max(input_start);
            let to = self.end.min(input_start + transform.summary.input.len);
            if from < to {
                self.text = Some(BufferChunks::new(&self.snapshot.buffer, from..to));
            } else {
                self.transforms.next();
            }
        }
    }
}

/// Buffer text with every line terminator as one `\n`, in chunks of at most
/// [`MAX_CHUNK_LEN`] bytes, none empty. The range must not end inside a
/// CRLF.
#[derive(Debug)]
struct BufferChunks<'a> {
    buffer_chunks: Option<Peekable<gv_text::Chunks<'a>>>,
    /// The rest of the current buffer chunk.
    pending: &'a str,
}

impl<'a> BufferChunks<'a> {
    fn new(buffer: &'a TextSnapshot, range: Range<usize>) -> Self {
        Self {
            buffer_chunks: buffer.chunks(range).ok().map(Iterator::peekable),
            pending: "",
        }
    }

    /// Whether the next buffer chunk starts with an LF.
    fn next_starts_with_lf(&mut self) -> bool {
        let Some(chunks) = self.buffer_chunks.as_mut() else {
            return false;
        };
        while let Some(next) = chunks.peek() {
            if !next.is_empty() {
                return next.starts_with('\n');
            }
            chunks.next();
        }
        false
    }
}

impl<'a> Iterator for BufferChunks<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Chunk<'a>> {
        loop {
            let text = self.pending;
            if text.is_empty() {
                self.pending = self.buffer_chunks.as_mut()?.next()?;
                continue;
            }
            let split = match text.find('\r') {
                Some(0) => {
                    self.pending = &text[1..];
                    let crlf = match text.as_bytes().get(1) {
                        Some(&byte) => byte == b'\n',
                        None => self.next_starts_with_lf(),
                    };
                    if crlf {
                        // The LF that follows ends the row.
                        continue;
                    }
                    return Some(Chunk::new("\n"));
                }
                Some(cr) => cr,
                None => text.len(),
            };
            let mut len = split.min(MAX_CHUNK_LEN);
            while !text.is_char_boundary(len) {
                len -= 1;
            }
            let (head, tail) = text.split_at(len);
            self.pending = tail;
            return Some(Chunk::new(head));
        }
    }
}

#[cfg(test)]
mod tests;

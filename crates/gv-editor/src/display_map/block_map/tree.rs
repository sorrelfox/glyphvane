// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The block layer's sum tree: runs of text rows and blocks, summarized by
//! rows and pixels, and the layout of a range of rows from its blocks.

use std::cmp::Ordering;
use std::ops::Range;

use sum_tree::{Bias, Cursor, Dimension, SeekTarget, SumTree};

use super::input::BlockInput;
use super::{Block, BlockId, BlockKind, BlockPlacement, Height, Reveal};

/// An item of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Transform {
    /// Input rows shown as text, one row height each. Never empty, and
    /// never next to another run (the tree is canonical, so a rebuild and
    /// an incremental update give the same items).
    Rows(u32),
    /// A block that is shown.
    Block(BlockTransform),
}

/// A block as the tree holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BlockTransform {
    pub id: BlockId,
    pub kind: BlockKind,
    pub reveal: Reveal,
    pub height: Height,
    /// The input rows it hides: those of its display lines for a replace
    /// block (with the rows of the blocks merged into it), else 0.
    pub replaced: u32,
}

impl sum_tree::Item for Transform {
    type Summary = TransformSummary;

    fn summary(&self, (): ()) -> TransformSummary {
        match self {
            Self::Rows(rows) => TransformSummary {
                input_rows: *rows,
                text_rows: *rows,
                block_height: 0,
                blocks: 0,
            },
            Self::Block(block) => TransformSummary {
                input_rows: block.replaced,
                text_rows: 0,
                block_height: u64::from(block.height.0),
                blocks: 1,
            },
        }
    }
}

/// Rows and pixels of a part of the tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TransformSummary {
    /// Input rows, shown or replaced.
    pub input_rows: u32,
    /// Input rows shown as text.
    pub text_rows: u32,
    /// The heights of the blocks, in 1/64 px.
    pub block_height: u64,
    /// Blocks shown.
    pub blocks: u32,
}

impl TransformSummary {
    /// The height in 1/64 px with text rows `row_height` tall.
    pub fn y(&self, row_height: Height) -> u64 {
        u64::from(self.text_rows) * u64::from(row_height.0) + self.block_height
    }
}

impl sum_tree::ContextLessSummary for TransformSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, other: &Self) {
        self.input_rows += other.input_rows;
        self.text_rows += other.text_rows;
        self.block_height += other.block_height;
        self.blocks += other.blocks;
    }
}

/// A position in input rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct InputRow(pub u32);

impl Dimension<'_, TransformSummary> for InputRow {
    fn zero((): ()) -> Self {
        Self(0)
    }

    fn add_summary(&mut self, summary: &TransformSummary, (): ()) {
        self.0 += summary.input_rows;
    }
}

impl SeekTarget<'_, TransformSummary, TransformSummary> for InputRow {
    fn cmp(&self, location: &TransformSummary, (): ()) -> Ordering {
        self.0.cmp(&location.input_rows)
    }
}

/// A position in 1/64 px, with the row height to count text rows by.
#[derive(Clone, Copy, Debug)]
pub(super) struct YTarget {
    pub y: u64,
    pub row_height: Height,
}

impl SeekTarget<'_, TransformSummary, TransformSummary> for YTarget {
    fn cmp(&self, location: &TransformSummary, (): ()) -> Ordering {
        self.y.cmp(&location.y(self.row_height))
    }
}

/// Appends `rows` text rows, extending a run at the end.
pub(super) fn push_rows(tree: &mut SumTree<Transform>, rows: u32) {
    if rows == 0 {
        return;
    }
    if matches!(tree.last(), Some(Transform::Rows(_))) {
        tree.update_last(
            |last| {
                if let Transform::Rows(run) = last {
                    *run += rows;
                }
            },
            (),
        );
    } else {
        tree.push(Transform::Rows(rows), ());
    }
}

/// Whether an item that starts at input row `start` belongs to the rows
/// before `row`: it ends there or before, or (a block between rows) sits
/// before it or at it below the row above.
fn before(item: &Transform, start: u32, row: u32) -> bool {
    match item {
        Transform::Rows(rows) => start + rows <= row,
        Transform::Block(block) if block.replaced > 0 => start + block.replaced <= row,
        Transform::Block(block) => start < row || (start == row && block.kind == BlockKind::Below),
    }
}

/// Walks the old tree during an update, copying what stays and skipping
/// what is laid out again. Rows are split between the two at row
/// boundaries; blocks between rows go with the row they belong to (below
/// the row above, or above the row below).
pub(super) struct Walker<'a> {
    cursor: Cursor<'a, 'static, Transform, InputRow>,
    /// Rows of a run the cursor has passed, neither copied nor skipped yet.
    pending: Option<Range<u32>>,
}

impl<'a> Walker<'a> {
    pub fn new(tree: &'a SumTree<Transform>) -> Self {
        let mut cursor = tree.cursor::<InputRow>(());
        cursor.seek(&InputRow(0), Bias::Left);
        Self {
            cursor,
            pending: None,
        }
    }

    /// Copies to `out` everything that belongs to the rows before `row`.
    pub fn copy_until(&mut self, row: u32, out: &mut SumTree<Transform>) {
        if let Some(pending) = self.pending.take() {
            if pending.end > row {
                push_rows(out, row.saturating_sub(pending.start));
                self.pending = Some(pending.start.max(row)..pending.end);
                return;
            }
            push_rows(out, pending.end - pending.start);
        }
        // The first item by hand (a run joins the one `out` ends with),
        // then whole subtrees, then the items at the boundary by hand.
        let mut sliced = false;
        while let Some(item) = self.cursor.item() {
            let start = self.cursor.start().0;
            if !before(item, start, row) {
                if let Transform::Rows(rows) = item
                    && start < row
                {
                    push_rows(out, row - start);
                    self.pending = Some(row..start + rows);
                    self.cursor.next();
                }
                return;
            }
            match item {
                Transform::Rows(rows) => push_rows(out, *rows),
                Transform::Block(_) => out.push(item.clone(), ()),
            }
            self.cursor.next();
            if !sliced {
                sliced = true;
                out.append(self.cursor.slice(&InputRow(row), Bias::Left), ());
            }
        }
    }

    /// Skips everything that belongs to the rows before `row`.
    pub fn skip_until(&mut self, row: u32) {
        if let Some(pending) = self.pending.take()
            && pending.end > row
        {
            self.pending = Some(pending.start.max(row)..pending.end);
            return;
        }
        if self.cursor.start().0 < row {
            self.cursor.seek_forward(&InputRow(row), Bias::Left);
        }
        while let Some(item) = self.cursor.item() {
            let start = self.cursor.start().0;
            if !before(item, start, row) {
                if let Transform::Rows(rows) = item
                    && start < row
                {
                    self.pending = Some(row..start + rows);
                    self.cursor.next();
                }
                return;
            }
            self.cursor.next();
        }
    }

    /// Copies the rest to `out`.
    pub fn finish(mut self, out: &mut SumTree<Transform>) {
        if let Some(pending) = self.pending.take() {
            push_rows(out, pending.end - pending.start);
        }
        if let Some(Transform::Rows(rows)) = self.cursor.item() {
            push_rows(out, *rows);
            self.cursor.next();
        }
        out.append(self.cursor.suffix(), ());
    }
}

/// Where a block goes in the input rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Resolved {
    /// Not shown: its display line is folded away.
    Folded,
    /// Between rows: before input row `gap`, after the zero-row blocks
    /// before it of lower `rank` (0: below the row above, 1: above the row
    /// below).
    Gap { gap: u32, rank: u8 },
    /// Instead of `rows`.
    Replace { rows: Range<u32> },
}

impl Resolved {
    /// Where the block's item would start in the tree.
    pub fn position(&self) -> Option<u32> {
        match self {
            Self::Folded => None,
            Self::Gap { gap, .. } => Some(*gap),
            Self::Replace { rows } => Some(rows.start),
        }
    }
}

/// The buffer line of a byte offset (clipped to the text).
fn line_of(input: &BlockInput, offset: usize) -> usize {
    let text = input.buffer();
    let offset = text.clip_offset(offset.min(text.len()), gv_text::Bias::Left);
    text.line_of_offset(offset)
        .unwrap_or_else(|_| text.line_count().saturating_sub(1))
}

/// Whether buffer line `line` starts inside a fold, so its display line is
/// another line's (the fold's).
fn is_folded(input: &BlockInput, line: usize) -> bool {
    let replace = input.replace_snapshot();
    replace.has_folds()
        && input
            .buffer()
            .line_start(line)
            .is_ok_and(|start| replace.folded_range_at(start).is_some())
}

/// The display lines of a replace block, first and last buffer line: the
/// lines its range touches, without the line its end starts (a range that
/// ends after a line break).
fn replaced_lines(input: &BlockInput, range: &Range<gv_text::Anchor>) -> (usize, usize) {
    let low = range.start.offset.min(range.end.offset);
    let high = range.start.offset.max(range.end.offset);
    let first = line_of(input, low);
    let mut last = line_of(input, high);
    if last > first && input.buffer().line_start(last) == Ok(high) {
        last -= 1;
    }
    (first, last)
}

/// Where `block` goes in `input`.
pub(super) fn resolve(block: &Block, input: &BlockInput) -> Resolved {
    match &block.placement {
        BlockPlacement::Above(anchor) => {
            let line = line_of(input, anchor.offset);
            if is_folded(input, line) {
                return Resolved::Folded;
            }
            Resolved::Gap {
                gap: input.line_rows(line).start,
                rank: 1,
            }
        }
        BlockPlacement::Below(anchor) => {
            let line = line_of(input, anchor.offset);
            if is_folded(input, line) {
                return Resolved::Folded;
            }
            Resolved::Gap {
                gap: input.line_rows(line).end,
                rank: 0,
            }
        }
        BlockPlacement::Replace(range) => {
            let (first, last) = replaced_lines(input, range);
            if is_folded(input, first) {
                return Resolved::Folded;
            }
            let start = input.line_rows(first).start;
            let end = input.line_rows(last).end.max(start + 1);
            Resolved::Replace { rows: start..end }
        }
    }
}

/// The rows of the display lines `block` is attached to, folded or not: an
/// over-estimate of the rows a change of the block touches.
pub(super) fn attached_rows(block: &Block, input: &BlockInput) -> Range<u32> {
    match &block.placement {
        BlockPlacement::Above(anchor) | BlockPlacement::Below(anchor) => {
            input.line_rows(line_of(input, anchor.offset))
        }
        BlockPlacement::Replace(range) => {
            let (first, last) = replaced_lines(input, range);
            let start = input.line_rows(first).start;
            start..input.line_rows(last).end.max(start + 1)
        }
    }
}

fn block_item(block: &Block, kind: BlockKind, replaced: u32) -> Transform {
    Transform::Block(BlockTransform {
        id: block.id,
        kind,
        reveal: block.reveal,
        height: block.height,
        replaced,
    })
}

/// The layout of some display lines from the blocks attached to them.
///
/// Replace blocks whose rows overlap merge into one: the first (by first
/// row, then id) is shown over all their rows. Blocks between rows that
/// belong to a replaced row are hidden; blocks between the same two rows
/// go below the row above first, then above the row below, each in id
/// order.
pub(super) struct Plan<'a> {
    /// Blocks between rows: gap, rank, block; sorted.
    gaps: Vec<(u32, u8, &'a Block)>,
    /// Merged replace blocks: their rows and the block shown; sorted.
    merged: Vec<(Range<u32>, &'a Block)>,
}

impl<'a> Plan<'a> {
    pub fn new(input: &BlockInput, blocks: &'a [Block]) -> Self {
        let mut gaps: Vec<(u32, u8, &Block)> = Vec::new();
        let mut replaces: Vec<(Range<u32>, &Block)> = Vec::new();
        for block in blocks {
            match resolve(block, input) {
                Resolved::Folded => {}
                Resolved::Gap { gap, rank } => gaps.push((gap, rank, block)),
                Resolved::Replace { rows } => replaces.push((rows, block)),
            }
        }
        replaces.sort_by_key(|(rows, block)| (rows.start, block.id));
        let mut merged: Vec<(Range<u32>, &Block)> = Vec::with_capacity(replaces.len());
        for (range, block) in replaces {
            match merged.last_mut() {
                Some((last, _)) if range.start < last.end => last.end = last.end.max(range.end),
                _ => merged.push((range, block)),
            }
        }
        let is_replaced = |row: u32| {
            let index = merged.partition_point(|(range, _)| range.end <= row);
            merged
                .get(index)
                .is_some_and(|(range, _)| range.start <= row)
        };
        gaps.retain(|&(gap, rank, _)| {
            let row = if rank == 0 {
                gap.checked_sub(1)
            } else {
                Some(gap)
            };
            !row.is_some_and(is_replaced)
        });
        gaps.sort_by_key(|&(gap, rank, block)| (gap, rank, block.id));
        Self { gaps, merged }
    }

    /// The end of the last replaced row, if any. A replace block attached
    /// to some lines can reach past them (a block whose first line was
    /// folded, unfolded): the lines laid out must then grow to take it in.
    pub fn reach(&self) -> Option<u32> {
        self.merged.last().map(|(range, _)| range.end)
    }

    /// Appends the items of input rows `rows` to `out`.
    pub fn emit(self, rows: Range<u32>, out: &mut SumTree<Transform>) {
        debug_assert!(
            self.gaps
                .iter()
                .all(|&(gap, ..)| rows.start <= gap && gap <= rows.end),
            "a block between rows outside {rows:?}"
        );
        debug_assert!(
            self.merged
                .iter()
                .all(|(range, _)| rows.start <= range.start && range.end <= rows.end),
            "a replace block outside {rows:?}"
        );
        let mut row = rows.start;
        let mut gaps = self.gaps.into_iter().peekable();
        let mut merged = self.merged.into_iter().peekable();
        loop {
            let gap_first = match (gaps.peek(), merged.peek()) {
                (Some(&(gap, ..)), Some((range, _))) => gap <= range.start,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if gap_first {
                let Some((gap, rank, block)) = gaps.next() else {
                    break;
                };
                let gap = gap.clamp(row, rows.end);
                push_rows(out, gap - row);
                row = gap;
                let kind = if rank == 0 {
                    BlockKind::Below
                } else {
                    BlockKind::Above
                };
                out.push(block_item(block, kind, 0), ());
            } else {
                let Some((range, block)) = merged.next() else {
                    break;
                };
                let start = range.start.clamp(row, rows.end);
                let end = range.end.clamp(start, rows.end);
                push_rows(out, start - row);
                out.push(block_item(block, BlockKind::Replace, end - start), ());
                row = end;
            }
        }
        push_rows(out, rows.end.saturating_sub(row));
    }
}

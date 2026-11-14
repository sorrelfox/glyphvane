// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The block layer: block widgets above, below or instead of the rows of
//! the layer below, with heights in pixels.
//!
//! A block is something the view draws itself at a height it measures: an
//! image preview, a rendered table, later an embed or a diagram. This
//! layer only places blocks and adds up heights; the view maps each
//! [`BlockId`] to its widget. The input is the wrap layer, or the tab layer
//! while soft wrap is off ([`BlockInput`]).
//!
//! # Placement
//!
//! A block is anchored to buffer positions ([`Anchor`]s, moved through
//! edits) and placed by **display line**: the rows that show one tab row,
//! so all the wrap rows of a buffer line, or of several lines a fold joins.
//!
//! - [`BlockPlacement::Above`] the display line of its anchor;
//! - [`BlockPlacement::Below`] it;
//! - [`BlockPlacement::Replace`] the display lines a range touches (a range
//!   that ends right after a line break does not touch the next line): the
//!   rows are hidden and the block is shown in their place.
//!
//! Conflicts are resolved the same way however the layer got there:
//!
//! - replace blocks whose rows overlap merge into one, shown with the
//!   height of the first (by first row, then [`BlockId`]) over all their
//!   rows;
//! - a block above or below a replaced row is not shown;
//! - a block whose display line starts inside a fold is not shown;
//! - blocks between the same two rows go below the upper row first, then
//!   above the lower one, each kind in the order the blocks were inserted.
//!
//! Hidden blocks are kept, and come back when the conflict goes.
//!
//! # Heights
//!
//! A text row is [`row_height`](BlockSnapshot::row_height) tall and a block
//! is its own [`Height`]. Heights are fixed point in 1/64 px, so sums are
//! exact over any document; positions from the top of the document are
//! [`Y`]s. The layer is a sum tree of runs of text rows and blocks,
//! summarized by input rows, text rows and block pixels side by side, so
//! row ↔ y lookups are O(log n), and a new row height costs nothing.
//!
//! # Updates
//!
//! [`BlockMap::sync`] takes the new input and its row edits (and the
//! buffer edit, if the text changed, to move the anchors). Only the display
//! lines the edits touch are laid out again, with the blocks attached to
//! them (O(log n) each); the rest of the tree is reused. An edit that
//! deletes the text an anchor sticks to keeps or removes the block, per
//! its [`WhenDeleted`]; both are reported in [`BlockChanges`].
//! [`BlockMap::resize`] (an image loaded) changes one item in place. A
//! change in many places at once (blocks inserted in bulk) lays the layer
//! out from scratch, which is cheaper then.
//!
//! The layer does not move carets. A replace block's [`Reveal`] tells the
//! [`Editor`](crate::Editor) what to do when a selection comes into the rows
//! it hides: skip them, or remove the block so the source shows.

mod input;
mod tree;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::{Add, Range};

use gv_text::{Anchor, Bias, EditSummary, TextSnapshot};
use sum_tree::{Cursor, SumTree};

pub use input::BlockInput;
use tree::{InputRow, Plan, Transform, TransformSummary, Walker, YTarget};

use super::primitives::Edit;
use super::tab_map::{TabEdit, TabSnapshot};
use super::wrap_map::{WrapEdit, WrapSnapshot};

/// Fixed-point steps per pixel: lengths in the block layer are in 1/64 px.
pub const SUBPIXELS_PER_PX: u32 = 64;

/// A change that lays out more places than this, and more than a third of
/// the blocks, rebuilds the layer instead.
const REBUILD_REGIONS: usize = 32;

/// An edit of the input rows: `old` rows of the previous input (end
/// exclusive) became `new` rows of the next.
pub type RowEdit = Edit<u32>;

/// A height in 1/64 px: of a text row or a block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Height(pub u32);

impl Height {
    /// No height.
    pub const ZERO: Self = Self(0);

    /// `px` pixels, rounded to 1/64 px. Negative values and NaN give zero,
    /// values too large the largest height.
    pub fn from_px(px: f32) -> Self {
        Self((px * SUBPIXELS_PER_PX as f32).round() as u32)
    }

    /// In pixels.
    pub fn to_px(self) -> f32 {
        self.0 as f32 / SUBPIXELS_PER_PX as f32
    }
}

/// A vertical position from the top of the document (or a distance), in
/// 1/64 px. Pixels are `f64` here: a long document is taller than an `f32`
/// counts to the pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Y(pub u64);

impl Y {
    /// The top of the document.
    pub const ZERO: Self = Self(0);

    /// `px` pixels, rounded to 1/64 px. Negative values and NaN give zero.
    pub fn from_px(px: f64) -> Self {
        Self((px * f64::from(SUBPIXELS_PER_PX)).round() as u64)
    }

    /// In pixels.
    pub fn to_px(self) -> f64 {
        self.0 as f64 / f64::from(SUBPIXELS_PER_PX)
    }
}

impl Add<Height> for Y {
    type Output = Self;

    fn add(self, height: Height) -> Self {
        Self(self.0 + u64::from(height.0))
    }
}

/// Identifies a block; never reused by its map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u64);

/// Where a block goes (see the [module docs](self#placement)).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BlockPlacement {
    /// Above the display line of the anchor.
    Above(Anchor),
    /// Below the display line of the anchor.
    Below(Anchor),
    /// Instead of the display lines the range touches. The ends may come
    /// in either order; an empty range replaces its line.
    Replace(Range<Anchor>),
}

impl BlockPlacement {
    /// The kind of placement.
    pub fn kind(&self) -> BlockKind {
        match self {
            Self::Above(_) => BlockKind::Above,
            Self::Below(_) => BlockKind::Below,
            Self::Replace(_) => BlockKind::Replace,
        }
    }

    /// The first buffer offset of the placement: what blocks are sorted by.
    fn key(&self) -> usize {
        match self {
            Self::Above(anchor) | Self::Below(anchor) => anchor.offset,
            Self::Replace(range) => range.start.offset.min(range.end.offset),
        }
    }

    fn anchors(&self) -> impl Iterator<Item = Anchor> {
        let (first, second) = match self {
            Self::Above(anchor) | Self::Below(anchor) => (*anchor, None),
            Self::Replace(range) => (range.start, Some(range.end)),
        };
        std::iter::once(first).chain(second)
    }

    fn map_anchors(&mut self, mut f: impl FnMut(Anchor) -> Anchor) {
        match self {
            Self::Above(anchor) | Self::Below(anchor) => *anchor = f(*anchor),
            Self::Replace(range) => {
                range.start = f(range.start);
                range.end = f(range.end);
            }
        }
    }
}

/// The kind of a [`BlockPlacement`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// Above a display line.
    Above,
    /// Below a display line.
    Below,
    /// Instead of display lines.
    Replace,
}

/// What happens to a block when an edit deletes the character one of its
/// anchors sticks to: the one before it for [`Bias::Left`], after it for
/// [`Bias::Right`]. (Replacing text deletes it too.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WhenDeleted {
    /// The block stays, its anchor moved to the edit, and is reported in
    /// [`BlockChanges::displaced`].
    #[default]
    Keep,
    /// The block is removed and reported in [`BlockChanges::removed`].
    Remove,
}

/// What the [`Editor`](crate::Editor) does when a selection end comes into
/// the rows a replace block hides. (Other blocks hide no rows.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Reveal {
    /// Keep them hidden: carets skip the rows, and a selection end that
    /// lands in them otherwise (a click, an edit, undo) moves out of the
    /// block, to the start of the row after it (or the end of the row
    /// before it at the end of the text).
    #[default]
    Never,
    /// Show them: carets move onto the rows like onto any other, and the
    /// block is removed (reported as [`Removal::Revealed`]) so the source
    /// shows. The block's owner inserts it again when it wants.
    OnSelection,
}

/// A block to insert.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlockProperties {
    /// Where it goes.
    pub placement: BlockPlacement,
    /// How tall it is.
    pub height: Height,
    /// What an edit that deletes its anchors' text does.
    pub when_deleted: WhenDeleted,
    /// What a selection in its rows does (replace blocks).
    pub reveal: Reveal,
}

impl BlockProperties {
    /// A block that is kept through edits and never revealed.
    pub fn new(placement: BlockPlacement, height: Height) -> Self {
        Self {
            placement,
            height,
            when_deleted: WhenDeleted::Keep,
            reveal: Reveal::Never,
        }
    }

    /// With `when_deleted`.
    #[must_use]
    pub fn when_deleted(mut self, when_deleted: WhenDeleted) -> Self {
        self.when_deleted = when_deleted;
        self
    }

    /// With `reveal`.
    #[must_use]
    pub fn reveal(mut self, reveal: Reveal) -> Self {
        self.reveal = reveal;
        self
    }
}

/// A block in a map, its anchors in the map's current text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Block {
    /// Its id.
    pub id: BlockId,
    /// Where it goes.
    pub placement: BlockPlacement,
    /// How tall it is.
    pub height: Height,
    /// What an edit that deletes its anchors' text does.
    pub when_deleted: WhenDeleted,
    /// What a selection in its rows does.
    pub reveal: Reveal,
}

impl Block {
    fn sort_key(&self) -> (usize, BlockId) {
        (self.placement.key(), self.id)
    }
}

/// Why a block was removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Removal {
    /// Its owner removed it.
    Requested,
    /// An edit deleted its anchors' text ([`WhenDeleted::Remove`]).
    AnchorDeleted,
    /// A selection came into its rows ([`Reveal::OnSelection`]).
    Revealed,
}

/// What happened to blocks in a change of the layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockChanges {
    /// Blocks inserted.
    pub inserted: Vec<BlockId>,
    /// Blocks whose height changed.
    pub resized: Vec<BlockId>,
    /// Blocks removed, and why.
    pub removed: Vec<(BlockId, Removal)>,
    /// Blocks kept although an edit deleted their anchors' text
    /// ([`WhenDeleted::Keep`]): their anchors moved to the edit.
    pub displaced: Vec<BlockId>,
}

impl BlockChanges {
    /// Whether nothing happened.
    pub fn is_empty(&self) -> bool {
        self.inserted.is_empty()
            && self.resized.is_empty()
            && self.removed.is_empty()
            && self.displaced.is_empty()
    }

    /// Adds `other`'s changes.
    pub fn extend(&mut self, other: Self) {
        self.inserted.extend(other.inserted);
        self.resized.extend(other.resized);
        self.removed.extend(other.removed);
        self.displaced.extend(other.displaced);
    }
}

/// A block as shown.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlockItem {
    /// The block.
    pub id: BlockId,
    /// Its placement's kind.
    pub kind: BlockKind,
    /// Its reveal rule.
    pub reveal: Reveal,
    /// Its top.
    pub top: Y,
    /// Its height.
    pub height: Height,
    /// The input rows it hides; for a block between rows, the empty range
    /// at the row below it.
    pub rows: Range<u32>,
}

/// One thing on the screen, from the top of the document down.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DisplayItem {
    /// An input row shown as text, one row height tall.
    Row {
        /// The row.
        row: u32,
        /// Its top.
        top: Y,
    },
    /// A block.
    Block(BlockItem),
}

impl DisplayItem {
    /// The item's top.
    pub fn top(&self) -> Y {
        match self {
            Self::Row { top, .. } => *top,
            Self::Block(block) => block.top,
        }
    }

    /// The input rows it shows or hides (see [`BlockItem::rows`]).
    pub fn rows(&self) -> Range<u32> {
        match self {
            Self::Row { row, .. } => *row..*row + 1,
            Self::Block(block) => block.rows.clone(),
        }
    }
}

/// An immutable view of the block layer. Cloning is O(1).
#[derive(Clone)]
pub struct BlockSnapshot {
    input: BlockInput,
    transforms: SumTree<Transform>,
    row_height: Height,
}

impl fmt::Debug for BlockSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let summary = self.transforms.summary();
        f.debug_struct("BlockSnapshot")
            .field("rows", &summary.input_rows)
            .field("blocks", &summary.blocks)
            .field("row_height", &self.row_height)
            .field("height", &self.total_height())
            .finish_non_exhaustive()
    }
}

/// The tree of `rows` text rows and no block.
fn rows_tree(rows: u32) -> SumTree<Transform> {
    let mut tree = SumTree::new(());
    tree::push_rows(&mut tree, rows);
    tree
}

impl BlockSnapshot {
    /// The input rows.
    pub fn input(&self) -> &BlockInput {
        &self.input
    }

    /// The buffer text under every layer.
    pub fn buffer(&self) -> &TextSnapshot {
        self.input.buffer()
    }

    /// The height of a text row.
    pub fn row_height(&self) -> Height {
        self.row_height
    }

    /// The number of input rows, shown or hidden.
    pub fn row_count(&self) -> u32 {
        self.transforms.summary().input_rows
    }

    /// The number of blocks shown.
    pub fn block_count(&self) -> u32 {
        self.transforms.summary().blocks
    }

    /// Whether any input row is hidden by a block.
    pub fn has_replaced_rows(&self) -> bool {
        let summary = self.transforms.summary();
        summary.input_rows != summary.text_rows
    }

    /// The height of the document.
    pub fn total_height(&self) -> Y {
        Y(self.transforms.summary().y(self.row_height))
    }

    /// The top of input row `row`, or of the block that hides it; the
    /// bottom of the document for a row past the end.
    pub fn row_top(&self, row: u32) -> Y {
        let (start, _, item) =
            self.transforms
                .find::<TransformSummary, _>((), &InputRow(row), sum_tree::Bias::Right);
        let top = start.y(self.row_height);
        match item {
            Some(Transform::Rows(_)) => {
                Y(top + u64::from(row - start.input_rows) * u64::from(self.row_height.0))
            }
            _ => Y(top),
        }
    }

    /// The block that hides input row `row`, if any.
    pub fn replacement_at(&self, row: u32) -> Option<BlockItem> {
        let (start, _, item) =
            self.transforms
                .find::<TransformSummary, _>((), &InputRow(row), sum_tree::Bias::Right);
        match item {
            Some(Transform::Block(block)) if block.replaced > 0 => {
                Some(self.block_item(block, &start))
            }
            _ => None,
        }
    }

    /// The item at `y`: the row or block whose top is at `y` or above it
    /// and whose bottom is below it (blocks of no height are never at a
    /// position). `None` at or past the bottom of the document.
    pub fn item_at(&self, y: Y) -> Option<DisplayItem> {
        let target = YTarget {
            y: y.0,
            row_height: self.row_height,
        };
        let (start, _, item) =
            self.transforms
                .find::<TransformSummary, _>((), &target, sum_tree::Bias::Right);
        let top = start.y(self.row_height);
        Some(match item? {
            Transform::Rows(_) => {
                let rows = (y.0 - top) / u64::from(self.row_height.0);
                DisplayItem::Row {
                    row: start.input_rows + rows as u32,
                    top: Y(top + rows * u64::from(self.row_height.0)),
                }
            }
            Transform::Block(block) => DisplayItem::Block(self.block_item(block, &start)),
        })
    }

    /// The items that intersect `range`, top down: every row and block
    /// with a part in it, and the blocks of no height at a position in it.
    /// For painting the visible part of a document.
    pub fn items(&self, range: Range<Y>) -> DisplayItems<'_> {
        let mut cursor = self.transforms.cursor::<TransformSummary>(());
        cursor.seek(
            &YTarget {
                y: range.start.0,
                row_height: self.row_height,
            },
            sum_tree::Bias::Left,
        );
        DisplayItems {
            snapshot: self,
            cursor,
            start: range.start.0,
            end: range.end.0,
            rows: None,
            done: range.end <= range.start,
        }
    }

    /// The input rows of the items that intersect `range` (rows a block
    /// hides included): the rows to lay out for painting it.
    pub fn rows_in(&self, range: Range<Y>) -> Range<u32> {
        let mut items = self.items(range);
        let Some(first) = items.next() else {
            return 0..0;
        };
        let start = first.rows().start;
        let end = items
            .last()
            .map_or(first.rows().end, |last| last.rows().end);
        start..end
    }

    fn block_item(&self, block: &tree::BlockTransform, start: &TransformSummary) -> BlockItem {
        BlockItem {
            id: block.id,
            kind: block.kind,
            reveal: block.reveal,
            top: Y(start.y(self.row_height)),
            height: block.height,
            rows: start.input_rows..start.input_rows + block.replaced,
        }
    }

    /// Where block `id`'s item starts if the tree shows it at input row
    /// `position` (its gap, or its first replaced row).
    fn find_block(&self, id: BlockId, position: u32) -> Option<BlockItem> {
        let mut cursor = self.transforms.cursor::<TransformSummary>(());
        cursor.seek(&InputRow(position), sum_tree::Bias::Left);
        while let Some(item) = cursor.item() {
            let start = *cursor.start();
            if start.input_rows > position {
                break;
            }
            if let Transform::Block(block) = item
                && block.id == id
            {
                return Some(self.block_item(block, &start));
            }
            cursor.next();
        }
        None
    }

    /// The items of the tree, for comparing two snapshots.
    #[cfg(test)]
    fn transforms(&self) -> Vec<Transform> {
        self.transforms.iter().cloned().collect()
    }
}

/// The items of a part of a [`BlockSnapshot`], top down. See
/// [`BlockSnapshot::items`].
pub struct DisplayItems<'a> {
    snapshot: &'a BlockSnapshot,
    cursor: Cursor<'a, 'static, Transform, TransformSummary>,
    start: u64,
    end: u64,
    /// In a run of text rows: the next row, the end of the run and the
    /// next row's top.
    rows: Option<(u32, u32, u64)>,
    done: bool,
}

impl fmt::Debug for DisplayItems<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplayItems")
            .field("start", &self.start)
            .field("end", &self.end)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl Iterator for DisplayItems<'_> {
    type Item = DisplayItem;

    fn next(&mut self) -> Option<DisplayItem> {
        let row_height = u64::from(self.snapshot.row_height.0);
        while !self.done {
            if let Some((row, end, top)) = self.rows {
                if top >= self.end {
                    break;
                }
                if row < end {
                    self.rows = Some((row + 1, end, top + row_height));
                    return Some(DisplayItem::Row { row, top: Y(top) });
                }
                self.rows = None;
                self.cursor.next();
                continue;
            }
            let Some(item) = self.cursor.item() else {
                break;
            };
            let start = *self.cursor.start();
            let top = start.y(self.snapshot.row_height);
            if top >= self.end {
                break;
            }
            match item {
                Transform::Rows(rows) => {
                    let skip = (self.start.saturating_sub(top) / row_height).min(u64::from(*rows));
                    self.rows = Some((
                        start.input_rows + skip as u32,
                        start.input_rows + rows,
                        top + skip * row_height,
                    ));
                }
                Transform::Block(block) => {
                    self.cursor.next();
                    let bottom = top + u64::from(block.height.0);
                    let visible = if block.height.0 == 0 {
                        top >= self.start
                    } else {
                        bottom > self.start
                    };
                    if visible {
                        return Some(DisplayItem::Block(self.snapshot.block_item(block, &start)));
                    }
                }
            }
        }
        self.done = true;
        None
    }
}

/// The block layer: blocks and the tree of rows and blocks over an input.
///
/// See the [module docs](self).
#[derive(Clone)]
pub struct BlockMap {
    snapshot: BlockSnapshot,
    /// Sorted by first offset, then id.
    blocks: Vec<Block>,
    /// Where each block is in `blocks`.
    index: HashMap<BlockId, usize>,
    next_id: u64,
}

impl fmt::Debug for BlockMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockMap")
            .field("snapshot", &self.snapshot)
            .field("blocks", &self.blocks.len())
            .finish_non_exhaustive()
    }
}

/// Whether `edits` delete the character `anchor` sticks to.
fn is_deleted(edits: &[gv_text::Edit], anchor: Anchor) -> bool {
    let offset = anchor.offset;
    match anchor.bias {
        Bias::Left => {
            let index = edits.partition_point(|edit| edit.old.end < offset);
            edits.get(index).is_some_and(|edit| edit.old.start < offset)
        }
        Bias::Right => {
            let index = edits.partition_point(|edit| edit.old.end <= offset);
            edits
                .get(index)
                .is_some_and(|edit| edit.old.start <= offset)
        }
    }
}

/// Sorts row ranges and merges those that overlap or touch.
fn merge_ranges(ranges: &mut Vec<Range<u32>>) {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<u32>> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    *ranges = merged;
}

impl BlockMap {
    /// A layer with no blocks over `input`, text rows `row_height` tall (at
    /// least 1/64 px).
    pub fn new(input: impl Into<BlockInput>, row_height: Height) -> Self {
        let input = input.into();
        let transforms = rows_tree(input.row_count());
        Self {
            snapshot: BlockSnapshot {
                input,
                transforms,
                row_height: Height(row_height.0.max(1)),
            },
            blocks: Vec::new(),
            index: HashMap::new(),
            next_id: 0,
        }
    }

    /// The current state.
    pub fn snapshot(&self) -> &BlockSnapshot {
        &self.snapshot
    }

    /// The blocks, shown or not, sorted by their first buffer offset.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Block `id`.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.index.get(&id).map(|&index| &self.blocks[index])
    }

    /// Where block `id` is shown; `None` if it is not (hidden by a
    /// conflict or a fold, or removed).
    pub fn bounds(&self, id: BlockId) -> Option<BlockItem> {
        let block = self.block(id)?;
        let position = tree::resolve(block, &self.snapshot.input).position()?;
        self.snapshot.find_block(id, position)
    }

    /// Changes the height of text rows (at least 1/64 px). O(1).
    pub fn set_row_height(&mut self, row_height: Height) {
        self.snapshot.row_height = Height(row_height.0.max(1));
    }

    /// Inserts blocks, with their anchors clipped to the text. Returns
    /// their ids, in order.
    pub fn insert(&mut self, blocks: impl IntoIterator<Item = BlockProperties>) -> Vec<BlockId> {
        let text = self.snapshot.input.buffer();
        let clip = |anchor: Anchor| Anchor {
            offset: text.clip_offset(anchor.offset.min(text.len()), Bias::Left),
            bias: anchor.bias,
        };
        let mut new = Vec::new();
        for properties in blocks {
            let mut placement = properties.placement;
            placement.map_anchors(clip);
            new.push(Block {
                id: BlockId(self.next_id + new.len() as u64),
                placement,
                height: properties.height,
                when_deleted: properties.when_deleted,
                reveal: properties.reveal,
            });
        }
        self.next_id += new.len() as u64;
        let ids: Vec<BlockId> = new.iter().map(|block| block.id).collect();
        if ids.is_empty() {
            return ids;
        }
        let dirty = if self.is_bulk(new.len(), self.blocks.len() + new.len()) {
            None
        } else {
            let input = &self.snapshot.input;
            Some(
                new.iter()
                    .map(|block| tree::attached_rows(block, input))
                    .collect(),
            )
        };
        self.blocks.extend(new);
        self.sort_blocks();
        self.relayout(dirty);
        ids
    }

    /// Removes blocks. Returns the ids that were in the map, in order.
    pub fn remove(&mut self, ids: impl IntoIterator<Item = BlockId>) -> Vec<BlockId> {
        let mut removed = Vec::new();
        let mut gone = HashSet::new();
        for id in ids {
            if self.index.contains_key(&id) && gone.insert(id) {
                removed.push(id);
            }
        }
        if removed.is_empty() {
            return removed;
        }
        let dirty = if self.is_bulk(removed.len(), self.blocks.len()) {
            None
        } else {
            let input = &self.snapshot.input;
            Some(
                removed
                    .iter()
                    .filter_map(|id| self.block(*id))
                    .map(|block| tree::attached_rows(block, input))
                    .collect(),
            )
        };
        self.blocks.retain(|block| !gone.contains(&block.id));
        self.reindex();
        self.relayout(dirty);
        removed
    }

    /// Whether changing `changed` of `blocks` blocks is cheaper as a
    /// rebuild than in place.
    fn is_bulk(&self, changed: usize, blocks: usize) -> bool {
        changed > REBUILD_REGIONS && changed * 3 > blocks
    }

    /// Lays out the rows `dirty` names again, or everything for `None`.
    fn relayout(&mut self, dirty: Option<Vec<Range<u32>>>) {
        match dirty {
            Some(dirty) => {
                let input = self.snapshot.input.clone();
                self.apply(input, &[], dirty);
            }
            None => self.rebuild(),
        }
    }

    /// Changes the height of blocks, in place (O(log n) each, nothing is
    /// laid out again). Returns the ids whose height changed.
    pub fn resize(&mut self, heights: impl IntoIterator<Item = (BlockId, Height)>) -> Vec<BlockId> {
        let mut resized = Vec::new();
        for (id, height) in heights {
            let Some(&index) = self.index.get(&id) else {
                continue;
            };
            let block = &mut self.blocks[index];
            if block.height == height {
                continue;
            }
            block.height = height;
            resized.push(id);
            if let Some(position) = tree::resolve(block, &self.snapshot.input).position() {
                self.snapshot.transforms =
                    replace_block_height(&self.snapshot.transforms, id, position, height);
            }
        }
        resized
    }

    /// Follows a change of the input. `input` is the new input, `edits` its
    /// row edits (sorted, in the rows of the previous input and of `input`),
    /// and `text` the buffer edit if the text changed: it moves the anchors
    /// (see [`WhenDeleted`]). Returns the blocks removed or displaced.
    pub fn sync(
        &mut self,
        input: impl Into<BlockInput>,
        edits: &[RowEdit],
        text: Option<&EditSummary>,
    ) -> BlockChanges {
        let mut changes = BlockChanges::default();
        let mut dirty = Vec::new();
        if let Some(summary) = text.filter(|summary| !summary.is_empty()) {
            self.move_anchors(summary, &mut changes, &mut dirty);
        }
        let mut edits = edits.to_vec();
        edits.sort_by_key(|edit| edit.old.start);
        self.apply(input.into(), &edits, dirty);
        changes
    }

    /// [`sync`](Self::sync) with the wrap layer's edits.
    pub fn sync_wrap(
        &mut self,
        wrap: WrapSnapshot,
        edits: &[WrapEdit],
        text: Option<&EditSummary>,
    ) -> BlockChanges {
        self.sync(wrap, &input::wrap_row_edits(edits), text)
    }

    /// [`sync`](Self::sync) with the tab layer's edits (soft wrap off).
    pub fn sync_tab(
        &mut self,
        tabs: TabSnapshot,
        edits: &[TabEdit],
        text: Option<&EditSummary>,
    ) -> BlockChanges {
        self.sync(tabs, &input::tab_row_edits(edits), text)
    }

    /// Lays the whole layer out again from its blocks. Incremental updates
    /// give the same result; this is O(blocks · log n).
    pub fn rebuild(&mut self) {
        let mut transforms = SumTree::new(());
        let rows = self.snapshot.input.row_count();
        Plan::new(&self.snapshot.input, &self.blocks).emit(0..rows, &mut transforms);
        self.snapshot.transforms = transforms;
    }

    fn reindex(&mut self) {
        self.index.clear();
        self.index.extend(
            self.blocks
                .iter()
                .enumerate()
                .map(|(index, block)| (block.id, index)),
        );
    }

    fn sort_blocks(&mut self) {
        if !self.blocks.is_sorted_by_key(Block::sort_key) {
            self.blocks.sort_by_key(Block::sort_key);
        }
        self.reindex();
    }

    /// Moves every anchor through `summary`, removing the blocks whose
    /// anchors' text it deleted if they ask for it. The old rows of removed
    /// blocks go to `dirty`.
    fn move_anchors(
        &mut self,
        summary: &EditSummary,
        changes: &mut BlockChanges,
        dirty: &mut Vec<Range<u32>>,
    ) {
        let edits = summary.edits();
        let Some(first) = edits.first().map(|edit| edit.old.start) else {
            return;
        };
        let input = &self.snapshot.input;
        let before = self.blocks.len();
        // Anchors before the first edit stay where they are, and no edit
        // deletes their text: of the blocks that start before it, only
        // replace blocks that reach it need moving.
        let prefix = self
            .blocks
            .partition_point(|block| block.placement.key() < first);
        let mut index = 0;
        self.blocks.retain_mut(|block| {
            index += 1;
            if index <= prefix
                && block
                    .placement
                    .anchors()
                    .all(|anchor| anchor.offset < first)
            {
                return true;
            }
            let deleted = block
                .placement
                .anchors()
                .any(|anchor| is_deleted(edits, anchor));
            if deleted {
                if block.when_deleted == WhenDeleted::Remove {
                    dirty.push(tree::attached_rows(block, input));
                    changes.removed.push((block.id, Removal::AnchorDeleted));
                    return false;
                }
                changes.displaced.push(block.id);
            }
            block
                .placement
                .map_anchors(|anchor| anchor.transform(summary));
            true
        });
        // The keys of the blocks before `prefix` did not change.
        if self.blocks.len() != before
            || !self.blocks[prefix.saturating_sub(1)..].is_sorted_by_key(Block::sort_key)
        {
            self.sort_blocks();
        }
    }

    /// The blocks attached to the display lines `rows` of `input`: a slice
    /// of `blocks`, which are sorted by first offset.
    fn blocks_in(&self, input: &BlockInput, rows: Range<u32>) -> &[Block] {
        let low = if rows.start == 0 {
            0
        } else {
            input.row_start_offset(rows.start)
        };
        let high = if rows.end >= input.row_count() {
            usize::MAX
        } else {
            input.row_start_offset(rows.end)
        };
        let start = self
            .blocks
            .partition_point(|block| block.placement.key() < low);
        let end = self
            .blocks
            .partition_point(|block| block.placement.key() < high);
        &self.blocks[start..end.max(start)]
    }

    /// Replaces the input with `input`, laying out again the display lines
    /// that `edits` change and that `dirty` (rows of the old input) names,
    /// with the blocks attached to them.
    fn apply(&mut self, input: BlockInput, edits: &[RowEdit], dirty: Vec<Range<u32>>) {
        if self.blocks.is_empty() {
            self.snapshot.transforms = rows_tree(input.row_count());
            self.snapshot.input = input;
            return;
        }
        let old = &self.snapshot;
        let old_rows = old.input.row_count();
        let mut regions: Vec<Range<u32>> = edits
            .iter()
            .map(|edit| edit.old.clone())
            .chain(dirty)
            .map(|range| {
                // Whole display lines; an empty range is the line it is at.
                let start = range.start.min(old_rows);
                let end = range.end.min(old_rows).max(start);
                if start == end {
                    old.input.display_line_of_row(start.min(old_rows - 1))
                } else {
                    old.input.display_line_of_row(start).start
                        ..old.input.display_line_of_row(end - 1).end
                }
            })
            .collect();
        if regions.is_empty() {
            debug_assert_eq!(input.row_count(), old_rows, "rows changed without edits");
            self.snapshot.input = input;
            return;
        }
        merge_ranges(&mut regions);
        // Many places at once (blocks inserted in bulk): a rebuild costs
        // less than laying out each one.
        if self.is_bulk(regions.len(), self.blocks.len()) {
            self.snapshot.input = input;
            self.rebuild();
            return;
        }
        for region in &mut regions {
            *region = whole_replace_blocks(&old.transforms, region.clone());
        }
        merge_ranges(&mut regions);

        let mut transforms = SumTree::new(());
        let mut walker = Walker::new(&old.transforms);
        let mut edits = edits.iter().peekable();
        let mut delta: i64 = 0;
        let mut regions = regions.into_iter().peekable();
        let shift = |row: u32, delta: i64| (i64::from(row) + delta) as u32;
        while let Some(mut region) = regions.next() {
            let new_start = shift(region.start, delta);
            let (new_end, plan) = loop {
                while let Some(edit) = edits.next_if(|edit| edit.old.end <= region.end) {
                    delta += i64::from(edit.new.end - edit.new.start)
                        - i64::from(edit.old.end - edit.old.start);
                }
                let new_end = shift(region.end, delta);
                let plan = Plan::new(&input, self.blocks_in(&input, new_start..new_end));
                let Some(reach) = plan.reach().filter(|&reach| reach > new_end) else {
                    break (new_end, plan);
                };
                // Past the region, rows are unchanged up to the next one.
                let mut end = shift(reach, -delta).min(old_rows);
                while let Some(next) = regions.next_if(|next| next.start <= end) {
                    end = end.max(next.end);
                }
                region.end = whole_replace_blocks(&old.transforms, region.start..end).end;
            };
            walker.copy_until(region.start, &mut transforms);
            walker.skip_until(region.end);
            plan.emit(new_start..new_end, &mut transforms);
        }
        walker.finish(&mut transforms);

        let consistent = transforms.summary().input_rows == input.row_count();
        debug_assert!(
            consistent,
            "block layer out of step with its input: {} rows, input {}",
            transforms.summary().input_rows,
            input.row_count()
        );
        self.snapshot.transforms = transforms;
        self.snapshot.input = input;
        if !consistent {
            self.rebuild();
        }
    }
}

/// `rows` grown so that no replace block of `tree` crosses its ends, and
/// with the replace block that ends at its start: the end of a replace
/// range at the start of a line does not take that line, so an edit there
/// can make the block take it.
fn whole_replace_blocks(tree: &SumTree<Transform>, mut rows: Range<u32>) -> Range<u32> {
    let (start, _, item) =
        tree.find::<InputRow, _>((), &InputRow(rows.start), sum_tree::Bias::Left);
    if matches!(item, Some(Transform::Block(block)) if block.replaced > 0) {
        rows.start = rows.start.min(start.0);
    }
    let (start, end, item) =
        tree.find::<InputRow, _>((), &InputRow(rows.end), sum_tree::Bias::Left);
    if matches!(item, Some(Transform::Block(block)) if block.replaced > 0) && start.0 < rows.end {
        rows.end = rows.end.max(end.0);
    }
    rows
}

/// `tree` with block `id` (at input row `position`) `height` tall, if the
/// tree shows it.
fn replace_block_height(
    tree: &SumTree<Transform>,
    id: BlockId,
    position: u32,
    height: Height,
) -> SumTree<Transform> {
    let mut cursor = tree.cursor::<InputRow>(());
    let mut new_tree = cursor.slice(&InputRow(position), sum_tree::Bias::Left);
    while let Some(item) = cursor.item() {
        if cursor.start().0 > position {
            break;
        }
        if let Transform::Block(block) = item
            && block.id == id
        {
            new_tree.push(
                Transform::Block(tree::BlockTransform { height, ..*block }),
                (),
            );
            cursor.next();
            new_tree.append(cursor.suffix(), ());
            return new_tree;
        }
        new_tree.push(item.clone(), ());
        cursor.next();
    }
    // Not shown (merged into another replace block): nothing changes.
    tree.clone()
}

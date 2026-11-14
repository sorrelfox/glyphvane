// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A per-revision index of the raw block structure
//! ([`SyntaxTree::block_index`]).
//!
//! Built on first use with one pass over the events (O(document)) and
//! cached in the tree, so later lookups on the same revision cost
//! O(log n + depth) ([`BlockIndex::path_at`]) or O(1)
//! ([`BlockIndex::get`]). It holds the blocks as opened ([`BlockSpan`]:
//! text blocks are not resolved into paragraphs, headings and definitions;
//! use [`crate::walk_range`] for resolved views of a range without
//! building an index).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use super::event::BlockId;
use super::{BlockSpan, SyntaxTree};

const NO_PARENT: u32 = u32::MAX;

/// Every block of one revision, in document order, with parent links and
/// lookup by [`BlockId`].
#[derive(Debug, Default)]
pub struct BlockIndex {
    /// Pre-order (sorted by start; a parent comes before its children).
    spans: Vec<BlockSpan>,
    parents: Vec<u32>,
    by_id: HashMap<BlockId, u32>,
}

impl BlockIndex {
    fn build(tree: &SyntaxTree) -> Self {
        let spans = tree.blocks();
        let mut parents = vec![NO_PARENT; spans.len()];
        // The open ancestors of the current block, by depth.
        let mut path: Vec<u32> = Vec::new();
        for (i, s) in spans.iter().enumerate() {
            path.truncate(s.depth as usize);
            if let Some(&p) = path.last() {
                parents[i] = p;
            }
            path.push(u32::try_from(i).unwrap_or(NO_PARENT));
        }
        let by_id = spans
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, u32::try_from(i).unwrap_or(NO_PARENT)))
            .collect();
        BlockIndex {
            spans,
            parents,
            by_id,
        }
    }

    /// Number of blocks.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether the document has no blocks.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Every block, in document order (pre-order).
    pub fn spans(&self) -> &[BlockSpan] {
        &self.spans
    }

    /// The block with this id.
    pub fn get(&self, id: BlockId) -> Option<&BlockSpan> {
        self.by_id.get(&id).map(|&i| &self.spans[i as usize])
    }

    /// The block that contains the block with this id.
    pub fn parent(&self, id: BlockId) -> Option<&BlockSpan> {
        let i = *self.by_id.get(&id)?;
        let p = self.parents[i as usize];
        (p != NO_PARENT).then(|| &self.spans[p as usize])
    }

    /// The blocks that contain the byte at `offset`, outermost first.
    pub fn path_at(&self, offset: usize) -> Vec<&BlockSpan> {
        let contains = |i: u32| {
            let r = &self.spans[i as usize].range;
            r.start <= offset && offset < r.end
        };
        let k = self.spans.partition_point(|s| s.range.start <= offset);
        let mut i = match k.checked_sub(1) {
            Some(i) => u32::try_from(i).unwrap_or(NO_PARENT),
            None => return Vec::new(),
        };
        // The deepest block containing `offset` is the last block that
        // starts at or before it, or one of its ancestors.
        while i != NO_PARENT && !contains(i) {
            i = self.parents[i as usize];
        }
        let mut out = Vec::new();
        while i != NO_PARENT {
            out.push(&self.spans[i as usize]);
            i = self.parents[i as usize];
        }
        out.reverse();
        out
    }

    /// The blocks that share a byte with `range` (an empty range at `p`
    /// stands for the byte at `p`), in document order.
    pub fn intersecting(&self, range: Range<usize>) -> Vec<&BlockSpan> {
        let start = range.start;
        let end = range.end.max(start + 1);
        let mut out = self.path_at(start);
        let k = self.spans.partition_point(|s| s.range.start < start);
        for s in &self.spans[k..] {
            if s.range.start >= end {
                break;
            }
            let contains_start = s.range.start <= start && start < s.range.end;
            if !contains_start && s.range.end > start {
                out.push(s);
            }
        }
        out
    }
}

impl SyntaxTree {
    /// The block index of this revision, built on first use and cached
    /// (see [`BlockIndex`]).
    pub fn block_index(&self) -> Arc<BlockIndex> {
        Arc::clone(
            self.0
                .block_index
                .get_or_init(|| Arc::new(BlockIndex::build(self))),
        )
    }
}

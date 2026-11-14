// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Range queries over the line sum tree: the blocks open at a line, where
//! open blocks close, and whether a list is tight, without walking the
//! document.
//!
//! Each query searches the chunk summaries (`min_depth`, `min_loosen`; see
//! [`super::chunk`]) and then scans the events of at most a few chunks:
//!
//! - The block at depth `d` that is open at the start of line `L` opened
//!   at the last `Open` event at depth `d` before `L`; that event is on the
//!   last line before `L` whose depth drops to `d` or less.
//! - It closes at the first point after that where the depth drops to `d`.
//! - A list at depth `d` is loose iff a `Loosen` marker for depth `d` lies
//!   between its `Open` and its `Close`. Inside a list every `Loosen`
//!   marker is for that list or a deeper one, so the first marker for
//!   depth `d` or less after the `Open`, if it comes before the `Close`,
//!   is the list's own.
//!
//! Every query costs O(log n) summary steps plus the events of the chunks
//! it lands in (64 lines each), per depth level asked for.

use sum_tree::{Bias, Dimensions};

use super::TreeData;
use super::chunk::{Bytes, Chunk, ChunkSummary, Lines};
use super::event::{BlockId, BlockKind};

type Dims = Dimensions<Lines, Bytes>;

/// An `Open` event located in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OpenAt {
    /// Depth of the block (0 for children of the document).
    pub depth: u32,
    /// Line of the event.
    pub line: usize,
    /// Byte offset of that line.
    pub line_start: usize,
    /// Index of the event among the line's events.
    pub event: usize,
    /// Index of the block's id among the line's ids.
    pub id_index: usize,
    /// Byte offset of the event (where the block starts).
    pub offset: usize,
    /// The block's kind as opened.
    pub kind: BlockKind,
    /// The block's id.
    pub id: BlockId,
}

/// Byte offsets of the first `upto` lines of a chunk.
fn line_starts(chunk: &Chunk, first_byte: usize, upto: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(upto);
    let mut b = first_byte;
    for rec in &chunk.0.lines[..upto] {
        out.push(b);
        b += rec.len as usize;
    }
    out
}

/// The blocks open at the start of line `line` (the ones its checkpoint
/// describes), outermost first.
pub(crate) fn open_blocks_at_line(t: &TreeData, line: usize) -> Vec<OpenAt> {
    let mut cursor = t.chunks.cursor::<Dims>(());
    cursor.seek(&Lines(line), Bias::Right);
    let Some(chunk) = cursor.item() else {
        return Vec::new();
    };
    let start = *cursor.start();
    let depths = chunk.0.line_depths();
    let local = line - start.0.0;
    let Some(&(c0, ..)) = depths.get(local) else {
        return Vec::new();
    };
    let mut out: Vec<Option<OpenAt>> = vec![None; c0 as usize];
    let mut need = i64::from(c0) - 1;
    scan_back(
        chunk, start.0.0, start.1.0, &depths, local, &mut need, &mut out,
    );
    while need >= 0 {
        let n = need as u32;
        cursor.search_backward(|s: &ChunkSummary| s.min_depth <= n);
        let Some(chunk) = cursor.item() else {
            break;
        };
        let start = *cursor.start();
        let depths = chunk.0.line_depths();
        scan_back(
            chunk,
            start.0.0,
            start.1.0,
            &depths,
            chunk.0.lines.len(),
            &mut need,
            &mut out,
        );
    }
    out.into_iter().flatten().collect()
}

/// Scans lines `upto - 1` down to 0 of a chunk for the `Open` events of the
/// blocks at depths `..=need` (see [`open_blocks_at_line`]).
fn scan_back(
    chunk: &Chunk,
    first_line: usize,
    first_byte: usize,
    depths: &[(u32, u32, u32)],
    upto: usize,
    need: &mut i64,
    out: &mut [Option<OpenAt>],
) {
    let starts = line_starts(chunk, first_byte, upto);
    for j in (0..upto).rev() {
        if *need < 0 {
            return;
        }
        let (line_depth, min_depth, _) = depths[j];
        if i64::from(min_depth) > *need {
            continue;
        }
        let hi = *need as u32;
        let (events, ids) = chunk.0.line_events(j);
        let mut depth = line_depth;
        let mut off = starts[j];
        let mut id_index = 0;
        for (k, e) in events.iter().enumerate() {
            if let Some(kind) = e.open_kind() {
                if depth >= min_depth
                    && depth <= hi
                    && let Some(slot) = out.get_mut(depth as usize)
                {
                    *slot = Some(OpenAt {
                        depth,
                        line: first_line + j,
                        line_start: starts[j],
                        event: k,
                        id_index,
                        offset: off,
                        kind,
                        id: ids.get(id_index).copied().unwrap_or(BlockId(0)),
                    });
                }
                id_index += 1;
                depth += 1;
            } else if e.is_close() {
                depth = depth.saturating_sub(1);
            } else {
                off += e.tok_len();
            }
        }
        *need = i64::from(min_depth) - 1;
    }
}

/// Where the blocks open at a point close: the point is event `event` of
/// line `line`, at byte `offset`, with `depth` blocks open. Returns the
/// close offset of the block at each depth `0..depth`.
pub(crate) fn closes_from(
    t: &TreeData,
    line: usize,
    event: usize,
    offset: usize,
    depth: u32,
) -> Vec<usize> {
    let mut out = vec![t.len; depth as usize];
    let mut need = i64::from(depth) - 1;
    if need < 0 {
        return out;
    }
    let mut cursor = t.chunks.cursor::<Dims>(());
    cursor.seek(&Lines(line), Bias::Right);
    let Some(chunk) = cursor.item() else {
        return out;
    };
    let start = *cursor.start();
    let local = line - start.0.0;
    let depths = chunk.0.line_depths();
    let mut scan = |chunk: &Chunk,
                    j: usize,
                    from_event: usize,
                    mut depth: u32,
                    mut off: usize,
                    need: &mut i64|
     -> bool {
        let (events, _) = chunk.0.line_events(j);
        for e in &events[from_event.min(events.len())..] {
            if e.is_open() {
                depth += 1;
            } else if e.is_close() {
                depth = depth.saturating_sub(1);
                if i64::from(depth) <= *need {
                    if let Some(slot) = out.get_mut(depth as usize) {
                        *slot = off;
                    }
                    *need = i64::from(depth) - 1;
                    if *need < 0 {
                        return true;
                    }
                }
            } else {
                off += e.tok_len();
            }
        }
        false
    };
    if scan(chunk, local, event, depth, offset, &mut need) {
        return out;
    }
    let len_of = |chunk: &Chunk, j: usize| chunk.0.lines[j].len as usize;
    let mut off = offset_of_line_end(chunk, start.1.0, local);
    for (j, &(line_depth, min_depth, _)) in depths.iter().enumerate().skip(local + 1) {
        if i64::from(min_depth) <= need && scan(chunk, j, 0, line_depth, off, &mut need) {
            return out;
        }
        off += len_of(chunk, j);
    }
    while need >= 0 {
        let n = need as u32;
        cursor.search_forward(|s: &ChunkSummary| s.min_depth <= n);
        let Some(chunk) = cursor.item() else {
            break;
        };
        let start = *cursor.start();
        let depths = chunk.0.line_depths();
        let mut off = start.1.0;
        for (j, &(line_depth, min_depth, _)) in depths.iter().enumerate() {
            if i64::from(min_depth) <= need && scan(chunk, j, 0, line_depth, off, &mut need) {
                return out;
            }
            off += len_of(chunk, j);
        }
    }
    out
}

/// Byte offset of the end of line `local` of a chunk that starts at
/// `first_byte`.
fn offset_of_line_end(chunk: &Chunk, first_byte: usize, local: usize) -> usize {
    first_byte
        + chunk.0.lines[..=local]
            .iter()
            .map(|l| l.len as usize)
            .sum::<usize>()
}

/// Whether the list opened at `open` is tight.
pub(crate) fn list_is_tight(t: &TreeData, open: &OpenAt) -> bool {
    let d = open.depth;
    let mut cursor = t.chunks.cursor::<Lines>(());
    cursor.seek(&Lines(open.line), Bias::Right);
    let Some(chunk) = cursor.item() else {
        return true;
    };
    let local = open.line - cursor.start().0;
    let depths = chunk.0.line_depths();
    // `Some(tight)` once decided.
    let scan = |chunk: &Chunk, j: usize, from_event: usize, mut depth: u32| -> Option<bool> {
        let (events, _) = chunk.0.line_events(j);
        for e in &events[from_event.min(events.len())..] {
            if e.is_open() {
                depth += 1;
            } else if e.is_close() {
                depth = depth.saturating_sub(1);
                if depth <= d {
                    return Some(true);
                }
            } else if let Some(up) = e.loosen_up()
                && depth.saturating_sub(1 + up) == d
            {
                return Some(false);
            }
        }
        None
    };
    if let Some(tight) = scan(chunk, local, open.event + 1, d + 1) {
        return tight;
    }
    let relevant = |min_depth: u32, min_loosen: u32| min_depth <= d || min_loosen <= d;
    for (j, &(line_depth, min_depth, min_loosen)) in depths.iter().enumerate().skip(local + 1) {
        if relevant(min_depth, min_loosen)
            && let Some(tight) = scan(chunk, j, 0, line_depth)
        {
            return tight;
        }
    }
    loop {
        cursor.search_forward(|s: &ChunkSummary| relevant(s.min_depth, s.min_loosen));
        let Some(chunk) = cursor.item() else {
            return true;
        };
        for (j, &(line_depth, min_depth, min_loosen)) in chunk.0.line_depths().iter().enumerate() {
            if relevant(min_depth, min_loosen)
                && let Some(tight) = scan(chunk, j, 0, line_depth)
            {
                return tight;
            }
        }
    }
}

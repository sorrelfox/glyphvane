// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Line records grouped into chunks, the items of the sum tree.
//!
//! A line record holds the line's byte length, its checkpoint and the end
//! of its events and BlockIds in the chunk's arrays. Nothing stores an
//! absolute offset: positions come from the sum-tree summaries, so an edit
//! never touches the chunks outside the reparsed window.
//!
//! The last line of every document is a zero-length pseudo-line at the end
//! of the text; its events close the blocks still open at the end.
//!
//! Besides sizes, a chunk's summary holds two depth minima that range
//! queries search on (see [`crate::tree::range`]). Depths are absolute (the
//! number of blocks open below the document), so they do not depend on
//! what comes before the chunk and survive splices unchanged:
//!
//! - `min_depth`: the smallest depth at any point of the chunk (line starts
//!   and after every event). A chunk with `min_depth > d` cannot hold the
//!   `Open` or `Close` of a block at depth `d` or less.
//! - `min_loosen`: the smallest depth of a list loosened by a `Loosen`
//!   marker in the chunk.

use std::sync::Arc;

use sum_tree::{ContextLessSummary, Dimension, Item};

use crate::block::state::StateId;
use crate::tree::event::{BlockId, Ev, Sink};

/// Target number of lines per chunk.
pub(crate) const CHUNK_LINES: usize = 64;

/// One line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineRec {
    /// Length in bytes, terminator included.
    pub len: u32,
    /// Checkpoint: the parser state at the line start.
    pub state: StateId,
    /// End of the line's events in the chunk's event array.
    pub ev_end: u32,
    /// End of the line's BlockIds in the chunk's id array.
    pub id_end: u32,
}

/// The data of one chunk.
#[derive(Debug, Default)]
pub(crate) struct ChunkData {
    pub lines: Vec<LineRec>,
    pub events: Vec<Ev>,
    pub ids: Vec<BlockId>,
    pub bytes: usize,
    pub refdefs: usize,
    /// Depth at the start of the first line.
    pub start_depth: u32,
    /// See the module documentation.
    pub min_depth: u32,
    /// See the module documentation (`u32::MAX`: no marker).
    pub min_loosen: u32,
}

/// Depth bookkeeping over a run of events (see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DepthScan {
    /// Current depth.
    pub depth: u32,
    pub min_depth: u32,
    pub min_loosen: u32,
    pub refdefs: usize,
}

impl DepthScan {
    pub(crate) fn new(depth: u32) -> Self {
        DepthScan {
            depth,
            min_depth: depth,
            min_loosen: u32::MAX,
            refdefs: 0,
        }
    }

    #[inline]
    pub(crate) fn event(&mut self, e: Ev) {
        if e.is_token() {
            return;
        }
        if e.is_close() {
            self.depth = self.depth.saturating_sub(1);
            self.min_depth = self.min_depth.min(self.depth);
        } else if e.is_open() {
            self.depth += 1;
            // A footnote definition defines a label, like a reference
            // definition.
            if e.is_footnote_open() {
                self.refdefs += 1;
            }
        } else if e.is_refdef() {
            self.refdefs += 1;
        } else if let Some(up) = e.loosen_up() {
            let list = self.depth.saturating_sub(1 + up);
            self.min_loosen = self.min_loosen.min(list);
        }
    }

    /// [`DepthScan::event`] over a run of events.
    #[inline]
    pub(crate) fn events(&mut self, events: &[Ev]) {
        for &e in events {
            self.event(e);
        }
    }
}

impl ChunkData {
    /// Events and BlockIds of line `i`.
    #[inline]
    pub(crate) fn line_events(&self, i: usize) -> (&[Ev], &[BlockId]) {
        let (es, is) = if i == 0 {
            (0, 0)
        } else {
            let p = self.lines[i - 1];
            (p.ev_end as usize, p.id_end as usize)
        };
        let l = self.lines[i];
        (
            &self.events[es..l.ev_end as usize],
            &self.ids[is..l.id_end as usize],
        )
    }

    /// Per line: (depth at its start, smallest depth during it, smallest
    /// loosened list depth in it).
    pub(crate) fn line_depths(&self) -> Vec<(u32, u32, u32)> {
        let mut out = Vec::with_capacity(self.lines.len());
        let mut depth = self.start_depth;
        for i in 0..self.lines.len() {
            let mut scan = DepthScan::new(depth);
            scan.events(self.line_events(i).0);
            out.push((depth, scan.min_depth, scan.min_loosen));
            depth = scan.depth;
        }
        out
    }

    /// Recomputes the depth fields from the events (`start_depth` given);
    /// returns the depth at the end.
    fn scan(&self, start_depth: u32) -> DepthScan {
        let mut scan = DepthScan::new(start_depth);
        scan.events(&self.events);
        scan
    }

    /// Checks the stored summary fields against the events; returns the
    /// depth at the end of the chunk.
    pub(crate) fn check(&self) -> Result<u32, String> {
        let scan = self.scan(self.start_depth);
        if (scan.refdefs, scan.min_depth, scan.min_loosen)
            != (self.refdefs, self.min_depth, self.min_loosen)
        {
            return Err(format!(
                "chunk summary out of sync: stored (refdefs, min depth, min loosen) {:?}, events give {:?}",
                (self.refdefs, self.min_depth, self.min_loosen),
                (scan.refdefs, scan.min_depth, scan.min_loosen)
            ));
        }
        Ok(scan.depth)
    }
}

/// A chunk of consecutive lines; cloning is O(1).
#[derive(Clone, Debug)]
pub(crate) struct Chunk(pub Arc<ChunkData>);

/// Summary of a run of chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChunkSummary {
    pub bytes: usize,
    pub lines: usize,
    pub refdefs: usize,
    /// Smallest depth anywhere in the run.
    pub min_depth: u32,
    /// Smallest loosened list depth in the run.
    pub min_loosen: u32,
}

impl ContextLessSummary for ChunkSummary {
    fn zero() -> Self {
        ChunkSummary {
            bytes: 0,
            lines: 0,
            refdefs: 0,
            min_depth: u32::MAX,
            min_loosen: u32::MAX,
        }
    }

    fn add_summary(&mut self, s: &Self) {
        self.bytes += s.bytes;
        self.lines += s.lines;
        self.refdefs += s.refdefs;
        self.min_depth = self.min_depth.min(s.min_depth);
        self.min_loosen = self.min_loosen.min(s.min_loosen);
    }
}

impl Item for Chunk {
    type Summary = ChunkSummary;

    fn summary(&self, _cx: ()) -> ChunkSummary {
        ChunkSummary {
            bytes: self.0.bytes,
            lines: self.0.lines.len(),
            refdefs: self.0.refdefs,
            min_depth: self.0.min_depth,
            min_loosen: self.0.min_loosen,
        }
    }
}

/// Byte offset dimension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Bytes(pub usize);

impl Dimension<'_, ChunkSummary> for Bytes {
    fn zero(_cx: ()) -> Self {
        Bytes(0)
    }

    fn add_summary(&mut self, s: &ChunkSummary, _cx: ()) {
        self.0 += s.bytes;
    }
}

/// Line index dimension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Lines(pub usize);

impl Dimension<'_, ChunkSummary> for Lines {
    fn zero(_cx: ()) -> Self {
        Lines(0)
    }

    fn add_summary(&mut self, s: &ChunkSummary, _cx: ()) {
        self.0 += s.lines;
    }
}

/// Accumulates lines and their events into chunks. The block pass writes
/// into [`ChunkBuilder::sink`]; [`ChunkBuilder::end_line`] closes a line.
///
/// `depth` is the depth at the start of the first line it receives.
#[derive(Debug)]
pub(crate) struct ChunkBuilder {
    pub chunks: Vec<Chunk>,
    lines: Vec<LineRec>,
    bytes: usize,
    /// Depth at the start of the pending chunk.
    depth: u32,
    pub sink: Sink,
}

impl ChunkBuilder {
    pub(crate) fn new(next_id: u64, depth: u32) -> Self {
        ChunkBuilder {
            chunks: Vec::new(),
            lines: Vec::new(),
            bytes: 0,
            depth,
            sink: Sink {
                events: Vec::new(),
                ids: Vec::new(),
                next_id,
            },
        }
    }

    /// Ends the current line: its events are those written to the sink
    /// since the previous line.
    #[inline]
    pub(crate) fn end_line(&mut self, len: usize, state: StateId) {
        self.lines.push(LineRec {
            len: u32::try_from(len).unwrap_or(u32::MAX),
            state,
            ev_end: u32::try_from(self.sink.events.len()).unwrap_or(u32::MAX),
            id_end: u32::try_from(self.sink.ids.len()).unwrap_or(u32::MAX),
        });
        self.bytes += len;
        if self.lines.len() >= CHUNK_LINES {
            self.flush();
        }
    }

    /// Copies a finished line.
    pub(crate) fn push_line(&mut self, rec: LineRec, events: &[Ev], ids: &[BlockId]) {
        self.sink.events.extend_from_slice(events);
        self.sink.ids.extend_from_slice(ids);
        self.end_line(rec.len as usize, rec.state);
    }

    /// Lines in the chunk being built.
    pub(crate) fn pending_lines(&self) -> usize {
        self.lines.len()
    }

    fn flush(&mut self) {
        if self.lines.is_empty() {
            return;
        }
        let events = std::mem::take(&mut self.sink.events);
        let mut scan = DepthScan::new(self.depth);
        scan.events(&events);
        let data = ChunkData {
            lines: std::mem::take(&mut self.lines),
            events,
            ids: std::mem::take(&mut self.sink.ids),
            bytes: std::mem::take(&mut self.bytes),
            refdefs: scan.refdefs,
            start_depth: self.depth,
            min_depth: scan.min_depth,
            min_loosen: scan.min_loosen,
        };
        self.depth = scan.depth;
        self.chunks.push(Chunk(Arc::new(data)));
    }

    /// The finished chunks and the id counter.
    pub(crate) fn finish(mut self) -> (Vec<Chunk>, u64) {
        self.flush();
        (self.chunks, self.sink.next_id)
    }
}

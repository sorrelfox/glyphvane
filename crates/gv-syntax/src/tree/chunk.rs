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
}

/// A chunk of consecutive lines; cloning is O(1).
#[derive(Clone, Debug)]
pub(crate) struct Chunk(pub Arc<ChunkData>);

/// Summary of a run of chunks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChunkSummary {
    pub bytes: usize,
    pub lines: usize,
    pub refdefs: usize,
}

impl ContextLessSummary for ChunkSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, s: &Self) {
        self.bytes += s.bytes;
        self.lines += s.lines;
        self.refdefs += s.refdefs;
    }
}

impl Item for Chunk {
    type Summary = ChunkSummary;

    fn summary(&self, _cx: ()) -> ChunkSummary {
        ChunkSummary {
            bytes: self.0.bytes,
            lines: self.0.lines.len(),
            refdefs: self.0.refdefs,
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
#[derive(Debug)]
pub(crate) struct ChunkBuilder {
    pub chunks: Vec<Chunk>,
    lines: Vec<LineRec>,
    bytes: usize,
    pub sink: Sink,
}

impl ChunkBuilder {
    pub(crate) fn new(next_id: u64) -> Self {
        ChunkBuilder {
            chunks: Vec::new(),
            lines: Vec::new(),
            bytes: 0,
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
        let refdefs = events.iter().filter(|e| e.is_refdef()).count();
        let data = ChunkData {
            lines: std::mem::take(&mut self.lines),
            events,
            ids: std::mem::take(&mut self.sink.ids),
            bytes: std::mem::take(&mut self.bytes),
            refdefs,
        };
        self.chunks.push(Chunk(Arc::new(data)));
    }

    /// The finished chunks and the id counter.
    pub(crate) fn finish(mut self) -> (Vec<Chunk>, u64) {
        self.flush();
        (self.chunks, self.sink.next_id)
    }
}

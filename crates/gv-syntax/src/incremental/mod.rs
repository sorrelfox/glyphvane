// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Incremental reparsing.
//!
//! 1. **Restart** at the line that holds the byte just before the edit: its
//!    start is a line start in the new text too, and everything before it
//!    is unchanged. The parser is restored from that line's checkpoint.
//! 2. **Re-run the block pass.** At each new line start `p` at or after
//!    the end of the inserted text, find the old line that starts at
//!    `p - delta`; stop as soon as its checkpoint equals the new state.
//!    Neither line may be the first line of its text (byte-0 rules such as
//!    the BOM), and states with a long pending definition candidate never
//!    match. Reaching the end of the text without a match is the fallback.
//! 3. **Match BlockIds** of blocks opened inside the window: anchors on
//!    (start position mapped through the edit, depth, kind), then in-order
//!    pairing by (depth, kind) between anchors. Blocks opened before the
//!    window keep their ids by construction.
//! 4. **Splice** the new lines into the sum tree: only the chunks that
//!    overlap the window are rebuilt.
//!
//! The pass can run in deadline slices ([`Budget`]); while it is paused,
//! [`ReparseTask::interim`] gives a tree for display: the new parse up to
//! where it stopped, then the old structure shifted into place.
//!
//! # Front matter
//!
//! Whether line 0 opens front matter depends on a closing line further
//! down (see [`crate::FrontMatterOptions`]), so with front matter enabled
//! ([`crate::ParseOptions`]) the reparse restarts at line 0 when an edit
//! can change that decision without touching line 0:
//!
//! - the restart line is inside front matter and no closing line follows
//!   in the new text (the edit removed the closing line);
//! - line 0 is an opening line that has no closing line (it parsed as
//!   Markdown) and the reparse meets a line that would close it.
//!
//! As a safety net, reaching the end of the text with front matter still
//! open also restarts at line 0. Line 0 is never a convergence point, and
//! the front matter's mode is part of every checkpoint inside it, so
//! convergence elsewhere stays exact.
//!
//! # Composing changes
//!
//! [`Changes::compose`] merges the reports of consecutive reparses into
//! one, for consumers that skip revisions.

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use gv_text::{EditSummary, TextSource};
use sum_tree::Bias;

use crate::block::front_matter;
use crate::block::state::{Interner, Kind, RefMode, StateId};
use crate::block::{BlockOptions, BlockParser};
use crate::tree::chunk::{Bytes, CHUNK_LINES, ChunkBuilder, LineRec, Lines};
use crate::tree::event::{BlockId, BlockKind, Ev, FrontMatterKind, Sink, TokenKind};
use crate::tree::{LineCursor, SyntaxTree, TreeData};

/// One replacement, in the coordinates of the old text: bytes
/// `start..old_end` were replaced by `new_len` bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Change {
    /// Start of the replaced range.
    pub start: usize,
    /// End of the replaced range (old text).
    pub old_end: usize,
    /// Length of the inserted text.
    pub new_len: usize,
}

impl Change {
    /// The single change covering all `edits` of a transaction (sorted,
    /// non-overlapping, as in [`EditSummary::edits`]); `None` if there are
    /// none. Unchanged text between the edits is treated as replaced, which
    /// only widens the reparse.
    pub fn covering(edits: &[gv_text::Edit]) -> Option<Change> {
        let first = edits.first()?;
        let last = edits.last()?;
        Some(Change {
            start: first.old.start,
            old_end: last.old.end,
            new_len: last.new.end - first.new.start,
        })
    }

    fn delta(self) -> isize {
        self.new_len as isize - (self.old_end - self.start) as isize
    }
}

/// How long a reparse may run before it pauses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Budget {
    /// Run to completion.
    Unlimited,
    /// Pause once this instant has passed (checked every 32 lines).
    Until(Instant),
}

impl Budget {
    /// A budget of `d` from now.
    pub fn from_now(d: std::time::Duration) -> Budget {
        Budget::Until(Instant::now() + d)
    }
}

/// What a reparse changed, and what it cost.
///
/// Outside the window, the two trees are the same: old lines before
/// `restart_line` are the new lines before it (same bytes, events and
/// BlockIds), and old lines from `old_end_line` on are the new lines from
/// `new_end_line` on, shifted by the byte and line differences.
///
/// `Changes::default()` (an empty window, see [`Changes::is_empty`])
/// means nothing changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// First reparsed line (the same index in the old and the new tree).
    pub restart_line: usize,
    /// Old lines `restart_line..old_end_line` were replaced...
    pub old_end_line: usize,
    /// ...by new lines `restart_line..new_end_line`.
    pub new_end_line: usize,
    /// Byte range of the replaced lines in the old text. It starts where
    /// `new_range` starts.
    pub old_range: Range<usize>,
    /// Byte range of the new lines in the new text.
    pub new_range: Range<usize>,
    /// Whether the block pass stopped on a checkpoint match (false: it ran
    /// to the end of the text).
    pub converged: bool,
    /// Lines the block pass scanned.
    pub lines_parsed: usize,
    /// Bytes the block pass scanned.
    pub bytes_parsed: usize,
    /// Blocks opened in the window that kept an old BlockId.
    pub ids_kept: usize,
    /// Blocks opened in the window that got a new BlockId.
    pub ids_new: usize,
}

impl Changes {
    /// Whether no line was replaced (the report of a reparse without
    /// edits).
    pub fn is_empty(&self) -> bool {
        self.old_end_line == self.restart_line && self.new_end_line == self.restart_line
    }

    /// The changes of this reparse followed by `next`, the reparse of the
    /// tree this one produced, as one report from this reparse's old tree
    /// to `next`'s new tree. The window is the smallest one that covers
    /// both (lines between two disjoint windows count as replaced). The
    /// statistics are summed; `converged` holds if both converged.
    ///
    /// Composition is associative, and an empty report is its identity.
    #[must_use]
    pub fn compose(&self, next: &Changes) -> Changes {
        if self.is_empty() {
            return next.clone();
        }
        if next.is_empty() {
            return self.clone();
        }
        let (a, b) = (self, next);
        // The covering window in the middle tree, mapped back through `a`
        // and forward through `b`; both of its ends lie outside the other
        // reparse's window or at its edge.
        let mid_end_line = a.new_end_line.max(b.old_end_line);
        let mid_end = a.new_range.end.max(b.old_range.end);
        let start = a.new_range.start.min(b.old_range.start);
        Changes {
            restart_line: a.restart_line.min(b.restart_line),
            old_end_line: mid_end_line - a.new_end_line + a.old_end_line,
            new_end_line: mid_end_line - b.old_end_line + b.new_end_line,
            old_range: start..mid_end - a.new_range.end + a.old_range.end,
            new_range: start..mid_end - b.old_range.end + b.new_range.end,
            converged: a.converged && b.converged,
            lines_parsed: a.lines_parsed + b.lines_parsed,
            bytes_parsed: a.bytes_parsed + b.bytes_parsed,
            ids_kept: a.ids_kept + b.ids_kept,
            ids_new: a.ids_new + b.ids_new,
        }
    }

    /// [`Changes::compose`] over a sequence of consecutive reports, in
    /// order. Empty for an empty sequence.
    pub fn compose_all<'a>(changes: impl IntoIterator<Item = &'a Changes>) -> Changes {
        changes
            .into_iter()
            .fold(Changes::default(), |acc, c| acc.compose(c))
    }
}

/// The result of [`SyntaxTree::reparse`].
#[derive(Debug)]
pub enum Reparse {
    /// The reparse finished within the budget.
    Done {
        /// The tree of the new text.
        tree: SyntaxTree,
        /// What changed.
        changes: Changes,
    },
    /// The budget ran out. Show `interim` and continue `task` (for example
    /// on a background thread) with [`ReparseTask::run`], then
    /// [`ReparseTask::finish`].
    Partial {
        /// A display-only tree of the new text (see
        /// [`SyntaxTree::is_interim`]).
        interim: SyntaxTree,
        /// The paused reparse.
        task: Box<ReparseTask>,
    },
}

/// A paused or running incremental reparse. It owns everything it needs
/// (the old tree is an `Arc` snapshot), so it can move to another thread.
#[derive(Debug)]
pub struct ReparseTask {
    base: SyntaxTree,
    change: Change,
    new_edit_end: usize,
    delta: isize,
    restart: LineCursor,
    parser: BlockParser,
    interner: Interner,
    lines: Vec<LineRec>,
    sink: Sink,
    pos: usize,
    old: LineCursor,
    old_valid: bool,
    /// The old line where the window ends, once done.
    done: Option<usize>,
    /// The first line start past the edit that is also an old line start:
    /// (window lines before it, the old line, the new state there). The
    /// interim tree switches from the new parse to the old lines here.
    seam: Option<(usize, LineCursor, StateId)>,
    /// Line 0 would open front matter of this kind but has no closing line:
    /// a closing line met in the window restarts the pass at line 0.
    watch: Option<FrontMatterKind>,
    /// The pass was restarted at line 0 for front matter.
    restarted: bool,
    scratch: Vec<u8>,
    changes: Changes,
}

impl SyntaxTree {
    /// Reparses after a transaction: `text` is the new text, `edits` the
    /// transaction's [`EditSummary`]. Within `budget` the result is
    /// [`Reparse::Done`]; otherwise [`Reparse::Partial`].
    pub fn reparse<S: TextSource + ?Sized>(
        &self,
        text: &S,
        edits: &EditSummary,
        budget: Budget,
    ) -> Reparse {
        match Change::covering(edits.edits()) {
            Some(change) => self.reparse_change(text, change, budget),
            None => Reparse::Done {
                tree: self.clone(),
                changes: Changes::default(),
            },
        }
    }

    /// Reparses after one replacement (see [`Change`]).
    pub fn reparse_change<S: TextSource + ?Sized>(
        &self,
        text: &S,
        change: Change,
        budget: Budget,
    ) -> Reparse {
        let mut task = self.begin_reparse(text, change);
        if task.run(text, budget) {
            let (tree, changes) = task.finish();
            Reparse::Done { tree, changes }
        } else {
            let interim = task.interim(text);
            Reparse::Partial {
                interim,
                task: Box::new(task),
            }
        }
    }

    /// Reparses after one replacement, to completion.
    pub fn reparse_full<S: TextSource + ?Sized>(
        &self,
        text: &S,
        change: Change,
    ) -> (SyntaxTree, Changes) {
        let mut task = self.begin_reparse(text, change);
        task.run(text, Budget::Unlimited);
        task.finish()
    }

    /// Prepares an incremental reparse: finds the restart line and restores
    /// the parser. An interim tree is not a valid base: the task then
    /// parses the whole text.
    ///
    /// # Panics
    ///
    /// Panics if `change` does not fit the old text.
    pub fn begin_reparse<S: TextSource + ?Sized>(&self, text: &S, change: Change) -> ReparseTask {
        assert!(
            change.start <= change.old_end && change.old_end <= self.0.len,
            "change {change:?} out of range (old length {})",
            self.0.len
        );
        let base = &self.0;
        let change = if base.interim {
            Change {
                start: 0,
                old_end: base.len,
                new_len: (base.len as isize + change.delta()) as usize,
            }
        } else {
            change
        };
        let mut restart = if change.start == 0 {
            base.cursor_at_line(0)
        } else {
            base.cursor_at_byte(change.start - 1)
        };
        let mut watch = None;
        let fm = base.options.front_matter;
        if fm.any() && restart.global > 0 {
            let mut scratch = Vec::new();
            let state = base
                .line(&restart)
                .map_or(crate::block::state::ROOT, |l| l.state);
            if let Kind::FrontMatter { kind } = base.interner.top(state).kind {
                // Inside front matter: it survives only if a closing line
                // still follows.
                if !front_matter::closer_from(text, restart.start, kind, &mut scratch) {
                    restart = base.cursor_at_line(0);
                }
            } else {
                // Line 0 is unchanged. If it would open front matter that has
                // no closing line, watch for one.
                let opens = base.line(&base.cursor_at_line(0)).is_some_and(|l| {
                    l.events
                        .iter()
                        .any(|e| matches!(e.open_kind(), Some(BlockKind::FrontMatter { .. })))
                });
                if !opens {
                    watch = front_matter::opener_at_start(text, fm, &mut scratch);
                }
            }
        }
        let state = base
            .line(&restart)
            .map_or(crate::block::state::ROOT, |l| l.state);
        let mut parser =
            BlockParser::from_checkpoint(&base.interner, state, BlockOptions::of(base.options));
        if parser.needs_pending_text() {
            parser.set_pending_text(pending_text_before(base, &restart, text));
        }
        let delta = change.delta();
        ReparseTask {
            base: self.clone(),
            change,
            new_edit_end: change.start + change.new_len,
            delta,
            restart: restart.clone(),
            parser,
            interner: base.interner.clone(),
            lines: Vec::new(),
            sink: Sink {
                events: Vec::new(),
                ids: Vec::new(),
                next_id: base.next_id,
            },
            pos: restart.start,
            old: restart.clone(),
            old_valid: true,
            done: None,
            seam: None,
            watch,
            restarted: false,
            scratch: Vec::new(),
            changes: Changes {
                restart_line: restart.global,
                ..Changes::default()
            },
        }
    }
}

/// The pending definition text of the paragraph open at `restart`: the
/// content of its lines since the last decided definition (or its first
/// line). The lines before `restart` are unchanged by the edit.
fn pending_text_before<S: TextSource + ?Sized>(
    base: &TreeData,
    restart: &LineCursor,
    text: &S,
) -> Vec<u8> {
    let mut g = restart.global;
    let first = loop {
        if g == 0 {
            break 0;
        }
        g -= 1;
        let c = base.cursor_at_line(g);
        let Some(l) = base.line(&c) else { break g };
        if l.events
            .iter()
            .any(|e| e.is_refdef() || e.open_kind() == Some(BlockKind::Text))
        {
            break g;
        }
    };
    let mut out = Vec::new();
    let mut c = base.cursor_at_line(first);
    while c.global < restart.global {
        let Some(l) = base.line(&c) else { break };
        let mut off = l.start;
        let mut content = None;
        for e in l.events {
            if let Some(t) = e.token() {
                if t.kind == TokenKind::Content {
                    content = Some(off..off + t.len as usize);
                }
                off += t.len as usize;
            }
        }
        if let Some(r) = content {
            text.copy_range(r, &mut out);
            out.push(b'\n');
        }
        if !base.advance(&mut c) {
            break;
        }
    }
    out
}

/// Whether lines with this state may serve as convergence points.
fn convergeable(interner: &Interner, state: StateId) -> bool {
    !matches!(
        interner.top(state).kind,
        Kind::Paragraph {
            refs: RefMode::Long,
            ..
        }
    )
}

impl ReparseTask {
    /// Whether the block pass has finished.
    pub fn is_done(&self) -> bool {
        self.done.is_some()
    }

    /// Statistics so far.
    pub fn changes(&self) -> &Changes {
        &self.changes
    }

    /// Restarts the block pass at line 0 (front matter, see the module
    /// documentation).
    fn restart_at_zero(&mut self) {
        let base = &self.base.0;
        self.restart = base.cursor_at_line(0);
        self.parser = BlockParser::with_options(BlockOptions::of(base.options));
        self.lines.clear();
        self.sink.events.clear();
        self.sink.ids.clear();
        self.sink.next_id = base.next_id;
        self.pos = 0;
        self.old = self.restart.clone();
        self.old_valid = true;
        self.seam = None;
        self.watch = None;
        self.restarted = true;
        self.changes.restart_line = 0;
    }

    /// Runs the block pass until it converges, reaches the end of the text,
    /// or the budget runs out. Returns true when done. `text` must be the
    /// same new text on every call.
    pub fn run<S: TextSource + ?Sized>(&mut self, text: &S, budget: Budget) -> bool {
        self.run_inner(text, budget, false)
    }

    /// `aligned`: also stop (unfinished) as soon as the old and new lines
    /// are aligned past the edit (for building an interim tree).
    fn run_inner<S: TextSource + ?Sized>(
        &mut self,
        text: &S,
        budget: Budget,
        aligned: bool,
    ) -> bool {
        if self.done.is_some() {
            return true;
        }
        let base = Arc::clone(&self.base.0);
        let n = text.len();
        let mut since_check = 0u32;
        loop {
            let state = self.parser.checkpoint(&mut self.interner);
            // Convergence: past the edit, at a line start that is also a line
            // start of the old text, the old checkpoint equals the new state.
            let q = self.pos as isize - self.delta;
            if self.pos >= self.new_edit_end && q >= 0 {
                let q = q as usize;
                while self.old_valid && self.old.start < q {
                    self.old_valid = base.advance(&mut self.old);
                }
                let at_old_line = self.old_valid && self.old.start == q;
                if at_old_line && self.seam.is_none() {
                    self.seam = Some((self.lines.len(), self.old.clone(), state));
                }
                if at_old_line
                    && self.pos > 0
                    && q > 0
                    && let Some(l) = base.line(&self.old)
                    && l.state == state
                    && convergeable(&self.interner, state)
                {
                    self.done = Some(self.old.global);
                    self.changes.converged = true;
                    return true;
                }
            }
            if self.pos >= n {
                if self.parser.in_front_matter() && !self.restarted {
                    // Front matter without a closing line is no front
                    // matter: line 0 must be parsed again.
                    self.restart_at_zero();
                    continue;
                }
                self.parser.finish(&mut self.sink);
                self.end_line(0, state);
                self.done = Some(base.line_count());
                return true;
            }
            if aligned && self.seam.is_some() {
                return false;
            }
            if let Budget::Until(deadline) = budget
                && !aligned
            {
                since_check += 1;
                if since_check >= 32 {
                    since_check = 0;
                    if Instant::now() >= deadline {
                        return false;
                    }
                }
            }
            let line = text.line(self.pos, &mut self.scratch);
            let len = line.len();
            if len == 0 {
                // A text shorter than it claims: finish here.
                self.parser.finish(&mut self.sink);
                self.end_line(0, state);
                self.done = Some(base.line_count());
                return true;
            }
            if self.pos > 0
                && self
                    .watch
                    .is_some_and(|kind| front_matter::is_closer(line, kind))
            {
                // Line 0 opens front matter after all.
                self.restart_at_zero();
                continue;
            }
            let fm = if self.pos == 0 {
                front_matter::detect(text, base.options.front_matter)
            } else {
                None
            };
            self.parser
                .process_line(line, self.pos == 0, fm, &mut self.interner, &mut self.sink);
            self.end_line(len, state);
            self.pos += len;
            self.changes.lines_parsed += 1;
            self.changes.bytes_parsed += len;
        }
    }

    fn end_line(&mut self, len: usize, state: StateId) {
        self.lines.push(LineRec {
            len: u32::try_from(len).unwrap_or(u32::MAX),
            state,
            ev_end: u32::try_from(self.sink.events.len()).unwrap_or(u32::MAX),
            id_end: u32::try_from(self.sink.ids.len()).unwrap_or(u32::MAX),
        });
    }

    /// Events and ids of window line `i`.
    fn window_line(&self, i: usize) -> (&[Ev], usize) {
        let es = if i == 0 {
            0
        } else {
            self.lines[i - 1].ev_end as usize
        };
        let is = if i == 0 {
            0
        } else {
            self.lines[i - 1].id_end as usize
        };
        (&self.sink.events[es..self.lines[i].ev_end as usize], is)
    }

    /// Matches the BlockIds of the blocks opened in the window against the
    /// old window `[restart, old_end)`, writing old ids into `ids`. Returns
    /// (kept, new, old window has definition markers).
    fn match_ids(&self, old_end: usize, ids: &mut [BlockId]) -> (usize, usize, bool) {
        self.match_ids_in(old_end, &self.sink.events, ids)
    }

    /// [`ReparseTask::match_ids`] for the window prefix whose events are
    /// `events`.
    fn match_ids_in(
        &self,
        old_end: usize,
        events: &[Ev],
        ids: &mut [BlockId],
    ) -> (usize, usize, bool) {
        #[derive(Clone, Copy)]
        struct O {
            pos: usize,
            depth: u32,
            tag: u8,
            id: BlockId,
            k: usize,
        }
        let base = &self.base.0;
        let depth0 = base
            .line(&self.restart)
            .map_or(0, |l| base.interner.depth(l.state) - 1) as u32;
        let mut old_refdefs = false;
        let mut old = Vec::new();
        {
            let mut c = self.restart.clone();
            let mut depth = depth0;
            while c.global < old_end {
                let Some(l) = base.line(&c) else { break };
                let mut off = l.start;
                let mut it = l.ids.iter();
                for e in l.events {
                    if let Some(k) = e.open_kind() {
                        old.push(O {
                            pos: off,
                            depth,
                            tag: k.tag(),
                            id: it.next().copied().unwrap_or(BlockId(0)),
                            k: 0,
                        });
                        depth += 1;
                        old_refdefs |= e.is_footnote_open();
                    } else if e.is_close() {
                        depth = depth.saturating_sub(1);
                    } else if e.is_refdef() {
                        old_refdefs = true;
                    } else {
                        off += e.tok_len();
                    }
                }
                if !base.advance(&mut c) {
                    break;
                }
            }
        }
        let mut new = Vec::new();
        {
            let mut off = self.restart.start;
            let mut depth = depth0;
            let mut k = 0;
            for e in events {
                if let Some(kind) = e.open_kind() {
                    new.push(O {
                        pos: off,
                        depth,
                        tag: kind.tag(),
                        id: BlockId(0),
                        k,
                    });
                    k += 1;
                    depth += 1;
                } else if e.is_close() {
                    depth = depth.saturating_sub(1);
                } else {
                    off += e.tok_len();
                }
            }
        }
        let (a, b, n) = (self.change.start, self.change.old_end, self.change.new_len);
        let old_key = |p: usize| {
            if p < a {
                Some(p)
            } else if p >= b {
                Some(p - b + a + n)
            } else {
                None
            }
        };
        let new_key = |p: usize| if p < a || p >= a + n { Some(p) } else { None };
        // Anchors: a merge join on (key, depth). Both lists are in document
        // order, keys are monotone in position, and blocks opening at the
        // same position nest (outer first).
        let mut pair: Vec<Option<usize>> = vec![None; new.len()];
        let mut used = vec![false; old.len()];
        let (mut i, mut j) = (0, 0);
        while i < new.len() && j < old.len() {
            let Some(nk) = new_key(new[i].pos) else {
                i += 1;
                continue;
            };
            let Some(ok) = old_key(old[j].pos) else {
                j += 1;
                continue;
            };
            match (nk, new[i].depth).cmp(&(ok, old[j].depth)) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    if new[i].tag == old[j].tag {
                        pair[i] = Some(j);
                        used[j] = true;
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        // Pair the unanchored blocks between anchors, in order.
        let mut oi = 0;
        for i in 0..new.len() {
            if let Some(j) = pair[i] {
                oi = j + 1;
                continue;
            }
            let mut j = oi;
            let mut steps = 0;
            while j < old.len() && !used[j] && steps < 64 {
                if old[j].depth == new[i].depth && old[j].tag == new[i].tag {
                    pair[i] = Some(j);
                    used[j] = true;
                    oi = j + 1;
                    break;
                }
                j += 1;
                steps += 1;
            }
        }
        let mut kept = 0;
        for (i, o) in new.iter().enumerate() {
            if let Some(j) = pair[i] {
                ids[o.k] = old[j].id;
                kept += 1;
            }
        }
        (kept, new.len() - kept, old_refdefs)
    }

    /// Matches BlockIds and splices the new window into the tree.
    ///
    /// # Panics
    ///
    /// Panics if the task is not done.
    pub fn finish(mut self) -> (SyntaxTree, Changes) {
        let old_end = self
            .done
            .expect("ReparseTask::finish before the task is done");
        let mut ids = std::mem::take(&mut self.sink.ids);
        let (kept, fresh, old_refdefs) = self.match_ids(old_end, &mut ids);
        self.sink.ids = ids;
        self.changes.ids_kept = kept;
        self.changes.ids_new = fresh;
        let new_refdefs = self
            .sink
            .events
            .iter()
            .any(|e| e.is_refdef() || e.is_footnote_open());

        let base = Arc::clone(&self.base.0);
        let restart = self.restart.global;
        let mut cursor = base.chunks.cursor::<Lines>(());
        let mut tree = cursor.slice(&Lines(restart), Bias::Right);
        let mut cb = ChunkBuilder::new(
            self.sink.next_id,
            cursor.item().map_or(0, |c| c.0.start_depth),
        );
        if let Some(first) = cursor.item() {
            let first_line = cursor.start().0;
            for i in 0..restart - first_line {
                let (ev, id) = first.0.line_events(i);
                cb.push_line(first.0.lines[i], ev, id);
            }
        }
        for i in 0..self.lines.len() {
            let (ev, is) = self.window_line(i);
            let ie = self.lines[i].id_end as usize;
            cb.push_line(self.lines[i], ev, &self.sink.ids[is..ie]);
        }
        cursor.seek_forward(&Lines(old_end), Bias::Right);
        if let Some(last) = cursor.item() {
            let last_line = cursor.start().0;
            for i in old_end - last_line..last.0.lines.len() {
                let (ev, id) = last.0.line_events(i);
                cb.push_line(last.0.lines[i], ev, id);
            }
            cursor.next();
        }
        // Avoid fragmenting into tiny chunks.
        if cb.pending_lines() > 0
            && cb.pending_lines() < CHUNK_LINES / 2
            && let Some(next) = cursor.item()
        {
            for i in 0..next.0.lines.len() {
                let (ev, id) = next.0.line_events(i);
                cb.push_line(next.0.lines[i], ev, id);
            }
            cursor.next();
        }
        let rest = cursor.suffix();
        let (chunks, next_id) = cb.finish();
        tree.extend(chunks, ());
        tree.append(rest, ());

        let new_len = (base.len as isize + self.delta) as usize;
        let old_end_byte = if self.old_valid && self.old.global == old_end {
            // Converged: the old cursor is at the old line where the window
            // ends.
            self.old.start
        } else if old_end < base.line_count() {
            let mut c = base.chunks.cursor::<sum_tree::Dimensions<Lines, Bytes>>(());
            c.seek(&Lines(old_end), Bias::Right);
            let start = *c.start();
            start.1.0
                + c.item().map_or(0, |ch| {
                    ch.0.lines[..old_end - start.0.0]
                        .iter()
                        .map(|l| l.len as usize)
                        .sum::<usize>()
                })
        } else {
            base.len
        };
        let data = TreeData::new(tree, self.interner, next_id, new_len, false, base.options);
        if !old_refdefs
            && !new_refdefs
            && let Some(m) = base.ref_map.get()
        {
            let _ = data.ref_map.set(Arc::clone(m));
        }
        self.changes.old_end_line = old_end;
        self.changes.new_end_line = restart + self.lines.len();
        self.changes.old_range = self.restart.start..old_end_byte;
        self.changes.new_range = self.restart.start..self.pos;
        (SyntaxTree(Arc::new(data)), self.changes)
    }

    /// A display-only tree of the new text while the task is paused: the
    /// previous tree shifted by the edit. The lines from the restart line
    /// up to the first line past the edit that starts an old line are
    /// parsed anew; from there on the old lines follow, with blocks closed
    /// and reopened at the seam so the stream stays balanced. BlockIds of
    /// the old part are kept, so the UI stays stable. The cost is the
    /// edited lines plus O(log n).
    ///
    /// If the slice stopped before the end of the inserted text, the block
    /// pass first runs (without a deadline) past it, so the cost grows with
    /// the size of the insertion.
    pub fn interim<S: TextSource + ?Sized>(&mut self, text: &S) -> SyntaxTree {
        if !self.is_done() && self.seam.is_none() {
            self.run_inner(text, Budget::Unlimited, true);
        }
        let base = Arc::clone(&self.base.0);
        let Some((k, old_line, new_state)) = self.seam.clone() else {
            // Done without a seam (the edit reaches the end of the text):
            // the window is the rest of the document; finish a copy.
            let copy = ReparseTask {
                base: self.base.clone(),
                change: self.change,
                new_edit_end: self.new_edit_end,
                delta: self.delta,
                restart: self.restart.clone(),
                parser: self.parser.clone(),
                interner: self.interner.clone(),
                lines: self.lines.clone(),
                sink: Sink {
                    events: self.sink.events.clone(),
                    ids: self.sink.ids.clone(),
                    next_id: self.sink.next_id,
                },
                pos: self.pos,
                old: self.old.clone(),
                old_valid: self.old_valid,
                done: self.done,
                seam: None,
                watch: self.watch,
                restarted: self.restarted,
                scratch: Vec::new(),
                changes: self.changes.clone(),
            };
            return copy.finish().0;
        };
        let Some(seam) = base.line(&old_line).map(|l| (l.state, l.index)) else {
            return SyntaxTree(Arc::clone(&self.base.0));
        };
        let (old_state, seam_line) = seam;
        let ev_end = if k == 0 {
            0
        } else {
            self.lines[k - 1].ev_end as usize
        };
        let id_end = if k == 0 {
            0
        } else {
            self.lines[k - 1].id_end as usize
        };
        let mut ids = self.sink.ids[..id_end].to_vec();
        self.match_ids_in(seam_line, &self.sink.events[..ev_end], &mut ids);

        // Rebalance at the seam: close the new-only blocks, open the
        // old-only ones (phantoms).
        let (new_stack, _) = self.interner.resolve(new_state);
        let (old_stack, _) = base.interner.resolve(old_state);
        let same = |a: Kind, b: Kind| match (a, b) {
            (
                Kind::List {
                    ordered: o1,
                    ch: c1,
                },
                Kind::List {
                    ordered: o2,
                    ch: c2,
                },
            ) => o1 == o2 && c1 == c2,
            (Kind::Item { .. }, Kind::Item { .. })
            | (Kind::Paragraph { .. }, Kind::Paragraph { .. })
            | (Kind::FencedCode { .. }, Kind::FencedCode { .. })
            | (Kind::MathBlock { .. }, Kind::MathBlock { .. })
            | (Kind::CommentBlock { .. }, Kind::CommentBlock { .. })
            | (Kind::FrontMatter { .. }, Kind::FrontMatter { .. }) => true,
            (a, b) => a == b,
        };
        let mut common = 1;
        while common < new_stack.len()
            && common < old_stack.len()
            && same(new_stack[common].kind, old_stack[common].kind)
        {
            common += 1;
        }
        let mut seam_events = vec![Ev::CLOSE; new_stack.len() - common];
        let mut seam_ids = Vec::new();
        let mut phantom = u64::MAX;
        for e in &old_stack[common..] {
            seam_events.push(Ev::open(phantom_kind(e.kind)));
            seam_ids.push(BlockId(phantom));
            phantom -= 1;
        }

        let restart = self.restart.global;
        let mut cursor = base.chunks.cursor::<Lines>(());
        let mut tree = cursor.slice(&Lines(restart), Bias::Right);
        let mut cb = ChunkBuilder::new(
            self.sink.next_id,
            cursor.item().map_or(0, |c| c.0.start_depth),
        );
        if let Some(first) = cursor.item() {
            let first_line = cursor.start().0;
            for i in 0..restart - first_line {
                let (ev, id) = first.0.line_events(i);
                cb.push_line(first.0.lines[i], ev, id);
            }
        }
        for i in 0..k {
            let (ev, is) = self.window_line(i);
            let ie = self.lines[i].id_end as usize;
            cb.push_line(self.lines[i], ev, &ids[is..ie]);
        }
        cursor.seek_forward(&Lines(seam_line), Bias::Right);
        if let Some(chunk) = cursor.item() {
            let first_line = cursor.start().0;
            for i in seam_line - first_line..chunk.0.lines.len() {
                let (ev, id) = chunk.0.line_events(i);
                if i == seam_line - first_line {
                    let mut evs = seam_events.clone();
                    evs.extend_from_slice(ev);
                    let mut is = seam_ids.clone();
                    is.extend_from_slice(id);
                    cb.push_line(chunk.0.lines[i], &evs, &is);
                } else {
                    cb.push_line(chunk.0.lines[i], ev, id);
                }
            }
            cursor.next();
        }
        let rest = cursor.suffix();
        let (chunks, next_id) = cb.finish();
        tree.extend(chunks, ());
        tree.append(rest, ());
        let new_len = (base.len as isize + self.delta) as usize;
        SyntaxTree(Arc::new(TreeData::new(
            tree,
            self.interner.clone(),
            next_id,
            new_len,
            true,
            base.options,
        )))
    }
}

/// The block kind shown for an old block reopened at an interim seam.
fn phantom_kind(k: Kind) -> BlockKind {
    match k {
        Kind::BlockQuote => BlockKind::BlockQuote,
        Kind::FootnoteDefinition => BlockKind::FootnoteDefinition,
        Kind::List { ordered: true, ch } => BlockKind::OrderedList {
            start: 1,
            delimiter: ch,
        },
        Kind::List { ch, .. } => BlockKind::BulletList { marker: ch },
        Kind::Item { .. } => BlockKind::Item,
        Kind::FencedCode { .. } => BlockKind::FencedCode,
        Kind::IndentedCode => BlockKind::IndentedCode,
        Kind::MathBlock { .. } => BlockKind::MathBlock,
        Kind::CommentBlock { .. } => BlockKind::CommentBlock,
        Kind::Html { ty } => BlockKind::Html { kind: ty },
        Kind::Heading => BlockKind::AtxHeading { level: 1 },
        Kind::ThematicBreak => BlockKind::ThematicBreak,
        Kind::FrontMatter { kind } => BlockKind::FrontMatter { kind },
        Kind::Paragraph { .. } | Kind::Table | Kind::Document => BlockKind::Text,
    }
}

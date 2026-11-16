// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors
//
// Ported from Zed (https://github.com/zed-industries/zed),
// crates/editor/src/display_map/wrap_map.rs at commit
// d2a4fb6ded7c470be25d1541281da3882495b65b.
// Copyright (C) Zed Industries, Inc. and the Zed contributors.
// Licensed under the GNU General Public License, version 3 or later.
//
// Modified by the Glyphvane contributors (2026-11-14): the layer sits on
// gv-editor's tab layer and measures through an injected `TextMeasure`
// instead of GPUI's text system; the GPUI entity, background tasks and
// `async` update become a synchronous map with a resumable `RewrapJob`;
// the soft-wrap indent comes from a per-row `WrapIndent` callback;
// isomorphic transforms are capped at 32 rows so an edit re-summarizes a
// bounded amount of text; invisibles, highlighting, tracing and multi-buffer
// row infos are removed, and the tests run without GPUI. The full list is in
// third_party/zed/PROVENANCE.md. (2026-11-16): chunks take caller highlights
// and pass them to the tab layer.

//! The wrap layer: soft-wraps the tab layer's rows at a width.
//!
//! Rows longer than the wrap width are broken by a [`LineWrapper`] (word
//! boundaries and spaces, CJK ideographs, long words broken before the
//! grapheme that overflows; see [`line_wrapper`](super::line_wrapper)).
//! Each break inserts a `\n` and the continuation row's indent, in spaces,
//! into the layer's text; the indent comes from a [`WrapIndent`] callback
//! (the row's leading whitespace by default). A [`WrapPoint`] is a
//! [`WrapRow`] and a byte column in that text, indent included.
//!
//! # Updates
//!
//! [`WrapMap::sync`] re-wraps only the rows a tab edit touched and returns
//! [`WrapEdit`]s in wrap rows. Changing the width, the measure or the
//! indent callback re-wraps every row:
//!
//! - [`WrapMap::set_wrap_width`] (and the other setters) does it at once;
//! - [`WrapMap::defer_wrap_width`] or [`WrapMap::begin_rewrap`] return a
//!   [`RewrapJob`] instead. It is `Send` and runs in steps of rows
//!   ([`RewrapJob::run`]) with its own [`LineWrapper`], so a view can run
//!   it on another thread or across frames. Until
//!   [`WrapMap::finish_rewrap`] takes it back, `sync` keeps the old wraps
//!   for unchanged text and leaves edited text unwrapped (the snapshot
//!   [`is_interpolated`](WrapSnapshot::is_interpolated)), then the edits
//!   made meanwhile are re-applied to the job's result.

use std::cmp;
use std::fmt;
use std::mem;
use std::ops::Range;
use std::sync::Arc;

use gv_text::{Bias, TextSnapshot};
use sum_tree::{Cursor, Dimensions, SumTree};

use super::BufferPoint;
use super::line_wrapper::{IndentAdjustment, LineFragment, LineWrapper};
use super::primitives::{Chunk, Edit, Highlight, MAX_CHUNK_LEN, Point, TextSummary};
use super::tab_map::{TabChunks, TabEdit, TabPoint, TabSnapshot};
use super::text_measure::TextMeasure;
use super::wrap_indent::WrapIndent;

/// An edit of the wrap layer, in whole rows: `old` and `new` are row
/// ranges (end exclusive) in the old and the new snapshot.
pub type WrapEdit = Edit<WrapRow>;

/// A row of the wrap layer's text.
#[derive(Copy, Clone, Debug, Default, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct WrapRow(pub u32);

impl std::ops::Add for WrapRow {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

/// Isomorphic transforms are split at row starts once they hold this many
/// rows, so that re-summarizing the part of a transform around an edit
/// scans a bounded amount of text. (Upstream merges them without limit.)
const MAX_ISOMORPHIC_ROWS: u32 = 32;

const WRAP_TEXT_LEN: usize = 1 + LineWrapper::MAX_INDENT as usize;
const WRAP_BYTES: [u8; WRAP_TEXT_LEN] = {
    let mut bytes = [b' '; WRAP_TEXT_LEN];
    bytes[0] = b'\n';
    bytes
};
/// A line break followed by the largest indent: every wrap's display text
/// is a prefix.
const WRAP_TEXT: &str = match std::str::from_utf8(&WRAP_BYTES) {
    Ok(text) => text,
    Err(_) => panic!("the wrap text is ASCII"),
};

/// Handles soft wrapping of text.
pub struct WrapMap {
    snapshot: WrapSnapshot,
    wrap_width: Option<f32>,
    wrapper: LineWrapper,
    indent: Arc<dyn WrapIndent>,
    /// The re-wrap handed out by `begin_rewrap`, and the tab edits that
    /// arrived since.
    pending: Option<PendingRewrap>,
    generation: u64,
}

impl fmt::Debug for WrapMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WrapMap")
            .field("snapshot", &self.snapshot)
            .field("wrap_width", &self.wrap_width)
            .field("pending", &self.pending)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct PendingRewrap {
    generation: u64,
    edits: Vec<(TabSnapshot, Vec<TabEdit>)>,
}

/// An immutable view of the wrap layer. Cloning is O(1).
#[derive(Clone, Debug)]
pub struct WrapSnapshot {
    tab_snapshot: TabSnapshot,
    transforms: SumTree<Transform>,
    interpolated: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Transform {
    summary: TransformSummary,
    display_text: Option<&'static str>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TransformSummary {
    input: TextSummary,
    output: TextSummary,
}

/// A position in the wrap layer's text: a row and a byte column, the
/// continuation indent included.
#[derive(Copy, Clone, Debug, Default, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct WrapPoint(pub Point);

/// The wrap layer's text in chunks of at most [`MAX_CHUNK_LEN`] bytes, none
/// of them empty. See [`WrapSnapshot::chunks`].
#[derive(Debug)]
pub struct WrapChunks<'a> {
    input_chunks: TabChunks<'a>,
    input_chunk: Chunk<'a>,
    output_position: WrapPoint,
    max_output_row: WrapRow,
    transforms: Cursor<'a, 'static, Transform, Dimensions<WrapPoint, TabPoint>>,
    snapshot: &'a WrapSnapshot,
    /// The rest of a wrap's display text that did not fit in one chunk.
    pending_display_text: &'static str,
}

/// What a wrap row shows. See [`WrapSnapshot::row_infos`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WrapRowInfo {
    /// The tab row the wrap row is part of.
    pub tab_row: u32,
    /// The buffer line of that tab row.
    pub buffer_line: usize,
    /// Whether the row continues a soft-wrapped row (it shows no line
    /// number).
    pub soft_wrapped: bool,
}

/// The rows of the wrap layer from a start row. See
/// [`WrapSnapshot::row_infos`].
#[derive(Clone, Debug)]
pub struct WrapRows<'a> {
    snapshot: &'a WrapSnapshot,
    input_row: u32,
    buffer_line: usize,
    output_row: WrapRow,
    soft_wrapped: bool,
    max_output_row: WrapRow,
    transforms: Cursor<'a, 'static, Transform, Dimensions<WrapPoint, TabPoint>>,
}

impl WrapRows<'_> {
    /// Restarts the iterator at `start_row`.
    pub fn seek(&mut self, start_row: WrapRow) {
        self.transforms
            .seek(&WrapPoint::new(start_row, 0), sum_tree::Bias::Left);
        let mut input_row = self.transforms.start().1.row();
        if self.transforms.item().is_some_and(|t| t.is_isomorphic()) {
            input_row += start_row.0 - self.transforms.start().0.row().0;
        }
        self.soft_wrapped = self.transforms.item().is_some_and(|t| !t.is_isomorphic());
        self.input_row = input_row;
        self.buffer_line = self.snapshot.buffer_line(input_row);
        self.output_row = start_row;
    }
}

impl WrapMap {
    /// A wrap layer over `tab_snapshot` that measures with `measure` and
    /// indents continuation rows like their row's leading whitespace, and
    /// its first snapshot, wrapped at `wrap_width` (`None`: no wrapping).
    pub fn new(
        tab_snapshot: TabSnapshot,
        measure: Box<dyn TextMeasure>,
        wrap_width: Option<f32>,
    ) -> (Self, WrapSnapshot) {
        Self::with_indent(
            tab_snapshot,
            measure,
            Arc::new(IndentAdjustment::SameIndent),
            wrap_width,
        )
    }

    /// Like [`new`](Self::new), with the continuation indent chosen by
    /// `indent`.
    pub fn with_indent(
        tab_snapshot: TabSnapshot,
        measure: Box<dyn TextMeasure>,
        indent: Arc<dyn WrapIndent>,
        wrap_width: Option<f32>,
    ) -> (Self, WrapSnapshot) {
        let mut this = Self {
            snapshot: WrapSnapshot::new(tab_snapshot),
            wrap_width: normalize_width(wrap_width),
            wrapper: LineWrapper::new(measure),
            indent,
            pending: None,
            generation: 0,
        };
        this.rewrap_now();
        let snapshot = this.snapshot.clone();
        (this, snapshot)
    }

    /// The current snapshot.
    pub fn snapshot(&self) -> WrapSnapshot {
        self.snapshot.clone()
    }

    /// The width rows are wrapped at, or `None` for no wrapping.
    pub fn wrap_width(&self) -> Option<f32> {
        self.wrap_width
    }

    /// Whether a [`RewrapJob`] is out: the snapshot is interpolated until
    /// it is finished.
    pub fn is_rewrapping(&self) -> bool {
        self.pending.is_some()
    }

    /// The measure, giving up the map.
    pub fn into_measure(self) -> Box<dyn TextMeasure> {
        self.wrapper.into_measure()
    }

    /// Moves the layer to `tab_snapshot`, which `edits` produced from the
    /// current input snapshot, re-wrapping the rows they touch, and returns
    /// the new snapshot with the edits in wrap rows.
    ///
    /// While a re-wrap is out, the edited text is left unwrapped instead
    /// (see the [module documentation](self)).
    pub fn sync(
        &mut self,
        tab_snapshot: TabSnapshot,
        edits: Vec<TabEdit>,
    ) -> (WrapSnapshot, Vec<WrapEdit>) {
        let wrap_edits = if let Some(pending) = &mut self.pending {
            pending.edits.push((tab_snapshot.clone(), edits.clone()));
            self.snapshot.interpolate(tab_snapshot, &edits)
        } else {
            self.snapshot.update(
                tab_snapshot,
                &edits,
                self.wrap_width,
                &mut self.wrapper,
                &*self.indent,
            )
        };
        (self.snapshot.clone(), wrap_edits)
    }

    /// Changes the wrap width (`None`: no wrapping) and re-wraps every row
    /// at once. Returns the edits (one over the whole text, none if the
    /// width stays the same).
    pub fn set_wrap_width(&mut self, wrap_width: Option<f32>) -> (WrapSnapshot, Vec<WrapEdit>) {
        match self.defer_wrap_width(wrap_width) {
            Some(job) => {
                let pending = self.pending.take();
                self.install(job, pending)
            }
            None => (self.snapshot.clone(), Vec::new()),
        }
    }

    /// Changes the wrap width without re-wrapping yet: returns the job that
    /// re-wraps every row (see [`begin_rewrap`](Self::begin_rewrap)), or
    /// `None` if the width stays the same.
    pub fn defer_wrap_width(&mut self, wrap_width: Option<f32>) -> Option<RewrapJob> {
        let wrap_width = normalize_width(wrap_width);
        if wrap_width == self.wrap_width {
            return None;
        }
        self.wrap_width = wrap_width;
        Some(self.begin_rewrap())
    }

    /// Measures with `measure` from now on (a new font or size) and
    /// re-wraps every row at once.
    pub fn set_measure(&mut self, measure: Box<dyn TextMeasure>) -> (WrapSnapshot, Vec<WrapEdit>) {
        self.wrapper = LineWrapper::new(measure);
        self.rewrap_now()
    }

    /// Chooses continuation indents with `indent` from now on and re-wraps
    /// every row at once.
    pub fn set_indent(&mut self, indent: Arc<dyn WrapIndent>) -> (WrapSnapshot, Vec<WrapEdit>) {
        self.indent = indent;
        self.rewrap_now()
    }

    /// Re-wraps tab rows `rows` (clamped to the text), for an indent
    /// callback whose answer for them changed.
    pub fn refresh_rows(&mut self, rows: Range<u32>) -> (WrapSnapshot, Vec<WrapEdit>) {
        let max_row = self.snapshot.tab_snapshot.max_point().row();
        if rows.is_empty() || rows.start > max_row {
            return (self.snapshot.clone(), Vec::new());
        }
        let last = (rows.end - 1).min(max_row);
        let range = TabPoint::new(rows.start, 0)..TabPoint::new(last, 0);
        let tab_snapshot = self.snapshot.tab_snapshot.clone();
        self.sync(
            tab_snapshot,
            vec![TabEdit {
                old: range.clone(),
                new: range,
            }],
        )
    }

    /// Starts re-wrapping every row with the current width, measure and
    /// indent callback, without doing any of the work.
    ///
    /// Run the job with [`RewrapJob::run`], on any thread, with a
    /// [`LineWrapper`] that measures like this map's, then hand it to
    /// [`finish_rewrap`](Self::finish_rewrap). Meanwhile
    /// [`sync`](Self::sync) interpolates. A later re-wrap (another width,
    /// measure or callback) makes this job stale.
    pub fn begin_rewrap(&mut self) -> RewrapJob {
        self.generation += 1;
        self.pending = Some(PendingRewrap {
            generation: self.generation,
            edits: Vec::new(),
        });
        self.snapshot.interpolated = true;
        RewrapJob::new(
            self.generation,
            self.snapshot.tab_snapshot.clone(),
            self.wrap_width,
            Arc::clone(&self.indent),
        )
    }

    /// Installs a job from [`begin_rewrap`](Self::begin_rewrap): finishes
    /// any rows it has not done (with this map's measure), re-applies the
    /// edits synced since it began, and returns the new snapshot with one
    /// edit over the whole text. Returns `None`, and changes nothing, for a
    /// stale job.
    pub fn finish_rewrap(&mut self, job: RewrapJob) -> Option<(WrapSnapshot, Vec<WrapEdit>)> {
        let pending = self
            .pending
            .take_if(|pending| pending.generation == job.generation)?;
        Some(self.install(job, Some(pending)))
    }

    fn rewrap_now(&mut self) -> (WrapSnapshot, Vec<WrapEdit>) {
        let job = self.begin_rewrap();
        let pending = self.pending.take();
        self.install(job, pending)
    }

    fn install(
        &mut self,
        mut job: RewrapJob,
        pending: Option<PendingRewrap>,
    ) -> (WrapSnapshot, Vec<WrapEdit>) {
        job.run_to_end(&mut self.wrapper);
        let mut snapshot = job.into_snapshot();
        for (tab_snapshot, tab_edits) in pending.map(|pending| pending.edits).unwrap_or_default() {
            snapshot.update(
                tab_snapshot,
                &tab_edits,
                self.wrap_width,
                &mut self.wrapper,
                &*self.indent,
            );
        }
        let old_rows = self.snapshot.max_point().row().0 + 1;
        let new_rows = snapshot.max_point().row().0 + 1;
        self.snapshot = snapshot;
        (
            self.snapshot.clone(),
            vec![WrapEdit {
                old: WrapRow(0)..WrapRow(old_rows),
                new: WrapRow(0)..WrapRow(new_rows),
            }],
        )
    }
}

/// `NaN` means no wrapping.
fn normalize_width(wrap_width: Option<f32>) -> Option<f32> {
    wrap_width.filter(|width| !width.is_nan())
}

/// A re-wrap of every row of a tab snapshot, done in steps. See
/// [`WrapMap::begin_rewrap`].
pub struct RewrapJob {
    generation: u64,
    tab_snapshot: TabSnapshot,
    wrap_width: Option<f32>,
    indent: Arc<dyn WrapIndent>,
    transforms: SumTree<Transform>,
    next_row: u32,
    row_count: u32,
}

impl fmt::Debug for RewrapJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RewrapJob")
            .field("generation", &self.generation)
            .field("wrap_width", &self.wrap_width)
            .field("next_row", &self.next_row)
            .field("row_count", &self.row_count)
            .finish_non_exhaustive()
    }
}

impl RewrapJob {
    fn new(
        generation: u64,
        tab_snapshot: TabSnapshot,
        wrap_width: Option<f32>,
        indent: Arc<dyn WrapIndent>,
    ) -> Self {
        let row_count = tab_snapshot.max_point().row() + 1;
        Self {
            generation,
            tab_snapshot,
            wrap_width,
            indent,
            transforms: SumTree::default(),
            next_row: 0,
            row_count,
        }
    }

    /// The tab rows to wrap.
    pub fn row_count(&self) -> u32 {
        self.row_count
    }

    /// The tab rows wrapped so far.
    pub fn rows_done(&self) -> u32 {
        self.next_row
    }

    /// Whether every row is wrapped.
    pub fn is_done(&self) -> bool {
        self.next_row >= self.row_count
    }

    /// Wraps up to `max_rows` more rows (at least one) with `wrapper`.
    /// Returns whether the job is done.
    pub fn run(&mut self, wrapper: &mut LineWrapper, max_rows: u32) -> bool {
        if self.is_done() {
            return true;
        }
        let end = self
            .next_row
            .saturating_add(max_rows.max(1))
            .min(self.row_count);
        let mut transforms = Vec::new();
        wrap_rows(
            &self.tab_snapshot,
            self.next_row..end,
            self.wrap_width,
            wrapper,
            &*self.indent,
            &mut transforms,
        );
        let mut transforms = transforms.into_iter();
        if let Some(transform) = transforms.next() {
            self.transforms.push_or_extend(transform);
        }
        self.transforms.extend(transforms, ());
        self.next_row = end;
        self.is_done()
    }

    /// Wraps the remaining rows.
    pub fn run_to_end(&mut self, wrapper: &mut LineWrapper) {
        while !self.run(wrapper, u32::MAX) {}
    }

    fn into_snapshot(self) -> WrapSnapshot {
        let snapshot = WrapSnapshot {
            tab_snapshot: self.tab_snapshot,
            transforms: self.transforms,
            interpolated: false,
        };
        snapshot.check_invariants();
        snapshot
    }
}

/// Wraps tab rows `rows` of `tab_snapshot` (all rows when `wrap_width` is
/// `None`: no wraps) and appends their transforms. Stops at the end of the
/// text.
fn wrap_rows(
    tab_snapshot: &TabSnapshot,
    rows: Range<u32>,
    wrap_width: Option<f32>,
    wrapper: &mut LineWrapper,
    indent: &dyn WrapIndent,
    transforms: &mut Vec<Transform>,
) {
    let mut line = String::new();
    let mut remaining = "";
    let mut chunks = tab_snapshot.chunks(TabPoint::new(rows.start, 0)..tab_snapshot.max_point());
    for row in rows {
        loop {
            let text = if remaining.is_empty() {
                match chunks.next() {
                    Some(chunk) => chunk.text,
                    None => break,
                }
            } else {
                mem::take(&mut remaining)
            };
            if let Some(ix) = text.find('\n') {
                let (prefix, suffix) = text.split_at(ix + 1);
                line.push_str(prefix);
                remaining = suffix;
                break;
            }
            line.push_str(text);
        }

        if line.is_empty() {
            break;
        }

        let mut prev_boundary_ix = 0;
        if let Some(wrap_width) = wrap_width
            && !fits(wrapper, &line, wrap_width)
        {
            let content = line.strip_suffix('\n').unwrap_or(&line);
            let adjustment = indent.indent(tab_snapshot, row, content);
            let fragments = [LineFragment::text(&line)];
            for boundary in wrapper.wrap_line(&fragments, wrap_width, adjustment) {
                let wrapped = &line[prev_boundary_ix..boundary.ix];
                push_isomorphic(transforms, TextSummary::from(wrapped));
                transforms.push(Transform::wrap(boundary.next_indent));
                prev_boundary_ix = boundary.ix;
            }
        }

        if prev_boundary_ix < line.len() {
            push_isomorphic(transforms, TextSummary::from(&line[prev_boundary_ix..]));
        }

        line.clear();
    }
}

/// Whether `line` fits in `wrap_width` without wrapping. The widths add up
/// in the same order as in [`LineWrapper::wrap_line`], which wraps only
/// once the sum exceeds the width, so a line that fits needs no wrapping
/// pass (and no break opportunities or graphemes).
fn fits(wrapper: &mut LineWrapper, line: &str, wrap_width: f32) -> bool {
    let mut width = 0.0;
    for c in line.chars() {
        if c != '\n' {
            width += wrapper.width_for_char(c);
            if width > wrap_width {
                return false;
            }
        }
    }
    true
}

impl WrapSnapshot {
    fn new(tab_snapshot: TabSnapshot) -> Self {
        Self {
            transforms: SumTree::default(),
            tab_snapshot,
            interpolated: true,
        }
    }

    /// The input layer's snapshot.
    pub fn tab_snapshot(&self) -> &TabSnapshot {
        &self.tab_snapshot
    }

    /// The buffer snapshot under all layers.
    pub fn buffer(&self) -> &TextSnapshot {
        self.tab_snapshot.buffer()
    }

    /// Whether some text is not wrapped yet because a re-wrap is running:
    /// row counts are estimates until it is finished.
    pub fn is_interpolated(&self) -> bool {
        self.interpolated
    }

    fn buffer_line(&self, tab_row: u32) -> usize {
        self.tab_snapshot
            .tab_point_to_buffer_point(TabPoint::new(tab_row, 0), Bias::Left)
            .line
    }

    fn interpolate(
        &mut self,
        new_tab_snapshot: TabSnapshot,
        tab_edits: &[TabEdit],
    ) -> Vec<WrapEdit> {
        let mut new_transforms;
        // Upstream has a fast path for a text without wraps that summarizes
        // the whole new text; here that is O(n), so the general path runs.
        if tab_edits.is_empty() {
            new_transforms = self.transforms.clone();
        } else {
            let mut old_cursor = self.transforms.cursor::<TabPoint>(());

            let mut tab_edits_iter = tab_edits.iter().peekable();
            new_transforms = match tab_edits_iter.peek() {
                Some(edit) => old_cursor.slice(&edit.old.start, sum_tree::Bias::Right),
                None => SumTree::default(),
            };

            while let Some(edit) = tab_edits_iter.next() {
                if edit.new.start > TabPoint::from(new_transforms.summary().input.lines) {
                    let summary = new_tab_snapshot.text_summary_for_range(
                        TabPoint::from(new_transforms.summary().input.lines)..edit.new.start,
                    );
                    new_transforms.push_or_extend(Transform::isomorphic(summary));
                }

                if !edit.new.is_empty() {
                    new_transforms.push_or_extend(Transform::isomorphic(
                        new_tab_snapshot.text_summary_for_range(edit.new.clone()),
                    ));
                }

                old_cursor.seek_forward(&edit.old.end, sum_tree::Bias::Right);
                if let Some(next_edit) = tab_edits_iter.peek() {
                    if next_edit.old.start > old_cursor.end() {
                        if old_cursor.end() > edit.old.end {
                            let summary = self
                                .tab_snapshot
                                .text_summary_for_range(edit.old.end..old_cursor.end());
                            new_transforms.push_or_extend(Transform::isomorphic(summary));
                        }

                        old_cursor.next();
                        new_transforms.append(
                            old_cursor.slice(&next_edit.old.start, sum_tree::Bias::Right),
                            (),
                        );
                    }
                } else {
                    if old_cursor.end() > edit.old.end {
                        let summary = self
                            .tab_snapshot
                            .text_summary_for_range(edit.old.end..old_cursor.end());
                        new_transforms.push_or_extend(Transform::isomorphic(summary));
                    }
                    old_cursor.next();
                    new_transforms.append(old_cursor.suffix(), ());
                }
            }
        }

        let old_snapshot = mem::replace(
            self,
            WrapSnapshot {
                tab_snapshot: new_tab_snapshot,
                transforms: new_transforms,
                interpolated: true,
            },
        );
        self.check_invariants();
        old_snapshot.compute_edits(tab_edits, self)
    }

    fn update(
        &mut self,
        new_tab_snapshot: TabSnapshot,
        tab_edits: &[TabEdit],
        wrap_width: Option<f32>,
        line_wrapper: &mut LineWrapper,
        indent: &dyn WrapIndent,
    ) -> Vec<WrapEdit> {
        #[derive(Debug)]
        struct RowEdit {
            old_rows: Range<u32>,
            new_rows: Range<u32>,
        }

        let mut tab_edits_iter = tab_edits.iter().peekable();
        let mut row_edits = Vec::with_capacity(tab_edits.len());
        while let Some(edit) = tab_edits_iter.next() {
            let mut row_edit = RowEdit {
                old_rows: edit.old.start.row()..edit.old.end.row() + 1,
                new_rows: edit.new.start.row()..edit.new.end.row() + 1,
            };

            while let Some(next_edit) = tab_edits_iter.peek() {
                if next_edit.old.start.row() <= row_edit.old_rows.end {
                    row_edit.old_rows.end =
                        cmp::max(row_edit.old_rows.end, next_edit.old.end.row() + 1);
                    row_edit.new_rows.end =
                        cmp::max(row_edit.new_rows.end, next_edit.new.end.row() + 1);
                    tab_edits_iter.next();
                } else {
                    break;
                }
            }

            row_edits.push(row_edit);
        }

        let mut new_transforms;
        if row_edits.is_empty() {
            new_transforms = self.transforms.clone();
        } else {
            let mut row_edits = row_edits.into_iter().peekable();
            let mut old_cursor = self.transforms.cursor::<TabPoint>(());

            new_transforms = match row_edits.peek() {
                Some(edit) => old_cursor.slice(
                    &TabPoint::new(edit.old_rows.start, 0),
                    sum_tree::Bias::Right,
                ),
                None => SumTree::default(),
            };

            while let Some(edit) = row_edits.next() {
                if edit.new_rows.start > new_transforms.summary().input.lines.row {
                    let summary = new_tab_snapshot.text_summary_for_range(
                        TabPoint(new_transforms.summary().input.lines)
                            ..TabPoint::new(edit.new_rows.start, 0),
                    );
                    new_transforms.push_or_extend(Transform::isomorphic(summary));
                }

                let mut edit_transforms = Vec::<Transform>::new();
                wrap_rows(
                    &new_tab_snapshot,
                    edit.new_rows.clone(),
                    wrap_width,
                    line_wrapper,
                    indent,
                    &mut edit_transforms,
                );

                let mut edit_transforms = edit_transforms.into_iter();
                if let Some(transform) = edit_transforms.next() {
                    new_transforms.push_or_extend(transform);
                }
                new_transforms.extend(edit_transforms, ());

                old_cursor
                    .seek_forward(&TabPoint::new(edit.old_rows.end, 0), sum_tree::Bias::Right);
                if let Some(next_edit) = row_edits.peek() {
                    if next_edit.old_rows.start > old_cursor.end().row() {
                        if old_cursor.end() > TabPoint::new(edit.old_rows.end, 0) {
                            let summary = self.tab_snapshot.text_summary_for_range(
                                TabPoint::new(edit.old_rows.end, 0)..old_cursor.end(),
                            );
                            new_transforms.push_or_extend(Transform::isomorphic(summary));
                        }
                        old_cursor.next();
                        new_transforms.append(
                            old_cursor.slice(
                                &TabPoint::new(next_edit.old_rows.start, 0),
                                sum_tree::Bias::Right,
                            ),
                            (),
                        );
                    }
                } else {
                    if old_cursor.end() > TabPoint::new(edit.old_rows.end, 0) {
                        let summary = self.tab_snapshot.text_summary_for_range(
                            TabPoint::new(edit.old_rows.end, 0)..old_cursor.end(),
                        );
                        new_transforms.push_or_extend(Transform::isomorphic(summary));
                    }
                    old_cursor.next();
                    new_transforms.append(old_cursor.suffix(), ());
                }
            }
        }

        let old_snapshot = mem::replace(
            self,
            WrapSnapshot {
                tab_snapshot: new_tab_snapshot,
                transforms: new_transforms,
                interpolated: false,
            },
        );
        self.check_invariants();
        old_snapshot.compute_edits(tab_edits, self)
    }

    fn compute_edits(&self, tab_edits: &[TabEdit], new_snapshot: &WrapSnapshot) -> Vec<WrapEdit> {
        let mut wrap_edits = Vec::with_capacity(tab_edits.len());
        let mut old_cursor = self.transforms.cursor::<TransformSummary>(());
        let mut new_cursor = new_snapshot.transforms.cursor::<TransformSummary>(());
        for mut tab_edit in tab_edits.iter().cloned() {
            tab_edit.old.start.0.column = 0;
            tab_edit.old.end.0 += Point::new(1, 0);
            tab_edit.new.start.0.column = 0;
            tab_edit.new.end.0 += Point::new(1, 0);

            old_cursor.seek(&tab_edit.old.start, sum_tree::Bias::Right);
            let mut old_start = old_cursor.start().output.lines;
            old_start += tab_edit.old.start.0 - old_cursor.start().input.lines;

            old_cursor.seek_forward(&tab_edit.old.end, sum_tree::Bias::Right);
            let mut old_end = old_cursor.start().output.lines;
            old_end += tab_edit.old.end.0 - old_cursor.start().input.lines;

            new_cursor.seek(&tab_edit.new.start, sum_tree::Bias::Right);
            let mut new_start = new_cursor.start().output.lines;
            new_start += tab_edit.new.start.0 - new_cursor.start().input.lines;

            new_cursor.seek_forward(&tab_edit.new.end, sum_tree::Bias::Right);
            let mut new_end = new_cursor.start().output.lines;
            new_end += tab_edit.new.end.0 - new_cursor.start().input.lines;

            wrap_edits.push(WrapEdit {
                old: WrapRow(old_start.row)..WrapRow(old_end.row),
                new: WrapRow(new_start.row)..WrapRow(new_end.row),
            });
        }

        consolidate_wrap_edits(wrap_edits)
    }

    /// The text of rows `rows` (end exclusive), in chunks. Each row but the
    /// last of the text ends with `\n`; a continuation row starts with its
    /// indent in spaces.
    pub fn chunks(&self, rows: Range<WrapRow>) -> WrapChunks<'_> {
        self.highlighted_chunks(rows, &[])
    }

    /// [`chunks`](Self::chunks) with the style ids of `highlights` (see
    /// [`ReplaceSnapshot::highlighted_chunks`](super::ReplaceSnapshot::highlighted_chunks));
    /// the breaks and indents of
    /// soft wraps carry none.
    pub fn highlighted_chunks<'a>(
        &'a self,
        rows: Range<WrapRow>,
        highlights: &'a [Highlight],
    ) -> WrapChunks<'a> {
        let output_start = WrapPoint::new(rows.start, 0);
        let output_end = WrapPoint::new(rows.end, 0);
        let mut transforms = self
            .transforms
            .cursor::<Dimensions<WrapPoint, TabPoint>>(());
        transforms.seek(&output_start, sum_tree::Bias::Right);
        let mut input_start = TabPoint(transforms.start().1.0);
        if transforms.item().is_some_and(|t| t.is_isomorphic()) {
            input_start.0 += output_start.0 - transforms.start().0.0;
        }
        let input_end = self.to_tab_point(output_end);
        let max_point = self.tab_snapshot.max_point();
        let input_start = input_start.min(max_point);
        let input_end = input_end.min(max_point);
        WrapChunks {
            input_chunks: self
                .tab_snapshot
                .highlighted_chunks(input_start..input_end, highlights),
            input_chunk: Chunk::default(),
            output_position: output_start,
            max_output_row: rows.end,
            transforms,
            snapshot: self,
            pending_display_text: "",
        }
    }

    /// The end of the text.
    pub fn max_point(&self) -> WrapPoint {
        WrapPoint(self.transforms.summary().output.lines)
    }

    /// Length of `row` in bytes, the indent included.
    pub fn line_len(&self, row: WrapRow) -> u32 {
        let (start, _, item) = self.transforms.find::<Dimensions<WrapPoint, TabPoint>, _>(
            (),
            &WrapPoint::new(row + WrapRow(1), 0),
            sum_tree::Bias::Left,
        );
        if item.is_some_and(|transform| transform.is_isomorphic()) {
            let overshoot = row.0 - start.0.row().0;
            let tab_row = start.1.row() + overshoot;
            let tab_line_len = self.tab_snapshot.line_len(tab_row);
            if overshoot == 0 {
                start.0.column() + (tab_line_len - start.1.column())
            } else {
                tab_line_len
            }
        } else {
            start.0.column()
        }
    }

    /// The summary of rows `rows` (end exclusive).
    pub fn text_summary_for_range(&self, rows: Range<WrapRow>) -> TextSummary {
        let mut summary = TextSummary::default();

        let start = WrapPoint::new(rows.start, 0);
        let end = WrapPoint::new(rows.end, 0);

        let mut cursor = self
            .transforms
            .cursor::<Dimensions<WrapPoint, TabPoint>>(());
        cursor.seek(&start, sum_tree::Bias::Right);
        if let Some(transform) = cursor.item() {
            let start_in_transform = start.0 - cursor.start().0.0;
            let end_in_transform = cmp::min(end, cursor.end().0).0 - cursor.start().0.0;
            if transform.is_isomorphic() {
                let tab_start = TabPoint(cursor.start().1.0 + start_in_transform);
                let tab_end = TabPoint(cursor.start().1.0 + end_in_transform);
                summary += &self.tab_snapshot.text_summary_for_range(tab_start..tab_end);
            } else {
                debug_assert_eq!(start_in_transform.row, end_in_transform.row);
                let indent_len = end_in_transform.column - start_in_transform.column;
                summary += &TextSummary {
                    lines: Point::new(0, indent_len),
                    first_line_chars: indent_len,
                    last_line_chars: indent_len,
                    longest_row: 0,
                    longest_row_chars: indent_len,
                };
            }

            cursor.next();
        }

        if rows.end > cursor.start().0.row() {
            summary += &cursor
                .summary::<_, TransformSummary>(&WrapPoint::new(rows.end, 0), sum_tree::Bias::Right)
                .output;

            if let Some(transform) = cursor.item() {
                let end_in_transform = end.0 - cursor.start().0.0;
                if transform.is_isomorphic() {
                    let char_start = cursor.start().1;
                    let char_end = TabPoint(char_start.0 + end_in_transform);
                    summary += &self
                        .tab_snapshot
                        .text_summary_for_range(char_start..char_end);
                } else {
                    debug_assert_eq!(end_in_transform, Point::new(1, 0));
                    summary += &TextSummary {
                        lines: Point::new(1, 0),
                        first_line_chars: 0,
                        last_line_chars: 0,
                        longest_row: 0,
                        longest_row_chars: 0,
                    };
                }
            }
        }

        summary
    }

    /// If `row` ends in a soft wrap, the indent (in columns) of the
    /// continuation row after it; `None` if the row ends in a line break or
    /// the end of the text.
    pub fn soft_wrap_indent(&self, row: WrapRow) -> Option<u32> {
        let (.., item) = self.transforms.find::<WrapPoint, _>(
            (),
            &WrapPoint::new(row + WrapRow(1), 0),
            sum_tree::Bias::Left,
        );
        item.and_then(|transform| {
            if transform.is_isomorphic() {
                None
            } else {
                Some(transform.summary.output.lines.column)
            }
        })
    }

    /// The row with the most `char`s (approximately, as in the tab layer).
    pub fn longest_row(&self) -> u32 {
        self.transforms.summary().output.longest_row
    }

    /// The rows from `start_row` to the end, with their tab rows and buffer
    /// lines.
    pub fn row_infos(&self, start_row: WrapRow) -> WrapRows<'_> {
        let mut transforms = self
            .transforms
            .cursor::<Dimensions<WrapPoint, TabPoint>>(());
        transforms.seek(&WrapPoint::new(start_row, 0), sum_tree::Bias::Left);
        let mut input_row = transforms.start().1.row();
        if transforms.item().is_some_and(|t| t.is_isomorphic()) {
            input_row += start_row.0 - transforms.start().0.row().0;
        }
        let soft_wrapped = transforms.item().is_some_and(|t| !t.is_isomorphic());
        WrapRows {
            snapshot: self,
            transforms,
            input_row,
            buffer_line: self.buffer_line(input_row),
            output_row: start_row,
            soft_wrapped,
            max_output_row: self.max_point().row(),
        }
    }

    /// The tab point of a wrap point. A point inside a continuation indent
    /// maps to the start of the row's text.
    pub fn to_tab_point(&self, point: WrapPoint) -> TabPoint {
        let (start, _, item) = self.transforms.find::<Dimensions<WrapPoint, TabPoint>, _>(
            (),
            &point,
            sum_tree::Bias::Right,
        );
        let mut tab_point = start.1.0;
        if item.is_some_and(|t| t.is_isomorphic()) {
            tab_point += point.0 - start.0.0;
        }
        TabPoint(tab_point)
    }

    /// The buffer point of a wrap point (clipped).
    pub fn wrap_point_to_buffer_point(&self, point: WrapPoint, bias: Bias) -> BufferPoint {
        self.tab_snapshot
            .tab_point_to_buffer_point(self.to_tab_point(point), bias)
    }

    /// The wrap point of a buffer point (clipped first).
    pub fn buffer_point_to_wrap_point(&self, point: BufferPoint, bias: Bias) -> WrapPoint {
        self.tab_point_to_wrap_point(self.tab_snapshot.buffer_point_to_tab_point(point, bias))
    }

    /// The wrap point of a tab point. A soft-wrap position (the end of a
    /// row that continues on the next) maps to the start of the
    /// continuation row's text, after its indent.
    pub fn tab_point_to_wrap_point(&self, point: TabPoint) -> WrapPoint {
        let (start, ..) = self.transforms.find::<Dimensions<TabPoint, WrapPoint>, _>(
            (),
            &point,
            sum_tree::Bias::Right,
        );
        WrapPoint(start.1.0 + (point.0 - start.0.0))
    }

    /// A cursor for converting many tab points to wrap points.
    pub fn wrap_point_cursor(&self) -> WrapPointCursor<'_> {
        WrapPointCursor {
            cursor: self
                .transforms
                .cursor::<Dimensions<TabPoint, WrapPoint>>(()),
        }
    }

    /// Clips a point to a valid one. A point inside a continuation indent
    /// moves to the end of the previous row ([`Bias::Left`]: before its last
    /// character, since the end of a soft-wrapped row is the same text
    /// position as the start of the next) or to the start of the row's text
    /// ([`Bias::Right`]); the end of a soft-wrapped row moves before its
    /// last character with [`Bias::Left`]. The tab layer clips the rest.
    pub fn clip_point(&self, mut point: WrapPoint, bias: Bias) -> WrapPoint {
        if bias == Bias::Left {
            let (start, _, item) =
                self.transforms
                    .find::<WrapPoint, _>((), &point, sum_tree::Bias::Right);
            if item.is_some_and(|t| !t.is_isomorphic()) {
                point = start;
                *point.column_mut() = point.column().saturating_sub(1);
            }
        }

        self.tab_point_to_wrap_point(self.tab_snapshot.clip_point(self.to_tab_point(point), bias))
    }

    /// Try to find a TabRow start that is also a WrapRow start
    /// Every TabRow start is a WrapRow start
    pub fn prev_row_boundary(&self, point: WrapPoint) -> WrapRow {
        if self.transforms.is_empty() {
            return WrapRow(0);
        }

        let point = WrapPoint::new(point.row(), 0);

        let mut cursor = self
            .transforms
            .cursor::<Dimensions<WrapPoint, TabPoint>>(());

        cursor.seek(&point, sum_tree::Bias::Right);
        if cursor.item().is_none() {
            cursor.prev();
        }

        //                          real newline     fake          fake
        // text:      helloworldasldlfjasd\njdlasfalsk\naskdjfasdkfj\n
        // dimensions v       v           v            v            v
        // transforms |-------|-----NW----|-----W------|-----W------|
        // cursor    ^        ^^^^^^^^^^^^^                          ^
        //                               (^)           ^^^^^^^^^^^^^^
        // point:                                            ^
        // point(col_zero):                           (^)

        while let Some(transform) = cursor.item() {
            if transform.is_isomorphic() {
                // this transform only has real linefeeds
                let tab_summary = &transform.summary.input;
                // is the wrap just before the end of the transform a tab row?
                // thats only if this transform has at least one newline
                //
                // "this wrap row is a tab row" <=> self.to_tab_point(WrapPoint::new(wrap_row, 0)).column() == 0

                // Note on comparison:
                // We have code that relies on this to be row > 1
                // It should work with row >= 1 but it does not :(
                //
                // That means that if every line is wrapped we walk back all the
                // way to the start. Which invalidates the entire state triggering
                // a full re-render.
                if tab_summary.lines.row > 1 {
                    let wrap_point_at_end = cursor.end().0.row();
                    return cmp::min(WrapRow(wrap_point_at_end.0 - 1), point.row());
                } else if cursor.start().1.column() == 0 {
                    return cmp::min(cursor.end().0.row(), point.row());
                }
            }

            cursor.prev();
        }

        WrapRow(0)
    }

    /// A wrap row after `point`'s row that starts a tab row, if one is found
    /// at the start of an unwrapped stretch of text (not necessarily the
    /// first such row).
    pub fn next_row_boundary(&self, mut point: WrapPoint) -> Option<WrapRow> {
        point.0 += Point::new(1, 0);

        let mut cursor = self
            .transforms
            .cursor::<Dimensions<WrapPoint, TabPoint>>(());
        cursor.seek(&point, sum_tree::Bias::Right);
        while let Some(transform) = cursor.item() {
            if transform.is_isomorphic() && cursor.start().1.column() == 0 {
                return Some(cmp::max(cursor.start().0.row(), point.row()));
            } else {
                cursor.next();
            }
        }

        None
    }

    /// The whole text.
    pub fn text(&self) -> String {
        self.text_chunks(WrapRow(0)).collect()
    }

    /// The text from `wrap_row` to the end, in chunks.
    pub fn text_chunks(&self, wrap_row: WrapRow) -> impl Iterator<Item = &str> {
        self.chunks(wrap_row..self.max_point().row() + WrapRow(1))
            .map(|h| h.text)
    }

    fn check_invariants(&self) {
        #[cfg(test)]
        {
            assert_eq!(
                TabPoint::from(self.transforms.summary().input.lines),
                self.tab_snapshot.max_point()
            );

            // Upstream: isomorphic and wrap transforms alternate. Here
            // isomorphic transforms may follow each other (they are capped),
            // but wraps never do, and no transform is empty.
            {
                let mut transforms = self.transforms.iter().peekable();
                while let Some(transform) = transforms.next() {
                    if transform.is_isomorphic() {
                        assert!(!transform.summary.input.lines.is_zero());
                    }
                    if let Some(next_transform) = transforms.peek() {
                        assert!(transform.is_isomorphic() || next_transform.is_isomorphic());
                    }
                }
            }

            let text = self.text();
            let text_lines: Vec<&str> = text.split('\n').collect();
            let mut expected_buffer_rows = Vec::new();
            let mut prev_tab_row = 0;
            for display_row in 0..=self.max_point().row().0 {
                let display_row = WrapRow(display_row);
                let tab_point = self.to_tab_point(WrapPoint::new(display_row, 0));
                if tab_point.row() == prev_tab_row && display_row != WrapRow(0) {
                    expected_buffer_rows.push(None);
                } else {
                    expected_buffer_rows.push(Some(self.buffer_line(tab_point.row())));
                }

                prev_tab_row = tab_point.row();
                assert_eq!(
                    self.line_len(display_row),
                    text_lines[display_row.0 as usize].len() as u32
                );
            }

            // Upstream checks every start row, which is quadratic; here the
            // first, the last and a few in between.
            let count = expected_buffer_rows.len();
            let step = (count / 4).max(1);
            for start_display_row in (0..count).step_by(step).chain([count - 1]) {
                assert_eq!(
                    self.row_infos(WrapRow(start_display_row as u32))
                        .map(|row_info| (!row_info.soft_wrapped).then_some(row_info.buffer_line))
                        .collect::<Vec<_>>(),
                    &expected_buffer_rows[start_display_row..],
                    "invalid buffer_rows({}..)",
                    start_display_row
                );
            }
        }
    }
}

/// Maps `TabPoint`s to `WrapPoint`s, reusing the cursor across calls. It is
/// most efficient when points are supplied in non-decreasing order.
#[derive(Debug)]
pub struct WrapPointCursor<'transforms> {
    cursor: Cursor<'transforms, 'static, Transform, Dimensions<TabPoint, WrapPoint>>,
}

impl WrapPointCursor<'_> {
    /// Resets the cursor to the start so it can seek backward again.
    pub fn reset(&mut self) {
        self.cursor.reset();
    }

    /// The wrap point of `point`, like
    /// [`WrapSnapshot::tab_point_to_wrap_point`].
    pub fn map(&mut self, point: TabPoint) -> WrapPoint {
        let cursor = &mut self.cursor;
        if cursor.did_seek() && point >= cursor.start().0 {
            cursor.seek_forward(&point, sum_tree::Bias::Right);
        } else {
            cursor.seek(&point, sum_tree::Bias::Right);
        }
        WrapPoint(cursor.start().1.0 + (point.0 - cursor.start().0.0))
    }
}

impl WrapChunks<'_> {
    /// Restarts the iterator on other rows of the same snapshot.
    pub fn seek(&mut self, rows: Range<WrapRow>) {
        let output_start = WrapPoint::new(rows.start, 0);
        let output_end = WrapPoint::new(rows.end, 0);
        self.transforms.seek(&output_start, sum_tree::Bias::Right);
        let mut input_start = TabPoint(self.transforms.start().1.0);
        if self.transforms.item().is_some_and(|t| t.is_isomorphic()) {
            input_start.0 += output_start.0 - self.transforms.start().0.0;
        }
        let input_end = self.snapshot.to_tab_point(output_end);
        let max_point = self.snapshot.tab_snapshot.max_point();
        let input_start = input_start.min(max_point);
        let input_end = input_end.min(max_point);
        self.input_chunks.seek(input_start..input_end);
        self.input_chunk = Chunk::default();
        self.output_position = output_start;
        self.max_output_row = rows.end;
        self.pending_display_text = "";
    }
}

/// A chunk of at most `MAX_CHUNK_LEN` bytes of a wrap's display text, with
/// its bitmaps, and the rest.
fn display_text_chunk(text: &'static str) -> (Chunk<'static>, &'static str) {
    let (head, rest) = text.split_at(text.len().min(MAX_CHUNK_LEN));
    (Chunk::new(head), rest)
}

impl<'a> Iterator for WrapChunks<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.pending_display_text.is_empty() {
            let (chunk, rest) = display_text_chunk(self.pending_display_text);
            self.pending_display_text = rest;
            return Some(chunk);
        }

        if self.output_position.row() >= self.max_output_row {
            return None;
        }

        let transform = self.transforms.item()?;
        if let Some(display_text) = transform.display_text {
            let mut start_ix = 0;
            let mut end_ix = display_text.len();
            let mut summary = transform.summary.output.lines;

            if self.output_position > self.transforms.start().0 {
                // Exclude newline starting prior to the desired row.
                start_ix = 1;
                summary.row = 0;
            } else if self.output_position.row() + WrapRow(1) >= self.max_output_row {
                // Exclude soft indentation ending after the desired row.
                end_ix = 1;
                summary.column = 0;
            }

            self.output_position.0 += summary;
            self.transforms.next();
            // Upstream yields the whole display text (up to 257 bytes) with
            // empty bitmaps; here it comes in chunks with their bitmaps, and
            // never as an empty chunk.
            let text = &display_text[start_ix..end_ix];
            if text.is_empty() {
                return self.next();
            }
            let (chunk, rest) = display_text_chunk(text);
            self.pending_display_text = rest;
            return Some(chunk);
        }

        if self.input_chunk.text.is_empty() {
            self.input_chunk = self.input_chunks.next()?;
        }

        let mut input_len = 0;
        let transform_end = self.transforms.end().0;
        for c in self.input_chunk.text.chars() {
            let char_len = c.len_utf8();
            input_len += char_len;
            if c == '\n' {
                *self.output_position.row_mut() += 1;
                *self.output_position.column_mut() = 0;
            } else {
                *self.output_position.column_mut() += char_len as u32;
            }

            if self.output_position >= transform_end {
                self.transforms.next();
                break;
            }
        }

        let (prefix, suffix) = self.input_chunk.text.split_at(input_len);

        let mask = 1u128.unbounded_shl(input_len as u32).wrapping_sub(1);
        let chars = self.input_chunk.chars & mask;
        let tabs = self.input_chunk.tabs & mask;
        let newlines = self.input_chunk.newlines & mask;
        self.input_chunk.tabs = self.input_chunk.tabs.unbounded_shr(input_len as u32);
        self.input_chunk.chars = self.input_chunk.chars.unbounded_shr(input_len as u32);
        self.input_chunk.newlines = self.input_chunk.newlines.unbounded_shr(input_len as u32);

        self.input_chunk.text = suffix;
        Some(Chunk {
            text: prefix,
            chars,
            tabs,
            newlines,
            ..self.input_chunk.clone()
        })
    }
}

impl Iterator for WrapRows<'_> {
    type Item = WrapRowInfo;

    fn next(&mut self) -> Option<Self::Item> {
        if self.output_row > self.max_output_row {
            return None;
        }

        let info = WrapRowInfo {
            tab_row: self.input_row,
            buffer_line: self.buffer_line,
            soft_wrapped: self.soft_wrapped,
        };

        self.output_row.0 += 1;
        self.transforms
            .seek_forward(&WrapPoint::new(self.output_row, 0), sum_tree::Bias::Left);
        if self.transforms.item().is_some_and(|t| t.is_isomorphic()) {
            self.input_row += 1;
            if self.output_row <= self.max_output_row {
                self.buffer_line = self.snapshot.buffer_line(self.input_row);
            }
            self.soft_wrapped = false;
        } else {
            self.soft_wrapped = true;
        }

        Some(info)
    }
}

impl Transform {
    fn isomorphic(summary: TextSummary) -> Self {
        debug_assert!(!summary.lines.is_zero());

        Self {
            summary: TransformSummary {
                input: summary,
                output: summary,
            },
            display_text: None,
        }
    }

    fn wrap(indent: u32) -> Self {
        let indent = indent.min(LineWrapper::MAX_INDENT);
        Self {
            summary: TransformSummary {
                input: TextSummary::default(),
                output: TextSummary {
                    lines: Point::new(1, indent),
                    first_line_chars: 0,
                    last_line_chars: indent,
                    longest_row: 1,
                    longest_row_chars: indent,
                },
            },
            display_text: Some(&WRAP_TEXT[..1 + indent as usize]),
        }
    }

    fn is_isomorphic(&self) -> bool {
        self.display_text.is_none()
    }

    /// Whether `other`, which follows, can be merged into this transform.
    fn can_absorb(&self, other: &Self) -> bool {
        self.is_isomorphic()
            && other.is_isomorphic()
            && self.summary.input.lines.row + other.summary.input.lines.row <= MAX_ISOMORPHIC_ROWS
    }
}

impl sum_tree::Item for Transform {
    type Summary = TransformSummary;

    fn summary(&self, _cx: ()) -> Self::Summary {
        self.summary.clone()
    }
}

fn push_isomorphic(transforms: &mut Vec<Transform>, summary: TextSummary) {
    let transform = Transform::isomorphic(summary);
    if let Some(last_transform) = transforms.last_mut()
        && last_transform.can_absorb(&transform)
    {
        last_transform.summary.input += &summary;
        last_transform.summary.output += &summary;
        return;
    }
    transforms.push(transform);
}

trait SumTreeExt {
    fn push_or_extend(&mut self, transform: Transform);
}

impl SumTreeExt for SumTree<Transform> {
    fn push_or_extend(&mut self, transform: Transform) {
        let mut transform = Some(transform);
        self.update_last(
            |last_transform| {
                if let Some(transform) =
                    transform.take_if(|transform| last_transform.can_absorb(transform))
                {
                    last_transform.summary.input += &transform.summary.input;
                    last_transform.summary.output += &transform.summary.output;
                }
            },
            (),
        );

        if let Some(transform) = transform {
            self.push(transform, ());
        }
    }
}

impl WrapPoint {
    /// Creates a point.
    pub fn new(row: WrapRow, column: u32) -> Self {
        Self(Point::new(row.0, column))
    }

    /// The row.
    pub fn row(self) -> WrapRow {
        WrapRow(self.0.row)
    }

    /// The row, mutably.
    pub fn row_mut(&mut self) -> &mut u32 {
        &mut self.0.row
    }

    /// The byte column, indent included.
    pub fn column(self) -> u32 {
        self.0.column
    }

    /// The byte column, mutably.
    pub fn column_mut(&mut self) -> &mut u32 {
        &mut self.0.column
    }
}

impl sum_tree::ContextLessSummary for TransformSummary {
    fn zero() -> Self {
        Default::default()
    }

    fn add_summary(&mut self, other: &Self) {
        self.input += &other.input;
        self.output += &other.output;
    }
}

impl<'a> sum_tree::Dimension<'a, TransformSummary> for TabPoint {
    fn zero(_cx: ()) -> Self {
        Default::default()
    }

    fn add_summary(&mut self, summary: &'a TransformSummary, _: ()) {
        self.0 += summary.input.lines;
    }
}

impl sum_tree::SeekTarget<'_, TransformSummary, TransformSummary> for TabPoint {
    fn cmp(&self, cursor_location: &TransformSummary, _: ()) -> std::cmp::Ordering {
        Ord::cmp(&self.0, &cursor_location.input.lines)
    }
}

impl<'a> sum_tree::Dimension<'a, TransformSummary> for WrapPoint {
    fn zero(_cx: ()) -> Self {
        Default::default()
    }

    fn add_summary(&mut self, summary: &'a TransformSummary, _: ()) {
        self.0 += summary.output.lines;
    }
}

/// Joins edits that touch or overlap. (Upstream does it in place with a
/// `scan`, asserting that the allocation is reused.)
fn consolidate_wrap_edits(edits: Vec<WrapEdit>) -> Vec<WrapEdit> {
    let mut merged: Vec<WrapEdit> = Vec::with_capacity(edits.len());
    for edit in edits {
        match merged.last_mut() {
            Some(prev_edit) if prev_edit.old.end >= edit.old.start => {
                prev_edit.old.end = edit.old.end;
                prev_edit.new.end = edit.new.end;
            }
            _ => merged.push(edit),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::display_map::replace_map::{ReplaceMap, ReplaceSnapshot};
    use crate::display_map::tab_map::TabMap;
    use crate::display_map::test_support::{
        ColumnMeasure, TestText, for_each_seed, random_edit, random_text,
    };
    use crate::display_map::text_measure::MonospaceMeasure;
    use crate::display_map::wrap_indent::MarkdownHangingIndent;
    use gv_text::Buffer;
    use rand::Rng;

    /// Zed's `SoftWrapIndent` setting, which upstream reads from the
    /// language settings; here a test-only [`WrapIndent`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SoftWrapIndent {
        None,
        Same,
        ExtraOne,
        ExtraTwo,
    }

    fn indent_adjustment_for(indent: SoftWrapIndent, tab_size: NonZeroU32) -> IndentAdjustment {
        match indent {
            SoftWrapIndent::None => IndentAdjustment::NoIndent,
            SoftWrapIndent::Same => IndentAdjustment::SameIndent,
            SoftWrapIndent::ExtraOne => IndentAdjustment::ExtraColumns(tab_size.get()),
            SoftWrapIndent::ExtraTwo => IndentAdjustment::ExtraColumns(tab_size.get() * 2),
        }
    }

    impl WrapIndent for SoftWrapIndent {
        fn indent(&self, tabs: &TabSnapshot, _row: u32, _line: &str) -> IndentAdjustment {
            indent_adjustment_for(*self, tabs.tab_size())
        }
    }

    // Upstream builds `MultiBuffer -> InlayMap -> FoldMap -> TabMap` and
    // measures the test font at 14 px, whose advance is 8.4 px; here every
    // character is one unit wide and the widths are in columns
    // (`px(14.0) * 15.0` is 25 columns).
    fn init_wrap_test(
        text: &str,
        tab_size: NonZeroU32,
        soft_wrapping: Option<f32>,
    ) -> (WrapMap, TabMap, ReplaceSnapshot) {
        let (_, replace_snapshot) = ReplaceMap::new(TextSnapshot::new(text));
        let (mut tab_map, _) = TabMap::new(replace_snapshot.clone(), tab_size);
        let tabs_snapshot = tab_map.set_max_expansion_column(32).0;
        let (wrap_map, _) = WrapMap::new(
            tabs_snapshot,
            Box::new(MonospaceMeasure::new(1.0)),
            soft_wrapping,
        );
        (wrap_map, tab_map, replace_snapshot)
    }

    #[test]
    fn test_soft_wrap_indent() {
        let text = "fn main() {\n    let some_variable = 1;\n    let some_variable = 2;\n}";
        let (mut wrap_map, _, _) = init_wrap_test(text, 4.try_into().unwrap(), Some(25.0));

        // Test None
        wrap_map.set_indent(Arc::new(SoftWrapIndent::None));
        let wrap_snapshot = wrap_map.snapshot();
        assert_eq!(
            wrap_snapshot.text(),
            "fn main() {\n    let some_variable = \n1;\n    let some_variable = \n2;\n}"
        );
        assert_eq!(wrap_snapshot.soft_wrap_indent(WrapRow(1)), Some(0));

        // Test Same
        wrap_map.set_indent(Arc::new(SoftWrapIndent::Same));
        let wrap_snapshot = wrap_map.snapshot();
        assert_eq!(
            wrap_snapshot.text(),
            "fn main() {\n    let some_variable = \n    1;\n    let some_variable = \n    2;\n}"
        );

        // Test ExtraOne
        wrap_map.set_indent(Arc::new(SoftWrapIndent::ExtraOne));
        let wrap_snapshot = wrap_map.snapshot();
        assert_eq!(
            wrap_snapshot.text(),
            "fn main() {\n    let some_variable = \n        1;\n    let some_variable = \n        2;\n}"
        );

        // Test ExtraTwo
        wrap_map.set_indent(Arc::new(SoftWrapIndent::ExtraTwo));
        let wrap_snapshot = wrap_map.snapshot();
        assert_eq!(
            wrap_snapshot.text(),
            "fn main() {\n    let some_variable = \n            1;\n    let some_variable = \n            2;\n}"
        );
    }

    #[test]
    fn test_soft_wrap_indent_updates_on_tab_size_change() {
        let text = "    let x = 1;\n";
        // Upstream: `px(14.0) * 8.0`, 13.3 columns.
        let (mut wrap_map, mut tab_map, replace_snapshot) =
            init_wrap_test(text, 4.try_into().unwrap(), Some(13.0));

        wrap_map.set_indent(Arc::new(SoftWrapIndent::ExtraOne));
        let wrap_snapshot = wrap_map.snapshot();
        assert_eq!(wrap_snapshot.text(), "    let x = \n        1;\n");

        // Change tab_size to 2
        let (tabs_snapshot, tab_edits) =
            tab_map.sync(replace_snapshot, Vec::new(), 2.try_into().unwrap());
        let (wrap_snapshot, _) = wrap_map.sync(tabs_snapshot, tab_edits);
        assert_eq!(wrap_snapshot.text(), "    let x = \n      1;\n");
    }

    #[test]
    fn test_prev_row_boundary() {
        fn test_wrap_snapshot(
            text: &str,
            soft_wrap_every: usize, // font size multiple
        ) -> WrapSnapshot {
            let tab_size = 4.try_into().unwrap();
            // Upstream estimates `font_size * soft_wrap_every * 0.6` for its
            // test font; with one-unit characters it is exact.
            let soft_wrapping = Some(soft_wrap_every as f32);

            let (_, replace_snapshot) = ReplaceMap::new(TextSnapshot::new(text));
            let (mut tab_map, _) = TabMap::new(replace_snapshot, tab_size);
            let tabs_snapshot = tab_map.set_max_expansion_column(32).0;
            let (_wrap_map, wrap_snapshot) = WrapMap::new(
                tabs_snapshot,
                Box::new(MonospaceMeasure::new(1.0)),
                soft_wrapping,
            );

            wrap_snapshot
        }

        // These two should pass but dont, see the comparison note in
        // prev_row_boundary about why.
        //
        // //                                      0123  4567  wrap_rows
        // let wrap_snapshot = test_wrap_snapshot("1234\n5678", 1);
        // assert_eq!(wrap_snapshot.text(), "1\n2\n3\n4\n5\n6\n7\n8");
        // let row = wrap_snapshot.prev_row_boundary(wrap_snapshot.max_point());
        // assert_eq!(row.0, 3);

        // //                                      012  345  678  wrap_rows
        // let wrap_snapshot = test_wrap_snapshot("123\n456\n789", 1);
        // assert_eq!(wrap_snapshot.text(), "1\n2\n3\n4\n5\n6\n7\n8\n9");
        // let row = wrap_snapshot.prev_row_boundary(wrap_snapshot.max_point());
        // assert_eq!(row.0, 5);

        //                                      012345678  wrap_rows
        let wrap_snapshot = test_wrap_snapshot("123456789", 1);
        assert_eq!(wrap_snapshot.text(), "1\n2\n3\n4\n5\n6\n7\n8\n9");
        let row = wrap_snapshot.prev_row_boundary(wrap_snapshot.max_point());
        assert_eq!(row.0, 0);

        //                                      111  2222    44  wrap_rows
        let wrap_snapshot = test_wrap_snapshot("123\n4567\n\n89", 4);
        assert_eq!(wrap_snapshot.text(), "123\n4567\n\n89");
        let row = wrap_snapshot.prev_row_boundary(wrap_snapshot.max_point());
        assert_eq!(row.0, 2);

        //                                      11  2223   wrap_rows
        let wrap_snapshot = test_wrap_snapshot("12\n3456\n\n", 3);
        assert_eq!(wrap_snapshot.text(), "12\n345\n6\n\n");
        let row = wrap_snapshot.prev_row_boundary(wrap_snapshot.max_point());
        assert_eq!(row.0, 3);
    }

    // Upstream `test_invisibles_wrap_at_replacement_glyph_width`: GPUI's
    // wrap map turns invisible characters into elements measured as their
    // replacement glyph. Here the measure answers for the glyph the view
    // draws, so the same guarantee holds when it does. (The companion test
    // `test_invisibles_become_width_measured_elements` checks upstream's
    // fragment builder, which is not ported.)
    #[test]
    fn test_invisibles_wrap_at_replacement_glyph_width() {
        #[derive(Clone, Copy)]
        struct ReplacementGlyphs;
        impl ReplacementGlyphs {
            fn width(c: char) -> f32 {
                if c.is_control() { 1.7 } else { 1.0 }
            }
        }
        impl TextMeasure for ReplacementGlyphs {
            fn char_width(&mut self, c: char) -> f32 {
                Self::width(c)
            }
        }

        let wrap_width = 10.0;
        let text = "\u{7f}\u{80}\u{81}\u{82}\u{83}\u{84}".repeat(20);
        let (_, replace_snapshot) = ReplaceMap::new(TextSnapshot::new(&text));
        let (mut tab_map, _) = TabMap::new(replace_snapshot, 4.try_into().unwrap());
        let tabs_snapshot = tab_map.set_max_expansion_column(32).0;
        let (_wrap_map, wrap_snapshot) =
            WrapMap::new(tabs_snapshot, Box::new(ReplacementGlyphs), Some(wrap_width));

        let wrapped_text = wrap_snapshot.text();
        assert!(wrapped_text.lines().count() > 1);
        for row in wrapped_text.split('\n') {
            let rendered_width: f32 = row.chars().map(ReplacementGlyphs::width).sum();
            assert!(
                rendered_width <= wrap_width,
                "a soft-wrapped row must fit the wrap width when drawn with the \
                 replacement glyphs the renderer substitutes for invisible characters, \
                 but this row renders at {rendered_width:?} against a wrap width of \
                 {wrap_width:?}: {row:?}"
            );
        }
    }

    #[test]
    fn test_random_wraps() {
        let operations = std::env::var("OPERATIONS")
            .map(|i| i.parse().expect("invalid `OPERATIONS` variable"))
            .unwrap_or(10);

        for_each_seed(100, |mut rng| {
            // Upstream picks 0 to 1000 px at 8.4 px per character.
            let random_width = |rng: &mut rand::rngs::StdRng, none: f64| {
                if rng.random_bool(none) {
                    None
                } else {
                    Some(rng.random_range(0.0..=120.0f32))
                }
            };
            let mut wrap_width = random_width(&mut rng, 0.1);
            let mut tab_size = NonZeroU32::new(rng.random_range(1..=4)).unwrap();

            // Upstream: `MultiBuffer::build_random` or a short text.
            let len = if rng.random() {
                rng.random_range(0..300)
            } else {
                rng.random_range(0..10)
            };
            let mut buffer = Buffer::new(&random_text(&mut rng, len));
            let (mut replace_map, replace_snapshot) = ReplaceMap::new(buffer.snapshot());
            let (mut tab_map, _) = TabMap::new(replace_snapshot, tab_size);
            let tabs_snapshot = tab_map.set_max_expansion_column(32).0;

            let mut line_wrapper = LineWrapper::new(Box::new(ColumnMeasure));
            let mut soft_wrap_indent = SoftWrapIndent::Same;
            let expected_text = wrap_text(
                &tabs_snapshot,
                wrap_width,
                indent_adjustment_for(soft_wrap_indent, tab_size),
                &mut line_wrapper,
            );

            let (mut wrap_map, initial_snapshot) = WrapMap::with_indent(
                tabs_snapshot.clone(),
                Box::new(ColumnMeasure),
                Arc::new(soft_wrap_indent),
                wrap_width,
            );
            assert!(!wrap_map.is_rewrapping());
            assert_eq!(
                initial_snapshot.text(),
                expected_text,
                "unwrapped text is: {:?}",
                tabs_snapshot.text()
            );

            // Upstream re-wraps on a background task; here a job runs in
            // steps with its own wrapper while edits keep coming.
            let mut job: Option<RewrapJob> = None;
            let mut background_wrapper = LineWrapper::new(Box::new(ColumnMeasure));
            let mut edits = Vec::new();
            for _ in 0..operations {
                let mut summary = None;
                match rng.random_range(0..=100) {
                    0..=19 => {
                        wrap_width = random_width(&mut rng, 0.2);
                        soft_wrap_indent = match rng.random_range(0..=3) {
                            0 => SoftWrapIndent::None,
                            1 => SoftWrapIndent::Same,
                            2 => SoftWrapIndent::ExtraOne,
                            _ => SoftWrapIndent::ExtraTwo,
                        };
                        edits.push(wrap_map.set_indent(Arc::new(soft_wrap_indent)));
                        if let Some(stale) = job.take() {
                            assert!(wrap_map.finish_rewrap(stale).is_none(), "stale job");
                        }
                        if rng.random() {
                            job = wrap_map.defer_wrap_width(wrap_width);
                        } else {
                            edits.push(wrap_map.set_wrap_width(wrap_width));
                        }
                    }
                    // Upstream mutates random folds (20 %) and inlays
                    // (20 %); the replace layer has neither yet.
                    20..=29 => {
                        let max_row = wrap_map.snapshot().tab_snapshot().max_point().row();
                        let start = rng.random_range(0..=max_row + 1);
                        let end = rng.random_range(start..=max_row + 2);
                        edits.push(wrap_map.refresh_rows(start..end));
                    }
                    30..=39 => tab_size = NonZeroU32::new(rng.random_range(1..=4)).unwrap(),
                    _ => summary = Some(random_edit(&mut rng, &mut buffer)),
                }

                let (replace_snapshot, replace_edits) = match &summary {
                    Some(summary) => replace_map.sync(buffer.snapshot(), summary),
                    None => (replace_map.snapshot(), Vec::new()),
                };
                let (tabs_snapshot, tab_edits) =
                    tab_map.sync(replace_snapshot, replace_edits, tab_size);

                let expected_text = wrap_text(
                    &tabs_snapshot,
                    wrap_width,
                    indent_adjustment_for(soft_wrap_indent, tab_size),
                    &mut line_wrapper,
                );
                let (snapshot, wrap_edits) = wrap_map.sync(tabs_snapshot.clone(), tab_edits);
                snapshot.check_invariants();
                snapshot.verify_chunks(&mut rng);
                edits.push((snapshot, wrap_edits));

                if let Some(running) = &mut job
                    && rng.random_bool(0.4)
                {
                    let done = running.run(&mut background_wrapper, rng.random_range(1..=20));
                    if done || rng.random() {
                        let finished = job.take().and_then(|job| wrap_map.finish_rewrap(job));
                        edits.push(finished.expect("the current job"));
                    }
                }

                if !wrap_map.is_rewrapping() {
                    let (wrapped_snapshot, wrap_edits) =
                        wrap_map.sync(tabs_snapshot.clone(), Vec::new());
                    let actual_text = wrapped_snapshot.text();
                    let actual_longest_row = wrapped_snapshot.longest_row();
                    wrapped_snapshot.check_invariants();
                    wrapped_snapshot.verify_chunks(&mut rng);
                    assert!(!wrapped_snapshot.is_interpolated());
                    edits.push((wrapped_snapshot.clone(), wrap_edits));
                    assert_eq!(
                        actual_text,
                        expected_text,
                        "unwrapped text is: {:?}",
                        tabs_snapshot.text()
                    );

                    if tab_size.get() == 1
                        || !wrapped_snapshot
                            .tab_snapshot()
                            .replace_snapshot()
                            .text()
                            .contains('\t')
                    {
                        let mut expected_longest_rows = Vec::new();
                        let mut longest_line_len = -1;
                        for (row, line) in expected_text.split('\n').enumerate() {
                            let line_char_count = line.chars().count() as isize;
                            if line_char_count > longest_line_len {
                                expected_longest_rows.clear();
                                longest_line_len = line_char_count;
                            }
                            if line_char_count >= longest_line_len {
                                expected_longest_rows.push(row as u32);
                            }
                        }

                        assert!(
                            expected_longest_rows.contains(&actual_longest_row),
                            "incorrect longest row {}. expected {:?} with length {}",
                            actual_longest_row,
                            expected_longest_rows,
                            longest_line_len,
                        )
                    }
                }
            }

            let mut initial_text = initial_snapshot.text();
            for (snapshot, patch) in edits {
                let snapshot_text = TestText::new(&snapshot.text());
                for edit in &patch {
                    let current = TestText::new(&initial_text);
                    let old_start = current.point_to_offset(Point::new(edit.new.start.0, 0));
                    let old_end = current.point_to_offset(cmp::min(
                        Point::new(edit.new.start.0 + (edit.old.end.0 - edit.old.start.0), 0),
                        current.max_point(),
                    ));
                    let new_text = snapshot_text
                        .slice(
                            Point::new(edit.new.start.0, 0)
                                ..cmp::min(
                                    Point::new(edit.new.end.0, 0),
                                    snapshot_text.max_point(),
                                ),
                        )
                        .to_owned();

                    initial_text.replace_range(old_start..old_end, &new_text);
                }
                assert_eq!(initial_text, snapshot_text.text());
            }

            if let Some(job) = job {
                assert!(wrap_map.finish_rewrap(job).is_some());
            }
            assert!(!wrap_map.is_rewrapping());
        });
    }

    fn wrap_text(
        tab_snapshot: &TabSnapshot,
        wrap_width: Option<f32>,
        indent_adjustment: IndentAdjustment,
        line_wrapper: &mut LineWrapper,
    ) -> String {
        if let Some(wrap_width) = wrap_width {
            let mut wrapped_text = String::new();
            for (row, line) in tab_snapshot.text().split('\n').enumerate() {
                if row > 0 {
                    wrapped_text.push('\n');
                }

                let mut prev_ix = 0;
                for boundary in line_wrapper.wrap_line(
                    &[LineFragment::text(line)],
                    wrap_width,
                    indent_adjustment,
                ) {
                    wrapped_text.push_str(&line[prev_ix..boundary.ix]);
                    wrapped_text.push('\n');
                    wrapped_text.push_str(&" ".repeat(boundary.next_indent as usize));
                    prev_ix = boundary.ix;
                }
                wrapped_text.push_str(&line[prev_ix..]);
            }

            wrapped_text
        } else {
            tab_snapshot.text()
        }
    }

    impl WrapSnapshot {
        fn verify_chunks(&self, rng: &mut impl Rng) {
            for _ in 0..5 {
                let mut end_row = rng.random_range(0..=self.max_point().row().0);
                let start_row = rng.random_range(0..=end_row);
                end_row += 1;

                let mut expected_text = self.text_chunks(WrapRow(start_row)).collect::<String>();
                if expected_text.ends_with('\n') {
                    expected_text.push('\n');
                }
                let mut expected_text = expected_text
                    .lines()
                    .take((end_row - start_row) as usize)
                    .collect::<Vec<_>>()
                    .join("\n");
                if end_row <= self.max_point().row().0 {
                    expected_text.push('\n');
                }

                let chunks: Vec<_> = self.chunks(WrapRow(start_row)..WrapRow(end_row)).collect();
                // Glyphvane: chunks are short and their bitmaps are right.
                for chunk in &chunks {
                    assert!(!chunk.text.is_empty() && chunk.text.len() <= MAX_CHUNK_LEN);
                    let expected = Chunk::new(chunk.text);
                    assert_eq!(
                        (chunk.chars, chunk.newlines),
                        (expected.chars, expected.newlines),
                        "bitmaps of {:?}",
                        chunk.text
                    );
                }
                let actual_text = chunks.iter().map(|c| c.text).collect::<String>();
                assert_eq!(
                    expected_text,
                    actual_text,
                    "chunks != highlighted_chunks for rows {:?}",
                    start_row..end_row
                );
            }
        }
    }

    // ----- Glyphvane tests ------------------------------------------------

    fn wrapped(text: &str, width: f32, indent: Arc<dyn WrapIndent>) -> WrapSnapshot {
        let (_, replace_snapshot) = ReplaceMap::new(TextSnapshot::new(text));
        let (_, tabs) = TabMap::new(replace_snapshot, 4.try_into().unwrap());
        WrapMap::with_indent(
            tabs,
            Box::new(MonospaceMeasure::new(1.0)),
            indent,
            Some(width),
        )
        .1
    }

    #[test]
    fn markdown_hanging_indent() {
        let snapshot = wrapped(
            "- one two three four\n  > quoted text here\nplain text goes on\n",
            12.0,
            Arc::new(MarkdownHangingIndent::default()),
        );
        assert_eq!(
            snapshot.text(),
            "- one two \n  three four\n  > quoted \n    text \n    here\nplain text \ngoes on\n"
        );
        assert_eq!(snapshot.soft_wrap_indent(WrapRow(0)), Some(2));
        assert_eq!(snapshot.soft_wrap_indent(WrapRow(1)), None);
        assert_eq!(snapshot.soft_wrap_indent(WrapRow(2)), Some(4));
        assert_eq!(snapshot.soft_wrap_indent(WrapRow(3)), Some(4));
        assert_eq!(snapshot.soft_wrap_indent(WrapRow(5)), Some(0));

        // Spaces count toward the width: at 10 columns `  > quoted ` does
        // not fit, and the indent is dropped where the carried word would
        // not fit after it.
        let snapshot = wrapped(
            "  > quoted text here",
            10.0,
            Arc::new(MarkdownHangingIndent::default()),
        );
        assert_eq!(snapshot.text(), "  > \nquoted \n    text \n    here");
    }

    #[test]
    fn point_conversions_at_a_soft_wrap() {
        // `aa bbb ` | `  cc`: the wrap is after the space, at tab column 7.
        let snapshot = wrapped(
            "  aa bbb cc\nx",
            9.0,
            Arc::new(IndentAdjustment::SameIndent),
        );
        assert_eq!(snapshot.text(), "  aa bbb \n  cc\nx");
        let boundary = TabPoint::new(0, 9);
        // The soft-wrap position belongs to the continuation row.
        assert_eq!(
            snapshot.tab_point_to_wrap_point(boundary),
            WrapPoint::new(WrapRow(1), 2)
        );
        // Inside the indent: the start of the row's text.
        for column in 0..=2 {
            assert_eq!(
                snapshot.to_tab_point(WrapPoint::new(WrapRow(1), column)),
                boundary
            );
        }
        // Clipping out of the indent and off the end of the wrapped row.
        assert_eq!(
            snapshot.clip_point(WrapPoint::new(WrapRow(1), 1), Bias::Right),
            WrapPoint::new(WrapRow(1), 2)
        );
        assert_eq!(
            snapshot.clip_point(WrapPoint::new(WrapRow(1), 1), Bias::Left),
            WrapPoint::new(WrapRow(0), 8)
        );
        assert_eq!(
            snapshot.clip_point(WrapPoint::new(WrapRow(0), 9), Bias::Left),
            WrapPoint::new(WrapRow(0), 8)
        );
        assert_eq!(
            snapshot.clip_point(WrapPoint::new(WrapRow(0), 9), Bias::Right),
            WrapPoint::new(WrapRow(1), 2)
        );
        // Buffer points, and the row after the wrapped one.
        assert_eq!(
            snapshot.buffer_point_to_wrap_point(BufferPoint::new(1, 1), Bias::Left),
            WrapPoint::new(WrapRow(2), 1)
        );
        assert_eq!(
            snapshot.wrap_point_to_buffer_point(WrapPoint::new(WrapRow(1), 3), Bias::Left),
            BufferPoint::new(0, 10)
        );
        assert_eq!(snapshot.line_len(WrapRow(0)), 9);
        assert_eq!(snapshot.line_len(WrapRow(1)), 4);
        let mut cursor = snapshot.wrap_point_cursor();
        assert_eq!(
            cursor.map(TabPoint::new(0, 3)),
            WrapPoint::new(WrapRow(0), 3)
        );
        assert_eq!(cursor.map(boundary), WrapPoint::new(WrapRow(1), 2));
        assert_eq!(
            cursor.map(TabPoint::new(1, 1)),
            WrapPoint::new(WrapRow(2), 1)
        );
    }

    #[test]
    fn row_infos_and_summaries() {
        let snapshot = wrapped(
            "aaaa bbbb\ncc\r\ndddd eeee ffff",
            5.0,
            Arc::new(IndentAdjustment::NoIndent),
        );
        assert_eq!(snapshot.text(), "aaaa \nbbbb\ncc\ndddd \neeee \nffff");
        let infos: Vec<_> = snapshot
            .row_infos(WrapRow(0))
            .map(|info| (info.tab_row, info.buffer_line, info.soft_wrapped))
            .collect();
        assert_eq!(
            infos,
            [
                (0, 0, false),
                (0, 0, true),
                (1, 1, false),
                (2, 2, false),
                (2, 2, true),
                (2, 2, true),
            ]
        );
        let mut rows = snapshot.row_infos(WrapRow(0));
        rows.seek(WrapRow(4));
        assert_eq!(rows.next().map(|info| info.soft_wrapped), Some(true));
        assert_eq!(
            snapshot.text_summary_for_range(WrapRow(1)..WrapRow(4)),
            TextSummary::from("bbbb\ncc\ndddd \n")
        );
        // As upstream, only transform starts are considered: row 2 is found
        // looking back, nothing looking forward.
        assert_eq!(
            snapshot.prev_row_boundary(WrapPoint::new(WrapRow(5), 2)),
            WrapRow(2)
        );
        assert_eq!(
            snapshot.next_row_boundary(WrapPoint::new(WrapRow(0), 0)),
            None
        );
        let unwrapped_first = wrapped("cc\naaaa bbbb", 5.0, Arc::new(IndentAdjustment::NoIndent));
        assert_eq!(
            unwrapped_first.next_row_boundary(WrapPoint::new(WrapRow(0), 0)),
            Some(WrapRow(1))
        );
        // Seeking chunks.
        let mut chunks = snapshot.chunks(WrapRow(0)..WrapRow(1));
        chunks.seek(WrapRow(4)..WrapRow(6));
        assert_eq!(chunks.map(|c| c.text).collect::<String>(), "eeee \nffff");
    }

    #[test]
    fn indents_longer_than_a_chunk() {
        // 200 columns of leading whitespace: the continuation rows' display
        // text (a line break and 200 spaces) comes in chunks.
        let text = format!("{}{}", " ".repeat(200), "word ".repeat(40));
        let snapshot = wrapped(&text, 220.0, Arc::new(IndentAdjustment::SameIndent));
        assert_eq!(snapshot.soft_wrap_indent(WrapRow(0)), Some(200));
        for chunk in snapshot.chunks(WrapRow(0)..snapshot.max_point().row() + WrapRow(1)) {
            assert!(chunk.text.len() <= MAX_CHUNK_LEN);
            assert_eq!(chunk.chars, Chunk::new(chunk.text).chars);
        }
        let text = snapshot.text();
        assert!(
            text.lines().all(|row| row.chars().count() <= 220),
            "{text:?}"
        );
        assert!(
            text.split('\n')
                .nth(1)
                .unwrap()
                .starts_with(&" ".repeat(200))
        );
    }

    #[test]
    fn rewrap_job_in_steps_with_edits_meanwhile() {
        let line = "the quick brown fox jumps over the lazy dog\n";
        let mut buffer = Buffer::new(&line.repeat(50));
        let tab_size = NonZeroU32::new(4).unwrap();
        let (mut replace_map, replace_snapshot) = ReplaceMap::new(buffer.snapshot());
        let (mut tab_map, tabs) = TabMap::new(replace_snapshot, tab_size);
        let measure = || Box::new(MonospaceMeasure::new(1.0));
        let (mut wrap_map, _) = WrapMap::new(tabs, measure(), Some(30.0));

        let mut job = wrap_map.defer_wrap_width(Some(12.0)).unwrap();
        assert!(wrap_map.is_rewrapping());
        assert!(wrap_map.snapshot().is_interpolated());
        assert_eq!(job.row_count(), 51);
        let mut wrapper = LineWrapper::new(measure());
        assert!(!job.run(&mut wrapper, 10));
        assert_eq!(job.rows_done(), 10);

        // Edits while the job is out are interpolated, then re-applied.
        let summary = buffer.edit([(0..0, "jumping ")]).unwrap();
        let (replace_snapshot, replace_edits) = replace_map.sync(buffer.snapshot(), &summary);
        let (tabs, tab_edits) = tab_map.sync(replace_snapshot, replace_edits, tab_size);
        let (interpolated, edits) = wrap_map.sync(tabs.clone(), tab_edits);
        assert!(interpolated.is_interpolated());
        assert_eq!(edits.len(), 1);

        // A job from before a newer re-wrap is stale.
        let newer = wrap_map.begin_rewrap();
        assert!(wrap_map.finish_rewrap(job).is_none());
        assert!(wrap_map.is_rewrapping());

        let (finished, edits) = wrap_map.finish_rewrap(newer).unwrap();
        assert!(!wrap_map.is_rewrapping());
        assert!(!finished.is_interpolated());
        let fresh = WrapMap::new(tabs, measure(), Some(12.0)).1;
        assert_eq!(finished.text(), fresh.text());
        assert_eq!(
            edits,
            [WrapEdit {
                old: WrapRow(0)..WrapRow(interpolated.max_point().row().0 + 1),
                new: WrapRow(0)..WrapRow(finished.max_point().row().0 + 1),
            }]
        );
        assert!(finished.text().starts_with("jumping the \nquick brown \n"));
    }

    #[test]
    fn rewrap_job_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<RewrapJob>();
        assert_send::<WrapSnapshot>();
    }

    #[test]
    fn isomorphic_transforms_are_capped() {
        let mut buffer = Buffer::new(&"short line\n".repeat(1000));
        let tab_size = NonZeroU32::new(4).unwrap();
        let (mut replace_map, replace_snapshot) = ReplaceMap::new(buffer.snapshot());
        let (mut tab_map, tabs) = TabMap::new(replace_snapshot, tab_size);
        let (mut wrap_map, snapshot) =
            WrapMap::new(tabs, Box::new(MonospaceMeasure::new(1.0)), Some(80.0));
        let rows = |snapshot: &WrapSnapshot| {
            snapshot
                .transforms
                .iter()
                .map(|transform| transform.summary.input.lines.row)
                .collect::<Vec<_>>()
        };
        assert!(
            rows(&snapshot)
                .iter()
                .all(|&rows| rows <= MAX_ISOMORPHIC_ROWS)
        );
        assert!(rows(&snapshot).len() >= 1000 / MAX_ISOMORPHIC_ROWS as usize);

        for offset in [5, 5000, 10_995] {
            let summary = buffer.edit([(offset..offset, "x\ny")]).unwrap();
            let (replace_snapshot, replace_edits) = replace_map.sync(buffer.snapshot(), &summary);
            let (tabs, tab_edits) = tab_map.sync(replace_snapshot, replace_edits, tab_size);
            let (snapshot, _) = wrap_map.sync(tabs, tab_edits);
            assert!(
                rows(&snapshot)
                    .iter()
                    .all(|&rows| rows <= MAX_ISOMORPHIC_ROWS),
                "{:?}",
                rows(&snapshot)
            );
        }
    }

    #[test]
    fn no_wrap_width_is_the_tab_text() {
        let text = "a\tb\r\n\nc d e f g h\r";
        let (_, replace_snapshot) = ReplaceMap::new(TextSnapshot::new(text));
        let (_, tabs) = TabMap::new(replace_snapshot, 4.try_into().unwrap());
        let (mut wrap_map, snapshot) =
            WrapMap::new(tabs.clone(), Box::new(MonospaceMeasure::new(1.0)), None);
        assert_eq!(snapshot.text(), tabs.text());
        assert_eq!(wrap_map.set_wrap_width(None).1, []);
        let (wrapped, edits) = wrap_map.set_wrap_width(Some(4.0));
        assert_eq!(wrapped.text(), "a   \nb\n\nc d \ne f \ng h\n");
        assert_eq!(edits.len(), 1);
        // NaN means no wrapping.
        assert_eq!(
            wrap_map.set_wrap_width(Some(f32::NAN)).0.text(),
            tabs.text()
        );
        assert_eq!(wrap_map.wrap_width(), None);
    }
}

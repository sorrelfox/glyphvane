// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors
//
// Ported from Zed (https://github.com/zed-industries/zed),
// crates/editor/src/display_map/tab_map.rs at commit
// d2a4fb6ded7c470be25d1541281da3882495b65b.
// Copyright (C) Zed Industries, Inc. and the Zed contributors.
// Licensed under the GNU General Public License, version 3 or later.
//
// Modified by the Glyphvane contributors (2026-11-13): the layer sits on
// gv-editor's replace layer and gv-text instead of Zed's fold map and
// multi-buffer, edits and chunk ranges are points instead of offsets,
// highlighting, row infos, tracing and `unsafe` are removed, and the tests
// run without GPUI. The full list is in third_party/zed/PROVENANCE.md.
// (2026-11-16): chunks take caller highlights and pass them to the replace
// layer.

//! The tab layer: expands hard tabs to the next tab stop.
//!
//! A tab at expanded `char` column `c` becomes `tab_size - c % tab_size`
//! spaces. Tabs at or after byte column `max_expansion_column` of the input
//! row (256 by default) become a single space, so very long rows do not
//! need a scan from the row start. A [`TabPoint`] is a row and a byte
//! column in the expanded text.

use std::cmp;
use std::num::NonZeroU32;
use std::ops::Range;

use gv_text::{Bias, TextSnapshot};

use super::BufferPoint;
use super::primitives::{Chunk, Edit, Highlight, MAX_CHUNK_LEN, Point, TextSummary};
use super::replace_map::{ReplaceChunks, ReplaceEdit, ReplacePoint, ReplaceSnapshot};

/// The default for [`TabSnapshot::max_expansion_column`].
pub const MAX_EXPANSION_COLUMN: u32 = 256;
const MAX_ROW_SCAN_CHUNKS: usize = 8;

const SPACE_BYTES: [u8; MAX_CHUNK_LEN] = [b' '; MAX_CHUNK_LEN];
// Handles a tab width <= 128
const SPACES: &str = match std::str::from_utf8(&SPACE_BYTES) {
    Ok(spaces) => spaces,
    Err(_) => panic!("spaces are ASCII"),
};
/// The largest tab size; larger sizes are reduced to it.
pub const MAX_TAB_SIZE: NonZeroU32 = match NonZeroU32::new(SPACES.len() as u32) {
    Some(size) => size,
    None => panic!("SPACES is not empty"),
};

/// Keeps track of hard tabs in a text buffer.
#[derive(Debug)]
pub struct TabMap(TabSnapshot);

impl TabMap {
    /// A tab layer over `replace_snapshot`, and its first snapshot.
    /// `tab_size` is capped at [`MAX_TAB_SIZE`].
    pub fn new(replace_snapshot: ReplaceSnapshot, tab_size: NonZeroU32) -> (Self, TabSnapshot) {
        let snapshot = TabSnapshot {
            replace_snapshot,
            tab_size: tab_size.min(MAX_TAB_SIZE),
            max_expansion_column: MAX_EXPANSION_COLUMN,
            version: 0,
        };
        (Self(snapshot.clone()), snapshot)
    }

    /// Changes the byte column from which tabs no longer expand. Every row
    /// may change, so the edit covers the whole text (none if the column
    /// stays the same).
    pub fn set_max_expansion_column(&mut self, column: u32) -> (TabSnapshot, Vec<TabEdit>) {
        if self.0.max_expansion_column == column {
            return (self.0.clone(), Vec::new());
        }
        let old_max_point = self.0.max_point();
        self.0.max_expansion_column = column;
        self.0.version += 1;
        (
            self.0.clone(),
            vec![TabEdit {
                old: TabPoint::zero()..old_max_point,
                new: TabPoint::zero()..self.0.max_point(),
            }],
        )
    }

    /// Moves the layer to `replace_snapshot`, which `replace_edits`
    /// produced from the current input snapshot, and returns the new
    /// snapshot with the edits in tab coordinates. A changed `tab_size`
    /// reports the whole text as edited.
    pub fn sync(
        &mut self,
        replace_snapshot: ReplaceSnapshot,
        mut replace_edits: Vec<ReplaceEdit>,
        tab_size: NonZeroU32,
    ) -> (TabSnapshot, Vec<TabEdit>) {
        let tab_size = tab_size.min(MAX_TAB_SIZE);

        if self.0.tab_size != tab_size {
            let old_max_point = self.0.max_point();
            self.0.version += 1;
            self.0.replace_snapshot = replace_snapshot;
            self.0.tab_size = tab_size;
            return (
                self.0.clone(),
                vec![TabEdit {
                    old: TabPoint::zero()..old_max_point,
                    new: TabPoint::zero()..self.0.max_point(),
                }],
            );
        }

        let old_snapshot = &mut self.0;
        let mut new_version = old_snapshot.version;
        if old_snapshot.replace_snapshot.version() != replace_snapshot.version() {
            new_version += 1;
        }

        if replace_edits.is_empty() {
            old_snapshot.version = new_version;
            old_snapshot.replace_snapshot = replace_snapshot;
            old_snapshot.tab_size = tab_size;
            return (old_snapshot.clone(), vec![]);
        }

        // Expand each edit to include the next tab on the same line as the edit,
        // and any subsequent tabs on that line that moved across the tab expansion
        // boundary.
        //
        // This is necessary because a tab's display width depends on its column
        // position: it expands to fill up to the next tab stop. When an edit
        // shifts text on a line, any tab character after the edit may now render
        // at a different width even though the tab byte itself wasn't touched.
        // Additionally, tabs beyond `max_expansion_column` are rendered as a
        // single space instead of expanding to the next tab stop. An edit that
        // shifts a tab across that boundary changes its display width, so the
        // edit must cover it. We scan forward from the edit end to the end of
        // the line, extending the edit to include the first subsequent tab (whose
        // rendered width may have changed) and the last tab that crossed the
        // expansion boundary (transitioning between expanded and non-expanded).
        let old_max_point = old_snapshot.replace_snapshot.max_point();
        let mut old_chunks: Option<ReplaceChunks<'_>> = None;
        for replace_edit in &mut replace_edits {
            let old_end = replace_edit.old.end;
            let new_end = replace_edit.new.end;

            let chunks = match &mut old_chunks {
                Some(chunks) => {
                    chunks.seek(replace_edit.old.end..old_max_point);
                    chunks
                }
                None => old_chunks.insert(
                    old_snapshot
                        .replace_snapshot
                        .chunks(replace_edit.old.end..old_max_point),
                ),
            };

            let mut offset_from_edit = 0;
            let mut first_tab_offset = None;
            let mut last_tab_with_changed_expansion_offset = None;
            'outer: for chunk in chunks {
                let row_end = chunk.newlines.trailing_zeros();
                let mut remaining_tabs = chunk.tabs & !u128::MAX.unbounded_shl(row_end);
                while remaining_tabs != 0 {
                    let ix = remaining_tabs.trailing_zeros();
                    let offset_from_edit = offset_from_edit + ix;
                    if first_tab_offset.is_none() {
                        first_tab_offset = Some(offset_from_edit);
                    }

                    let old_column = old_end.column() + offset_from_edit;
                    let new_column = new_end.column() + offset_from_edit;
                    let was_expanded = old_column < old_snapshot.max_expansion_column;
                    let is_expanded = new_column < old_snapshot.max_expansion_column;
                    if was_expanded != is_expanded {
                        last_tab_with_changed_expansion_offset = Some(offset_from_edit);
                    } else if !was_expanded && !is_expanded {
                        break 'outer;
                    }

                    remaining_tabs &= remaining_tabs - 1;
                }

                if chunk.newlines != 0 {
                    break;
                }

                offset_from_edit += chunk.text.len() as u32;
                if old_end.column() + offset_from_edit >= old_snapshot.max_expansion_column
                    && new_end.column() + offset_from_edit >= old_snapshot.max_expansion_column
                {
                    break;
                }
            }

            if let Some(offset) = last_tab_with_changed_expansion_offset.or(first_tab_offset) {
                replace_edit.old.end.0.column += offset + 1;
                replace_edit.new.end.0.column += offset + 1;
            }
        }
        drop(old_chunks);

        let new_snapshot = TabSnapshot {
            replace_snapshot,
            tab_size,
            max_expansion_column: old_snapshot.max_expansion_column,
            version: new_version,
        };

        // Combine any edits that overlap due to the expansion.
        let mut merged: Vec<ReplaceEdit> = Vec::with_capacity(replace_edits.len());
        for edit in replace_edits {
            match merged.last_mut() {
                Some(last) if last.old.end >= edit.old.start => {
                    last.old.end = edit.old.end;
                    last.new.end = edit.new.end;
                }
                _ => merged.push(edit),
            }
        }
        let tab_edits = {
            let mut old_cursor = old_snapshot.tab_point_cursor();
            let mut new_cursor = new_snapshot.tab_point_cursor();
            merged
                .into_iter()
                .map(|replace_edit| TabEdit {
                    old: old_cursor.map(replace_edit.old.start)
                        ..old_cursor.map(replace_edit.old.end),
                    new: new_cursor.map(replace_edit.new.start)
                        ..new_cursor.map(replace_edit.new.end),
                })
                .collect()
        };
        *old_snapshot = new_snapshot;
        (old_snapshot.clone(), tab_edits)
    }
}

/// An immutable view of the tab layer. Cloning is O(1).
#[derive(Clone, Debug)]
pub struct TabSnapshot {
    replace_snapshot: ReplaceSnapshot,
    tab_size: NonZeroU32,
    /// The maximum column up to which a tab can expand.
    /// Any tab after this column will not expand.
    max_expansion_column: u32,
    version: usize,
}

impl TabSnapshot {
    /// The input layer's snapshot.
    pub fn replace_snapshot(&self) -> &ReplaceSnapshot {
        &self.replace_snapshot
    }

    /// The buffer snapshot under all layers.
    #[inline]
    pub fn buffer(&self) -> &TextSnapshot {
        self.replace_snapshot.buffer()
    }

    /// Columns per tab stop.
    pub fn tab_size(&self) -> NonZeroU32 {
        self.tab_size
    }

    /// The input byte column from which tabs are one space wide.
    pub fn max_expansion_column(&self) -> u32 {
        self.max_expansion_column
    }

    /// Increases with every change of the text, the tab size or the
    /// expansion limit.
    pub fn version(&self) -> usize {
        self.version
    }

    /// Length of `row` in bytes of expanded text.
    pub fn line_len(&self, row: u32) -> u32 {
        let max_point = self.max_point();
        if row < max_point.row() {
            self.replace_point_to_tab_point(ReplacePoint::new(
                row,
                self.replace_snapshot.line_len(row),
            ))
            .0
            .column
        } else {
            max_point.column()
        }
    }

    /// The summary of the whole text.
    pub fn text_summary(&self) -> TextSummary {
        self.text_summary_for_range(TabPoint::zero()..self.max_point())
    }

    /// The summary of the text in `range`. `longest_row` comes from the
    /// input layer and ignores tab expansion.
    pub fn text_summary_for_range(&self, range: Range<TabPoint>) -> TextSummary {
        let input_start = self.tab_point_to_replace_point(range.start, Bias::Left).0;
        let input_end = self.tab_point_to_replace_point(range.end, Bias::Right).0;
        let input_summary = self
            .replace_snapshot
            .text_summary_for_range(input_start..input_end);

        let line_end = if range.start.row() == range.end.row() {
            range.end
        } else {
            self.max_point()
        };
        let first_line_chars = self
            .chunks(range.start..line_end)
            .flat_map(|chunk| chunk.text.chars())
            .take_while(|&c| c != '\n')
            .count() as u32;

        let last_line_chars = if range.start.row() == range.end.row() {
            first_line_chars
        } else {
            self.chunks(TabPoint::new(range.end.row(), 0)..range.end)
                .flat_map(|chunk| chunk.text.chars())
                .count() as u32
        };

        TextSummary {
            lines: range.end.0 - range.start.0,
            first_line_chars,
            last_line_chars,
            longest_row: input_summary.longest_row,
            longest_row_chars: input_summary.longest_row_chars,
        }
    }

    /// The expanded text in `range`, in chunks. Expanded tabs come as
    /// chunks of spaces with `is_tab` set.
    pub fn chunks(&self, range: Range<TabPoint>) -> TabChunks<'_> {
        self.highlighted_chunks(range, &[])
    }

    /// [`chunks`](Self::chunks) with the style ids of `highlights` (see
    /// [`ReplaceSnapshot::highlighted_chunks`]); an expanded tab carries
    /// the style of its tab.
    pub fn highlighted_chunks<'a>(
        &'a self,
        range: Range<TabPoint>,
        highlights: &'a [Highlight],
    ) -> TabChunks<'a> {
        let range = range.start..range.end.max(range.start);
        let (input_start, expanded_char_column, to_next_stop) =
            self.tab_point_to_replace_point(range.start, Bias::Left);
        let input_column = input_start.column();
        let input_end = self.tab_point_to_replace_point(range.end, Bias::Right).0;
        let to_next_stop = if range.start.0 + Point::new(0, to_next_stop) > range.end.0 {
            range.end.column() - range.start.column()
        } else {
            to_next_stop
        };

        TabChunks {
            snapshot: self,
            replace_chunks: self
                .replace_snapshot
                .highlighted_chunks(input_start..input_end, highlights),
            input_column,
            column: expanded_char_column,
            max_expansion_column: self.max_expansion_column,
            output_position: range.start.0,
            max_output_position: range.end.0,
            tab_size: self.tab_size,
            chunk: Chunk {
                text: &SPACES[..to_next_stop as usize],
                is_tab: true,
                chars: u128::MAX.unbounded_shr(u128::BITS - to_next_stop),
                ..Default::default()
            },
            inside_leading_tab: to_next_stop > 0,
        }
    }

    /// The whole expanded text.
    pub fn text(&self) -> String {
        self.chunks(TabPoint::zero()..self.max_point())
            .map(|chunk| chunk.text)
            .collect()
    }

    /// The end of the text.
    pub fn max_point(&self) -> TabPoint {
        self.replace_point_to_tab_point(self.replace_snapshot.max_point())
    }

    /// Clips a point to a valid one: inside an expanded tab it moves to the
    /// tab's start ([`Bias::Left`]) or end ([`Bias::Right`]), and the input
    /// layer clips rows, row ends and characters.
    pub fn clip_point(&self, point: TabPoint, bias: Bias) -> TabPoint {
        self.replace_point_to_tab_point(
            self.replace_snapshot
                .clip_point(self.tab_point_to_replace_point(point, bias).0, bias),
        )
    }

    /// The tab point of an input point. A column past the end of the row
    /// keeps its overshoot.
    pub fn replace_point_to_tab_point(&self, input: ReplacePoint) -> TabPoint {
        let chunks = self
            .replace_snapshot
            .chunks_at(ReplacePoint::new(input.row(), 0));
        let mut tab_cursor = TabStopCursor::new(chunks);
        let expanded = self.expand_tabs(&mut tab_cursor, &mut 0, &mut 0, input.column());
        TabPoint::new(input.row(), expanded)
    }

    /// A cursor for converting many input points to tab points.
    pub fn tab_point_cursor(&self) -> TabPointCursor<'_> {
        TabPointCursor {
            snapshot: self,
            tab_stop_cursor: None,
            row: 0,
            tab_count: 0,
            expanded_tab_len: 0,
            needs_seek: false,
            max_scan_chunks: MAX_ROW_SCAN_CHUNKS,
        }
    }

    /// The input point of a tab point, with the expanded `char` column of
    /// that input point and, for a point inside an expanded tab with
    /// [`Bias::Left`], the distance to the next tab stop (otherwise 0).
    ///
    /// Inside an expanded tab, [`Bias::Left`] gives the tab and
    /// [`Bias::Right`] the character after it.
    pub fn tab_point_to_replace_point(
        &self,
        output: TabPoint,
        bias: Bias,
    ) -> (ReplacePoint, u32, u32) {
        let chunks = self
            .replace_snapshot
            .chunks_at(ReplacePoint::new(output.row(), 0));

        let tab_cursor = TabStopCursor::new(chunks);
        let expanded = output.column();
        let (collapsed, expanded_char_column, to_next_stop) =
            self.collapse_tabs(tab_cursor, expanded, bias);

        (
            ReplacePoint::new(output.row(), collapsed),
            expanded_char_column,
            to_next_stop,
        )
    }

    /// The tab row that shows buffer line `line`.
    pub fn buffer_line_to_tab_row(&self, line: usize) -> u32 {
        self.buffer_point_to_tab_point(BufferPoint::new(line, 0), Bias::Left)
            .row()
    }

    /// The tab point of a buffer point (clipped first).
    pub fn buffer_point_to_tab_point(&self, point: BufferPoint, bias: Bias) -> TabPoint {
        let replace_point = self.replace_snapshot.to_replace_point(point, bias);
        self.replace_point_to_tab_point(replace_point)
    }

    /// The buffer point of a tab point (clipped first).
    pub fn tab_point_to_buffer_point(&self, point: TabPoint, bias: Bias) -> BufferPoint {
        let replace_point = self.tab_point_to_replace_point(point, bias).0;
        self.replace_snapshot.to_buffer_point(replace_point, bias)
    }

    /// Expands the tabs between the cursor's position and `column`. `tab_count`
    /// and `expanded_tab_len` carry the tabs already passed on this row, so the
    /// same cursor can be advanced to a later column of the row.
    fn expand_tabs(
        &self,
        cursor: &mut TabStopCursor<'_>,
        tab_count: &mut u32,
        expanded_tab_len: &mut u32,
        column: u32,
    ) -> u32 {
        // we only ever act on a single row at a time
        // so the main difference is that other layers build a transform sumtree, and can then just run through that
        // we cant quite do this here, as we need to work with the previous layer chunk to understand the tabs of the corresponding row
        // we can still do forward searches for this though, we search for a row, then traverse the column up to where we need to be
        let tab_size = self.tab_size.get();

        let end_column = column.min(self.max_expansion_column);
        let mut seek_target = end_column - cursor.byte_offset;

        while let Some(tab_stop) = cursor.seek_forward(seek_target) {
            let expanded_chars_old = tab_stop.char_offset + *expanded_tab_len - *tab_count;
            let tab_len = tab_size - ((expanded_chars_old - 1) % tab_size);
            *tab_count += 1;
            *expanded_tab_len += tab_len;

            seek_target = end_column - cursor.byte_offset;
        }

        let left_over_char_bytes = if !cursor.is_char_boundary() {
            cursor.bytes_until_next_char().unwrap_or(0) as u32
        } else {
            0
        };

        let collapsed_bytes = cursor.byte_offset() + left_over_char_bytes;
        let expanded_bytes =
            cursor.byte_offset() + *expanded_tab_len - *tab_count + left_over_char_bytes;

        expanded_bytes + column.saturating_sub(collapsed_bytes)
    }

    fn collapse_tabs(
        &self,
        mut cursor: TabStopCursor<'_>,
        column: u32,
        bias: Bias,
    ) -> (u32, u32, u32) {
        let tab_size = self.tab_size.get();
        let mut collapsed_column = column;
        let mut seek_target = column.min(self.max_expansion_column);
        let mut tab_count = 0;
        let mut expanded_tab_len = 0;

        while let Some(tab_stop) = cursor.seek_forward(seek_target) {
            // Calculate how much we want to expand this tab stop (into spaces)
            let expanded_chars_old = tab_stop.char_offset + expanded_tab_len - tab_count;
            let tab_len = tab_size - ((expanded_chars_old - 1) % tab_size);
            // Increment tab count
            tab_count += 1;
            // The count of how many spaces we've added to this line in place of tab bytes
            expanded_tab_len += tab_len;

            // The count of bytes at this point in the iteration while considering tab_count and previous expansions
            let expanded_bytes = tab_stop.byte_offset + expanded_tab_len - tab_count;

            // Did we expand past the search target?
            if expanded_bytes > column {
                let mut expanded_chars = tab_stop.char_offset + expanded_tab_len - tab_count;
                // We expanded past the search target, so need to account for the offshoot
                expanded_chars -= expanded_bytes - column;
                return match bias {
                    Bias::Left => (
                        cursor.byte_offset() - 1,
                        expanded_chars,
                        expanded_bytes - column,
                    ),
                    Bias::Right => (cursor.byte_offset(), expanded_chars, 0),
                };
            } else {
                // otherwise we only want to move the cursor collapse column forward
                collapsed_column = collapsed_column - tab_len + 1;
                seek_target = (collapsed_column - cursor.byte_offset)
                    .min(self.max_expansion_column - cursor.byte_offset);
            }
        }

        let collapsed_bytes = cursor.byte_offset();
        let expanded_bytes = cursor.byte_offset() + expanded_tab_len - tab_count;
        let expanded_chars = cursor.char_offset() + expanded_tab_len - tab_count;
        (
            collapsed_bytes + column.saturating_sub(expanded_bytes),
            expanded_chars,
            0,
        )
    }
}

/// Maps `ReplacePoint`s to `TabPoint`s, reusing the scan over the input chunks
/// across calls. It is most efficient when points are supplied in
/// non-decreasing order; a point before the previous one is still mapped
/// correctly, at the cost of an extra seek.
#[derive(Debug)]
pub struct TabPointCursor<'a> {
    snapshot: &'a TabSnapshot,
    tab_stop_cursor: Option<TabStopCursor<'a>>,
    row: u32,
    tab_count: u32,
    expanded_tab_len: u32,
    needs_seek: bool,
    max_scan_chunks: usize,
}

impl TabPointCursor<'_> {
    /// Resets the cursor to the start so it can seek backward again.
    pub fn reset(&mut self) {
        self.needs_seek = true;
    }

    /// The tab point of `point`, like
    /// [`TabSnapshot::replace_point_to_tab_point`].
    pub fn map(&mut self, point: ReplacePoint) -> TabPoint {
        let snapshot = self.snapshot;
        let row = point.row();
        let end_column = point.column().min(snapshot.max_expansion_column);

        let cursor = match &mut self.tab_stop_cursor {
            Some(cursor) => {
                let can_continue = !self.needs_seek && !cursor.crossed_newline;
                if !(can_continue && row == self.row && end_column >= cursor.byte_offset) {
                    // Scanning over the rows in between is cheaper than seeking
                    // the input chunks, as long as the target row is nearby.
                    let advanced = can_continue
                        && row > self.row
                        && cursor.advance_rows(row - self.row, self.max_scan_chunks);
                    if !advanced {
                        cursor.seek_to(
                            ReplacePoint::new(row, 0)..snapshot.replace_snapshot.max_point(),
                        );
                    }
                    self.tab_count = 0;
                    self.expanded_tab_len = 0;
                    self.needs_seek = false;
                }
                cursor
            }
            None => {
                self.tab_count = 0;
                self.expanded_tab_len = 0;
                self.needs_seek = false;
                self.tab_stop_cursor.insert(TabStopCursor::new(
                    snapshot
                        .replace_snapshot
                        .chunks_at(ReplacePoint::new(row, 0)),
                ))
            }
        };
        self.row = row;

        let expanded = snapshot.expand_tabs(
            cursor,
            &mut self.tab_count,
            &mut self.expanded_tab_len,
            point.column(),
        );
        TabPoint::new(row, expanded)
    }
}

/// A position in the tab layer's text: a row and a byte column of the
/// expanded text.
#[derive(Copy, Clone, Debug, Default, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct TabPoint(pub Point);

impl TabPoint {
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

impl From<Point> for TabPoint {
    fn from(point: Point) -> Self {
        Self(point)
    }
}

/// An edit of the tab layer's text.
pub type TabEdit = Edit<TabPoint>;

/// The tab layer's text in chunks. See [`TabSnapshot::chunks`].
#[derive(Debug)]
pub struct TabChunks<'a> {
    snapshot: &'a TabSnapshot,
    max_expansion_column: u32,
    max_output_position: Point,
    tab_size: NonZeroU32,
    // region: iteration state
    replace_chunks: ReplaceChunks<'a>,
    chunk: Chunk<'a>,
    column: u32,
    output_position: Point,
    input_column: u32,
    inside_leading_tab: bool,
    // endregion: iteration state
}

impl TabChunks<'_> {
    /// Restarts the iterator on another range of the same snapshot.
    pub fn seek(&mut self, range: Range<TabPoint>) {
        let range = range.start..range.end.max(range.start);
        let (input_start, expanded_char_column, to_next_stop) = self
            .snapshot
            .tab_point_to_replace_point(range.start, Bias::Left);
        let input_column = input_start.column();
        let input_end = self
            .snapshot
            .tab_point_to_replace_point(range.end, Bias::Right)
            .0;
        let to_next_stop = if range.start.0 + Point::new(0, to_next_stop) > range.end.0 {
            range.end.column() - range.start.column()
        } else {
            to_next_stop
        };

        self.replace_chunks.seek(input_start..input_end);
        self.input_column = input_column;
        self.column = expanded_char_column;
        self.output_position = range.start.0;
        self.max_output_position = range.end.0;
        self.chunk = Chunk {
            text: &SPACES[..to_next_stop as usize],
            is_tab: true,
            chars: u128::MAX.unbounded_shr(u128::BITS - to_next_stop),
            ..Default::default()
        };
        self.inside_leading_tab = to_next_stop > 0;
    }
}

impl<'a> Iterator for TabChunks<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.chunk.text.is_empty() {
            let chunk = self.replace_chunks.next()?;
            self.chunk = chunk;
            if self.inside_leading_tab {
                self.chunk.text = &self.chunk.text[1..];
                self.chunk.tabs >>= 1;
                self.chunk.chars >>= 1;
                self.chunk.newlines >>= 1;
                self.inside_leading_tab = false;
                self.input_column += 1;
            }
        }

        if self.chunk.tabs & 1 != 0 {
            self.chunk.text = &self.chunk.text[1..];
            self.chunk.tabs >>= 1;
            self.chunk.chars >>= 1;
            self.chunk.newlines >>= 1;

            let tab_size = if self.input_column < self.max_expansion_column {
                self.tab_size.get()
            } else {
                1
            };
            let mut len = tab_size - self.column % tab_size;
            let next_output_position = cmp::min(
                self.output_position + Point::new(0, len),
                self.max_output_position,
            );
            len = next_output_position.column - self.output_position.column;
            self.column += len;
            self.input_column += 1;
            self.output_position = next_output_position;

            return Some(Chunk {
                text: &SPACES[..len as usize],
                is_tab: true,
                chars: u128::MAX.unbounded_shr(u128::BITS - len),
                tabs: 0,
                newlines: 0,
                ..self.chunk.clone()
            });
        }

        // Fast path: no tabs in the remaining chunk, return it directly
        if self.chunk.tabs == 0 {
            let chunk = self.chunk.clone();
            self.chunk.text = "";
            self.chunk.tabs = 0;
            self.chunk.chars = 0;
            self.chunk.newlines = 0;
            let chunk_len = chunk.text.len() as u32;

            let newline_count = chunk.newlines.count_ones();
            if newline_count > 0 {
                let last_newline_bit = 128 - chunk.newlines.leading_zeros();
                let chars_after_last_newline =
                    chunk.chars.unbounded_shr(last_newline_bit).count_ones();
                let bytes_after_last_newline = chunk_len - last_newline_bit;

                self.column = chars_after_last_newline;
                self.input_column = bytes_after_last_newline;
                self.output_position = Point::new(
                    self.output_position.row + newline_count,
                    bytes_after_last_newline,
                );
            } else {
                let char_count = chunk.chars.count_ones();
                self.column += char_count;
                if !self.inside_leading_tab {
                    self.input_column += chunk_len;
                }
                self.output_position.column += chunk_len;
            }

            return Some(chunk);
        }

        // Split at the next tab position
        let prefix_len = self.chunk.tabs.trailing_zeros() as usize;
        let (prefix, suffix) = self.chunk.text.split_at(prefix_len);

        let mask = 1u128.unbounded_shl(prefix_len as u32).wrapping_sub(1);
        let prefix_chars = self.chunk.chars & mask;
        let prefix_newlines = self.chunk.newlines & mask;

        self.chunk.text = suffix;
        self.chunk.tabs = self.chunk.tabs.unbounded_shr(prefix_len as u32);
        self.chunk.chars = self.chunk.chars.unbounded_shr(prefix_len as u32);
        self.chunk.newlines = self.chunk.newlines.unbounded_shr(prefix_len as u32);

        let newline_count = prefix_newlines.count_ones();
        if newline_count > 0 {
            let last_newline_bit = 128 - prefix_newlines.leading_zeros();
            let chars_after_last_newline =
                prefix_chars.unbounded_shr(last_newline_bit).count_ones();
            let bytes_after_last_newline = prefix_len as u32 - last_newline_bit;

            self.column = chars_after_last_newline;
            self.input_column = bytes_after_last_newline;
            self.output_position = Point::new(
                self.output_position.row + newline_count,
                bytes_after_last_newline,
            );
        } else {
            let char_count = prefix_chars.count_ones();
            self.column += char_count;
            if !self.inside_leading_tab {
                self.input_column += prefix_len as u32;
            }
            self.output_position.column += prefix_len as u32;
        }

        Some(Chunk {
            text: prefix,
            chars: prefix_chars,
            tabs: 0,
            newlines: prefix_newlines,
            ..self.chunk.clone()
        })
    }
}

#[derive(Debug)]
struct TabStopCursor<'a> {
    chunks: ReplaceChunks<'a>,
    byte_offset: u32,
    char_offset: u32,
    /// Chunk
    /// last tab position iterated through
    current_chunk: Option<(TabStopChunk<'a>, u32)>,
    /// Whether a seek moved past the end of the row it started on, which only
    /// happens for columns beyond the end of the row.
    crossed_newline: bool,
}

#[derive(Debug)]
struct TabStopChunk<'a> {
    chars: u128,
    text: &'a str,
    tabs: u128,
    newlines: u128,
}

impl<'a> TabStopChunk<'a> {
    fn new(chunk: Chunk<'a>) -> Self {
        Self {
            chars: chunk.chars,
            text: chunk.text,
            tabs: chunk.tabs,
            newlines: chunk.newlines,
        }
    }

    fn has_newline_in_byte_range(&self, range: Range<u32>) -> bool {
        let mask = u128::MAX.unbounded_shl(range.start) & !u128::MAX.unbounded_shl(range.end);
        self.newlines & mask != 0
    }
}

impl<'a> TabStopCursor<'a> {
    fn new(chunks: ReplaceChunks<'a>) -> Self {
        Self {
            chunks,
            byte_offset: 0,
            char_offset: 0,
            current_chunk: None,
            crossed_newline: false,
        }
    }

    fn seek_to(&mut self, range: Range<ReplacePoint>) {
        self.chunks.seek(range);
        self.byte_offset = 0;
        self.char_offset = 0;
        self.current_chunk = None;
        self.crossed_newline = false;
    }

    /// Moves to the start of the row that is `row_delta` rows below the current
    /// one, pulling at most `max_chunks` new chunks. Returns false if that row
    /// was not reached, in which case the cursor must be repositioned with
    /// `seek_to`.
    fn advance_rows(&mut self, row_delta: u32, max_chunks: usize) -> bool {
        let mut rows_remaining = row_delta;
        let mut chunks_remaining = max_chunks;
        loop {
            let (mut chunk, chunk_position) = match self.current_chunk.take() {
                Some(current_chunk) => current_chunk,
                None => {
                    if chunks_remaining == 0 {
                        return false;
                    }
                    chunks_remaining -= 1;
                    match self.chunks.next() {
                        Some(chunk) => (TabStopChunk::new(chunk), 0),
                        None => return false,
                    }
                }
            };

            let mut pending_newlines = chunk.newlines & u128::MAX.unbounded_shl(chunk_position);
            let newline_count = pending_newlines.count_ones();
            if newline_count < rows_remaining {
                rows_remaining -= newline_count;
                continue;
            }

            for _ in 1..rows_remaining {
                pending_newlines &= pending_newlines - 1;
            }
            let row_start = pending_newlines.trailing_zeros() + 1;
            // `seek_forward` relies on every remaining tab being at or after the chunk position.
            chunk.tabs &= u128::MAX.unbounded_shl(row_start);
            if (row_start as usize) < chunk.text.len() {
                self.current_chunk = Some((chunk, row_start));
            }
            self.byte_offset = 0;
            self.char_offset = 0;
            return true;
        }
    }

    fn bytes_until_next_char(&self) -> Option<usize> {
        self.current_chunk.as_ref().map(|&(ref chunk, idx)| {
            let higher_chars = chunk.chars.unbounded_shr(idx + 1);

            if higher_chars != 0 {
                higher_chars.trailing_zeros() as usize + 1
            } else {
                chunk.text.len() - idx as usize
            }
        })
    }

    fn is_char_boundary(&self) -> bool {
        self.current_chunk
            .as_ref()
            .is_some_and(|&(ref chunk, idx)| {
                (1u128.unbounded_shl(idx) & chunk.chars) != 0 || idx as usize == chunk.text.len()
            })
    }

    /// distance: length to move forward while searching for the next tab stop
    fn seek_forward(&mut self, distance: u32) -> Option<TabStop> {
        if distance == 0 {
            return None;
        }

        let mut distance_remaining = distance;

        while let Some((mut chunk, chunk_position)) = self.current_chunk.take().or_else(|| {
            self.chunks
                .next()
                .map(|chunk| (TabStopChunk::new(chunk), 0))
        }) {
            let chunk_len = chunk.text.len() as u32;

            if chunk.tabs == 0 {
                let chunk_remaining = chunk_len - chunk_position;
                if chunk_remaining >= distance_remaining {
                    let end = chunk_position + distance_remaining;
                    self.crossed_newline |= chunk.has_newline_in_byte_range(chunk_position..end);
                    self.byte_offset += distance_remaining;
                    self.char_offset +=
                        count_chars_in_byte_range(chunk_position..(end - 1), chunk.chars);
                    if end < 128 {
                        self.current_chunk = Some((chunk, end));
                    }
                    return None;
                }

                self.crossed_newline |= chunk.has_newline_in_byte_range(chunk_position..chunk_len);
                self.byte_offset += chunk_remaining;
                self.char_offset +=
                    count_chars_in_byte_range(chunk_position..(chunk_len - 1), chunk.chars);
                distance_remaining -= chunk_remaining;
                continue;
            }

            let tab_end = chunk.tabs.trailing_zeros() + 1;
            let bytes_to_tab = tab_end - chunk_position;

            if bytes_to_tab > distance_remaining {
                let end = chunk_position + distance_remaining;
                self.crossed_newline |= chunk.has_newline_in_byte_range(chunk_position..end);
                self.byte_offset += distance_remaining;
                self.char_offset +=
                    count_chars_in_byte_range(chunk_position..(end - 1), chunk.chars);
                self.current_chunk = Some((chunk, end));
                return None;
            }

            self.crossed_newline |= chunk.has_newline_in_byte_range(chunk_position..tab_end);
            self.byte_offset += bytes_to_tab;
            self.char_offset +=
                count_chars_in_byte_range(chunk_position..(tab_end - 1), chunk.chars);

            let tabstop = TabStop {
                char_offset: self.char_offset,
                byte_offset: self.byte_offset,
            };

            chunk.tabs = (chunk.tabs - 1) & chunk.tabs;

            if tab_end != chunk_len {
                self.current_chunk = Some((chunk, tab_end));
            }

            return Some(tabstop);
        }

        None
    }

    fn byte_offset(&self) -> u32 {
        self.byte_offset
    }

    fn char_offset(&self) -> u32 {
        self.char_offset
    }
}

#[inline(always)]
fn count_chars_in_byte_range(range: Range<u32>, bitmap: u128) -> u32 {
    let low_mask = u128::MAX << range.start;
    let high_mask = u128::MAX >> (127 - range.end);
    (bitmap & low_mask & high_mask).count_ones()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabStop {
    char_offset: u32,
    byte_offset: u32,
}

#[cfg(test)]
mod tests {
    use std::mem;

    use super::*;
    use crate::display_map::replace_map::ReplaceMap;
    use crate::display_map::test_support::{
        TestText, for_each_seed, random_edit, random_text, replay_edits,
    };
    use gv_text::Buffer;
    use rand::Rng;

    // Upstream builds `MultiBuffer -> InlayMap -> FoldMap -> TabMap`; here the
    // input is the identity replace layer over a gv-text snapshot.
    fn replace_snapshot(text: &str) -> ReplaceSnapshot {
        ReplaceMap::new(TextSnapshot::new(text)).1
    }

    fn tab_snapshot(text: &str, tab_size: u32) -> TabSnapshot {
        TabMap::new(replace_snapshot(text), NonZeroU32::new(tab_size).unwrap()).1
    }

    impl TabSnapshot {
        fn expected_collapse_tabs(
            &self,
            chars: impl Iterator<Item = char>,
            column: u32,
            bias: Bias,
        ) -> (u32, u32, u32) {
            let tab_size = self.tab_size.get();

            let mut expanded_bytes = 0;
            let mut expanded_chars = 0;
            let mut collapsed_bytes = 0;
            for c in chars {
                if expanded_bytes >= column {
                    break;
                }
                if collapsed_bytes >= self.max_expansion_column {
                    break;
                }

                if c == '\t' {
                    let tab_len = tab_size - (expanded_chars % tab_size);
                    expanded_chars += tab_len;
                    expanded_bytes += tab_len;
                    if expanded_bytes > column {
                        expanded_chars -= expanded_bytes - column;
                        return match bias {
                            Bias::Left => {
                                (collapsed_bytes, expanded_chars, expanded_bytes - column)
                            }
                            Bias::Right => (collapsed_bytes + 1, expanded_chars, 0),
                        };
                    }
                } else {
                    expanded_chars += 1;
                    expanded_bytes += c.len_utf8() as u32;
                }

                if expanded_bytes > column && matches!(bias, Bias::Left) {
                    expanded_chars -= 1;
                    break;
                }

                collapsed_bytes += c.len_utf8() as u32;
            }

            (
                collapsed_bytes + column.saturating_sub(expanded_bytes),
                expanded_chars,
                0,
            )
        }

        pub fn expected_to_tab_point(&self, input: ReplacePoint) -> TabPoint {
            let chars = self
                .replace_snapshot
                .chars_at(ReplacePoint::new(input.row(), 0));
            let expanded = self.expected_expand_tabs(chars, input.column());
            TabPoint::new(input.row(), expanded)
        }

        fn expected_expand_tabs(&self, chars: impl Iterator<Item = char>, column: u32) -> u32 {
            let tab_size = self.tab_size.get();

            let mut expanded_chars = 0;
            let mut expanded_bytes = 0;
            let mut collapsed_bytes = 0;
            let end_column = column.min(self.max_expansion_column);
            for c in chars {
                if collapsed_bytes >= end_column {
                    break;
                }
                if c == '\t' {
                    let tab_len = tab_size - expanded_chars % tab_size;
                    expanded_bytes += tab_len;
                    expanded_chars += tab_len;
                } else {
                    expanded_bytes += c.len_utf8() as u32;
                    expanded_chars += 1;
                }
                collapsed_bytes += c.len_utf8() as u32;
            }

            expanded_bytes + column.saturating_sub(collapsed_bytes)
        }

        fn expected_to_replace_point(
            &self,
            output: TabPoint,
            bias: Bias,
        ) -> (ReplacePoint, u32, u32) {
            let chars = self
                .replace_snapshot
                .chars_at(ReplacePoint::new(output.row(), 0));
            let expanded = output.column();
            let (collapsed, expanded_char_column, to_next_stop) =
                self.expected_collapse_tabs(chars, expanded, bias);
            (
                ReplacePoint::new(output.row(), collapsed),
                expanded_char_column,
                to_next_stop,
            )
        }
    }

    #[test]
    fn test_expand_tabs() {
        let input = "A\tBC\tDEF\tG\tHI\tJ\tK\tL\tM";

        let tab_snapshot = tab_snapshot(input, 4);

        for (ix, _) in input.char_indices() {
            let replace_point = ReplacePoint::new(0, ix as u32);

            assert_eq!(
                tab_snapshot.expected_to_tab_point(replace_point),
                tab_snapshot.replace_point_to_tab_point(replace_point),
                "Failed with replace_point at column {ix}"
            );
        }
    }

    #[test]
    fn test_collapse_tabs() {
        let input = "A\tBC\tDEF\tG\tHI\tJ\tK\tL\tM";

        let tab_snapshot = tab_snapshot(input, 4);

        for (ix, _) in input.char_indices() {
            let range = TabPoint::new(0, ix as u32)..tab_snapshot.max_point();

            assert_eq!(
                tab_snapshot.expected_to_replace_point(range.start, Bias::Left),
                tab_snapshot.tab_point_to_replace_point(range.start, Bias::Left),
                "Failed with tab_point at column {ix}"
            );
            assert_eq!(
                tab_snapshot.expected_to_replace_point(range.start, Bias::Right),
                tab_snapshot.tab_point_to_replace_point(range.start, Bias::Right),
                "Failed with tab_point at column {ix}"
            );

            assert_eq!(
                tab_snapshot.expected_to_replace_point(range.end, Bias::Left),
                tab_snapshot.tab_point_to_replace_point(range.end, Bias::Left),
                "Failed with tab_point at column {ix}"
            );
            assert_eq!(
                tab_snapshot.expected_to_replace_point(range.end, Bias::Right),
                tab_snapshot.tab_point_to_replace_point(range.end, Bias::Right),
                "Failed with tab_point at column {ix}"
            );
        }
    }

    #[test]
    fn test_to_replace_point_panic_reproduction() {
        // Upstream: `test_to_fold_point_panic_reproduction`. It reproduces a
        // specific panic where to_fold_point returned incorrect results.
        let text = "γ\tw⭐\n🍐🍗 \t";
        let tab_snapshot = tab_snapshot(text, 4);

        let tab_point = TabPoint::new(0, 9);
        let result = tab_snapshot.tab_point_to_replace_point(tab_point, Bias::Left);
        let expected = tab_snapshot.expected_to_replace_point(tab_point, Bias::Left);

        assert_eq!(result, expected);
    }

    #[test]
    fn test_collapse_tabs_random() {
        for_each_seed(100, |mut rng| {
            // Generate random input string with up to 200 characters including tabs
            // to stay within the MAX_EXPANSION_COLUMN limit of 256
            let len = rng.random_range(0..=2048);
            let tab_size = NonZeroU32::new(rng.random_range(1..=4)).unwrap();
            let mut input = String::with_capacity(len);

            for _ in 0..len {
                if rng.random_bool(0.1) {
                    // 10% chance of inserting a tab
                    input.push('\t');
                } else {
                    // 90% chance of inserting a random ASCII character (excluding tab, newline, carriage return)
                    let ch = loop {
                        let ascii_code = rng.random_range(32..=126); // printable ASCII range
                        let ch = ascii_code as u8 as char;
                        if ch != '\t' {
                            break ch;
                        }
                    };
                    input.push(ch);
                }
            }

            let mut tab_snapshot = self::tab_snapshot(&input, 4);
            tab_snapshot.max_expansion_column = rng.random_range(0..323);
            tab_snapshot.tab_size = tab_size;

            for (ix, _) in input.char_indices() {
                let range = TabPoint::new(0, ix as u32)..tab_snapshot.max_point();

                assert_eq!(
                    tab_snapshot.expected_to_replace_point(range.start, Bias::Left),
                    tab_snapshot.tab_point_to_replace_point(range.start, Bias::Left),
                    "Failed with input: {}, with idx: {ix}",
                    input
                );
                assert_eq!(
                    tab_snapshot.expected_to_replace_point(range.start, Bias::Right),
                    tab_snapshot.tab_point_to_replace_point(range.start, Bias::Right),
                    "Failed with input: {}, with idx: {ix}",
                    input
                );

                assert_eq!(
                    tab_snapshot.expected_to_replace_point(range.end, Bias::Left),
                    tab_snapshot.tab_point_to_replace_point(range.end, Bias::Left),
                    "Failed with input: {}, with idx: {ix}",
                    input
                );
                assert_eq!(
                    tab_snapshot.expected_to_replace_point(range.end, Bias::Right),
                    tab_snapshot.tab_point_to_replace_point(range.end, Bias::Right),
                    "Failed with input: {}, with idx: {ix}",
                    input
                );
            }
        });
    }

    #[test]
    fn test_long_lines() {
        let max_expansion_column = 12;
        let input = "A\tBC\tDEF\tG\tHI\tJ\tK\tL\tM";
        let output = "A   BC  DEF G   HI J K L M";

        let mut tab_snapshot = tab_snapshot(input, 4);

        tab_snapshot.max_expansion_column = max_expansion_column;
        assert_eq!(tab_snapshot.text(), output);

        for (ix, c) in input.char_indices() {
            assert_eq!(
                tab_snapshot
                    .chunks(TabPoint::new(0, ix as u32)..tab_snapshot.max_point())
                    .map(|c| c.text)
                    .collect::<String>(),
                &output[ix..],
                "text from index {ix}"
            );

            if c != '\t' {
                let input_point = Point::new(0, ix as u32);
                let output_point = Point::new(0, output.find(c).unwrap() as u32);
                assert_eq!(
                    tab_snapshot.replace_point_to_tab_point(ReplacePoint(input_point)),
                    TabPoint(output_point),
                    "to_tab_point({input_point:?})"
                );
                assert_eq!(
                    tab_snapshot
                        .tab_point_to_replace_point(TabPoint(output_point), Bias::Left)
                        .0,
                    ReplacePoint(input_point),
                    "to_replace_point({output_point:?})"
                );
            }
        }
    }

    #[test]
    fn test_long_lines_with_character_spanning_max_expansion_column() {
        let max_expansion_column = 8;
        let input = "abcdefg⋯hij";

        let mut tab_snapshot = tab_snapshot(input, 4);

        tab_snapshot.max_expansion_column = max_expansion_column;
        assert_eq!(tab_snapshot.text(), input);
    }

    #[test]
    fn test_marking_tabs() {
        let input = "\t \thello";

        let tab_snapshot = tab_snapshot(input, 4);

        assert_eq!(
            chunks(&tab_snapshot, TabPoint::zero()),
            vec![
                ("    ".to_string(), true),
                (" ".to_string(), false),
                ("   ".to_string(), true),
                ("hello".to_string(), false),
            ]
        );
        assert_eq!(
            chunks(&tab_snapshot, TabPoint::new(0, 2)),
            vec![
                ("  ".to_string(), true),
                (" ".to_string(), false),
                ("   ".to_string(), true),
                ("hello".to_string(), false),
            ]
        );

        fn chunks(snapshot: &TabSnapshot, start: TabPoint) -> Vec<(String, bool)> {
            let mut chunks = Vec::new();
            let mut was_tab = false;
            let mut text = String::new();
            for chunk in snapshot.chunks(start..snapshot.max_point()) {
                if chunk.is_tab != was_tab {
                    if !text.is_empty() {
                        chunks.push((mem::take(&mut text), was_tab));
                    }
                    was_tab = chunk.is_tab;
                }
                text.push_str(chunk.text);
            }

            if !text.is_empty() {
                chunks.push((text, was_tab));
            }
            chunks
        }
    }

    #[test]
    fn test_max_tab_size_full_chunks() {
        for tab_size in [127, 128, 129] {
            let tab_snapshot = tab_snapshot("\tα\t🏀", tab_size);
            let width = tab_size.min(128);
            let expected = format!(
                "{}α{}🏀",
                " ".repeat(width as usize),
                " ".repeat(width as usize - 1)
            );
            let mut chunks = tab_snapshot.chunks(TabPoint::zero()..tab_snapshot.max_point());
            assert_max_tab_chunk_bitmaps(&mut chunks, &expected);
            assert_eq!(tab_snapshot.text(), expected);
            assert_eq!(
                tab_snapshot.buffer_point_to_tab_point(BufferPoint::new(0, 4), Bias::Left),
                TabPoint::new(0, 2 * width + 1),
            );
            assert_eq!(
                tab_snapshot.tab_point_to_buffer_point(TabPoint::new(0, 2 * width + 1), Bias::Left),
                BufferPoint::new(0, 4),
            );
        }
    }

    #[test]
    fn test_max_tab_size_partial_chunks_and_seek() {
        let tab_snapshot = tab_snapshot("\t🏀", 128);
        let expected = format!("{}🏀", " ".repeat(128));
        let mut reused = tab_snapshot.chunks(TabPoint::zero()..tab_snapshot.max_point());
        for start in 0..=128 {
            for end in [start, (start + 1).min(128), 128, 132] {
                let range = TabPoint::new(0, start)..TabPoint::new(0, end);
                let mut chunks = tab_snapshot.chunks(range.clone());
                assert_max_tab_chunk_bitmaps(&mut chunks, &expected[start as usize..end as usize]);
                reused.seek(range);
                assert_max_tab_chunk_bitmaps(&mut reused, &expected[start as usize..end as usize]);
            }
        }
    }

    #[test]
    fn test_empty_chunk_after_leading_tab_trim() {
        // Upstream folds "hello" (offsets 1..6) so the fold map creates a
        // transform boundary at offset 1, producing a 1-byte fold chunk for
        // the tab. The replace layer has no folds yet; it splits its chunks
        // at a lone CR instead, which gives the same 1-byte chunk "\t".
        let text = "\t\rhello";
        let tab_size = NonZeroU32::new(4).unwrap();
        let (_, tab_snapshot) = TabMap::new(replace_snapshot(text), tab_size);
        assert_eq!(
            tab_snapshot
                .replace_snapshot
                .chunks_at(ReplacePoint::zero())
                .next()
                .unwrap()
                .text,
            "\t"
        );

        // The tab at column 0 expands to 4 spaces (columns 0‥4).
        // Seek starting at column 2 (middle of that tab) so that
        // `inside_leading_tab = true` and `to_next_stop = 2`.
        // Set the end just past the tab expansion so the iterator must
        // process the tab byte from the input chunk.
        let max = tab_snapshot.max_point();
        let start = TabPoint::new(0, 2);
        let end = max;

        // This should not panic.
        let result: String = tab_snapshot.chunks(start..end).map(|c| c.text).collect();
        assert!(!result.is_empty());
        assert_eq!(result, "  \nhello");
    }

    #[test]
    fn test_random_tabs() {
        for_each_seed(100, |mut rng| {
            let tab_size = NonZeroU32::new(rng.random_range(1..=4)).unwrap();
            // Upstream picks a short text or `MultiBuffer::build_random`, and
            // mutates random folds and inlays, which the replace layer does
            // not have yet.
            let len = if rng.random() {
                rng.random_range(0..30)
            } else {
                rng.random_range(0..200)
            };
            let input = random_text(&mut rng, len);
            let replace_snapshot = replace_snapshot(&input);

            let (mut tab_map, _) = TabMap::new(replace_snapshot.clone(), tab_size);
            let tabs_snapshot = tab_map.set_max_expansion_column(32).0;

            let text = TestText::new(&tabs_snapshot.text());

            for _ in 0..5 {
                let end_row = rng.random_range(0..=text.max_point().row);
                let end_column = rng.random_range(0..=text.line_len(end_row));
                let mut end =
                    TabPoint(text.clip_point(Point::new(end_row, end_column), Bias::Right));
                let start_row = rng.random_range(0..=text.max_point().row);
                let start_column = rng.random_range(0..=text.line_len(start_row));
                let mut start =
                    TabPoint(text.clip_point(Point::new(start_row, start_column), Bias::Left));
                if start > end {
                    mem::swap(&mut start, &mut end);
                }

                let expected_text = text.slice(start.0..end.0);
                let expected_summary = TextSummary::from(expected_text);
                assert_eq!(
                    tabs_snapshot
                        .chunks(start..end)
                        .map(|c| c.text)
                        .collect::<String>(),
                    expected_text,
                    "chunks({:?}..{:?})",
                    start,
                    end
                );

                let mut actual_summary = tabs_snapshot.text_summary_for_range(start..end);
                if tab_size.get() > 1 && replace_snapshot.text().contains('\t') {
                    actual_summary.longest_row = expected_summary.longest_row;
                    actual_summary.longest_row_chars = expected_summary.longest_row_chars;
                }
                assert_eq!(actual_summary, expected_summary);
            }

            for row in 0..=text.max_point().row {
                assert_eq!(
                    tabs_snapshot.line_len(row),
                    text.line_len(row),
                    "line_len({row})"
                );
            }
        });
    }

    #[test]
    fn test_to_tab_point_random() {
        for_each_seed(100, |mut rng| {
            let tab_size = NonZeroU32::new(rng.random_range(1..=16)).unwrap();
            let len = rng.random_range(0..=2000);

            // Generate random text using RandomCharIter
            let text = random_text(&mut rng, len);

            // Upstream also syncs random inlay edits through the fold map
            // here; the replace layer has no inlays yet.
            let replace_snapshot = replace_snapshot(&text);
            let (_, mut tab_snapshot) = TabMap::new(replace_snapshot.clone(), tab_size);
            let max_replace_point = replace_snapshot.max_point();

            // Test random replace points
            for _ in 0..50 {
                tab_snapshot.max_expansion_column = rng.random_range(0..=256);
                // Generate random replace point
                let row = rng.random_range(0..=max_replace_point.row());
                let max_column = if row < max_replace_point.row() {
                    replace_snapshot.line_len(row)
                } else {
                    max_replace_point.column()
                };
                let column = rng.random_range(0..=max_column + 10);
                let replace_point = ReplacePoint::new(row, column);

                let actual = tab_snapshot.replace_point_to_tab_point(replace_point);
                let expected = tab_snapshot.expected_to_tab_point(replace_point);

                assert_eq!(
                    actual, expected,
                    "to_tab_point mismatch for replace_point {:?} in text {:?}",
                    replace_point, text
                );
            }
        });
    }

    #[test]
    fn test_random_tab_point_cursor() {
        for_each_seed(100, |mut rng| {
            let tab_size = NonZeroU32::new(rng.random_range(1..=16)).unwrap();
            let len = rng.random_range(0..=2000);
            let text = random_text(&mut rng, len);

            // Upstream mutates random inlays and folds before mapping; here
            // the text goes through random buffer edits instead.
            let mut buffer = Buffer::new(&text);
            let (mut replace_map, replace_snapshot) = ReplaceMap::new(buffer.snapshot());
            let (mut tab_map, _) = TabMap::new(replace_snapshot, tab_size);
            let mut replace_snapshot = replace_map.snapshot();
            for _ in 0..rng.random_range(0..=2) {
                let summary = random_edit(&mut rng, &mut buffer);
                let (snapshot, edits) = replace_map.sync(buffer.snapshot(), &summary);
                replace_snapshot = snapshot;
                tab_map.sync(replace_snapshot.clone(), edits, tab_size);
            }
            let tab_snapshot = tab_map
                .set_max_expansion_column(rng.random_range(0..=256))
                .0;

            let max_replace_point = replace_snapshot.max_point();
            let mut replace_points = Vec::new();
            for _ in 0..50 {
                let row = rng.random_range(0..=max_replace_point.row());
                let max_column = if row < max_replace_point.row() {
                    replace_snapshot.line_len(row)
                } else {
                    max_replace_point.column()
                };
                let replace_point = ReplacePoint::new(row, rng.random_range(0..=max_column + 10));
                replace_points.push(replace_point);
                if rng.random_bool(0.2) {
                    replace_points.push(replace_point);
                }
            }
            replace_points.sort();

            let assert_maps = |cursor: &mut TabPointCursor<'_>,
                               replace_point: ReplacePoint,
                               context: &str| {
                let actual = cursor.map(replace_point);
                assert_eq!(
                    actual,
                    tab_snapshot.expected_to_tab_point(replace_point),
                    "{context}: cursor mismatch for {replace_point:?}"
                );
                assert_eq!(
                    actual,
                    tab_snapshot.replace_point_to_tab_point(replace_point),
                    "{context}: cursor disagrees with replace_point_to_tab_point for {replace_point:?}"
                );
            };

            let mut cursor = tab_snapshot.tab_point_cursor();
            cursor.max_scan_chunks = rng.random_range(0..=3);
            for replace_point in &replace_points {
                assert_maps(&mut cursor, *replace_point, "forward");
            }

            cursor.reset();
            for replace_point in &replace_points {
                assert_maps(&mut cursor, *replace_point, "after reset");
            }

            for _ in 0..20 {
                let ix = rng.random_range(0..replace_points.len());
                assert_maps(&mut cursor, replace_points[ix], "unordered");
            }
        });
    }

    #[test]
    fn test_tab_map_sync_edits() {
        let mut buffer = Buffer::new("a\tb\nc\td\ne\tf\n");
        let (mut replace_map, replace_snapshot) = ReplaceMap::new(buffer.snapshot());
        let tab_size = NonZeroU32::new(4).unwrap();
        let (mut tab_map, tab_snapshot) = TabMap::new(replace_snapshot, tab_size);
        assert_eq!(tab_snapshot.text(), "a   b\nc   d\ne   f\n");

        // Upstream edits at points 0:0, 1:0 and 2:3.
        let summary = buffer
            .edit([(0..0, "x"), (4..4, "yz"), (11..11, "w")])
            .unwrap();
        let (replace_snapshot, replace_edits) = replace_map.sync(buffer.snapshot(), &summary);
        let (tab_snapshot, tab_edits) = tab_map.sync(replace_snapshot, replace_edits, tab_size);
        assert_eq!(tab_snapshot.text(), "xa  b\nyzc d\ne   fw\n");
        assert_eq!(
            tab_edits,
            vec![
                TabEdit {
                    old: TabPoint::new(0, 0)..TabPoint::new(0, 4),
                    new: TabPoint::new(0, 0)..TabPoint::new(0, 4),
                },
                TabEdit {
                    old: TabPoint::new(1, 0)..TabPoint::new(1, 4),
                    new: TabPoint::new(1, 0)..TabPoint::new(1, 4),
                },
                TabEdit {
                    old: TabPoint::new(2, 5)..TabPoint::new(2, 5),
                    new: TabPoint::new(2, 5)..TabPoint::new(2, 6),
                },
            ]
        );
    }

    #[test]
    fn test_random_tab_map_sync() {
        let operations = std::env::var("OPERATIONS")
            .map(|i| i.parse().expect("invalid `OPERATIONS` variable"))
            .unwrap_or(10);

        for_each_seed(100, |mut rng| {
            let mut tab_size = NonZeroU32::new(rng.random_range(1..=4)).unwrap();
            let len = rng.random_range(0..200);
            let text = random_text(&mut rng, len);
            let mut buffer = Buffer::new(&text);
            let (mut replace_map, replace_snapshot) = ReplaceMap::new(buffer.snapshot());
            let (mut tab_map, _) = TabMap::new(replace_snapshot, tab_size);
            let initial_snapshot = tab_map.set_max_expansion_column(32).0;

            let mut snapshot_edits = Vec::new();
            for _ in 0..operations {
                // Upstream also mutates random folds (30 %) and inlays
                // (20 %); the replace layer has neither yet, so those cases
                // edit the buffer.
                let summary = match rng.random_range(0..100) {
                    0..=9 => {
                        tab_size = NonZeroU32::new(rng.random_range(1..=4)).unwrap();
                        None
                    }
                    _ => Some(random_edit(&mut rng, &mut buffer)),
                };

                let (replace_snapshot, replace_edits) = match summary {
                    Some(summary) => replace_map.sync(buffer.snapshot(), &summary),
                    None => (replace_map.snapshot(), Vec::new()),
                };
                let (tab_snapshot, tab_edits) =
                    tab_map.sync(replace_snapshot, replace_edits, tab_size);
                snapshot_edits.push((tab_snapshot, tab_edits));
            }

            let mut expected_text = initial_snapshot.text();
            for (tab_snapshot, tab_edits) in snapshot_edits {
                let snapshot_text = tab_snapshot.text();
                expected_text = replay_edits(&expected_text, &snapshot_text, &tab_edits, |p| p.0);
                assert_eq!(
                    expected_text, snapshot_text,
                    "tab edits {tab_edits:?} do not transform the previous text into the new text"
                );
            }
        });
    }

    /// The tab stops of a row: the byte and `char` offset just after each
    /// tab. (Upstream builds this list inline in each test, from the char
    /// index for ASCII inputs and from `enumerate` otherwise.)
    fn tab_stops_in(text: &str) -> Vec<TabStop> {
        let mut tab_stops = Vec::new();
        let mut byte_offset = 0;
        for (i, ch) in text.chars().enumerate() {
            byte_offset += ch.len_utf8() as u32;
            if ch == '\t' {
                tab_stops.push(TabStop {
                    byte_offset,
                    char_offset: i as u32 + 1,
                });
            }
        }
        tab_stops
    }

    #[test]
    fn test_tab_stop_cursor_utf8() {
        let text = "\tfoo\tbarbarbar\t\tbaz\n";
        let replace_snapshot = replace_snapshot(text);
        let chunks = replace_snapshot.chunks(ReplacePoint::zero()..replace_snapshot.max_point());
        let mut cursor = TabStopCursor::new(chunks);
        assert!(cursor.seek_forward(0).is_none());
        let mut tab_stops = Vec::new();

        let all_tab_stops = tab_stops_in(text);
        let byte_offset = text.len() as u32;

        while let Some(tab_stop) = cursor.seek_forward(u32::MAX) {
            tab_stops.push(tab_stop);
        }
        assert_eq!(tab_stops.as_slice(), all_tab_stops.as_slice());

        assert_eq!(cursor.byte_offset(), byte_offset);
    }

    #[test]
    fn test_tab_stop_with_end_range_utf8() {
        let input = "A\tBC\t"; // DEF\tG\tHI\tJ\tK\tL\tM

        let replace_snapshot = replace_snapshot(input);

        let chunks = replace_snapshot.chunks_at(ReplacePoint::new(0, 0));
        let mut cursor = TabStopCursor::new(chunks);

        let mut actual_tab_stops = Vec::new();

        let expected_tab_stops = tab_stops_in(input);
        let byte_offset = input.len() as u32;

        while let Some(tab_stop) = cursor.seek_forward(u32::MAX) {
            actual_tab_stops.push(tab_stop);
        }
        assert_eq!(actual_tab_stops.as_slice(), expected_tab_stops.as_slice());

        assert_eq!(cursor.byte_offset(), byte_offset);
    }

    #[test]
    fn test_tab_stop_cursor_random_utf8() {
        for_each_seed(100, |mut rng| {
            // Generate random input string with up to 512 characters including tabs
            let len = rng.random_range(0..=2048);
            let mut input = String::with_capacity(len);

            let mut skip_tabs = rng.random_bool(0.10);
            for idx in 0..len {
                if idx % 128 == 0 {
                    skip_tabs = rng.random_bool(0.10);
                }

                if rng.random_bool(0.15) && !skip_tabs {
                    input.push('\t');
                } else {
                    let ch = loop {
                        let ascii_code = rng.random_range(32..=126); // printable ASCII range
                        let ch = ascii_code as u8 as char;
                        if ch != '\t' {
                            break ch;
                        }
                    };
                    input.push(ch);
                }
            }

            // Build the buffer and create cursor
            let replace_snapshot = replace_snapshot(&input);

            // First, collect all expected tab positions
            let mut all_tab_stops = Vec::new();
            let mut byte_offset = 1;
            let mut char_offset = 1;
            #[expect(
                clippy::explicit_counter_loop,
                reason = "Lint does not account for char_offset being needed after the loop"
            )]
            for ch in input.chars() {
                if ch == '\t' {
                    all_tab_stops.push(TabStop {
                        byte_offset,
                        char_offset,
                    });
                }
                byte_offset += ch.len_utf8() as u32;
                char_offset += 1;
            }

            // Test with various distances
            let distances = vec![1, 5, 10, 50, 100, u32::MAX];

            for distance in distances {
                let chunks = replace_snapshot.chunks_at(ReplacePoint::new(0, 0));
                let mut cursor = TabStopCursor::new(chunks);

                let mut found_tab_stops = Vec::new();
                let mut position = distance;
                while let Some(tab_stop) = cursor.seek_forward(position) {
                    found_tab_stops.push(tab_stop);
                    position = distance - tab_stop.byte_offset;
                }

                let expected_found_tab_stops: Vec<_> = all_tab_stops
                    .iter()
                    .take_while(|tab_stop| tab_stop.byte_offset <= distance)
                    .cloned()
                    .collect();

                assert_eq!(
                    found_tab_stops, expected_found_tab_stops,
                    "TabStopCursor output mismatch for distance {}. Input: {:?}",
                    distance, input
                );

                let final_position = cursor.byte_offset();
                if !found_tab_stops.is_empty() {
                    let last_tab_stop = found_tab_stops.last().unwrap();
                    assert!(
                        final_position >= last_tab_stop.byte_offset,
                        "Cursor final position {} is before last tab stop {}. Input: {:?}",
                        final_position,
                        last_tab_stop.byte_offset,
                        input
                    );
                }
            }
        });
    }

    #[test]
    fn test_tab_stop_cursor_utf16() {
        let text = "\r\t😁foo\tb😀arbar🤯bar\t\tbaz\n";
        let replace_snapshot = replace_snapshot(text);
        let chunks = replace_snapshot.chunks(ReplacePoint::zero()..replace_snapshot.max_point());
        let mut cursor = TabStopCursor::new(chunks);
        assert!(cursor.seek_forward(0).is_none());

        let replace_text: String = replace_snapshot.chars_at(ReplacePoint::new(0, 0)).collect();
        let expected_tab_stops = tab_stops_in(&replace_text);
        let byte_offset = replace_text.len() as u32;

        let mut actual_tab_stops = Vec::new();
        while let Some(tab_stop) = cursor.seek_forward(u32::MAX) {
            actual_tab_stops.push(tab_stop);
        }

        assert_eq!(actual_tab_stops.as_slice(), expected_tab_stops.as_slice());

        assert_eq!(cursor.byte_offset(), byte_offset);
    }

    #[test]
    fn test_tab_stop_cursor_random_utf16() {
        for_each_seed(100, |mut rng| {
            // Generate random input string with up to 512 characters including tabs
            let len = rng.random_range(0..=2048);
            let input = random_text(&mut rng, len);

            // Build the buffer and create cursor
            let replace_snapshot = replace_snapshot(&input);

            // First, collect all expected tab positions. Upstream reads the
            // buffer text, which Zed has normalized to LF; here that is the
            // replace layer's text.
            let all_tab_stops = tab_stops_in(&replace_snapshot.text());

            // Test with various distances
            let distances = vec![150];

            for distance in distances {
                let chunks = replace_snapshot.chunks_at(ReplacePoint::new(0, 0));
                let mut cursor = TabStopCursor::new(chunks);

                let mut found_tab_stops = Vec::new();
                let mut position = distance;
                while let Some(tab_stop) = cursor.seek_forward(position) {
                    found_tab_stops.push(tab_stop);
                    position = distance - tab_stop.byte_offset;
                }

                let expected_found_tab_stops: Vec<_> = all_tab_stops
                    .iter()
                    .take_while(|tab_stop| tab_stop.byte_offset <= distance)
                    .cloned()
                    .collect();

                assert_eq!(
                    found_tab_stops, expected_found_tab_stops,
                    "TabStopCursor output mismatch for distance {}. Input: {:?}",
                    distance, input
                );

                let final_position = cursor.byte_offset();
                if !found_tab_stops.is_empty() {
                    let last_tab_stop = found_tab_stops.last().unwrap();
                    assert!(
                        final_position >= last_tab_stop.byte_offset,
                        "Cursor final position {} is before last tab stop {}. Input: {:?}",
                        final_position,
                        last_tab_stop.byte_offset,
                        input
                    );
                }
            }
        });
    }

    fn assert_max_tab_chunk_bitmaps(chunks: &mut TabChunks<'_>, expected_text: &str) {
        let initial_mask = chunks
            .chunk
            .text
            .char_indices()
            .fold(0u128, |mask, (index, _)| mask | (1u128 << index));
        assert_eq!(chunks.chunk.chars, initial_mask);
        let text = chunks
            .map(|chunk| {
                let expected_mask = chunk
                    .text
                    .char_indices()
                    .fold(0u128, |mask, (index, _)| mask | (1u128 << index));
                assert_eq!(chunk.chars, expected_mask, "chunk {:?}", chunk.text);
                assert_eq!(chunk.tabs, 0);
                assert_eq!(chunk.newlines, 0);
                chunk.text
            })
            .collect::<String>();
        assert_eq!(text, expected_text);
    }
}

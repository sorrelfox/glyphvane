// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The rows the block layer sits on: wrap rows, or tab rows while soft wrap
//! is off.

use std::ops::Range;

use gv_text::{Bias, TextSnapshot};

use crate::display_map::{
    BufferPoint, ReplaceSnapshot, TabEdit, TabPoint, TabSnapshot, WrapEdit, WrapPoint, WrapRow,
    WrapSnapshot,
};

use super::RowEdit;

/// The layer under the block layer: the wrap layer, or the tab layer while
/// soft wrap is off. Its rows are the block layer's input rows.
///
/// A **display line** is the rows that show one tab row: one row of the
/// tab layer, all its wrap rows with soft wrap. It holds one buffer line,
/// or several when a fold joins them. Blocks are placed by display line.
#[derive(Clone, Debug)]
pub enum BlockInput {
    /// Tab rows (soft wrap off).
    Tab(TabSnapshot),
    /// Wrap rows.
    Wrap(WrapSnapshot),
}

impl From<TabSnapshot> for BlockInput {
    fn from(tabs: TabSnapshot) -> Self {
        Self::Tab(tabs)
    }
}

impl From<WrapSnapshot> for BlockInput {
    fn from(wrap: WrapSnapshot) -> Self {
        Self::Wrap(wrap)
    }
}

impl BlockInput {
    /// The number of rows (at least one).
    pub fn row_count(&self) -> u32 {
        match self {
            Self::Tab(tabs) => tabs.max_point().row() + 1,
            Self::Wrap(wrap) => wrap.max_point().row().0 + 1,
        }
    }

    /// The tab layer (under the wrap layer, or the input itself).
    pub fn tab_snapshot(&self) -> &TabSnapshot {
        match self {
            Self::Tab(tabs) => tabs,
            Self::Wrap(wrap) => wrap.tab_snapshot(),
        }
    }

    /// The wrap layer, if it is the input.
    pub fn wrap_snapshot(&self) -> Option<&WrapSnapshot> {
        match self {
            Self::Tab(_) => None,
            Self::Wrap(wrap) => Some(wrap),
        }
    }

    /// The replace layer (folds).
    pub fn replace_snapshot(&self) -> &ReplaceSnapshot {
        self.tab_snapshot().replace_snapshot()
    }

    /// The buffer text under every layer.
    pub fn buffer(&self) -> &TextSnapshot {
        self.tab_snapshot().buffer()
    }

    /// The rows of tab row `tab_row`.
    fn tab_row_rows(&self, tab_row: u32) -> Range<u32> {
        match self {
            Self::Tab(_) => tab_row..tab_row + 1,
            Self::Wrap(wrap) => {
                let start = wrap
                    .tab_point_to_wrap_point(TabPoint::new(tab_row, 0))
                    .row()
                    .0;
                let end = if tab_row >= wrap.tab_snapshot().max_point().row() {
                    wrap.max_point().row().0 + 1
                } else {
                    wrap.tab_point_to_wrap_point(TabPoint::new(tab_row + 1, 0))
                        .row()
                        .0
                };
                start..end.max(start + 1)
            }
        }
    }

    /// The rows of the display line that shows buffer line `line`.
    pub(super) fn line_rows(&self, line: usize) -> Range<u32> {
        self.tab_row_rows(self.tab_snapshot().buffer_line_to_tab_row(line))
    }

    /// The rows of the display line that row `row` is part of.
    pub(super) fn display_line_of_row(&self, row: u32) -> Range<u32> {
        match self {
            Self::Tab(_) => row..row + 1,
            Self::Wrap(wrap) => {
                let tab_row = wrap.to_tab_point(WrapPoint::new(WrapRow(row), 0)).row();
                self.tab_row_rows(tab_row)
            }
        }
    }

    /// The buffer offset where display line row `row` starts (the start of
    /// its first buffer line).
    pub(super) fn row_start_offset(&self, row: u32) -> usize {
        let point = match self {
            Self::Tab(tabs) => tabs.tab_point_to_buffer_point(TabPoint::new(row, 0), Bias::Left),
            Self::Wrap(wrap) => {
                wrap.wrap_point_to_buffer_point(WrapPoint::new(WrapRow(row), 0), Bias::Left)
            }
        };
        let text = self.buffer();
        text.point_to_offset(BufferPoint::new(point.line, 0))
            .unwrap_or(text.len())
    }
}

/// Wrap edits as row edits.
pub(super) fn wrap_row_edits(edits: &[WrapEdit]) -> Vec<RowEdit> {
    edits
        .iter()
        .map(|edit| RowEdit {
            old: edit.old.start.0..edit.old.end.0,
            new: edit.new.start.0..edit.new.end.0,
        })
        .collect()
}

/// Tab edits as edits of whole tab rows, merged where they share a row.
pub(super) fn tab_row_edits(edits: &[TabEdit]) -> Vec<RowEdit> {
    let mut rows: Vec<RowEdit> = Vec::with_capacity(edits.len());
    for edit in edits {
        let old = edit.old.start.row()..edit.old.end.row() + 1;
        let new = edit.new.start.row()..edit.new.end.row() + 1;
        match rows.last_mut() {
            Some(last) if old.start <= last.old.end => {
                last.old.end = last.old.end.max(old.end);
                last.new.end = last.new.end.max(new.end);
            }
            _ => rows.push(RowEdit { old, new }),
        }
    }
    rows
}

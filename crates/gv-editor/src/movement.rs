// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Caret motions.
//!
//! Horizontal motions work on buffer offsets ([`TextSnapshot`]); vertical
//! motions work on display rows of the tab layer ([`TabSnapshot`]), where a
//! row is one buffer line until soft wrap arrives. A vertical motion keeps a
//! [`SelectionGoal`] so the caret returns to its column after passing
//! shorter rows.

use gv_text::{Bias, TextSnapshot};

use crate::boundaries::{
    clip_caret, indent_end, line_end, line_start, next_grapheme_boundary, next_word_end,
    prev_grapheme_boundary, prev_word_start, snap_to_grapheme,
};
use crate::display_map::{TabPoint, TabSnapshot};
use crate::selection::SelectionGoal;

/// A caret motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Motion {
    /// One grapheme cluster left.
    Left,
    /// One grapheme cluster right.
    Right,
    /// To the start of the previous word (UAX #29 word boundaries).
    WordLeft,
    /// To the end of the next word.
    WordRight,
    /// To the start of the line.
    LineStart,
    /// "Smart home": to the first non-blank character of the line, or to the
    /// start of the line if the caret is already there.
    SmartLineStart,
    /// To the end of the line's content (before its terminator).
    LineEnd,
    /// To the start of the text.
    DocumentStart,
    /// To the end of the text.
    DocumentEnd,
    /// One display row up, keeping the goal column.
    Up,
    /// One display row down, keeping the goal column.
    Down,
    /// Several display rows up (page up), keeping the goal column.
    UpRows(u32),
    /// Several display rows down (page down), keeping the goal column.
    DownRows(u32),
}

impl Motion {
    /// Whether the motion moves between display rows and keeps a goal.
    pub fn is_vertical(self) -> bool {
        matches!(
            self,
            Self::Up | Self::Down | Self::UpRows(_) | Self::DownRows(_)
        )
    }
}

/// Where `motion` takes a caret at `offset` with `goal`. `display` must show
/// the same revision as its buffer (`display.buffer()`), which is the text
/// moved in. Offsets are clipped first; the result is a valid caret
/// position (never inside a character or a CRLF). Horizontal motions return
/// [`SelectionGoal::None`].
pub fn move_caret(
    display: &TabSnapshot,
    offset: usize,
    goal: SelectionGoal,
    motion: Motion,
) -> (usize, SelectionGoal) {
    let text = display.buffer();
    let offset = clip_caret(text, offset, Bias::Left);
    let target = match motion {
        Motion::Left => prev_grapheme_boundary(text, offset),
        Motion::Right => next_grapheme_boundary(text, offset),
        Motion::WordLeft => prev_word_start(text, offset),
        Motion::WordRight => next_word_end(text, offset),
        Motion::LineStart => line_start(text, offset),
        Motion::SmartLineStart => smart_line_start(text, offset),
        Motion::LineEnd => line_end(text, offset),
        Motion::DocumentStart => 0,
        Motion::DocumentEnd => text.len(),
        Motion::Up => return move_rows(display, offset, goal, -1),
        Motion::Down => return move_rows(display, offset, goal, 1),
        Motion::UpRows(rows) => return move_rows(display, offset, goal, -i64::from(rows)),
        Motion::DownRows(rows) => return move_rows(display, offset, goal, i64::from(rows)),
    };
    (clip_caret(text, target, Bias::Left), SelectionGoal::None)
}

/// The first non-blank character of the line, or the line start if the
/// caret is already there.
pub fn smart_line_start(text: &TextSnapshot, offset: usize) -> usize {
    let indent = indent_end(text, offset);
    if offset == indent {
        line_start(text, offset)
    } else {
        indent
    }
}

/// The display point of a buffer offset.
pub fn offset_to_display(display: &TabSnapshot, offset: usize) -> TabPoint {
    let text = display.buffer();
    let offset = clip_caret(text, offset, Bias::Left);
    let point = text
        .offset_to_point(offset)
        .unwrap_or_else(|_| text.max_point());
    display.buffer_point_to_tab_point(point, Bias::Left)
}

/// The buffer offset of a display point (clipped; inside an expanded tab it
/// goes to the tab).
pub fn display_to_offset(display: &TabSnapshot, point: TabPoint) -> usize {
    let text = display.buffer();
    let point = display.tab_point_to_buffer_point(point, Bias::Left);
    let offset = text.point_to_offset(point).unwrap_or_else(|_| {
        text.point_to_offset(text.clip_point(point, Bias::Left))
            .unwrap_or(0)
    });
    clip_caret(text, offset, Bias::Left)
}

/// The display column of `point` in characters of the expanded row.
pub fn display_column(display: &TabSnapshot, point: TabPoint) -> u32 {
    let start = TabPoint::new(point.row(), 0);
    let count: usize = display
        .chunks(start..point)
        .map(|chunk| chunk.text.chars().count())
        .sum();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The point of display row `row` at character column `column`, or the end
/// of the row if it is shorter.
pub fn point_at_column(display: &TabSnapshot, row: u32, column: u32) -> TabPoint {
    let len = display.line_len(row);
    let mut chars = 0u32;
    let mut bytes = 0u32;
    for chunk in display.chunks(TabPoint::new(row, 0)..TabPoint::new(row, len)) {
        for c in chunk.text.chars() {
            if chars == column {
                return TabPoint::new(row, bytes);
            }
            chars += 1;
            bytes += c.len_utf8() as u32;
        }
    }
    TabPoint::new(row, len)
}

/// Moves `rows` display rows (negative: up). Past the first row the caret
/// goes to the start of the text, past the last to its end; the goal is
/// kept either way.
fn move_rows(
    display: &TabSnapshot,
    offset: usize,
    goal: SelectionGoal,
    rows: i64,
) -> (usize, SelectionGoal) {
    let text = display.buffer();
    let point = offset_to_display(display, offset);
    let column = match goal {
        SelectionGoal::Column(column) => column,
        SelectionGoal::None => display_column(display, point),
    };
    let goal = SelectionGoal::Column(column);
    let max_row = i64::from(display.max_point().row());
    let target = i64::from(point.row()) + rows;
    if target < 0 {
        return (0, goal);
    }
    if target > max_row {
        return (text.len(), goal);
    }
    let row = u32::try_from(target).unwrap_or(0);
    let offset = display_to_offset(display, point_at_column(display, row, column));
    (snap_to_grapheme(text, offset), goal)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::display_map::{ReplaceMap, TabMap};

    fn display(text: &str) -> TabSnapshot {
        let (_, replace) = ReplaceMap::new(TextSnapshot::new(text));
        let (_, tabs) = TabMap::new(replace, NonZeroU32::new(4).unwrap());
        tabs
    }

    fn walk(display: &TabSnapshot, from: usize, motion: Motion, steps: usize) -> Vec<usize> {
        let mut out = vec![];
        let (mut at, mut goal) = (from, SelectionGoal::None);
        for _ in 0..steps {
            (at, goal) = move_caret(display, at, goal, motion);
            out.push(at);
        }
        out
    }

    #[test]
    fn horizontal() {
        let d = display("  a\u{301}b\r\ncd");
        assert_eq!(walk(&d, 0, Motion::Right, 7), [1, 2, 5, 6, 8, 9, 10]);
        assert_eq!(walk(&d, 10, Motion::Left, 7), [9, 8, 6, 5, 2, 1, 0]);
        let (end, _) = move_caret(&d, 3, SelectionGoal::None, Motion::LineEnd);
        assert_eq!(end, 6);
        let (start, _) = move_caret(&d, 9, SelectionGoal::None, Motion::LineStart);
        assert_eq!(start, 8);
        assert_eq!(walk(&d, 5, Motion::SmartLineStart, 3), [2, 0, 2]);
        let (end, _) = move_caret(&d, 0, SelectionGoal::None, Motion::DocumentEnd);
        assert_eq!(end, 10);
        // Inside the CRLF: clipped before the CR first.
        let (right, _) = move_caret(&d, 7, SelectionGoal::None, Motion::Right);
        assert_eq!(right, 8);
    }

    #[test]
    fn vertical_keeps_the_goal() {
        let d = display("long line\nab\n\nanother line\n\tx");
        let (at, goal) = move_caret(&d, 7, SelectionGoal::None, Motion::Down);
        assert_eq!((at, goal), (12, SelectionGoal::Column(7)));
        let (at, goal) = move_caret(&d, at, goal, Motion::Down);
        assert_eq!(at, 13);
        let (at, goal) = move_caret(&d, at, goal, Motion::Down);
        assert_eq!(at, 14 + 7);
        // The tab expands to 4 columns: column 7 is past `x` (column 5).
        let (at, goal) = move_caret(&d, at, goal, Motion::Down);
        assert_eq!(at, 29);
        let (at, goal) = move_caret(&d, at, goal, Motion::Up);
        assert_eq!(at, 21);
        // Past the last row: the end of the text; past the first: the start.
        let (end, goal) = move_caret(&d, 21, goal, Motion::DownRows(10));
        assert_eq!(end, 29);
        let (start, goal) = move_caret(&d, end, goal, Motion::UpRows(10));
        assert_eq!(start, 0);
        assert_eq!(goal, SelectionGoal::Column(7));
    }

    #[test]
    fn vertical_lands_on_grapheme_and_tab_starts() {
        // Column 2 on the second row is inside `a` + combining accent (one
        // cluster, two chars): the caret snaps to the cluster start.
        let d = display("xyz\nxa\u{301}b\n\tq");
        let (at, goal) = move_caret(&d, 2, SelectionGoal::None, Motion::Down);
        assert_eq!(at, 5);
        // Column 2 on the third row is inside the expanded tab: the tab.
        let (at, _) = move_caret(&d, 6, goal, Motion::Down);
        assert_eq!(at, 10);
    }

    #[test]
    fn vertical_with_crlf_and_cr() {
        let d = display("abc\r\nde\rfghij");
        let (at, goal) = move_caret(&d, 3, SelectionGoal::None, Motion::Down);
        assert_eq!(at, 7);
        let (at, _) = move_caret(&d, at, goal, Motion::Down);
        assert_eq!(at, 11);
    }
}

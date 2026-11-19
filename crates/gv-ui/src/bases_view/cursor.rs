// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keyboard's place in a table or list layout.
//!
//! The view keeps the keyboard; the rows are not Tab stops. The arrow
//! keys move a cursor over the rows (and, in a table, over the columns),
//! drawn as the row's selection colour with a ring around the cell. The
//! cursor's cell (a list's item) is the focused view's active
//! descendant, so a screen reader reads it as if it had the focus: the
//! cell's text, its column header and its row. Group headers and
//! summary lines are passed over. The cursor follows its file when the
//! base is evaluated again, and starts afresh in another view.

use super::table::Line;

/// The rows Page Up and Page Down move by.
pub(crate) const PAGE: usize = 10;

/// Where a key moves the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Next,
    Previous,
    First,
    Last,
    PageDown,
    PageUp,
}

/// The line of `lines` the cursor moves to from `from` (a line, `None`
/// before the first move): always a row, `None` without rows. Moving
/// stops at the ends; the first move from nowhere goes to the first row
/// (the last for End).
pub(crate) fn step_row(lines: &[Line], from: Option<usize>, step: Step) -> Option<usize> {
    let rows: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| matches!(l, Line::Row(..)))
        .map(|(i, _)| i)
        .collect();
    let last = rows.len().checked_sub(1)?;
    let Some(from) = from else {
        return Some(if step == Step::Last {
            rows[last]
        } else {
            rows[0]
        });
    };
    // The row at `from`, or the first one after it.
    let at = rows.partition_point(|&r| r < from).min(last);
    let to = match step {
        Step::Next => (at + 1).min(last),
        Step::Previous => at.saturating_sub(1),
        Step::First => 0,
        Step::Last => last,
        Step::PageDown => (at + PAGE).min(last),
        Step::PageUp => at.saturating_sub(PAGE),
    };
    Some(rows[to])
}

/// The column the cursor moves to from `column` of `count`, one step
/// right (`forward`) or left; it stops at the ends.
pub(crate) fn step_column(column: usize, count: usize, forward: bool) -> usize {
    let last = count.saturating_sub(1);
    if forward {
        (column + 1).min(last)
    } else {
        column.saturating_sub(1).min(last)
    }
}

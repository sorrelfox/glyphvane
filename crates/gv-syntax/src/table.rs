// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GFM table rows: cell splitting, the delimiter row and cell text.
//!
//! The rules are cmark-gfm's (`extensions/table.c`, GFM spec 0.29):
//!
//! - A row is split at `|` characters that are not preceded by a
//!   backslash. A leading `|` and a trailing `|` (each with the spaces and
//!   tabs after it) open and close the row without adding an empty cell.
//!   Cells are trimmed. A line with no cell (`|` alone, or blank) is not a
//!   row.
//! - The delimiter row is `|? :?-+:? (| :?-+:?)* |?`, with spaces and tabs
//!   around each part; its colons give the column alignments.
//! - Before a cell's inlines are parsed, every backslash directly before a
//!   `|` is removed (also inside code spans), see [`cell_pieces`].
//!
//! Every function takes one line's content, without its line ending.

use std::ops::Range;

/// cmark-gfm `table_spacechar`.
#[inline]
fn is_table_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x0B | 0x0C)
}

/// Rows with more cells are not rows (cmark-gfm caps the count at
/// `u16::MAX`; one value less here, so that a count always fits a `u16`
/// below the block pass's sentinel).
pub(crate) const MAX_CELLS: usize = u16::MAX as usize - 1;

/// Column alignment from the delimiter row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Alignment {
    /// No colon.
    None,
    /// `:--`.
    Left,
    /// `:-:`.
    Center,
    /// `--:`.
    Right,
}

impl Alignment {
    /// The value of the HTML `align` attribute, if any.
    pub fn html(self) -> Option<&'static str> {
        match self {
            Alignment::None => None,
            Alignment::Left => Some("left"),
            Alignment::Center => Some("center"),
            Alignment::Right => Some("right"),
        }
    }
}

/// `[|] table_spacechar*` at `i`: its length, or 0.
#[inline]
fn cell_end(s: &[u8], i: usize) -> usize {
    if s.get(i) != Some(&b'|') {
        return 0;
    }
    let mut j = i + 1;
    while j < s.len() && is_table_space(s[j]) {
        j += 1;
    }
    j - i
}

/// The cell text at `i`: up to the next `|` that does not follow a
/// backslash (cmark-gfm's longest match of `(escaped_char | [^|\r\n])+`).
#[inline]
fn cell_text(s: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < s.len() {
        match s[j] {
            b'|' if j == i || s[j - 1] != b'\\' => break,
            b'\n' | b'\r' => break,
            _ => j += 1,
        }
    }
    j - i
}

/// Trims cmark whitespace (space, tab, LF, VT, FF, CR) from both ends.
fn trim(s: &[u8], mut a: usize, mut b: usize) -> Range<usize> {
    while a < b && matches!(s[a], b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
        a += 1;
    }
    while b > a && matches!(s[b - 1], b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') {
        b -= 1;
    }
    a..b
}

/// Calls `f` with the trimmed range of each cell of the row `line` and
/// returns the number of cells; `None` if `line` is not a row.
fn scan_row(line: &[u8], mut f: impl FnMut(Range<usize>)) -> Option<usize> {
    let len = line.len();
    let mut offset = cell_end(line, 0);
    let mut n = 0usize;
    while offset < len {
        let cell = cell_text(line, offset);
        let pipe = cell_end(line, offset + cell);
        if cell == 0 && pipe == 0 {
            // A line ending inside the content: not a row.
            return None;
        }
        if n == MAX_CELLS {
            return None;
        }
        n += 1;
        f(trim(line, offset, offset + cell));
        offset += cell + pipe;
    }
    (n > 0).then_some(n)
}

/// The number of cells of the row `line`, or 0 if it is not a row.
pub(crate) fn cell_count(line: &[u8]) -> usize {
    scan_row(line, |_| {}).unwrap_or(0)
}

/// The cells of the row `line` (trimmed byte ranges into `line`), or
/// `None` if it is not a row.
pub fn row_cells(line: &[u8]) -> Option<Vec<Range<usize>>> {
    let mut cells = Vec::new();
    scan_row(line, |r| cells.push(r))?;
    Some(cells)
}

/// Whether `line` matches cmark-gfm's `table_start` (the delimiter row
/// syntax), with at least `min_dashes` dashes in every cell.
fn is_delimiter_row(s: &[u8], min_dashes: u8) -> bool {
    let len = s.len();
    let mut i = 0;
    if s.first() == Some(&b'|') {
        i = 1;
    }
    let mut groups = 0;
    loop {
        // table_spacechar* :? -+ :? table_spacechar*
        let mut j = i;
        while j < len && is_table_space(s[j]) {
            j += 1;
        }
        if j < len && s[j] == b':' {
            j += 1;
        }
        let dashes = j;
        while j < len && s[j] == b'-' {
            j += 1;
        }
        if j == dashes {
            break;
        }
        if j - dashes < usize::from(min_dashes) {
            return false;
        }
        if j < len && s[j] == b':' {
            j += 1;
        }
        while j < len && is_table_space(s[j]) {
            j += 1;
        }
        groups += 1;
        i = j;
        if i < len && s[i] == b'|' {
            i += 1;
        } else {
            break;
        }
    }
    if groups == 0 {
        return false;
    }
    // An optional trailing `|` was consumed by the loop; spaces may follow.
    while i < len && is_table_space(s[i]) {
        i += 1;
    }
    i == len
}

/// The column alignments if `line` is a delimiter row.
pub fn delimiter_row(line: &[u8]) -> Option<Vec<Alignment>> {
    if !is_delimiter_row(line, 1) {
        return None;
    }
    let mut out = Vec::new();
    scan_row(line, |r| {
        let c = &line[r];
        let left = c.first() == Some(&b':');
        let right = c.last() == Some(&b':');
        out.push(match (left, right) {
            (true, true) => Alignment::Center,
            (true, false) => Alignment::Left,
            (false, true) => Alignment::Right,
            (false, false) => Alignment::None,
        });
    })?;
    Some(out)
}

/// The number of columns if `line` is a delimiter row with at least
/// `min_dashes` dashes per cell, else 0.
pub(crate) fn delimiter_columns(line: &[u8], min_dashes: u8) -> usize {
    if is_delimiter_row(line, min_dashes) {
        cell_count(line)
    } else {
        0
    }
}

/// The pieces of a cell's text once the backslash before each `|` is
/// removed: `cell` (a range into the document) split around those
/// backslashes. `bytes` are the document bytes of `cell`.
pub fn cell_pieces(cell: Range<usize>, bytes: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::with_capacity(1);
    let mut start = 0;
    for i in memchr::memchr_iter(b'|', bytes) {
        if i > 0 && bytes[i - 1] == b'\\' {
            if i - 1 > start {
                out.push(cell.start + start..cell.start + i - 1);
            }
            start = i;
        }
    }
    if start < bytes.len() || out.is_empty() {
        out.push(cell.start + start..cell.end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(s: &str) -> Option<Vec<&str>> {
        row_cells(s.as_bytes()).map(|v| v.into_iter().map(|r| &s[r]).collect())
    }

    #[test]
    fn rows() {
        assert_eq!(cells("| foo | bar |"), Some(vec!["foo", "bar"]));
        assert_eq!(cells("foo|bar"), Some(vec!["foo", "bar"]));
        assert_eq!(cells("a"), Some(vec!["a"]));
        assert_eq!(cells("|"), None);
        assert_eq!(cells("| "), None);
        assert_eq!(cells(""), None);
        assert_eq!(cells("||"), Some(vec![""]));
        assert_eq!(cells("| |"), Some(vec![""]));
        assert_eq!(cells("|||"), Some(vec!["", ""]));
        assert_eq!(cells(" | a"), Some(vec!["", "a"]));
        assert_eq!(cells(r"| f\|oo  |"), Some(vec![r"f\|oo"]));
        assert_eq!(cells(r"a\\|b"), Some(vec![r"a\\|b"]));
        assert_eq!(cells("a | b |  "), Some(vec!["a", "b"]));
    }

    #[test]
    fn delimiters() {
        use Alignment as A;
        assert_eq!(
            delimiter_row(b"| --- | :-: |"),
            Some(vec![A::None, A::Center])
        );
        assert_eq!(
            delimiter_row(b":-: | -----------:"),
            Some(vec![A::Center, A::Right])
        );
        assert_eq!(delimiter_row(b"---"), Some(vec![A::None]));
        assert_eq!(delimiter_row(b"|:--"), Some(vec![A::Left]));
        assert_eq!(delimiter_row(b"| - | - |  "), Some(vec![A::None, A::None]));
        assert_eq!(delimiter_row(b"|"), None);
        assert_eq!(delimiter_row(b"| a |"), None);
        assert_eq!(delimiter_row(b"-- -"), None);
        assert_eq!(delimiter_row(b"::"), None);
        assert_eq!(delimiter_row(b"|-||-|"), None);
        assert_eq!(delimiter_columns(b"--|--", 1), 2);
        assert_eq!(delimiter_columns(b"--|--", 2), 2);
        assert_eq!(delimiter_columns(b"--|-:", 2), 0);
    }

    #[test]
    fn pieces() {
        let s = r"a\|b\|";
        let p = cell_pieces(10..10 + s.len(), s.as_bytes());
        assert_eq!(p, vec![10..11, 12..14, 15..16]);
        assert_eq!(cell_pieces(0..3, b"abc"), vec![0..3]);
        assert_eq!(cell_pieces(0..2, br"\|"), vec![1..2]);
        assert_eq!(cell_pieces(0..0, b""), vec![0..0]);
    }
}

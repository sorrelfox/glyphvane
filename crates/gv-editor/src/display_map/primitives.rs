// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors
//
// Derived from Zed (https://github.com/zed-industries/zed) at commit
// d2a4fb6ded7c470be25d1541281da3882495b65b:
//   - `Point` and its arithmetic: crates/rope/src/point.rs
//   - `Edit`: crates/text/src/text.rs (`Edit<D>`)
//   - `TextSummary`: crates/editor/src/display_map/tab_map.rs, with the
//     string scan of crates/rope/src/rope.rs (`From<&str> for TextSummary`)
//   - `Chunk`: crates/editor/src/display_map/fold_map.rs
// Copyright (C) Zed Industries, Inc. and the Zed contributors.
// Licensed under the GNU General Public License, version 3 or later.
//
// Modified by the Glyphvane contributors (2026-11-13): reduced to the fields
// and operations the display-map layers use, without highlighting, renderer
// or UTF-16 fields; see third_party/zed/PROVENANCE.md. (2026-11-16): chunks
// carry a style id and the replacement they show instead of Zed's
// highlight style and renderer.

//! Types shared by every display-map layer: a row/column [`Point`], an
//! [`Edit`] between two snapshots, a [`TextSummary`] of a range, the
//! [`Chunk`]s that layers hand to each other, and the [`Highlight`]s a
//! caller can ask chunks to carry.

use std::ops::{Add, AddAssign, Range, Sub};

use super::replace_map::Replaced;

/// The largest chunk a layer hands on, in bytes. Chunk bitmaps are `u128`,
/// one bit per byte.
pub const MAX_CHUNK_LEN: usize = u128::BITS as usize;

/// A zero-based row and a column in **bytes** from the start of the row, in
/// the text of one display-map layer.
///
/// Every layer point (`ReplacePoint`, `TabPoint`, …) wraps one. Rows and
/// columns are `u32`: the display map handles texts up to 4 GiB.
///
/// Adding and subtracting treat a point as a *distance*: adding a point with
/// `row == 0` moves the column, adding one with `row > 0` moves down that
/// many rows and sets the column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Point {
    /// Zero-based row.
    pub row: u32,
    /// Byte offset from the start of the row.
    pub column: u32,
}

impl Point {
    /// Creates a point.
    pub const fn new(row: u32, column: u32) -> Self {
        Self { row, column }
    }

    /// The point `0:0`.
    pub const fn zero() -> Self {
        Self::new(0, 0)
    }

    /// Whether this is `0:0`.
    pub const fn is_zero(self) -> bool {
        self.row == 0 && self.column == 0
    }
}

impl Add for Point {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        if other.row == 0 {
            Self::new(self.row, self.column + other.column)
        } else {
            Self::new(self.row + other.row, other.column)
        }
    }
}

impl AddAssign for Point {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

impl Sub for Point {
    type Output = Self;

    /// The distance from `other` to `self`. `other` must not be after
    /// `self`.
    fn sub(self, other: Self) -> Self {
        debug_assert!(other <= self, "{other:?} is after {self:?}");
        if self.row == other.row {
            Self::new(0, self.column.saturating_sub(other.column))
        } else {
            Self::new(self.row.saturating_sub(other.row), self.column)
        }
    }
}

/// One change between two snapshots of a layer: the range it replaced in the
/// old snapshot and the range of the replacement in the new one.
///
/// A layer's `sync` returns its edits sorted and non-overlapping; together
/// they turn the old text into the new text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Edit<D> {
    /// The replaced range, in the old snapshot.
    pub old: Range<D>,
    /// The replacement, in the new snapshot.
    pub new: Range<D>,
}

/// Line lengths of a range of layer text, in `char`s.
///
/// `longest_row` and `longest_row_chars` are taken from the layer below the
/// tab layer, so they do not count tab expansion (as in Zed): they are an
/// estimate for the widest row, used for horizontal scrolling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextSummary {
    /// The end of the range relative to its start: rows spanned and the
    /// length of the last row in bytes.
    pub lines: Point,
    /// `char`s in the first row.
    pub first_line_chars: u32,
    /// `char`s in the last row.
    pub last_line_chars: u32,
    /// The row (relative to the start of the range) with the most `char`s;
    /// the first one on a tie.
    pub longest_row: u32,
    /// `char`s in that row.
    pub longest_row_chars: u32,
}

impl From<&str> for TextSummary {
    /// The summary of `text`, whose rows are separated by `\n`.
    fn from(text: &str) -> Self {
        let mut lines = Point::zero();
        let mut first_line_chars = 0;
        let mut last_line_chars = 0;
        let mut longest_row = 0;
        let mut longest_row_chars = 0;
        for c in text.chars() {
            if c == '\n' {
                lines += Point::new(1, 0);
                last_line_chars = 0;
            } else {
                lines.column += c.len_utf8() as u32;
                last_line_chars += 1;
            }
            if lines.row == 0 {
                first_line_chars = last_line_chars;
            }
            if last_line_chars > longest_row_chars {
                longest_row = lines.row;
                longest_row_chars = last_line_chars;
            }
        }
        Self {
            lines,
            first_line_chars,
            last_line_chars,
            longest_row,
            longest_row_chars,
        }
    }
}

impl AddAssign<&Self> for TextSummary {
    /// Appends the summary of the text that directly follows.
    fn add_assign(&mut self, other: &Self) {
        let joined_chars = self.last_line_chars + other.first_line_chars;
        if joined_chars > self.longest_row_chars {
            self.longest_row = self.lines.row;
            self.longest_row_chars = joined_chars;
        }
        if other.longest_row_chars > self.longest_row_chars {
            self.longest_row = self.lines.row + other.longest_row;
            self.longest_row_chars = other.longest_row_chars;
        }

        if self.lines.row == 0 {
            self.first_line_chars += other.first_line_chars;
        }

        if other.lines.row == 0 {
            self.last_line_chars += other.first_line_chars;
        } else {
            self.last_line_chars = other.last_line_chars;
        }

        self.lines += other.lines;
    }
}

/// A style class: an id the caller of `chunks` gives meaning to (a syntax
/// class, a Live Preview style). The display map only carries it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StyleId(pub u32);

/// A style class over a range of **buffer** bytes, for the
/// `highlighted_chunks` of every layer. A slice of them must be sorted and
/// must not overlap; an end inside a character takes in the whole
/// character.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Highlight {
    /// Buffer bytes.
    pub range: Range<usize>,
    /// The class.
    pub style: StyleId,
}

/// A piece of layer text, at most [`MAX_CHUNK_LEN`] bytes, with bitmaps
/// that have one bit per byte of `text`.
///
/// Rows are separated by `\n` only: the layer above the buffer turns every
/// line terminator (LF, CRLF, lone CR) into one `\n`.
///
/// The tab and wrap layers keep `style` and `replaced` on the pieces they
/// cut a chunk into (expanded tabs included); the text they add (soft-wrap
/// breaks and indents) has neither.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// The text.
    pub text: &'a str,
    /// Bit `i` is set where a `char` starts at byte `i`.
    pub chars: u128,
    /// Bit `i` is set where byte `i` is a `\t`.
    pub tabs: u128,
    /// Bit `i` is set where byte `i` is a `\n`.
    pub newlines: u128,
    /// Whether the text is the expansion of a tab (only spaces).
    pub is_tab: bool,
    /// The style class: a widget's own, or the [`Highlight`] over the
    /// buffer text the chunk shows. `None` without either.
    pub style: Option<StyleId>,
    /// The replacement the chunk shows (a fold placeholder or a widget's
    /// text, always a chunk of its own); `None` for buffer text.
    pub replaced: Option<Replaced>,
}

impl<'a> Chunk<'a> {
    /// A chunk of plain text, with its bitmaps computed.
    ///
    /// `text` must be at most [`MAX_CHUNK_LEN`] bytes long; bytes past that
    /// get no bits.
    pub fn new(text: &'a str) -> Self {
        debug_assert!(text.len() <= MAX_CHUNK_LEN, "chunk of {} bytes", text.len());
        let mut chars = 0u128;
        let mut tabs = 0u128;
        let mut newlines = 0u128;
        for (index, c) in text.char_indices().take_while(|(i, _)| *i < MAX_CHUNK_LEN) {
            let bit = 1u128 << index;
            chars |= bit;
            match c {
                '\t' => tabs |= bit,
                '\n' => newlines |= bit,
                _ => {}
            }
        }
        Self {
            text,
            chars,
            tabs,
            newlines,
            is_tab: false,
            style: None,
            replaced: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_arithmetic() {
        assert_eq!(Point::new(1, 2) + Point::new(0, 3), Point::new(1, 5));
        assert_eq!(Point::new(1, 2) + Point::new(2, 3), Point::new(3, 3));
        assert_eq!(Point::new(3, 3) - Point::new(1, 2), Point::new(2, 3));
        assert_eq!(Point::new(1, 5) - Point::new(1, 2), Point::new(0, 3));
        assert!(Point::new(0, 9) < Point::new(1, 0));
    }

    #[test]
    fn summary_of_concatenation() {
        let texts = ["", "a", "ab\n", "\nxyz", "αβγ\n\nlonger line\nend", "\n"];
        for a in texts {
            for b in texts {
                let mut summary = TextSummary::from(a);
                summary += &TextSummary::from(b);
                let joined = format!("{a}{b}");
                assert_eq!(summary, TextSummary::from(joined.as_str()), "{a:?} + {b:?}");
            }
        }
    }

    #[test]
    fn chunk_bitmaps() {
        let chunk = Chunk::new("a\tβ\n");
        assert_eq!(chunk.chars, 0b10111);
        assert_eq!(chunk.tabs, 0b10);
        assert_eq!(chunk.newlines, 0b10000);
        assert!(!chunk.is_tab);
    }
}

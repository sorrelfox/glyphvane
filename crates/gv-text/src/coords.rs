// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Coordinate types and the position error.

use std::fmt;
use std::ops::{Add, AddAssign, Sub};

/// Which side a position attaches to when it has to move.
///
/// Used in two places:
///
/// - **Clipping.** An invalid position (inside a multi-byte character or a
///   surrogate pair) is moved to the nearest valid position on the left or
///   on the right.
/// - **Anchors.** An anchor at an insertion point stays before the inserted
///   text ([`Bias::Left`]) or moves after it ([`Bias::Right`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Bias {
    /// Towards the start of the text.
    #[default]
    Left,
    /// Towards the end of the text.
    Right,
}

/// A position as a zero-based line and a column in **bytes** from the start
/// of that line.
///
/// Lines are separated by LF, CRLF or a lone CR (see the crate docs). The
/// column of the position between the CR and the LF of a CRLF is the length
/// of the line's content plus one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Point {
    /// Zero-based line number.
    pub line: usize,
    /// Byte offset from the start of the line.
    pub column: usize,
}

impl Point {
    /// Creates a point.
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// A position as a zero-based line and a column in **UTF-16 code units**
/// from the start of that line. This is what IME and accessibility APIs use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PointUtf16 {
    /// Zero-based line number.
    pub line: usize,
    /// UTF-16 code units from the start of the line.
    pub column: usize,
}

impl PointUtf16 {
    /// Creates a point.
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// An offset in UTF-16 code units from the start of the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OffsetUtf16(pub usize);

impl Add for OffsetUtf16 {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0 + rhs.0)
    }
}

impl AddAssign for OffsetUtf16 {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for OffsetUtf16 {
    type Output = Self;
    /// # Panics
    ///
    /// On underflow (in debug builds), like `usize` subtraction.
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 - rhs.0)
    }
}

/// Why a position was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PositionError {
    /// A byte offset is past the end of the text.
    OffsetOutOfBounds {
        /// The offending offset.
        offset: usize,
        /// The text length in bytes.
        len: usize,
    },
    /// A byte offset falls inside a multi-byte UTF-8 character.
    NotCharBoundary {
        /// The offending offset.
        offset: usize,
    },
    /// A UTF-16 offset is past the end of the text.
    Utf16OutOfBounds {
        /// The offending offset.
        offset: usize,
        /// The text length in UTF-16 code units.
        len: usize,
    },
    /// A UTF-16 offset falls between the two halves of a surrogate pair.
    InsideSurrogatePair {
        /// The offending UTF-16 offset.
        offset: usize,
    },
    /// A line number is past the last line.
    LineOutOfBounds {
        /// The offending line.
        line: usize,
        /// The number of lines.
        line_count: usize,
    },
    /// A column is past the end of its line.
    ColumnOutOfBounds {
        /// The line.
        line: usize,
        /// The offending column.
        column: usize,
        /// The largest valid column on that line.
        max: usize,
    },
    /// A range ends before it starts.
    InvertedRange {
        /// The range start.
        start: usize,
        /// The range end.
        end: usize,
    },
}

impl fmt::Display for PositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::OffsetOutOfBounds { offset, len } => {
                write!(
                    f,
                    "byte offset {offset} is past the end of the text ({len} bytes)"
                )
            }
            Self::NotCharBoundary { offset } => {
                write!(f, "byte offset {offset} is inside a character")
            }
            Self::Utf16OutOfBounds { offset, len } => write!(
                f,
                "UTF-16 offset {offset} is past the end of the text ({len} code units)"
            ),
            Self::InsideSurrogatePair { offset } => {
                write!(f, "UTF-16 offset {offset} is inside a surrogate pair")
            }
            Self::LineOutOfBounds { line, line_count } => {
                write!(
                    f,
                    "line {line} does not exist (the text has {line_count} lines)"
                )
            }
            Self::ColumnOutOfBounds { line, column, max } => {
                write!(
                    f,
                    "column {column} is past the end of line {line} (at most {max})"
                )
            }
            Self::InvertedRange { start, end } => {
                write!(f, "range {start}..{end} ends before it starts")
            }
        }
    }
}

impl std::error::Error for PositionError {}

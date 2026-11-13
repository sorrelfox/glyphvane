// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`TextSnapshot`]: the immutable text with its line index, and every
//! position conversion.

use std::fmt;
use std::ops::Range;

use rope::Rope;

use crate::coords::{Bias, OffsetUtf16, Point, PointUtf16, PositionError};
use crate::lines::{LineEnding, LineEndingStats, LineIndex, LineIter, LineRef};

/// A buffer revision. It starts at zero and increases by one with every
/// transaction that changes the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Revision(pub u64);

impl Revision {
    /// The revision of a newly created buffer.
    pub const ZERO: Self = Self(0);

    pub(crate) fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

/// An immutable view of the text at one revision.
///
/// Cloning is O(1) (two reference-counted trees), and snapshots are `Send` and
/// `Sync`, so the parser and other background work can hold one while the
/// buffer keeps changing.
///
/// Every method that takes a position from outside either validates it and
/// returns a [`PositionError`], or clips it explicitly (`clip_*`). None of them
/// silently moves a position.
#[derive(Clone)]
pub struct TextSnapshot {
    pub(crate) rope: Rope,
    pub(crate) lines: LineIndex,
    pub(crate) revision: Revision,
}

impl Default for TextSnapshot {
    fn default() -> Self {
        Self::new("")
    }
}

impl fmt::Debug for TextSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextSnapshot")
            .field("revision", &self.revision)
            .field("len", &self.len())
            .field("lines", &self.line_count())
            .finish_non_exhaustive()
    }
}

impl fmt::Display for TextSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.rope.chunks() {
            f.write_str(chunk)?;
        }
        Ok(())
    }
}

/// A line with its byte range and terminator.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LineInfo {
    /// Zero-based line number.
    pub line: usize,
    /// The line's content, without the terminator.
    pub content: Range<usize>,
    /// The terminator, or `None` for the last line.
    pub terminator: Option<LineEnding>,
}

impl LineInfo {
    /// The line's range including its terminator.
    pub fn range_with_terminator(&self) -> Range<usize> {
        self.content.start..self.content.end + self.terminator.map_or(0, LineEnding::byte_len)
    }

    fn from_ref(line: LineRef) -> Self {
        Self {
            line: line.index,
            content: line.start..line.content_end(),
            terminator: line.item.terminator,
        }
    }
}

impl TextSnapshot {
    /// A snapshot of `text` at [`Revision::ZERO`].
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from(text),
            lines: LineIndex::from_str(text),
            revision: Revision::ZERO,
        }
    }

    /// The revision this snapshot was taken at.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.rope.len()
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.rope.is_empty()
    }

    /// Length in UTF-16 code units.
    pub fn len_utf16(&self) -> OffsetUtf16 {
        OffsetUtf16(self.rope.summary().len_utf16.0)
    }

    /// Number of `char`s (Unicode scalar values).
    pub fn char_count(&self) -> usize {
        self.rope.summary().chars
    }

    /// Whether this snapshot has the same text as `other`.
    pub fn text_eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .all_chunks()
                .flat_map(str::bytes)
                .eq(other.all_chunks().flat_map(str::bytes))
    }

    // ----- byte offsets -------------------------------------------------

    /// Whether `offset` is a valid position: at most [`len`](Self::len) and
    /// not inside a multi-byte character.
    pub fn is_char_boundary(&self, offset: usize) -> bool {
        offset == self.len() || (offset < self.len() && self.rope.is_char_boundary(offset))
    }

    /// Returns `offset` if it is a valid position.
    pub fn check_offset(&self, offset: usize) -> Result<usize, PositionError> {
        if offset > self.len() {
            Err(PositionError::OffsetOutOfBounds {
                offset,
                len: self.len(),
            })
        } else if !self.is_char_boundary(offset) {
            Err(PositionError::NotCharBoundary { offset })
        } else {
            Ok(offset)
        }
    }

    /// Returns `range` if both ends are valid positions and it is not
    /// inverted.
    pub fn check_range(&self, range: Range<usize>) -> Result<Range<usize>, PositionError> {
        if range.start > range.end {
            return Err(PositionError::InvertedRange {
                start: range.start,
                end: range.end,
            });
        }
        Ok(self.check_offset(range.start)?..self.check_offset(range.end)?)
    }

    /// Clamps `offset` to the text and moves it out of a multi-byte character
    /// in the direction of `bias`.
    pub fn clip_offset(&self, offset: usize, bias: Bias) -> usize {
        if offset >= self.len() {
            return self.len();
        }
        match bias {
            Bias::Left => self.rope.floor_char_boundary(offset),
            Bias::Right => self.rope.ceil_char_boundary(offset),
        }
    }

    /// Whether `offset` lies between the CR and the LF of a CRLF. Such a
    /// position is a valid char boundary, but an editor caret should not rest
    /// there.
    pub fn is_inside_crlf(&self, offset: usize) -> bool {
        if offset == 0 || offset >= self.len() {
            return false;
        }
        let line = self.lines.line_at(offset);
        line.item.terminator == Some(LineEnding::CrLf) && offset == line.end() - 1
    }

    // ----- lines --------------------------------------------------------

    /// Number of lines. Always at least 1; a text ending in a terminator has
    /// an empty last line.
    pub fn line_count(&self) -> usize {
        self.lines.line_count()
    }

    /// Terminator counts, in O(1).
    pub fn line_ending_stats(&self) -> LineEndingStats {
        self.lines.summary().stats
    }

    fn line_ref(&self, line: usize) -> Result<LineRef, PositionError> {
        self.lines.line(line).ok_or(PositionError::LineOutOfBounds {
            line,
            line_count: self.line_count(),
        })
    }

    /// The line `line`: its content range and terminator.
    pub fn line_info(&self, line: usize) -> Result<LineInfo, PositionError> {
        self.line_ref(line).map(LineInfo::from_ref)
    }

    /// Byte offset of the start of line `line`.
    pub fn line_start(&self, line: usize) -> Result<usize, PositionError> {
        Ok(self.line_ref(line)?.start)
    }

    /// The content of line `line`, without its terminator.
    pub fn line_range(&self, line: usize) -> Result<Range<usize>, PositionError> {
        let line = self.line_ref(line)?;
        Ok(line.start..line.content_end())
    }

    /// Line `line` including its terminator.
    pub fn line_range_with_terminator(&self, line: usize) -> Result<Range<usize>, PositionError> {
        let line = self.line_ref(line)?;
        Ok(line.start..line.end())
    }

    /// The terminator of line `line` (`None` for the last line).
    pub fn line_terminator(&self, line: usize) -> Result<Option<LineEnding>, PositionError> {
        Ok(self.line_ref(line)?.item.terminator)
    }

    /// The line holding byte `offset`.
    pub fn line_of_offset(&self, offset: usize) -> Result<usize, PositionError> {
        self.check_offset(offset)?;
        Ok(self.lines.line_at(offset).index)
    }

    /// Iterates lines from line `first` on (nothing if `first` is past the
    /// end).
    pub fn lines_from(&self, first: usize) -> Lines<'_> {
        Lines {
            inner: self.lines.iter_from(first),
        }
    }

    // ----- points -------------------------------------------------------

    /// The largest valid point (the end of the text).
    pub fn max_point(&self) -> Point {
        let last = self.lines.line_at(self.len());
        Point::new(last.index, last.item.len)
    }

    /// Converts a byte offset to a line and byte column.
    pub fn offset_to_point(&self, offset: usize) -> Result<Point, PositionError> {
        self.check_offset(offset)?;
        let line = self.lines.line_at(offset);
        Ok(Point::new(line.index, offset - line.start))
    }

    /// Converts a line and byte column to a byte offset. The column may be at
    /// most the line's content length (plus one between the CR and LF of a
    /// CRLF) and must not fall inside a character.
    pub fn point_to_offset(&self, point: Point) -> Result<usize, PositionError> {
        let line = self.line_ref(point.line)?;
        let max = line.item.max_column();
        if point.column > max {
            return Err(PositionError::ColumnOutOfBounds {
                line: point.line,
                column: point.column,
                max,
            });
        }
        self.check_offset(line.start + point.column)
    }

    /// Clips a point to a valid one: a line past the end goes to the end of
    /// the text, a column past the end of its line goes to the end of the
    /// line's content, and a column inside a character moves by `bias`.
    pub fn clip_point(&self, point: Point, bias: Bias) -> Point {
        let Some(line) = self.lines.line(point.line) else {
            return self.max_point();
        };
        if point.column > line.item.max_column() {
            return Point::new(line.index, line.item.content_len());
        }
        let offset = self.clip_offset(line.start + point.column, bias);
        Point::new(line.index, offset - line.start)
    }

    // ----- UTF-16 offsets -----------------------------------------------

    /// Converts a byte offset to a UTF-16 offset.
    pub fn offset_to_utf16(&self, offset: usize) -> Result<OffsetUtf16, PositionError> {
        self.check_offset(offset)?;
        Ok(self.utf16_of(offset))
    }

    /// UTF-16 offset of a valid byte offset.
    fn utf16_of(&self, offset: usize) -> OffsetUtf16 {
        OffsetUtf16(self.rope.offset_to_offset_utf16(offset).0)
    }

    /// Byte offset of a UTF-16 offset that is at most the UTF-16 length;
    /// `None` if it falls inside a surrogate pair.
    fn offset_of_utf16(&self, offset: OffsetUtf16) -> Option<usize> {
        let bytes = self
            .rope
            .offset_utf16_to_offset(rope::OffsetUtf16(offset.0));
        // Zed's rope returns an offset inside the character for a UTF-16
        // offset between surrogates; reject anything that does not map back.
        (self.is_char_boundary(bytes) && self.utf16_of(bytes) == offset).then_some(bytes)
    }

    /// Converts a UTF-16 offset to a byte offset.
    pub fn utf16_to_offset(&self, offset: OffsetUtf16) -> Result<usize, PositionError> {
        let len = self.len_utf16();
        if offset > len {
            return Err(PositionError::Utf16OutOfBounds {
                offset: offset.0,
                len: len.0,
            });
        }
        self.offset_of_utf16(offset)
            .ok_or(PositionError::InsideSurrogatePair { offset: offset.0 })
    }

    /// Clamps a UTF-16 offset to the text and moves it out of a surrogate
    /// pair in the direction of `bias`.
    pub fn clip_offset_utf16(&self, offset: OffsetUtf16, bias: Bias) -> OffsetUtf16 {
        self.utf16_of(self.clip_utf16_to_offset(offset, bias))
    }

    /// Like [`clip_offset_utf16`](Self::clip_offset_utf16), returning the
    /// byte offset.
    pub fn clip_utf16_to_offset(&self, offset: OffsetUtf16, bias: Bias) -> usize {
        if offset >= self.len_utf16() {
            return self.len();
        }
        if let Some(bytes) = self.offset_of_utf16(offset) {
            return bytes;
        }
        // `offset` splits a surrogate pair: find the character around it.
        let inside = self
            .rope
            .offset_utf16_to_offset(rope::OffsetUtf16(offset.0));
        match bias {
            Bias::Left => {
                let mut bytes = self.clip_offset(inside, Bias::Left);
                while bytes > 0 && self.utf16_of(bytes) > offset {
                    bytes = self.clip_offset(bytes - 1, Bias::Left);
                }
                bytes
            }
            Bias::Right => {
                let mut bytes = self.clip_offset(inside, Bias::Right);
                while bytes < self.len() && self.utf16_of(bytes) < offset {
                    bytes = self.clip_offset(bytes + 1, Bias::Right);
                }
                bytes
            }
        }
    }

    /// Converts a byte range to a UTF-16 range.
    pub fn range_to_utf16(&self, range: Range<usize>) -> Result<Range<OffsetUtf16>, PositionError> {
        let range = self.check_range(range)?;
        Ok(self.utf16_of(range.start)..self.utf16_of(range.end))
    }

    /// Converts a UTF-16 range to a byte range.
    pub fn utf16_to_range(&self, range: Range<OffsetUtf16>) -> Result<Range<usize>, PositionError> {
        if range.start > range.end {
            return Err(PositionError::InvertedRange {
                start: range.start.0,
                end: range.end.0,
            });
        }
        Ok(self.utf16_to_offset(range.start)?..self.utf16_to_offset(range.end)?)
    }

    /// Clips an untrusted UTF-16 range (from an IME or accessibility API) to
    /// a byte range: both ends are clamped to the text, an inverted range is
    /// reordered, and a range that cuts a surrogate pair grows to include the
    /// whole character.
    pub fn clip_utf16_range(&self, range: Range<OffsetUtf16>) -> Range<usize> {
        let (start, end) = if range.start <= range.end {
            (range.start, range.end)
        } else {
            (range.end, range.start)
        };
        self.clip_utf16_to_offset(start, Bias::Left)..self.clip_utf16_to_offset(end, Bias::Right)
    }

    // ----- UTF-16 points ------------------------------------------------

    /// The largest valid UTF-16 point (the end of the text).
    pub fn max_point_utf16(&self) -> PointUtf16 {
        let last = self.lines.line_at(self.len());
        PointUtf16::new(
            last.index,
            self.utf16_of(self.len()).0 - self.utf16_of(last.start).0,
        )
    }

    /// Converts a byte offset to a line and UTF-16 column.
    pub fn offset_to_point_utf16(&self, offset: usize) -> Result<PointUtf16, PositionError> {
        self.check_offset(offset)?;
        let line = self.lines.line_at(offset);
        Ok(PointUtf16::new(
            line.index,
            self.utf16_of(offset).0 - self.utf16_of(line.start).0,
        ))
    }

    /// Converts a line and UTF-16 column to a byte offset. The column may be
    /// at most the line's content length in UTF-16 (plus one between the CR
    /// and LF of a CRLF) and must not fall inside a surrogate pair.
    pub fn point_utf16_to_offset(&self, point: PointUtf16) -> Result<usize, PositionError> {
        let line = self.line_ref(point.line)?;
        let start = self.utf16_of(line.start);
        let max = self.utf16_of(line.content_end()).0 - start.0
            + usize::from(line.item.terminator == Some(LineEnding::CrLf));
        if point.column > max {
            return Err(PositionError::ColumnOutOfBounds {
                line: point.line,
                column: point.column,
                max,
            });
        }
        let target = OffsetUtf16(start.0 + point.column);
        self.offset_of_utf16(target)
            .ok_or(PositionError::InsideSurrogatePair { offset: target.0 })
    }

    /// Converts a byte point to a UTF-16 point.
    pub fn point_to_point_utf16(&self, point: Point) -> Result<PointUtf16, PositionError> {
        self.offset_to_point_utf16(self.point_to_offset(point)?)
    }

    /// Converts a UTF-16 point to a byte point.
    pub fn point_utf16_to_point(&self, point: PointUtf16) -> Result<Point, PositionError> {
        self.offset_to_point(self.point_utf16_to_offset(point)?)
    }

    /// Clips a UTF-16 point like [`clip_point`](Self::clip_point): a column
    /// inside a surrogate pair moves by `bias`.
    pub fn clip_point_utf16(&self, point: PointUtf16, bias: Bias) -> PointUtf16 {
        let Some(line) = self.lines.line(point.line) else {
            return self.max_point_utf16();
        };
        let start = self.utf16_of(line.start);
        let content = self.utf16_of(line.content_end()).0 - start.0;
        let max = content + usize::from(line.item.terminator == Some(LineEnding::CrLf));
        if point.column > max {
            return PointUtf16::new(line.index, content);
        }
        let offset = self.clip_utf16_to_offset(OffsetUtf16(start.0 + point.column), bias);
        PointUtf16::new(line.index, self.utf16_of(offset).0 - start.0)
    }

    // ----- content ------------------------------------------------------

    /// The chunks of the text in `range`, in order. Chunks are at most 128
    /// bytes and always whole characters.
    pub fn chunks(&self, range: Range<usize>) -> Result<Chunks<'_>, PositionError> {
        let range = self.check_range(range)?;
        Ok(Chunks {
            inner: self.rope.chunks_in_range(range),
        })
    }

    /// All chunks of the text.
    pub fn all_chunks(&self) -> Chunks<'_> {
        Chunks {
            inner: self.rope.chunks(),
        }
    }

    /// The text in `range`.
    pub fn text_for_range(&self, range: Range<usize>) -> Result<String, PositionError> {
        Ok(self.chunks(range)?.collect())
    }

    /// The characters from `offset` on.
    pub fn chars_at(
        &self,
        offset: usize,
    ) -> Result<impl Iterator<Item = char> + '_, PositionError> {
        Ok(self.rope.chars_at(self.check_offset(offset)?))
    }

    /// The characters before `offset`, from nearest to farthest.
    pub fn reversed_chars_at(
        &self,
        offset: usize,
    ) -> Result<impl Iterator<Item = char> + '_, PositionError> {
        Ok(self.rope.reversed_chars_at(self.check_offset(offset)?))
    }
}

/// Iterator over text chunks. See [`TextSnapshot::chunks`].
#[derive(Clone)]
pub struct Chunks<'a> {
    inner: rope::Chunks<'a>,
}

impl fmt::Debug for Chunks<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chunks")
            .field("offset", &self.inner.offset())
            .finish()
    }
}

impl Chunks<'_> {
    /// Byte offset of the next chunk.
    pub fn offset(&self) -> usize {
        self.inner.offset()
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        self.inner.next()
    }
}

/// Iterator over lines. See [`TextSnapshot::lines_from`].
pub struct Lines<'a> {
    inner: LineIter<'a>,
}

impl fmt::Debug for Lines<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lines").finish_non_exhaustive()
    }
}

impl Iterator for Lines<'_> {
    type Item = LineInfo;

    fn next(&mut self) -> Option<LineInfo> {
        self.inner.next().map(LineInfo::from_ref)
    }
}

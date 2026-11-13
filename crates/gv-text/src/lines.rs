// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The line model: line endings, line-ending statistics, and the line index.
//!
//! The index is a sum tree with one item per line (its length including the
//! terminator, and the terminator kind). It is independent of the rope's own
//! row model, which only knows LF.

use std::ops::Range;

use rope::Rope;
use sum_tree::{Dimension, Dimensions, SumTree};

/// A line terminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// `\n` (Unix).
    Lf,
    /// `\r\n` (Windows).
    CrLf,
    /// A lone `\r` (classic Mac OS).
    Cr,
}

impl LineEnding {
    /// The terminator text.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }

    /// The terminator length in bytes (and in UTF-16 code units).
    pub const fn byte_len(self) -> usize {
        match self {
            Self::Lf | Self::Cr => 1,
            Self::CrLf => 2,
        }
    }
}

/// How many lines end with each terminator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LineEndingStats {
    /// Lines ending with `\n`.
    pub lf: usize,
    /// Lines ending with `\r\n`.
    pub crlf: usize,
    /// Lines ending with a lone `\r`.
    pub cr: usize,
}

/// The line-ending style of a text as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineEndingStyle {
    /// The text has no line terminators (at most one line).
    None,
    /// Every terminator is the same.
    Uniform(LineEnding),
    /// More than one kind of terminator occurs. `dominant` is the most
    /// frequent one.
    Mixed {
        /// The most frequent terminator.
        dominant: LineEnding,
    },
}

impl LineEndingStats {
    /// Counts the terminators in `text`.
    pub fn from_text(text: &str) -> Self {
        let mut stats = Self::default();
        let mut scanner = LineScanner::default();
        scanner.feed(text.as_bytes(), |_, kind| stats.add(kind));
        scanner.finish(|_, kind| stats.add(kind));
        stats
    }

    fn add(&mut self, kind: LineEnding) {
        match kind {
            LineEnding::Lf => self.lf += 1,
            LineEnding::CrLf => self.crlf += 1,
            LineEnding::Cr => self.cr += 1,
        }
    }

    /// The total number of terminators.
    pub const fn total(&self) -> usize {
        self.lf + self.crlf + self.cr
    }

    /// The most frequent terminator, or `None` if there is none. Ties are
    /// broken in the order LF, CRLF, CR.
    pub fn dominant(&self) -> Option<LineEnding> {
        let mut best = None;
        let mut best_count = 0;
        for (kind, count) in [
            (LineEnding::Lf, self.lf),
            (LineEnding::CrLf, self.crlf),
            (LineEnding::Cr, self.cr),
        ] {
            if count > best_count {
                best = Some(kind);
                best_count = count;
            }
        }
        best
    }

    /// Whether more than one kind of terminator occurs.
    pub fn is_mixed(&self) -> bool {
        [self.lf, self.crlf, self.cr]
            .iter()
            .filter(|&&n| n > 0)
            .count()
            > 1
    }

    /// The style of the text as a whole.
    pub fn style(&self) -> LineEndingStyle {
        match self.dominant() {
            None => LineEndingStyle::None,
            Some(dominant) if self.is_mixed() => LineEndingStyle::Mixed { dominant },
            Some(kind) => LineEndingStyle::Uniform(kind),
        }
    }
}

/// Streaming line splitter over byte chunks. Reports each terminated line as
/// `(end offset, terminator)`; a CR at the end of a chunk waits for the next
/// chunk to decide between CR and CRLF.
#[derive(Debug, Default)]
pub(crate) struct LineScanner {
    /// Offset of the start of the next chunk.
    pos: usize,
    /// The previous chunk ended with `\r`.
    pending_cr: bool,
}

impl LineScanner {
    pub(crate) fn feed(&mut self, chunk: &[u8], mut on_line: impl FnMut(usize, LineEnding)) {
        let mut i = 0;
        if self.pending_cr && !chunk.is_empty() {
            self.pending_cr = false;
            if chunk[0] == b'\n' {
                on_line(self.pos + 1, LineEnding::CrLf);
                i = 1;
            } else {
                on_line(self.pos, LineEnding::Cr);
            }
        }
        while let Some(found) = memchr::memchr2(b'\n', b'\r', &chunk[i..]) {
            let at = i + found;
            if chunk[at] == b'\n' {
                on_line(self.pos + at + 1, LineEnding::Lf);
                i = at + 1;
            } else if at + 1 == chunk.len() {
                self.pending_cr = true;
                i = chunk.len();
            } else if chunk[at + 1] == b'\n' {
                on_line(self.pos + at + 2, LineEnding::CrLf);
                i = at + 2;
            } else {
                on_line(self.pos + at + 1, LineEnding::Cr);
                i = at + 1;
            }
        }
        self.pos += chunk.len();
    }

    /// Flushes a trailing CR. Returns the total length.
    pub(crate) fn finish(self, mut on_line: impl FnMut(usize, LineEnding)) -> usize {
        if self.pending_cr {
            on_line(self.pos, LineEnding::Cr);
        }
        self.pos
    }
}

/// One line in the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineItem {
    /// Length in bytes, including the terminator.
    pub(crate) len: usize,
    /// `None` only for the last line.
    pub(crate) terminator: Option<LineEnding>,
}

impl LineItem {
    /// Length of the content, without the terminator.
    pub(crate) fn content_len(&self) -> usize {
        self.len - self.terminator.map_or(0, LineEnding::byte_len)
    }

    /// The largest valid column: the content length, plus one for the
    /// position between the CR and LF of a CRLF.
    pub(crate) fn max_column(&self) -> usize {
        self.content_len() + usize::from(self.terminator == Some(LineEnding::CrLf))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LineSummary {
    pub(crate) lines: usize,
    pub(crate) bytes: usize,
    pub(crate) stats: LineEndingStats,
}

impl sum_tree::ContextLessSummary for LineSummary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, other: &Self) {
        self.lines += other.lines;
        self.bytes += other.bytes;
        self.stats.lf += other.stats.lf;
        self.stats.crlf += other.stats.crlf;
        self.stats.cr += other.stats.cr;
    }
}

impl sum_tree::Item for LineItem {
    type Summary = LineSummary;

    fn summary(&self, _cx: ()) -> LineSummary {
        let mut stats = LineEndingStats::default();
        if let Some(kind) = self.terminator {
            stats.add(kind);
        }
        LineSummary {
            lines: 1,
            bytes: self.len,
            stats,
        }
    }
}

/// Byte dimension of the line tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct ByteDim(usize);

/// Line-count dimension of the line tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct LineDim(usize);

impl<'a> Dimension<'a, LineSummary> for ByteDim {
    fn zero(_cx: ()) -> Self {
        Self(0)
    }

    fn add_summary(&mut self, summary: &'a LineSummary, _cx: ()) {
        self.0 += summary.bytes;
    }
}

impl<'a> Dimension<'a, LineSummary> for LineDim {
    fn zero(_cx: ()) -> Self {
        Self(0)
    }

    fn add_summary(&mut self, summary: &'a LineSummary, _cx: ()) {
        self.0 += summary.lines;
    }
}

/// A line found in the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LineRef {
    pub(crate) index: usize,
    pub(crate) start: usize,
    pub(crate) item: LineItem,
}

impl LineRef {
    pub(crate) fn end(&self) -> usize {
        self.start + self.item.len
    }

    pub(crate) fn content_end(&self) -> usize {
        self.start + self.item.content_len()
    }
}

/// One replacement of a transaction, in old and new coordinates.
#[derive(Debug)]
pub(crate) struct AppliedEdit<'a> {
    pub(crate) old: Range<usize>,
    pub(crate) new: Range<usize>,
    pub(crate) text: &'a str,
}

/// Line starts in a sum tree. Always holds at least one line.
#[derive(Clone)]
pub(crate) struct LineIndex {
    tree: SumTree<LineItem>,
}

impl std::fmt::Debug for LineIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineIndex")
            .field("summary", self.tree.summary())
            .finish()
    }
}

impl LineIndex {
    pub(crate) fn from_str(text: &str) -> Self {
        let mut items = Vec::new();
        let mut start = 0;
        let mut scanner = LineScanner::default();
        let mut push = |end: usize, kind: LineEnding| {
            items.push(LineItem {
                len: end - start,
                terminator: Some(kind),
            });
            start = end;
        };
        scanner.feed(text.as_bytes(), &mut push);
        let len = scanner.finish(&mut push);
        items.push(LineItem {
            len: len - start,
            terminator: None,
        });
        Self {
            tree: SumTree::from_iter(items, ()),
        }
    }

    pub(crate) fn summary(&self) -> &LineSummary {
        self.tree.summary()
    }

    pub(crate) fn line_count(&self) -> usize {
        self.tree.summary().lines
    }

    fn last_line(&self) -> LineRef {
        let summary = self.tree.summary();
        let item = self.tree.last().copied().unwrap_or(LineItem {
            len: 0,
            terminator: None,
        });
        LineRef {
            index: summary.lines.saturating_sub(1),
            start: summary.bytes - item.len,
            item,
        }
    }

    /// The line containing byte `offset`. An offset at the end of the text
    /// (or past it) is on the last line.
    pub(crate) fn line_at(&self, offset: usize) -> LineRef {
        let (start, _, item) = self.tree.find::<Dimensions<ByteDim, LineDim>, _>(
            (),
            &ByteDim(offset),
            sum_tree::Bias::Right,
        );
        match item {
            Some(item) => LineRef {
                index: start.1.0,
                start: start.0.0,
                item: *item,
            },
            None => self.last_line(),
        }
    }

    /// Line number `index`, if it exists.
    pub(crate) fn line(&self, index: usize) -> Option<LineRef> {
        if index >= self.line_count() {
            return None;
        }
        let (start, _, item) = self.tree.find::<Dimensions<LineDim, ByteDim>, _>(
            (),
            &LineDim(index),
            sum_tree::Bias::Right,
        );
        item.map(|item| LineRef {
            index,
            start: start.1.0,
            item: *item,
        })
    }

    /// Iterates lines from line `first` on.
    pub(crate) fn iter_from(&self, first: usize) -> LineIter<'_> {
        let mut cursor = self.tree.cursor::<Dimensions<LineDim, ByteDim>>(());
        cursor.seek(&LineDim(first), sum_tree::Bias::Right);
        LineIter { cursor }
    }

    /// The index after a transaction. `edits` are sorted, non-overlapping
    /// and in old coordinates; `new_rope` is the text after all of them.
    ///
    /// Only the bytes around each edit are examined, so the cost is
    /// O(k log n + inserted bytes), independent of line lengths: a line
    /// start depends only on the byte before it and the byte at it.
    pub(crate) fn apply(&self, edits: &[AppliedEdit<'_>], new_rope: &Rope) -> Self {
        if edits.is_empty() {
            return self.clone();
        }
        let last_index = self.line_count() - 1;
        let new_len = new_rope.len();
        let mut cursor = self.tree.cursor::<LineDim>(());
        cursor.seek(&LineDim(0), sum_tree::Bias::Right);
        let mut tree = SumTree::new(());
        let mut items = Vec::new();
        let mut i = 0;
        while i < edits.len() {
            // A group of edits that touch the same or overlapping lines. The
            // lines from the one holding the byte before the first edit to the
            // one holding the end of the last edit are rebuilt.
            let first = &edits[i];
            let first_line = self.line_before(first.old.start);
            let mut last_line = self.line_at(first.old.end);
            let mut j = i;
            while let Some(next) = edits.get(j + 1) {
                if self.line_before(next.old.start).index > last_line.index {
                    break;
                }
                j += 1;
                let line = self.line_at(next.old.end);
                if line.index > last_line.index {
                    last_line = line;
                }
            }
            let last = &edits[j];
            let region_start = first.new.start - (first.old.start - first_line.start);
            let region_end = last.new.end + (last_line.end() - last.old.end);

            items.clear();
            let mut segment_start = region_start;
            let mut last_window = None;
            for edit in &edits[i..=j] {
                let window = Window::new(edit, new_rope);
                let lo = edit.new.start;
                let candidates = (lo >= 1).then_some(lo).into_iter().chain(
                    memchr::memchr2_iter(b'\n', b'\r', edit.text.as_bytes()).map(|k| lo + k + 1),
                );
                for start in candidates {
                    if start <= segment_start {
                        continue;
                    }
                    if let Some(kind) = window.start_kind(start) {
                        items.push(LineItem {
                            len: start - segment_start,
                            terminator: Some(kind),
                        });
                        segment_start = start;
                    }
                }
                last_window = Some(window);
            }
            let terminator = if last_line.index == last_index {
                None
            } else if last_line.end() - last.old.end >= 2 {
                last_line.item.terminator
            } else {
                last_window.and_then(|w| w.known_start_kind(region_end))
            };
            debug_assert!(terminator.is_some() || region_end == new_len);
            items.push(LineItem {
                len: region_end - segment_start,
                terminator,
            });

            tree.append(
                cursor.slice(&LineDim(first_line.index), sum_tree::Bias::Right),
                (),
            );
            cursor.seek_forward(&LineDim(last_line.index + 1), sum_tree::Bias::Right);
            tree.extend(items.drain(..), ());
            i = j + 1;
        }
        tree.append(cursor.suffix(), ());
        Self { tree }
    }

    /// The line holding the byte before `offset` (line 0 for offset 0).
    fn line_before(&self, offset: usize) -> LineRef {
        if offset == 0 {
            LineRef {
                index: 0,
                start: 0,
                item: self.tree.first().copied().unwrap_or(LineItem {
                    len: 0,
                    terminator: None,
                }),
            }
        } else {
            self.line_at(offset - 1)
        }
    }
}

/// The bytes around one edit in the new text: up to two bytes before it, the
/// inserted text, and the byte after it.
struct Window<'a> {
    lo: usize,
    hi: usize,
    before: [Option<u8>; 2],
    text: &'a [u8],
    after: Option<u8>,
}

impl<'a> Window<'a> {
    fn new(edit: &AppliedEdit<'a>, rope: &Rope) -> Self {
        let lo = edit.new.start;
        let hi = edit.new.end;
        let mut before = [None, None];
        let mut filled = 0;
        // `lo` is a char boundary of the new text, so reversed chunks work.
        'outer: for chunk in rope.reversed_chunks_in_range(0..lo) {
            for &byte in chunk.as_bytes().iter().rev() {
                before[filled] = Some(byte);
                filled += 1;
                if filled == 2 {
                    break 'outer;
                }
            }
        }
        let after = if hi < rope.len() {
            rope.chunks_in_range(hi..rope.len())
                .next()
                .and_then(|chunk| chunk.as_bytes().first().copied())
        } else {
            None
        };
        Self {
            lo,
            hi,
            before,
            text: edit.text.as_bytes(),
            after,
        }
    }

    fn byte(&self, pos: usize) -> Option<u8> {
        if pos < self.lo {
            let back = self.lo - pos;
            self.before.get(back - 1).copied().flatten()
        } else if pos < self.hi {
            self.text.get(pos - self.lo).copied()
        } else if pos == self.hi {
            self.after
        } else {
            None
        }
    }

    /// Whether a line starts at `pos` (with `lo <= pos <= hi`), and if so the
    /// terminator of the line that ends there.
    fn start_kind(&self, pos: usize) -> Option<LineEnding> {
        match self.byte(pos - 1)? {
            b'\n' => Some(self.lf_kind(pos)),
            b'\r' if self.byte(pos) != Some(b'\n') => Some(LineEnding::Cr),
            _ => None,
        }
    }

    /// The terminator before `pos`, which is known to be a line start.
    fn known_start_kind(&self, pos: usize) -> Option<LineEnding> {
        match self.byte(pos - 1)? {
            b'\n' => Some(self.lf_kind(pos)),
            b'\r' => Some(LineEnding::Cr),
            _ => None,
        }
    }

    fn lf_kind(&self, pos: usize) -> LineEnding {
        if pos >= 2 && self.byte(pos - 2) == Some(b'\r') {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }
}

/// Iterator over lines, yielding `(line number, start, item)`.
pub(crate) struct LineIter<'a> {
    cursor: sum_tree::Cursor<'a, 'static, LineItem, Dimensions<LineDim, ByteDim>>,
}

impl Iterator for LineIter<'_> {
    type Item = LineRef;

    fn next(&mut self) -> Option<LineRef> {
        let item = *self.cursor.item()?;
        let start = self.cursor.start();
        let line = LineRef {
            index: start.0.0,
            start: start.1.0,
            item,
        };
        self.cursor.next();
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<(usize, Option<LineEnding>)> {
        LineIndex::from_str(text)
            .iter_from(0)
            .map(|l| (l.item.len, l.item.terminator))
            .collect()
    }

    #[test]
    fn scanner_splits_on_lf_crlf_and_cr() {
        use LineEnding::*;
        assert_eq!(lines(""), vec![(0, None)]);
        assert_eq!(lines("a"), vec![(1, None)]);
        assert_eq!(lines("a\n"), vec![(2, Some(Lf)), (0, None)]);
        assert_eq!(lines("a\r\nb"), vec![(3, Some(CrLf)), (1, None)]);
        assert_eq!(lines("a\rb"), vec![(2, Some(Cr)), (1, None)]);
        assert_eq!(lines("\r"), vec![(1, Some(Cr)), (0, None)]);
        assert_eq!(lines("\n\r"), vec![(1, Some(Lf)), (1, Some(Cr)), (0, None)]);
        assert_eq!(
            lines("\r\r\n\n"),
            vec![(1, Some(Cr)), (2, Some(CrLf)), (1, Some(Lf)), (0, None)]
        );
    }

    #[test]
    fn scanner_handles_cr_at_chunk_end() {
        let text = b"ab\r\ncd\r\ref\r";
        for split in 0..=text.len() {
            let mut ends = Vec::new();
            let mut scanner = LineScanner::default();
            scanner.feed(&text[..split], |e, k| ends.push((e, k)));
            scanner.feed(&text[split..], |e, k| ends.push((e, k)));
            scanner.finish(|e, k| ends.push((e, k)));
            assert_eq!(
                ends,
                vec![
                    (4, LineEnding::CrLf),
                    (7, LineEnding::Cr),
                    (8, LineEnding::Cr),
                    (11, LineEnding::Cr)
                ],
                "split at {split}"
            );
        }
    }

    #[test]
    fn stats_and_style() {
        let stats = LineEndingStats::from_text("a\r\nb\r\nc\nd\re");
        assert_eq!(
            stats,
            LineEndingStats {
                lf: 1,
                crlf: 2,
                cr: 1
            }
        );
        assert_eq!(
            stats.style(),
            LineEndingStyle::Mixed {
                dominant: LineEnding::CrLf
            }
        );
        assert_eq!(
            LineEndingStats::from_text("x").style(),
            LineEndingStyle::None
        );
        assert_eq!(
            LineEndingStats::from_text("x\ry\r").style(),
            LineEndingStyle::Uniform(LineEnding::Cr)
        );
        // Ties go to LF, then CRLF.
        assert_eq!(
            LineEndingStats::from_text("a\r\nb\n").dominant(),
            Some(LineEnding::Lf)
        );
    }

    #[test]
    fn lookups() {
        let index = LineIndex::from_str("ab\r\ncd\ref\n");
        assert_eq!(index.line_count(), 4);
        let starts: Vec<_> = index.iter_from(0).map(|l| l.start).collect();
        assert_eq!(starts, vec![0, 4, 7, 10]);
        for (offset, line) in [
            (0, 0),
            (3, 0),
            (4, 1),
            (6, 1),
            (7, 2),
            (9, 2),
            (10, 3),
            (99, 3),
        ] {
            assert_eq!(index.line_at(offset).index, line, "offset {offset}");
        }
        assert_eq!(index.line(2).map(|l| l.start), Some(7));
        assert_eq!(index.line(4), None);
        assert_eq!(index.iter_from(2).count(), 2);
    }
}

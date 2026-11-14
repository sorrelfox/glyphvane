// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The replace layer: the first layer above the buffer.
//!
//! It will hold folds and, for Live Preview, hidden markers and inline
//! widgets. Until then it is an **identity** layer with the interface the
//! full layer will have, so the tab layer above it does not change when
//! replacements arrive.
//!
//! What the identity layer already does is give the layers above a uniform
//! row model. Its text is the buffer text with **every line terminator (LF,
//! CRLF or a lone CR) turned into one `\n`**, so a trailing `\r` belongs to
//! the line ending and never takes up a column. A [`ReplacePoint`] is a
//! buffer line and byte column, except that the position between the CR and
//! the LF of a CRLF does not exist here. There is no offset coordinate:
//! offsets in this text differ from buffer offsets wherever CRLF occurs.

use std::iter::Peekable;
use std::ops::Range;

use gv_text::{Bias, EditSummary, TextSnapshot};

use super::BufferPoint;
use super::primitives::{Chunk, Edit, MAX_CHUNK_LEN, Point, TextSummary};

/// A position in the replace layer's text: a row and a byte column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReplacePoint(pub Point);

impl ReplacePoint {
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

/// An edit of the replace layer's text.
pub type ReplaceEdit = Edit<ReplacePoint>;

/// Keeps the replace layer in step with the buffer.
#[derive(Debug)]
pub struct ReplaceMap {
    snapshot: ReplaceSnapshot,
}

impl ReplaceMap {
    /// A replace layer over `buffer`, and its first snapshot.
    pub fn new(buffer: TextSnapshot) -> (Self, ReplaceSnapshot) {
        let snapshot = ReplaceSnapshot { buffer, version: 0 };
        (
            Self {
                snapshot: snapshot.clone(),
            },
            snapshot,
        )
    }

    /// The current snapshot.
    pub fn snapshot(&self) -> ReplaceSnapshot {
        self.snapshot.clone()
    }

    /// Moves the layer to `buffer`, which `edits` produced from the buffer
    /// of the current snapshot, and returns the new snapshot with the edits
    /// in replace coordinates (sorted, non-overlapping, old ranges in the
    /// previous snapshot and new ranges in the returned one).
    ///
    /// Edits are widened by a byte where a CR before them or an LF after
    /// them may join or split a CRLF, so no edit ends inside a terminator.
    /// If the revisions of `edits` do not match the two buffers, the whole
    /// text is reported as changed.
    pub fn sync(
        &mut self,
        buffer: TextSnapshot,
        edits: &EditSummary,
    ) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
        let old = &self.snapshot;
        let lined_up = edits.old_revision() == old.buffer.revision()
            && edits.new_revision() == buffer.revision();
        if lined_up && edits.is_empty() {
            self.snapshot.buffer = buffer;
            return (self.snapshot.clone(), Vec::new());
        }

        let new = ReplaceSnapshot {
            buffer,
            version: old.version + 1,
        };
        let replace_edits = lined_up
            .then(|| replace_edits(&old.buffer, &new.buffer, edits.edits()))
            .flatten()
            .unwrap_or_else(|| {
                vec![Edit {
                    old: ReplacePoint::zero()..old.max_point(),
                    new: ReplacePoint::zero()..new.max_point(),
                }]
            });
        self.snapshot = new;
        (self.snapshot.clone(), replace_edits)
    }
}

/// Converts buffer edits to replace edits. `None` if the edits do not fit
/// the snapshots.
fn replace_edits(
    old: &TextSnapshot,
    new: &TextSnapshot,
    edits: &[gv_text::Edit],
) -> Option<Vec<ReplaceEdit>> {
    // Join edits that touch, so the byte before and the byte after each one
    // is unchanged text, the same in both snapshots.
    let mut joined = Vec::with_capacity(edits.len());
    for edit in edits {
        push_joined(&mut joined, edit.old.clone(), edit.new.clone());
    }

    // An edit end lies inside a CRLF when a CR just before it and an LF
    // just after it pair up, in the old text or in the new one. Taking that
    // CR or LF (unchanged text) into the edit moves the end outside the
    // terminator. For an empty side, the "first" and "last" characters are
    // the unchanged ones after and before it, which covers an insertion
    // between CR and LF and a deletion that brings them together.
    let first_is = |snapshot: &TextSnapshot, range: &Range<usize>, c: char| {
        Some(snapshot.chars_at(range.start).ok()?.next() == Some(c))
    };
    let last_is = |snapshot: &TextSnapshot, range: &Range<usize>, c: char| {
        Some(snapshot.reversed_chars_at(range.end).ok()?.next() == Some(c))
    };
    let mut widened = Vec::with_capacity(joined.len());
    for (mut old_range, mut new_range) in joined {
        let cr_before = old.reversed_chars_at(old_range.start).ok()?.next() == Some('\r');
        if cr_before && (first_is(old, &old_range, '\n')? || first_is(new, &new_range, '\n')?) {
            old_range.start -= 1;
            new_range.start -= 1;
        }
        let lf_after = old.chars_at(old_range.end).ok()?.next() == Some('\n');
        if lf_after && (last_is(old, &old_range, '\r')? || last_is(new, &new_range, '\r')?) {
            old_range.end += 1;
            new_range.end += 1;
        }
        push_joined(&mut widened, old_range, new_range);
    }

    widened
        .into_iter()
        .map(|(old_range, new_range)| {
            Some(Edit {
                old: point_at(old, old_range.start)?..point_at(old, old_range.end)?,
                new: point_at(new, new_range.start)?..point_at(new, new_range.end)?,
            })
        })
        .collect()
}

/// Appends an edit, joining it with the previous one if they touch.
fn push_joined(
    edits: &mut Vec<(Range<usize>, Range<usize>)>,
    old: Range<usize>,
    new: Range<usize>,
) {
    match edits.last_mut() {
        Some((last_old, last_new)) if last_old.end >= old.start => {
            last_old.end = old.end;
            last_new.end = new.end;
        }
        _ => edits.push((old, new)),
    }
}

/// The replace point of a buffer offset that is not inside a CRLF.
fn point_at(buffer: &TextSnapshot, offset: usize) -> Option<ReplacePoint> {
    debug_assert!(
        !buffer.is_inside_crlf(offset),
        "offset {offset} splits a CRLF"
    );
    let point = buffer.offset_to_point(offset).ok()?;
    Some(ReplacePoint::new(to_u32(point.line), to_u32(point.column)))
}

/// Saturates: the display map handles texts up to 4 GiB.
fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// An immutable view of the replace layer. Cloning is O(1).
#[derive(Clone, Debug)]
pub struct ReplaceSnapshot {
    buffer: TextSnapshot,
    version: usize,
}

impl ReplaceSnapshot {
    /// The buffer snapshot this layer shows.
    pub fn buffer(&self) -> &TextSnapshot {
        &self.buffer
    }

    /// Increases by one with every [`ReplaceMap::sync`] that changes
    /// something.
    pub fn version(&self) -> usize {
        self.version
    }

    /// The end of the text.
    pub fn max_point(&self) -> ReplacePoint {
        let point = self.buffer.max_point();
        ReplacePoint::new(to_u32(point.line), to_u32(point.column))
    }

    /// Length of `row` in bytes, without its terminator; 0 past the last
    /// row.
    pub fn line_len(&self, row: u32) -> u32 {
        to_u32(self.content_len(row as usize))
    }

    fn content_len(&self, line: usize) -> usize {
        self.buffer.line_range(line).map_or(0, |range| range.len())
    }

    /// Clips a point to a valid one: a row past the end goes to the end of
    /// the text, a column past the end of its row to the end of the row, and
    /// a column inside a character moves by `bias`.
    pub fn clip_point(&self, point: ReplacePoint, bias: Bias) -> ReplacePoint {
        let clipped = self.buffer.clip_point(
            BufferPoint::new(point.row() as usize, point.column() as usize),
            bias,
        );
        // The position between CR and LF is valid in the buffer only.
        let column = clipped.column.min(self.content_len(clipped.line));
        ReplacePoint::new(to_u32(clipped.line), to_u32(column))
    }

    /// The replace point of a buffer point, clipped first. The position
    /// between the CR and the LF of a CRLF goes to the end of its line
    /// ([`Bias::Left`]) or the start of the next ([`Bias::Right`]).
    pub fn to_replace_point(&self, point: BufferPoint, bias: Bias) -> ReplacePoint {
        let clipped = self.buffer.clip_point(point, bias);
        let content = self.content_len(clipped.line);
        if clipped.column <= content {
            ReplacePoint::new(to_u32(clipped.line), to_u32(clipped.column))
        } else {
            match bias {
                Bias::Left => ReplacePoint::new(to_u32(clipped.line), to_u32(content)),
                Bias::Right => ReplacePoint::new(to_u32(clipped.line + 1), 0),
            }
        }
    }

    /// The buffer point of a replace point, clipped first.
    pub fn to_buffer_point(&self, point: ReplacePoint, bias: Bias) -> BufferPoint {
        let clipped = self.clip_point(point, bias);
        BufferPoint::new(clipped.row() as usize, clipped.column() as usize)
    }

    /// The buffer offset of a replace point, clipped first.
    fn buffer_offset(&self, point: ReplacePoint, bias: Bias) -> usize {
        self.buffer
            .point_to_offset(self.to_buffer_point(point, bias))
            .unwrap_or(self.buffer.len())
    }

    /// The text in `range`, in chunks. The ends are clipped, the start with
    /// [`Bias::Left`] and the end with [`Bias::Right`].
    pub fn chunks(&self, range: Range<ReplacePoint>) -> ReplaceChunks<'_> {
        let start = self.buffer_offset(range.start, Bias::Left);
        let end = self.buffer_offset(range.end, Bias::Right).max(start);
        ReplaceChunks {
            snapshot: self,
            buffer_chunks: self.buffer.chunks(start..end).ok().map(Iterator::peekable),
            pending: "",
        }
    }

    /// The text from `start` to the end, in chunks.
    pub fn chunks_at(&self, start: ReplacePoint) -> ReplaceChunks<'_> {
        self.chunks(start..self.max_point())
    }

    /// The characters from `start` to the end.
    pub fn chars_at(&self, start: ReplacePoint) -> impl Iterator<Item = char> + '_ {
        self.chunks_at(start).flat_map(|chunk| chunk.text.chars())
    }

    /// The whole text.
    pub fn text(&self) -> String {
        self.chunks_at(ReplacePoint::zero())
            .map(|chunk| chunk.text)
            .collect()
    }

    /// The summary of the text in `range`.
    ///
    /// The identity layer scans the range, so this is O(length of the
    /// range); a layer with a transform tree answers in O(log n).
    pub fn text_summary_for_range(&self, range: Range<ReplacePoint>) -> TextSummary {
        let mut summary = TextSummary::default();
        for chunk in self.chunks(range) {
            summary += &TextSummary::from(chunk.text);
        }
        summary
    }

    /// The summary of the whole text. O(n) for now, see
    /// [`text_summary_for_range`](Self::text_summary_for_range).
    pub fn text_summary(&self) -> TextSummary {
        self.text_summary_for_range(ReplacePoint::zero()..self.max_point())
    }
}

/// The replace layer's text in chunks of at most
/// [`MAX_CHUNK_LEN`] bytes. Never yields an empty
/// chunk.
#[derive(Debug)]
pub struct ReplaceChunks<'a> {
    snapshot: &'a ReplaceSnapshot,
    buffer_chunks: Option<Peekable<gv_text::Chunks<'a>>>,
    /// The rest of the current buffer chunk.
    pending: &'a str,
}

impl<'a> ReplaceChunks<'a> {
    /// Restarts the iterator on another range of the same snapshot.
    pub fn seek(&mut self, range: Range<ReplacePoint>) {
        *self = self.snapshot.chunks(range);
    }

    /// Whether the next buffer chunk starts with an LF.
    fn next_starts_with_lf(&mut self) -> bool {
        let Some(chunks) = self.buffer_chunks.as_mut() else {
            return false;
        };
        while let Some(next) = chunks.peek() {
            if !next.is_empty() {
                return next.starts_with('\n');
            }
            chunks.next();
        }
        false
    }
}

impl<'a> Iterator for ReplaceChunks<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Chunk<'a>> {
        loop {
            let text = self.pending;
            if text.is_empty() {
                self.pending = self.buffer_chunks.as_mut()?.next()?;
                continue;
            }
            let split = match text.find('\r') {
                Some(0) => {
                    self.pending = &text[1..];
                    let crlf = match text.as_bytes().get(1) {
                        Some(&byte) => byte == b'\n',
                        None => self.next_starts_with_lf(),
                    };
                    if crlf {
                        // The LF that follows ends the row.
                        continue;
                    }
                    return Some(Chunk::new("\n"));
                }
                Some(cr) => cr,
                None => text.len(),
            };
            let mut len = split.min(MAX_CHUNK_LEN);
            while !text.is_char_boundary(len) {
                len -= 1;
            }
            let (head, tail) = text.split_at(len);
            self.pending = tail;
            return Some(Chunk::new(head));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_map::test_support::{TestText, normalize, random_text, replay_edits};
    use gv_text::Buffer;
    use rand::{Rng, SeedableRng, rngs::StdRng};

    fn snapshot(text: &str) -> ReplaceSnapshot {
        ReplaceMap::new(TextSnapshot::new(text)).1
    }

    #[test]
    fn terminators_become_newlines() {
        let snapshot = snapshot("a\r\nbc\rd\n\r\n\r");
        assert_eq!(snapshot.text(), "a\nbc\nd\n\n\n");
        assert_eq!(snapshot.max_point(), ReplacePoint::new(5, 0));
        assert_eq!(snapshot.line_len(0), 1);
        assert_eq!(snapshot.line_len(1), 2);
        assert_eq!(snapshot.line_len(3), 0);
        assert_eq!(snapshot.line_len(9), 0);
    }

    #[test]
    fn crlf_across_buffer_chunks() {
        // Long enough that some CRLFs straddle the rope's chunk boundaries.
        for prefix in 0..8 {
            let text = format!("{}{}", "x".repeat(prefix), "ab\r\n\r\tc\r".repeat(100));
            let snapshot = snapshot(&text);
            assert_eq!(snapshot.text(), normalize(&text));
            for chunk in snapshot.chunks_at(ReplacePoint::zero()) {
                assert!(!chunk.text.is_empty());
                assert!(chunk.text.len() <= MAX_CHUNK_LEN);
                assert_eq!(chunk, Chunk::new(chunk.text));
            }
        }
    }

    #[test]
    fn chunks_of_ranges() {
        let text = "αβ\r\n\tγ\rδ";
        let snapshot = snapshot(text);
        let expected = TestText::new(&normalize(text));
        let max = snapshot.max_point();
        for start_row in 0..=max.row() {
            for start_column in 0..=snapshot.line_len(start_row) {
                let start =
                    snapshot.clip_point(ReplacePoint::new(start_row, start_column), Bias::Left);
                let actual: String = snapshot
                    .chunks(start..max)
                    .map(|chunk| chunk.text)
                    .collect();
                let offset = expected.point_to_offset(start.0);
                assert_eq!(actual, &expected.text()[offset..], "from {start:?}");
            }
        }
    }

    #[test]
    fn clipping_and_buffer_points() {
        let snapshot = snapshot("aé\r\nb");
        // Inside `é` (bytes 1..3).
        assert_eq!(
            snapshot.clip_point(ReplacePoint::new(0, 2), Bias::Left),
            ReplacePoint::new(0, 1)
        );
        assert_eq!(
            snapshot.clip_point(ReplacePoint::new(0, 2), Bias::Right),
            ReplacePoint::new(0, 3)
        );
        // Past the end of the row (the CRLF interior is not a column here).
        assert_eq!(
            snapshot.clip_point(ReplacePoint::new(0, 4), Bias::Right),
            ReplacePoint::new(0, 3)
        );
        assert_eq!(
            snapshot.clip_point(ReplacePoint::new(7, 0), Bias::Left),
            ReplacePoint::new(1, 1)
        );
        // Between CR and LF in the buffer.
        assert_eq!(
            snapshot.to_replace_point(BufferPoint::new(0, 4), Bias::Left),
            ReplacePoint::new(0, 3)
        );
        assert_eq!(
            snapshot.to_replace_point(BufferPoint::new(0, 4), Bias::Right),
            ReplacePoint::new(1, 0)
        );
        assert_eq!(
            snapshot.to_buffer_point(ReplacePoint::new(1, 1), Bias::Left),
            BufferPoint::new(1, 1)
        );
    }

    /// A text and the replacements applied to it.
    type Case = (&'static str, &'static [(Range<usize>, &'static str)]);

    /// Edits around CR and LF, where a CRLF is created or split.
    #[test]
    fn edits_that_join_or_split_crlf() {
        let cases: &[Case] = &[
            ("a\r\nb", &[(2..2, "x")]),
            ("a\r\nb", &[(2..3, "")]),
            ("a\r\nb", &[(1..2, "")]),
            ("a\rb", &[(2..2, "\n")]),
            ("a\nb", &[(1..1, "\r")]),
            ("a\r", &[(2..2, "\n")]),
            ("a\rX\nb", &[(2..3, "")]),
            ("\r\n\r\n", &[(1..1, "x"), (2..3, "\r"), (3..3, "\n")]),
            ("\r\r\n\n", &[(0..1, ""), (1..2, ""), (3..4, "\r")]),
        ];
        for (text, edits) in cases {
            let mut buffer = Buffer::new(text);
            let (mut map, old) = ReplaceMap::new(buffer.snapshot());
            let summary = buffer
                .edit(edits.iter().map(|(range, text)| (range.clone(), *text)))
                .unwrap();
            let (new, replace_edits) = map.sync(buffer.snapshot(), &summary);
            assert_eq!(new.text(), normalize(&buffer.to_string()));
            assert_eq!(
                replay_edits(&old.text(), &new.text(), &replace_edits, |p| p.0),
                new.text(),
                "{text:?} with {edits:?}: {replace_edits:?}"
            );
        }
    }

    /// Typing at the end of a line is not widened to the terminator.
    #[test]
    fn edits_at_line_ends_stay_tight() {
        for text in ["ab\r\ncd", "ab\ncd", "ab\rcd"] {
            let mut buffer = Buffer::new(text);
            let (mut map, _) = ReplaceMap::new(buffer.snapshot());
            let summary = buffer.edit([(2..2, "x")]).unwrap();
            let (_, edits) = map.sync(buffer.snapshot(), &summary);
            assert_eq!(
                edits,
                vec![Edit {
                    old: ReplacePoint::new(0, 2)..ReplacePoint::new(0, 2),
                    new: ReplacePoint::new(0, 2)..ReplacePoint::new(0, 3),
                }],
                "{text:?}"
            );
        }
    }

    #[test]
    fn mismatched_revisions_report_everything() {
        let mut buffer = Buffer::new("a\nb");
        let (mut map, _) = ReplaceMap::new(buffer.snapshot());
        let first = buffer.edit([(0..0, "x")]).unwrap();
        let _second = buffer.edit([(0..0, "y")]).unwrap();
        let (snapshot, edits) = map.sync(buffer.snapshot(), &first);
        assert_eq!(snapshot.text(), "yxa\nb");
        assert_eq!(
            edits,
            vec![Edit {
                old: ReplacePoint::zero()..ReplacePoint::new(1, 1),
                new: ReplacePoint::zero()..ReplacePoint::new(1, 1),
            }]
        );
        assert_eq!(snapshot.version(), 1);
    }

    #[test]
    fn random_edits() {
        for seed in 0..200 {
            let mut rng = StdRng::seed_from_u64(seed);
            let len = rng.random_range(0..60);
            let mut buffer = Buffer::new(&random_text(&mut rng, len));
            let (mut map, mut old) = ReplaceMap::new(buffer.snapshot());
            for _ in 0..5 {
                let summary = crate::display_map::test_support::random_edit(&mut rng, &mut buffer);
                let (new, edits) = map.sync(buffer.snapshot(), &summary);
                assert_eq!(new.text(), normalize(&buffer.to_string()), "seed {seed}");
                assert_eq!(
                    replay_edits(&old.text(), &new.text(), &edits, |p| p.0),
                    new.text(),
                    "seed {seed}: {edits:?}"
                );
                old = new;
            }
        }
    }
}

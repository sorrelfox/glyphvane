// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Helpers for the display-map unit tests: a plain-string model of layer
//! text, random text and edits, and replaying edits.

use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use gv_text::{Bias, Buffer, EditSummary};
use rand::{Rng, SeedableRng, rngs::StdRng};
use util::RandomCharIter;

use super::primitives::{Edit, Point};

/// Runs `test` with a seeded RNG for seeds `0..iterations`, or only for the
/// seed in the `SEED` environment variable. Reports the failing seed.
pub(crate) fn for_each_seed(iterations: u64, mut test: impl FnMut(StdRng)) {
    let seeds = match std::env::var("SEED").ok().and_then(|s| s.parse().ok()) {
        Some(seed) => seed..seed + 1,
        None => 0..iterations,
    };
    for seed in seeds {
        let result = catch_unwind(AssertUnwindSafe(|| test(StdRng::seed_from_u64(seed))));
        if let Err(payload) = result {
            eprintln!("failed with SEED={seed}");
            resume_unwind(payload);
        }
    }
}

/// `len` characters from Zed's `RandomCharIter`: ASCII, whitespace (with
/// `\r` and `\n`), and 2-, 3- and 4-byte UTF-8.
pub(crate) fn random_text(rng: &mut StdRng, len: usize) -> String {
    RandomCharIter::new(rng).take(len).collect()
}

/// The text with every terminator (LF, CRLF, lone CR) as one `\n`.
pub(crate) fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Applies 1 to 5 random replacements to `buffer`, in the manner of Zed's
/// `Buffer::randomly_edit`.
pub(crate) fn random_edit(rng: &mut StdRng, buffer: &mut Buffer) -> EditSummary {
    let edit_count = rng.random_range(1..=5);
    let mut edits = Vec::new();
    let mut last_end: Option<usize> = None;
    for _ in 0..edit_count {
        if last_end.is_some_and(|end| end >= buffer.len()) {
            break;
        }
        let from = last_end.map_or(0, |end| end + 1);
        let end = buffer.clip_offset(rng.random_range(from..=buffer.len()), Bias::Right);
        let start = buffer.clip_offset(rng.random_range(from..=end), Bias::Right);
        last_end = Some(end);
        let new_len = rng.random_range(0..10);
        edits.push((start..end, random_text(rng, new_len)));
    }
    buffer.edit(edits).expect("valid random edit")
}

/// Applies `edits` (in the coordinates of `old_text` and `new_text`) to
/// `old_text`, taking the inserted text from `new_text`. Correct edits give
/// back `new_text`.
pub(crate) fn replay_edits<D: Copy>(
    old_text: &str,
    new_text: &str,
    edits: &[Edit<D>],
    point: impl Fn(D) -> Point,
) -> String {
    let new = TestText::new(new_text);
    let mut text = old_text.to_owned();
    for edit in edits {
        let current = TestText::new(&text);
        let start = point(edit.new.start);
        let old_start = current.point_to_offset(start);
        let old_end =
            current.point_to_offset(start + (point(edit.old.end) - point(edit.old.start)));
        let inserted = new
            .slice(point(edit.new.start)..point(edit.new.end))
            .to_owned();
        text.replace_range(old_start..old_end.max(old_start), &inserted);
    }
    text
}

/// Layer text as a plain string with `\n` rows.
#[derive(Debug, Clone)]
pub(crate) struct TestText {
    text: String,
    /// Byte offset of the start of every row.
    row_starts: Vec<usize>,
}

impl TestText {
    pub(crate) fn new(text: &str) -> Self {
        let mut row_starts = vec![0];
        row_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self {
            text: text.to_owned(),
            row_starts,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    fn row_range(&self, row: usize) -> Range<usize> {
        let start = self.row_starts[row];
        let end = self
            .row_starts
            .get(row + 1)
            .map_or(self.text.len(), |next| next - 1);
        start..end
    }

    pub(crate) fn max_point(&self) -> Point {
        let row = self.row_starts.len() - 1;
        Point::new(row as u32, self.row_range(row).len() as u32)
    }

    pub(crate) fn line_len(&self, row: u32) -> u32 {
        self.row_range(row as usize).len() as u32
    }

    /// Clamps to the text and the row, then out of a character by `bias`.
    pub(crate) fn clip_point(&self, point: Point, bias: Bias) -> Point {
        let max = self.max_point();
        if point.row > max.row {
            return max;
        }
        let range = self.row_range(point.row as usize);
        let mut offset = range.start + (point.column as usize).min(range.len());
        while !self.text.is_char_boundary(offset) {
            match bias {
                Bias::Left => offset -= 1,
                Bias::Right => offset += 1,
            }
        }
        Point::new(point.row, (offset - range.start) as u32)
    }

    /// The offset of a point, clamped to the text and the row and floored
    /// to a character boundary.
    pub(crate) fn point_to_offset(&self, point: Point) -> usize {
        let clipped = self.clip_point(point, Bias::Left);
        if clipped.row < point.row {
            return self.text.len();
        }
        self.row_range(clipped.row as usize).start + clipped.column as usize
    }

    pub(crate) fn slice(&self, range: Range<Point>) -> &str {
        let start = self.point_to_offset(range.start);
        let end = self.point_to_offset(range.end).max(start);
        &self.text[start..end]
    }
}

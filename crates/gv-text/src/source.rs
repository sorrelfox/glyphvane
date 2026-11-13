// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`TextSource`]: the read interface the parser uses.

use std::ops::Range;

use crate::snapshot::TextSnapshot;

/// Read access to a document's bytes, line by line.
///
/// The parser's block pass reads one line at a time, and occasionally copies
/// a byte range (when an incremental reparse restarts inside a paragraph).
/// Contiguous storage returns slices into itself; chunked storage such as
/// [`TextSnapshot`] returns a slice of one chunk when the line fits in it and
/// otherwise assembles the line in the caller's scratch buffer.
///
/// Line terminators are LF, CRLF and a lone CR, as in CommonMark.
pub trait TextSource {
    /// Length in bytes.
    fn len(&self) -> usize;

    /// Whether the text is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The bytes from `start` up to and including the end of the line's
    /// terminator (or the end of the text). `start` is normally a line start;
    /// any other offset returns the rest of its line. Returns an empty slice
    /// if `start >= len()`.
    fn line<'a>(&'a self, start: usize, scratch: &'a mut Vec<u8>) -> &'a [u8];

    /// Appends the bytes in `range`, clamped to the text, to `out`.
    fn copy_range(&self, range: Range<usize>, out: &mut Vec<u8>);
}

/// End of the line starting at `start` in contiguous bytes (exclusive, after
/// the terminator).
fn line_end(bytes: &[u8], start: usize) -> usize {
    match memchr::memchr2(b'\n', b'\r', &bytes[start..]) {
        None => bytes.len(),
        Some(i) => {
            let i = start + i;
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                i + 2
            } else {
                i + 1
            }
        }
    }
}

impl TextSource for str {
    fn len(&self) -> usize {
        str::len(self)
    }

    fn line<'a>(&'a self, start: usize, _scratch: &'a mut Vec<u8>) -> &'a [u8] {
        let bytes = self.as_bytes();
        if start >= bytes.len() {
            return &[];
        }
        &bytes[start..line_end(bytes, start)]
    }

    fn copy_range(&self, range: Range<usize>, out: &mut Vec<u8>) {
        let end = range.end.min(self.len());
        if range.start < end {
            out.extend_from_slice(&self.as_bytes()[range.start..end]);
        }
    }
}

impl TextSource for String {
    fn len(&self) -> usize {
        String::len(self)
    }

    fn line<'a>(&'a self, start: usize, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        self.as_str().line(start, scratch)
    }

    fn copy_range(&self, range: Range<usize>, out: &mut Vec<u8>) {
        self.as_str().copy_range(range, out);
    }
}

impl TextSnapshot {
    /// Calls `f` with the bytes of `range` (clamped to the text), one chunk at
    /// a time. Works for offsets inside characters too.
    fn for_each_byte_chunk(&self, range: Range<usize>, mut f: impl FnMut(&[u8])) {
        let end = range.end.min(self.len());
        if range.start >= end {
            return;
        }
        let first = self.clip_offset(range.start, crate::Bias::Left);
        let last = self.clip_offset(end, crate::Bias::Right);
        let mut pos = first;
        for chunk in self.rope.chunks_in_range(first..last) {
            let bytes = chunk.as_bytes();
            let chunk_start = pos;
            pos += bytes.len();
            let lo = range.start.max(chunk_start) - chunk_start;
            let hi = end.min(pos) - chunk_start;
            if lo < hi {
                f(&bytes[lo..hi]);
            }
        }
    }
}

impl TextSource for TextSnapshot {
    fn len(&self) -> usize {
        TextSnapshot::len(self)
    }

    fn line<'a>(&'a self, start: usize, scratch: &'a mut Vec<u8>) -> &'a [u8] {
        if start >= TextSnapshot::len(self) {
            return &[];
        }
        let end = self.lines.line_at(start).end();
        if self.is_char_boundary(start) {
            let mut chunks = self.rope.chunks_in_range(start..end);
            if let Some(first) = chunks.next()
                && first.len() == end - start
            {
                return first.as_bytes();
            }
        }
        scratch.clear();
        self.for_each_byte_chunk(start..end, |bytes| scratch.extend_from_slice(bytes));
        scratch
    }

    fn copy_range(&self, range: Range<usize>, out: &mut Vec<u8>) {
        self.for_each_byte_chunk(range, |bytes| out.extend_from_slice(bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of<T: TextSource + ?Sized>(text: &T) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut scratch = Vec::new();
        let mut pos = 0;
        while pos < text.len() {
            let line = text.line(pos, &mut scratch).to_vec();
            pos += line.len();
            out.push(line);
        }
        out
    }

    #[test]
    fn snapshot_lines_match_str_lines() {
        let mut text = String::from("a\r\nbb\rccc\n\r\n\rlast");
        assert_eq!(lines_of(&TextSnapshot::new(&text)), lines_of(text.as_str()));
        // Long lines span many 128-byte rope chunks, and a CRLF can straddle
        // a chunk boundary.
        for i in 0..300 {
            text.push_str(&"é".repeat(i % 70));
            text.push_str(["\r\n", "\r", "\n"][i % 3]);
        }
        let snapshot = TextSnapshot::new(&text);
        assert_eq!(lines_of(&snapshot), lines_of(text.as_str()));
        let mut out = Vec::new();
        snapshot.copy_range(3..2000, &mut out);
        assert_eq!(&out[..], &text.as_bytes()[3..2000]);
    }

    #[test]
    fn out_of_range_and_mid_char_requests_do_not_panic() {
        let snapshot = TextSnapshot::new("aé\nb");
        let mut scratch = Vec::new();
        assert_eq!(snapshot.line(99, &mut scratch), b"");
        assert_eq!(snapshot.line(2, &mut scratch), b"\xA9\n");
        let mut out = Vec::new();
        snapshot.copy_range(2..99, &mut out);
        assert_eq!(out, b"\xA9\nb");
        out.clear();
        "aé".copy_range(1..99, &mut out);
        assert_eq!(out, "é".as_bytes());
    }
}

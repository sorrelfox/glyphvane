// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Source ranges of the parts of a link reference definition
//! (`[label]: destination "title"`).
//!
//! The parser decides that a definition exists and decodes it
//! ([`parse_ref_def`]); this module finds where its label, destination and
//! title are in the document, which refactors need to rewrite a
//! destination in place.

use std::ops::Range;

use gv_syntax::inline::{RefDef, parse_ref_def};

/// A definition with its decoded parts and their document ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocatedDef {
    /// The normalized label.
    pub label: String,
    /// The decoded destination and title.
    pub def: RefDef,
    /// The label as written, between the brackets.
    pub label_range: Range<usize>,
    /// The destination as written (with `<` and `>` if present).
    pub destination_range: Range<usize>,
    /// The title as written, with its delimiters.
    pub title_range: Option<Range<usize>>,
}

/// The joined content of a leaf's lines and the map back to the document.
#[derive(Debug, Default)]
pub(crate) struct Joined {
    /// The lines' content joined with `\n` (and a final `\n`).
    pub text: String,
    /// Per line: (offset in `text`, document range of its content).
    starts: Vec<(usize, Range<usize>)>,
}

impl Joined {
    /// Joins `lines` (document ranges of content) read from `bytes`.
    pub(crate) fn new(lines: impl IntoIterator<Item = (Range<usize>, Vec<u8>)>) -> Self {
        let mut j = Joined::default();
        let mut buf = Vec::new();
        for (range, content) in lines {
            j.starts.push((buf.len(), range));
            buf.extend_from_slice(&content);
            buf.push(b'\n');
        }
        j.text = String::from_utf8_lossy(&buf).into_owned();
        j
    }

    /// The document offset of joined offset `o` (a line ending maps to the
    /// end of its line's content).
    pub(crate) fn doc(&self, o: usize) -> usize {
        let k = self.starts.partition_point(|(s, _)| *s <= o);
        match k.checked_sub(1).and_then(|i| self.starts.get(i)) {
            Some((s, r)) => (r.start + (o - s)).min(r.end),
            None => 0,
        }
    }

    /// The document range of the joined range `r`.
    pub(crate) fn doc_range(&self, r: Range<usize>) -> Range<usize> {
        let a = self.doc(r.start);
        let b = if r.end > r.start {
            self.doc(r.end - 1) + 1
        } else {
            a
        };
        a..b.max(a)
    }
}

/// Parses and locates the definition in `joined` (one definition's lines).
pub(crate) fn locate(joined: &Joined) -> Option<LocatedDef> {
    let s = joined.text.as_str();
    let (_, label, def) = parse_ref_def(s)?;
    let b = s.as_bytes();
    // The label: up to the first unescaped `]` (labels cannot contain
    // unescaped brackets).
    let mut i = 1;
    while i < b.len() && b[i] != b']' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    let label_range = 1..i.min(b.len());
    // `]:`, then spaces or tabs and at most one line ending.
    i = skip_space(b, i + 2);
    // The destination.
    let dest_start = i;
    if b.get(i) == Some(&b'<') {
        i += 1;
        while i < b.len() && b[i] != b'>' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        i = (i + 1).min(b.len());
    } else {
        while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
    }
    let destination_range = dest_start..i;
    let title_range = def.title.as_ref().and_then(|_| {
        let start = skip_space(b, i);
        let close = match b.get(start)? {
            b'"' => b'"',
            b'\'' => b'\'',
            b'(' => b')',
            _ => return None,
        };
        let mut j = start + 1;
        while j < b.len() && b[j] != close {
            j += if b[j] == b'\\' { 2 } else { 1 };
        }
        Some(start..(j + 1).min(b.len()))
    });
    Some(LocatedDef {
        label,
        def,
        label_range: joined.doc_range(label_range),
        destination_range: joined.doc_range(destination_range),
        title_range: title_range.map(|r| joined.doc_range(r)),
    })
}

/// Skips spaces and tabs, at most one line ending, then spaces and tabs.
fn skip_space(b: &[u8], mut i: usize) -> usize {
    let blank = |i: usize| matches!(b.get(i), Some(b' ' | b'\t'));
    while blank(i) {
        i += 1;
    }
    if b.get(i) == Some(&b'\n') {
        i += 1;
        while blank(i) {
            i += 1;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(lines: &[(usize, &str)]) -> (LocatedDef, String) {
        let mut doc = String::new();
        let mut parts = Vec::new();
        for &(indent, l) in lines {
            doc.push_str(&" ".repeat(indent));
            let start = doc.len();
            doc.push_str(l);
            parts.push((start..doc.len(), l.as_bytes().to_vec()));
            doc.push('\n');
        }
        (locate(&Joined::new(parts)).expect("a definition"), doc)
    }

    #[test]
    fn ranges() {
        let (d, doc) = loc(&[(0, "[Foo\\]x]: <my url> \"t\\\"\"")]);
        // Labels are normalized, not unescaped.
        assert_eq!(d.label, "foo\\]x");
        assert_eq!(&doc[d.label_range], "Foo\\]x");
        assert_eq!(&doc[d.destination_range], "<my url>");
        assert_eq!(&doc[d.title_range.expect("title")], "\"t\\\"\"");
        assert_eq!(d.def.destination, "my url");

        let (d, doc) = loc(&[(2, "[a]:"), (4, "/url"), (2, "'multi"), (2, "line'")]);
        assert_eq!(&doc[d.destination_range], "/url");
        assert_eq!(&doc[d.title_range.expect("title")], "'multi\n  line'");

        let (d, doc) = loc(&[(0, "[b]: /u(1)")]);
        assert_eq!(&doc[d.destination_range], "/u(1)");
        assert!(d.title_range.is_none());
    }
}

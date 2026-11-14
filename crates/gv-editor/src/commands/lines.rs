// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Column-exact rewrites of a line's structural prefix.
//!
//! Moving a list item (indent, outdent, a marker that grows from `9.` to
//! `10.`) shifts the columns of everything after some point of a line. Block
//! structure depends only on columns, so the shift is exact as long as no
//! tab after that point changes its width. When the prefix part being moved
//! holds a tab, it is rewritten with the spaces the tab spanned; otherwise
//! the edit is a plain insertion or deletion of spaces.

use std::ops::Range;

use gv_syntax::TokenKind;

use super::context::{col_after, detab, spaces};
use super::doc::{Doc, Line};

/// Where the line's structural prefix ends at or after `from`: the start of
/// its first leaf token (content, a fence or `#` marker, an info string or
/// the line ending), else the end of the line.
pub(crate) fn structural_end(doc: &Doc, line: &Line, from: usize) -> usize {
    doc.tree()
        .tokens(line.start..line.full_end().max(line.start + 1))
        .into_iter()
        .find(|(range, token)| {
            range.start >= from
                && matches!(
                    token.kind,
                    TokenKind::Content
                        | TokenKind::Marker
                        | TokenKind::InfoString
                        | TokenKind::LineEnding
                )
        })
        .map_or(line.end(), |(range, _)| range.start)
        .clamp(from, line.end())
}

/// A replacement trimmed to the bytes that actually change.
pub(crate) fn minimal(old: &str, at: usize, new: String) -> Option<(Range<usize>, String)> {
    if old == new {
        return None;
    }
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let mut prefix = prefix;
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    let mut suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    Some((
        at + prefix..at + old.len() - suffix,
        new[prefix..new.len() - suffix].to_owned(),
    ))
}

/// Shifts everything from byte `at` of `line` by `delta` columns: spaces
/// inserted at `at`, or up to `-delta` columns of whitespace removed there.
/// The prefix part from `at` to the leaf content is rewritten without tabs
/// if it holds one. `None` if nothing changes.
pub(crate) fn shift_edit(
    doc: &Doc,
    line: &Line,
    at: usize,
    delta: isize,
) -> Option<(Range<usize>, String)> {
    if delta == 0 {
        return None;
    }
    let end = structural_end(doc, line, at);
    let b = line.bytes();
    let (ra, re) = (line.rel(at), line.rel(end));
    let region = &b[ra..re];
    let col = col_after(&b[..ra], 0);
    let mut new = if region.contains(&b'\t') {
        detab(region, col)
    } else {
        String::from_utf8_lossy(region).into_owned()
    };
    if delta > 0 {
        new.insert_str(0, &spaces(delta.unsigned_abs()));
    } else {
        let n = new
            .bytes()
            .take(delta.unsigned_abs())
            .take_while(|&c| c == b' ')
            .count();
        new.drain(..n);
    }
    minimal(&line.text[ra..re], at, new)
}

/// Replaces `range` of `line` (inside its structural prefix) with `text`.
/// If that moves the rest of the prefix by some columns and the rest holds
/// a tab, the rest is rewritten without tabs too.
pub(crate) fn replace_in_prefix(
    doc: &Doc,
    line: &Line,
    range: Range<usize>,
    text: &str,
) -> Option<(Range<usize>, String)> {
    let b = line.bytes();
    let (ra, rb) = (line.rel(range.start), line.rel(range.end));
    let col = col_after(&b[..ra], 0);
    let old_end_col = col_after(&b[ra..rb], col);
    let new_end_col = col_after(text.as_bytes(), col);
    let end = structural_end(doc, line, range.end.max(line.start + rb));
    let re = line.rel(end).max(rb);
    let rest = &b[rb..re];
    let mut new = text.to_owned();
    if old_end_col != new_end_col && rest.contains(&b'\t') {
        new.push_str(&detab(rest, old_end_col));
    } else {
        new.push_str(&String::from_utf8_lossy(rest));
    }
    minimal(&line.text[ra..re], line.start + ra, new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_trims_common_ends() {
        assert_eq!(
            minimal("  - a", 10, "    - a".to_owned()),
            Some((12..12, "  ".to_owned()))
        );
        assert_eq!(minimal("ab", 0, "ab".to_owned()), None);
        assert_eq!(
            minimal("9.", 5, "10.".to_owned()),
            Some((5..6, "10".to_owned()))
        );
        assert_eq!(
            minimal("é", 0, "è".to_owned()),
            Some((0..2, "è".to_owned()))
        );
    }
}

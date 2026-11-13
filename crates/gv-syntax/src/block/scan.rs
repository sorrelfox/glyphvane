// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Line-level scanners for block starts and ends.
//!
//! Each scanner receives the line from its first non-space byte to the end
//! of the line (terminator included). They follow the CommonMark block rules
//! as cmark implements them (re2c patterns), including the convention that
//! the end of the input behaves like a sentinel byte that only matches where
//! a pattern allows "end of line or end of input".

/// Byte at `i`, or the end-of-input sentinel `0xFF` (never valid in UTF-8).
#[inline]
fn at(s: &[u8], i: usize) -> u8 {
    if i < s.len() { s[i] } else { 0xFF }
}

/// `\n` or `\r`.
#[inline]
pub(crate) fn is_line_end(c: u8) -> bool {
    c == b'\n' || c == b'\r'
}

/// Space or tab.
#[inline]
pub(crate) fn is_space_or_tab(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// `[\r\n\xff]`: end of line or end of input.
#[inline]
fn is_eol(c: u8) -> bool {
    c == b'\n' || c == b'\r' || c == 0xFF
}

/// re2c `spacechar`: `[ \t\v\f\r\n]`.
#[inline]
fn is_spacechar(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | 0x0B | 0x0C | b'\r' | b'\n')
}

/// cmark's `isspace`: space, tab, LF, CR (and the end-of-input sentinel
/// where the caller says so).
#[inline]
pub(crate) fn is_cmark_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// First bytes that can start any block other than indented code and
/// paragraphs.
#[inline]
pub(crate) fn maybe_special(c: u8) -> bool {
    matches!(
        c,
        b'#' | b'`' | b'~' | b'*' | b'+' | b'_' | b'=' | b'<' | b'>' | b'-' | b'0'..=b'9'
    )
}

/// ATX heading opener `#{1,6}` followed by spaces/tabs or the end of the
/// line. Returns `(level, matched_len)`; `matched_len` covers the following
/// spaces and tabs.
pub(crate) fn atx_heading_start(s: &[u8]) -> Option<(usize, usize)> {
    let mut n = 0;
    while n < 6 && at(s, n) == b'#' {
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let c = at(s, n);
    if is_space_or_tab(c) {
        let mut m = n;
        while is_space_or_tab(at(s, m)) {
            m += 1;
        }
        Some((n, m))
    } else if is_eol(c) {
        Some((n, n))
    } else {
        None
    }
}

/// Opening code fence: 3+ backticks (and no backtick later on the line) or
/// 3+ tildes. Returns the fence length.
pub(crate) fn open_code_fence(s: &[u8]) -> Option<usize> {
    let c = at(s, 0);
    if c != b'`' && c != b'~' {
        return None;
    }
    let mut n = 0;
    while at(s, n) == c {
        n += 1;
    }
    if n < 3 {
        return None;
    }
    if c == b'`' {
        let rest = &s[n..];
        let line = match memchr::memchr2(b'\n', b'\r', rest) {
            Some(i) => &rest[..i],
            None => rest,
        };
        if memchr::memchr(b'`', line).is_some() {
            return None;
        }
    }
    Some(n)
}

/// Closing code fence: 3+ backticks or tildes followed only by spaces and
/// tabs. Returns the fence length.
pub(crate) fn close_code_fence(s: &[u8]) -> Option<usize> {
    let c = at(s, 0);
    if c != b'`' && c != b'~' {
        return None;
    }
    let mut n = 0;
    while at(s, n) == c {
        n += 1;
    }
    if n < 3 {
        return None;
    }
    let mut i = n;
    while is_space_or_tab(at(s, i)) {
        i += 1;
    }
    if is_eol(at(s, i)) { Some(n) } else { None }
}

/// Setext underline: `=+` or `-+` followed only by spaces and tabs.
/// Returns `(level, run_len)`.
pub(crate) fn setext_heading_line(s: &[u8]) -> Option<(u8, usize)> {
    let c = at(s, 0);
    let level = match c {
        b'=' => 1,
        b'-' => 2,
        _ => return None,
    };
    let mut n = 0;
    while at(s, n) == c {
        n += 1;
    }
    let mut i = n;
    while is_space_or_tab(at(s, i)) {
        i += 1;
    }
    if is_eol(at(s, i)) {
        Some((level, n))
    } else {
        None
    }
}

/// Thematic break: 3+ of the same `*`, `-` or `_`, optionally separated by
/// spaces and tabs.
///
/// `Err(p)` reports a failure caused by the byte at `p`: any scan of the same
/// line that starts before `p` fails there too, which lets the caller skip
/// those scans (cmark's `thematic_break_kill_pos`; without it a line of many
/// list markers is quadratic).
pub(crate) fn thematic_break(s: &[u8]) -> Result<(), usize> {
    let b = at(s, 0);
    if b != b'*' && b != b'_' && b != b'-' {
        return Err(0);
    }
    let mut count = 1;
    let mut i = 1;
    loop {
        let c = at(s, i);
        if c == b {
            count += 1;
        } else if !is_space_or_tab(c) {
            break;
        }
        i += 1;
    }
    let c = at(s, i);
    if count >= 3 && is_eol(c) {
        Ok(())
    } else if is_eol(c) {
        // Too few markers: a scan from a later start sees even fewer.
        Err(i)
    } else {
        Err(i)
    }
}

fn starts_with_ci(s: &[u8], at_: usize, word: &[u8]) -> bool {
    s.len() >= at_ + word.len() && s[at_..at_ + word.len()].eq_ignore_ascii_case(word)
}

/// The block-level tag names of HTML block condition 6.
const BLOCK_TAGS: &[&[u8]] = &[
    b"address",
    b"article",
    b"aside",
    b"base",
    b"basefont",
    b"blockquote",
    b"body",
    b"caption",
    b"center",
    b"col",
    b"colgroup",
    b"dd",
    b"details",
    b"dialog",
    b"dir",
    b"div",
    b"dl",
    b"dt",
    b"fieldset",
    b"figcaption",
    b"figure",
    b"footer",
    b"form",
    b"frame",
    b"frameset",
    b"h1",
    b"h2",
    b"h3",
    b"h4",
    b"h5",
    b"h6",
    b"head",
    b"header",
    b"hr",
    b"html",
    b"iframe",
    b"legend",
    b"li",
    b"link",
    b"main",
    b"menu",
    b"menuitem",
    b"nav",
    b"noframes",
    b"ol",
    b"optgroup",
    b"option",
    b"p",
    b"param",
    b"search",
    b"section",
    b"summary",
    b"table",
    b"tbody",
    b"td",
    b"tfoot",
    b"th",
    b"thead",
    b"title",
    b"tr",
    b"track",
    b"ul",
];

/// HTML block start conditions 1 to 6. Returns the block type.
pub(crate) fn html_block_start(s: &[u8]) -> Option<u8> {
    if at(s, 0) != b'<' {
        return None;
    }
    for tag in [&b"script"[..], b"pre", b"textarea", b"style"] {
        if starts_with_ci(s, 1, tag) {
            let c = at(s, 1 + tag.len());
            if is_spacechar(c) || c == b'>' || c == 0xFF {
                return Some(1);
            }
        }
    }
    if s.starts_with(b"<!--") {
        return Some(2);
    }
    if at(s, 1) == b'?' {
        return Some(3);
    }
    if at(s, 1) == b'!' && at(s, 2).is_ascii_alphabetic() {
        return Some(4);
    }
    if s.starts_with(b"<![CDATA[") {
        return Some(5);
    }
    let p = if at(s, 1) == b'/' { 2 } else { 1 };
    for tag in BLOCK_TAGS {
        if starts_with_ci(s, p, tag) {
            let i = p + tag.len();
            let c = at(s, i);
            if is_spacechar(c) || c == b'>' || c == 0xFF || (c == b'/' && at(s, i + 1) == b'>') {
                return Some(6);
            }
        }
    }
    None
}

fn tag_name(s: &[u8], mut i: usize) -> Option<usize> {
    if !at(s, i).is_ascii_alphabetic() {
        return None;
    }
    i += 1;
    while at(s, i).is_ascii_alphanumeric() || at(s, i) == b'-' {
        i += 1;
    }
    Some(i)
}

fn skip_spacechars(s: &[u8], mut i: usize) -> usize {
    while is_spacechar(at(s, i)) {
        i += 1;
    }
    i
}

/// Parses `attribute*` after a tag name; returns the position after them,
/// or `None` if a quoted value is unterminated.
fn attributes(s: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let j = skip_spacechars(s, i);
        if j == i {
            return Some(i);
        }
        let c = at(s, j);
        if !(c.is_ascii_alphabetic() || c == b'_' || c == b':') {
            return Some(i);
        }
        let mut k = j + 1;
        while {
            let c = at(s, k);
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-')
        } {
            k += 1;
        }
        // Optional value specification.
        let v = skip_spacechars(s, k);
        if at(s, v) == b'=' {
            let v = skip_spacechars(s, v + 1);
            let q = at(s, v);
            let end = if q == b'"' || q == b'\'' {
                let mut e = v + 1;
                loop {
                    let c = at(s, e);
                    if c == 0xFF {
                        return None;
                    }
                    e += 1;
                    if c == q {
                        break;
                    }
                }
                e
            } else {
                let mut e = v;
                while {
                    let c = at(s, e);
                    c != 0xFF
                        && !is_spacechar(c)
                        && !matches!(c, b'"' | b'\'' | b'=' | b'<' | b'>' | b'`')
                } {
                    e += 1;
                }
                if e == v {
                    return None;
                }
                e
            };
            i = end;
        } else {
            i = k;
        }
    }
}

/// HTML block start condition 7: a complete open or closing tag followed
/// only by whitespace on the line.
pub(crate) fn html_block_start_7(s: &[u8]) -> bool {
    if at(s, 0) != b'<' {
        return false;
    }
    let after_tag = if at(s, 1) == b'/' {
        let Some(i) = tag_name(s, 2) else {
            return false;
        };
        let i = skip_spacechars(s, i);
        if at(s, i) != b'>' {
            return false;
        }
        i + 1
    } else {
        let Some(i) = tag_name(s, 1) else {
            return false;
        };
        let Some(i) = attributes(s, i) else {
            return false;
        };
        let mut i = skip_spacechars(s, i);
        if at(s, i) == b'/' {
            i += 1;
        }
        if at(s, i) != b'>' {
            return false;
        }
        i + 1
    };
    let mut i = after_tag;
    while matches!(at(s, i), b'\t' | b'\n' | 0x0C | b' ') {
        i += 1;
    }
    // The pattern ends with `[\r\n\xff]`; a `\n` consumed above leaves the
    // scan at the sentinel, which also matches.
    is_eol(at(s, i)) || (i > after_tag && at(s, i - 1) == b'\n')
}

/// HTML block end conditions for types 1 to 5, searched in the line.
pub(crate) fn html_block_end(ty: u8, s: &[u8]) -> bool {
    let line = match memchr::memchr(b'\n', s) {
        Some(i) => &s[..i],
        None => s,
    };
    match ty {
        1 => {
            let mut i = 0;
            while let Some(j) = memchr::memchr(b'<', &line[i..]) {
                let p = i + j;
                if at(line, p + 1) == b'/' {
                    for tag in [&b"script"[..], b"pre", b"textarea", b"style"] {
                        if starts_with_ci(line, p + 2, tag) && at(line, p + 2 + tag.len()) == b'>' {
                            return true;
                        }
                    }
                }
                i = p + 1;
            }
            false
        }
        2 => memchr::memmem::find(line, b"-->").is_some(),
        3 => memchr::memmem::find(line, b"?>").is_some(),
        4 => memchr::memchr(b'>', line).is_some(),
        5 => memchr::memmem::find(line, b"]]>").is_some(),
        _ => false,
    }
}

/// A parsed list marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ListMarker {
    pub ordered: bool,
    /// Bullet character, or the delimiter (`.` or `)`) of an ordered marker.
    pub ch: u8,
    pub start: u32,
}

/// The list marker at `pos` of `line` (cmark's `parse_list_marker`).
/// Returns the marker length and the marker data.
///
/// The end of the input counts as the end of a line for both marker kinds
/// (`1.` at the very end of a document is an empty list item, like `-`).
pub(crate) fn parse_list_marker(
    line: &[u8],
    mut pos: usize,
    interrupts_paragraph: bool,
) -> Option<(usize, ListMarker)> {
    let start_pos = pos;
    let c = *line.get(pos)?;
    // Whether the item would be empty (only whitespace after the marker).
    let empty_after = |mut i: usize| {
        while i < line.len() && is_space_or_tab(line[i]) {
            i += 1;
        }
        i == line.len() || is_line_end(line[i])
    };
    if c == b'*' || c == b'-' || c == b'+' {
        pos += 1;
        if !line.get(pos).is_none_or(|&b| is_cmark_space(b)) {
            return None;
        }
        if interrupts_paragraph && empty_after(pos) {
            return None;
        }
        return Some((
            pos - start_pos,
            ListMarker {
                ordered: false,
                ch: c,
                start: 1,
            },
        ));
    }
    if c.is_ascii_digit() {
        let mut start: u32 = 0;
        let mut digits = 0;
        while digits < 9 && line.get(pos).is_some_and(u8::is_ascii_digit) {
            start = 10 * start + u32::from(line[pos] - b'0');
            pos += 1;
            digits += 1;
        }
        if interrupts_paragraph && start != 1 {
            return None;
        }
        let d = *line.get(pos)?;
        if d != b'.' && d != b')' {
            return None;
        }
        pos += 1;
        if !line.get(pos).is_none_or(|&b| is_cmark_space(b)) {
            return None;
        }
        if interrupts_paragraph && empty_after(pos) {
            return None;
        }
        return Some((
            pos - start_pos,
            ListMarker {
                ordered: true,
                ch: d,
                start,
            },
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html7() {
        assert!(html_block_start_7(b"<a href=\"foo\">\n"));
        assert!(html_block_start_7(b"<Warning>\n"));
        assert!(html_block_start_7(b"</ins>\n"));
        assert!(html_block_start_7(b"<a b='c' d=e/>  \n"));
        assert!(!html_block_start_7(b"<a> foo\n"));
        assert!(!html_block_start_7(b"<a h*#ref=\"hi\">\n"));
        assert!(html_block_start_7(b"<del>"));
    }

    #[test]
    fn fences_and_breaks() {
        assert_eq!(open_code_fence(b"```rust\n"), Some(3));
        assert_eq!(open_code_fence(b"``` a`b\n"), None);
        assert_eq!(open_code_fence(b"~~~~ a`b\n"), Some(4));
        assert_eq!(close_code_fence(b"````  \n"), Some(4));
        assert_eq!(close_code_fence(b"```x\n"), None);
        assert!(thematic_break(b"* * *\n").is_ok());
        assert!(thematic_break(b"---").is_ok());
        assert_eq!(thematic_break(b"--a\n"), Err(2));
        assert_eq!(thematic_break(b"- - x\n"), Err(4));
        assert_eq!(atx_heading_start(b"## x\n"), Some((2, 3)));
        assert_eq!(atx_heading_start(b"#\n"), Some((1, 1)));
        assert_eq!(atx_heading_start(b"#5\n"), None);
    }

    #[test]
    fn list_markers() {
        assert_eq!(parse_list_marker(b"1.", 0, false).map(|m| m.0), Some(2));
        assert_eq!(parse_list_marker(b"-", 0, false).map(|m| m.0), Some(1));
        assert_eq!(parse_list_marker(b"1.", 0, true), None);
        assert_eq!(parse_list_marker(b"2. a\n", 0, true), None);
        assert_eq!(parse_list_marker(b"1. a\n", 0, true).map(|m| m.0), Some(2));
        assert_eq!(parse_list_marker(b"1234567890. a\n", 0, false), None);
        assert_eq!(parse_list_marker(b"-a\n", 0, false), None);
    }
}

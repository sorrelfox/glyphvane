// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Sub-grammars shared by inline parsing and link reference definitions:
//! link destinations, titles and labels, entities, autolinks, raw HTML,
//! label normalization and the backslash/entity decoding of destinations
//! and titles.
//!
//! Scanners work on bytes and take explicit bounds; none of them allocates
//! and none of them looks past its `end`. Each returns `None` without
//! side effects when the construct does not match (raw HTML additionally
//! records which unterminated constructs need not be searched again).
//!
//! U+0000 is treated as U+FFFD everywhere (CommonMark replaces it before
//! parsing; this parser keeps the byte and replaces it when rendering), so
//! it is never an ASCII control character here.

use super::api::{RefDef, RefDefSpans};
use super::entities::ENTITIES;
use super::tree::ENTITY_NAMED;
use super::unicode;

/// Longest link label, in characters (CommonMark: at most 999).
pub(super) const MAX_LABEL_CHARS: usize = 999;

/// Deepest nesting of unescaped parentheses in a raw link destination.
const MAX_DEST_PARENS: u32 = 32;

/// ASCII punctuation: what a backslash can escape.
#[inline]
pub(super) fn is_escapable(b: u8) -> bool {
    b.is_ascii_punctuation()
}

/// Space, tab, LF or CR: the whitespace of labels and link syntax.
#[inline]
pub(super) fn is_space_or_eol(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// The end of the line ending at `i` (`\n`, `\r\n` or `\r`), if there is one.
#[inline]
pub(super) fn line_ending(b: &[u8], i: usize, end: usize) -> Option<usize> {
    match b.get(i) {
        Some(b'\n') if i < end => Some(i + 1),
        Some(b'\r') if i < end => Some(if i + 1 < end && b[i + 1] == b'\n' {
            i + 2
        } else {
            i + 1
        }),
        _ => None,
    }
}

/// Skips spaces and tabs.
#[inline]
pub(super) fn skip_spaces(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

/// Skips spaces and tabs, at most one line ending, and spaces and tabs.
pub(super) fn skip_spnl(b: &[u8], i: usize, end: usize) -> usize {
    let i = skip_spaces(b, i, end);
    match line_ending(b, i, end) {
        Some(j) => skip_spaces(b, j, end),
        None => i,
    }
}

/// Skips spaces, tabs and line endings.
pub(super) fn skip_whitespace(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && is_space_or_eol(b[i]) {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Link destinations, titles and labels

/// Scans a link destination at `i`: `<…>` or a raw destination. Returns its
/// end. A raw destination may be empty (the caller decides whether that is
/// allowed).
pub(super) fn link_destination(b: &[u8], i: usize, end: usize) -> Option<usize> {
    if i < end && b[i] == b'<' {
        let mut j = i + 1;
        while j < end {
            match b[j] {
                b'>' => return Some(j + 1),
                b'\\' if j + 1 < end && is_escapable(b[j + 1]) => j += 2,
                b'\n' | b'\r' | b'<' => return None,
                _ => j += 1,
            }
        }
        return None;
    }
    let mut j = i;
    let mut depth = 0u32;
    while j < end {
        let c = b[j];
        if c == b'\\' && j + 1 < end && is_escapable(b[j + 1]) {
            j += 2;
        } else if c == b'(' {
            depth += 1;
            if depth > MAX_DEST_PARENS {
                return None;
            }
            j += 1;
        } else if c == b')' {
            if depth == 0 {
                break;
            }
            depth -= 1;
            j += 1;
        } else if c == b' ' || (c < 0x20 && c != 0) || c == 0x7f {
            break;
        } else {
            j += 1;
        }
    }
    if depth != 0 {
        return None;
    }
    Some(j)
}

/// Scans a link title at `i` (`"…"`, `'…'` or `(…)`). Returns its end. A
/// title cannot contain a blank line.
pub(super) fn link_title(b: &[u8], i: usize, end: usize) -> Option<usize> {
    let open = *b.get(i).filter(|_| i < end)?;
    let close = match open {
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };
    let mut j = i + 1;
    // Whether only spaces and tabs were seen since the last line ending.
    let mut blank_so_far = false;
    while j < end {
        let c = b[j];
        if c == b'\\' && j + 1 < end && is_escapable(b[j + 1]) {
            j += 2;
            blank_so_far = false;
            continue;
        }
        if c == close {
            return Some(j + 1);
        }
        if open == b'(' && c == b'(' {
            return None;
        }
        if let Some(k) = line_ending(b, j, end) {
            if blank_so_far {
                return None;
            }
            blank_so_far = true;
            j = k;
            continue;
        }
        if !matches!(c, b' ' | b'\t') {
            blank_so_far = false;
        }
        j += 1;
    }
    None
}

/// Scans a link label `[…]` at `i`: at most 999 characters, no unescaped
/// brackets. Returns `(content start, content end, end after ']')`.
pub(super) fn link_label(text: &str, i: usize, end: usize) -> Option<(usize, usize, usize)> {
    let b = text.as_bytes();
    if i >= end || b[i] != b'[' {
        return None;
    }
    let mut j = i + 1;
    let mut chars = 0usize;
    while j < end {
        match b[j] {
            b']' => return Some((i + 1, j, j + 1)),
            b'[' => return None,
            b'\\' if j + 1 < end && is_escapable(b[j + 1]) => {
                j += 2;
                chars += 2;
            }
            c => {
                j += utf8_len(c);
                chars += 1;
            }
        }
        if chars > MAX_LABEL_CHARS {
            return None;
        }
    }
    None
}

/// Length of the UTF-8 sequence starting with `b` (1 for stray bytes).
#[inline]
fn utf8_len(b: u8) -> usize {
    match b {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

/// Whether `s` holds only spaces, tabs and line endings.
pub(super) fn is_blank(s: &[u8]) -> bool {
    s.iter().all(|&c| is_space_or_eol(c))
}

/// Whether `s` has at most `max` characters.
pub(super) fn chars_at_most(s: &str, max: usize) -> bool {
    s.len() <= max || s.chars().nth(max).is_none()
}

// ---------------------------------------------------------------------------
// Label normalization and decoding

/// Appends the CommonMark normalization of a link label: Unicode full case
/// folding, leading and trailing whitespace removed, internal whitespace
/// runs collapsed to one space.
pub(super) fn normalize_label_into(raw: &str, out: &mut String) {
    let b = raw.as_bytes();
    let mut i = 0;
    let mut pending_space = false;
    let mut started = false;
    while i < b.len() {
        let c = b[i];
        if is_space_or_eol(c) {
            pending_space = started;
            i += 1;
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        started = true;
        if c.is_ascii() && c != 0 {
            // A run of ASCII without whitespace: copy and lowercase it.
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii() && b[j] != 0 && !is_space_or_eol(b[j]) {
                j += 1;
            }
            let start = out.len();
            out.push_str(&raw[i..j]);
            out[start..].make_ascii_lowercase();
            i = j;
        } else {
            let ch = raw[i..].chars().next().unwrap_or('\u{FFFD}');
            unicode::case_fold_char(ch, out);
            i += ch.len_utf8();
        }
    }
}

/// Appends `s` with backslash escapes and entity references decoded, in one
/// left-to-right pass (a decoded `&#92;` does not escape what follows).
pub(super) fn decode_into(s: &str, out: &mut String) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        match b[i] {
            b'\\' if i + 1 < b.len() && is_escapable(b[i + 1]) => {
                out.push_str(&s[copied..i]);
                i += 1;
                copied = i;
                i += 1;
            }
            b'&' => {
                if let Some((len, value)) = entity(b, i, b.len()) {
                    out.push_str(&s[copied..i]);
                    push_entity(value, out);
                    i += len;
                    copied = i;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out.push_str(&s[copied..]);
}

/// Appends `s` with entity references decoded (autolinks: backslash
/// escapes do not work there).
pub(super) fn decode_entities_into(s: &str, out: &mut String) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i] == b'&'
            && let Some((len, value)) = entity(b, i, b.len())
        {
            out.push_str(&s[copied..i]);
            push_entity(value, out);
            i += len;
            copied = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[copied..]);
}

/// The range of `b[start..end]` without leading and trailing ASCII
/// whitespace (cmark trims link destinations this way, which matters for
/// `<…>` destinations).
pub(super) fn trim_range(b: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    let ws = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r');
    while start < end && ws(b[start]) {
        start += 1;
    }
    while end > start && ws(b[end - 1]) {
        end -= 1;
    }
    (start, end)
}

/// Whether `s` needs [`decode_into`] (it holds a backslash or an `&`).
pub(super) fn needs_decoding(s: &[u8]) -> bool {
    s.iter().any(|&c| c == b'\\' || c == b'&')
}

// ---------------------------------------------------------------------------
// Entities

/// Scans an entity or numeric character reference at `i` (an `&`). Returns
/// its length and value: a code point (invalid ones and U+0000 become
/// U+FFFD), or [`ENTITY_NAMED`] plus an index into the named-entity table.
pub(super) fn entity(b: &[u8], i: usize, end: usize) -> Option<(usize, u32)> {
    let end = end.min(b.len());
    if i + 2 >= end || b[i] != b'&' {
        return None;
    }
    if b[i + 1] == b'#' {
        let (hex, digits_start) = if matches!(b[i + 2], b'x' | b'X') {
            (true, i + 3)
        } else {
            (false, i + 2)
        };
        let max = if hex { 6 } else { 7 };
        let mut j = digits_start;
        let mut value: u32 = 0;
        while j < end && j - digits_start < max {
            let d = match (hex, b[j]) {
                (_, c @ b'0'..=b'9') => u32::from(c - b'0'),
                (true, c @ b'a'..=b'f') => u32::from(c - b'a') + 10,
                (true, c @ b'A'..=b'F') => u32::from(c - b'A') + 10,
                _ => break,
            };
            value = value * if hex { 16 } else { 10 } + d;
            j += 1;
        }
        if j == digits_start || j >= end || b[j] != b';' {
            return None;
        }
        let value = if value == 0 || char::from_u32(value).is_none() {
            0xFFFD
        } else {
            value
        };
        return Some((j + 1 - i, value));
    }
    if !b[i + 1].is_ascii_alphabetic() {
        return None;
    }
    let mut j = i + 2;
    while j < end && j - i <= 32 && b[j].is_ascii_alphanumeric() {
        j += 1;
    }
    if j >= end || b[j] != b';' {
        return None;
    }
    let name = std::str::from_utf8(&b[i + 1..j]).ok()?;
    let (lo, hi) = ENTITY_BY_FIRST[usize::from(b[i + 1] & 0x7F)];
    let slice = ENTITIES.get(lo as usize..hi as usize)?;
    let index = lo as usize + slice.binary_search_by(|(n, _)| (*n).cmp(name)).ok()?;
    Some((j + 1 - i, ENTITY_NAMED | index as u32))
}

/// For each ASCII byte, the range of [`ENTITIES`] whose names start with it.
static ENTITY_BY_FIRST: [(u16, u16); 128] = {
    let mut t = [(0u16, 0u16); 128];
    let mut i = 0;
    while i < ENTITIES.len() {
        let first = (ENTITIES[i].0.as_bytes()[0] & 0x7F) as usize;
        if t[first].1 == 0 {
            t[first].0 = i as u16;
        }
        t[first].1 = i as u16 + 1;
        i += 1;
    }
    t
};

/// Appends the text of an entity value from [`entity`].
pub(super) fn push_entity(value: u32, out: &mut String) {
    if value & ENTITY_NAMED != 0 {
        if let Some((_, text)) = ENTITIES.get((value & !ENTITY_NAMED) as usize) {
            out.push_str(text);
        }
    } else {
        out.push(char::from_u32(value).unwrap_or('\u{FFFD}'));
    }
}

// ---------------------------------------------------------------------------
// Autolinks

/// Scans a URI autolink at `i` (a `<`). Returns its length.
pub(super) fn autolink_uri(b: &[u8], i: usize, end: usize) -> Option<usize> {
    let mut j = i + 1;
    if j >= end || !b[j].is_ascii_alphabetic() {
        return None;
    }
    j += 1;
    while j < end && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'+' | b'.' | b'-')) {
        j += 1;
    }
    let scheme = j - i - 1;
    if !(2..=32).contains(&scheme) || j >= end || b[j] != b':' {
        return None;
    }
    j += 1;
    while j < end {
        match b[j] {
            b'>' => return Some(j + 1 - i),
            b'<' | b' ' | 0x7f => return None,
            c if c < 0x20 && c != 0 => return None,
            _ => j += 1,
        }
    }
    None
}

/// Bytes allowed in the local part of an email autolink.
static EMAIL_LOCAL: [bool; 256] = {
    let mut t = [false; 256];
    let mut c = 0;
    while c < 128 {
        t[c] = (c as u8).is_ascii_alphanumeric();
        c += 1;
    }
    let extra = b".!#$%&'*+/=?^_`{|}~-";
    let mut i = 0;
    while i < extra.len() {
        t[extra[i] as usize] = true;
        i += 1;
    }
    t
};

/// Scans an email autolink at `i` (a `<`). Returns its length.
pub(super) fn autolink_email(b: &[u8], i: usize, end: usize) -> Option<usize> {
    let mut j = i + 1;
    let local = j;
    while j < end && EMAIL_LOCAL[usize::from(b[j])] {
        j += 1;
    }
    if j == local || j >= end || b[j] != b'@' {
        return None;
    }
    j += 1;
    loop {
        let label = j;
        if j >= end || !b[j].is_ascii_alphanumeric() {
            return None;
        }
        while j < end && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        if j - label > 63 || b[j - 1] == b'-' {
            return None;
        }
        if j < end && b[j] == b'.' {
            j += 1;
        } else {
            break;
        }
    }
    (j < end && b[j] == b'>').then_some(j + 1 - i)
}

// ---------------------------------------------------------------------------
// Raw HTML

/// Searches for terminators that were not found once are not searched again:
/// a failed search ran to the end of the text, so every later one fails too.
pub(super) const SKIP_COMMENT: u8 = 1;
pub(super) const SKIP_PI: u8 = 2;
pub(super) const SKIP_DECLARATION: u8 = 4;
pub(super) const SKIP_CDATA: u8 = 8;
pub(super) const SKIP_DQUOTE: u8 = 16;
pub(super) const SKIP_SQUOTE: u8 = 32;

/// Finds `needle` in `b[from..end]`.
fn find(b: &[u8], from: usize, end: usize, needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    let mut j = from;
    while j + needle.len() <= end {
        if b[j] == first && &b[j..j + needle.len()] == needle {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Finds `needle` from `from`; on failure sets `flag` in `skip`.
fn find_or_skip(
    b: &[u8],
    from: usize,
    end: usize,
    needle: &[u8],
    skip: &mut u8,
    flag: u8,
) -> Option<usize> {
    if *skip & flag != 0 {
        return None;
    }
    let found = find(b, from, end, needle);
    if found.is_none() {
        *skip |= flag;
    }
    found
}

/// HTML whitespace: spaces, tabs and at most one line ending.
fn skip_html_ws(b: &[u8], i: usize, end: usize) -> usize {
    skip_spnl(b, i, end)
}

/// Scans a tag name; returns its end.
fn tag_name(b: &[u8], i: usize, end: usize) -> Option<usize> {
    if i >= end || !b[i].is_ascii_alphabetic() {
        return None;
    }
    let mut j = i + 1;
    while j < end && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
        j += 1;
    }
    Some(j)
}

/// Scans raw inline HTML at `i` (a `<`): an open or closing tag, a comment,
/// a processing instruction, a declaration or a CDATA section. Returns its
/// length.
pub(super) fn raw_html(b: &[u8], i: usize, end: usize, skip: &mut u8) -> Option<usize> {
    let rest = |n: usize| b.get(i + n..end).unwrap_or(&[]);
    let c = *b.get(i + 1).filter(|_| i + 1 < end)?;
    let close = match c {
        b'!' => {
            if rest(2).starts_with(b"--") {
                if rest(4).starts_with(b">") {
                    return Some(5);
                }
                if rest(4).starts_with(b"->") {
                    return Some(6);
                }
                find_or_skip(b, i + 4, end, b"-->", skip, SKIP_COMMENT)? + 3
            } else if rest(2).starts_with(b"[CDATA[") {
                find_or_skip(b, i + 9, end, b"]]>", skip, SKIP_CDATA)? + 3
            } else if rest(2).first().is_some_and(u8::is_ascii_alphabetic) {
                find_or_skip(b, i + 3, end, b">", skip, SKIP_DECLARATION)? + 1
            } else {
                return None;
            }
        }
        b'?' => find_or_skip(b, i + 2, end, b"?>", skip, SKIP_PI)? + 2,
        b'/' => {
            let j = tag_name(b, i + 2, end)?;
            let j = skip_html_ws(b, j, end);
            if j < end && b[j] == b'>' {
                j + 1
            } else {
                return None;
            }
        }
        _ => open_tag(b, i, end, skip)?,
    };
    Some(close - i)
}

/// Scans an open tag at `i`; returns its end.
fn open_tag(b: &[u8], i: usize, end: usize, skip: &mut u8) -> Option<usize> {
    let mut j = tag_name(b, i + 1, end)?;
    loop {
        let ws = j;
        j = skip_html_ws(b, j, end);
        if j < end && b[j] == b'>' {
            return Some(j + 1);
        }
        if j + 1 < end && b[j] == b'/' && b[j + 1] == b'>' {
            return Some(j + 2);
        }
        if j == ws || j >= end {
            return None;
        }
        // Attribute name.
        if !(b[j].is_ascii_alphabetic() || matches!(b[j], b'_' | b':')) {
            return None;
        }
        j += 1;
        while j < end && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'_' | b'.' | b':' | b'-'))
        {
            j += 1;
        }
        // Optional value.
        let k = skip_html_ws(b, j, end);
        if k < end && b[k] == b'=' {
            let v = skip_html_ws(b, k + 1, end);
            if v >= end {
                return None;
            }
            j = match b[v] {
                b'"' => find_or_skip(b, v + 1, end, b"\"", skip, SKIP_DQUOTE)? + 1,
                b'\'' => find_or_skip(b, v + 1, end, b"'", skip, SKIP_SQUOTE)? + 1,
                _ => {
                    let mut e = v;
                    while e < end
                        && !matches!(
                            b[e],
                            b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b'=' | b'<' | b'>' | b'`'
                        )
                    {
                        e += 1;
                    }
                    if e == v {
                        return None;
                    }
                    e
                }
            };
        }
    }
}

// ---------------------------------------------------------------------------
// Link reference definitions

/// Parses one link reference definition at the start of `text` (see
/// [`super::api::parse_ref_def`]).
pub(super) fn ref_def(text: &str) -> Option<(usize, String, RefDef)> {
    ref_def_spans(text).map(|(n, d)| (n, d.label, d.def))
}

/// [`ref_def`] with the ranges of the parts in `text` (see
/// [`super::api::parse_ref_def_spans`]).
pub(super) fn ref_def_spans(text: &str) -> Option<(usize, RefDefSpans)> {
    let b = text.as_bytes();
    let end = b.len();
    let (label_start, label_end, mut i) = link_label(text, 0, end)?;
    if is_blank(&b[label_start..label_end]) || i >= end || b[i] != b':' {
        return None;
    }
    i = skip_spnl(b, i + 1, end);
    let dest_start = i;
    let dest_end = link_destination(b, i, end)?;
    if dest_end == dest_start {
        return None;
    }
    let before_title = dest_end;
    let mut title = None;
    let t = skip_spnl(b, before_title, end);
    if t > before_title
        && let Some(title_end) = link_title(b, t, end)
    {
        let after = skip_spaces(b, title_end, end);
        if after == end || line_ending(b, after, end).is_some() {
            title = Some((t, title_end));
            i = after;
        }
    }
    if title.is_none() {
        i = skip_spaces(b, before_title, end);
        if i < end && line_ending(b, i, end).is_none() {
            return None;
        }
    }
    let consumed = line_ending(b, i, end).unwrap_or(end);
    let mut label = String::new();
    normalize_label_into(&text[label_start..label_end], &mut label);
    let (ds, de) = if b[dest_start] == b'<' {
        trim_range(b, dest_start + 1, dest_end - 1)
    } else {
        (dest_start, dest_end)
    };
    let raw_dest = &text[ds..de];
    let mut destination = String::with_capacity(raw_dest.len());
    decode_into(raw_dest, &mut destination);
    let title_text = title.map(|(s, e)| {
        let mut t = String::with_capacity(e - s);
        decode_into(&text[s + 1..e - 1], &mut t);
        t
    });
    Some((
        consumed,
        RefDefSpans {
            label,
            def: RefDef {
                destination,
                title: title_text,
            },
            range: 0..i,
            label_range: label_start..label_end,
            destination_range: dest_start..dest_end,
            title_range: title.map(|(s, e)| s..e),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(s: &str) -> Option<String> {
        let (len, v) = entity(s.as_bytes(), 0, s.len())?;
        let mut out = String::new();
        push_entity(v, &mut out);
        Some(format!("{len}:{out}"))
    }

    #[test]
    fn entities() {
        assert_eq!(ent("&amp;x").as_deref(), Some("5:&"));
        assert_eq!(ent("&#35;").as_deref(), Some("5:#"));
        assert_eq!(ent("&#X22;").as_deref(), Some("6:\""));
        assert_eq!(ent("&#0;").as_deref(), Some("4:\u{FFFD}"));
        assert_eq!(ent("&#xD800;").as_deref(), Some("8:\u{FFFD}"));
        assert_eq!(ent("&#9999999;").as_deref(), Some("10:\u{FFFD}"));
        assert_eq!(ent("&#12345678;"), None);
        assert_eq!(ent("&#xabcdef0;"), None);
        assert_eq!(ent("&ngE;").as_deref(), Some("5:\u{2267}\u{338}"));
        assert_eq!(ent("&nosuch;"), None);
        assert_eq!(ent("&copy"), None);
        assert_eq!(ent("&#;"), None);
        assert_eq!(ent("&#x;"), None);
    }

    #[test]
    fn destinations_and_titles() {
        let d = |s: &str| link_destination(s.as_bytes(), 0, s.len());
        assert_eq!(d("<a b>c"), Some(5));
        assert_eq!(d("<a\\>b>"), Some(6));
        assert_eq!(d("<a\nb>"), None);
        assert_eq!(d("a(b)c)"), Some(5));
        assert_eq!(d("a(b"), None);
        assert_eq!(d("a b"), Some(1));
        assert_eq!(d(")"), Some(0));
        assert_eq!(
            d(&format!("{}x{}", "(".repeat(32), ")".repeat(32))),
            Some(65)
        );
        assert_eq!(d(&format!("{}x{}", "(".repeat(33), ")".repeat(33))), None);
        let t = |s: &str| link_title(s.as_bytes(), 0, s.len());
        assert_eq!(t("\"a\\\"b\" x"), Some(6));
        assert_eq!(t("'a'"), Some(3));
        assert_eq!(t("(a(b)"), None);
        assert_eq!(t("(a\\(b)"), Some(6));
        assert_eq!(t("\"a\n\nb\""), None);
        assert_eq!(t("\"a\n \nb\""), None);
        assert_eq!(t("\"a\nb\""), Some(5));
    }

    #[test]
    fn labels() {
        let l = |s: &str| link_label(s, 0, s.len());
        assert_eq!(l("[foo]x"), Some((1, 4, 5)));
        assert_eq!(l("[fo\\]o]"), Some((1, 6, 7)));
        assert_eq!(l("[fo[o]"), None);
        assert_eq!(l(&format!("[{}]", "x".repeat(999))), Some((1, 1000, 1001)));
        assert_eq!(l(&format!("[{}]", "x".repeat(1000))), None);
        assert_eq!(
            l(&format!("[{}]", "é".repeat(999))).map(|x| x.2),
            Some(1 + 999 * 2 + 1)
        );
        let mut s = String::new();
        normalize_label_into("  Foo \t\n BAR ẞ ", &mut s);
        assert_eq!(s, "foo bar ss");
        s.clear();
        normalize_label_into("A\0bÉ\u{3000}x", &mut s);
        assert_eq!(s, "a\u{FFFD}bé\u{3000}x");
    }

    #[test]
    fn autolinks() {
        let u = |s: &str| autolink_uri(s.as_bytes(), 0, s.len());
        assert_eq!(u("<http://a>"), Some(10));
        assert_eq!(u("<a:b>"), None);
        assert_eq!(u("<ab:>"), Some(5));
        assert_eq!(u("<http://a b>"), None);
        let e = |s: &str| autolink_email(s.as_bytes(), 0, s.len());
        assert_eq!(e("<a@b.c>"), Some(7));
        assert_eq!(e("<a@-b.c>"), None);
        assert_eq!(e("<a@b-.c>"), None);
        assert_eq!(e("<a@b.>"), None);
        assert_eq!(e("<@b>"), None);
    }

    #[test]
    fn html() {
        let h = |s: &str| {
            let mut skip = 0;
            raw_html(s.as_bytes(), 0, s.len(), &mut skip)
        };
        assert_eq!(h("<a>"), Some(3));
        assert_eq!(h("<a/>"), Some(4));
        assert_eq!(h("<a b='c' d=\"e\" f=g h>"), Some(21));
        assert_eq!(h("<a b='c'd>"), None);
        assert_eq!(h("<a\nb>"), Some(5));
        assert_eq!(h("</a >"), Some(5));
        assert_eq!(h("</a b>"), None);
        assert_eq!(h("<!-->"), Some(5));
        assert_eq!(h("<!--->"), Some(6));
        assert_eq!(h("<!-- a -- b -->"), Some(15));
        assert_eq!(h("<?php ?>"), Some(8));
        assert_eq!(h("<!DOCTYPE x>"), Some(12));
        assert_eq!(h("<![CDATA[ x ]]>"), Some(15));
        assert_eq!(h("<33>"), None);
        assert_eq!(h("<a b=>"), None);
    }

    #[test]
    fn decoding() {
        let mut s = String::new();
        decode_into(r"a\*b&amp;c&#92;*\q&nope;", &mut s);
        assert_eq!(s, r"a*b&c\*\q&nope;");
    }

    #[test]
    fn ref_defs() {
        let r = |s: &str| ref_def(s).map(|(n, l, d)| (n, l, d.destination, d.title));
        assert_eq!(
            r("[Foo]: /url \"t\"\nrest"),
            Some((16, "foo".into(), "/url".into(), Some("t".into())))
        );
        assert_eq!(r("[a]: <>"), Some((7, "a".into(), String::new(), None)));
        assert_eq!(r("[a]:"), None);
        assert_eq!(
            r("[a]:\n/u\n'x\ny'"),
            Some((13, "a".into(), "/u".into(), Some("x\ny".into())))
        );
        assert_eq!(r("[a]: /u 'x' z"), None);
        assert_eq!(
            r("[a]: /u\n'x' z"),
            Some((8, "a".into(), "/u".into(), None))
        );
        assert_eq!(r("[ ]: /u"), None);
        assert_eq!(r("[a]: /u(x"), None);
        assert_eq!(r("[a]: <b>(c)"), None);
        assert_eq!(r("[a]: < b >"), Some((10, "a".into(), "b".into(), None)));
    }
}

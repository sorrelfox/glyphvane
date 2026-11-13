// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Character classes for delimiter flanking and case folding for label
//! normalization.

use super::unicode_tables::{CASE_FOLD, PUNCT_RANGES, ZS_RANGES};

/// Whether `ranges` (sorted, inclusive) contains `cp`.
fn in_ranges(ranges: &[(u32, u32)], cp: u32) -> bool {
    let i = ranges.partition_point(|&(_, hi)| hi < cp);
    ranges.get(i).is_some_and(|&(lo, _)| lo <= cp)
}

/// CommonMark's Unicode whitespace: general category Zs, tab, line feed,
/// form feed, carriage return.
pub(super) fn is_whitespace(c: char) -> bool {
    match c {
        '\t' | '\n' | '\x0C' | '\r' | ' ' => true,
        c if c.is_ascii() => false,
        c => in_ranges(ZS_RANGES, c as u32),
    }
}

/// CommonMark's Unicode punctuation: general categories P and S.
pub(super) fn is_punctuation(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_punctuation()
    } else {
        in_ranges(PUNCT_RANGES, c as u32)
    }
}

/// Appends the full case folding of `c` (U+0000 folds to U+FFFD, which is
/// what CommonMark parsers see in its place).
pub(super) fn case_fold_char(c: char, out: &mut String) {
    if c.is_ascii() {
        out.push(if c == '\0' {
            '\u{FFFD}'
        } else {
            c.to_ascii_lowercase()
        });
        return;
    }
    match CASE_FOLD.binary_search_by_key(&(c as u32), |&(cp, _)| cp) {
        Ok(i) => out.push_str(CASE_FOLD[i].1),
        Err(_) => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert!(is_whitespace('\u{A0}'));
        assert!(is_whitespace('\u{3000}'));
        assert!(!is_whitespace('\u{200B}'));
        assert!(!is_whitespace('\x0B'));
        assert!(is_punctuation('!'));
        assert!(is_punctuation('$'));
        assert!(is_punctuation('\u{A3}'));
        assert!(is_punctuation('\u{20AC}'));
        assert!(is_punctuation('\u{FFFD}'));
        assert!(is_punctuation('\u{2014}'));
        assert!(!is_punctuation('a'));
        assert!(!is_punctuation('\u{E9}'));
    }

    #[test]
    fn folding() {
        let fold = |s: &str| {
            let mut out = String::new();
            for c in s.chars() {
                case_fold_char(c, &mut out);
            }
            out
        };
        assert_eq!(fold("ẞß"), "ssss");
        assert_eq!(fold("ΑΣς"), "ασσ");
        assert_eq!(fold("İ"), "i\u{307}");
        assert_eq!(fold("ﬃ"), "ffi");
        assert_eq!(fold("Ab\0"), "ab\u{FFFD}");
    }
}

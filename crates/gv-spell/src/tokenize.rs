// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Words of a run of prose.
//!
//! - Words are UAX #29 words that hold a letter or digit. UAX #29 keeps `'`
//!   and `’` between letters (`can't`) and never takes a leading or
//!   trailing apostrophe (`'color'` is `color`).
//! - Hyphenated words pass whole (`well-known`): spellbook splits them at
//!   the dictionary's `BREAK` characters itself.
//! - Which words are then skipped (digits, all capitals, `file.md`,
//!   `snake_case`, scripts without spaces) is [`crate::Rules`]' business.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation as _;

/// Hyphens that join a hyphenated word: `-` and U+2010.
fn is_hyphen(s: &str) -> bool {
    s == "-" || s == "\u{2010}"
}

fn is_word(s: &str) -> bool {
    s.chars().any(char::is_alphanumeric)
}

/// The words of `text`, as byte ranges into it.
pub fn words(text: &str) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    // Whether the last piece was a hyphen right after the last word.
    let mut hyphen_after_word: Option<usize> = None;
    for (at, piece) in text.split_word_bound_indices() {
        let end = at + piece.len();
        if is_word(piece) {
            match (hyphen_after_word.take(), out.last_mut()) {
                (Some(h), Some(last)) if h == at => last.end = end,
                _ => out.push(at..end),
            }
        } else if is_hyphen(piece) && out.last().is_some_and(|w| w.end == at) {
            hyphen_after_word = Some(end);
        } else {
            hyphen_after_word = None;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(text: &str) -> Vec<&str> {
        words(text).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn apostrophes_and_hyphens() {
        assert_eq!(
            of("It's a well-known fact."),
            ["It's", "a", "well-known", "fact"]
        );
        assert_eq!(
            of("can\u{2019}t 'quoted' countries'"),
            ["can\u{2019}t", "quoted", "countries"]
        );
        assert_eq!(of("a - b -c d-"), ["a", "b", "c", "d"]);
        assert_eq!(of("e-mail-ish x\u{2010}y"), ["e-mail-ish", "x\u{2010}y"]);
    }

    #[test]
    fn domains_and_identifiers_stay_whole() {
        // So the rules can skip them as a whole.
        assert_eq!(
            of("see file.md and snake_case 3.14"),
            ["see", "file.md", "and", "snake_case", "3.14"]
        );
    }

    #[test]
    fn non_latin_scripts() {
        assert_eq!(of("Вода — это ü-Umlaut"), ["Вода", "это", "ü-Umlaut"]);
        // Ideographs come one by one.
        assert_eq!(of("漢字"), ["漢", "字"]);
    }
}

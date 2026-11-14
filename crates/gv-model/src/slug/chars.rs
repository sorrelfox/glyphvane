// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Character classes the slug algorithms are defined in, each named after
//! the predicate of the tool it stands for.
//!
//! General categories come from `unicode-properties` (Unicode 18); the
//! `Alphabetic` and `White_Space` properties from the standard library.
//! The tools use their own Unicode versions, so characters assigned in
//! recent versions can differ (see the module docs of [`super`]).

use unicode_properties::{GeneralCategory as Gc, UnicodeGeneralCategory};

/// The general category (ASCII is answered without a table lookup by the
/// callers that care).
#[inline]
pub(crate) fn gc(c: char) -> Gc {
    c.general_category()
}

/// Letter: `Lu`, `Ll`, `Lt`, `Lm`, `Lo`.
#[inline]
pub(crate) fn is_letter(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphabetic();
    }
    matches!(
        gc(c),
        Gc::UppercaseLetter
            | Gc::LowercaseLetter
            | Gc::TitlecaseLetter
            | Gc::ModifierLetter
            | Gc::OtherLetter
    )
}

/// Mark: `Mn`, `Mc`, `Me`.
#[inline]
pub(crate) fn is_mark(c: char) -> bool {
    !c.is_ascii()
        && matches!(
            gc(c),
            Gc::NonspacingMark | Gc::SpacingMark | Gc::EnclosingMark
        )
}

/// Nonspacing mark: `Mn`.
#[inline]
pub(crate) fn is_nonspacing_mark(c: char) -> bool {
    !c.is_ascii() && gc(c) == Gc::NonspacingMark
}

/// Number: `Nd`, `Nl`, `No`.
#[inline]
pub(crate) fn is_number(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    matches!(
        gc(c),
        Gc::DecimalNumber | Gc::LetterNumber | Gc::OtherNumber
    )
}

/// Decimal number: `Nd`.
#[inline]
pub(crate) fn is_decimal(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    gc(c) == Gc::DecimalNumber
}

/// Connector punctuation: `Pc` (`_` and a few others).
#[inline]
pub(crate) fn is_connector(c: char) -> bool {
    if c.is_ascii() {
        return c == '_';
    }
    gc(c) == Gc::ConnectorPunctuation
}

/// Haskell's `Data.Char.isSpace`: space, `\t` to `\r`, U+00A0, and every
/// other `Zs` character.
#[inline]
pub(crate) fn is_haskell_space(c: char) -> bool {
    match c {
        ' ' | '\t'..='\r' | '\u{a0}' => true,
        c if (c as u32) <= 0x377 => false,
        c => gc(c) == Gc::SpaceSeparator,
    }
}

/// Python's `str.isspace` (what `\s` matches in a `str` pattern): the
/// `White_Space` property plus the information separators U+001C to
/// U+001F.
#[inline]
pub(crate) fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `\w` in a `str` pattern: `str.isalnum()` (letters and every
/// character with a numeric value) or `_`.
#[inline]
pub(crate) fn is_python_word(c: char) -> bool {
    c == '_' || is_letter(c) || is_number(c)
}

/// The simple (one character) lowercase mapping, as Go's
/// `unicode.ToLower`. The full mapping differs only for U+0130, whose
/// simple mapping is the first character of the full one.
#[inline]
pub(crate) fn simple_lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Lowercases character by character with the full mappings but without
/// the context-dependent final sigma (Haskell's `Data.Text.toLower`).
pub(crate) fn lower_no_final_sigma(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        out.extend(c.to_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert!(is_letter('é') && is_letter('Ж') && is_letter('你'));
        assert!(!is_letter('1') && !is_letter('_'));
        assert!(is_mark('\u{301}') && is_mark('\u{93f}') && !is_mark('a'));
        assert!(is_number('²') && is_number('Ⅻ') && !is_decimal('²'));
        assert!(is_decimal('٣'));
        assert!(is_connector('_') && is_connector('‿') && !is_connector('-'));
        assert!(is_haskell_space('\u{3000}') && !is_haskell_space('\u{85}'));
        assert!(is_python_space('\u{1c}') && is_python_space('\u{85}'));
        assert_eq!(simple_lower('\u{130}'), 'i');
        assert_eq!(lower_no_final_sigma("ΑΣ"), "ασ");
        assert_eq!("ΑΣ".to_lowercase(), "ας");
    }
}

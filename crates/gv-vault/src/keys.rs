// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Matching keys for names and paths.
//!
//! Link resolution compares names on three levels, in this order:
//!
//! 1. **exact bytes**;
//! 2. **NFC**: canonically equivalent names (macOS stores decomposed NFD
//!    names, most other systems and editors produce NFC);
//! 3. **case fold**: the Unicode *canonical caseless match* of
//!    Unicode 3.13 (D145), i.e. `NFD(fold(NFD(x)))`, using the **full** case
//!    folding table (`C` + `F` mappings, so `ß` and `ss` fold together).
//!
//! The keys are plain `String`s so they can be stored in hash maps, sorted
//! and compared without allocation at lookup time. Keys are normalized to
//! NFC (not NFD) because NFC is shorter for the common Latin case.

use std::borrow::Cow;

use caseless::Caseless;
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

/// The Unicode version of the case-folding table behind [`fold_key`].
pub const CASE_FOLDING_UNICODE_VERSION: (u64, u64, u64) = caseless::UNICODE_VERSION;

/// Returns `s` in Unicode Normalization Form C. Borrows when `s` is already
/// NFC (always the case for ASCII).
#[must_use]
pub fn nfc_key(s: &str) -> Cow<'_, str> {
    if s.is_ascii() || is_nfc_quick(s.chars()) == IsNormalized::Yes {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(s.nfc().collect())
    }
}

/// Returns `s` in Unicode Normalization Form D.
#[must_use]
pub fn nfd(s: &str) -> String {
    s.nfd().collect()
}

/// Returns `true` if `s` is in NFC.
#[must_use]
pub fn is_nfc(s: &str) -> bool {
    s.is_ascii() || unicode_normalization::is_nfc(s)
}

/// The canonical caseless key of `s`: `NFC(fold(NFD(s)))`.
///
/// Two strings have equal keys exactly when they are a canonical caseless
/// match (Unicode D145), so the key also absorbs NFC/NFD differences.
#[must_use]
pub fn fold_key(s: &str) -> String {
    if s.is_ascii() {
        // Full case folding maps nothing in ASCII except A-Z.
        return s.to_ascii_lowercase();
    }
    s.nfd().default_case_fold().nfc().collect()
}

/// Simple per-character uppercase mapping, the comparison NTFS uses for
/// case-insensitive names (its upcase table). Characters whose uppercase
/// form is more than one character are kept as they are, as in NTFS
/// (`ß` stays `ß`).
#[must_use]
pub fn simple_upper_key(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_borrows_ascii_and_nfc() {
        assert!(matches!(nfc_key("Note.md"), Cow::Borrowed(_)));
        assert!(matches!(nfc_key("Caf\u{e9}"), Cow::Borrowed(_)));
        assert_eq!(nfc_key("Cafe\u{301}"), "Caf\u{e9}");
    }

    #[test]
    fn fold_is_canonical_caseless() {
        assert_eq!(fold_key("README.md"), "readme.md");
        assert_eq!(fold_key("Caf\u{e9}"), fold_key("CAFE\u{301}"));
        assert_eq!(fold_key("Stra\u{df}e"), fold_key("STRASSE"));
        assert_eq!(
            fold_key("\u{3a3}\u{3b1}\u{3c2}"),
            fold_key("\u{3c3}\u{3b1}\u{3c3}")
        );
        // Kelvin sign folds to k.
        assert_eq!(fold_key("\u{212a}elvin"), "kelvin");
        // U+0345 inside a decomposition: the inner NFD matters.
        assert_eq!(fold_key("\u{1fc3}"), fold_key("\u{3b7}\u{345}"));
    }

    #[test]
    fn simple_upper_keeps_multi_char_uppercase() {
        assert_eq!(simple_upper_key("Stra\u{df}e"), "STRA\u{df}E");
        assert_eq!(simple_upper_key("readme"), "README");
    }
}

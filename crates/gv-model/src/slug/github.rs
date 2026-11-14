// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GitHub heading anchors, as github-slugger 2.0.0 computes them
//! (`index.js` and `script/generate-regex.js`, checked 2026-11-13 at
//! commit 3461c43 of github.com/Flet/github-slugger).
//!
//! ```js
//! value.toLowerCase().replace(regex, '').replace(/ /g, '-')
//! ```
//!
//! `regex` removes the general categories `No`, `P*` except `Pc` (and
//! `-`), `S*`, `Cc`, `Cf`, `Co`, `Cn` and `Z*` (except the space), minus
//! every `Alphabetic` character. What survives: letters, marks, `Nd`, `Nl`,
//! `Pc`, `Alphabetic` symbols (such as Ⓐ), `-` and spaces; each space
//! becomes a `-`.

use super::chars::{gc, is_letter};
use unicode_properties::GeneralCategory as Gc;

/// The GitHub base slug.
pub(super) fn slug(text: &str) -> String {
    // JavaScript's `toLowerCase`: full mappings, final sigma included.
    let lower = text.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for c in lower.chars() {
        if c == ' ' {
            out.push('-');
        } else if keeps(c) {
            out.push(c);
        }
    }
    out
}

/// Whether github-slugger's pattern leaves `c` in place (space aside).
fn keeps(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || c == '_' || c == '-';
    }
    if c.is_alphabetic() || is_letter(c) {
        return true;
    }
    matches!(
        gc(c),
        Gc::NonspacingMark
            | Gc::SpacingMark
            | Gc::EnclosingMark
            | Gc::DecimalNumber
            | Gc::LetterNumber
            | Gc::ConnectorPunctuation
            | Gc::Surrogate
    )
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn basics() {
        assert_eq!(slug("Spaces   between"), "spaces---between");
        assert_eq!(slug("a_b-c.d/e"), "a_b-cde");
        assert_eq!(slug("😄 emoji"), "-emoji");
        assert_eq!(slug("Θ"), "θ");
        assert_eq!(slug("İstanbul"), "i\u{307}stanbul");
        assert_eq!(slug("ΟΔΟΣ"), "οδος");
        assert_eq!(slug("Ⓐ ²"), "ⓐ-");
        assert_eq!(slug("tab\there"), "tabhere");
    }
}

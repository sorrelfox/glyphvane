// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Hugo's Goldmark heading ids (`markup/goldmark/autoid.go` and
//! `markup/blackfriday/anchors.go`, checked 2026-11-13 at commit ef7c525
//! of github.com/gohugoio/hugo).
//!
//! - `github`: trim spaces; `-` and space become `-`; `_`, letters (`L`)
//!   and decimal digits (`Nd`) are kept, lowercased (Go's simple
//!   mapping); everything else is dropped.
//! - `github-ascii`: the same after removing accents (NFD, `Mn` dropped,
//!   NFC), keeping only ASCII.
//! - `blackfriday`: letters and numbers (`N`) are kept, lowercased; a run
//!   of anything else between two of them becomes one `-`.
//!
//! Goldmark generates the id from the source of the heading's last line,
//! so Markdown syntax is part of the input. An empty id becomes `heading`.

use unicode_normalization::UnicodeNormalization;

use super::HugoIds;
use super::chars::{is_decimal, is_letter, is_nonspacing_mark, is_number, simple_lower};

/// The Hugo base id.
pub(super) fn slug(text: &str, ids: HugoIds) -> String {
    match ids {
        HugoIds::Blackfriday => blackfriday(text),
        HugoIds::GitHub => github(text.trim(), false),
        HugoIds::GitHubAscii => {
            let plain: String = text
                .nfd()
                .filter(|&c| !is_nonspacing_mark(c))
                .nfc()
                .collect();
            github(plain.trim(), true)
        }
    }
}

fn github(text: &str, ascii_only: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if ascii_only && !c.is_ascii() {
            continue;
        }
        if c == '-' || c == ' ' {
            out.push('-');
        } else if c == '_' || is_letter(c) || is_decimal(c) {
            out.push(simple_lower(c));
        }
    }
    out
}

fn blackfriday(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut dash = false;
    for c in text.chars() {
        if is_letter(c) || is_number(c) {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(simple_lower(c));
        } else {
            dash = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TestSanitizeAnchorName` in Hugo's `markup/goldmark/autoid_test.go`
    /// (Apache-2.0; "generated manually on github.com"). Also run through
    /// the GitHub pipeline in the integration tests.
    const HUGO_GITHUB_CASES: &[(&str, &str)] = &[
        ("God is good: 神真美好", "god-is-good-神真美好"),
        ("Number 32", "number-32"),
        ("Question?", "question"),
        ("1+2=3", "123"),
        ("Special !\"#$%&(parens)=?´* chars", "special-parens-chars"),
        ("Resumé", "resumé"),
        ("One-Hyphen", "one-hyphen"),
        ("Multiple--Hyphens", "multiple--hyphens"),
        ("Trailing hyphen-", "trailing-hyphen-"),
        ("Many   spaces  here", "many---spaces--here"),
        ("Forward/slash", "forwardslash"),
        ("Backward\\slash", "backwardslash"),
        ("Under_score", "under_score"),
        ("Nonbreaking\u{a0}Space", "nonbreakingspace"),
        ("Tab\tSpace", "tabspace"),
        ("Trailing Space ", "trailing-space"),
    ];

    #[test]
    fn test_suite() {
        for &(input, want) in HUGO_GITHUB_CASES {
            assert_eq!(slug(input, HugoIds::GitHub), want, "{input:?}");
        }
        // TestSanitizeAnchorNameAsciiOnly, TestSanitizeAnchorNameBlackfriday.
        assert_eq!(
            slug("god is神真美好 good", HugoIds::GitHubAscii),
            "god-is-good"
        );
        assert_eq!(slug("Resumé", HugoIds::GitHubAscii), "resume");
        assert_eq!(
            slug("Let's try this, shall we?", HugoIds::Blackfriday),
            "let-s-try-this-shall-we"
        );
    }

    #[test]
    fn simple_lowercase() {
        assert_eq!(slug("İ", HugoIds::GitHub), "i");
        assert_eq!(slug("x²", HugoIds::GitHub), "x");
        assert_eq!(slug("x²", HugoIds::Blackfriday), "x²");
    }
}

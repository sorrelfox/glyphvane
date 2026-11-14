// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Pandoc's `auto_identifiers` (`textToIdentifier` and `uniqueIdent` in
//! `src/Text/Pandoc/Shared.hs`, checked 2026-11-13 at commit 300d24f of
//! github.com/jgm/pandoc):
//!
//! ```haskell
//! textToIdentifier exts = dropNonLetter . filterAscii . toIdent
//!   -- default:  T.intercalate "-" . T.words . filterPunct . T.toLower
//!   -- gfm:      filterPunct . spaceToDash . T.toLower
//! filterPunct = T.filter (\c -> isSpace c || isAlphaNum c || isAllowedPunct c)
//!   -- default allows _ - .   gfm allows - _ and Mn Mc Me Pc
//! dropNonLetter = T.dropWhile (not . isAlpha)   -- not with gfm
//! ```
//!
//! The text is pandoc's `stringify`: text, code, image descriptions, line
//! breaks as spaces, raw HTML dropped.

use unicode_normalization::UnicodeNormalization;

use super::PandocIds;
use super::chars::{
    is_connector, is_haskell_space, is_letter, is_mark, is_number, lower_no_final_sigma,
};

/// The pandoc base identifier.
pub(super) fn slug(text: &str, ids: PandocIds) -> String {
    let lower = lower_no_final_sigma(text);
    let alnum = |c: char| is_letter(c) || is_number(c);
    let ident = if ids.gfm {
        lower
            .chars()
            .map(|c| if is_haskell_space(c) { '-' } else { c })
            .filter(|&c| alnum(c) || c == '-' || c == '_' || is_mark(c) || is_connector(c))
            .collect::<String>()
    } else {
        let kept: String = lower
            .chars()
            .filter(|&c| is_haskell_space(c) || alnum(c) || matches!(c, '_' | '-' | '.'))
            .collect();
        let mut out = String::with_capacity(kept.len());
        for word in kept.split(is_haskell_space).filter(|w| !w.is_empty()) {
            if !out.is_empty() {
                out.push('-');
            }
            out.push_str(word);
        }
        out
    };
    let ident = if ids.ascii { to_ascii(&ident) } else { ident };
    if ids.gfm {
        ident
    } else {
        ident.trim_start_matches(|c: char| !is_letter(c)).to_owned()
    }
}

/// `ascii_identifiers`: accents stripped (NFD, combining marks dropped),
/// other non-ASCII characters dropped. Pandoc uses its own transliteration
/// table, so a few letters (`ø`, `đ`) differ.
fn to_ascii(s: &str) -> String {
    s.nfd().filter(char::is_ascii).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> String {
        slug(s, PandocIds::default())
    }

    #[test]
    fn manual_examples() {
        // The table in pandoc's MANUAL.txt, "Extension: auto_identifiers"
        // (headings as pandoc's Markdown reader stringifies them).
        assert_eq!(
            d("Heading identifiers in HTML"),
            "heading-identifiers-in-html"
        );
        assert_eq!(d("Maître d'hôtel"), "maître-dhôtel");
        assert_eq!(d("Dogs?--in my house?"), "dogs--in-my-house");
        assert_eq!(d("[HTML], [S5], or [RTF]?"), "html-s5-or-rtf");
        assert_eq!(d("3. Applications"), "applications");
        assert_eq!(d("33"), "");
    }

    #[test]
    fn variants() {
        assert_eq!(d("Spaces   between"), "spaces-between");
        assert_eq!(d("v1.2 notes"), "v1.2-notes");
        let gfm = PandocIds {
            gfm: true,
            ascii: false,
        };
        assert_eq!(slug("3. Spaces  here", gfm), "3-spaces--here");
        let ascii = PandocIds {
            gfm: false,
            ascii: true,
        };
        assert_eq!(slug("Maître d'hôtel", ascii), "maitre-dhotel");
        assert_eq!(slug("Привет world", ascii), "world");
    }
}

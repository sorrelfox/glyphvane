// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! kramdown's `auto_ids` (`lib/kramdown/converter/base.rb`, checked
//! 2026-11-13 at commit 12b3d2e of github.com/gettalong/kramdown):
//!
//! ```ruby
//! def basic_generate_id(str)
//!   gen_id = str.gsub(/^[^a-zA-Z]+/, '')
//!   gen_id.tr!('^a-zA-Z0-9 -', '')
//!   gen_id.tr!(' ', '-')
//!   gen_id.downcase!
//!   gen_id
//! end
//! ```
//!
//! The input is the heading's raw text (kramdown's `raw_text`, Markdown
//! syntax included) unless `auto_id_stripping` is set. An empty id becomes
//! `section`; duplicates get a per-id counter (`x-1`, `x-2`) that does not
//! check ids taken otherwise.

/// The kramdown base id.
pub(super) fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // `^` matches at the start of every line in Ruby; the line endings
    // themselves are not in the kept set.
    for line in text.split('\n') {
        let rest = line.trim_start_matches(|c: char| !c.is_ascii_alphabetic());
        for c in rest.chars() {
            match c {
                'a'..='z' | '0'..='9' | '-' => out.push(c),
                'A'..='Z' => out.push(c.to_ascii_lowercase()),
                ' ' => out.push('-'),
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::slug;
    use crate::slug::{SlugAlgorithm, Slugger};

    #[test]
    fn test_suite() {
        // test/testcases/block/04_header/with_auto_ids.{text,html} of
        // kramdown (MIT; copyright Thomas Leitner). The transliterated case
        // is left out (Unidecode is not supported).
        let mut s = Slugger::new(SlugAlgorithm::Kramdown);
        let ids: Vec<String> = [
            "This is a header",
            "12. Another one-1-here",
            "Do ^& it now",
            "Hallo",
            "Not now",
            "Hallo",
            "23232",
            "33333",
            "hallO",
        ]
        .iter()
        .map(|t| s.slug(t))
        .collect();
        assert_eq!(
            ids,
            [
                "this-is-a-header",
                "another-one-1-here",
                "do--it-now",
                "hallo",
                "not-now",
                "hallo-1",
                "section",
                "section-1",
                "hallo-2"
            ]
        );
    }

    #[test]
    fn raw_text() {
        assert_eq!(
            slug("[Link](http://example.com) *x*"),
            "linkhttpexamplecom-x"
        );
        assert_eq!(slug("Café au lait"), "caf-au-lait");
        assert_eq!(slug("a\n1b"), "ab");
    }
}

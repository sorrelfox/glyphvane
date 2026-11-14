// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Python-Markdown's `toc` extension (`markdown/extensions/toc.py`,
//! checked 2026-11-13 at commit a4d4428 of
//! github.com/Python-Markdown/markdown), the MkDocs default:
//!
//! ```python
//! def slugify(value, separator, unicode=False):
//!     if not unicode:
//!         value = unicodedata.normalize('NFKD', value)
//!         value = value.encode('ascii', 'ignore').decode('ascii')
//!     value = re.sub(r'[^\w\s-]', '', value).strip().lower()
//!     return re.sub(r'[{}\s]+'.format(separator), separator, value)
//! ```
//!
//! The text is the heading's rendered HTML with tags stripped and
//! whitespace collapsed, then unescaped: the rendered text without image
//! descriptions. Duplicates get `_1`, `_2` (see `unique`).

use unicode_normalization::UnicodeNormalization;

use super::chars::{is_python_space, is_python_word};

/// The Python-Markdown base slug with the default separator `-`.
pub(super) fn slug(text: &str, unicode: bool) -> String {
    let folded: String;
    let value = if unicode {
        text
    } else {
        folded = text.nfkd().filter(char::is_ascii).collect();
        &folded
    };
    let kept: String = value
        .chars()
        .filter(|&c| is_python_word(c) || is_python_space(c) || c == '-')
        .collect();
    let lower = kept.trim_matches(is_python_space).to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut in_run = false;
    for c in lower.chars() {
        if c == '-' || is_python_space(c) {
            if !in_run {
                out.push('-');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

/// `IDCOUNT_RE = re.compile(r'^(.*)_([0-9]+)$')`: an id ending in `_N`
/// splits into its base and `N`; any other id is its own base with count
/// 0. A count too large for `usize` counts as no match.
pub(super) fn split_idcount(id: &str) -> (&str, usize) {
    if let Some(pos) = id.rfind('_') {
        let digits = &id[pos + 1..];
        if !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = digits.parse::<usize>()
        {
            return (&id[..pos], n);
        }
    }
    (id, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slug::{SlugAlgorithm, Slugger};

    #[test]
    fn toc_test_suite() {
        // tests/test_syntax/extensions/test_toc.py of Python-Markdown
        // (BSD-3-Clause; copyright The Python Markdown Project).
        assert_eq!(slug("Foo & bar", false), "foo-bar"); // testHtmlEntitiesInTOC
        assert_eq!(slug("Foo > & bar", false), "foo-bar"); // testHtmlSpecialCharsInTOC
        assert_eq!(slug("Foo Bar Baz.", false), "foo-bar-baz"); // testRawHtmlInTOC
        assert_eq!(
            slug("Some Header with markup.", false),
            "some-header-with-markup"
        ); // testHeaderInlineMarkup
        assert_eq!(slug("<test>", false), "test"); // test_escaped_code
        assert_eq!(slug("escaped_character", false), "escaped_character");
        assert_eq!(slug("foo@example.org", false), "fooexampleorg"); // testAutoLinkEmail
        assert_eq!(slug("Unicode ヘッダー", true), "unicode-ヘッダー"); // testPermalinkWithUnicodeInID
        assert_eq!(slug("Unicode ヘッダー", false), "unicode");
        assert_eq!(slug("Théâtre", false), "theatre"); // testPermalinkWithExtendedLatinInID
    }

    #[test]
    fn unique_test_suite() {
        let alg = SlugAlgorithm::PythonMarkdown { unicode: false };
        // testUniqueIds
        let mut s = Slugger::new(alg);
        let ids: Vec<String> = (0..3).map(|_| s.slug("Header")).collect();
        assert_eq!(ids, ["header", "header_1", "header_2"]);
        // testTOCUniqueFunc
        let mut s = Slugger::new(alg);
        s.reserve("foo");
        assert_eq!(s.unique("foo".into()), "foo_1");
        // testTOCUniqueFuncCounters: same results as without the cache.
        let mut s = Slugger::new(alg);
        for id in ["foo", "foo_1", "foo_3"] {
            s.reserve(id);
        }
        let mut u = |id: &str| s.unique(id.to_owned());
        assert_eq!(u("foo"), "foo_2");
        assert_eq!(u("foo"), "foo_4");
        assert_eq!(u("foo_1"), "foo_5");
        assert_eq!(u("foo_8"), "foo_8");
        assert_eq!(u("foo_8"), "foo_9");
        assert_eq!(u("foo"), "foo_6");
        assert_eq!(u("foo_07"), "foo_07");
        assert_eq!(u("foo_07"), "foo_10");
        assert_eq!(u("foo"), "foo_7");
        assert_eq!(u("foo"), "foo_11");
    }

    #[test]
    fn separators_and_runs() {
        assert_eq!(slug("  a -- b  ", false), "a-b");
        assert_eq!(slug("- a", false), "-a");
        assert_eq!(slug("Spaces   between", false), "spaces-between");
        assert_eq!(slug("Привет мир", false), "");
        assert_eq!(slug("Привет мир", true), "привет-мир");
        // Marks are not `\w` in Python: slugify_unicode drops them.
        assert_eq!(slug("e\u{301}", true), "e");
        assert_eq!(split_idcount("a_b_12"), ("a_b", 12));
        assert_eq!(split_idcount("a_"), ("a_", 0));
    }
}

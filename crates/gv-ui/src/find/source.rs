// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Find and replace over source text (the Source view's matching).
//!
//! - **Matching** follows find in page ([`gv_render_tree::Finder`]): a
//!   literal or a regular expression in Rust `regex` syntax, Unicode case
//!   folding, the whole-word rule, empty matches skipped. It runs over the
//!   whole text, so a regular expression can span lines; offsets are byte
//!   offsets of the text.
//! - **Regular expressions run in CRLF mode** as well as multi-line mode:
//!   `^` and `$` match at LF, CRLF and CR line ends, and `.` matches neither
//!   CR nor LF, so `.*` never takes the CR of a CRLF and a replacement keeps
//!   the line ends of the file.
//! - **Replacement**: literal, or with the regular expression on, capture
//!   groups expanded as `regex` does: `$1`, `${1}`, `$name`, `${name}`, and
//!   `$$` for a dollar sign. `$1a` names the group `1a`; write `${1}a`.

use std::ops::Range;

use gv_render_tree::{FindError, FindOptions, Finder};
use regex::{Regex, RegexBuilder};

/// At most this many matches are kept for the count, the highlights and
/// next and previous; Replace all replaces every match.
pub(crate) const MAX_MATCHES: usize = 10_000;

/// The inline flag of CRLF mode.
const CRLF: &str = "(?R)";

/// The pattern to compile for `query`: a regular expression gets CRLF
/// mode; a literal stays as it is ([`Finder`] escapes it).
fn pattern(query: &str, options: FindOptions) -> String {
    if options.regex {
        format!("{CRLF}{query}")
    } else {
        query.to_owned()
    }
}

/// The finder for `query`, or `None` for an empty query.
pub(crate) fn finder(query: &str, options: FindOptions) -> Result<Option<Finder>, FindError> {
    if query.is_empty() {
        return Ok(None);
    }
    Finder::new(&pattern(query, options), options).map(Some)
}

/// The matches of `finder` in `hay`, at most `limit`, and whether there
/// are more; `None` as soon as `stale` says a newer search replaced this
/// one.
pub(crate) fn find_all(
    finder: &Finder,
    hay: &str,
    limit: usize,
    stale: impl Fn() -> bool,
) -> Option<(Vec<Range<usize>>, bool)> {
    let mut out = Vec::new();
    for m in finder.find_iter(hay) {
        if stale() {
            return None;
        }
        if out.len() >= limit {
            return Some((out, true));
        }
        out.push(m);
    }
    Some((out, false))
}

/// The replacement of a match.
#[derive(Debug, Clone)]
pub(crate) struct Replacer {
    /// The query as a regular expression, when capture groups expand.
    re: Option<Regex>,
    template: String,
}

impl Replacer {
    /// Replaces matches of `query` with `template`.
    pub(crate) fn new(
        query: &str,
        options: FindOptions,
        template: &str,
    ) -> Result<Self, FindError> {
        let re = if options.regex {
            let re = RegexBuilder::new(&pattern(query, options))
                .case_insensitive(!options.case_sensitive)
                .multi_line(true)
                .build()
                .map_err(|err| FindError::Regex(err.to_string()))?;
            Some(re)
        } else {
            None
        };
        Ok(Self {
            re,
            template: template.to_owned(),
        })
    }

    /// The text that replaces the match `m` of `hay`.
    pub(crate) fn replacement(&self, hay: &str, m: Range<usize>) -> String {
        let Some(re) = &self.re else {
            return self.template.clone();
        };
        // The match found from its own start is the same match: the
        // whole-word rule only moves where the search starts.
        match re.captures_at(hay, m.start) {
            Some(caps) if caps.get(0).map(|g| g.range()) == Some(m) => {
                let mut out = String::new();
                caps.expand(&self.template, &mut out);
                out
            }
            _ => self.template.clone(),
        }
    }

    /// One edit per match of `finder` in `hay`, every match.
    pub(crate) fn edits(&self, finder: &Finder, hay: &str) -> Vec<(Range<usize>, String)> {
        finder
            .find_iter(hay)
            .map(|m| {
                let text = self.replacement(hay, m.clone());
                (m, text)
            })
            .collect()
    }
}

/// The first match at or after `offset`, else the first one (wrapping);
/// `None` when there are none.
pub(crate) fn at_or_after(matches: &[Range<usize>], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let k = matches.partition_point(|m| m.start < offset);
    Some(if k < matches.len() { k } else { 0 })
}

/// The last match that ends at or before `offset`, else the last one
/// (wrapping); `None` when there are none.
pub(crate) fn before(matches: &[Range<usize>], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let k = matches.partition_point(|m| m.end <= offset);
    Some(if k > 0 { k - 1 } else { matches.len() - 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(case_sensitive: bool, whole_word: bool, regex: bool) -> FindOptions {
        FindOptions {
            case_sensitive,
            whole_word,
            regex,
        }
    }

    fn found(query: &str, o: FindOptions, hay: &str) -> Vec<Range<usize>> {
        let f = finder(query, o).expect("compiles").expect("a query");
        find_all(&f, hay, MAX_MATCHES, || false).expect("current").0
    }

    fn replaced(query: &str, o: FindOptions, with: &str, hay: &str) -> String {
        let f = finder(query, o).expect("compiles").expect("a query");
        let r = Replacer::new(query, o, with).expect("compiles");
        let mut out = hay.to_owned();
        for (range, text) in r.edits(&f, hay).into_iter().rev() {
            out.replace_range(range, &text);
        }
        out
    }

    #[test]
    fn an_empty_query_has_no_finder() {
        assert!(finder("", FindOptions::default()).expect("ok").is_none());
        assert!(finder("(", opts(false, false, true)).is_err());
    }

    #[test]
    fn matches_span_lines_with_a_regex() {
        let o = opts(false, false, true);
        assert_eq!(found(r"a\nb", o, "a\nb a\nb"), [0..3, 4..7]);
    }

    #[test]
    fn crlf_lines_keep_their_cr() {
        let o = opts(false, false, true);
        // `$` matches before the CRLF, `.` never takes the CR.
        assert_eq!(found("o$", o, "foo\r\nbar\r\n"), vec![2..3; 1]);
        assert_eq!(found("b.*", o, "foo\r\nbar\r\n"), vec![5..8; 1]);
        assert_eq!(replaced(".+", o, "x", "ab\r\ncd\r\n"), "x\r\nx\r\n");
    }

    #[test]
    fn the_cap_reports_more() {
        let f = finder("a", FindOptions::default())
            .expect("compiles")
            .expect("a query");
        let (m, more) = find_all(&f, "aaaa", 3, || false).expect("current");
        assert_eq!((m.len(), more), (3, true));
        let (m, more) = find_all(&f, "aaa", 3, || false).expect("current");
        assert_eq!((m.len(), more), (3, false));
        assert!(find_all(&f, "aaa", 3, || true).is_none());
    }

    #[test]
    fn capture_groups_expand_with_the_regex_on() {
        let o = opts(false, false, true);
        assert_eq!(
            replaced(r"(\w+)@(\w+)", o, "$2 at $1", "me@home, you@work"),
            "home at me, work at you"
        );
        assert_eq!(
            replaced(r"(?P<word>\d+)", o, "<${word}>", "a1 b22"),
            "a<1> b<22>"
        );
        assert_eq!(replaced("(a)", o, "${1}b $$", "a"), "ab $");
        // A literal replacement takes `$` as it is.
        assert_eq!(
            replaced("a", opts(false, false, false), "$1", "a a"),
            "$1 $1"
        );
    }

    #[test]
    fn replacing_follows_the_whole_word_and_case_rules() {
        assert_eq!(
            replaced("cat", opts(false, true, false), "dog", "cat Cat cats"),
            "dog dog cats"
        );
        assert_eq!(
            replaced("cat", opts(true, false, false), "dog", "cat Cat cats"),
            "dog Cat dogs"
        );
        // The whole-word rule with groups: the expansion is of the match
        // that was found.
        assert_eq!(
            replaced("(a)a", opts(false, true, true), "<$1>", "aaa aa"),
            "aaa <a>"
        );
    }

    #[test]
    fn neighbours_wrap_around() {
        let m = [2..4, 6..8, 10..12];
        assert_eq!(at_or_after(&m, 0), Some(0));
        assert_eq!(at_or_after(&m, 4), Some(1));
        assert_eq!(at_or_after(&m, 11), Some(0));
        assert_eq!(before(&m, 12), Some(2));
        assert_eq!(before(&m, 6), Some(0));
        assert_eq!(before(&m, 2), Some(2));
        assert_eq!(at_or_after(&[], 0), None);
        assert_eq!(before(&[], 0), None);
    }
}

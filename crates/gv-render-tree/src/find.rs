// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Find in page over the plain-text projection.
//!
//! - [`Finder`]: a compiled query. The needle is a literal or a regular
//!   expression (Rust `regex` syntax); case-insensitive matching uses
//!   Unicode simple case folding. Regular expressions run in multi-line
//!   mode, so `^` and `$` match at the line breaks inside an item (hard
//!   breaks, code lines).
//! - **Whole word**: a match that starts with a word character must not
//!   follow one, and a match that ends with a word character must not be
//!   followed by one. Word characters are letters, digits (Unicode
//!   alphanumerics) and `_`. A rejected match is tried again one character
//!   later, so `aa` finds the second word of `aaa aa`.
//! - Empty matches (`^`, `a*` on `b`) are skipped: there is nothing to
//!   highlight.
//! - **Matches stay inside one item.** A paragraph, a heading and a list
//!   item's paragraph are one item each, so a phrase across a soft line
//!   break (a space in the text) is found; a code block is one item per 50
//!   lines, so a multi-line regular expression does not match across those
//!   chunk boundaries.
//! - [`FindIndex`]: the text of every item, built once per tree, so that
//!   each keystroke of an incremental search only runs the matcher.

use std::fmt;
use std::ops::Range;

use regex::{Regex, RegexBuilder};

use crate::text::{FindMatch, FindOptions, ItemText};
use crate::tree::RenderTree;

/// Why a query cannot be searched for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindError {
    /// The regular expression does not parse, or compiles too big; the
    /// message says why.
    Regex(String),
}

impl fmt::Display for FindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Regex(why) => write!(f, "invalid regular expression: {why}"),
        }
    }
}

impl std::error::Error for FindError {}

/// A compiled find query.
#[derive(Debug, Clone)]
pub struct Finder {
    /// `None` for an empty needle, which matches nothing.
    re: Option<Regex>,
    whole_word: bool,
}

impl Finder {
    /// Compiles `needle` with `opts`. An empty needle matches nothing.
    pub fn new(needle: &str, opts: FindOptions) -> Result<Self, FindError> {
        if needle.is_empty() {
            return Ok(Self {
                re: None,
                whole_word: opts.whole_word,
            });
        }
        let pattern = if opts.regex {
            std::borrow::Cow::Borrowed(needle)
        } else {
            std::borrow::Cow::Owned(regex::escape(needle))
        };
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(!opts.case_sensitive)
            .multi_line(true)
            .build()
            .map_err(|err| FindError::Regex(err.to_string()))?;
        Ok(Self {
            re: Some(re),
            whole_word: opts.whole_word,
        })
    }

    /// Whether the query matches nothing (an empty needle).
    pub fn is_empty(&self) -> bool {
        self.re.is_none()
    }

    /// The matches in `hay`, in order, not overlapping.
    pub fn find_iter<'f, 'h>(&'f self, hay: &'h str) -> Matches<'f, 'h> {
        Matches {
            finder: self,
            hay,
            at: 0,
        }
    }
}

/// The iterator of [`Finder::find_iter`].
#[derive(Debug)]
pub struct Matches<'f, 'h> {
    finder: &'f Finder,
    hay: &'h str,
    at: usize,
}

impl Iterator for Matches<'_, '_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let re = self.finder.re.as_ref()?;
        while self.at <= self.hay.len() {
            let m = re.find_at(self.hay, self.at)?;
            let r = m.range();
            if r.is_empty() || (self.finder.whole_word && !is_whole_word(self.hay, &r)) {
                self.at = next_char(self.hay, r.start);
                continue;
            }
            self.at = r.end;
            return Some(r);
        }
        None
    }
}

/// The offset after the character at `i` (past the end at the end).
fn next_char(s: &str, i: usize) -> usize {
    s.get(i..)
        .and_then(|rest| rest.chars().next())
        .map_or(s.len() + 1, |c| i + c.len_utf8())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether the match `r` of `hay` is not part of a longer word.
fn is_whole_word(hay: &str, r: &Range<usize>) -> bool {
    let m = &hay[r.clone()];
    let joined_before = m.chars().next().is_some_and(is_word)
        && hay[..r.start].chars().next_back().is_some_and(is_word);
    let joined_after = m.chars().next_back().is_some_and(is_word)
        && hay[r.end..].chars().next().is_some_and(is_word);
    !joined_before && !joined_after
}

/// Appends the matches of `finder` in the items `items` to `out`, in
/// document order, until `out` holds `limit` matches. Returns whether there
/// are more matches past the limit in these items.
fn collect<'t>(
    finder: &Finder,
    items: Range<usize>,
    limit: usize,
    out: &mut Vec<FindMatch>,
    mut text: impl FnMut(usize) -> std::borrow::Cow<'t, ItemText>,
) -> bool {
    if finder.is_empty() {
        return false;
    }
    for ix in items {
        let t = text(ix);
        for r in finder.find_iter(&t.text) {
            if out.len() >= limit {
                return true;
            }
            out.push(FindMatch {
                item: ix,
                source: t.source_range(r.clone()),
                text: r,
            });
        }
    }
    false
}

/// The plain text of every item of a tree, with the mapping to source
/// offsets: the find index. Build it once per tree revision with
/// [`RenderTree::find_index`]; it does not borrow the tree, so a search can
/// run on another thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindIndex {
    items: Vec<ItemText>,
}

impl FindIndex {
    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The text of item `ix`.
    pub fn item(&self, ix: usize) -> Option<&ItemText> {
        self.items.get(ix)
    }

    /// Appends the matches of `finder` in the items `items` to `out`, in
    /// document order, until `out` holds `limit` matches. Returns whether
    /// these items hold more matches past the limit.
    pub fn find_in(
        &self,
        finder: &Finder,
        items: Range<usize>,
        limit: usize,
        out: &mut Vec<FindMatch>,
    ) -> bool {
        let end = items.end.min(self.items.len());
        let start = items.start.min(end);
        collect(finder, start..end, limit, out, |ix| {
            std::borrow::Cow::Borrowed(&self.items[ix])
        })
    }

    /// Every match of `finder`, at most `limit`; and whether there are
    /// more.
    pub fn find(&self, finder: &Finder, limit: usize) -> (Vec<FindMatch>, bool) {
        let mut out = Vec::new();
        let more = self.find_in(finder, 0..self.items.len(), limit, &mut out);
        (out, more)
    }
}

impl RenderTree {
    /// The find index of the tree: every item's text.
    pub fn find_index(&self) -> FindIndex {
        FindIndex {
            items: (0..self.items().len())
                .map(|ix| self.item_text(ix))
                .collect(),
        }
    }

    /// The matches of `finder` in items `items`, appended to `out` until it
    /// holds `limit`; whether there are more. Projects each item's text on
    /// the way (see [`FindIndex`] for repeated searches).
    pub fn find_with(
        &self,
        finder: &Finder,
        items: Range<usize>,
        limit: usize,
        out: &mut Vec<FindMatch>,
    ) -> bool {
        let end = items.end.min(self.items().len());
        let start = items.start.min(end);
        collect(finder, start..end, limit, out, |ix| {
            std::borrow::Cow::Owned(self.item_text(ix))
        })
    }
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

    fn all(needle: &str, o: FindOptions, hay: &str) -> Vec<String> {
        Finder::new(needle, o)
            .expect("compiles")
            .find_iter(hay)
            .map(|r| hay[r].to_owned())
            .collect()
    }

    #[test]
    fn literals_are_not_patterns() {
        let o = opts(false, false, false);
        assert_eq!(all("a.c", o, "abc a.c"), ["a.c"]);
        assert_eq!(all("(x)", o, "(x) x"), ["(x)"]);
    }

    #[test]
    fn case_folds_unicode() {
        let o = opts(false, false, false);
        assert_eq!(all("straße", o, "STRASSE Straße"), ["Straße"]);
        assert_eq!(all("émile", o, "Émile"), ["Émile"]);
        assert_eq!(
            all("émile", opts(true, false, false), "Émile"),
            [] as [&str; 0]
        );
    }

    #[test]
    fn whole_word_checks_both_edges() {
        let o = opts(false, true, false);
        assert_eq!(all("the", o, "the other bathe The"), ["the", "The"]);
        // A rejected match is retried one character later.
        assert_eq!(all("aa", o, "aaa aa"), ["aa"]);
        // Edges that are not word characters need no boundary.
        assert_eq!(all(".x", o, "a.x"), [".x"]);
        assert_eq!(all("_id", o, "user_id _id"), ["_id"]);
        assert_eq!(all("café", o, "cafés café"), ["café"]);
    }

    #[test]
    fn regex_syntax_and_lines() {
        let o = opts(false, false, true);
        assert_eq!(all(r"\d+", o, "a1 b22"), ["1", "22"]);
        assert_eq!(all("^b", o, "ab\nbc"), ["b"]);
        assert_eq!(all("colou?r", o, "Color colour"), ["Color", "colour"]);
        // Empty matches are skipped.
        assert_eq!(all("x*", o, "ab"), [] as [&str; 0]);
        assert_eq!(all("^", o, "ab\ncd"), [] as [&str; 0]);
        // Whole word applies to regex matches too.
        assert_eq!(all("t[a-z]+", opts(false, true, true), "at tea"), ["tea"]);
    }

    #[test]
    fn invalid_regex_is_an_error() {
        let err = Finder::new("(", opts(false, false, true)).expect_err("does not parse");
        assert!(matches!(err, FindError::Regex(_)));
        assert!(err.to_string().starts_with("invalid regular expression"));
        // The same needle is fine as a literal.
        assert!(Finder::new("(", opts(false, false, false)).is_ok());
    }

    #[test]
    fn empty_needle_matches_nothing() {
        let f = Finder::new("", FindOptions::default()).expect("compiles");
        assert!(f.is_empty());
        assert_eq!(f.find_iter("abc").count(), 0);
    }
}

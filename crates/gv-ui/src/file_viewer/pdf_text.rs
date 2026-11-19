// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The text of the PDF viewer's pages, as the engine read it
//! ([`PdfPageText`]): matching a query across pages (a match can run from
//! one page on to the next, and over a hyphen at a line end), the boxes to
//! highlight for a run of characters, the character nearest a point, the
//! word and line around a character, and a selection from one page to
//! another ([`selection`]). Positions are characters (`char`) of a page's
//! text; coordinates are PDF points from the page's top left.

use std::cmp::Reverse;
use std::ops::Range;
use std::sync::Arc;

use gv_render_tree::Finder;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::export::PdfPageText;

/// The text of one page, ready for matching and hit testing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PageText {
    text: String,
    /// One box per character: left, top, right, bottom.
    boxes: Vec<[f32; 4]>,
    /// The byte offset of each character, and the text's length last.
    starts: Vec<usize>,
}

impl PageText {
    /// `page`, with characters beyond the boxes given dropped (and boxes
    /// beyond the characters), and boxes that are not numbers emptied.
    pub fn new(page: PdfPageText) -> Self {
        let mut starts: Vec<usize> = page.text.char_indices().map(|(i, _)| i).collect();
        let n = starts.len().min(page.boxes.len());
        let mut text = page.text;
        text.truncate(starts.get(n).copied().unwrap_or(text.len()));
        starts.truncate(n);
        starts.push(text.len());
        let boxes = page.boxes[..n]
            .iter()
            .map(|b| {
                if b.iter().all(|v| v.is_finite()) {
                    [
                        b[0].min(b[2]),
                        b[1].min(b[3]),
                        b[0].max(b[2]),
                        b[1].max(b[3]),
                    ]
                } else {
                    [0.0; 4]
                }
            })
            .collect();
        Self {
            text,
            boxes,
            starts,
        }
    }

    /// The number of characters.
    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The text of characters `chars`.
    pub fn slice(&self, chars: Range<usize>) -> &str {
        let end = chars.end.min(self.len());
        let start = chars.start.min(end);
        &self.text[self.starts[start]..self.starts[end]]
    }

    /// The boxes covering characters `chars`, one per line: the boxes of
    /// neighbouring characters on the same line are joined. Line ends and
    /// boxes without area add nothing.
    pub fn rects(&self, chars: Range<usize>) -> Vec<[f32; 4]> {
        let end = chars.end.min(self.len());
        let mut out: Vec<[f32; 4]> = Vec::new();
        for i in chars.start.min(end)..end {
            let b = self.boxes[i];
            if b[2] - b[0] <= 0.0 || b[3] - b[1] <= 0.0 {
                continue;
            }
            if let Some(last) = out.last_mut() {
                let overlap = last[3].min(b[3]) - last[1].max(b[1]);
                let height = (last[3] - last[1]).min(b[3] - b[1]);
                if overlap > height * 0.5 {
                    *last = [
                        last[0].min(b[0]),
                        last[1].min(b[1]),
                        last[2].max(b[2]),
                        last[3].max(b[3]),
                    ];
                    continue;
                }
            }
            out.push(b);
        }
        out
    }

    /// The character nearest the point (`x`, `y`) and its box: the one
    /// whose box holds the point, else the nearest (a point beside a line
    /// is nearer that line than the next). Line ends and characters
    /// without width are never it. `None` for a page without text.
    fn nearest(&self, x: f32, y: f32) -> Option<(usize, [f32; 4])> {
        let distance = |b: &[f32; 4]| {
            let dx = (b[0] - x).max(x - b[2]).max(0.0);
            let dy = (b[1] - y).max(y - b[3]).max(0.0);
            dy * 4.0 + dx
        };
        self.boxes
            .iter()
            .enumerate()
            .filter(|(i, b)| b[2] > b[0] && !self.slice(*i..*i + 1).starts_with('\n'))
            .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
            .map(|(i, b)| (i, *b))
    }

    /// The caret nearest the point (`x`, `y`): before the character whose
    /// box holds the point or is the nearest, or after it when the point
    /// is in its right half. `None` for a page without text.
    pub fn caret_at(&self, x: f32, y: f32) -> Option<usize> {
        let (i, b) = self.nearest(x, y)?;
        Some(if x > (b[0] + b[2]) / 2.0 { i + 1 } else { i })
    }

    /// The character nearest the point (`x`, `y`), as [`Self::caret_at`]
    /// finds it (a double or triple click).
    pub fn char_near(&self, x: f32, y: f32) -> Option<usize> {
        self.nearest(x, y).map(|(i, _)| i)
    }

    /// The line of character `at`: from the character after the previous
    /// line end to the next line end (not included).
    pub fn line_at(&self, at: usize) -> Range<usize> {
        let Some(&byte) = self.starts.get(at.min(self.len())) else {
            return 0..0;
        };
        let start = self.text[..byte].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[byte..]
            .find('\n')
            .map_or(self.text.len(), |i| byte + i);
        self.char_of(start)..self.char_of(end)
    }

    /// The word, run of punctuation or run of spaces holding character
    /// `at` (Unicode word boundaries within its line, as a double click in
    /// the Source view finds it).
    pub fn word_at(&self, at: usize) -> Range<usize> {
        let line = self.line_at(at);
        if at >= line.end {
            return at.min(self.len())..at.min(self.len());
        }
        let from = self.starts[line.start];
        let byte = self.starts[at];
        let content = &self.text[from..self.starts[line.end]];
        content
            .split_word_bound_indices()
            .map(|(i, w)| from + i..from + i + w.len())
            .find(|r| r.contains(&byte))
            .map_or(at..at + 1, |r| self.char_of(r.start)..self.char_of(r.end))
    }

    /// The character starting at byte offset `byte` (or the text's
    /// length at its end).
    fn char_of(&self, byte: usize) -> usize {
        self.starts.partition_point(|s| *s < byte)
    }

    /// The characters an end of a selection at `spot` holds: the caret
    /// there (an empty range) for characters, the word or line of the
    /// character there for words and lines.
    pub fn span(&self, spot: Spot, unit: Unit) -> Range<usize> {
        let (x, y) = match spot {
            Spot::Start => return 0..0,
            Spot::End => return self.len()..self.len(),
            Spot::At(x, y) => (x, y),
        };
        match unit {
            Unit::Char => self.caret_at(x, y).map_or(0..0, |c| c..c),
            Unit::Word => self.char_near(x, y).map_or(0..0, |c| self.word_at(c)),
            Unit::Line => self.char_near(x, y).map_or(0..0, |c| self.line_at(c)),
        }
    }
}

/// What a selection grows by: what the click that started it took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Unit {
    /// A press: characters.
    #[default]
    Char,
    /// A double click: words.
    Word,
    /// A triple click: lines.
    Line,
}

/// Where on its page an end of a selection is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Spot {
    /// Before the page's first character.
    Start,
    /// At the character nearest a point (points from the page's top left).
    At(f32, f32),
    /// After the page's last character.
    End,
}

/// One end of a selection: a page and where on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Mark {
    pub page: usize,
    pub spot: Spot,
}

/// The characters selected from `anchor` to `head` (either way round),
/// grown to whole words or lines by `unit`, as runs of characters page
/// by page in page order: on one page, from the earlier end to the later
/// one; across pages, from the first end to the end of its page, every
/// page between whole, and the last page from its start to the last end.
/// Pages whose text is not read yet (`None`) and empty runs are left out.
pub(crate) fn selection(
    pages: &[Option<Arc<PageText>>],
    anchor: Mark,
    head: Mark,
    unit: Unit,
) -> Vec<(usize, Range<usize>)> {
    let (first, last) = if head.page < anchor.page {
        (head, anchor)
    } else {
        (anchor, head)
    };
    let text = |page: usize| pages.get(page).and_then(Option::as_ref);
    let mut out = Vec::new();
    if first.page == last.page {
        if let Some(text) = text(first.page) {
            let a = text.span(first.spot, unit);
            let b = text.span(last.spot, unit);
            out.push((first.page, a.start.min(b.start)..a.end.max(b.end)));
        }
    } else {
        for page in first.page..=last.page.min(pages.len().saturating_sub(1)) {
            let Some(text) = text(page) else {
                continue;
            };
            let start = if page == first.page {
                text.span(first.spot, unit).start
            } else {
                0
            };
            let end = if page == last.page {
                text.span(last.spot, unit).end
            } else {
                text.len()
            };
            out.push((page, start..end.max(start)));
        }
    }
    out.retain(|(_, r)| !r.is_empty());
    out
}

/// The text of `runs` (as [`selection`] gives them), the runs of one page
/// after another separated by a line end. Soft hyphens (U+00AD) are left
/// out, and so is a line end after one, so that a word split at a soft
/// hyphen comes out whole (also across pages); other hyphens and line ends
/// are kept as they are.
pub(crate) fn selection_text(
    pages: &[Option<Arc<PageText>>],
    runs: &[(usize, Range<usize>)],
) -> String {
    let mut out = String::new();
    // The page of the run before, and whether it ended in a soft hyphen.
    let mut last: Option<(usize, bool)> = None;
    for (page, chars) in runs {
        let Some(text) = pages.get(*page).and_then(Option::as_ref) else {
            continue;
        };
        let slice = text.slice(chars.clone());
        match last {
            Some((before, true)) if before + 1 == *page => {}
            Some(_) => out.push('\n'),
            None => {}
        }
        let mut cs = slice.chars().peekable();
        while let Some(c) = cs.next() {
            if c == SOFT_HYPHEN {
                cs.next_if_eq(&'\n');
            } else {
                out.push(c);
            }
        }
        let hyphen = slice.trim_end_matches('\n').ends_with(SOFT_HYPHEN);
        last = Some((*page, hyphen));
    }
    out
}

/// The soft hyphen: where a word may be split at a line end.
const SOFT_HYPHEN: char = '\u{ad}';

/// A find query, and how the pages' text reads for it.
#[derive(Debug, Clone)]
pub(crate) struct Query {
    pub finder: Finder,
    /// A regular expression: line ends (and page ends) stay line ends, for
    /// `^`, `$` and `\n`. Otherwise they read as spaces, so that a phrase
    /// is found across them.
    pub regex: bool,
}

/// A match: characters `chars` of page `page`, and for a match that runs
/// on to the next page, its first `spill` characters (else 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PdfMatch {
    pub page: usize,
    pub chars: Range<usize>,
    pub spill: usize,
}

/// What a hyphen at a line end, after a letter and before a lowercase
/// letter, becomes in the text matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hyphen {
    /// Left out with the line end: "infor-⏎mation" reads "information".
    Drop,
    /// Kept, the line end left out: "well-⏎known" reads "well-known".
    Keep,
}

/// The text a query is matched against: a page's text, then (when it was
/// read) the next page's, the page end reading as a line end between them.
/// Line ends read as spaces unless the query is a regular expression (a
/// line end beside a space is left out); soft hyphens are left out, and so
/// is a line end after one or after a hyphen that follows a letter or a
/// digit; a hyphen at a line end between a letter and a lowercase letter
/// is left out or kept by [`Hyphen`].
struct Hay {
    text: String,
    /// The byte offset of each character of `text`, and its length last.
    starts: Vec<usize>,
    /// Where each character of `text` comes from: whether the next page,
    /// and its character there (the page end: the page's length).
    from: Vec<(bool, usize)>,
    /// Whether a hyphen was left out ([`Hyphen::Drop`]).
    dropped: bool,
}

impl Hay {
    fn new(text: &PageText, next: Option<&PageText>, regex: bool, hyphen: Hyphen) -> Self {
        let mut source: Vec<(char, (bool, usize))> = text
            .text
            .chars()
            .enumerate()
            .map(|(i, c)| (c, (false, i)))
            .collect();
        if let Some(next) = next {
            source.push(('\n', (false, text.len())));
            source.extend(next.text.chars().enumerate().map(|(i, c)| (c, (true, i))));
        }
        let at = |i: Option<usize>| i.and_then(|i| source.get(i)).map(|s| s.0);
        let mut hay = Self {
            text: String::with_capacity(source.len()),
            starts: Vec::with_capacity(source.len() + 1),
            from: Vec::with_capacity(source.len()),
            dropped: false,
        };
        for (i, &(c, from)) in source.iter().enumerate() {
            let before = at(i.checked_sub(1));
            let after = at(Some(i + 1));
            let joins = at(i.checked_sub(2)).is_some_and(char::is_alphanumeric);
            let read = match c {
                SOFT_HYPHEN => None,
                '\n' if before == Some(SOFT_HYPHEN) || (before == Some('-') && joins) => None,
                '\n' if regex => Some('\n'),
                '\n' if before.is_none_or(char::is_whitespace)
                    || after.is_none_or(char::is_whitespace) =>
                {
                    None
                }
                '\n' => Some(' '),
                '-' if hyphen == Hyphen::Drop
                    && before.is_some_and(char::is_alphabetic)
                    && after == Some('\n')
                    && at(Some(i + 2)).is_some_and(char::is_lowercase) =>
                {
                    hay.dropped = true;
                    None
                }
                c => Some(c),
            };
            if let Some(c) = read {
                hay.starts.push(hay.text.len());
                hay.from.push(from);
                hay.text.push(c);
            }
        }
        hay.starts.push(hay.text.len());
        hay
    }

    /// The matches of `finder` that start on the page (of `len`
    /// characters), at most `max`: the characters on the page, and those
    /// on the next page (from its start).
    fn matches(&self, finder: &Finder, len: usize, max: usize) -> Vec<(Range<usize>, usize)> {
        let mut out = Vec::new();
        for r in finder.find_iter(&self.text) {
            if out.len() >= max {
                break;
            }
            let first = self.starts.partition_point(|s| *s < r.start);
            let last = self
                .starts
                .partition_point(|s| *s < r.end)
                .saturating_sub(1)
                .max(first);
            let (Some(&(on_next, start)), Some(&(ends_next, end))) =
                (self.from.get(first), self.from.get(last))
            else {
                break;
            };
            if on_next {
                break;
            }
            if start >= len {
                // At the page end.
                continue;
            }
            out.push(if ends_next {
                (start..len, end + 1)
            } else {
                (start..(end + 1).clamp(start + 1, len), 0)
            });
        }
        out
    }
}

/// The matches of `query` that start on page `page` (its text `text`),
/// at most `max`. With the next page's text (`next`), a match can run on
/// to it, across the page end as across a line end.
pub(crate) fn page_matches(
    page: usize,
    text: &PageText,
    next: Option<&PageText>,
    query: &Query,
    max: usize,
) -> Vec<PdfMatch> {
    let hay = Hay::new(text, next, query.regex, Hyphen::Drop);
    let mut found = hay.matches(&query.finder, text.len(), max);
    if hay.dropped {
        // Both readings of a line-end hyphen: the matches of both, in
        // order, one that overlaps an earlier or longer one left out.
        let kept = Hay::new(text, next, query.regex, Hyphen::Keep);
        found.extend(kept.matches(&query.finder, text.len(), max));
        found.sort_by_key(|(chars, spill)| (chars.start, Reverse((*spill, chars.end))));
        let mut end = 0;
        found.retain(|(chars, spill)| {
            let keep = chars.start >= end;
            if keep {
                end = if *spill > 0 { usize::MAX } else { chars.end };
            }
            keep
        });
        found.truncate(max);
    }
    found
        .into_iter()
        .map(|(chars, spill)| PdfMatch { page, chars, spill })
        .collect()
}

/// Leaves out of `matches` (in page order) those that overlap a match
/// running on from the page before.
fn drop_overlaps(matches: &mut Vec<PdfMatch>) {
    let mut reach: Option<(usize, usize)> = None;
    matches.retain(|m| {
        if reach.is_some_and(|(page, end)| m.page == page && m.chars.start < end) {
            return false;
        }
        if m.spill > 0 {
            reach = Some((m.page + 1, m.spill));
        }
        true
    });
}

/// The matches of `query` in the pages read so far, in page order, at
/// most `max`; and whether there are more. A match can run from a page on
/// to the next one when both were read; it counts once, on the page it
/// starts on.
pub(crate) fn search(
    pages: &[Option<Arc<PageText>>],
    query: &Query,
    max: usize,
) -> (Vec<PdfMatch>, bool) {
    let mut out: Vec<PdfMatch> = Vec::new();
    for (page, text) in pages.iter().enumerate() {
        let Some(text) = text else {
            continue;
        };
        let next = pages.get(page + 1).and_then(Option::as_deref);
        let mut found = page_matches(page, text, next, query, max + 1 - out.len());
        let reach = out
            .last()
            .filter(|m| m.page + 1 == page)
            .map_or(0, |m| m.spill);
        found.retain(|m| m.chars.start >= reach);
        out.extend(found);
        if out.len() > max {
            out.truncate(max);
            return (out, true);
        }
    }
    (out, false)
}

/// Brings `matches` (as [`search`] gives them, at most `max`) up to date
/// now that page `page`'s text came: its matches, and those of the page
/// before, which can now run on to it. Returns whether matches were left
/// out to keep at most `max` (the last ones, in page order).
pub(crate) fn rematch(
    matches: &mut Vec<PdfMatch>,
    pages: &[Option<Arc<PageText>>],
    page: usize,
    query: &Query,
    max: usize,
) -> bool {
    let first = page.saturating_sub(1);
    let from = matches.partition_point(|m| m.page < first);
    let to = matches.partition_point(|m| m.page <= page);
    let mut found = Vec::new();
    for p in first..=page {
        let Some(text) = pages.get(p).and_then(Option::as_deref) else {
            continue;
        };
        let next = pages.get(p + 1).and_then(Option::as_deref);
        found.extend(page_matches(p, text, next, query, max + 1));
    }
    matches.splice(from..to, found);
    drop_overlaps(matches);
    let more = matches.len() > max;
    matches.truncate(max);
    more
}

#[cfg(test)]
mod tests {
    use gv_render_tree::FindOptions;

    use super::*;

    /// A page of `lines`, each character 6 pt wide and 12 pt high, lines
    /// 20 pt apart from 10 pt down; a line end has no area.
    fn page(lines: &[&str]) -> PdfPageText {
        let mut text = String::new();
        let mut boxes = Vec::new();
        for (l, line) in lines.iter().enumerate() {
            let y = 10.0 + 20.0 * l as f32;
            if l > 0 {
                text.push('\n');
                boxes.push([0.0, y, 0.0, y]);
            }
            for (k, c) in line.chars().enumerate() {
                let x = 10.0 + 6.0 * k as f32;
                text.push(c);
                boxes.push([x, y, x + 6.0, y + 12.0]);
            }
        }
        PdfPageText { text, boxes }
    }

    fn query(q: &str) -> Query {
        Query {
            finder: Finder::new(q, FindOptions::default()).expect("compiles"),
            regex: false,
        }
    }

    fn regex(q: &str) -> Query {
        let options = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        Query {
            finder: Finder::new(q, options).expect("compiles"),
            regex: true,
        }
    }

    /// The matches of `q` on one page: their first character and the
    /// one after.
    fn find(p: &PageText, q: &Query) -> Vec<(usize, usize)> {
        page_matches(0, p, None, q, 10)
            .into_iter()
            .map(|m| (m.chars.start, m.chars.end))
            .collect()
    }

    fn read(pages: &[&[&str]]) -> Vec<Option<Arc<PageText>>> {
        pages
            .iter()
            .map(|lines| Some(Arc::new(PageText::new(page(lines)))))
            .collect()
    }

    #[test]
    fn matches_are_characters_and_ignore_case() {
        let p = PageText::new(page(&["Größe and GRÖSSE", "größe"]));
        assert_eq!(find(&p, &query("größe")), [(0, 5), (17, 22)]);
        assert_eq!(p.slice(17..22), "größe");
        // A phrase across a line end, which reads as a space; a regular
        // expression sees the line end.
        let p = PageText::new(page(&["end of", "line"]));
        assert_eq!(find(&p, &query("of line")), [(4, 11)]);
        assert!(find(&p, &query("of\nline")).is_empty());
        assert_eq!(find(&p, &regex("of\\nline")), [(4, 11)]);
        assert_eq!(find(&p, &regex("^line")), [(7, 11)]);
        // A line end beside a space is not a second space.
        let p = PageText::new(page(&["end of ", "line"]));
        assert_eq!(find(&p, &query("of line")), [(4, 12)]);
    }

    #[test]
    fn a_search_goes_through_the_pages_read_and_stops_at_the_cap() {
        let pages = vec![
            Some(Arc::new(PageText::new(page(&["a b a"])))),
            None,
            Some(Arc::new(PageText::new(page(&["a"])))),
        ];
        let (found, more) = search(&pages, &query("a"), 10);
        assert!(!more);
        let at: Vec<(usize, usize)> = found.iter().map(|m| (m.page, m.chars.start)).collect();
        assert_eq!(at, [(0, 0), (0, 4), (2, 0)]);
        let (found, more) = search(&pages, &query("a"), 2);
        assert_eq!((found.len(), more), (2, true));
    }

    #[test]
    fn a_match_runs_on_to_the_next_page() {
        let pages = read(&[&["the end of"], &["page two"], &["three"]]);
        // The page end reads as a space; the match is on page 1 and runs
        // on to page 2, and counts once.
        let (found, _) = search(&pages, &query("of page"), 10);
        assert_eq!(
            found,
            [PdfMatch {
                page: 0,
                chars: 8..10,
                spill: 4
            }]
        );
        let (found, _) = search(&pages, &query("two three"), 10);
        assert_eq!((found[0].page, found[0].spill), (1, 5));
        // Whole words see the page end as a space.
        let options = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        let whole = Query {
            finder: Finder::new("of", options).expect("compiles"),
            regex: false,
        };
        assert_eq!(search(&pages, &whole, 10).0.len(), 1);
        // A regular expression sees a line end.
        assert_eq!(search(&pages, &regex("of\\npage"), 10).0.len(), 1);
        assert_eq!(search(&pages, &regex("^page"), 10).0[0].page, 1);
        // Not across a page whose text is not read.
        let mut pages = pages;
        pages[1] = None;
        assert!(search(&pages, &query("two three"), 10).0.is_empty());
        assert_eq!(search(&pages, &query("of"), 10).0.len(), 1);
    }

    #[test]
    fn a_match_running_on_leaves_out_those_it_overlaps() {
        // "ab ab" from page 1 takes page 2's first "ab", so page 2's own
        // "ab ab" is not counted again.
        let pages = read(&[&["x ab"], &["ab ab"]]);
        let (found, _) = search(&pages, &query("ab ab"), 10);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].page, found[0].spill), (0, 2));
    }

    #[test]
    fn a_page_read_later_adds_matches_that_run_on_to_it() {
        let all = read(&[&["the end of"], &["page and of"], &["page"]]);
        let mut pages = all.clone();
        pages[1] = None;
        let q = query("of page");
        let (mut found, _) = search(&pages, &q, 10);
        assert!(found.is_empty());
        // Page 2 comes: the match from page 1 runs on to it, and its own
        // runs on to page 3.
        pages[1] = all[1].clone();
        assert!(!rematch(&mut found, &pages, 1, &q, 10));
        assert_eq!(found, search(&pages, &q, 10).0);
        let at: Vec<(usize, usize)> = found.iter().map(|m| (m.page, m.spill)).collect();
        assert_eq!(at, [(0, 4), (1, 4)]);
        // Over the cap: the last ones are left out.
        let mut capped = Vec::new();
        assert!(!rematch(&mut capped, &pages, 0, &q, 1));
        assert!(rematch(&mut capped, &pages, 1, &q, 1));
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn a_word_split_at_a_line_end_hyphen_is_found_whole() {
        // A hyphen between a letter and a lowercase letter: found with or
        // without it, once.
        let p = PageText::new(page(&["the infor-", "mation age"]));
        assert_eq!(find(&p, &query("information")), [(4, 17)]);
        assert_eq!(find(&p, &query("infor-mation")), [(4, 17)]);
        assert_eq!(find(&p, &query("infor")), [(4, 9)]);
        assert!(find(&p, &query("infor- mation")).is_empty());
        let options = FindOptions {
            whole_word: true,
            ..FindOptions::default()
        };
        let whole = Query {
            finder: Finder::new("information", options).expect("compiles"),
            regex: false,
        };
        assert_eq!(find(&p, &whole), [(4, 17)]);
        // Before a capital or a digit the hyphen stays.
        let p = PageText::new(page(&["Jean-", "Paul and 1990-", "2000"]));
        assert_eq!(find(&p, &query("Jean-Paul")), [(0, 10)]);
        assert!(find(&p, &query("JeanPaul")).is_empty());
        assert_eq!(find(&p, &query("1990-2000")), [(15, 25)]);
        // A dash after a space is not a hyphen.
        let p = PageText::new(page(&["this -", "that"]));
        assert_eq!(find(&p, &query("- that")), [(5, 11)]);
        // A soft hyphen is never read: at a line end, nor within a line.
        let p = PageText::new(page(&["infor\u{ad}", "mation, re\u{ad}ad"]));
        assert_eq!(find(&p, &query("information")), [(0, 13)]);
        assert_eq!(find(&p, &query("read")), [(15, 20)]);
        // Across pages.
        let pages = read(&[&["infor-"], &["mation"]]);
        let (found, _) = search(&pages, &query("information"), 10);
        assert_eq!(
            found,
            [PdfMatch {
                page: 0,
                chars: 0..6,
                spill: 6
            }]
        );
    }

    #[test]
    fn highlights_join_a_line_and_split_at_line_ends() {
        let p = PageText::new(page(&["ab cd", "ef"]));
        assert_eq!(p.rects(0..2), [[10.0, 10.0, 22.0, 22.0]]);
        assert_eq!(
            p.rects(3..8),
            [[28.0, 10.0, 40.0, 22.0], [10.0, 30.0, 22.0, 42.0]]
        );
    }

    #[test]
    fn the_caret_goes_to_the_nearest_character() {
        let p = PageText::new(page(&["abcd", "efgh"]));
        // In "b"'s left half, then its right half.
        assert_eq!(p.caret_at(17.0, 15.0), Some(1));
        assert_eq!(p.caret_at(21.0, 15.0), Some(2));
        // Past the end of the first line: after "d".
        assert_eq!(p.caret_at(200.0, 15.0), Some(4));
        // Below everything: the last line.
        assert_eq!(p.caret_at(11.0, 300.0), Some(5));
        assert_eq!(PageText::default().caret_at(0.0, 0.0), None);
    }

    #[test]
    fn words_and_lines_around_a_character() {
        let p = PageText::new(page(&["Hello, wide world", "next line"]));
        // A word, a run of punctuation, a run of spaces.
        assert_eq!(p.slice(p.word_at(1)), "Hello");
        assert_eq!(p.slice(p.word_at(5)), ",");
        assert_eq!(p.slice(p.word_at(6)), " ");
        assert_eq!(p.slice(p.word_at(14)), "world");
        // The second line's first word: not across the line end.
        assert_eq!(p.slice(p.word_at(18)), "next");
        // Lines, without their line ends.
        assert_eq!(p.slice(p.line_at(3)), "Hello, wide world");
        assert_eq!(p.slice(p.line_at(20)), "next line");
        // The character under a point: "w" of "wide", whichever half.
        assert_eq!(p.char_near(53.0, 15.0), Some(7));
        assert_eq!(p.char_near(57.0, 15.0), Some(7));
        assert_eq!(PageText::default().char_near(0.0, 0.0), None);
        assert_eq!(PageText::default().line_at(0), 0..0);
    }

    #[test]
    fn a_selection_runs_across_pages_in_order() {
        let pages = vec![
            Some(Arc::new(PageText::new(page(&["one two", "three"])))),
            Some(Arc::new(PageText::new(page(&["middle page"])))),
            None,
            Some(Arc::new(PageText::new(page(&["last page here"])))),
        ];
        // From before "two" on page 1 to after "last" on page 4: the end
        // of page 1, page 2 whole, page 3 (not read) left out, the start of
        // page 4.
        let before_two = Mark {
            page: 0,
            spot: Spot::At(35.0, 15.0),
        };
        let after_last = Mark {
            page: 3,
            spot: Spot::At(33.0, 15.0),
        };
        let runs = selection(&pages, before_two, after_last, Unit::Char);
        assert_eq!(runs, [(0, 4..13), (1, 0..11), (3, 0..4)]);
        assert_eq!(
            selection_text(&pages, &runs),
            "two\nthree\nmiddle page\nlast"
        );
        // Either way round.
        assert_eq!(selection(&pages, after_last, before_two, Unit::Char), runs);
        // By words: both ends grow to their words.
        let in_two = Mark {
            page: 0,
            spot: Spot::At(38.0, 15.0),
        };
        let in_page = Mark {
            page: 1,
            spot: Spot::At(65.0, 15.0),
        };
        let runs = selection(&pages, in_page, in_two, Unit::Word);
        assert_eq!(selection_text(&pages, &runs), "two\nthree\nmiddle page");
        // By lines.
        let runs = selection(&pages, in_two, in_page, Unit::Line);
        assert_eq!(runs, [(0, 0..13), (1, 0..11)]);
        // A page's start and end; an end at a page's very start adds
        // nothing of it.
        let start = |page| Mark {
            page,
            spot: Spot::Start,
        };
        let end = |page| Mark {
            page,
            spot: Spot::End,
        };
        assert_eq!(
            selection(&pages, start(1), end(1), Unit::Char),
            [(1, 0..11)]
        );
        assert!(selection(&pages, end(0), start(1), Unit::Char).is_empty());
        assert_eq!(
            selection(&pages, before_two, start(1), Unit::Char),
            [(0, 4..13)]
        );
    }

    #[test]
    fn copied_text_joins_words_split_at_a_soft_hyphen() {
        let pages = read(&[
            &["infor\u{ad}", "mation, well-", "known"],
            &["con\u{ad}"],
            &["tinued"],
        ]);
        let all = |page: usize| (page, 0..pages[page].as_ref().map_or(0, |t| t.len()));
        // A soft hyphen goes, with the line end after it; a hyphen stays,
        // with its line end.
        assert_eq!(
            selection_text(&pages, &[all(0)]),
            "information, well-\nknown"
        );
        // Across pages too.
        assert_eq!(
            selection_text(&pages, &[all(0), all(1), all(2)]),
            "information, well-\nknown\ncontinued"
        );
    }

    #[test]
    fn text_and_boxes_that_disagree_are_trimmed() {
        let p = PageText::new(PdfPageText {
            text: "abc".into(),
            boxes: vec![[0.0, 0.0, 1.0, 1.0], [f32::NAN, 0.0, 1.0, 1.0]],
        });
        assert_eq!((p.len(), p.text()), (2, "ab"));
        assert!(p.rects(0..2).len() == 1);
    }
}

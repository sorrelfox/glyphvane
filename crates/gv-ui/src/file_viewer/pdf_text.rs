// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The text of the PDF viewer's pages, as the engine read it
//! ([`PdfPageText`]): matching a query across pages, the boxes to
//! highlight for a run of characters, the character nearest a point, the
//! word and line around a character, and a selection from one page to
//! another ([`selection`]). Positions are characters (`char`) of a page's
//! text; coordinates are PDF points from the page's top left.

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

    /// The character at byte offset `byte` (the one it falls in).
    fn char_at(&self, byte: usize) -> usize {
        self.starts[..self.len()]
            .partition_point(|s| *s <= byte)
            .saturating_sub(1)
    }

    /// The matches of `finder`, as character ranges, at most `max`.
    pub fn find(&self, finder: &Finder, max: usize) -> Vec<Range<usize>> {
        finder
            .find_iter(&self.text)
            .take(max)
            .map(|r| {
                let start = self.char_at(r.start);
                let end = if r.end >= self.text.len() {
                    self.len()
                } else {
                    self.char_at(r.end)
                };
                start..end.max(start + 1).min(self.len())
            })
            .collect()
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
/// after another separated by a line end.
pub(crate) fn selection_text(
    pages: &[Option<Arc<PageText>>],
    runs: &[(usize, Range<usize>)],
) -> String {
    let mut out = String::new();
    for (k, (page, chars)) in runs.iter().enumerate() {
        let Some(text) = pages.get(*page).and_then(Option::as_ref) else {
            continue;
        };
        if k > 0 {
            out.push('\n');
        }
        out.push_str(text.slice(chars.clone()));
    }
    out
}

/// A match: characters `chars` of page `page`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PdfMatch {
    pub page: usize,
    pub chars: Range<usize>,
}

/// The matches of `finder` in the pages read so far, in page order, at
/// most `max`; and whether there are more.
pub(crate) fn search(
    pages: &[Option<Arc<PageText>>],
    finder: &Finder,
    max: usize,
) -> (Vec<PdfMatch>, bool) {
    let mut out = Vec::new();
    for (page, text) in pages.iter().enumerate() {
        let Some(text) = text else {
            continue;
        };
        let left = max - out.len();
        let found = text.find(finder, left + 1);
        if found.len() > left {
            out.extend(
                found
                    .into_iter()
                    .take(left)
                    .map(|chars| PdfMatch { page, chars }),
            );
            return (out, true);
        }
        out.extend(found.into_iter().map(|chars| PdfMatch { page, chars }));
    }
    (out, false)
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

    fn finder(q: &str) -> Finder {
        Finder::new(q, FindOptions::default()).expect("compiles")
    }

    #[test]
    fn matches_are_characters_and_ignore_case() {
        let p = PageText::new(page(&["Größe and GRÖSSE", "größe"]));
        let found = p.find(&finder("größe"), 10);
        assert_eq!(found, [0..5, 17..22]);
        assert_eq!(p.slice(17..22), "größe");
        // Across a line end.
        let p = PageText::new(page(&["end of", "line"]));
        let found = p.find(&finder("of\nline"), 10);
        assert_eq!((found.len(), found.first().cloned()), (1, Some(4..11)));
    }

    #[test]
    fn a_search_goes_through_the_pages_read_and_stops_at_the_cap() {
        let pages = vec![
            Some(Arc::new(PageText::new(page(&["a b a"])))),
            None,
            Some(Arc::new(PageText::new(page(&["a"])))),
        ];
        let (found, more) = search(&pages, &finder("a"), 10);
        assert!(!more);
        let at: Vec<(usize, usize)> = found.iter().map(|m| (m.page, m.chars.start)).collect();
        assert_eq!(at, [(0, 0), (0, 4), (2, 0)]);
        let (found, more) = search(&pages, &finder("a"), 2);
        assert_eq!((found.len(), more), (2, true));
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
    fn text_and_boxes_that_disagree_are_trimmed() {
        let p = PageText::new(PdfPageText {
            text: "abc".into(),
            boxes: vec![[0.0, 0.0, 1.0, 1.0], [f32::NAN, 0.0, 1.0, 1.0]],
        });
        assert_eq!((p.len(), p.text()), (2, "ab"));
        assert!(p.rects(0..2).len() == 1);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! One editor row as it is shaped: the display map's chunks of a row of the
//! tab layer turned into display text, and the mapping between the two.
//!
//! The tab layer has already expanded hard tabs (into chunks of spaces with
//! `is_tab` set) and put fold placeholders (`⋯`, [`super::fold`]) in place
//! of folded text, so *columns* here are byte columns of that row. The
//! display text shows C0 control characters as their Unicode control
//! pictures and, with whitespace shown (FR-EDIT-13), draws trailing spaces
//! as `·`, the start of a tab as `→`, no-break spaces as `⍽` and a `↵`
//! after a line that ends in a hard break of two or more spaces. A
//! placeholder is drawn with a space on each side, room for its chip.
//! Display text never changes a line's number of rows.

use std::ops::Range;

use gv_editor::display_map::{Chunk, Replaced, StyleId};

/// The middle dot drawn for a trailing space.
const SPACE_MARK: char = '·';
/// The arrow drawn at the start of a tab.
const TAB_MARK: char = '→';
/// Drawn for U+00A0 NO-BREAK SPACE.
const NBSP_MARK: char = '⍽';
/// Drawn after a hard break made of trailing spaces.
const BREAK_MARK: char = '↵';

/// A run of a row's columns with what its chunks carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub columns: Range<usize>,
    pub style: Option<StyleId>,
    pub replaced: Option<Replaced>,
}

/// An expanded tab: its column in the replace layer's row, and its columns
/// in the tab layer's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tab {
    input: usize,
    start: usize,
    width: usize,
}

/// A row's display text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowText {
    /// The row's text in the tab layer: what columns count.
    pub content: String,
    /// What is shaped.
    pub display: String,
    /// The display offset of every byte of `content`, plus one for its end;
    /// `None` when display and content are the same.
    map: Option<Vec<u32>>,
    /// Display ranges of whitespace marks and control pictures, drawn in
    /// `text.tertiary`.
    pub marks: Vec<Range<usize>>,
    /// The chunks' styles and replacements, in columns, in order and
    /// without gaps.
    pub spans: Vec<Span>,
    /// Columns of the fold placeholders.
    pub placeholders: Vec<Range<usize>>,
    /// Box widgets (Live Preview): the display byte each is drawn over (a
    /// space, spaced out to the width after shaping) and its width in
    /// 1/64 px.
    pub boxes: Vec<(usize, u32)>,
    tabs: Vec<Tab>,
}

/// Whether a chunk's text is drawn as it is (outside whitespace marks).
fn plain(text: &str) -> bool {
    !text.bytes().any(|b| b < 0x20 || b == 0x7f)
}

impl RowText {
    /// The display text of the chunks of one tab-layer row (no `\n`).
    pub fn from_chunks<'a>(
        chunks: impl IntoIterator<Item = Chunk<'a>>,
        show_whitespace: bool,
    ) -> Self {
        let chunks: Vec<Chunk<'a>> = chunks.into_iter().collect();
        let mut content = String::with_capacity(chunks.iter().map(|c| c.text.len()).sum());
        let mut spans: Vec<Span> = Vec::new();
        let mut placeholders = Vec::new();
        let mut tabs = Vec::new();
        let mut input = 0;
        for chunk in &chunks {
            let start = content.len();
            content.push_str(chunk.text);
            let columns = start..content.len();
            if chunk.is_tab {
                tabs.push(Tab {
                    input,
                    start,
                    width: chunk.text.len(),
                });
                input += 1;
            } else {
                input += chunk.text.len();
            }
            if matches!(chunk.replaced, Some(Replaced::Fold(_))) {
                placeholders.push(columns.clone());
            }
            match spans.last_mut() {
                Some(last)
                    if last.style == chunk.style
                        && last.replaced == chunk.replaced
                        && chunk.replaced.is_none() =>
                {
                    last.columns.end = columns.end;
                }
                _ => spans.push(Span {
                    columns,
                    style: chunk.style,
                    replaced: chunk.replaced,
                }),
            }
        }

        // Trailing whitespace (spaces and tabs) and a hard break: two or
        // more spaces typed at the end.
        let mut trailing_from = content.len();
        let mut typed_spaces = 0;
        let mut counting = true;
        for chunk in chunks.iter().rev() {
            if chunk.replaced.is_some() || !chunk.text.bytes().all(|b| b == b' ') {
                let kept = chunk.text.trim_end_matches(' ');
                if chunk.replaced.is_none() {
                    trailing_from -= chunk.text.len() - kept.len();
                    if counting {
                        typed_spaces += chunk.text.len() - kept.len();
                    }
                }
                break;
            }
            trailing_from -= chunk.text.len();
            if chunk.is_tab {
                counting = false;
            } else if counting {
                typed_spaces += chunk.text.len();
            }
        }
        let hard_break = show_whitespace && typed_spaces >= 2;

        let needs_map = !placeholders.is_empty()
            || chunks
                .iter()
                .any(|c| !plain(c.text) || c.box_width.is_some())
            || (show_whitespace
                && (!tabs.is_empty()
                    || trailing_from < content.len()
                    || content.contains('\u{a0}')));
        if !needs_map {
            let mut display = content.clone();
            let mut marks = Vec::new();
            if hard_break {
                let start = display.len();
                display.push(BREAK_MARK);
                marks.push(start..display.len());
            }
            return Self {
                content,
                display,
                map: None,
                marks,
                spans,
                placeholders,
                boxes: Vec::new(),
                tabs,
            };
        }

        let mut display = String::with_capacity(content.len() + 8);
        let mut map = vec![0u32; content.len() + 1];
        let mut marks = Vec::new();
        let mut boxes = Vec::new();
        let mut at = 0;
        for chunk in &chunks {
            let text = chunk.text;
            if let Some(width) = chunk.box_width {
                let start = display.len();
                for slot in &mut map[at..at + text.len()] {
                    *slot = to_u32(start);
                }
                display.push(' ');
                boxes.push((start, width));
                at += text.len();
                continue;
            }
            if matches!(chunk.replaced, Some(Replaced::Fold(_))) {
                let start = display.len();
                for slot in &mut map[at..at + text.len()] {
                    *slot = to_u32(start);
                }
                display.push(' ');
                display.push_str(text);
                display.push(' ');
                at += text.len();
                continue;
            }
            if chunk.is_tab {
                for i in 0..text.len() {
                    let start = display.len();
                    map[at + i] = to_u32(start);
                    if show_whitespace && i == 0 {
                        display.push(TAB_MARK);
                        marks.push(start..display.len());
                    } else {
                        display.push(' ');
                    }
                }
                at += text.len();
                continue;
            }
            for (i, c) in text.char_indices() {
                let start = display.len();
                for slot in &mut map[at + i..at + i + c.len_utf8()] {
                    *slot = to_u32(start);
                }
                let column = at + i;
                match c {
                    ' ' if show_whitespace
                        && column >= trailing_from
                        && chunk.replaced.is_none() =>
                    {
                        display.push(SPACE_MARK);
                        marks.push(start..display.len());
                    }
                    '\u{a0}' if show_whitespace => {
                        display.push(NBSP_MARK);
                        marks.push(start..display.len());
                    }
                    c if (c as u32) < 0x20 => {
                        display.push(char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}'));
                        marks.push(start..display.len());
                    }
                    '\u{7f}' => {
                        display.push('\u{2421}');
                        marks.push(start..display.len());
                    }
                    c => display.push(c),
                }
            }
            at += text.len();
        }
        map[content.len()] = to_u32(display.len());
        if hard_break {
            let start = display.len();
            display.push(BREAK_MARK);
            marks.push(start..display.len());
        }
        Self {
            content,
            display,
            map: Some(map),
            marks,
            spans,
            placeholders,
            boxes,
            tabs,
        }
    }

    /// The display offset of column `column` (clamped).
    pub fn to_display(&self, column: usize) -> usize {
        match &self.map {
            None => column.min(self.content.len()),
            Some(map) => map
                .get(column.min(map.len() - 1))
                .map_or(self.display.len(), |&d| d as usize),
        }
    }

    /// The column of display offset `display`: the start of the character
    /// it falls in.
    pub fn to_column(&self, display: usize) -> usize {
        match &self.map {
            None => display.min(self.content.len()),
            Some(map) => {
                let display = to_u32(display);
                // The last byte whose display offset is at or before
                // `display`, moved back to its character's first byte.
                let k = map.partition_point(|&d| d <= display);
                let mut column = k.saturating_sub(1);
                while column > 0 && map[column - 1] == map[column] {
                    column -= 1;
                }
                column
            }
        }
    }

    /// Display ranges for column ranges.
    pub fn display_range(&self, range: Range<usize>) -> Range<usize> {
        self.to_display(range.start)..self.to_display(range.end)
    }

    /// The display length of the content (without a `↵` mark).
    pub fn content_end(&self) -> usize {
        match &self.map {
            None => self.content.len(),
            Some(map) => map.last().map_or(0, |&d| d as usize),
        }
    }

    /// The column of byte column `input` of the replace layer's row: past
    /// the tabs before it, expanded.
    pub fn column_of_input(&self, input: usize) -> usize {
        let index = self.tabs.partition_point(|tab| tab.input < input);
        match index.checked_sub(1).map(|i| self.tabs[i]) {
            Some(tab) => tab.start + tab.width + (input - tab.input - 1),
            None => input,
        }
        .min(self.content.len())
    }

    /// The start of the expanded tab that has `column` inside, or `column`.
    pub fn tab_start(&self, column: usize) -> usize {
        let index = self.tabs.partition_point(|tab| tab.start < column);
        match index.checked_sub(1).map(|i| self.tabs[i]) {
            Some(tab) if column < tab.start + tab.width => tab.start,
            _ => column,
        }
    }

    /// The replace layer's byte column of column `column`; a column inside
    /// an expanded tab is the tab's.
    pub fn input_of_column(&self, column: usize) -> usize {
        let index = self.tabs.partition_point(|tab| tab.start <= column);
        match index.checked_sub(1).map(|i| self.tabs[i]) {
            Some(tab) if column < tab.start + tab.width => tab.input,
            Some(tab) => tab.input + 1 + (column - tab.start - tab.width),
            None => column,
        }
    }
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use gv_editor::display_map::{
        Highlight, ReplaceMap, ReplacePoint, StyleId, TabMap, TabPoint, TabSnapshot,
    };
    use gv_text::TextSnapshot;

    use super::*;

    /// The tab layer of `text` with `folds` folded.
    fn tabs(text: &str, folds: &[Range<usize>]) -> TabSnapshot {
        let (mut replace, _) = ReplaceMap::new(TextSnapshot::new(text));
        let (snapshot, _) = replace.fold(folds.iter().cloned());
        TabMap::new(snapshot, NonZeroU32::new(4).unwrap()).1
    }

    fn row(tabs: &TabSnapshot, row: u32, whitespace: bool, highlights: &[Highlight]) -> RowText {
        let end = TabPoint::new(row, tabs.line_len(row));
        RowText::from_chunks(
            tabs.highlighted_chunks(TabPoint::new(row, 0)..end, highlights),
            whitespace,
        )
    }

    fn of(text: &str, whitespace: bool) -> RowText {
        row(&tabs(text, &[]), 0, whitespace, &[])
    }

    #[test]
    fn plain_rows_map_to_themselves() {
        let row = of("abc é", false);
        assert_eq!(row.display, "abc é");
        assert_eq!(row.content, "abc é");
        assert_eq!(row.to_display(4), 4);
        assert_eq!(row.to_column(6), 6);
        assert!(row.marks.is_empty());
        assert_eq!(row.spans.len(), 1);
    }

    #[test]
    fn tabs_come_expanded() {
        let row = of("a\tb\t\tc", false);
        assert_eq!(row.content, "a   b       c");
        assert_eq!(row.display, row.content, "no map without marks");
        // Replace-layer columns and tab columns.
        assert_eq!(row.column_of_input(1), 1);
        assert_eq!(row.column_of_input(2), 4);
        assert_eq!(row.column_of_input(5), 12);
        assert_eq!(row.input_of_column(2), 1, "inside a tab: the tab");
        assert_eq!(row.input_of_column(4), 2);
        assert_eq!(row.input_of_column(9), 4);
        assert_eq!(row.input_of_column(13), 6);
    }

    #[test]
    fn whitespace_marks() {
        let row = of("a\tb  ", true);
        assert_eq!(row.display, "a→  b··↵");
        assert_eq!(row.marks.len(), 4);
        assert_eq!(row.content_end(), row.display.len() - '↵'.len_utf8());
        // Column 6 (the second trailing space) starts after one dot.
        assert_eq!(row.to_display(6), "a→  b·".len());
        assert_eq!(row.to_display(5), "a→  b".len());
        assert_eq!(row.to_column("a→  b·".len()), 6);
        assert_eq!(row.to_column("a→  b".len() + 1), 5);
        // Inside the arrow: its column.
        assert_eq!(row.to_column(2), 1);
        // A trailing tab is not a hard break; trailing spaces after one
        // are drawn.
        assert_eq!(of("a \t", true).display, "a·→ ");
        assert_eq!(of("a\t  ", true).display, "a→  ··↵");
        assert_eq!(of("a  ", false).display, "a  ");
        assert_eq!(of("a\u{a0}b", true).display, "a⍽b");
    }

    #[test]
    fn placeholders_get_room() {
        let tabs = tabs("a\nb\tc", std::slice::from_ref(&(1..3)));
        let row = row(&tabs, 0, false, &[]);
        // The placeholder counts one column for tab stops.
        assert_eq!(row.content, "a⋯  c");
        assert_eq!(row.display, "a ⋯   c");
        assert_eq!(row.placeholders, vec![1..4]);
        // The placeholder's ends: before its room, after it.
        assert_eq!(row.to_display(1), 1);
        assert_eq!(row.to_display(4), " ⋯ ".len() + 1);
        assert_eq!(row.to_column(" ⋯".len() + 1), 1);
        assert!(matches!(row.spans[1].replaced, Some(Replaced::Fold(_))));
        // A literal ellipsis is plain text.
        let plain = of("a⋯", false);
        assert_eq!(plain.display, "a⋯");
        assert!(plain.placeholders.is_empty());
    }

    #[test]
    fn control_pictures() {
        let row = of("a\u{1b}b", false);
        assert_eq!(row.display, "a\u{241b}b");
        assert_eq!(row.to_display(2), 4);
        assert_eq!(row.to_column(4), 2);
    }

    /// The display text as rows were made before the display map gave
    /// them: from the replace layer's row (`content`, with `⋯` at
    /// `placeholders`), expanding tabs here. Returns the display text, the
    /// display offset of every content byte and its end, and the marks.
    fn expanded_here(
        content: &str,
        tab_size: usize,
        show_whitespace: bool,
        placeholders: &[Range<usize>],
    ) -> (String, Vec<usize>, Vec<Range<usize>>) {
        let hard_break = content.ends_with("  ");
        let trailing_from = content.trim_end_matches([' ', '\t']).len();
        let mut display = String::new();
        let mut map = vec![0; content.len() + 1];
        let mut marks = Vec::new();
        let mut column = 0;
        let mut placeholders = placeholders.iter().peekable();
        for (at, c) in content.char_indices() {
            let start = display.len();
            for slot in &mut map[at..at + c.len_utf8()] {
                *slot = start;
            }
            if placeholders.next_if(|p| p.start == at).is_some() {
                display.push(' ');
                display.push(c);
                display.push(' ');
                column += 1;
                continue;
            }
            match c {
                '\t' => {
                    let width = tab_size - column % tab_size;
                    if show_whitespace {
                        display.push(TAB_MARK);
                        marks.push(start..display.len());
                        display.extend(std::iter::repeat_n(' ', width - 1));
                    } else {
                        display.extend(std::iter::repeat_n(' ', width));
                    }
                    column += width;
                }
                ' ' if show_whitespace && at >= trailing_from => {
                    display.push(SPACE_MARK);
                    marks.push(start..display.len());
                    column += 1;
                }
                '\u{a0}' if show_whitespace => {
                    display.push(NBSP_MARK);
                    marks.push(start..display.len());
                    column += 1;
                }
                c if (c as u32) < 0x20 => {
                    display.push(char::from_u32(0x2400 + c as u32).unwrap());
                    marks.push(start..display.len());
                    column += 1;
                }
                '\u{7f}' => {
                    display.push('\u{2421}');
                    marks.push(start..display.len());
                    column += 1;
                }
                c => {
                    display.push(c);
                    column += 1;
                }
            }
        }
        map[content.len()] = display.len();
        if show_whitespace && hard_break {
            let start = display.len();
            display.push(BREAK_MARK);
            marks.push(start..display.len());
        }
        (display, map, marks)
    }

    #[test]
    fn rows_from_chunks_draw_as_rows_expanded_here() {
        const CHARS: &[char] = &[
            'a', 'b', ' ', ' ', '\t', '\u{a0}', '\u{1b}', '\u{7f}', 'é', '⋯', '中',
        ];
        /// xorshift64.
        struct Rng(u64);
        impl Rng {
            fn below(&mut self, n: usize) -> usize {
                self.0 ^= self.0 << 13;
                self.0 ^= self.0 >> 7;
                self.0 ^= self.0 << 17;
                (self.0 % n as u64) as usize
            }
            fn line(&mut self, max: usize) -> String {
                let len = self.below(max);
                (0..len).map(|_| CHARS[self.below(CHARS.len())]).collect()
            }
        }
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..400 {
            let first = rng.line(12);
            let folded = rng.below(3) == 0;
            let text = if folded {
                format!("{first}\nhidden\n{}", rng.line(8))
            } else {
                first.clone()
            };
            let fold = first.len()..first.len() + "\nhidden\n".len();
            let tabs = tabs(
                &text,
                if folded {
                    std::slice::from_ref(&fold)
                } else {
                    &[]
                },
            );
            let replace = tabs.replace_snapshot();
            let content: String = replace.text().split('\n').next().unwrap().to_owned();
            let placeholders: Vec<Range<usize>> = replace
                .placeholders(0..1)
                .iter()
                .map(|p| p.range.start.column() as usize..p.range.end.column() as usize)
                .collect();
            for whitespace in [false, true] {
                let new = row(&tabs, 0, whitespace, &[]);
                let (display, map, marks) = expanded_here(&content, 4, whitespace, &placeholders);
                assert_eq!(new.display, display, "{content:?} {whitespace}");
                assert_eq!(new.marks, marks, "{content:?} {whitespace}");
                for (input, &at) in map.iter().enumerate() {
                    if !content.is_char_boundary(input) {
                        continue;
                    }
                    assert_eq!(
                        new.to_display(new.column_of_input(input)),
                        at,
                        "{content:?} {input}"
                    );
                }
                // A display offset maps back to the content byte it shows.
                for d in (0..=display.len()).filter(|&d| display.is_char_boundary(d)) {
                    let k = map.partition_point(|&m| m <= d);
                    let mut expected = k.saturating_sub(1);
                    while expected > 0 && map[expected - 1] == map[expected] {
                        expected -= 1;
                    }
                    assert_eq!(
                        new.input_of_column(new.to_column(d)),
                        expected,
                        "{content:?} {d}"
                    );
                }
            }
        }
    }

    #[test]
    fn spans_carry_the_chunks_styles() {
        let text = "ab\tcd";
        let tabs = tabs(text, &[]);
        let highlights = [
            Highlight {
                range: 1..3,
                style: StyleId(7),
            },
            Highlight {
                range: 4..5,
                style: StyleId(8),
            },
        ];
        let row = row(&tabs, 0, false, &highlights);
        let spans: Vec<(Range<usize>, Option<StyleId>)> = row
            .spans
            .iter()
            .map(|s| (s.columns.clone(), s.style))
            .collect();
        // The tab (two columns) keeps its style.
        assert_eq!(
            spans,
            [
                (0..1, None),
                (1..4, Some(StyleId(7))),
                (4..5, None),
                (5..6, Some(StyleId(8))),
            ]
        );
        assert_eq!(tabs.replace_snapshot().max_point(), ReplacePoint::new(0, 5));
    }
}

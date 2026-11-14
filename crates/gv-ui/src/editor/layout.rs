// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! One editor row as it is shaped: the buffer line turned into display
//! text, and the mapping between the two.
//!
//! The display text expands hard tabs to the tab stops of the display map
//! (counted in characters, as `gv_editor::movement::display_column`
//! counts), shows C0 control characters as their Unicode control pictures
//! and, with whitespace shown (FR-EDIT-13), draws trailing spaces as `·`,
//! tabs as `→`, no-break spaces as `⍽` and a `↵` after a line that ends in
//! a hard break of two or more spaces. Display text never changes a line's
//! number of rows.

use std::ops::Range;

/// The middle dot drawn for a trailing space.
const SPACE_MARK: char = '·';
/// The arrow drawn at the start of a tab.
const TAB_MARK: char = '→';
/// Drawn for U+00A0 NO-BREAK SPACE.
const NBSP_MARK: char = '⍽';
/// Drawn after a hard break made of trailing spaces.
const BREAK_MARK: char = '↵';

/// A line's display text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowText {
    /// What is shaped.
    pub display: String,
    /// The display offset of every buffer byte of the content, plus one for
    /// its end; `None` when display and buffer bytes are the same.
    map: Option<Vec<u32>>,
    /// Display ranges of whitespace marks and control pictures, drawn in
    /// `text.tertiary`.
    pub marks: Vec<Range<usize>>,
}

impl RowText {
    /// The display text of `content` (a line without its terminator).
    /// `hard_break` says whether the line ends in a hard break of spaces
    /// (only drawn with `show_whitespace`).
    pub fn new(content: &str, tab_size: u32, show_whitespace: bool, hard_break: bool) -> Self {
        let needs_map = content.bytes().any(|b| b == b'\t' || b < 0x20 || b == 0x7f)
            || (show_whitespace && (content.ends_with(' ') || content.contains('\u{a0}')));
        if !needs_map && !(show_whitespace && hard_break) {
            return Self {
                display: content.to_owned(),
                map: None,
                marks: Vec::new(),
            };
        }
        let tab_size = tab_size.max(1) as usize;
        let trailing = content.len() - content.trim_end_matches([' ', '\t']).len();
        let trailing_from = content.len() - trailing;
        let mut display = String::with_capacity(content.len() + 8);
        let mut map = vec![0u32; content.len() + 1];
        let mut marks = Vec::new();
        let mut column = 0usize;
        for (at, c) in content.char_indices() {
            let start = display.len();
            for slot in &mut map[at..at + c.len_utf8()] {
                *slot = u32::try_from(start).unwrap_or(u32::MAX);
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
                    display.push(char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}'));
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
        map[content.len()] = u32::try_from(display.len()).unwrap_or(u32::MAX);
        if show_whitespace && hard_break {
            let start = display.len();
            display.push(BREAK_MARK);
            marks.push(start..display.len());
        }
        Self {
            display,
            map: Some(map),
            marks,
        }
    }

    /// The display offset of buffer column `column` (clamped).
    pub fn to_display(&self, column: usize) -> usize {
        match &self.map {
            None => column.min(self.display.len()),
            Some(map) => map
                .get(column.min(map.len() - 1))
                .map_or(self.display.len(), |&d| d as usize),
        }
    }

    /// The buffer column of display offset `display`: the start of the
    /// character it falls in.
    pub fn to_buffer(&self, display: usize) -> usize {
        match &self.map {
            None => display.min(self.display.len()),
            Some(map) => {
                let display = u32::try_from(display).unwrap_or(u32::MAX);
                // The last buffer byte whose display offset is at or before
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

    /// Display ranges for buffer ranges (line-relative).
    pub fn display_range(&self, range: Range<usize>) -> Range<usize> {
        self.to_display(range.start)..self.to_display(range.end)
    }

    /// The display length of the content (without a `↵` mark).
    pub fn content_end(&self) -> usize {
        match &self.map {
            None => self.display.len(),
            Some(map) => map.last().map_or(0, |&d| d as usize),
        }
    }
}

/// Whether `content` ends in a hard break of spaces (two or more).
pub(crate) fn ends_in_hard_break(content: &str) -> bool {
    content.ends_with("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_lines_map_to_themselves() {
        let row = RowText::new("abc é", 4, false, false);
        assert_eq!(row.display, "abc é");
        assert_eq!(row.to_display(4), 4);
        assert_eq!(row.to_buffer(6), 6);
        assert!(row.marks.is_empty());
    }

    #[test]
    fn tabs_expand_to_stops() {
        let row = RowText::new("a\tb\t\tc", 4, false, false);
        assert_eq!(row.display, "a   b       c");
        assert_eq!(row.to_display(1), 1);
        assert_eq!(row.to_display(2), 4);
        assert_eq!(row.to_display(5), 12);
        // Inside an expanded tab: the tab.
        assert_eq!(row.to_buffer(2), 1);
        assert_eq!(row.to_buffer(4), 2);
        assert_eq!(row.to_buffer(13), 6);
    }

    #[test]
    fn whitespace_marks() {
        let row = RowText::new("a\tb  ", 4, true, true);
        assert_eq!(row.display, "a→  b··↵");
        assert_eq!(row.marks.len(), 4);
        assert_eq!(row.content_end(), row.display.len() - '↵'.len_utf8());
        // Buffer column 3 (the second trailing space) starts after one dot.
        assert_eq!(row.to_display(3), "a→  b".len());
        assert_eq!(row.to_display(4), "a→  b·".len());
        assert_eq!(row.to_buffer("a→  b·".len()), 4);
        assert_eq!(row.to_buffer("a→  b".len() + 1), 3);
    }

    #[test]
    fn control_pictures() {
        let row = RowText::new("a\u{1b}b", 4, false, false);
        assert_eq!(row.display, "a\u{241b}b");
        assert_eq!(row.to_display(2), 4);
        assert_eq!(row.to_buffer(4), 2);
    }
}

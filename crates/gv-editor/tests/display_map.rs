// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests of the replace and tab layers against naive models, using
//! only the public API.
//!
//! - Chunks (and line lengths, summaries) equal a naive tab expansion of the
//!   normalized buffer text.
//! - Point conversions round-trip, and clipping gives valid points.
//! - Syncing through edits, tab-size and expansion-limit changes gives the
//!   same snapshot as rebuilding, and the reported edits turn the old text
//!   into the new text.

use std::num::NonZeroU32;
use std::ops::Range;

use gv_editor::display_map::{
    BufferPoint, Edit, MAX_EXPANSION_COLUMN, Point, ReplaceMap, ReplacePoint, ReplaceSnapshot,
    TabMap, TabPoint, TabSnapshot, TextSummary,
};
use gv_text::{Bias, Buffer, TextSnapshot};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError};

/// Cases per property; `PROPTEST_CASES` overrides it for longer runs.
const CASES: u32 = 96;

fn config() -> Config {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(CASES);
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

// ----- generators -------------------------------------------------------

/// Tabs, every terminator, and 2-, 3- and 4-byte UTF-8.
fn text_char() -> impl Strategy<Value = char> {
    prop_oneof![
        5 => prop::sample::select(vec!['a', 'b', ' ']),
        3 => Just('\t'),
        2 => Just('\n'),
        1 => Just('\r'),
        1 => prop::sample::select(vec!['é', '語', '🍐']),
    ]
}

/// Mostly short texts; some long enough for long rows and several rope
/// chunks.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::collection::vec(text_char(), 0..60),
        1 => prop::collection::vec(text_char(), 0..400),
    ]
    .prop_map(|chars| chars.into_iter().collect())
}

fn tab_size() -> impl Strategy<Value = NonZeroU32> {
    (1u32..=8).prop_map(|size| NonZeroU32::new(size).expect("1..=8 is not zero"))
}

fn max_expansion_column() -> impl Strategy<Value = u32> {
    prop_oneof![1 => Just(MAX_EXPANSION_COLUMN), 2 => 0u32..40]
}

/// A replacement as random numbers; turned into a valid edit by `build`.
type RawEdit = (usize, usize, String);

fn raw_edits() -> impl Strategy<Value = Vec<RawEdit>> {
    let inserted = prop::collection::vec(text_char(), 0..8)
        .prop_map(|chars| chars.into_iter().collect::<String>());
    prop::collection::vec((any::<usize>(), 0usize..10, inserted), 1..5)
}

#[derive(Clone, Debug)]
enum Operation {
    Edit(Vec<RawEdit>),
    TabSize(NonZeroU32),
    MaxExpansionColumn(u32),
}

fn operation() -> impl Strategy<Value = Operation> {
    prop_oneof![
        8 => raw_edits().prop_map(Operation::Edit),
        1 => tab_size().prop_map(Operation::TabSize),
        1 => max_expansion_column().prop_map(Operation::MaxExpansionColumn),
    ]
}

/// Turns random numbers into sorted, non-overlapping edits on char
/// boundaries.
fn build(text: &str, raw: &[RawEdit]) -> Vec<(Range<usize>, String)> {
    let floor = |mut i: usize| {
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let mut starts: Vec<(usize, usize, &String)> = raw
        .iter()
        .map(|(pos, len, s)| (floor(pos % (text.len() + 1)), *len, s))
        .collect();
    starts.sort_by_key(|(start, _, _)| *start);
    let mut edits = Vec::new();
    let mut prev_end = 0;
    for (start, len, s) in starts {
        let start = start.max(prev_end);
        let mut end = (start + len).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        edits.push((start..end, s.clone()));
        prev_end = end;
    }
    edits
}

// ----- naive models -----------------------------------------------------

/// Every terminator as one `\n`.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Expands the tabs of one row: a tab whose input byte column is below
/// `max_expansion_column` goes to the next stop of `tab_size` `char`s, any
/// other tab becomes one space.
fn expand_row(row: &str, tab_size: u32, max_expansion_column: u32) -> String {
    let mut out = String::new();
    let mut chars = 0;
    let mut input_column = 0;
    for c in row.chars() {
        if c == '\t' {
            let len = if input_column < max_expansion_column {
                tab_size - chars % tab_size
            } else {
                1
            };
            out.extend(std::iter::repeat_n(' ', len as usize));
            chars += len;
        } else {
            out.push(c);
            chars += 1;
        }
        input_column += c.len_utf8() as u32;
    }
    out
}

fn expand(text: &str, tab_size: u32, max_expansion_column: u32) -> String {
    text.split('\n')
        .map(|row| expand_row(row, tab_size, max_expansion_column))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rows of a `\n`-separated text.
fn rows(text: &str) -> Vec<&str> {
    text.split('\n').collect()
}

fn offset_of(text: &str, point: Point) -> usize {
    let start: usize = rows(text)
        .iter()
        .take(point.row as usize)
        .map(|row| row.len() + 1)
        .sum();
    start + point.column as usize
}

fn max_point_of(text: &str) -> Point {
    let rows = rows(text);
    Point::new(
        rows.len() as u32 - 1,
        rows.last().map_or(0, |r| r.len()) as u32,
    )
}

/// Applies `edits` to `old_text`, taking inserted text from `new_text`.
fn replay<D: Copy>(
    old_text: &str,
    new_text: &str,
    edits: &[Edit<D>],
    point: impl Fn(D) -> Point,
) -> String {
    let mut text = old_text.to_owned();
    for edit in edits {
        let start = point(edit.new.start);
        let old_start = offset_of(&text, start);
        let old_end = offset_of(&text, start + (point(edit.old.end) - point(edit.old.start)));
        let inserted = &new_text
            [offset_of(new_text, point(edit.new.start))..offset_of(new_text, point(edit.new.end))];
        text.replace_range(old_start..old_end, inserted);
    }
    text
}

fn layers(buffer: TextSnapshot, tab_size: NonZeroU32, max: u32) -> (ReplaceSnapshot, TabSnapshot) {
    let (_, replace) = ReplaceMap::new(buffer);
    let (mut tab_map, _) = TabMap::new(replace.clone(), tab_size);
    let (tabs, _) = tab_map.set_max_expansion_column(max);
    (replace, tabs)
}

/// Every valid replace point (row and char-boundary column).
fn replace_points(normalized: &str) -> Vec<ReplacePoint> {
    let mut points = Vec::new();
    for (row, text) in rows(normalized).iter().enumerate() {
        for (column, _) in text.char_indices() {
            points.push(ReplacePoint::new(row as u32, column as u32));
        }
        points.push(ReplacePoint::new(row as u32, text.len() as u32));
    }
    points
}

fn check_chunk_bitmaps(
    tabs: &TabSnapshot,
    range: Range<TabPoint>,
) -> Result<String, TestCaseError> {
    let mut text = String::new();
    for chunk in tabs.chunks(range) {
        let mut chars = 0u128;
        let mut newlines = 0u128;
        for (index, c) in chunk.text.char_indices() {
            chars |= 1 << index;
            if c == '\n' {
                newlines |= 1 << index;
            }
        }
        prop_assert_eq!(chunk.chars, chars, "chars of {:?}", chunk.text);
        prop_assert_eq!(chunk.newlines, newlines, "newlines of {:?}", chunk.text);
        prop_assert_eq!(chunk.tabs, 0);
        if chunk.is_tab {
            prop_assert!(chunk.text.bytes().all(|b| b == b' '), "{:?}", chunk.text);
        }
        text.push_str(chunk.text);
    }
    Ok(text)
}

// ----- properties -------------------------------------------------------

proptest! {
    #![proptest_config(config())]

    #[test]
    fn chunks_equal_naive_expansion(
        input in text(),
        tab_size in tab_size(),
        max in max_expansion_column(),
        picks in prop::collection::vec((any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>()), 8),
    ) {
        let (replace, tabs) = layers(TextSnapshot::new(&input), tab_size, max);
        let normalized = normalize(&input);
        prop_assert_eq!(replace.text(), normalized.clone());
        let expected = expand(&normalized, tab_size.get(), max);
        prop_assert_eq!(tabs.text(), expected.clone());
        prop_assert_eq!(tabs.max_point().0, max_point_of(&expected));
        for (row, text) in rows(&expected).iter().enumerate() {
            prop_assert_eq!(tabs.line_len(row as u32), text.len() as u32, "row {}", row);
        }

        let max_point = tabs.max_point();
        let expected_rows = rows(&expected);
        for (a, b, c, d) in picks {
            let point = |row: u32, column: u32, bias| {
                let row = row % (max_point.row() + 1);
                let len = expected_rows[row as usize].len() as u32;
                tabs.clip_point(TabPoint::new(row, column % (len + 1)), bias)
            };
            let mut start = point(a, b, Bias::Left);
            let mut end = point(c, d, Bias::Right);
            if start > end {
                std::mem::swap(&mut start, &mut end);
            }
            let slice = &expected[offset_of(&expected, start.0)..offset_of(&expected, end.0)];
            prop_assert_eq!(check_chunk_bitmaps(&tabs, start..end)?, slice, "{:?}..{:?}", start, end);

            let summary = tabs.text_summary_for_range(start..end);
            let naive = TextSummary::from(slice);
            prop_assert_eq!(summary.lines, naive.lines);
            prop_assert_eq!(summary.first_line_chars, naive.first_line_chars);
            prop_assert_eq!(summary.last_line_chars, naive.last_line_chars);
            if !normalized.contains('\t') {
                prop_assert_eq!(summary, naive);
            }
            // The longest row ignores tab expansion, as upstream.
            let input_start = tabs.tab_point_to_replace_point(start, Bias::Left).0;
            let input_end = tabs.tab_point_to_replace_point(end, Bias::Right).0;
            let input_summary = replace.text_summary_for_range(input_start..input_end);
            prop_assert_eq!(summary.longest_row, input_summary.longest_row);
            prop_assert_eq!(summary.longest_row_chars, input_summary.longest_row_chars);
        }
    }

    #[test]
    fn conversions_round_trip(
        input in text(),
        tab_size in tab_size(),
        max in max_expansion_column(),
    ) {
        let buffer = TextSnapshot::new(&input);
        let (replace, tabs) = layers(buffer.clone(), tab_size, max);
        let normalized = normalize(&input);
        let normalized_rows = rows(&normalized);

        // Replace -> tab -> replace, against the naive expansion.
        let mut previous: Option<(ReplacePoint, TabPoint)> = None;
        for point in replace_points(&normalized) {
            let row = normalized_rows[point.row() as usize];
            let expanded = expand_row(&row[..point.column() as usize], tab_size.get(), max);
            let tab_point = tabs.replace_point_to_tab_point(point);
            prop_assert_eq!(tab_point, TabPoint::new(point.row(), expanded.len() as u32));
            for bias in [Bias::Left, Bias::Right] {
                prop_assert_eq!(tabs.tab_point_to_replace_point(tab_point, bias).0, point);
                prop_assert_eq!(tabs.clip_point(tab_point, bias), tab_point);
            }
            if let Some((previous_point, previous_tab)) = previous
                && previous_point.row() == point.row()
            {
                prop_assert!(previous_tab < tab_point, "{:?} {:?}", previous_tab, tab_point);
            }
            previous = Some((point, tab_point));
        }
        let mut cursor = tabs.tab_point_cursor();
        for point in replace_points(&normalized) {
            prop_assert_eq!(cursor.map(point), tabs.replace_point_to_tab_point(point));
        }

        // Clipping any tab point gives a point that round-trips.
        for row in 0..=tabs.max_point().row() {
            let len = tabs.line_len(row);
            for column in 0..=len + 2 {
                let point = TabPoint::new(row, column);
                for bias in [Bias::Left, Bias::Right] {
                    let clipped = tabs.clip_point(point, bias);
                    prop_assert_eq!(clipped.row(), row);
                    prop_assert!(clipped.column() <= len);
                    let back = tabs.tab_point_to_replace_point(clipped, bias).0;
                    prop_assert_eq!(tabs.replace_point_to_tab_point(back), clipped);
                    if column <= len {
                        match bias {
                            Bias::Left => prop_assert!(clipped <= point),
                            Bias::Right => prop_assert!(clipped >= point),
                        }
                    }
                }
            }
        }

        // Buffer -> tab -> buffer for every buffer position outside a CRLF.
        for offset in 0..=input.len() {
            if !input.is_char_boundary(offset) || buffer.is_inside_crlf(offset) {
                continue;
            }
            let point: BufferPoint = buffer.offset_to_point(offset).unwrap();
            for bias in [Bias::Left, Bias::Right] {
                let tab_point = tabs.buffer_point_to_tab_point(point, bias);
                prop_assert_eq!(tabs.tab_point_to_buffer_point(tab_point, bias), point);
                prop_assert_eq!(replace.to_buffer_point(replace.to_replace_point(point, bias), bias), point);
            }
            prop_assert_eq!(tabs.buffer_line_to_tab_row(point.line), point.line as u32);
        }
    }

    #[test]
    fn incremental_sync_equals_rebuild(
        input in text(),
        initial_tab_size in tab_size(),
        initial_max in max_expansion_column(),
        operations in prop::collection::vec(operation(), 1..8),
    ) {
        let mut buffer = Buffer::new(&input);
        let mut tab_size = initial_tab_size;
        let mut max = initial_max;
        let (mut replace_map, replace) = ReplaceMap::new(buffer.snapshot());
        let (mut tab_map, _) = TabMap::new(replace, tab_size);
        let (mut tabs, _) = tab_map.set_max_expansion_column(max);

        for operation in operations {
            let old_replace = tabs.replace_snapshot().clone();
            let old_text = tabs.text();
            let (new_tabs, tab_edits) = match operation {
                Operation::Edit(raw) => {
                    let edits = build(&buffer.to_string(), &raw);
                    let summary = buffer.edit(edits).unwrap();
                    let (replace, replace_edits) = replace_map.sync(buffer.snapshot(), &summary);
                    prop_assert_eq!(
                        replay(&old_replace.text(), &replace.text(), &replace_edits, |p| p.0),
                        replace.text(),
                        "replace edits {:?}", replace_edits
                    );
                    tab_map.sync(replace, replace_edits, tab_size)
                }
                Operation::TabSize(size) => {
                    tab_size = size;
                    tab_map.sync(replace_map.snapshot(), Vec::new(), tab_size)
                }
                Operation::MaxExpansionColumn(column) => {
                    max = column;
                    tab_map.set_max_expansion_column(max)
                }
            };
            tabs = new_tabs;

            let (_, rebuilt) = layers(buffer.snapshot(), tab_size, max);
            let new_text = tabs.text();
            prop_assert_eq!(&new_text, &rebuilt.text());
            prop_assert_eq!(&new_text, &expand(&normalize(&buffer.to_string()), tab_size.get(), max));
            prop_assert_eq!(tabs.max_point(), rebuilt.max_point());
            prop_assert_eq!(
                replay(&old_text, &new_text, &tab_edits, |p| p.0),
                new_text.clone(),
                "tab edits {:?}", tab_edits
            );
            for point in replace_points(&normalize(&buffer.to_string())) {
                prop_assert_eq!(
                    tabs.replace_point_to_tab_point(point),
                    rebuilt.replace_point_to_tab_point(point)
                );
            }
        }
    }
}

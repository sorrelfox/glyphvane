// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests of folds in the display layers against naive models,
//! using only the public API. Random texts, folds, unfolds and edits:
//!
//! - the replace layer's text is the buffer text with every run of
//!   overlapping folds replaced by one placeholder, then normalized;
//!   the tab layer's text is that text with its tabs expanded;
//! - point conversions round-trip, and buffer positions inside a run go to
//!   its placeholder's start or end;
//! - an edit keeps exactly the folds it does not change inside (with their
//!   text) and reports the others;
//! - syncing gives the same replace, tab and wrap layers as rebuilding
//!   them from the buffer and the folds, and the edits each layer reports
//!   turn its old text into its new text.

use std::num::NonZeroU32;
use std::ops::Range;

use gv_editor::display_map::{
    BufferPoint, Edit, MonospaceMeasure, PLACEHOLDER, Point, ReplaceMap, ReplacePoint,
    ReplaceSnapshot, TabMap, TabSnapshot, WrapEdit, WrapMap, WrapSnapshot,
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

fn text_char() -> impl Strategy<Value = char> {
    prop_oneof![
        6 => prop::sample::select(vec!['a', 'b', ' ', '#', '-']),
        2 => Just('\t'),
        3 => Just('\n'),
        1 => Just('\r'),
        1 => prop::sample::select(vec!['é', '語', '🍐']),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::collection::vec(text_char(), 0..60),
        1 => prop::collection::vec(text_char(), 0..300),
    ]
    .prop_map(|chars| chars.into_iter().collect())
}

/// A replacement as random numbers; made valid by `build`.
type RawEdit = (usize, usize, String);

fn raw_edits() -> impl Strategy<Value = Vec<RawEdit>> {
    let inserted = prop::collection::vec(text_char(), 0..6)
        .prop_map(|chars| chars.into_iter().collect::<String>());
    prop::collection::vec((any::<usize>(), 0usize..12, inserted), 1..4)
}

/// A range as random numbers: a start and a length.
fn raw_ranges() -> impl Strategy<Value = Vec<(usize, usize)>> {
    prop::collection::vec((any::<usize>(), 0usize..40), 1..4)
}

#[derive(Clone, Debug)]
enum Operation {
    Fold(Vec<(usize, usize)>),
    Unfold(Vec<(usize, usize)>, bool),
    UnfoldAll,
    Edit(Vec<RawEdit>),
}

fn operation() -> impl Strategy<Value = Operation> {
    prop_oneof![
        4 => raw_ranges().prop_map(Operation::Fold),
        2 => (raw_ranges(), any::<bool>()).prop_map(|(ranges, inclusive)| Operation::Unfold(ranges, inclusive)),
        1 => Just(Operation::UnfoldAll),
        6 => raw_edits().prop_map(Operation::Edit),
    ]
}

fn floor(text: &str, mut offset: usize) -> usize {
    offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Random numbers to byte ranges on character boundaries (possibly inside
/// a CRLF: the layer clips them).
fn ranges(text: &str, raw: &[(usize, usize)]) -> Vec<Range<usize>> {
    raw.iter()
        .map(|&(start, len)| {
            let start = floor(text, start % (text.len() + 1));
            let mut end = (start + len).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            start..end
        })
        .collect()
}

/// Random numbers to sorted, non-overlapping edits on char boundaries.
fn build(text: &str, raw: &[RawEdit]) -> Vec<(Range<usize>, String)> {
    let mut starts: Vec<(usize, usize, &String)> = raw
        .iter()
        .map(|(pos, len, s)| (floor(text, pos % (text.len() + 1)), *len, s))
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

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The unions of overlapping folds, sorted.
fn runs(snapshot: &ReplaceSnapshot) -> Vec<Range<usize>> {
    let mut runs: Vec<Range<usize>> = Vec::new();
    let mut folds: Vec<Range<usize>> = snapshot.folds().iter().map(|f| f.range.clone()).collect();
    folds.sort_by_key(|range| range.start);
    for range in folds {
        match runs.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => runs.push(range),
        }
    }
    runs
}

fn expected_text(buffer: &TextSnapshot, runs: &[Range<usize>]) -> String {
    let mut text = buffer.to_string();
    for range in runs.iter().rev() {
        text.replace_range(range.clone(), PLACEHOLDER);
    }
    normalize(&text)
}

fn expand_row(row: &str, tab_size: u32) -> String {
    let mut out = String::new();
    let mut chars = 0;
    for c in row.chars() {
        if c == '\t' {
            let len = tab_size - chars % tab_size;
            out.extend(std::iter::repeat_n(' ', len as usize));
            chars += len;
        } else {
            out.push(c);
            chars += 1;
        }
    }
    out
}

fn expand(text: &str, tab_size: u32) -> String {
    text.split('\n')
        .map(|row| expand_row(row, tab_size))
        .collect::<Vec<_>>()
        .join("\n")
}

fn offset_of(text: &str, point: Point) -> usize {
    let mut offset = 0;
    for (row, line) in text.split('\n').enumerate() {
        if row as u32 == point.row {
            return offset + (point.column as usize).min(line.len());
        }
        offset += line.len() + 1;
    }
    text.len()
}

/// Applies point edits to `old_text`, taking inserted text from
/// `new_text`.
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
        let distance = point(edit.old.end) - point(edit.old.start);
        let old_end = if distance.row == 0 {
            offset_of(&text, Point::new(start.row, start.column + distance.column))
        } else {
            offset_of(&text, Point::new(start.row + distance.row, distance.column))
        };
        let inserted = &new_text
            [offset_of(new_text, point(edit.new.start))..offset_of(new_text, point(edit.new.end))];
        text.replace_range(old_start..old_end, inserted);
    }
    text
}

/// Applies row edits of the wrap layer.
fn replay_rows(old_text: &str, new_text: &str, edits: &[WrapEdit]) -> String {
    let mut text = old_text.to_owned();
    for edit in edits {
        let rows = edit.old.end.0 - edit.old.start.0;
        let old_start = offset_of(&text, Point::new(edit.new.start.0, 0));
        let old_end = offset_of(&text, Point::new(edit.new.start.0 + rows, 0));
        let new_start = offset_of(new_text, Point::new(edit.new.start.0, 0));
        let new_end = offset_of(new_text, Point::new(edit.new.end.0, 0));
        text.replace_range(old_start..old_end, &new_text[new_start..new_end]);
    }
    text
}

/// The folded text of a fold that an edit kept: the same, except that a
/// CR or LF at an end may be cut when the edit made it part of a CRLF
/// outside.
fn same_folded_text(old: &str, new: &str) -> bool {
    let trimmed = |text: &str, front: bool, back: bool| {
        let mut text = text.to_owned();
        if back && text.ends_with('\r') {
            text.pop();
        }
        if front && text.starts_with('\n') {
            text.remove(0);
        }
        text
    };
    [(false, false), (true, false), (false, true), (true, true)]
        .into_iter()
        .any(|(front, back)| trimmed(old, front, back) == new)
}

// ----- checks -----------------------------------------------------------

fn check_layer(snapshot: &ReplaceSnapshot) -> Result<(), TestCaseError> {
    let buffer = snapshot.buffer();
    let runs = runs(snapshot);
    let expected = expected_text(buffer, &runs);
    prop_assert_eq!(snapshot.text(), expected.clone());
    let rows: Vec<&str> = expected.split('\n').collect();
    prop_assert_eq!(snapshot.max_point().row() as usize, rows.len() - 1);
    for (row, text) in rows.iter().enumerate() {
        prop_assert_eq!(
            snapshot.line_len(row as u32),
            text.len() as u32,
            "row {}",
            row
        );
    }
    for fold in snapshot.folds() {
        prop_assert!(fold.range.start < fold.range.end, "{:?}", fold);
        prop_assert!(!buffer.is_inside_crlf(fold.range.start), "{:?}", fold);
        prop_assert!(!buffer.is_inside_crlf(fold.range.end), "{:?}", fold);
        prop_assert!(buffer.is_char_boundary(fold.range.start));
        prop_assert!(buffer.is_char_boundary(fold.range.end));
    }

    // Layer -> buffer -> layer at every character.
    for (row, text) in rows.iter().enumerate() {
        let columns = text
            .char_indices()
            .map(|(column, _)| column)
            .chain([text.len()]);
        for column in columns {
            let point = ReplacePoint::new(row as u32, column as u32);
            for bias in [Bias::Left, Bias::Right] {
                prop_assert_eq!(snapshot.clip_point(point, bias), point);
                let buffer_point = snapshot.to_buffer_point(point, bias);
                prop_assert_eq!(
                    snapshot.to_replace_point(buffer_point, bias),
                    point,
                    "{:?}",
                    buffer_point
                );
            }
        }
    }

    // Buffer -> layer: outside a run back to the same position, inside one
    // to its placeholder's ends.
    let text = buffer.to_string();
    for offset in 0..=text.len() {
        if !text.is_char_boundary(offset) || buffer.is_inside_crlf(offset) {
            continue;
        }
        let point: BufferPoint = buffer
            .offset_to_point(offset)
            .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
        let run = runs
            .iter()
            .find(|run| run.start < offset && offset < run.end);
        prop_assert_eq!(snapshot.folded_range_at(offset), run.cloned());
        for bias in [Bias::Left, Bias::Right] {
            let replace_point = snapshot.to_replace_point(point, bias);
            let back = snapshot.to_buffer_point(replace_point, bias);
            let back = buffer
                .point_to_offset(back)
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            match run {
                Some(run) => {
                    let end = match bias {
                        Bias::Left => run.start,
                        Bias::Right => run.end,
                    };
                    prop_assert_eq!(back, end, "offset {} in {:?}", offset, run);
                }
                None => prop_assert_eq!(back, offset),
            }
        }
    }

    // Rebuilding from the folds gives the same layer.
    let (mut rebuilt, _) = ReplaceMap::new(buffer.clone());
    let (rebuilt, _) = rebuilt.fold(snapshot.folds().iter().map(|fold| fold.range.clone()));
    prop_assert_eq!(rebuilt.text(), snapshot.text());
    prop_assert_eq!(rebuilt.folds().len(), snapshot.folds().len());
    prop_assert_eq!(
        rebuilt.buffer_lines(0).collect::<Vec<_>>(),
        snapshot.buffer_lines(0).collect::<Vec<_>>()
    );
    let placeholders = |snapshot: &ReplaceSnapshot| {
        snapshot
            .placeholders(0..u32::MAX)
            .into_iter()
            .map(|placeholder| (placeholder.range, placeholder.buffer_range))
            .collect::<Vec<_>>()
    };
    prop_assert_eq!(placeholders(&rebuilt), placeholders(snapshot));
    Ok(())
}

fn rebuilt_wrap(tabs: &TabSnapshot, width: Option<f32>) -> WrapSnapshot {
    WrapMap::new(tabs.clone(), Box::new(MonospaceMeasure::new(1.0)), width).1
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn folds_through_edits(
        input in text(),
        tab_size in (1u32..=8).prop_map(|size| NonZeroU32::new(size).unwrap()),
        width in prop_oneof![1 => Just(None), 3 => (3u32..20).prop_map(|w| Some(w as f32))],
        operations in prop::collection::vec(operation(), 1..10),
    ) {
        let mut buffer = Buffer::new(&input);
        let (mut replace_map, replace) = ReplaceMap::new(buffer.snapshot());
        let (mut tab_map, tabs) = TabMap::new(replace, tab_size);
        let (mut wrap_map, mut wrap) =
            WrapMap::new(tabs.clone(), Box::new(MonospaceMeasure::new(1.0)), width);
        let mut tabs = tabs;

        for operation in operations {
            let old_replace = tabs.replace_snapshot().clone();
            let old_tabs = tabs.clone();
            let old_wrap = wrap.clone();
            let (replace, replace_edits) = match &operation {
                Operation::Fold(raw) => {
                    let text = buffer.to_string();
                    replace_map.fold(ranges(&text, raw))
                }
                Operation::Unfold(raw, inclusive) => {
                    let text = buffer.to_string();
                    replace_map.unfold_intersecting(ranges(&text, raw), *inclusive)
                }
                Operation::UnfoldAll => replace_map.unfold_all(),
                Operation::Edit(raw) => {
                    let old_buffer = buffer.snapshot();
                    let edits = build(&old_buffer.to_string(), raw);
                    let summary = buffer.edit(edits).unwrap();
                    let (replace, replace_edits, unfolded) =
                        replace_map.sync_with_unfolded(buffer.snapshot(), &summary);
                    // Exactly the folds changed inside are gone; the others
                    // kept their text.
                    let old_text = old_buffer.to_string();
                    let new_text = buffer.to_string();
                    for fold in old_replace.folds() {
                        let changed_inside = summary.edits().iter().any(|edit| {
                            edit.old.start < fold.range.end
                                && edit.old.end > fold.range.start
                                && old_text[edit.old.clone()] != new_text[edit.new.clone()]
                        });
                        let kept = replace.folds().iter().find(|new| new.id == fold.id);
                        let removed = unfolded.iter().any(|old| old == fold);
                        prop_assert_eq!(kept.is_none(), removed, "{:?}", fold);
                        match kept {
                            Some(kept) => {
                                prop_assert!(!changed_inside, "{:?} kept", fold);
                                prop_assert!(
                                    same_folded_text(
                                        &old_text[fold.range.clone()],
                                        &new_text[kept.range.clone()]
                                    ),
                                    "{:?} -> {:?}", fold, kept
                                );
                            }
                            // A fold of a lone line break that an edit next
                            // to it made half of a CRLF is gone too, and so
                            // is one the edit gave the range of another.
                            None => {
                                let folded = &old_text[fold.range.clone()];
                                let duplicate = replace.folds().iter().any(|other| {
                                    same_folded_text(folded, &new_text[other.range.clone()])
                                });
                                prop_assert!(
                                    changed_inside || matches!(folded, "\n" | "\r") || duplicate,
                                    "{:?} removed", fold
                                );
                            }
                        }
                    }
                    (replace, replace_edits)
                }
            };
            prop_assert_eq!(
                replay(&old_replace.text(), &replace.text(), &replace_edits, |p| p.0),
                replace.text(),
                "replace edits {:?} after {:?}", replace_edits, operation
            );
            check_layer(&replace)?;

            let (new_tabs, tab_edits) = tab_map.sync(replace.clone(), replace_edits, tab_size);
            prop_assert_eq!(new_tabs.text(), expand(&replace.text(), tab_size.get()));
            prop_assert_eq!(
                replay(&old_tabs.text(), &new_tabs.text(), &tab_edits, |p| p.0),
                new_tabs.text(),
                "tab edits {:?}", tab_edits
            );
            tabs = new_tabs;

            let (new_wrap, wrap_edits) = wrap_map.sync(tabs.clone(), tab_edits);
            prop_assert_eq!(new_wrap.text(), rebuilt_wrap(&tabs, width).text());
            prop_assert_eq!(
                replay_rows(&old_wrap.text(), &new_wrap.text(), &wrap_edits),
                new_wrap.text(),
                "wrap edits {:?}", wrap_edits
            );
            wrap = new_wrap;
        }
    }
}

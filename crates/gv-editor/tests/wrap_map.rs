// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests of the wrap layer against its own text and a rebuild,
//! using only the public API, with a variable-width measure:
//!
//! - every row fits the width, unless it holds a single grapheme (with its
//!   indent), which cannot be broken;
//! - the rows, without the inserted line breaks and indents, are the tab
//!   layer's text;
//! - point conversions round-trip and clipping gives valid points;
//! - syncing through edits, tab-size changes, width changes (at once or
//!   through a resumable job) and row refreshes gives the same rows as
//!   rebuilding, and the reported edits turn the old text into the new;
//! - wrapping at another width and back gives the original rows.

use std::num::NonZeroU32;
use std::ops::Range;
use std::sync::Arc;

use gv_editor::display_map::{
    IndentAdjustment, LineWrapper, MarkdownHangingIndent, MonospaceMeasure, Point, ReplaceMap,
    TabMap, TabPoint, TabSnapshot, TextMeasure, WrapEdit, WrapIndent, WrapMap, WrapPoint, WrapRow,
    WrapSnapshot,
};
use gv_text::{Bias, Buffer, TextSnapshot};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError};
use unicode_segmentation::UnicodeSegmentation;

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

/// East Asian wide characters and emoji are two columns, combining marks
/// none, everything else one.
#[derive(Clone, Copy, Debug)]
struct Columns;

impl Columns {
    fn width(c: char) -> f32 {
        match c as u32 {
            0x0300..=0x036F | 0x200D => 0.0,
            0x3000..=0x9FFF | 0xFF01..=0xFF60 | 0x1F300..=0x1F64F => 2.0,
            _ => 1.0,
        }
    }
}

impl TextMeasure for Columns {
    fn char_width(&mut self, c: char) -> f32 {
        Self::width(c)
    }
}

// ----- generators -------------------------------------------------------

/// Words, spaces, tabs, every terminator, CJK and punctuation, combining
/// marks and emoji, list markers.
fn text_char() -> impl Strategy<Value = char> {
    prop_oneof![
        10 => prop::sample::select(vec!['a', 'b', 'c', 'd']),
        5 => Just(' '),
        1 => Just('\t'),
        2 => Just('\n'),
        1 => prop::sample::select(vec!['\r', '-', '>', '/', '.', '1']),
        2 => prop::sample::select(vec!['é', '語', '本', '、', '「', '」', '🍐']),
        1 => prop::sample::select(vec!['\u{301}', '\u{200d}']),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::collection::vec(text_char(), 0..80),
        1 => prop::collection::vec(text_char(), 0..400),
    ]
    .prop_map(|chars| chars.into_iter().collect())
}

fn tab_size() -> impl Strategy<Value = NonZeroU32> {
    (1u32..=8).prop_map(|size| NonZeroU32::new(size).expect("1..=8 is not zero"))
}

fn wrap_width() -> impl Strategy<Value = f32> {
    prop_oneof![
        1 => Just(0.0f32),
        2 => 0.5f32..4.0,
        6 => 4.0f32..40.0,
    ]
}

#[derive(Clone, Copy, Debug)]
enum Indent {
    None,
    Same,
    Extra(u32),
    Columns(u32),
    Markdown,
}

impl Indent {
    fn callback(self) -> Arc<dyn WrapIndent> {
        match self {
            Self::None => Arc::new(IndentAdjustment::NoIndent),
            Self::Same => Arc::new(IndentAdjustment::SameIndent),
            Self::Extra(columns) => Arc::new(IndentAdjustment::ExtraColumns(columns)),
            Self::Columns(columns) => Arc::new(IndentAdjustment::Columns(columns)),
            Self::Markdown => Arc::new(MarkdownHangingIndent::default()),
        }
    }
}

fn indent() -> impl Strategy<Value = Indent> {
    prop_oneof![
        Just(Indent::None),
        Just(Indent::Same),
        (0u32..6).prop_map(Indent::Extra),
        (0u32..6).prop_map(Indent::Columns),
        Just(Indent::Markdown),
    ]
}

/// A replacement as random numbers; turned into a valid edit by `build`.
type RawEdit = (usize, usize, String);

fn raw_edits() -> impl Strategy<Value = Vec<RawEdit>> {
    let inserted = prop::collection::vec(text_char(), 0..12)
        .prop_map(|chars| chars.into_iter().collect::<String>());
    prop::collection::vec((any::<usize>(), 0usize..12, inserted), 1..4)
}

#[derive(Clone, Debug)]
enum Operation {
    Edit(Vec<RawEdit>),
    TabSize(NonZeroU32),
    Width(Option<f32>),
    /// Change the width through a job, run `rows` rows of it, and edit
    /// meanwhile before finishing it.
    DeferredWidth(Option<f32>, u32, Vec<RawEdit>),
    Refresh(u32, u32),
}

fn operation() -> impl Strategy<Value = Operation> {
    let width = prop_oneof![1 => Just(None), 5 => wrap_width().prop_map(Some)];
    prop_oneof![
        8 => raw_edits().prop_map(Operation::Edit),
        1 => tab_size().prop_map(Operation::TabSize),
        2 => width.clone().prop_map(Operation::Width),
        2 => (width, 1u32..8, raw_edits())
            .prop_map(|(width, rows, edits)| Operation::DeferredWidth(width, rows, edits)),
        1 => (0u32..20, 0u32..20).prop_map(|(a, b)| Operation::Refresh(a.min(b), a.max(b))),
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

// ----- helpers ----------------------------------------------------------

/// A buffer and every layer over it.
struct Layers {
    buffer: Buffer,
    replace_map: ReplaceMap,
    tab_map: TabMap,
    tab_size: NonZeroU32,
    wrap_map: WrapMap,
    snapshot: WrapSnapshot,
}

impl Layers {
    fn new(text: &str, tab_size: NonZeroU32, width: Option<f32>, indent: Indent) -> Self {
        let buffer = Buffer::new(text);
        let (replace_map, replace) = ReplaceMap::new(buffer.snapshot());
        let (tab_map, tabs) = TabMap::new(replace, tab_size);
        let (wrap_map, snapshot) =
            WrapMap::with_indent(tabs, Box::new(Columns), indent.callback(), width);
        Self {
            buffer,
            replace_map,
            tab_map,
            tab_size,
            wrap_map,
            snapshot,
        }
    }

    /// Edits the buffer and syncs every layer; checks the reported wrap
    /// edits against the texts.
    fn edit(&mut self, raw: &[RawEdit]) -> Result<(), TestCaseError> {
        let current = self.buffer.text().to_string();
        let summary = self.buffer.edit(build(&current, raw)).expect("valid edits");
        let (replace, replace_edits) = self.replace_map.sync(self.buffer.snapshot(), &summary);
        let (tabs, tab_edits) = self.tab_map.sync(replace, replace_edits, self.tab_size);
        let (snapshot, wrap_edits) = self.wrap_map.sync(tabs, tab_edits);
        prop_assert_eq!(
            replay(&self.snapshot.text(), &snapshot.text(), &wrap_edits),
            snapshot.text()
        );
        self.snapshot = snapshot;
        Ok(())
    }
}

fn tabs_of(text: &str, tab_size: NonZeroU32) -> TabSnapshot {
    let (_, replace) = ReplaceMap::new(TextSnapshot::new(text));
    TabMap::new(replace, tab_size).1
}

fn wrap(tabs: &TabSnapshot, width: Option<f32>, indent: Indent) -> WrapSnapshot {
    WrapMap::with_indent(tabs.clone(), Box::new(Columns), indent.callback(), width).1
}

/// The continuation indent of every row (0 for the first row of a tab
/// row).
fn row_indents(snapshot: &WrapSnapshot) -> Vec<u32> {
    snapshot
        .row_infos(WrapRow(0))
        .enumerate()
        .map(|(row, info)| {
            if info.soft_wrapped {
                snapshot
                    .soft_wrap_indent(WrapRow(row as u32 - 1))
                    .expect("a soft-wrapped row follows a soft wrap")
            } else {
                0
            }
        })
        .collect()
}

/// What a view would draw and hit-test: every row with its info.
fn rows_of(snapshot: &WrapSnapshot) -> Vec<(String, u32, bool)> {
    let text = snapshot.text();
    text.split('\n')
        .zip(snapshot.row_infos(WrapRow(0)))
        .map(|(row, info)| (row.to_owned(), info.tab_row, info.soft_wrapped))
        .collect()
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

/// Applies row edits to `old_text`, taking the new rows from `new_text`
/// (edits are whole rows; an end past the last row is the end of the
/// text).
fn replay(old_text: &str, new_text: &str, edits: &[WrapEdit]) -> String {
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

fn check_rows(snapshot: &WrapSnapshot, width: f32, tab_text: &str) -> Result<(), TestCaseError> {
    let text = snapshot.text();
    let indents = row_indents(snapshot);
    let rows: Vec<&str> = text.split('\n').collect();
    prop_assert_eq!(rows.len(), indents.len());
    let infos: Vec<_> = snapshot.row_infos(WrapRow(0)).collect();

    let mut joined = String::new();
    for (row, (&line, &indent)) in rows.iter().zip(&indents).enumerate() {
        prop_assert_eq!(snapshot.line_len(WrapRow(row as u32)) as usize, line.len());
        let (prefix, content) = line.split_at((indent as usize).min(line.len()));
        prop_assert!(
            prefix.bytes().all(|b| b == b' '),
            "indent of row {row}: {line:?}"
        );
        prop_assert_eq!(prefix.len(), indent as usize, "row {}: {:?}", row, line);
        if row > 0 && !infos[row].soft_wrapped {
            joined.push('\n');
        }
        joined.push_str(content);

        let row_width: f32 = line.chars().map(Columns::width).sum();
        let graphemes = content.graphemes(true).count();
        prop_assert!(
            row_width <= width + 1e-3 || graphemes == 1,
            "row {row} {line:?} is {row_width} wide at {width}, with {graphemes} graphemes"
        );
        if row + 1 < rows.len() && infos[row + 1].soft_wrapped {
            prop_assert!(!content.is_empty(), "empty soft-wrapped row {row}");
        }
    }
    prop_assert_eq!(joined, tab_text);
    Ok(())
}

fn check_same_rows(actual: &WrapSnapshot, expected: &WrapSnapshot) -> Result<(), TestCaseError> {
    prop_assert_eq!(rows_of(actual), rows_of(expected));
    prop_assert_eq!(actual.max_point(), expected.max_point());
    // As upstream, summaries taken from the tab layer count rows without
    // tab expansion, so the longest row is exact only without tabs.
    let tab_size = expected.tab_snapshot().tab_size().get();
    if tab_size == 1
        || !expected
            .tab_snapshot()
            .replace_snapshot()
            .text()
            .contains('\t')
    {
        prop_assert_eq!(actual.longest_row(), expected.longest_row());
    }
    let rows = expected.max_point().row().0;
    for row in 0..=rows {
        prop_assert_eq!(
            actual.soft_wrap_indent(WrapRow(row)),
            expected.soft_wrap_indent(WrapRow(row))
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn rows_fit_and_join_to_the_tab_text(
        text in text(),
        tab_size in tab_size(),
        width in wrap_width(),
        indent in indent(),
    ) {
        let tabs = tabs_of(&text, tab_size);
        let snapshot = wrap(&tabs, Some(width), indent);
        check_rows(&snapshot, width, &tabs.text())?;

        // Chunks of any row range are the slice of the text, in chunks of
        // at most 128 bytes.
        let full = snapshot.text();
        let rows: Vec<&str> = full.split('\n').collect();
        let max_row = snapshot.max_point().row().0;
        for start in 0..=max_row {
            let end = (start + 3).min(max_row + 1);
            let chunks: String = snapshot
                .chunks(WrapRow(start)..WrapRow(end))
                .inspect(|chunk| assert!(chunk.text.len() <= 128))
                .map(|chunk| chunk.text)
                .collect();
            let mut expected = rows[start as usize..end as usize].join("\n");
            if end <= max_row {
                expected.push('\n');
            }
            prop_assert_eq!(chunks, expected, "rows {}..{}", start, end);
        }

        // Without a width, the wrap layer is the tab layer.
        let unwrapped = wrap(&tabs, None, indent);
        prop_assert_eq!(unwrapped.text(), tabs.text());
    }

    #[test]
    fn conversions_round_trip(
        text in text(),
        tab_size in tab_size(),
        width in wrap_width(),
        indent in indent(),
        probes in prop::collection::vec((0u32..60, 0u32..60), 0..30),
    ) {
        let tabs = tabs_of(&text, tab_size);
        let snapshot = wrap(&tabs, Some(width), indent);
        let indents = row_indents(&snapshot);
        let wrap_text = snapshot.text();
        let wrap_rows: Vec<&str> = wrap_text.split('\n').collect();

        // Every valid tab position (not inside an expanded tab) maps to a
        // valid wrap position and back.
        let tab_text = tabs.text();
        let mut tab_points = Vec::new();
        for (row, line) in tab_text.split('\n').enumerate() {
            for (column, _) in line.char_indices().chain([(line.len(), ' ')]) {
                let point = TabPoint::new(row as u32, column as u32);
                if tabs.clip_point(point, Bias::Left) == point {
                    tab_points.push(point);
                }
            }
        }
        let mut cursor = snapshot.wrap_point_cursor();
        for &tab_point in &tab_points {
            let point = snapshot.tab_point_to_wrap_point(tab_point);
            prop_assert_eq!(cursor.map(tab_point), point);
            prop_assert_eq!(snapshot.to_tab_point(point), tab_point);
            prop_assert_eq!(snapshot.clip_point(point, Bias::Right), point);
            prop_assert!(point.column() >= indents[point.row().0 as usize]);
        }

        // Clipping any point gives a valid one that round-trips.
        let max_row = snapshot.max_point().row().0;
        for (row, column) in probes {
            let point = WrapPoint::new(WrapRow(row.min(max_row + 1)), column);
            for bias in [Bias::Left, Bias::Right] {
                let clipped = snapshot.clip_point(point, bias);
                let row = clipped.row().0;
                prop_assert!(row <= max_row);
                let line = wrap_rows[row as usize];
                prop_assert!(clipped.column() as usize <= line.len(), "{:?}", clipped);
                prop_assert!(line.is_char_boundary(clipped.column() as usize));
                prop_assert!(clipped.column() >= indents[row as usize], "{:?} {:?}", point, clipped);
                prop_assert_eq!(
                    snapshot.tab_point_to_wrap_point(snapshot.to_tab_point(clipped)),
                    clipped
                );
            }
        }

        // Buffer positions round-trip through the wrap layer.
        let buffer = snapshot.buffer();
        for offset in (0..=buffer.len()).filter(|&offset| {
            text.is_char_boundary(offset) && !buffer.is_inside_crlf(offset)
        }) {
            let point = buffer.offset_to_point(offset).expect("a char boundary");
            let wrap_point = snapshot.buffer_point_to_wrap_point(point, Bias::Left);
            prop_assert_eq!(snapshot.wrap_point_to_buffer_point(wrap_point, Bias::Left), point);
        }
    }

    #[test]
    fn incremental_sync_equals_rebuild(
        text in text(),
        tab_size in tab_size(),
        width in prop::option::weighted(0.9, wrap_width()),
        indent in indent(),
        operations in prop::collection::vec(operation(), 1..8),
    ) {
        let mut layers = Layers::new(&text, tab_size, width, indent);
        let mut width = width;
        for operation in operations {
            let old_text = layers.snapshot.text();
            // `None`: the edits were checked on the way.
            let synced = match operation {
                Operation::Edit(raw) => {
                    layers.edit(&raw)?;
                    None
                }
                Operation::TabSize(size) => {
                    layers.tab_size = size;
                    let (tabs, tab_edits) =
                        layers.tab_map.sync(layers.replace_map.snapshot(), Vec::new(), size);
                    Some(layers.wrap_map.sync(tabs, tab_edits))
                }
                Operation::Width(new_width) => {
                    width = new_width;
                    Some(layers.wrap_map.set_wrap_width(width))
                }
                Operation::DeferredWidth(new_width, rows, raw) => {
                    width = new_width;
                    match layers.wrap_map.defer_wrap_width(width) {
                        Some(mut job) => {
                            prop_assert!(layers.wrap_map.is_rewrapping());
                            let mut wrapper = LineWrapper::new(Box::new(Columns));
                            job.run(&mut wrapper, rows);
                            layers.edit(&raw)?;
                            prop_assert!(layers.snapshot.is_interpolated());
                            let old_text = layers.snapshot.text();
                            let (finished, edits) =
                                layers.wrap_map.finish_rewrap(job).expect("the current job");
                            prop_assert_eq!(
                                replay(&old_text, &finished.text(), &edits),
                                finished.text()
                            );
                            layers.snapshot = finished;
                        }
                        None => prop_assert!(!layers.wrap_map.is_rewrapping()),
                    }
                    None
                }
                Operation::Refresh(start, end) => Some(layers.wrap_map.refresh_rows(start..end)),
            };
            prop_assert!(!layers.wrap_map.is_rewrapping());
            if let Some((new_snapshot, edits)) = synced {
                prop_assert_eq!(
                    replay(&old_text, &new_snapshot.text(), &edits),
                    new_snapshot.text()
                );
                layers.snapshot = new_snapshot;
            }

            let rebuilt = wrap(layers.snapshot.tab_snapshot(), width, indent);
            check_same_rows(&layers.snapshot, &rebuilt)?;
            if let Some(width) = width {
                check_rows(&layers.snapshot, width, &layers.snapshot.tab_snapshot().text())?;
            }
        }
    }

    #[test]
    fn width_change_and_back(
        text in text(),
        tab_size in tab_size(),
        first in wrap_width(),
        second in prop::option::weighted(0.8, wrap_width()),
        indent in indent(),
    ) {
        let tabs = tabs_of(&text, tab_size);
        let (mut wrap_map, original) =
            WrapMap::with_indent(tabs, Box::new(Columns), indent.callback(), Some(first));
        let (changed, edits) = wrap_map.set_wrap_width(second);
        prop_assert_eq!(replay(&original.text(), &changed.text(), &edits), changed.text());
        let (back, edits) = wrap_map.set_wrap_width(Some(first));
        prop_assert_eq!(replay(&changed.text(), &back.text(), &edits), back.text());
        check_same_rows(&back, &original)?;

        // A new measure with the same widths changes nothing either.
        let (remeasured, _) = wrap_map.set_measure(Box::new(Columns));
        check_same_rows(&remeasured, &original)?;
    }
}

/// A re-wrap job runs on another thread with its own wrapper while the map
/// keeps syncing edits.
#[test]
fn rewrap_job_on_another_thread() {
    let mut buffer = Buffer::new(&"lorem ipsum dolor sit amet, consectetur\n".repeat(500));
    let tab_size = NonZeroU32::new(4).expect("not zero");
    let (mut replace_map, replace) = ReplaceMap::new(buffer.snapshot());
    let (mut tab_map, tabs) = TabMap::new(replace, tab_size);
    let (mut wrap_map, _) = WrapMap::new(tabs, Box::new(MonospaceMeasure::new(7.5)), Some(300.0));

    let mut job = wrap_map.defer_wrap_width(Some(150.0)).expect("a new width");
    let worker = std::thread::spawn(move || {
        let mut wrapper = LineWrapper::new(Box::new(MonospaceMeasure::new(7.5)));
        while !job.run(&mut wrapper, 64) {}
        job
    });

    let summary = buffer
        .edit([(0..0, "edited while wrapping ")])
        .expect("valid");
    let (replace, replace_edits) = replace_map.sync(buffer.snapshot(), &summary);
    let (tabs, tab_edits) = tab_map.sync(replace, replace_edits, tab_size);
    let (interpolated, _) = wrap_map.sync(tabs.clone(), tab_edits);
    assert!(interpolated.is_interpolated());

    let job = worker.join().expect("the worker");
    assert!(job.is_done());
    let (finished, _) = wrap_map.finish_rewrap(job).expect("the current job");
    let rebuilt = WrapMap::new(tabs, Box::new(MonospaceMeasure::new(7.5)), Some(150.0)).1;
    assert_eq!(finished.text(), rebuilt.text());
    assert!(
        finished
            .text()
            .starts_with("edited while \nwrapping lorem \n")
    );
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests of the editor against naive models.
//!
//! - **History:** any sequence of edits, undos, redos, motions, selection
//!   changes, input-method calls and pauses keeps the text equal to a naive
//!   `String` model (every edit is recomputed on a `String` from the
//!   selections before it, with whole-string Unicode segmentation; undo and
//!   redo must return to the exact text of each recorded state), and undoing
//!   everything returns the original bytes, CRLF and lone CR included.
//! - **Selections** stay sorted, merged and inside the text, on character
//!   boundaries and never inside a CRLF, after every operation.
//! - **Multi-cursor:** an edit at many cursors equals the same edit applied
//!   at each cursor alone, from the last cursor to the first.
//! - **Input method:** UTF-16 replacements with arbitrary (out of range,
//!   inverted, mid-surrogate, mid-CRLF) ranges equal a naive model of the
//!   clipping rules, never split a character, and report valid UTF-16
//!   ranges.
//!
//! `PROPTEST_CASES` overrides the case counts.

use std::ops::Range;
use std::time::Duration;

use gv_editor::boundaries::{next_grapheme_boundary, prev_grapheme_boundary};
use gv_editor::{
    DeleteUnit, EditKind, Editor, EditorConfig, HistoryConfig, ManualClock, Motion, SelectionSpec,
    UndoOutcome,
};
use gv_text::{Buffer, OffsetUtf16};
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::{Config, TestCaseError};
use unicode_segmentation::UnicodeSegmentation;

fn config(default_cases: u32) -> Config {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(default_cases);
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

// ----- generators -----------------------------------------------------------

/// Pieces of text that exercise grapheme clusters, words, CRLF and lone CR,
/// tabs, surrogate pairs and Markdown punctuation.
const PIECES: &[&str] = &[
    "a",
    "word",
    "can't",
    "3.14",
    " ",
    "  ",
    "\t",
    "\n",
    "\r\n",
    "\r",
    "é",
    "e\u{301}",
    "😀",
    "🇩🇪",
    "👨\u{200d}👩\u{200d}👧",
    "日本",
    "--",
    "...",
    "- [ ] ",
    "x_y",
];

fn arb_text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 0..max).prop_map(|pieces| pieces.concat())
}

fn arb_insert() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(PIECES), 1..3).prop_map(|pieces| pieces.concat())
}

fn arb_unit() -> impl Strategy<Value = DeleteUnit> {
    prop_oneof![
        Just(DeleteUnit::Grapheme),
        Just(DeleteUnit::Word),
        Just(DeleteUnit::Line),
    ]
}

fn arb_motion() -> impl Strategy<Value = Motion> {
    prop_oneof![
        Just(Motion::Left),
        Just(Motion::Right),
        Just(Motion::WordLeft),
        Just(Motion::WordRight),
        Just(Motion::LineStart),
        Just(Motion::SmartLineStart),
        Just(Motion::LineEnd),
        Just(Motion::DocumentStart),
        Just(Motion::DocumentEnd),
        Just(Motion::Up),
        Just(Motion::Down),
        (1u32..4).prop_map(Motion::UpRows),
        (1u32..4).prop_map(Motion::DownRows),
    ]
}

#[derive(Clone, Debug)]
enum Op {
    Type(String),
    Paste(String),
    Newline,
    DeleteBackward(DeleteUnit),
    DeleteForward(DeleteUnit),
    Undo,
    Redo,
    Move(Motion, bool),
    AddCursor(Index),
    Select(Index, Index),
    SelectAll,
    Collapse,
    AddCursorBelow,
    Columns(Index, Index),
    Wait(u64),
    BreakGroup,
    MarkSaved,
    ImeMark(Option<(usize, usize)>, String),
    ImeCommit(Option<(usize, usize)>, String),
    ImeUnmark,
}

fn arb_ime_range() -> impl Strategy<Value = Option<(usize, usize)>> {
    prop_oneof![
        2 => Just(None),
        1 => (0usize..40, 0usize..40).prop_map(Some),
    ]
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => arb_insert().prop_map(Op::Type),
        1 => arb_insert().prop_map(Op::Paste),
        1 => Just(Op::Newline),
        3 => arb_unit().prop_map(Op::DeleteBackward),
        2 => arb_unit().prop_map(Op::DeleteForward),
        3 => Just(Op::Undo),
        2 => Just(Op::Redo),
        3 => (arb_motion(), any::<bool>()).prop_map(|(m, e)| Op::Move(m, e)),
        2 => any::<Index>().prop_map(Op::AddCursor),
        1 => (any::<Index>(), any::<Index>()).prop_map(|(a, b)| Op::Select(a, b)),
        1 => Just(Op::SelectAll),
        1 => Just(Op::Collapse),
        1 => Just(Op::AddCursorBelow),
        1 => (any::<Index>(), any::<Index>()).prop_map(|(a, b)| Op::Columns(a, b)),
        2 => prop_oneof![Just(50u64), Just(400), Just(1500)].prop_map(Op::Wait),
        1 => Just(Op::BreakGroup),
        1 => Just(Op::MarkSaved),
        1 => (arb_ime_range(), "[a-z]{0,2}|[かな😀]{1,2}").prop_map(|(r, s)| Op::ImeMark(r, s)),
        1 => (arb_ime_range(), "[a-z]{0,2}|[仮名😀]{1,2}").prop_map(|(r, s)| Op::ImeCommit(r, s)),
        1 => Just(Op::ImeUnmark),
    ]
}

// ----- naive models -----------------------------------------------------------

fn graphemes(text: &str) -> Vec<usize> {
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect()
}

fn prev_grapheme(text: &str, offset: usize) -> usize {
    graphemes(text)
        .into_iter()
        .filter(|&b| b < offset)
        .max()
        .unwrap_or(0)
}

fn next_grapheme(text: &str, offset: usize) -> usize {
    graphemes(text)
        .into_iter()
        .filter(|&b| b > offset)
        .min()
        .unwrap_or(text.len())
}

fn snap_grapheme(text: &str, offset: usize) -> usize {
    graphemes(text)
        .into_iter()
        .filter(|&b| b <= offset)
        .max()
        .unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(segment: &str) -> Class {
    if segment.chars().all(char::is_whitespace) {
        Class::Space
    } else if segment.chars().any(char::is_alphanumeric) {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Word end after `offset`, segmenting the whole rest of the text at once.
fn next_word(text: &str, offset: usize) -> usize {
    let mut found: Option<(Class, usize)> = None;
    for (index, segment) in text[offset..].split_word_bound_indices() {
        let c = class(segment);
        let end = offset + index + segment.len();
        match found {
            None if c == Class::Space => {}
            None => found = Some((c, end)),
            Some((Class::Punct, _)) if c == Class::Punct => found = Some((c, end)),
            Some(_) => break,
        }
    }
    snap_grapheme(text, found.map_or(text.len(), |(_, end)| end))
}

/// Word start before `offset`, segmenting the whole text before it at once.
fn prev_word(text: &str, offset: usize) -> usize {
    let mut found: Option<(Class, usize)> = None;
    for (index, segment) in text[..offset].split_word_bound_indices().rev() {
        let c = class(segment);
        match found {
            None if c == Class::Space => {}
            None => found = Some((c, index)),
            Some((Class::Punct, _)) if c == Class::Punct => found = Some((c, index)),
            Some(_) => break,
        }
    }
    snap_grapheme(text, found.map_or(0, |(_, start)| start))
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind(['\n', '\r']).map_or(0, |i| i + 1)
}

fn line_end(text: &str, offset: usize) -> usize {
    text[offset..]
        .find(['\n', '\r'])
        .map_or(text.len(), |i| offset + i)
}

fn replace_all(text: &str, ranges: &[Range<usize>], new: &[&str]) -> String {
    let mut out = text.to_owned();
    for (range, new) in ranges.iter().zip(new).rev() {
        out.replace_range(range.clone(), new);
    }
    out
}

fn merge_ranges(ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if range.is_empty() {
            continue;
        }
        match merged.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// The text a delete command should produce.
fn naive_delete(
    text: &str,
    selections: &[Range<usize>],
    backward: bool,
    unit: DeleteUnit,
) -> String {
    let ranges: Vec<Range<usize>> = selections
        .iter()
        .map(|range| {
            if !range.is_empty() {
                return range.clone();
            }
            let head = range.start;
            if backward {
                let start = match unit {
                    DeleteUnit::Grapheme => prev_grapheme(text, head),
                    DeleteUnit::Word => prev_word(text, head),
                    DeleteUnit::Line => {
                        let start = line_start(text, head);
                        if start == head {
                            prev_grapheme(text, head)
                        } else {
                            start
                        }
                    }
                };
                start..head
            } else {
                let end = match unit {
                    DeleteUnit::Grapheme => next_grapheme(text, head),
                    DeleteUnit::Word => next_word(text, head),
                    DeleteUnit::Line => {
                        let end = line_end(text, head);
                        if end == head {
                            next_grapheme(text, head)
                        } else {
                            end
                        }
                    }
                };
                head..end
            }
        })
        .collect();
    let merged = merge_ranges(ranges);
    let empty = vec![""; merged.len()];
    replace_all(text, &merged, &empty)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Byte offset of a UTF-16 offset in `text`, clamped; inside a surrogate
/// pair, the character start (`left`) or end.
fn utf16_to_byte(text: &str, utf16: usize, left: bool) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        if units >= utf16 {
            return index;
        }
        let next = units + c.len_utf16();
        if next > utf16 {
            return if left { index } else { index + c.len_utf8() };
        }
        units = next;
    }
    text.len()
}

fn byte_to_utf16(text: &str, byte: usize) -> usize {
    utf16_len(&text[..byte])
}

fn inside_crlf(text: &str, offset: usize) -> bool {
    let bytes = text.as_bytes();
    offset > 0 && offset < bytes.len() && bytes[offset - 1] == b'\r' && bytes[offset] == b'\n'
}

/// The clipping rules of `Editor::clip_utf16_range`, on a `String`.
fn naive_clip(text: &str, range: Range<usize>) -> Range<usize> {
    if range.start == range.end {
        let mut at = utf16_to_byte(text, range.start, true);
        if inside_crlf(text, at) {
            at -= 1;
        }
        return at..at;
    }
    let (start, end) = (range.start.min(range.end), range.start.max(range.end));
    let mut start = utf16_to_byte(text, start, true);
    let mut end = utf16_to_byte(text, end, false);
    if inside_crlf(text, start) {
        start -= 1;
    }
    if inside_crlf(text, end) {
        end += 1;
    }
    start..end
}

fn new_editor(text: &str, clock: &ManualClock) -> Editor {
    let config = EditorConfig {
        history: HistoryConfig {
            max_entries: usize::MAX,
            max_bytes: usize::MAX,
            ..HistoryConfig::default()
        },
        ..EditorConfig::default()
    };
    Editor::with_config(Buffer::new(text), config, Box::new(clock.clone()))
}

fn check_selections(editor: &Editor) -> Result<(), TestCaseError> {
    editor
        .selections()
        .check(editor.text())
        .map_err(TestCaseError::fail)?;
    for range in editor.marked_ranges() {
        prop_assert!(
            editor.text().check_range(range.clone()).is_ok(),
            "{range:?}"
        );
    }
    Ok(())
}

// ----- (a) history and (b) selections ----------------------------------------------

fn run_ops(original: &str, ops: &[Op]) -> Result<(), TestCaseError> {
    let clock = ManualClock::new();
    let mut editor = new_editor(original, &clock);
    // states[i]: the text with i undo steps applied.
    let mut states = vec![original.to_owned()];
    for op in ops {
        let before = editor.text().to_string();
        let undo_before = editor.history().undo_len();
        let selections: Vec<Range<usize>> = editor.selections().ranges().collect();
        let len = before.len();
        // The text the op must produce, where a naive model exists.
        let expected: Option<String> = match op {
            Op::Type(text) | Op::Paste(text) => Some(replace_all(
                &before,
                &selections,
                &vec![text.as_str(); selections.len()],
            )),
            Op::Newline => Some(replace_all(
                &before,
                &selections,
                &vec!["\n"; selections.len()],
            )),
            Op::DeleteBackward(unit) => Some(naive_delete(&before, &selections, true, *unit)),
            Op::DeleteForward(unit) => Some(naive_delete(&before, &selections, false, *unit)),
            _ => None,
        };
        match op {
            Op::Type(text) => {
                editor.insert_at_selections(text);
            }
            Op::Paste(text) => {
                editor.paste(text);
            }
            Op::Newline => {
                editor.newline();
            }
            Op::DeleteBackward(unit) => {
                editor.delete_backward(*unit);
            }
            Op::DeleteForward(unit) => {
                editor.delete_forward(*unit);
            }
            Op::Undo => {
                let _ = editor.undo();
            }
            Op::Redo => {
                editor.redo();
            }
            Op::Move(motion, extend) => editor.move_selections(*motion, *extend),
            Op::AddCursor(index) => {
                editor.add_cursor(index.index(len + 1));
            }
            Op::Select(a, b) => {
                editor.set_selections(&[SelectionSpec::new(a.index(len + 1), b.index(len + 1))], 0)
            }
            Op::SelectAll => editor.select_all(),
            Op::Collapse => editor.collapse_selections(),
            Op::AddCursorBelow => {
                editor.add_cursor_below();
            }
            Op::Columns(a, b) => editor.select_columns(a.index(len + 1), b.index(len + 1)),
            Op::Wait(ms) => clock.advance(Duration::from_millis(*ms)),
            Op::BreakGroup => editor.break_undo_group(),
            Op::MarkSaved => editor.mark_saved(),
            Op::ImeMark(range, text) => {
                editor.replace_and_mark_text_in_range_utf16(range.map(|(a, b)| a..b), text, None)
            }
            Op::ImeCommit(range, text) => {
                editor.replace_text_in_range_utf16(range.map(|(a, b)| a..b), text);
            }
            Op::ImeUnmark => editor.unmark_text(),
        }
        let after = editor.text().to_string();
        if let Some(expected) = expected {
            prop_assert_eq!(
                &after,
                &expected,
                "after {:?} on {:?} at {:?}",
                op,
                before,
                selections
            );
        }
        check_selections(&editor)?;
        let undo = editor.history().undo_len();
        let redo = editor.history().redo_len();
        match op {
            Op::Undo | Op::Redo => {
                prop_assert!(states.len() > undo, "no state {undo} after {op:?}");
                prop_assert_eq!(&after, &states[undo], "after {:?}", op);
            }
            // A new step, even if the edits add up to the same text (two
            // cursors moving a line break, say).
            _ if after != before || undo > undo_before => {
                prop_assert_eq!(redo, 0);
                states.truncate(undo);
                states.push(after.clone());
            }
            _ => {
                prop_assert!(states.len() > undo, "no state {undo} after {op:?}");
                prop_assert_eq!(&after, &states[undo], "after {:?}", op);
                states.truncate(undo + 1 + redo);
            }
        }
        prop_assert!(states.len() >= undo + 1 + redo);
    }
    // Undo everything: the original bytes, exactly.
    loop {
        match editor.undo() {
            UndoOutcome::Undone => check_selections(&editor)?,
            UndoOutcome::NothingToUndo => break,
            UndoOutcome::StoppedAtSavePoint => {}
        }
    }
    prop_assert_eq!(editor.text().to_string(), original);
    // And redo everything: the last recorded state.
    while editor.redo() {
        check_selections(&editor)?;
    }
    let last = states.last().cloned().unwrap_or_default();
    prop_assert_eq!(editor.text().to_string(), last);
    Ok(())
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn history_matches_a_naive_model(
        original in arb_text(16),
        ops in prop::collection::vec(arb_op(), 1..48),
    ) {
        run_ops(&original, &ops)?;
    }

    #[test]
    fn crlf_documents_round_trip(
        lines in prop::collection::vec(("[a-z ]{0,6}", prop_oneof![Just("\r\n"), Just("\r"), Just("\n")]), 1..8),
        ops in prop::collection::vec(arb_op(), 1..32),
    ) {
        let original: String = lines.iter().map(|(line, end)| format!("{line}{end}")).collect();
        run_ops(&original, &ops)?;
    }
}

// ----- (c) multi-cursor = one cursor at a time, from the end -------------------------

#[derive(Clone, Debug)]
enum MultiOp {
    Insert(String),
    Backspace,
    Delete,
}

fn arb_multi_op() -> impl Strategy<Value = MultiOp> {
    prop_oneof![
        2 => arb_insert().prop_map(MultiOp::Insert),
        1 => Just(MultiOp::Backspace),
        1 => Just(MultiOp::Delete),
    ]
}

fn apply_multi(editor: &mut Editor, op: &MultiOp) {
    match op {
        MultiOp::Insert(text) => {
            editor.insert_at_selections(text);
        }
        MultiOp::Backspace => {
            editor.delete_backward(DeleteUnit::Grapheme);
        }
        MultiOp::Delete => {
            editor.delete_forward(DeleteUnit::Grapheme);
        }
    }
}

proptest! {
    #![proptest_config(config(512))]

    #[test]
    fn multi_cursor_equals_one_at_a_time_from_the_end(
        text in arb_text(24),
        picks in prop::collection::vec((any::<Index>(), any::<Index>(), any::<bool>()), 1..8),
        op in arb_multi_op(),
    ) {
        // Selections on grapheme boundaries, made disjoint by the editor.
        let boundaries = graphemes(&text);
        let specs: Vec<SelectionSpec> = picks
            .iter()
            .map(|(a, b, empty)| {
                let a = boundaries[a.index(boundaries.len())];
                let b = if *empty { a } else { boundaries[b.index(boundaries.len())] };
                SelectionSpec::new(a, b)
            })
            .collect();
        let clock = ManualClock::new();
        let mut multi = new_editor(&text, &clock);
        multi.set_selections(&specs, 0);
        let selections: Vec<SelectionSpec> = multi
            .selections()
            .iter()
            .map(|s| SelectionSpec::new(s.tail(), s.head()))
            .collect();
        let multi_original = multi.snapshot();
        apply_multi(&mut multi, &op);

        // The same per-cursor edits, computed on the original text, applied
        // one at a time from the last cursor to the first. (Going through
        // the selections of a single-cursor editor instead would differ
        // where a later edit joins a CR and an LF around a selection end:
        // that end is then clipped out of the new CRLF.)
        let original = multi_original.clone();
        let mut single = new_editor(&text, &clock);
        for spec in selections.iter().rev() {
            let range = spec.tail.min(spec.head)..spec.tail.max(spec.head);
            let (range, new) = match &op {
                MultiOp::Insert(new) => (range, new.as_str()),
                MultiOp::Backspace if range.is_empty() => {
                    (prev_grapheme_boundary(&original, range.start)..range.start, "")
                }
                MultiOp::Delete if range.is_empty() => {
                    (range.start..next_grapheme_boundary(&original, range.start), "")
                }
                _ => (range, ""),
            };
            single.edit([(range, new)], EditKind::Command).map_err(|e| TestCaseError::fail(e.to_string()))?;
        }
        prop_assert_eq!(multi.text().to_string(), single.text().to_string());
        check_selections(&multi)?;
        // One undo step takes the multi-cursor edit back.
        multi.undo();
        prop_assert_eq!(multi.text().to_string(), text);
    }
}

// ----- (d) input method ---------------------------------------------------------------

#[derive(Clone, Debug)]
enum ImeOp {
    Mark(Option<(usize, usize)>, String, Option<(usize, usize)>),
    Commit(Option<(usize, usize)>, String),
    Unmark,
    Read(usize, usize),
}

fn arb_ime_text() -> impl Strategy<Value = String> {
    "[a-z]{0,2}|[かな😀🇩🇪é\r\n]{1,3}"
}

fn arb_ime_op() -> impl Strategy<Value = ImeOp> {
    let range = || prop_oneof![Just(None), (0usize..30, 0usize..30).prop_map(Some)];
    prop_oneof![
        3 => (range(), arb_ime_text(), range()).prop_map(|(r, t, s)| ImeOp::Mark(r, t, s)),
        2 => (range(), arb_ime_text()).prop_map(|(r, t)| ImeOp::Commit(r, t)),
        1 => Just(ImeOp::Unmark),
        2 => (0usize..30, 0usize..30).prop_map(|(a, b)| ImeOp::Read(a, b)),
    ]
}

/// The UTF-16 range is valid: in bounds and not splitting a surrogate pair.
fn check_utf16(editor: &Editor, range: &Range<usize>) -> Result<(), TestCaseError> {
    let text = editor.text();
    prop_assert!(range.start <= range.end);
    prop_assert!(
        text.utf16_to_range(OffsetUtf16(range.start)..OffsetUtf16(range.end))
            .is_ok(),
        "bad UTF-16 range {:?}",
        range
    );
    Ok(())
}

fn run_ime(original: &str, caret: Index, ops: &[ImeOp]) -> Result<(), TestCaseError> {
    let clock = ManualClock::new();
    let mut editor = new_editor(original, &clock);
    let boundaries: Vec<usize> = (0..=original.len())
        .filter(|&i| original.is_char_boundary(i) && !inside_crlf(original, i))
        .collect();
    editor.set_cursor(boundaries[caret.index(boundaries.len())]);
    for op in ops {
        let before = editor.text().to_string();
        let selection = editor.selections().primary().range();
        let marked = editor.marked_ranges().first().cloned();
        match op {
            ImeOp::Mark(range, text, selected) => {
                let target = match range {
                    Some((a, b)) => naive_clip(&before, *a..*b),
                    None => marked.unwrap_or(selection),
                };
                editor.replace_and_mark_text_in_range_utf16(
                    range.map(|(a, b)| a..b),
                    text,
                    selected.map(|(a, b)| a..b),
                );
                let expected = replace_all(&before, std::slice::from_ref(&target), &[text]);
                prop_assert_eq!(
                    editor.text().to_string(),
                    expected,
                    "{:?} on {:?}",
                    op,
                    before
                );
                if text.is_empty() {
                    prop_assert!(!editor.is_composing());
                } else {
                    let marked = target.start..target.start + text.len();
                    let after = editor.text().to_string();
                    prop_assert_eq!(
                        editor.marked_text_range_utf16(),
                        Some(
                            byte_to_utf16(&after, marked.start)..byte_to_utf16(&after, marked.end)
                        )
                    );
                    // A marked text that starts with LF after a CR joins a
                    // CRLF; carets stay out of it.
                    let low = marked.start - usize::from(inside_crlf(&after, marked.start));
                    let selection = editor.selections().primary().range();
                    prop_assert!(low <= selection.start && selection.end <= marked.end);
                }
            }
            ImeOp::Commit(range, text) => {
                let target = match range {
                    Some((a, b)) => naive_clip(&before, *a..*b),
                    None => marked.unwrap_or(selection),
                };
                editor.replace_text_in_range_utf16(range.map(|(a, b)| a..b), text);
                let expected = replace_all(&before, std::slice::from_ref(&target), &[text]);
                prop_assert_eq!(
                    editor.text().to_string(),
                    expected,
                    "{:?} on {:?}",
                    op,
                    before
                );
                prop_assert!(!editor.is_composing());
                let after = editor.text().to_string();
                let caret = target.start + text.len();
                let caret = caret - usize::from(inside_crlf(&after, caret));
                prop_assert_eq!(editor.selections().primary().head(), caret);
            }
            ImeOp::Unmark => {
                editor.unmark_text();
                prop_assert_eq!(editor.text().to_string(), before);
            }
            ImeOp::Read(a, b) => {
                let (text, range) = editor.text_for_range_utf16(*a..*b);
                check_utf16(&editor, &range)?;
                prop_assert_eq!(utf16_len(&text), range.end - range.start);
                let start = utf16_to_byte(&before, range.start, true);
                prop_assert_eq!(&before[start..start + text.len()], text.as_str());
            }
        }
        check_selections(&editor)?;
        check_utf16(&editor, &editor.selected_text_range_utf16().range)?;
        if let Some(range) = editor.marked_text_range_utf16() {
            check_utf16(&editor, &range)?;
        }
    }
    editor.unmark_text();
    while editor.undo() == UndoOutcome::Undone {}
    prop_assert_eq!(editor.text().to_string(), original);
    Ok(())
}

proptest! {
    #![proptest_config(config(512))]

    #[test]
    fn ime_ranges_never_split_characters(
        original in arb_text(12),
        caret in any::<Index>(),
        ops in prop::collection::vec(arb_ime_op(), 1..16),
    ) {
        run_ime(&original, caret, &ops)?;
    }

    /// With several cursors there is no naive model, but nothing may break:
    /// invariants hold, ranges are valid, and undo restores the text.
    #[test]
    fn multi_cursor_ime_keeps_invariants(
        original in arb_text(12),
        cursors in prop::collection::vec(any::<Index>(), 1..4),
        ops in prop::collection::vec(arb_ime_op(), 1..16),
    ) {
        let clock = ManualClock::new();
        let mut editor = new_editor(&original, &clock);
        for cursor in &cursors {
            editor.add_cursor(cursor.index(original.len() + 1));
        }
        for op in &ops {
            match op {
                ImeOp::Mark(range, text, selected) => editor.replace_and_mark_text_in_range_utf16(
                    range.map(|(a, b)| a..b),
                    text,
                    selected.map(|(a, b)| a..b),
                ),
                ImeOp::Commit(range, text) => {
                    editor.replace_text_in_range_utf16(range.map(|(a, b)| a..b), text);
                }
                ImeOp::Unmark => editor.unmark_text(),
                ImeOp::Read(a, b) => {
                    let (_, range) = editor.text_for_range_utf16(*a..*b);
                    check_utf16(&editor, &range)?;
                }
            }
            check_selections(&editor)?;
            if let Some(range) = editor.marked_text_range_utf16() {
                check_utf16(&editor, &range)?;
            }
        }
        editor.unmark_text();
        while editor.undo() == UndoOutcome::Undone {}
        prop_assert_eq!(editor.text().to_string(), original);
    }
}

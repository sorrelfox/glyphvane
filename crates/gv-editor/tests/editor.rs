// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Unit tests of the editor commands through the public API: typing,
//! deleting, multi-cursor edits, undo grouping and save points, movement,
//! selection commands, observers and the input-method interface.

// Inverted ranges stand for untrusted input-method ranges.
#![allow(clippy::reversed_empty_ranges)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gv_editor::{
    DeleteUnit, EditCause, EditKind, Editor, EditorEvent, HistoryConfig, Motion, SelectionGoal,
    SelectionSpec, UndoOutcome,
};
use gv_text::{Bias, Buffer, LineEnding, TextFormat};
use support::{editor, editor_with, render, type_chars};

const FAST: Duration = Duration::from_millis(100);
const SLOW: Duration = Duration::from_millis(1500);

/// Counts `Edited` events by cause.
fn count_edits(editor: &mut Editor) -> Rc<RefCell<Vec<EditCause>>> {
    let edits = Rc::new(RefCell::new(Vec::new()));
    let sink = edits.clone();
    editor.observe(move |event| {
        if let EditorEvent::Edited(edit) = event {
            sink.borrow_mut().push(edit.cause);
        }
    });
    edits
}

// ----- typing -----------------------------------------------------------------

#[test]
fn typing_at_one_and_many_cursors() {
    let (mut e, _) = editor("a|b\nc|d\n|");
    let edits = count_edits(&mut e);
    assert!(e.insert_at_selections("xy"));
    assert_eq!(render(&e), "axy|b\ncxy|d\nxy|");
    // One transaction for all cursors.
    assert_eq!(edits.borrow().len(), 1);
    assert!(!e.insert_at_selections(""));
    assert_eq!(edits.borrow().len(), 1);
}

#[test]
fn typing_replaces_selections() {
    let (mut e, _) = editor("{ab}c}de{f|");
    e.insert_at_selections("X");
    assert_eq!(render(&e), "X|cX|fX|");
}

#[test]
fn touching_selections_are_replaced_separately() {
    let (mut e, _) = editor("{ab}{cd}");
    assert_eq!(e.selections().len(), 2);
    e.insert_at_selections("-");
    assert_eq!(render(&e), "-|-|");
}

#[test]
fn newline_uses_the_buffer_line_ending() {
    let mut buffer = Buffer::new("a\r\nb");
    buffer.set_format(TextFormat {
        line_ending: LineEnding::CrLf,
        ..TextFormat::default()
    });
    let mut e = Editor::new(buffer);
    e.set_cursor(1);
    e.newline();
    assert_eq!(e.text().to_string(), "a\r\n\r\nb");
    assert_eq!(e.selections().primary().head(), 3);
}

#[test]
fn paste_per_selection() {
    let (mut e, _) = editor("|\n|\n|");
    assert!(!e.insert_per_selection(&["a", "b"], EditKind::Paste));
    assert!(e.insert_per_selection(&["a", "b", "c"], EditKind::Paste));
    assert_eq!(render(&e), "a|\nb|\nc|");
}

// ----- deleting ---------------------------------------------------------------

#[test]
fn backspace_by_grapheme() {
    let (mut e, _) = editor("x👨‍👩‍👧|y🇩🇪|e\u{301}|a\r\n|b");
    e.delete_backward(DeleteUnit::Grapheme);
    // `e` + combining accent is one cluster: both go, and two carets meet.
    assert_eq!(render(&e), "x|y|a|b");
    e.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(render(&e), "|b");
    // At the start of the text nothing happens.
    assert!(!e.delete_backward(DeleteUnit::Grapheme));
}

#[test]
fn backspace_takes_a_whole_crlf() {
    let (mut e, _) = editor("a\r\n|b");
    e.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(render(&e), "a|b");
}

#[test]
fn delete_forward_by_grapheme() {
    let (mut e, _) = editor("|🇩🇪x|\r\ny|");
    e.delete_forward(DeleteUnit::Grapheme);
    assert_eq!(render(&e), "|x|y|");
}

#[test]
fn delete_by_word() {
    let (mut e, _) = editor("foo bar|  baz");
    e.delete_backward(DeleteUnit::Word);
    assert_eq!(render(&e), "foo |  baz");
    e.delete_forward(DeleteUnit::Word);
    assert_eq!(render(&e), "foo |");
    e.delete_backward(DeleteUnit::Word);
    assert_eq!(render(&e), "|");
}

#[test]
fn delete_by_line() {
    let (mut e, _) = editor("one\r\ntw|o\nthree");
    e.delete_backward(DeleteUnit::Line);
    assert_eq!(render(&e), "one\r\n|o\nthree");
    // At the line start: the line break goes, joining the lines.
    e.delete_backward(DeleteUnit::Line);
    assert_eq!(render(&e), "one|o\nthree");
    e.delete_forward(DeleteUnit::Line);
    assert_eq!(render(&e), "one|\nthree");
    e.delete_forward(DeleteUnit::Line);
    assert_eq!(render(&e), "one|three");
}

#[test]
fn overlapping_deletes_merge() {
    // Both carets delete back to the start of `word`: one range.
    let (mut e, _) = editor("x word|s|");
    e.delete_backward(DeleteUnit::Word);
    assert_eq!(render(&e), "x |");
    assert_eq!(e.selections().len(), 1);
}

#[test]
fn delete_selections_and_carets_together() {
    let (mut e, _) = editor("{abc}d|ef");
    e.delete_forward(DeleteUnit::Grapheme);
    assert_eq!(render(&e), "|d|f");
}

// ----- generic edits ------------------------------------------------------------

#[test]
fn edits_move_selections_by_anchor() {
    let (mut e, _) = editor("ab|cd{ef}gh");
    let summary = e
        .edit(
            [(8..8, "!"), (2..2, "X"), (4..4, "Y"), (6..6, "Z")],
            EditKind::Command,
        )
        .unwrap()
        .unwrap();
    assert_eq!(summary.edits().len(), 4);
    // The caret moves after text inserted at it; the selection does not grow.
    assert_eq!(render(&e), "abX|cdY{ef}Zgh!");
}

#[test]
fn rejected_edits_change_nothing() {
    let (mut e, _) = editor("ab|cdef");
    let edits = count_edits(&mut e);
    assert!(
        e.edit([(0..3, "x"), (2..4, "y")], EditKind::Command)
            .is_err()
    );
    assert!(e.edit([(0..99, "x")], EditKind::Command).is_err());
    assert_eq!(render(&e), "ab|cdef");
    assert!(!e.history().can_undo());
    assert!(edits.borrow().is_empty());
}

// ----- undo -------------------------------------------------------------------

#[test]
fn quick_typing_is_one_step_per_word() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "hello world", FAST);
    assert_eq!(e.history().undo_len(), 2);
    assert_eq!(e.undo(), UndoOutcome::Undone);
    assert_eq!(render(&e), "hello |");
    assert_eq!(e.undo(), UndoOutcome::Undone);
    assert_eq!(render(&e), "|");
    assert_eq!(e.undo(), UndoOutcome::NothingToUndo);
}

#[test]
fn a_pause_starts_a_new_step() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "ab", FAST);
    type_chars(&mut e, &clock, "c", SLOW);
    type_chars(&mut e, &clock, "d", FAST);
    e.undo();
    assert_eq!(render(&e), "ab|");
}

#[test]
fn a_new_line_is_its_own_step() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "ab", FAST);
    clock.advance(FAST);
    e.newline();
    type_chars(&mut e, &clock, "  cd", FAST);
    e.undo();
    assert_eq!(render(&e), "ab\n  |");
    e.undo();
    assert_eq!(render(&e), "ab|");
}

#[test]
fn kinds_do_not_mix_and_moves_break_steps() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "abcd", FAST);
    clock.advance(FAST);
    e.delete_backward(DeleteUnit::Grapheme);
    clock.advance(FAST);
    e.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(e.history().undo_len(), 2);
    e.move_selections(Motion::Left, false);
    type_chars(&mut e, &clock, "x", FAST);
    assert_eq!(e.history().undo_len(), 3);
    e.undo();
    assert_eq!(render(&e), "a|b");
    e.undo();
    assert_eq!(render(&e), "abcd|");
}

#[test]
fn paste_and_commands_are_single_steps() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "ab", FAST);
    e.paste("cd");
    e.paste("ef");
    type_chars(&mut e, &clock, "gh", FAST);
    assert_eq!(e.history().undo_len(), 4);
}

#[test]
fn undo_and_redo_restore_selections() {
    let (mut e, clock) = editor("{one} {two}");
    clock.advance(FAST);
    e.insert_at_selections("1");
    assert_eq!(render(&e), "1| 1|");
    e.undo();
    assert_eq!(render(&e), "{one} {two}");
    assert!(e.redo());
    assert_eq!(render(&e), "1| 1|");
    assert!(!e.redo());
}

#[test]
fn a_new_edit_clears_redo() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "ab", FAST);
    e.undo();
    assert!(e.history().can_redo());
    type_chars(&mut e, &clock, "c", FAST);
    assert!(!e.history().can_redo());
    assert!(!e.redo());
}

#[test]
fn undo_all_restores_exact_bytes_of_crlf_text() {
    let original = "a\r\nb\rc\n\r\n";
    let (mut e, clock) = editor(&format!("|{original}"));
    type_chars(&mut e, &clock, "x\ny", SLOW);
    e.move_selections(Motion::DocumentEnd, false);
    e.delete_backward(DeleteUnit::Grapheme);
    e.delete_backward(DeleteUnit::Line);
    e.select_all();
    e.paste("replaced");
    while e.undo() == UndoOutcome::Undone {}
    assert_eq!(e.text().to_string(), original);
    while e.redo() {}
    assert_eq!(e.text().to_string(), "replaced");
}

#[test]
fn undo_steps_are_one_transaction_each() {
    let (mut e, clock) = editor("|\n|\n|");
    let edits = count_edits(&mut e);
    type_chars(&mut e, &clock, "abc", FAST);
    e.undo();
    e.redo();
    assert_eq!(
        *edits.borrow(),
        [
            EditCause::Edit(EditKind::Typing),
            EditCause::Edit(EditKind::Typing),
            EditCause::Edit(EditKind::Typing),
            EditCause::Undo,
            EditCause::Redo,
        ]
    );
}

#[test]
fn history_is_bounded_by_entries() {
    let config = HistoryConfig {
        max_entries: 3,
        ..HistoryConfig::default()
    };
    let (mut e, clock) = editor_with("|", config);
    for _ in 0..10 {
        type_chars(&mut e, &clock, "a", SLOW);
    }
    assert_eq!(e.history().undo_len(), 3);
    while e.undo() == UndoOutcome::Undone {}
    assert_eq!(e.text().to_string(), "aaaaaaa");
}

#[test]
fn history_is_bounded_by_bytes() {
    let config = HistoryConfig {
        max_bytes: 4096,
        ..HistoryConfig::default()
    };
    let (mut e, _) = editor_with("|", config);
    for _ in 0..20 {
        e.paste(&"p".repeat(500));
    }
    assert!(e.history().memory_bytes() <= 4096);
    assert!(e.history().undo_len() >= 2);
    // The newest step is kept even when it alone is over the limit.
    e.paste(&"q".repeat(10_000));
    assert_eq!(e.history().undo_len(), 1);
    e.undo();
    assert_eq!(e.text().len(), 20 * 500);
}

#[test]
fn save_points() {
    let (mut e, clock) = editor("|");
    assert!(!e.is_modified());
    type_chars(&mut e, &clock, "ab", FAST);
    assert!(e.is_modified());
    e.mark_saved();
    assert!(!e.is_modified());
    // Saving sealed the step: typing on starts a new one.
    type_chars(&mut e, &clock, "c", FAST);
    assert!(e.is_modified());
    e.undo();
    assert_eq!(render(&e), "ab|");
    assert!(!e.is_modified());
    e.undo();
    assert!(e.is_modified());
    e.redo();
    assert!(!e.is_modified());
}

#[test]
fn deferred_save_with_edits_in_between() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "ab", FAST);
    let point = e.save_point();
    type_chars(&mut e, &clock, "c", FAST);
    e.mark_saved_at(point);
    assert!(e.is_modified());
    e.undo();
    assert_eq!(e.text().to_string(), "ab");
    assert!(!e.is_modified());
}

#[test]
fn undo_can_stop_at_the_save_point() {
    let config = HistoryConfig {
        stop_at_save_point: true,
        ..HistoryConfig::default()
    };
    let (mut e, clock) = editor_with("|", config);
    type_chars(&mut e, &clock, "a", FAST);
    e.mark_saved();
    type_chars(&mut e, &clock, "b", SLOW);
    assert_eq!(e.undo(), UndoOutcome::Undone);
    assert_eq!(e.undo(), UndoOutcome::StoppedAtSavePoint);
    assert_eq!(e.text().to_string(), "a");
    assert_eq!(e.undo(), UndoOutcome::Undone);
    assert_eq!(e.text().to_string(), "");
}

#[test]
fn explicit_group_break() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "ab", FAST);
    e.break_undo_group();
    type_chars(&mut e, &clock, "cd", FAST);
    e.undo();
    assert_eq!(e.text().to_string(), "ab");
}

#[test]
fn multi_cursor_typing_groups_and_undoes_at_once() {
    let (mut e, clock) = editor("|\n|\n|");
    type_chars(&mut e, &clock, "abc", FAST);
    assert_eq!(e.history().undo_len(), 1);
    e.undo();
    assert_eq!(render(&e), "|\n|\n|");
}

// ----- movement -----------------------------------------------------------------

#[test]
fn moving_and_extending() {
    let (mut e, _) = editor("ab|cd ef");
    e.move_selections(Motion::Right, true);
    e.move_selections(Motion::Right, true);
    assert_eq!(render(&e), "ab{cd} ef");
    e.move_selections(Motion::Left, false);
    assert_eq!(render(&e), "ab|cd ef");
    e.move_selections(Motion::WordRight, true);
    e.move_selections(Motion::WordRight, true);
    assert_eq!(render(&e), "ab{cd ef}");
    e.move_selections(Motion::Right, false);
    assert_eq!(render(&e), "abcd ef|");
    e.move_selections(Motion::WordLeft, true);
    assert_eq!(render(&e), "abcd }ef{");
    e.move_selections(Motion::DocumentStart, false);
    assert_eq!(render(&e), "|abcd ef");
}

#[test]
fn smart_home_and_end() {
    let (mut e, _) = editor("  - item|\r\nnext");
    e.move_selections(Motion::SmartLineStart, false);
    assert_eq!(render(&e), "  |- item\r\nnext");
    e.move_selections(Motion::SmartLineStart, false);
    assert_eq!(render(&e), "|  - item\r\nnext");
    e.move_selections(Motion::LineEnd, true);
    assert_eq!(render(&e), "{  - item}\r\nnext");
}

#[test]
fn vertical_motion_keeps_the_goal_column() {
    let (mut e, _) = editor("a long line|\nshort\n\na long line");
    e.move_selections(Motion::Down, false);
    assert_eq!(render(&e), "a long line\nshort|\n\na long line");
    e.move_selections(Motion::Down, false);
    e.move_selections(Motion::Down, false);
    assert_eq!(render(&e), "a long line\nshort\n\na long line|");
    assert_eq!(e.selections().primary().goal(), SelectionGoal::Column(11));
    // A horizontal move drops the goal.
    e.move_selections(Motion::Left, false);
    assert_eq!(e.selections().primary().goal(), SelectionGoal::None);
    e.move_selections(Motion::Up, false);
    e.move_selections(Motion::Up, false);
    assert_eq!(render(&e), "a long line\nshort|\n\na long line");
}

#[test]
fn vertical_motion_with_tabs_and_selection() {
    let (mut e, _) = editor("\tx|\nabcdefgh");
    e.move_selections(Motion::Down, true);
    // The caret was at display column 5 (the tab is 4 wide).
    assert_eq!(render(&e), "\tx{\nabcde}fgh");
    // Down without extending collapses from the end.
    e.move_selections(Motion::Down, false);
    assert_eq!(render(&e), "\tx\nabcdefgh|");
}

#[test]
fn many_cursors_moving_into_each_other_merge() {
    let (mut e, _) = editor("|a|b|c");
    e.move_selections(Motion::LineEnd, false);
    assert_eq!(render(&e), "abc|");
    assert_eq!(e.selections().len(), 1);
}

// ----- selection commands -----------------------------------------------------

#[test]
fn select_all_collapse_and_primary() {
    let (mut e, _) = editor("a|b|c");
    e.select_all();
    assert_eq!(render(&e), "{abc}");
    e.collapse_selections();
    assert_eq!(render(&e), "abc|");
    e.set_cursor(1);
    e.add_cursor(2);
    assert_eq!(e.selections().len(), 2);
    assert_eq!(e.selections().primary().head(), 2);
    e.collapse_to_primary();
    assert_eq!(render(&e), "ab|c");
}

#[test]
fn add_and_remove_cursors() {
    let (mut e, _) = editor("ab|cd");
    let id = e.add_cursor(4);
    assert_eq!(render(&e), "ab|cd|");
    // A caret on an existing caret merges with it (and keeps the new id).
    let same = e.add_cursor(4);
    assert_eq!(e.selections().len(), 2);
    assert!(e.selections().get(same).is_some());
    let _ = id;
    assert!(e.remove_selection(same));
    assert_eq!(render(&e), "ab|cd");
    let last = e.selections().primary().id();
    assert!(!e.remove_selection(last));
}

#[test]
fn added_offsets_are_clipped() {
    let (mut e, _) = editor("é\r\n|x");
    e.add_cursor(1);
    e.add_cursor(3);
    e.add_cursor(999);
    assert_eq!(render(&e), "|é|\r\n|x|");
    e.selections().check(e.text()).unwrap();
}

#[test]
fn add_cursor_above_and_below() {
    let (mut e, _) = editor("abcd\nab\nabcd|\nabcd");
    assert!(e.add_cursor_above());
    assert!(e.add_cursor_above());
    assert!(!e.add_cursor_above());
    assert_eq!(render(&e), "abcd|\nab|\nabcd|\nabcd");
    assert!(e.add_cursor_below());
    assert!(!e.add_cursor_below());
    assert_eq!(render(&e), "abcd|\nab|\nabcd|\nabcd|");
}

#[test]
fn column_selection() {
    let (mut e, _) = editor("abcdef\nab\n\tabcdefgh\n|");
    // From column 1 of row 0 to display column 6 of row 2 (the tab is 4
    // columns wide, so that is before `c`). Column 1 of row 2 is inside the
    // tab, so that row's selection starts at the tab; row 1 is clamped.
    e.select_columns(1, 13);
    assert_eq!(render(&e), "a{bcdef}\na{b}\n{\tab}cdefgh\n");
    assert_eq!(e.selections().primary().range(), 10..13);
    // Upwards, right to left: reversed selections, the top one primary.
    e.select_columns(16, 2);
    assert_eq!(render(&e), "ab}cdef{\nab|\n}\tabcde{fgh\n");
    assert_eq!(e.selections().primary().range(), 2..6);
    // A row that ends before the left column gets no selection.
    e.select_columns(3, 16);
    assert_eq!(render(&e), "abc{def}\nab\n{\tabcde}fgh\n");
    // Zero width: a caret per row.
    e.select_columns(1, 8);
    assert_eq!(render(&e), "a|bcdef\na|b\n\tabcdefgh\n");
}

#[test]
fn tab_size_changes_reach_observers() {
    let (mut e, _) = editor("\tx|");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    e.observe(move |event| {
        if let EditorEvent::DisplayChanged { display, edits } = event {
            sink.borrow_mut().push((display.text(), edits.len()));
        }
    });
    e.set_tab_size(std::num::NonZeroU32::new(2).unwrap());
    assert_eq!(*seen.borrow(), [("  x".to_owned(), 1)]);
}

// ----- observers ----------------------------------------------------------------

#[test]
fn observers_see_updated_state_and_can_be_removed() {
    let (mut e, _) = editor("ab|");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let id = e.observe(move |event| {
        let entry = match event {
            EditorEvent::Edited(edit) => format!(
                "edit {} -> {} at {:?}",
                edit.old_text,
                edit.text,
                edit.selections.primary().head()
            ),
            EditorEvent::SelectionsChanged(selections) => {
                format!("selections {:?}", selections.primary().range())
            }
            _ => "other".to_owned(),
        };
        sink.borrow_mut().push(entry);
    });
    e.insert_at_selections("c");
    e.move_selections(Motion::Left, false);
    assert!(e.unobserve(id));
    assert!(!e.unobserve(id));
    e.insert_at_selections("d");
    assert_eq!(*seen.borrow(), ["edit ab -> abc at 3", "selections 2..2"]);
}

#[test]
fn display_map_follows_edits() {
    let (mut e, _) = editor("|x");
    e.insert_at_selections("\t");
    assert_eq!(e.display().text(), "    x");
    let display_edits = Rc::new(RefCell::new(0usize));
    let sink = display_edits.clone();
    e.observe(move |event| {
        if let EditorEvent::Edited(edit) = event {
            *sink.borrow_mut() += edit.display_edits.len();
            assert_eq!(edit.display.text(), "a   x");
        }
    });
    e.move_selections(Motion::DocumentStart, false);
    e.insert_at_selections("a");
    assert!(*display_edits.borrow() >= 1);
}

// ----- input method ---------------------------------------------------------------

#[test]
fn composition_is_one_undo_step() {
    let (mut e, clock) = editor("ab|");
    type_chars(&mut e, &clock, "c", FAST);
    clock.advance(FAST);
    e.replace_and_mark_text_in_range_utf16(None, "k", None);
    assert!(e.is_composing());
    assert_eq!(e.marked_text_range_utf16(), Some(3..4));
    clock.advance(SLOW);
    e.replace_and_mark_text_in_range_utf16(None, "か", None);
    clock.advance(SLOW);
    e.replace_and_mark_text_in_range_utf16(None, "かn", None);
    e.replace_and_mark_text_in_range_utf16(None, "かな", Some(1..1));
    assert_eq!(render(&e), "abcか|な");
    assert_eq!(e.marked_text_range_utf16(), Some(3..5));
    e.replace_text_in_range_utf16(None, "仮名");
    assert!(!e.is_composing());
    assert_eq!(render(&e), "abc仮名|");
    assert_eq!(e.marked_text_range_utf16(), None);
    e.undo();
    assert_eq!(render(&e), "abc|");
    e.redo();
    assert_eq!(e.text().to_string(), "abc仮名");
}

#[test]
fn cancelled_composition_leaves_no_step() {
    let (mut e, clock) = editor("|");
    type_chars(&mut e, &clock, "a", FAST);
    e.replace_and_mark_text_in_range_utf16(None, "x", None);
    e.replace_and_mark_text_in_range_utf16(None, "", None);
    assert!(!e.is_composing());
    assert_eq!(e.text().to_string(), "a");
    assert_eq!(e.history().undo_len(), 1);
    e.mark_saved();
    e.replace_and_mark_text_in_range_utf16(None, "y", None);
    e.replace_text_in_range_utf16(None, "");
    assert!(!e.is_modified());
}

#[test]
fn unmark_keeps_the_text() {
    let (mut e, _) = editor("|");
    e.replace_and_mark_text_in_range_utf16(None, "ü", None);
    e.unmark_text();
    assert!(!e.is_composing());
    assert_eq!(render(&e), "ü|");
    e.undo();
    assert_eq!(render(&e), "|");
}

#[test]
fn selected_range_is_relative_and_clamped() {
    let (mut e, _) = editor("x|");
    e.replace_and_mark_text_in_range_utf16(None, "a😀b", Some(5..1));
    // Reordered to 1..5, clamped to the 4 UTF-16 units of the text.
    assert_eq!(render(&e), "xa{😀b}");
    e.replace_and_mark_text_in_range_utf16(None, "a😀b", Some(2..2));
    // Inside the surrogate pair: the caret moves before the emoji.
    assert_eq!(render(&e), "xa|😀b");
    assert_eq!(e.selected_text_range_utf16().range, 2..2);
}

#[test]
fn explicit_replacement_ranges() {
    let (mut e, _) = editor("hello|");
    // A dead key: the platform replaces the last character.
    e.replace_and_mark_text_in_range_utf16(Some(4..5), "ó", None);
    assert_eq!(e.marked_text_range_utf16(), Some(4..5));
    e.replace_text_in_range_utf16(Some(4..5), "ö");
    assert_eq!(render(&e), "hellö|");
}

#[test]
fn bad_utf16_ranges_never_split_characters() {
    let (mut e, _) = editor("a😀b\r\nc|");
    // 2 is inside the surrogate pair: the range grows to the whole emoji.
    e.replace_text_in_range_utf16(Some(2..2), "X");
    assert_eq!(e.text().to_string(), "aX😀b\r\nc");
    e.replace_text_in_range_utf16(Some(3..4), "Y");
    assert_eq!(e.text().to_string(), "aXYb\r\nc");
    // 5 is between CR and LF: an insertion goes before the CR, a range
    // grows to the whole CRLF.
    e.replace_text_in_range_utf16(Some(5..5), "Z");
    assert_eq!(e.text().to_string(), "aXYbZ\r\nc");
    e.replace_text_in_range_utf16(Some(4..6), "-");
    assert_eq!(e.text().to_string(), "aXYb-c");
    // Out of range and inverted.
    e.replace_text_in_range_utf16(Some(99..3), "!");
    assert_eq!(e.text().to_string(), "aXY!");
    let (text, range) = e.text_for_range_utf16(0..usize::MAX);
    assert_eq!((text.as_str(), range), ("aXY!", 0..4));
}

#[test]
fn text_for_range_reports_the_adjusted_range() {
    let (e, _) = editor("a😀b|");
    let (text, range) = e.text_for_range_utf16(2..3);
    assert_eq!((text.as_str(), range), ("😀", 1..3));
    let (text, range) = e.text_for_range_utf16(5..1);
    assert_eq!((text.as_str(), range), ("😀b", 1..4));
}

#[test]
fn multi_cursor_composition() {
    let (mut e, _) = editor("a|\nb|");
    e.replace_and_mark_text_in_range_utf16(None, "n", None);
    e.replace_and_mark_text_in_range_utf16(None, "な", None);
    assert_eq!(render(&e), "aな|\nbな|");
    assert_eq!(e.marked_ranges(), [1..4, 6..9]);
    // The platform sees the primary (last added) caret's range.
    assert_eq!(e.marked_text_range_utf16(), Some(4..5));
    // A replacement range relative to the primary applies to both.
    e.replace_text_in_range_utf16(Some(4..5), "名");
    assert_eq!(render(&e), "a名|\nb名|");
    e.undo();
    assert_eq!(render(&e), "a|\nb|");
}

#[test]
fn commands_commit_the_composition() {
    let (mut e, _) = editor("|");
    let events = Rc::new(RefCell::new(0usize));
    let sink = events.clone();
    e.observe(move |event| {
        if matches!(event, EditorEvent::CompositionChanged) {
            *sink.borrow_mut() += 1;
        }
    });
    e.replace_and_mark_text_in_range_utf16(None, "x", None);
    e.move_selections(Motion::Left, false);
    assert!(!e.is_composing());
    assert_eq!(*events.borrow(), 2);
    assert_eq!(render(&e), "|x");
}

#[test]
fn clip_utf16_range_examples() {
    let (e, _) = editor("😀\r\n|");
    assert_eq!(e.clip_utf16_range(1..1), 0..0);
    assert_eq!(e.clip_utf16_range(1..2), 0..4);
    assert_eq!(e.clip_utf16_range(3..3), 4..4);
    assert_eq!(e.clip_utf16_range(3..4), 4..6);
    assert_eq!(e.clip_utf16_range(9..0), 0..6);
}

#[test]
fn selections_after_edit_have_valid_biases() {
    let (mut e, _) = editor("ab{cd}ef");
    let selection = *e.selections().primary();
    assert_eq!(selection.tail_anchor().bias, Bias::Right);
    assert_eq!(selection.head_anchor().bias, Bias::Left);
    e.set_selections(&[SelectionSpec::new(4, 2)], 0);
    assert!(e.selections().primary().is_reversed());
    assert!(e.selected_text_range_utf16().reversed);
}

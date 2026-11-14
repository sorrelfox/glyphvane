// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The editor with soft wrap on: the wrap layer follows edits and settings,
//! vertical motion moves by wrap rows with a goal column, Home and End stop
//! at the display row first, and cursors and column selections use wrap
//! rows.

mod support;

use std::cell::RefCell;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;

use gv_editor::display_map::{
    IndentAdjustment, MarkdownHangingIndent, MonospaceMeasure, TextMeasure, WrapMap, WrapRow,
    WrapSnapshot,
};
use gv_editor::{Editor, EditorEvent, Motion, SelectionGoal};
use gv_text::{Bias, Buffer};
use proptest::prelude::*;
use proptest::test_runner::Config;
use support::{editor, render};

/// The wrap row of a caret offset.
fn wrap_row(wrap: &WrapSnapshot, offset: usize) -> u32 {
    let point = wrap.buffer().offset_to_point(offset).expect("an offset");
    wrap.buffer_point_to_wrap_point(point, Bias::Left).row().0
}

fn rows(editor: &Editor) -> Vec<String> {
    let wrap = editor.wrap().expect("soft wrap is on");
    wrap.text().split('\n').map(str::to_owned).collect()
}

fn moved(editor: &mut Editor, motion: Motion) -> String {
    editor.move_selections(motion, false);
    render(editor)
}

/// Event kinds, with the wrap layer's text after each.
type WrapEvents = Rc<RefCell<Vec<(&'static str, Option<String>)>>>;

/// Records the kind of every event and the wrap layer's text after it.
fn record_wrap_events(editor: &mut Editor) -> WrapEvents {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    editor.observe(move |event| {
        let entry = match event {
            EditorEvent::Edited(edit) => ("edited", edit.wrap.map(WrapSnapshot::text)),
            EditorEvent::DisplayChanged { .. } => ("display", None),
            EditorEvent::WrapChanged { wrap, .. } => ("wrap", wrap.map(WrapSnapshot::text)),
            _ => return,
        };
        sink.borrow_mut().push(entry);
    });
    events
}

#[test]
fn soft_wrap_on_and_off() {
    let (mut e, _) = editor("aaaa bbbb cccc\nshort|\n");
    let events = record_wrap_events(&mut e);
    assert!(e.wrap().is_none());
    assert_eq!(e.wrap_width(), None);
    // Turning it off when it is off changes nothing.
    e.set_wrap_width(None);
    assert!(events.borrow().is_empty());

    e.set_wrap_width(Some(5.0));
    assert_eq!(e.wrap_width(), Some(5.0));
    assert_eq!(rows(&e), ["aaaa ", "bbbb ", "cccc", "short", ""]);
    // The same width again: no event.
    e.set_wrap_width(Some(5.0));
    e.set_wrap_width(Some(10.0));
    assert_eq!(rows(&e), ["aaaa bbbb ", "cccc", "short", ""]);
    e.set_wrap_width(None);
    assert!(e.wrap().is_none());
    assert_eq!(
        *events.borrow(),
        [
            ("wrap", Some("aaaa \nbbbb \ncccc\nshort\n".to_owned())),
            ("wrap", Some("aaaa bbbb \ncccc\nshort\n".to_owned())),
            ("wrap", None),
        ]
    );
}

#[test]
fn edits_and_settings_rewrap() {
    let (mut e, _) = editor("one two three|");
    e.set_wrap_width(Some(8.0));
    let events = record_wrap_events(&mut e);
    e.insert_at_selections(" four five");
    assert_eq!(rows(&e), ["one two ", "three ", "four ", "five"]);

    // A wider character measure wraps sooner.
    #[derive(Clone, Copy)]
    struct Wide;
    impl TextMeasure for Wide {
        fn char_width(&mut self, c: char) -> f32 {
            if c == 'o' { 3.0 } else { 1.0 }
        }
    }
    e.set_measure(Box::new(Wide));
    assert_eq!(rows(&e), ["one ", "two ", "three ", "four ", "five"]);
    e.set_measure(Box::new(MonospaceMeasure::new(1.0)));

    // Tabs: the tab layer changes first, then the wrap layer.
    e.set_selections(&[gv_editor::SelectionSpec::caret(0)], 0);
    e.insert_at_selections("\t");
    e.set_tab_size(NonZeroU32::new(2).expect("not zero"));
    assert_eq!(rows(&e)[0], "  one ");

    let kinds: Vec<&str> = events.borrow().iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        ["edited", "wrap", "wrap", "edited", "display", "wrap"]
    );
    // Every event carries the wrap layer as it now is.
    let last = events.borrow().last().cloned();
    assert_eq!(
        last.and_then(|(_, text)| text),
        e.wrap().map(WrapSnapshot::text)
    );
}

#[test]
fn edit_events_carry_wrap_edits() {
    let (mut e, _) = editor("aaaa bbbb\ncc dd ee|\nff");
    e.set_wrap_width(Some(5.0));
    let before = Rc::new(RefCell::new(e.wrap().map(WrapSnapshot::text)));
    let checked = Rc::new(RefCell::new(0));
    let (state, count) = (before.clone(), checked.clone());
    e.observe(move |event| {
        if let EditorEvent::Edited(edit) = event {
            let wrap = edit.wrap.expect("soft wrap is on");
            let old = state.borrow_mut().replace(wrap.text()).expect("a text");
            let new = wrap.text();
            // Replay the row edits.
            let old_rows: Vec<&str> = old.split('\n').collect();
            let new_rows: Vec<&str> = new.split('\n').collect();
            let mut rows: Vec<&str> = old_rows.clone();
            for wrap_edit in edit.wrap_edits {
                let start = wrap_edit.new.start.0 as usize;
                let old_len = (wrap_edit.old.end.0 - wrap_edit.old.start.0) as usize;
                let end = (start + old_len).min(rows.len());
                let new_end = (wrap_edit.new.end.0 as usize).min(new_rows.len());
                rows.splice(start..end, new_rows[start..new_end].iter().copied());
            }
            assert_eq!(rows.join("\n"), new);
            *count.borrow_mut() += 1;
        }
    });
    e.insert_at_selections(" gg hh");
    e.undo();
    e.insert_at_selections("\n");
    assert_eq!(*checked.borrow(), 3);
}

#[test]
fn vertical_motion_by_wrap_rows() {
    let (mut e, _) = editor("aa|aa bbbb cccc\nshort\n");
    e.set_wrap_width(Some(5.0));
    assert_eq!(rows(&e), ["aaaa ", "bbbb ", "cccc", "short", ""]);
    assert_eq!(moved(&mut e, Motion::Down), "aaaa bb|bb cccc\nshort\n");
    assert_eq!(moved(&mut e, Motion::Down), "aaaa bbbb cc|cc\nshort\n");
    assert_eq!(moved(&mut e, Motion::Down), "aaaa bbbb cccc\nsh|ort\n");
    assert_eq!(e.selections().primary().goal(), SelectionGoal::Column(2));
    assert_eq!(moved(&mut e, Motion::Up), "aaaa bbbb cc|cc\nshort\n");
    assert_eq!(moved(&mut e, Motion::UpRows(5)), "|aaaa bbbb cccc\nshort\n");
    assert_eq!(
        moved(&mut e, Motion::DownRows(3)),
        "aaaa bbbb cccc\nsh|ort\n"
    );

    // A goal past the end of a soft-wrapped row stops before its last
    // character, on that row (its end is the next row's start).
    let (mut e, _) = editor("aaaa bbbb cccc\nshort|\n");
    e.set_wrap_width(Some(5.0));
    assert_eq!(moved(&mut e, Motion::Up), "aaaa bbbb cccc|\nshort\n");
    assert_eq!(moved(&mut e, Motion::Up), "aaaa bbbb| cccc\nshort\n");
    assert_eq!(moved(&mut e, Motion::Up), "aaaa| bbbb cccc\nshort\n");
    assert_eq!(e.selections().primary().goal(), SelectionGoal::Column(5));
    assert_eq!(moved(&mut e, Motion::Up), "|aaaa bbbb cccc\nshort\n");
    assert_eq!(moved(&mut e, Motion::Down), "aaaa bbbb| cccc\nshort\n");
}

#[test]
fn hanging_indent_and_the_goal_column() {
    let (mut e, _) = editor("|- item one two three");
    e.set_wrap_indent(Arc::new(MarkdownHangingIndent::default()));
    e.set_wrap_width(Some(10.0));
    assert_eq!(rows(&e), ["- item ", "  one two ", "  three"]);
    // Column 0 is inside the indent: the start of the row's text.
    assert_eq!(moved(&mut e, Motion::Down), "- item |one two three");
    assert_eq!(moved(&mut e, Motion::Down), "- item one two |three");
    assert_eq!(e.selections().primary().goal(), SelectionGoal::Column(0));
    assert_eq!(moved(&mut e, Motion::Up), "- item |one two three");
    assert_eq!(moved(&mut e, Motion::Up), "|- item one two three");
    // Columns count the indent: column 4 of `  one two ` is `e`.
    e.set_selections(&[gv_editor::SelectionSpec::caret(4)], 0);
    assert_eq!(moved(&mut e, Motion::Down), "- item on|e two three");
    // The indent can change while wrapped.
    e.set_wrap_indent(Arc::new(IndentAdjustment::NoIndent));
    assert_eq!(rows(&e), ["- item ", "one two ", "three"]);
}

#[test]
fn home_and_end_on_wrap_rows() {
    let (mut e, _) = editor("aaaa bb|bb cccc\nshort\n");
    e.set_wrap_width(Some(5.0));
    // On a continuation row: its start, then the line start.
    assert_eq!(moved(&mut e, Motion::LineStart), "aaaa |bbbb cccc\nshort\n");
    assert_eq!(moved(&mut e, Motion::LineStart), "|aaaa bbbb cccc\nshort\n");
    // On the first row the line start is the row start.
    assert_eq!(moved(&mut e, Motion::LineStart), "|aaaa bbbb cccc\nshort\n");
    // End: before the last character of a soft-wrapped row, then the line
    // end.
    assert_eq!(moved(&mut e, Motion::LineEnd), "aaaa| bbbb cccc\nshort\n");
    assert_eq!(moved(&mut e, Motion::LineEnd), "aaaa bbbb cccc|\nshort\n");
    assert_eq!(moved(&mut e, Motion::LineEnd), "aaaa bbbb cccc|\nshort\n");
    // On the last row of the line, Home goes to the row start first.
    assert_eq!(moved(&mut e, Motion::LineStart), "aaaa bbbb |cccc\nshort\n");

    // Smart home on a continuation row: the row start, then the first
    // non-blank of the line, then the line start.
    let (mut e, _) = editor("    aaaa bbbb cc|cc");
    e.set_wrap_width(Some(10.0));
    assert_eq!(rows(&e), ["    aaaa ", "    bbbb ", "    cccc"]);
    assert_eq!(moved(&mut e, Motion::SmartLineStart), "    aaaa bbbb |cccc");
    assert_eq!(moved(&mut e, Motion::SmartLineStart), "    |aaaa bbbb cccc");
    assert_eq!(moved(&mut e, Motion::SmartLineStart), "|    aaaa bbbb cccc");
    assert_eq!(moved(&mut e, Motion::SmartLineStart), "    |aaaa bbbb cccc");

    // Shift+End extends to the row end.
    let (mut e, _) = editor("aaaa |bbbb cccc");
    e.set_wrap_width(Some(5.0));
    e.move_selections(Motion::LineEnd, true);
    assert_eq!(render(&e), "aaaa {bbbb} cccc");
}

#[test]
fn cursors_and_column_selection_use_wrap_rows() {
    let (mut e, _) = editor("aa|aa bbbb cccc");
    e.set_wrap_width(Some(5.0));
    assert!(e.add_cursor_below());
    assert!(e.add_cursor_below());
    assert_eq!(render(&e), "aa|aa bb|bb cc|cc");
    assert!(!e.add_cursor_below());
    assert!(!e.add_cursor_above());

    let (mut e, _) = editor("aaaa bbbb cccc");
    e.set_wrap_width(Some(5.0));
    // Columns 1 to 3 of every wrap row.
    e.select_columns(1, 13);
    assert_eq!(render(&e), "a{aa}a b{bb}b c{cc}c");
}

#[test]
fn wide_tabs_across_a_soft_wrap() {
    // A tab expanded across the wrap: moving down never gets stuck.
    let (mut e, _) = editor("|ab\tcd\tef");
    e.set_tab_size(NonZeroU32::new(8).expect("not zero"));
    e.set_wrap_width(Some(5.0));
    let wrap = e.wrap().expect("on").clone();
    let max_row = wrap.max_point().row().0;
    let mut row = 0;
    for _ in 0..=max_row + 1 {
        e.move_selections(Motion::Down, false);
        let head = e.selections().primary().head();
        let next = wrap_row(&wrap, head);
        assert!(next > row || head == e.text().len(), "stuck at row {row}");
        row = next;
    }
    assert_eq!(e.selections().primary().head(), e.text().len());
}

// ----- properties -----------------------------------------------------------

fn text_char() -> impl Strategy<Value = char> {
    prop_oneof![
        10 => prop::sample::select(vec!['a', 'b', 'c']),
        5 => Just(' '),
        1 => Just('\t'),
        2 => Just('\n'),
        1 => prop::sample::select(vec!['\r', '-', '>']),
        2 => prop::sample::select(vec!['é', '語', '🍐', '\u{301}']),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(text_char(), 0..200).prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #![proptest_config(Config { cases: 64, failure_persistence: None, ..Config::default() })]

    /// Down from the start visits every wrap row that holds a caret
    /// position once and in order, then the end of the text; Up from the
    /// end visits them in reverse. (A row can hold none when it is all
    /// inside an expanded tab.)
    #[test]
    fn vertical_motion_visits_every_wrap_row(
        text in text(),
        width in 0.5f32..30.0,
        tab_size in 1u32..=8,
        hanging in any::<bool>(),
    ) {
        let mut e = Editor::new(Buffer::new(&text));
        e.set_tab_size(NonZeroU32::new(tab_size).expect("not zero"));
        if hanging {
            e.set_wrap_indent(Arc::new(MarkdownHangingIndent::default()));
        }
        e.set_wrap_width(Some(width));
        let wrap = e.wrap().expect("on").clone();
        // The editor's wrap layer is the one a wrap map builds.
        let rebuilt = WrapMap::with_indent(
            e.display().clone(),
            Box::new(MonospaceMeasure::new(1.0)),
            if hanging {
                Arc::new(MarkdownHangingIndent::default())
            } else {
                Arc::new(IndentAdjustment::SameIndent)
            },
            Some(width),
        )
        .1;
        prop_assert_eq!(wrap.text(), rebuilt.text());

        let buffer = e.text().clone();
        // The first caret position of every row that has one.
        let mut first_carets: Vec<(u32, usize)> = (0..=buffer.len())
            .filter(|&offset| {
                text.is_char_boundary(offset)
                    && !buffer.is_inside_crlf(offset)
                    && gv_editor::boundaries::snap_to_grapheme(&buffer, offset) == offset
            })
            .map(|offset| (wrap_row(&wrap, offset), offset))
            .collect();
        first_carets.dedup_by_key(|(row, _)| *row);
        let caret_rows: Vec<u32> = first_carets.iter().map(|(row, _)| *row).collect();

        e.set_selections(&[gv_editor::SelectionSpec::caret(0)], 0);
        for &expected in &caret_rows[1..] {
            e.move_selections(Motion::Down, false);
            let head = e.selections().primary().head();
            prop_assert_eq!(wrap_row(&wrap, head), expected, "down at {}", head);
        }
        e.move_selections(Motion::Down, false);
        prop_assert_eq!(e.selections().primary().head(), e.text().len());

        let end = e.text().len();
        e.set_selections(&[gv_editor::SelectionSpec::caret(end)], 0);
        for &expected in caret_rows[..caret_rows.len() - 1].iter().rev() {
            e.move_selections(Motion::Up, false);
            let head = e.selections().primary().head();
            prop_assert_eq!(wrap_row(&wrap, head), expected, "up at {}", head);
        }

        // Home and End stay on the row, or go to the line, and pressing
        // them twice reaches the line start or end.
        for offset in (0..=e.text().len()).step_by(7) {
            e.set_selections(&[gv_editor::SelectionSpec::caret(offset)], 0);
            let caret = e.selections().primary().head();
            let row = wrap_row(&wrap, caret);
            let line_end = gv_editor::boundaries::line_end(e.text(), caret);
            e.move_selections(Motion::LineEnd, false);
            let row_end = e.selections().primary().head();
            prop_assert!(row_end >= caret);
            // A row of one grapheme has no end before its last character
            // other than its start: End goes to the line end at once.
            prop_assert!(wrap_row(&wrap, row_end) == row || row_end == line_end);
            e.move_selections(Motion::LineEnd, false);
            prop_assert_eq!(e.selections().primary().head(), line_end);

            e.set_selections(&[gv_editor::SelectionSpec::caret(caret)], 0);
            let line_start = gv_editor::boundaries::line_start(e.text(), caret);
            e.move_selections(Motion::LineStart, false);
            let row_start = e.selections().primary().head();
            prop_assert!(row_start <= caret);
            // The first caret position of the row (after its indent, or
            // after a tab that ends on it), unless the caret is there.
            let first = first_carets
                .iter()
                .find(|(caret_row, _)| *caret_row == row)
                .map(|(_, offset)| *offset)
                .expect("the caret's row has a caret position");
            if caret > first {
                prop_assert_eq!(row_start, first);
                let point = wrap.buffer_point_to_wrap_point(
                    wrap.buffer().offset_to_point(row_start).expect("an offset"),
                    Bias::Left,
                );
                let indent = match row.checked_sub(1) {
                    Some(previous) => wrap.soft_wrap_indent(WrapRow(previous)).unwrap_or(0),
                    None => 0,
                };
                prop_assert!(point.row() == WrapRow(row) && point.column() >= indent);
            } else {
                prop_assert_eq!(row_start, line_start);
            }
            e.move_selections(Motion::LineStart, false);
            prop_assert_eq!(e.selections().primary().head(), line_start);
        }
    }
}

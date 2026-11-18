// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Replacements other than folds through the editor (Live Preview's hidden
//! markers): splicing them, the events, and motions that never stop inside
//! a run of adjacent hidden text.

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use gv_editor::display_map::{MonospaceMeasure, Replacement};
use gv_editor::{Editor, EditorEvent, Motion};
use gv_text::{Bias, Buffer};
use rand::{Rng, SeedableRng, rngs::StdRng};

fn caret(editor: &Editor) -> usize {
    editor.selections().primary().head()
}

fn hide(editor: &mut Editor, ranges: &[std::ops::Range<usize>]) {
    let len = editor.text().len();
    let replacements = ranges.iter().cloned().map(Replacement::hide).collect();
    editor.splice_replacements([(0..len, replacements)]);
}

#[test]
fn splices_show_in_the_display_and_tell_observers() {
    let mut editor = Editor::new(Buffer::new("a **bold** b\nnext"));
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    editor.observe(move |event| {
        sink.borrow_mut().push(match event {
            EditorEvent::DisplayChanged { .. } => "display",
            EditorEvent::WrapChanged { .. } => "wrap",
            EditorEvent::Edited(_) => "edited",
            _ => "other",
        });
    });
    assert!(editor.splice_replacements([(
        2..10,
        vec![Replacement::hide(2..4), Replacement::hide(8..10),]
    )]));
    assert_eq!(editor.display().text(), "a bold b\nnext");
    assert_eq!(*events.borrow(), ["display"]);
    // The same again changes nothing and says nothing.
    assert!(!editor.splice_replacements([(
        2..10,
        vec![Replacement::hide(2..4), Replacement::hide(8..10),]
    )]));
    assert_eq!(events.borrow().len(), 1);
    assert_eq!(editor.replacements_in(0..3).len(), 1);

    // Soft wrap is computed on the text after hiding.
    editor.set_measure(Box::new(MonospaceMeasure::new(1.0)));
    editor.set_wrap_width(Some(100.0));
    events.borrow_mut().clear();
    editor.splice_replacements([(2..10, Vec::new())]);
    assert_eq!(*events.borrow(), ["display", "wrap"]);
    assert_eq!(editor.display().text(), "a **bold** b\nnext");

    // An edit that touches a hidden range drops it; others move.
    hide(&mut editor, &[2..4, 8..10]);
    editor.set_cursor(10);
    editor.insert_at_selections("x");
    assert_eq!(editor.text().to_string(), "a **bold**x b\nnext");
    assert_eq!(editor.display().text(), "a bold**x b\nnext");
}

#[test]
fn horizontal_motions_skip_hidden_runs() {
    //                          0123456789012345678901
    let mut editor = Editor::new(Buffer::new("a **[x](y)** b"));
    // `**[` and `](y)**`, each in two adjacent pieces.
    hide(&mut editor, &[2..4, 4..5, 6..10, 10..12]);
    assert_eq!(editor.display().text(), "a x b");
    editor.set_cursor(2);
    editor.move_selections(Motion::Right, false);
    assert_eq!(caret(&editor), 5, "over `**[` as one");
    editor.move_selections(Motion::Right, false);
    assert_eq!(caret(&editor), 6);
    editor.move_selections(Motion::Right, false);
    assert_eq!(caret(&editor), 12, "over `](y)**` as one");
    editor.move_selections(Motion::Left, false);
    assert_eq!(caret(&editor), 6);
    editor.move_selections(Motion::Left, false);
    assert_eq!(caret(&editor), 5);
    editor.move_selections(Motion::Left, false);
    assert_eq!(caret(&editor), 2);

    // Word motions never stop inside.
    editor.set_cursor(14);
    editor.move_selections(Motion::WordLeft, false);
    assert!(
        editor
            .display()
            .replace_snapshot()
            .atomic_range_at(caret(&editor))
            .is_none(),
        "{}",
        caret(&editor)
    );
    editor.set_cursor(0);
    for _ in 0..6 {
        editor.move_selections(Motion::WordRight, false);
        let at = caret(&editor);
        assert!(
            editor
                .display()
                .replace_snapshot()
                .atomic_range_at(at)
                .is_none(),
            "{at}"
        );
    }

    // Shift-selection extends over runs as a whole; the tail stays.
    editor.set_cursor(2);
    editor.move_selections(Motion::Right, true);
    editor.move_selections(Motion::Right, true);
    assert_eq!(editor.selections().primary().range(), 2..6);
    editor.move_selections(Motion::Right, true);
    assert_eq!(editor.selections().primary().range(), 2..12);
}

#[test]
fn line_start_and_end_with_a_hidden_heading_marker() {
    let mut editor = Editor::new(Buffer::new("## Title **x**\nnext"));
    hide(&mut editor, &[0..3, 9..11, 12..14]);
    assert_eq!(editor.display().text(), "Title x\nnext");
    editor.set_cursor(6);
    editor.move_selections(Motion::LineEnd, false);
    assert_eq!(caret(&editor), 14);
    editor.move_selections(Motion::LineStart, false);
    let at = caret(&editor);
    assert!(at == 0 || at == 3, "{at}");
    editor.set_cursor(6);
    editor.move_selections(Motion::LineEnd, true);
    assert_eq!(editor.selections().primary().range(), 6..14);
}

/// Random text with random hidden ranges: after any motion, extending or
/// not, with soft wrap or not, no caret is strictly inside a run of hidden
/// text, and splices against a fresh editor give the same display.
#[test]
fn motions_never_end_inside_hidden_runs() {
    const WORDS: &[&str] = &[
        "**", "*", "_", "`", "[", "](u)", "[[", "]]", "|", "==", "~~", "# ", "word", "x", " ", " ",
        "\n", "\t", "é", "ab cd",
    ];
    let seeds = std::env::var("ITERATIONS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(200u64);
    for seed in 0..seeds {
        let mut rng = StdRng::seed_from_u64(seed);
        let text: String = (0..rng.random_range(1..40))
            .map(|_| WORDS[rng.random_range(0..WORDS.len())])
            .collect();
        let mut editor = Editor::new(Buffer::new(&text));
        if rng.random_bool(0.5) {
            editor.set_measure(Box::new(MonospaceMeasure::new(1.0)));
            editor.set_wrap_width(Some(rng.random_range(4..20) as f32));
        }
        // Random hidden ranges, adjacent ones included.
        let len = text.len();
        let mut cuts: Vec<usize> = (0..rng.random_range(0..12))
            .map(|_| {
                editor
                    .text()
                    .clip_offset(rng.random_range(0..=len), Bias::Left)
            })
            .collect();
        cuts.sort_unstable();
        cuts.dedup();
        let hidden: Vec<_> = cuts
            .windows(2)
            .filter(|_| rng.random_bool(0.7))
            .map(|pair| pair[0]..pair[1])
            .collect();
        hide(&mut editor, &hidden);
        let display = editor.display().text();
        let mut fresh = Editor::new(Buffer::new(&text));
        hide(&mut fresh, &hidden);
        assert_eq!(fresh.display().text(), display, "seed {seed}");

        let motions = [
            Motion::Left,
            Motion::Right,
            Motion::WordLeft,
            Motion::WordRight,
            Motion::LineStart,
            Motion::SmartLineStart,
            Motion::LineEnd,
            Motion::Up,
            Motion::Down,
            Motion::UpRows(2),
            Motion::DownRows(3),
            Motion::DocumentStart,
            Motion::DocumentEnd,
        ];
        let start = editor
            .text()
            .clip_offset(rng.random_range(0..=len), Bias::Left);
        let replace = editor.display().replace_snapshot().clone();
        let start = replace.atomic_range_at(start).map_or(start, |r| r.start);
        editor.set_cursor(start);
        for _ in 0..60 {
            let motion = motions[rng.random_range(0..motions.len())];
            let extend = rng.random_bool(0.3);
            editor.move_selections(motion, extend);
            let head = caret(&editor);
            assert!(
                replace.atomic_range_at(head).is_none(),
                "seed {seed}: {motion:?} (extend {extend}) ended at {head} in {text:?} with {hidden:?}"
            );
        }
    }
}

#[test]
fn box_widgets_take_their_width_in_soft_wrap() {
    use gv_editor::display_map::{Widget, WidgetId};
    // 0123456789012
    // aa XXXX bb cc
    let mut editor = Editor::new(Buffer::new("aa XXXX bb cc"));
    editor.set_measure(Box::new(MonospaceMeasure::new(1.0)));
    editor.set_wrap_width(Some(10.0));
    let rows = |editor: &Editor| editor.wrap().expect("soft wrap").max_point().row().0 + 1;
    assert_eq!(rows(&editor), 2, "13 characters in 10 px");
    // A box 1 px wide: "aa ￼ bb cc" is 10 px and fits.
    let narrow = Widget::boxed(WidgetId(1), 1.0, None);
    editor.splice_replacements([(0..13, vec![Replacement::widget(3..7, narrow)])]);
    assert_eq!(editor.display().text(), "aa \u{fffc} bb cc");
    assert_eq!(rows(&editor), 1);
    // A box 6 px wide: "aa ￼ " is 10 px, "bb cc" goes on.
    let wide = Widget::boxed(WidgetId(2), 6.0, None);
    editor.splice_replacements([(0..13, vec![Replacement::widget(3..7, wide)])]);
    assert_eq!(rows(&editor), 2);
    // A box wider than the column gets a row of its own.
    let huge = Widget::boxed(WidgetId(3), 30.0, None);
    editor.splice_replacements([(0..13, vec![Replacement::widget(3..7, huge)])]);
    assert_eq!(rows(&editor), 3);
    // The caret moves over it as one unit.
    editor.set_cursor(3);
    editor.move_selections(Motion::Right, false);
    assert_eq!(caret(&editor), 7);
}

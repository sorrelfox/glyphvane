// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view for assistive technology, in headless windows: the
//! rows as text runs (characters as grapheme clusters, words, a `\n` at
//! the end of a line, a tab as one character, long rows split), the
//! primary selection's anchor and focus in them after caret moves and
//! Shift-selection, the caret's line when it is off screen, a screen
//! reader's selection mapped back to buffer offsets, and Live Preview's
//! shown text. GPUI's test platform has no AccessKit adapter, so the
//! nodes themselves (built from this description) are not checked here.

use super::*;

/// The runs' texts, lengths and word starts, and the selection.
#[allow(clippy::type_complexity)]
fn described(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
) -> (
    Vec<(String, Vec<u8>, Vec<u8>)>,
    Option<((usize, usize), (usize, usize))>,
) {
    draw(vcx);
    view.read_with(vcx, |v, _| {
        let source = v.a11y_source();
        (
            source
                .runs
                .iter()
                .map(|r| (r.text.clone(), r.lengths.clone(), r.word_starts.clone()))
                .collect(),
            source.selection,
        )
    })
}

fn offsets(view: &Entity<EditorView>, vcx: &mut VisualTestContext, run: usize) -> Vec<usize> {
    view.read_with(vcx, |v, _| v.a11y_source().runs[run].offsets.clone())
}

/// A screen reader's selection from `anchor` to `focus` (run, character).
fn select(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
    anchor: (usize, usize),
    focus: (usize, usize),
) {
    view.update_in(vcx, |v, window, cx| {
        let (a, f) = {
            let source = v.a11y_source();
            (
                source.offset_at(anchor).expect("anchor in the runs"),
                source.offset_at(focus).expect("focus in the runs"),
            )
        };
        v.select_from_a11y(a, f, window, cx);
    });
    draw(vcx);
}

fn caret_at(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| v.set_cursor(offset, cx));
    draw(vcx);
}

const MIXED: &str = "Hello world\n👍🏽 ok\n日本語\n";

#[gpui::test]
fn rows_are_text_runs_of_grapheme_clusters(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, MIXED);
    let (runs, selection) = described(&view, &mut vcx);
    assert_eq!(
        runs,
        vec![
            ("Hello world\n".into(), vec![1; 12], vec![0, 6]),
            // The emoji with its skin tone is one character of 8 bytes.
            ("👍🏽 ok\n".into(), vec![8, 1, 1, 1, 1], vec![0, 2]),
            // Each ideograph is a word (UAX #29).
            ("日本語\n".into(), vec![3, 3, 3, 1], vec![0, 1, 2]),
            // The empty last line.
            (String::new(), vec![], vec![]),
        ]
    );
    // Each character keeps its buffer offset; the `\n` ends at the next
    // line's start.
    assert_eq!(offsets(&view, &mut vcx, 1), vec![12, 20, 21, 22, 23, 24]);
    assert_eq!(offsets(&view, &mut vcx, 2), vec![24, 27, 30, 33, 34]);
    // The caret at the start.
    assert_eq!(selection, Some(((0, 0), (0, 0))));
}

#[gpui::test]
fn the_selection_follows_the_caret_and_shift_selection(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, MIXED);
    // After the emoji.
    caret_at(&view, &mut vcx, 20);
    assert_eq!(described(&view, &mut vcx).1, Some(((1, 1), (1, 1))));
    // At the end of a line the caret is on its line break.
    caret_at(&view, &mut vcx, 11);
    assert_eq!(described(&view, &mut vcx).1, Some(((0, 11), (0, 11))));
    // Shift+Right over the line break and the emoji: the anchor stays.
    vcx.simulate_keystrokes("shift-right shift-right");
    assert_eq!(selections(&view, &mut vcx), vec![(11, 20)]);
    assert_eq!(described(&view, &mut vcx).1, Some(((0, 11), (1, 1))));
    // Backwards: the focus is before the anchor.
    caret_at(&view, &mut vcx, 30);
    vcx.simulate_keystrokes("shift-left shift-left");
    assert_eq!(selections(&view, &mut vcx), vec![(30, 24)]);
    assert_eq!(described(&view, &mut vcx).1, Some(((2, 2), (2, 0))));
    // An edit: the runs and the caret follow.
    vcx.simulate_keystrokes("x");
    let (runs_now, selection) = described(&view, &mut vcx);
    assert_eq!(runs_now[2].0, "x語\n");
    assert_eq!(selection, Some(((2, 1), (2, 1))));
}

#[gpui::test]
fn a_screen_reader_moves_the_caret_and_selects(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, MIXED);
    // A caret before 本.
    select(&view, &mut vcx, (2, 1), (2, 1));
    assert_eq!(selections(&view, &mut vcx), vec![(27, 27)]);
    // From the emoji to after "ok", backwards.
    select(&view, &mut vcx, (1, 4), (1, 0));
    assert_eq!(selections(&view, &mut vcx), vec![(23, 12)]);
    assert_eq!(described(&view, &mut vcx).1, Some(((1, 4), (1, 0))));
    // Past a run's end is its end.
    select(&view, &mut vcx, (0, 99), (0, 99));
    assert_eq!(selections(&view, &mut vcx), vec![(12, 12)]);
}

#[gpui::test]
fn a_tab_is_one_character(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "\tx\n");
    let (runs, _) = described(&view, &mut vcx);
    assert_eq!(runs[0].0, "\tx\n");
    assert_eq!(runs[0].1, vec![1, 1, 1]);
    assert_eq!(offsets(&view, &mut vcx, 0), vec![0, 1, 2, 3]);
    caret_at(&view, &mut vcx, 1);
    assert_eq!(described(&view, &mut vcx).1, Some(((0, 1), (0, 1))));
}

#[gpui::test]
fn long_rows_are_split_and_linked(cx: &mut TestAppContext) {
    let line = "ab ".repeat(100);
    let (view, mut vcx) = editor(cx, &format!("{line}\n"));
    view.update(&mut vcx, |v, cx| v.set_soft_wrap(false, cx));
    let (runs, _) = described(&view, &mut vcx);
    assert_eq!(runs[0].1.len(), source_a11y_max());
    assert_eq!(runs[1].1.len(), 301 - source_a11y_max());
    // Word starts are counted from each piece's start.
    assert_eq!(runs[1].2.first(), Some(&0));
    let linked = view.read_with(&vcx, |v, _| {
        let s = v.a11y_source();
        (
            s.runs[0].next_on_line,
            s.runs[1].previous_on_line,
            s.runs[1].next_on_line,
        )
    });
    assert_eq!(linked, (true, true, false));
    // The caret at the line's end is on its `\n`, in the second piece.
    caret_at(&view, &mut vcx, 300);
    assert_eq!(
        described(&view, &mut vcx).1,
        Some(((1, 300 - source_a11y_max()), (1, 300 - source_a11y_max())))
    );
}

fn source_a11y_max() -> usize {
    crate::editor::source_a11y::MAX_RUN
}

#[gpui::test]
fn the_caret_line_off_screen_is_described(cx: &mut TestAppContext) {
    let text: String = (0..400).map(|i| format!("line {i}\n")).collect();
    let (view, mut vcx) = editor(cx, &text);
    let offset = text.find("line 300").expect("there") + 5;
    // The caret far below, the view left at the top.
    view.update(&mut vcx, |v, cx| {
        if let Some(doc) = v.doc.as_mut() {
            doc.editor.set_cursor(offset);
        }
        cx.notify();
    });
    let (runs, selection) = described(&view, &mut vcx);
    let last = runs.len() - 1;
    assert_eq!(runs[0].0, "line 0\n");
    assert_eq!(runs[last].0, "line 300\n");
    assert_eq!(selection, Some(((last, 5), (last, 5))));
}

#[gpui::test]
fn live_preview_runs_show_what_is_shown(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "Some **bold** text\nnext\n");
    view.update(&mut vcx, |v, cx| v.set_live_preview(true, cx));
    // The caret on the second line: the first one's markers are hidden.
    caret_at(&view, &mut vcx, 20);
    let (runs, selection) = described(&view, &mut vcx);
    assert_eq!(runs[0].0, "Some bold text\n");
    assert_eq!(selection, Some(((1, 1), (1, 1))));
    // "bold" starts after its hidden marker and ends before the other.
    let at = offsets(&view, &mut vcx, 0);
    assert_eq!(&at[5..10], &[7, 8, 9, 10, 13]);
    // A screen reader selecting "bold" selects its text in the buffer
    // (its end is the next character's start, past the hidden marker).
    select(&view, &mut vcx, (0, 5), (0, 9));
    assert_eq!(selections(&view, &mut vcx), vec![(7, 13)]);
}

#[gpui::test]
fn plain_text_is_described_as_source(cx: &mut TestAppContext) {
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_plain(true, cx);
                v.set_text("a **b**\tc\n", cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    let (runs, _) = described(&view, &mut vcx);
    assert_eq!(runs[0].0, "a **b**\tc\n");
}

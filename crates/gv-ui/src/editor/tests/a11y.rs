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

fn numbered(lines: usize) -> String {
    (0..lines).map(|i| format!("line {i}\n")).collect()
}

/// The lines drawn in the last frame.
fn on_screen(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> std::ops::Range<usize> {
    draw(vcx);
    probe_of(view, vcx).lines
}

fn run_texts(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<String> {
    described(view, vcx).0.into_iter().map(|r| r.0).collect()
}

#[gpui::test]
fn lines_past_the_screen_are_described(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, &numbered(1000));
    let shown = on_screen(&view, &mut vcx);
    let margin = crate::editor::source_a11y::margin(shown.len());
    assert!(margin >= crate::editor::source_a11y::MARGIN);
    // At the top: the screen and a margin below it.
    let runs = run_texts(&view, &mut vcx);
    assert_eq!(runs.len(), shown.len() + margin);
    assert_eq!(runs[0], "line 0\n");
    assert_eq!(
        runs[runs.len() - 1],
        format!("line {}\n", shown.end + margin - 1)
    );
    // In the middle: a margin above too, in order, after the caret's
    // line (at the top of the file).
    view.update(&mut vcx, |v, cx| v.scroll_to_line(500, cx));
    let shown = on_screen(&view, &mut vcx);
    assert_eq!(shown.start, 500);
    let (runs, selection) = described(&view, &mut vcx);
    let runs: Vec<String> = runs.into_iter().map(|r| r.0).collect();
    let mut expected = vec!["line 0\n".to_owned()];
    expected.extend((500 - margin..shown.end + margin).map(|i| format!("line {i}\n")));
    assert_eq!(runs, expected);
    assert_eq!(selection, Some(((0, 0), (0, 0))));
}

#[gpui::test]
fn a_selection_past_the_screen_scrolls_it_into_view(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, &numbered(1000));
    let shown = on_screen(&view, &mut vcx);
    // "Read next line" past the bottom: the caret on the first line below.
    let below = shown.end;
    let runs = run_texts(&view, &mut vcx);
    assert_eq!(runs[below], format!("line {below}\n"));
    select(&view, &mut vcx, (below, 0), (below, 0));
    let text = numbered(1000);
    let offset = text.find(&format!("line {below}\n")).expect("there");
    assert_eq!(selections(&view, &mut vcx), vec![(offset, offset)]);
    let now = on_screen(&view, &mut vcx);
    assert!(now.contains(&below), "{now:?}");
    // The description moved with it.
    let (runs, selection) = described(&view, &mut vcx);
    let at = runs
        .iter()
        .position(|r| r.0 == format!("line {below}\n"))
        .expect("described");
    assert_eq!(selection, Some(((at, 0), (at, 0))));
}

#[gpui::test]
fn a_screen_reader_scrolls_runs_and_the_input(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, &numbered(1000));
    let shown = on_screen(&view, &mut vcx);
    // A run below the screen into view: it is the bottom line now, and
    // the caret stays.
    let target = shown.end + 10;
    view.update(&mut vcx, |v, cx| v.scroll_run_from_a11y(target, None, cx));
    let now = on_screen(&view, &mut vcx);
    assert!(now.contains(&target), "{now:?}");
    assert!(!now.contains(&(target + 2)), "{now:?}");
    assert_eq!(selections(&view, &mut vcx), vec![(0, 0)]);
    // A run already shown: nothing moves.
    let first = now.start;
    let index = run_texts(&view, &mut vcx)
        .iter()
        .position(|r| *r == format!("line {}\n", first + 2))
        .expect("described");
    view.update(&mut vcx, |v, cx| v.scroll_run_from_a11y(index, None, cx));
    assert_eq!(on_screen(&view, &mut vcx).start, first);
    // A run's top to the view's top.
    let (index, top) = view.read_with(&vcx, |v, _| {
        let s = v.a11y_source();
        let i = s
            .runs
            .iter()
            .position(|r| r.text == format!("line {}\n", first + 20))
            .expect("described");
        (i, s.viewport.0)
    });
    view.update(&mut vcx, |v, cx| {
        v.scroll_run_from_a11y(index, Some(top), cx)
    });
    assert_eq!(on_screen(&view, &mut vcx).start, first + 20);
    // The input by a line, and by a page.
    view.update(&mut vcx, |v, cx| v.scroll_from_a11y(false, None, cx));
    assert_eq!(on_screen(&view, &mut vcx).start, first + 21);
    view.update(&mut vcx, |v, cx| v.scroll_from_a11y(true, None, cx));
    assert_eq!(on_screen(&view, &mut vcx).start, first + 20);
    let page = gpui::accesskit::ActionData::ScrollUnit(gpui::accesskit::ScrollUnit::Page);
    let before = on_screen(&view, &mut vcx);
    view.update(&mut vcx, |v, cx| v.scroll_from_a11y(false, Some(&page), cx));
    let after = on_screen(&view, &mut vcx);
    // A page less a line: the last line stays in view.
    assert!(
        after.start > before.start + 1 && after.start < before.end,
        "{before:?} {after:?}"
    );
}

fn obsidian() -> ParseOptions {
    ParseOptions::from_profile(&gv_profile::compile_builtin(
        gv_profile::ProfileId::Obsidian,
    ))
}

fn live(cx: &mut TestAppContext, text: &str) -> (Entity<EditorView>, VisualTestContext) {
    let (view, mut vcx) = editor_with(cx, text, obsidian());
    view.update(&mut vcx, |v, cx| {
        v.set_profile("obsidian", cx);
        v.set_live_preview(true, cx);
    });
    for _ in 0..8 {
        draw(&mut vcx);
    }
    (view, vcx)
}

#[gpui::test]
fn inline_math_reads_as_its_tex(cx: &mut TestAppContext) {
    // 012345678
    // a $x^2$ b
    let (view, mut vcx) = live(cx, "a $x^2$ b\nnext\n");
    caret_at(&view, &mut vcx, 12);
    let (runs, _) = described(&view, &mut vcx);
    assert_eq!(runs[0].0, "a x^2 b\n");
    // Words by UAX #29: `^` stands alone.
    assert_eq!(runs[0].2, vec![0, 2, 3, 4, 6]);
    // Its characters are all at the formula's start; the next one after
    // its end.
    assert_eq!(&offsets(&view, &mut vcx, 0)[1..7], &[1, 2, 2, 2, 7, 8]);
    // A screen reader's caret on it goes to the formula's start.
    select(&view, &mut vcx, (0, 3), (0, 3));
    assert_eq!(selections(&view, &mut vcx), vec![(2, 2)]);
}

#[gpui::test]
fn images_read_as_their_alt_text(cx: &mut TestAppContext) {
    let dir = temp(&[("note.md", "![a pic](pic.png) and ![](pic.png)\nnext\n")]);
    image::RgbaImage::from_pixel(40, 20, image::Rgba([10, 20, 30, 255]))
        .save(dir.path().join("pic.png"))
        .expect("png");
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| EditorView::new(obsidian(), cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    let path = dir.path().join("note.md");
    view.update(&mut vcx, |v, cx| {
        v.open(path, None, cx);
        v.set_profile("obsidian", cx);
        v.set_live_preview(true, cx);
    });
    for _ in 0..3 {
        draw(&mut vcx);
    }
    caret_at(&view, &mut vcx, 40);
    let kinds: Vec<&str> = probe_of(&view, &mut vcx)
        .widgets
        .iter()
        .map(|(_, k)| *k)
        .collect();
    assert_eq!(kinds, ["image", "image"]);
    // The alt text, or the file name without one.
    let (runs, _) = described(&view, &mut vcx);
    assert_eq!(runs[0].0, "a pic and pic.png\n");
}

#[gpui::test]
fn a_selection_in_a_block_shows_its_source(cx: &mut TestAppContext) {
    let text = "intro\n\n```rust\nfn a() {}\n```\n\nafter\n";
    let (view, mut vcx) = live(cx, text);
    assert_eq!(
        view.read_with(&vcx, |v, _| v.live_blocks()).len(),
        1,
        "rendered"
    );
    // The block's nodes go with where its source starts.
    draw(&mut vcx);
    let start = view.read_with(&vcx, |v, _| {
        let s = v.a11y_source();
        assert_eq!(s.blocks.len(), 1);
        s.blocks[0].1
    });
    assert_eq!(start, 7);
    // A screen reader's caret there: the block shows as source text.
    view.update_in(&mut vcx, |v, window, cx| {
        v.select_from_a11y(start, start, window, cx)
    });
    for _ in 0..4 {
        draw(&mut vcx);
    }
    assert!(view.read_with(&vcx, |v, _| v.live_blocks()).is_empty());
    let runs = run_texts(&view, &mut vcx);
    assert!(runs.iter().any(|r| r == "fn a() {}\n"), "{runs:?}");
}

#[gpui::test]
fn several_carets_are_counted(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, MIXED);
    let carets = |view: &Entity<EditorView>, vcx: &mut VisualTestContext| {
        draw(vcx);
        view.read_with(vcx, |v, _| v.a11y_source().carets)
    };
    assert_eq!(carets(&view, &mut vcx), 1);
    view.update(&mut vcx, |v, cx| {
        if let Some(doc) = v.doc.as_mut() {
            doc.editor.set_selections(
                &[
                    gv_editor::SelectionSpec::new(0, 0),
                    gv_editor::SelectionSpec::new(12, 12),
                    gv_editor::SelectionSpec::new(24, 27),
                ],
                1,
            );
        }
        cx.notify();
    });
    assert_eq!(carets(&view, &mut vcx), 3);
    // The selection is the primary one.
    assert_eq!(described(&view, &mut vcx).1, Some(((1, 0), (1, 0))));
    assert_eq!(
        crate::editor::source_a11y::carets_description(3),
        "3 carets"
    );
}

#[gpui::test]
fn crlf_and_cr_line_ends_are_one_character(cx: &mut TestAppContext) {
    // 0 1 2  3  4 5 6  7 8 9
    // a b \r \n c d \r e
    let (view, mut vcx) = editor(cx, "ab\r\ncd\re");
    let (runs, _) = described(&view, &mut vcx);
    let texts: Vec<&str> = runs.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(texts, ["ab\n", "cd\n", "e"]);
    assert_eq!(runs[0].1, vec![1, 1, 1]);
    // The `\n` stands for both bytes of `\r\n`: the next line starts at 4.
    assert_eq!(offsets(&view, &mut vcx, 0), vec![0, 1, 2, 4]);
    assert_eq!(offsets(&view, &mut vcx, 1), vec![4, 5, 6, 7]);
    assert_eq!(offsets(&view, &mut vcx, 2), vec![7, 8]);
    // The caret at a line's end is on its line break, at the next line's
    // start on the next run.
    caret_at(&view, &mut vcx, 2);
    assert_eq!(described(&view, &mut vcx).1, Some(((0, 2), (0, 2))));
    caret_at(&view, &mut vcx, 4);
    assert_eq!(described(&view, &mut vcx).1, Some(((1, 0), (1, 0))));
    // Shift+Left over the CRLF goes back two bytes, one character.
    vcx.simulate_keystrokes("shift-left");
    assert_eq!(selections(&view, &mut vcx), vec![(4, 2)]);
    assert_eq!(described(&view, &mut vcx).1, Some(((1, 0), (0, 2))));
    // A screen reader selecting the line break selects both bytes.
    select(&view, &mut vcx, (0, 2), (0, 3));
    assert_eq!(selections(&view, &mut vcx), vec![(2, 4)]);
    // The end of the text, after the last character.
    caret_at(&view, &mut vcx, 8);
    assert_eq!(described(&view, &mut vcx).1, Some(((2, 1), (2, 1))));
}

#[gpui::test]
fn combined_characters_are_one_character(cx: &mut TestAppContext) {
    // A family emoji joined by ZWJs (18 bytes), an e with a combining
    // acute accent (3 bytes) and a flag (8 bytes).
    let text = "👨‍👩‍👧 e\u{301} 🇩🇪\n";
    let (view, mut vcx) = editor(cx, text);
    let (runs, _) = described(&view, &mut vcx);
    assert_eq!(runs[0].0, text);
    assert_eq!(runs[0].1, vec![18, 1, 3, 1, 8, 1]);
    assert_eq!(offsets(&view, &mut vcx, 0), vec![0, 18, 19, 22, 23, 31, 32]);
    // Shift+Right selects a whole cluster; backwards too.
    caret_at(&view, &mut vcx, 19);
    vcx.simulate_keystrokes("shift-right");
    assert_eq!(selections(&view, &mut vcx), vec![(19, 22)]);
    assert_eq!(described(&view, &mut vcx).1, Some(((0, 2), (0, 3))));
    caret_at(&view, &mut vcx, 31);
    vcx.simulate_keystrokes("shift-left shift-left");
    assert_eq!(selections(&view, &mut vcx), vec![(31, 22)]);
    assert_eq!(described(&view, &mut vcx).1, Some(((0, 5), (0, 3))));
}

#[gpui::test]
fn a_big_document_is_described_around_the_screen_only(cx: &mut TestAppContext) {
    let lines = 20_000;
    let text = numbered(lines);
    let (view, mut vcx) = editor(cx, &text);
    view.update(&mut vcx, |v, cx| v.scroll_to_line(10_000, cx));
    let shown = on_screen(&view, &mut vcx);
    let margin = crate::editor::source_a11y::margin(shown.len());
    assert!(margin <= crate::editor::source_a11y::MAX_MARGIN);
    // The caret at the end of the text, far below the screen.
    view.update(&mut vcx, |v, cx| {
        if let Some(doc) = v.doc.as_mut() {
            doc.editor.set_cursor(text.len() - 3);
        }
        cx.notify();
    });
    let (runs, selection) = described(&view, &mut vcx);
    // The screen, a margin each side and the caret's line: no more.
    assert_eq!(runs.len(), shown.len() + 2 * margin + 1);
    assert_eq!(runs[0].0, format!("line {}\n", shown.start - margin));
    let last = runs.len() - 1;
    assert_eq!(runs[last].0, format!("line {}\n", lines - 1));
    let caret = runs[last].0.len() - 3;
    assert_eq!(selection, Some(((last, caret), (last, caret))));
    // A selection from the top of the screen to the caret: its anchor
    // in the runs, its focus on the caret's line.
    let top = text
        .find(&format!("line {}\n", shown.start))
        .expect("there");
    view.update(&mut vcx, |v, cx| {
        if let Some(doc) = v.doc.as_mut() {
            doc.editor
                .set_selections(&[gv_editor::SelectionSpec::new(top, text.len() - 3)], 0);
        }
        cx.notify();
    });
    let (_, selection) = described(&view, &mut vcx);
    assert_eq!(selection, Some(((margin, 0), (last, caret))));
}

/// A run of single-byte characters at `start`, for the description's
/// own lookups.
fn plain_run(line: usize, text: &str, start: usize) -> crate::editor::source_a11y::Run {
    let n = text.len();
    crate::editor::source_a11y::Run {
        key: (line, 0, 0),
        text: text.to_owned(),
        lengths: vec![1; n],
        word_starts: vec![0],
        positions: (0..n).map(|i| i as f32 * 8.0).collect(),
        widths: vec![8.0; n],
        offsets: (start..=start + n).collect(),
        bounds: gpui::Bounds::new(
            gpui::point(gpui::px(0.0), gpui::px(line as f32 * 20.0)),
            gpui::size(gpui::px(n as f32 * 8.0), gpui::px(20.0)),
        ),
        next_on_line: false,
        previous_on_line: false,
    }
}

#[test]
fn offsets_are_located_in_the_runs_and_clamped_outside_them() {
    // Lines 10 and 11 of a text, at offsets 100 and 104.
    let mut source = crate::editor::source_a11y::SourceA11y::default();
    source.runs = vec![plain_run(10, "abc\n", 100), plain_run(11, "de\n", 104)];
    assert_eq!(source.locate(100), Some((0, 0)));
    assert_eq!(source.locate(103), Some((0, 3)));
    assert_eq!(source.locate(104), Some((1, 0)));
    assert_eq!(source.locate(106), Some((1, 2)));
    // Before the first run and after the last: their edges.
    assert_eq!(source.locate(3), Some((0, 0)));
    assert_eq!(source.locate(107), Some((1, 3)));
    assert_eq!(source.locate(5000), Some((1, 3)));
    // And back.
    assert_eq!(source.offset_at((0, 2)), Some(102));
    assert_eq!(source.offset_at((1, 3)), Some(107));
    assert_eq!(source.offset_at((1, 99)), Some(107));
    assert_eq!(source.offset_at((2, 0)), None);
    // No runs: nowhere.
    let empty = crate::editor::source_a11y::SourceA11y::default();
    assert_eq!(empty.locate(0), None);
}

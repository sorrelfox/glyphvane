// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view in headless GPUI windows (GPUI's no-op text system: 0.6
//! em per character, so a 12.5 px character is 7.5 px wide): typing, input
//! method composition through the input handler, undo, several carets, the
//! Markdown commands, the mouse, the clipboard, the Mod+E round trip with
//! the Reading view, highlighting, the current line and the gutter. The timing runs with the
//! platform's real text system are `#[ignore]`d (`cargo test -p gv-ui
//! --release editor -- --ignored --nocapture`).

use std::path::Path;

use gpui::{
    AnyWindowHandle, AppContext as _, Bounds, ClipboardItem, Entity, EntityInputHandler as _,
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, TestAppContext,
    VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};
use gv_syntax::ParseOptions;

use super::{AddCursorBelow, EditorStatus, EditorView, LINE};
use crate::chrome::WindowChrome;
use crate::inline::Token;
use crate::keymap::actions::editor as commands;
use crate::metrics::size::{document, editor as editor_size};
use crate::shell::{OpenTarget, ViewMode, Workspace};
use crate::{AppSettings, ThemeChoice};

const WIDTH: f32 = 1280.0;
const HEIGHT: f32 = 800.0;
/// A character's advance in the test text system.
const CHAR: f32 = 7.5;

fn options() -> WindowOptions {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

/// A window holding only a Source view of `text`, focused and drawn.
fn editor_with(
    cx: &mut TestAppContext,
    text: &str,
    parse: ParseOptions,
) -> (Entity<EditorView>, VisualTestContext) {
    let text = text.to_owned();
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(parse, cx);
                v.set_text(&text, cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
    });
    draw(&mut vcx);
    (view, vcx)
}

fn editor(cx: &mut TestAppContext, text: &str) -> (Entity<EditorView>, VisualTestContext) {
    editor_with(cx, text, ParseOptions::default())
}

fn text_of(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> String {
    view.read_with(vcx, |v, _| {
        let text = v.editor().expect("loaded").text();
        text.text_for_range(0..text.len()).unwrap_or_default()
    })
}

fn selections(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<(usize, usize)> {
    view.read_with(vcx, |v, _| {
        v.editor()
            .expect("loaded")
            .selections()
            .iter()
            .map(|s| (s.tail(), s.head()))
            .collect()
    })
}

/// The window point of the caret position `offset` (the middle of its
/// row, at its left edge).
fn point_of(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
    offset: usize,
) -> Point<Pixels> {
    view.update_in(vcx, |v, window, cx| {
        let b = v
            .range_bounds(offset..offset, None, window, cx)
            .expect("on screen");
        point(b.left() + px(0.5), b.top() + px(LINE / 2.0))
    })
}

fn press(vcx: &mut VisualTestContext, at: Point<Pixels>, clicks: usize, modifiers: Modifiers) {
    vcx.simulate_event(MouseDownEvent {
        position: at,
        modifiers,
        button: MouseButton::Left,
        click_count: clicks,
        first_mouse: false,
    });
    vcx.simulate_event(MouseUpEvent {
        position: at,
        modifiers,
        button: MouseButton::Left,
        click_count: clicks,
    });
    draw(vcx);
}

// --- Typing, undo, several carets ---------------------------------------------

#[gpui::test]
fn typing_inserts_at_the_caret(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "world\n");
    vcx.simulate_input("Hello ");
    assert_eq!(text_of(&view, &mut vcx), "Hello world\n");
    vcx.simulate_keystrokes("end enter");
    vcx.simulate_input("again");
    assert_eq!(text_of(&view, &mut vcx), "Hello world\nagain\n");
    vcx.simulate_keystrokes("backspace backspace shift-left");
    assert_eq!(text_of(&view, &mut vcx), "Hello world\naga\n");
    assert_eq!(selections(&view, &mut vcx), [(15, 14)]);
    vcx.simulate_input("A");
    assert_eq!(text_of(&view, &mut vcx), "Hello world\nagA\n");
    // Printable keys as key events (how Wayland delivers a one-byte input
    // method commit) reach the text: none is bound in the editor.
    vcx.simulate_keystrokes("x space shift-y");
    assert_eq!(text_of(&view, &mut vcx), "Hello world\nagAx Y\n");
}

#[gpui::test]
fn undo_and_redo_restore_text_and_carets(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "abc\n");
    vcx.simulate_keystrokes("end");
    vcx.simulate_input("def");
    assert_eq!(text_of(&view, &mut vcx), "abcdef\n");
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "abc\n");
    assert_eq!(selections(&view, &mut vcx), [(3, 3)]);
    vcx.simulate_keystrokes("secondary-shift-z");
    assert_eq!(text_of(&view, &mut vcx), "abcdef\n");
    assert_eq!(selections(&view, &mut vcx), [(6, 6)]);
}

#[gpui::test]
fn several_carets_type_together(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "one\ntwo\nthree\n");
    vcx.dispatch_action(AddCursorBelow);
    vcx.dispatch_action(AddCursorBelow);
    assert_eq!(selections(&view, &mut vcx).len(), 3);
    vcx.simulate_input("- ");
    assert_eq!(text_of(&view, &mut vcx), "- one\n- two\n- three\n");
    // One undo step for all carets.
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "one\ntwo\nthree\n");
    // Escape keeps only the primary caret.
    vcx.simulate_keystrokes("escape");
    assert_eq!(selections(&view, &mut vcx).len(), 1);
}

// --- Input method ----------------------------------------------------------------

#[gpui::test]
fn composition_through_the_input_handler(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "ab\ncd\n");
    vcx.simulate_keystrokes("right");
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "に", None, window, cx);
        assert_eq!(v.marked_text_range(window, cx), Some(1..2));
        v.replace_and_mark_text_in_range(None, "にほ", Some(1..1), window, cx);
        assert_eq!(v.marked_text_range(window, cx), Some(1..3));
        let selected = v.selected_text_range(false, window, cx).expect("selection");
        assert_eq!(selected.range, 2..2);
    });
    assert_eq!(text_of(&view, &mut vcx), "aにほb\ncd\n");
    draw(&mut vcx);
    let (marked, columns) = view.read_with(&vcx, |v, _| {
        (v.probe.marked.clone(), v.probe.columns.expect("laid out"))
    });
    assert_eq!(marked.len(), 1, "the composition is underlined");
    assert_eq!(marked[0].0.x, px(columns.text_x + CHAR));
    assert_eq!(marked[0].1, px(2.0 * CHAR));

    // Commit: one undo step removes the whole composition.
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_text_in_range(None, "日本", window, cx);
        assert_eq!(v.marked_text_range(window, cx), None);
    });
    assert_eq!(text_of(&view, &mut vcx), "a日本b\ncd\n");
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "ab\ncd\n");

    // Unmark keeps the text.
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "´", None, window, cx);
        v.unmark_text(window, cx);
        assert_eq!(v.marked_text_range(window, cx), None);
    });
    assert_eq!(text_of(&view, &mut vcx), "a´b\ncd\n");
}

#[gpui::test]
fn input_geometry_follows_the_current_text_before_a_paint(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "ab\ncd\n");
    let columns = view.read_with(&vcx, |v, _| v.probe.columns.expect("laid out"));
    let row = |line: usize| columns.top + line as f32 * LINE;
    vcx.simulate_keystrokes("down end");
    view.update_in(&mut vcx, |v, window, cx| {
        // No frame between the edit and the queries.
        v.replace_and_mark_text_in_range(None, "かな", None, window, cx);
        let marked = v.marked_text_range(window, cx).expect("composing");
        assert_eq!(marked, 5..7);
        let start = v
            .bounds_for_range(5..5, columns.bounds, window, cx)
            .expect("bounds");
        assert_eq!(
            start.origin,
            point(px(columns.text_x + 2.0 * CHAR), px(row(1)))
        );
        let end = v
            .bounds_for_range(7..7, columns.bounds, window, cx)
            .expect("bounds");
        assert_eq!(end.origin.x, px(columns.text_x + 4.0 * CHAR));
        // And back from a point.
        let at = point(px(columns.text_x + 3.0 * CHAR + 1.0), px(row(1) + 5.0));
        assert_eq!(v.character_index_for_point(at, window, cx), Some(6));
        let mut adjusted = None;
        let text = v.text_for_range(3..7, &mut adjusted, window, cx);
        assert_eq!(text.as_deref(), Some("cdかな"));
        assert_eq!(adjusted, Some(3..7));
    });
}

#[gpui::test]
fn escape_cancels_a_composition_and_enter_commits_it(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "x\n");
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "かんじ", None, window, cx);
    });
    vcx.simulate_keystrokes("escape");
    assert_eq!(text_of(&view, &mut vcx), "x\n");
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "かんじ", None, window, cx);
    });
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "かんじx\n");
    let composing = view.read_with(&vcx, |v, _| v.editor().unwrap().is_composing());
    assert!(!composing);
}

// --- Mouse -------------------------------------------------------------------------

#[gpui::test]
fn clicking_and_dragging_select(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "alpha beta\ngamma delta\n");
    // Click: a caret.
    let at = point_of(&view, &mut vcx, 6);
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(6, 6)]);
    // Drag from "beta" to inside "delta".
    let from = point_of(&view, &mut vcx, 6);
    let to = point_of(&view, &mut vcx, 20);
    vcx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    vcx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(&mut vcx);
    assert_eq!(selections(&view, &mut vcx), [(6, 20)]);
    let probe = view.read_with(&vcx, |v, _| v.probe.clone());
    assert_eq!(probe.selections.len(), 2, "one rectangle per row");
    // Shift-click extends the selection.
    let at = point_of(&view, &mut vcx, 2);
    press(&mut vcx, at, 1, Modifiers::shift());
    assert_eq!(selections(&view, &mut vcx), [(6, 2)]);
    // Double click: the word; triple click: the line.
    let at = point_of(&view, &mut vcx, 14);
    press(&mut vcx, at, 2, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(11, 16)]);
    press(&mut vcx, at, 3, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(11, 23)]);
}

#[gpui::test]
fn alt_click_adds_a_caret(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "one\ntwo\n");
    let at = point_of(&view, &mut vcx, 1);
    press(&mut vcx, at, 1, Modifiers::none());
    let at = point_of(&view, &mut vcx, 5);
    press(&mut vcx, at, 1, Modifiers::alt());
    assert_eq!(selections(&view, &mut vcx), [(1, 1), (5, 5)]);
    vcx.simulate_input("_");
    assert_eq!(text_of(&view, &mut vcx), "o_ne\nt_wo\n");
}

// --- Clipboard ------------------------------------------------------------------------

#[gpui::test]
fn copy_cut_and_paste(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "one two\n");
    vcx.simulate_keystrokes("shift-right shift-right shift-right");
    vcx.dispatch_action(commands::Copy);
    let copied = vcx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied.as_deref(), Some("one"));
    vcx.simulate_keystrokes("end");
    vcx.dispatch_action(commands::Paste);
    assert_eq!(text_of(&view, &mut vcx), "one twoone\n");
    vcx.simulate_keystrokes("home shift-right shift-right shift-right shift-right");
    vcx.dispatch_action(commands::Cut);
    assert_eq!(text_of(&view, &mut vcx), "twoone\n");
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("one ")
    );
}

#[gpui::test]
fn several_carets_copy_and_paste_one_piece_each(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "ab\ncd\n");
    vcx.dispatch_action(AddCursorBelow);
    vcx.simulate_keystrokes("shift-right");
    vcx.dispatch_action(commands::Copy);
    assert_eq!(
        vcx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("a\nc")
    );
    vcx.simulate_keystrokes("end");
    vcx.dispatch_action(commands::Paste);
    assert_eq!(text_of(&view, &mut vcx), "aba\ncdc\n");
    // Lines from elsewhere: one per caret when the counts match.
    vcx.write_to_clipboard(ClipboardItem::new_string("1\n2".to_owned()));
    vcx.dispatch_action(commands::Paste);
    assert_eq!(text_of(&view, &mut vcx), "aba1\ncdc2\n");
    // Otherwise every caret gets the whole text.
    vcx.write_to_clipboard(ClipboardItem::new_string("x\ny\nz".to_owned()));
    vcx.dispatch_action(commands::Paste);
    assert_eq!(text_of(&view, &mut vcx), "aba1x\ny\nz\ncdc2x\ny\nz\n");
}

// --- Drawing --------------------------------------------------------------------------

#[gpui::test]
fn current_line_gutter_and_column(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "# Title\n\nbody\n```\ncode\n```\n");
    vcx.simulate_keystrokes("down down");
    draw(&mut vcx);
    let probe = view.read_with(&vcx, |v, _| v.probe.clone());
    let columns = probe.columns.expect("laid out");
    // The Reading view's text column: 680 wide, centred.
    let left = columns.bounds.left();
    let width = f32::from(columns.bounds.size.width);
    let expected = f32::from(left) + ((width - document::COLUMN_MAX) / 2.0).floor();
    assert_eq!(columns.text_x, expected);
    assert_eq!(columns.text_w, document::COLUMN_MAX);
    assert_eq!(
        columns.numbers_right,
        columns.text_x - editor_size::GUTTER_GAP
    );
    assert_eq!(columns.numbers_w, editor_size::LINE_NUMBER_WIDTH);
    // Every line has its number; the caret's is primary.
    assert_eq!(probe.lines, 0..7);
    assert_eq!(probe.numbers.len(), 7);
    for (line, token) in &probe.numbers {
        let expected = if *line == 2 {
            Token::TextPrimary
        } else {
            Token::TextTertiary
        };
        assert_eq!(*token, expected, "line {line}");
    }
    assert_eq!(probe.current, [2]);
    assert_eq!(probe.code, [3, 4, 5]);
    // The caret: 2 × 16 in the middle of its row.
    let caret = probe.carets[0];
    assert_eq!(caret.size, size(px(2.0), px(editor_size::CARET_HEIGHT)));
    assert_eq!(caret.origin.x, px(columns.text_x));
    assert_eq!(caret.origin.y, px(columns.top + 2.0 * LINE + 2.0));
}

#[gpui::test]
fn highlighting_roles_in_the_view(cx: &mut TestAppContext) {
    let text =
        "---\ntitle: Notes\n---\n# Heading\n\nSome **strong**, `code` and [a link](other.md).\n";
    let parse = ParseOptions::for_profile("gfm-file").expect("profile");
    let (view, mut vcx) = editor_with(cx, text, parse);
    let style = |vcx: &mut VisualTestContext, line: usize, needle: &str| {
        let col = text.lines().nth(line).unwrap().find(needle).unwrap();
        view.read_with(vcx, |v, _| {
            let doc = v.doc.as_ref().unwrap();
            let s = doc.highlights.get(line).expect("highlighted");
            s.runs
                .iter()
                .find(|(r, _)| r.contains(&col))
                .map(|(_, s)| *s)
                .unwrap()
        })
    };
    assert_eq!(style(&mut vcx, 1, "title").color, Token::SyntaxKey);
    assert_eq!(style(&mut vcx, 1, "Notes").color, Token::SyntaxString);
    assert_eq!(style(&mut vcx, 3, "#").color, Token::TextTertiary);
    let heading = style(&mut vcx, 3, "Heading");
    assert!(heading.bold && heading.color == Token::TextPrimary);
    let strong = style(&mut vcx, 5, "strong");
    assert!(strong.bold && strong.color == Token::TextPrimary);
    assert_eq!(style(&mut vcx, 5, "**").color, Token::TextTertiary);
    assert_eq!(style(&mut vcx, 5, "code").color, Token::SyntaxCode);
    assert_eq!(style(&mut vcx, 5, "other.md").color, Token::AccentDefault);
    assert_eq!(style(&mut vcx, 5, "a link").color, Token::TextPrimary);
    // Typing keeps the highlighting in step (incremental reparse).
    vcx.simulate_keystrokes("secondary-end");
    vcx.simulate_input("`more`");
    draw(&mut vcx);
    assert_eq!(style(&mut vcx, 5, "Some").color, Token::TextPrimary);
    let last = text.lines().count();
    let code = view.read_with(&vcx, |v, _| {
        let doc = v.doc.as_ref().unwrap();
        doc.highlights.get(last).unwrap().runs.clone()
    });
    assert!(
        code.iter().any(|(_, s)| s.color == Token::SyntaxCode),
        "{code:?}"
    );
}

#[gpui::test]
fn whitespace_toggle_shows_marks(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a\tb  \nc\n");
    vcx.dispatch_action(commands::ToggleWhitespace);
    draw(&mut vcx);
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(0, window, cx).unwrap());
    assert_eq!(row.text.display, "a→  b··↵");
    vcx.dispatch_action(commands::ToggleWhitespace);
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(0, window, cx).unwrap());
    assert_eq!(row.text.display, "a   b  ");
}

#[gpui::test]
fn long_lines_scroll_sideways_to_the_caret_without_soft_wrap(cx: &mut TestAppContext) {
    let long = "x".repeat(400);
    let (view, mut vcx) = editor(cx, &format!("{long}\n"));
    view.update(&mut vcx, |v, cx| v.set_soft_wrap(false, cx));
    draw(&mut vcx);
    vcx.simulate_keystrokes("end");
    draw(&mut vcx);
    let (scroll_x, carets, columns) = view.read_with(&vcx, |v, _| {
        (v.scroll_x, v.probe.carets.clone(), v.probe.columns.unwrap())
    });
    assert!(scroll_x > 0.0);
    let caret = carets[0];
    assert!(f32::from(caret.right()) <= columns.text_right() + 2.0);
    assert!(f32::from(caret.left()) > columns.text_x);
}

// --- Soft wrap -------------------------------------------------------------------

/// The text column of a 1280 px window: 680 px, 90 characters and a bit.
const COLUMN: f32 = document::COLUMN_MAX;

/// `count` numbered words of five characters and a space ("w0000 ").
fn words(count: usize) -> String {
    (0..count).map(|i| format!("w{i:04} ")).collect()
}

fn probe_of(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> super::element::Probe {
    view.read_with(vcx, |v, _| v.probe.clone())
}

/// The rows drawn for `line`: (display row, continuation indent).
fn rows_of(probe: &super::element::Probe, line: usize) -> Vec<(usize, Option<f32>)> {
    probe
        .row_lines
        .iter()
        .filter(|(_, l, _)| *l == line)
        .map(|(row, _, indent)| (*row, *indent))
        .collect()
}

#[gpui::test]
fn a_long_list_item_wraps_at_the_column_with_a_hanging_indent(cx: &mut TestAppContext) {
    // 40 words of 6 characters after "- ": 242 characters, three rows of at
    // most 90.
    let item = format!("- {}", words(40));
    let (view, mut vcx) = editor(cx, &format!("{item}\nplain\n"));
    let width = view.read_with(&vcx, |v, _| v.editor().unwrap().wrap_width());
    assert_eq!(width, Some(COLUMN));
    let probe = probe_of(&view, &mut vcx);
    let rows = rows_of(&probe, 0);
    assert_eq!(rows.len(), 3, "{rows:?}");
    // Continuation rows start two characters in, under the item's text.
    assert_eq!(rows[0], (0, None));
    assert_eq!(rows[1], (1, Some(2.0 * CHAR)));
    assert_eq!(rows[2], (2, Some(2.0 * CHAR)));
    assert_eq!(rows_of(&probe, 1), [(3, None)]);
    // No row is wider than the column, and words are not split.
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(0, window, cx).unwrap());
    assert!(row.content_width() <= COLUMN, "{}", row.content_width());
    for segment in &row.segments[1..] {
        let first = &row.text.display[segment.display.clone()];
        assert!(first.starts_with('w'), "{first:?}");
        assert_eq!(segment.columns.start % 6, 2, "breaks after a word");
    }
    // Plain prose keeps its own indent (none).
    let prose = words(40);
    let (view, mut vcx) = editor(cx, &format!("{prose}\n"));
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(rows_of(&probe, 0)[1], (1, Some(0.0)));
}

#[gpui::test]
fn gutter_numbers_and_the_current_line_follow_wrapped_lines(cx: &mut TestAppContext) {
    let text = format!("short\n{}\n> {}\nend\n", words(30), words(20));
    let (view, mut vcx) = editor(cx, &text);
    // The caret on the long line (line 1).
    view.update(&mut vcx, |v, cx| {
        v.doc.as_mut().unwrap().editor.set_cursor(10);
        v.autoscroll = true;
        cx.notify();
    });
    draw(&mut vcx);
    let probe = probe_of(&view, &mut vcx);
    // 180 characters: two rows; the quote 122: two rows.
    // Line 4 is the empty line after the last line break.
    let firsts: Vec<usize> = (0..5).map(|l| rows_of(&probe, l)[0].0).collect();
    assert_eq!(firsts, [0, 1, 3, 5, 6]);
    assert_eq!(probe.number_rows, firsts, "a number on first rows only");
    assert_eq!(probe.numbers.len(), 5);
    assert_eq!(probe.row_lines.len(), 7);
    assert_eq!(probe.rows, 0..7);
    assert_eq!(rows_of(&probe, 2)[1].1, Some(2.0 * CHAR), "quote hangs");
    // The current-line fill covers both rows of line 1.
    assert_eq!(probe.current, [1]);
    let fill = probe.current_fills[0];
    let columns = probe.columns.unwrap();
    assert_eq!(fill.origin.y, px(columns.top + LINE));
    assert_eq!(fill.size.height, px(2.0 * LINE));
}

#[gpui::test]
fn resizing_rewraps_and_keeps_the_column(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, &format!("{}\n", words(40)));
    let rows = |vcx: &mut VisualTestContext| rows_of(&probe_of(&view, vcx), 0).len();
    assert_eq!(rows(&mut vcx), 3);
    // 400 px less 2 × 40 px of padding: 320 px, 42 characters a row.
    vcx.simulate_resize(size(px(400.0), px(HEIGHT)));
    draw(&mut vcx);
    let width = view.read_with(&vcx, |v, _| v.editor().unwrap().wrap_width());
    assert_eq!(width, Some(320.0));
    assert_eq!(rows(&mut vcx), 6);
    vcx.simulate_resize(size(px(WIDTH), px(HEIGHT)));
    draw(&mut vcx);
    assert_eq!(rows(&mut vcx), 3);
    // No sideways scroll while wrapping.
    vcx.simulate_keystrokes("ctrl-end");
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |v, _| v.scroll_x), 0.0);
}

#[gpui::test]
fn up_and_down_move_by_wrap_rows(cx: &mut TestAppContext) {
    let paragraph = words(40);
    let (view, mut vcx) = editor(cx, &format!("{paragraph}\nnext line\n"));
    let breaks: Vec<usize> = view.update_in(&mut vcx, |v, window, cx| {
        let row = v.row(0, window, cx).unwrap();
        row.segments.iter().map(|s| s.columns.start).collect()
    });
    assert_eq!(breaks.len(), 3, "{breaks:?}");
    vcx.simulate_keystrokes("right right right");
    vcx.simulate_keystrokes("down");
    assert_eq!(
        selections(&view, &mut vcx),
        [(breaks[1] + 3, breaks[1] + 3)]
    );
    vcx.simulate_keystrokes("down");
    assert_eq!(
        selections(&view, &mut vcx),
        [(breaks[2] + 3, breaks[2] + 3)]
    );
    // The caret is drawn on the third row.
    draw(&mut vcx);
    let probe = probe_of(&view, &mut vcx);
    let columns = probe.columns.unwrap();
    let caret = probe.carets[0];
    assert_eq!(caret.origin.y, px(columns.top + 2.0 * LINE + 2.0));
    assert_eq!(caret.origin.x, px(columns.text_x + 3.0 * CHAR));
    vcx.simulate_keystrokes("down");
    let next = paragraph.len() + 1;
    assert_eq!(selections(&view, &mut vcx), [(next + 3, next + 3)]);
    vcx.simulate_keystrokes("up up");
    assert_eq!(
        selections(&view, &mut vcx),
        [(breaks[1] + 3, breaks[1] + 3)]
    );
    // Home goes to the row's start first, then the line's; End to the
    // row's end (before the space it wraps at), then the line's.
    vcx.simulate_keystrokes("home");
    assert_eq!(selections(&view, &mut vcx), [(breaks[1], breaks[1])]);
    vcx.simulate_keystrokes("home");
    assert_eq!(selections(&view, &mut vcx), [(0, 0)]);
    vcx.simulate_keystrokes("down end");
    assert_eq!(
        selections(&view, &mut vcx),
        [(breaks[2] - 1, breaks[2] - 1)]
    );
    vcx.simulate_keystrokes("end");
    assert_eq!(
        selections(&view, &mut vcx),
        [(paragraph.len(), paragraph.len())]
    );
}

#[gpui::test]
fn clicks_on_continuation_rows_map_to_their_text(cx: &mut TestAppContext) {
    let item = format!("- {}", words(40));
    let (view, mut vcx) = editor(cx, &format!("{item}\n"));
    let (breaks, columns) = view.update_in(&mut vcx, |v, window, cx| {
        let row = v.row(0, window, cx).unwrap();
        let breaks: Vec<usize> = row.segments.iter().map(|s| s.columns.start).collect();
        (breaks, v.probe.columns.unwrap())
    });
    // The fourth character of the second row, after the hanging indent.
    let at = point(
        px(columns.text_x + 2.0 * CHAR + 3.0 * CHAR + 1.0),
        px(columns.top + LINE + LINE / 2.0),
    );
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(
        selections(&view, &mut vcx),
        [(breaks[1] + 3, breaks[1] + 3)]
    );
    // In the indent: the row's start.
    let at = point(px(columns.text_x + 4.0), px(columns.top + LINE + 4.0));
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(breaks[1], breaks[1])]);
    // Past the end of a row that wraps: before its last character, on it.
    let at = point(
        px(columns.text_right() - 2.0),
        px(columns.top + LINE + LINE / 2.0),
    );
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(
        selections(&view, &mut vcx),
        [(breaks[2] - 1, breaks[2] - 1)]
    );
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(
        probe.carets[0].origin.y,
        px(columns.top + LINE + 2.0),
        "the caret stays on the row clicked"
    );
    // A drag from the first row to the third selects across rows: one
    // rectangle per row.
    let from = point(px(columns.text_x + 1.0), px(columns.top + LINE / 2.0));
    let to = point(
        px(columns.text_x + 2.0 * CHAR + 1.0),
        px(columns.top + 2.0 * LINE + LINE / 2.0),
    );
    vcx.simulate_event(MouseDownEvent {
        position: from,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    vcx.simulate_event(MouseUpEvent {
        position: to,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 1,
    });
    draw(&mut vcx);
    assert_eq!(selections(&view, &mut vcx), [(0, breaks[2])]);
    assert_eq!(probe_selections(&view, &mut vcx), 2);
}

fn probe_selections(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> usize {
    view.read_with(vcx, |v, _| v.probe.selections.len())
}

#[gpui::test]
fn input_method_bounds_on_a_wrapped_row(cx: &mut TestAppContext) {
    let item = format!("- {}", words(40));
    let (view, mut vcx) = editor(cx, &format!("{item}\n"));
    let (breaks, columns) = view.update_in(&mut vcx, |v, window, cx| {
        let row = v.row(0, window, cx).unwrap();
        let breaks: Vec<usize> = row.segments.iter().map(|s| s.columns.start).collect();
        (breaks, v.probe.columns.unwrap())
    });
    // Compose on the second row, two characters in (all ASCII: UTF-16
    // offsets are bytes).
    let at = breaks[1] + 2;
    view.update_in(&mut vcx, |v, window, cx| {
        v.doc.as_mut().unwrap().editor.set_cursor(at);
        v.replace_and_mark_text_in_range(None, "xy", None, window, cx);
    });
    draw(&mut vcx);
    let bounds = view
        .update_in(&mut vcx, |v, window, cx| {
            let element = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
            v.bounds_for_range(at..at + 2, element, window, cx)
        })
        .expect("bounds");
    assert_eq!(bounds.top(), px(columns.top + LINE));
    assert_eq!(bounds.left(), px(columns.text_x + 2.0 * CHAR + 2.0 * CHAR));
    assert_eq!(bounds.size.height, px(LINE));
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(probe.marked.len(), 1);
    assert_eq!(probe.marked[0].0.x, bounds.left());
    // And the point there maps back to the same character.
    let back = view.update_in(&mut vcx, |v, window, cx| {
        v.character_index_for_point(bounds.origin + point(px(1.0), px(LINE / 2.0)), window, cx)
    });
    assert_eq!(back, Some(at));
}

#[gpui::test]
fn large_file_mode_keeps_soft_wrap_off(cx: &mut TestAppContext) {
    let limits = gv_settings::Editor {
        large_file_bytes: 1024,
        large_file_lines: 1000,
        large_file_line_chars: 1000,
        ..gv_settings::Editor::default()
    };
    let small = gv_text::TextSnapshot::new("a\nb\n");
    assert!(!super::wrap::is_large_file(&small, &limits));
    let big = gv_text::TextSnapshot::new(&"x".repeat(2000));
    assert!(super::wrap::is_large_file(&big, &limits));
    let roomy = gv_settings::Editor {
        large_file_bytes: 1 << 20,
        ..limits.clone()
    };
    let long_line = gv_text::TextSnapshot::new(&format!("{}\n", "é".repeat(900)));
    assert!(
        !super::wrap::is_large_file(&long_line, &roomy),
        "900 characters in 1800 bytes"
    );
    let longer = gv_text::TextSnapshot::new(&format!("a\n{}\n", "é".repeat(1001)));
    assert!(super::wrap::is_large_file(&longer, &roomy));
    let many = gv_text::TextSnapshot::new(&"\n".repeat(1001));
    assert!(super::wrap::is_large_file(&many, &roomy));

    let (view, mut vcx) = editor(cx, &format!("{}\n", words(40)));
    view.update(&mut vcx, |v, cx| {
        v.doc.as_mut().unwrap().large_file = true;
        cx.notify();
    });
    draw(&mut vcx);
    let (width, large) = view.read_with(&vcx, |v, _| {
        (v.editor().unwrap().wrap_width(), v.is_large_file())
    });
    assert!(large);
    assert_eq!(width, None);
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 1);
}

#[gpui::test]
fn the_top_line_stays_when_the_wrap_width_changes(cx: &mut TestAppContext) {
    let mut text = String::new();
    for _ in 0..100 {
        text.push_str(&words(40));
        text.push('\n');
    }
    let (view, mut vcx) = editor(cx, &text);
    view.update(&mut vcx, |v, cx| v.scroll_to_line(50, cx));
    draw(&mut vcx);
    let top = view.read_with(&vcx, |v, _| v.top_line());
    assert_eq!(top, 50);
    vcx.simulate_resize(size(px(400.0), px(HEIGHT)));
    draw(&mut vcx);
    let top = view.read_with(&vcx, |v, _| v.top_line());
    assert_eq!(top, 50);
}

/// `lines` lines of 40 words: over [`super::wrap::SYNC_REWRAP_BYTES`] from
/// 1100 lines, each three rows in the full column.
fn long_lines(lines: usize) -> String {
    let mut text = String::new();
    for _ in 0..lines {
        text.push_str(&words(40));
        text.push('\n');
    }
    text
}

/// What the wrap layer of `view` shows, and the same text wrapped at the
/// same width at once with the test text system's advance.
fn wrapped_and_expected(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
) -> (String, String) {
    view.read_with(vcx, |v, _| {
        let editor = v.editor().expect("loaded");
        let shown = editor.wrap().expect("soft wrap is on").text();
        let text = editor.text();
        let mut fresh = gv_editor::Editor::new(gv_text::Buffer::new(
            &text.text_for_range(0..text.len()).unwrap(),
        ));
        fresh.set_measure(Box::new(gv_editor::display_map::MonospaceMeasure::new(
            CHAR,
        )));
        fresh.set_wrap_indent(std::sync::Arc::new(
            gv_editor::display_map::MarkdownHangingIndent::default(),
        ));
        fresh.set_wrap_width(editor.wrap_width());
        (shown, fresh.wrap().expect("on").text())
    })
}

#[gpui::test]
fn a_large_document_wraps_in_the_background(cx: &mut TestAppContext) {
    let text = long_lines(1200);
    assert!(text.len() > super::wrap::SYNC_REWRAP_BYTES);
    let (view, mut vcx) = editor(cx, &text);
    draw(&mut vcx);
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 3);
    let (shown, expected) = wrapped_and_expected(&view, &mut vcx);
    assert_eq!(shown, expected);

    // Soft wrap turned on: the job starts with the frame and the lines
    // show unwrapped until it lands; an edit made meanwhile is kept.
    view.update(&mut vcx, |v, cx| v.set_soft_wrap(false, cx));
    draw(&mut vcx);
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 1);
    view.update(&mut vcx, |v, cx| {
        v.set_soft_wrap(true, cx);
        v.sync_wrap(COLUMN, cx);
        let editor = &mut v.doc.as_mut().unwrap().editor;
        assert!(editor.is_rewrapping());
        assert_eq!(editor.wrap_width(), Some(COLUMN));
        assert_eq!(editor.wrap().unwrap().max_point().row().0, 1200);
        editor.insert_at_selections("new words ");
        assert!(editor.wrap().unwrap().is_interpolated());
    });
    draw(&mut vcx);
    let rewrapping = view.read_with(&vcx, |v, _| v.editor().unwrap().is_rewrapping());
    assert!(!rewrapping);
    assert!(text_of(&view, &mut vcx).starts_with("new words w0000 "));
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 3);
    let (shown, expected) = wrapped_and_expected(&view, &mut vcx);
    assert_eq!(shown, expected);
}

#[gpui::test]
fn resizing_a_large_document_rewraps_in_the_background_and_keeps_the_top_line(
    cx: &mut TestAppContext,
) {
    let (view, mut vcx) = editor(cx, &long_lines(1200));
    draw(&mut vcx);
    view.update(&mut vcx, |v, cx| v.scroll_to_line(600, cx));
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |v, _| v.top_line()), 600);
    let rows = |vcx: &mut VisualTestContext| rows_of(&probe_of(&view, vcx), 600).len();
    assert_eq!(rows(&mut vcx), 3);

    // The old wraps stay while the width settles.
    vcx.simulate_resize(size(px(400.0), px(HEIGHT)));
    draw(&mut vcx);
    let width = view.read_with(&vcx, |v, _| v.editor().unwrap().wrap_width());
    assert_eq!(width, Some(COLUMN));
    assert_eq!(rows(&mut vcx), 3);
    // Then the job runs and lands with the top line in place.
    vcx.executor().advance_clock(super::wrap::REWRAP_DELAY);
    draw(&mut vcx);
    let (width, rewrapping, top) = view.read_with(&vcx, |v, _| {
        let editor = v.editor().unwrap();
        (editor.wrap_width(), editor.is_rewrapping(), v.top_line())
    });
    assert_eq!(width, Some(320.0));
    assert!(!rewrapping);
    assert_eq!(top, 600);
    assert_eq!(rows(&mut vcx), 6);
    let (shown, expected) = wrapped_and_expected(&view, &mut vcx);
    assert_eq!(shown, expected);

    // Soft wrap turned off while a new width settles: no re-wrap.
    vcx.simulate_resize(size(px(WIDTH), px(HEIGHT)));
    draw(&mut vcx);
    view.update(&mut vcx, |v, cx| v.toggle_soft_wrap(cx));
    draw(&mut vcx);
    vcx.executor().advance_clock(super::wrap::REWRAP_DELAY);
    draw(&mut vcx);
    let (width, rewrapping, top) = view.read_with(&vcx, |v, _| {
        let editor = v.editor().unwrap();
        (editor.wrap_width(), editor.is_rewrapping(), v.top_line())
    });
    assert_eq!(width, None);
    assert!(!rewrapping);
    assert_eq!(top, 600);
    assert_eq!(rows(&mut vcx), 1);
    // And on again, in the background.
    view.update(&mut vcx, |v, cx| v.toggle_soft_wrap(cx));
    draw(&mut vcx);
    let (width, rewrapping, top) = view.read_with(&vcx, |v, _| {
        let editor = v.editor().unwrap();
        (editor.wrap_width(), editor.is_rewrapping(), v.top_line())
    });
    assert_eq!(width, Some(COLUMN));
    assert!(!rewrapping);
    assert_eq!(top, 600);
    assert_eq!(rows(&mut vcx), 3);
}

#[gpui::test]
fn the_soft_wrap_command_turns_soft_wrap_off_and_on(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, &format!("{}\n", words(40)));
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 3);
    vcx.dispatch_action(crate::keymap::actions::view::ToggleSoftWrap);
    draw(&mut vcx);
    let state = view.read_with(&vcx, |v, _| {
        (v.soft_wrap(), v.editor().unwrap().wrap_width())
    });
    assert_eq!(state, (false, None));
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 1);
    vcx.dispatch_action(crate::keymap::actions::view::ToggleSoftWrap);
    draw(&mut vcx);
    let state = view.read_with(&vcx, |v, _| {
        (v.soft_wrap(), v.editor().unwrap().wrap_width())
    });
    assert_eq!(state, (true, Some(COLUMN)));
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 3);
}

// --- In the window: Mod+E -----------------------------------------------------------

fn temp(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    dir
}

fn open_workspace(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        gpui::open_window(options(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

#[gpui::test]
fn mod_e_round_trip_shows_the_edited_text_in_preview(cx: &mut TestAppContext) {
    let dir = temp(&[("note.md", "# Title\n\nFirst paragraph.\n")]);
    let (window, workspace) = open_workspace(cx, &dir.path().join("note.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);

    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    let (mode, panels) =
        workspace.read_with(&vcx, |ws, _| (ws.active_tab().mode(), ws.panels_open()));
    assert_eq!((mode, panels), (ViewMode::Edit, (false, false)));
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    assert_eq!(
        editor.read_with(&vcx, |e, _| e.status().clone()),
        EditorStatus::Ready
    );
    let focused = vcx.update(|window, cx| editor.read(cx).focus.is_focused(window));
    assert!(focused, "the editor has the keyboard");

    vcx.simulate_keystrokes("secondary-end");
    vcx.simulate_input("\nAdded in the editor.");
    draw(&mut vcx);

    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    let (mode, panels) =
        workspace.read_with(&vcx, |ws, _| (ws.active_tab().mode(), ws.panels_open()));
    assert_eq!((mode, panels), (ViewMode::Preview, (false, false)));
    let reader = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("reader");
    let shown = reader
        .read_with(&vcx, |r, _| r.plain_text())
        .unwrap_or_default();
    assert!(shown.contains("Added in the editor."), "{shown:?}");
    // Not saved yet: autosave waits for a second without edits, and the
    // test clock stands still.
    let on_disk = std::fs::read_to_string(dir.path().join("note.md")).unwrap();
    assert!(!on_disk.contains("Added"));

    // Back to Edit: the same editor, with the edit.
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    let again = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("editor");
    assert_eq!(again, editor);
    assert!(text_of(&editor, &mut vcx).ends_with("\n\nAdded in the editor."));
}

#[gpui::test]
fn mode_switch_keeps_the_top_line(cx: &mut TestAppContext) {
    let mut doc = String::new();
    for i in 0..200 {
        doc.push_str(&format!("Paragraph {i} with some words.\n\n"));
    }
    let dir = temp(&[("long.md", &doc)]);
    let (window, workspace) = open_workspace(cx, &dir.path().join("long.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    let reader = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().reader().cloned())
        .expect("reader");
    // Paragraph 100 starts at line 200.
    let target = doc.find("Paragraph 100 ").unwrap();
    reader.update(&mut vcx, |r, cx| {
        r.scroll_to(crate::reader::ScrollTarget::Offset(target), cx);
    });
    draw(&mut vcx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("editor");
    let top = editor.read_with(&vcx, |e, _| e.top_offset()).unwrap();
    assert_eq!(top, target);
    let probe = editor.read_with(&vcx, |e, _| e.probe.clone());
    assert!(probe.lines.contains(&200), "{:?}", probe.lines);

    // And back: the Reading view's top block is the editor's top line.
    editor.update(&mut vcx, |e, cx| {
        e.scroll_to_offset(doc.find("Paragraph 150 ").unwrap(), cx);
    });
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    draw(&mut vcx);
    let position = reader.read_with(&vcx, |r, _| r.position());
    assert_eq!(
        position,
        Some(crate::reader::ScrollTarget::Offset(
            doc.find("Paragraph 150 ").unwrap()
        ))
    );
}

#[gpui::test]
fn a_file_that_cannot_be_read_shows_why(cx: &mut TestAppContext) {
    let dir = temp(&[]);
    let (window, workspace) = open_workspace(cx, &dir.path().join("missing.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("editor");
    let status = editor.read_with(&vcx, |e, _| e.status().clone());
    assert!(matches!(status, EditorStatus::Failed(_)), "{status:?}");
}

// --- Markdown commands -----------------------------------------------------------------

/// A Source view of `text` in profile `profile`, with these selections
/// (start and end offsets).
fn editor_in(
    cx: &mut TestAppContext,
    profile: &str,
    text: &str,
    ranges: &[(usize, usize)],
) -> (Entity<EditorView>, VisualTestContext) {
    let text = text.to_owned();
    let profile = profile.to_owned();
    let ranges: Vec<_> = ranges.iter().map(|&(start, end)| start..end).collect();
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options(), cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_profile(&profile, cx);
                v.set_text(&text, cx);
                let doc = v.doc.as_mut().expect("loaded");
                doc.editor.select_ranges(ranges);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
    });
    draw(&mut vcx);
    (view, vcx)
}

/// Whether the command `id` has a handler where the editor is, as the
/// application menu asks.
fn handled(view: &Entity<EditorView>, vcx: &mut VisualTestContext, id: &str) -> bool {
    draw(vcx);
    let action = crate::keymap::action_for(id).expect("an action");
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        window.is_action_available_in(action.as_ref(), &focus)
    })
}

#[gpui::test]
fn enter_continues_a_list_and_renumbers(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "1. one\n2. two\n", &[(6, 6)]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "1. one\n2. \n3. two\n");
    assert_eq!(selections(&view, &mut vcx), [(10, 10)]);
    vcx.simulate_input("new");
    assert_eq!(text_of(&view, &mut vcx), "1. one\n2. new\n3. two\n");
    // Enter on an empty item at the top level ends the list.
    let (view, mut vcx) = editor_in(cx, "commonmark", "1. one\n2. \n", &[(10, 10)]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "1. one\n\n");
    // Shift+Enter stays in the item without a marker.
    let (view, mut vcx) = editor_in(cx, "commonmark", "- one\n", &[(5, 5)]);
    vcx.simulate_keystrokes("shift-enter");
    assert_eq!(text_of(&view, &mut vcx), "- one\n  \n");
}

#[gpui::test]
fn enter_in_a_quote_continues_it(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "> quoted\n", &[(8, 8)]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "> quoted\n> \n");
    // Outside lists and quotes Enter is a plain line break.
    let (view, mut vcx) = editor_in(cx, "commonmark", "plain\n", &[(5, 5)]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "plain\n\n");
}

#[gpui::test]
fn tab_and_shift_tab_nest_and_unnest(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "- a\n- b\n", &[(7, 7)]);
    vcx.simulate_keystrokes("tab");
    assert_eq!(text_of(&view, &mut vcx), "- a\n  - b\n");
    vcx.simulate_keystrokes("shift-tab");
    assert_eq!(text_of(&view, &mut vcx), "- a\n- b\n");
    // Ordered children take the parent's content offset.
    let (view, mut vcx) = editor_in(cx, "commonmark", "1. a\n2. b\n3. c\n", &[(9, 9)]);
    vcx.simulate_keystrokes("tab");
    assert_eq!(text_of(&view, &mut vcx), "1. a\n   1. b\n2. c\n");
    // Outside a list Tab inserts a tab.
    let (view, mut vcx) = editor_in(cx, "commonmark", "text\n", &[(0, 0)]);
    vcx.simulate_keystrokes("tab");
    assert_eq!(text_of(&view, &mut vcx), "\ttext\n");
}

#[gpui::test]
fn mod_b_toggles_bold_on_a_selection(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "one two\n", &[(4, 7)]);
    vcx.simulate_keystrokes("secondary-b");
    assert_eq!(text_of(&view, &mut vcx), "one **two**\n");
    assert_eq!(selections(&view, &mut vcx), [(6, 9)]);
    vcx.simulate_keystrokes("secondary-b");
    assert_eq!(text_of(&view, &mut vcx), "one two\n");
    vcx.simulate_keystrokes("secondary-i");
    assert_eq!(text_of(&view, &mut vcx), "one *two*\n");
    vcx.simulate_keystrokes("secondary-shift-c");
    assert_eq!(text_of(&view, &mut vcx), "one *`two`*\n");
}

#[gpui::test]
fn heading_commands_set_the_level(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "Title\n\ntext\n", &[(2, 2)]);
    vcx.simulate_keystrokes("secondary-alt-2");
    assert_eq!(text_of(&view, &mut vcx), "## Title\n\ntext\n");
    vcx.simulate_keystrokes("secondary-alt-1");
    assert_eq!(text_of(&view, &mut vcx), "# Title\n\ntext\n");
    vcx.simulate_keystrokes("secondary-alt-0");
    assert_eq!(text_of(&view, &mut vcx), "Title\n\ntext\n");
    vcx.dispatch_action(commands::Quote);
    assert_eq!(text_of(&view, &mut vcx), "> Title\n\ntext\n");
}

#[gpui::test]
fn insert_link_takes_the_url_on_the_clipboard(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "see docs\n", &[(4, 8)]);
    vcx.write_to_clipboard(ClipboardItem::new_string(
        "https://example.com/a\n".to_owned(),
    ));
    vcx.simulate_keystrokes("secondary-k");
    assert_eq!(
        text_of(&view, &mut vcx),
        "see [docs](https://example.com/a)\n"
    );
    // Text that is not a URL is not used.
    let (view, mut vcx) = editor_in(cx, "commonmark", "see docs\n", &[(4, 8)]);
    vcx.write_to_clipboard(ClipboardItem::new_string("not a url".to_owned()));
    vcx.simulate_keystrokes("secondary-k");
    assert_eq!(text_of(&view, &mut vcx), "see [docs]()\n");
    assert_eq!(selections(&view, &mut vcx), [(11, 11)]);
}

#[gpui::test]
fn commands_missing_from_the_profile_are_disabled(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "one two\n", &[(4, 7)]);
    for id in [
        "editor.strikethrough",
        "editor.highlight",
        "editor.toggle_task",
    ] {
        assert!(!handled(&view, &mut vcx, id), "{id}");
    }
    assert!(handled(&view, &mut vcx, "editor.bold"));
    // The key does nothing.
    vcx.simulate_keystrokes("secondary-shift-x secondary-l");
    assert_eq!(text_of(&view, &mut vcx), "one two\n");

    // GFM has strikethrough and task boxes.
    let (view, mut vcx) = editor_in(cx, "gfm-file", "- one two\n", &[(6, 9)]);
    assert!(handled(&view, &mut vcx, "editor.strikethrough"));
    assert!(!handled(&view, &mut vcx, "editor.highlight"));
    vcx.simulate_keystrokes("secondary-shift-x");
    assert_eq!(text_of(&view, &mut vcx), "- one ~~two~~\n");
    vcx.simulate_keystrokes("secondary-l");
    assert_eq!(text_of(&view, &mut vcx), "- [ ] one ~~two~~\n");
}

#[gpui::test]
fn formatting_is_disabled_where_it_cannot_apply(cx: &mut TestAppContext) {
    let text = "```\ncode\n```\n\n1. a\n";
    let (view, mut vcx) = editor_in(cx, "commonmark", text, &[(5, 5)]);
    assert!(!handled(&view, &mut vcx, "editor.bold"));
    assert!(!handled(&view, &mut vcx, "editor.renumber_list"));
    vcx.simulate_keystrokes("secondary-b");
    assert_eq!(text_of(&view, &mut vcx), text);
    // In the ordered list, renumbering applies.
    view.update(&mut vcx, |v, cx| {
        let doc = v.doc.as_mut().expect("loaded");
        doc.editor.set_cursor(18);
        cx.notify();
    });
    assert!(handled(&view, &mut vcx, "editor.renumber_list"));
    assert!(handled(&view, &mut vcx, "editor.bold"));
}

#[gpui::test]
fn one_undo_reverts_a_command(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "1. one\n2. two\n", &[(6, 6)]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "1. one\n2. \n3. two\n");
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "1. one\n2. two\n");
    assert_eq!(selections(&view, &mut vcx), [(6, 6)]);
    vcx.simulate_keystrokes("secondary-shift-z");
    assert_eq!(text_of(&view, &mut vcx), "1. one\n2. \n3. two\n");
}

#[gpui::test]
fn enter_at_several_carets(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "- a\n- b\n", &[(3, 3), (7, 7)]);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "- a\n- \n- b\n- \n");
    assert_eq!(selections(&view, &mut vcx), [(6, 6), (13, 13)]);
    vcx.simulate_input("x");
    assert_eq!(text_of(&view, &mut vcx), "- a\n- x\n- b\n- x\n");
    // One undo step for every caret.
    vcx.simulate_keystrokes("secondary-z secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "- a\n- b\n");
}

#[gpui::test]
fn no_command_runs_while_composing(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "- x\n", &[(3, 3)]);
    view.update_in(&mut vcx, |v, window, cx| {
        v.replace_and_mark_text_in_range(None, "かな", None, window, cx);
    });
    vcx.simulate_keystrokes("secondary-b");
    assert_eq!(text_of(&view, &mut vcx), "- xかな\n");
    // Enter commits the composition and does not continue the list.
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "- xかな\n");
    let composing = view.read_with(&vcx, |v, _| v.editor().unwrap().is_composing());
    assert!(!composing);
    vcx.simulate_keystrokes("enter");
    assert_eq!(text_of(&view, &mut vcx), "- xかな\n- \n");
}

// --- Timing ------------------------------------------------------------------------------

mod timing {
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use gpui::{PlatformTextSystem, TestDispatcher};

    use super::*;

    fn percentiles(samples: &[Duration]) -> String {
        let mut s = samples.to_vec();
        s.sort_unstable();
        let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize].as_secs_f64() * 1e3;
        format!(
            "p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms ({} samples)",
            at(0.5),
            at(0.95),
            at(0.99),
            at(1.0),
            s.len()
        )
    }

    fn platform_text_system() -> Arc<dyn PlatformTextSystem> {
        static TEXT: OnceLock<Arc<dyn PlatformTextSystem>> = OnceLock::new();
        TEXT.get_or_init(|| gpui::platform::current_platform(true).text_system())
            .clone()
    }

    /// About `bytes` of mixed prose, lists, quotes, links and code.
    fn generated(bytes: usize) -> String {
        let mut s = String::with_capacity(bytes + 256);
        let mut i = 0usize;
        while s.len() < bytes {
            match i % 6 {
                0 => s.push_str(&format!("## Section {i}\n\n")),
                1 => s.push_str(&format!(
                    "Some **strong** and *emphasised* prose with `code` and a [link](notes/n{i}.md), item {i}.\n\n"
                )),
                2 => s.push_str(&format!("- first item {i}\n- second item with [[wiki]]\n\n")),
                3 => s.push_str(&format!("> quoted text {i}\n> more quote\n\n")),
                4 => s.push_str(&format!("```rust\nfn f{i}(x: u32) -> u32 {{\n    x * 2\n}}\n```\n\n")),
                _ => s.push_str(&format!("Plain paragraph number {i} with several words of text in it.\n\n")),
            }
            i += 1;
        }
        s
    }

    /// Typing in the middle of a generated document: per keystroke, the
    /// input event to the frame drawn (layout and scene, no GPU). `wrap`:
    /// `None` leaves soft wrap as the view sets it (on, off in large-file
    /// mode), `Some(true)` forces it on whatever the size, `Some(false)`
    /// turns it off.
    fn run(name: &str, bytes: usize, wrap: Option<bool>) {
        let markdown = generated(bytes);
        // What opening costs the UI thread: the editor over a decoded
        // buffer (read, decode and parse run in the background).
        let buffer = gv_text::Buffer::new(&markdown);
        let t = Instant::now();
        let editor = gv_editor::Editor::new(buffer);
        let built = t.elapsed();
        drop(editor);
        let dir = temp(&[("doc.md", &markdown)]);
        let dispatcher = TestDispatcher::new(0);
        let mut cx = TestAppContext::build_with_text_system(
            dispatcher.clone(),
            Some("editor_timing"),
            platform_text_system(),
        );
        {
            let (window, workspace) = open_workspace(&mut cx, &dir.path().join("doc.md"));
            let mut vcx = VisualTestContext::from_window(window, &cx);
            draw(&mut vcx);
            let t = Instant::now();
            vcx.simulate_keystrokes("secondary-e");
            draw(&mut vcx);
            let opened = t.elapsed();
            let editor = workspace
                .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
                .expect("editor");
            if wrap == Some(false) {
                editor.update(&mut vcx, |e, cx| e.set_soft_wrap(false, cx));
                draw(&mut vcx);
            }
            if wrap == Some(true) {
                use super::super::wrap::{EditorMeasure, RowMap, run_rewrap};
                let ms = |d: Duration| d.as_secs_f64() * 1e3;
                // Soft wrap turned on: the frame that starts the job (UI
                // thread), the job (on this thread under the test
                // executor) with its landing, and the next frame.
                editor.update(&mut vcx, |e, cx| {
                    e.wrap.force = true;
                    e.set_soft_wrap(false, cx);
                });
                draw(&mut vcx);
                let t = Instant::now();
                editor.update(&mut vcx, |e, cx| e.set_soft_wrap(true, cx));
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                let start_frame = t.elapsed();
                let t = Instant::now();
                vcx.run_until_parked();
                let job_on_executor = t.elapsed();
                let t = Instant::now();
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                let landed_frame = t.elapsed();
                // A new width, phase by phase, the job on a real thread
                // with its own measure.
                let (defer, job, finish, rows) = editor.update(&mut vcx, |e, cx| {
                    let measure = EditorMeasure::new(cx.text_system().clone(), e.fonts.regular());
                    let width = e.doc.as_ref().unwrap().editor.wrap_width().unwrap() - 100.0;
                    let t = Instant::now();
                    let pending = e
                        .keep_top_line(|ed| ed.defer_wrap_width(Some(width)))
                        .flatten()
                        .expect("a job");
                    let defer = t.elapsed();
                    let t = Instant::now();
                    let done = std::thread::spawn(move || {
                        run_rewrap(pending, measure, &std::sync::atomic::AtomicBool::new(false))
                    })
                    .join()
                    .expect("the wrap thread")
                    .expect("not cancelled");
                    let job = t.elapsed();
                    let t = Instant::now();
                    assert_eq!(e.keep_top_line(|ed| ed.finish_rewrap(done)), Some(true));
                    let finish = t.elapsed();
                    let rows = e.row_map().map_or(0, RowMap::row_count);
                    (defer, job, finish, rows)
                });
                // Of the frame starting the first job: the unwrapped wrap
                // layer the editor builds.
                let mut fresh = gv_editor::Editor::new(gv_text::Buffer::new(&markdown));
                let t = Instant::now();
                let first_job = fresh.defer_wrap_width(Some(600.0));
                let unwrapped_layer = t.elapsed();
                assert!(first_job.is_some());
                drop((first_job, fresh));
                // The same at once, as a document under the synchronous
                // limit is re-wrapped.
                let t = Instant::now();
                editor.update(&mut vcx, |e, _| e.apply_wrap_width(Some(super::COLUMN)));
                let sync = t.elapsed();
                draw(&mut vcx);
                eprintln!(
                    "{name}: soft wrap turned on: frame starting the job {:.1} ms (of which the unwrapped layer {:.1} ms), job + landing on the test executor {:.1} ms, next frame {:.1} ms",
                    ms(start_frame),
                    ms(unwrapped_layer),
                    ms(job_on_executor),
                    ms(landed_frame),
                );
                eprintln!(
                    "{name}: new width ({rows} rows): UI thread {:.2} ms to start + {:.2} ms to land; job on a worker thread {:.1} ms; synchronous re-wrap {:.1} ms",
                    ms(defer),
                    ms(finish),
                    ms(job),
                    ms(sync),
                );
            }
            // The caret in the middle of the text, in a paragraph.
            editor.update(&mut vcx, |e, cx| {
                let doc = e.doc.as_mut().unwrap();
                let text = doc.editor.text().clone();
                let middle = text.len() / 2;
                let line = text.line_of_offset(middle).unwrap();
                let at = (line..text.line_count())
                    .find_map(|l| {
                        let r = text.line_range(l).ok()?;
                        let s = text.text_for_range(r.clone()).ok()?;
                        s.starts_with("Plain").then_some(r.end)
                    })
                    .unwrap();
                doc.editor.set_cursor(at);
                e.autoscroll = true;
                cx.notify();
            });
            draw(&mut vcx);
            let mut samples: Vec<Duration> = Vec::new();
            let keys = ["a", "b", "c", " ", "*", "x", "*", " "];
            for k in 0..400 {
                let t = Instant::now();
                vcx.simulate_input(keys[k % keys.len()]);
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                samples.push(t.elapsed());
            }
            eprintln!(
                "{name}: typing, input to frame drawn: {}",
                percentiles(&samples[20..])
            );
            let mut enters: Vec<Duration> = Vec::new();
            for _ in 0..100 {
                let t = Instant::now();
                vcx.simulate_keystrokes("enter");
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                enters.push(t.elapsed());
            }
            eprintln!(
                "{name}: Enter, input to frame drawn: {}",
                percentiles(&enters[10..])
            );
            // Opening a fence at the top reparses everything below it.
            editor.update(&mut vcx, |e, _| {
                e.doc.as_mut().unwrap().editor.set_cursor(0);
            });
            let t = Instant::now();
            vcx.simulate_input("```\n");
            vcx.update(|window, cx| window.draw(cx).clear(cx));
            let fence = t.elapsed();
            vcx.run_until_parked();
            let mut scrolls = Vec::new();
            for _ in 0..200 {
                editor.update(&mut vcx, |e, cx| {
                    e.scroll_y += 400.0;
                    cx.notify();
                });
                let t = Instant::now();
                vcx.update(|window, cx| window.draw(cx).clear(cx));
                scrolls.push(t.elapsed());
            }
            eprintln!(
                "{name}: Mod+E to the first editor frame {:.1} ms (of which the editor over the loaded text {:.1} ms on the UI thread); fence at the top {:.2} ms (UI thread, rest in the background); scroll 400 px/frame: {}",
                opened.as_secs_f64() * 1e3,
                built.as_secs_f64() * 1e3,
                fence.as_secs_f64() * 1e3,
                percentiles(&scrolls[10..])
            );
        }
        cx.run_until_parked();
        cx.update(|cx| cx.quit());
        cx.run_until_parked();
        drop(cx);
        dispatcher.drain_tasks();
    }

    #[test]
    #[ignore = "timing run; see the module docs"]
    fn timing_typing_in_one_and_ten_megabytes() {
        run("generated 1 MB, no wrap", 1 << 20, Some(false));
        run("generated 1 MB, soft wrap", 1 << 20, Some(true));
        run("generated 10 MB, large-file mode (no wrap)", 10 << 20, None);
        run("generated 10 MB, soft wrap forced", 10 << 20, Some(true));
    }
}

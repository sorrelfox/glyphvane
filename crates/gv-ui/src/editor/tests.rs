// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view in headless GPUI windows (GPUI's no-op text system: 0.6
//! em per character, so a 12.5 px character is 7.5 px wide): typing, input
//! method composition through the input handler, undo, several carets, the
//! mouse, the clipboard, the Mod+E round trip with the Reading view,
//! highlighting, the current line and the gutter. The timing runs with the
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
fn long_lines_scroll_sideways_to_the_caret(cx: &mut TestAppContext) {
    let long = "x".repeat(400);
    let (view, mut vcx) = editor(cx, &format!("{long}\n"));
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
    // The file is not written.
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
    /// input event to the frame drawn (layout and scene, no GPU).
    fn run(name: &str, bytes: usize) {
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
        run("generated 1 MB", 1 << 20);
        run("generated 10 MB", 10 << 20);
    }
}

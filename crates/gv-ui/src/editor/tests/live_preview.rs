// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live Preview in headless windows: markers hidden away from the caret
//! and revealed where a selection touches them, caret motion over hidden
//! markers, undo and redo, edits inside revealed constructs, the whole
//! document decorated in the background, and the modes that never use it.

use super::*;

/// A Source view of `text` in Live Preview, drawn.
fn live(cx: &mut TestAppContext, text: &str) -> (Entity<EditorView>, VisualTestContext) {
    let (view, mut vcx) = editor(cx, text);
    view.update(&mut vcx, |v, cx| v.set_live_preview(true, cx));
    draw(&mut vcx);
    (view, vcx)
}

/// The text the display map shows.
fn shown(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> String {
    draw(vcx);
    view.read_with(vcx, |v, _| v.editor().expect("loaded").display().text())
}

fn caret_at(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| v.set_cursor(offset, cx));
    draw(vcx);
}

#[gpui::test]
fn markers_hide_away_from_the_caret_and_show_where_it_is(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, "Some **bold** and `code`.\n");
    // The caret is at the start: nothing touched.
    assert_eq!(shown(&view, &mut vcx), "Some bold and code.\n");
    // Inside the strong: it shows its markers, the code span does not.
    caret_at(&view, &mut vcx, 8);
    assert_eq!(shown(&view, &mut vcx), "Some **bold** and code.\n");
    // Touching the code span's end.
    caret_at(&view, &mut vcx, 24);
    assert_eq!(shown(&view, &mut vcx), "Some bold and `code`.\n");
    // A selection reveals everything it touches.
    view.update(&mut vcx, |v, cx| {
        if let Some(doc) = v.doc.as_mut() {
            doc.editor.select_ranges(std::iter::once(0..20));
        }
        cx.notify();
    });
    assert_eq!(shown(&view, &mut vcx), "Some **bold** and `code`.\n");
    // Source again: everything shows.
    caret_at(&view, &mut vcx, 0);
    view.update(&mut vcx, |v, cx| v.set_live_preview(false, cx));
    assert_eq!(shown(&view, &mut vcx), "Some **bold** and `code`.\n");
    assert!(view.read_with(&vcx, |v, _| v.live_hidden().is_empty()));
}

#[gpui::test]
fn headings_links_and_wikilinks(cx: &mut TestAppContext) {
    let obsidian = ParseOptions::from_profile(&gv_profile::compile_builtin(
        gv_profile::ProfileId::Obsidian,
    ));
    let (view, mut vcx) = editor_with(
        cx,
        "x\n\n## Title\n\nSee [link](https://x.org) and [[Note|alias]] or [[Other]].\n",
        obsidian,
    );
    view.update(&mut vcx, |v, cx| v.set_live_preview(true, cx));
    assert_eq!(
        shown(&view, &mut vcx),
        "x\n\nTitle\n\nSee link and alias or Other.\n"
    );
    // The caret on the heading line shows its `#` run.
    caret_at(&view, &mut vcx, 6);
    assert_eq!(
        shown(&view, &mut vcx),
        "x\n\n## Title\n\nSee link and alias or Other.\n"
    );
}

#[gpui::test]
fn the_caret_moves_over_hidden_markers_as_one(cx: &mut TestAppContext) {
    // 0         1         2
    // 0123456789012345678901234567890
    // See [link](https://x.org) now
    let (view, mut vcx) = live(cx, "See [link](https://x.org) now\n");
    assert_eq!(shown(&view, &mut vcx), "See link now\n");
    caret_at(&view, &mut vcx, 29);
    vcx.simulate_keystrokes("ctrl-left");
    draw(&mut vcx);
    assert_eq!(selections(&view, &mut vcx), [(26, 26)]);
    // The next word start is inside the hidden `](https://x.org)`: the
    // caret stops before it, and the link shows its source.
    vcx.simulate_keystrokes("ctrl-left");
    assert_eq!(selections(&view, &mut vcx), [(9, 9)]);
    assert_eq!(shown(&view, &mut vcx), "See [link](https://x.org) now\n");

    // Right from just before the link: onto its `[`, which then shows.
    caret_at(&view, &mut vcx, 3);
    assert_eq!(shown(&view, &mut vcx), "See link now\n");
    vcx.simulate_keystrokes("right");
    assert_eq!(selections(&view, &mut vcx), [(4, 4)]);
    assert_eq!(shown(&view, &mut vcx), "See [link](https://x.org) now\n");

    // Left from after the link: to the end of its source.
    caret_at(&view, &mut vcx, 27);
    assert_eq!(shown(&view, &mut vcx), "See link now\n");
    vcx.simulate_keystrokes("left left");
    assert_eq!(selections(&view, &mut vcx), [(25, 25)]);

    // Shift-selection by words from the end keeps its tail and stops out
    // of the hidden destination.
    caret_at(&view, &mut vcx, 29);
    vcx.simulate_keystrokes("ctrl-shift-left ctrl-shift-left");
    assert_eq!(selections(&view, &mut vcx), [(29, 9)]);

    // Home and End.
    caret_at(&view, &mut vcx, 12);
    vcx.simulate_keystrokes("end");
    assert_eq!(selections(&view, &mut vcx), [(29, 29)]);
    vcx.simulate_keystrokes("home");
    assert_eq!(selections(&view, &mut vcx), [(0, 0)]);
    vcx.simulate_keystrokes("shift-end");
    assert_eq!(selections(&view, &mut vcx), [(0, 29)]);
}

#[gpui::test]
fn up_and_down_keep_the_column_of_what_is_shown(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, "## Title\nbody text\n");
    caret_at(&view, &mut vcx, 13);
    assert_eq!(shown(&view, &mut vcx), "Title\nbody text\n");
    vcx.simulate_keystrokes("up");
    // Column 4 of the row as shown is after "Titl".
    assert_eq!(selections(&view, &mut vcx), [(7, 7)]);
    assert_eq!(shown(&view, &mut vcx), "## Title\nbody text\n");
}

#[gpui::test]
fn edits_inside_revealed_constructs_and_undo(cx: &mut TestAppContext) {
    // a0 1 *2 *3 b4 *5 *6 7 c8
    let (view, mut vcx) = live(cx, "a **b** c\n");
    assert_eq!(shown(&view, &mut vcx), "a b c\n");
    caret_at(&view, &mut vcx, 4);
    vcx.simulate_keystrokes("x y");
    assert_eq!(text_of(&view, &mut vcx), "a **xyb** c\n");
    assert_eq!(shown(&view, &mut vcx), "a **xyb** c\n", "still revealed");
    vcx.simulate_keystrokes("end");
    assert_eq!(shown(&view, &mut vcx), "a xyb c\n");
    // Typing at a hidden marker's edge from outside the construct.
    vcx.simulate_keystrokes("home");
    assert_eq!(shown(&view, &mut vcx), "a xyb c\n");

    // Undo puts the text and the caret back: the caret touches the
    // strong again, so it shows.
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "a **b** c\n");
    assert_eq!(selections(&view, &mut vcx), [(4, 4)]);
    assert_eq!(shown(&view, &mut vcx), "a **b** c\n");
    vcx.simulate_keystrokes("end");
    assert_eq!(shown(&view, &mut vcx), "a b c\n");
    vcx.simulate_keystrokes("secondary-shift-z");
    assert_eq!(text_of(&view, &mut vcx), "a **xyb** c\n");
    assert_eq!(shown(&view, &mut vcx), "a **xyb** c\n");

    // Breaking the construct: the markers are text again.
    caret_at(&view, &mut vcx, 3);
    vcx.simulate_keystrokes("backspace");
    assert_eq!(text_of(&view, &mut vcx), "a *xyb** c\n");
    vcx.simulate_keystrokes("end");
    let shown_now = shown(&view, &mut vcx);
    assert!(shown_now.contains("xyb"), "{shown_now}");
    assert!(view.read_with(&vcx, |v, _| {
        let hidden = v.live_hidden();
        hidden.windows(2).all(|p| p[0].end <= p[1].start)
    }));
}

#[gpui::test]
fn the_whole_document_is_decorated_in_the_background(cx: &mut TestAppContext) {
    let mut text = String::new();
    for i in 0..600 {
        text.push_str(&format!("Line {i} with **bold** and *it*.\n\n"));
    }
    let (view, mut vcx) = live(cx, &text);
    let on_screen = view.read_with(&vcx, |v, _| v.live_hidden().len());
    assert!(on_screen > 0 && on_screen < 600 * 4, "{on_screen}");
    vcx.executor()
        .advance_clock(super::super::live_preview::FULL_DELAY * 2);
    draw(&mut vcx);
    draw(&mut vcx);
    let all = view.read_with(&vcx, |v, _| v.live_hidden().len());
    assert_eq!(all, 600 * 4);
    // An edit far below decorates again only that block.
    let end = text.len();
    view.update(&mut vcx, |v, cx| {
        if let Some(doc) = v.doc.as_mut() {
            doc.editor.set_cursor(end);
            doc.editor.insert_at_selections("Last **one**.\n");
        }
        v.input_changed(cx);
    });
    draw(&mut vcx);
    vcx.executor()
        .advance_clock(super::super::live_preview::FULL_DELAY * 2);
    draw(&mut vcx);
    draw(&mut vcx);
    let all = view.read_with(&vcx, |v, _| v.live_hidden().len());
    // The caret is after the new line: its strong hides too.
    assert_eq!(all, 600 * 4 + 2);
}

#[gpui::test]
fn plain_text_and_large_files_never_use_live_preview(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, "Some **bold** text\n");
    assert_eq!(shown(&view, &mut vcx), "Some bold text\n");
    view.update(&mut vcx, |v, cx| v.set_plain(true, cx));
    assert_eq!(shown(&view, &mut vcx), "Some **bold** text\n");
    assert!(view.read_with(&vcx, |v, _| v.live_preview() && !v.is_live_preview_active()));

    let (view, mut vcx) = live(cx, "Some **bold** text\n");
    assert_eq!(shown(&view, &mut vcx), "Some bold text\n");
    view.update(&mut vcx, |v, cx| {
        v.doc.as_mut().expect("loaded").large_file = true;
        v.live_settings_changed();
        cx.notify();
    });
    assert_eq!(shown(&view, &mut vcx), "Some **bold** text\n");
    assert!(!view.read_with(&vcx, |v, _| v.is_live_preview_active()));
}

/// Live Preview keeps its timing budget on a large note: decorating it
/// whole (cold and from the cache), and a refresh after a caret move.
#[test]
#[ignore = "timing; run with --ignored --nocapture"]
fn decorating_a_large_note() {
    let mut text = String::new();
    let mut i = 0;
    while text.len() < 1 << 20 {
        text.push_str(&format!(
            "## Heading {i}\n\nSome **bold**, *em*, `code`, ~~gone~~ and a [link](https://example.org/{i}) in a paragraph that runs on for a while.\n\n"
        ));
        i += 1;
    }
    let snapshot = gv_text::TextSnapshot::new(&text);
    let tree = gv_syntax::SyntaxTree::parse_with(&snapshot, ParseOptions::default());
    let report = super::super::live_preview::bench(&tree, &snapshot);
    eprintln!("{report}");
}

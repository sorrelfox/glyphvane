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

/// Draws until the whole-document splices, a slice per frame, are all
/// applied: the number of replacements then.
fn settle(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> usize {
    let mut last = usize::MAX;
    for _ in 0..200 {
        draw(vcx);
        let now = view.read_with(vcx, |v, _| v.live_hidden().len());
        if now == last && view.read_with(vcx, |v, _| v.live_settled()) {
            return now;
        }
        last = now;
    }
    last
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
    let all = settle(&view, &mut vcx);
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
    let all = settle(&view, &mut vcx);
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

fn obsidian() -> ParseOptions {
    ParseOptions::from_profile(&gv_profile::compile_builtin(
        gv_profile::ProfileId::Obsidian,
    ))
}

/// A Source view of `text` in Live Preview with the Obsidian syntax.
fn live_obsidian(cx: &mut TestAppContext, text: &str) -> (Entity<EditorView>, VisualTestContext) {
    let (view, mut vcx) = editor_with(cx, text, obsidian());
    view.update(&mut vcx, |v, cx| {
        v.set_profile("obsidian", cx);
        v.set_live_preview(true, cx);
    });
    draw(&mut vcx);
    (view, vcx)
}

/// The widgets drawn: their kinds.
fn widget_kinds(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<&'static str> {
    draw(vcx);
    probe_of(view, vcx)
        .widgets
        .iter()
        .map(|(_, k)| *k)
        .collect()
}

/// The display text drawn for line `line`.
fn drawn(view: &Entity<EditorView>, vcx: &mut VisualTestContext, line: usize) -> String {
    draw(vcx);
    probe_of(view, vcx)
        .row_texts
        .iter()
        .find(|(l, _)| *l == line)
        .map(|(_, t)| t.clone())
        .unwrap_or_default()
}

#[gpui::test]
fn drawn_rows_follow_hidden_markers(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, "Some **bold** text\n");
    assert_eq!(drawn(&view, &mut vcx, 0), "Some bold text");
    caret_at(&view, &mut vcx, 8);
    assert_eq!(drawn(&view, &mut vcx, 0), "Some **bold** text");
    caret_at(&view, &mut vcx, 0);
    assert_eq!(drawn(&view, &mut vcx, 0), "Some bold text");
}

#[gpui::test]
fn a_click_lands_on_the_character_under_the_pointer(cx: &mut TestAppContext) {
    // 0123456789012345678
    // Some **bold** text
    let (view, mut vcx) = live(cx, "Some **bold** text\n");
    caret_at(&view, &mut vcx, 18);
    assert_eq!(shown(&view, &mut vcx), "Some bold text\n");
    // The left half of the `b` shown: the caret goes before the `b`, past
    // the hidden `**`, and stays there once they show.
    let at = point_of(&view, &mut vcx, 5) + point(px(1.0), px(0.0));
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(7, 7)]);
    assert_eq!(shown(&view, &mut vcx), "Some **bold** text\n");
    // The right half of the `d`: after it, before the closing `**`.
    caret_at(&view, &mut vcx, 18);
    let at = point_of(&view, &mut vcx, 11) - point(px(1.0), px(0.0));
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(11, 11)]);
    // The right half of the space before it: before the opening `**`.
    caret_at(&view, &mut vcx, 18);
    let at = point_of(&view, &mut vcx, 5) - point(px(1.0), px(0.0));
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(5, 5)]);
}

#[gpui::test]
fn task_checkboxes_toggle_their_task(cx: &mut TestAppContext) {
    // 0123456789
    // - [ ] task
    let (view, mut vcx) = live_obsidian(cx, "x\n- [ ] task\n");
    let base = 2;
    caret_at(&view, &mut vcx, 0);
    assert_eq!(shown(&view, &mut vcx), "x\n- \u{fffc} task\n");
    assert_eq!(widget_kinds(&view, &mut vcx), ["task"]);
    // The box is as wide as the source it replaces.
    let probe = probe_of(&view, &mut vcx);
    let (bounds, _) = probe.widgets[0];
    assert!((f32::from(bounds.size.width) - 3.0 * CHAR).abs() < 0.01);
    // A click toggles the task, as one undoable edit; the caret stays.
    let center = bounds.center();
    press(&mut vcx, center, 1, Modifiers::none());
    assert_eq!(text_of(&view, &mut vcx), "x\n- [x] task\n");
    assert_eq!(selections(&view, &mut vcx), [(0, 0)]);
    assert_eq!(shown(&view, &mut vcx), "x\n- \u{fffc} task\n");
    press(&mut vcx, center, 1, Modifiers::none());
    assert_eq!(text_of(&view, &mut vcx), "x\n- [ ] task\n");
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "x\n- [x] task\n");
    // A caret next to it shows its source (ends touch); motions through
    // a shown box are tested in gv-editor (`box_widgets_take_their_width`).
    caret_at(&view, &mut vcx, base + 6);
    assert_eq!(shown(&view, &mut vcx), "x\n- \u{fffc} task\n");
    vcx.simulate_keystrokes("left");
    assert_eq!(selections(&view, &mut vcx), [(base + 5, base + 5)]);
    assert_eq!(shown(&view, &mut vcx), "x\n- [x] task\n");
}

#[gpui::test]
fn inline_math_is_drawn_and_shows_its_source_when_touched(cx: &mut TestAppContext) {
    // 012345678
    // a $x^2$ b
    let (view, mut vcx) = live_obsidian(cx, "a $x^2$ b\nnext\n");
    caret_at(&view, &mut vcx, 12);
    assert_eq!(shown(&view, &mut vcx), "a \u{fffc} b\nnext\n");
    assert_eq!(widget_kinds(&view, &mut vcx), ["math"]);
    caret_at(&view, &mut vcx, 7);
    assert_eq!(shown(&view, &mut vcx), "a $x^2$ b\nnext\n");
    assert!(widget_kinds(&view, &mut vcx).is_empty());
    // Right from before it: over it in one step.
    caret_at(&view, &mut vcx, 1);
    vcx.simulate_keystrokes("right");
    assert_eq!(selections(&view, &mut vcx), [(2, 2)]);
    // A formula that does not typeset shows its source, as text.
    let (view, mut vcx) = live_obsidian(cx, "a $\\frac{$ b\nnext\n");
    caret_at(&view, &mut vcx, 14);
    assert_eq!(shown(&view, &mut vcx), "a $\\frac{$ b\nnext\n");
    assert_eq!(widget_kinds(&view, &mut vcx), ["text"]);
}

#[gpui::test]
fn tags_are_chips_that_search(cx: &mut TestAppContext) {
    let (view, mut vcx) = live_obsidian(cx, "see #idea now\nnext\n");
    caret_at(&view, &mut vcx, 16);
    assert_eq!(shown(&view, &mut vcx), "see #idea now\nnext\n");
    assert_eq!(widget_kinds(&view, &mut vcx), ["chip"]);
    let (bounds, _) = probe_of(&view, &mut vcx).widgets[0];
    press(&mut vcx, bounds.center(), 1, Modifiers::none());
    let search = view.read_with(&vcx, |v, _| v.take_tag_search());
    assert_eq!(search.as_deref(), Some("tag:#idea"));
    assert_eq!(selections(&view, &mut vcx), [(16, 16)], "the caret stays");
    // The caret on it shows it as source.
    caret_at(&view, &mut vcx, 6);
    assert!(widget_kinds(&view, &mut vcx).is_empty());
}

#[gpui::test]
fn emoji_shortcodes_follow_the_profile(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_with(
        cx,
        "hi :smile: there\nnext\n",
        ParseOptions::builtin(gv_profile::ProfileId::GfmFile),
    );
    view.update(&mut vcx, |v, cx| {
        v.set_profile("gfm-file", cx);
        v.set_live_preview(true, cx);
    });
    caret_at(&view, &mut vcx, 20);
    assert_eq!(shown(&view, &mut vcx), "hi 😄 there\nnext\n");
    caret_at(&view, &mut vcx, 5);
    assert_eq!(shown(&view, &mut vcx), "hi :smile: there\nnext\n");
    // Obsidian has no shortcodes.
    let (view, mut vcx) = live_obsidian(cx, "hi :smile: there\nnext\n");
    caret_at(&view, &mut vcx, 20);
    assert_eq!(shown(&view, &mut vcx), "hi :smile: there\nnext\n");
}

#[gpui::test]
fn images_without_a_file_show_their_alt_text(cx: &mut TestAppContext) {
    let (view, mut vcx) = live(cx, "![A cat](cat.png) and ![](https://x.org/d.png)\nnext\n");
    caret_at(&view, &mut vcx, 50);
    assert_eq!(shown(&view, &mut vcx), "A cat and d.png\nnext\n");
    assert_eq!(widget_kinds(&view, &mut vcx), ["chip", "chip"]);
}

#[gpui::test]
fn local_images_show_as_thumbnails_one_line_high(cx: &mut TestAppContext) {
    let dir = temp(&[("note.md", "![pic](pic.png) and ![[pic.png|10]]\nnext\n")]);
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
    draw(&mut vcx);
    draw(&mut vcx);
    draw(&mut vcx);
    caret_at(&view, &mut vcx, 40);
    assert_eq!(shown(&view, &mut vcx), "\u{fffc} and \u{fffc}\nnext\n");
    let probe = probe_of(&view, &mut vcx);
    let widths: Vec<f32> = probe
        .widgets
        .iter()
        .map(|(b, _)| f32::from(b.size.width))
        .collect();
    assert_eq!(widget_kinds(&view, &mut vcx), ["image", "image"]);
    // Twice as wide as high, one line high; the size in the alias caps it.
    assert!((widths[0] - 2.0 * LINE).abs() < 0.01, "{widths:?}");
    assert!((widths[1] - 10.0).abs() < 0.01, "{widths:?}");
    // Touched, the image shows its source.
    caret_at(&view, &mut vcx, 0);
    assert_eq!(
        shown(&view, &mut vcx),
        "![pic](pic.png) and \u{fffc}\nnext\n"
    );
}

#[gpui::test]
fn widgets_take_their_width_in_soft_wrap(cx: &mut TestAppContext) {
    // A long paragraph of tasks' worth of math: rows wrap where the boxes
    // say, and every widget lands on a drawn row.
    let line = "word $x$ ".repeat(60);
    let (view, mut vcx) = live_obsidian(cx, &format!("{line}\nnext\n"));
    let end = line.len() + 3;
    caret_at(&view, &mut vcx, end);
    let probe = probe_of(&view, &mut vcx);
    let rows = rows_of(&probe, 0).len();
    assert!(rows > 1, "the line wraps");
    let math = probe.widgets.iter().filter(|(_, k)| *k == "math").count();
    assert_eq!(math, 60);
    // Every box stays inside the text column.
    let columns = probe.columns.expect("columns");
    for (bounds, _) in &probe.widgets {
        assert!(f32::from(bounds.right()) <= columns.text_x + columns.text_w + 0.5);
    }
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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Go popovers and the line commands in headless windows: Go to line
//! and Go to heading preview, jump on Enter and go back on Esc; the
//! callout picker inserts; the line commands, toggle comment and expand
//! selection run from their keys; viewer mode keeps the text.

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_syntax::ParseOptions;

use super::{GotoPopover, goto_of};
use crate::editor::EditorView;
use crate::keymap::actions::editor as commands;
use crate::{AppSettings, ThemeChoice};

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

/// A window holding only a Source view of `text` with `profile`, focused
/// and drawn.
fn editor(
    cx: &mut TestAppContext,
    text: &str,
    profile: &str,
) -> (Entity<EditorView>, VisualTestContext) {
    let (text, profile) = (text.to_owned(), profile.to_owned());
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    };
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        gpui::open_window(options, cx, |_, cx| {
            cx.new(|cx| {
                let mut v = EditorView::new(ParseOptions::default(), cx);
                v.set_profile(&profile, cx);
                v.set_text(&text, cx);
                v
            })
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    });
    draw(&mut vcx);
    (view, vcx)
}

fn popover(vcx: &mut VisualTestContext) -> Entity<GotoPopover> {
    vcx.update(|window, cx| goto_of(window, cx))
        .expect("the window root has a Go popover")
}

fn is_open(vcx: &mut VisualTestContext) -> bool {
    let open = popover(vcx).read_with(vcx, |p, _| p.is_open());
    assert_eq!(open, vcx.debug_bounds("goto-popover").is_some());
    open
}

fn listed(vcx: &mut VisualTestContext) -> Vec<String> {
    popover(vcx).read_with(vcx, |p, _| p.listed())
}

fn keys(vcx: &mut VisualTestContext, keys: &str) {
    vcx.simulate_keystrokes(keys);
    draw(vcx);
}

fn type_text(vcx: &mut VisualTestContext, text: &str) {
    vcx.simulate_input(text);
    draw(vcx);
}

fn text_of(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> String {
    view.read_with(vcx, |v, _| v.editor().expect("loaded").text().to_string())
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

fn editor_focused(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> bool {
    vcx.update(|window, cx| view.read(cx).focus_handle(cx).is_focused(window))
}

fn set_caret(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| v.set_cursor(offset, cx));
    draw(vcx);
}

const LINES: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7 is long\nl8\n";

#[gpui::test]
fn go_to_line_previews_jumps_and_goes_back(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, LINES, "gfm-file");
    set_caret(&view, &mut vcx, 1);
    keys(&mut vcx, "ctrl-g");
    assert!(is_open(&mut vcx));
    assert_eq!(
        popover(&mut vcx).read_with(&vcx, |p, _| p.title().map(str::to_owned)),
        Some("Go to line".to_owned())
    );
    assert!(vcx.debug_bounds("goto-line-note").is_some());
    // Typing previews: the caret follows.
    type_text(&mut vcx, "5");
    assert_eq!(selections(&view, &mut vcx), vec![(12, 12)]);
    // Esc goes back and gives the keyboard back.
    keys(&mut vcx, "escape");
    assert!(!is_open(&mut vcx));
    assert_eq!(selections(&view, &mut vcx), vec![(1, 1)]);
    assert!(editor_focused(&view, &mut vcx));
    // Line and column; Enter keeps it.
    keys(&mut vcx, "ctrl-g");
    type_text(&mut vcx, "7:4");
    keys(&mut vcx, "enter");
    assert!(!is_open(&mut vcx));
    assert_eq!(selections(&view, &mut vcx), vec![(21, 21)]);
    assert!(editor_focused(&view, &mut vcx));
    // Past the end: the last line. Not a number: Enter does nothing.
    keys(&mut vcx, "ctrl-g");
    type_text(&mut vcx, "x");
    keys(&mut vcx, "enter");
    assert!(is_open(&mut vcx));
    keys(&mut vcx, "escape");
    keys(&mut vcx, "ctrl-g");
    type_text(&mut vcx, "99");
    keys(&mut vcx, "enter");
    assert_eq!(
        selections(&view, &mut vcx),
        vec![(LINES.len(), LINES.len())]
    );
    // Nothing was edited.
    assert_eq!(text_of(&view, &mut vcx), LINES);
}

const HEADINGS: &str = "# Alpha\n\ntext\n\n## Beta\n\nmore\n\n## Gamma ray\n\nend\n";

#[gpui::test]
fn go_to_heading_filters_jumps_and_goes_back(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, HEADINGS, "gfm-file");
    set_caret(&view, &mut vcx, 10);
    keys(&mut vcx, "secondary-shift-o");
    assert!(is_open(&mut vcx));
    assert_eq!(listed(&mut vcx), vec!["Alpha", "Beta", "Gamma ray"]);
    // The heading the caret is in is highlighted; opening moves nothing.
    assert_eq!(
        popover(&mut vcx).read_with(&vcx, |p, _| p.selected()),
        Some("Alpha".to_owned())
    );
    assert_eq!(selections(&view, &mut vcx), vec![(10, 10)]);
    // Down previews the next heading.
    keys(&mut vcx, "down");
    let beta = HEADINGS.find("Beta").expect("beta");
    assert_eq!(selections(&view, &mut vcx), vec![(beta, beta)]);
    keys(&mut vcx, "escape");
    assert_eq!(selections(&view, &mut vcx), vec![(10, 10)]);
    assert!(editor_focused(&view, &mut vcx));
    // Typing filters; Enter jumps.
    keys(&mut vcx, "secondary-shift-o");
    type_text(&mut vcx, "gam");
    assert_eq!(listed(&mut vcx), vec!["Gamma ray"]);
    keys(&mut vcx, "enter");
    let gamma = HEADINGS.find("Gamma").expect("gamma");
    assert!(!is_open(&mut vcx));
    assert_eq!(selections(&view, &mut vcx), vec![(gamma, gamma)]);
    // No match: the list says so and Enter does nothing.
    keys(&mut vcx, "secondary-shift-o");
    type_text(&mut vcx, "zzz");
    assert!(vcx.debug_bounds("goto-empty").is_some());
    keys(&mut vcx, "enter");
    assert!(is_open(&mut vcx));
    keys(&mut vcx, "escape");
    assert_eq!(selections(&view, &mut vcx), vec![(gamma, gamma)]);
}

#[gpui::test]
fn the_callout_picker_inserts_the_type_picked(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a\n", "obsidian");
    vcx.dispatch_action(commands::InsertCallout);
    draw(&mut vcx);
    assert!(is_open(&mut vcx));
    assert_eq!(listed(&mut vcx).first().map(String::as_str), Some("note"));
    type_text(&mut vcx, "warn");
    assert_eq!(listed(&mut vcx), vec!["warning"]);
    keys(&mut vcx, "enter");
    assert!(!is_open(&mut vcx));
    assert_eq!(text_of(&view, &mut vcx), "> [!warning]\n> a\n");
    // One undo step.
    keys(&mut vcx, "secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "a\n");
    // Esc inserts nothing.
    vcx.dispatch_action(commands::InsertCallout);
    draw(&mut vcx);
    keys(&mut vcx, "escape");
    assert_eq!(text_of(&view, &mut vcx), "a\n");
}

#[gpui::test]
fn github_alerts_and_no_callouts_in_commonmark(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a\n", "gfm-file");
    vcx.dispatch_action(commands::InsertCallout);
    draw(&mut vcx);
    assert_eq!(
        listed(&mut vcx),
        vec!["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"]
    );
    keys(&mut vcx, "enter");
    assert_eq!(text_of(&view, &mut vcx), "> [!NOTE]\n> a\n");
    let available =
        vcx.update(|window, cx| window.is_action_available(&commands::InsertCallout, cx));
    assert!(available);
    let (_, mut vcx) = editor(cx, "a\n", "commonmark");
    let available =
        vcx.update(|window, cx| window.is_action_available(&commands::InsertCallout, cx));
    assert!(!available, "CommonMark has no callouts");
}

#[gpui::test]
fn line_commands_run_from_their_keys(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a\nb\nc\n", "gfm-file");
    set_caret(&view, &mut vcx, 0);
    keys(&mut vcx, "alt-down");
    assert_eq!(text_of(&view, &mut vcx), "b\na\nc\n");
    assert_eq!(selections(&view, &mut vcx), vec![(2, 2)]);
    keys(&mut vcx, "alt-up");
    assert_eq!(text_of(&view, &mut vcx), "a\nb\nc\n");
    keys(&mut vcx, "secondary-/");
    assert_eq!(text_of(&view, &mut vcx), "<!-- a -->\nb\nc\n");
    keys(&mut vcx, "secondary-/");
    assert_eq!(text_of(&view, &mut vcx), "a\nb\nc\n");
    // Duplicate, join, sort and change case have no keys: their actions.
    vcx.dispatch_action(commands::DuplicateLine);
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "a\na\nb\nc\n");
    vcx.dispatch_action(commands::JoinLines);
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "a\na b\nc\n");
    vcx.dispatch_action(commands::ChangeCase);
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "a\nA b\nc\n");
    // Each is one undo step.
    for expected in ["a\na b\nc\n", "a\na\nb\nc\n", "a\nb\nc\n"] {
        keys(&mut vcx, "secondary-z");
        assert_eq!(text_of(&view, &mut vcx), expected);
    }
    // Expand and shrink selection.
    set_caret(&view, &mut vcx, 0);
    keys(&mut vcx, "shift-alt-right");
    assert_eq!(selections(&view, &mut vcx), vec![(0, 1)]);
    keys(&mut vcx, "shift-alt-right");
    keys(&mut vcx, "shift-alt-left");
    assert_eq!(selections(&view, &mut vcx), vec![(0, 1)]);
}

#[gpui::test]
fn viewer_mode_keeps_the_text_but_selects(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "one two\nb\n", "gfm-file");
    view.update(&mut vcx, |v, cx| v.set_read_only(true, cx));
    set_caret(&view, &mut vcx, 1);
    keys(&mut vcx, "alt-down");
    keys(&mut vcx, "secondary-/");
    assert_eq!(text_of(&view, &mut vcx), "one two\nb\n");
    let available = vcx.update(|window, cx| window.is_action_available(&commands::SortLines, cx));
    assert!(!available);
    keys(&mut vcx, "shift-alt-right");
    assert_eq!(selections(&view, &mut vcx), vec![(0, 3)]);
    // Go to line still works.
    keys(&mut vcx, "ctrl-g");
    type_text(&mut vcx, "2");
    keys(&mut vcx, "enter");
    assert_eq!(selections(&view, &mut vcx), vec![(8, 8)]);
}

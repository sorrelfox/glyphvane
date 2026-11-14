// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Find and replace in the Source view, in headless windows: the popover's
//! keys and rows, matching over the text, next and previous, Replace,
//! Replace all as one undo step, capture groups, the toggles, select all
//! matches, Esc, CRLF text, the highlights and the background search.

use gpui::{Focusable as _, Modifiers};

use super::*;
use crate::find::{FindBar, FindStatus};
use gv_settings::keymap::Platform;

fn bar(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Entity<FindBar> {
    view.read_with(vcx, |v, _| v.find_bar().cloned())
        .expect("the find popover exists")
}

fn count(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Option<String> {
    let bar = bar(view, vcx);
    bar.read_with(vcx, |b, _| b.status()).text()
}

fn is_open(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> bool {
    view.read_with(vcx, |v, _| v.is_find_open())
}

fn editor_focused(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> bool {
    let view = view.clone();
    vcx.update(|window, cx| view.read(cx).focus_handle(cx).is_focused(window))
}

fn field_focused(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> bool {
    let bar = bar(view, vcx);
    vcx.update(|window, cx| bar.read(cx).field_focused(window, cx))
}

fn replace_focused(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> bool {
    let bar = bar(view, vcx);
    vcx.update(|window, cx| bar.read(cx).replace_focused(window, cx))
}

fn mac() -> bool {
    Platform::current() == Platform::MacOs
}

/// Find and replace's keys.
fn replace_keys() -> &'static str {
    if mac() { "cmd-alt-f" } else { "ctrl-h" }
}

fn next_keys() -> &'static str {
    if mac() { "cmd-g" } else { "f3" }
}

/// Opens find with Mod+F and types `query`.
fn find(vcx: &mut VisualTestContext, query: &str) {
    vcx.simulate_keystrokes("secondary-f");
    draw(vcx);
    if !query.is_empty() {
        vcx.simulate_input(query);
    }
    draw(vcx);
}

/// Opens find and replace, types `query`, then `replacement` in the
/// replace field.
fn find_replace(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
    query: &str,
    replacement: &str,
) {
    vcx.simulate_keystrokes(replace_keys());
    draw(vcx);
    assert!(
        field_focused(view, vcx),
        "an empty query takes the keyboard"
    );
    vcx.simulate_input(query);
    let bar = bar(view, vcx);
    vcx.update(|window, cx| bar.update(cx, |b, cx| b.focus_replace(window, cx)));
    vcx.simulate_input(replacement);
    draw(vcx);
}

fn click(vcx: &mut VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    vcx.simulate_click(bounds.center(), Modifiers::none());
    draw(vcx);
}

/// The highlights drawn: how many, and how many current.
fn highlights(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> (usize, usize) {
    let probe = probe_of(view, vcx);
    let current = probe.matches.iter().filter(|(_, c)| *c).count();
    (probe.matches.len(), current)
}

const TEXT: &str = "one two one\nthree one\n";

#[gpui::test]
fn mod_f_finds_in_the_text_and_selects_the_match(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find(&mut vcx, "one");
    assert!(is_open(&view, &mut vcx));
    assert!(vcx.debug_bounds("find-bar").is_some());
    // Find only: no replace row.
    assert!(vcx.debug_bounds("find-replace-row").is_none());
    assert!(field_focused(&view, &mut vcx));
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 3"));
    // The current match is selected in the editor, by byte offsets.
    assert_eq!(selections(&view, &mut vcx), [(0, 3)]);
    let matches = view.read_with(&vcx, |v, _| v.find_marks().matches.to_vec());
    assert_eq!(matches, [0..3, 8..11, 18..21]);
    assert_eq!(highlights(&view, &mut vcx), (3, 1));
    // Typing more keeps the place.
    vcx.simulate_input(" t");
    draw(&mut vcx);
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 1"));
    assert_eq!(selections(&view, &mut vcx), [(0, 5)]);
}

#[gpui::test]
fn mod_f_takes_a_one_line_selection(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", TEXT, &[(8, 11)]);
    vcx.simulate_keystrokes("secondary-f");
    draw(&mut vcx);
    let bar = bar(&view, &mut vcx);
    assert_eq!(bar.read_with(&vcx, |b, cx| b.query(cx)), "one");
    // The selected match is the current one.
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("2 of 3"));
    assert_eq!(selections(&view, &mut vcx), [(8, 11)]);
}

#[gpui::test]
fn next_and_previous_wrap_around(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find(&mut vcx, "one");
    vcx.simulate_keystrokes("enter");
    assert_eq!(selections(&view, &mut vcx), [(8, 11)]);
    vcx.simulate_keystrokes(next_keys());
    assert_eq!(selections(&view, &mut vcx), [(18, 21)]);
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("3 of 3"));
    vcx.simulate_keystrokes("enter");
    assert_eq!(selections(&view, &mut vcx), [(0, 3)], "wraps to the first");
    vcx.simulate_keystrokes("shift-enter");
    assert_eq!(selections(&view, &mut vcx), [(18, 21)], "wraps to the last");
    draw(&mut vcx);
    click(&mut vcx, "find-previous");
    assert_eq!(selections(&view, &mut vcx), [(8, 11)]);
    click(&mut vcx, "find-next");
    assert_eq!(selections(&view, &mut vcx), [(18, 21)]);
    // From the editor too, and from a caret moved by hand.
    vcx.simulate_keystrokes("escape");
    view.update(&mut vcx, |v, cx| v.set_cursor(4, cx));
    vcx.simulate_keystrokes(next_keys());
    draw(&mut vcx);
    assert!(is_open(&view, &mut vcx), "F3 opens find");
    assert_eq!(selections(&view, &mut vcx), [(8, 11)]);
    vcx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
    vcx.simulate_keystrokes(next_keys());
    assert_eq!(selections(&view, &mut vcx), [(18, 21)]);
}

#[gpui::test]
fn replace_replaces_the_current_match_then_selects_the_next(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find_replace(&view, &mut vcx, "one", "1");
    assert!(vcx.debug_bounds("find-replace-row").is_some());
    assert!(replace_focused(&view, &mut vcx));
    assert_eq!(selections(&view, &mut vcx), [(0, 3)]);
    // Enter in the replace field replaces.
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(text_of(&view, &mut vcx), "1 two one\nthree one\n");
    assert_eq!(selections(&view, &mut vcx), [(6, 9)]);
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 2"));
    click(&mut vcx, "find-replace");
    assert_eq!(text_of(&view, &mut vcx), "1 two 1\nthree one\n");
    assert_eq!(selections(&view, &mut vcx), [(14, 17)]);
    // Each Replace is its own undo step.
    vcx.simulate_keystrokes("escape");
    assert!(editor_focused(&view, &mut vcx));
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), "1 two one\nthree one\n");
}

#[gpui::test]
fn replace_all_is_one_undo_step(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find_replace(&view, &mut vcx, "one", "uno");
    click(&mut vcx, "find-replace-all");
    assert_eq!(text_of(&view, &mut vcx), "uno two uno\nthree uno\n");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("No results"));
    vcx.simulate_keystrokes("escape");
    assert!(!is_open(&view, &mut vcx));
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(text_of(&view, &mut vcx), TEXT);
    vcx.simulate_keystrokes("secondary-shift-z");
    assert_eq!(text_of(&view, &mut vcx), "uno two uno\nthree uno\n");
}

#[gpui::test]
fn replace_all_goes_past_the_count_cap(cx: &mut TestAppContext) {
    let n = crate::find::source::MAX_MATCHES + 5;
    let (view, mut vcx) = editor(cx, &"a\n".repeat(n));
    find_replace(&view, &mut vcx, "a", "b");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 10,000+"));
    click(&mut vcx, "find-replace-all");
    assert_eq!(text_of(&view, &mut vcx), "b\n".repeat(n));
}

#[gpui::test]
fn a_regex_replacement_expands_capture_groups(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "me@home and you@work\n");
    find_replace(&view, &mut vcx, r"(\w+)@(?P<place>\w+)", "${place}/$1");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("No results"));
    click(&mut vcx, "find-regex");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 2"));
    click(&mut vcx, "find-replace");
    assert_eq!(text_of(&view, &mut vcx), "home/me and you@work\n");
    click(&mut vcx, "find-replace-all");
    assert_eq!(text_of(&view, &mut vcx), "home/me and work/you\n");
    // Without the regex, `$1` is text.
    click(&mut vcx, "find-regex");
    let bar = bar(&view, &mut vcx);
    vcx.update(|window, cx| {
        bar.update(cx, |b, cx| {
            b.set_query("home", window, cx);
            b.set_replacement("$1", window, cx);
        });
    });
    view.update(&mut vcx, |v, cx| v.find_changed(cx));
    click(&mut vcx, "find-replace-all");
    assert_eq!(text_of(&view, &mut vcx), "$1/me and work/you\n");
}

#[gpui::test]
fn an_invalid_regex_finds_nothing(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find(&mut vcx, "(");
    // As a literal, `(` is not in the text.
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("No results"));
    click(&mut vcx, "find-regex");
    let bar = bar(&view, &mut vcx);
    assert_eq!(bar.read_with(&vcx, |b, _| b.status()), FindStatus::Invalid);
    assert_eq!(highlights(&view, &mut vcx), (0, 0));
}

#[gpui::test]
fn case_and_whole_word_toggles(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "Cat cat cats\n");
    find(&mut vcx, "cat");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 3"));
    click(&mut vcx, "find-match-case");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 2"));
    assert_eq!(selections(&view, &mut vcx), [(4, 7)]);
    click(&mut vcx, "find-whole-word");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 1"));
    // Case off again: "Cat" and "cat", the place kept.
    click(&mut vcx, "find-match-case");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("2 of 2"));
    let matches = view.read_with(&vcx, |v, _| v.find_marks().matches.to_vec());
    assert_eq!(matches, [0..3, 4..7]);
}

#[gpui::test]
fn alt_enter_selects_every_match(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find(&mut vcx, "one");
    vcx.simulate_keystrokes("enter");
    vcx.simulate_keystrokes("alt-enter");
    draw(&mut vcx);
    assert!(!is_open(&view, &mut vcx), "the popover closes");
    assert!(editor_focused(&view, &mut vcx));
    assert_eq!(selections(&view, &mut vcx), [(0, 3), (8, 11), (18, 21)]);
    let primary = view.read_with(&vcx, |v, _| {
        v.editor().expect("loaded").selections().primary().range()
    });
    assert_eq!(primary, 8..11, "the current match stays primary");
    // Typing replaces every match.
    vcx.simulate_input("1");
    assert_eq!(text_of(&view, &mut vcx), "1 two 1\nthree 1\n");
}

#[gpui::test]
fn escape_closes_and_gives_the_keyboard_back(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find(&mut vcx, "one");
    assert!(!editor_focused(&view, &mut vcx));
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(!is_open(&view, &mut vcx));
    assert!(vcx.debug_bounds("find-bar").is_none());
    assert!(editor_focused(&view, &mut vcx));
    assert_eq!(highlights(&view, &mut vcx), (0, 0));
    // The found match stays selected; the next Esc collapses it.
    assert_eq!(selections(&view, &mut vcx), [(0, 3)]);
    vcx.simulate_keystrokes("escape");
    assert_eq!(selections(&view, &mut vcx), [(3, 3)]);
    // From the editor, Esc closes the popover first.
    find(&mut vcx, "");
    vcx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
    vcx.simulate_keystrokes("escape");
    assert!(!is_open(&view, &mut vcx));
    // The close button too; the query stays for the next time.
    find(&mut vcx, "");
    click(&mut vcx, "find-close");
    assert!(!is_open(&view, &mut vcx));
    assert!(editor_focused(&view, &mut vcx));
    let bar = bar(&view, &mut vcx);
    assert_eq!(bar.read_with(&vcx, |b, cx| b.query(cx)), "one");
}

#[gpui::test]
fn editor_commands_do_not_act_from_the_fields(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "commonmark", "- item\n", &[(2, 6)]);
    find(&mut vcx, "");
    // Tab and Mod+B in the field leave the document alone.
    vcx.simulate_keystrokes("tab secondary-b");
    assert_eq!(text_of(&view, &mut vcx), "- item\n");
}

#[gpui::test]
fn crlf_text_keeps_its_line_ends(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a one\r\nb one\r\n");
    find_replace(&view, &mut vcx, "one", "two");
    click(&mut vcx, "find-replace-all");
    assert_eq!(text_of(&view, &mut vcx), "a two\r\nb two\r\n");
    // `$` matches before a CRLF and `.` never takes its CR.
    let bar = bar(&view, &mut vcx);
    click(&mut vcx, "find-regex");
    vcx.update(|window, cx| {
        bar.update(cx, |b, cx| {
            b.set_query("(.)$", window, cx);
            b.set_replacement("[$1]", window, cx);
        });
    });
    view.update(&mut vcx, |v, cx| v.find_changed(cx));
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 2"));
    click(&mut vcx, "find-replace-all");
    assert_eq!(text_of(&view, &mut vcx), "a tw[o]\r\nb tw[o]\r\n");
    let format = view.read_with(&vcx, |v, _| {
        v.editor().expect("loaded").text().line_ending_stats()
    });
    assert_eq!(format.crlf, 2);
}

#[gpui::test]
fn edits_while_find_is_open_recount(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, TEXT);
    find(&mut vcx, "one");
    vcx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window, cx));
    vcx.simulate_keystrokes("end");
    vcx.simulate_input(" one");
    draw(&mut vcx);
    // The selection stays; the current match is the next one after it.
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("4 of 4"));
    assert_eq!(selections(&view, &mut vcx), [(15, 15)]);
    assert_eq!(highlights(&view, &mut vcx), (4, 1));
    // Replace selects that match first, then replaces it.
    let bar = bar(&view, &mut vcx);
    vcx.update(|window, cx| {
        bar.update(cx, |b, cx| {
            b.set_replacing(true, cx);
            b.set_replacement("1", window, cx);
        });
    });
    draw(&mut vcx);
    click(&mut vcx, "find-replace");
    assert_eq!(text_of(&view, &mut vcx), "one two one one\nthree one\n");
    assert_eq!(selections(&view, &mut vcx), [(22, 25)]);
    click(&mut vcx, "find-replace");
    assert_eq!(text_of(&view, &mut vcx), "one two one one\nthree 1\n");
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 3"), "wraps");
}

#[gpui::test]
fn a_large_text_is_searched_in_the_background(cx: &mut TestAppContext) {
    let line = "lorem ipsum dolor sit amet\n";
    let lines = super::super::find::SYNC_BYTES / line.len() + 10;
    let text = format!("{}needle\n", line.repeat(lines));
    let (view, mut vcx) = editor(cx, &text);
    vcx.simulate_keystrokes("secondary-f");
    vcx.simulate_input("needle");
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(count(&view, &mut vcx).as_deref(), Some("1 of 1"));
    let start = text.len() - "needle\n".len();
    assert_eq!(selections(&view, &mut vcx), [(start, start + 6)]);
    // The match is scrolled into view and highlighted.
    assert_eq!(highlights(&view, &mut vcx), (1, 1));
}

#[gpui::test]
fn in_the_split_view_find_follows_the_focused_pane(cx: &mut TestAppContext) {
    let dir = temp(&[("note.md", "Some text, then more text.\n")]);
    let (window, workspace) = open_workspace(cx, &dir.path().join("note.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-alt-e");
    draw(&mut vcx);
    let split = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().split().cloned())
        .expect("the tab is split");
    let (editor, reader) = split.read_with(&vcx, |s, _| (s.editor().clone(), s.reader().clone()));
    assert!(editor_focused(&editor, &mut vcx));

    // The editor pane: find and replace.
    vcx.simulate_keystrokes(replace_keys());
    draw(&mut vcx);
    assert!(is_open(&editor, &mut vcx));
    assert!(vcx.debug_bounds("find-replace-row").is_some());
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);
    assert!(!is_open(&editor, &mut vcx));

    // The preview pane: find only, and find and replace does nothing.
    vcx.update(|window, cx| reader.read(cx).focus_handle(cx).focus(window, cx));
    vcx.simulate_keystrokes(replace_keys());
    draw(&mut vcx);
    assert!(!reader.read_with(&vcx, |r, _| r.is_find_open()));
    assert!(!is_open(&editor, &mut vcx));
    vcx.simulate_keystrokes("secondary-f");
    draw(&mut vcx);
    assert!(reader.read_with(&vcx, |r, _| r.is_find_open()));
    assert!(!is_open(&editor, &mut vcx));
    assert!(vcx.debug_bounds("find-bar").is_some());
    assert!(vcx.debug_bounds("find-replace-row").is_none());
}

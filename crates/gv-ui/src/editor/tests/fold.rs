// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Folding in the Source view, in headless windows: the Split gutter's
//! chevrons and their clicks, the placeholder chip, the fold commands,
//! carets that reveal folded text, rows and scrolling, mode switches.

use std::ops::Range;

use super::*;
use crate::editor::EditorLayout;
use crate::editor::fold::{FOLDABLE_DELAY, FoldHit};
use crate::keymap::actions::view as view_commands;

/// Headings, a section, a code block. Lines:
///
/// ```text
///  0 # One          4 ## Two        8 ```          12 # Three
///  1                5               9 x            13
///  2 Alpha text.    6 Beta text.   10 ```          14 Gamma.
///  3                7              11              15
/// ```
const DOC: &str =
    "# One\n\nAlpha text.\n\n## Two\n\nBeta text.\n\n```\nx\n```\n\n# Three\n\nGamma.\n";

/// A Source view of `text` laid out as the Split view's editor pane.
fn pane(cx: &mut TestAppContext, text: &str) -> (Entity<EditorView>, VisualTestContext) {
    let (view, mut vcx) = editor(cx, text);
    view.update(&mut vcx, |v, cx| v.set_layout(EditorLayout::Pane, cx));
    draw(&mut vcx);
    (view, vcx)
}

fn chevrons(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<(usize, bool)> {
    probe_of(view, vcx)
        .chevrons
        .iter()
        .map(|(line, folded, _)| (*line, *folded))
        .collect()
}

fn folds(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<Range<usize>> {
    view.read_with(vcx, |v, _| v.editor().expect("loaded").folded_ranges())
}

fn row_count(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> usize {
    view.read_with(vcx, |v, _| v.row_map().map_or(0, |m| m.row_count()))
}

/// The line numbers drawn.
fn numbers(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> Vec<usize> {
    probe_of(view, vcx)
        .numbers
        .iter()
        .map(|(l, _)| *l)
        .collect()
}

fn centre(b: Bounds<Pixels>) -> Point<Pixels> {
    point(b.left() + b.size.width / 2.0, b.top() + b.size.height / 2.0)
}

/// Clicks the chevron of `line`.
fn click_chevron(view: &Entity<EditorView>, vcx: &mut VisualTestContext, line: usize) {
    let rect = probe_of(view, vcx)
        .chevrons
        .iter()
        .find(|(l, _, _)| *l == line)
        .map(|(_, _, b)| *b)
        .unwrap_or_else(|| panic!("a chevron on line {line}"));
    press(vcx, centre(rect), 1, Modifiers::none());
}

/// The byte range of the region "## Two" folds: the end of its line to
/// the end of the code block.
fn two_section() -> Range<usize> {
    let start = DOC.find("## Two").unwrap() + "## Two".len();
    let end = DOC.find("```\n\n").unwrap() + "```".len();
    start..end
}

#[gpui::test]
fn chevrons_mark_the_lines_that_fold_in_the_split_pane(cx: &mut TestAppContext) {
    let (view, mut vcx) = pane(cx, DOC);
    assert_eq!(
        chevrons(&view, &mut vcx),
        [(0, false), (4, false), (8, false), (12, false)]
    );
    // 10 px, `text.tertiary`, 4 after the line numbers, centred in the row.
    let probe = probe_of(&view, &mut vcx);
    let columns = probe.columns.expect("laid out");
    let (_, _, rect) = probe.chevrons[1];
    assert_eq!(rect.size, size(px(10.0), px(10.0)));
    assert_eq!(
        f32::from(rect.left()),
        columns.numbers_right + crate::metrics::size::split::CHEVRON_GAP
    );
    assert!(f32::from(rect.right()) <= columns.text_x);

    // Edit mode's gutter has none, as designed.
    view.update(&mut vcx, |v, cx| v.set_layout(EditorLayout::Column, cx));
    draw(&mut vcx);
    assert!(chevrons(&view, &mut vcx).is_empty());
}

#[gpui::test]
fn a_chevron_click_folds_and_unfolds(cx: &mut TestAppContext) {
    let (view, mut vcx) = pane(cx, DOC);
    let rows = row_count(&view, &mut vcx);
    let caret = selections(&view, &mut vcx);

    click_chevron(&view, &mut vcx, 4);
    assert_eq!(folds(&view, &mut vcx), [two_section()]);
    assert_eq!(row_count(&view, &mut vcx), rows - 6, "lines 5 to 10 hidden");
    assert_eq!(
        numbers(&view, &mut vcx),
        [0, 1, 2, 3, 4, 11, 12, 13, 14, 15]
    );
    assert!(chevrons(&view, &mut vcx).contains(&(4, true)));
    assert!(
        !chevrons(&view, &mut vcx).iter().any(|(l, _)| *l == 8),
        "the hidden code block has no chevron"
    );
    assert_eq!(selections(&view, &mut vcx), caret, "the caret stays put");
    // Its placeholder is a chip at the end of the heading.
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(probe.chips.len(), 1);
    let (chip, hidden) = probe.chips[0].clone();
    assert_eq!(hidden, two_section());
    let heading_end = point_of(&view, &mut vcx, two_section().start);
    assert!(
        chip.left() > heading_end.x,
        "{chip:?} after {heading_end:?}"
    );
    assert_eq!(f32::from(chip.size.height), editor_size::FOLD_CHIP_HEIGHT);
    assert_eq!(probe.chip_tokens, [Token::SurfaceSunken]);

    click_chevron(&view, &mut vcx, 4);
    assert!(folds(&view, &mut vcx).is_empty());
    assert_eq!(row_count(&view, &mut vcx), rows);
    assert!(probe_of(&view, &mut vcx).chips.is_empty());
}

#[gpui::test]
fn a_chip_click_unfolds(cx: &mut TestAppContext) {
    let (view, mut vcx) = pane(cx, DOC);
    click_chevron(&view, &mut vcx, 12);
    let (chip, _) = probe_of(&view, &mut vcx).chips[0].clone();
    press(&mut vcx, centre(chip), 1, Modifiers::none());
    assert!(folds(&view, &mut vcx).is_empty());

    // In Edit mode too.
    view.update(&mut vcx, |v, cx| v.set_layout(EditorLayout::Column, cx));
    vcx.dispatch_action(view_commands::FoldAll);
    draw(&mut vcx);
    let chips = probe_of(&view, &mut vcx).chips;
    assert_eq!(chips.len(), 2);
    press(&mut vcx, centre(chips[1].0), 1, Modifiers::none());
    let left = folds(&view, &mut vcx);
    assert_eq!(left.len(), 3, "the folds of the first chip stay: {left:?}");
    assert!(left.iter().all(|r| r.start < DOC.find("# Three").unwrap()));
}

/// Fold to heading levels 4 to 6: the headings above the level stay open.
#[gpui::test]
fn fold_deep_levels(cx: &mut TestAppContext) {
    // Lines: 0 `####`, 4 `#####`, 8 `######`.
    const DEEP: &str = "#### Four\n\nA.\n\n##### Five\n\nB.\n\n###### Six\n\nC.\n";
    let (view, mut vcx) = pane(cx, DEEP);
    vcx.dispatch_action(view_commands::FoldLevel6);
    draw(&mut vcx);
    assert_eq!(
        chevrons(&view, &mut vcx),
        [(0, false), (4, false), (8, true)]
    );
    vcx.dispatch_action(view_commands::FoldLevel5);
    draw(&mut vcx);
    assert_eq!(chevrons(&view, &mut vcx), [(0, false), (4, true)]);
    vcx.dispatch_action(view_commands::FoldLevel4);
    draw(&mut vcx);
    assert_eq!(chevrons(&view, &mut vcx), [(0, true)]);
    vcx.dispatch_action(view_commands::FoldLevel6);
    draw(&mut vcx);
    assert_eq!(
        chevrons(&view, &mut vcx),
        [(0, false), (4, false), (8, true)],
        "a deeper level unfolds the higher sections"
    );
}

#[gpui::test]
fn fold_commands(cx: &mut TestAppContext) {
    let (view, mut vcx) = pane(cx, DOC);
    vcx.dispatch_action(view_commands::FoldAll);
    draw(&mut vcx);
    assert_eq!(chevrons(&view, &mut vcx), [(0, true), (12, true)]);
    assert_eq!(numbers(&view, &mut vcx), [0, 11, 12, 15]);

    vcx.dispatch_action(view_commands::UnfoldAll);
    draw(&mut vcx);
    assert!(folds(&view, &mut vcx).is_empty());
    assert_eq!(numbers(&view, &mut vcx), (0..16).collect::<Vec<_>>());

    // Level 2: the headings of levels 1 and 2 stay visible.
    vcx.dispatch_action(view_commands::FoldLevel2);
    draw(&mut vcx);
    assert_eq!(folds(&view, &mut vcx), [two_section()]);
    vcx.dispatch_action(view_commands::FoldLevel1);
    draw(&mut vcx);
    assert_eq!(
        chevrons(&view, &mut vcx),
        [(0, true), (12, true)],
        "every heading folds"
    );
    vcx.dispatch_action(view_commands::FoldLevel3);
    draw(&mut vcx);
    assert!(folds(&view, &mut vcx).is_empty(), "no heading of level 3");

    // Fold and Unfold work at the caret: in the code block, the block.
    view.update(&mut vcx, |v, cx| v.set_cursor(DOC.find("x\n").unwrap(), cx));
    vcx.dispatch_action(view_commands::Fold);
    draw(&mut vcx);
    assert_eq!(chevrons(&view, &mut vcx)[2], (8, true));
    // On the code fill and the current line, a darker chip.
    assert_eq!(probe_of(&view, &mut vcx).chip_tokens, [Token::StatePress]);
    let fence_end = DOC.find("```\nx").unwrap() + "```".len();
    assert_eq!(
        selections(&view, &mut vcx),
        [(fence_end, fence_end)],
        "the caret leaves the fold"
    );
    vcx.dispatch_action(view_commands::Unfold);
    draw(&mut vcx);
    assert!(folds(&view, &mut vcx).is_empty());
}

#[gpui::test]
fn a_caret_entering_a_fold_unfolds_it(cx: &mut TestAppContext) {
    let (view, mut vcx) = pane(cx, DOC);
    click_chevron(&view, &mut vcx, 4);
    assert_eq!(folds(&view, &mut vcx).len(), 1);
    // Find selects the match inside the fold.
    vcx.simulate_keystrokes("secondary-f");
    draw(&mut vcx);
    vcx.simulate_input("Beta");
    draw(&mut vcx);
    assert!(folds(&view, &mut vcx).is_empty());
    let beta = DOC.find("Beta").unwrap();
    assert_eq!(selections(&view, &mut vcx), [(beta, beta + 4)]);
    vcx.simulate_keystrokes("escape");
    draw(&mut vcx);

    // A click in the text never lands in a fold; one on the row after the
    // placeholder places the caret after it.
    click_chevron(&view, &mut vcx, 4);
    let after = two_section().end;
    let at = point_of(&view, &mut vcx, after);
    press(&mut vcx, point(at.x + px(40.0), at.y), 1, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(after, after)]);
    assert_eq!(folds(&view, &mut vcx).len(), 1, "still folded");
    // Right from there goes past the fold's end, Left jumps over it.
    vcx.simulate_keystrokes("left");
    draw(&mut vcx);
    assert_eq!(
        selections(&view, &mut vcx),
        [(two_section().start, two_section().start)]
    );
    assert_eq!(folds(&view, &mut vcx).len(), 1);
}

#[gpui::test]
fn folds_change_the_rows_and_the_scroll_range(cx: &mut TestAppContext) {
    // "# Intro" on line 0, "# Long" on 42, "# End" on 444.
    let mut doc = String::from("# Intro\n\n");
    for i in 0..20 {
        doc.push_str(&format!("Paragraph {i}.\n\n"));
    }
    doc.push_str("# Long\n\n");
    for i in 0..200 {
        doc.push_str(&format!("Paragraph {i}.\n\n"));
    }
    doc.push_str("# End\n\nLast.\n");
    let (view, mut vcx) = pane(cx, &doc);
    let rows = row_count(&view, &mut vcx);
    vcx.simulate_keystrokes("ctrl-end");
    draw(&mut vcx);
    let scrolled = view.read_with(&vcx, |v, _| v.scroll_y);
    assert!(scrolled > 1000.0, "{scrolled}");

    vcx.dispatch_action(view_commands::FoldAll);
    draw(&mut vcx);
    // The headings, the blank lines after their sections, the last line.
    assert_eq!(row_count(&view, &mut vcx), 6, "{rows} rows before");
    assert_eq!(view.read_with(&vcx, |v, _| v.scroll_y), 0.0, "all fits");
    assert_eq!(numbers(&view, &mut vcx), [0, 41, 42, 443, 444, 447]);

    vcx.dispatch_action(view_commands::UnfoldAll);
    draw(&mut vcx);
    assert_eq!(row_count(&view, &mut vcx), rows);

    // Folding above the top keeps the top line in place.
    view.update(&mut vcx, |v, cx| v.scroll_to_line(300, cx));
    draw(&mut vcx);
    let (top, scroll_y) = view.read_with(&vcx, |v, _| (v.top_line(), v.scroll_y));
    assert_eq!(top, 300);
    view.update_in(&mut vcx, |v, window, cx| {
        let hit = FoldHit::Chevron {
            line: 0,
            folded: Vec::new(),
        };
        v.press_fold(hit, window, cx);
    });
    draw(&mut vcx);
    let (top, scrolled) = view.read_with(&vcx, |v, _| (v.top_line(), v.scroll_y));
    assert_eq!(top, 300, "still at the top");
    assert!(scrolled < scroll_y, "{scrolled} < {scroll_y}");
}

#[gpui::test]
fn folds_wrap_with_their_rows(cx: &mut TestAppContext) {
    // A heading long enough to wrap: the placeholder ends its last row.
    let heading = format!("# {}", words(40));
    let doc = format!("{heading}\n\nBody.\n\n# Next\n");
    let (view, mut vcx) = pane(cx, &doc);
    let first_rows = rows_of(&probe_of(&view, &mut vcx), 0).len();
    assert!(first_rows > 1, "the heading wraps");
    click_chevron(&view, &mut vcx, 0);
    let probe = probe_of(&view, &mut vcx);
    assert!(rows_of(&probe, 0).len() >= first_rows);
    assert_eq!(numbers(&view, &mut vcx), [0, 3, 4, 5]);
    let (chip, _) = probe.chips[0].clone();
    let last_row = rows_of(&probe, 0).last().unwrap().0;
    let columns = probe.columns.unwrap();
    let row_top = columns.top + last_row as f32 * LINE;
    assert!(
        f32::from(chip.top()) > row_top && f32::from(chip.bottom()) < row_top + LINE,
        "{chip:?} on row {last_row}"
    );
}

#[gpui::test]
fn foldable_regions_follow_edits(cx: &mut TestAppContext) {
    let (view, mut vcx) = pane(cx, "Text.\n");
    assert!(chevrons(&view, &mut vcx).is_empty());
    vcx.simulate_keystrokes("ctrl-end");
    vcx.simulate_input("\n# New\n\nBody.\n");
    draw(&mut vcx);
    vcx.executor().advance_clock(FOLDABLE_DELAY);
    draw(&mut vcx);
    assert_eq!(chevrons(&view, &mut vcx), [(2, false)]);
}

#[gpui::test]
fn folds_last_through_mode_switches(cx: &mut TestAppContext) {
    let dir = temp(&[("note.md", DOC)]);
    let (window, workspace) = open_workspace(cx, &dir.path().join("note.md"));
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-alt-e");
    draw(&mut vcx);
    draw(&mut vcx);
    let mode = workspace.read_with(&vcx, |ws, _| ws.active_tab().mode());
    assert_eq!(mode, ViewMode::Split);
    let editor = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    assert_eq!(
        chevrons(&editor, &mut vcx),
        [(0, false), (4, false), (8, false), (12, false)]
    );
    click_chevron(&editor, &mut vcx, 4);
    assert_eq!(folds(&editor, &mut vcx), [two_section()]);

    // Edit, Preview, then Split again.
    vcx.simulate_keystrokes("secondary-alt-e");
    draw(&mut vcx);
    let mode = workspace.read_with(&vcx, |ws, _| ws.active_tab().mode());
    assert_eq!(mode, ViewMode::Edit);
    assert_eq!(folds(&editor, &mut vcx), [two_section()]);
    assert_eq!(probe_of(&editor, &mut vcx).chips.len(), 1);
    vcx.simulate_keystrokes("secondary-e");
    draw(&mut vcx);
    vcx.simulate_keystrokes("secondary-alt-e");
    draw(&mut vcx);
    draw(&mut vcx);
    let same = workspace
        .read_with(&vcx, |ws, _| ws.active_tab().editor().cloned())
        .expect("the tab has an editor");
    assert_eq!(same, editor);
    assert_eq!(folds(&editor, &mut vcx), [two_section()]);
    assert!(chevrons(&editor, &mut vcx).contains(&(4, true)));
}

#[gpui::test]
fn selecting_a_folded_line_selects_what_it_hides(cx: &mut TestAppContext) {
    let (view, mut vcx) = pane(cx, DOC);
    click_chevron(&view, &mut vcx, 4);
    // A click on the line number selects the line, the folded text and
    // the terminator of its last line; the fold stays.
    let probe = probe_of(&view, &mut vcx);
    let columns = probe.columns.expect("laid out");
    let (_, _, chevron) = probe.chevrons[1];
    let at = point(px(columns.gutter_left() + 4.0), centre(chevron).y);
    press(&mut vcx, at, 1, Modifiers::none());
    let start = DOC.find("## Two").unwrap();
    let end = two_section().end + 1;
    assert_eq!(selections(&view, &mut vcx), [(start, end)]);
    assert_eq!(folds(&view, &mut vcx), [two_section()]);
}

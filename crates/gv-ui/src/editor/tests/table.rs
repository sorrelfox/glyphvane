// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Tables and footnotes in the Source view, in headless windows: Tab and
//! Shift+Tab move between cells and format the table, Tab in the last cell
//! adds a row, the table commands run from their actions, and insert
//! footnote puts the caret in the new definition.

use super::*;

const TABLE: &str = "| a | b |\n|---|---|\n| c | d |\n";

#[gpui::test]
fn tab_moves_between_table_cells(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "gfm-file", TABLE, &[(2, 2)]);
    vcx.simulate_keystrokes("tab");
    let formatted = "| a   | b   |\n| --- | --- |\n| c   | d   |\n";
    assert_eq!(text_of(&view, &mut vcx), formatted);
    // The next cell's text is selected.
    assert_eq!(selections(&view, &mut vcx), [(8, 9)]);
    vcx.simulate_keystrokes("tab");
    assert_eq!(selections(&view, &mut vcx), [(30, 31)]);
    vcx.simulate_keystrokes("shift-tab");
    assert_eq!(selections(&view, &mut vcx), [(8, 9)]);
    vcx.simulate_keystrokes("tab tab");
    assert_eq!(selections(&view, &mut vcx), [(36, 37)]);
    // Tab in the last cell adds a row; typing fills its first cell.
    vcx.simulate_keystrokes("tab");
    assert_eq!(
        text_of(&view, &mut vcx),
        format!("{formatted}|     |     |\n")
    );
    vcx.simulate_input("e");
    vcx.simulate_keystrokes("tab");
    assert_eq!(
        text_of(&view, &mut vcx),
        format!("{formatted}| e   |     |\n")
    );
    // One undo step per command.
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(
        text_of(&view, &mut vcx),
        format!("{formatted}| e    |     |\n")
    );
}

#[gpui::test]
fn tab_outside_tables_and_in_commonmark(cx: &mut TestAppContext) {
    // CommonMark has no tables: Tab inserts a tab.
    let (view, mut vcx) = editor_in(cx, "commonmark", TABLE, &[(2, 2)]);
    vcx.simulate_keystrokes("tab");
    assert_eq!(
        text_of(&view, &mut vcx),
        "| \ta | b |\n|---|---|\n| c | d |\n"
    );
}

#[gpui::test]
fn table_commands_run_from_their_actions(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "gfm-file", TABLE, &[(22, 22)]);
    vcx.dispatch_action(commands::TableInsertColumnRight);
    assert_eq!(
        text_of(&view, &mut vcx),
        "| a   |     | b   |\n| --- | --- | --- |\n| c   |     | d   |\n"
    );
    vcx.dispatch_action(commands::TableAlignRight);
    assert_eq!(
        text_of(&view, &mut vcx),
        "| a   |     | b   |\n| --- | --: | --- |\n| c   |     | d   |\n"
    );
    // Outside a table the row commands have no handler; Insert or format
    // table inserts a table.
    let (view, mut vcx) = editor_in(cx, "gfm-file", "text\n", &[(4, 4)]);
    vcx.dispatch_action(commands::TableDeleteRow);
    assert_eq!(text_of(&view, &mut vcx), "text\n");
    vcx.dispatch_action(commands::Table);
    assert_eq!(
        text_of(&view, &mut vcx),
        "text\n\n|     |     |\n| --- | --- |\n|     |     |\n"
    );
    assert_eq!(selections(&view, &mut vcx), [(8, 8)]);
}

#[gpui::test]
fn insert_footnote_goes_to_the_definition(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor_in(cx, "gfm-file", "Text here.\n", &[(4, 4)]);
    vcx.dispatch_action(commands::InsertFootnote);
    assert_eq!(text_of(&view, &mut vcx), "Text[^1] here.\n\n[^1]: \n");
    assert_eq!(selections(&view, &mut vcx), [(22, 22)]);
    vcx.simulate_input("note");
    // Go to footnote goes back to the reference from the definition's text.
    vcx.dispatch_action(crate::keymap::actions::navigation::GoToFootnote);
    assert_eq!(selections(&view, &mut vcx), [(6, 6)]);
    // CommonMark has no footnotes.
    let (view, mut vcx) = editor_in(cx, "commonmark", "Text\n", &[(4, 4)]);
    vcx.dispatch_action(commands::InsertFootnote);
    assert_eq!(text_of(&view, &mut vcx), "Text\n");
}

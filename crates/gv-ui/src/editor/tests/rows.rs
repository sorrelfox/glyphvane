// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Rows made from the display map's chunks: expanded tabs and fold
//! placeholders in the row text, highlighting carried by the chunks, and
//! carets, clicks, selections and input-method bounds placed through
//! display-map columns.

use super::*;
use crate::editor::highlight::Style;

/// Folds `range` in the view's editor and draws.
fn fold(view: &Entity<EditorView>, vcx: &mut VisualTestContext, range: std::ops::Range<usize>) {
    view.update(vcx, |v, cx| {
        assert!(v.doc.as_mut().unwrap().editor.fold_ranges([range]));
        cx.notify();
    });
    draw(vcx);
}

fn set_cursor(view: &Entity<EditorView>, vcx: &mut VisualTestContext, offset: usize) {
    view.update(vcx, |v, cx| {
        v.doc.as_mut().unwrap().editor.set_cursor(offset);
        cx.notify();
    });
    draw(vcx);
}

/// The caret's left edge, from the left of the text column.
fn caret_x(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> f32 {
    let probe = probe_of(view, vcx);
    let columns = probe.columns.unwrap();
    f32::from(probe.carets[0].left()) - columns.text_x
}

/// The style drawn at column `column` of `line`'s row.
fn style_at(
    view: &Entity<EditorView>,
    vcx: &mut VisualTestContext,
    line: usize,
    column: usize,
) -> Style {
    let row = view.update_in(vcx, |v, window, cx| v.row(line, window, cx).unwrap());
    row.runs
        .iter()
        .find(|(r, _)| r.contains(&column))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| panic!("a run at {column}: {:?}", row.runs))
}

#[gpui::test]
fn rows_hold_the_tab_layer_text(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a\tb\n# A\nbody\n# B\n");
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(0, window, cx).unwrap());
    assert_eq!(row.text.content, "a   b");
    assert_eq!(row.text.display, "a   b");
    // A fold: the placeholder's chunk in the text, with room for its chip.
    fold(&view, &mut vcx, 7..12);
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(1, window, cx).unwrap());
    assert_eq!(row.text.content, "# A⋯");
    assert_eq!(row.text.display, "# A ⋯ ");
    assert_eq!(row.text.placeholders, vec![3..6]);
    // The next line keeps its own row.
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(3, window, cx).unwrap());
    assert_eq!(row.text.content, "# B");
}

#[gpui::test]
fn highlighting_comes_with_the_chunks(cx: &mut TestAppContext) {
    let text = "Some **strong**\tx\n# Head\nmore **b**\n";
    let (view, mut vcx) = editor(cx, text);
    // Columns: the tab after `**strong**` is one column wide.
    let strong = style_at(&view, &mut vcx, 0, "Some **".len());
    assert!(strong.bold, "{strong:?}");
    assert_eq!(
        style_at(&view, &mut vcx, 0, "Some ".len()).color,
        Token::TextTertiary
    );
    assert_eq!(
        style_at(&view, &mut vcx, 0, "Some **strong** ".len()).color,
        Token::TextPrimary
    );
    // A fold joins the heading to the next line: each piece keeps its
    // line's highlighting, the placeholder is `text.secondary`.
    let head = text.find("# Head").unwrap();
    let from = head + "# Head".len();
    let to = text.find("more ").unwrap() + "more ".len();
    fold(&view, &mut vcx, from..to);
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(1, window, cx).unwrap());
    assert_eq!(row.text.content, "# Head⋯**b**");
    assert_eq!(style_at(&view, &mut vcx, 1, 0).color, Token::TextTertiary);
    assert!(style_at(&view, &mut vcx, 1, "# ".len()).bold);
    assert_eq!(
        style_at(&view, &mut vcx, 1, "# Head".len()).color,
        Token::TextSecondary
    );
    assert_eq!(
        style_at(&view, &mut vcx, 1, "# Head⋯".len()).color,
        Token::TextTertiary
    );
    assert!(style_at(&view, &mut vcx, 1, "# Head⋯**".len()).bold);
}

#[gpui::test]
fn carets_and_clicks_go_through_tabs_and_placeholders(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a\tbc\nxy\nzw\n");
    // After the tab: four columns in.
    set_cursor(&view, &mut vcx, 2);
    assert_eq!(caret_x(&view, &mut vcx), 4.0 * CHAR);
    set_cursor(&view, &mut vcx, 1);
    assert_eq!(caret_x(&view, &mut vcx), CHAR);
    let columns = probe_of(&view, &mut vcx).columns.unwrap();
    let row_y = |row: f32| px(columns.top + row * LINE + LINE / 2.0);
    // A click inside the expanded tab lands before or after it.
    press(
        &mut vcx,
        point(px(columns.text_x + 1.4 * CHAR), row_y(0.0)),
        1,
        Modifiers::none(),
    );
    assert_eq!(selections(&view, &mut vcx), [(1, 1)]);
    press(
        &mut vcx,
        point(px(columns.text_x + 4.4 * CHAR), row_y(0.0)),
        1,
        Modifiers::none(),
    );
    assert_eq!(selections(&view, &mut vcx), [(2, 2)]);

    // Fold "y\nz": the row reads "x ⋯ w" (the chip's room on each side).
    fold(&view, &mut vcx, 6..9);
    set_cursor(&view, &mut vcx, 9);
    assert_eq!(caret_x(&view, &mut vcx), 4.0 * CHAR, "after the chip");
    set_cursor(&view, &mut vcx, 6);
    assert_eq!(caret_x(&view, &mut vcx), CHAR, "before it");
    // A click on the text after the chip.
    press(
        &mut vcx,
        point(px(columns.text_x + 4.9 * CHAR), row_y(1.0)),
        1,
        Modifiers::none(),
    );
    assert_eq!(selections(&view, &mut vcx), [(10, 10)]);
}

#[gpui::test]
fn selections_and_input_bounds_are_measured_in_columns(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "a\tb\tc\n");
    view.update(&mut vcx, |v, cx| {
        v.doc.as_mut().unwrap().editor.select_ranges(Some(1..4));
        cx.notify();
    });
    draw(&mut vcx);
    let probe = probe_of(&view, &mut vcx);
    let columns = probe.columns.unwrap();
    assert_eq!(probe.selections.len(), 1);
    let rect = probe.selections[0];
    // From the first tab (column 1) to the end of the second (column 8).
    assert_eq!(f32::from(rect.left()) - columns.text_x, CHAR);
    assert_eq!(f32::from(rect.size.width), 7.0 * CHAR);
    // The input method's bounds of `b`: after the first tab.
    let bounds = view.update_in(&mut vcx, |v, window, cx| {
        v.range_bounds(2..3, None, window, cx).unwrap()
    });
    assert_eq!(f32::from(bounds.left()) - columns.text_x, 4.0 * CHAR);
    assert_eq!(f32::from(bounds.size.width), CHAR);
}

#[gpui::test]
fn wrapped_rows_start_at_tab_layer_columns(cx: &mut TestAppContext) {
    // A tab, then words: the tab takes four columns, one buffer byte.
    let line = format!("\t{}", words(40));
    let (view, mut vcx) = editor(cx, &format!("{line}\n"));
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(0, window, cx).unwrap());
    assert_eq!(&row.text.content[..4], "    ");
    assert!(row.segments.len() > 1);
    let second = &row.segments[1];
    let columns = probe_of(&view, &mut vcx).columns.unwrap();
    // Three characters into the second row.
    let at = point(
        px(columns.text_x + second.indent + 3.0 * CHAR + 1.0),
        px(columns.top + LINE + LINE / 2.0),
    );
    press(&mut vcx, at, 1, Modifiers::none());
    let offset = second.columns.start - 3 + 3;
    assert_eq!(selections(&view, &mut vcx), [(offset, offset)]);
    // The caret is drawn there, on the second row.
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(probe.carets[0].origin.y, px(columns.top + LINE + 2.0));
    assert_eq!(
        f32::from(probe.carets[0].left()) - columns.text_x,
        second.indent + 3.0 * CHAR
    );
}

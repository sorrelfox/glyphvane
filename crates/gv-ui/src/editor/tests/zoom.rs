// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The zoom and the editor font settings in the Source view: rows get
//! taller, the line at the top stays there, soft wrap re-wraps with the
//! new measure (in the background for a large document), and carets,
//! selections, input-method bounds, line numbers and fold chips follow.

use super::*;
use crate::editor::EditorLayout;
use crate::settings::Target;

/// Sets `key` to `value` in the settings in effect and draws.
fn set(vcx: &mut VisualTestContext, key: &str, value: toml::Value) {
    vcx.update(|_, cx| {
        AppSettings::set_value(cx, Target::AllVaults, key, &value).expect("a valid setting");
    });
    draw(vcx);
}

fn zoom(vcx: &mut VisualTestContext, percent: i64) {
    set(vcx, crate::zoom::KEY, toml::Value::Integer(percent));
}

fn numbered(lines: usize) -> String {
    (0..lines).map(|i| format!("line {i}\n")).collect()
}

/// The row height, the scroll position and the line at the top.
fn place(view: &Entity<EditorView>, vcx: &mut VisualTestContext) -> (f32, f64, usize) {
    view.read_with(vcx, |v, _| (v.metrics().line, v.scroll_y, v.top_line()))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[gpui::test]
fn zoom_scales_the_rows_and_keeps_the_top_line(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, &numbered(300));
    let pad = f64::from(document::PAD_TOP);
    // Line 50 at the top, a quarter of its row scrolled away.
    view.update(&mut vcx, |v, cx| {
        v.scroll_y = pad + 50.0 * f64::from(LINE) + 5.0;
        cx.notify();
    });
    draw(&mut vcx);
    assert_eq!(place(&view, &mut vcx).2, 50);

    zoom(&mut vcx, 150);
    let (line, scroll_y, top) = place(&view, &mut vcx);
    assert_eq!(line, 30.0, "20 px rows at 150 %");
    assert!(close(scroll_y, pad + 50.0 * 30.0 + 7.5), "{scroll_y}");
    assert_eq!(top, 50);
    // Line 50's row is drawn 30 px tall, a quarter of it above the view.
    let start = view.read_with(&vcx, |v, _| {
        v.editor().unwrap().text().line_start(50).unwrap()
    });
    let bounds = view.update_in(&mut vcx, |v, window, cx| {
        v.range_bounds(start..start, None, window, cx).unwrap()
    });
    assert_eq!(f32::from(bounds.size.height), 30.0);
    assert_eq!(f32::from(bounds.top()), -7.5);
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(probe.lines.start, 50);

    // Back to 100 %: the same place.
    zoom(&mut vcx, 100);
    let (line, scroll_y, top) = place(&view, &mut vcx);
    assert_eq!(line, LINE);
    assert!(
        close(scroll_y, pad + 50.0 * f64::from(LINE) + 5.0),
        "{scroll_y}"
    );
    assert_eq!(top, 50);

    // At the very top the padding does not scale.
    view.update(&mut vcx, |v, cx| {
        v.scroll_y = 0.0;
        cx.notify();
    });
    draw(&mut vcx);
    zoom(&mut vcx, 200);
    assert_eq!(place(&view, &mut vcx), (40.0, 0.0, 0));
}

#[gpui::test]
fn zoom_re_wraps_with_soft_wrap_on(cx: &mut TestAppContext) {
    // 40 words of 6 characters: 240 characters.
    let (view, mut vcx) = editor(cx, &format!("{}\nplain\n", words(40)));
    view.update(&mut vcx, |v, cx| v.set_layout(EditorLayout::Pane, cx));
    draw(&mut vcx);
    let rows_at = |vcx: &mut VisualTestContext| rows_of(&probe_of(&view, vcx), 0).len();
    let before = rows_at(&mut vcx);
    let text_w = probe_of(&view, &mut vcx).columns.unwrap().text_w;
    assert!(before >= 2, "{before}");

    // Twice the size: the pane's text column keeps its width, so the line
    // takes about twice the rows, none wider than the column.
    zoom(&mut vcx, 200);
    let after = rows_at(&mut vcx);
    let probe = probe_of(&view, &mut vcx);
    let columns = probe.columns.unwrap();
    assert_eq!(columns.text_w, text_w);
    assert!(after >= 2 * before - 1, "{before} → {after}");
    let row = view.update_in(&mut vcx, |v, window, cx| v.row(0, window, cx).unwrap());
    assert!(row.content_width() <= text_w, "{}", row.content_width());
    // The line after it starts on the next row, 40 px down.
    assert_eq!(rows_of(&probe, 1), [(after, None)]);
    assert_eq!(probe.number_rows[..2], [0, after]);
}

#[gpui::test]
fn a_large_document_re_wraps_in_the_background_after_a_zoom(cx: &mut TestAppContext) {
    // Lines of 40 words (over the synchronous limit), three rows each in
    // the 680 px column.
    let text = long_lines(1200);
    assert!(text.len() > super::super::wrap::SYNC_REWRAP_BYTES);
    let (view, mut vcx) = editor(cx, &text);
    vcx.run_until_parked();
    draw(&mut vcx);
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 0).len(), 3);
    let pad = f64::from(document::PAD_TOP);
    view.update(&mut vcx, |v, cx| {
        v.scroll_y = pad + 3.0 * 100.0 * f64::from(LINE);
        cx.notify();
    });
    draw(&mut vcx);
    assert_eq!(place(&view, &mut vcx).2, 100);

    // A larger editor font (the column stays 680 px): the change starts a
    // job and the old wraps show until it lands.
    vcx.update(|_, cx| {
        let value = toml::Value::Float(25.0);
        AppSettings::set_value(cx, Target::AllVaults, "appearance.editor_font_size", &value)
            .expect("a valid setting");
    });
    let rewrapping = view.read_with(&vcx, |v, _| v.editor().unwrap().is_rewrapping());
    assert!(rewrapping, "a large document re-wraps in the background");
    vcx.run_until_parked();
    draw(&mut vcx);
    let (line, scroll_y, top) = place(&view, &mut vcx);
    assert_eq!(line, 40.0);
    assert_eq!(top, 100, "the top line stays");
    let rows = view.read_with(&vcx, |v, _| {
        let editor = v.editor().unwrap();
        assert!(!editor.is_rewrapping());
        v.row_map().unwrap().first_row(100)
    });
    // 45 characters a row: six rows a line.
    assert_eq!(rows, 600);
    assert!(close(scroll_y, pad + 600.0 * 40.0), "{scroll_y}");
    assert_eq!(rows_of(&probe_of(&view, &mut vcx), 100).len(), 6);
}

#[gpui::test]
fn carets_selections_and_input_bounds_follow_the_zoom(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "hello world\nsecond line\n# A\nbody\n");
    // "world" selected, the caret after it.
    view.update(&mut vcx, |v, cx| {
        v.doc
            .as_mut()
            .unwrap()
            .editor
            .select_ranges(std::iter::once(6..11));
        cx.notify();
    });
    draw(&mut vcx);
    zoom(&mut vcx, 150);
    let char_w = CHAR * 1.5;
    let probe = probe_of(&view, &mut vcx);
    let columns = probe.columns.unwrap();
    let selection = probe.selections[0];
    assert_eq!(f32::from(selection.size.height), 30.0);
    assert_eq!(f32::from(selection.left()), columns.text_x + 6.0 * char_w);
    assert_eq!(f32::from(selection.size.width), 5.0 * char_w);
    let caret = probe.carets[0];
    assert_eq!(caret.size, size(px(2.0), px(24.0)), "16 px at 150 %");
    assert_eq!(
        f32::from(caret.top()),
        columns.top + 3.0,
        "centred in its row"
    );
    assert_eq!(f32::from(caret.left()), columns.text_x + 11.0 * char_w);

    // The input method's bounds of "second", on the second row.
    let bounds = view.update_in(&mut vcx, |v, window, cx| {
        v.range_bounds(12..18, None, window, cx).unwrap()
    });
    assert_eq!(f32::from(bounds.top()), columns.top + 30.0);
    assert_eq!(f32::from(bounds.size.height), 30.0);
    assert_eq!(f32::from(bounds.size.width), 6.0 * char_w);

    // A click lands where the text is drawn now.
    let at = point(
        px(columns.text_x + 3.0 * char_w + 1.0),
        px(columns.top + 30.0 + 15.0),
    );
    press(&mut vcx, at, 1, Modifiers::none());
    assert_eq!(selections(&view, &mut vcx), [(15, 15)]);

    // A folded heading's chip is 24 px tall too.
    view.update(&mut vcx, |v, cx| {
        assert!(
            v.doc
                .as_mut()
                .unwrap()
                .editor
                .fold_ranges(std::iter::once(27..32))
        );
        cx.notify();
    });
    draw(&mut vcx);
    let probe = probe_of(&view, &mut vcx);
    let (chip, _) = &probe.chips[0];
    assert_eq!(f32::from(chip.size.height), 24.0);
    assert_eq!(f32::from(chip.top()), columns.top + 2.0 * 30.0 + 3.0);
}

#[gpui::test]
fn the_editor_font_size_applies_live(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "abcd\n");
    set(
        &mut vcx,
        "appearance.editor_font_size",
        toml::Value::Float(25.0),
    );
    let (metrics, width) = view.update_in(&mut vcx, |v, window, cx| {
        let row = v.row(0, window, cx).unwrap();
        (v.metrics().clone(), row.content_width())
    });
    assert_eq!(metrics.text_size, 25.0);
    assert_eq!(metrics.line, 40.0);
    assert_eq!(metrics.number_size, 22.0);
    // The test text system: 0.6 em a character.
    assert_eq!(width, 4.0 * 15.0);
    // The column is the Reading view's: the editor font does not change it.
    let probe = probe_of(&view, &mut vcx);
    assert_eq!(probe.columns.unwrap().text_w, COLUMN);
    // The line numbers' gutter grows with them: one digit of 0.6 × 22 px
    // still fits the 26 px numbers column.
    assert_eq!(
        probe.columns.unwrap().numbers_w,
        editor_size::LINE_NUMBER_WIDTH
    );

    // The family: one that is not installed falls back to the usual one.
    set(
        &mut vcx,
        "appearance.editor_font",
        toml::Value::String("No Such Font".to_owned()),
    );
    let family = view.read_with(&vcx, |v, _| v.fonts.regular().family.clone());
    let fallback = vcx.update(|_, cx| crate::inline::FontFamilies::from_app(cx).mono);
    assert_eq!(family, fallback);
}

#[gpui::test]
fn the_readable_width_setting_sets_the_column(cx: &mut TestAppContext) {
    let (view, mut vcx) = editor(cx, "text\n");
    zoom(&mut vcx, 120);
    let columns = probe_of(&view, &mut vcx).columns.unwrap();
    assert_eq!(columns.text_w, (COLUMN * 1.2).round());
    set(
        &mut vcx,
        "appearance.readable_line_width",
        toml::Value::Boolean(false),
    );
    let columns = probe_of(&view, &mut vcx).columns.unwrap();
    assert_eq!(columns.text_w, WIDTH - 2.0 * document::PAD_X);
}

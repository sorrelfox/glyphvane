// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The platform text input handler, and the geometry it and the pointer
//! need.
//!
//! Every answer is computed from the current buffer and the view's bounds,
//! never from the last paint: Wayland asks for the input method's area
//! inside the event that delivered the preedit, before the next frame is
//! laid out (IME spike, Problem 1). A row that changed since the last frame
//! is shaped again on the spot ([`super::rows::RowCache`]).
//!
//! UTF-16 ranges from the platform go through `gv-editor`'s input-method
//! API, which clips them to characters and never splits a surrogate pair
//! or a CRLF.

use std::ops::Range;

use gpui::{Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, point, px};
use gv_editor::boundaries::{clip_caret, snap_to_grapheme};
use gv_text::{Bias, OffsetUtf16};

use super::rows::{Row, RowEnv};
use super::{EditorView, LINE};
use crate::appearance::Appearance;
use crate::inline::Palette;
use crate::metrics::size::{document, editor};

/// Where the parts of the editor are, for given bounds and line count. It
/// does not depend on the text's content or on a paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Columns {
    pub bounds: Bounds<Pixels>,
    /// The left edge of the text column: the Reading view's text column.
    pub text_x: f32,
    pub text_w: f32,
    /// The right edge of the line numbers.
    pub numbers_right: f32,
    pub numbers_w: f32,
    /// The top of the first row at no scroll.
    pub top: f32,
}

impl Columns {
    /// The columns in `bounds` for `lines` lines, with `digit` the advance
    /// of a line-number digit. The line numbers are 26 wide and grow to
    /// the left when the numbers do not fit.
    pub fn new(bounds: Bounds<Pixels>, lines: usize, digit: f32) -> Self {
        let left = f32::from(bounds.left());
        let width = f32::from(bounds.size.width);
        let column = (width - 2.0 * document::PAD_X)
            .clamp(1.0, document::COLUMN_MAX)
            .floor();
        let digits = lines.max(1).ilog10() + 1;
        let numbers_w = editor::LINE_NUMBER_WIDTH.max((digits as f32 * digit).ceil());
        let gutter = numbers_w + editor::GUTTER_GAP;
        let text_x = (left + ((width - column) / 2.0).max(0.0).floor()).max(left + gutter);
        let text_w = column.min(left + width - text_x).max(1.0);
        Self {
            bounds,
            text_x,
            text_w,
            numbers_right: text_x - editor::GUTTER_GAP,
            numbers_w,
            top: f32::from(bounds.top()) + document::PAD_TOP,
        }
    }

    pub fn gutter_left(&self) -> f32 {
        self.numbers_right - self.numbers_w
    }

    pub fn text_right(&self) -> f32 {
        self.text_x + self.text_w
    }
}

impl EditorView {
    /// The columns for `bounds` (the last layout's when `None`).
    pub(crate) fn columns(
        &mut self,
        bounds: Option<Bounds<Pixels>>,
        window: &Window,
    ) -> Option<Columns> {
        let bounds = bounds.or(self.bounds)?;
        let lines = self.doc.as_ref()?.editor.text().line_count();
        let (digit, _) = self.advances(window);
        Some(Columns::new(bounds, lines, digit))
    }

    /// The top of `line`'s row in window coordinates.
    pub(crate) fn row_top(&self, columns: &Columns, line: usize) -> f32 {
        let y = line as f64 * f64::from(LINE) - self.scroll_y;
        columns.top + y as f32
    }

    /// The left of the text, scrolled.
    pub(crate) fn text_origin_x(&self, columns: &Columns) -> f32 {
        columns.text_x - self.scroll_x
    }

    /// The shaped row of `line` in the current text.
    pub(crate) fn row(&mut self, line: usize, window: &Window, cx: &gpui::App) -> Option<Row> {
        let theme = Appearance::current_id(cx);
        let palette = Palette::of(theme);
        let whitespace = self.show_whitespace;
        let fonts = self.fonts.clone();
        let doc = self.doc.as_mut()?;
        let env = RowEnv {
            text: doc.editor.text(),
            fonts: &fonts,
            palette: &palette,
            theme,
            whitespace,
            tab_size: doc.editor.display().tab_size().get(),
        };
        doc.rows
            .row(line, doc.highlights.get(line), env, window)
            .cloned()
    }

    /// The caret offset nearest to window point `position`.
    pub(crate) fn offset_at(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<usize> {
        let columns = self.columns(None, window)?;
        let (line_count, len) = {
            let text = self.doc.as_ref()?.editor.text();
            (text.line_count(), text.len())
        };
        let y = f64::from(f32::from(position.y) - columns.top) + self.scroll_y;
        if y < 0.0 {
            return Some(0);
        }
        let line = (y / f64::from(LINE)).floor() as usize;
        if line >= line_count {
            return Some(len);
        }
        let row = self.row(line, window, cx)?;
        let x = f32::from(position.x) - self.text_origin_x(&columns);
        let column = row.column_at(x).min(row.content_len());
        let text = self.doc.as_ref()?.editor.text();
        let start = text.line_start(line).ok()?;
        let offset = snap_to_grapheme(text, start + column);
        Some(clip_caret(text, offset, Bias::Left))
    }

    /// The window bounds of byte range `range` on its first line: from its
    /// start to its end, or to the end of the line if it goes on.
    pub(crate) fn range_bounds(
        &mut self,
        range: Range<usize>,
        element_bounds: Option<Bounds<Pixels>>,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.bounds.or(element_bounds);
        let columns = self.columns(bounds, window)?;
        let (line, start, end) = {
            let text = self.doc.as_ref()?.editor.text();
            let line = text.line_of_offset(range.start).ok()?;
            let line_range = text.line_range(line).ok()?;
            let start = range.start.saturating_sub(line_range.start);
            let end = (range.end.min(line_range.end)).saturating_sub(line_range.start);
            (line, start, end.max(start))
        };
        let row = self.row(line, window, cx)?;
        let x = self.text_origin_x(&columns);
        let top = self.row_top(&columns, line);
        Some(Bounds::from_corners(
            point(px(x + row.x(start)), px(top)),
            point(px(x + row.x(end)), px(top + LINE)),
        ))
    }

    fn utf16_of(&self, offset: usize) -> Option<usize> {
        let text = self.doc.as_ref()?.editor.text();
        text.offset_to_utf16(offset).ok().map(|OffsetUtf16(u)| u)
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<'_, Self>,
    ) -> Option<String> {
        let (text, actual) = self.editor()?.text_for_range_utf16(range);
        *adjusted_range = Some(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<'_, Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.editor()?.selected_text_range_utf16();
        Some(UTF16Selection {
            range: selection.range,
            reversed: selection.reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<'_, Self>,
    ) -> Option<Range<usize>> {
        self.editor()?.marked_text_range_utf16()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(doc) = self.doc.as_mut() {
            doc.editor.unmark_text();
            self.input_changed(cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(doc) = self.doc.as_mut() {
            doc.editor.replace_text_in_range_utf16(range, text);
            self.input_changed(cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(doc) = self.doc.as_mut() {
            doc.editor
                .replace_and_mark_text_in_range_utf16(range, new_text, new_selected_range);
            self.input_changed(cx);
        }
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.editor()?.clip_utf16_range(range_utf16);
        self.range_bounds(range, Some(element_bounds), window, cx)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<usize> {
        let offset = self.offset_at(point, window, cx)?;
        self.utf16_of(offset)
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let range = doc.editor.clip_utf16_range(range_utf16);
        doc.editor.select_ranges([range]);
        self.input_changed(cx);
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<'_, Self>,
    ) -> Option<usize> {
        Some(self.editor()?.text().len_utf16().0)
    }
}

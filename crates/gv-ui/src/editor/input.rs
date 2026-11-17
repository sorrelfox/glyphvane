// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The platform text input handler, and the geometry it and the pointer
//! need.
//!
//! Every answer is computed from the current buffer and the view's bounds,
//! never from the last paint: Wayland asks for the input method's area
//! inside the event that delivered the preedit, before the next frame is
//! laid out (IME spike, Problem 1). A row that changed since the last frame
//! is made from the display map and shaped again on the spot
//! ([`super::rows::RowCache`]). Positions are on display rows: the wrap
//! rows of the wrap layer while soft wrap is on.
//!
//! UTF-16 ranges from the platform go through `gv-editor`'s input-method
//! API, which clips them to characters and never splits a surrogate pair
//! or a CRLF. In viewer mode text and compositions from the platform are
//! ignored.

use std::ops::Range;

use gpui::{Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, point, px};
use gv_editor::boundaries::{clip_caret, snap_to_grapheme};
use gv_text::{Bias, OffsetUtf16};

use super::fold::VisualLine;
use super::rows::{Row, RowEnv};
use super::wrap::RowMap;
use super::{EditorLayout, EditorView};
use crate::appearance::Appearance;
use crate::inline::Palette;
use crate::metrics::size::{SCROLLBAR_THUMB, document, editor, split};
use crate::metrics::spacing::scrollbar;

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
    /// Where the current line's fill ends on the right.
    pub fill_right: f32,
}

impl Columns {
    /// The columns in `bounds` for `lines` lines, with `digit` the advance
    /// of a line-number digit and `column_max` the widest the centred
    /// column gets.
    ///
    /// - [`EditorLayout::Column`]: the Reading view's text column, centred;
    ///   the line numbers are 26 wide, 14 before the text, and grow to the
    ///   left when the numbers do not fit.
    /// - [`EditorLayout::Pane`]: from the pane's left edge; the line
    ///   numbers are 28 wide and grow to the right, then the fold
    ///   chevron's slot ([`super::fold`]) and the text, which ends clear
    ///   of the scrollbar.
    pub fn new(
        bounds: Bounds<Pixels>,
        lines: usize,
        digit: f32,
        layout: EditorLayout,
        column_max: f32,
    ) -> Self {
        let left = f32::from(bounds.left());
        let width = f32::from(bounds.size.width);
        let digits = lines.max(1).ilog10() + 1;
        let digits_w = (digits as f32 * digit).ceil();
        let top = f32::from(bounds.top()) + layout.pad_top();
        match layout {
            EditorLayout::Column => {
                let column = (width - 2.0 * document::PAD_X)
                    .min(column_max)
                    .max(1.0)
                    .floor();
                let numbers_w = editor::LINE_NUMBER_WIDTH.max(digits_w);
                let gutter = numbers_w + editor::GUTTER_GAP;
                let text_x = (left + ((width - column) / 2.0).max(0.0).floor()).max(left + gutter);
                let text_w = column.min(left + width - text_x).max(1.0);
                Self {
                    bounds,
                    text_x,
                    text_w,
                    numbers_right: text_x - editor::GUTTER_GAP,
                    numbers_w,
                    top,
                    fill_right: text_x + text_w,
                }
            }
            EditorLayout::Pane => {
                let numbers_w = split::LINE_NUMBER_WIDTH.max(digits_w);
                let numbers_right = left + numbers_w;
                let text_x = numbers_right
                    + (split::GUTTER - split::LINE_NUMBER_WIDTH)
                    + split::TEXT_PAD_LEFT;
                let clear = 2.0 * scrollbar::INSET_X + SCROLLBAR_THUMB;
                let text_w = (left + width - clear - text_x).floor().max(1.0);
                Self {
                    bounds,
                    text_x,
                    text_w,
                    numbers_right,
                    numbers_w,
                    top,
                    fill_right: left + width,
                }
            }
        }
    }

    pub fn gutter_left(&self) -> f32 {
        self.numbers_right - self.numbers_w
    }

    #[cfg(test)]
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
        let column_max = self.metrics.column_max;
        Some(Columns::new(bounds, lines, digit, self.layout, column_max))
    }

    /// The top of display row `row` in window coordinates.
    pub(crate) fn row_top(&self, columns: &Columns, row: usize) -> f32 {
        let y = row as f64 * f64::from(self.metrics.line) - self.scroll_y;
        columns.top + y as f32
    }

    /// The left of the text, scrolled.
    pub(crate) fn text_origin_x(&self, columns: &Columns) -> f32 {
        columns.text_x - self.scroll_x
    }

    /// The shaped row of the visual line that shows buffer line `line` in
    /// the current text, its first display row and the visual line.
    pub(crate) fn line_row(
        &mut self,
        line: usize,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<(usize, VisualLine, Row)> {
        let theme = Appearance::current_id(cx);
        let palette = Palette::of(theme);
        let whitespace = self.show_whitespace;
        let fonts = self.fonts.clone();
        let (_, space) = self.advances(window);
        let doc = self.doc.as_mut()?;
        let map = RowMap::of(&doc.editor);
        let start = map.visual_start(line);
        let first = map.first_row(start);
        let text = doc.editor.text();
        let tabs = doc.editor.display();
        let visual = VisualLine::new(text, tabs.replace_snapshot(), start)?;
        let breaks = map.breaks(&visual);
        let env = RowEnv {
            text,
            tabs,
            fonts: &fonts,
            palette: &palette,
            theme,
            whitespace,
            tab_size: tabs.tab_size().get(),
            space,
        };
        let styles = visual.styles(&doc.highlights);
        let row = doc
            .rows
            .row(&visual, styles, breaks, env, window)
            .cloned()?;
        Some((first, visual, row))
    }

    /// The shaped row of `line` in the current text.
    #[cfg(test)]
    pub(crate) fn row(&mut self, line: usize, window: &Window, cx: &gpui::App) -> Option<Row> {
        self.line_row(line, window, cx).map(|(_, _, row)| row)
    }

    /// The caret offset nearest to window point `position`.
    pub(crate) fn offset_at(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<usize> {
        let columns = self.columns(None, window)?;
        let (row_count, line, len, display_row) = {
            let map = self.row_map()?;
            let y = f64::from(f32::from(position.y) - columns.top) + self.scroll_y;
            if y < 0.0 {
                return Some(0);
            }
            let row = (y / f64::from(self.metrics.line)).floor() as usize;
            let len = self.doc.as_ref()?.editor.text().len();
            (map.row_count(), map.line_of_row(row), len, row)
        };
        if display_row >= row_count {
            return Some(len);
        }
        let (first, _, row) = self.line_row(line, window, cx)?;
        let segment = display_row.saturating_sub(first);
        let x = f32::from(position.x) - self.text_origin_x(&columns);
        let column = row.column_at(segment, x).min(row.content_len());
        let editor = &self.doc.as_ref()?.editor;
        let text = editor.text();
        let offset = row.offset_of(text, editor.display().replace_snapshot(), column);
        let offset = snap_to_grapheme(text, offset);
        Some(clip_caret(text, offset, Bias::Left))
    }

    /// The window bounds of byte range `range` on its first display row:
    /// from its start to its end, or to the end of the row if it goes on.
    pub(crate) fn range_bounds(
        &mut self,
        range: Range<usize>,
        element_bounds: Option<Bounds<Pixels>>,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.bounds.or(element_bounds);
        let columns = self.columns(bounds, window)?;
        let line = {
            let text = self.doc.as_ref()?.editor.text();
            text.line_of_offset(range.start).ok()?
        };
        let (first, visual, row) = self.line_row(line, window, cx)?;
        let editor = &self.doc.as_ref()?.editor;
        let (text, replace) = (editor.text(), editor.display().replace_snapshot());
        let start = row.column_of(text, replace, range.start, Bias::Left);
        let end = row
            .column_of(text, replace, range.end.min(visual.range.end), Bias::Right)
            .max(start);
        let (segment, x0) = row.x(start);
        let x1 = row.x_in(segment, end);
        let x = self.text_origin_x(&columns);
        let top = self.row_top(&columns, first + segment);
        Some(Bounds::from_corners(
            point(px(x + x0), px(top)),
            point(px(x + x1.max(x0)), px(top + self.metrics.line)),
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
        if self.read_only {
            return;
        }
        // Text an input method commits is not typing: it opens no
        // autocomplete.
        self.autocomplete.typed = self.doc.as_ref().is_some_and(|d| !d.editor.is_composing());
        if self.type_smart(range.as_ref(), text) {
            self.input_changed(cx);
        } else if let Some(doc) = self.doc.as_mut() {
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
        if let Some(doc) = self.doc.as_mut().filter(|_| !self.read_only) {
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

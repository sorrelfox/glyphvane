// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The table and list layouts.
//!
//! A result is flattened into lines of one height ([`Line`]): group
//! headers, rows, and group summaries. The lines are a virtual list, so
//! only the rows on screen are built. The table's header and its footer
//! (the summaries of the whole view) stay put above and below it; the
//! three scroll sideways together when the columns are wider than the
//! view.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, px, uniform_list,
};
use gv_bases::{Column, PropertyId, Summary, Value, ViewResult};

use super::cells::tooltip;
use super::{BasesView, Colors};
use crate::metrics::text;

/// The height of every line.
pub(crate) const ROW_HEIGHT: f32 = 30.0;
/// A column without `columnSize`.
pub(crate) const DEFAULT_COLUMN: f32 = 180.0;
const MIN_COLUMN: f32 = 48.0;
const MAX_COLUMN: f32 = 1600.0;
/// Horizontal padding of a cell.
const CELL_PAD: f32 = 8.0;

/// One line of the table or list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Line {
    /// The header of group `g`.
    Group(usize),
    /// Row `r` of group `g`.
    Row(usize, usize),
    /// The summaries of group `g`.
    GroupSummary(usize),
}

/// The lines of `result`: group headers and summaries only when the view
/// is grouped.
pub(crate) fn lines(result: &ViewResult) -> Vec<Line> {
    let mut out = Vec::with_capacity(result.shown + result.groups.len() * 2);
    for (g, group) in result.groups.iter().enumerate() {
        if result.grouped {
            out.push(Line::Group(g));
        }
        out.extend((0..group.rows.len()).map(|r| Line::Row(g, r)));
        if result.grouped && group.summaries.iter().any(Option::is_some) {
            out.push(Line::GroupSummary(g));
        }
    }
    out
}

/// The width of each column.
pub(crate) fn widths(columns: &[Column]) -> Vec<f32> {
    columns
        .iter()
        .map(|c| {
            #[allow(clippy::cast_possible_truncation)]
            c.width
                .map_or(DEFAULT_COLUMN, |w| w as f32)
                .clamp(MIN_COLUMN, MAX_COLUMN)
        })
        .collect()
}

/// Whether a column's text cell opens its row's file.
pub(crate) fn opens_row(property: &PropertyId) -> bool {
    matches!(property, PropertyId::File(n) if n == "name" || n == "basename" || n == "path")
}

/// The text of a group's key.
pub(crate) fn group_label(key: &Value) -> String {
    if key.is_empty() {
        "No value".to_owned()
    } else {
        key.display()
    }
}

impl BasesView {
    pub(crate) fn render_table(
        &mut self,
        result: &Arc<ViewResult>,
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let widths = widths(&result.columns);
        let total: f32 = widths.iter().sum::<f32>().max(1.0);
        let mut header = div()
            .flex()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .w(px(total))
            .bg(colors.header)
            .border_b_1()
            .border_color(colors.border_strong)
            .text_size(px(text::UI_SMALL.size))
            .font_weight(FontWeight::MEDIUM)
            .text_color(colors.secondary);
        for (i, (col, w)) in result.columns.iter().zip(&widths).enumerate() {
            header = header.child(
                div()
                    .id(("bases-header", i))
                    .debug_selector(move || format!("bases-header:{i}"))
                    .flex_none()
                    .w(px(*w))
                    .h_full()
                    .px(px(CELL_PAD))
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(colors.border)
                    .child(div().min_w_0().truncate().child(col.label.clone()))
                    .tooltip(tooltip(col.property.to_string().into(), *colors)),
            );
        }
        let count = self.lines.len();
        let list = uniform_list(
            "bases-table-rows",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| this.table_lines(range, cx)),
        )
        .track_scroll(&self.table_scroll)
        .flex_1()
        .w(px(total));
        let mut table = div()
            .flex()
            .flex_col()
            .h_full()
            .w(px(total))
            .child(header)
            .child(list);
        if result.summaries.iter().any(Option::is_some) {
            table = table.child(
                self.summary_line(&result.summaries, &widths, colors)
                    .border_t_1()
                    .border_color(colors.border_strong)
                    .bg(colors.header)
                    .debug_selector(|| "bases-footer".into()),
            );
        }
        div()
            .id("bases-table")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .child(table)
            .into_any_element()
    }

    /// The table's lines in `range`.
    fn table_lines(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let Some(result) = self.result.clone() else {
            return Vec::new();
        };
        let colors = Colors::of(crate::appearance::Appearance::theme(cx));
        let widths = widths(&result.columns);
        let total: f32 = widths.iter().sum::<f32>().max(1.0);
        let lines = self.lines.clone();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(line) = lines.get(ix).copied() else {
                continue;
            };
            let el = match line {
                Line::Group(g) => group_header(&result, g, total, &colors),
                Line::GroupSummary(g) => self
                    .summary_line(&result.groups[g].summaries, &widths, &colors)
                    .bg(colors.chip)
                    .into_any_element(),
                Line::Row(g, r) => {
                    self.rows_drawn.set(self.rows_drawn.get() + 1);
                    let row = &result.groups[g].rows[r];
                    let mut el = div()
                        .flex()
                        .h(px(ROW_HEIGHT))
                        .w(px(total))
                        .border_b_1()
                        .border_color(colors.border);
                    for (c, (col, w)) in result.columns.iter().zip(&widths).enumerate() {
                        let value = row.cells.get(c).cloned().unwrap_or(Ok(Value::Null));
                        let cell = self.cell(
                            &value,
                            Some(&row.path),
                            opens_row(&col.property),
                            (ix, c),
                            &colors,
                            cx,
                        );
                        el = el.child(
                            div()
                                .flex_none()
                                .w(px(*w))
                                .h_full()
                                .px(px(CELL_PAD))
                                .flex()
                                .items_center()
                                .overflow_hidden()
                                .child(cell),
                        );
                    }
                    el.into_any_element()
                }
            };
            out.push(el);
        }
        out
    }

    /// A line of summaries, one under each column that has one.
    fn summary_line(
        &self,
        summaries: &[Option<Summary>],
        widths: &[f32],
        colors: &Colors,
    ) -> gpui::Div {
        let total: f32 = widths.iter().sum::<f32>().max(1.0);
        let mut line = div()
            .flex()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .w(px(total))
            .text_size(px(text::UI_SMALL.size));
        for (i, w) in widths.iter().enumerate() {
            let mut cell = div()
                .flex_none()
                .w(px(*w))
                .h_full()
                .px(px(CELL_PAD))
                .flex()
                .items_center()
                .gap(px(6.0))
                .overflow_hidden();
            if let Some(Some(s)) = summaries.get(i) {
                cell = cell.child(
                    div()
                        .flex_none()
                        .text_color(colors.tertiary)
                        .child(s.name.clone()),
                );
                cell = cell.child(match &s.value {
                    Ok(v) => div()
                        .min_w_0()
                        .truncate()
                        .text_color(colors.text)
                        .child(v.display()),
                    Err(e) => div()
                        .min_w_0()
                        .truncate()
                        .text_color(colors.error)
                        .child(format!("Error: {e}")),
                });
            }
            line = line.child(cell);
        }
        line
    }

    pub(crate) fn render_list(
        &mut self,
        _result: &Arc<ViewResult>,
        _colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let count = self.lines.len();
        uniform_list(
            "bases-list-rows",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| this.list_lines(range, cx)),
        )
        .track_scroll(&self.list_scroll)
        .flex_1()
        .w_full()
        .into_any_element()
    }

    /// The list's lines in `range`: the first property, then the others,
    /// dimmer, separated by dots.
    fn list_lines(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let Some(result) = self.result.clone() else {
            return Vec::new();
        };
        let colors = Colors::of(crate::appearance::Appearance::theme(cx));
        let lines = self.lines.clone();
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(line) = lines.get(ix).copied() else {
                continue;
            };
            let el = match line {
                Line::Group(g) => group_header(&result, g, 0.0, &colors),
                Line::GroupSummary(_) => div().h(px(ROW_HEIGHT)).into_any_element(),
                Line::Row(g, r) => {
                    self.rows_drawn.set(self.rows_drawn.get() + 1);
                    let row = &result.groups[g].rows[r];
                    let mut el = div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .h(px(ROW_HEIGHT))
                        .w_full()
                        .px(px(16.0))
                        .overflow_hidden();
                    let mut first = true;
                    for (c, col) in result.columns.iter().enumerate() {
                        let value = row.cells.get(c).cloned().unwrap_or(Ok(Value::Null));
                        if matches!(value, Ok(ref v) if v.is_empty()) {
                            continue;
                        }
                        if !first {
                            el = el.child(div().flex_none().text_color(colors.tertiary).child("·"));
                        }
                        let opens = first || opens_row(&col.property);
                        let cell = self.cell(&value, Some(&row.path), opens, (ix, c), &colors, cx);
                        el = el.child(div().min_w_0().when_first(first, &colors).child(cell));
                        first = false;
                    }
                    if first {
                        // Nothing to show: the file's name.
                        let name: Arc<str> =
                            Arc::from(row.path.rsplit('/').next().unwrap_or(&row.path));
                        let cell = self.cell(
                            &Ok(Value::String(name)),
                            Some(&row.path),
                            true,
                            (ix, 0),
                            &colors,
                            cx,
                        );
                        el = el.child(cell);
                    }
                    el.into_any_element()
                }
            };
            out.push(el);
        }
        out
    }
}

trait FirstStyle {
    fn when_first(self, first: bool, colors: &Colors) -> Self;
}

impl FirstStyle for gpui::Div {
    fn when_first(self, first: bool, colors: &Colors) -> Self {
        if first {
            self.font_weight(FontWeight::MEDIUM)
                .flex_none()
                .max_w(px(360.0))
        } else {
            self.text_color(colors.secondary)
        }
    }
}

/// A group's header: its key and its number of rows.
fn group_header(result: &ViewResult, g: usize, width: f32, colors: &Colors) -> AnyElement {
    let group = &result.groups[g];
    let mut el = div()
        .debug_selector(move || format!("bases-group:{g}"))
        .flex()
        .items_center()
        .gap(px(8.0))
        .h(px(ROW_HEIGHT))
        .px(px(CELL_PAD))
        .bg(colors.header)
        .border_b_1()
        .border_color(colors.border)
        .font_weight(FontWeight::SEMIBOLD)
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(if group.key.is_empty() {
                    colors.tertiary
                } else {
                    colors.text
                })
                .child(group_label(&group.key)),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(text::UI_SMALL.size))
                .font_weight(FontWeight::NORMAL)
                .text_color(colors.tertiary)
                .child(group.rows.len().to_string()),
        );
    el = if width > 0.0 {
        el.w(px(width))
    } else {
        el.w_full()
    };
    el.into_any_element()
}

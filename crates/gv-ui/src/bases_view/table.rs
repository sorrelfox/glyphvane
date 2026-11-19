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
//!
//! Assistive technology gets a table: the header row's cells are column
//! headers, each line a row (numbered from 2, after the header) with a
//! cell per column named after the cell's text; group headers and
//! summary lines are rows too. The list layout is a list of items. The
//! keyboard's row is selected and its cell (a list's item) is the active
//! descendant ([`super::cursor`]).

use std::ops::Range;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, div, px, uniform_list,
};
use gv_bases::{Column, PropertyId, Summary, Value, ViewResult};

use super::cells::{cell_text, tooltip};
use super::{BasesView, Colors};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
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

/// The position of each line's row among the rows (`None` for group
/// headers and summaries): the list layout's item numbers.
pub(crate) fn row_numbers(lines: &[Line]) -> Vec<Option<usize>> {
    let mut n = 0;
    lines
        .iter()
        .map(|l| match l {
            Line::Row(..) => {
                n += 1;
                Some(n - 1)
            }
            _ => None,
        })
        .collect()
}

/// A group header's accessible name.
fn group_name(result: &ViewResult, g: usize) -> String {
    let group = &result.groups[g];
    let n = group.rows.len();
    format!(
        "Group {}, {n} {}",
        group_label(&group.key),
        if n == 1 { "file" } else { "files" }
    )
}

/// A summary line's accessible name: "Summaries: pages Sum 900, …".
fn summaries_name(columns: &[Column], summaries: &[Option<Summary>]) -> String {
    let parts: Vec<String> = columns
        .iter()
        .zip(summaries)
        .filter_map(|(c, s)| {
            let s = s.as_ref()?;
            let value = match &s.value {
                Ok(v) => v.display(),
                Err(e) => format!("Error: {e}"),
            };
            Some(format!("{} {} {value}", c.label, s.name))
        })
        .collect();
    format!("Summaries: {}", parts.join(", "))
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
            .id("bases-header-row")
            .role(Role::Row)
            .aria_row_index(1)
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
                    .role(Role::ColumnHeader)
                    .aria_label(col.label.clone())
                    .aria_column_index(i + 1)
                    .flex_none()
                    .w(px(*w))
                    .h_full()
                    .px(px(CELL_PAD))
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(colors.border)
                    .child(div().min_w_0().truncate().child(col.label.clone()))
                    .tooltip(tooltip(col.property.to_string().into(), *colors))
                    .accessible(),
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
            .child(header.accessible())
            .child(list);
        if result.summaries.iter().any(Option::is_some) {
            table = table.child(
                self.summary_line(&result.summaries, &widths, colors)
                    .id("bases-footer-row")
                    .role(Role::Row)
                    .aria_label(summaries_name(&result.columns, &result.summaries))
                    .aria_row_index(count + 2)
                    .border_t_1()
                    .border_color(colors.border_strong)
                    .bg(colors.header)
                    .debug_selector(|| "bases-footer".into())
                    .accessible(),
            );
        }
        let rows = count + 1 + usize::from(result.summaries.iter().any(Option::is_some));
        div()
            .id("bases-table")
            .a11y_group(Role::Table, self.view_label())
            .aria_row_count(rows)
            .aria_column_count(result.columns.len())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .child(table)
            .accessible()
            .into_any_element()
    }

    /// The table's lines in `range`.
    fn table_lines(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let Some(result) = self.result.clone() else {
            return Vec::new();
        };
        let theme = crate::appearance::Appearance::theme(cx);
        let colors = Colors::of(theme);
        let ring = crate::a11y::focus_ring(theme);
        let widths = widths(&result.columns);
        let total: f32 = widths.iter().sum::<f32>().max(1.0);
        let lines = self.lines.clone();
        let cursor = self.cursor;
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(line) = lines.get(ix).copied() else {
                continue;
            };
            let el = match line {
                Line::Group(g) => group_header(&result, g, total, &colors)
                    .id(("bases-group-row", ix))
                    .role(Role::Row)
                    .aria_label(group_name(&result, g))
                    .aria_row_index(ix + 2)
                    .accessible()
                    .into_any_element(),
                Line::GroupSummary(g) => self
                    .summary_line(&result.groups[g].summaries, &widths, &colors)
                    .id(("bases-summary-row", ix))
                    .role(Role::Row)
                    .aria_label(summaries_name(&result.columns, &result.groups[g].summaries))
                    .aria_row_index(ix + 2)
                    .bg(colors.chip)
                    .accessible()
                    .into_any_element(),
                Line::Row(g, r) => {
                    self.rows_drawn.set(self.rows_drawn.get() + 1);
                    let row = &result.groups[g].rows[r];
                    let name = row
                        .cells
                        .first()
                        .map(cell_text)
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| row.path.to_string());
                    let active = cursor.filter(|&(line, _)| line == ix).map(|(_, c)| c);
                    let mut el = div()
                        .id(("bases-row", ix))
                        .role(Role::Row)
                        .aria_label(name)
                        .aria_row_index(ix + 2)
                        .aria_selected(active.is_some())
                        .when(active.is_some(), |row| row.bg(colors.accent_tint))
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
                        let here = active == Some(c);
                        el = el.child(
                            div()
                                .id(ElementId::Name(format!("bases-cell:{ix}:{c}").into()))
                                .role(Role::Cell)
                                .aria_label(cell_text(&value))
                                .aria_row_index(ix + 2)
                                .aria_column_index(c + 1)
                                .when(here, |cell| {
                                    cell.aria_selected(true)
                                        .aria_active_descendant()
                                        .shadow(ring.clone())
                                })
                                .flex_none()
                                .w(px(*w))
                                .h_full()
                                .px(px(CELL_PAD))
                                .flex()
                                .items_center()
                                .overflow_hidden()
                                .child(cell)
                                .accessible(),
                        );
                    }
                    el.accessible().into_any_element()
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
        div()
            .id("bases-list")
            .a11y_group(Role::List, self.view_label())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                uniform_list(
                    "bases-list-rows",
                    count,
                    cx.processor(|this, range: Range<usize>, _, cx| this.list_lines(range, cx)),
                )
                .track_scroll(&self.list_scroll)
                .flex_1()
                .w_full(),
            )
            .accessible()
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
        let cursor = self.cursor.map(|(line, _)| line);
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(line) = lines.get(ix).copied() else {
                continue;
            };
            let el = match line {
                Line::Group(g) => group_header(&result, g, 0.0, &colors)
                    .id(("bases-list-group", ix))
                    .role(Role::Heading)
                    .aria_label(group_name(&result, g))
                    .accessible()
                    .into_any_element(),
                Line::GroupSummary(_) => div().h(px(ROW_HEIGHT)).into_any_element(),
                Line::Row(g, r) => {
                    self.rows_drawn.set(self.rows_drawn.get() + 1);
                    let row = &result.groups[g].rows[r];
                    let texts: Vec<String> = row
                        .cells
                        .iter()
                        .map(cell_text)
                        .filter(|t| !t.is_empty())
                        .collect();
                    let name = if texts.is_empty() {
                        row.path.rsplit('/').next().unwrap_or(&row.path).to_owned()
                    } else {
                        texts.join(", ")
                    };
                    let position = self.row_numbers.get(ix).copied().flatten().unwrap_or(0);
                    let here = cursor == Some(ix);
                    let mut el = div()
                        .id(("bases-list-item", ix))
                        .a11y_list_item(name, position, result.shown)
                        .aria_selected(here)
                        .when(here, |item| {
                            item.aria_active_descendant().bg(colors.accent_tint)
                        })
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
                    el.accessible().into_any_element()
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
fn group_header(result: &ViewResult, g: usize, width: f32, colors: &Colors) -> gpui::Div {
    let group = &result.groups[g];
    let el = div()
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
    if width > 0.0 {
        el.w(px(width))
    } else {
        el.w_full()
    }
}

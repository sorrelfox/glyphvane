// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Outline section of the right panel: the active document's headings
//! as a tree, the section being read highlighted. A click on a heading asks
//! the window to scroll there. Rows are drawn lazily.

use std::ops::Range;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Div, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, ScrollStrategy, SharedString, StatefulInteractiveElement as _,
    Styled as _, UniformListScrollHandle, Window, div, px, uniform_list,
};
use gv_model::DocModel;

use super::panels::section_head;
use crate::appearance::Appearance;
use crate::metrics::spacing::outline_row;
use crate::metrics::{radius, size, text};
use crate::theme::Theme;

/// What the Outline asks of its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutlinePanelEvent {
    /// Scroll the document to heading `n` (an index into the model's
    /// headings).
    Jump(usize),
}

/// The Outline section.
#[derive(Debug)]
pub(super) struct OutlinePanel {
    /// The active document's model; `None` while there is none loaded.
    model: Option<DocModel>,
    /// The heading whose section is being read.
    current: Option<usize>,
    /// The section is expanded.
    open: bool,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<OutlinePanelEvent> for OutlinePanel {}

impl OutlinePanel {
    pub fn new() -> Self {
        Self {
            model: None,
            current: None,
            open: true,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Shows the outline of `model`, with `current` the section being read.
    pub fn set_document(
        &mut self,
        model: Option<DocModel>,
        current: Option<usize>,
        cx: &mut Context<'_, Self>,
    ) {
        self.model = model;
        self.current = None;
        self.set_current(current, cx);
        cx.notify();
    }

    /// Moves the highlight to `current` and keeps its row in view.
    pub fn set_current(&mut self, current: Option<usize>, cx: &mut Context<'_, Self>) {
        if self.current == current {
            return;
        }
        self.current = current;
        if let Some(row) = current {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    fn toggle(&mut self, cx: &mut Context<'_, Self>) {
        self.open = !self.open;
        cx.notify();
    }

    fn heading_count(&self) -> usize {
        self.model.as_ref().map_or(0, |m| m.headings().len())
    }

    /// The rows as `(depth, text)`, for tests.
    #[cfg(test)]
    pub fn rows(&self) -> Vec<(u32, String)> {
        self.model.as_ref().map_or_else(Vec::new, |m| {
            m.headings()
                .iter()
                .zip(m.outline())
                .map(|(h, e)| (e.depth, h.text.to_string()))
                .collect()
        })
    }

    /// The highlighted row, for tests.
    #[cfg(test)]
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    /// Whether the section is expanded, for tests.
    #[cfg(test)]
    pub fn is_open(&self) -> bool {
        self.open
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<Div> {
        let theme = Appearance::theme(cx);
        let Some(model) = &self.model else {
            return Vec::new();
        };
        let (headings, outline) = (model.headings(), model.outline());
        let end = range.end.min(headings.len());
        let start = range.start.min(end);
        (start..end)
            .map(|ix| {
                let depth = outline[ix].depth;
                let current = self.current == Some(ix);
                let strong = current || depth == 0;
                let hover = theme.state.hover;
                let indent = size::OUTLINE_INDENT * depth as f32;
                let label: SharedString = headings[ix].text.to_string().into();
                let row = div()
                    .id(("outline-row", ix))
                    .debug_selector(move || format!("outline-row-{ix}"))
                    .size_full()
                    .flex()
                    .items_center()
                    .pl(px(outline_row::PAD_LEFT + indent))
                    .pr(px(outline_row::PAD_RIGHT))
                    .rounded(px(radius::CONTROL))
                    .map(|row| {
                        if strong {
                            row.text_color(theme.text.primary)
                                .font_weight(FontWeight(text::weight::ACTIVE))
                        } else {
                            row.text_color(theme.text.secondary)
                        }
                    })
                    .map(|row| {
                        if current {
                            row.bg(theme.accent.tint)
                        } else {
                            row.hover(move |style| style.bg(hover))
                        }
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(OutlinePanelEvent::Jump(ix));
                    }))
                    .child(div().min_w_0().truncate().child(label));
                // The list sizes its items to their content: a full-width
                // slot holds the row and its side margins.
                div()
                    .w_full()
                    .h(px(size::OUTLINE_ROW))
                    .px(px(size::LIST_ROW_INSET))
                    .child(row)
            })
            .collect()
    }

    fn render_body(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let body = div()
            .min_h_0()
            .flex_shrink_1()
            .flex()
            .flex_col()
            .pb(px(outline_row::SECTION_PAD_BOTTOM));
        let count = self.heading_count();
        if count == 0 {
            let note = if self.model.is_some() {
                "No headings"
            } else {
                "No document"
            };
            return body.child(message(note, theme));
        }
        body.child(
            uniform_list(
                "outline",
                count,
                cx.processor(|this, range: Range<usize>, _, cx| this.render_rows(range, cx)),
            )
            // As tall as its rows, and shorter when the panel runs out of
            // room; then it scrolls.
            .h(px(size::OUTLINE_ROW * count as f32))
            .min_h_0()
            .flex_shrink_1()
            .track_scroll(&self.scroll),
        )
    }
}

impl Render for OutlinePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let count = self.model.as_ref().map(|m| m.headings().len());
        div()
            .id("outline-panel")
            .debug_selector(|| "outline-panel".into())
            .min_h_0()
            .flex_shrink_1()
            .flex()
            .flex_col()
            .child(
                section_head("outline-head", "Outline", count, self.open, false, theme)
                    .debug_selector(|| "outline-head".into())
                    .on_click(cx.listener(|this, _, _, cx| this.toggle(cx))),
            )
            .when(self.open, |panel| panel.child(self.render_body(theme, cx)))
    }
}

/// A quiet one-line state where the rows would be (not designed).
fn message(note: &'static str, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "outline-message".into())
        .h(px(size::OUTLINE_ROW))
        .flex_none()
        .mx(px(size::LIST_ROW_INSET))
        .pl(px(outline_row::PAD_LEFT))
        .flex()
        .items_center()
        .text_color(theme.text.secondary)
        .child(note)
}

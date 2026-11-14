// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The document area: the active tab's Reading view in Preview mode. Edit
//! and Split hold a placeholder until the editor exists.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, Styled as _, div, px,
};

use super::{ViewMode, Workspace};
use crate::metrics::size::document;
use crate::metrics::spacing::document as spacing;
use crate::metrics::text;
use crate::theme::Theme;

impl Workspace {
    pub(super) fn render_document(&self, theme: &Theme, _cx: &mut Context<'_, Self>) -> AnyElement {
        let area = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .bg(theme.surface.page);
        let tab = self.active_tab();
        if let (ViewMode::Preview, Some(reader)) = (self.mode(), &tab.reader) {
            return area.child(reader.clone()).into_any_element();
        }
        let note = match self.mode() {
            ViewMode::Preview => "Nothing to show",
            ViewMode::Edit => "Editor not implemented yet",
            ViewMode::Split => "Split view not implemented yet",
        };
        // The text column is at most 680 wide plus its side padding, centred,
        // in every mode, so text does not move sideways when the mode changes.
        let column_width = document::COLUMN_MAX + 2.0 * document::PAD_X;

        area.flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(column_width))
                    .px(px(document::PAD_X))
                    .pt(px(document::PAD_TOP))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .mb(px(spacing::HEADING_GAP))
                            .text_size(px(text::PREVIEW_H1.size))
                            .line_height(px(text::PREVIEW_H1.line))
                            .font_weight(FontWeight(text::weight::BOLD))
                            .child(tab.label.name.clone()),
                    )
                    .child(
                        div()
                            .mb(px(spacing::PARAGRAPH_GAP))
                            .text_size(px(text::PREVIEW_BODY.size))
                            .line_height(px(text::PREVIEW_BODY.line))
                            .text_color(theme.text.secondary)
                            .child(note),
                    ),
            )
            .into_any_element()
    }
}

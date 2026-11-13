// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The status bar: where the document is at the left, facts about it at the
//! right.

use gpui::prelude::FluentBuilder as _;
use gpui::{Corners, Div, FontWeight, ParentElement as _, Pixels, Styled as _, div, px};

use super::{FileLabel, Tab};
use crate::icons::Icon;
use crate::metrics::spacing::{profile_chip, status_bar as spacing};
use crate::metrics::{icon, radius, size, text};
use crate::theme::Theme;

pub(super) fn status_bar(tab: &Tab, theme: &Theme, radii: Corners<Pixels>) -> Div {
    let file = &tab.label;
    div()
        .h(px(size::STATUS_BAR))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(spacing::GAP))
        .px(px(spacing::PAD_X))
        .bg(theme.surface.window)
        .border_t_1()
        .border_color(theme.border.default)
        .rounded_bl(radii.bottom_left)
        .rounded_br(radii.bottom_right)
        .text_size(px(text::UI_SMALL.size))
        .line_height(px(text::UI_SMALL.line))
        .text_color(theme.text.secondary)
        .whitespace_nowrap()
        .overflow_hidden()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(spacing::GAP))
                .child(breadcrumb(file, theme)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(spacing::GAP))
                // Where the Reading view will be scrolled to, until it exists.
                .when_some(tab.position, |row, position| {
                    row.child(match position.column {
                        Some(column) => format!("Ln {}, Col {column}", position.line),
                        None => format!("Ln {}", position.line),
                    })
                })
                // Counted once the document model exists.
                .child("— words")
                .child(profile(theme)),
        )
}

/// Folder, then file; the last item in `text.primary`.
fn breadcrumb(file: &FileLabel, theme: &Theme) -> Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(spacing::CRUMB_GAP))
        .when_some(file.folder.clone(), |crumbs, folder| {
            crumbs
                .child(folder)
                .child(Icon::ChevronRight.sized(icon::TINY))
        })
        .child(
            div()
                .text_color(theme.text.primary)
                .child(file.name.clone()),
        )
}

/// The profile chip. It is always visible; profiles are not detected yet.
fn profile(theme: &Theme) -> Div {
    div()
        .h(px(size::PROFILE_CHIP))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(profile_chip::GAP))
        .pl(px(profile_chip::PAD_LEFT))
        .pr(px(profile_chip::PAD_RIGHT))
        .rounded(px(radius::CHIP))
        .bg(theme.accent.tint)
        .text_color(theme.text.primary)
        .font_weight(FontWeight(text::weight::ACTIVE))
        .child(Icon::Profile.sized(icon::SMALL))
        .child("Default")
        .child(Icon::Select.sized(icon::TINY))
}

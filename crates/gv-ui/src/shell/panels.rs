// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The side panels. Only their frames and head rows exist so far.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Corners, Div, FontWeight, InteractiveElement as _, ParentElement as _, Pixels,
    Stateful, Styled as _, div, px,
};

use super::Workspace;
use super::controls::icon_button;
use super::title_row::{HAS_MENU_BUTTON, menu_button};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::icon_button as button;
use crate::metrics::spacing::{panel_head, section_head, sidebar_head};
use crate::metrics::{HAIRLINE, icon, size, text};
use crate::theme::Theme;

impl Workspace {
    /// The sidebar: full height above the status bar, with the panel
    /// switcher in a head that lines up with the title row.
    pub(super) fn render_sidebar(
        &self,
        client_decorated: bool,
        radii: Corners<Pixels>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let theme = Appearance::theme(cx);
        div()
            .id("sidebar")
            .w(px(size::SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.surface.panel)
            .border_r_1()
            .border_color(theme.border.default)
            .rounded_tl(radii.top_left)
            .child(
                div()
                    .h(px(size::TITLE_ROW))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(sidebar_head::GAP))
                    .px(px(sidebar_head::PAD_X))
                    .when(HAS_MENU_BUTTON, |head| head.child(menu_button(theme)))
                    .child(self.drag_region("sidebar-drag", client_decorated, cx))
                    .child(panel_switcher(theme)),
            )
            .child(panel_head("Files", theme))
    }
}

/// Files, Search, Tags and Bookmarks. Files is the active panel.
fn panel_switcher(theme: &Theme) -> Div {
    let panels = [Icon::Files, Icon::Search, Icon::Tags, Icon::Bookmarks];
    let size = (button::WIDTH, button::HEIGHT);
    div()
        .flex()
        .flex_none()
        .gap(px(sidebar_head::SWITCHER_GAP))
        .children(panels.into_iter().enumerate().map(|(index, panel)| {
            let button = icon_button(("panel", index), panel, icon::TOOLBAR, size, theme);
            if index == 0 {
                button.bg(theme.accent.tint).text_color(theme.text.primary)
            } else {
                button
            }
        }))
}

/// A panel head: title at the left, actions at the right.
fn panel_head(title: &'static str, theme: &Theme) -> Div {
    let actions = [Icon::NewNote, Icon::NewFolder, Icon::Sort];
    let size = (size::SMALL_ICON_BUTTON, size::SMALL_ICON_BUTTON);
    div()
        .h(px(size::PANEL_HEADER))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(panel_head::GAP))
        .pl(px(panel_head::PAD_LEFT))
        .pr(px(panel_head::PAD_RIGHT))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight(text::weight::STRONG))
                .child(title),
        )
        .children(actions.into_iter().enumerate().map(|(index, action)| {
            icon_button(("panel-action", index), action, icon::ROW, size, theme)
        }))
}

/// The right panel: collapsible sections stacked from the top.
pub(super) fn right_panel(theme: &Theme) -> Div {
    div()
        .w(px(size::PANEL_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(theme.surface.panel)
        .border_l_1()
        .border_color(theme.border.default)
        .child(section(theme, "Outline", false))
        .child(section(theme, "Backlinks", true))
}

fn section(theme: &Theme, title: &'static str, separated: bool) -> Div {
    // A separating line sits inside the head, which grows by its width.
    let height = if separated {
        size::PANEL_HEADER + HAIRLINE
    } else {
        size::PANEL_HEADER
    };
    div()
        .h(px(height))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(section_head::GAP))
        .px(px(section_head::PAD_X))
        .when(separated, |head| {
            head.border_t_1().border_color(theme.border.default)
        })
        .text_size(px(text::UI_SECTION.size))
        .line_height(px(text::UI_SECTION.line))
        .font_weight(FontWeight(text::weight::STRONG))
        .text_color(theme.text.secondary)
        .child(Icon::ChevronDown.sized(icon::CHEVRON))
        .child(title)
}

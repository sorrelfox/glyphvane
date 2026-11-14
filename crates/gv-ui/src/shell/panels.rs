// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The side panels. The sidebar holds the Files panel; the right panel
//! holds the Outline and the head of Backlinks.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Corners, Div, ElementId, Entity, FontWeight, InteractiveElement as _,
    ParentElement as _, Pixels, Stateful, Styled as _, Window, div, px,
};

use super::Workspace;
use super::controls::{RowStart, icon_button};
use super::outline_panel::OutlinePanel;
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::icon_button as button;
use crate::metrics::spacing::{section_head, sidebar_head};
use crate::metrics::{HAIRLINE, icon, size, text};
use crate::theme::Theme;

impl Workspace {
    /// The sidebar: full height above the status bar, with the panel
    /// switcher in a head that lines up with the title row.
    pub(super) fn render_sidebar(
        &self,
        radii: Corners<Pixels>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let theme = Appearance::theme(cx);
        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
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
                    .children(self.row_start(RowStart::SidebarHead, theme, window, cx))
                    .child(self.drag_region("sidebar-drag", window, cx).flex_1())
                    .child(panel_switcher(theme)),
            )
            .when_some(self.files.clone(), |sidebar, files| sidebar.child(files))
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

/// The right panel: collapsible sections stacked from the top. The Outline
/// comes first; Backlinks has only its head so far.
pub(super) fn right_panel(theme: &Theme, outline: Option<Entity<OutlinePanel>>) -> Div {
    div()
        .w(px(size::PANEL_WIDTH))
        .flex_none()
        .h_full()
        .min_h_0()
        .flex()
        .flex_col()
        .bg(theme.surface.panel)
        .border_l_1()
        .border_color(theme.border.default)
        .when_some(outline, |panel, outline| panel.child(outline))
        .child(
            section_head("backlinks-head", "Backlinks", None, true, true, theme)
                .debug_selector(|| "backlinks-head".into()),
        )
}

/// A section head: chevron, title and, at the right, a count. A
/// separating line above it sits inside the head, which grows by its width.
pub(super) fn section_head(
    id: impl Into<ElementId>,
    title: &'static str,
    count: Option<usize>,
    open: bool,
    separated: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let height = if separated {
        size::PANEL_HEADER + HAIRLINE
    } else {
        size::PANEL_HEADER
    };
    let chevron = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    div()
        .id(id)
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
        .child(chevron.sized(icon::CHEVRON))
        .child(div().flex_1().min_w_0().truncate().child(title))
        .when_some(count, |head, count| {
            head.child(
                div()
                    .flex_none()
                    .font_weight(FontWeight(text::weight::ACTIVE))
                    .child(count.to_string()),
            )
        })
}

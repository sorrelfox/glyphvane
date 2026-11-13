// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The title row: panel toggles, history, tabs, the mode switch.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Corners, Decorations, Div, FontWeight, InteractiveElement as _, ParentElement as _,
    Pixels, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px,
    transparent_black,
};

use super::controls::{
    RowStart, disabled_icon_button, icon_button, linux_window_controls, windows_caption_buttons,
};
use super::{ViewMode, Workspace};
use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::icons::Icon;
use crate::metrics::size::{icon_button as button, segment, tab};
use crate::metrics::spacing::{linux_controls, mode_switch, tab as tab_spacing, title_row};
use crate::metrics::{HAIRLINE, icon, radius, size, text};
use crate::theme::Theme;

impl Workspace {
    pub(super) fn render_title_row(
        &self,
        radii: Corners<Pixels>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let theme = Appearance::theme(cx);
        let wide = (button::WIDTH, button::HEIGHT);
        let square = (button::SQUARE, button::SQUARE);
        let caption_buttons = self.chrome.has_caption_buttons();
        let draws_linux_controls = self.chrome == WindowChrome::Linux
            && matches!(window.window_decorations(), Decorations::Client { .. });

        div()
            .id("title-row")
            .debug_selector(|| "title-row".into())
            .h(px(size::TITLE_ROW))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(title_row::GAP))
            .pl(px(title_row::PAD_X))
            // The caption buttons run to the window's right edge.
            .when(!caption_buttons, |row| row.pr(px(title_row::PAD_X)))
            .bg(theme.surface.window)
            .border_b_1()
            .border_color(theme.border.default)
            .rounded_tr(radii.top_right)
            .when(!self.sidebar_open, |row| {
                row.rounded_tl(radii.top_left)
                    .children(self.row_start(RowStart::TitleRow, theme, window, cx))
            })
            .child(
                icon_button(
                    "toggle-sidebar",
                    Icon::SidebarToggle,
                    icon::TOOLBAR,
                    wide,
                    theme,
                )
                .debug_selector(|| "toggle-sidebar".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
            )
            .child(disabled_icon_button(
                Icon::Back,
                icon::TOOLBAR,
                square,
                theme,
            ))
            .child(disabled_icon_button(
                Icon::Forward,
                icon::TOOLBAR,
                square,
                theme,
            ))
            .child(self.tab_strip(theme, cx))
            .child(icon_button(
                "new-tab",
                Icon::NewTab,
                icon::ROW,
                square,
                theme,
            ))
            .child(
                self.drag_region("title-drag", window, cx)
                    .flex_1()
                    .min_w(px(title_row::DRAG_MIN)),
            )
            .child(self.mode_switch(theme, cx))
            .child(
                icon_button(
                    "more-actions",
                    Icon::MoreActions,
                    icon::TOOLBAR,
                    square,
                    theme,
                )
                .ml(px(title_row::MORE_MARGIN)),
            )
            .child(
                icon_button(
                    "toggle-panel",
                    Icon::PanelToggle,
                    icon::TOOLBAR,
                    wide,
                    theme,
                )
                .debug_selector(|| "toggle-panel".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(cx))),
            )
            .when(caption_buttons, |row| {
                row.child(windows_caption_buttons(window, theme))
            })
            .when(draws_linux_controls, |row| {
                row.child(
                    linux_window_controls(window, theme)
                        .mr(px(linux_controls::INSET_RIGHT - title_row::PAD_X)),
                )
            })
    }

    /// Preview, Edit and Split as a segmented control.
    fn mode_switch(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let segments = [
            (ViewMode::Preview, Icon::Preview, Some("Preview")),
            (ViewMode::Edit, Icon::Edit, Some("Edit")),
            (ViewMode::Split, Icon::Split, None),
        ];
        div()
            .h(px(segment::FRAME))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(mode_switch::FRAME_GAP))
            .p(px(mode_switch::FRAME_PAD))
            .border_1()
            .border_color(theme.border.strong)
            .rounded(px(radius::SEGMENT_FRAME))
            .children(
                segments
                    .into_iter()
                    .enumerate()
                    .map(|(index, (mode, icon, label))| {
                        let active = self.mode() == mode;
                        let hover = theme.state.hover;
                        div()
                            .id(("mode", index))
                            .h(px(segment::INNER))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(mode_switch::SEGMENT_GAP))
                            .rounded(px(radius::SEGMENT))
                            .border_1()
                            .map(|seg| match label {
                                // The ring is inside the box, so it takes from the padding.
                                Some(_) => seg.px(px(mode_switch::SEGMENT_PAD_X - HAIRLINE)),
                                None => seg.w(px(mode_switch::ICON_SEGMENT_WIDTH)),
                            })
                            .map(|seg| {
                                if active {
                                    seg.bg(theme.surface.page)
                                        .border_color(theme.border.strong)
                                        .text_color(theme.text.primary)
                                        .font_weight(FontWeight(text::weight::ACTIVE))
                                } else {
                                    seg.border_color(transparent_black())
                                        .text_color(theme.text.secondary)
                                        .hover(move |style| style.bg(hover))
                                }
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
                            .child(icon.sized(icon::ROW))
                            .when_some(label, |seg, label| seg.child(label))
                    }),
            )
    }
}

impl Workspace {
    /// The open tabs, in order. Clicking a tab activates it.
    fn tab_strip(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        div()
            .flex()
            .min_w_0()
            .overflow_hidden()
            .items_center()
            .gap(px(tab_spacing::GAP))
            .ml(px(title_row::TAB_STRIP_MARGIN))
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let active = index == self.active;
                let hover = theme.state.hover;
                let close = (tab::CLOSE_BUTTON, tab::CLOSE_BUTTON);
                div()
                    .id(("tab", index))
                    .h(px(tab::HEIGHT))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(tab_spacing::GAP))
                    .pl(px(tab::PAD_LEFT - HAIRLINE))
                    .pr(px(tab::PAD_RIGHT - HAIRLINE))
                    .rounded(px(radius::TAB))
                    .border_1()
                    .map(|el| {
                        if active {
                            el.bg(theme.surface.page)
                                .border_color(theme.border.default)
                                .text_color(theme.text.primary)
                                .font_weight(FontWeight(text::weight::ACTIVE))
                        } else {
                            el.border_color(transparent_black())
                                .text_color(theme.text.secondary)
                                .hover(move |style| style.bg(hover))
                        }
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.activate_tab(index, window, cx)
                        }),
                    )
                    .child(div().min_w_0().truncate().child(tab.label.name.clone()))
                    .child(
                        icon_button(
                            ("close-tab", index),
                            Icon::CloseTab,
                            icon::TAB_CLOSE,
                            close,
                            theme,
                        )
                        .rounded(px(radius::SMALL_CONTROL)),
                    )
            }))
    }
}

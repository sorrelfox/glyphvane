// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The title row: panel toggles, history, tabs, the mode switch, More
//! actions. A right-click on a tab opens the tab menu.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, BoxShadow, Context, Corners, Div, FontWeight, InteractiveElement as _,
    IntoElement as _, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Stateful, StatefulInteractiveElement as _, Styled as _, Window, canvas, div, point, px,
    transparent_black,
};

use super::controls::{
    RowStart, client_decorated, disabled_icon_button, icon_button, linux_window_controls,
    windows_caption_buttons,
};
use super::{ViewMode, Workspace};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role, Toggled};
use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::icons::Icon;
use crate::metrics::size::{icon_button as button, segment, tab};
use crate::metrics::spacing::{linux_controls, mode_switch, tab as tab_spacing, title_row};
use crate::metrics::{DISABLED_OPACITY, HAIRLINE, icon, radius, size, text};
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
        let draws_linux_controls =
            self.chrome == WindowChrome::Linux && client_decorated(window, cx);

        div()
            .id("title-row")
            .a11y_group(Role::Toolbar, "Title bar")
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
                    "Sidebar",
                    Icon::SidebarToggle,
                    icon::TOOLBAR,
                    wide,
                    theme,
                )
                .aria_toggled(pressed(self.sidebar_open))
                .debug_selector(|| "toggle-sidebar".into())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx)))
                .accessible(),
            )
            .child(self.history_button(Icon::Back, theme, cx))
            .child(self.history_button(Icon::Forward, theme, cx))
            .child(self.tab_strip(theme, window, cx).accessible())
            .child(
                icon_button("new-tab", "New tab", Icon::NewTab, icon::ROW, square, theme)
                    .debug_selector(|| "new-tab".into())
                    .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx)))
                    .accessible(),
            )
            .child(
                self.drag_region("title-drag", window, cx)
                    .flex_1()
                    .min_w(px(title_row::DRAG_MIN)),
            )
            .child(self.mode_switch(theme, cx).accessible())
            .child(
                self.more_actions_button(theme, cx)
                    .ml(px(title_row::MORE_MARGIN))
                    .accessible(),
            )
            .child(
                icon_button(
                    "toggle-panel",
                    "Right panel",
                    Icon::PanelToggle,
                    icon::TOOLBAR,
                    wide,
                    theme,
                )
                .aria_toggled(pressed(self.panel_open))
                .debug_selector(|| "toggle-panel".into())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_panel(window, cx)))
                .accessible(),
            )
            .when(caption_buttons, |row| {
                row.child(windows_caption_buttons(window, self.chrome, theme))
            })
            .when(draws_linux_controls, |row| {
                let workspace = cx.weak_entity();
                let close = move |window: &mut Window, cx: &mut App| {
                    let _ = workspace.update(cx, |ws, cx| ws.request_close(window, cx));
                };
                row.child(
                    linux_window_controls(window, theme, close)
                        .mr(px(linux_controls::INSET_RIGHT - title_row::PAD_X)),
                )
            })
    }

    /// More actions: opens its menu below the button.
    fn more_actions_button(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Stateful<Div> {
        let square = (button::SQUARE, button::SQUARE);
        let anchor = self.menus.more_anchor.clone();
        icon_button(
            "more-actions",
            "More actions",
            Icon::MoreActions,
            icon::TOOLBAR,
            square,
            theme,
        )
        .debug_selector(|| "more-actions".into())
        .on_click(cx.listener(|this, _, window, cx| this.toggle_more_actions(window, cx)))
        // Where the button is, for the menu to hang below it.
        .child(
            canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
        )
    }

    /// Back or Forward: disabled while the active tab has nowhere to go.
    fn history_button(&self, icon: Icon, theme: &Theme, cx: &mut Context<'_, Self>) -> AnyElement {
        let square = (button::SQUARE, button::SQUARE);
        let (id, label, enabled) = match icon {
            Icon::Forward => ("history-forward", "Forward", self.can_go_forward()),
            _ => ("history-back", "Back", self.can_go_back()),
        };
        if !enabled {
            return disabled_icon_button(id, label, icon, icon::TOOLBAR, square, theme)
                .debug_selector(move || id.into())
                .into_any_element();
        }
        icon_button(id, label, icon, icon::TOOLBAR, square, theme)
            .debug_selector(move || id.into())
            .on_click(cx.listener(move |this, _, window, cx| {
                if icon == Icon::Forward {
                    this.go_forward(window, cx);
                } else {
                    this.go_back(window, cx);
                }
            }))
            .accessible()
            .into_any_element()
    }

    /// Shows the active tab in `mode`, as the mode switch does (the View
    /// menu's Preview, Edit and Split commands).
    pub(crate) fn show_mode(&mut self, mode: ViewMode, cx: &mut Context<'_, Self>) {
        self.set_mode(mode, cx);
    }

    /// Preview, Edit and Split as a segmented control.
    fn mode_switch(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Stateful<Div> {
        let segments = [
            (ViewMode::Preview, Icon::Preview, Some("Preview")),
            (ViewMode::Edit, Icon::Edit, Some("Edit")),
            (ViewMode::Split, Icon::Split, None),
        ];
        div()
            .id("mode-switch")
            .a11y_group(Role::RadioGroup, "View mode")
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
                        let name = match mode {
                            ViewMode::Preview => "Preview",
                            ViewMode::Edit => "Edit",
                            ViewMode::Split => "Split",
                        };
                        // Viewer mode: Edit and Split cannot be chosen.
                        let available = self.mode_available(mode, cx);
                        let hover = theme.state.hover;
                        div()
                            .id(("mode", index))
                            .role(Role::RadioButton)
                            .aria_label(name)
                            .aria_toggled(pressed(active))
                            .when(available, |seg| seg.keyboard_control(theme))
                            .debug_selector(move || {
                                match mode {
                                    ViewMode::Preview => "mode-preview",
                                    ViewMode::Edit => "mode-edit",
                                    ViewMode::Split => "mode-split",
                                }
                                .into()
                            })
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
                                } else if available {
                                    seg.border_color(transparent_black())
                                        .text_color(theme.text.secondary)
                                        .hover(move |style| style.bg(hover))
                                } else {
                                    seg.border_color(transparent_black())
                                        .text_color(theme.text.tertiary)
                                        .opacity(DISABLED_OPACITY)
                                }
                            })
                            .when(available, |seg| {
                                seg.on_click(
                                    cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)),
                                )
                            })
                            .child(icon.sized(icon::ROW))
                            .when_some(label, |seg, label| seg.child(label))
                            .accessible()
                            .disabled(!available)
                    }),
            )
    }
}

impl Workspace {
    /// The open tabs, in order. Clicking a tab activates it. A tab with
    /// unsaved changes shows a 7 px dot in its title's colour instead of
    /// the close icon; the dot still closes the tab (the text is saved or
    /// kept in the crash journal).
    ///
    /// From the keyboard the strip is one Tab stop (the focused tab, else
    /// the active one, and its close button); Left, Right, Home and End
    /// move between the tabs, Enter or Space shows the focused one (manual
    /// activation: a tab's document may take a while to open).
    fn tab_strip(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let count = self.tabs.len();
        let stop = self.tab_keys.stop(window, cx, Some(self.active));
        div()
            .id("tab-strip")
            .a11y_group(Role::TabList, "Open tabs")
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
                let unsaved = tab
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.read(cx).is_modified());
                let title_colour = if active {
                    theme.text.primary
                } else {
                    theme.text.secondary
                };
                let trailing = if unsaved {
                    let press = theme.state.press;
                    div()
                        .id(("close-tab", index))
                        .a11y_button(format!("Close {} (unsaved changes)", tab.label.name))
                        .keyboard_control(theme)
                        .debug_selector(move || format!("unsaved-dot-{index}"))
                        .flex_none()
                        .w(px(close.0))
                        .h(px(close.1))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(move |style| style.bg(hover))
                        .active(move |style| style.bg(press))
                        .child(
                            div()
                                .size(px(tab::UNSAVED_DOT))
                                .rounded_full()
                                .bg(title_colour),
                        )
                } else {
                    icon_button(
                        ("close-tab", index),
                        format!("Close {}", tab.label.name),
                        Icon::CloseTab,
                        icon::TAB_CLOSE,
                        close,
                        theme,
                    )
                    .debug_selector(move || format!("close-tab-{index}"))
                }
                .rounded(px(radius::SMALL_CONTROL))
                .tab_stop(index == stop)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_tab(index, window, cx);
                }));
                div()
                    .id(("tab", index))
                    .a11y_tab(tab.label.name.clone(), active, index, count)
                    .when(unsaved, |el| el.aria_description("Unsaved changes"))
                    .roving_tab(theme, &self.tab_keys, index, index == stop, |_, _, _| {})
                    .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                        if e.keystroke.key == "delete" && !e.keystroke.modifiers.modified() {
                            cx.stop_propagation();
                            this.close_tab_from_keys(index, window, cx);
                        }
                    }))
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
                                .shadow(tab_shadow(theme))
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
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_tab_menu(index, e.position, window, cx);
                        }),
                    )
                    .debug_selector(move || format!("tab-{index}"))
                    .child(div().min_w_0().truncate().child(tab.label.name.clone()))
                    .child(trailing.accessible())
                    .accessible()
            }))
    }
}

impl Workspace {
    /// Delete on a focused tab (or its close button): closes it as its
    /// close button would, and the keyboard goes to the tab that takes its
    /// place (the one before it when it was the last). A close that waits
    /// for an answer about unsaved changes leaves the keyboard alone.
    pub(super) fn close_tab_from_keys(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let before = self.tabs.len();
        self.close_tab(index, window, cx);
        let closed = self.tabs.len() < before || (before == 1 && self.tabs[0].is_placeholder());
        if closed {
            let to = index.min(self.tabs.len().saturating_sub(1));
            self.tab_keys.focus(to, window, cx);
            cx.notify();
        }
    }
}

/// A toggle button's state.
fn pressed(on: bool) -> Toggled {
    if on { Toggled::True } else { Toggled::False }
}

/// `shadow.tab`, the active tab's lift.
pub(super) fn tab_shadow(theme: &Theme) -> Vec<BoxShadow> {
    theme
        .shadow
        .tab
        .iter()
        .map(|l| BoxShadow {
            color: l.color.into(),
            offset: point(px(l.x), px(l.y)),
            blur_radius: px(l.blur),
            spread_radius: px(l.spread),
            inset: false,
        })
        .collect()
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Small controls shared by the window's rows.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Div, ElementId, InteractiveElement as _, MouseButton, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, WindowControlArea, div, px,
};

use super::Workspace;
use crate::icons::Icon;
use crate::metrics::spacing::linux_controls;
use crate::metrics::{DISABLED_OPACITY, radius};
use crate::theme::Theme;

/// An icon button: `text.secondary`, no fill at rest, the hover overlay on
/// hover and the press overlay while pressed.
pub(super) fn icon_button(
    id: impl Into<ElementId>,
    icon: Icon,
    icon_size: f32,
    (width, height): (f32, f32),
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .flex_none()
        .w(px(width))
        .h(px(height))
        .rounded(px(radius::CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.text.secondary)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(press))
        .child(icon.sized(icon_size))
}

/// An icon button that cannot be used now: `text.tertiary` at half opacity.
pub(super) fn disabled_icon_button(
    icon: Icon,
    icon_size: f32,
    (width, height): (f32, f32),
    theme: &Theme,
) -> Div {
    div()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.text.tertiary)
        .opacity(DISABLED_OPACITY)
        .child(icon.sized(icon_size))
}

impl Workspace {
    /// Empty chrome the user can drag the window by. With client-side
    /// decorations the window moves itself; a double click maximises and a
    /// right click opens the window menu.
    pub(super) fn drag_region(
        &self,
        id: impl Into<ElementId>,
        client_decorated: bool,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .flex_1()
            .h_full()
            .window_control_area(WindowControlArea::Drag)
            .when(client_decorated, |region| {
                region
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.drag_pending = true),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.drag_pending = false),
                    )
                    .on_mouse_move(cx.listener(|this, _, window, _| {
                        if this.drag_pending {
                            this.drag_pending = false;
                            window.start_window_move();
                        }
                    }))
                    .on_click(|event, window, _| {
                        if event.click_count() == 2 {
                            window.zoom_window();
                        }
                    })
                    .on_mouse_down(MouseButton::Right, |event, window, _| {
                        window.show_window_menu(event.position);
                    })
            })
    }
}

/// Minimise, maximise and close, drawn by the application for a
/// client-decorated window on Linux: round buttons on the press overlay.
pub(super) fn linux_window_controls(window: &Window, theme: &Theme) -> Div {
    let supported = window.window_controls();
    let maximize_icon = if window.is_maximized() {
        Icon::WindowRestore
    } else {
        Icon::WindowMaximize
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(linux_controls::GAP))
        .ml(px(linux_controls::MARGIN_LEFT))
        .when(supported.minimize, |row| {
            row.child(window_control(
                "minimize",
                Icon::WindowMinimize,
                theme,
                |w| w.minimize_window(),
            ))
        })
        .when(supported.maximize, |row| {
            row.child(window_control("maximize", maximize_icon, theme, |w| {
                w.zoom_window()
            }))
        })
        .child(window_control("close", Icon::WindowClose, theme, |w| {
            w.remove_window()
        }))
}

fn window_control(
    id: &'static str,
    icon: Icon,
    theme: &Theme,
    action: fn(&mut Window),
) -> Stateful<Div> {
    let hover = theme.state.hover;
    // The hover overlay is drawn on top of the press fill.
    let face = div()
        .size_full()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .hover(move |style| style.bg(hover))
        .child(icon.sized(linux_controls::GLYPH));
    div()
        .id(id)
        .flex_none()
        .size(px(linux_controls::DIAMETER))
        .rounded_full()
        .bg(theme.state.press)
        .text_color(theme.text.primary)
        .child(face)
        .on_click(move |_, window, _| action(window))
}

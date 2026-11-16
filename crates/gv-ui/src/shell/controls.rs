// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Small controls shared by the window's rows.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, Decorations, Div, ElementId, InteractiveElement as _,
    IntoElement as _, MouseButton, ParentElement as _, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, WindowControlArea, div, px,
};

use super::Workspace;
use crate::chrome::WindowChrome;
use crate::icons::{CaptionGlyph, Icon};
use crate::menu::MenuButton;
use crate::metrics::size::icon_button as button;
use crate::metrics::spacing::{
    linux_controls, macos_controls, sidebar_head, title_row, windows_controls,
};
use crate::metrics::{DISABLED_OPACITY, icon, radius};
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

/// The application menu button (Windows and Linux). It opens the
/// application menu ([`crate::menu`]).
pub(super) fn menu_button(theme: &Theme) -> Stateful<Div> {
    icon_button(
        "app-menu",
        Icon::Menu,
        icon::TOOLBAR,
        (button::MENU_WIDTH, button::HEIGHT),
        theme,
    )
    .debug_selector(|| "app-menu".into())
    .text_color(theme.text.primary)
}

/// Where the window's controls or its menu button go at the start of a row:
/// the sidebar head, or the title row while the sidebar is hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowStart {
    SidebarHead,
    TitleRow,
}

impl Workspace {
    /// The start of the sidebar head or of the title row: the menu button on
    /// Windows and Linux, free space for the system's window controls on
    /// macOS (none in full screen, where the system hides them).
    pub(super) fn row_start(
        &self,
        place: RowStart,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if self.chrome.has_menu_button() {
            let state = self.menu_state(self.active, cx);
            return Some(MenuButton::new(menu_button(theme), state).into_any_element());
        }
        if !self.chrome.has_system_window_controls() || window.is_fullscreen() {
            return None;
        }
        // The controls sit at a fixed place in the window, 13 from its left
        // edge; the row's own padding is part of that inset.
        let (id, width) = match place {
            RowStart::SidebarHead => (
                "sidebar-window-controls",
                macos_controls::INSET_LEFT - sidebar_head::PAD_X + macos_controls::WIDTH,
            ),
            RowStart::TitleRow => (
                "title-window-controls",
                macos_controls::INSET_LEFT - title_row::PAD_X
                    + macos_controls::WIDTH
                    + macos_controls::MARGIN_RIGHT,
            ),
        };
        Some(
            self.drag_region(id, window, cx)
                .debug_selector(move || id.into())
                .flex_none()
                .w(px(width))
                .into_any_element(),
        )
    }

    /// Empty chrome the user can drag the window by. Each platform moves the
    /// window its own way:
    ///
    /// - Windows: the system, from the hit test. It also maximises on a
    ///   double click, snaps, and opens the system menu on a right click.
    /// - macOS: the window moves itself; a double click does what the user
    ///   chose in System Settings (zoom or minimise).
    /// - Linux with client-side decorations: the window moves itself; a
    ///   double click maximises and a right click opens the window menu.
    pub(super) fn drag_region(
        &self,
        id: impl Into<ElementId>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let region = div()
            .id(id)
            .h_full()
            .window_control_area(WindowControlArea::Drag);
        let moves_itself = match self.chrome {
            WindowChrome::Windows => false,
            WindowChrome::MacOs => true,
            WindowChrome::Linux => {
                matches!(window.window_decorations(), Decorations::Client { .. })
            }
        };
        if !moves_itself {
            return region;
        }
        let chrome = self.chrome;
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
            .on_click(move |event, window, _| {
                if event.click_count() == 2 {
                    match chrome {
                        WindowChrome::MacOs => window.titlebar_double_click(),
                        WindowChrome::Windows | WindowChrome::Linux => window.zoom_window(),
                    }
                }
            })
            .when(chrome == WindowChrome::Linux, |region| {
                region.on_mouse_down(MouseButton::Right, |event, window, _| {
                    window.show_window_menu(event.position);
                })
            })
    }
}

/// Minimise, maximise and close at the far right of the title row on
/// Windows. The buttons only mark their area: Windows hit-tests it and
/// handles the click itself, which also gives the maximise button its Snap
/// Layouts flyout.
pub(super) fn windows_caption_buttons(window: &Window, theme: &Theme) -> Div {
    let maximize = if window.is_maximized() {
        CaptionGlyph::Restore
    } else {
        CaptionGlyph::Maximize
    };
    div()
        .flex()
        .flex_none()
        .h_full()
        .ml(px(windows_controls::MARGIN_LEFT))
        .child(caption_button(
            "caption-minimize",
            CaptionGlyph::Minimize,
            WindowControlArea::Min,
            theme,
        ))
        .child(caption_button(
            "caption-maximize",
            maximize,
            WindowControlArea::Max,
            theme,
        ))
        .child(caption_button(
            "caption-close",
            CaptionGlyph::Close,
            WindowControlArea::Close,
            theme,
        ))
}

/// One caption button. Hover and press are the usual overlays; the design
/// has no separate close-button colour.
fn caption_button(
    id: &'static str,
    glyph: CaptionGlyph,
    area: WindowControlArea,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .w(px(windows_controls::WIDTH))
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.text.primary)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(press))
        .window_control_area(area)
        .child(glyph.sized(windows_controls::GLYPH))
}

/// Draws the Linux window controls in a test window, whose platform
/// decorates it itself.
#[cfg(test)]
pub(crate) struct ClientDecorationsForTests;

#[cfg(test)]
impl gpui::Global for ClientDecorationsForTests {}

/// Whether `window` is decorated by the application.
pub(super) fn client_decorated(window: &Window, cx: &App) -> bool {
    #[cfg(test)]
    if cx.has_global::<ClientDecorationsForTests>() {
        return true;
    }
    let _ = cx;
    matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Minimise, maximise and close, drawn by the application for a
/// client-decorated window on Linux: round buttons on the press overlay.
/// `on_close` asks the window to close, as the platform would.
pub(super) fn linux_window_controls(
    window: &Window,
    theme: &Theme,
    on_close: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
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
                |w, _| w.minimize_window(),
            ))
        })
        .when(supported.maximize, |row| {
            row.child(window_control("maximize", maximize_icon, theme, |w, _| {
                w.zoom_window()
            }))
        })
        .child(window_control("close", Icon::WindowClose, theme, on_close))
}

fn window_control(
    id: &'static str,
    icon: Icon,
    theme: &Theme,
    action: impl Fn(&mut Window, &mut App) + 'static,
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
        .debug_selector(move || format!("window-{id}"))
        .flex_none()
        .size(px(linux_controls::DIAMETER))
        .rounded_full()
        .bg(theme.state.press)
        .text_color(theme.text.primary)
        .child(face)
        .on_click(move |_, window, cx| action(window, cx))
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Window chrome per platform. The window has the same structure everywhere:
//! one title row and one status bar. Only the window controls, the place of
//! the menu and the corner radius differ.

use gpui::{Pixels, Point, SharedString, TitlebarOptions, WindowDecorations, point, px};

use crate::metrics::radius;
use crate::metrics::size::TITLE_ROW;
use crate::metrics::spacing::macos_controls;

/// The platform conventions a window's chrome follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowChrome {
    /// The system draws the window controls at the left of the sidebar head,
    /// or at the start of the title row while the sidebar is hidden. The
    /// application menu is the system menu bar.
    MacOs,
    /// The application draws three caption buttons at the far right of the
    /// title row and a menu button at the left of the sidebar head.
    Windows,
    /// Client-side decorations: the application draws round window controls
    /// at the right of the title row and a menu button at the left of the
    /// sidebar head.
    Linux,
}

impl WindowChrome {
    /// The chrome of the platform this build runs on.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// Windows and Linux open the application menu from a button in the
    /// window; macOS uses the system menu bar.
    pub fn has_menu_button(self) -> bool {
        !matches!(self, Self::MacOs)
    }

    /// The system draws the window controls over the window's content.
    pub fn has_system_window_controls(self) -> bool {
        matches!(self, Self::MacOs)
    }

    /// The application draws caption buttons that the system hit-tests.
    pub fn has_caption_buttons(self) -> bool {
        matches!(self, Self::Windows)
    }

    pub fn window_radius(self) -> f32 {
        match self {
            Self::MacOs => radius::window::MACOS,
            Self::Windows => radius::window::WINDOWS,
            Self::Linux => radius::window::LINUX,
        }
    }

    /// Title bar options. On macOS and Windows the system title bar is
    /// hidden and the title row takes its place; on Linux the same is asked
    /// for through [`Self::decorations`].
    pub fn titlebar(self, title: SharedString) -> TitlebarOptions {
        TitlebarOptions {
            title: Some(title),
            appears_transparent: !matches!(self, Self::Linux),
            traffic_light_position: self.has_system_window_controls().then(traffic_light_origin),
        }
    }

    /// Linux asks for client-side decorations. The compositor may refuse;
    /// the window then gets the system title bar and draws no controls.
    pub fn decorations(self) -> WindowDecorations {
        match self {
            Self::Linux => WindowDecorations::Client,
            Self::MacOs | Self::Windows => WindowDecorations::Server,
        }
    }

    /// On macOS the window moves itself from its drag regions, so AppKit
    /// neither drags from the hidden title bar nor delays clicks in the title
    /// row to tell single from double clicks.
    pub fn app_owns_titlebar_drag(self) -> bool {
        matches!(self, Self::MacOs)
    }
}

/// Where GPUI places the macOS close button's frame, from the window's top
/// left corner. The frame is wider and taller than the circle it holds, so
/// the origin is set to put the circle 13 from the left and centred in the
/// title row.
pub fn traffic_light_origin() -> Point<Pixels> {
    let frame_inset_x = (macos_controls::BUTTON_FRAME_WIDTH - macos_controls::DIAMETER) / 2.0;
    point(
        px(macos_controls::INSET_LEFT - frame_inset_x),
        px((TITLE_ROW - macos_controls::BUTTON_FRAME_HEIGHT) / 2.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [WindowChrome; 3] = [
        WindowChrome::MacOs,
        WindowChrome::Windows,
        WindowChrome::Linux,
    ];

    #[test]
    fn traffic_lights_sit_where_the_design_puts_them() {
        let origin = traffic_light_origin();
        // The 12 px circle is centred in AppKit's 14 × 16 button frame.
        let circle_left = f32::from(origin.x) + 1.0;
        let circle_centre_y = f32::from(origin.y) + macos_controls::BUTTON_FRAME_HEIGHT / 2.0;
        assert_eq!(circle_left, macos_controls::INSET_LEFT);
        assert_eq!(circle_centre_y, TITLE_ROW / 2.0);
        assert_eq!(macos_controls::WIDTH, 52.0);
    }

    #[test]
    fn only_macos_and_windows_hide_the_system_title_bar() {
        for chrome in ALL {
            let titlebar = chrome.titlebar("Note — Glyphvane".into());
            assert_eq!(titlebar.appears_transparent, chrome != WindowChrome::Linux);
            assert_eq!(
                titlebar.traffic_light_position.is_some(),
                chrome == WindowChrome::MacOs
            );
            assert_eq!(
                chrome.decorations() == WindowDecorations::Client,
                chrome == WindowChrome::Linux
            );
        }
    }

    #[test]
    fn each_platform_has_exactly_one_place_for_the_controls() {
        for chrome in ALL {
            let drawn_by_us = chrome.has_caption_buttons() || chrome == WindowChrome::Linux;
            assert_ne!(drawn_by_us, chrome.has_system_window_controls());
            assert_eq!(chrome.has_menu_button(), chrome != WindowChrome::MacOs);
        }
    }
}

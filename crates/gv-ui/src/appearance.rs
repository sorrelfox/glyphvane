// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The GPUI side of theming: the theme in effect, its colours as GPUI colours,
//! and GPUI Component's own theme kept in step with it.

use gpui::component::{Theme as ComponentTheme, ThemeMode};
use gpui::{App, Fill, Global, Hsla, Rgba, SharedString, WindowAppearance, px};

use crate::metrics::{font, radius, text};
use crate::theme::{Color, OsAppearance, PreferredLight, Scheme, Theme, ThemeChoice, ThemeId};

impl From<Color> for Rgba {
    fn from(color: Color) -> Self {
        let channel = |value: u8| f32::from(value) / 255.0;
        Rgba {
            r: channel(color.r),
            g: channel(color.g),
            b: channel(color.b),
            a: color.alpha(),
        }
    }
}

impl From<Color> for Hsla {
    fn from(color: Color) -> Self {
        Rgba::from(color).into()
    }
}

impl From<Color> for Fill {
    fn from(color: Color) -> Self {
        Hsla::from(color).into()
    }
}

/// Maps GPUI's window appearance to the operating system appearance.
pub fn os_appearance(appearance: WindowAppearance) -> OsAppearance {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => OsAppearance::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => OsAppearance::Dark,
    }
}

/// The theme setting, the operating system's appearance and the theme that
/// follows from them. One per application.
#[derive(Debug)]
pub struct Appearance {
    choice: ThemeChoice,
    preferred_light: PreferredLight,
    os: OsAppearance,
    interface_font: SharedString,
}

impl Global for Appearance {}

impl Appearance {
    /// Installs the theme state and applies it. Call once at startup, after
    /// GPUI Component is initialised.
    pub fn init(cx: &mut App, choice: ThemeChoice) {
        let installed = cx.text_system().all_font_names();
        let interface_font = if installed.iter().any(|name| name == font::INTERFACE) {
            font::INTERFACE
        } else {
            font::SYSTEM_FALLBACK
        };
        cx.set_global(Self {
            choice,
            preferred_light: PreferredLight::default(),
            os: os_appearance(cx.window_appearance()),
            interface_font: interface_font.into(),
        });
        Self::apply(cx);
    }

    /// The theme in effect.
    pub fn theme(cx: &App) -> &'static Theme {
        cx.global::<Self>().theme_id().theme()
    }

    /// The interface font family: Inter when installed, else the system font.
    pub fn interface_font(cx: &App) -> SharedString {
        cx.global::<Self>().interface_font.clone()
    }

    pub fn choice(cx: &App) -> ThemeChoice {
        cx.global::<Self>().choice
    }

    pub fn theme_id(&self) -> ThemeId {
        self.choice.resolve(self.os, self.preferred_light)
    }

    pub fn set_choice(cx: &mut App, choice: ThemeChoice) {
        cx.global_mut::<Self>().choice = choice;
        Self::apply(cx);
    }

    /// Records a change of the operating system's appearance.
    pub fn set_os_appearance(cx: &mut App, appearance: WindowAppearance) {
        let os = os_appearance(appearance);
        if cx.global::<Self>().os != os {
            cx.global_mut::<Self>().os = os;
            Self::apply(cx);
        }
    }

    /// Pushes the theme in effect into GPUI Component's theme, so that its
    /// components and the window root draw with our tokens, then redraws.
    fn apply(cx: &mut App) {
        let theme = Self::theme(cx);
        let interface_font = Self::interface_font(cx);
        let mode = match theme.scheme {
            Scheme::Light => ThemeMode::Light,
            Scheme::Dark => ThemeMode::Dark,
        };
        if ComponentTheme::global(cx).mode != mode {
            ComponentTheme::change(mode, None, cx);
        }
        ComponentTheme::update(cx, |component| {
            component.font_family = interface_font;
            component.font_size = px(text::UI.size);
            component.radius = px(radius::CONTROL);
            component.radius_lg = px(radius::POPOVER);
            component.window_radius = px(window_radius());
            map_colors(theme, component);
        });
    }
}

fn window_radius() -> f32 {
    if cfg!(target_os = "macos") {
        radius::window::MACOS
    } else if cfg!(target_os = "windows") {
        radius::window::WINDOWS
    } else {
        radius::window::LINUX
    }
}

/// The GPUI Component roles that have a counterpart in our tokens. Roles left
/// out keep GPUI Component's value for the same light or dark mode.
fn map_colors(theme: &Theme, component: &mut ComponentTheme) {
    let c = &mut component.colors;
    c.background = theme.surface.page.into();
    c.foreground = theme.text.primary.into();
    c.border = theme.border.default.into();
    c.input = theme.border.strong.into();
    c.ring = theme.accent.default.into();
    c.caret = theme.accent.default.into();
    c.link = theme.accent.default.into();
    c.selection = theme.selection.text.into();
    c.muted = theme.surface.sunken.into();
    c.muted_foreground = theme.text.secondary.into();
    c.accent = theme.state.hover.into();
    c.accent_foreground = theme.text.primary.into();
    c.primary = theme.accent.default.into();
    c.primary_foreground = theme.accent.on.into();
    c.secondary = theme.surface.raised.into();
    c.secondary_foreground = theme.text.primary.into();
    c.secondary_hover = theme.state.hover.into();
    c.secondary_active = theme.state.press.into();
    c.popover = theme.surface.raised.into();
    c.popover_foreground = theme.text.primary.into();
    c.danger = theme.status.error.into();
    c.warning = theme.status.warning.into();
    c.success = theme.status.success.into();
    c.info = theme.status.info.into();
    c.list_hover = theme.state.hover.into();
    c.list_active = theme.accent.tint.into();
    c.scrollbar_thumb = theme.border.strong.into();
    c.title_bar = theme.surface.window.into();
    c.title_bar_border = theme.border.default.into();
    c.status_bar = theme.surface.window.into();
    c.status_bar_border = theme.border.default.into();
    c.sidebar = theme.surface.panel.into();
    c.sidebar_border = theme.border.default.into();
    c.sidebar_foreground = theme.text.primary.into();
    c.sidebar_accent = theme.accent.tint.into();
    c.sidebar_accent_foreground = theme.text.primary.into();
    c.window_border = theme.border.strong.into();
}

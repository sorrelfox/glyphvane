// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The GPUI side of theming: the theme in effect, its colours as GPUI colours,
//! and GPUI Component's own theme kept in step with it.

use gpui::component::{Theme as ComponentTheme, ThemeMode};
use gpui::{App, Fill, Global, Hsla, Rgba, SharedString, WindowAppearance, px};

use crate::chrome::WindowChrome;
use crate::metrics::{font, radius, text};
use crate::settings::AppSettings;
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
    /// The interface font family in effect (installed, or the system's).
    interface_font: SharedString,
    /// The interface font setting it came from, to skip work when an
    /// unrelated setting changes.
    interface_font_setting: String,
    /// The interface font size in pixels.
    interface_font_size: f32,
}

impl Global for Appearance {}

impl Appearance {
    /// Installs the theme state and applies it. Call once at startup, after
    /// GPUI Component is initialised. The interface font and size come from
    /// the [`AppSettings`] global when it is installed, else their defaults.
    pub fn init(cx: &mut App, choice: ThemeChoice, preferred_light: PreferredLight) {
        let (family, size) = interface_settings(cx);
        let interface_font = resolve_interface_font(cx, &family);
        text::set_interface_scale(text::scale_for_size(size));
        cx.set_global(Self {
            choice,
            preferred_light,
            os: os_appearance(cx.window_appearance()),
            interface_font,
            interface_font_setting: family,
            interface_font_size: size,
        });
        Self::apply(cx);
    }

    /// The theme in effect.
    pub fn theme(cx: &App) -> &'static Theme {
        cx.global::<Self>().theme_id().theme()
    }

    /// The interface font family: the one the settings name when it is
    /// installed, else the system's interface font.
    pub fn interface_font(cx: &App) -> SharedString {
        cx.global::<Self>().interface_font.clone()
    }

    /// The interface font size in pixels, as the settings give it.
    pub fn interface_font_size(cx: &App) -> f32 {
        cx.global::<Self>().interface_font_size
    }

    /// Sets the interface font family (a family name, or
    /// [`font::SYSTEM_SETTING`]) and size in pixels. The interface sizes of
    /// [`text`] follow the size at once, and every window redraws when
    /// either changed. Returns whether anything changed.
    pub fn set_interface(cx: &mut App, family: &str, size: f32) -> bool {
        let size = if size.is_finite() {
            size
        } else {
            text::base::UI.size
        };
        let this = cx.global::<Self>();
        let family_changed = this.interface_font_setting != family;
        let size_changed = this.interface_font_size.to_bits() != size.to_bits();
        if !family_changed && !size_changed {
            return false;
        }
        if family_changed {
            let resolved = resolve_interface_font(cx, family);
            let this = cx.global_mut::<Self>();
            this.interface_font = resolved;
            this.interface_font_setting = family.to_owned();
        }
        cx.global_mut::<Self>().interface_font_size = size;
        text::set_interface_scale(text::scale_for_size(size));
        Self::apply(cx);
        cx.refresh_windows();
        true
    }

    /// The theme choice in effect (from settings or the command line).
    pub fn choice(cx: &App) -> ThemeChoice {
        cx.global::<Self>().choice
    }

    /// The light theme that [`ThemeChoice::System`] uses in light appearance.
    pub fn preferred_light(cx: &App) -> PreferredLight {
        cx.global::<Self>().preferred_light
    }

    /// The theme in effect, as an id.
    pub fn current_id(cx: &App) -> ThemeId {
        cx.global::<Self>().theme_id()
    }

    pub fn theme_id(&self) -> ThemeId {
        self.choice.resolve(self.os, self.preferred_light)
    }

    /// Sets the theme choice and the preferred light theme, and redraws every
    /// window when the theme in effect changes.
    pub fn set_preference(cx: &mut App, choice: ThemeChoice, preferred_light: PreferredLight) {
        let this = cx.global::<Self>();
        if (this.choice, this.preferred_light) == (choice, preferred_light) {
            return;
        }
        let before = this.theme_id();
        let this = cx.global_mut::<Self>();
        this.choice = choice;
        this.preferred_light = preferred_light;
        if this.theme_id() != before {
            Self::apply(cx);
        }
    }

    /// Records a change of the operating system's appearance. With the
    /// System choice this switches between Dark and the preferred light
    /// theme.
    pub fn set_os_appearance(cx: &mut App, appearance: WindowAppearance) {
        let os = os_appearance(appearance);
        let this = cx.global::<Self>();
        if this.os == os {
            return;
        }
        let before = this.theme_id();
        cx.global_mut::<Self>().os = os;
        if cx.global::<Self>().theme_id() != before {
            Self::apply(cx);
        }
    }

    /// Pushes the theme in effect into GPUI Component's theme, so that its
    /// components and the window root draw with our tokens. GPUI Component
    /// refreshes every window after the update, so open windows redraw at
    /// once.
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
            component.window_radius = px(WindowChrome::current().window_radius());
            map_colors(theme, component);
        });
    }
}

/// The interface font family and size the settings give, or their
/// defaults without settings.
fn interface_settings(cx: &App) -> (String, f32) {
    let defaults;
    let appearance = match cx.try_global::<AppSettings>() {
        Some(settings) => &settings.settings().appearance,
        None => {
            defaults = gv_settings::Appearance::default();
            &defaults
        }
    };
    (
        appearance.interface_font.clone(),
        appearance.interface_font_size,
    )
}

/// The family GPUI draws the interface with for the setting `family`: that
/// family when installed, else the system's interface font (also for
/// [`font::SYSTEM_SETTING`] and an empty name).
fn resolve_interface_font(cx: &App, family: &str) -> SharedString {
    let family = family.trim();
    if family.is_empty() || family == font::SYSTEM_SETTING {
        return font::SYSTEM_FALLBACK.into();
    }
    let installed = cx.text_system().all_font_names();
    if installed.iter().any(|name| name == family) {
        SharedString::from(family.to_owned())
    } else {
        font::SYSTEM_FALLBACK.into()
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

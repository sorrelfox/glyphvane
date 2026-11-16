// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Theme model: the colour roles of the three built-in themes and the rule that
//! picks one.
//!
//! This module is plain Rust with no GPUI types, so it can be tested and reused
//! without a window. [`crate::appearance`] turns these colours into GPUI colours.

use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

/// One of the built-in themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeId {
    Light,
    SemiLight,
    Dark,
}

impl ThemeId {
    /// Every built-in theme, in the order the theme picker shows them.
    pub const ALL: [Self; 3] = [Self::Light, Self::SemiLight, Self::Dark];

    /// The parsed theme. The built-in theme files are checked by the tests, so
    /// a parse failure here is a build defect, not a runtime condition.
    pub fn theme(self) -> &'static Theme {
        static THEMES: OnceLock<[Theme; 3]> = OnceLock::new();
        let themes = THEMES.get_or_init(|| {
            ThemeId::ALL.map(|id| {
                Theme::from_toml(id.source()).expect("built-in theme files are valid TOML")
            })
        });
        &themes[self as usize]
    }

    fn source(self) -> &'static str {
        match self {
            Self::Light => include_str!("../../../assets/themes/light.toml"),
            Self::SemiLight => include_str!("../../../assets/themes/semi-light.toml"),
            Self::Dark => include_str!("../../../assets/themes/dark.toml"),
        }
    }
}

/// The light theme to use when the choice is [`ThemeChoice::System`] and the
/// operating system is in light appearance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PreferredLight {
    #[default]
    Light,
    SemiLight,
}

/// The operating system's appearance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OsAppearance {
    #[default]
    Light,
    Dark,
}

/// The user's theme setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    SemiLight,
    Dark,
}

impl ThemeChoice {
    /// Every choice, in the order the theme picker shows them.
    pub const ALL: [Self; 4] = [Self::System, Self::Light, Self::SemiLight, Self::Dark];

    /// The theme to show. `System` follows the operating system: Dark in dark
    /// appearance, the preferred light theme otherwise.
    pub fn resolve(self, os: OsAppearance, preferred_light: PreferredLight) -> ThemeId {
        match self {
            Self::System => match (os, preferred_light) {
                (OsAppearance::Dark, _) => ThemeId::Dark,
                (OsAppearance::Light, PreferredLight::Light) => ThemeId::Light,
                (OsAppearance::Light, PreferredLight::SemiLight) => ThemeId::SemiLight,
            },
            Self::Light => ThemeId::Light,
            Self::SemiLight => ThemeId::SemiLight,
            Self::Dark => ThemeId::Dark,
        }
    }

    /// The next choice in picker order, wrapping around.
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|c| *c == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// Whether theme-aware content (images, diagrams, native controls) should get
/// its light or its dark variant. Semi-light counts as light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scheme {
    Light,
    Dark,
}

/// A colour in sRGB with straight (not premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Parses `#RRGGBB` or `#RRGGBBAA`.
    pub fn from_hex(text: &str) -> Result<Self, ColorError> {
        let digits = text.strip_prefix('#').ok_or(ColorError(()))?;
        if !matches!(digits.len(), 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ColorError(()));
        }
        let channel = |i: usize| {
            digits
                .get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or(ColorError(()))
        };
        Ok(Self {
            r: channel(0)?,
            g: channel(2)?,
            b: channel(4)?,
            a: if digits.len() == 8 { channel(6)? } else { 0xFF },
        })
    }

    /// Alpha as a fraction from 0 to 1.
    pub fn alpha(self) -> f32 {
        f32::from(self.a) / 255.0
    }

    /// WCAG 2 relative luminance of the opaque colour.
    pub fn relative_luminance(self) -> f64 {
        fn linear(channel: u8) -> f64 {
            let c = f64::from(channel) / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// WCAG 2 contrast ratio between two opaque colours, from 1 to 21.
    pub fn contrast_ratio(self, other: Self) -> f64 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        let (light, dark) = if a > b { (a, b) } else { (b, a) };
        (light + 0.05) / (dark + 0.05)
    }
}

impl TryFrom<String> for Color {
    type Error = ColorError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::from_hex(&text)
    }
}

/// A colour string that is not `#RRGGBB` or `#RRGGBBAA`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorError(());

impl fmt::Display for ColorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected a colour as #RRGGBB or #RRGGBBAA")
    }
}

impl std::error::Error for ColorError {}

/// A theme: every colour role of the design. Each role has one purpose; use it
/// only for that purpose.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub scheme: Scheme,
    pub surface: Surface,
    pub state: State,
    pub border: Border,
    pub text: Text,
    pub accent: Accent,
    pub status: Status,
    pub selection: Selection,
    pub syntax: Syntax,
    pub shadow: Shadows,
}

impl Theme {
    /// Parses a theme file.
    pub fn from_toml(source: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(source)
    }
}

/// Backgrounds, from the darkest (`window`) to the lightest (`page`) in light
/// themes; dark themes keep the same order of roles.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Surface {
    /// Title row, status bar, settings header.
    pub window: Color,
    /// Sidebars and side panels.
    pub panel: Color,
    /// Document and editor, active tab, active segment.
    pub page: Color,
    /// Popovers, menus, bordered buttons and fields.
    pub raised: Color,
    /// Code blocks, table headers, keycaps, inline code, neutral infobar.
    pub sunken: Color,
}

/// Translucent overlays drawn on top of whatever surface is beneath.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    /// Hover, and the current editor line.
    pub hover: Color,
    /// Pressed controls, and the round window buttons on Linux.
    pub press: Color,
    /// Hover fill of the Windows caption close button, and nothing else:
    /// Windows' red. macOS and Linux keep the neutral hover.
    pub close_hover: Color,
    /// The close glyph on [`State::close_hover`].
    pub close_hover_fg: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Border {
    /// Hairlines: panel edges, row separators, table rules.
    pub default: Color,
    /// Control borders, segmented control frame, popover border, scrollbar thumb.
    pub strong: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Text {
    /// Body text, active labels.
    pub primary: Color,
    /// Inactive tabs and icons, descriptions, counts, status bar.
    pub secondary: Color,
    /// Line numbers, Markdown markers, disabled icons. Never on `accent.tint`
    /// or `selection.text`.
    pub tertiary: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accent {
    /// Links, caret, focus border, primary button fill.
    pub default: Color,
    /// Selected rows, tags, profile chip, match highlight, focus halo.
    pub tint: Color,
    /// Underline of resolved links.
    pub line: Color,
    /// Text and icons on an `accent.default` fill.
    pub on: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub error: Color,
    pub error_tint: Color,
    pub warning: Color,
    pub warning_tint: Color,
    pub success: Color,
    pub success_tint: Color,
    pub info: Color,
    pub info_tint: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    /// Selected text in the editor and in preview.
    pub text: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Syntax {
    pub key: Color,
    pub string: Color,
    pub number: Color,
    pub tag: Color,
    pub url: Color,
    pub code: Color,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shadows {
    /// Popovers, menus, switcher.
    pub popover: Vec<ShadowLayer>,
    /// The active tab's lift.
    pub tab: Vec<ShadowLayer>,
}

/// One layer of a box shadow, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowLayer {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_themes_parse_with_every_role() {
        // `deny_unknown_fields` and the absence of defaults make a missing or
        // misspelt role a parse error, so parsing is the completeness check.
        for id in ThemeId::ALL {
            let theme = Theme::from_toml(id.source())
                .unwrap_or_else(|err| panic!("{id:?} does not parse: {err}"));
            assert_eq!(
                theme.shadow.popover.len(),
                2,
                "{id:?} popover shadow layers"
            );
            assert!(
                theme.state.hover.a < 0xFF,
                "{id:?} hover must be an overlay"
            );
            assert!(
                theme.state.press.a < 0xFF,
                "{id:?} press must be an overlay"
            );
        }
    }

    #[test]
    fn every_theme_has_the_tab_shadow_and_close_hover() {
        for id in ThemeId::ALL {
            let theme = id.theme();
            // The design's value: 0 1 2 at 6 % black.
            let expected = ShadowLayer {
                x: 0.0,
                y: 1.0,
                blur: 2.0,
                spread: 0.0,
                color: Color::from_hex("#0000000F").expect("valid"),
            };
            assert_eq!(theme.shadow.tab, [expected], "{id:?} shadow.tab");
            let state = theme.state;
            assert_eq!(state.close_hover.a, 0xFF, "{id:?} close hover is opaque");
            let ratio = state.close_hover_fg.contrast_ratio(state.close_hover);
            assert!(ratio >= 4.5, "{id:?} close glyph contrast {ratio:.2}:1");
        }
        // Windows 11's red with a white glyph in the designed themes.
        for id in [ThemeId::Light, ThemeId::Dark] {
            assert_eq!(
                id.theme().state.close_hover,
                Color::from_hex("#C42B1C").expect("valid")
            );
            assert_eq!(
                id.theme().state.close_hover_fg,
                Color::from_hex("#FFFFFF").expect("valid")
            );
        }
    }

    #[test]
    fn theme_ids_match_files() {
        let expected = [
            ("light", Scheme::Light),
            ("semi-light", Scheme::Light),
            ("dark", Scheme::Dark),
        ];
        for (id, (name, scheme)) in ThemeId::ALL.into_iter().zip(expected) {
            assert_eq!(id.theme().id, name);
            assert_eq!(id.theme().scheme, scheme);
        }
    }

    #[test]
    fn system_choice_follows_the_os() {
        use OsAppearance as Os;
        use PreferredLight as Pref;
        let system = ThemeChoice::System;
        assert_eq!(system.resolve(Os::Dark, Pref::Light), ThemeId::Dark);
        assert_eq!(system.resolve(Os::Dark, Pref::SemiLight), ThemeId::Dark);
        assert_eq!(system.resolve(Os::Light, Pref::Light), ThemeId::Light);
        assert_eq!(
            system.resolve(Os::Light, Pref::SemiLight),
            ThemeId::SemiLight
        );
        assert_eq!(PreferredLight::default(), PreferredLight::Light);
    }

    #[test]
    fn explicit_choice_ignores_the_os() {
        for os in [OsAppearance::Light, OsAppearance::Dark] {
            for pref in [PreferredLight::Light, PreferredLight::SemiLight] {
                assert_eq!(ThemeChoice::Light.resolve(os, pref), ThemeId::Light);
                assert_eq!(ThemeChoice::SemiLight.resolve(os, pref), ThemeId::SemiLight);
                assert_eq!(ThemeChoice::Dark.resolve(os, pref), ThemeId::Dark);
            }
        }
    }

    #[test]
    fn choices_cycle_in_picker_order() {
        let mut choice = ThemeChoice::System;
        let mut seen = Vec::new();
        for _ in 0..ThemeChoice::ALL.len() {
            choice = choice.next();
            seen.push(choice);
        }
        assert_eq!(
            seen,
            [
                ThemeChoice::Light,
                ThemeChoice::SemiLight,
                ThemeChoice::Dark,
                ThemeChoice::System
            ]
        );
    }

    #[test]
    fn body_text_keeps_aa_contrast_on_every_surface() {
        const MIN: f64 = 4.5;
        for id in ThemeId::ALL {
            let theme = id.theme();
            let s = &theme.surface;
            let surfaces = [
                ("page", s.page),
                ("panel", s.panel),
                ("window", s.window),
                ("raised", s.raised),
                ("sunken", s.sunken),
            ];
            let texts = [
                ("primary", theme.text.primary),
                ("secondary", theme.text.secondary),
            ];
            for (text_name, text) in texts {
                for (surface_name, surface) in surfaces {
                    let ratio = text.contrast_ratio(surface);
                    assert!(
                        ratio >= MIN,
                        "{id:?}: text.{text_name} on surface.{surface_name} is {ratio:.2}:1, below {MIN}:1"
                    );
                }
            }
        }
    }

    #[test]
    fn contrast_matches_known_values() {
        let black = Color::from_hex("#000000").expect("valid");
        let white = Color::from_hex("#FFFFFF").expect("valid");
        assert!((black.contrast_ratio(white) - 21.0).abs() < 1e-9);
        assert!((white.contrast_ratio(white) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn colours_parse_with_and_without_alpha() {
        assert_eq!(
            Color::from_hex("#1018200E"),
            Ok(Color {
                r: 0x10,
                g: 0x18,
                b: 0x20,
                a: 0x0E
            })
        );
        assert_eq!(Color::from_hex("#EBEEF0").map(|c| c.a), Ok(0xFF));
        for bad in [
            "EBEEF0", "#EBEEF", "#EBEEF0F", "#GGGGGG", "#ÉBEEF0", "#+1+1+1",
        ] {
            assert!(Color::from_hex(bad).is_err(), "{bad} should not parse");
        }
    }
}

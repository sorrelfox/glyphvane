// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the Appearance settings change in the Reading view: the zoom
//! ([`crate::zoom`]), the preview font and size, the editor font (code
//! blocks and inline code use it, so code reads the same in both views),
//! and the readable line width.
//!
//! - **Scale**: type and spacing are multiplied by the zoom and by the
//!   preview font size over the designed body size (15 px). The Split
//!   view's preview keeps its own, smaller scale under the same factor.
//! - **Families**: a family that is not installed falls back as before
//!   (Inter, else the system interface font; JetBrains Mono, else a common
//!   monospace family).
//! - **Readable line width**: in Preview the text column is at most
//!   `readable_line_width_chars` characters of the body font wide (72 by
//!   default, the design's 680 px at 15 px), scaled with the type; off, the
//!   column fills the view between its side paddings.

use gpui::{App, SharedString};

use crate::inline::FontFamilies;
use crate::metrics::size::document;
use crate::metrics::text;
use crate::settings::AppSettings;

/// The designed readable width in characters: 680 px at the 15 px body.
const DESIGN_CHARS: f32 = 72.0;

/// The width of one character of the body text, in em: what makes 72
/// characters the design's 680 px column.
const CHAR_EM: f32 = document::COLUMN_MAX / DESIGN_CHARS / text::PREVIEW_BODY.size;

/// The Reading view's typography from the settings.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Typography {
    /// The zoom factor (1.0 at 100 %).
    pub zoom: f32,
    /// The preview body size, in px at 100 %.
    pub body_size: f32,
    /// The preview font family as set.
    pub text_family: SharedString,
    /// The editor font family as set (code in the preview).
    pub mono_family: SharedString,
    /// The readable column in characters; `None` when the setting is off.
    pub readable_chars: Option<u32>,
}

impl Default for Typography {
    fn default() -> Self {
        Self::from_settings(&gv_settings::Appearance::default(), 1.0)
    }
}

impl Typography {
    /// The typography of the settings in effect.
    pub fn from_app(cx: &App) -> Self {
        let zoom = crate::zoom::factor(cx);
        match cx.try_global::<AppSettings>() {
            Some(settings) => Self::from_settings(&settings.settings().appearance, zoom),
            None => Self {
                zoom,
                ..Self::default()
            },
        }
    }

    /// The typography of `appearance` at `zoom`.
    pub fn from_settings(appearance: &gv_settings::Appearance, zoom: f32) -> Self {
        Self {
            zoom,
            body_size: appearance.preview_font_size,
            text_family: appearance.preview_font.clone().into(),
            mono_family: appearance.editor_font.clone().into(),
            readable_chars: appearance
                .readable_line_width
                .then_some(appearance.readable_line_width_chars),
        }
    }

    /// The factor every length of the Reading view is multiplied by.
    pub fn scale(&self) -> f32 {
        let size = if self.body_size.is_finite() && self.body_size > 0.0 {
            self.body_size / text::PREVIEW_BODY.size
        } else {
            1.0
        };
        let zoom = if self.zoom.is_finite() && self.zoom > 0.0 {
            self.zoom
        } else {
            1.0
        };
        size * zoom
    }

    /// The widest the Preview text column gets, in px; infinite when the
    /// readable line width is off.
    pub fn column_max(&self) -> f32 {
        match self.readable_chars {
            Some(chars) => {
                (chars as f32 * CHAR_EM * text::PREVIEW_BODY.size * self.scale()).round()
            }
            None => f32::INFINITY,
        }
    }

    /// The families to use among the `installed` ones: the set family
    /// when it is installed, else the usual fallback.
    pub fn families(&self, installed: &[String]) -> FontFamilies {
        let fallback = FontFamilies::detect(installed);
        let pick = |set: &SharedString, fallback: SharedString| {
            if installed.iter().any(|name| name.as_str() == set.as_ref()) {
                set.clone()
            } else {
                fallback
            }
        };
        FontFamilies {
            text: pick(&self.text_family, fallback.text.clone()),
            mono: pick(&self.mono_family, fallback.mono.clone()),
        }
    }

    /// Whether `other` needs new fonts (the families differ).
    pub fn fonts_differ(&self, other: &Self) -> bool {
        (&self.text_family, &self.mono_family) != (&other.text_family, &other.mono_family)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appearance() -> gv_settings::Appearance {
        gv_settings::Appearance::default()
    }

    #[test]
    fn defaults_are_the_design() {
        let t = Typography::from_settings(&appearance(), 1.0);
        assert_eq!(t.scale(), 1.0);
        assert_eq!(t.column_max(), document::COLUMN_MAX);
        assert_eq!(t, Typography::default());
    }

    #[test]
    fn zoom_and_font_size_multiply() {
        let mut a = appearance();
        a.preview_font_size = 18.0;
        let t = Typography::from_settings(&a, 1.5);
        assert!((t.scale() - 1.8).abs() < 1e-6);
        // The column holds the same 72 characters of the larger type.
        assert_eq!(t.column_max(), (document::COLUMN_MAX * 1.8).round());
        // Nonsense values fall back to no scaling.
        let mut a = appearance();
        a.preview_font_size = 0.0;
        assert_eq!(Typography::from_settings(&a, f32::NAN).scale(), 1.0);
    }

    #[test]
    fn readable_width_follows_the_settings() {
        let mut a = appearance();
        a.readable_line_width_chars = 100;
        let wide = Typography::from_settings(&a, 1.0).column_max();
        assert_eq!(wide, (document::COLUMN_MAX * 100.0 / 72.0).round());
        a.readable_line_width = false;
        assert_eq!(
            Typography::from_settings(&a, 1.0).column_max(),
            f32::INFINITY
        );
    }

    #[test]
    fn families_fall_back_when_not_installed() {
        let mut a = appearance();
        a.preview_font = "Literata".to_owned();
        a.editor_font = "Fira Code".to_owned();
        let t = Typography::from_settings(&a, 1.0);
        let installed: Vec<String> = ["Literata", "Inter", "JetBrains Mono"]
            .map(str::to_owned)
            .to_vec();
        let f = t.families(&installed);
        assert_eq!(f.text.as_ref(), "Literata");
        assert_eq!(f.mono.as_ref(), "JetBrains Mono");
        let f = t.families(&[]);
        assert_eq!(f, FontFamilies::detect(&[]));
        assert!(t.fonts_differ(&Typography::default()));
        assert!(
            !Typography::from_settings(&appearance(), 2.0).fonts_differ(&Typography::default())
        );
    }
}

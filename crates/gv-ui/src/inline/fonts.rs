// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Fonts of inline content and the GPUI measurer.
//!
//! A [`FontTable`] maps the opaque [`StyleId`]s of `gv-inline` to GPUI
//! fonts. [`GpuiMeasure`] shapes runs with GPUI's text system and reports
//! the metrics of each style's primary font.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    App, Font, FontStyle, FontWeight, Hsla, LineLayout, SharedString, TextRun, WindowTextSystem, px,
};
use gv_inline::{FontMetrics, Measure, ShapedRun, StyleId};

use crate::metrics::font;

/// What a shaped run carries for painting: GPUI's shaped line, with glyph
/// ids, positions and the faces that font fallback picked.
pub type Glyphs = Arc<LineLayout>;

/// Monospace families tried, in order, when the editor font is not
/// installed.
const MONO_FALLBACKS: &[&str] = &[
    "SF Mono",
    "Menlo",
    "Cascadia Mono",
    "Consolas",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
];

/// The font families of inline content, resolved against the installed
/// fonts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFamilies {
    /// Preview text: Inter, else the system interface font.
    pub text: SharedString,
    /// Code: JetBrains Mono, else a common monospace family, else the
    /// system interface font.
    pub mono: SharedString,
}

impl FontFamilies {
    /// Picks the families from a list of installed family names.
    pub fn detect(installed: &[String]) -> Self {
        let has = |name: &str| installed.iter().any(|n| n == name);
        let text = if has(font::INTERFACE) {
            font::INTERFACE
        } else {
            font::SYSTEM_FALLBACK
        };
        let mono = std::iter::once(font::EDITOR)
            .chain(MONO_FALLBACKS.iter().copied())
            .find(|name| has(name))
            .unwrap_or(font::SYSTEM_FALLBACK);
        Self {
            text: text.into(),
            mono: mono.into(),
        }
    }

    /// The families for the application's text system.
    pub fn from_app(cx: &App) -> Self {
        Self::detect(&cx.text_system().all_font_names())
    }
}

/// A GPUI font from a family, a weight and a slant.
pub fn font_of(family: impl Into<SharedString>, weight: f32, italic: bool) -> Font {
    let mut font = gpui::font(family);
    font.weight = FontWeight(weight);
    font.style = if italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    font
}

/// The fonts of the styles of inline content, filled by the caller.
///
/// A style that is not in the table shapes in the system interface font.
#[derive(Debug, Clone)]
pub struct FontTable {
    fonts: HashMap<StyleId, Font>,
    default: Font,
}

impl Default for FontTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FontTable {
    pub fn new() -> Self {
        Self {
            fonts: HashMap::new(),
            default: gpui::font(font::SYSTEM_FALLBACK),
        }
    }

    /// Sets the font of `style`.
    pub fn set(&mut self, style: StyleId, font: Font) -> &mut Self {
        self.fonts.insert(style, font);
        self
    }

    /// Builder form of [`Self::set`].
    pub fn with(mut self, style: StyleId, font: Font) -> Self {
        self.set(style, font);
        self
    }

    /// The font of `style`.
    pub fn font(&self, style: StyleId) -> &Font {
        self.fonts.get(&style).unwrap_or(&self.default)
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
}

/// A [`Measure`] over GPUI's text system.
///
/// Each run is shaped once, as a whole, with `layout_line`; the result keeps
/// GPUI's [`LineLayout`] for painting. Metrics come from the style's
/// **primary** font, resolved by GPUI, never from `LineLayout::ascent` and
/// `descent`, which are the maximum over all faces of the run, fallback
/// faces included (one emoji would grow a line).
pub struct GpuiMeasure<'a> {
    text_system: &'a WindowTextSystem,
    fonts: &'a FontTable,
    shaped: u64,
}

impl std::fmt::Debug for GpuiMeasure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuiMeasure")
            .field("fonts", &self.fonts.len())
            .field("shaped", &self.shaped)
            .finish_non_exhaustive()
    }
}

impl<'a> GpuiMeasure<'a> {
    pub fn new(text_system: &'a WindowTextSystem, fonts: &'a FontTable) -> Self {
        Self {
            text_system,
            fonts,
            shaped: 0,
        }
    }

    /// Runs shaped through this measurer.
    pub fn shaped(&self) -> u64 {
        self.shaped
    }
}

impl Measure for GpuiMeasure<'_> {
    type Glyphs = Glyphs;

    fn shape(&mut self, text: &str, style: StyleId, size: f32) -> ShapedRun<Glyphs> {
        self.shaped += 1;
        let run = TextRun {
            len: text.len(),
            font: self.fonts.font(style).clone(),
            // Colour and decorations are painted by the element; the text
            // system only shapes.
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = self.text_system.layout_line(text, px(size), &[run], None);
        let clusters: Vec<(usize, f32)> = layout
            .runs
            .iter()
            .flat_map(|r| r.glyphs.iter())
            .map(|g| (g.index, f32::from(g.position.x)))
            .collect();
        let glyph_bytes = size_of::<LineLayout>()
            + layout
                .runs
                .iter()
                .map(|r| {
                    size_of::<gpui::ShapedRun>()
                        + r.glyphs.capacity() * size_of::<gpui::ShapedGlyph>()
                })
                .sum::<usize>();
        let width = f32::from(layout.width);
        ShapedRun::new(text, width, clusters, layout, glyph_bytes)
    }

    fn metrics(&mut self, style: StyleId, size: f32) -> FontMetrics {
        let id = self.text_system.resolve_font(self.fonts.font(style));
        let size = px(size);
        FontMetrics {
            ascent: f32::from(self.text_system.ascent(id, size)),
            descent: f32::from(self.text_system.descent(id, size)),
            // GPUI's `TextSystem` does not expose the line gap; it only
            // matters for `LineHeight::Normal`, which the preview does not
            // use.
            line_gap: 0.0,
            x_height: f32::from(self.text_system.x_height(id, size)),
        }
        .sanitized()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_prefer_the_design_fonts_and_fall_back() {
        let all = |names: &[&str]| names.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let both = FontFamilies::detect(&all(&["Inter", "JetBrains Mono", "Menlo"]));
        assert_eq!(
            (both.text.as_ref(), both.mono.as_ref()),
            ("Inter", "JetBrains Mono")
        );
        let some = FontFamilies::detect(&all(&["DejaVu Sans Mono", "Noto Sans"]));
        assert_eq!(
            (some.text.as_ref(), some.mono.as_ref()),
            (font::SYSTEM_FALLBACK, "DejaVu Sans Mono")
        );
        let none = FontFamilies::detect(&[]);
        assert_eq!(none.mono.as_ref(), font::SYSTEM_FALLBACK);
    }

    #[test]
    fn unknown_styles_use_the_system_font() {
        let table = FontTable::new().with(StyleId(3), font_of("Inter", 700.0, true));
        assert_eq!(table.font(StyleId(3)).weight, FontWeight(700.0));
        assert_eq!(table.font(StyleId(3)).style, FontStyle::Italic);
        assert_eq!(
            table.font(StyleId(9)).family.as_ref(),
            font::SYSTEM_FALLBACK
        );
        assert_eq!(table.len(), 1);
    }
}

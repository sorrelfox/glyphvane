// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Text measurement for soft wrap.
//!
//! The wrap layer needs advance widths but must not depend on a GPU or a
//! font system, so the editor view injects a [`TextMeasure`] backed by its
//! text system, and tests use [`MonospaceMeasure`].

/// Measures the advance widths of characters in the editor font, in logical
/// pixels.
///
/// Widths are per character, as in Zed's line wrapper: no shaping, so
/// ligatures and combining marks are approximations. Implementations may
/// cache, hence `&mut self`.
pub trait TextMeasure {
    /// The advance width of `c`.
    fn char_width(&mut self, c: char) -> f32;

    /// The advance width of `text`: the sum of its characters' widths.
    fn text_width(&mut self, text: &str) -> f32 {
        text.chars().map(|c| self.char_width(c)).sum()
    }
}

/// A [`TextMeasure`] in which every character has the same width. For
/// tests: it does not treat wide (East Asian) characters as two cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonospaceMeasure {
    advance: f32,
}

impl MonospaceMeasure {
    /// Every character is `advance` pixels wide.
    pub const fn new(advance: f32) -> Self {
        Self { advance }
    }

    /// The width of every character.
    pub const fn advance(&self) -> f32 {
        self.advance
    }
}

impl TextMeasure for MonospaceMeasure {
    fn char_width(&mut self, _c: char) -> f32 {
        self.advance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width_of(measure: &mut dyn TextMeasure, text: &str) -> f32 {
        measure.text_width(text)
    }

    #[test]
    fn monospace_widths() {
        let mut measure = MonospaceMeasure::new(7.5);
        assert_eq!(measure.advance(), 7.5);
        assert_eq!(measure.char_width('a'), 7.5);
        assert_eq!(measure.char_width('語'), 7.5);
        assert_eq!(width_of(&mut measure, ""), 0.0);
        // Characters, not bytes: `é` is two bytes and `🍐` four.
        assert_eq!(width_of(&mut measure, "aé 🍐"), 30.0);
    }
}

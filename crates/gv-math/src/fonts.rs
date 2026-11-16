// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Glyph outlines from the bundled KaTeX fonts.
//!
//! The fonts are parsed once per process, and every outline is converted
//! once to a [`MathPath`] in em units with y pointing down, shared between
//! formulas through an [`Arc`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

use crate::path::{MathPath, PathBuilder};

/// The KaTeX faces RaTeX names in its display list, with their files.
const FACES: [(&str, &str); 19] = [
    ("AMS-Regular", "KaTeX_AMS-Regular.ttf"),
    ("Caligraphic-Regular", "KaTeX_Caligraphic-Regular.ttf"),
    ("Fraktur-Regular", "KaTeX_Fraktur-Regular.ttf"),
    ("Fraktur-Bold", "KaTeX_Fraktur-Bold.ttf"),
    ("Main-Bold", "KaTeX_Main-Bold.ttf"),
    ("Main-BoldItalic", "KaTeX_Main-BoldItalic.ttf"),
    ("Main-Italic", "KaTeX_Main-Italic.ttf"),
    ("Main-Regular", "KaTeX_Main-Regular.ttf"),
    ("Math-BoldItalic", "KaTeX_Math-BoldItalic.ttf"),
    ("Math-Italic", "KaTeX_Math-Italic.ttf"),
    ("SansSerif-Bold", "KaTeX_SansSerif-Bold.ttf"),
    ("SansSerif-Italic", "KaTeX_SansSerif-Italic.ttf"),
    ("SansSerif-Regular", "KaTeX_SansSerif-Regular.ttf"),
    ("Script-Regular", "KaTeX_Script-Regular.ttf"),
    ("Size1-Regular", "KaTeX_Size1-Regular.ttf"),
    ("Size2-Regular", "KaTeX_Size2-Regular.ttf"),
    ("Size3-Regular", "KaTeX_Size3-Regular.ttf"),
    ("Size4-Regular", "KaTeX_Size4-Regular.ttf"),
    ("Typewriter-Regular", "KaTeX_Typewriter-Regular.ttf"),
];

/// The index of the face RaTeX calls `name`, if it is a KaTeX face.
pub(crate) fn face_index(name: &str) -> Option<usize> {
    FACES.iter().position(|(n, _)| *n == name)
}

/// The bytes of every face (`None` if one is missing from the bundle).
fn font_bytes() -> &'static [Option<Cow<'static, [u8]>>] {
    static BYTES: OnceLock<Vec<Option<Cow<'static, [u8]>>>> = OnceLock::new();
    BYTES.get_or_init(|| {
        FACES
            .iter()
            .map(|(_, file)| ratex_katex_fonts::ttf_bytes(file))
            .collect()
    })
}

/// An outline in em units (y down, origin on the baseline), or `None` when
/// the face has no glyph for the character. Empty outlines (spaces) are
/// `Some` with no elements.
type Outline = Option<Arc<MathPath>>;

fn outlines() -> &'static Mutex<HashMap<(usize, u32), Outline>> {
    static CACHE: OnceLock<Mutex<HashMap<(usize, u32), Outline>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The outline of `ch` in face `face`, in em units with y down.
pub(crate) fn glyph(face: usize, ch: char) -> Outline {
    let key = (face, u32::from(ch));
    if let Some(hit) = outlines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return hit.clone();
    }
    let outline = load(face, ch);
    // Misses are not kept: the map stays bounded by the fonts' glyphs.
    if outline.is_some() {
        outlines()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, outline.clone());
    }
    outline
}

fn load(face: usize, ch: char) -> Outline {
    let bytes = font_bytes().get(face)?.as_deref()?;
    let font = FontRef::new(bytes).ok()?;
    let id = font.charmap().map(ch)?;
    let glyph = font.outline_glyphs().get(id)?;
    let upem = font
        .metrics(Size::unscaled(), LocationRef::default())
        .units_per_em;
    let mut sink = Sink {
        path: PathBuilder::default(),
        scale: 1.0 / f32::from(upem.max(1)),
    };
    // Unscaled: font units, exact (a scaled draw rounds to 1/64 px). A glyph
    // that fails to draw keeps what it drew (usually nothing).
    let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
    let _ = glyph.draw(settings, &mut sink);
    Some(Arc::new(sink.path.finish()))
}

/// Whether some KaTeX face has a glyph for `ch`.
pub(crate) fn covered(ch: char) -> bool {
    font_bytes().iter().flatten().any(|bytes| {
        FontRef::new(bytes)
            .ok()
            .and_then(|f| f.charmap().map(ch))
            .is_some()
    })
}

/// Scales font units to em and flips y (fonts point up, layouts down).
struct Sink {
    path: PathBuilder,
    scale: f32,
}

impl OutlinePen for Sink {
    fn move_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.path.move_to(x * s, -y * s);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.path.line_to(x * s, -y * s);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let s = self.scale;
        self.path.quad_to(cx0 * s, -cy0 * s, x * s, -y * s);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let s = self.scale;
        self.path
            .cubic_to(cx0 * s, -cy0 * s, cx1 * s, -cy1 * s, x * s, -y * s);
    }

    fn close(&mut self) {
        self.path.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_face_is_bundled_and_parses() {
        for (i, (name, _)) in FACES.iter().enumerate() {
            let bytes = font_bytes()[i].as_deref().expect(name);
            assert!(FontRef::new(bytes).is_ok(), "{name}");
        }
    }

    #[test]
    fn outlines_are_in_em_with_y_down() {
        let face = face_index("Main-Regular").expect("face");
        let x = glyph(face, 'x').expect("glyph");
        let b = x.bounds().expect("ink");
        // An x sits on the baseline and is about half an em tall.
        assert!(b.max_y.abs() < 0.02, "{b:?}");
        assert!((-0.5..-0.4).contains(&b.min_y), "{b:?}");
        assert!(glyph(face, '中').is_none());
        assert!(covered('x') && !covered('中'));
    }
}

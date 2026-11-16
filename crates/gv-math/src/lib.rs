// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! LaTeX to display list or SVG, with caches and the macro pass.
//!
//! # Pipeline
//!
//! 1. [`MacroPass`] walks a document's formulas in order and gives each the
//!    [`MacroSet`] defined before it (`\newcommand`, `\def`, …; MathJax's
//!    document-wide scope).
//! 2. [`MathEngine::render`] checks our caps ([`Limits`]: source length,
//!    tighter for mhchem; drawing items; extent), parses and lays out the
//!    formula with RaTeX in display or text style, and converts RaTeX's
//!    display list into a [`MathLayout`]: filled and stroked [`MathPath`]s
//!    in em units (glyph outlines from the bundled KaTeX fonts, shared
//!    between formulas), plus [`MathItem::Text`] for characters the KaTeX
//!    fonts lack, which the host draws with its own fonts. RaTeX panics are
//!    caught.
//! 3. Results, errors included, are cached by (source, macro set, style).
//!    Layouts carry no size or theme: the surrounding text colour is
//!    [`MathColor::Current`], and the painter scales em to pixels.
//!
//! Errors ([`MathError`]) carry a short message for the Reading view, which
//! shows the source in the error style below it.

mod engine;
mod error;
mod fonts;
mod layout;
mod macros;
mod path;

pub use engine::{
    CacheStats, FallbackMetrics, GlyphMetrics, Limits, MathEngine, MathResult, typeset,
};
pub use error::MathError;
pub use layout::{MathColor, MathItem, MathLayout};
pub use macros::{
    DefKind, Definition, MacroLimits, MacroPass, MacroSet, definitions, document_macros,
};
pub use path::{Bounds, MathPath, PathBuilder, PathEl};

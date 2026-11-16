// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Mermaid diagrams to a theme-agnostic display list, with caches, caps and
//! fallback cards.
//!
//! merman (without ELK) parses, lays out and writes an SVG; usvg turns that
//! SVG into filled outlines (text in the measuring font, strokes and dashes
//! flattened). Colours come out as [`Role`]s, so one cached [`Diagram`]
//! paints in every theme: the host resolves roles to its theme tokens
//! ([`Palette`]). [`Diagram::to_svg`] writes a themed SVG for export.
//!
//! ```no_run
//! # use gv_diagram::*;
//! # fn fonts() -> DiagramFonts { unimplemented!() }
//! let engine = DiagramEngine::new(fonts(), Limits::default())?;
//! // UI thread: look up the cache, else start a job.
//! let key = DiagramKey::new("graph TD; A-->B", Trust::Interactive);
//! if engine.cached(&key).is_none() {
//!     let job = engine.job("graph TD; A-->B", Trust::Interactive);
//!     let _cancel = job.canceller(); // keep it; cancel when the block changes
//!     // Background executor:
//!     match job.run(&engine) {
//!         Outcome::Rendered(diagram) => { /* paint diagram.items */ }
//!         Outcome::Card(card) => { /* card.title(), card.message */ }
//!         Outcome::NotRecognised => { /* source + NOT_RECOGNISED_NOTE */ }
//!         Outcome::Cancelled => {}
//!     }
//! }
//! # Ok::<(), SetupError>(())
//! ```

mod card;
mod convert;
mod display;
mod engine;
mod palette;

pub use card::{CardKind, FallbackCard};
pub use display::{Diagram, DiagramItem, DiagramPath, FillRule, Paint, PathEl, Rect};
pub use engine::{
    CacheStats, Canceller, DiagramEngine, DiagramFonts, DiagramKey, Job, Limits,
    NOT_RECOGNISED_NOTE, Outcome, SetupError, Trust, TrustLimits,
};
pub use palette::{CATEGORY_COUNT, Palette, Role};

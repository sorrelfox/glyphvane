// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Inline formatting context: the pure layout of a paragraph of styled text
//! runs and atomic boxes. No GUI types; the text system is reached through
//! the [`Measure`] trait, and tests use the deterministic [`FakeMono`].
//!
//! # Pipeline
//!
//! 1. [`ParagraphBuilder`] → [`Paragraph`]: items (text runs with a
//!    [`RunStyle`], [`InlineBox`]es, hard breaks), each with its source
//!    range; the logical text and its UAX #14 break opportunities. Built
//!    when the content changes.
//! 2. [`Shaped`]: every text run shaped once through a [`RunCache`],
//!    letter spacing applied, metrics and box alignment resolved. Built
//!    when the content or the fonts change.
//! 3. [`Layout`]: lines and fragments at one width. Built from the shaping
//!    without reshaping; [`BlockCache`] keeps the last few widths.
//!
//! [`Layout`] answers the geometry queries: [`Layout::hit`] (point →
//! offset, affinity, item and source offset), [`Layout::caret`],
//! [`Layout::selection_rects`], [`Layout::decorations`],
//! [`Layout::item_at_point`]. [`Shaped`] gives the caret stops
//! ([`Shaped::next_stop`], [`Shaped::prev_stop`], [`Shaped::snap`]).
//!
//! # Coordinates
//!
//! - **Logical offsets**: bytes in [`Paragraph::text`], where a box is
//!   U+FFFC and a hard break `\n`. Caret stops are logical offsets at
//!   grapheme boundaries that start a glyph cluster, item boundaries, and
//!   both sides of each box and hard break.
//! - **Source offsets**: bytes in the document. [`Paragraph::source_offset`] and
//!   [`Paragraph::logical_offset`] convert, with [`Affinity`] for positions at
//!   boundaries and [`Bias`] for positions inside atomic units.
//! - **Geometry**: px, origin at the block's top left, y down. Each line
//!   has a baseline; fragments sit on it.
//!
//! # Rules
//!
//! - **Line height.** Every line is at least as tall as the strut (the
//!   paragraph's primary font and line height). Text adds the content area
//!   of its own primary font, from [`Measure::metrics`], never that of
//!   fallback faces, so an emoji does not grow a line. Boxes and shifted
//!   runs that stick out grow it.
//! - **Breaking.** UAX #14 across style runs; a box is U+FFFC, so it breaks
//!   like an object and never inside. White space at a line end hangs.
//!   Pieces wider than the line break at caret stops (`overflow-wrap:
//!   anywhere`) after moving to a line of their own.
//! - **Letter spacing and baseline shift** are applied here, not by the
//!   text system: painters move glyphs by [`Shaped::glyph_shift`] and paint
//!   each fragment on `line.baseline - fragment.shift`.
//! - **Decorations** are drawn by the painter from [`Layout::decorations`]:
//!   solid, dotted, dashed and wavy underlines with an offset, and
//!   strikethroughs, merged across style runs.
//! - **Direction.** Left to right only; bidi reordering is not done.

mod breaking;
mod cache;
mod geometry;
mod layout;
mod measure;
mod paragraph;
mod shaped;
mod style;

pub use cache::{BlockCache, BlockCacheStats, RunCache, RunCacheStats, WidthCache};
pub use geometry::{Caret, Decoration, DecorationKind, Hit};
pub use layout::{Fragment, Layout, Line, Rect};
pub use measure::{FakeMono, FontMetrics, Measure, ShapedRun, Stop};
pub use paragraph::{
    Affinity, Bias, Column, HARD_BREAK, Item, ItemKind, OBJECT, OBJECT_LEN, Paragraph,
    ParagraphBuilder,
};
pub use shaped::{Shaped, Strut};
pub use style::{
    Align, Background, BoxAlign, BoxKind, ColorRole, InlineBox, LineHeight, ParagraphStyle,
    RunStyle, Strikethrough, StyleId, Underline, UnderlineStyle,
};

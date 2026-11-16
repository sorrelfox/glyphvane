// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The inline parser: CommonMark 0.31.2 inlines over one leaf block.
//!
//! The block side hands over a leaf's content as document byte ranges
//! ([`InlineInput`]) plus a way to look up link reference definitions
//! ([`RefLookup`]); [`parse_inlines`] returns an [`InlineTree`]:
//!
//! - **lossless**: every byte of the segments belongs to exactly one token
//!   (markers, whitespace and line endings included), and tokens never
//!   cross a segment boundary;
//! - **compact**: one copy of the leaf's text, a preorder array of 20-byte
//!   nodes, a side table for links and an arena for decoded strings;
//! - **position-independent** in its text: offsets are relative to the
//!   gathered text and mapped to document offsets through the segment map
//!   on demand.
//!
//! [`render_inlines_html`] writes cmark-exact HTML, [`cst_dump_inlines`]
//! the S-expression dump, and the accessors on [`InlineTree`] (`links`,
//! `code_spans`, `emphasis`, `tokens`, `nodes`, `plain_text`,
//! `label_lookups`) serve later consumers, as do the text projections
//! (`text_content`, `collect_text`; see [`TextProjection`]).
//!
//! # Conformance notes
//!
//! Every spec example passes. Where cmark (and comrak, which ports it)
//! differs from the text of CommonMark 0.31.2 on inputs the examples do not
//! cover, this parser follows the text:
//!
//! - a code span closes at the first backtick string of the same length
//!   (cmark's closer cache can miss one after a partial scan);
//! - no links inside links, ever: each bracket keeps its own active flag
//!   (cmark's single "no link openers" flag is reset by any later `[`);
//! - backslash escapes and entities in destinations and titles are decoded
//!   in one pass, so `&#92;*` is a backslash and a star (cmark decodes
//!   entities first);
//! - a backslash does not hide a line ending inside `<…>` destinations;
//! - an unclosed `<!--` does not disable later CDATA sections and
//!   declarations; declarations are `<!` and any ASCII letter, comments
//!   follow the 0.31 rules (comrak 0.59 still has the older grammars);
//! - U+0000 counts as U+FFFD (punctuation) for flanking, as in cmark, which
//!   replaces it before parsing; output replaces it too;
//! - link labels hold at most 999 characters (cmark counts 1000 bytes);
//!   URI autolinks exclude U+007F.
//!
//! Where the text is silent, cmark's behavior is kept: `<…>` destinations
//! are trimmed, and URLs are percent-encoded like cmark except that a `%`
//! not followed by two hex digits becomes `%25` (as comrak and
//! commonmark.js do).

pub mod api;
mod dump;
mod entities;
pub(crate) mod html;
mod parse;
mod scan;
mod tree;
mod unicode;
mod unicode_tables;

pub use api::*;
pub use parse::is_tag_char;
pub use tree::{
    Children, CodeSpanRef, EmphasisRef, InlineKind, InlineNode, LinkKind, LinkRef, TextProjection,
    WikiLinkRef, image_size,
};

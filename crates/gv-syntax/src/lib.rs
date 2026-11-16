// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Lossless incremental Markdown parser: CST, line checkpoints, profiles, CommonMark HTML emitter.
//!
//! # Overview
//!
//! [`SyntaxTree::parse`] runs the block pass over a [`gv_text::TextSource`]
//! and returns an immutable snapshot ([`SyntaxTree`], an `Arc`). Inline
//! content is parsed lazily per leaf block ([`SyntaxTree::inlines`]).
//!
//! - **Lossless.** The block pass emits a flat stream of events per line
//!   ([`tree::event`]): `Open`/`Close` pairs and tokens that cover every
//!   byte exactly once, plus markers for decisions CommonMark makes
//!   retroactively (setext underlines, link reference definitions, list
//!   looseness, GFM table headers). Events are append-only per line.
//! - **Line checkpoints.** Each line records the parser state at its start
//!   (an interned `u32`). Lines are stored in small chunks in a persistent
//!   sum tree with byte and line summaries; nothing stores absolute
//!   offsets.
//! - **Incremental.** [`SyntaxTree::reparse`] restarts at the line before
//!   an edit and stops as soon as the new state at a line start equals the
//!   old checkpoint there ([`incremental`]). It can run in deadline slices
//!   ([`Budget`]) and continue on another thread ([`ReparseTask`]).
//! - **Stable [`BlockId`]s.** Blocks opened in the reparsed window are
//!   matched against the old window by (position mapped through the edit,
//!   depth, kind), then in order between those anchors; blocks outside the
//!   window keep their ids.
//! - **Views.** [`tree::walk`] gives the nested block structure with the
//!   markers resolved, [`walk_range`] the same for the blocks of a byte
//!   range without walking the document, [`SyntaxTree::block_index`] a
//!   cached index of the raw blocks; [`to_commonmark_html`] is the
//!   CommonMark-exact HTML emitter and [`to_gfm_html`] the cmark-gfm-exact
//!   one; [`cst_dump`] the S-expression dump.
//! - **Options.** [`ParseOptions`] holds the profile switches the parser
//!   reads (front matter, GFM tables, task lists, strikethrough, extended
//!   autolinks, footnotes, GitHub alerts and math); the default is
//!   CommonMark exactly.
//! - **Changes.** A reparse reports the replaced window ([`Changes`]);
//!   reports of consecutive reparses compose ([`Changes::compose`]).
//!
//! # Limits
//!
//! At most [`MAX_NESTING`] blocks are open at once; deeper block quote and
//! list markers are paragraph text. Every line costs O(its length) plus
//! amortized opens and closes; a blank line inside deep lists costs
//! O(log depth).
//!
//! [`harness`] holds the string-level entry points of the conformance
//! runner, the differential and pathological tests and the fuzz targets.

pub(crate) mod block;
pub mod cst_dump;
pub mod harness;
pub mod html;
pub mod incremental;
pub mod inline;
pub mod options;
pub mod table;
pub mod tree;

pub use block::MAX_NESTING;
pub use block::callout_marker;
pub use gv_profile::{CompiledProfile, ProfileId};
pub use html::{GfmHtmlOptions, to_commonmark_html, to_gfm_html};
pub use incremental::{Budget, Change, Changes, Reparse, ReparseTask};
pub use options::{DollarMath, FrontMatterOptions, ParseOptions, TaskLists};
pub use tree::event::{
    AlertKind, BlockId, BlockKind, CalloutFold, Event, FrontMatterKind, Token, TokenKind,
};
pub use tree::index::BlockIndex;
pub use tree::walk::{
    Container, ContainerKind, Leaf, LeafKind, LeafLine, Visitor, walk, walk_range,
};
pub use tree::{BlockSpan, RefMap, SyntaxTree};

/// Parses `text` with the CommonMark options (see [`SyntaxTree::parse`]).
pub fn parse<S: gv_text::TextSource + ?Sized>(text: &S) -> SyntaxTree {
    SyntaxTree::parse(text)
}

/// Parses `text` with `options` (see [`SyntaxTree::parse_with`]).
pub fn parse_with<S: gv_text::TextSource + ?Sized>(text: &S, options: ParseOptions) -> SyntaxTree {
    SyntaxTree::parse_with(text, options)
}

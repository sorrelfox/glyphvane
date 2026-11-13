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
//!   looseness). Events are append-only per line.
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
//!   markers resolved; [`to_commonmark_html`] is the CommonMark-exact HTML
//!   emitter; [`cst_dump`] the S-expression dump.
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
pub mod tree;

pub use block::MAX_NESTING;
pub use html::to_commonmark_html;
pub use incremental::{Budget, Change, Changes, Reparse, ReparseTask};
pub use tree::event::{BlockId, BlockKind, Event, Token, TokenKind};
pub use tree::walk::{Container, ContainerKind, Leaf, LeafKind, LeafLine, Visitor, walk};
pub use tree::{BlockSpan, RefMap, SyntaxTree};

/// Parses `text` (see [`SyntaxTree::parse`]).
pub fn parse<S: gv_text::TextSource + ?Sized>(text: &S) -> SyntaxTree {
    SyntaxTree::parse(text)
}

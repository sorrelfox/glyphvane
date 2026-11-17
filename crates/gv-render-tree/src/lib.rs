// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Render tree from CST and model, flattened virtual items, plain-text projection.
//!
//! # Pipeline
//!
//! 1. [`RenderTree::build`] walks a [`gv_syntax::SyntaxTree`] and makes one
//!    [`BlockNode`] per rendered leaf block, keyed by its
//!    [`BlockId`](gv_syntax::BlockId), with its containers as a
//!    [`Context`] of [`Frame`]s (quotes, lists, list items with their
//!    markers). Inline content becomes a `gv-inline`
//!    [`Paragraph`](gv_inline::Paragraph) whose runs carry
//!    [`StyleId`](gv_inline::StyleId)s from the [`StyleTable`] and colour
//!    roles from [`ColorToken`]s; links are decorated by the status an
//!    injected [`LinkStatusSource`] gives them, and images are sized by an
//!    injected [`ImageSizeSource`]. Headings get their anchor from the
//!    [`gv_model::DocModel`]'s slugs.
//! 2. The nodes are flattened into virtual [`Item`]s: one per paragraph or
//!    heading, code blocks in chunks of 50 lines, each with a stable
//!    [`ItemId`], its decorations, its source range and a height
//!    [`Estimate`] that is a function of the width.
//! 3. [`RenderTree::update`] follows reparses: it rebuilds the top-level
//!    blocks the reparse touched, keeps the content of every other block,
//!    and reports the item splice ([`UpdateReport`]).
//! 4. [`RenderTree::item_text`], [`RenderTree::plain_text`] and
//!    [`RenderTree::find`] project the tree to plain text with a mapping
//!    back to source offsets, for copy and find in page; a [`Finder`]
//!    (literal or regex, case, whole word) runs over them, or over a
//!    [`FindIndex`] built once per tree.
//!
//! # Profiles
//!
//! Inline content is parsed with the syntax tree's profile switches
//! ([`gv_syntax::ParseOptions`]): GFM tables (one item per row, cells
//! separated by gap boxes the view sizes from the column widths),
//! strikethrough, extended autolinks and math (formula boxes, ` ```math `
//! and `$$` blocks; [`RenderTree::math_macros`] runs the document's macro
//! pass), task items (their marker is a checkbox), footnotes (numbered
//! references; the definitions in a footnote section after the document,
//! see [`RenderTree::footnote_section_start`]) and GitHub alerts (an
//! [`FrameKind::Alert`] box). Raw HTML (blocks and inline) is shown as
//! text and flagged `raw_html_pending` until the HTML pipeline exists,
//! except `<details>` (a summary row folding the blocks up to its end) and
//! `<picture>`; front matter is a property list (or hidden). Canvas and
//! base embeds and ` ```base ` and ` ```query ` fences are live blocks the
//! view fills in itself ([`LiveBlock`], see `live`).
//!
//! # Coordinates
//!
//! Item and node source ranges are absolute; everything inside a node
//! (paragraph sources, link, image and code line ranges) is relative to the
//! node's start (see [`BlockNode::to_document`]).

mod build;
mod dump;
mod find;
mod footnote;
mod html;
mod inline;
mod item;
mod live;
mod metrics;
mod node;
mod properties;
mod status;
mod style;
mod text;
mod tree;

pub use build::{
    CALLOUT_TITLE_GUTTER, MAX_EMBED_BYTES, MAX_EMBED_DEPTH, MAX_EMBED_NODES, MAX_FRAMES,
};
pub use find::{FindError, FindIndex, Finder, Matches};
pub use footnote::FootnoteIndex;
pub use inline::MATH_DISPLAY_SCALE;
pub use item::{Estimate, Item, ItemId, ItemKind, indent, right_indent};
pub use live::{LIVE_EMBED_HEIGHT, LIVE_QUERY_HEIGHT, LiveBlock, LiveKey, LiveKind};
pub use metrics::{Metrics, Spacing};
pub use node::{
    BlockNode, BoxRef, CalloutType, CodeBlock, CodeLine, Context, FootnoteRefInfo, Frame,
    FrameKind, ImageInfo, InlineContent, InlineRole, InlineSpan, LinkInfo, ListMarker, MathInfo,
    NodeKind, Picture, PictureSource, TableBlock, TableRow, Units, default_callout_title,
};
pub use properties::Property;
pub use status::{
    AllPending, EmbedFetch, ImageSize, ImageSizeSource, LinkStatus, LinkStatusSource, LinkTarget,
    NoImageSizes, Sources,
};
pub use style::{
    ColorToken, Family, LinkDecor, LinkSuffix, StatusIcon, StyleSpec, StyleTable, TextRole,
    Variant, decode_style_id, link_decor, style_id,
};
pub use text::{CopyOptions, FindMatch, FindOptions, ItemText};
pub use tree::{FrontMatterDisplay, RenderOptions, RenderTree, UpdateReport};

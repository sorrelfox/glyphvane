// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Document semantics from the CST: outline, slugs, links, embeds, tags, block IDs, front matter.
//!
//! [`DocModel`] is the meaning of one document revision, derived from a
//! [`gv_syntax::SyntaxTree`] and its text:
//!
//! - **headings** with their rendered text, source text, level and ranges,
//!   and the **outline** tree with heading sections;
//! - **links and images** with their kind, decoded destination, title,
//!   label and source ranges, as the link resolver takes them;
//! - **link reference definitions** with the ranges of label, destination
//!   and title, for refactors;
//! - **slugs** of every heading under each [`SlugAlgorithm`], and fragment
//!   matching ([`DocModel::match_fragment`]) with the fallbacks of the
//!   link specification;
//! - **front matter** ([`DocModel::front_matter`]): where the metadata
//!   block is and its syntax, when the tree was parsed with front matter
//!   enabled (`gv_syntax::ParseOptions::from_profile`). Its properties are
//!   not parsed yet.
//!
//! The model is an immutable `Arc` snapshot. [`DocModel::update`] builds
//! the next revision's model from the previous one, taking over the records
//! of blocks outside the reparsed window.
//!
//! # Profiles
//!
//! This version covers the CommonMark profile. Later profiles add records
//! without changing these: [`LinkKind`] is non-exhaustive (wikilinks,
//! embeds, extended autolinks, HTML attributes), headings carry an
//! [`explicit_id`](Heading::explicit_id) slot (`{#id}`), and
//! [`DocModel::block_anchors`] with [`DocModel::match_block`] hold Obsidian
//! block ids. Front matter follows the tree's parse options. Tags,
//! footnotes, tasks and front matter properties come with the profiles
//! that have them.
//!
//! # Link resolution
//!
//! This crate does not depend on the resolver (`gv-vault` depends on it).
//! The resolver adapts [`Link`] into its own link type and implements its
//! fragment-resolver trait over [`DocModel::match_fragment`] and
//! [`DocModel::match_block`].

mod build;
mod fragment;
mod model;
pub mod outline;
pub mod slug;

pub use fragment::{Anchoring, BlockMatch, FragmentMatch};
pub use gv_syntax::FrontMatterKind;
pub use model::{
    BlockAnchor, BuildStats, Definition, DocModel, FrontMatter, Heading, Link, LinkKind, Tag,
};
pub use outline::OutlineEntry;
pub use slug::{HugoIds, PandocIds, Projection, SlugAlgorithm, Slugger};

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Query language, parallel scan and quick-switcher matching.
//!
//! # Parts
//!
//! | Part | What |
//! |---|---|
//! | [`query`] | The query language (Obsidian-compatible subset) and its lenient parser |
//! | [`Query`] | A parsed and compiled query; [`Query::match_note`] matches one note |
//! | [`search()`] | The parallel scan: streamed [`ResultBatch`]es, [`CancelToken`], [`SearchOptions`] limits |
//! | [`search_all`] / [`rank`] | Whole result lists, ranked (file name and title first, then density, then recency) |
//! | [`SearchSource`] | What is scanned: [`VaultSource`] (a content-index snapshot) or [`MemorySource`] |
//!
//! # Query language
//!
//! - Words and `"quoted phrases"` match as substrings of the note text or
//!   its file name; juxtaposition is AND, `OR` (upper case) is OR,
//!   `-term` negates, parentheses group.
//! - `/regex/` uses Rust `regex` syntax, multi-line (`^` and `$` match at
//!   line breaks).
//! - Operators: `file:` (file name), `path:` (vault path), `content:`
//!   (text only), `tag:` (tags from the index, so tags in code are never
//!   found; `tag:#a` finds `#a/b`), `line:`, `block:`, `section:` (all
//!   terms in one line, block or section), `task:`, `task-todo:`,
//!   `task-done:` (the line of a task; any status other than a space is
//!   done; `task:""` finds every task), `match-case:`, `ignore-case:`.
//! - Properties: `[name]` (present), `[name:null]` (empty),
//!   `[name:value]` (the value, or an item of a list, contains `value`;
//!   the value may be any expression, `[status:(draft OR review)]`),
//!   `[name:>5]`, `[due:<2026-01-01]` (numbers numerically, other text
//!   ignoring case).
//! - Case is ignored unless the caller asks otherwise
//!   ([`Query::new`]) or `match-case:` applies.
//!
//! # Scanning
//!
//! Metadata tests (properties, `file:`, `path:`, `tag:`) run before text
//! tests, and a note's text is read only when a text test needs it, so a
//! property query reads no files. Notes are scanned on rayon worker
//! threads; results go back to the calling thread over a channel and are
//! handed out in batches: the first match at once, then every
//! [`SearchOptions::batch_size`] notes or [`SearchOptions::batch_interval`].
//!
//! The quick switcher's fuzzy matching lives with its view in `gv-ui`
//! (`switcher::model`), on `nucleo-matcher`.

mod matcher;
pub mod query;
mod scan;
mod source;

#[cfg(test)]
mod tests;

pub use matcher::QueryError;
pub use query::{Comparison, Expr, Operator, PropertyTest, TextPattern, parse};
pub use scan::{
    CancelToken, FileResult, Query, ResultBatch, SearchOptions, SearchSummary, Snippet, rank,
    search, search_all,
};
pub use source::{MemorySource, SearchSource, VaultSource};

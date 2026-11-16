// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Code highlighting with caches and the Linguist alias map.
//!
//! - [`Language::from_info`] resolves a fence info string (`rust`, `rs`,
//!   `py3`, `{.js}`, `console`) through the alias map built from GitHub
//!   Linguist and the grammar bundle.
//! - [`CodeHighlighter`] highlights a text into [`Span`]s of
//!   [`TokenClass`]es, never colours: the UI colours each class with a
//!   design token ([`TokenClass::token`]), so results do not depend on the
//!   theme and are cached by language and content hash only. Text over
//!   [`Limits`] is left plain.
//! - The engine ([`GialloHighlighter`], TextMate grammars through giallo)
//!   sits behind the [`Highlighter`] trait. Grammars come from the embedded,
//!   licence-filtered bundle (`bundle/`, built by
//!   `cargo xtask highlight-bundle`) and load lazily per language.
//!
//! Everything here is `Send + Sync`. [`CodeHighlighter::highlight`] blocks
//! (milliseconds per thousand lines): call it on a background executor and
//! use [`CodeHighlighter::get`] on the UI thread.

mod bundle;
mod class;
mod engine;
mod service;

pub use bundle::{Language, alias_count, bundle_sources};
pub use class::{FontStyle, TokenClass};
pub use engine::{EngineError, GialloHighlighter, Highlighter, Span};
pub use service::{CacheStats, CodeHighlighter, Highlight, HighlightKey, Limits, Outcome};

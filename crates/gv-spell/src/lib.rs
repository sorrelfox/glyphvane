// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Spell-check backend trait, spellbook backend and prose tokenizer.
//!
//! # Pieces
//!
//! - [`SpellBackend`]: one language's checker. [`SpellbookBackend`] wraps a
//!   spellbook dictionary (Hunspell-compatible, pure Rust); native OS
//!   spellers can implement the same trait later.
//! - [`Library`]: the dictionaries that can be loaded. `en_US` (SCOWL
//!   2026.02.25) is bundled; `.aff`/`.dic` pairs in the user's dictionary
//!   folder add or replace languages. Both files are decoded from the
//!   `.aff`'s `SET` label first (spellbook reads UTF-8 only), and a label
//!   that names no known encoding is reported as such. Loaded dictionaries
//!   are shared (`Arc`) and loaded once.
//! - [`WordList`]: a user word list (one word per line), for the global
//!   dictionary and a vault's. Words are kept apart from the dictionaries
//!   and checked first.
//! - [`Checker`]: the dictionaries of a document's languages, the word sets
//!   and the token rules. A word is correct if any dictionary or word set
//!   accepts it.
//! - [`prose`]: what is prose in a parsed document. Only text runs of the
//!   CST are checked: code, math, URLs and autolinks, link destinations and
//!   labels, raw HTML, front-matter keys, wikilink targets, tags, emoji
//!   shortcodes, callout types and block ids are skipped.
//! - [`tokenize`]: words by UAX #29, with apostrophes kept between letters
//!   and hyphenated words passed whole.
//! - [`check_document`]: misspellings of a document, block by block, with
//!   a [`BlockCache`] keyed by the block's text and the checker, so a
//!   re-check after an edit only checks the blocks that changed.
//! - [`document_languages`]: the front matter's `lang` or `language`.
//!
//! Calls into spellbook are wrapped in `catch_unwind`: it is alpha, and a
//! panic on odd input must not take the editor down (a word that panics
//! counts as correct).

mod backend;
mod check;
mod dictionary;
mod document;
mod language;
pub mod prose;
pub mod tokenize;
mod words;

pub use backend::{SpellBackend, SpellbookBackend, SuggestDepth};
pub use check::{Checker, Rules, WordSet};
pub use dictionary::{
    BUNDLED, Bundled, DictionaryError, DictionarySource, Library, decode_hunspell,
};
pub use document::{BlockCache, CheckStats, DocumentCheck, Misspelling, check_document};
pub use language::{document_languages, normalize_tag};
pub use words::WordList;

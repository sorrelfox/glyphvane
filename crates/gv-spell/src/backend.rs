// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The backend trait and the spellbook backend.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

/// How hard [`SpellBackend::suggest`] looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SuggestDepth {
    /// Edits and replacement tables only: under 1 ms, for showing a menu at
    /// once.
    Fast,
    /// Also n-gram similarity: p99 15 ms for `en_US`, up to 0.7 s for long
    /// German compounds. Never on the UI thread.
    Full,
}

/// One language's spell checker.
pub trait SpellBackend: Send + Sync + std::fmt::Debug {
    /// The language id, as the dictionary is named (`en_US`).
    fn language(&self) -> &str;

    /// Whether `word` is spelled correctly.
    fn check(&self, word: &str) -> bool;

    /// Corrections for `word`, best first.
    fn suggest(&self, word: &str, depth: SuggestDepth) -> Vec<String>;
}

/// A spellbook dictionary.
#[derive(Clone)]
pub struct SpellbookBackend {
    language: Arc<str>,
    dictionary: Arc<spellbook::Dictionary>,
}

impl std::fmt::Debug for SpellbookBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpellbookBackend")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl SpellbookBackend {
    /// A backend over a loaded dictionary.
    pub fn new(language: &str, dictionary: Arc<spellbook::Dictionary>) -> Self {
        Self {
            language: language.into(),
            dictionary,
        }
    }
}

impl SpellBackend for SpellbookBackend {
    fn language(&self) -> &str {
        &self.language
    }

    fn check(&self, word: &str) -> bool {
        // A panic inside spellbook counts the word as correct.
        catch_unwind(AssertUnwindSafe(|| self.dictionary.check(word))).unwrap_or(true)
    }

    fn suggest(&self, word: &str, depth: SuggestDepth) -> Vec<String> {
        catch_unwind(AssertUnwindSafe(|| {
            let mut out = Vec::new();
            self.dictionary
                .suggester()
                .with_ngram_suggestions(depth == SuggestDepth::Full)
                .suggest(word, &mut out);
            out
        }))
        .unwrap_or_default()
    }
}

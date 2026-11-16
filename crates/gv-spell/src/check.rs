// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The checker of one document: its dictionaries, word sets and rules.

use std::collections::HashSet;
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use crate::backend::{SpellBackend, SuggestDepth};

/// Which words are not checked at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rules {
    /// Words with a digit (`h2o`, `3rd`).
    pub skip_words_with_digits: bool,
    /// Words in capitals (`NASA`), two letters or more.
    pub skip_all_caps: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            skip_words_with_digits: true,
            skip_all_caps: true,
        }
    }
}

/// Words accepted besides the dictionaries (a user list, a vault list,
/// the words ignored this session). As in Hunspell's personal
/// dictionaries, a lower-case word also accepts its capitalized and
/// all-caps forms, and a capitalized one its all-caps form.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WordSet(HashSet<String>);

impl WordSet {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(words: I) -> Self {
        Self(words.into_iter().map(Into::into).collect())
    }

    pub fn insert(&mut self, word: &str) -> bool {
        self.0.insert(word.to_owned())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the set accepts `word`.
    pub fn accepts(&self, word: &str) -> bool {
        if self.0.is_empty() {
            return false;
        }
        if self.0.contains(word) {
            return true;
        }
        let mut chars = word.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        let rest: String = chars.collect();
        let all_caps = word.chars().all(|c| !c.is_lowercase());
        let capitalized = first.is_uppercase() && rest.chars().all(|c| !c.is_uppercase());
        if !(all_caps || capitalized) {
            return false;
        }
        if self.0.contains(&word.to_lowercase()) {
            return true;
        }
        all_caps && self.0.contains(&format!("{first}{}", rest.to_lowercase()))
    }
}

/// Scripts written without spaces between words: their words cannot be
/// told apart by UAX #29, so they are not checked.
fn unspaced_script(c: char) -> bool {
    matches!(c as u32,
        0x0E00..=0x0EFF   // Thai, Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x3040..=0x30FF // Hiragana, Katakana
        | 0x3400..=0x4DBF // CJK Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
        | 0x20000..=0x3FFFF)
}

/// One document's checker. Cheap to share (`Arc` inside).
#[derive(Debug, Clone)]
pub struct Checker {
    backends: Vec<Arc<dyn SpellBackend>>,
    words: Vec<Arc<WordSet>>,
    rules: Rules,
    fingerprint: u64,
}

impl Checker {
    /// A checker over `backends` (a word is correct if any accepts it) and
    /// `words`. `revision` names the word sets' contents: results cached
    /// under one checker are reused by another only when their languages,
    /// rules and revision are equal ([`Checker::fingerprint`]).
    pub fn new(
        backends: Vec<Arc<dyn SpellBackend>>,
        words: Vec<Arc<WordSet>>,
        rules: Rules,
        revision: u64,
    ) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for backend in &backends {
            backend.language().hash(&mut hasher);
        }
        rules.hash(&mut hasher);
        revision.hash(&mut hasher);
        Self {
            backends,
            words,
            rules,
            fingerprint: hasher.finish(),
        }
    }

    /// The languages checked, in order.
    pub fn languages(&self) -> Vec<&str> {
        self.backends.iter().map(|b| b.language()).collect()
    }

    /// Whether any dictionary is loaded (without one nothing is checked).
    pub fn is_active(&self) -> bool {
        !self.backends.is_empty()
    }

    pub fn rules(&self) -> Rules {
        self.rules
    }

    /// Identifies what this checker accepts, for caching results.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Whether `word` is left alone: a single character, digits or
    /// capitals per the rules, a dotted name (`file.md`, `e.g`), an
    /// identifier (`snake_case`), or a script written without spaces.
    pub fn skips(&self, word: &str) -> bool {
        let mut chars = word.chars();
        if chars.next().is_none() || chars.next().is_none() {
            return true;
        }
        if word.contains(['.', '_']) || word.chars().any(unspaced_script) {
            return true;
        }
        if self.rules.skip_words_with_digits && word.chars().any(|c| c.is_numeric()) {
            return true;
        }
        if self.rules.skip_all_caps {
            let letters = word.chars().filter(|c| c.is_alphabetic()).count();
            if letters >= 2 && word.chars().all(|c| !c.is_lowercase()) {
                return true;
            }
        }
        false
    }

    /// Whether a word set or any dictionary accepts `word`.
    pub fn is_correct(&self, word: &str) -> bool {
        self.words.iter().any(|w| w.accepts(word)) || self.backends.iter().any(|b| b.check(word))
    }

    /// Whether `word` is checked and found wrong.
    pub fn is_misspelled(&self, word: &str) -> bool {
        self.is_active() && !self.skips(word) && !self.is_correct(word)
    }

    /// Up to `limit` corrections, alternating between the dictionaries,
    /// without repeats.
    pub fn suggest(&self, word: &str, depth: SuggestDepth, limit: usize) -> Vec<String> {
        let lists: Vec<Vec<String>> = self
            .backends
            .iter()
            .map(|b| b.suggest(word, depth))
            .collect();
        let mut out: Vec<String> = Vec::new();
        let longest = lists.iter().map(Vec::len).max().unwrap_or(0);
        for i in 0..longest {
            for list in &lists {
                if let Some(s) = list.get(i)
                    && !out.contains(s)
                    && out.len() < limit
                {
                    out.push(s.clone());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Library;

    fn en(words: &[&str]) -> Checker {
        let library = Library::new(None);
        let backend = library.backend("en_US").expect("bundled");
        Checker::new(
            vec![backend],
            vec![Arc::new(WordSet::new(words.iter().copied()))],
            Rules::default(),
            0,
        )
    }

    #[test]
    fn rules_skip_words() {
        let c = en(&[]);
        for word in ["x", "NASA", "h2o", "3rd", "file.md", "snake_case", "漢字"] {
            assert!(c.skips(word), "{word}");
            assert!(!c.is_misspelled(word), "{word}");
        }
        let all = Checker::new(
            Vec::new(),
            Vec::new(),
            Rules {
                skip_words_with_digits: false,
                skip_all_caps: false,
            },
            0,
        );
        assert!(!all.skips("NASA"));
        assert!(!all.skips("h2o"));
        // Without a dictionary nothing is misspelled.
        assert!(!all.is_misspelled("qwrtzp"));
    }

    #[test]
    fn word_sets_accept_case_forms() {
        let c = en(&["glyphvane", "Zettelkasten"]);
        assert!(!c.is_misspelled("glyphvane"));
        assert!(!c.is_misspelled("Glyphvane"));
        assert!(!c.is_misspelled("Zettelkasten"));
        assert!(c.is_misspelled("zettelkasten"));
        assert!(c.is_misspelled("teh"));
        assert!(!c.is_misspelled("the"));
        assert!(!c.is_misspelled("well-known"));
    }

    #[test]
    fn suggestions() {
        let c = en(&[]);
        let fast = c.suggest("teh", SuggestDepth::Fast, 6);
        assert!(fast.contains(&"the".to_owned()), "{fast:?}");
        let full = c.suggest("recieve", SuggestDepth::Full, 6);
        assert_eq!(
            full.first().map(String::as_str),
            Some("receive"),
            "{full:?}"
        );
        let capital = c.suggest("Recieve", SuggestDepth::Full, 6);
        assert_eq!(
            capital.first().map(String::as_str),
            Some("Receive"),
            "{capital:?}"
        );
        assert!(c.suggest("teh", SuggestDepth::Full, 1).len() <= 1);
    }
}

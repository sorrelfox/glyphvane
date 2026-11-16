// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The highlighting engine behind a trait, so another engine can replace
//! giallo without touching callers.

use std::collections::HashMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{PoisonError, RwLock};

use giallo::{HighlightOptions, Registry, ThemeVariant};

use crate::bundle::{Language, bundle};
use crate::class::{CLASSIFIER_THEME, TokenClass, classifier_theme_json};

/// A classified byte range of the highlighted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// Start byte offset in the highlighted text.
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
    /// What the text is.
    pub class: TokenClass,
}

/// Why an engine could not highlight a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The language's grammar could not be loaded.
    Grammar(String),
    /// The engine failed (an error or a panic inside it).
    Engine(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grammar(msg) => write!(f, "grammar unavailable: {msg}"),
            Self::Engine(msg) => write!(f, "highlighter failed: {msg}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// A highlighting engine: text and language in, classified spans out.
///
/// Implementations are `Send + Sync` and are called from background
/// threads; one call may take milliseconds per thousand lines.
pub trait Highlighter: Send + Sync {
    /// Classifies `code` as `language`. Spans are sorted, do not overlap,
    /// never cross a line break, never cover [`TokenClass::Plain`] text, and
    /// lie on `char` boundaries.
    fn highlight(&self, code: &str, language: Language) -> Result<Vec<Span>, EngineError>;

    /// Loads `language` and the grammars it needs ahead of the first
    /// highlight (optional; `highlight` loads lazily).
    fn preload(&self, language: Language) -> Result<(), EngineError> {
        let _ = language;
        Ok(())
    }
}

/// The giallo engine (TextMate grammars), with grammars loaded lazily from
/// the bundle: a language's grammar and everything it embeds are added on
/// first use, under a write lock; highlighting takes a read lock, so
/// highlights run in parallel.
///
/// Regex runtime errors (fancy-regex's backtrack limit) count as "no
/// match", so pathological input degrades to fewer classes rather than
/// running long.
pub struct GialloHighlighter {
    state: RwLock<State>,
    /// Sentinel colour (`#RRGGBB`) to class.
    classes: HashMap<String, TokenClass>,
}

struct State {
    registry: Registry,
    /// Grammars added to the registry, by bundle id.
    loaded: Vec<bool>,
    /// Grammars whose load failed, by bundle id, with the reason.
    failed: HashMap<u16, String>,
}

impl fmt::Debug for GialloHighlighter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("GialloHighlighter")
            .field("loaded", &state.loaded.iter().filter(|l| **l).count())
            .field("failed", &state.failed.len())
            .finish()
    }
}

impl Default for GialloHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl GialloHighlighter {
    /// An engine with no grammar loaded yet (cheap: builds the classifier
    /// theme only).
    pub fn new() -> Self {
        let mut registry = Registry::default();
        if let Err(err) = registry.add_theme_from_bytes(classifier_theme_json().as_bytes()) {
            log::error!("gv-highlight: classifier theme rejected: {err}");
        }
        registry.link_grammars();
        let classes = TokenClass::ALL
            .into_iter()
            .map(|c| (c.sentinel(), c))
            .collect();
        Self {
            state: RwLock::new(State {
                registry,
                loaded: vec![false; bundle().len()],
                failed: HashMap::new(),
            }),
            classes,
        }
    }

    /// Adds `language` and its closure to the registry if needed.
    fn ensure_loaded(&self, language: Language) -> Result<(), EngineError> {
        let id = language.id();
        {
            let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(reason) = state.failed.get(&id) {
                return Err(EngineError::Grammar(reason.clone()));
            }
            if state.loaded.get(usize::from(id)).copied().unwrap_or(false) {
                return Ok(());
            }
        }
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        if state.loaded.get(usize::from(id)).copied().unwrap_or(false) {
            return Ok(());
        }
        let bundle = bundle();
        let mut added = false;
        for g in bundle.closure(id) {
            let index = usize::from(g);
            if state.loaded.get(index).copied().unwrap_or(true) || state.failed.contains_key(&g) {
                continue;
            }
            let result = bundle.grammar_json(g).and_then(|json| {
                catch_unwind(AssertUnwindSafe(|| {
                    state.registry.add_grammar_from_bytes(&json)
                }))
                .map_err(|_| "panic while compiling the grammar".to_owned())?
                .map_err(|e| e.to_string())
            });
            match result {
                Ok(()) => {
                    state.loaded[index] = true;
                    added = true;
                }
                Err(reason) => {
                    log::warn!(
                        "gv-highlight: grammar {} not loaded: {reason}",
                        Language::from_id(g).name()
                    );
                    state.failed.insert(g, reason);
                }
            }
        }
        if added {
            let registry = &mut state.registry;
            if catch_unwind(AssertUnwindSafe(|| registry.link_grammars())).is_err() {
                log::error!("gv-highlight: linking grammars panicked");
            }
        }
        match state.failed.get(&id) {
            Some(reason) => Err(EngineError::Grammar(reason.clone())),
            None => Ok(()),
        }
    }

    fn class_of(&self, color: giallo::Color) -> TokenClass {
        self.classes
            .get(&color.as_hex())
            .copied()
            .unwrap_or(TokenClass::Plain)
    }
}

impl Highlighter for GialloHighlighter {
    fn preload(&self, language: Language) -> Result<(), EngineError> {
        self.ensure_loaded(language)
    }

    fn highlight(&self, code: &str, language: Language) -> Result<Vec<Span>, EngineError> {
        self.ensure_loaded(language)?;
        let state = self.state.read().unwrap_or_else(PoisonError::into_inner);
        let options =
            HighlightOptions::new(language.name(), ThemeVariant::Single(CLASSIFIER_THEME))
                .merge_whitespace(false)
                .merge_same_style_tokens(true);
        let highlighted = catch_unwind(AssertUnwindSafe(|| {
            state.registry.highlight(code, &options)
        }))
        .map_err(|_| EngineError::Engine("panic while highlighting".to_owned()))?
        .map_err(|e| EngineError::Engine(e.to_string()))?;

        let mut spans = Vec::new();
        for (line, tokens) in line_ranges(code).zip(&highlighted.tokens) {
            let line_text = &code[line.clone()];
            // giallo's tokens cover the line from its start, in order. If
            // they do not add up (a normalisation difference), the line is
            // left plain rather than misplacing spans.
            let total: usize = tokens.iter().map(|t| t.text.len()).sum();
            if total != line_text.len() {
                continue;
            }
            let mut pos = line.start;
            for token in tokens {
                let end = pos + token.text.len();
                let class = match token.style {
                    ThemeVariant::Single(style) => self.class_of(style.foreground),
                    ThemeVariant::Dual { light, .. } => self.class_of(light.foreground),
                };
                if class != TokenClass::Plain && end > pos {
                    match spans.last_mut() {
                        Some(Span {
                            end: last_end,
                            class: last,
                            ..
                        }) if *last == class && *last_end == pos => {
                            *last_end = end;
                        }
                        _ => spans.push(Span {
                            start: pos,
                            end,
                            class,
                        }),
                    }
                }
                pos = end;
            }
        }
        Ok(spans)
    }
}

/// The byte ranges of the lines of `text`, without their line breaks,
/// splitting the way giallo normalises: `\r\n`, `\n` and a lone `\r` each
/// end a line.
pub(crate) fn line_ranges(text: &str) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        let mut i = start;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => {
                    let range = start..i;
                    start = i + 1;
                    return Some(range);
                }
                b'\r' => {
                    let range = start..i;
                    start = if bytes.get(i + 1) == Some(&b'\n') {
                        i + 2
                    } else {
                        i + 1
                    };
                    return Some(range);
                }
                _ => i += 1,
            }
        }
        done = true;
        Some(start..bytes.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_like_giallo() {
        let lines: Vec<_> = line_ranges("a\r\nb\rc\n\nd").collect();
        assert_eq!(lines, [0..1, 3..4, 5..6, 7..7, 8..9]);
        let empty: Vec<_> = line_ranges("").collect();
        assert_eq!((empty.len(), empty.first()), (1, Some(&(0..0))));
        assert_eq!(line_ranges("x\n").collect::<Vec<_>>(), [0..1, 2..2]);
    }

    #[test]
    fn engine_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GialloHighlighter>();
    }
}

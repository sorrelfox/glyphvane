// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The cached, size-capped highlighting service the UI uses.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::bundle::Language;
use crate::engine::{GialloHighlighter, Highlighter, Span};

/// Caps on what is highlighted. Text over a cap is shown plain
/// ([`Outcome::TooLarge`]) instead of tying up a background thread.
///
/// The engine has no cancellation, and some grammars are quadratic in the
/// length of a line on adversarial input (Markdown emphasis runs such as
/// `*a *a *a ...`: about 27 ms for a 1 KB line, 380 ms for 4 KB, 6 s for
/// 16 KB in an optimised build). So besides the size caps, the sum of the
/// squared line lengths is capped: it bounds the worst case of a whole block
/// to about a second while leaving ordinary code (short lines) untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest text highlighted, in bytes.
    pub max_bytes: usize,
    /// Longest line highlighted, in bytes (minified files, data blobs).
    pub max_line_bytes: usize,
    /// Largest sum over the lines of the squared line length, in bytes².
    pub max_line_work: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 512 << 10,
            max_line_bytes: 4 << 10,
            max_line_work: 64_000_000,
        }
    }
}

impl Limits {
    /// No caps (for measurements; never for user content).
    pub const UNLIMITED: Self = Self {
        max_bytes: usize::MAX,
        max_line_bytes: usize::MAX,
        max_line_work: u64::MAX,
    };

    /// Whether `code` is within the caps.
    pub fn allows(&self, code: &str) -> bool {
        if code.len() > self.max_bytes {
            return false;
        }
        let mut work: u64 = 0;
        for line in crate::engine::line_ranges(code) {
            if line.len() > self.max_line_bytes {
                return false;
            }
            let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
            work = work.saturating_add(len.saturating_mul(len));
            if work > self.max_line_work {
                return false;
            }
        }
        true
    }
}

/// How a highlight request ended. Every outcome but `Highlighted` has no
/// spans: the text is shown plain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Spans were produced (possibly none, for text with no classes).
    Highlighted,
    /// No info string, or no bundled grammar for it.
    UnknownLanguage,
    /// Over [`Limits`].
    TooLarge,
    /// The engine failed; the reason is logged.
    Failed(String),
}

/// The result of highlighting one text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    /// The resolved language, if any.
    pub language: Option<Language>,
    /// Classified spans, sorted, non-overlapping, never crossing a line
    /// break; text outside them is plain.
    pub spans: Vec<Span>,
    /// How the request ended.
    pub outcome: Outcome,
}

impl Highlight {
    fn plain(language: Option<Language>, outcome: Outcome) -> Self {
        Self {
            language,
            spans: Vec::new(),
            outcome,
        }
    }

    /// The spans that intersect `range` (byte offsets in the highlighted
    /// text), for drawing one line or one chunk.
    pub fn spans_in(&self, range: Range<usize>) -> &[Span] {
        let first = self.spans.partition_point(|s| s.end <= range.start);
        let last = self.spans.partition_point(|s| s.start < range.end);
        self.spans.get(first..last.max(first)).unwrap_or(&[])
    }

    fn cost(&self) -> usize {
        std::mem::size_of::<Self>() + self.spans.len() * std::mem::size_of::<Span>()
    }
}

/// The cache key of a text: language and content hash. Colours are not part
/// of it: results are token classes, valid for every theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HighlightKey {
    language: Option<Language>,
    len: usize,
    hash: u64,
}

impl HighlightKey {
    /// The key of `code` highlighted as the fence info string `info`.
    /// Costs one pass over `code` (no highlighting): cheap enough for the UI
    /// thread on a code chunk.
    pub fn new(code: &str, info: &str) -> Self {
        Self::for_language(code, Language::from_info(info))
    }

    /// The key of `code` highlighted as `language`.
    pub fn for_language(code: &str, language: Option<Language>) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        code.hash(&mut hasher);
        Self {
            language,
            len: code.len(),
            hash: hasher.finish(),
        }
    }

    /// The resolved language.
    pub fn language(&self) -> Option<Language> {
        self.language
    }
}

/// Cache statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    /// Cached results.
    pub entries: usize,
    /// Approximate memory held by the cached results, in bytes.
    pub bytes: usize,
    /// Lookups answered from the cache.
    pub hits: u64,
    /// Lookups that had to highlight.
    pub misses: u64,
}

#[derive(Debug, Default)]
struct Cache {
    map: HashMap<HighlightKey, (Arc<Highlight>, u64)>,
    tick: u64,
    stats: CacheStats,
}

impl Cache {
    fn get(&mut self, key: &HighlightKey) -> Option<Arc<Highlight>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.1 = tick;
        Some(Arc::clone(&entry.0))
    }

    fn insert(&mut self, key: HighlightKey, value: Arc<Highlight>, budget: usize) {
        self.tick += 1;
        let cost = value.cost();
        if let Some((old, _)) = self.map.insert(key, (value, self.tick)) {
            self.stats.bytes -= old.cost();
        }
        self.stats.bytes += cost;
        if self.stats.bytes > budget {
            // Drop the least recently used entries down to 3/4 of the
            // budget, so eviction runs rarely.
            let mut by_age: Vec<(u64, HighlightKey)> =
                self.map.iter().map(|(k, (_, t))| (*t, *k)).collect();
            by_age.sort_unstable_by_key(|(t, _)| *t);
            for (_, k) in by_age {
                if self.stats.bytes <= budget / 4 * 3 {
                    break;
                }
                if let Some((old, _)) = self.map.remove(&k) {
                    self.stats.bytes -= old.cost();
                }
            }
        }
        self.stats.entries = self.map.len();
    }
}

/// Highlighting with a cache and input caps, safe to share between
/// threads.
///
/// [`highlight`](Self::highlight) runs the engine and may take milliseconds:
/// call it on a background executor. On the UI thread, use
/// [`get`](Self::get) (cache only) and draw the text plain until the
/// background result arrives.
#[derive(Debug)]
pub struct CodeHighlighter<H = GialloHighlighter> {
    engine: H,
    limits: Limits,
    budget: usize,
    cache: Mutex<Cache>,
}

/// Default cache budget: 16 MB of spans.
const DEFAULT_BUDGET: usize = 16 << 20;

impl Default for CodeHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeHighlighter {
    /// The giallo engine with default limits and cache budget.
    pub fn new() -> Self {
        Self::with_engine(GialloHighlighter::new(), Limits::default(), DEFAULT_BUDGET)
    }

    /// The process-wide instance.
    pub fn shared() -> &'static Self {
        static SHARED: OnceLock<CodeHighlighter> = OnceLock::new();
        SHARED.get_or_init(Self::new)
    }
}

impl<H: Highlighter> CodeHighlighter<H> {
    /// A highlighter over `engine`, with `limits` and a cache of about
    /// `cache_budget` bytes.
    pub fn with_engine(engine: H, limits: Limits, cache_budget: usize) -> Self {
        Self {
            engine,
            limits,
            budget: cache_budget,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// The limits in force.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Resolves a fence info string to a bundled language.
    pub fn resolve(&self, info: &str) -> Option<Language> {
        Language::from_info(info)
    }

    /// Highlights `code` as the fence info string `info` (`rust`, `py`,
    /// `{.js}`, ...), from the cache when possible. Blocking: run it off the
    /// UI thread.
    pub fn highlight(&self, code: &str, info: &str) -> Arc<Highlight> {
        self.highlight_keyed(HighlightKey::new(code, info), code)
    }

    /// Highlights `code` under a key computed earlier with
    /// [`HighlightKey::new`] for the same text.
    pub fn highlight_keyed(&self, key: HighlightKey, code: &str) -> Arc<Highlight> {
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(hit) = cache.get(&key) {
                cache.stats.hits += 1;
                return hit;
            }
            cache.stats.misses += 1;
        }
        let result = Arc::new(self.compute(code, key.language));
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, Arc::clone(&result), self.budget);
        result
    }

    /// The cached result for `key`, without highlighting: for the UI thread.
    pub fn get(&self, key: &HighlightKey) -> Option<Arc<Highlight>> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let hit = cache.get(key);
        if hit.is_some() {
            cache.stats.hits += 1;
        }
        hit
    }

    /// Loads a language's grammars ahead of use (on a background thread).
    pub fn preload(&self, info: &str) {
        if let Some(language) = Language::from_info(info)
            && let Err(err) = self.engine.preload(language)
        {
            log::warn!("gv-highlight: preloading {}: {err}", language.name());
        }
    }

    /// Empties the cache.
    pub fn clear_cache(&self) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let stats = cache.stats;
        *cache = Cache::default();
        cache.stats.hits = stats.hits;
        cache.stats.misses = stats.misses;
    }

    /// Cache statistics.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats
    }

    fn compute(&self, code: &str, language: Option<Language>) -> Highlight {
        let Some(language) = language else {
            return Highlight::plain(None, Outcome::UnknownLanguage);
        };
        if !self.limits.allows(code) {
            return Highlight::plain(Some(language), Outcome::TooLarge);
        }
        match self.engine.highlight(code, language) {
            Ok(spans) => Highlight {
                language: Some(language),
                spans,
                outcome: Outcome::Highlighted,
            },
            Err(err) => {
                log::warn!("gv-highlight: {}: {err}", language.name());
                Highlight::plain(Some(language), Outcome::Failed(err.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::TokenClass;

    fn span(start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            class: TokenClass::Keyword,
        }
    }

    #[test]
    fn spans_in_ranges() {
        let h = Highlight {
            language: None,
            spans: vec![span(0, 2), span(4, 6), span(10, 12)],
            outcome: Outcome::Highlighted,
        };
        assert_eq!(h.spans_in(0..1), &[span(0, 2)]);
        assert_eq!(h.spans_in(2..4), &[]);
        assert_eq!(h.spans_in(3..11), &[span(4, 6), span(10, 12)]);
        assert_eq!(h.spans_in(12..20), &[]);
    }

    #[test]
    fn shared_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CodeHighlighter>();
        assert_send_sync::<Arc<Highlight>>();
    }
}

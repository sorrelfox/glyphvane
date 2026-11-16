// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The engine: caps, RaTeX, conversion and the layout cache.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use ratex_layout::{
    FallbackGlyphMetrics, LayoutOptions, layout, layout_with_fallback_metrics, to_display_list,
    validate_layout,
};
use ratex_parser::parser::Parser;
use ratex_types::math_style::MathStyle;

use crate::error::MathError;
use crate::fonts;
use crate::layout::{self, MathLayout};
use crate::macros::MacroSet;

/// What a render gives: a shared layout or why there is none.
pub type MathResult = Result<Arc<MathLayout>, MathError>;

/// Our caps, on top of RaTeX's own (input depth 32; an expansion budget of
/// 100k tokens, 8 MiB and 100k nodes; 65,536 layout items).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Bytes of one formula's source.
    pub max_source_bytes: usize,
    /// Bytes of a formula that uses `\ce` or `\pu` (mhchem's time is
    /// quadratic in the length and cannot be cancelled).
    pub max_chem_source_bytes: usize,
    /// Drawing items of one formula.
    pub max_items: usize,
    /// Width, and height plus depth, of one formula's ink, in em.
    pub max_extent_em: f32,
    /// Cached layouts (errors included).
    pub cache_entries: usize,
    /// Approximate bytes of cached layouts.
    pub cache_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: 4096,
            max_chem_source_bytes: 1024,
            max_items: 20_000,
            max_extent_em: 200.0,
            cache_entries: 4096,
            cache_bytes: 64 << 20,
        }
    }
}

/// Size of a glyph outside the KaTeX fonts, in em, measured by the host
/// with the font it will draw the glyph with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphMetrics {
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

/// The host's font stack, for characters the KaTeX fonts lack (CJK,
/// Cyrillic, emoji in `\text{}`). Without one, RaTeX guesses their size.
pub trait FallbackMetrics: Send + Sync {
    /// The metrics of `ch`, or `None` to let RaTeX guess.
    fn metrics(&self, ch: char) -> Option<GlyphMetrics>;
}

/// Cache counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
    pub bytes: usize,
}

struct Entry {
    tex: Box<str>,
    macros: u64,
    display: bool,
    result: MathResult,
    bytes: usize,
    used: u64,
}

#[derive(Default)]
struct Cache {
    map: HashMap<u64, Vec<Entry>>,
    tick: u64,
    stats: CacheStats,
}

impl Cache {
    fn get(&mut self, h: u64, tex: &str, macros: u64, display: bool) -> Option<MathResult> {
        self.tick += 1;
        let tick = self.tick;
        let hit = self.map.get_mut(&h).and_then(|v| {
            v.iter_mut()
                .find(|e| e.display == display && e.macros == macros && &*e.tex == tex)
        });
        match hit {
            Some(e) => {
                e.used = tick;
                self.stats.hits += 1;
                Some(e.result.clone())
            }
            None => {
                self.stats.misses += 1;
                None
            }
        }
    }

    fn insert(&mut self, h: u64, mut entry: Entry, limits: &Limits) {
        self.tick += 1;
        entry.used = self.tick;
        let bucket = self.map.entry(h).or_default();
        if bucket
            .iter()
            .any(|e| e.display == entry.display && e.macros == entry.macros && e.tex == entry.tex)
        {
            return;
        }
        self.stats.entries += 1;
        self.stats.bytes += entry.bytes;
        bucket.push(entry);
        if self.stats.entries > limits.cache_entries || self.stats.bytes > limits.cache_bytes {
            self.evict(limits);
        }
    }

    /// Drops the least recently used entries down to three quarters of
    /// both budgets.
    fn evict(&mut self, limits: &Limits) {
        let mut ages: Vec<(u64, usize)> = self
            .map
            .values()
            .flatten()
            .map(|e| (e.used, e.bytes))
            .collect();
        ages.sort_unstable();
        let (mut entries, mut bytes) = (self.stats.entries, self.stats.bytes);
        let mut cutoff = 0;
        for (used, b) in ages {
            if entries <= limits.cache_entries * 3 / 4 && bytes <= limits.cache_bytes * 3 / 4 {
                break;
            }
            entries -= 1;
            bytes -= b;
            cutoff = used;
        }
        for bucket in self.map.values_mut() {
            bucket.retain(|e| e.used > cutoff);
        }
        self.map.retain(|_, v| !v.is_empty());
        self.stats.entries = self.map.values().map(Vec::len).sum();
        self.stats.bytes = self.map.values().flatten().map(|e| e.bytes).sum();
    }
}

/// Typesets formulas with RaTeX, under caps, with a layout cache. Shared
/// between threads: render on the background executor, look up from the
/// UI thread with [`MathEngine::cached`].
///
/// Layouts are in em with theme colours left symbolic
/// ([`MathColor::Current`](crate::MathColor::Current)), so the cache key is
/// the source, the macro set and the style only: font size, zoom and
/// theme apply when painting.
pub struct MathEngine {
    limits: Limits,
    fallback: Option<Arc<dyn FallbackMetrics>>,
    cache: Mutex<Cache>,
}

impl std::fmt::Debug for MathEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MathEngine")
            .field("limits", &self.limits)
            .field("fallback", &self.fallback.is_some())
            .field("cache", &self.stats())
            .finish()
    }
}

impl Default for MathEngine {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

fn key_hash(tex: &str, macros: u64, display: bool) -> u64 {
    let mut h = DefaultHasher::new();
    tex.hash(&mut h);
    macros.hash(&mut h);
    display.hash(&mut h);
    h.finish()
}

impl MathEngine {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            fallback: None,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Measures glyphs outside the KaTeX fonts with the host's fonts.
    #[must_use]
    pub fn with_fallback(mut self, fallback: Arc<dyn FallbackMetrics>) -> Self {
        self.fallback = Some(fallback);
        self
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cached result for a formula, without typesetting it.
    pub fn cached(&self, tex: &str, display: bool, macros: &MacroSet) -> Option<MathResult> {
        let m = macros.hash_value();
        self.cache().get(key_hash(tex, m, display), tex, m, display)
    }

    /// Typesets `tex` (the formula without its delimiters) in display or
    /// text style with document `macros`, through the cache.
    pub fn render(&self, tex: &str, display: bool, macros: &MacroSet) -> MathResult {
        let m = macros.hash_value();
        let h = key_hash(tex, m, display);
        if let Some(hit) = self.cache().get(h, tex, m, display) {
            return hit;
        }
        let result =
            typeset(tex, display, macros, &self.limits, self.fallback.as_deref()).map(Arc::new);
        let bytes = tex.len()
            + match &result {
                Ok(l) => l.approx_bytes(),
                Err(_) => 128,
            };
        self.cache().insert(
            h,
            Entry {
                tex: tex.into(),
                macros: m,
                display,
                result: result.clone(),
                bytes,
                used: 0,
            },
            &self.limits,
        );
        result
    }

    pub fn stats(&self) -> CacheStats {
        self.cache().stats
    }

    /// Empties the cache (counters stay).
    pub fn clear(&self) {
        let mut c = self.cache();
        c.map.clear();
        c.stats.entries = 0;
        c.stats.bytes = 0;
    }
}

/// Whether the source uses mhchem.
fn uses_chemistry(tex: &str) -> bool {
    tex.contains("\\ce") || tex.contains("\\pu")
}

/// Typesets one formula, uncached.
pub fn typeset(
    tex: &str,
    display: bool,
    macros: &MacroSet,
    limits: &Limits,
    fallback: Option<&dyn FallbackMetrics>,
) -> Result<MathLayout, MathError> {
    let len = tex.len();
    if len > limits.max_source_bytes {
        return Err(MathError::TooLong {
            len,
            max: limits.max_source_bytes,
            chemistry: false,
        });
    }
    let chemistry = uses_chemistry(tex) || macros.iter().any(|(_, b)| uses_chemistry(b));
    if chemistry && len > limits.max_chem_source_bytes {
        return Err(MathError::TooLong {
            len,
            max: limits.max_chem_source_bytes,
            chemistry: true,
        });
    }

    let metrics = fallback.map(|fb| fallback_metrics(tex, macros, fb));
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };

    let run = || -> Result<_, MathError> {
        let mut parser = Parser::new(tex);
        for (name, body) in macros.iter() {
            parser.gullet.set_text_macro(name, body);
        }
        let nodes = parser.parse().map_err(|e| MathError::Parse {
            span: e.loc.map(|l| l.start..l.end.max(l.start)),
            message: e.message,
        })?;
        let mut options = LayoutOptions::default().with_style(style);
        options.color = layout::CURRENT;
        let tree = match metrics {
            Some(m) if !m.is_empty() => layout_with_fallback_metrics(&nodes, &options, Arc::new(m)),
            _ => layout(&nodes, &options),
        };
        validate_layout(&tree).map_err(|_| MathError::TooComplex {
            items: ratex_layout::MAX_LAYOUT_OUTPUT_ITEMS + 1,
            max: limits.max_items.min(ratex_layout::MAX_LAYOUT_OUTPUT_ITEMS),
        })?;
        Ok(to_display_list(&tree))
    };
    let list = catch_unwind(AssertUnwindSafe(run)).map_err(|_| MathError::Internal)??;

    if list.items.len() > limits.max_items {
        return Err(MathError::TooComplex {
            items: list.items.len(),
            max: limits.max_items,
        });
    }
    // Check the box before converting: `\rule` and `\kern` sizes are
    // unbounded in RaTeX.
    let box_w = list.width.abs();
    let box_h = (list.height + list.depth).abs();
    let max = f64::from(limits.max_extent_em);
    if !(box_w <= max && box_h <= max) {
        return Err(too_large(box_w, box_h, limits));
    }
    let out = catch_unwind(AssertUnwindSafe(|| layout::convert(&list, display)))
        .map_err(|_| MathError::Internal)?;
    if let Some(ink) = out.ink
        && !(ink.width() <= limits.max_extent_em && ink.height() <= limits.max_extent_em)
    {
        return Err(too_large(
            f64::from(ink.width()),
            f64::from(ink.height()),
            limits,
        ));
    }
    Ok(out)
}

fn too_large(w: f64, h: f64, limits: &Limits) -> MathError {
    #[allow(clippy::cast_possible_truncation)]
    let clamp = |v: f64| if v.is_finite() { v as f32 } else { f32::MAX };
    MathError::TooLarge {
        width: clamp(w),
        height: clamp(h),
        max: limits.max_extent_em,
    }
}

/// Host metrics for every character of the source and macros that no
/// KaTeX face has.
fn fallback_metrics(
    tex: &str,
    macros: &MacroSet,
    fb: &dyn FallbackMetrics,
) -> HashMap<char, FallbackGlyphMetrics> {
    let mut out = HashMap::new();
    for ch in tex.chars().chain(macros.chars()) {
        if ch.is_ascii() || out.contains_key(&ch) || fonts::covered(ch) {
            continue;
        }
        if let Some(m) = fb.metrics(ch) {
            out.insert(
                ch,
                FallbackGlyphMetrics {
                    width: f64::from(m.width),
                    height: f64::from(m.height),
                    depth: f64::from(m.depth),
                },
            );
        }
    }
    out
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Caches: shaped runs under a byte budget (shared by all blocks), and
//! layouts at a few recent widths per block.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use crate::layout::{Layout, sanitize_width};
use crate::measure::{FontMetrics, Measure, ShapedRun};
use crate::paragraph::Paragraph;
use crate::shaped::{EPS, Shaped};
use crate::style::StyleId;

const NIL: usize = usize::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct RunKey {
    text_hash: u64,
    font: StyleId,
    size_bits: u32,
}

impl RunKey {
    fn new(text: &str, font: StyleId, size: f32) -> Self {
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        Self {
            text_hash: h.finish(),
            font,
            size_bits: size.to_bits(),
        }
    }
}

#[derive(Debug)]
struct Node<G> {
    key: RunKey,
    /// Kept to rule out hash collisions.
    text: Box<str>,
    run: Arc<ShapedRun<G>>,
    bytes: usize,
    prev: usize,
    next: usize,
}

/// Hit and miss counters of a [`RunCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// Shaped runs keyed by (text hash, [`StyleId`], size), least recently used
/// first out once the entries exceed a byte budget. Also caches font
/// metrics per (style, size).
///
/// Shared by all blocks of a document (or the whole app). Evicting a run
/// only drops the cache's reference: layouts that use it keep it alive.
/// Colour, decorations and letter spacing are not part of the key, so a
/// link and plain text in the same font share a run.
#[derive(Debug)]
pub struct RunCache<G = ()> {
    map: HashMap<RunKey, usize>,
    nodes: Vec<Option<Node<G>>>,
    free: Vec<usize>,
    /// Most recently used.
    head: usize,
    /// Least recently used.
    tail: usize,
    bytes: usize,
    budget: usize,
    metrics: HashMap<(StyleId, u32), FontMetrics>,
    stats: RunCacheStats,
}

impl<G> Default for RunCache<G> {
    fn default() -> Self {
        Self::new(Self::DEFAULT_BUDGET)
    }
}

impl<G> RunCache<G> {
    /// 16 MiB: about 5,000 runs of the average size measured with GPUI.
    pub const DEFAULT_BUDGET: usize = 16 << 20;

    pub fn new(budget_bytes: usize) -> Self {
        Self {
            map: HashMap::new(),
            nodes: Vec::new(),
            free: Vec::new(),
            head: NIL,
            tail: NIL,
            bytes: 0,
            budget: budget_bytes,
            metrics: HashMap::new(),
            stats: RunCacheStats::default(),
        }
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Changes the budget, evicting at once if the entries exceed it.
    pub fn set_budget(&mut self, budget_bytes: usize) {
        self.budget = budget_bytes;
        self.evict();
    }

    /// Bytes held by the cached runs (approximate, see
    /// [`ShapedRun::heap_bytes`]).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn stats(&self) -> RunCacheStats {
        self.stats
    }

    /// Drops every run and all metrics (for example after a font change).
    pub fn clear(&mut self) {
        self.map.clear();
        self.nodes.clear();
        self.free.clear();
        self.head = NIL;
        self.tail = NIL;
        self.bytes = 0;
        self.metrics.clear();
    }

    /// The cached run for `text`, if any; marks it as recently used.
    pub fn get(&mut self, text: &str, font: StyleId, size: f32) -> Option<Arc<ShapedRun<G>>> {
        let key = RunKey::new(text, font, size);
        let ix = *self.map.get(&key)?;
        let node = self.nodes[ix].as_ref()?;
        if &*node.text != text {
            return None;
        }
        let run = node.run.clone();
        self.touch(ix);
        Some(run)
    }

    /// The run for `text` from the cache, or shaped by `measure` and added.
    pub fn shape<M>(
        &mut self,
        measure: &mut M,
        text: &str,
        font: StyleId,
        size: f32,
    ) -> Arc<ShapedRun<G>>
    where
        M: Measure<Glyphs = G> + ?Sized,
    {
        if let Some(run) = self.get(text, font, size) {
            self.stats.hits += 1;
            return run;
        }
        self.stats.misses += 1;
        let run = Arc::new(measure.shape(text, font, size));
        self.insert(RunKey::new(text, font, size), text, run.clone());
        run
    }

    /// Primary-font metrics of `font` at `size`, cached.
    pub fn metrics<M>(&mut self, measure: &mut M, font: StyleId, size: f32) -> FontMetrics
    where
        M: Measure<Glyphs = G> + ?Sized,
    {
        *self
            .metrics
            .entry((font, size.to_bits()))
            .or_insert_with(|| measure.metrics(font, size).sanitized())
    }

    fn insert(&mut self, key: RunKey, text: &str, run: Arc<ShapedRun<G>>) {
        if let Some(old) = self.map.remove(&key) {
            self.remove_node(old);
        }
        let bytes = run.heap_bytes() + text.len() + size_of::<Node<G>>() + 2 * size_of::<RunKey>();
        let node = Node {
            key,
            text: text.into(),
            run,
            bytes,
            prev: NIL,
            next: NIL,
        };
        let ix = match self.free.pop() {
            Some(ix) => {
                self.nodes[ix] = Some(node);
                ix
            }
            None => {
                self.nodes.push(Some(node));
                self.nodes.len() - 1
            }
        };
        self.map.insert(key, ix);
        self.bytes += bytes;
        self.push_front(ix);
        self.evict();
    }

    /// Evicts least recently used runs until the budget holds, keeping at
    /// least the newest one.
    fn evict(&mut self) {
        while self.bytes > self.budget && self.map.len() > 1 && self.tail != NIL {
            let ix = self.tail;
            if let Some(key) = self.nodes[ix].as_ref().map(|n| n.key) {
                self.map.remove(&key);
            }
            self.remove_node(ix);
            self.stats.evictions += 1;
        }
    }

    fn remove_node(&mut self, ix: usize) {
        self.unlink(ix);
        if let Some(node) = self.nodes[ix].take() {
            self.bytes -= node.bytes;
            self.free.push(ix);
        }
    }

    fn touch(&mut self, ix: usize) {
        if self.head != ix {
            self.unlink(ix);
            self.push_front(ix);
        }
    }

    fn links(&self, ix: usize) -> (usize, usize) {
        self.nodes[ix]
            .as_ref()
            .map_or((NIL, NIL), |n| (n.prev, n.next))
    }

    fn set_prev(&mut self, ix: usize, prev: usize) {
        if let Some(n) = self.nodes[ix].as_mut() {
            n.prev = prev;
        }
    }

    fn set_next(&mut self, ix: usize, next: usize) {
        if let Some(n) = self.nodes[ix].as_mut() {
            n.next = next;
        }
    }

    fn unlink(&mut self, ix: usize) {
        let (prev, next) = self.links(ix);
        if prev == NIL {
            if self.head == ix {
                self.head = next;
            }
        } else {
            self.set_next(prev, next);
        }
        if next == NIL {
            if self.tail == ix {
                self.tail = prev;
            }
        } else {
            self.set_prev(next, prev);
        }
        self.set_prev(ix, NIL);
        self.set_next(ix, NIL);
    }

    fn push_front(&mut self, ix: usize) {
        let old = self.head;
        self.set_prev(ix, NIL);
        self.set_next(ix, old);
        if old != NIL {
            self.set_prev(old, ix);
        }
        self.head = ix;
        if self.tail == NIL {
            self.tail = ix;
        }
    }
}

/// A few values keyed by layout width, most recently used first.
///
/// GPUI's layout engine measures a block at a max-content probe width and
/// then at its real width every frame, so one slot would re-break the lines
/// twice a frame; a handful of slots does not. Widths within 0.01 px match.
#[derive(Debug, Clone)]
pub struct WidthCache<T> {
    slots: Vec<(f32, T)>,
    capacity: usize,
}

impl<T> Default for WidthCache<T> {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

impl<T> WidthCache<T> {
    pub const DEFAULT_CAPACITY: usize = 4;

    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            slots: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The value for `width`, now the most recently used.
    pub fn get(&mut self, width: f32) -> Option<&T> {
        let ix = self.slots.iter().position(|(w, _)| same_width(*w, width))?;
        if ix > 0 {
            let slot = self.slots.remove(ix);
            self.slots.insert(0, slot);
        }
        self.slots.first().map(|(_, v)| v)
    }

    /// Stores `value` for `width`, replacing an entry for the same width or
    /// evicting the least recently used one.
    pub fn insert(&mut self, width: f32, value: T) {
        if let Some(ix) = self.slots.iter().position(|(w, _)| same_width(*w, width)) {
            self.slots.remove(ix);
        } else if self.slots.len() >= self.capacity {
            self.slots.pop();
        }
        self.slots.insert(0, (width, value));
    }

    pub fn clear(&mut self) {
        self.slots.clear();
    }

    /// `(width, value)` pairs, most recently used first.
    pub fn iter(&self) -> impl Iterator<Item = (f32, &T)> {
        self.slots.iter().map(|(w, v)| (*w, v))
    }
}

fn same_width(a: f32, b: f32) -> bool {
    a == b || (a - b).abs() < EPS
}

/// Counters of a [`BlockCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockCacheStats {
    /// Paragraph shapings (each one looks its runs up in the run cache).
    pub shapings: u64,
    /// Layouts computed.
    pub layouts: u64,
    /// Layouts served from the width cache.
    pub hits: u64,
}

/// The layout state of one block: its paragraph, its shaping (made on
/// first use) and its layouts at the last few widths.
#[derive(Debug)]
pub struct BlockCache<G = ()> {
    paragraph: Arc<Paragraph>,
    shaped: Option<Arc<Shaped<G>>>,
    layouts: WidthCache<Arc<Layout<G>>>,
    stats: BlockCacheStats,
}

impl<G> BlockCache<G> {
    pub fn new(paragraph: Arc<Paragraph>) -> Self {
        Self::with_widths(paragraph, WidthCache::<()>::DEFAULT_CAPACITY)
    }

    /// A cache that keeps layouts for `widths` widths.
    pub fn with_widths(paragraph: Arc<Paragraph>, widths: usize) -> Self {
        Self {
            paragraph,
            shaped: None,
            layouts: WidthCache::new(widths),
            stats: BlockCacheStats::default(),
        }
    }

    pub fn paragraph(&self) -> &Arc<Paragraph> {
        &self.paragraph
    }

    /// Replaces the content; shaping and layouts are dropped.
    pub fn set_paragraph(&mut self, paragraph: Arc<Paragraph>) {
        self.paragraph = paragraph;
        self.invalidate();
    }

    /// Drops shaping and layouts (fonts or scale changed).
    pub fn invalidate(&mut self) {
        self.shaped = None;
        self.layouts.clear();
    }

    pub fn stats(&self) -> BlockCacheStats {
        self.stats
    }

    /// The shaping, made on first use.
    pub fn shaped<M>(&mut self, measure: &mut M, runs: &mut RunCache<G>) -> Arc<Shaped<G>>
    where
        M: Measure<Glyphs = G> + ?Sized,
    {
        if let Some(shaped) = &self.shaped {
            return shaped.clone();
        }
        self.stats.shapings += 1;
        let shaped = Arc::new(Shaped::new(self.paragraph.clone(), measure, runs));
        self.shaped = Some(shaped.clone());
        shaped
    }

    /// The layout at `width`: cached, or broken into lines from the cached
    /// shaping (shaping first if needed).
    pub fn layout<M>(
        &mut self,
        width: f32,
        measure: &mut M,
        runs: &mut RunCache<G>,
    ) -> Arc<Layout<G>>
    where
        M: Measure<Glyphs = G> + ?Sized,
    {
        let width = sanitize_width(width);
        if let Some(layout) = self.cached(width) {
            return layout;
        }
        let shaped = self.shaped(measure, runs);
        self.stats.layouts += 1;
        let layout = Arc::new(Layout::new(shaped, width));
        self.layouts.insert(width, layout.clone());
        layout
    }

    /// The layout at `width` if it is cached; never shapes.
    pub fn cached(&mut self, width: f32) -> Option<Arc<Layout<G>>> {
        let layout = self.layouts.get(sanitize_width(width)).cloned();
        if layout.is_some() {
            self.stats.hits += 1;
        }
        layout
    }

    /// The most recently used layout, at any width.
    pub fn last_layout(&self) -> Option<&Arc<Layout<G>>> {
        self.layouts.iter().next().map(|(_, l)| l)
    }

    /// Approximate heap memory held by this block's shaping and layouts,
    /// without the shared runs and the paragraph.
    pub fn heap_bytes(&self) -> usize {
        self.shaped.as_ref().map_or(0, |s| s.heap_bytes())
            + self
                .layouts
                .iter()
                .map(|(_, l)| l.heap_bytes())
                .sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::FakeMono;

    fn run_bytes(cache: &mut RunCache, m: &mut FakeMono, text: &str) -> usize {
        let before = cache.bytes();
        cache.shape(m, text, StyleId(0), 10.0);
        cache.bytes() - before
    }

    #[test]
    fn hits_do_not_reshape() {
        let mut m = FakeMono::default();
        let mut cache = RunCache::default();
        let a = cache.shape(&mut m, "hello", StyleId(1), 10.0);
        let b = cache.shape(&mut m, "hello", StyleId(1), 10.0);
        assert!(Arc::ptr_eq(&a, &b));
        cache.shape(&mut m, "hello", StyleId(2), 10.0);
        cache.shape(&mut m, "hello", StyleId(1), 11.0);
        assert_eq!(m.shaped, 3);
        assert_eq!(
            cache.stats(),
            RunCacheStats {
                hits: 1,
                misses: 3,
                evictions: 0
            }
        );
    }

    #[test]
    fn byte_budget_evicts_least_recently_used() {
        let mut m = FakeMono::default();
        let mut probe = RunCache::default();
        let one = run_bytes(&mut probe, &mut m, "aaaa");
        // Room for three runs of this size.
        let mut cache = RunCache::new(3 * one + one / 2);
        for t in ["aaaa", "bbbb", "cccc"] {
            cache.shape(&mut m, t, StyleId(0), 10.0);
        }
        assert_eq!(cache.len(), 3);
        // Touch "aaaa"; "bbbb" is now the oldest.
        assert!(cache.get("aaaa", StyleId(0), 10.0).is_some());
        cache.shape(&mut m, "dddd", StyleId(0), 10.0);
        assert_eq!(cache.len(), 3);
        assert!(cache.get("bbbb", StyleId(0), 10.0).is_none());
        assert!(cache.get("aaaa", StyleId(0), 10.0).is_some());
        assert!(cache.get("cccc", StyleId(0), 10.0).is_some());
        assert!(cache.bytes() <= cache.budget());
        assert_eq!(cache.stats().evictions, 1);
        // Shrinking the budget evicts at once but keeps the newest entry.
        cache.set_budget(0);
        assert_eq!(cache.len(), 1);
        assert!(cache.get("cccc", StyleId(0), 10.0).is_some());
        cache.clear();
        assert_eq!((cache.len(), cache.bytes()), (0, 0));
    }

    #[test]
    fn slots_are_reused_after_eviction() {
        let mut m = FakeMono::default();
        let mut cache = RunCache::new(1);
        for i in 0..100 {
            cache.shape(&mut m, &format!("run {i}"), StyleId(0), 10.0);
        }
        assert_eq!(cache.len(), 1);
        assert!(cache.nodes.len() <= 2, "{}", cache.nodes.len());
    }

    #[test]
    fn width_cache_keeps_recent_widths() {
        let mut c = WidthCache::new(2);
        c.insert(100.0, 'a');
        c.insert(200.0, 'b');
        assert_eq!(c.get(100.004), Some(&'a'));
        c.insert(300.0, 'c');
        assert_eq!(c.get(200.0), None, "least recently used went first");
        assert_eq!(c.get(f32::INFINITY), None);
        c.insert(f32::INFINITY, 'i');
        assert_eq!(c.get(f32::INFINITY), Some(&'i'));
        assert_eq!(c.len(), 2);
    }
}

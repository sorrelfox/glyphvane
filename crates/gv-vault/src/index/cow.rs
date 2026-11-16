// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Copy-on-write containers for index snapshots.
//!
//! A snapshot clone copies only a short vector of `Arc`s. A change then
//! clones just the one shard (or chunk) it touches, through
//! [`Arc::make_mut`], so an incremental update costs about `n / SHARDS`
//! instead of `n`, and published snapshots are never mutated.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};
use std::sync::Arc;

/// Number of shards per map. A power of two.
const SHARDS: usize = 256;

/// Entries per chunk of a [`CowTable`].
const CHUNK: usize = 1024;

/// A fixed hasher for shard selection, so shard placement does not depend on
/// per-process random state.
#[derive(Debug, Clone, Copy, Default)]
struct FixedState;

impl BuildHasher for FixedState {
    type Hasher = std::hash::DefaultHasher;
    fn build_hasher(&self) -> Self::Hasher {
        std::hash::DefaultHasher::new()
    }
}

/// A hash map split into `Arc`-shared shards.
#[derive(Debug)]
pub(crate) struct CowMap<K, V> {
    shards: Vec<Arc<HashMap<K, V>>>,
    len: usize,
}

impl<K, V> Clone for CowMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            len: self.len,
        }
    }
}

impl<K: Hash + Eq + Clone, V: Clone> CowMap<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Arc::new(HashMap::new())).collect(),
            len: 0,
        }
    }

    fn shard<Q: Hash + ?Sized>(key: &Q) -> usize {
        // Truncation is intended: only the low bits pick the shard.
        #[expect(clippy::cast_possible_truncation, reason = "masked to the shard count")]
        let h = FixedState.hash_one(key) as usize;
        h & (SHARDS - 1)
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.shards[Self::shard(key)].get(key)
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let shard = Arc::make_mut(&mut self.shards[Self::shard(&key)]);
        let old = shard.insert(key, value);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = Self::shard(key);
        if !self.shards[i].contains_key(key) {
            return None;
        }
        let old = Arc::make_mut(&mut self.shards[i]).remove(key);
        if old.is_some() {
            self.len -= 1;
        }
        old
    }

    /// Every entry, in no particular order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.shards.iter().flat_map(|s| s.iter())
    }

    /// Mutable access to an existing value.
    pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let i = Self::shard(key);
        if !self.shards[i].contains_key(key) {
            return None;
        }
        Arc::make_mut(&mut self.shards[i]).get_mut(key)
    }
}

/// The values of one multimap key. A single value is stored inline and
/// several share one allocation, so cloning a shard never allocates per
/// value.
#[derive(Debug, Clone)]
enum List<T> {
    One(T),
    Many(Arc<Vec<T>>),
}

impl<T: Clone + PartialEq> List<T> {
    fn as_slice(&self) -> &[T] {
        match self {
            Self::One(t) => std::slice::from_ref(t),
            Self::Many(v) => v,
        }
    }

    fn push(&mut self, value: T) {
        if self.as_slice().contains(&value) {
            return;
        }
        match self {
            Self::One(t) => *self = Self::Many(Arc::new(vec![t.clone(), value])),
            Self::Many(v) => Arc::make_mut(v).push(value),
        }
    }

    /// Removes `value`; returns `true` if the list is now empty.
    fn remove(&mut self, value: &T) -> bool {
        match self {
            Self::One(t) => t == value,
            Self::Many(v) => {
                let v = Arc::make_mut(v);
                v.retain(|x| x != value);
                match v.len() {
                    0 => true,
                    1 => {
                        *self = Self::One(v[0].clone());
                        false
                    }
                    _ => false,
                }
            }
        }
    }
}

/// A multimap `K -> [T]` on top of [`CowMap`].
#[derive(Debug)]
pub(crate) struct CowMultiMap<K, T> {
    map: CowMap<K, List<T>>,
}

impl<K, T> Clone for CowMultiMap<K, T> {
    fn clone(&self) -> Self {
        Self {
            map: self.map.clone(),
        }
    }
}

impl<K: Hash + Eq + Clone, T: Clone + PartialEq> CowMultiMap<K, T> {
    pub(crate) fn new() -> Self {
        Self { map: CowMap::new() }
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> &[T]
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get(key).map_or(&[], List::as_slice)
    }

    pub(crate) fn add(&mut self, key: K, value: T) {
        match self.map.get_mut(&key) {
            Some(list) => list.push(value),
            None => {
                self.map.insert(key, List::One(value));
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &K, value: &T) {
        if let Some(list) = self.map.get_mut(key)
            && list.remove(value)
        {
            self.map.remove(key);
        }
    }

    #[cfg(test)]
    pub(crate) fn key_count(&self) -> usize {
        self.map.len()
    }
}

/// A growable table `index -> Option<T>` in `Arc`-shared chunks.
#[derive(Debug)]
pub(crate) struct CowTable<T> {
    chunks: Vec<Arc<Vec<Option<T>>>>,
    len: usize,
}

impl<T> Clone for CowTable<T> {
    fn clone(&self) -> Self {
        Self {
            chunks: self.chunks.clone(),
            len: self.len,
        }
    }
}

impl<T: Clone> CowTable<T> {
    pub(crate) fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    /// Number of slots (including empty ones).
    #[cfg(test)]
    pub(crate) fn slots(&self) -> usize {
        self.len
    }

    pub(crate) fn get(&self, index: usize) -> Option<&T> {
        self.chunks.get(index / CHUNK)?.get(index % CHUNK)?.as_ref()
    }

    /// Sets slot `index`, growing the table as needed.
    pub(crate) fn set(&mut self, index: usize, value: Option<T>) {
        while self.chunks.len() <= index / CHUNK {
            self.chunks.push(Arc::new(Vec::with_capacity(CHUNK)));
        }
        let chunk = Arc::make_mut(&mut self.chunks[index / CHUNK]);
        let slot = index % CHUNK;
        if chunk.len() <= slot {
            chunk.resize_with(slot + 1, || None);
        }
        chunk[slot] = value;
        self.len = self.len.max(index + 1);
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks
            .iter()
            .flat_map(|c| c.iter().filter_map(Option::as_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_clone_is_isolated() {
        let mut a: CowMap<String, u32> = CowMap::new();
        for i in 0..1000 {
            a.insert(format!("k{i}"), i);
        }
        let snapshot = a.clone();
        a.insert("k1".into(), 99);
        a.remove("k2");
        assert_eq!(snapshot.get("k1"), Some(&1));
        assert_eq!(snapshot.get("k2"), Some(&2));
        assert_eq!(a.get("k1"), Some(&99));
        assert_eq!(a.get("k2"), None);
        assert_eq!(a.len(), 999);
        assert_eq!(snapshot.len(), 1000);
    }

    #[test]
    fn multimap() {
        let mut m: CowMultiMap<String, u32> = CowMultiMap::new();
        m.add("a".into(), 1);
        m.add("a".into(), 2);
        m.add("a".into(), 2);
        assert_eq!(m.get("a"), &[1, 2]);
        let snap = m.clone();
        m.remove(&"a".to_string(), &1);
        assert_eq!(m.get("a"), &[2]);
        assert_eq!(snap.get("a"), &[1, 2]);
        m.remove(&"a".to_string(), &2);
        assert!(m.get("a").is_empty());
        assert_eq!(m.key_count(), 0);
    }

    #[test]
    fn table() {
        let mut t: CowTable<u32> = CowTable::new();
        t.set(0, Some(1));
        t.set(3000, Some(2));
        let snap = t.clone();
        t.set(0, None);
        assert_eq!(snap.get(0), Some(&1));
        assert_eq!(t.get(0), None);
        assert_eq!(t.get(3000), Some(&2));
        assert_eq!(t.slots(), 3001);
        assert_eq!(t.iter().count(), 1);
    }
}

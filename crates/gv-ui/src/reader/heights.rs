// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The height index: one slot per virtual item with its height (measured or
//! estimated) in a `SumTree`. Prefix sums, "which item is at y" and point
//! updates are O(log n); measured heights are written back in one batched
//! range update per frame.

use std::cmp::Ordering;
use sum_tree::{Bias, ContextLessSummary, Dimension, SeekTarget, SumTree};

/// The height of one item, gap above it included.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Slot {
    pub height: f32,
    /// Laid out at the current width (else an estimate).
    pub measured: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Summary {
    count: usize,
    measured: usize,
    /// f64, so that 100k additions do not drift.
    height: f64,
}

impl sum_tree::Item for Slot {
    type Summary = Summary;

    fn summary(&self, _: ()) -> Summary {
        Summary {
            count: 1,
            measured: usize::from(self.measured),
            height: f64::from(self.height),
        }
    }
}

impl ContextLessSummary for Summary {
    fn zero() -> Self {
        Self::default()
    }

    fn add_summary(&mut self, s: &Self) {
        self.count += s.count;
        self.measured += s.measured;
        self.height += s.height;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Count(usize);

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Height(f64);

impl Dimension<'_, Summary> for Count {
    fn zero(_: ()) -> Self {
        Self(0)
    }

    fn add_summary(&mut self, s: &Summary, _: ()) {
        self.0 += s.count;
    }
}

impl SeekTarget<'_, Summary, Summary> for Count {
    fn cmp(&self, other: &Summary, _: ()) -> Ordering {
        Ord::cmp(&self.0, &other.count)
    }
}

impl SeekTarget<'_, Summary, Summary> for Height {
    fn cmp(&self, other: &Summary, _: ()) -> Ordering {
        self.0.total_cmp(&other.height)
    }
}

/// Heights of all items, in document order.
#[derive(Debug, Clone, Default)]
pub(crate) struct Heights {
    tree: SumTree<Slot>,
    /// Batched updates written by [`Self::flush`].
    pub updates: u64,
}

impl Heights {
    pub fn new(slots: impl IntoIterator<Item = Slot>) -> Self {
        Self {
            tree: SumTree::from_iter(slots, ()),
            updates: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.tree.summary().count
    }

    /// Sum of all heights.
    pub fn total(&self) -> f64 {
        self.tree.summary().height
    }

    /// Number of measured slots.
    #[cfg(test)]
    pub fn measured(&self) -> usize {
        self.tree.summary().measured
    }

    /// Top of item `ix` (the total for `ix >= len`).
    pub fn top_of(&self, ix: usize) -> f64 {
        let (start, _, _) = self.tree.find::<Summary, _>((), &Count(ix), Bias::Right);
        start.height
    }

    /// The item containing `y`, and `y`'s offset inside it. Past the end:
    /// the last item; before the start: the first.
    pub fn at_y(&self, y: f64) -> (usize, f64) {
        let n = self.len();
        if n == 0 {
            return (0, 0.0);
        }
        let y = y.max(0.0);
        let (start, _, item) = self.tree.find::<Summary, _>((), &Height(y), Bias::Right);
        if item.is_none() || start.count >= n {
            let top = self.top_of(n - 1);
            return (n - 1, y - top);
        }
        (start.count, y - start.height)
    }

    pub fn get(&self, ix: usize) -> Option<Slot> {
        let (_, _, item) = self.tree.find::<Summary, _>((), &Count(ix), Bias::Right);
        item.copied()
    }

    /// Writes `updates` (`(index, slot)`, any order, later entries win) in
    /// one rebuild of the range they span.
    pub fn write(&mut self, updates: &mut Vec<(usize, Slot)>) {
        let n = self.len();
        updates.retain(|(ix, _)| *ix < n);
        if updates.is_empty() {
            return;
        }
        // Stable sort, then keep the last write of each index.
        updates.sort_by_key(|(ix, _)| *ix);
        let mut deduped: Vec<(usize, Slot)> = Vec::with_capacity(updates.len());
        for &(ix, slot) in updates.iter() {
            match deduped.last_mut() {
                Some(last) if last.0 == ix => last.1 = slot,
                _ => deduped.push((ix, slot)),
            }
        }
        updates.clear();
        let lo = deduped[0].0;
        let hi = deduped[deduped.len() - 1].0;
        let mut cursor = self.tree.cursor::<Count>(());
        let mut new = cursor.slice(&Count(lo), Bias::Right);
        let mut next = deduped.iter().peekable();
        let mut slots = Vec::with_capacity(hi - lo + 1);
        for ix in lo..=hi {
            let Some(old) = cursor.item().copied() else {
                break;
            };
            cursor.next();
            let slot = match next.peek() {
                Some((at, slot)) if *at == ix => {
                    next.next();
                    *slot
                }
                _ => old,
            };
            slots.push(slot);
        }
        new.extend(slots, ());
        new.append(cursor.suffix(), ());
        drop(cursor);
        self.tree = new;
        self.updates += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(height: f32) -> Slot {
        Slot {
            height,
            measured: false,
        }
    }

    #[test]
    fn prefix_sums_and_lookup() {
        let h = Heights::new([10.0, 20.0, 30.0].map(slot));
        assert_eq!(h.len(), 3);
        assert_eq!(h.total(), 60.0);
        assert_eq!(h.top_of(0), 0.0);
        assert_eq!(h.top_of(2), 30.0);
        assert_eq!(h.top_of(3), 60.0);
        assert_eq!(h.at_y(0.0), (0, 0.0));
        assert_eq!(h.at_y(15.0), (1, 5.0));
        assert_eq!(h.at_y(30.0), (2, 0.0));
        assert_eq!(h.at_y(100.0), (2, 70.0));
        assert_eq!(h.at_y(-5.0), (0, 0.0));
    }

    #[test]
    fn batched_writes_touch_only_their_slots() {
        let mut h = Heights::new((0..100).map(|_| slot(1.0)));
        let measured = |height| Slot {
            height,
            measured: true,
        };
        let mut updates = vec![
            (50, measured(3.0)),
            (10, measured(2.0)),
            (50, measured(4.0)),
        ];
        h.write(&mut updates);
        assert!(updates.is_empty());
        assert_eq!(h.updates, 1);
        assert_eq!(h.get(10), Some(measured(2.0)));
        assert_eq!(h.get(50), Some(measured(4.0)));
        assert_eq!(h.get(11), Some(slot(1.0)));
        assert_eq!(h.len(), 100);
        assert_eq!(h.total(), 98.0 + 2.0 + 4.0);
        assert_eq!(h.measured(), 2);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use std::num::NonZeroU32;
use std::ops::Range;

use gv_text::{Anchor, Buffer, EditSummary};
use rand::Rng;
use rand::rngs::StdRng;

use super::*;
use crate::display_map::test_support::{ColumnMeasure, for_each_seed, random_edit, random_text};
use crate::display_map::{ReplaceMap, TabMap, WrapMap};

const TAB: NonZeroU32 = NonZeroU32::new(4).unwrap();

fn px(px: f32) -> Height {
    Height::from_px(px)
}

/// A buffer, every layer over it, and a block layer on top.
struct Layers {
    buffer: Buffer,
    replace_map: ReplaceMap,
    tab_map: TabMap,
    wrap_map: Option<WrapMap>,
    blocks: BlockMap,
}

impl Layers {
    fn new(text: &str, wrap_width: Option<f32>) -> Self {
        let buffer = Buffer::new(text);
        let (replace_map, replace) = ReplaceMap::new(buffer.snapshot());
        let (tab_map, tabs) = TabMap::new(replace, TAB);
        let (wrap_map, input) = match wrap_width {
            Some(width) => {
                let (map, wrap) = WrapMap::new(tabs, Box::new(ColumnMeasure), Some(width));
                (Some(map), BlockInput::Wrap(wrap))
            }
            None => (None, BlockInput::Tab(tabs)),
        };
        Self {
            buffer,
            replace_map,
            tab_map,
            wrap_map,
            blocks: BlockMap::new(input, px(20.0)),
        }
    }

    fn snapshot(&self) -> &BlockSnapshot {
        self.blocks.snapshot()
    }

    /// Passes a replace-layer change up the layers and into the blocks.
    fn apply_replace(
        &mut self,
        replace: crate::display_map::ReplaceSnapshot,
        edits: Vec<crate::display_map::ReplaceEdit>,
        text: Option<&EditSummary>,
    ) -> BlockChanges {
        let (tabs, tab_edits) = self.tab_map.sync(replace, edits, TAB);
        match &mut self.wrap_map {
            Some(map) => {
                let (wrap, wrap_edits) = map.sync(tabs, tab_edits);
                self.blocks.sync_wrap(wrap, &wrap_edits, text)
            }
            None => self.blocks.sync_tab(tabs, &tab_edits, text),
        }
    }

    fn edit(&mut self, range: Range<usize>, text: &str) -> BlockChanges {
        let summary = self.buffer.edit([(range, text)]).expect("a valid edit");
        self.sync(&summary)
    }

    fn sync(&mut self, summary: &EditSummary) -> BlockChanges {
        let (replace, edits) = self.replace_map.sync(self.buffer.snapshot(), summary);
        self.apply_replace(replace, edits, Some(summary))
    }

    fn fold(&mut self, range: Range<usize>) {
        let (replace, edits) = self.replace_map.fold([range]);
        self.apply_replace(replace, edits, None);
    }

    fn unfold_all(&mut self) {
        let (replace, edits) = self.replace_map.unfold_all();
        self.apply_replace(replace, edits, None);
    }

    fn insert(&mut self, placement: BlockPlacement, height: f32) -> BlockId {
        self.blocks
            .insert([BlockProperties::new(placement, px(height))])[0]
    }

    /// Checks the layer against a rebuild and its own summaries.
    fn check(&self) {
        let mut rebuilt = self.blocks.clone();
        rebuilt.rebuild();
        assert_eq!(
            self.snapshot().transforms(),
            rebuilt.snapshot().transforms(),
            "incremental != rebuild"
        );
        let snapshot = self.snapshot();
        assert_eq!(snapshot.row_count(), snapshot.input().row_count());
        // The tree is canonical: no empty run, no two runs side by side.
        let items = snapshot.transforms();
        for pair in items.windows(2) {
            assert!(
                !matches!(pair, [Transform::Rows(_), Transform::Rows(_)]),
                "{items:?}"
            );
        }
        assert!(!items.contains(&Transform::Rows(0)));
    }
}

fn above(offset: usize) -> BlockPlacement {
    BlockPlacement::Above(Anchor::after(offset))
}

fn below(offset: usize) -> BlockPlacement {
    BlockPlacement::Below(Anchor::after(offset))
}

fn replace(range: Range<usize>) -> BlockPlacement {
    BlockPlacement::Replace(Anchor::after(range.start)..Anchor::before(range.end))
}

/// The items over the whole document, as (rows, top px, height px, block).
fn layout(snapshot: &BlockSnapshot) -> Vec<(Range<u32>, f64, Option<BlockId>)> {
    snapshot
        .items(Y::ZERO..snapshot.total_height() + Height(1))
        .map(|item| {
            let block = match &item {
                DisplayItem::Row { .. } => None,
                DisplayItem::Block(block) => Some(block.id),
            };
            (item.rows(), item.top().to_px(), block)
        })
        .collect()
}

#[test]
fn rows_and_blocks_add_up_in_pixels() {
    // Rows: 0 "zero", 1 "one", 2 "two", 3 "three", 4 "".
    let text = "zero\none\ntwo\nthree\n";
    let mut layers = Layers::new(text, None);
    let a = layers.insert(above(5), 10.0);
    let b = layers.insert(below(5), 7.5);
    let c = layers.insert(replace(13..18), 100.0);
    layers.check();
    let snapshot = layers.snapshot();
    assert_eq!(
        snapshot.total_height().to_px(),
        4.0 * 20.0 + 10.0 + 7.5 + 100.0
    );
    assert_eq!(
        layout(snapshot),
        vec![
            (0..1, 0.0, None),
            (1..1, 20.0, Some(a)),
            (1..2, 30.0, None),
            (2..2, 50.0, Some(b)),
            (2..3, 57.5, None),
            (3..4, 77.5, Some(c)),
            (4..5, 177.5, None),
        ]
    );
    assert_eq!(snapshot.row_top(1).to_px(), 30.0);
    assert_eq!(snapshot.row_top(3).to_px(), 77.5);
    assert_eq!(snapshot.row_top(5), snapshot.total_height());
    assert_eq!(
        snapshot.replacement_at(3).map(|item| (item.id, item.rows)),
        Some((c, 3..4))
    );
    assert_eq!(snapshot.replacement_at(2), None);
    assert!(snapshot.has_replaced_rows());
    assert_eq!(
        snapshot.item_at(Y::from_px(25.0)),
        Some(DisplayItem::Block(BlockItem {
            id: a,
            kind: BlockKind::Above,
            reveal: Reveal::Never,
            top: Y::from_px(20.0),
            height: px(10.0),
            rows: 1..1,
        }))
    );
    assert_eq!(
        snapshot.item_at(Y::from_px(56.0)).map(|item| item.rows()),
        Some(2..2)
    );
    assert_eq!(
        snapshot.item_at(Y::from_px(57.5)),
        Some(DisplayItem::Row {
            row: 2,
            top: Y::from_px(57.5)
        })
    );
    assert_eq!(snapshot.item_at(snapshot.total_height()), None);
    assert_eq!(snapshot.rows_in(Y::from_px(45.0)..Y::from_px(90.0)), 1..4);
    assert_eq!(
        layers.blocks.bounds(c).map(|item| item.top.to_px()),
        Some(77.5)
    );

    // A new row height costs nothing and moves everything.
    layers.blocks.set_row_height(px(10.0));
    assert_eq!(layers.snapshot().total_height().to_px(), 4.0 * 10.0 + 117.5);
    assert_eq!(layers.snapshot().row_top(2).to_px(), 37.5);
}

#[test]
fn blocks_take_every_wrap_row_of_their_line() {
    // "aaaa bbbb cccc" wraps at 5 columns into three rows.
    let text = "x\naaaa bbbb cccc\ny";
    let mut layers = Layers::new(text, Some(5.0));
    assert_eq!(layers.snapshot().row_count(), 5);
    let a = layers.insert(above(8), 1.0);
    let b = layers.insert(below(8), 2.0);
    layers.check();
    let rows: Vec<_> = layout(layers.snapshot())
        .into_iter()
        .map(|(rows, _, block)| (rows, block))
        .collect();
    assert_eq!(
        rows,
        vec![
            (0..1, None),
            (1..1, Some(a)),
            (1..2, None),
            (2..3, None),
            (3..4, None),
            (4..4, Some(b)),
            (4..5, None),
        ]
    );
    let c = layers.insert(replace(5..7), 50.0);
    layers.check();
    assert_eq!(layers.blocks.bounds(c).map(|item| item.rows), Some(1..4));
    // The blocks above and below the replaced line are hidden.
    assert_eq!(layers.blocks.bounds(a), None);
    assert_eq!(layers.blocks.bounds(b), None);
    assert_eq!(layers.snapshot().total_height().to_px(), 2.0 * 20.0 + 50.0);
    // Typing in the line re-wraps it; the block still takes all its rows.
    layers.edit(2..2, "dddd eeee ");
    layers.check();
    assert_eq!(layers.blocks.bounds(c).map(|item| item.rows), Some(1..6));
}

#[test]
fn a_range_ending_after_a_line_break_does_not_take_the_next_line() {
    let text = "a\nb\nc\nd\n";
    let mut layers = Layers::new(text, None);
    let id = layers.insert(replace(2..6), 5.0);
    layers.check();
    assert_eq!(layers.blocks.bounds(id).map(|item| item.rows), Some(1..3));
    // An empty range replaces its line; the ends may be in either order.
    let empty = layers.insert(replace(0..0), 5.0);
    assert_eq!(
        layers.blocks.bounds(empty).map(|item| item.rows),
        Some(0..1)
    );
    let reversed = layers.insert(
        BlockPlacement::Replace(Anchor::after(8)..Anchor::before(7)),
        5.0,
    );
    assert_eq!(
        layers.blocks.bounds(reversed).map(|item| item.rows),
        Some(3..4)
    );
    layers.check();
}

#[test]
fn overlapping_replace_blocks_merge_and_come_back() {
    let text = "0\n1\n2\n3\n4\n5\n6\n";
    let mut layers = Layers::new(text, None);
    let first = layers.insert(replace(2..7), 30.0); // rows 1..=3
    let second = layers.insert(replace(6..11), 40.0); // rows 3..=5
    let third = layers.insert(replace(10..13), 50.0); // rows 5..=6
    layers.check();
    // One block over rows 1..7, with the first one's height.
    assert_eq!(
        layers.blocks.bounds(first).map(|item| item.rows),
        Some(1..7)
    );
    assert_eq!(layers.blocks.bounds(second), None);
    assert_eq!(layers.blocks.bounds(third), None);
    assert_eq!(layers.snapshot().total_height().to_px(), 2.0 * 20.0 + 30.0);
    // Removing the first shows the second over 3..6 and merges the third.
    layers.blocks.remove([first]);
    layers.check();
    assert_eq!(
        layers.blocks.bounds(second).map(|item| item.rows),
        Some(3..7)
    );
    // Removing it too shows the third alone.
    layers.blocks.remove([second]);
    layers.check();
    assert_eq!(
        layers.blocks.bounds(third).map(|item| item.rows),
        Some(5..7)
    );
}

#[test]
fn blocks_in_folds_are_hidden() {
    let text = "# h\nimage\ntable\n# next\n";
    let mut layers = Layers::new(text, None);
    let image = layers.insert(below(4), 30.0);
    let table = layers.insert(replace(10..15), 40.0);
    let heading = layers.insert(below(0), 5.0);
    layers.check();
    assert!(layers.blocks.bounds(image).is_some());
    // Fold the section after its heading line.
    layers.fold(3..15);
    layers.check();
    assert_eq!(layers.blocks.bounds(image), None);
    assert_eq!(layers.blocks.bounds(table), None);
    assert_eq!(
        layers.blocks.bounds(heading).map(|item| item.rows),
        Some(1..1)
    );
    assert_eq!(layers.snapshot().total_height().to_px(), 3.0 * 20.0 + 5.0);
    layers.unfold_all();
    layers.check();
    assert_eq!(
        layers.blocks.bounds(table).map(|item| item.rows),
        Some(2..3)
    );
}

#[test]
fn deleting_an_anchor_keeps_or_removes_the_block() {
    let text = "one\n![](a.png)\ntwo\n![](b.png)\n";
    let mut layers = Layers::new(text, None);
    let kept = layers.insert(below(4), 10.0);
    let removed = layers
        .blocks
        .insert([BlockProperties::new(below(19), px(10.0)).when_deleted(WhenDeleted::Remove)])[0];
    // Deleting the line before the first image keeps it: its anchor sticks
    // to the `!` after it.
    let changes = layers.edit(0..4, "");
    assert_eq!(changes, BlockChanges::default());
    layers.check();
    // Deleting the image lines displaces one and removes the other.
    let changes = layers.edit(0..11, "");
    assert_eq!(changes.displaced, vec![kept]);
    layers.check();
    let at = layers
        .buffer
        .text()
        .to_string()
        .find('!')
        .expect("an image");
    let changes = layers.edit(at..at + 11, "x");
    assert_eq!(changes.removed, vec![(removed, Removal::AnchorDeleted)]);
    assert_eq!(layers.blocks.block(removed), None);
    assert_eq!(layers.blocks.blocks().len(), 1);
    layers.check();
}

#[test]
fn resizing_changes_one_item() {
    let text = "a\nb\nc\n";
    let mut layers = Layers::new(text, None);
    let ids = layers.blocks.insert([
        BlockProperties::new(below(0), px(1.0)),
        BlockProperties::new(replace(2..3), px(2.0)),
        BlockProperties::new(replace(2..5), px(3.0)),
    ]);
    assert_eq!(
        layers
            .blocks
            .resize([(ids[0], px(10.0)), (ids[1], px(20.0)), (ids[2], px(4.0))]),
        ids
    );
    // The merged block's height counts only once it shows.
    assert_eq!(
        layers.snapshot().total_height().to_px(),
        2.0 * 20.0 + 10.0 + 20.0
    );
    layers.check();
    assert_eq!(
        layers.blocks.resize([(ids[0], px(10.0))]),
        Vec::<BlockId>::new()
    );
    layers.blocks.remove([ids[1]]);
    assert_eq!(
        layers.snapshot().total_height().to_px(),
        2.0 * 20.0 + 10.0 + 4.0
    );
    layers.check();
}

#[test]
fn blocks_between_the_same_rows_keep_their_order() {
    let text = "a\nb\n";
    let mut layers = Layers::new(text, None);
    let above_b = layers.insert(above(2), 1.0);
    let below_a = layers.insert(below(0), 1.0);
    let below_a2 = layers.insert(below(1), 1.0);
    let above_b2 = layers.insert(above(3), 1.0);
    layers.check();
    let blocks: Vec<_> = layout(layers.snapshot())
        .into_iter()
        .filter_map(|(_, _, block)| block)
        .collect();
    assert_eq!(blocks, vec![below_a, below_a2, above_b, above_b2]);
}

#[test]
fn display_items_cover_a_range() {
    let text = "a\nb\nc\nd\n";
    let mut layers = Layers::new(text, None);
    let zero = layers.insert(below(2), 0.0);
    layers.insert(replace(4..5), 100.0);
    let snapshot = layers.snapshot();
    let rows = |range: Range<f64>| -> Vec<Range<u32>> {
        snapshot
            .items(Y::from_px(range.start)..Y::from_px(range.end))
            .map(|item| item.rows())
            .collect()
    };
    assert_eq!(rows(0.0..20.0), vec![0..1]);
    // A block of no height at the top of the range is in it, at its end not.
    assert_eq!(rows(39.0..41.0), vec![1..2, 2..2, 2..3]);
    assert_eq!(rows(30.0..40.0), vec![1..2]);
    assert_eq!(rows(45.0..150.0), vec![2..3, 3..4]);
    assert_eq!(rows(500.0..600.0), Vec::<Range<u32>>::new());
    assert_eq!(rows(10.0..10.0), Vec::<Range<u32>>::new());
    assert_eq!(
        layers.blocks.bounds(zero).map(|item| item.top.to_px()),
        Some(40.0)
    );
}

#[test]
fn many_blocks_at_once_match_one_at_a_time() {
    let text: String = (0..400).map(|i| format!("line {i} with words\n")).collect();
    let mut bulk = Layers::new(&text, Some(8.0));
    let mut single = Layers::new(&text, Some(8.0));
    let blocks: Vec<BlockProperties> = (0..200)
        .map(|i| {
            let offset = i * 37 % text.len();
            let placement = match i % 3 {
                0 => above(offset),
                1 => below(offset),
                _ => replace(offset..offset + 50),
            };
            BlockProperties::new(placement, px((i % 50) as f32))
        })
        .collect();
    bulk.blocks.insert(blocks.clone());
    for block in blocks {
        single.blocks.insert([block]);
    }
    bulk.check();
    single.check();
    assert_eq!(bulk.snapshot().transforms(), single.snapshot().transforms());
    // Removing most of them at once, and one at a time.
    let ids: Vec<BlockId> = bulk.blocks.blocks().iter().map(|block| block.id).collect();
    bulk.blocks.remove(ids.iter().copied().skip(1).step_by(2));
    for &id in ids.iter().skip(1).step_by(2) {
        single.blocks.remove([id]);
    }
    single.check();
    assert_eq!(bulk.snapshot().transforms(), single.snapshot().transforms());
}

// ----- randomized: incremental == rebuild ------------------------------------

fn random_block(rng: &mut StdRng, len: usize) -> BlockProperties {
    let anchor = |rng: &mut StdRng| {
        let offset = rng.random_range(0..=len);
        if rng.random_bool(0.5) {
            Anchor::before(offset)
        } else {
            Anchor::after(offset)
        }
    };
    let placement = match rng.random_range(0..3) {
        0 => BlockPlacement::Above(anchor(rng)),
        1 => BlockPlacement::Below(anchor(rng)),
        _ => BlockPlacement::Replace(anchor(rng)..anchor(rng)),
    };
    let height = Height(rng.random_range(0..3000));
    let mut properties = BlockProperties::new(placement, height);
    if rng.random_bool(0.3) {
        properties = properties.when_deleted(WhenDeleted::Remove);
    }
    properties
}

#[test]
fn random_blocks_and_edits_match_a_rebuild() {
    let iterations = std::env::var("ITERATIONS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(300);
    for_each_seed(iterations, |mut rng| {
        let len = rng.random_range(0..300);
        let text = random_text(&mut rng, len);
        let width = rng.random_bool(0.7).then(|| rng.random_range(2.0..30.0));
        let mut layers = Layers::new(&text, width);
        for _ in 0..40 {
            let len = layers.buffer.len();
            match rng.random_range(0..10) {
                0..=2 => {
                    let count = rng.random_range(1..4);
                    let blocks: Vec<_> = (0..count).map(|_| random_block(&mut rng, len)).collect();
                    layers.blocks.insert(blocks);
                }
                3 => {
                    let ids: Vec<BlockId> = layers
                        .blocks
                        .blocks()
                        .iter()
                        .map(|block| block.id)
                        .filter(|_| rng.random_bool(0.3))
                        .collect();
                    layers.blocks.remove(ids);
                }
                4 => {
                    let sizes: Vec<(BlockId, Height)> = layers
                        .blocks
                        .blocks()
                        .iter()
                        .map(|block| (block.id, Height(rng.random_range(0..3000))))
                        .collect::<Vec<_>>()
                        .into_iter()
                        .filter(|_| rng.random_bool(0.4))
                        .collect();
                    layers.blocks.resize(sizes);
                }
                5 => {
                    if rng.random_bool(0.5) {
                        let start = rng.random_range(0..=len);
                        let end = rng.random_range(start..=len.min(start + 40));
                        layers.fold(start..end);
                    } else {
                        layers.unfold_all();
                    }
                }
                6 => {
                    if let Some(map) = &mut layers.wrap_map {
                        let (wrap, edits) = map.set_wrap_width(Some(rng.random_range(2.0..30.0)));
                        layers.blocks.sync_wrap(wrap, &edits, None);
                    }
                }
                _ => {
                    let before: Vec<Block> = layers.blocks.blocks().to_vec();
                    let summary = random_edit(&mut rng, &mut layers.buffer);
                    let changes = layers.sync(&summary);
                    // Every removed block asked for it.
                    for (id, removal) in &changes.removed {
                        assert_eq!(*removal, Removal::AnchorDeleted);
                        let block = before.iter().find(|block| block.id == *id);
                        assert_eq!(
                            block.map(|block| block.when_deleted),
                            Some(WhenDeleted::Remove)
                        );
                    }
                }
            }
            layers.check();
            check_queries(layers.snapshot());
        }
    });
}

/// Row ↔ y round trips and the summary of the items.
fn check_queries(snapshot: &BlockSnapshot) {
    let row_height = u64::from(snapshot.row_height().0);
    let mut expected = 0u64;
    let mut next_row = 0;
    for item in snapshot.items(Y::ZERO..Y(u64::MAX)) {
        assert_eq!(item.top().0, expected, "{item:?}");
        assert_eq!(item.rows().start, next_row, "{item:?}");
        match &item {
            DisplayItem::Row { row, top } => {
                assert_eq!(snapshot.row_top(*row), *top);
                assert_eq!(snapshot.item_at(*top), Some(item.clone()));
                assert_eq!(
                    snapshot.item_at(Y(top.0 + row_height - 1)),
                    Some(item.clone())
                );
                assert_eq!(snapshot.replacement_at(*row), None);
                expected += row_height;
            }
            DisplayItem::Block(block) => {
                if block.height.0 > 0 {
                    assert_eq!(snapshot.item_at(block.top), Some(item.clone()));
                }
                for row in block.rows.clone() {
                    assert_eq!(snapshot.row_top(row), block.top);
                    assert_eq!(snapshot.replacement_at(row).as_ref(), Some(block));
                }
                expected += u64::from(block.height.0);
            }
        }
        next_row = item.rows().end;
    }
    assert_eq!(next_row, snapshot.row_count());
    assert_eq!(expected, snapshot.total_height().0);
}

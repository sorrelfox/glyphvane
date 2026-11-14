// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The editor with blocks: the block layer follows edits, folds and soft
//! wrap; motions skip the rows a block hides or reveal it; a caret put on
//! hidden rows moves out; paging and y queries go by pixels. A property
//! test runs random editing against a rebuild of the layer.

// The fold and selection commands take lists of ranges.
#![allow(clippy::single_range_in_vec_init)]

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use gv_editor::display_map::{
    BlockChanges, BlockId, BlockItem, BlockKind, BlockPlacement, BlockProperties, BlockSnapshot,
    DisplayItem, Height, Removal, Reveal, WhenDeleted, Y,
};
use gv_editor::{DeleteUnit, Editor, EditorEvent, Motion};
use gv_text::{Anchor, Bias, Buffer};
use proptest::prelude::*;
use proptest::test_runner::Config;
use support::{editor, render};

fn px(px: f32) -> Height {
    Height::from_px(px)
}

fn above(offset: usize) -> BlockPlacement {
    BlockPlacement::Above(Anchor::after(offset))
}

fn below(offset: usize) -> BlockPlacement {
    BlockPlacement::Below(Anchor::after(offset))
}

fn replace(range: std::ops::Range<usize>) -> BlockPlacement {
    BlockPlacement::Replace(Anchor::after(range.start)..Anchor::before(range.end))
}

fn insert(editor: &mut Editor, placement: BlockPlacement, height: f32) -> BlockId {
    editor.insert_blocks([BlockProperties::new(placement, px(height))])[0]
}

/// Every item of the layer.
fn items(blocks: &BlockSnapshot) -> Vec<DisplayItem> {
    blocks.items(Y::ZERO..Y(u64::MAX)).collect()
}

/// What observers see, in order: event kinds, and block changes.
type Events = Rc<RefCell<Vec<String>>>;

fn record(editor: &mut Editor) -> Events {
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    editor.observe(move |event| {
        let entry = match event {
            EditorEvent::Edited(_) => "edited".to_owned(),
            EditorEvent::SelectionsChanged(_) => "selections".to_owned(),
            EditorEvent::DisplayChanged { .. } => "display".to_owned(),
            EditorEvent::WrapChanged { .. } => "wrap".to_owned(),
            EditorEvent::BlocksChanged { changes, .. } => format!("blocks {changes:?}"),
            _ => return,
        };
        sink.borrow_mut().push(entry);
    });
    events
}

#[test]
fn blocks_follow_edits_and_report_changes() {
    // Rows: 0 "# Title", 1 "![](a.png)", 2 "", 3 "end", 4 "".
    let (mut e, _) = editor("|# Title\n![](a.png)\n\nend\n");
    e.set_row_height(px(20.0));
    let events = record(&mut e);
    let image = e.insert_blocks([
        BlockProperties::new(below(8), px(100.0)).when_deleted(WhenDeleted::Remove)
    ])[0];
    assert_eq!(e.blocks().total_height().to_px(), 5.0 * 20.0 + 100.0);
    assert_eq!(e.y_for_offset(20).to_px(), 3.0 * 20.0 + 100.0);
    assert_eq!(
        e.block_bounds(image),
        Some(BlockItem {
            id: image,
            kind: BlockKind::Below,
            reveal: Reveal::Never,
            top: Y::from_px(40.0),
            height: px(100.0),
            rows: 2..2,
        })
    );
    // Typing above moves the anchor, and no block changes.
    e.insert_at_selections("x");
    assert_eq!(
        e.block(image).map(|block| block.placement.clone()),
        Some(below(9))
    );
    // An image loads: resized in place.
    assert!(e.resize_block(image, px(50.0)));
    assert!(!e.resize_block(image, px(50.0)));
    assert_eq!(e.y_for_offset(21).to_px(), 3.0 * 20.0 + 50.0);
    // Deleting the image line removes the block, after the edit's event.
    e.select_ranges([9..20]);
    e.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(e.text().to_string(), "x# Title\n\nend\n");
    assert_eq!(e.block(image), None);
    assert_eq!(e.blocks().total_height().to_px(), 4.0 * 20.0);
    let inserted = BlockChanges {
        inserted: vec![image],
        ..BlockChanges::default()
    };
    let resized = BlockChanges {
        resized: vec![image],
        ..BlockChanges::default()
    };
    let removed = BlockChanges {
        removed: vec![(image, Removal::AnchorDeleted)],
        ..BlockChanges::default()
    };
    assert_eq!(
        *events.borrow(),
        vec![
            format!("blocks {inserted:?}"),
            "edited".to_owned(),
            format!("blocks {resized:?}"),
            "selections".to_owned(),
            "edited".to_owned(),
            format!("blocks {removed:?}"),
        ]
    );
}

#[test]
fn motions_skip_hidden_rows() {
    // Rows: 0 "ab", 1 "b" and 2 "c" hidden, 3 "d", 4 "".
    let (mut e, _) = editor("a|b\nb\nc\nd\n");
    let table = insert(&mut e, replace(3..6), 30.0);
    assert_eq!(e.block_bounds(table).map(|item| item.rows), Some(1..3));
    let moved = |e: &mut Editor, motion| {
        e.move_selections(motion, false);
        render(e)
    };
    // Down keeps the goal column on the row after the block, Up returns.
    assert_eq!(moved(&mut e, Motion::Down), "ab\nb\nc\nd|\n");
    assert_eq!(moved(&mut e, Motion::Up), "a|b\nb\nc\nd\n");
    // Right from the end of a row jumps over the block, Left comes back.
    assert_eq!(moved(&mut e, Motion::LineEnd), "ab|\nb\nc\nd\n");
    assert_eq!(moved(&mut e, Motion::Right), "ab\nb\nc\n|d\n");
    assert_eq!(moved(&mut e, Motion::Left), "ab|\nb\nc\nd\n");
    // Word motions too: out of the block, to the start of the row after.
    assert_eq!(moved(&mut e, Motion::WordRight), "ab\nb\nc\n|d\n");
    // Extending selects across the block.
    e.set_cursor(0);
    e.move_selections(Motion::Down, true);
    assert_eq!(render(&e), "{ab\nb\nc\n}d\n");
    // Adding a cursor below skips it.
    e.set_cursor(1);
    assert!(e.add_cursor_below());
    assert_eq!(render(&e), "a|b\nb\nc\nd|\n");
}

#[test]
fn a_caret_put_on_hidden_rows_moves_out() {
    let (mut e, _) = editor("|a\nb\nc\n");
    insert(&mut e, replace(2..3), 30.0);
    let events = record(&mut e);
    // A click on the hidden row goes to the start of the row after.
    e.set_cursor(2);
    assert_eq!(render(&e), "a\nb\n|c\n");
    assert_eq!(*events.borrow(), vec!["selections".to_owned()]);
    // Backspace that joins the next line into the hidden row moves the
    // caret on to the row after the block.
    e.delete_backward(DeleteUnit::Grapheme);
    assert_eq!(render(&e), "a\nbc\n|");
    assert_eq!(e.block_hiding(3).map(|item| item.rows), Some(1..2));
    // At the end of the text, the caret goes to the end of the row before.
    let (mut e, _) = editor("|a\nb");
    insert(&mut e, replace(2..3), 30.0);
    e.set_cursor(3);
    assert_eq!(render(&e), "a|\nb");
    // A new block over the caret moves it too.
    let (mut e, _) = editor("a\nb|\nc\n");
    insert(&mut e, replace(2..3), 30.0);
    assert_eq!(render(&e), "a\nb\n|c\n");
}

#[test]
fn a_block_revealed_on_selection_lets_the_caret_in() {
    let (mut e, _) = editor("|a\nb\nc\n");
    let table = e
        .insert_blocks([BlockProperties::new(replace(2..3), px(30.0)).reveal(Reveal::OnSelection)])
        [0];
    let events = record(&mut e);
    e.move_selections(Motion::Down, false);
    assert_eq!(render(&e), "a\n|b\nc\n");
    assert_eq!(e.block(table), None);
    let revealed = BlockChanges {
        removed: vec![(table, Removal::Revealed)],
        ..BlockChanges::default()
    };
    assert_eq!(
        *events.borrow(),
        vec![format!("blocks {revealed:?}"), "selections".to_owned()]
    );
    // Inserted back where the caret is, it is revealed at once.
    let again = e
        .insert_blocks([BlockProperties::new(replace(2..3), px(30.0)).reveal(Reveal::OnSelection)])
        [0];
    assert_eq!(e.block(again), None);
    // A selection that spans it does not reveal it.
    e.select_ranges([0..6]);
    let kept = e
        .insert_blocks([BlockProperties::new(replace(2..3), px(30.0)).reveal(Reveal::OnSelection)])
        [0];
    assert!(e.block(kept).is_some());
}

#[test]
fn paging_goes_by_pixels() {
    let text: String = (0..10).map(|i| format!("line {i}\n")).collect();
    let mut e = Editor::new(Buffer::new(&text));
    e.set_row_height(px(20.0));
    // A 200 px image below line 2: rows 0..3 are 0..60 px, the image
    // 60..260, row 3 from 260.
    insert(&mut e, below(14), 200.0);
    let line = |e: &Editor| {
        let head = e.selections().primary().head();
        e.text().line_of_offset(head).expect("an offset")
    };
    e.set_cursor(2);
    e.move_selections_by_pixels(100.0, false);
    assert_eq!(line(&e), 3);
    assert_eq!(e.selections().primary().head(), 21 + 2);
    e.move_selections_by_pixels(100.0, false);
    assert_eq!(line(&e), 8);
    e.move_selections_by_pixels(-100.0, false);
    assert_eq!(line(&e), 3);
    e.move_selections_by_pixels(-30.0, false);
    assert_eq!(line(&e), 2);
    e.move_selections_by_pixels(10_000.0, true);
    assert_eq!(line(&e), 10);
    assert_eq!(e.selections().primary().tail(), 14 + 2);
}

#[test]
fn soft_wrap_and_folds_move_blocks() {
    // Line 1 wraps into three rows at 5 columns.
    let (mut e, _) = editor("|x\naaaa bbbb cccc\ny\n");
    let image = insert(&mut e, below(2), 10.0);
    assert_eq!(e.block_bounds(image).map(|item| item.rows), Some(2..2));
    let events = record(&mut e);
    e.set_wrap_width(Some(5.0));
    assert_eq!(e.block_bounds(image).map(|item| item.rows), Some(4..4));
    assert_eq!(e.blocks().row_count(), 6);
    // A rewrap handed out as a job: the same rows meanwhile, then the new.
    let job = e.defer_wrap_width(Some(10.0)).expect("a new width");
    assert_eq!(e.block_bounds(image).map(|item| item.rows), Some(4..4));
    let mut job = job;
    let mut wrapper = gv_editor::display_map::LineWrapper::new(Box::new(
        gv_editor::display_map::MonospaceMeasure::new(1.0),
    ));
    job.run_to_end(&mut wrapper);
    assert!(e.finish_rewrap(job));
    assert_eq!(e.block_bounds(image).map(|item| item.rows), Some(3..3));
    e.set_wrap_width(None);
    assert_eq!(e.block_bounds(image).map(|item| item.rows), Some(2..2));
    // Folding the line away hides the block, unfolding shows it.
    e.fold_ranges([1..16]);
    assert_eq!(e.block_bounds(image), None);
    e.unfold_all();
    assert_eq!(e.block_bounds(image).map(|item| item.rows), Some(2..2));
    // Rows moving under the blocks are not block changes.
    assert!(
        events
            .borrow()
            .iter()
            .all(|event| !event.starts_with("blocks"))
    );
}

#[test]
fn the_view_queries_by_y() {
    let (mut e, _) = editor("|a\nb\nc\nd\n");
    e.set_row_height(px(24.0));
    let table = insert(&mut e, replace(2..5), 70.0);
    let image = insert(&mut e, above(6), 30.0);
    let blocks = e.blocks();
    // a 0..24, table 24..94 (rows 1..3), image 94..124, d 124..148, "" ..172.
    assert_eq!(blocks.total_height().to_px(), 172.0);
    let ids: Vec<_> = blocks
        .items(Y::from_px(30.0)..Y::from_px(125.0))
        .map(|item| match item {
            DisplayItem::Row { row, .. } => format!("row {row}"),
            DisplayItem::Block(block) => format!("block {}", block.id.0),
        })
        .collect();
    assert_eq!(
        ids,
        vec![
            format!("block {}", table.0),
            format!("block {}", image.0),
            "row 3".to_owned()
        ]
    );
    assert_eq!(blocks.rows_in(Y::from_px(30.0)..Y::from_px(125.0)), 1..4);
    assert_eq!(
        blocks
            .item_at(Y::from_px(100.0))
            .map(|item| item.top().to_px()),
        Some(94.0)
    );
    assert_eq!(blocks.row_top(2).to_px(), 24.0);
    assert_eq!(e.y_for_offset(6).to_px(), 124.0);
    assert_eq!(e.block_hiding(4).map(|item| item.id), Some(table));
    assert_eq!(e.block_hiding(6), None);
    assert_eq!(e.remove_blocks([table, table, image]), 2);
    assert_eq!(e.blocks().total_height().to_px(), 5.0 * 24.0);
}

// ----- property test ----------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    Type(String),
    Backspace,
    Delete,
    Move(Motion, bool),
    Click(usize),
    Select(usize, usize),
    Undo,
    Redo,
    Fold(usize, usize),
    UnfoldAll,
    Wrap(Option<f32>),
    Insert {
        kind: u8,
        a: usize,
        b: usize,
        biases: (bool, bool),
        height: u32,
        remove: bool,
        reveal: bool,
    },
    Remove(usize),
    Resize(usize, u32),
    Page(f32, bool),
}

fn motion() -> impl Strategy<Value = Motion> {
    prop_oneof![
        Just(Motion::Left),
        Just(Motion::Right),
        Just(Motion::Up),
        Just(Motion::Down),
        Just(Motion::WordLeft),
        Just(Motion::WordRight),
        Just(Motion::LineStart),
        Just(Motion::LineEnd),
        Just(Motion::DocumentStart),
        Just(Motion::DocumentEnd),
        Just(Motion::UpRows(3)),
        Just(Motion::DownRows(3)),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    let typed = prop::collection::vec(
        prop::sample::select(vec!["a", "b", " ", "\n", "\r\n", "é", "語", "\t", "- "]),
        1..4,
    )
    .prop_map(|parts| parts.concat());
    prop_oneof![
        4 => typed.prop_map(Op::Type),
        2 => Just(Op::Backspace),
        1 => Just(Op::Delete),
        6 => (motion(), any::<bool>()).prop_map(|(motion, extend)| Op::Move(motion, extend)),
        2 => any::<usize>().prop_map(Op::Click),
        1 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::Select(a, b)),
        1 => Just(Op::Undo),
        1 => Just(Op::Redo),
        1 => (any::<usize>(), 0usize..30).prop_map(|(a, len)| Op::Fold(a, len)),
        1 => Just(Op::UnfoldAll),
        1 => prop_oneof![Just(None), (2.0f32..20.0).prop_map(Some)].prop_map(Op::Wrap),
        4 => (0u8..3, any::<usize>(), any::<usize>(), any::<(bool, bool)>(), 0u32..4000, any::<bool>(), any::<bool>())
            .prop_map(|(kind, a, b, biases, height, remove, reveal)| Op::Insert {
                kind, a, b, biases, height, remove, reveal,
            }),
        1 => any::<usize>().prop_map(Op::Remove),
        2 => (any::<usize>(), 0u32..4000).prop_map(|(index, height)| Op::Resize(index, height)),
        2 => (-300.0f32..300.0, any::<bool>()).prop_map(|(dy, extend)| Op::Page(dy, extend)),
    ]
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![
            "word ", "x", "\n", "\n\n", "- item\n", "\r\n", "長い", "\t",
        ]),
        0..40,
    )
    .prop_map(|parts| parts.concat())
}

fn apply(e: &mut Editor, op: &Op) {
    let len = e.text().len();
    let at = |n: usize| e.text().clip_offset(n % (len + 1), Bias::Left);
    match op {
        Op::Type(text) => {
            e.insert_at_selections(text);
        }
        Op::Backspace => {
            e.delete_backward(DeleteUnit::Grapheme);
        }
        Op::Delete => {
            e.delete_forward(DeleteUnit::Grapheme);
        }
        Op::Move(motion, extend) => e.move_selections(*motion, *extend),
        Op::Click(n) => e.set_cursor(at(*n)),
        Op::Select(a, b) => e.select_ranges([at(*a)..at(*b)]),
        Op::Undo => {
            e.undo();
        }
        Op::Redo => {
            e.redo();
        }
        Op::Fold(n, extra) => {
            let start = at(*n);
            e.fold_ranges([start..at(start + extra).max(start)]);
        }
        Op::UnfoldAll => {
            e.unfold_all();
        }
        Op::Wrap(width) => e.set_wrap_width(*width),
        Op::Insert {
            kind,
            a,
            b,
            biases,
            height,
            remove,
            reveal,
        } => {
            let anchor = |n: usize, right: bool| {
                Anchor::new(at(n), if right { Bias::Right } else { Bias::Left })
            };
            let placement = match kind {
                0 => BlockPlacement::Above(anchor(*a, biases.0)),
                1 => BlockPlacement::Below(anchor(*a, biases.0)),
                _ => BlockPlacement::Replace(anchor(*a, biases.0)..anchor(*b, biases.1)),
            };
            let mut properties = BlockProperties::new(placement, Height(*height));
            if *remove {
                properties = properties.when_deleted(WhenDeleted::Remove);
            }
            if *reveal {
                properties = properties.reveal(Reveal::OnSelection);
            }
            e.insert_blocks([properties]);
        }
        Op::Remove(n) => {
            let blocks = e.block_map().blocks();
            if !blocks.is_empty() {
                let id = blocks[n % blocks.len()].id;
                e.remove_blocks([id]);
            }
        }
        Op::Resize(n, height) => {
            let blocks = e.block_map().blocks();
            if !blocks.is_empty() {
                let id = blocks[n % blocks.len()].id;
                e.resize_block(id, Height(*height));
            }
        }
        Op::Page(dy, extend) => e.move_selections_by_pixels(*dy, *extend),
    }
}

/// The invariants of the block layer in an editor.
fn check(e: &Editor) -> Result<(), TestCaseError> {
    let blocks = e.blocks();
    let shown = items(blocks);

    // Incremental == rebuild.
    let mut fresh = e.block_map().clone();
    fresh.rebuild();
    prop_assert_eq!(&items(fresh.snapshot()), &shown, "incremental != rebuild");

    // The input rows are the display rows.
    let rows = match e.wrap() {
        Some(wrap) => wrap.max_point().row().0 + 1,
        None => e.display().max_point().row() + 1,
    };
    prop_assert_eq!(blocks.row_count(), rows);

    // Total height = rows × row height + Σ blocks − Σ replaced rows.
    let row_height = u64::from(blocks.row_height().0);
    let (heights, replaced) =
        shown
            .iter()
            .fold((0u64, 0u32), |(heights, replaced), item| match item {
                DisplayItem::Row { .. } => (heights, replaced),
                DisplayItem::Block(block) => (
                    heights + u64::from(block.height.0),
                    replaced + (block.rows.end - block.rows.start),
                ),
            });
    prop_assert_eq!(
        blocks.total_height().0,
        u64::from(rows) * row_height + heights - u64::from(replaced) * row_height
    );

    // y ↔ row round trips; the items tile the document.
    let mut y = 0u64;
    let mut next_row = 0u32;
    for item in &shown {
        prop_assert_eq!(item.top().0, y);
        prop_assert_eq!(item.rows().start, next_row);
        match item {
            DisplayItem::Row { row, top } => {
                prop_assert_eq!(blocks.row_top(*row), *top);
                prop_assert_eq!(blocks.item_at(*top), Some(item.clone()));
                y += row_height;
            }
            DisplayItem::Block(block) => {
                for row in block.rows.clone() {
                    prop_assert_eq!(blocks.row_top(row), block.top);
                }
                if block.height.0 > 0 {
                    prop_assert_eq!(blocks.item_at(block.top), Some(item.clone()));
                }
                y += u64::from(block.height.0);
            }
        }
        next_row = item.rows().end;
    }
    prop_assert_eq!(next_row, rows);

    // No selection end on a hidden row (unless no row is shown at all).
    let any_row = shown
        .iter()
        .any(|item| matches!(item, DisplayItem::Row { .. }));
    for selection in e.selections().iter() {
        for end in [selection.tail(), selection.head()] {
            let hidden = e.block_hiding(end);
            prop_assert!(
                hidden.is_none() || !any_row,
                "selection end {} on rows {:?} of {:?}",
                end,
                hidden.map(|item| item.rows),
                e.text().to_string()
            );
        }
    }
    Ok(())
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|cases| cases.parse().ok())
        .unwrap_or(128)
}

proptest! {
    #![proptest_config(Config { cases: cases(), failure_persistence: None, ..Config::default() })]

    #[test]
    fn random_editing_keeps_the_block_layer_exact(
        text in text(),
        ops in prop::collection::vec(op(), 1..60),
        row_height in 1u32..2000,
    ) {
        let mut e = Editor::new(Buffer::new(&text));
        e.set_row_height(Height(row_height));
        for op in &ops {
            apply(&mut e, op);
            check(&e)?;
        }
    }
}

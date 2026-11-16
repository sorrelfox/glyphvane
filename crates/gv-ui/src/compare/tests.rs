// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The comparison model: regions, default choices, choices per region and
//! the result text. The view in a window is tested with the documents it
//! compares (`files::safety_tests`).

use gv_text::LineEnding;

use super::{Comparison, HunkKind, Pick, Region, Sides};

fn sides() -> Sides {
    Sides {
        left: "mine".into(),
        right: "theirs".into(),
    }
}

#[test]
fn three_way_starts_with_the_side_that_changed() {
    let base = "a\nb\nc\nd\ne\n";
    let mine = "A\nb\nc\nd\ne\n";
    let theirs = "a\nb\nc\nd\nE\n";
    let mut c = Comparison::three_way(base, mine, theirs, sides(), LineEnding::Lf);
    let hunks: Vec<_> = c.hunks().map(|h| (h.kind, h.pick)).collect();
    assert_eq!(
        hunks,
        [
            (HunkKind::Left, Some(Pick::Left)),
            (HunkKind::Right, Some(Pick::Right))
        ]
    );
    assert_eq!(c.unresolved(), 0);
    assert_eq!(c.result(), "A\nb\nc\nd\nE\n", "a clean merge");
    assert!(matches!(c.regions()[1], Region::Unchanged(_)));

    c.pick(0, Pick::Right);
    assert_eq!(c.result(), "a\nb\nc\nd\nE\n");
    c.pick_all(Pick::Left);
    assert_eq!(c.result(), mine);
    c.pick_all(Pick::Right);
    assert_eq!(c.result(), theirs);
}

#[test]
fn conflicts_start_unresolved_and_are_marked() {
    let c = Comparison::three_way(
        "t\r\nbody\r\nend",
        "t\r\nmine\r\nend",
        "t\r\ntheirs\r\nend",
        sides(),
        LineEnding::CrLf,
    );
    assert_eq!(c.unresolved(), 1);
    assert_eq!(c.hunks().next().map(|h| h.kind), Some(HunkKind::Conflict));
    assert_eq!(
        c.result(),
        "t\r\n<<<<<<< mine\r\nmine\r\n=======\r\ntheirs\r\n>>>>>>> theirs\r\nend"
    );
}

#[test]
fn both_writes_left_then_right() {
    let mut c = Comparison::three_way("x\nend", "x\nmine", "x\ntheirs", sides(), LineEnding::Lf);
    c.pick(0, Pick::Both);
    // The left side had no line ending: one is added between the two.
    assert_eq!(c.result(), "x\nmine\ntheirs");
    assert_eq!(c.unresolved(), 0);
}

#[test]
fn the_same_change_on_both_sides_is_written_once() {
    let c = Comparison::three_way("a\nb\n", "a\nB\n", "a\nB\n", sides(), LineEnding::Lf);
    let kinds: Vec<_> = c.hunks().map(|h| h.kind).collect();
    assert_eq!(kinds, [HunkKind::Same]);
    assert_eq!(c.result(), "a\nB\n");
}

#[test]
fn two_way_keeps_the_left_side_until_told() {
    let mine = "# Note\n\nRecovered line.\nkept\n";
    let disk = "# Note\n\nkept\nOn disk only.\n";
    let mut c = Comparison::two_way(mine, disk, sides(), LineEnding::Lf);
    assert!(c.hunks().all(|h| h.kind == HunkKind::Left));
    assert!(c.hunk_count() >= 2);
    assert_eq!(c.result(), mine);
    c.pick_all(Pick::Right);
    assert_eq!(c.result(), disk);
    // No differences: nothing to choose.
    let same = Comparison::two_way(mine, mine, sides(), LineEnding::Lf);
    assert_eq!(same.hunk_count(), 0);
    assert_eq!(same.result(), mine);
}

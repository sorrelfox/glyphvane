// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Recording block walks, for comparing [`gv_syntax::walk_range`] with
//! [`gv_syntax::walk`] filtered to a range.

use std::ops::Range;

use gv_syntax::{Container, Leaf, SyntaxTree, Visitor};

/// One visitor call, with everything it carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Visit {
    Enter(String),
    Exit(String, Range<usize>),
    Leaf(String, Range<usize>),
}

#[derive(Default)]
struct Rec(Vec<Visit>);

impl Visitor for Rec {
    fn enter(&mut self, c: &Container) {
        assert!(c.range.is_empty(), "enter with a non-empty range: {c:?}");
        self.0.push(Visit::Enter(format!(
            "{:?} {:?} start={} line={}",
            c.id, c.kind, c.range.start, c.line
        )));
    }

    fn exit(&mut self, c: &Container) {
        self.0.push(Visit::Exit(
            format!(
                "{:?} {:?} start={} line={}",
                c.id, c.kind, c.range.start, c.line
            ),
            c.range.clone(),
        ));
    }

    fn leaf(&mut self, l: &Leaf<'_>) {
        self.0.push(Visit::Leaf(
            format!(
                "{:?} {:?} lines={:?} info={:?} open={} start={} end={}",
                l.id, l.kind, l.lines, l.info, l.open_line, l.start_line, l.end_line
            ),
            l.range.clone(),
        ));
    }
}

/// Every visit of a full walk.
pub fn record_walk(tree: &SyntaxTree) -> Vec<Visit> {
    let mut r = Rec::default();
    gv_syntax::walk(tree, &mut r);
    r.0
}

/// Every visit of a range walk.
pub fn record_walk_range(tree: &SyntaxTree, range: Range<usize>) -> Vec<Visit> {
    let mut r = Rec::default();
    gv_syntax::walk_range(tree, range, &mut r);
    r.0
}

/// The range a range walk reports for `range`: start clamped to the text,
/// an empty range widened to one byte.
pub fn effective(tree: &SyntaxTree, range: &Range<usize>) -> Range<usize> {
    let start = range.start.min(tree.len());
    start..range.end.max(start + 1)
}

/// The visits of a full walk whose block shares a byte with `range` (as
/// [`effective`] widens it).
pub fn filtered_walk(tree: &SyntaxTree, range: Range<usize>) -> Vec<Visit> {
    let r = effective(tree, &range);
    let hits = |b: &Range<usize>| b.start < r.end && b.end > r.start;
    let all = record_walk(tree);
    // The range of every container, from its exit.
    let mut keep = vec![false; all.len()];
    let mut open: Vec<usize> = Vec::new();
    for (i, v) in all.iter().enumerate() {
        match v {
            Visit::Enter(_) => open.push(i),
            Visit::Exit(_, range) => {
                let j = open.pop().expect("balanced walk");
                keep[i] = hits(range);
                keep[j] = keep[i];
            }
            Visit::Leaf(_, range) => keep[i] = hits(range),
        }
    }
    all.into_iter()
        .zip(keep)
        .filter_map(|(v, k)| k.then_some(v))
        .collect()
}

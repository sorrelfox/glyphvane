// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Shared helpers: building, editing, and the structural invariants every
//! render tree must satisfy.

#![allow(dead_code)]

use std::collections::HashSet;
use std::fmt::Write as _;
use std::ops::Range;

use gv_model::DocModel;
use gv_render_tree::{
    FrontMatterDisplay, NodeKind, RenderOptions, RenderTree, Sources, UpdateReport,
};
use gv_syntax::{Change, Changes, Leaf, LeafKind, ParseOptions, SyntaxTree, Visitor};

/// A parsed document with its model and render tree.
pub struct Doc {
    pub text: String,
    pub tree: SyntaxTree,
    pub model: DocModel,
    pub render: RenderTree,
}

pub fn build(text: &str) -> Doc {
    build_with(
        text,
        ParseOptions::default(),
        RenderOptions::default(),
        Sources::default(),
    )
}

pub fn build_with(
    text: &str,
    parse: ParseOptions,
    opts: RenderOptions,
    sources: Sources<'_>,
) -> Doc {
    let tree = gv_syntax::parse_with(text, parse);
    let model = DocModel::build(&tree, text);
    let render = RenderTree::build(&tree, &model, text, opts, sources);
    Doc {
        text: text.to_owned(),
        tree,
        model,
        render,
    }
}

impl Doc {
    /// Replaces `range` with `insert`, reparses incrementally and updates
    /// the model and the render tree. Returns the parser's changes and the
    /// render tree's report.
    pub fn edit(
        &mut self,
        range: Range<usize>,
        insert: &str,
        sources: Sources<'_>,
    ) -> (Changes, UpdateReport) {
        let mut text = String::with_capacity(self.text.len() + insert.len());
        text.push_str(&self.text[..range.start]);
        text.push_str(insert);
        text.push_str(&self.text[range.end..]);
        let change = Change {
            start: range.start,
            old_end: range.end,
            new_len: insert.len(),
        };
        let (tree, changes) = self.tree.reparse_full(text.as_str(), change);
        let model = self.model.update(&tree, text.as_str(), &changes);
        let report = self
            .render
            .update(&tree, &model, text.as_str(), &changes, sources);
        self.text = text;
        self.tree = tree;
        self.model = model;
        (changes, report)
    }
}

/// A canonical description of a render tree: the dump plus the nodes'
/// sources and contexts.
pub fn canonical(rt: &RenderTree) -> String {
    let mut s = rt.dump();
    for n in rt.nodes() {
        let _ = writeln!(
            s,
            "node #{} {} {:?} ctx={}",
            n.id.0,
            n.kind.name(),
            n.source,
            n.context.len()
        );
    }
    s
}

/// Content byte ranges of every rendered leaf.
fn rendered_content(tree: &SyntaxTree, opts: &RenderOptions, text: &str) -> Vec<Range<usize>> {
    struct V<'a> {
        out: Vec<Range<usize>>,
        opts: &'a RenderOptions,
        text: &'a str,
    }
    impl Visitor for V<'_> {
        fn leaf(&mut self, leaf: &Leaf<'_>) {
            match leaf.kind {
                LeafKind::LinkRefDef => return,
                // A block of only `</details>` ends its details and shows
                // nothing.
                LeafKind::Html { .. }
                    if leaf.lines.len() == 1
                        && self.text[leaf.lines[0].content.clone()].trim() == "</details>" =>
                {
                    return;
                }
                LeafKind::FrontMatter { .. }
                    if self.opts.front_matter == FrontMatterDisplay::Hidden =>
                {
                    return;
                }
                _ => {}
            }
            for l in leaf.lines {
                if l.has_content && !l.content.is_empty() {
                    self.out.push(l.content.clone());
                }
            }
        }
    }
    let mut v = V {
        out: Vec::new(),
        opts,
        text,
    };
    gv_syntax::walk(tree, &mut v);
    v.out
}

/// Widths at which estimates are checked for monotonicity.
const WIDTHS: [f32; 7] = [20.0, 80.0, 150.0, 300.0, 451.0, 680.0, 2000.0];

/// Checks the structural invariants of `doc.render`.
pub fn check(doc: &Doc) {
    let rt = &doc.render;
    let items = rt.items();
    let nodes = rt.nodes();
    // Items: sorted, disjoint, unique ids, valid nodes in order.
    let mut ids = HashSet::new();
    for (i, it) in items.iter().enumerate() {
        assert!(ids.insert(it.id), "duplicate item id {:?}", it.id);
        assert!(it.source.start <= it.source.end, "item {i} {:?}", it.source);
        assert!(it.source.end <= doc.text.len(), "item {i} past the end");
        assert!(it.node < nodes.len(), "item {i} node {}", it.node);
        if i > 0 {
            let p = &items[i - 1];
            assert!(
                p.source.end <= it.source.start,
                "items {} and {i} overlap: {:?} {:?}\n{}",
                i - 1,
                p.source,
                it.source,
                rt.dump()
            );
            assert!(p.node <= it.node, "item nodes out of order");
        }
        assert_eq!(rt.index_of(it.id), Some(i));
        // Estimates are non-increasing in the width and finite.
        let mut last = f32::INFINITY;
        for w in WIDTHS {
            let h = it.height_estimate(w);
            assert!(h.is_finite() && h >= 0.0, "item {i} height {h} at {w}");
            assert!(
                h <= last + 1e-3,
                "item {i} grows with the width: {h} > {last} at {w}"
            );
            last = h;
        }
    }
    // Nodes: sorted, disjoint; each node's items tile its source.
    for (k, n) in nodes.iter().enumerate() {
        if k > 0 {
            assert!(
                nodes[k - 1].source.end <= n.source.start,
                "nodes {k} overlap"
            );
        }
        let its: Vec<_> = items.iter().filter(|it| it.node == k).collect();
        assert!(!its.is_empty(), "node {k} has no items");
        assert_eq!(its[0].source.start, n.source.start, "node {k} start");
        assert_eq!(its[its.len() - 1].source.end, n.source.end, "node {k} end");
        for w in its.windows(2) {
            assert_eq!(w[0].source.end, w[1].source.start, "node {k} chunks");
        }
        if let Some(c) = n.inline() {
            let p = &c.paragraph;
            assert_eq!(p.items().len(), c.spans.len());
            for it in p.items() {
                assert!(
                    it.source().end <= n.source.len(),
                    "node {k} paragraph item {:?} outside {:?}",
                    it.source(),
                    n.source
                );
            }
        }
        if let NodeKind::Heading { anchor, .. } = &n.kind {
            assert!(anchor.is_some(), "heading node {k} without anchor");
        }
    }
    // Every rendered content byte is in exactly one item (items are
    // disjoint, so: in one item).
    for r in rendered_content(&doc.tree, rt.options(), &doc.text) {
        let k = rt.item_at(r.start).unwrap_or(usize::MAX);
        let ok = items
            .get(k)
            .is_some_and(|it| it.source.start <= r.start && r.end <= it.source.end);
        assert!(ok, "content {r:?} not inside one item ({k})\n{}", rt.dump());
    }
    // Item texts map into their items, monotonically.
    for (ix, item) in items.iter().enumerate() {
        let t = rt.item_text(ix);
        let mut last = 0;
        for off in (0..=t.text.len()).filter(|&o| t.text.is_char_boundary(o)) {
            let s = t.source_offset(off);
            assert!(
                s >= last,
                "item {ix} text mapping not monotonic at {off}: {s} < {last}"
            );
            assert!(
                item.source.start <= s && s <= item.source.end,
                "item {ix} offset {off} maps to {s} outside {:?}",
                item.source
            );
            last = s;
        }
    }
}

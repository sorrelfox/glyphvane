// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Expand and shrink selection by syntax node: word → inline → block →
//! section (FR-EDIT-22).
//!
//! The steps around a selection, smallest first:
//!
//! 1. the word at a caret;
//! 2. inline spans containing it (emphasis, strikethrough, code spans,
//!    links and images, autolinks, math, inline HTML): first their content
//!    inside the delimiters (a link's text), then the whole span;
//! 3. the leaf block: its text (a heading without its `#` marker, the code
//!    of a code block), then the whole block;
//! 4. the containers around it: list item, list, block quote, innermost
//!    first;
//! 5. the heading sections around it, innermost first;
//! 6. the whole document.
//!
//! Expand selects the smallest step that strictly contains the selection.
//! Shrink goes back through the expansions made since the selections last
//! changed otherwise ([`SelectionHistory`]); without such a history it
//! selects the largest step around the head that the selection strictly
//! contains.

use std::ops::Range;

use gv_syntax::inline::InlineKind;
use gv_text::Revision;

use crate::selection::SelectionSpec;

use super::context::ctx_at;
use super::doc::Doc;
use super::inline::inlines;
use super::line_ops::word_at;
use super::navigate::sections_at;

/// The selections before each expansion, so that shrink can go back.
#[derive(Clone, Debug, Default)]
pub struct SelectionHistory {
    /// The selections before each expansion, oldest first.
    stack: Vec<(Vec<SelectionSpec>, usize)>,
    /// The selections the last expand or shrink left, and the text's
    /// revision then: the stack applies only while they are unchanged.
    after: Option<(Revision, Vec<SelectionSpec>, usize)>,
}

impl SelectionHistory {
    /// Forgets the expansions.
    pub fn clear(&mut self) {
        self.stack.clear();
        self.after = None;
    }

    /// Whether the stack belongs to `current` at `revision`.
    fn applies(&self, revision: Revision, current: &(Vec<SelectionSpec>, usize)) -> bool {
        self.after.as_ref().is_some_and(|(r, specs, primary)| {
            *r == revision && *primary == current.1 && same(specs, &current.0)
        })
    }

    pub(crate) fn remember(&mut self, revision: Revision, current: (Vec<SelectionSpec>, usize)) {
        self.after = Some((revision, current.0, current.1));
    }
}

/// Whether two selection lists cover the same ranges in the same
/// directions.
fn same(a: &[SelectionSpec], b: &[SelectionSpec]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.tail == y.tail && x.head == y.head)
}

/// `range` with trailing whitespace and line endings left out (not before
/// its start).
fn trim_end(doc: &Doc, range: Range<usize>) -> Range<usize> {
    let text = doc.slice(range.clone());
    let kept = text.trim_end().len();
    range.start..range.start + kept
}

/// The content of an inline node: from its first child that is not a
/// delimiter to its last.
fn inner(node: &gv_syntax::inline::InlineNode<'_>) -> Option<Range<usize>> {
    let parts: Vec<Range<usize>> = node
        .children()
        .filter(|child| !matches!(child.kind(), InlineKind::Marker | InlineKind::Whitespace))
        .map(|child| child.range())
        .collect();
    Some(parts.first()?.start..parts.last()?.end)
}

/// Every step around the point or range `s..e`, unsorted.
fn steps(doc: &Doc, s: usize, e: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if let Some(word) = word_at(doc, s).filter(|word| word.start <= s && e <= word.end) {
        out.push(word);
    }
    let ctx = ctx_at(doc, s);
    if let Some(leaf) = &ctx.leaf {
        if leaf.is_inline() {
            let tree = inlines(doc, leaf);
            for node in tree.nodes() {
                let kind = node.kind();
                if kind.is_token() || kind == InlineKind::Text {
                    continue;
                }
                let range = node.range();
                if !(range.start <= s && e <= range.end) {
                    continue;
                }
                let content = match node.link() {
                    Some(link) => Some(link.text_range.clone()),
                    None => inner(&node),
                };
                out.extend(content);
                out.push(range);
            }
        }
        let content: Vec<Range<usize>> = leaf
            .lines
            .iter()
            .filter(|line| line.has_content)
            .map(|line| line.content.clone())
            .collect();
        if let (Some(first), Some(last)) = (content.first(), content.last()) {
            out.push(first.start..last.end);
        }
        out.push(trim_end(doc, leaf.range.clone()));
    }
    for level in ctx.chain.iter().rev() {
        out.push(trim_end(doc, level.range.clone()));
    }
    for section in sections_at(doc, s) {
        out.push(trim_end(doc, section));
    }
    out.push(0..doc.len());
    out
}

/// The next step out from the selection `spec`.
fn expanded(doc: &Doc, spec: SelectionSpec) -> SelectionSpec {
    let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
    let best = steps(doc, s, e)
        .into_iter()
        .filter(|r| r.start <= s && e <= r.end && r.len() > e - s)
        .min_by_key(Range::len);
    match best {
        Some(range) if spec.tail > spec.head => SelectionSpec::new(range.end, range.start),
        Some(range) => SelectionSpec::new(range.start, range.end),
        None => spec,
    }
}

/// The next step in from the selection `spec`, without history.
fn shrunk(doc: &Doc, spec: SelectionSpec) -> SelectionSpec {
    let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
    if s == e {
        return spec;
    }
    let h = spec.head.min(e.saturating_sub(1)).max(s);
    let best = steps(doc, h, h)
        .into_iter()
        .filter(|r| s <= r.start && r.end <= e && r.len() < e - s && !r.is_empty())
        .max_by_key(Range::len);
    match best {
        Some(range) if spec.tail > spec.head => SelectionSpec::new(range.end, range.start),
        Some(range) => SelectionSpec::new(range.start, range.end),
        None => SelectionSpec::caret(spec.head),
    }
}

/// Expands every selection one step. Returns whether one changed.
pub(crate) fn expand(doc: &mut Doc, history: &mut SelectionHistory, revision: Revision) -> bool {
    let before = (doc.sels.clone(), doc.primary);
    if !history.applies(revision, &before) {
        history.stack.clear();
    }
    let after: Vec<SelectionSpec> = before.0.iter().map(|&spec| expanded(doc, spec)).collect();
    if same(&after, &before.0) {
        return false;
    }
    history.stack.push(before);
    doc.sels = after;
    true
}

/// Shrinks every selection one step. Returns whether one changed.
pub(crate) fn shrink(doc: &mut Doc, history: &mut SelectionHistory, revision: Revision) -> bool {
    let current = (doc.sels.clone(), doc.primary);
    if history.applies(revision, &current)
        && let Some((specs, primary)) = history.stack.pop()
    {
        doc.sels = specs;
        doc.primary = primary;
        return true;
    }
    history.clear();
    let after: Vec<SelectionSpec> = current.0.iter().map(|&spec| shrunk(doc, spec)).collect();
    if same(&after, &current.0) {
        return false;
    }
    doc.sels = after;
    true
}

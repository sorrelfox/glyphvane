// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live Preview (FR-EDIT-17), phase 1: inline markers are hidden except
//! where a selection touches them.
//!
//! # Model
//!
//! Each leaf block gets *constructs* from the syntax tree: a byte range
//! that is revealed as a whole, and the marker ranges it hides while no
//! selection touches it (ends included):
//!
//! - strong, emphasis, strikethrough, highlight, code spans, inline `%%`
//!   comments and `<…>` autolinks: their delimiters;
//! - links: `[` and everything from the closing `]` on (`](url "title")`,
//!   `][label]`, `]`); the text stays, styled as a link;
//! - wikilinks: `[[` and `]]`, and with an alias also `target|`;
//! - ATX headings: the `#` run with the spaces after it, and a closing run;
//! - `%%` block comments: the opening and closing `%%`.
//!
//! Images, embeds, math and the like stay source until inline widgets come
//! (phase 2). A construct whose hidden text would hold a line break is left
//! alone: hiding it would join two lines.
//!
//! # Updating
//!
//! The hidden ranges live in the display map's replace layer
//! ([`gv_editor::Editor::splice_replacements`]), so rows, carets, hit-tests
//! and soft wrap see the text without them; an edit that touches one drops
//! it there at once. Every frame (when the text, the tree, the selections
//! or the lines on screen changed) the controller works out the hidden
//! ranges of the lines on screen with a margin and splices the difference
//! against the layer. Decorations are cached per leaf, keyed by its text,
//! kind and the parse inputs, so only blocks that changed are decorated
//! again. Once the tree is the complete parse of the text, the whole
//! document is decorated on the background executor and diffed once, so
//! row counts (and the scrollbar) stop changing while scrolling.
//!
//! Live Preview is never used in plain-text mode or large-file mode.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{AppContext as _, Context, Task};
use gv_editor::display_map::Replacement;
use gv_syntax::inline::{InlineKind, InlineNode};
use gv_syntax::{Leaf, LeafKind, SyntaxTree, TokenKind, Visitor, walk, walk_range};
use gv_text::{Revision, TextSnapshot, TextSource as _};

use super::EditorView;

/// Lines decorated above and below the ones on screen.
pub(super) const MARGIN_LINES: usize = 80;

/// How long the text rests before the whole document is decorated again.
pub(super) const FULL_DELAY: Duration = Duration::from_millis(150);

/// Leaves kept in the cache at most (the entries a whole-document pass did
/// not use go at the next one).
const CACHE_CAP: usize = 200_000;

/// Text revealed as a whole, with the markers it hides otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Construct {
    /// The whole construct: a selection that touches it (ends included)
    /// reveals it.
    pub range: Range<usize>,
    /// What it hides while not revealed: sorted, not overlapping, each on
    /// one line.
    pub hidden: Vec<Range<usize>>,
}

/// The constructs of one leaf, relative to its start.
type Decor = Arc<[Construct]>;

/// Decorations of leaves seen before, by key ([`leaf_key`]).
#[derive(Debug, Default)]
struct DecorCache {
    current: HashMap<u64, Decor>,
    previous: HashMap<u64, Decor>,
}

impl DecorCache {
    fn get(&mut self, key: u64) -> Option<Decor> {
        if let Some(hit) = self.current.get(&key) {
            return Some(Arc::clone(hit));
        }
        let hit = self.previous.remove(&key)?;
        self.current.insert(key, Arc::clone(&hit));
        Some(hit)
    }

    fn insert(&mut self, key: u64, decor: Decor) {
        if self.current.len() > CACHE_CAP {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(key, decor);
    }

    /// After a whole-document pass: what it did not use goes at the next.
    fn finish(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }
}

/// The decorations of every leaf of one text, by leaf start.
#[derive(Debug)]
struct Full {
    revision: Revision,
    leaves: Vec<(usize, Decor)>,
    /// Applied to the whole document once.
    applied: bool,
}

/// What the last refresh worked from: nothing changed, nothing to do.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefreshKey {
    revision: Revision,
    generation: u64,
    selections: u64,
    window: Range<usize>,
    full: bool,
}

/// The Live Preview state of a view.
#[derive(Debug, Default)]
pub(super) struct LivePreview {
    /// The user chose Live Preview for this tab (it may still be inactive:
    /// plain text, large file).
    pub chosen: bool,
    cache: Arc<Mutex<DecorCache>>,
    full: Option<Full>,
    last: Option<RefreshKey>,
    /// The revision a whole-document pass runs or waits for.
    job_for: Option<Revision>,
    _job: Option<Task<()>>,
}

impl LivePreview {
    /// A new document (or Live Preview turned on or off): everything is
    /// worked out again; the decoration cache stays.
    pub fn reset(&mut self) {
        self.full = None;
        self.last = None;
        self.job_for = None;
        self._job = None;
    }
}

// ----- decorations ---------------------------------------------------------------

/// A key for the decorations of `leaf`: its kind, the lines' content
/// ranges and text (relative to it), the parse options and the link
/// definitions. `buf` gets the leaf's text.
fn leaf_key(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    leaf: &Leaf<'_>,
    refs: u64,
    buf: &mut Vec<u8>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind_code(leaf.kind).hash(&mut hasher);
    tree.options().hash(&mut hasher);
    refs.hash(&mut hasher);
    let start = leaf.range.start;
    for line in leaf.lines {
        (
            line.content.start - start,
            line.content.len(),
            line.line_ending.len(),
        )
            .hash(&mut hasher);
    }
    buf.clear();
    text.copy_range(leaf.range.clone(), buf);
    buf.hash(&mut hasher);
    hasher.finish()
}

/// The leaf kinds that have constructs, as numbers for keys.
fn kind_code(kind: LeafKind) -> Option<(u8, u8)> {
    match kind {
        LeafKind::Paragraph => Some((0, 0)),
        LeafKind::Heading { level, setext } => Some((1 + u8::from(setext), level)),
        LeafKind::CalloutTitle => Some((3, 0)),
        LeafKind::Comment => Some((4, 0)),
        _ => None,
    }
}

/// A fingerprint of the tree's link definitions (reference links depend
/// on them): 0 without any.
fn refs_fingerprint(tree: &SyntaxTree, text: &TextSnapshot) -> u64 {
    let refs = tree.ref_map(text);
    if refs.is_empty() && refs.footnote_count() == 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    (
        Arc::as_ptr(&refs) as usize,
        refs.len(),
        refs.footnote_count(),
    )
        .hash(&mut hasher);
    hasher.finish() | 1
}

/// The constructs of `leaf` (absolute offsets), its text in `bytes`.
pub(crate) fn decorate_leaf(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    leaf: &Leaf<'_>,
    bytes: &[u8],
) -> Vec<Construct> {
    let mut out = Vec::new();
    let base = leaf.range.start;
    let one_line = |range: &Range<usize>| {
        bytes
            .get(range.start.wrapping_sub(base)..range.end.wrapping_sub(base))
            .is_some_and(|b| !b.contains(&b'\n') && !b.contains(&b'\r'))
    };
    let mut push = |range: Range<usize>, hidden: Vec<Range<usize>>| {
        let mut hidden: Vec<Range<usize>> = hidden.into_iter().filter(|r| !r.is_empty()).collect();
        if hidden.is_empty() || !hidden.iter().all(one_line) {
            return;
        }
        hidden.sort_by_key(|r| r.start);
        if hidden.windows(2).any(|p| p[0].end > p[1].start) {
            return;
        }
        out.push(Construct { range, hidden });
    };
    match leaf.kind {
        LeafKind::Heading { setext: false, .. } => {
            if let Some((range, hidden)) = heading_markers(tree, leaf) {
                push(range, hidden);
            }
            inlines(tree, text, leaf, &mut push);
        }
        LeafKind::Heading { setext: true, .. } | LeafKind::Paragraph | LeafKind::CalloutTitle => {
            inlines(tree, text, leaf, &mut push);
        }
        LeafKind::Comment => {
            let content = leaf.lines.first().map(|l| l.content.start).unwrap_or(base);
            let end = leaf.lines.last().map_or(leaf.range.end, |l| l.content.end);
            let at = |offset: usize| bytes.get(offset.wrapping_sub(base)..).unwrap_or_default();
            let mut hidden = Vec::new();
            if at(content).starts_with(b"%%") {
                hidden.push(content..content + 2);
            }
            if end >= content + 4 && at(end - 2).starts_with(b"%%") {
                hidden.push(end - 2..end);
            }
            push(base..end, hidden);
        }
        _ => {}
    }
    out.sort_by_key(|c| (c.range.start, std::cmp::Reverse(c.range.end)));
    out
}

/// The `#` runs of an ATX heading: the opening run with the spaces after
/// it, and a closing run with the spaces before it.
fn heading_markers(
    tree: &SyntaxTree,
    leaf: &Leaf<'_>,
) -> Option<(Range<usize>, Vec<Range<usize>>)> {
    let line = leaf.lines.first()?;
    let line_end = line.line_ending.start;
    let line_start = leaf.range.start;
    let markers: Vec<Range<usize>> = tree
        .tokens(line_start..line_end)
        .into_iter()
        .filter(|(_, token)| token.kind == TokenKind::Marker)
        .map(|(range, _)| range)
        .collect();
    let open = markers.first()?.clone();
    let content = &line.content;
    let mut hidden = Vec::new();
    if content.is_empty() {
        hidden.push(open.start..line_end);
    } else {
        hidden.push(open.start..content.start.max(open.end));
        if let Some(close) = markers.iter().skip(1).find(|m| m.start >= content.end) {
            hidden.push(content.end..close.end);
        }
    }
    Some((line_start..line_end, hidden))
}

/// The inline constructs of `leaf`.
fn inlines(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    leaf: &Leaf<'_>,
    push: &mut dyn FnMut(Range<usize>, Vec<Range<usize>>),
) {
    let inline = tree.inlines(leaf, text);
    for node in inline.top_level() {
        inline_node(node, push);
    }
}

fn inline_node(node: InlineNode<'_>, push: &mut dyn FnMut(Range<usize>, Vec<Range<usize>>)) {
    let range = node.range();
    let markers = || -> Vec<Range<usize>> {
        node.children()
            .filter(|c| c.kind() == InlineKind::Marker)
            .map(|c| c.range())
            .collect()
    };
    match node.kind() {
        InlineKind::Strong
        | InlineKind::Emphasis
        | InlineKind::Strikethrough
        | InlineKind::Highlight
        | InlineKind::CodeSpan
        | InlineKind::Comment => push(range, markers()),
        InlineKind::Autolink => {
            // A GFM extended autolink has no markers.
            push(range, markers());
        }
        InlineKind::Link => {
            if let Some(text) = link_text(&node) {
                push(
                    range.clone(),
                    vec![range.start..text.start, text.end..range.end],
                );
            }
        }
        InlineKind::Wikilink => {
            let alias = node
                .children()
                .find(|c| c.kind() == InlineKind::WikiAlias)
                .map(|c| c.range());
            match alias {
                Some(alias) => push(
                    range.clone(),
                    vec![range.start..alias.start, alias.end..range.end],
                ),
                None => push(range, markers()),
            }
            return;
        }
        // Inline widgets come in phase 2: their source stays.
        InlineKind::Image
        | InlineKind::Embed
        | InlineKind::InlineMath
        | InlineKind::DisplayMath
        | InlineKind::RawHtml => return,
        _ => {}
    }
    for child in node.children() {
        if !child.kind().is_token() {
            inline_node(child, push);
        }
    }
}

/// The text of a link: between its `[` and its closing `]`.
pub(crate) fn link_text(node: &InlineNode<'_>) -> Option<Range<usize>> {
    let mut children = node.children();
    let open = children.next().filter(|c| c.kind() == InlineKind::Marker)?;
    let close = children.find(|c| c.kind() == InlineKind::Marker && c.text() == "]")?;
    Some(open.range().end..close.range().start)
}

/// Collects the decorations of the leaves a walk visits, from the cache
/// or worked out.
struct Collect<'a> {
    tree: &'a SyntaxTree,
    text: &'a TextSnapshot,
    cache: &'a Mutex<DecorCache>,
    refs: u64,
    buf: Vec<u8>,
    leaves: Vec<(usize, Decor)>,
}

impl Visitor for Collect<'_> {
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if kind_code(leaf.kind).is_none() {
            return;
        }
        let key = leaf_key(self.tree, self.text, leaf, self.refs, &mut self.buf);
        let hit = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key);
        let decor = match hit {
            Some(decor) => decor,
            None => {
                let base = leaf.range.start;
                let relative = |r: &Range<usize>| r.start - base..r.end - base;
                let decor: Decor = decorate_leaf(self.tree, self.text, leaf, &self.buf)
                    .into_iter()
                    .map(|c| Construct {
                        range: relative(&c.range),
                        hidden: c.hidden.iter().map(relative).collect(),
                    })
                    .collect();
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key, Arc::clone(&decor));
                decor
            }
        };
        if !decor.is_empty() {
            self.leaves.push((leaf.range.start, decor));
        }
    }
}

/// The decorations of the leaves of `range` (the whole text with `None`),
/// from `cache` or worked out, as `(leaf start, constructs relative to
/// it)` sorted by start.
fn decorate(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    range: Option<Range<usize>>,
    cache: &Mutex<DecorCache>,
) -> Vec<(usize, Decor)> {
    let mut collect = Collect {
        tree,
        text,
        cache,
        refs: refs_fingerprint(tree, text),
        buf: Vec::new(),
        leaves: Vec::new(),
    };
    match range {
        Some(range) => walk_range(tree, range, &mut collect),
        None => {
            walk(tree, &mut collect);
            cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .finish();
        }
    }
    let mut leaves = collect.leaves;
    leaves.sort_by_key(|(start, _)| *start);
    leaves.dedup_by_key(|(start, _)| *start);
    leaves
}

/// The hidden ranges of `leaves` (from [`decorate`]) inside `span`, with
/// the constructs a selection touches (ends included) revealed. Sorted, not
/// overlapping.
fn hidden_ranges(
    leaves: &[(usize, Decor)],
    span: &Range<usize>,
    selections: &[Range<usize>],
) -> Vec<Range<usize>> {
    let touched = |range: &Range<usize>| {
        let index = selections.partition_point(|s| s.end < range.start);
        selections.get(index).is_some_and(|s| s.start <= range.end)
    };
    let first = leaves.partition_point(|(start, _)| *start < span.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for (start, decor) in &leaves[first..] {
        if *start >= span.end {
            break;
        }
        for construct in decor.iter() {
            let range = start + construct.range.start..start + construct.range.end;
            if touched(&range) {
                continue;
            }
            out.extend(
                construct
                    .hidden
                    .iter()
                    .map(|r| start + r.start..start + r.end)
                    .filter(|r| r.end <= span.end),
            );
        }
    }
    out.sort_by_key(|r| (r.start, r.end));
    let mut kept: Vec<Range<usize>> = Vec::with_capacity(out.len());
    for range in out {
        if kept.last().is_none_or(|last| last.end <= range.start) {
            kept.push(range);
        }
    }
    kept
}

/// The splices that turn `actual` into `desired` (both sorted, not
/// overlapping): one per cluster of differing ranges.
pub(crate) fn splices_between(
    actual: &[Range<usize>],
    desired: &[Range<usize>],
) -> Vec<(Range<usize>, Vec<Replacement>)> {
    let mut differing: Vec<Range<usize>> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let key = |r: &Range<usize>| (r.start, r.end);
    loop {
        match (actual.get(i), desired.get(j)) {
            (None, None) => break,
            (Some(a), Some(d)) if a == d => {
                i += 1;
                j += 1;
            }
            (Some(a), Some(d)) if key(a) < key(d) => {
                differing.push(a.clone());
                i += 1;
            }
            (Some(a), None) => {
                differing.push(a.clone());
                i += 1;
            }
            (_, Some(d)) => {
                differing.push(d.clone());
                j += 1;
            }
        }
    }
    differing.sort_by_key(|r| (r.start, r.end));
    let mut clusters: Vec<Range<usize>> = Vec::new();
    for range in differing {
        match clusters.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => clusters.push(range),
        }
    }
    clusters
        .into_iter()
        .map(|cluster| {
            let from = desired.partition_point(|d| d.start < cluster.start);
            let inside = desired[from..]
                .iter()
                .take_while(|d| d.end <= cluster.end)
                .map(|d| Replacement::hide(d.clone()))
                .collect();
            (cluster, inside)
        })
        .collect()
}

// ----- the view ------------------------------------------------------------------

impl EditorView {
    /// Whether the user chose Live Preview for this view (the tab's
    /// editing style), active or not.
    pub fn live_preview(&self) -> bool {
        self.live.chosen
    }

    /// Whether Live Preview is shown: chosen, a Markdown document (not
    /// plain text) and not in large-file mode.
    pub fn is_live_preview_active(&self) -> bool {
        self.live.chosen
            && self.plain.is_none()
            && self.doc.as_ref().is_some_and(|doc| !doc.large_file)
    }

    /// Chooses Live Preview (`true`) or Source for this view.
    pub fn set_live_preview(&mut self, on: bool, cx: &mut Context<'_, Self>) {
        if self.live.chosen == on {
            return;
        }
        self.live.chosen = on;
        self.live_settings_changed();
        cx.notify();
    }

    /// Live Preview may have turned on or off (chosen, a new document,
    /// plain text): the highlighting follows, and hidden markers go when
    /// it is off.
    pub(super) fn live_settings_changed(&mut self) {
        let active = self.is_live_preview_active();
        self.live.reset();
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        doc.highlights.set_live(active);
        doc.rows = Default::default();
        if !active && doc.editor.display().replace_snapshot().has_replacements() {
            let len = doc.editor.text().len();
            doc.editor.splice_replacements([(0..len, Vec::new())]);
        }
    }

    /// The lines on screen, or around the line the next frame scrolls to.
    pub(super) fn live_window(&self, viewport: f32) -> Range<usize> {
        let Some(doc) = self.doc.as_ref() else {
            return 0..0;
        };
        let text = doc.editor.text();
        let rows_on_screen = (viewport / self.metrics.line).ceil().max(1.0) as usize + 1;
        let first = match self.pending_top {
            Some(super::Top::Offset(offset)) => {
                text.line_of_offset(offset.min(text.len())).unwrap_or(0)
            }
            Some(super::Top::Line(line)) => line,
            None => {
                let pad = f64::from(self.layout.pad_top());
                let row = ((self.scroll_y - pad) / f64::from(self.metrics.line))
                    .floor()
                    .max(0.0) as usize;
                self.row_map().map_or(row, |map| map.line_of_row(row))
            }
        };
        first..first + rows_on_screen
    }

    /// Brings the hidden markers of lines `lines` (the ones on screen) up
    /// to date: see the module docs. Cheap when nothing changed.
    pub(super) fn refresh_live_preview(&mut self, lines: Range<usize>, cx: &mut Context<'_, Self>) {
        if !self.is_live_preview_active() {
            return;
        }
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let text = doc.editor.snapshot();
        let revision = text.revision();
        let (tree, parsed) = doc.syntax.shown();
        if parsed.revision() != revision || tree.is_interim() {
            // Edits dropped the hidden ranges they touched; the reparse
            // that catches up comes back here.
            return;
        }
        let tree = tree.clone();
        let generation = doc.syntax.generation();
        let selections: Vec<Range<usize>> =
            doc.editor.selections().iter().map(|s| s.range()).collect();
        let mut hasher = DefaultHasher::new();
        selections.hash(&mut hasher);
        let selection_key = hasher.finish();

        let line_count = text.line_count();
        let first = lines
            .start
            .saturating_sub(MARGIN_LINES)
            .min(line_count.saturating_sub(1));
        let last = (lines.end + MARGIN_LINES).min(line_count.saturating_sub(1));
        let window = text.line_start(first).unwrap_or(0)
            ..text.line_range(last).map_or(text.len(), |r| r.end);

        let full = self
            .live
            .full
            .as_ref()
            .filter(|full| full.revision == revision);
        let whole = full.is_some_and(|full| !full.applied);
        let key = RefreshKey {
            revision,
            generation,
            selections: selection_key,
            window: window.clone(),
            full: full.is_some(),
        };
        if self.live.last.as_ref() == Some(&key) && !whole {
            self.schedule_live_full(&tree, &text, cx);
            return;
        }

        let (leaves, span) = match full {
            Some(full) if whole => (full.leaves.clone(), 0..text.len()),
            Some(full) => {
                // The leaves around the window, from the whole-document
                // decorations.
                let from = full
                    .leaves
                    .partition_point(|(start, _)| *start < window.start);
                let from = from.saturating_sub(1);
                let to = full
                    .leaves
                    .partition_point(|(start, _)| *start < window.end);
                let leaves = full.leaves[from..to].to_vec();
                let start = leaves
                    .first()
                    .map_or(window.start, |(s, _)| *s)
                    .min(window.start);
                (leaves, start..window.end)
            }
            None => {
                let leaves = decorate(&tree, &text, Some(window.clone()), &self.live.cache);
                let start = leaves
                    .first()
                    .map_or(window.start, |(s, _)| *s)
                    .min(window.start);
                (leaves, start..window.end)
            }
        };
        // A leaf that starts in the span may end after it.
        let span = span.start
            ..leaves
                .iter()
                .map(|(start, decor)| decor.iter().map(|c| start + c.range.end).max().unwrap_or(0))
                .max()
                .unwrap_or(0)
                .max(span.end);
        let desired = hidden_ranges(&leaves, &span, &selections);
        let actual: Vec<Range<usize>> = doc
            .editor
            .replacements_in(span.clone())
            .into_iter()
            .filter(|r| {
                r.widget.is_none() && span.start <= r.range.start && r.range.end <= span.end
            })
            .map(|r| r.range)
            .collect();
        let splices = splices_between(&actual, &desired);
        if !splices.is_empty() {
            doc.editor.splice_replacements(splices);
        }
        if let Some(full) = self.live.full.as_mut().filter(|_| whole) {
            full.applied = true;
        }
        self.live.last = Some(key);
        self.schedule_live_full(&tree, &text, cx);
    }

    /// Decorates the whole document on the background executor once the
    /// text has rested, unless that is done or running for this text.
    fn schedule_live_full(
        &mut self,
        tree: &SyntaxTree,
        text: &TextSnapshot,
        cx: &mut Context<'_, Self>,
    ) {
        let revision = text.revision();
        if self.live.job_for == Some(revision)
            || self
                .live
                .full
                .as_ref()
                .is_some_and(|f| f.revision == revision)
        {
            return;
        }
        self.live.job_for = Some(revision);
        let (tree, text) = (tree.clone(), text.clone());
        let cache = Arc::clone(&self.live.cache);
        self.live._job = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FULL_DELAY).await;
            let leaves = cx
                .background_spawn(async move { decorate(&tree, &text, None, &cache) })
                .await;
            this.update(cx, |view, cx| {
                if view.live.job_for != Some(revision) {
                    return;
                }
                view.live.job_for = None;
                view.live._job = None;
                view.live.full = Some(Full {
                    revision,
                    leaves,
                    applied: false,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// The hidden ranges of the display map (for tests).
    #[cfg(test)]
    pub(crate) fn live_hidden(&self) -> Vec<Range<usize>> {
        self.doc.as_ref().map_or_else(Vec::new, |doc| {
            let len = doc.editor.text().len();
            doc.editor
                .replacements_in(0..len)
                .into_iter()
                .map(|r| r.range)
                .collect()
        })
    }
}

/// Timings of decorating a document (the ignored timing test).
#[cfg(test)]
pub(crate) fn bench(tree: &SyntaxTree, text: &TextSnapshot) -> String {
    use std::time::{Duration, Instant};
    let cache = Mutex::new(DecorCache::default());
    let time = |f: &mut dyn FnMut()| {
        let start = Instant::now();
        f();
        start.elapsed()
    };
    let mut leaves = Vec::new();
    let cold = time(&mut || leaves = decorate(tree, text, None, &cache));
    let warm = time(&mut || leaves = decorate(tree, text, None, &cache));
    let mut hidden = Vec::new();
    let ranges = time(&mut || hidden = hidden_ranges(&leaves, &(0..text.len()), &[]));
    let mut editor = gv_editor::Editor::new(gv_text::Buffer::new(&text.to_string()));
    let splice = time(&mut || {
        editor.splice_replacements(splices_between(&[], &hidden));
    });
    // Caret moves mid-document: decorate a window of about 200 lines from
    // the cache, reveal, diff and splice, as a frame does.
    let mid = text.len() / 2;
    let mut moves: Vec<Duration> = Vec::new();
    for step in 0..200 {
        let caret = text.clip_offset(mid + step * 37, gv_text::Bias::Left);
        let window = caret.saturating_sub(8_000)..(caret + 8_000).min(text.len());
        moves.push(time(&mut || {
            let leaves = decorate(tree, text, Some(window.clone()), &cache);
            let span = leaves
                .first()
                .map_or(window.start, |(s, _)| *s)
                .min(window.start)..window.end;
            let desired = hidden_ranges(&leaves, &span, std::slice::from_ref(&(caret..caret)));
            let actual: Vec<Range<usize>> = editor
                .replacements_in(span.clone())
                .into_iter()
                .filter(|r| span.start <= r.range.start && r.range.end <= span.end)
                .map(|r| r.range)
                .collect();
            editor.splice_replacements(splices_between(&actual, &desired));
        }));
    }
    moves.sort();
    format!(
        "{} bytes, {} leaves with constructs, {} hidden ranges\n\
         whole document: decorate cold {cold:?}, from cache {warm:?}, reveal {ranges:?}, splice into the layer {splice:?}\n\
         caret move + window refresh: p50 {:?}, p95 {:?}, max {:?}",
        text.len(),
        leaves.len(),
        hidden.len(),
        moves[moves.len() / 2],
        moves[moves.len() * 95 / 100],
        moves[moves.len() - 1],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_syntax::ParseOptions;

    /// The hidden ranges of `text` as the strings they hide, with the
    /// caret at `caret` (`None`: no selection).
    fn hidden(text: &str, options: ParseOptions, caret: Option<usize>) -> Vec<String> {
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, options);
        let cache = Mutex::new(DecorCache::default());
        let leaves = decorate(&tree, &snapshot, None, &cache);
        let selections: Vec<Range<usize>> = caret.map(|c| c..c).into_iter().collect();
        hidden_ranges(&leaves, &(0..text.len()), &selections)
            .into_iter()
            .map(|r| text[r].to_owned())
            .collect()
    }

    fn obsidian() -> ParseOptions {
        ParseOptions::from_profile(&gv_profile::compile_builtin(
            gv_profile::ProfileId::Obsidian,
        ))
    }

    #[test]
    fn emphasis_code_and_links() {
        let text = "a **b** _c_ `d` [e](http://x \"t\") [f][r] <http://g>\n\n[r]: /u\n";
        assert_eq!(
            hidden(text, ParseOptions::default(), None),
            [
                "**",
                "**",
                "_",
                "_",
                "`",
                "`",
                "[",
                "](http://x \"t\")",
                "[",
                "][r]",
                "<",
                ">"
            ]
        );
    }

    #[test]
    fn headings_by_atx_markers() {
        assert_eq!(
            hidden("## Title ##\n", ParseOptions::default(), None),
            ["## ", " ##"]
        );
        assert_eq!(
            hidden("# *x*\n", ParseOptions::default(), None),
            ["# ", "*", "*"]
        );
        // Setext headings keep their underline.
        assert!(hidden("Title\n===\n", ParseOptions::default(), None).is_empty());
    }

    #[test]
    fn obsidian_markers() {
        let text = "x ==h== ~~s~~ [[Note]] [[Note|alias]] %%c%%\n";
        assert_eq!(
            hidden(text, obsidian(), None),
            [
                "==", "==", "~~", "~~", "[[", "]]", "[[Note|", "]]", "%%", "%%"
            ]
        );
    }

    #[test]
    fn the_caret_reveals_the_constructs_it_touches() {
        let text = "a **b *c* d** e\n";
        // Inside the emphasis: it and the strong around it.
        assert!(hidden(text, ParseOptions::default(), Some(7)).is_empty());
        // At the end of the strong (ends count): the emphasis stays hidden.
        assert_eq!(hidden(text, ParseOptions::default(), Some(13)), ["*", "*"]);
        // Away from both.
        assert_eq!(hidden(text, ParseOptions::default(), Some(15)).len(), 4);
        // In the strong, outside the emphasis.
        assert_eq!(hidden(text, ParseOptions::default(), Some(4)), ["*", "*"]);
    }

    #[test]
    fn images_and_multiline_links_stay_source() {
        assert!(hidden("![alt](i.png)\n", ParseOptions::default(), None).is_empty());
        // The destination is on the next line: hiding it would join them.
        assert_eq!(
            hidden("[a](\n/u)\n", ParseOptions::default(), None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn splices_cover_only_differences() {
        let actual = [0..2, 5..7, 10..12];
        let desired = [0..2, 5..6, 10..12, 20..22];
        let splices = splices_between(&actual, &desired);
        let ranges: Vec<_> = splices
            .iter()
            .map(|(r, list)| (r.clone(), list.len()))
            .collect();
        assert_eq!(ranges, [(5..7, 1), (20..22, 1)]);
        assert!(splices_between(&desired, &desired).is_empty());
    }
}

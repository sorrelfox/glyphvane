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

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Task};
use gv_editor::display_map::Replacement;
use gv_syntax::inline::{InlineKind, InlineNode};
use gv_syntax::{Leaf, LeafKind, SyntaxTree, TokenKind, Visitor, walk, walk_range};
use gv_text::{Revision, TextSnapshot, TextSource as _};

use super::EditorView;
use super::live_widgets::{Sizes, WidgetClick, WidgetSpec, Widgets};

/// Lines decorated above and below the ones on screen.
pub(super) const MARGIN_LINES: usize = 80;

/// How long the text rests before the whole document is decorated again.
pub(super) const FULL_DELAY: Duration = Duration::from_millis(150);

/// Notes at most this long get their whole-document pass before their
/// first frame ([`EditorView::live_first_pass`]).
pub(super) const SMALL_NOTE_BYTES: usize = 64 * 1024;

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
    /// The widget it shows in place of a range while not revealed (on one
    /// line, apart from `hidden`).
    pub widget: Option<(Range<usize>, Arc<WidgetSpec>)>,
}

/// What a construct puts in the layer while not revealed: hidden bytes,
/// or a widget in place of them.
pub(crate) type Desired = (Range<usize>, Option<Arc<WidgetSpec>>);

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
    /// The whole-document splices still to apply, a slice per frame
    /// (applying them all at once takes too long on a large note), with
    /// what they were worked out for.
    pending: VecDeque<(Range<usize>, Vec<Replacement>)>,
    pending_for: Option<(u64, u64)>,
}

/// What the last refresh worked from: nothing changed, nothing to do.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefreshKey {
    revision: Revision,
    generation: u64,
    selections: u64,
    window: Range<usize>,
    full: bool,
    widgets: u64,
}

/// The Live Preview state of a view.
#[derive(Debug, Default)]
pub(super) struct LivePreview {
    /// The user chose Live Preview for this tab (it may still be inactive:
    /// plain text, large file).
    pub chosen: bool,
    /// The profile's options decorations follow.
    pub options: Options,
    cache: Arc<Mutex<DecorCache>>,
    full: Option<Full>,
    last: Option<RefreshKey>,
    /// The revision a whole-document pass runs or waits for.
    job_for: Option<Revision>,
    _job: Option<Task<()>>,
    /// Inline widgets: images, math, checkboxes, emoji, tags.
    pub widgets: Widgets,
    /// The search a click on a tag asked for (`tag:#name`), taken by the
    /// vault search.
    tag_search: std::cell::RefCell<Option<Arc<str>>>,
    /// Clickable widgets drawn in the last frame: bounds and id.
    pub hits: Vec<(gpui::Bounds<gpui::Pixels>, gv_editor::display_map::WidgetId)>,
    /// Block widgets ([`super::live_blocks`]).
    pub blocks: super::live_blocks::LiveBlocks,
    /// The blocks drawn in the last frame: id, bounds and view.
    pub blocks_drawn: Vec<(
        gv_editor::display_map::BlockId,
        gpui::Bounds<gpui::Pixels>,
        gpui::Entity<crate::reader::Reader>,
    )>,
    /// A press went to a block's view: the editor takes the focus back
    /// when it ends.
    pub refocus: bool,
    /// The profile block views read their source with.
    pub profile: Option<Arc<gv_profile::CompiledProfile>>,
}

impl LivePreview {
    /// A new document (or Live Preview turned on or off): everything is
    /// worked out again; the decoration cache stays.
    pub fn reset(&mut self) {
        self.full = None;
        self.last = None;
        self.job_for = None;
        self._job = None;
        self.widgets.reset();
        self.blocks.invalidate();
    }
}

/// How long a frame may spend on whole-document splices.
const SLICE_BUDGET: Duration = Duration::from_millis(6);

/// Whole-document splices applied together, between two looks at the
/// clock.
const SLICE_SPLICES: usize = 256;

// ----- decorations ---------------------------------------------------------------

/// A key for the decorations of `leaf`: its kind, the lines' content
/// ranges and text (relative to it), the parse options and the link
/// definitions. `buf` gets the leaf's text.
fn leaf_key(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    leaf: &Leaf<'_>,
    refs: u64,
    options: Options,
    buf: &mut Vec<u8>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind_code(leaf.kind).hash(&mut hasher);
    options.hash(&mut hasher);
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

/// What decorations depend on besides the tree and the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Options {
    /// The profile has `:shortcode:` emoji.
    pub emoji: bool,
}

/// A construct found in a leaf: its range, what it hides and the widget
/// it shows.
type Found = (
    Range<usize>,
    Vec<Range<usize>>,
    Option<(Range<usize>, WidgetSpec)>,
);

/// The constructs of `leaf` (absolute offsets), its text in `bytes`.
pub(crate) fn decorate_leaf(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    leaf: &Leaf<'_>,
    bytes: &[u8],
    options: Options,
) -> Vec<Construct> {
    let mut out = Vec::new();
    let base = leaf.range.start;
    let one_line = |range: &Range<usize>| {
        bytes
            .get(range.start.wrapping_sub(base)..range.end.wrapping_sub(base))
            .is_some_and(|b| !b.contains(&b'\n') && !b.contains(&b'\r'))
    };
    let mut push = |(range, hidden, widget): Found| {
        let mut hidden: Vec<Range<usize>> = hidden.into_iter().filter(|r| !r.is_empty()).collect();
        let widget = widget.filter(|(r, _)| !r.is_empty());
        if (hidden.is_empty() && widget.is_none())
            || !hidden.iter().all(one_line)
            || widget.as_ref().is_some_and(|(r, _)| !one_line(r))
        {
            return;
        }
        if let Some((r, _)) = &widget {
            hidden.push(r.clone());
        }
        hidden.sort_by_key(|r| r.start);
        if hidden.windows(2).any(|p| p[0].end > p[1].start) {
            return;
        }
        if let Some((r, _)) = &widget {
            hidden.retain(|h| h != r);
        }
        out.push(Construct {
            range,
            hidden,
            widget: widget.map(|(r, spec)| (r, Arc::new(spec))),
        });
    };
    match leaf.kind {
        LeafKind::Heading { setext: false, .. } => {
            if let Some((range, hidden)) = heading_markers(tree, leaf) {
                push((range, hidden, None));
            }
            inlines(tree, text, leaf, options, &mut push);
        }
        LeafKind::Heading { setext: true, .. } | LeafKind::Paragraph | LeafKind::CalloutTitle => {
            inlines(tree, text, leaf, options, &mut push);
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
            push((base..end, hidden, None));
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
    options: Options,
    push: &mut dyn FnMut(Found),
) {
    let inline = tree.inlines(leaf, text);
    for node in inline.top_level() {
        inline_node(node, options, push);
    }
}

/// Image file extensions an embed shows inline.
const IMAGE_EXTENSIONS: [&str; 10] = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "tif", "tiff",
];

/// Whether an embed's path names an image file.
fn is_image_path(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        IMAGE_EXTENSIONS
            .iter()
            .any(|e| e.eq_ignore_ascii_case(ext.trim()))
    })
}

/// The `:shortcode:` emoji of a text token: each code's range and emoji.
fn emoji_in(text: &str, start: usize) -> Vec<(Range<usize>, &'static str)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(open) = bytes[i..].iter().position(|&b| b == b':').map(|p| p + i) {
        let code_len = bytes[open + 1..]
            .iter()
            .take_while(|&&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_+-".contains(&b))
            .count();
        let close = open + 1 + code_len;
        if code_len > 0
            && bytes.get(close) == Some(&b':')
            && let Some(emoji) = emojis::get_by_shortcode(&text[open + 1..close])
        {
            out.push((start + open..start + close + 1, emoji.as_str()));
            i = close + 1;
            continue;
        }
        i = open + 1;
    }
    out
}

fn inline_node(node: InlineNode<'_>, options: Options, push: &mut dyn FnMut(Found)) {
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
        | InlineKind::Comment => push((range, markers(), None)),
        InlineKind::Autolink => {
            // A GFM extended autolink has no markers.
            push((range, markers(), None));
        }
        InlineKind::Link => {
            if let Some(text) = link_text(&node) {
                push((
                    range.clone(),
                    vec![range.start..text.start, text.end..range.end],
                    None,
                ));
            }
        }
        InlineKind::Text if options.emoji => {
            for (code, glyph) in emoji_in(node.text(), range.start) {
                push((
                    code.clone(),
                    Vec::new(),
                    Some((code, WidgetSpec::Emoji { glyph })),
                ));
            }
            return;
        }
        InlineKind::Tag => {
            let spec = WidgetSpec::Tag {
                text: node.text().into(),
            };
            push((range.clone(), Vec::new(), Some((range, spec))));
            return;
        }
        InlineKind::InlineMath => {
            if let Some(tex) = node.math() {
                let spec = WidgetSpec::Math {
                    tex: tex.into(),
                    source: node.text().into(),
                };
                push((range.clone(), Vec::new(), Some((range, spec))));
            }
            return;
        }
        InlineKind::Image => {
            if let Some(link) = node.link() {
                let alt = node
                    .text()
                    .get(link.text_range.start - range.start..link.text_range.end - range.start)
                    .unwrap_or_default();
                let (alt, width) = match alt.rsplit_once('|') {
                    Some((a, size)) => match gv_syntax::inline::image_size(size) {
                        Some((w, _)) => (a, Some(w)),
                        None => (alt, None),
                    },
                    None => (alt, None),
                };
                let spec = WidgetSpec::Image {
                    destination: link.destination.into(),
                    embed: false,
                    alt: alt.into(),
                    width,
                };
                push((range.clone(), Vec::new(), Some((range, spec))));
            }
            return;
        }
        InlineKind::Embed => {
            if let Some(wiki) = node.wikilink()
                && is_image_path(wiki.path())
            {
                let width = wiki.size().map(|(w, _)| w);
                let alt = match (wiki.alias, width) {
                    (Some(alias), None) => alias,
                    _ => "",
                };
                let spec = WidgetSpec::Image {
                    destination: wiki.path().into(),
                    embed: true,
                    alt: alt.into(),
                    width,
                };
                push((range.clone(), Vec::new(), Some((range, spec))));
            }
            return;
        }
        InlineKind::Wikilink => {
            let alias = node
                .children()
                .find(|c| c.kind() == InlineKind::WikiAlias)
                .map(|c| c.range());
            match alias {
                Some(alias) => push((
                    range.clone(),
                    vec![range.start..alias.start, alias.end..range.end],
                    None,
                )),
                None => push((range, markers(), None)),
            }
            return;
        }
        // Display math is a block widget (phase 3): its source stays.
        InlineKind::DisplayMath | InlineKind::RawHtml => return,
        _ => {}
    }
    for child in node.children() {
        if !child.kind().is_token() || child.kind() == InlineKind::Text {
            inline_node(child, options, push);
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
    options: Options,
    buf: Vec<u8>,
    leaves: Vec<(usize, Decor)>,
}

impl Visitor for Collect<'_> {
    fn enter(&mut self, container: &gv_syntax::Container) {
        // A task's checkbox, as an entry of its own at the `[`.
        let Some(marker) = container.marker.clone().filter(|_| container.is_task()) else {
            return;
        };
        let mut status = Vec::new();
        self.text.copy_range(
            marker.start + 1..(marker.start + 2).min(marker.end),
            &mut status,
        );
        let checked = !matches!(status.as_slice(), b" " | b"\t");
        if marker.len() != 3 {
            return;
        }
        let decor: Decor = Arc::from([Construct {
            range: 0..marker.len(),
            hidden: Vec::new(),
            widget: Some((0..marker.len(), Arc::new(WidgetSpec::Task { checked }))),
        }]);
        self.leaves.push((marker.start, decor));
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if kind_code(leaf.kind).is_none() {
            return;
        }
        let key = leaf_key(
            self.tree,
            self.text,
            leaf,
            self.refs,
            self.options,
            &mut self.buf,
        );
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
                let decor: Decor =
                    decorate_leaf(self.tree, self.text, leaf, &self.buf, self.options)
                        .into_iter()
                        .map(|c| Construct {
                            range: relative(&c.range),
                            hidden: c.hidden.iter().map(relative).collect(),
                            widget: c.widget.map(|(r, spec)| (relative(&r), spec)),
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
    options: Options,
) -> Vec<(usize, Decor)> {
    let mut collect = Collect {
        tree,
        text,
        cache,
        options,
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

/// What `leaves` (from [`decorate`]) put in the layer inside `span`, with
/// the constructs a selection touches (ends included) revealed, and the
/// widgets a caret is strictly inside or a selection overlaps: hidden
/// ranges and widgets, sorted, not overlapping.
fn hidden_ranges(
    leaves: &[(usize, Decor)],
    span: &Range<usize>,
    selections: &[Range<usize>],
) -> Vec<Desired> {
    let touched = |range: &Range<usize>| {
        let index = selections.partition_point(|s| s.end < range.start);
        selections.get(index).is_some_and(|s| s.start <= range.end)
    };
    let inside = |range: &Range<usize>| {
        let index = selections.partition_point(|s| s.end < range.start);
        selections[index..]
            .iter()
            .take_while(|s| s.start <= range.end)
            .any(|s| {
                if s.is_empty() {
                    range.start < s.start && s.start < range.end
                } else {
                    s.start < range.end && s.end > range.start
                }
            })
    };
    let first = leaves.partition_point(|(start, _)| *start < span.start);
    let mut out: Vec<Desired> = Vec::new();
    for (start, decor) in &leaves[first..] {
        if *start >= span.end {
            break;
        }
        for construct in decor.iter() {
            let range = start + construct.range.start..start + construct.range.end;
            // An inline widget stays drawn while a caret is only next to
            // it: it shows its source when a caret is strictly inside or a
            // selection overlaps it. Markers show when anything touches
            // their construct, ends included.
            let revealed = if construct.widget.is_some() {
                inside(&range)
            } else {
                touched(&range)
            };
            if revealed {
                continue;
            }
            out.extend(
                construct
                    .hidden
                    .iter()
                    .map(|r| (start + r.start..start + r.end, None))
                    .chain(
                        construct.widget.iter().map(|(r, spec)| {
                            (start + r.start..start + r.end, Some(Arc::clone(spec)))
                        }),
                    )
                    .filter(|(r, _)| r.end <= span.end),
            );
        }
    }
    out.sort_by_key(|(r, _)| (r.start, r.end));
    let mut kept: Vec<Desired> = Vec::with_capacity(out.len());
    for item in out {
        if kept.last().is_none_or(|(last, _)| last.end <= item.0.start) {
            kept.push(item);
        }
    }
    kept
}

/// The splices that turn `actual` into `desired` (both sorted, not
/// overlapping): one per cluster of differing replacements.
pub(crate) fn splices_between(
    actual: &[Replacement],
    desired: &[Replacement],
) -> Vec<(Range<usize>, Vec<Replacement>)> {
    let mut differing: Vec<Range<usize>> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let key = |r: &Replacement| (r.range.start, r.range.end);
    loop {
        match (actual.get(i), desired.get(j)) {
            (None, None) => break,
            (Some(a), Some(d)) if a == d => {
                i += 1;
                j += 1;
            }
            (Some(a), Some(d)) if key(a) == key(d) => {
                differing.push(a.range.clone());
                i += 1;
                j += 1;
            }
            (Some(a), Some(d)) if key(a) < key(d) => {
                differing.push(a.range.clone());
                i += 1;
            }
            (Some(a), None) => {
                differing.push(a.range.clone());
                i += 1;
            }
            (_, Some(d)) => {
                differing.push(d.range.clone());
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
            let from = desired.partition_point(|d| d.range.start < cluster.start);
            let inside = desired[from..]
                .iter()
                .take_while(|d| d.range.end <= cluster.end)
                .cloned()
                .collect();
            (cluster, inside)
        })
        .collect()
}

/// Where widgets are made for a splice.
pub(super) struct WidgetEnv<'a> {
    pub sizes: &'a Sizes,
    /// Images are found through it.
    pub resolver: Option<&'a crate::reader::Resolver>,
    /// Images are decoded only inside it (the lines on screen with a
    /// margin).
    pub load: Range<usize>,
    /// Lines that are images on their own (block widgets): no inline
    /// widget there.
    pub lone_images: &'a [Range<usize>],
}

/// The splices that bring the layer to what `leaves` put in `span` with
/// `selections`, widgets made in `env`.
fn splices_for(
    editor: &gv_editor::Editor,
    leaves: &[(usize, Decor)],
    span: Range<usize>,
    selections: &[Range<usize>],
    widgets: &mut Widgets,
    env: &WidgetEnv<'_>,
) -> Vec<(Range<usize>, Vec<Replacement>)> {
    // A leaf that starts in the span may end after it.
    let span = span.start
        ..leaves
            .iter()
            .map(|(start, decor)| decor.iter().map(|c| start + c.range.end).max().unwrap_or(0))
            .max()
            .unwrap_or(0)
            .max(span.end);
    let lone = |range: &Range<usize>| {
        let index = env.lone_images.partition_point(|l| l.end < range.start);
        env.lone_images
            .get(index)
            .is_some_and(|l| l.start <= range.start && range.end <= l.end)
    };
    let desired: Vec<Replacement> = hidden_ranges(leaves, &span, selections)
        .into_iter()
        .filter(|(range, spec)| spec.is_none() || !lone(range))
        .map(|(range, spec)| match spec {
            None => Replacement::hide(range),
            Some(spec) => {
                let load = range.start < env.load.end && range.end > env.load.start;
                Replacement::widget(range, widgets.widget(&spec, env.sizes, env.resolver, load))
            }
        })
        .collect();
    let actual: Vec<Replacement> = editor
        .replacements_in(span.clone())
        .into_iter()
        .filter(|r| span.start <= r.range.start && r.range.end <= span.end)
        .collect();
    splices_between(&actual, &desired)
}

/// The images of `leaves` inside `window`.
fn image_keys(
    leaves: &[(usize, Decor)],
    window: &Range<usize>,
) -> std::collections::HashSet<super::live_widgets::ImageKey> {
    let mut keys = std::collections::HashSet::new();
    for (start, decor) in leaves {
        for construct in decor.iter() {
            let Some((range, spec)) = &construct.widget else {
                continue;
            };
            let (a, b) = (start + range.start, start + range.end);
            if let WidgetSpec::Image {
                destination, embed, ..
            } = spec.as_ref()
                && a < window.end
                && b > window.start
            {
                keys.insert((Arc::clone(destination), *embed));
            }
        }
    }
    keys
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

    /// The profile's switches: emoji shortcodes show as emoji with them.
    /// The profile block widgets read their source with.
    pub(super) fn set_live_profile(&mut self, profile: Arc<gv_profile::CompiledProfile>) {
        if self.live.profile.as_ref().is_some_and(|p| **p == *profile) {
            return;
        }
        self.live.profile = Some(profile);
        self.live.blocks.forget_views();
    }

    pub(super) fn set_live_switches(&mut self, switches: &gv_profile::Switches) {
        let options = Options {
            emoji: switches.emoji,
        };
        if self.live.options != options {
            self.live.options = options;
            self.live.reset();
        }
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
        self.clear_live_blocks();
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
                let rows = self.row_map().map_or(0, |map| map.row_count());
                let row = self.geo().row_at(self.scroll_y - pad, rows);
                self.row_map().map_or(row, |map| map.line_of_row(row))
            }
        };
        first..first + rows_on_screen
    }

    /// The bytes of `lines` with the decoration margin around them.
    pub(super) fn live_span(&self, lines: Range<usize>) -> Range<usize> {
        let Some(doc) = self.doc.as_ref() else {
            return 0..0;
        };
        let text = doc.editor.text();
        let line_count = text.line_count();
        let first = lines
            .start
            .saturating_sub(MARGIN_LINES)
            .min(line_count.saturating_sub(1));
        let last = (lines.end + MARGIN_LINES).min(line_count.saturating_sub(1));
        text.line_start(first).unwrap_or(0)..text.line_range(last).map_or(text.len(), |r| r.end)
    }

    /// Brings the hidden markers and widgets of lines `lines` (the ones on
    /// screen) up to date: see the module docs. Widgets are measured for
    /// `sizes`. Cheap when nothing changed.
    pub(super) fn refresh_live_preview(
        &mut self,
        lines: Range<usize>,
        sizes: &Sizes,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.is_live_preview_active() {
            return;
        }
        self.live.widgets.set_sizes(sizes);
        let resolver = self.links.resolver();
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
        let widgets_generation = {
            // Images on their own line have no inline widget: a change of
            // those lines is a change of the widgets.
            let mut hasher = DefaultHasher::new();
            self.live.widgets.generation().hash(&mut hasher);
            self.live.blocks.lone_images.hash(&mut hasher);
            hasher.finish()
        };

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
            widgets: widgets_generation,
        };
        if self.live.last.as_ref() != Some(&key) {
            let (leaves, span) = match full {
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
                    let leaves = decorate(
                        &tree,
                        &text,
                        Some(window.clone()),
                        &self.live.cache,
                        self.live.options,
                    );
                    let start = leaves
                        .first()
                        .map_or(window.start, |(s, _)| *s)
                        .min(window.start);
                    (leaves, start..window.end)
                }
            };
            let env = WidgetEnv {
                sizes,
                resolver,
                load: window.clone(),
                lone_images: &self.live.blocks.lone_images,
            };
            let splices = splices_for(
                &doc.editor,
                &leaves,
                span,
                &selections,
                &mut self.live.widgets,
                &env,
            );
            // Image jobs for what left the lines on screen go.
            self.live
                .widgets
                .keep_loading(&image_keys(&leaves, &window));
            if !splices.is_empty() {
                doc.editor.splice_replacements(splices);
            }
        }
        // The whole document, once per text, in slices over frames.
        if whole && let Some(full) = self.live.full.as_mut() {
            let pending_key = (selection_key, widgets_generation);
            if full.pending_for != Some(pending_key) {
                let env = WidgetEnv {
                    sizes,
                    resolver,
                    load: window.clone(),
                    lone_images: &self.live.blocks.lone_images,
                };
                full.pending = splices_for(
                    &doc.editor,
                    &full.leaves,
                    0..text.len(),
                    &selections,
                    &mut self.live.widgets,
                    &env,
                )
                .into();
                full.pending_for = Some(pending_key);
            }
            let started = Instant::now();
            while !full.pending.is_empty() && started.elapsed() < SLICE_BUDGET {
                let take = full.pending.len().min(SLICE_SPLICES);
                let slice: Vec<_> = full.pending.drain(..take).collect();
                doc.editor.splice_replacements(slice);
            }
            if full.pending.is_empty() {
                full.applied = true;
                full.pending_for = None;
            } else {
                cx.notify();
            }
        }
        self.live.last = Some(key);
        for job in self.live.widgets.take_jobs() {
            let key = job.key.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn(async move { super::live_widgets::load_image(&job) })
                    .await;
                this.update(cx, |view, cx| {
                    view.live.widgets.landed(&key, result);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        self.schedule_live_full(&tree, &text, cx);
    }

    /// A short note's first whole-document pass, before its first frame
    /// in Live Preview: every block is in the block layer from the start
    /// (at estimated heights), so the content height does not grow while
    /// scrolling into text the window pass has not seen. Longer notes wait
    /// for the background pass.
    pub(super) fn live_first_pass(&mut self) {
        if self.live.full.is_some() || self.live.blocks.full.is_some() {
            return;
        }
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let text = doc.editor.snapshot();
        if text.len() > SMALL_NOTE_BYTES {
            return;
        }
        let revision = text.revision();
        let (tree, parsed) = doc.syntax.shown();
        if parsed.revision() != revision || tree.is_interim() {
            return;
        }
        let tree = tree.clone();
        let leaves = decorate(&tree, &text, None, &self.live.cache, self.live.options);
        let context = super::live_context::NoteContext::compute(&tree, &text, true);
        let blocks = super::live_blocks::find_blocks(&tree, &text, None, Some(&context));
        self.live.blocks.full = Some((revision, Arc::new(blocks)));
        self.live.blocks.context = Some((revision, Arc::new(context)));
        self.live.full = Some(Full {
            revision,
            leaves,
            applied: false,
            pending: VecDeque::new(),
            pending_for: None,
        });
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
        let options = self.live.options;
        self.live._job = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FULL_DELAY).await;
            let (leaves, blocks, context) = cx
                .background_spawn(async move {
                    let leaves = decorate(&tree, &text, None, &cache, options);
                    let context = super::live_context::NoteContext::compute(&tree, &text, true);
                    let blocks =
                        super::live_blocks::find_blocks(&tree, &text, None, Some(&context));
                    (leaves, blocks, Arc::new(context))
                })
                .await;
            this.update(cx, |view, cx| {
                if view.live.job_for != Some(revision) {
                    return;
                }
                view.live.job_for = None;
                view.live._job = None;
                view.live.blocks.full = Some((revision, Arc::new(blocks)));
                view.live.blocks.context = Some((revision, context));
                view.live.full = Some(Full {
                    revision,
                    leaves,
                    applied: false,
                    pending: VecDeque::new(),
                    pending_for: None,
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// The search a click on a tag asked for (`tag:#name`), taken.
    pub fn take_tag_search(&self) -> Option<Arc<str>> {
        self.live.tag_search.borrow_mut().take()
    }

    /// A plain click at window point `position`: on a checkbox it toggles
    /// its task (one undoable edit), on a tag it runs the tag's search, on
    /// another widget it puts the caret inside it (its source shows).
    /// Returns whether the click was taken.
    pub(super) fn click_live_widget(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut gpui::Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        if !self.is_live_preview_active() {
            return false;
        }
        let Some(id) = self
            .live
            .hits
            .iter()
            .find(|(bounds, _)| bounds.contains(&position))
            .map(|(_, id)| *id)
        else {
            return false;
        };
        let Some((_, click)) = self.live.widgets.paint(id).cloned() else {
            return false;
        };
        let Some(offset) = self.offset_at(position, window, cx) else {
            return false;
        };
        let Some(range) = self.doc.as_ref().and_then(|doc| {
            doc.editor
                .replacements_in(offset..offset)
                .into_iter()
                .find(|r| r.widget.as_ref().is_some_and(|w| w.id == id))
                .map(|r| r.range)
        }) else {
            return false;
        };
        match click {
            WidgetClick::Task => {
                let Some(from) = self.doc.as_ref().and_then(|doc| {
                    let text = doc.editor.text().text_for_range(range.clone()).ok()?;
                    text.chars().nth(1)
                }) else {
                    return false;
                };
                let to = if from == ' ' { 'x' } else { ' ' };
                let toggle = crate::reader::TaskToggle {
                    offset: range.start + 1,
                    from,
                    to,
                };
                self.apply_task_toggle(toggle, cx)
            }
            WidgetClick::Tag(tag) => {
                *self.live.tag_search.borrow_mut() = Some(format!("tag:{tag}").into());
                window.dispatch_action(Box::new(crate::keymap::actions::search::Vault), cx);
                true
            }
            WidgetClick::Reveal => {
                // The caret goes inside it, which shows its source.
                let Some(doc) = self.doc.as_mut() else {
                    return false;
                };
                let first = doc
                    .editor
                    .text()
                    .text_for_range(range.clone())
                    .ok()
                    .and_then(|t| t.chars().next())
                    .map_or(0, char::len_utf8);
                doc.editor.set_cursor((range.start + first).min(range.end));
                self.autoscroll = true;
                true
            }
        }
    }

    /// Whether the whole document's splices are all applied (for tests).
    #[cfg(test)]
    pub(crate) fn live_settled(&self) -> bool {
        self.live.full.as_ref().is_some_and(|f| f.applied)
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
    fn env(sizes: &Sizes) -> WidgetEnv<'_> {
        WidgetEnv {
            sizes,
            resolver: None,
            load: 0..usize::MAX,
            lone_images: &[],
        }
    }
    use std::time::{Duration, Instant};
    let cache = Mutex::new(DecorCache::default());
    let options = Options { emoji: true };
    let time = |f: &mut dyn FnMut()| {
        let start = Instant::now();
        f();
        start.elapsed()
    };
    let sizes = Sizes {
        line: 22.0,
        font: 15.0,
        advance: 9.0,
        column: 700.0,
        scale: 1.0,
    };
    let mut widgets = Widgets::default();
    let mut leaves = Vec::new();
    let cold = time(&mut || leaves = decorate(tree, text, None, &cache, options));
    let warm = time(&mut || leaves = decorate(tree, text, None, &cache, options));
    let mut hidden = Vec::new();
    let ranges = time(&mut || hidden = hidden_ranges(&leaves, &(0..text.len()), &[]));
    let widget_count = hidden.iter().filter(|(_, w)| w.is_some()).count();
    let new_editor = |wrap: bool| {
        let mut editor = gv_editor::Editor::new(gv_text::Buffer::new(&text.to_string()));
        if wrap {
            editor.set_measure(Box::new(gv_editor::display_map::MonospaceMeasure::new(
                sizes.advance,
            )));
            editor.set_wrap_width(Some(sizes.column));
        }
        editor
    };
    let mut lines = Vec::new();
    for wrap in [false, true] {
        // At once, as phase 1 did.
        let mut editor = new_editor(wrap);
        let mut splices = Vec::new();
        let diff = time(&mut || {
            splices = splices_for(
                &editor,
                &leaves,
                0..text.len(),
                &[],
                &mut widgets,
                &env(&sizes),
            );
        });
        let once = time(&mut || {
            editor.splice_replacements(splices.clone());
        });
        // In slices, as frames do.
        let mut editor = new_editor(wrap);
        let mut pending: VecDeque<_> = splices.into();
        let mut frames: Vec<Duration> = Vec::new();
        while !pending.is_empty() {
            frames.push(time(&mut || {
                let started = Instant::now();
                while !pending.is_empty() && started.elapsed() < SLICE_BUDGET {
                    let take = pending.len().min(SLICE_SPLICES);
                    let slice: Vec<_> = pending.drain(..take).collect();
                    editor.splice_replacements(slice);
                }
            }));
        }
        let worst = frames.iter().max().copied().unwrap_or_default();
        lines.push(format!(
            "whole document ({}): diff {diff:?}; splice at once {once:?}; in {} frames of at most {worst:?}",
            if wrap { "soft wrap" } else { "no wrap" },
            frames.len()
        ));
    }
    // Caret moves mid-document: decorate a window of about 200 lines from
    // the cache, reveal, diff and splice, as a frame does.
    let mut editor = new_editor(true);
    let all = splices_for(
        &editor,
        &leaves,
        0..text.len(),
        &[],
        &mut widgets,
        &env(&sizes),
    );
    editor.splice_replacements(all);
    let mid = text.len() / 2;
    let mut moves: Vec<Duration> = Vec::new();
    for step in 0..200 {
        let caret = text.clip_offset(mid + step * 37, gv_text::Bias::Left);
        let window = caret.saturating_sub(8_000)..(caret + 8_000).min(text.len());
        moves.push(time(&mut || {
            let leaves = decorate(tree, text, Some(window.clone()), &cache, options);
            let span = leaves
                .first()
                .map_or(window.start, |(s, _)| *s)
                .min(window.start)..window.end;
            let splices = splices_for(
                &editor,
                &leaves,
                span,
                std::slice::from_ref(&(caret..caret)),
                &mut widgets,
                &env(&sizes),
            );
            editor.splice_replacements(splices);
        }));
    }
    moves.sort();
    format!(
        "{} bytes, {} leaves with constructs, {} replacements ({widget_count} widgets)\n\
         decorate cold {cold:?}, from cache {warm:?}, reveal {ranges:?}\n{}\n\
         caret move + window refresh (soft wrap): p50 {:?}, p95 {:?}, max {:?}",
        text.len(),
        leaves.len(),
        hidden.len(),
        lines.join("\n"),
        moves[moves.len() / 2],
        moves[moves.len() * 95 / 100],
        moves[moves.len() - 1],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_syntax::ParseOptions;

    /// The hidden ranges of `text` as the strings they hide (`w:` and
    /// the source for a widget), with the caret at `caret` (`None`: no
    /// selection).
    fn hidden(text: &str, options: ParseOptions, caret: Option<usize>) -> Vec<String> {
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, options);
        let cache = Mutex::new(DecorCache::default());
        let leaves = decorate(&tree, &snapshot, None, &cache, Options { emoji: true });
        let selections: Vec<Range<usize>> = caret.map(|c| c..c).into_iter().collect();
        hidden_ranges(&leaves, &(0..text.len()), &selections)
            .into_iter()
            .map(|(r, widget)| match widget {
                Some(_) => format!("w:{}", &text[r]),
                None => text[r].to_owned(),
            })
            .collect()
    }

    /// The widgets of `text` (with no selection).
    fn widgets(text: &str, options: ParseOptions) -> Vec<(String, WidgetSpec)> {
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, options);
        let cache = Mutex::new(DecorCache::default());
        let leaves = decorate(&tree, &snapshot, None, &cache, Options { emoji: true });
        hidden_ranges(&leaves, &(0..text.len()), &[])
            .into_iter()
            .filter_map(|(r, w)| Some((text[r].to_owned(), (*w?).clone())))
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
    fn multiline_links_stay_source() {
        // The destination is on the next line: hiding it would join them.
        assert_eq!(
            hidden("[a](\n/u)\n", ParseOptions::default(), None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn splices_cover_only_differences() {
        let hide = |list: &[Range<usize>]| -> Vec<Replacement> {
            list.iter().cloned().map(Replacement::hide).collect()
        };
        let actual = hide(&[0..2, 5..7, 10..12]);
        let desired = hide(&[0..2, 5..6, 10..12, 20..22]);
        let splices = splices_between(&actual, &desired);
        let ranges: Vec<_> = splices
            .iter()
            .map(|(r, list)| (r.clone(), list.len()))
            .collect();
        assert_eq!(ranges, [(5..7, 1), (20..22, 1)]);
        assert!(splices_between(&desired, &desired).is_empty());
    }

    #[test]
    fn widgets_of_images_math_tasks_emoji_and_tags() {
        let text = "- [ ] a ![cat|40](c.png) and ![[d.png|200]] ![[Note]]\n- [x] $x^2$ :smile: #tag `:smile:`\n";
        let found = widgets(text, obsidian());
        let sources: Vec<&str> = found.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(
            sources,
            [
                "[ ]",
                "![cat|40](c.png)",
                "![[d.png|200]]",
                "[x]",
                "$x^2$",
                ":smile:",
                "#tag"
            ]
        );
        assert_eq!(found[0].1, WidgetSpec::Task { checked: false });
        assert_eq!(
            found[1].1,
            WidgetSpec::Image {
                destination: "c.png".into(),
                embed: false,
                alt: "cat".into(),
                width: Some(40),
            }
        );
        assert_eq!(
            found[2].1,
            WidgetSpec::Image {
                destination: "d.png".into(),
                embed: true,
                alt: "".into(),
                width: Some(200),
            }
        );
        assert_eq!(found[3].1, WidgetSpec::Task { checked: true });
        assert!(matches!(&found[4].1, WidgetSpec::Math { tex, .. } if &**tex == "x^2"));
        assert_eq!(found[5].1, WidgetSpec::Emoji { glyph: "😄" });
        assert_eq!(
            found[6].1,
            WidgetSpec::Tag {
                text: "#tag".into()
            }
        );
        // Without emoji in the profile, shortcodes stay text.
        let snapshot = TextSnapshot::new(":smile:\n");
        let tree = SyntaxTree::parse_with(&snapshot, obsidian());
        let cache = Mutex::new(DecorCache::default());
        let leaves = decorate(&tree, &snapshot, None, &cache, Options { emoji: false });
        assert!(hidden_ranges(&leaves, &(0..8), &[]).is_empty());
    }

    #[test]
    fn a_widget_shows_its_source_with_a_caret_inside_only() {
        let text = "a $x$ b\n";
        assert_eq!(hidden(text, obsidian(), None), ["w:$x$"]);
        // Next to it (either end): still drawn.
        assert_eq!(hidden(text, obsidian(), Some(2)), ["w:$x$"]);
        assert_eq!(hidden(text, obsidian(), Some(5)), ["w:$x$"]);
        assert_eq!(hidden(text, obsidian(), Some(6)), ["w:$x$"]);
        // Strictly inside: source.
        assert!(hidden(text, obsidian(), Some(3)).is_empty());
        assert!(hidden(text, obsidian(), Some(4)).is_empty());
        // A selection that overlaps it shows its source; one that ends at
        // its start does not.
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, obsidian());
        let cache = Mutex::new(DecorCache::default());
        let leaves = decorate(&tree, &snapshot, None, &cache, Options { emoji: true });
        assert!(hidden_ranges(&leaves, &(0..8), std::slice::from_ref(&(1..3))).is_empty());
        assert_eq!(
            hidden_ranges(&leaves, &(0..8), std::slice::from_ref(&(0..2))).len(),
            1
        );
        // Markers keep the touching rule: a caret at a construct's end
        // shows them.
        assert!(hidden("**b** x\n", obsidian(), Some(5)).is_empty());
    }
}

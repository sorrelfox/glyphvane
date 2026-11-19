// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Obsidian blocks: callout titles and folds, and note embeds shown in
//! place (transclusion).
//!
//! A note embed is a paragraph holding only `![[Note]]`, `![[Note#Heading]]`
//! or `![[Note#^block]]`. Its own line stays (a link to the note, the
//! embed's header) and the embedded blocks follow inside a
//! [`FrameKind::Embed`] frame. The embedded note comes from the link
//! source ([`crate::LinkStatusSource::embed`]). Its blocks are built by a
//! builder of their own, like the document's: code with its language,
//! tables, callouts (folding), display math, diagrams, HTML blocks, images
//! and links resolved from the embedded note ([`crate::LinkInfo::from`],
//! [`crate::ImageInfo::from`]); headings are paragraphs in a heading's
//! type (not anchors of the document). Their sources are in another file:
//! every embedded node, and everything in it, maps to the end of the
//! embed's line, and every block gets a synthetic id. The footnotes the
//! embedded part refers to are numbered from 1 and listed at the embed's
//! end; inline footnotes show in place.
//! Embeds inside the embedded note are shown the same way, up to the
//! budgets: [`MAX_EMBED_DEPTH`] nested embeds, [`MAX_EMBED_NODES`] blocks
//! and [`MAX_EMBED_BYTES`] of embedded text per outermost embed. A cycle
//! (the same note and section again inside itself) shows a notice instead.
//!
//! A paragraph that is only a canvas or base embed is a live block instead
//! ([`crate::live`]); inside an embedded note, canvas and base embeds and
//! ` ```base ` and ` ```query ` fences are live blocks too, counted
//! against the depth limit.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use gv_model::{Anchoring, BlockMatch, DocModel, FragmentMatch};
use gv_syntax::inline::{InlineInput, InlineKind, InlineOptions, parse_inlines_with};
use gv_syntax::{BlockId, CalloutFold, Container, ContainerKind, Leaf, SyntaxTree, Visitor};
use gv_text::TextSource;

use super::{NodeBuilder, Open};
use crate::footnote::FootnoteIndex;
use crate::inline::{self, is_image_path};
use crate::live::{LiveBlock, LiveKind, split_embed};
use crate::node::{
    BlockNode, CalloutType, CodeBlock, Context, Frame, FrameKind, ListMarker, NodeKind,
    default_callout_title,
};
use crate::status::EmbedFetch;
use crate::style::TextRole;

/// Embeds nested deeper than this are not shown.
pub const MAX_EMBED_DEPTH: usize = 5;
/// Blocks one outermost embed shows at most.
pub const MAX_EMBED_NODES: usize = 10_000;
/// Embedded text one outermost embed reads at most (2 MB).
pub const MAX_EMBED_BYTES: usize = 2 << 20;

/// The space before a callout's title for its icon: the icon (15) and the
/// gap after it (7), in px at zoom 1.
pub const CALLOUT_TITLE_GUTTER: f32 = 22.0;

/// Synthetic block ids have this bit set (the parser's ids never do).
const SYNTHETIC: u64 = 1 << 63;

/// What one outermost embed has used of the budgets.
#[derive(Default)]
pub(crate) struct Budget {
    nodes: usize,
    bytes: usize,
    /// The node budget ran out (its notice is shown).
    full: bool,
    /// Notes and sections embedded around the current one, outermost
    /// first: the key, the title and the anchor.
    chain: Vec<(Arc<str>, Arc<str>, Option<String>)>,
}

/// What a builder of an embedded note's blocks builds.
pub(crate) struct EmbedCx {
    /// The embedded note's key: its links and images resolve from it.
    pub key: Arc<str>,
    /// Where the part starts in the note.
    pub at: usize,
    /// Note embeds around this one (0: the outermost).
    pub depth: usize,
    /// The embedded part of the note.
    pub range: Range<usize>,
}

impl<S: TextSource + ?Sized> NodeBuilder<'_, S> {
    /// A callout starts: without a title of its own it gets the type's
    /// name as its title row, made here.
    pub(super) fn callout_untitled(&mut self, c: &Container, fold: CalloutFold) {
        let Some(marker) = c.marker.clone() else {
            return;
        };
        if self
            .embed
            .as_ref()
            .is_some_and(|e| marker.start < e.range.start)
        {
            // Around a note embed's part: not shown.
            return;
        }
        // A title is the rest of the marker's line.
        let mut line = Vec::new();
        let rest = self.text.line(marker.end, &mut line);
        if rest.iter().any(|b| !b.is_ascii_whitespace()) {
            return;
        }
        let name = match self.frames.last().map(|f| &f.kind) {
            Some(FrameKind::Callout { name, .. }) => Arc::clone(name),
            _ => return,
        };
        self.title_cx = self.callout_title_cx();
        let cx = self.inline_cx(c.id);
        let title = Arc::new(inline::build_plain(
            &default_callout_title(&name),
            marker.clone(),
            marker.start,
            TextRole::Body,
            &cx,
        ));
        self.title_cx = None;
        let context = self.context();
        self.push(
            BlockNode {
                id: c.id,
                kind: NodeKind::CalloutTitle { title, fold },
                source: marker.clone(),
                context,
                block: marker.clone(),
                top: marker.clone(),
                lookups: None,
                table: false,
            },
            None,
        );
        self.callout_body(c.id);
    }

    /// The colour and icon gutter of the innermost callout's title.
    pub(super) fn callout_title_cx(&self) -> Option<(crate::style::ColorToken, f32)> {
        let name = self.frames.iter().rev().find_map(|f| match &f.kind {
            FrameKind::Callout { name, .. } => Some(name),
            _ => None,
        })?;
        let (_, color) = CalloutType::of(name).colors();
        Some((color, CALLOUT_TITLE_GUTTER * self.opts.styles.zoom()))
    }

    /// After a foldable callout's title (node `title`): its other blocks
    /// fold with it.
    pub(super) fn callout_body(&mut self, title: BlockId) {
        let Some(Open {
            kind: ContainerKind::Callout { fold },
            body_frame,
            ..
        }) = self.open.last_mut()
        else {
            return;
        };
        if !fold.is_foldable() || *body_frame || self.frames.len() >= super::MAX_FRAMES {
            return;
        }
        *body_frame = true;
        self.frames.push(Frame {
            id: title,
            kind: FrameKind::Details,
        });
        self.ctx = None;
    }

    /// A paragraph that is only a note embed: its line, then the embedded
    /// blocks in an embed frame. Returns false for other paragraphs.
    pub(super) fn embed_paragraph(&mut self, leaf: &Leaf<'_>) -> bool {
        let Some(parse_opts) = self.parse_opts else {
            return false;
        };
        let Some((inner, anchor)) = self.only_note_embed(leaf) else {
            return false;
        };
        if let Some(kind) = LiveKind::of_embed(split_embed(&inner).0) {
            if self.embed_too_deep() {
                let n = self.embed_notice(leaf, &too_deep());
                self.push(n, None);
            } else {
                self.live_embed(leaf, kind, &inner);
            }
            return true;
        }
        let outermost = self.embed.is_none();
        let depth = self.embed.as_ref().map_or(0, |e| e.depth + 1);
        let frame = self.frames.len() < super::MAX_FRAMES;
        if frame {
            self.frames.push(Frame {
                id: leaf.id,
                kind: FrameKind::Embed {
                    depth: u16::try_from(depth).unwrap_or(u16::MAX),
                },
            });
            self.ctx = None;
        }
        // The embed's own line: a link to the note. The embedded blocks map
        // to its end.
        let mut at = leaf.range.end;
        if let Some(n) = self.build_leaf(leaf) {
            at = n.source.end;
            self.push(n, None);
        }
        if outermost {
            self.budget = Budget::default();
            if let Some((key, title)) = self.sources.links.document_key() {
                self.budget.chain.push((key, title, None));
            }
        }
        let from = self.embed.as_ref().map(|e| Arc::clone(&e.key));
        match self.sources.links.embed(&inner, from.as_deref()) {
            EmbedFetch::Note { key, title, text } => {
                self.transclude(
                    &text,
                    anchor.as_deref(),
                    &key,
                    &title,
                    at,
                    depth,
                    parse_opts,
                );
            }
            EmbedFetch::Unavailable if !outermost => {
                self.notice(&format!("\u{201c}{inner}\u{201d} cannot be shown."), at);
            }
            EmbedFetch::Unavailable | EmbedFetch::Pending => {}
        }
        if frame {
            self.frames.pop();
            self.ctx = None;
        }
        true
    }

    /// A paragraph that is only a canvas or base embed: a live block,
    /// its own line kept as its label (copy, find, exports).
    fn live_embed(&mut self, leaf: &Leaf<'_>, kind: LiveKind, inner: &str) {
        let Some(mut node) = self.build_leaf(leaf) else {
            return;
        };
        let label = match &node.kind {
            NodeKind::Paragraph(c) => Some(Arc::clone(c)),
            _ => None,
        };
        let (path, anchor) = split_embed(inner);
        let (host, depth) = self.live_host();
        node.kind = NodeKind::Live(Arc::new(LiveBlock::new(
            kind,
            Some(path.into()),
            anchor.map(Into::into),
            None,
            label,
            host,
            depth,
        )));
        self.push(node, None);
    }

    /// Inside a note embed: whether a live block or another embed would be
    /// nested too deep.
    pub(super) fn embed_too_deep(&self) -> bool {
        self.embed
            .as_ref()
            .is_some_and(|e| e.depth + 1 >= MAX_EMBED_DEPTH)
    }

    /// The note a live block is in (`None`: the document) and the number
    /// of note embeds around it.
    pub(super) fn live_host(&self) -> (Option<Arc<str>>, u16) {
        match &self.embed {
            Some(e) => (
                Some(Arc::clone(&e.key)),
                u16::try_from(e.depth + 1).unwrap_or(u16::MAX),
            ),
            None => (None, 0),
        }
    }

    /// Whether `leaf` is built: inside a note embed only the leaves of the
    /// embedded part (and the footnote definitions it refers to).
    pub(super) fn embed_wants(&self, leaf: &Leaf<'_>) -> bool {
        match &self.embed {
            None => true,
            Some(e) => {
                let s = leaf.range.start;
                s == e.range.start || e.range.contains(&s) || !self.diverting.is_empty()
            }
        }
    }

    /// A notice in place of `leaf` (inside a note embed).
    pub(super) fn embed_notice(&mut self, leaf: &Leaf<'_>, text: &str) -> BlockNode {
        let at = leaf.range.start;
        let cx = self.inline_cx(leaf.id);
        let content = Arc::new(inline::build_plain(text, at..at, at, TextRole::Body, &cx));
        BlockNode {
            id: leaf.id,
            kind: NodeKind::Paragraph(content),
            source: at..at,
            context: self.context(),
            block: at..at,
            top: at..at,
            lookups: None,
            table: false,
        }
    }

    /// The embed of a paragraph that holds only a note embed (not an
    /// image): what is between its brackets, and its anchor.
    fn only_note_embed(&mut self, leaf: &Leaf<'_>) -> Option<(String, Option<String>)> {
        if !self.inline_opts.embeds {
            return None;
        }
        let [line] = leaf.lines else {
            return None;
        };
        let mut buf = Vec::new();
        self.text.copy_range(line.content.clone(), &mut buf);
        let s = buf.trim_ascii();
        if !(s.starts_with(b"![[") && s.ends_with(b"]]")) {
            return None;
        }
        let segments = [line.content.clone()];
        let tree = parse_inlines_with(
            self.text,
            InlineInput {
                segments: &segments,
            },
            &*self.refs,
            InlineOptions {
                block_ids: false,
                ..self.inline_opts
            },
        );
        let mut it = tree
            .top_level()
            .filter(|n| n.kind() != InlineKind::Whitespace);
        let node = it.next()?;
        if it.next().is_some() {
            return None;
        }
        let w = node.wikilink().filter(|w| w.embed)?;
        if is_image_path(w.path().trim()) {
            return None;
        }
        Some((w.inner.to_string(), w.anchor().map(str::to_owned)))
    }

    /// Pushes the blocks of the embedded `text` (the part `anchor` names)
    /// at `at`, the embed's depth being `depth`: built like the
    /// document's blocks (by a builder of their own over the embedded
    /// note), then mapped to `at` with ids of their own, inside the frames
    /// open here. The footnotes they refer to follow, numbered from 1.
    #[allow(clippy::too_many_arguments)]
    fn transclude(
        &mut self,
        text: &str,
        anchor: Option<&str>,
        key: &Arc<str>,
        title: &Arc<str>,
        at: usize,
        depth: usize,
        parse_opts: gv_syntax::ParseOptions,
    ) {
        if self.budget.full {
            return;
        }
        if let Some(i) = self
            .budget
            .chain
            .iter()
            .position(|(k, _, a)| k == key && a.as_deref() == anchor)
        {
            let mut names: Vec<&str> = self.budget.chain[i..]
                .iter()
                .map(|(_, t, _)| &**t)
                .collect();
            names.push(title);
            let notice = format!("Embed cycle: {}", names.join(" \u{2192} "));
            self.notice(&notice, at);
            return;
        }
        if depth >= MAX_EMBED_DEPTH {
            self.notice(&too_deep(), at);
            return;
        }
        let tree = SyntaxTree::parse_with(text, parse_opts);
        let model = DocModel::build(&tree, text);
        let Some(range) = embed_range(&tree, &model, text, anchor) else {
            let what = anchor.unwrap_or("");
            self.notice(&format!("\u{201c}{what}\u{201d} is not in {title}."), at);
            return;
        };
        if self.budget.bytes + range.len() > MAX_EMBED_BYTES {
            self.notice("This embed is too large to show here.", at);
            return;
        }
        self.budget.bytes += range.len();
        self.budget.chain.push((
            Arc::clone(key),
            Arc::clone(title),
            anchor.map(str::to_owned),
        ));
        let refs = tree.ref_map(text);
        let mut sub = NodeBuilder::new(
            text,
            Arc::clone(&refs),
            self.opts,
            self.inline_opts,
            self.sources,
            None,
        );
        sub.parse_opts = Some(parse_opts);
        sub.footnotes = FootnoteIndex::scan_range(&tree, text, &refs, range.clone()).map(Arc::new);
        sub.embed = Some(EmbedCx {
            key: Arc::clone(key),
            at: range.start,
            depth,
            range: range.clone(),
        });
        sub.budget = std::mem::take(&mut self.budget);
        tree.walk_range(range.clone(), &mut sub);
        if let Some(index) = sub.footnotes.clone() {
            // Definitions outside the part that it refers to.
            tree.walk(&mut OutsideDefinitions {
                range: range.clone(),
                text,
                index: &index,
                depth: 0,
                inner: &mut sub,
            });
        }
        self.budget = std::mem::take(&mut sub.budget);
        let nodes = std::mem::take(&mut sub.nodes);
        let sections = std::mem::take(&mut sub.sections);
        drop(sub);
        if self.splice(nodes, at) {
            self.embed_footnotes(sections, at);
        }
        self.budget.chain.pop();
    }

    /// The footnote definitions of an embed (`sections`, numbered in the
    /// embed) after its blocks, as a numbered list below a rule.
    fn embed_footnotes(&mut self, sections: Vec<super::Section>, at: usize) {
        let mut defs: Vec<super::Section> = sections
            .into_iter()
            .filter(|s| s.number.is_some())
            .collect();
        defs.sort_by_key(|s| s.number);
        defs.dedup_by_key(|s| s.number);
        if defs.is_empty() {
            return;
        }
        self.synthetic += 1;
        let context = self.context();
        self.push(
            BlockNode {
                id: BlockId(SYNTHETIC | self.synthetic),
                kind: NodeKind::ThematicBreak,
                source: at..at,
                context,
                block: at..at,
                top: at..at,
                lookups: None,
                table: false,
            },
            None,
        );
        // One list (its frame's id is mapped like the others, once).
        let list = Frame {
            id: BlockId(SYNTHETIC),
            kind: FrameKind::List {
                ordered: true,
                tight: true,
                marker: b'.',
                start: 1,
                depth: 0,
            },
        };
        let mut nodes = Vec::new();
        for def in defs {
            let item = Frame {
                id: def.id,
                kind: FrameKind::Item {
                    marker: ListMarker::Ordered {
                        number: u64::from(def.number.unwrap_or(0)),
                        delimiter: b'.',
                    },
                },
            };
            for mut n in def.nodes {
                let mut frames = vec![list.clone(), item.clone()];
                frames.extend(n.context.iter().skip(1).cloned());
                n.context = frames.into();
                nodes.push(n);
            }
        }
        self.splice(nodes, at);
    }

    /// Pushes the nodes of an embedded note (built by another builder)
    /// inside the frames open here, every source mapped to `at` and every
    /// block id replaced by a synthetic one. False when the node budget
    /// ran out.
    fn splice(&mut self, nodes: Vec<BlockNode>, at: usize) -> bool {
        let mut ids: HashMap<BlockId, BlockId> = HashMap::new();
        let mut contexts: HashMap<usize, Context> = HashMap::new();
        let outer = self.frames.clone();
        let mut synthetic = self.synthetic;
        let mut map = |id: BlockId| {
            *ids.entry(id).or_insert_with(|| {
                synthetic += 1;
                BlockId(SYNTHETIC | synthetic)
            })
        };
        let mut ok = true;
        for n in nodes {
            // Nodes of the embedded note itself count (deeper embeds
            // counted theirs).
            if n.id.0 & SYNTHETIC == 0 {
                self.budget.nodes += 1;
                if self.budget.nodes > MAX_EMBED_NODES {
                    ok = false;
                    break;
                }
            }
            let ptr = Arc::as_ptr(&n.context).cast::<Frame>() as usize;
            let context = match contexts.get(&ptr) {
                Some(c) => Arc::clone(c),
                None => {
                    let room = super::MAX_FRAMES.saturating_sub(outer.len());
                    let mut frames = outer.clone();
                    frames.extend(n.context.iter().take(room).map(|f| Frame {
                        id: map(f.id),
                        kind: f.kind.clone(),
                    }));
                    let c: Context = frames.into();
                    contexts.insert(ptr, Arc::clone(&c));
                    c
                }
            };
            let node = BlockNode {
                id: map(n.id),
                kind: collapse(n.kind),
                source: at..at,
                context,
                block: at..at,
                top: at..at,
                lookups: None,
                table: n.table,
            };
            self.push(node, None);
        }
        self.synthetic = synthetic;
        if !ok && !self.budget.full {
            self.budget.full = true;
            self.notice("This embed has too many blocks to show them all.", at);
        }
        ok
    }

    /// A notice in an embed (a cycle, a budget, a missing section).
    fn notice(&mut self, text: &str, at: usize) {
        self.synthetic += 1;
        let id = BlockId(SYNTHETIC | self.synthetic);
        let cx = self.inline_cx(id);
        let content = Arc::new(inline::build_plain(text, at..at, at, TextRole::Body, &cx));
        let context = self.context();
        self.push(
            BlockNode {
                id,
                kind: NodeKind::Paragraph(content),
                source: at..at,
                context,
                block: at..at,
                top: at..at,
                lookups: None,
                table: false,
            },
            None,
        );
    }
}

/// The notice of embeds nested too deep.
pub(super) fn too_deep() -> String {
    format!("Embeds nested more than {MAX_EMBED_DEPTH} deep are not shown.")
}

/// A node of an embedded note as the document shows it: headings are
/// paragraphs in a heading's type (no anchors of the document), and the
/// lines of code, math, diagrams, HTML and table rows map to the node's
/// start.
fn collapse(kind: NodeKind) -> NodeKind {
    match kind {
        NodeKind::Heading { content, .. } => NodeKind::Paragraph(content),
        NodeKind::Code(c) => NodeKind::Code(collapse_code(c)),
        NodeKind::Math(c) => NodeKind::Math(collapse_code(c)),
        NodeKind::Diagram(c) => NodeKind::Diagram(collapse_code(c)),
        NodeKind::Html {
            kind,
            raw_html_pending,
            code,
        } => NodeKind::Html {
            kind,
            raw_html_pending,
            code: collapse_code(code),
        },
        NodeKind::Table(t) => {
            let mut t = (*t).clone();
            for r in t.rows.iter_mut() {
                r.source = 0..0;
            }
            NodeKind::Table(Arc::new(t))
        }
        NodeKind::Live(l) if l.code.is_some() => {
            let mut l = (*l).clone();
            l.code = l.code.map(collapse_code);
            NodeKind::Live(Arc::new(l))
        }
        other => other,
    }
}

fn collapse_code(code: Arc<CodeBlock>) -> Arc<CodeBlock> {
    if code
        .lines
        .iter()
        .all(|l| l.source == (0..0) && l.start == 0)
    {
        return code;
    }
    let mut c = (*code).clone();
    for l in c.lines.iter_mut() {
        l.source = 0..0;
        l.start = 0;
    }
    Arc::new(c)
}

/// Feeds a builder the footnote definitions outside an embed's part that
/// the part refers to.
struct OutsideDefinitions<'b, 'a, 't> {
    range: Range<usize>,
    text: &'t str,
    index: &'b FootnoteIndex,
    /// Containers open inside the definition being fed.
    depth: usize,
    inner: &'b mut NodeBuilder<'a, str>,
}

impl Visitor for OutsideDefinitions<'_, '_, '_> {
    fn enter(&mut self, c: &Container) {
        if self.depth > 0 {
            self.depth += 1;
            self.inner.enter(c);
            return;
        }
        if c.kind != ContainerKind::FootnoteDefinition
            || (c.range.start < self.range.end && self.range.start < c.range.end)
        {
            return;
        }
        let label = c
            .footnote_label()
            .and_then(|l| self.text.get(l))
            .map(gv_syntax::inline::normalize_label);
        if label.is_some_and(|l| self.index.number(&l).is_some()) {
            self.depth = 1;
            self.inner.enter(c);
        }
    }

    fn exit(&mut self, c: &Container) {
        if self.depth > 0 {
            self.depth -= 1;
            self.inner.exit(c);
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if self.depth > 0 {
            self.inner.leaf(leaf);
        }
    }
}

/// The part of an embedded note `anchor` names: the whole note without
/// its front matter, a heading's section (up to the next heading of the
/// same or a higher level), or a block (a list item with its children;
/// the block before a block id on a line of its own). `None` when the
/// anchor names nothing.
fn embed_range(
    tree: &SyntaxTree,
    model: &DocModel,
    text: &str,
    anchor: Option<&str>,
) -> Option<Range<usize>> {
    let Some(anchor) = anchor.map(str::trim).filter(|a| !a.is_empty()) else {
        let start = model.front_matter().map_or(0, |f| f.range.end);
        return Some(start..text.len());
    };
    let parts: Vec<&str> = anchor
        .split('#')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if let Some(id) = parts.last().and_then(|p| p.strip_prefix('^')) {
        let BlockMatch::Found { index, .. } = model.match_block(id) else {
            return None;
        };
        let a = model.block_anchors().get(index)?;
        return Some(block_range(tree, text, a.range.clone(), a.own_line));
    }
    match model.match_fragment(&parts, Anchoring::HeadingText) {
        FragmentMatch::Heading { index, .. } => model.section(index),
        FragmentMatch::Top => Some(0..text.len()),
        _ => None,
    }
}

/// The block a block id names: the list item whose first paragraph ends
/// with it, else the paragraph; for an id alone, the block before it.
fn block_range(tree: &SyntaxTree, text: &str, para: Range<usize>, own_line: bool) -> Range<usize> {
    struct Find {
        para: Range<usize>,
        own_line: bool,
        /// Blocks seen, as (range, is an item).
        blocks: Vec<(Range<usize>, bool)>,
        open: Vec<(usize, bool)>,
    }
    impl Visitor for Find {
        fn enter(&mut self, c: &Container) {
            self.open
                .push((c.range.start, c.kind == ContainerKind::Item));
        }
        fn exit(&mut self, c: &Container) {
            self.open.pop();
            self.blocks
                .push((c.range.clone(), c.kind == ContainerKind::Item));
        }
        fn leaf(&mut self, l: &Leaf<'_>) {
            self.blocks.push((l.range.clone(), false));
        }
    }
    let mut f = Find {
        para: para.clone(),
        own_line,
        blocks: Vec::new(),
        open: Vec::new(),
    };
    tree.walk(&mut f);
    if f.own_line {
        // The outermost block that ends last before the id's paragraph.
        return f
            .blocks
            .iter()
            .filter(|(r, _)| r.end <= f.para.start && *r != f.para)
            .max_by_key(|(r, _)| (r.end, std::cmp::Reverse(r.start)))
            .map_or(para, |(r, _)| r.clone());
    }
    // The innermost item that starts on the paragraph's line.
    f.blocks
        .iter()
        .filter(|(r, item)| *item && r.start <= f.para.start && f.para.end <= r.end)
        .filter(|(r, _)| line_of(text, r.start) == line_of(text, f.para.start))
        .min_by_key(|(r, _)| r.len())
        .map_or(para, |(r, _)| r.clone())
}

fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

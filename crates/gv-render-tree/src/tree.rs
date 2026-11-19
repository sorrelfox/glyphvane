// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`RenderTree`]: render nodes and virtual items of one document
//! revision, built in full or updated from the previous revision.
//!
//! # Incremental updates
//!
//! [`RenderTree::update`] rebuilds only the **top-level blocks** that the
//! reparse window touches, in the new tree and in the old one (a list's
//! numbering, tightness and markers depend on all of its items, so a
//! top-level block is the unit). Inside them, leaves outside the window
//! keep their content (paragraphs are not rebuilt); everything after the
//! region only moves by the length difference. Blocks elsewhere whose
//! reference lookups now resolve differently are rebuilt in place. The
//! result equals a full build (property-tested).
//!
//! # Refreshes
//!
//! Link statuses and image sizes come from injected sources whose answers
//! change on their own (the index finished, an image header arrived).
//! [`RenderTree::refresh`] re-asks them for every link and image;
//! [`RenderTree::refresh_where`] only for those a predicate selects (the
//! caller knows which answers can have changed: only images, only links to
//! local paths, only destinations under changed folders). Blocks with a
//! changed answer are rebuilt whole.
//!
//! # Code wrap
//!
//! Whether a code block soft-wraps is [`Metrics::wrap_code`], unless the
//! block has its own setting ([`RenderTree::set_code_wrap`], the code
//! head's wrap toggle). Settings are kept by block id across updates,
//! refreshes and metric changes.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gv_math::{MacroLimits, MacroPass, MacroSet};
use gv_model::{DocModel, SlugAlgorithm};
use gv_profile::switches::{Math, SoftBreak};
use gv_profile::{CompiledProfile, ProfileId};
use gv_syntax::inline::RefLookup;
use gv_syntax::{BlockId, Changes, Container, Leaf, LeafKind, RefMap, SyntaxTree, Visitor};
use gv_text::TextSource;

use crate::build::{NodeBuilder, Reuse, Section};
use crate::footnote::FootnoteIndex;
use crate::item::{Flatten, Item, ItemId, ItemKind, link};
use crate::metrics::Metrics;
use crate::node::{BlockNode, Context, NodeKind};
use crate::status::{LinkTarget, Sources};
use crate::style::StyleTable;

/// How front matter is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FrontMatterDisplay {
    /// Not rendered.
    Hidden,
    /// A properties block (a placeholder showing the raw lines until the
    /// model's typed properties exist).
    #[default]
    Properties,
}

/// Everything a build depends on besides the document and the sources.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOptions {
    /// Heading anchors (the profile's primary slug algorithm).
    pub slugs: SlugAlgorithm,
    pub front_matter: FrontMatterDisplay,
    pub styles: StyleTable,
    pub metrics: Metrics,
    /// Lines per code chunk (50; see the virtual list spike).
    pub chunk_lines: usize,
    /// ` ```math ` fences are display math (GFM, Extended), not code.
    pub math_fences: bool,
    /// ` ```mermaid ` fences are diagrams (the profile's `mermaid` switch),
    /// not code.
    pub mermaid: bool,
    /// ` ```base ` and ` ```query ` fences are live blocks
    /// ([`LiveBlock`](crate::LiveBlock)), not code: Obsidian's bases and
    /// search embeds (the profiles with note embeds).
    pub obsidian_fences: bool,
    /// A soft line break is a line break (the profile's `soft_break =
    /// br`: Obsidian unless `strictLineBreaks`, GitHub comments), not a
    /// space.
    pub soft_breaks: bool,
    /// A Markdown image's alt text may end with its size, as Obsidian
    /// reads it (`![alt|300](x.png)`, `![alt|300x200](x.png)`); the
    /// profiles with note embeds.
    pub alt_sizes: bool,
    /// `:shortcode:` emoji are drawn as the emoji (the profile's `emoji`
    /// switch).
    pub emoji: bool,
    /// White space in text collapses as a browser collapses it (CSS
    /// `white-space: normal`): a run of spaces, tabs and line endings is
    /// one space, and none follows another space or a line break. Code
    /// spans keep theirs. The Obsidian profile, whose Reading view is HTML
    /// in a browser.
    pub collapse_spaces: bool,
}

/// The macros in force for every formula of a tree, from one pass over
/// the formulas in document order (see [`RenderTree::math_macros`]).
#[derive(Debug, Default)]
struct MacroTable {
    /// Per node, the index of its first formula in `sets` (and one more
    /// entry: the total).
    starts: Vec<u32>,
    sets: Vec<MacroSet>,
}

impl RenderOptions {
    /// The CommonMark profile's options: GitHub anchors, the Preview type,
    /// 50-line chunks.
    pub fn commonmark() -> Self {
        Self {
            slugs: SlugAlgorithm::GitHub,
            front_matter: FrontMatterDisplay::Properties,
            styles: StyleTable::preview(),
            metrics: Metrics::default(),
            chunk_lines: 50,
            math_fences: false,
            mermaid: false,
            obsidian_fences: false,
            soft_breaks: false,
            alt_sizes: false,
            emoji: false,
            collapse_spaces: false,
        }
    }

    /// The options of a compiled profile: the CommonMark options with the
    /// profile's math syntax (` ```math ` fences with GitHub math) and its
    /// Mermaid switch.
    pub fn for_profile(profile: &CompiledProfile) -> Self {
        Self {
            math_fences: profile.switches.math == Math::GitHub,
            mermaid: profile.switches.mermaid,
            obsidian_fences: profile.switches.embeds,
            soft_breaks: profile.switches.soft_break == SoftBreak::Br,
            alt_sizes: profile.switches.embeds,
            emoji: profile.switches.emoji,
            collapse_spaces: profile.id == ProfileId::Obsidian,
            ..Self::commonmark()
        }
    }

    /// The options of built-in profile `id`, without tuning or overrides.
    pub fn builtin(id: ProfileId) -> Self {
        let switches = &gv_profile::definition(id).switches;
        Self {
            math_fences: switches.math == Math::GitHub,
            mermaid: switches.mermaid,
            obsidian_fences: switches.embeds,
            soft_breaks: switches.soft_break == SoftBreak::Br,
            alt_sizes: switches.embeds,
            emoji: switches.emoji,
            collapse_spaces: id == ProfileId::Obsidian,
            ..Self::commonmark()
        }
    }

    /// These options with the profile's settings of `profile` (its math
    /// syntax and Mermaid switch), keeping the type scale and metrics.
    #[must_use]
    pub fn with_profile(self, profile: &CompiledProfile) -> Self {
        let p = Self::for_profile(profile);
        Self {
            math_fences: p.math_fences,
            mermaid: p.mermaid,
            obsidian_fences: p.obsidian_fences,
            soft_breaks: p.soft_breaks,
            alt_sizes: p.alt_sizes,
            emoji: p.emoji,
            collapse_spaces: p.collapse_spaces,
            ..self
        }
    }

    /// The same options at view zoom `zoom` (type and metrics).
    #[must_use]
    pub fn zoomed(mut self, zoom: f32) -> Self {
        self.styles = self.styles.with_zoom(zoom);
        self.metrics = self.metrics.zoomed(zoom);
        self
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::commonmark()
    }
}

/// What an update or refresh did, for the UI's height index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateReport {
    /// Item indices of the previous tree that were replaced...
    pub removed: Range<usize>,
    /// ...by these item indices of the new tree (splice the height index
    /// with them; items after them only moved).
    pub inserted: Range<usize>,
    /// Items whose content or width changed: their layouts and measured
    /// heights are stale. Other items keep theirs (their gap may change).
    pub changed: Vec<ItemId>,
    /// Nodes built from the syntax tree.
    pub nodes_built: usize,
    /// Nodes taken over from the previous tree.
    pub nodes_reused: usize,
    /// The whole tree was rebuilt.
    pub full: bool,
}

/// The render tree of one document revision: nodes per rendered block,
/// and the flattened virtual items.
#[derive(Debug, Clone)]
pub struct RenderTree {
    opts: RenderOptions,
    len: usize,
    nodes: Vec<BlockNode>,
    items: Vec<Item>,
    refs: Option<Arc<RefMap>>,
    index: OnceLock<HashMap<ItemId, usize>>,
    /// The document's math macros, from the first query on.
    macros: OnceLock<Arc<MacroTable>>,
    /// Per-block soft wrap of code blocks.
    code_wrap: HashMap<BlockId, bool>,
    /// The footnote numbers, when the document defines footnotes.
    footnotes: Option<Arc<FootnoteIndex>>,
    /// The first item of the footnote section (the items' length without
    /// one). Section items render the footnote definitions, in the order of
    /// their numbers: their sources are not in document order.
    section: usize,
    /// The first node of the footnote section.
    section_node: usize,
}

/// The block id of the rule above the footnote section.
pub const FOOTNOTE_RULE: BlockId = BlockId(u64::MAX);

/// Inline footnote `n`'s definition in the footnote section is block
/// `INLINE_NOTE_IDS - n` (below the ids of the backreference lines, which
/// count down from `u64::MAX - 1`; above the synthetic ids of embeds).
const INLINE_NOTE_IDS: u64 = u64::MAX - (1 << 33);

impl RenderTree {
    /// Builds the render tree of `tree` (the parse of `text`), with
    /// `model` its document model.
    pub fn build<S: TextSource + ?Sized>(
        tree: &SyntaxTree,
        model: &DocModel,
        text: &S,
        opts: RenderOptions,
        sources: Sources<'_>,
    ) -> RenderTree {
        Self::build_with_wrap(tree, model, text, opts, sources, HashMap::new())
    }

    fn build_with_wrap<S: TextSource + ?Sized>(
        tree: &SyntaxTree,
        model: &DocModel,
        text: &S,
        opts: RenderOptions,
        sources: Sources<'_>,
        code_wrap: HashMap<BlockId, bool>,
    ) -> RenderTree {
        let refs = tree.ref_map(text);
        let footnotes = FootnoteIndex::scan(tree, text, &refs).map(Arc::new);
        let mut t = RenderTree {
            opts,
            len: tree.len(),
            nodes: Vec::new(),
            items: Vec::new(),
            refs: Some(Arc::clone(&refs)),
            index: OnceLock::new(),
            macros: OnceLock::new(),
            code_wrap,
            footnotes: footnotes.clone(),
            section: 0,
            section_node: 0,
        };
        let inline_opts = tree.options().inline();
        let mut b = NodeBuilder::new(text, refs, &t.opts, inline_opts, sources, None);
        b.parse_opts = Some(tree.options());
        b.footnotes = footnotes;
        gv_syntax::walk(tree, &mut b);
        let nodes = std::mem::take(&mut b.nodes);
        let sections = std::mem::take(&mut b.sections);
        drop(b);
        t.nodes = nodes;
        let main = t.nodes.len();
        t.section_node = main;
        t.footnote_section(tree, text, sources, sections);
        t.assign_anchors(model);
        t.items = t.flatten(0..main);
        t.section = t.items.len();
        let section = t.flatten(main..t.nodes.len());
        t.items.extend(section);
        let n = t.items.len();
        link(&mut t.items, 0..n, &t.opts.metrics);
        t
    }

    /// Appends the footnote section: a rule, then the referenced footnote
    /// definitions by number (the first definition of a label), each with
    /// its backreferences after its last paragraph (or on a line of their
    /// own when it does not end with one).
    fn footnote_section<S: TextSource + ?Sized>(
        &mut self,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
        sections: Vec<Section>,
    ) {
        let Some(index) = self.footnotes.clone() else {
            return;
        };
        let mut defs: Vec<Section> = sections
            .into_iter()
            .filter(|s| s.number.is_some())
            .collect();
        let refs = self.refs.clone().unwrap_or_else(|| tree.ref_map(text));
        defs.extend(self.inline_notes(&index, tree, text, sources, &refs));
        defs.sort_by_key(|s| s.number);
        defs.dedup_by_key(|s| s.number);
        if defs.is_empty() {
            return;
        }
        let end = self.len;
        self.nodes.push(BlockNode {
            id: FOOTNOTE_RULE,
            kind: NodeKind::ThematicBreak,
            source: end..end,
            context: Arc::from([]),
            block: end..end,
            top: end..end,
            lookups: None,
            table: false,
        });
        for mut def in defs {
            if def.label.starts_with('\u{1}') {
                // An inline footnote: built whole, backreference included.
                self.nodes.extend(def.nodes);
                continue;
            }
            let count = index.references(&def.label);
            let last = def.nodes.last();
            let ends_with_paragraph = last
                .is_some_and(|n| matches!(n.kind, NodeKind::Paragraph(_)) && n.context.len() == 1);
            if ends_with_paragraph && let Some(last) = def.nodes.last() {
                // Built again with its backreferences.
                let mut finder = LeafFinder {
                    id: last.id,
                    table: false,
                    builder: NodeBuilder::new(
                        text,
                        Arc::clone(&refs),
                        &self.opts,
                        tree.options().inline(),
                        sources,
                        None,
                    ),
                    found: None,
                };
                finder.builder.footnotes = Some(Arc::clone(&index));
                finder.builder.divert = false;
                finder.builder.backrefs = Some((last.id, count));
                gv_syntax::walk_range(tree, last.block.clone(), &mut finder);
                if let (Some(mut node), Some(last)) = (finder.found, def.nodes.last_mut()) {
                    node.context = last.context.clone();
                    node.top = last.top.clone();
                    *last = node;
                }
            } else {
                // A line of backreferences after the definition.
                let at = def.range.end;
                let context = def.nodes.first().map_or_else(
                    || {
                        Context::from(vec![crate::node::Frame {
                            id: def.id,
                            kind: crate::node::FrameKind::Footnote {
                                number: def.number.unwrap_or(0),
                                label: Arc::clone(&def.label),
                            },
                        }])
                    },
                    |n| Context::from(&n.context[..1]),
                );
                let cx = crate::inline::InlineCx {
                    styles: &self.opts.styles,
                    metrics: &self.opts.metrics,
                    sources,
                    block: def.id,
                    footnotes: Some(&index),
                    soft_breaks: self.opts.soft_breaks,
                    alt_sizes: self.opts.alt_sizes,
                    emoji: self.opts.emoji,
                    collapse_spaces: self.opts.collapse_spaces,
                    title: None,
                    embed: None,
                };
                let content = crate::inline::build_backrefs_only(at, count, &cx);
                def.nodes.push(BlockNode {
                    id: BlockId(u64::MAX - 1 - u64::from(def.number.unwrap_or(0))),
                    kind: NodeKind::Paragraph(Arc::new(content)),
                    source: at..at,
                    context,
                    block: at..at,
                    top: def.range.clone(),
                    lookups: None,
                    table: false,
                });
            }
            self.nodes.extend(def.nodes);
        }
    }

    /// The definitions of the inline footnotes (`^[…]`) for the footnote
    /// section: one paragraph each, its text then its backreference.
    fn inline_notes<S: TextSource + ?Sized>(
        &self,
        index: &FootnoteIndex,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
        refs: &RefMap,
    ) -> Vec<Section> {
        index
            .inline
            .iter()
            .filter_map(|note| self.inline_note(note, index, tree, text, sources, refs))
            .collect()
    }

    /// The definition of inline footnote `note` for the footnote section.
    fn inline_note<S: TextSource + ?Sized>(
        &self,
        note: &crate::footnote::InlineNote,
        index: &FootnoteIndex,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
        refs: &RefMap,
    ) -> Option<Section> {
        let opts = tree.options().inline();
        let parsed = if note.cell {
            let cell = note.segments.first()?;
            gv_syntax::inline::parse_table_cell(
                text,
                cell.clone(),
                refs,
                gv_syntax::inline::InlineOptions {
                    block_ids: false,
                    ..opts
                },
            )
        } else {
            gv_syntax::inline::parse_inlines_with(
                text,
                gv_syntax::inline::InlineInput {
                    segments: &note.segments,
                },
                refs,
                opts,
            )
        };
        let id = BlockId(INLINE_NOTE_IDS - u64::from(note.number));
        let cx = crate::inline::InlineCx {
            styles: &self.opts.styles,
            metrics: &self.opts.metrics,
            sources,
            block: id,
            footnotes: Some(index),
            soft_breaks: self.opts.soft_breaks,
            alt_sizes: self.opts.alt_sizes,
            emoji: self.opts.emoji,
            collapse_spaces: self.opts.collapse_spaces,
            title: None,
            embed: None,
        };
        let base = note.source.start;
        let content = crate::inline::build_inline_note(&parsed, note.source.start, base, &cx)?;
        let label: Arc<str> = Arc::from(note.label.as_str());
        let context = Context::from(vec![crate::node::Frame {
            id,
            kind: crate::node::FrameKind::Footnote {
                number: note.number,
                label: Arc::clone(&label),
            },
        }]);
        Some(Section {
            label,
            number: Some(note.number),
            id,
            range: note.source.clone(),
            nodes: vec![BlockNode {
                id,
                kind: NodeKind::Paragraph(Arc::new(content)),
                source: note.source.clone(),
                context,
                block: note.block_range.clone(),
                top: note.source.clone(),
                lookups: None,
                table: false,
            }],
        })
    }

    /// The document's footnote numbers, if it defines footnotes.
    pub fn footnotes(&self) -> Option<&FootnoteIndex> {
        self.footnotes.as_deref()
    }

    /// The index of the first item of the footnote section (the number of
    /// items when there is none).
    pub fn footnote_section_start(&self) -> usize {
        self.section
    }

    /// The first item of footnote `label`'s definition in the footnote
    /// section.
    pub fn footnote_item(&self, label: &str) -> Option<usize> {
        (self.section..self.items.len()).find(|&ix| {
            self.items[ix].context.first().is_some_and(
                |f| matches!(&f.kind, crate::node::FrameKind::Footnote { label: l, .. } if &**l == label),
            )
        })
    }

    /// The references to footnote `label`, in document order: the item and
    /// the reference's document range.
    pub fn footnote_refs(&self, label: &str) -> Vec<(usize, Range<usize>)> {
        let mut out = Vec::new();
        for (ix, item) in self.items.iter().enumerate() {
            let Some(node) = self.nodes.get(item.node) else {
                continue;
            };
            let Some(c) = node.item_inline(&item.kind) else {
                continue;
            };
            for f in c
                .footnotes
                .iter()
                .filter(|f| !f.embedded && &*f.label == label)
            {
                out.push((
                    ix,
                    node.to_document(f.source.start)..node.to_document(f.source.end),
                ));
            }
        }
        out.sort_by_key(|(_, r)| r.start);
        out
    }

    /// Inside a note embed: the first item of the entry of footnote
    /// `number` in the list of footnotes at the end of the embed that item
    /// `ix` (a reference to it) is in.
    pub fn embed_footnote_item(&self, ix: usize, number: u32) -> Option<usize> {
        use crate::node::{FrameKind, ListMarker};
        let ctx = &self.items.get(ix)?.context;
        let p = ctx
            .iter()
            .rposition(|f| matches!(f.kind, FrameKind::Embed { .. }))?;
        let embed = ctx[p].id;
        self.items.iter().position(|it| {
            it.context.get(p).is_some_and(|f| f.id == embed)
                && it
                    .context
                    .get(p + 1)
                    .is_some_and(crate::build::is_embed_notes)
                && it.context.get(p + 2).is_some_and(|f| {
                    matches!(f.kind, FrameKind::Item {
                        marker: ListMarker::Ordered { number: n, .. },
                    } if n == u64::from(number))
                })
        })
    }

    /// Inside the list of footnotes at a note embed's end: the items of
    /// the references (in the embed) to the footnote whose entry item `ix`
    /// is in, in order.
    pub fn embed_footnote_refs(&self, ix: usize) -> Vec<usize> {
        use crate::node::{FrameKind, ListMarker};
        let Some(item) = self.items.get(ix) else {
            return Vec::new();
        };
        let ctx = &item.context;
        let Some(p) = (0..ctx.len().saturating_sub(2)).rev().find(|&p| {
            matches!(ctx[p].kind, FrameKind::Embed { .. })
                && crate::build::is_embed_notes(&ctx[p + 1])
        }) else {
            return Vec::new();
        };
        let FrameKind::Item {
            marker: ListMarker::Ordered { number, .. },
        } = ctx[p + 2].kind
        else {
            return Vec::new();
        };
        let embed = ctx[p].id;
        let mut out = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            let c = &it.context;
            let innermost = c
                .iter()
                .rposition(|f| matches!(f.kind, FrameKind::Embed { .. }));
            if innermost != Some(p) || c[p].id != embed {
                continue;
            }
            let Some(content) = self
                .nodes
                .get(it.node)
                .and_then(|n| n.item_inline(&it.kind))
            else {
                continue;
            };
            let n = content
                .footnotes
                .iter()
                .filter(|f| f.embedded && u64::from(f.number) == number)
                .count();
            out.extend(std::iter::repeat_n(i, n));
        }
        out
    }

    /// Updates the tree after a reparse: `tree` is the new syntax tree,
    /// `model` its model, `text` the new text and `changes` the reparse's
    /// report (consecutive reports composed with [`Changes::compose`] if
    /// revisions were skipped). An interim tree, or changes that do not fit,
    /// rebuild everything.
    pub fn update<S: TextSource + ?Sized>(
        &mut self,
        tree: &SyntaxTree,
        model: &DocModel,
        text: &S,
        changes: &Changes,
        sources: Sources<'_>,
    ) -> UpdateReport {
        let delta = tree.len() as isize - self.len as isize;
        let w = changes.new_range.clone();
        let ow = changes.old_range.clone();
        let fits = !tree.is_interim()
            && w.start <= w.end
            && w.end <= tree.len()
            && ow.start == w.start
            && ow.start <= ow.end
            && ow.end <= self.len
            && ow.end as isize + delta == w.end as isize;
        if !fits {
            return self.rebuild(tree, model, text, sources);
        }
        if changes.is_empty() && delta == 0 {
            self.assign_anchors(model);
            return UpdateReport {
                removed: 0..0,
                inserted: 0..0,
                ..UpdateReport::default()
            };
        }
        let refs = tree.ref_map(text);
        if self.footnotes.is_some() || refs.footnote_count() > 0 {
            // Footnote numbers and the footnote section are document-wide.
            return self.rebuild(tree, model, text, sources);
        }
        let same_refs = self.refs.as_ref().is_some_and(|r| Arc::ptr_eq(r, &refs));

        // The region to rebuild, in new coordinates: whole top-level blocks
        // of both trees around the window.
        let (mut s, mut e) = (w.start, w.end);
        let mut stable = false;
        for _ in 0..8 {
            let (ns, ne) = top_level_extent(tree, s..e);
            let old_end = (e as isize - delta).max(s as isize) as usize;
            let (os, oe) = self.old_top_extent(s..old_end);
            let map_start = |x: usize| {
                if x <= ow.start {
                    x
                } else if x >= ow.end {
                    x.saturating_add_signed(delta)
                } else {
                    w.start
                }
            };
            let map_end = |x: usize| {
                if x <= ow.start {
                    x
                } else if x >= ow.end {
                    x.saturating_add_signed(delta)
                } else {
                    w.end
                }
            };
            let s2 = s.min(ns).min(map_start(os));
            let e2 = e.max(ne).max(map_end(oe));
            if (s2, e2) == (s, e) {
                stable = true;
                break;
            }
            (s, e) = (s2, e2);
        }
        if !stable {
            return self.rebuild(tree, model, text, sources);
        }
        if tree.options().inline().inline_footnotes && holds_inline_note(text, s..e) {
            // The region may now hold an inline footnote: numbered
            // document-wide.
            return self.rebuild(tree, model, text, sources);
        }
        let old_end = (e as isize - delta).max(s as isize) as usize;

        // Old nodes and items of the region.
        let a = self.nodes.partition_point(|n| n.source.start < s);
        let b = self.nodes.partition_point(|n| n.source.start < old_end);
        let mut b = b.max(a);
        // The blocks of a note embed map to the end of its line: at the
        // region's end (the document's), they are the region's.
        while b > a && b < self.nodes.len() && self.nodes[b].source == (old_end..old_end) {
            b += 1;
        }
        let ia = self.items.partition_point(|it| it.node < a);
        let ib = self.items.partition_point(|it| it.node < b);

        let reuse = Reuse {
            by_id: self.nodes[a..b]
                .iter()
                .enumerate()
                .map(|(i, n)| ((n.id, n.table), (a + i, n)))
                .collect(),
            window: w.clone(),
            delta,
            same_refs,
        };
        let mut builder = NodeBuilder::new(
            text,
            Arc::clone(&refs),
            &self.opts,
            tree.options().inline(),
            sources,
            Some(reuse),
        );
        builder.parse_opts = Some(tree.options());
        walk_region(tree, s..e, &mut builder);
        let built = builder.built;
        let reused = builder.reused;
        let new_nodes = std::mem::take(&mut builder.nodes);
        let origin = std::mem::take(&mut builder.origin);
        drop(builder);

        // Items of the new nodes: a reused node at the same indentation
        // takes over its old items (moved); others are flattened afresh
        // and reported changed.
        let flat = self.flattener();
        let mut new_items = Vec::with_capacity(ib - ia);
        let mut changed = Vec::new();
        for (j, node) in new_nodes.iter().enumerate() {
            let ix = a + j;
            if let Some(k) = origin.get(j).copied().flatten() {
                let old = &self.nodes[k];
                if same_indent(old, node) {
                    let s0 = self.items.partition_point(|it| it.node < k);
                    let s1 = self.items.partition_point(|it| it.node <= k);
                    let d = node.source.start as isize - old.source.start as isize;
                    new_items.extend(self.items[s0..s1].iter().map(|it| Item {
                        node: ix,
                        source: shift(&it.source, d),
                        context: Arc::clone(&node.context),
                        ..it.clone()
                    }));
                    continue;
                }
            }
            let first = new_items.len();
            flat.node_items(node, ix, &mut new_items);
            changed.extend(new_items[first..].iter().map(|it| it.id));
        }

        // Splice nodes and items; what follows only moves.
        let added = new_nodes.len();
        self.nodes.splice(a..b, new_nodes);
        for n in &mut self.nodes[a + added..] {
            n.source = shift(&n.source, delta);
            n.block = shift(&n.block, delta);
            n.top = shift(&n.top, delta);
        }
        let inserted = new_items.len();
        self.items.splice(ia..ib, new_items);
        let node_delta = added as isize - (b - a) as isize;
        for it in &mut self.items[ia + inserted..] {
            it.source = shift(&it.source, delta);
            it.node = it.node.saturating_add_signed(node_delta);
        }
        link(&mut self.items, ia..ia + inserted, &self.opts.metrics);
        // No footnotes here (see above): no section.
        self.section = self.items.len();
        self.section_node = self.nodes.len();

        let mut report = UpdateReport {
            removed: ia..ib,
            inserted: ia..ia + inserted,
            changed,
            nodes_built: built,
            nodes_reused: reused,
            full: false,
        };
        self.len = tree.len();
        self.refs = Some(Arc::clone(&refs));
        if !same_refs {
            // Blocks outside the region whose references now resolve
            // differently.
            let stale: Vec<usize> = (0..self.nodes.len())
                .filter(|&k| k < a || k >= a + added)
                .filter(|&k| {
                    self.nodes[k].lookups.as_ref().is_some_and(|l| {
                        l.iter()
                            .any(|(label, def)| refs.lookup(label) != def.as_ref())
                    })
                })
                .collect();
            let mut embeds = std::collections::HashSet::new();
            for k in stale {
                self.rebuild_stale(k, tree, text, sources, &mut embeds, &mut report);
            }
        }
        self.assign_anchors(model);
        self.index = OnceLock::new();
        self.macros = OnceLock::new();
        report
    }

    /// Re-asks the sources for every link status and image size and
    /// rebuilds the blocks whose answers changed (the index finished, an
    /// image header arrived). Item ids and order stay; the report lists the
    /// changed items. Same as [`refresh_where`](Self::refresh_where) with a
    /// predicate that selects everything.
    pub fn refresh<S: TextSource + ?Sized>(
        &mut self,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
    ) -> UpdateReport {
        self.refresh_where(tree, text, sources, &|_| true)
    }

    /// Like [`refresh`](Self::refresh), but only the links and images
    /// `which` selects are re-asked ([`LinkTarget::image`] tells them
    /// apart); the others are assumed unchanged. A block with a changed
    /// answer is rebuilt whole, so its other links are re-asked too.
    ///
    /// A refresh costs one source query per selected link or image: when
    /// only some answers can have changed, select those. For example,
    /// after the path index changed, URLs keep their status
    /// (`|t| !t.image && is_local(t.destination)`); after image headers
    /// arrived, only images need asking (`|t| t.image`).
    pub fn refresh_where<S: TextSource + ?Sized>(
        &mut self,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
        which: &dyn Fn(&LinkTarget<'_>) -> bool,
    ) -> UpdateReport {
        let mut stale = Vec::new();
        for (k, n) in self.nodes.iter().enumerate() {
            if let NodeKind::Picture(p) = &n.kind {
                // The images a `<picture>` picks in a light and a dark scheme.
                let changed = [false, true].into_iter().any(|dark| {
                    p.pick(dark).is_some_and(|url| {
                        let target = LinkTarget {
                            kind: gv_model::LinkKind::Inline,
                            image: true,
                            destination: url,
                            title: None,
                            range: n.block.clone(),
                            block: n.id,
                            from: p.from.as_deref(),
                        };
                        which(&target) && sources.images.size(&target) != p.sizes[usize::from(dark)]
                    })
                });
                if changed {
                    stale.push(k);
                }
                continue;
            }
            for c in n.inlines() {
                let link_changed = || {
                    c.links.iter().any(|l| {
                        let target = LinkTarget {
                            kind: l.kind,
                            image: false,
                            destination: &l.destination,
                            title: l.title.as_deref(),
                            range: n.to_document(l.source.start)..n.to_document(l.source.end),
                            block: n.id,
                            from: l.from.as_deref(),
                        };
                        which(&target) && sources.links.status(&target) != l.status
                    })
                };
                let image_changed = || {
                    c.images.iter().any(|i| {
                        let target = LinkTarget {
                            kind: i.kind,
                            image: true,
                            destination: &i.destination,
                            title: i.title.as_deref(),
                            range: n.to_document(i.source.start)..n.to_document(i.source.end),
                            block: n.id,
                            from: i.from.as_deref(),
                        };
                        which(&target) && sources.images.size(&target) != i.size
                    })
                };
                if link_changed() || image_changed() {
                    stale.push(k);
                    break;
                }
            }
        }
        let mut report = UpdateReport::default();
        let mut embeds = std::collections::HashSet::new();
        for k in stale {
            self.rebuild_stale(k, tree, text, sources, &mut embeds, &mut report);
        }
        report
    }

    /// Builds again, in place, every outermost note embed holding a note
    /// embed (itself or a nested one) whose line's link `which` selects
    /// (its destination is what is between the brackets of `![[…]]`,
    /// `from` the embedding note inside an embed), for example the embeds
    /// of a note whose text changed: the embedded notes are asked again
    /// ([`LinkStatusSource::embed`]).
    /// `None` when a selected embed cannot be built in place (its number
    /// of nodes or items would change): build the document again.
    ///
    /// [`LinkStatusSource::embed`]: crate::LinkStatusSource::embed
    pub fn rebuild_embeds_where<S: TextSource + ?Sized>(
        &mut self,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
        which: &dyn Fn(&LinkTarget<'_>) -> bool,
    ) -> Option<UpdateReport> {
        let mut report = UpdateReport::default();
        let mut done = std::collections::HashSet::new();
        // An embed's own line (the first node of its frame) links to the
        // embedded note.
        let selected: Vec<usize> = (0..self.nodes.len())
            .filter(|&k| {
                let n = &self.nodes[k];
                let line = n.context.iter().any(|f| {
                    f.id == n.id && matches!(f.kind, crate::node::FrameKind::Embed { .. })
                });
                line && n.inline().and_then(|c| c.links.first()).is_some_and(|l| {
                    which(&LinkTarget {
                        kind: l.kind,
                        image: false,
                        destination: &l.destination,
                        title: l.title.as_deref(),
                        range: n.to_document(l.source.start)..n.to_document(l.source.end),
                        block: n.id,
                        from: l.from.as_deref(),
                    })
                })
            })
            .collect();
        for k in selected {
            let Some(embed) = self.nodes[k]
                .context
                .iter()
                .find(|f| matches!(f.kind, crate::node::FrameKind::Embed { .. }))
                .map(|f| f.id)
            else {
                continue;
            };
            if !done.insert(embed) {
                continue;
            }
            let nodes = self.rebuild_embed(k, tree, text, sources)?;
            report.nodes_built += nodes.len();
            for j in nodes {
                self.push_changed(j, &mut report.changed);
            }
        }
        Some(report)
    }

    /// Rebuilds node `k` in place, or (a node inside a note embed) the
    /// embed it is in, unless that was rebuilt already (`embeds`); the
    /// rebuilt nodes and their items go to `report`.
    fn rebuild_stale<S: TextSource + ?Sized>(
        &mut self,
        k: usize,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
        embeds: &mut std::collections::HashSet<BlockId>,
        report: &mut UpdateReport,
    ) {
        if self.rebuild_node(k, tree, text, sources) {
            report.nodes_built += 1;
            self.push_changed(k, &mut report.changed);
            return;
        }
        let embed = self.nodes[k]
            .context
            .iter()
            .find(|f| matches!(f.kind, crate::node::FrameKind::Embed { .. }))
            .map(|f| f.id);
        if let Some(embed) = embed
            && embeds.insert(embed)
            && let Some(nodes) = self.rebuild_embed(k, tree, text, sources)
        {
            report.nodes_built += nodes.len();
            for j in nodes {
                self.push_changed(j, &mut report.changed);
            }
        }
    }

    /// Builds the outermost note embed node `k` is in again, in place (a
    /// node of an embedded note cannot be built alone), and returns its
    /// nodes. `None` when `k` is in no embed or the number of its nodes or
    /// items would change.
    fn rebuild_embed<S: TextSource + ?Sized>(
        &mut self,
        k: usize,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
    ) -> Option<Range<usize>> {
        let ctx = Arc::clone(&self.nodes[k].context);
        let p = ctx
            .iter()
            .position(|f| matches!(f.kind, crate::node::FrameKind::Embed { .. }))?;
        let embed = ctx[p].id;
        let inside = |n: &BlockNode| n.context.get(p).is_some_and(|f| f.id == embed);
        let mut a = k;
        while a > 0 && inside(&self.nodes[a - 1]) {
            a -= 1;
        }
        let mut b = k + 1;
        while b < self.nodes.len() && inside(&self.nodes[b]) {
            b += 1;
        }
        if self.nodes[a].id != embed {
            return None;
        }
        let refs = self.refs.clone().unwrap_or_else(|| tree.ref_map(text));
        let mut builder = NodeBuilder::new(
            text,
            refs,
            &self.opts,
            tree.options().inline(),
            sources,
            None,
        );
        builder.parse_opts = Some(tree.options());
        builder.footnotes = self.footnotes.clone();
        builder.divert = false;
        let mut again = crate::build::EmbedAgain::new(embed, ctx[..p].to_vec(), builder);
        gv_syntax::walk_range(tree, self.nodes[a].block.clone(), &mut again);
        let mut nodes = again.into_nodes();
        if nodes.len() != b - a {
            return None;
        }
        let ia = self.items.partition_point(|it| it.node < a);
        let ib = self.items.partition_point(|it| it.node < b);
        let mut items = Vec::new();
        let flat = self.flattener();
        for (j, (node, old)) in nodes.iter_mut().zip(&self.nodes[a..b]).enumerate() {
            node.top = old.top.clone();
            flat.node_items(node, a + j, &mut items);
        }
        if items.len() != ib - ia {
            return None;
        }
        for (slot, node) in self.nodes[a..b].iter_mut().zip(nodes) {
            *slot = node;
        }
        self.macros = OnceLock::new();
        self.index = OnceLock::new();
        for (slot, new) in self.items[ia..ib].iter_mut().zip(items) {
            *slot = Item {
                gap_before: slot.gap_before,
                opens: slot.opens,
                closes: slot.closes,
                ..new
            };
        }
        Some(a..b)
    }

    /// Soft-wraps code block `block` (a code or HTML block) or makes it
    /// scroll sideways, overriding [`Metrics::wrap_code`] for it, and
    /// re-estimates its items. Returns the indices of its items (their
    /// measured heights and layouts are stale); empty if the block has no
    /// items. The setting is kept by block id across updates and refreshes.
    pub fn set_code_wrap(&mut self, block: BlockId, wrap: bool) -> Range<usize> {
        self.code_wrap.insert(block, wrap);
        let range = self.items_of(block);
        for it in &mut self.items[range.clone()] {
            if matches!(it.kind, ItemKind::Code { .. } | ItemKind::Html { .. }) {
                it.estimate.wrap = wrap;
            }
        }
        range
    }

    /// Whether code block `block` soft-wraps (its own setting, else
    /// [`Metrics::wrap_code`]).
    pub fn code_wrap(&self, block: BlockId) -> bool {
        self.code_wrap
            .get(&block)
            .copied()
            .unwrap_or(self.opts.metrics.wrap_code)
    }

    /// The blocks with their own code wrap setting, and the setting (to
    /// carry them over to another tree of the same document).
    pub fn code_wrap_settings(&self) -> impl Iterator<Item = (BlockId, bool)> + '_ {
        self.code_wrap.iter().map(|(&b, &w)| (b, w))
    }

    /// Recomputes every item's estimate and gaps for new metrics (a font
    /// change). Style changes (zoom) need a new build.
    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.opts.metrics = metrics;
        self.items = self.flatten(0..self.nodes.len());
        self.section = self.items.partition_point(|it| it.node < self.section_node);
        let n = self.items.len();
        link(&mut self.items, 0..n, &self.opts.metrics);
        self.index = OnceLock::new();
    }

    fn rebuild<S: TextSource + ?Sized>(
        &mut self,
        tree: &SyntaxTree,
        model: &DocModel,
        text: &S,
        sources: Sources<'_>,
    ) -> UpdateReport {
        let old = self.items.len();
        let code_wrap = std::mem::take(&mut self.code_wrap);
        *self = Self::build_with_wrap(tree, model, text, self.opts.clone(), sources, code_wrap);
        UpdateReport {
            removed: 0..old,
            inserted: 0..self.items.len(),
            changed: self.items.iter().map(|it| it.id).collect(),
            nodes_built: self.nodes.len(),
            nodes_reused: 0,
            full: true,
        }
    }

    /// Rebuilds node `k` from the syntax tree in place (same id, same
    /// context); its items are re-estimated. Returns false if the leaf is
    /// gone or its item count would change.
    fn rebuild_node<S: TextSource + ?Sized>(
        &mut self,
        k: usize,
        tree: &SyntaxTree,
        text: &S,
        sources: Sources<'_>,
    ) -> bool {
        let refs = self.refs.clone().unwrap_or_else(|| tree.ref_map(text));
        let id = self.nodes[k].id;
        if k >= self.section_node()
            && let Some(index) = self.footnotes.clone()
            && let Some(note) = index
                .inline
                .iter()
                .find(|n| BlockId(INLINE_NOTE_IDS - u64::from(n.number)) == id)
        {
            // An inline footnote's definition: built again from the note.
            return self
                .inline_note(note, &index, tree, text, sources, &refs)
                .and_then(|mut s| s.nodes.pop())
                .is_some_and(|node| self.replace_node(k, node));
        }
        let old = &self.nodes[k];
        let mut finder = LeafFinder {
            id: old.id,
            table: old.table,
            builder: NodeBuilder::new(
                text,
                refs,
                &self.opts,
                tree.options().inline(),
                sources,
                None,
            ),
            found: None,
        };
        finder.builder.footnotes = self.footnotes.clone();
        finder.builder.divert = false;
        if k >= self.section_node()
            && let Some(label) = old.context.first().and_then(|f| match &f.kind {
                crate::node::FrameKind::Footnote { label, .. } => Some(label.clone()),
                _ => None,
            })
            && matches!(old.kind, NodeKind::Paragraph(_))
            && self.nodes.get(k + 1).is_none_or(|n| {
                n.context.first().map(|f| f.id) != old.context.first().map(|f| f.id)
            })
        {
            let count = self.footnotes.as_ref().map_or(0, |f| f.references(&label));
            finder.builder.backrefs = Some((old.id, count));
        }
        gv_syntax::walk_range(tree, old.block.clone(), &mut finder);
        let Some(mut node) = finder.found else {
            return false;
        };
        let old = &self.nodes[k];
        node.context = old.context.clone();
        node.top = old.top.clone();
        if let (
            NodeKind::Heading { anchor, .. },
            NodeKind::Heading {
                anchor: old_anchor, ..
            },
        ) = (&mut node.kind, &old.kind)
        {
            anchor.clone_from(old_anchor);
        }
        self.replace_node(k, node)
    }

    /// Puts `node` in place of node `k` (same id and context), its items
    /// re-estimated. False if its item count would change.
    fn replace_node(&mut self, k: usize, node: BlockNode) -> bool {
        let ia = self.items.partition_point(|it| it.node < k);
        let ib = self.items.partition_point(|it| it.node <= k);
        let mut items = Vec::new();
        self.flattener().node_items(&node, k, &mut items);
        if items.len() != ib - ia {
            return false;
        }
        self.nodes[k] = node;
        self.macros = OnceLock::new();
        for (slot, new) in self.items[ia..ib].iter_mut().zip(items) {
            *slot = Item {
                gap_before: slot.gap_before,
                opens: slot.opens,
                closes: slot.closes,
                ..new
            };
        }
        true
    }

    fn push_changed(&self, k: usize, out: &mut Vec<ItemId>) {
        let ia = self.items.partition_point(|it| it.node < k);
        let ib = self.items.partition_point(|it| it.node <= k);
        out.extend(self.items[ia..ib].iter().map(|it| it.id));
    }

    fn flattener(&self) -> Flatten<'_> {
        Flatten {
            styles: &self.opts.styles,
            metrics: &self.opts.metrics,
            chunk_lines: self.opts.chunk_lines,
            code_wrap: &self.code_wrap,
        }
    }

    /// Items of nodes `range`.
    fn flatten(&self, range: Range<usize>) -> Vec<Item> {
        let f = self.flattener();
        let mut out = Vec::with_capacity(range.len());
        for k in range {
            f.node_items(&self.nodes[k], k, &mut out);
        }
        out
    }

    /// The extent (old coordinates) of the old top-level blocks that
    /// intersect `r` (an empty range stands for the byte at its start);
    /// `(r.start, r.end)` if none.
    fn old_top_extent(&self, r: Range<usize>) -> (usize, usize) {
        let end = r.end.max(r.start + 1);
        let mut k = self.nodes.partition_point(|n| n.top.end <= r.start);
        let (mut s, mut e) = (r.start, r.end);
        while let Some(n) = self.nodes.get(k) {
            if n.top.start >= end {
                break;
            }
            s = s.min(n.top.start);
            e = e.max(n.top.end);
            k += 1;
        }
        (s, e)
    }

    /// Sets heading anchors from the model's slugs.
    fn assign_anchors(&mut self, model: &DocModel) {
        let slugs = model.slugs(self.opts.slugs);
        let by_block: HashMap<BlockId, usize> = model
            .headings()
            .iter()
            .enumerate()
            .map(|(i, h)| (h.block, i))
            .collect();
        for n in &mut self.nodes {
            if let NodeKind::Heading { anchor, .. } = &mut n.kind {
                let slug = by_block.get(&n.id).and_then(|&i| slugs.get(i));
                if anchor.as_deref() != slug.map(|s| &**s) {
                    *anchor = slug.map(|s| Arc::from(&**s));
                }
            }
        }
    }

    /// The options the tree was built with.
    pub fn options(&self) -> &RenderOptions {
        &self.opts
    }

    /// Length of the document in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the document is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The render nodes in document order.
    pub fn nodes(&self) -> &[BlockNode] {
        &self.nodes
    }

    /// The virtual items in document order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The node of item `ix`.
    pub fn node_of(&self, ix: usize) -> Option<&BlockNode> {
        self.items.get(ix).and_then(|it| self.nodes.get(it.node))
    }

    /// The inline content item `ix` lays out (a paragraph's, a heading's
    /// or a table row's).
    pub fn item_inline(&self, ix: usize) -> Option<&Arc<crate::InlineContent>> {
        let item = self.items.get(ix)?;
        self.nodes.get(item.node)?.item_inline(&item.kind)
    }

    /// The index of the item with id `id`.
    pub fn index_of(&self, id: ItemId) -> Option<usize> {
        self.index
            .get_or_init(|| {
                self.items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| (it.id, i))
                    .collect()
            })
            .get(&id)
            .copied()
    }

    /// The items of block `block` ([`ItemId::block`]): one, or the chunks
    /// of a code block, HTML block or front matter. Empty if the block has
    /// no items.
    pub fn items_of(&self, block: BlockId) -> Range<usize> {
        let Some(start) = self.index_of(ItemId { block, chunk: 0 }) else {
            return 0..0;
        };
        let (_, chunks) = self.items[start].kind.chunk();
        let end = start
            .saturating_add(usize::try_from(chunks).unwrap_or(usize::MAX))
            .min(self.items.len());
        start..end
    }

    /// The item whose source contains `offset`, or the first item after
    /// it (`None` past the last item). The footnote section's items count
    /// where their sources are.
    pub fn item_at(&self, offset: usize) -> Option<usize> {
        let main = &self.items[..self.section];
        let k = main.partition_point(|it| it.source.end <= offset);
        if let Some(ix) = (self.section..self.items.len()).find(|&ix| {
            let s = &self.items[ix].source;
            s.start <= offset && offset < s.end
        }) && main.get(k).is_none_or(|it| it.source.start > offset)
        {
            return Some(ix);
        }
        (k < self.section).then_some(k)
    }

    /// The items whose sources intersect `range` (an empty range stands for
    /// the position: the item containing it), outside the footnote
    /// section.
    pub fn items_in(&self, range: Range<usize>) -> Range<usize> {
        let main = &self.items[..self.section];
        let a = main.partition_point(|it| it.source.end <= range.start);
        let end = range.end.max(range.start + 1);
        let b = main.partition_point(|it| it.source.start < end);
        a..b.max(a)
    }

    /// The index of the first node of the footnote section.
    fn section_node(&self) -> usize {
        self.section_node
    }

    /// The math macros in force for formula `k` of node `node` (a math
    /// block's formula is 0; a paragraph's, heading's or table's are its
    /// [`InlineContent::math`](crate::InlineContent::math) in order, row by
    /// row): the definitions of the formulas before it in document order
    /// (MathJax's document-wide scope). The pass over all formulas runs
    /// once per tree, on the first query.
    pub fn math_macros(&self, node: usize, k: usize) -> MacroSet {
        let table = self.macros.get_or_init(|| Arc::new(self.macro_table()));
        let Some(&start) = table.starts.get(node) else {
            return MacroSet::empty();
        };
        table
            .sets
            .get(start as usize + k)
            .cloned()
            .unwrap_or_default()
    }

    fn macro_table(&self) -> MacroTable {
        let mut pass = MacroPass::new(MacroLimits::default());
        let mut t = MacroTable {
            starts: Vec::with_capacity(self.nodes.len() + 1),
            sets: Vec::new(),
        };
        for n in &self.nodes {
            t.starts
                .push(u32::try_from(t.sets.len()).unwrap_or(u32::MAX));
            if let NodeKind::Math(code) = &n.kind {
                t.sets.push(pass.next(&code.text));
            }
            for c in n.inlines() {
                for m in &c.math {
                    t.sets.push(pass.next(&m.tex));
                }
            }
        }
        t.starts
            .push(u32::try_from(t.sets.len()).unwrap_or(u32::MAX));
        t
    }

    /// The item of the heading with anchor `anchor`.
    pub fn heading_item(&self, anchor: &str) -> Option<usize> {
        let k = self.nodes.iter().position(
            |n| matches!(&n.kind, NodeKind::Heading { anchor: Some(a), .. } if &**a == anchor),
        )?;
        Some(self.items.partition_point(|it| it.node < k))
    }

    /// Estimated total height at `width` (O(items)).
    pub fn height_estimate(&self, width: f32) -> f32 {
        self.items.iter().map(|it| it.height_estimate(width)).sum()
    }

    /// Keeps only the items whose sources intersect `range` (the footnote
    /// section goes): a part of a document shown with the rest of the
    /// document as its context. Nodes stay, so footnote numbers, reference
    /// definitions and math macros are the whole text's; the kept items
    /// are spaced as if they were the whole document.
    pub fn keep_items_in(&mut self, range: Range<usize>) {
        let keep = self.items_in(range);
        self.items.truncate(keep.end);
        self.items.drain(..keep.start);
        self.section = self.items.len();
        self.index = OnceLock::new();
        let n = self.items.len();
        link(&mut self.items, 0..n, &self.opts.metrics);
    }
}

/// Whether two nodes' containers indent them the same way.
fn same_indent(a: &BlockNode, b: &BlockNode) -> bool {
    a.context.len() == b.context.len()
        && a.context
            .iter()
            .zip(b.context.iter())
            .all(|(x, y)| std::mem::discriminant(&x.kind) == std::mem::discriminant(&y.kind))
}

fn shift(r: &Range<usize>, d: isize) -> Range<usize> {
    r.start.saturating_add_signed(d)..r.end.saturating_add_signed(d)
}

/// The extent of the top-level blocks of `tree` that intersect `r`
/// (`(r.start, r.end)` if none). Only blocks that contain the first or the
/// last byte of `r` can reach past it, so only those two bytes are walked.
fn top_level_extent(tree: &SyntaxTree, r: Range<usize>) -> (usize, usize) {
    struct Extent {
        depth: usize,
        s: usize,
        e: usize,
    }
    impl Visitor for Extent {
        fn enter(&mut self, c: &Container) {
            if self.depth == 0 {
                self.s = self.s.min(c.range.start);
            }
            self.depth += 1;
        }
        fn exit(&mut self, c: &Container) {
            self.depth = self.depth.saturating_sub(1);
            if self.depth == 0 {
                self.e = self.e.max(c.range.end);
            }
        }
        fn leaf(&mut self, l: &Leaf<'_>) {
            if self.depth == 0 {
                self.s = self.s.min(l.range.start);
                self.e = self.e.max(l.range.end);
            }
        }
    }
    let mut v = Extent {
        depth: 0,
        s: r.start,
        e: r.end,
    };
    let last = r.end.max(r.start + 1) - 1;
    for p in [r.start, last] {
        if p < tree.len() {
            v.depth = 0;
            gv_syntax::walk_range(tree, p..p + 1, &mut v);
        }
    }
    (v.s, v.e)
}

/// Visits the top-level blocks that start in `r` (a union of whole
/// top-level blocks): a range walk for small regions, a filtered full walk
/// for large ones (a range walk costs more per line).
fn walk_region(tree: &SyntaxTree, r: Range<usize>, v: &mut impl Visitor) {
    if r.start >= r.end {
        return;
    }
    if r.len() < tree.len() / 8 {
        gv_syntax::walk_range(tree, r, v);
        return;
    }
    struct Filter<'v, V> {
        r: Range<usize>,
        depth: usize,
        inside: bool,
        v: &'v mut V,
    }
    impl<V: Visitor> Visitor for Filter<'_, V> {
        fn enter(&mut self, c: &Container) {
            if self.depth == 0 {
                self.inside = self.r.contains(&c.range.start);
            }
            self.depth += 1;
            if self.inside {
                self.v.enter(c);
            }
        }
        fn exit(&mut self, c: &Container) {
            self.depth = self.depth.saturating_sub(1);
            if self.inside {
                self.v.exit(c);
            }
        }
        fn leaf(&mut self, l: &Leaf<'_>) {
            if self.depth == 0 {
                self.inside = self.r.contains(&l.range.start);
            }
            if self.inside {
                self.v.leaf(l);
            }
        }
    }
    gv_syntax::walk(
        tree,
        &mut Filter {
            r,
            depth: 0,
            inside: false,
            v,
        },
    );
}

/// Builds the node of one leaf found by id.
struct LeafFinder<'a, S: TextSource + ?Sized> {
    id: BlockId,
    /// The table of the block (else its paragraph or other leaf).
    table: bool,
    builder: NodeBuilder<'a, S>,
    found: Option<BlockNode>,
}

impl<S: TextSource + ?Sized> Visitor for LeafFinder<'_, S> {
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if leaf.id == self.id
            && leaf.kind != LeafKind::LinkRefDef
            && (leaf.kind == LeafKind::Table) == self.table
            && self.found.is_none()
        {
            self.builder.leaf(leaf);
            self.found = self.builder.nodes.pop();
        }
    }
}

/// Whether `range` of `text` holds `^[` (an inline footnote may start
/// there).
fn holds_inline_note<S: TextSource + ?Sized>(text: &S, range: Range<usize>) -> bool {
    let mut buf = Vec::new();
    text.copy_range(range, &mut buf);
    buf.windows(2).any(|w| w == b"^[")
}

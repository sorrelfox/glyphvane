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
        let refs = self.refs.clone().unwrap_or_else(|| tree.ref_map(text));
        for mut def in defs {
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
                    title: None,
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
            for f in c.footnotes.iter().filter(|f| &*f.label == label) {
                out.push((
                    ix,
                    node.to_document(f.source.start)..node.to_document(f.source.end),
                ));
            }
        }
        out.sort_by_key(|(_, r)| r.start);
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
        let old_end = (e as isize - delta).max(s as isize) as usize;

        // Old nodes and items of the region.
        let a = self.nodes.partition_point(|n| n.source.start < s);
        let b = self.nodes.partition_point(|n| n.source.start < old_end);
        let b = b.max(a);
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
            for k in stale {
                if self.rebuild_node(k, tree, text, sources) {
                    report.nodes_built += 1;
                    self.push_changed(k, &mut report.changed);
                }
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
        for k in stale {
            if self.rebuild_node(k, tree, text, sources) {
                report.nodes_built += 1;
                self.push_changed(k, &mut report.changed);
            }
        }
        report
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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Building a [`DocModel`] from a syntax tree, from scratch or by taking
//! over the records of untouched blocks from the previous revision.
//!
//! # Full build
//!
//! One [`walk`] over the tree. Headings are always inline-parsed (for
//! their text and links); paragraphs only if a line holds `[` or `<`,
//! since no link, image or autolink can start without one. Link reference
//! definitions come with the ranges of their parts
//! ([`Leaf::link_ref_def`]). Front matter is recorded, never
//! inline-parsed. Then the document-global passes: the outline, which
//! definitions count (the first per label), and, on first use, the slugs.
//!
//! # Update
//!
//! The reparse reports the byte window it re-parsed ([`Changes`]): outside
//! it, the old and the new tree are the same, shifted by the length
//! difference after it. The update walks only the window
//! ([`SyntaxTree::walk_range`]), which visits every leaf that shares a byte
//! with it, whole (so it may start before the window and end after it).
//! The first and last leaf visited are the split points: the previous
//! model's blocks before the first are kept as they are, those after the
//! last are kept shifted by the length and line differences, and the
//! window's records go in between. One case moves the first split point:
//! a block before the window that the edit ended on the window's first
//! line now closes where the window starts, shares no byte with it and is
//! not visited, so the walk is redone from that block's start.
//!
//! A kept block is rebuilt (by a range walk over it) only if the reference
//! labels its inlines looked up resolve differently now, which needs a
//! changed reference map: an edit that touched a definition.
//!
//! The cost is the window's leaves plus copying the kept records (no
//! inline parsing, no walk outside the window), and the global passes.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gv_syntax::inline::{
    InlineInput, InlineTree, LinkKind as SynLinkKind, RefDef, RefLookup, parse_inlines,
};
use gv_syntax::{
    Changes, Container, FrontMatterKind, Leaf, LeafKind, RefMap, SyntaxTree, Visitor, walk,
};
use gv_text::TextSource;

use crate::model::{
    BlockFacts, BuildStats, Definition, DocModel, FrontMatter, Heading, Link, LinkKind, ModelData,
};
use crate::outline;

impl DocModel {
    /// Builds the model of `tree`, the parse of `text`.
    pub fn build<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S) -> DocModel {
        let mut b = Builder::new(tree, text, None);
        walk(tree, &mut b);
        b.flush();
        b.finish()
    }

    /// Builds the model of `tree` from `self`, the model of the tree that
    /// `tree` was reparsed from, with `changes` as that reparse reported
    /// them ([`gv_syntax::Reparse::Done`] or
    /// [`gv_syntax::ReparseTask::finish`]). Blocks outside the reparsed
    /// window keep their records. The result equals
    /// [`build`](Self::build) of the same tree.
    ///
    /// To skip revisions, compose their changes ([`Changes::compose_all`])
    /// and update once. An interim tree (a paused reparse) or changes that
    /// do not fit the two lengths fall back to a full build.
    pub fn update<S: TextSource + ?Sized>(
        &self,
        tree: &SyntaxTree,
        text: &S,
        changes: &Changes,
    ) -> DocModel {
        let old: &ModelData = &self.0;
        let delta = tree.len() as isize - old.len as isize;
        let (w, ow) = (changes.new_range.clone(), changes.old_range.clone());
        let fits = !tree.is_interim()
            && w.start == ow.start
            && w.start <= w.end
            && ow.start <= ow.end
            && w.end <= tree.len()
            && ow.end <= old.len
            && w.len() as isize - ow.len() as isize == delta;
        if !fits {
            return DocModel::build(tree, text);
        }
        let line_delta = changes.new_end_line as isize - changes.old_end_line as isize;

        // The window's records, and the split points.
        let mut win = Builder::new(tree, text, None);
        let (mut lo, mut hi) = (w.start, w.start);
        if !changes.is_empty() {
            win.walk_range(w.clone());
            lo = win.first_start.map_or(w.start, |s| s.min(w.start));
            hi = win.last_end.max(w.end);
        }
        // A block before the window that closed inside it may now close
        // where the window starts: then it shares no byte with the window
        // and was not visited, but its range changed. Walk again from its
        // start (rare: the edit ended the block on the window's first line).
        let p = old.blocks.partition_point(|b| b.range.start < lo);
        if let Some(b) = p.checked_sub(1).map(|i| &old.blocks[i])
            && b.range.end > lo
        {
            win = Builder::new(tree, text, None);
            win.walk_range(b.range.start..w.end.max(b.range.start + 1));
            lo = b.range.start;
            hi = hi.max(win.last_end);
        }
        let p = old.blocks.partition_point(|b| b.range.start < lo);
        let old_hi = hi.saturating_add_signed(-delta);
        let s = old.blocks.partition_point(|b| b.range.start < old_hi);
        // Leaves do not overlap, so no other kept block reaches over a
        // split point; if one did, the window would not be what the reparse
        // reported.
        let straddles = |i: Option<usize>, at: usize| {
            i.and_then(|i| old.blocks.get(i))
                .is_some_and(|b| b.range.end > at)
        };
        if straddles(p.checked_sub(1), lo) || (s > p && straddles(Some(s - 1), old_hi)) {
            return DocModel::build(tree, text);
        }

        let mut out = Builder::new(tree, text, Some(old));
        // The reference map: kept blocks whose lookups resolve differently
        // now are rebuilt. With an unchanged map (shared by the reparse)
        // nothing is.
        let changed_refs = old.ref_map.as_ref().and_then(|old_refs| {
            let refs = out.refs();
            (!Arc::ptr_eq(old_refs, refs)).then(|| Arc::clone(refs))
        });
        let stale = |b: &BlockFacts| {
            changed_refs.as_ref().is_some_and(|refs| {
                b.lookups.as_ref().is_some_and(|lookups| {
                    lookups
                        .iter()
                        .any(|(label, def)| refs.lookup(label) != def.as_ref())
                })
            })
        };
        out.keep(old, &old.blocks[..p], 0, 0, stale);
        out.append(win);
        out.keep(old, &old.blocks[s..], delta, line_delta, stale);
        // Front matter is at the start: the walk saw it if it reaches into
        // the window; otherwise it is kept if it lies before the window.
        if out.front_matter.is_none() {
            out.front_matter = old
                .front_matter
                .clone()
                .filter(|f| changes.is_empty() || f.range.end <= lo);
        }
        out.finish()
    }
}

struct Builder<'a, S: TextSource + ?Sized> {
    tree: &'a SyntaxTree,
    text: &'a S,
    refs: OnceLock<Arc<RefMap>>,
    headings: Vec<Heading>,
    links: Vec<Link>,
    definitions: Vec<Definition>,
    blocks: Vec<BlockFacts>,
    front_matter: Option<FrontMatter>,
    /// The block being recorded.
    open: Option<BlockFacts>,
    depth: u32,
    /// Start of the first leaf visited.
    first_start: Option<usize>,
    /// End of the last leaf visited.
    last_end: usize,
    scratch: Vec<u8>,
    stats: BuildStats,
}

impl<'a, S: TextSource + ?Sized> Builder<'a, S> {
    /// A builder; `like` is a model the result will be about as large as.
    fn new(tree: &'a SyntaxTree, text: &'a S, like: Option<&ModelData>) -> Self {
        let cap = |n: usize| n + n / 16;
        Builder {
            tree,
            text,
            refs: OnceLock::new(),
            headings: Vec::with_capacity(like.map_or(0, |m| cap(m.headings.len()))),
            links: Vec::with_capacity(like.map_or(0, |m| cap(m.links.len()))),
            definitions: Vec::with_capacity(like.map_or(0, |m| cap(m.definitions.len()))),
            blocks: Vec::with_capacity(like.map_or(0, |m| cap(m.blocks.len()))),
            front_matter: None,
            open: None,
            depth: 0,
            first_start: None,
            last_end: 0,
            scratch: Vec::new(),
            stats: BuildStats::default(),
        }
    }

    fn refs(&self) -> &Arc<RefMap> {
        self.refs.get_or_init(|| self.tree.ref_map(self.text))
    }

    /// Visits the leaves that intersect `range` and records them.
    fn walk_range(&mut self, range: Range<usize>) {
        self.depth = 0;
        let tree = self.tree;
        tree.walk_range(range, self);
        self.flush();
    }

    /// The document-global passes; the model.
    fn finish(mut self) -> DocModel {
        let outline = outline::build(self.headings.iter().map(|h| h.level));
        let mut seen = HashSet::new();
        for d in &mut self.definitions {
            d.first = seen.insert(Arc::clone(&d.label));
        }
        DocModel(Arc::new(ModelData {
            len: self.tree.len(),
            line_count: self.tree.line_count(),
            headings: self.headings,
            outline,
            links: self.links,
            definitions: self.definitions,
            block_anchors: Vec::new(),
            front_matter: self.front_matter,
            blocks: self.blocks,
            ref_map: self.refs.into_inner(),
            stats: self.stats,
            slugs: Default::default(),
            slug_keys: Default::default(),
            keys: OnceLock::new(),
        }))
    }

    /// Ends the open block: keeps its facts if it recorded anything.
    fn flush(&mut self) {
        if let Some(b) = self.open.take()
            && (!b.headings.is_empty()
                || !b.links.is_empty()
                || !b.definitions.is_empty()
                || b.parsed)
        {
            self.blocks.push(b);
        }
    }

    fn counts(&self) -> (u32, u32, u32) {
        (
            to_u32(self.headings.len()),
            to_u32(self.links.len()),
            to_u32(self.definitions.len()),
        )
    }

    /// Takes over `old`'s blocks `blocks` with their records, shifted by
    /// `delta` bytes and `line_delta` lines, except the stale ones, which
    /// are rebuilt.
    fn keep(
        &mut self,
        old: &ModelData,
        blocks: &[BlockFacts],
        delta: isize,
        line_delta: isize,
        stale: impl Fn(&BlockFacts) -> bool,
    ) {
        let mut rest = blocks;
        while let Some(b) = rest.first() {
            if stale(b) {
                self.walk_range(shift(&b.range, delta));
                rest = &rest[1..];
                continue;
            }
            let n = rest.iter().position(&stale).unwrap_or(rest.len());
            self.keep_run(old, &rest[..n], delta, line_delta);
            rest = &rest[n..];
        }
    }

    /// [`keep`](Self::keep) for consecutive blocks none of which is stale:
    /// their records are consecutive too.
    fn keep_run(&mut self, old: &ModelData, run: &[BlockFacts], delta: isize, line_delta: isize) {
        let (Some(first), Some(last)) = (run.first(), run.last()) else {
            return;
        };
        let (h0, l0, d0) = self.counts();
        let span = |a: &Range<u32>, b: &Range<u32>| a.start as usize..b.end as usize;
        let headings = &old.headings[span(&first.headings, &last.headings)];
        let links = &old.links[span(&first.links, &last.links)];
        let definitions = &old.definitions[span(&first.definitions, &last.definitions)];
        if delta == 0 && line_delta == 0 {
            self.headings.extend_from_slice(headings);
            self.links.extend_from_slice(links);
            self.definitions.extend_from_slice(definitions);
        } else {
            self.headings.extend(headings.iter().map(|h| Heading {
                range: shift(&h.range, delta),
                content_range: shift(&h.content_range, delta),
                line: shift_line(h.line, line_delta),
                end_line: shift_line(h.end_line, line_delta),
                ..h.clone()
            }));
            self.links.extend(links.iter().map(|l| Link {
                range: shift(&l.range, delta),
                text_range: shift(&l.text_range, delta),
                destination_range: l.destination_range.as_ref().map(|r| shift(r, delta)),
                label_range: l.label_range.as_ref().map(|r| shift(r, delta)),
                ..l.clone()
            }));
            self.definitions
                .extend(definitions.iter().map(|d| Definition {
                    label_range: shift(&d.label_range, delta),
                    destination_range: shift(&d.destination_range, delta),
                    title_range: d.title_range.as_ref().map(|r| shift(r, delta)),
                    range: shift(&d.range, delta),
                    line: shift_line(d.line, line_delta),
                    ..d.clone()
                }));
        }
        // Record indices move by the same amount for the whole run.
        let at = |r: &Range<u32>, from: u32, to: u32| r.start - from + to..r.end - from + to;
        let (fh, fl, fd) = (
            first.headings.start,
            first.links.start,
            first.definitions.start,
        );
        self.blocks.extend(run.iter().map(|b| BlockFacts {
            id: b.id,
            range: shift(&b.range, delta),
            headings: at(&b.headings, fh, h0),
            links: at(&b.links, fl, l0),
            definitions: at(&b.definitions, fd, d0),
            lookups: b.lookups.clone(),
            parsed: b.parsed,
        }));
        self.stats.reused += run.len();
    }

    /// Appends the records of `other` (built over the same tree).
    fn append(&mut self, other: Builder<'_, S>) {
        let (h, l, d) = self.counts();
        let at = |r: &Range<u32>, base: u32| r.start + base..r.end + base;
        self.blocks
            .extend(other.blocks.into_iter().map(|b| BlockFacts {
                headings: at(&b.headings, h),
                links: at(&b.links, l),
                definitions: at(&b.definitions, d),
                ..b
            }));
        self.headings.extend(other.headings);
        self.links.extend(other.links);
        self.definitions.extend(other.definitions);
        if other.front_matter.is_some() {
            self.front_matter = other.front_matter;
        }
        if let Some(refs) = other.refs.into_inner() {
            let _ = self.refs.set(refs);
        }
        self.stats.leaves += other.stats.leaves;
        self.stats.reused += other.stats.reused;
        self.stats.inline_parses += other.stats.inline_parses;
    }

    /// Parses the inlines of `leaf`, records its links and lookups.
    fn inlines(&mut self, leaf: &Leaf<'_>, in_heading: bool) -> InlineTree {
        let segments = leaf.segments();
        let refs = Arc::clone(self.refs());
        let tree = parse_inlines(
            self.text,
            InlineInput {
                segments: &segments,
            },
            &*refs,
        );
        self.stats.inline_parses += 1;
        if let Some(open) = self.open.as_mut() {
            open.parsed = true;
        }
        for l in tree.links() {
            let kind = match l.kind {
                SynLinkKind::Inline => LinkKind::Inline,
                SynLinkKind::Full => LinkKind::Full,
                SynLinkKind::Collapsed => LinkKind::Collapsed,
                SynLinkKind::Shortcut => LinkKind::Shortcut,
                SynLinkKind::UriAutolink => LinkKind::UriAutolink,
                SynLinkKind::EmailAutolink => LinkKind::EmailAutolink,
            };
            let destination: Arc<str> = if kind == LinkKind::EmailAutolink {
                format!("mailto:{}", l.destination).into()
            } else {
                l.destination.into()
            };
            self.links.push(Link {
                kind,
                image: l.is_image,
                range: l.range,
                text_range: l.text_range,
                destination,
                destination_range: l.raw_destination,
                title: l.title.map(Arc::from),
                label: l.label.map(Arc::from),
                label_range: l.label_range,
                in_heading,
                block: leaf.id,
            });
        }
        let mut seen = HashSet::new();
        let lookups: Vec<(Box<str>, Option<RefDef>)> = tree
            .label_lookups()
            .filter(|label| seen.insert(*label))
            .map(|label| (label.into(), refs.lookup(label).cloned()))
            .collect();
        if !lookups.is_empty()
            && let Some(open) = self.open.as_mut()
        {
            let merged: Vec<_> = open
                .lookups
                .iter()
                .flat_map(|l| l.iter().cloned())
                .chain(lookups)
                .collect();
            open.lookups = Some(merged.into());
        }
        tree
    }

    fn heading(&mut self, leaf: &Leaf<'_>, level: u8, setext: bool) {
        let tree = self.inlines(leaf, true);
        let content: Vec<&gv_syntax::LeafLine> =
            leaf.lines.iter().filter(|l| l.has_content).collect();
        let content_range = match (content.first(), content.last()) {
            (Some(a), Some(b)) => a.content.start..b.content.end,
            _ => leaf
                .lines
                .first()
                .map_or(leaf.range.start..leaf.range.start, |l| l.content.clone()),
        };
        let mut source = Vec::new();
        for (i, l) in content.iter().enumerate() {
            if i > 0 {
                source.push(b'\n');
            }
            self.text.copy_range(l.content.clone(), &mut source);
        }
        let range = if setext {
            content_range.start..leaf.range.end
        } else {
            leaf.range.clone()
        };
        let text: Arc<str> = tree.text_content().into();
        let with_alt = tree.collect_text();
        let text_with_alt = (with_alt != *text).then(|| with_alt.into());
        self.headings.push(Heading {
            level,
            setext,
            range,
            content_range,
            line: to_u32(leaf.start_line),
            end_line: to_u32(leaf.end_line),
            container_depth: self.depth,
            text,
            source: String::from_utf8_lossy(&source).into(),
            text_with_alt,
            explicit_id: None,
            block: leaf.id,
        });
    }

    fn paragraph(&mut self, leaf: &Leaf<'_>) {
        // No link, image or autolink starts without `[` or `<`.
        let mut candidate = false;
        for l in leaf.lines.iter().filter(|l| l.has_content) {
            let line = self.text.line(l.content.start, &mut self.scratch);
            let n = (l.content.end - l.content.start).min(line.len());
            if memchr::memchr2(b'[', b'<', &line[..n]).is_some() {
                candidate = true;
                break;
            }
        }
        if candidate {
            self.inlines(leaf, false);
        }
    }

    fn definition(&mut self, leaf: &Leaf<'_>) {
        let Some(d) = leaf.link_ref_def(self.text) else {
            return;
        };
        self.definitions.push(Definition {
            label: d.label.into(),
            label_range: d.label_range,
            destination: d.def.destination.into(),
            destination_range: d.destination_range,
            title: d.def.title.map(Arc::from),
            title_range: d.title_range,
            range: d.range,
            line: to_u32(leaf.start_line),
            first: false,
            block: leaf.id,
        });
    }

    fn front_matter(&mut self, leaf: &Leaf<'_>, kind: FrontMatterKind) {
        // The content runs from the end of the opening delimiter line to
        // the start of the closing one.
        let open_end = leaf.range.start + self.text.line(leaf.range.start, &mut self.scratch).len();
        let content_end = leaf.lines.last().map_or(open_end, |l| l.line_ending.end);
        self.front_matter = Some(FrontMatter {
            range: leaf.range.clone(),
            content_range: open_end..content_end.max(open_end),
            kind,
        });
    }
}

impl<S: TextSource + ?Sized> Visitor for Builder<'_, S> {
    fn enter(&mut self, _: &Container) {
        self.depth += 1;
    }

    fn exit(&mut self, _: &Container) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        self.stats.leaves += 1;
        self.first_start.get_or_insert(leaf.range.start);
        self.last_end = self.last_end.max(leaf.range.end);
        match leaf.kind {
            LeafKind::Heading { .. } | LeafKind::Paragraph | LeafKind::LinkRefDef => {}
            LeafKind::FrontMatter { kind } => {
                self.front_matter(leaf, kind);
                return;
            }
            _ => return,
        }
        if self.open.as_ref().map(|b| b.id) != Some(leaf.id) {
            self.flush();
            let (h, l, d) = self.counts();
            self.open = Some(BlockFacts {
                id: leaf.id,
                range: leaf.range.clone(),
                headings: h..h,
                links: l..l,
                definitions: d..d,
                lookups: None,
                parsed: false,
            });
        }
        match leaf.kind {
            LeafKind::Heading { level, setext } => self.heading(leaf, level, setext),
            LeafKind::Paragraph => self.paragraph(leaf),
            LeafKind::LinkRefDef => self.definition(leaf),
            _ => {}
        }
        let (h, l, d) = self.counts();
        if let Some(open) = self.open.as_mut() {
            open.headings.end = h;
            open.links.end = l;
            open.definitions.end = d;
        }
    }
}

fn shift(r: &Range<usize>, delta: isize) -> Range<usize> {
    r.start.saturating_add_signed(delta)..r.end.saturating_add_signed(delta)
}

fn shift_line(line: u32, delta: isize) -> u32 {
    to_u32((line as usize).saturating_add_signed(delta))
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

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
//! definitions are located in their lines. Then the document-global
//! passes: the outline, which definitions count (the first per label), and,
//! on first use, the slugs.
//!
//! # Update
//!
//! The reparse reports the byte window it re-parsed ([`Changes`]). Blocks
//! entirely before the window are unchanged; blocks entirely after it are
//! unchanged but shifted by the length difference. For such a block, the
//! previous model's records are taken over (shifted) when the previous
//! model has a block with the same [`BlockId`] at the corresponding range,
//! and the reference labels its inlines looked up still resolve the same.
//! Everything else is rebuilt as in a full build. The walk itself is still
//! O(document); only the inline parsing and record building are saved.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gv_syntax::inline::{
    InlineInput, InlineTree, LinkKind as SynLinkKind, RefDef, RefLookup, parse_inlines,
};
use gv_syntax::{BlockId, Changes, Container, Leaf, LeafKind, RefMap, SyntaxTree, Visitor, walk};
use gv_text::TextSource;

use crate::model::{
    BlockFacts, BuildStats, Definition, DocModel, Heading, Link, LinkKind, ModelData,
};
use crate::refdef::{Joined, locate};
use crate::{outline, text};

impl DocModel {
    /// Builds the model of `tree`, the parse of `text`.
    pub fn build<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S) -> DocModel {
        Builder::new(tree, text, None).run()
    }

    /// Builds the model of `tree` from `self`, the model of the tree that
    /// `tree` was reparsed from, with `changes` as that reparse reported
    /// them ([`gv_syntax::Reparse::Done`] or
    /// [`gv_syntax::ReparseTask::finish`]). Blocks outside the reparsed
    /// window keep their records. The result equals
    /// [`build`](Self::build) of the same tree.
    ///
    /// An interim tree (a paused reparse) or changes that do not fit the
    /// two lengths fall back to a full build.
    pub fn update<S: TextSource + ?Sized>(
        &self,
        tree: &SyntaxTree,
        text: &S,
        changes: &Changes,
    ) -> DocModel {
        let delta = tree.len() as isize - self.0.len as isize;
        let w = changes.new_range.clone();
        let fits = !tree.is_interim()
            && w.start <= w.end
            && w.end <= tree.len()
            && w.end as isize - delta >= w.start as isize;
        if !fits {
            return DocModel::build(tree, text);
        }
        let reuse = Reuse {
            old: &self.0,
            window: w,
            delta,
            line_delta: changes.new_end_line as isize - changes.old_end_line as isize,
            cursor: 0,
        };
        Builder::new(tree, text, Some(reuse)).run()
    }
}

/// The previous model and how the reparse moved its blocks.
struct Reuse<'a> {
    old: &'a ModelData,
    /// The reparsed bytes in the new text.
    window: Range<usize>,
    /// New length minus old length.
    delta: isize,
    /// New line count minus old line count.
    line_delta: isize,
    /// Merge-join position in `old.blocks`.
    cursor: usize,
}

struct Builder<'a, S: TextSource + ?Sized> {
    tree: &'a SyntaxTree,
    text: &'a S,
    refs: OnceLock<Arc<RefMap>>,
    reuse: Option<Reuse<'a>>,
    headings: Vec<Heading>,
    links: Vec<Link>,
    definitions: Vec<Definition>,
    blocks: Vec<BlockFacts>,
    /// The block being recorded.
    open: Option<BlockFacts>,
    /// A block taken over from the previous model: its other leaves are
    /// skipped.
    skip: Option<BlockId>,
    depth: u32,
    scratch: Vec<u8>,
    stats: BuildStats,
}

impl<'a, S: TextSource + ?Sized> Builder<'a, S> {
    fn new(tree: &'a SyntaxTree, text: &'a S, reuse: Option<Reuse<'a>>) -> Self {
        // An update ends up about as large as the previous model.
        let cap = |n: usize| n + n / 16;
        let old = reuse.as_ref().map(|r| r.old);
        Builder {
            tree,
            text,
            refs: OnceLock::new(),
            headings: Vec::with_capacity(old.map_or(0, |m| cap(m.headings.len()))),
            links: Vec::with_capacity(old.map_or(0, |m| cap(m.links.len()))),
            definitions: Vec::with_capacity(old.map_or(0, |m| cap(m.definitions.len()))),
            blocks: Vec::with_capacity(old.map_or(0, |m| cap(m.blocks.len()))),
            reuse,
            open: None,
            skip: None,
            depth: 0,
            scratch: Vec::new(),
            stats: BuildStats::default(),
        }
    }

    fn refs(&self) -> &Arc<RefMap> {
        self.refs.get_or_init(|| self.tree.ref_map(self.text))
    }

    fn run(mut self) -> DocModel {
        let tree = self.tree;
        walk(tree, &mut self);
        self.flush();
        // Document-global passes.
        let outline = outline::build(self.headings.iter().map(|h| h.level));
        let mut seen = HashSet::new();
        for d in &mut self.definitions {
            d.first = seen.insert(Arc::clone(&d.label));
        }
        let ref_map = self.refs.into_inner();
        DocModel(Arc::new(ModelData {
            len: tree.len(),
            line_count: tree.line_count(),
            headings: self.headings,
            outline,
            links: self.links,
            definitions: self.definitions,
            block_anchors: Vec::new(),
            blocks: self.blocks,
            ref_map,
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
            u32::try_from(self.headings.len()).unwrap_or(u32::MAX),
            u32::try_from(self.links.len()).unwrap_or(u32::MAX),
            u32::try_from(self.definitions.len()).unwrap_or(u32::MAX),
        )
    }

    /// Takes over the previous model's records of the block `leaf` starts,
    /// if it is unchanged.
    fn try_reuse(&mut self, leaf: &Leaf<'_>) -> bool {
        let Some(r) = self.reuse.as_mut() else {
            return false;
        };
        let old: &ModelData = r.old;
        let range = leaf.range.clone();
        let after = range.start >= r.window.end;
        let (delta, line_delta) = if range.end <= r.window.start && !after {
            (0, 0)
        } else if after {
            (r.delta, r.line_delta)
        } else {
            return false;
        };
        let old_range = shift(&range, -delta);
        while r.cursor < old.blocks.len() && old.blocks[r.cursor].range.start < old_range.start {
            r.cursor += 1;
        }
        let Some(b) = old.blocks.get(r.cursor) else {
            return false;
        };
        if b.id != leaf.id || b.range != old_range {
            return false;
        }
        if let Some(lookups) = &b.lookups {
            let refs = self.refs.get_or_init(|| self.tree.ref_map(self.text));
            let same_map = old.ref_map.as_ref().is_some_and(|m| Arc::ptr_eq(m, refs));
            if !same_map
                && lookups
                    .iter()
                    .any(|(label, def)| refs.lookup(label) != def.as_ref())
            {
                return false;
            }
        }
        if let Some(r) = self.reuse.as_mut() {
            r.cursor += 1;
        }
        let (h0, l0, d0) = self.counts();
        let span = |r: &Range<u32>| r.start as usize..r.end as usize;
        self.headings
            .extend(old.headings[span(&b.headings)].iter().map(|h| Heading {
                range: shift(&h.range, delta),
                content_range: shift(&h.content_range, delta),
                line: shift_line(h.line, line_delta),
                end_line: shift_line(h.end_line, line_delta),
                ..h.clone()
            }));
        self.links
            .extend(old.links[span(&b.links)].iter().map(|l| Link {
                range: shift(&l.range, delta),
                text_range: shift(&l.text_range, delta),
                destination_range: l.destination_range.as_ref().map(|r| shift(r, delta)),
                label_range: l.label_range.as_ref().map(|r| shift(r, delta)),
                ..l.clone()
            }));
        self.definitions.extend(
            old.definitions[span(&b.definitions)]
                .iter()
                .map(|d| Definition {
                    label_range: shift(&d.label_range, delta),
                    destination_range: shift(&d.destination_range, delta),
                    title_range: d.title_range.as_ref().map(|r| shift(r, delta)),
                    range: shift(&d.range, delta),
                    line: shift_line(d.line, line_delta),
                    ..d.clone()
                }),
        );
        let (h1, l1, d1) = self.counts();
        self.blocks.push(BlockFacts {
            id: leaf.id,
            range,
            headings: h0..h1,
            links: l0..l1,
            definitions: d0..d1,
            lookups: b.lookups.clone(),
            parsed: b.parsed,
        });
        true
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
        let p = text::project(&tree);
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
        let text: Arc<str> = p.text.into();
        let text_with_alt = (p.with_alt != *text).then(|| p.with_alt.into());
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
        let text = self.text;
        let joined = Joined::new(leaf.lines.iter().map(|l| {
            let mut buf = Vec::with_capacity(l.content.len());
            text.copy_range(l.content.clone(), &mut buf);
            (l.content.clone(), buf)
        }));
        let Some(d) = locate(&joined) else {
            return;
        };
        let start = leaf
            .lines
            .first()
            .map_or(leaf.range.start, |l| l.content.start);
        let end = leaf.lines.last().map_or(start, |l| l.content.end);
        self.definitions.push(Definition {
            label: d.label.into(),
            label_range: d.label_range,
            destination: d.def.destination.into(),
            destination_range: d.destination_range,
            title: d.def.title.map(Arc::from),
            title_range: d.title_range,
            range: start..end,
            line: to_u32(leaf.start_line),
            first: false,
            block: leaf.id,
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
        if !matches!(
            leaf.kind,
            LeafKind::Heading { .. } | LeafKind::Paragraph | LeafKind::LinkRefDef
        ) {
            return;
        }
        if self.skip == Some(leaf.id) {
            return;
        }
        if self.open.as_ref().map(|b| b.id) != Some(leaf.id) {
            self.flush();
            self.skip = None;
            if self.try_reuse(leaf) {
                self.skip = Some(leaf.id);
                self.stats.reused += 1;
                return;
            }
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

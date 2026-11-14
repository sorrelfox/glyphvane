// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`DocModel`] and the records it holds.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gv_syntax::inline::RefDef;
use gv_syntax::{BlockId, FrontMatterKind, RefMap};
use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

use crate::outline::OutlineEntry;
use crate::slug::{Projection, SlugAlgorithm, Slugger, obsidian};

/// One heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    /// 1 to 6.
    pub level: u8,
    /// A setext heading (underlined) rather than an ATX one (`#`).
    pub setext: bool,
    /// From the start of the heading (the line with the `#`s, or the first
    /// text line) to the end of its block (line ending or underline
    /// included).
    pub range: Range<usize>,
    /// From the first to the last content byte (empty at the end of the
    /// opening `#`s for an empty heading).
    pub content_range: Range<usize>,
    /// 0-based first line.
    pub line: u32,
    /// 0-based last line (the underline of a setext heading).
    pub end_line: u32,
    /// Number of containers (block quotes, list items) around it.
    pub container_depth: u32,
    /// The rendered text ([`Projection::Text`]).
    pub text: Arc<str>,
    /// The Markdown source of the content, lines joined with `\n`
    /// ([`Projection::Source`]); Obsidian's heading text.
    pub source: Arc<str>,
    /// [`Projection::TextWithAlt`] when it differs from `text`.
    pub(crate) text_with_alt: Option<Arc<str>>,
    /// An explicit identifier (`{#id}` in later profiles); never set by the
    /// CommonMark profile.
    pub explicit_id: Option<Arc<str>>,
    /// The block it comes from.
    pub block: BlockId,
}

impl Heading {
    /// The heading text in `projection`.
    pub fn projection(&self, projection: Projection) -> &str {
        match projection {
            Projection::Text => &self.text,
            Projection::TextWithAlt => self.text_with_alt.as_deref().unwrap_or(&self.text),
            Projection::Source => &self.source,
            Projection::SourceLastLine => self.source.rsplit('\n').next().unwrap_or(""),
        }
    }
}

/// How a link got its destination. More kinds (wikilinks, embeds, extended
/// autolinks, HTML attributes) come with later profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinkKind {
    /// `[text](destination "title")`.
    Inline,
    /// `[text][label]`.
    Full,
    /// `[label][]`.
    Collapsed,
    /// `[label]`.
    Shortcut,
    /// `<scheme:address>`.
    UriAutolink,
    /// `<user@example.com>`.
    EmailAutolink,
}

impl LinkKind {
    /// The destination comes from a link reference definition.
    pub fn is_reference(self) -> bool {
        matches!(self, Self::Full | Self::Collapsed | Self::Shortcut)
    }

    /// An autolink (`<…>`).
    pub fn is_autolink(self) -> bool {
        matches!(self, Self::UriAutolink | Self::EmailAutolink)
    }
}

/// A link or image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// How the destination was given.
    pub kind: LinkKind,
    /// An image (`![…](…)`) rather than a link.
    pub image: bool,
    /// The whole construct.
    pub range: Range<usize>,
    /// The link text or image description (autolinks: the address).
    pub text_range: Range<usize>,
    /// The destination as the resolver takes it: backslash escapes and
    /// entities decoded, angle brackets removed (reference links: the
    /// definition's). Email autolinks get `mailto:`.
    pub destination: Arc<str>,
    /// The destination as written in this link, `<` and `>` included
    /// (inline links and autolinks; reference links have theirs in the
    /// [`Definition`]).
    pub destination_range: Option<Range<usize>>,
    /// The title, decoded.
    pub title: Option<Arc<str>>,
    /// The normalized label (reference links).
    pub label: Option<Arc<str>>,
    /// The label as written (reference links).
    pub label_range: Option<Range<usize>>,
    /// Inside a heading.
    pub in_heading: bool,
    /// The block it is in.
    pub block: BlockId,
}

/// A link reference definition (`[label]: destination "title"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The normalized label.
    pub label: Arc<str>,
    /// The label as written, between the brackets.
    pub label_range: Range<usize>,
    /// The destination, decoded.
    pub destination: Arc<str>,
    /// The destination as written (with `<` and `>` if present).
    pub destination_range: Range<usize>,
    /// The title, decoded.
    pub title: Option<Arc<str>>,
    /// The title as written, with its delimiters.
    pub title_range: Option<Range<usize>>,
    /// From the `[` to the end of the definition's last line.
    pub range: Range<usize>,
    /// 0-based first line.
    pub line: u32,
    /// Whether this is the label's first definition (the one that counts).
    pub first: bool,
    /// The block it comes from.
    pub block: BlockId,
}

/// A block identifier (`^id`, Obsidian). The CommonMark profile has none;
/// the Obsidian profile fills them in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAnchor {
    /// The id as written, without `^`.
    pub id: Arc<str>,
    /// The block it identifies.
    pub range: Range<usize>,
    /// 0-based line of the block.
    pub line: u32,
    /// The block.
    pub block: BlockId,
}

/// Front matter: the metadata block at the start of the document (YAML or
/// TOML, as the parse options of the tree allow; see
/// [`gv_syntax::ParseOptions`]). Its content is not Markdown: no headings,
/// links or definitions come from it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FrontMatter {
    /// The whole block, both delimiter lines included.
    pub range: Range<usize>,
    /// The content between the delimiter lines, line endings included
    /// (empty, at the closing delimiter, when there is none).
    pub content_range: Range<usize>,
    /// Its syntax.
    pub kind: FrontMatterKind,
}

/// What a build or update did, for tests and profiling.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildStats {
    /// Leaf blocks visited (an update walks only the reparsed window and
    /// the blocks it rebuilds).
    pub leaves: usize,
    /// Blocks whose records were taken over from the previous model
    /// (blocks with records: headings, links, definitions, or parsed
    /// inlines).
    pub reused: usize,
    /// Inline parses run.
    pub inline_parses: usize,
}

/// Normalized labels a block's inlines looked up, with the definitions
/// they resolved to (`None`: no definition, the brackets stayed text).
pub(crate) type Lookups = Arc<[(Box<str>, Option<RefDef>)]>;

/// Per-block bookkeeping for incremental updates: where a block's records
/// are, and the reference labels its inlines looked up.
#[derive(Debug, Clone)]
pub(crate) struct BlockFacts {
    pub id: BlockId,
    /// The block's range (the whole text block for its parts).
    pub range: Range<usize>,
    pub headings: Range<u32>,
    pub links: Range<u32>,
    pub definitions: Range<u32>,
    /// Labels looked up while parsing the block's inlines, with what they
    /// resolved to.
    pub lookups: Option<Lookups>,
    /// The block's inlines were parsed (kept even when they held nothing,
    /// so that an update does not parse them again).
    pub parsed: bool,
}

/// Two comparison keys of one heading.
pub(crate) type KeyPair = (Box<str>, Box<str>);

pub(crate) struct ModelData {
    pub len: usize,
    pub line_count: usize,
    pub headings: Vec<Heading>,
    pub outline: Vec<OutlineEntry>,
    pub links: Vec<Link>,
    pub definitions: Vec<Definition>,
    pub block_anchors: Vec<BlockAnchor>,
    pub front_matter: Option<FrontMatter>,
    pub blocks: Vec<BlockFacts>,
    pub ref_map: Option<Arc<RefMap>>,
    pub stats: BuildStats,
    /// Slugs per algorithm of [`SlugAlgorithm::ALL`], computed on first use.
    pub slugs: [OnceLock<Vec<Box<str>>>; SlugAlgorithm::ALL.len()],
    /// Per algorithm of [`SlugAlgorithm::ALL`]: each slug in NFC, and
    /// lowercased, for fragment matching; on first use.
    pub slug_keys: [OnceLock<Vec<KeyPair>>; SlugAlgorithm::ALL.len()],
    /// NFC Obsidian keys of each heading's source and text, on first use.
    pub keys: OnceLock<Vec<KeyPair>>,
}

/// The semantics of one document revision: headings and outline, links,
/// link reference definitions, slugs. Immutable; cloning is O(1).
///
/// Built from a [`gv_syntax::SyntaxTree`] and its text with
/// [`DocModel::build`], or from the previous revision's model with
/// [`DocModel::update`], which takes over the records of blocks the
/// reparse did not touch.
#[derive(Clone)]
pub struct DocModel(pub(crate) Arc<ModelData>);

impl std::fmt::Debug for DocModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocModel")
            .field("len", &self.0.len)
            .field("headings", &self.0.headings.len())
            .field("links", &self.0.links.len())
            .field("definitions", &self.0.definitions.len())
            .finish_non_exhaustive()
    }
}

impl DocModel {
    /// Length of the text in bytes.
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Whether the text was empty.
    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// Number of lines of the syntax tree it was built from.
    pub fn line_count(&self) -> usize {
        self.0.line_count
    }

    /// The headings in document order.
    pub fn headings(&self) -> &[Heading] {
        &self.0.headings
    }

    /// The outline: one entry per heading, same indices.
    pub fn outline(&self) -> &[OutlineEntry] {
        &self.0.outline
    }

    /// The direct children of heading `index` in the outline (or the
    /// top-level headings for `None`).
    pub fn outline_children(&self, index: Option<usize>) -> impl Iterator<Item = usize> + '_ {
        let range = match index {
            Some(i) => i + 1..self.0.outline.get(i).map_or(i + 1, |e| e.section_end),
            None => 0..self.0.outline.len(),
        };
        range.filter(move |&j| self.0.outline[j].parent == index)
    }

    /// The byte range of heading `index`'s section: from the heading to the
    /// next heading of the same or a higher level, or the end of the text.
    pub fn section(&self, index: usize) -> Option<Range<usize>> {
        let h = self.0.headings.get(index)?;
        let end = self.0.outline[index].section_end;
        let end = self
            .0
            .headings
            .get(end)
            .map_or(self.0.len, |next| next.range.start);
        Some(h.range.start..end)
    }

    /// The innermost heading whose section contains `offset`.
    pub fn heading_at(&self, offset: usize) -> Option<usize> {
        let k = self.0.headings.partition_point(|h| h.range.start <= offset);
        k.checked_sub(1)
    }

    /// Links and images in document order (by start; an image inside a
    /// link comes after it).
    pub fn links(&self) -> &[Link] {
        &self.0.links
    }

    /// The innermost link or image at `offset` (start inclusive, end
    /// exclusive).
    pub fn link_at(&self, offset: usize) -> Option<&Link> {
        let links = &self.0.links;
        let k = links.partition_point(|l| l.range.start <= offset);
        let last = links[..k].last()?;
        // A link containing `offset` contains every later link that starts
        // before `offset`, so all of them are in its block.
        links[..k]
            .iter()
            .rev()
            .take_while(|l| l.block == last.block)
            .find(|l| l.range.contains(&offset))
    }

    /// Link reference definitions in document order.
    pub fn definitions(&self) -> &[Definition] {
        &self.0.definitions
    }

    /// Block identifiers in document order (none in the CommonMark
    /// profile).
    pub fn block_anchors(&self) -> &[BlockAnchor] {
        &self.0.block_anchors
    }

    /// The front matter, if the document starts with one (only trees
    /// parsed with front matter enabled have it).
    pub fn front_matter(&self) -> Option<FrontMatter> {
        self.0.front_matter.clone()
    }

    /// What the build or update that made this model did.
    pub fn stats(&self) -> BuildStats {
        self.0.stats
    }

    /// The unique slug of every heading under `algorithm`, in heading order.
    /// Headings with an explicit id keep it. Cached per model for the
    /// algorithms in [`SlugAlgorithm::ALL`].
    pub fn slugs(&self, algorithm: SlugAlgorithm) -> Cow<'_, [Box<str>]> {
        match algorithm.index() {
            Some(i) => Cow::Borrowed(self.0.slugs[i].get_or_init(|| self.compute_slugs(algorithm))),
            None => Cow::Owned(self.compute_slugs(algorithm)),
        }
    }

    fn compute_slugs(&self, algorithm: SlugAlgorithm) -> Vec<Box<str>> {
        let mut slugger = Slugger::new(algorithm);
        // Python-Markdown collects every id in the document before
        // assigning any.
        if matches!(algorithm, SlugAlgorithm::PythonMarkdown { .. }) {
            for h in &self.0.headings {
                if let Some(id) = &h.explicit_id {
                    slugger.reserve(id);
                }
            }
        }
        let projection = algorithm.projection();
        self.0
            .headings
            .iter()
            .map(|h| match &h.explicit_id {
                Some(id) => {
                    slugger.reserve(id);
                    Box::from(&**id)
                }
                None => slugger.slug(h.projection(projection)).into_boxed_str(),
            })
            .collect()
    }

    /// Every slug under `algorithm` in NFC, and lowercased.
    pub(crate) fn slug_keys(&self, algorithm: SlugAlgorithm) -> Cow<'_, [KeyPair]> {
        let compute = || {
            self.slugs(algorithm)
                .iter()
                .map(|s| {
                    let n = nfc(s);
                    let lower = n.to_lowercase().into_boxed_str();
                    (Box::from(n.as_ref()), lower)
                })
                .collect::<Vec<_>>()
        };
        match algorithm.index() {
            Some(i) => Cow::Borrowed(self.0.slug_keys[i].get_or_init(compute)),
            None => Cow::Owned(compute()),
        }
    }

    /// The NFC Obsidian keys (source, text) of every heading.
    pub(crate) fn heading_keys(&self) -> &[KeyPair] {
        self.0.keys.get_or_init(|| {
            self.0
                .headings
                .iter()
                .map(|h| {
                    (
                        nfc(&obsidian::heading_key(&h.source)).into(),
                        nfc(&obsidian::heading_key(&h.text)).into(),
                    )
                })
                .collect()
        })
    }
}

/// `s` in NFC (borrowed when it already is).
pub(crate) fn nfc(s: &str) -> Cow<'_, str> {
    match is_nfc_quick(s.chars()) {
        IsNormalized::Yes => Cow::Borrowed(s),
        _ => Cow::Owned(s.nfc().collect()),
    }
}

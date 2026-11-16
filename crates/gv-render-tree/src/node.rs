// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Render nodes: one per rendered leaf block, keyed by its [`BlockId`],
//! with the containers around it.
//!
//! # Relative offsets
//!
//! A node's [`source`](BlockNode::source) is absolute (bytes of the current
//! revision). Everything *inside* a node is relative to
//! `source.start`: the source ranges of its [`Paragraph`], its links,
//! images and code lines. An edit above a block then moves only the
//! node's start; its paragraph is reused as is. Use
//! [`BlockNode::to_document`] and [`BlockNode::to_local`] to convert.

use std::ops::Range;
use std::sync::Arc;

use gv_inline::Paragraph;
use gv_model::LinkKind;
use gv_syntax::inline::RefDef;
use gv_syntax::{BlockId, FrontMatterKind};

use crate::status::{ImageSize, LinkStatus};

/// A container around a block. The decorations of a virtual item come
/// from its frames, outermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The container's block.
    pub id: BlockId,
    pub kind: FrameKind,
}

/// What a container is, with its resolved properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameKind {
    /// A block quote: a bar on the left and an indent.
    Quote {
        /// Number of block quotes around it (0 for an outermost quote).
        depth: u16,
    },
    /// A list (draws nothing itself; its items do).
    List {
        /// An ordered list.
        ordered: bool,
        /// Tight lists have no paragraph spacing between items.
        tight: bool,
        /// Bullet character (`-`, `+`, `*`) or ordered delimiter (`.`,
        /// `)`).
        marker: u8,
        /// Start number of an ordered list.
        start: u32,
        /// Number of lists around it (0 for a top-level list).
        depth: u16,
    },
    /// A list item: an indent, and the marker on its first line.
    Item { marker: ListMarker },
}

/// The marker of a list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListMarker {
    /// A bullet; the glyph cycles with the nesting depth of bullet lists
    /// (•, ◦, ▪), as browsers draw them.
    Bullet {
        /// Number of lists around this item's list.
        depth: u16,
    },
    /// An ordered item: its number and the delimiter (`.` or `)`).
    Ordered { number: u64, delimiter: u8 },
    /// A task item (GFM and later profiles; never made by the CommonMark
    /// profile): a checkbox, and the status character for custom
    /// statuses.
    Task { checked: bool, status: char },
}

impl ListMarker {
    /// The marker as text (for drawing and for copying): `•`, `◦`, `▪`,
    /// `3.`, `[x]`.
    pub fn text(&self) -> String {
        match *self {
            Self::Bullet { depth } => match depth % 3 {
                0 => "\u{2022}",
                1 => "\u{25e6}",
                _ => "\u{25aa}",
            }
            .to_owned(),
            Self::Ordered { number, delimiter } => {
                format!("{number}{}", char::from(delimiter))
            }
            Self::Task { status, .. } => format!("[{status}]"),
        }
    }
}

/// The containers of a block, outermost first; shared by the blocks that
/// have the same ancestors.
pub type Context = Arc<[Frame]>;

/// Normalized labels a block's inlines looked up, with the definitions they
/// resolved to (for incremental updates).
pub(crate) type Lookups = Arc<[(Box<str>, Option<RefDef>)]>;

/// One rendered block.
#[derive(Debug, Clone)]
pub struct BlockNode {
    /// The leaf block (a list item's id for an empty item, the quote's for
    /// an empty quote).
    pub id: BlockId,
    pub kind: NodeKind,
    /// The bytes this node renders (absolute): from its first content line
    /// (an ATX heading or a fence: the block's start) to the block's end.
    pub source: Range<usize>,
    /// The containers around it.
    pub context: Context,
    /// The whole leaf block (absolute; a text block with definitions
    /// before the paragraph starts at the first definition).
    pub(crate) block: Range<usize>,
    /// The top-level block it belongs to (absolute).
    pub(crate) top: Range<usize>,
    /// Reference labels its inlines looked up.
    pub(crate) lookups: Option<Lookups>,
}

impl BlockNode {
    /// The document offset of a node-relative offset.
    pub fn to_document(&self, local: usize) -> usize {
        self.source.start + local
    }

    /// The node-relative offset of a document offset (clamped to the
    /// node's start).
    pub fn to_local(&self, offset: usize) -> usize {
        offset.saturating_sub(self.source.start)
    }

    /// The inline content of a paragraph or heading.
    pub fn inline(&self) -> Option<&Arc<InlineContent>> {
        match &self.kind {
            NodeKind::Paragraph(c) | NodeKind::Heading { content: c, .. } => Some(c),
            _ => None,
        }
    }

    /// The lines of a code block, HTML block, math block or front matter.
    pub fn lines(&self) -> Option<&Arc<CodeBlock>> {
        match &self.kind {
            NodeKind::Code(c) | NodeKind::Html { code: c, .. } | NodeKind::Math(c) => Some(c),
            NodeKind::Properties { lines, .. } => Some(lines),
            _ => None,
        }
    }
}

/// What a node renders.
#[derive(Debug, Clone)]
pub enum NodeKind {
    /// A paragraph.
    Paragraph(Arc<InlineContent>),
    /// An ATX or setext heading.
    Heading {
        /// 1 to 6.
        level: u8,
        setext: bool,
        /// The anchor id under the profile's slug algorithm.
        anchor: Option<Arc<str>>,
        content: Arc<InlineContent>,
    },
    /// A fenced or indented code block.
    Code(Arc<CodeBlock>),
    /// Display math from a ` ```math ` fence (profiles with math): the
    /// fence's lines are the TeX source, joined in [`CodeBlock::text`].
    /// Copy and find see the source.
    Math(Arc<CodeBlock>),
    /// A thematic break.
    ThematicBreak,
    /// An HTML block. Raw HTML is rendered natively in a later milestone;
    /// until then its source is shown as code-like text.
    Html {
        /// The CommonMark start condition (1 to 7).
        kind: u8,
        /// Always true for now: the block waits for the HTML pipeline.
        raw_html_pending: bool,
        code: Arc<CodeBlock>,
    },
    /// Front matter, shown as a properties placeholder: its raw lines
    /// (typed properties come with the model's front matter).
    Properties {
        kind: FrontMatterKind,
        lines: Arc<CodeBlock>,
    },
    /// A list item with no content: only its marker.
    EmptyItem,
    /// A block quote with no content: only its bar.
    EmptyQuote,
}

impl NodeKind {
    /// A short name, for dumps.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Paragraph(_) => "paragraph",
            Self::Heading { .. } => "heading",
            Self::Code(_) => "code",
            Self::Math(_) => "math",
            Self::ThematicBreak => "thematic-break",
            Self::Html { .. } => "html",
            Self::Properties { .. } => "properties",
            Self::EmptyItem => "empty-item",
            Self::EmptyQuote => "empty-quote",
        }
    }
}

/// The inline content of a paragraph or heading.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineContent {
    /// The `gv-inline` paragraph. Its source offsets are relative to the
    /// node's start.
    pub paragraph: Arc<Paragraph>,
    /// One span per paragraph item (same index): what the item is.
    pub spans: Box<[InlineSpan]>,
    /// Links and autolinks, in order.
    pub links: Box<[LinkInfo]>,
    /// Images, in order.
    pub images: Box<[ImageInfo]>,
    /// The content holds raw inline HTML, shown as text until the HTML
    /// pipeline renders it.
    pub raw_html_pending: bool,
    /// Width units per line for height estimates.
    pub(crate) measure: Measure,
}

/// What a paragraph item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InlineSpan {
    pub role: InlineRole,
    /// The link it belongs to (index into [`InlineContent::links`]).
    pub link: Option<u32>,
}

/// Roles of paragraph items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InlineRole {
    /// Text (plain, emphasized, strong, link text).
    Text,
    /// Inline code.
    Code,
    /// A hard line break.
    Break,
    /// Raw inline HTML, shown as text.
    RawHtml,
    /// An image box (index into [`InlineContent::images`]); the box's id is
    /// [`BoxRef::Image`] of the same index.
    Image(u32),
    /// The alt text of an image that is not shown (size unknown or
    /// failed).
    ImageAlt(u32),
    /// The status decoration after a link: `≈` text or an icon box (the
    /// box's id is [`BoxRef::LinkSuffix`] of the link index). Not content:
    /// copy and find skip it.
    LinkSuffix,
}

/// What an inline box of a paragraph stands for, as encoded in its
/// `gv-inline` box id ([`gv_inline::InlineBox::id`]). Images and link
/// suffixes are numbered separately, so the id carries a tag: one
/// paragraph's boxes never share an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoxRef {
    /// Image `i` of [`InlineContent::images`].
    Image(u32),
    /// The status icon after link `i` of [`InlineContent::links`].
    LinkSuffix(u32),
}

impl BoxRef {
    /// The tag bit of link suffixes (indices stay below it).
    const LINK_SUFFIX: u32 = 1 << 31;

    /// The box id.
    pub const fn id(self) -> u32 {
        match self {
            Self::Image(i) => i & !Self::LINK_SUFFIX,
            Self::LinkSuffix(i) => i | Self::LINK_SUFFIX,
        }
    }

    /// What box id `id` stands for.
    pub const fn from_id(id: u32) -> Self {
        if id & Self::LINK_SUFFIX == 0 {
            Self::Image(id)
        } else {
            Self::LinkSuffix(id & !Self::LINK_SUFFIX)
        }
    }
}

impl InlineRole {
    /// Whether the item is document content (copied and searched).
    pub fn is_content(self) -> bool {
        !matches!(self, Self::LinkSuffix | Self::Image(_))
    }
}

/// A link or autolink of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub kind: LinkKind,
    /// As the resolver takes it (see [`crate::LinkTarget::destination`]).
    pub destination: Arc<str>,
    pub title: Option<Arc<str>>,
    pub status: LinkStatus,
    /// The whole construct, relative to the node's start.
    pub source: Range<usize>,
}

/// An image of a block.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageInfo {
    /// How the destination was given.
    pub kind: LinkKind,
    pub destination: Arc<str>,
    pub title: Option<Arc<str>>,
    /// The plain text of the description.
    pub alt: Arc<str>,
    pub size: ImageSize,
    /// The whole construct, relative to the node's start.
    pub source: Range<usize>,
    /// The link around it.
    pub link: Option<u32>,
}

/// The lines of a code block (also of HTML blocks and front matter).
#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    /// A fenced block (else indented).
    pub fenced: bool,
    /// The info string, decoded (fenced blocks).
    pub info: Option<Arc<str>>,
    /// The first word of the info string.
    pub language: Option<Arc<str>>,
    /// The lines joined with `\n` (partially consumed tabs expanded to
    /// spaces; line endings not included).
    pub text: String,
    pub lines: Box<[CodeLine]>,
}

/// One line of a [`CodeBlock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLine {
    /// The line in [`CodeBlock::text`].
    pub text: Range<usize>,
    /// Its content bytes, relative to the node's start; the first
    /// `indent` characters of the text are virtual spaces before them.
    pub source: Range<usize>,
    /// Where the document line starts (after the previous line's line
    /// ending; container prefixes are before `source.start`), relative to
    /// the node's start.
    pub start: usize,
    /// Virtual spaces (a tab partly consumed by the container prefix).
    pub indent: u8,
    /// Width units (characters; wide ones count twice, tabs four times).
    pub units: u32,
}

impl CodeBlock {
    /// The text of line `i`.
    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", |l| &self.text[l.text.clone()])
    }

    /// The node-relative source offset of `offset` in line `i`'s text.
    pub fn line_source(&self, i: usize, offset: usize) -> usize {
        let Some(l) = self.lines.get(i) else {
            return 0;
        };
        let indent = usize::from(l.indent);
        (l.source.start + offset.saturating_sub(indent)).min(l.source.end)
    }
}

/// Width units of each line of inline content, for estimates: font sizes
/// in px summed per character (wide characters twice), split by family,
/// plus fixed widths in px. A line's advance is
/// `text * text_advance_em + mono * mono_advance_em + px`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Units {
    pub text: f32,
    pub mono: f32,
    pub px: f32,
}

/// Units per forced line of inline content, and the extra height of boxes
/// taller than a line.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Measure {
    pub lines: Box<[Units]>,
    pub extra_height: f32,
}

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
use gv_syntax::table::Alignment;
use gv_syntax::{AlertKind, BlockId, CalloutFold, FrontMatterKind};

use crate::status::{ImageSize, LinkStatus};
use crate::style::ColorToken;

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
    /// A GitHub alert: a box tinted in its type's colour with a title row
    /// (icon and the type's name) above its blocks.
    Alert { kind: AlertKind },
    /// A footnote definition in the footnote section at the end of the
    /// document: an indent with its number on the first line.
    Footnote {
        /// Its number (order of first reference).
        number: u32,
        /// Its normalized label.
        label: Arc<str>,
    },
    /// The blocks of a `<details>` element, after its summary row
    /// ([`NodeKind::Details`], whose block id is the frame's id). Folded
    /// away while the details are closed. A foldable callout's blocks
    /// after its title ([`NodeKind::CalloutTitle`]) are in one too.
    Details,
    /// An Obsidian callout: a box tinted in its type's colour; its first
    /// block is its title ([`NodeKind::CalloutTitle`]).
    Callout {
        /// The type as written, in lower case (`faq`, or a custom type);
        /// [`CalloutType::of`] gives the built-in type that styles it.
        name: Arc<str>,
        /// Its fold sign.
        fold: CalloutFold,
    },
    /// A note embed (`![[Note#Heading]]`) shown in place: a bordered box
    /// holding the embed's own line (a link to the note) and the embedded
    /// blocks.
    Embed {
        /// Number of embeds around it (0 for an outermost embed).
        depth: u16,
    },
}

/// The built-in Obsidian callout types (aliases in parentheses), which
/// decide a callout's icon and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalloutType {
    Note,
    /// (summary, tldr)
    Abstract,
    Info,
    Todo,
    /// (hint, important)
    Tip,
    /// (check, done)
    Success,
    /// (help, faq)
    Question,
    /// (caution, attention)
    Warning,
    /// (fail, missing)
    Failure,
    /// (error)
    Danger,
    Bug,
    Example,
    /// (cite)
    Quote,
}

impl CalloutType {
    /// The type a callout named `name` gets (any case); unknown types are
    /// notes, as in Obsidian.
    pub fn of(name: &str) -> CalloutType {
        match name.to_ascii_lowercase().as_str() {
            "abstract" | "summary" | "tldr" => Self::Abstract,
            "info" => Self::Info,
            "todo" => Self::Todo,
            "tip" | "hint" | "important" => Self::Tip,
            "success" | "check" | "done" => Self::Success,
            "question" | "help" | "faq" => Self::Question,
            "warning" | "caution" | "attention" => Self::Warning,
            "failure" | "fail" | "missing" => Self::Failure,
            "danger" | "error" => Self::Danger,
            "bug" => Self::Bug,
            "example" => Self::Example,
            "quote" | "cite" => Self::Quote,
            _ => Self::Note,
        }
    }

    /// The callout's tint (its box) and colour (its title and icon). The
    /// design gives notes `status.info` and tips `status.success`; the
    /// other types follow Obsidian's hues with the nearest tokens.
    pub fn colors(self) -> (ColorToken, ColorToken) {
        use ColorToken as T;
        match self {
            Self::Note | Self::Info | Self::Todo => (T::StatusInfoTint, T::StatusInfo),
            Self::Abstract | Self::Tip => (T::StatusSuccessTint, T::StatusSuccess),
            Self::Success => (T::StatusSuccessTint, T::StatusSuccess),
            Self::Question | Self::Warning => (T::StatusWarningTint, T::StatusWarning),
            Self::Failure | Self::Danger | Self::Bug => (T::StatusErrorTint, T::StatusError),
            Self::Example => (T::AccentTint, T::SyntaxTag),
            Self::Quote => (T::SurfaceSunken, T::TextSecondary),
        }
    }

    /// Whether `name` is a built-in type or alias.
    pub fn is_known(name: &str) -> bool {
        Self::of(name) != Self::Note || name.eq_ignore_ascii_case("note")
    }
}

/// The title a callout without one shows: its type as written, in title
/// case (`Faq`, `My-type`).
pub fn default_callout_title(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
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
    /// statuses. The checkbox's place in the source comes from the
    /// item's container ([`gv_syntax::Container::marker`]).
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
    /// A GFM table (it shares its block id with the paragraph before it
    /// in the same text block).
    pub(crate) table: bool,
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
            NodeKind::Paragraph(c)
            | NodeKind::Heading { content: c, .. }
            | NodeKind::Details { summary: c, .. }
            | NodeKind::CalloutTitle { title: c, .. } => Some(c),
            _ => None,
        }
    }

    /// The inline content an item of this node lays out: a table row's,
    /// or the paragraph's or heading's.
    pub fn item_inline(&self, kind: &crate::ItemKind) -> Option<&Arc<InlineContent>> {
        match (&self.kind, kind) {
            (NodeKind::Table(t), crate::ItemKind::TableRow { row, .. }) => {
                t.rows.get(*row as usize).map(|r| &r.content)
            }
            _ => self.inline(),
        }
    }

    /// Every inline content of the node: a paragraph's or heading's, or
    /// each table row's.
    pub fn inlines(&self) -> impl Iterator<Item = &Arc<InlineContent>> + '_ {
        let rows = match &self.kind {
            NodeKind::Table(t) => &t.rows[..],
            _ => &[],
        };
        self.inline()
            .into_iter()
            .chain(rows.iter().map(|r| &r.content))
    }

    /// The lines of a code block, HTML block, math block or front matter.
    pub fn lines(&self) -> Option<&Arc<CodeBlock>> {
        match &self.kind {
            NodeKind::Code(c)
            | NodeKind::Html { code: c, .. }
            | NodeKind::Math(c)
            | NodeKind::Diagram(c) => Some(c),
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
    /// Display math from a ` ```math ` fence or a `$$` block (profiles
    /// with math): the TeX source lines, joined in [`CodeBlock::text`].
    /// Copy and find see the source.
    Math(Arc<CodeBlock>),
    /// A ` ```mermaid ` fence (profiles with the `mermaid` switch): the
    /// diagram source lines, joined in [`CodeBlock::text`]. The view draws
    /// the diagram, or a fallback card above the source; copy and find see
    /// the source.
    Diagram(Arc<CodeBlock>),
    /// A GFM table: one virtual item per row (the delimiter row is part of
    /// the header row's source).
    Table(Arc<TableBlock>),
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
        /// The properties read from the lines.
        props: Arc<[crate::properties::Property]>,
    },
    /// A list item with no content: only its marker.
    EmptyItem,
    /// A block quote with no content: only its bar.
    EmptyQuote,
    /// The summary row of a `<details>` element (an HTML block that opens
    /// one): a disclosure triangle and the summary's text. The blocks up
    /// to the closing `</details>` are in [`FrameKind::Details`] frames
    /// with this node's id.
    Details {
        /// The summary as plain text (`Details` without one, as browsers
        /// show it).
        summary: Arc<InlineContent>,
        /// Open by default (`<details open>`).
        open: bool,
    },
    /// A `<picture>` element (an HTML block): its sources and fallback
    /// image. The view picks a source by the app theme
    /// ([`Picture::pick`]).
    Picture(Arc<Picture>),
    /// The title row of an Obsidian callout (the first block of its
    /// [`FrameKind::Callout`] frame): its title, or the type's name in
    /// title case. A foldable callout's other blocks are in a
    /// [`FrameKind::Details`] frame with this node's id.
    CalloutTitle {
        title: Arc<InlineContent>,
        fold: CalloutFold,
    },
    /// A live block (a canvas or base embed, a ` ```base ` or ` ```query `
    /// fence): the view fills it in ([`crate::live`]).
    Live(Arc<crate::live::LiveBlock>),
}

/// A `<source>` of a `<picture>`: its media query and the first URL of
/// its source set.
pub type PictureSource = (Option<Arc<str>>, Arc<str>);

/// The sources of a `<picture>` element.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    /// `<source media srcset>` in order: the media query and the first
    /// URL of the source set.
    pub sources: Box<[PictureSource]>,
    /// The `<img>` fallback's `src`.
    pub fallback: Option<Arc<str>>,
    /// The `<img>`'s `alt`.
    pub alt: Arc<str>,
    /// Inside a note embed: the embedded note's key, which the sources
    /// resolve from ([`crate::image_key`]).
    pub from: Option<Arc<str>>,
    /// The size of the image picked in a light and a dark scheme (asked of
    /// the [`ImageSizeSource`](crate::ImageSizeSource) when the block is
    /// built).
    pub sizes: [ImageSize; 2],
}

impl Picture {
    /// The image a browser shows when the colour scheme is dark (`dark`)
    /// or light: the first source whose media query is empty or holds
    /// `prefers-color-scheme` with the scheme asked for, else the
    /// fallback.
    pub fn pick(&self, dark: bool) -> Option<&str> {
        let want = if dark { "dark" } else { "light" };
        self.sources
            .iter()
            .find(|(media, _)| match media {
                None => true,
                Some(m) => {
                    let m = m.to_ascii_lowercase();
                    !m.contains("prefers-color-scheme") || m.contains(want)
                }
            })
            .map(|(_, url)| &**url)
            .or(self.fallback.as_deref())
    }

    /// The size of the image picked for the scheme (see [`Picture::pick`]).
    pub fn size(&self, dark: bool) -> ImageSize {
        self.sizes[usize::from(dark)]
    }
}

impl NodeKind {
    /// A short name, for dumps.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Paragraph(_) => "paragraph",
            Self::Heading { .. } => "heading",
            Self::Code(_) => "code",
            Self::Math(_) => "math",
            Self::Diagram(_) => "diagram",
            Self::Table(_) => "table",
            Self::ThematicBreak => "thematic-break",
            Self::Html { .. } => "html",
            Self::Properties { .. } => "properties",
            Self::EmptyItem => "empty-item",
            Self::EmptyQuote => "empty-quote",
            Self::Details { .. } => "details",
            Self::Picture(_) => "picture",
            Self::CalloutTitle { .. } => "callout-title",
            Self::Live(_) => "live",
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
    /// Inline and display math, in order.
    pub math: Box<[MathInfo]>,
    /// Footnote references, in order.
    pub footnotes: Box<[FootnoteRefInfo]>,
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
    /// A formula box (index into [`InlineContent::math`]); the box's id is
    /// [`BoxRef::Math`] of the same index. Copy and find see its TeX.
    Math(u32),
    /// The space before table column `col` of a row (`col` = the column
    /// count: after the last cell); the box's id is [`BoxRef::Cell`]. Copy
    /// and find see a tab between two cells, nothing at the row's ends.
    CellGap(u32),
    /// The number of footnote reference `i` of
    /// [`InlineContent::footnotes`], raised: a click jumps to the
    /// footnote.
    FootnoteRef(u32),
    /// The `↩` after a footnote's text that leads back to its reference
    /// number `k` (0-based, in document order). Not content.
    FootnoteBackref(u32),
    /// An Obsidian `#tag`, drawn as a pill; its click target is a link of
    /// kind [`LinkKind::Tag`](gv_model::LinkKind::Tag).
    Tag,
}

/// A footnote reference of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootnoteRefInfo {
    /// The normalized label.
    pub label: Arc<str>,
    /// The footnote's number.
    pub number: u32,
    /// The `[^label]`, relative to the node's start.
    pub source: Range<usize>,
    /// A reference inside a note embed: numbered within the embed, whose
    /// footnotes are listed at the embed's end (not in the document's
    /// footnote section).
    pub embedded: bool,
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
    /// Formula `i` of [`InlineContent::math`].
    Math(u32),
    /// The gap before table column `i` of a row.
    Cell(u32),
}

impl BoxRef {
    /// The tag bits (indices stay below them).
    const TAGS: u32 = 3 << 30;
    const LINK_SUFFIX: u32 = 1 << 31;
    const MATH: u32 = 1 << 30;
    const CELL: u32 = 3 << 30;

    /// The box id.
    pub const fn id(self) -> u32 {
        match self {
            Self::Image(i) => i & !Self::TAGS,
            Self::LinkSuffix(i) => (i & !Self::TAGS) | Self::LINK_SUFFIX,
            Self::Math(i) => (i & !Self::TAGS) | Self::MATH,
            Self::Cell(i) => (i & !Self::TAGS) | Self::CELL,
        }
    }

    /// What box id `id` stands for.
    pub const fn from_id(id: u32) -> Self {
        let i = id & !Self::TAGS;
        match id & Self::TAGS {
            0 => Self::Image(i),
            Self::LINK_SUFFIX => Self::LinkSuffix(i),
            Self::MATH => Self::Math(i),
            _ => Self::Cell(i),
        }
    }
}

impl InlineRole {
    /// Whether the item is document content (copied and searched).
    pub fn is_content(self) -> bool {
        !matches!(
            self,
            Self::LinkSuffix | Self::Image(_) | Self::FootnoteBackref(_)
        )
    }
}

/// A formula of a paragraph, heading or table cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathInfo {
    /// The TeX between the delimiters.
    pub tex: Arc<str>,
    /// `$$…$$` or `\[…\]` inside a paragraph: display style.
    pub display: bool,
    /// The whole construct, relative to the node's start.
    pub source: Range<usize>,
}

/// A GFM table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableBlock {
    /// The column alignments, from the delimiter row.
    pub aligns: Box<[Alignment]>,
    /// The header row, then the body rows.
    pub rows: Box<[TableRow]>,
}

/// One row of a [`TableBlock`]: its cells' inline content in one
/// paragraph, each cell after a [`BoxRef::Cell`] gap box and a last gap
/// box after the last cell. The gaps are 0 wide: the view sets them from
/// the column widths it measures.
#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    /// The header row.
    pub header: bool,
    pub content: Arc<InlineContent>,
    /// The row's line, relative to the node's start (the header row's
    /// also covers the delimiter row).
    pub source: Range<usize>,
}

/// A link or autolink of a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub kind: LinkKind,
    /// As the resolver takes it (see [`crate::LinkTarget::destination`]).
    pub destination: Arc<str>,
    pub title: Option<Arc<str>>,
    pub status: LinkStatus,
    /// The whole construct, relative to the node's start (inside a note
    /// embed, empty at the embed's end).
    pub source: Range<usize>,
    /// Inside a note embed: the embedded note's key
    /// ([`crate::EmbedFetch::Note::key`]), which the link resolves from.
    /// `None`: a link of the document.
    pub from: Option<Arc<str>>,
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
    /// Inside a note embed: the embedded note's key, which the image
    /// resolves from ([`crate::image_key`]). `None`: the document's.
    pub from: Option<Arc<str>>,
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

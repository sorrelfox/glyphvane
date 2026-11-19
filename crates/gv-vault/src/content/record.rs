// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the content index stores per note.

use std::sync::Arc;

use crate::link::LinkKind;
use crate::path::VaultPath;

/// A byte range in a note's text. `u32` offsets: notes larger than
/// [`super::ContentOptions::max_file_bytes`] are never parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Span {
    /// First byte.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
}

impl Span {
    /// The span of `range`, clamped to `u32`.
    #[must_use]
    pub fn of(range: std::ops::Range<usize>) -> Self {
        Self {
            start: u32::try_from(range.start).unwrap_or(u32::MAX),
            end: u32::try_from(range.end).unwrap_or(u32::MAX),
        }
    }

    /// As a `usize` range.
    #[must_use]
    pub fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }

    /// Whether `offset` is inside.
    #[must_use]
    pub fn contains(self, offset: usize) -> bool {
        (self.start as usize..self.end as usize).contains(&offset)
    }
}

/// How a record was validated against the file on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileStamp {
    /// Size in bytes.
    pub size: u64,
    /// Modification time in nanoseconds since the Unix epoch, if known.
    pub mtime_ns: Option<i64>,
    /// CRC-32 of the bytes (0 for placeholders and overlays).
    pub crc: u32,
    /// The modification time was too close to the time the file was read
    /// to trust it (a write in the same clock tick would keep size and
    /// time): the file is read again and compared by CRC on the next
    /// update ("hash on doubt").
    pub doubt: bool,
}

/// What kind of record this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// Parsed from the file on disk.
    Indexed,
    /// An online-only cloud placeholder: indexed by path only, never read
    /// (reading would download it).
    Placeholder,
    /// Larger than the size limit: indexed by path only.
    TooLarge,
    /// Could not be read (the error is logged): indexed by path only.
    Unreadable,
    /// Parsed from an unsaved editor buffer (the dirty-buffer overlay).
    Overlay,
}

/// One heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedHeading {
    /// 1 to 6.
    pub level: u8,
    /// The rendered text.
    pub text: Arc<str>,
    /// The Markdown source of the content (Obsidian's heading text).
    pub source: Arc<str>,
    /// The heading block.
    pub span: Span,
    /// 0-based line.
    pub line: u32,
}

/// One link or embed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedLink {
    /// The resolver's link kind.
    pub kind: LinkKind,
    /// The destination as the resolver takes it ([`crate::LinkRef::raw_destination`]).
    pub raw: Arc<str>,
    /// The whole construct.
    pub span: Span,
    /// 0-based line.
    pub line: u32,
    /// In a front matter property value.
    pub front_matter: bool,
}

impl IndexedLink {
    /// Embeds and images load their target into the note.
    #[must_use]
    pub fn is_embed(&self) -> bool {
        self.kind.loads_content()
    }
}

/// One tag occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedTag {
    /// The tag as written, without `#` (`project/alpha`).
    pub name: Arc<str>,
    /// Where it is (`None` for front matter `tags`).
    pub span: Option<Span>,
    /// 0-based line (of the front matter for front matter tags).
    pub line: u32,
}

/// One task list item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedTask {
    /// The character between the brackets (`' '`, `'x'`, `'/'`, ...).
    pub status: char,
    /// The text of its first line.
    pub text: Arc<str>,
    /// The list item.
    pub span: Span,
    /// 0-based line.
    pub line: u32,
}

/// One block identifier (`^id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedBlockId {
    /// The id, without `^`.
    pub id: Arc<str>,
    /// The block it names.
    pub span: Span,
    /// 0-based line of that block.
    pub line: u32,
}

/// A front matter value, as written (types are applied per property name,
/// vault-wide, by the properties view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    /// Empty or `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, as written.
    Number(Arc<str>),
    /// A string.
    Text(Arc<str>),
    /// A list of strings.
    List(Vec<Arc<str>>),
    /// Anything else (nested maps, lists of maps), as written.
    Other(Arc<str>),
}

/// One front matter property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// The key.
    pub key: Arc<str>,
    /// The value.
    pub value: PropertyValue,
}

/// Where a note's title comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TitleSource {
    /// The front matter `title`.
    FrontMatter,
    /// The first level-1 heading.
    Heading,
    /// Neither: the file name.
    #[default]
    FileName,
}

/// What a note contains, as the panels and refactors need it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NoteData {
    /// The title from front matter or the first H1 (`None`: the file name).
    pub title: Option<Arc<str>>,
    /// Where the title comes from.
    pub title_source: TitleSource,
    /// Headings in order.
    pub headings: Vec<IndexedHeading>,
    /// Links and embeds in text order (front matter links first).
    pub links: Vec<IndexedLink>,
    /// The local `href`s of `<a>` tags in raw HTML (HTML blocks and inline
    /// HTML), in text order: kind [`LinkKind::HtmlHref`], `span` the
    /// attribute's value without its quotes, `raw` the value with
    /// character references decoded.
    ///
    /// Kept apart from `links` on purpose: Obsidian does not count
    /// `<a href>` as a link (no backlink, no graph edge, no unresolved
    /// link), so neither do the backlinks, the graph or the link checker
    /// here. Renames and moves of notes and folders, and heading and
    /// block-id renames, still rewrite them so they keep working.
    pub html_links: Vec<IndexedLink>,
    /// Tags: front matter tags first, then inline tags in text order.
    pub tags: Vec<IndexedTag>,
    /// Front matter `aliases`.
    pub aliases: Vec<Arc<str>>,
    /// Front matter `cssclasses`.
    pub cssclasses: Vec<Arc<str>>,
    /// Every front matter property, in order.
    pub properties: Vec<Property>,
    /// Block ids.
    pub block_ids: Vec<IndexedBlockId>,
    /// Task list items.
    pub tasks: Vec<IndexedTask>,
    /// Footnote definition labels.
    pub footnotes: Vec<Arc<str>>,
    /// The front matter block, delimiters included.
    pub front_matter: Option<Span>,
    /// Text that is not prose (code, math, HTML blocks, comments, front
    /// matter), sorted: unlinked mentions skip it.
    pub skip: Vec<Span>,
}

impl NoteData {
    /// Releases spare capacity.
    pub(crate) fn shrink(&mut self) {
        self.headings.shrink_to_fit();
        self.links.shrink_to_fit();
        self.html_links.shrink_to_fit();
        self.tags.shrink_to_fit();
        self.aliases.shrink_to_fit();
        self.cssclasses.shrink_to_fit();
        self.properties.shrink_to_fit();
        self.block_ids.shrink_to_fit();
        self.tasks.shrink_to_fit();
        self.footnotes.shrink_to_fit();
        self.skip.shrink_to_fit();
    }
}

/// One note in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRecord {
    /// Its path in the vault.
    pub path: VaultPath,
    /// What it was validated against.
    pub stamp: FileStamp,
    /// How it was indexed.
    pub kind: RecordKind,
    /// Its contents (empty unless [`RecordKind::Indexed`] or
    /// [`RecordKind::Overlay`]).
    pub data: NoteData,
}

impl NoteRecord {
    /// The display title: the front matter `title`, else the first H1,
    /// else the file name without its extension.
    #[must_use]
    pub fn title(&self) -> &str {
        match &self.data.title {
            Some(t) => t,
            None => file_stem(&self.path),
        }
    }

    /// The heading path (outermost first) of the section holding `offset`.
    #[must_use]
    pub fn heading_path(&self, offset: usize) -> Vec<Arc<str>> {
        let mut path: Vec<&IndexedHeading> = Vec::new();
        for h in &self.data.headings {
            if h.span.start as usize > offset {
                break;
            }
            while path.last().is_some_and(|p| p.level >= h.level) {
                path.pop();
            }
            path.push(h);
        }
        path.into_iter().map(|h| h.text.clone()).collect()
    }
}

/// The file name of `path` without its last extension.
#[must_use]
pub fn file_stem(path: &VaultPath) -> &str {
    let name = path.file_name().unwrap_or("");
    crate::path::stem_of(name)
}

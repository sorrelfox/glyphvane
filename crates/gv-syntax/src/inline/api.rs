// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The interface between the block pass and the inline parser.

use core::ops::Range;
use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasher;

use super::tree::{LinkData, Node, Segs};
use super::{dump, html, parse, scan, tree};
pub use crate::options::DollarMath;

/// The content of one leaf block.
#[derive(Debug, Clone, Copy)]
pub struct InlineInput<'a> {
    /// Byte ranges in the document of a leaf block's content lines (already
    /// stripped of container prefixes and leading indentation per
    /// CommonMark), in order.
    ///
    /// A segment may include its line ending; when it does not, a `\n` is
    /// implied between it and the next segment (it maps to no document
    /// bytes). Include the line endings for cmark-exact raw HTML on CRLF
    /// and CR documents: raw HTML reproduces the line endings it spans.
    /// Trailing spaces are kept (they make hard breaks); the leaf's leading
    /// and trailing whitespace stays in the tree as tokens that render
    /// nothing.
    pub segments: &'a [Range<usize>],
}

/// Looks up link reference definitions by normalized label.
pub trait RefLookup {
    /// The definition of `normalized_label` (see [`normalize_label`]), if
    /// any.
    fn lookup(&self, normalized_label: &str) -> Option<&RefDef>;

    /// Whether a footnote is defined for `normalized_label` (the label
    /// without its `^`). None are by default.
    fn has_footnote(&self, normalized_label: &str) -> bool {
        let _ = normalized_label;
        false
    }
}

/// A link reference definition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RefDef {
    /// The destination, unescaped and entity-decoded.
    pub destination: String,
    /// The title, unescaped and entity-decoded.
    pub title: Option<String>,
}

impl<S: BuildHasher> RefLookup for HashMap<String, RefDef, S> {
    fn lookup(&self, normalized_label: &str) -> Option<&RefDef> {
        self.get(normalized_label)
    }
}

impl RefLookup for BTreeMap<String, RefDef> {
    fn lookup(&self, normalized_label: &str) -> Option<&RefDef> {
        self.get(normalized_label)
    }
}

/// No definitions.
impl RefLookup for () {
    fn lookup(&self, _: &str) -> Option<&RefDef> {
        None
    }
}

/// The inline tree of one leaf block: lossless over its segments (every
/// byte of every segment belongs to exactly one token), opaque to the block
/// side. See the accessors (`links`, `code_spans`, `emphasis`, `tokens`, …)
/// for reading it.
#[derive(Debug, Clone, Default)]
pub struct InlineTree {
    /// The segments concatenated (with implied line endings).
    pub(super) text: String,
    /// Gathered offset to document offset.
    pub(super) segs: Segs,
    /// Nodes in preorder.
    pub(super) nodes: Vec<Node>,
    /// Links, images and autolinks.
    pub(super) links: Vec<LinkData>,
    /// Decoded strings (destinations, titles, labels).
    pub(super) arena: String,
    /// Labels looked up (arena ranges).
    pub(super) lookups: Vec<(u32, u32)>,
    /// Normalized labels of the footnote references (arena ranges), indexed
    /// by the nodes' data.
    pub(super) footnotes: Vec<(u32, u32)>,
}

/// The inline switches of a flavor profile. The default is CommonMark
/// exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct InlineOptions {
    /// GFM strikethrough: a run of two `~` is a delimiter; an opener and a
    /// closer of the same length make an
    /// [`InlineKind::Strikethrough`](super::InlineKind::Strikethrough).
    pub strikethrough: bool,
    /// With `strikethrough`, a run of one `~` is a delimiter too (GFM).
    pub strikethrough_single: bool,
    /// GFM extended autolinks (`www.…`, `http://…`, `https://…`, `ftp://…`
    /// and bare email addresses), as
    /// [`InlineKind::Autolink`](super::InlineKind::Autolink) nodes without
    /// angle brackets.
    pub autolinks: bool,
    /// `$x$` inline math and `$$x$$` display math, with the given `$` rule
    /// (see [`DollarMath`]).
    pub math: DollarMath,
    /// `` $`x`$ `` inline math.
    pub math_code: bool,
    /// `\(x\)` inline and `\[x\]` display math.
    pub math_brackets: bool,
    /// Footnote references: `[^label]` when a footnote is defined for the
    /// label ([`RefLookup::has_footnote`]) and the brackets are no link.
    pub footnotes: bool,
    /// `==x==` highlight: a run of exactly two `=` is a delimiter
    /// ([`InlineKind::Highlight`](super::InlineKind::Highlight)).
    pub highlight: bool,
    /// `[[target#anchor|alias]]` wikilinks
    /// ([`InlineKind::Wikilink`](super::InlineKind::Wikilink)).
    pub wikilinks: bool,
    /// `![[…]]` embeds ([`InlineKind::Embed`](super::InlineKind::Embed)).
    pub embeds: bool,
    /// `%%…%%` inline comments, both markers on one line
    /// ([`InlineKind::Comment`](super::InlineKind::Comment)).
    pub comments: bool,
    /// Obsidian `#tag`s ([`InlineKind::Tag`](super::InlineKind::Tag)).
    pub tags: bool,
    /// A `^block-id` at the end of the content
    /// ([`InlineKind::BlockId`](super::InlineKind::BlockId)); for
    /// paragraphs only (see [`crate::ParseOptions::inline_for`]).
    pub block_ids: bool,
    /// `^[…]` inline footnotes
    /// ([`InlineKind::InlineFootnote`](super::InlineKind::InlineFootnote)).
    pub inline_footnotes: bool,
}

impl InlineOptions {
    /// CommonMark: no extensions.
    pub const COMMONMARK: InlineOptions = InlineOptions {
        strikethrough: false,
        strikethrough_single: false,
        autolinks: false,
        math: DollarMath::Off,
        math_code: false,
        math_brackets: false,
        footnotes: false,
        highlight: false,
        wikilinks: false,
        embeds: false,
        comments: false,
        tags: false,
        block_ids: false,
        inline_footnotes: false,
    };

    /// The GFM extensions.
    pub const GFM: InlineOptions = InlineOptions {
        strikethrough: true,
        strikethrough_single: true,
        autolinks: true,
        ..InlineOptions::COMMONMARK
    };
}

/// Parses the inline content of a leaf block with the CommonMark rules.
pub fn parse_inlines<S: gv_text::TextSource + ?Sized>(
    src: &S,
    input: InlineInput<'_>,
    refs: &dyn RefLookup,
) -> InlineTree {
    parse_inlines_with(src, input, refs, InlineOptions::COMMONMARK)
}

/// Parses the inline content of a leaf block with the given switches.
pub fn parse_inlines_with<S: gv_text::TextSource + ?Sized>(
    src: &S,
    input: InlineInput<'_>,
    refs: &dyn RefLookup,
    options: InlineOptions,
) -> InlineTree {
    let (text, segs) = tree::gather(src, input.segments);
    parse::parse(text, segs, refs, options)
}

/// Parses one GFM table cell: `cell` is the cell's trimmed document range
/// (see [`crate::table::row_cells`]). Every backslash directly before a
/// `|` is removed first, also inside code spans, as cmark-gfm does. The
/// removed backslashes are not part of the tree's text; a token around one
/// covers it in its document range.
pub fn parse_table_cell<S: gv_text::TextSource + ?Sized>(
    src: &S,
    cell: Range<usize>,
    refs: &dyn RefLookup,
    options: InlineOptions,
) -> InlineTree {
    let mut bytes = Vec::new();
    src.copy_range(cell.clone(), &mut bytes);
    let pieces = crate::table::cell_pieces(cell, &bytes);
    let (text, segs) = tree::gather_joined(src, &pieces);
    parse::parse(text, segs, refs, options)
}

/// Renders an inline tree as CommonMark HTML (cmark's output, byte for
/// byte).
pub fn render_inlines_html(tree: &InlineTree, out: &mut String) {
    html::render(tree, out, html::RenderOptions::COMMONMARK);
}

/// Inline HTML rendering switches of the GFM-exact emitter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct InlineHtmlOptions {
    /// cmark-gfm's renderer: a strong emphasis directly inside another
    /// strong emphasis writes no tags of its own.
    pub gfm: bool,
    /// The GFM tagfilter: `<` of raw HTML that opens one of the disallowed
    /// tags (`title`, `textarea`, `style`, `xmp`, `iframe`, `noembed`,
    /// `noframes`, `script`, `plaintext`) is written as `&lt;`.
    pub tagfilter: bool,
}

/// Appends raw HTML (an HTML block's text) with the GFM tagfilter applied
/// (see [`InlineHtmlOptions::tagfilter`]).
pub fn push_tagfiltered(out: &mut String, raw: &str) {
    html::push_filtered(out, raw);
}

/// Renders an inline tree as HTML with the given switches.
pub fn render_inlines_html_with(tree: &InlineTree, out: &mut String, options: InlineHtmlOptions) {
    html::render(
        tree,
        out,
        html::RenderOptions {
            gfm: options.gfm,
            tagfilter: options.tagfilter,
        },
    );
}

/// [`render_inlines_html_with`], with each footnote reference written by
/// `footnote` from its normalized label (the block emitter numbers them).
pub fn render_inlines_html_footnotes(
    tree: &InlineTree,
    out: &mut String,
    options: InlineHtmlOptions,
    footnote: &mut dyn FnMut(&str, &mut String),
) {
    html::render_with(
        tree,
        out,
        html::RenderOptions {
            gfm: options.gfm,
            tagfilter: options.tagfilter,
        },
        Some(footnote),
    );
}

/// CommonMark label normalization: Unicode full case folding, leading and
/// trailing spaces, tabs and line endings removed, internal runs of them
/// collapsed to one space.
pub fn normalize_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    scan::normalize_label_into(raw, &mut out);
    out
}

/// Tries to parse one link reference definition at the start of `text` (a
/// paragraph's content, joined with '\n'). Returns the bytes consumed (up to
/// and including the line ending), the normalized label and the definition.
///
/// `text` must start with the `[` of the label (indentation already
/// stripped). The destination is trimmed and, like the title, unescaped
/// and entity-decoded. [`parse_ref_def_spans`] also reports where the
/// parts are.
pub fn parse_ref_def(text: &str) -> Option<(usize, String, RefDef)> {
    scan::ref_def(text)
}

/// A link reference definition with the source ranges of its parts.
///
/// From [`parse_ref_def_spans`] the ranges are offsets into its input;
/// from [`Leaf::link_ref_def`](crate::Leaf::link_ref_def) they are
/// document offsets (a part that spans lines then also covers the
/// container prefixes between them).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RefDefSpans {
    /// The normalized label (see [`normalize_label`]).
    pub label: String,
    /// The destination and title, decoded.
    pub def: RefDef,
    /// The whole definition, from the `[` to the end of its last line's
    /// content (trailing spaces included, line ending excluded).
    pub range: Range<usize>,
    /// The label as written, between the brackets.
    pub label_range: Range<usize>,
    /// The destination as written, with its `<` and `>` if it has them.
    pub destination_range: Range<usize>,
    /// The title as written, with its delimiters.
    pub title_range: Option<Range<usize>>,
}

/// [`parse_ref_def`] with the ranges of the label, destination and title
/// in `text`: returns the bytes consumed and the located definition.
pub fn parse_ref_def_spans(text: &str) -> Option<(usize, RefDefSpans)> {
    scan::ref_def_spans(text)
}

/// Decodes backslash escapes and entity and numeric character references
/// in one left-to-right pass, as for link destinations and titles. Also the
/// decoding CommonMark applies to fenced code info strings.
pub fn unescape_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    scan::decode_into(raw, &mut out);
    out
}

/// Writes the S-expression lines of an inline tree for the CST dump (one
/// node per line, two spaces of indentation per level, starting at column
/// 0).
pub fn cst_dump_inlines(tree: &InlineTree, out: &mut String) {
    dump::dump(tree, 0, out);
}

/// [`cst_dump_inlines`] with every line indented by `indent` spaces, for
/// nesting the lines under a block node.
pub fn cst_dump_inlines_indented(tree: &InlineTree, indent: usize, out: &mut String) {
    dump::dump(tree, indent, out);
}

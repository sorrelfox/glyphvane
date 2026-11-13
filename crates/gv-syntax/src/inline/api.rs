// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The interface between the block pass and the inline parser.

use core::ops::Range;
use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasher;

use super::tree::{LinkData, Node, Segs};
use super::{dump, html, parse, scan, tree};

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
}

/// Parses the inline content of a leaf block.
pub fn parse_inlines<S: gv_text::TextSource + ?Sized>(
    src: &S,
    input: InlineInput<'_>,
    refs: &dyn RefLookup,
) -> InlineTree {
    let (text, segs) = tree::gather(src, input.segments);
    parse::parse(text, segs, refs)
}

/// Renders an inline tree as CommonMark HTML (cmark's output, byte for
/// byte).
pub fn render_inlines_html(tree: &InlineTree, out: &mut String) {
    html::render(tree, out);
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
/// and entity-decoded.
pub fn parse_ref_def(text: &str) -> Option<(usize, String, RefDef)> {
    scan::ref_def(text)
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

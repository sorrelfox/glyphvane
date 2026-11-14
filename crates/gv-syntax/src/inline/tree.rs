// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The inline tree's storage, its construction from the parser's working
//! arena, and the read-only accessor API.
//!
//! # Layout
//!
//! An [`InlineTree`] owns a copy of its leaf's content (the *gathered
//! text*): the segments concatenated, with a virtual `\n` between two
//! segments when the first does not end in a line ending. All offsets in the
//! tree are offsets into that text (`u32`); the segment map turns them into
//! document offsets on demand.
//!
//! Nodes are stored in preorder. Every node records the index just past its
//! subtree (`next`), so children are walked by jumping from `i + 1` along
//! `next`, and a subtree is the slice `i..next`. Leaves (tokens) never cross
//! a segment boundary: the tokens of a tree cover its segments exactly, in
//! order.

use core::ops::Range;

use gv_text::TextSource;

use super::api::InlineTree;
use super::html;

/// Kinds of inline nodes (with children) and tokens (leaves).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum InlineKind {
    /// A code span: [`Marker`](Self::Marker), [`CodeText`](Self::CodeText)
    /// tokens, [`Marker`](Self::Marker).
    CodeSpan,
    /// `*x*` or `_x_`: a marker, the content, a marker.
    Emphasis,
    /// `**x**` or `__x__`: a marker, the content, a marker.
    Strong,
    /// A link: `[` marker, the link text, `]` marker, then the destination,
    /// title or label syntax.
    Link,
    /// An image: `![` marker, the description, `]` marker, then the
    /// destination, title or label syntax.
    Image,
    /// `<scheme:…>` or `<address@example.com>`: marker, text, marker.
    Autolink,
    /// Raw inline HTML: one or more [`HtmlText`](Self::HtmlText) tokens.
    RawHtml,
    /// Literal text.
    Text,
    /// A backslash escape such as `\*` (renders the escaped character).
    Escape,
    /// An entity or numeric character reference such as `&amp;`.
    Entity,
    /// A soft line break: trailing spaces or tabs plus the line ending.
    SoftBreak,
    /// A hard line break: two or more trailing spaces, or a backslash, plus
    /// the line ending.
    HardBreak,
    /// Whitespace that renders nothing: indentation at the start of a
    /// continuation line, the leaf's leading and trailing whitespace, and the
    /// whitespace between the parts of a link.
    Whitespace,
    /// Syntax: emphasis delimiters, backticks, brackets, parentheses, the
    /// angle brackets of autolinks.
    Marker,
    /// The content of a code span.
    CodeText,
    /// Raw HTML.
    HtmlText,
    /// The address of an autolink.
    AutolinkText,
    /// A link destination as written (including `<` and `>`).
    LinkDestination,
    /// A link title as written (including its quotes or parentheses).
    LinkTitle,
    /// The label of a full reference link (`[text][label]`).
    LinkLabel,
}

impl InlineKind {
    /// Whether this kind is a token (a leaf) rather than a node with children.
    #[inline]
    pub fn is_token(self) -> bool {
        // The node kinds come first in the declaration.
        self as u8 > Self::RawHtml as u8
    }

    /// The kind's name, as used in the CST dump.
    pub fn name(self) -> &'static str {
        match self {
            Self::CodeSpan => "CodeSpan",
            Self::Emphasis => "Emphasis",
            Self::Strong => "Strong",
            Self::Link => "Link",
            Self::Image => "Image",
            Self::Autolink => "Autolink",
            Self::RawHtml => "RawHtml",
            Self::Text => "Text",
            Self::Escape => "Escape",
            Self::Entity => "Entity",
            Self::SoftBreak => "SoftBreak",
            Self::HardBreak => "HardBreak",
            Self::Whitespace => "Whitespace",
            Self::Marker => "Marker",
            Self::CodeText => "CodeText",
            Self::HtmlText => "HtmlText",
            Self::AutolinkText => "AutolinkText",
            Self::LinkDestination => "LinkDestination",
            Self::LinkTitle => "LinkTitle",
            Self::LinkLabel => "LinkLabel",
        }
    }
}

/// A plain-text projection of inline content: what heading slug
/// algorithms and text search start from.
///
/// Both keep text with backslash escapes and entity references decoded,
/// code span content as it renders (line endings as spaces, one space
/// stripped from each end), link text and autolink addresses, and drop
/// markup: emphasis and link syntax, destinations, titles and raw HTML
/// tags (the text between tags stays). U+0000 becomes U+FFFD, as in the
/// rendered HTML. They differ in images and line breaks.
///
/// | Slug algorithm (gv-model) | Projection |
/// |---|---|
/// | GitHub (github-slugger), Python-Markdown `toc` | [`TextContent`](Self::TextContent) |
/// | GitLab (comrak's anchorizer), Pandoc | [`CollectText`](Self::CollectText) |
/// | kramdown, Hugo | the Markdown source, not a projection |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextProjection {
    /// The `textContent` of the rendered HTML: images contribute nothing
    /// (their description is an attribute), and soft and hard line breaks
    /// are the `\n` the HTML holds.
    TextContent,
    /// The text of the syntax tree, as comrak's `collect_text` and
    /// pandoc's `stringify` gather it: image descriptions are included, and
    /// line breaks are spaces.
    CollectText,
}

/// How a link or image got its destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// The kind's name, as used in the CST dump.
    pub fn name(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Full => "full",
            Self::Collapsed => "collapsed",
            Self::Shortcut => "shortcut",
            Self::UriAutolink => "uri",
            Self::EmailAutolink => "email",
        }
    }

    /// Whether the destination comes from a link reference definition.
    pub fn is_reference(self) -> bool {
        matches!(self, Self::Full | Self::Collapsed | Self::Shortcut)
    }
}

/// One node of the preorder array.
#[derive(Clone, Copy, Debug)]
pub(super) struct Node {
    pub(super) kind: InlineKind,
    /// Gathered-text range.
    pub(super) start: u32,
    pub(super) end: u32,
    /// Index just past this node's subtree.
    pub(super) next: u32,
    /// Kind-specific: entity value (see [`ENTITY_NAMED`]), backtick count of
    /// a code span, index into the link table of links, images and
    /// autolinks.
    pub(super) data: u32,
}

/// Set in an entity's `data` when the low bits index the named-entity table;
/// otherwise `data` is the code point.
pub(super) const ENTITY_NAMED: u32 = 1 << 31;

/// A string payload: a range of the gathered text (when no decoding was
/// needed) or of the tree's arena of decoded strings.
#[derive(Clone, Copy, Debug)]
pub(super) enum Str {
    Text(u32, u32),
    Arena(u32, u32),
}

/// Side table entry of a link, image or autolink.
#[derive(Clone, Copy, Debug)]
pub(super) struct LinkData {
    pub(super) kind: LinkKind,
    /// Decoded destination. For email autolinks, the address without
    /// `mailto:`.
    pub(super) dest: Str,
    pub(super) title: Option<Str>,
    /// Normalized label of reference links (arena).
    pub(super) label: Option<Str>,
    /// Gathered range of the link text (autolinks: the address).
    pub(super) text: (u32, u32),
    /// Gathered range of the destination as written (inline links,
    /// autolinks).
    pub(super) dest_raw: Option<(u32, u32)>,
    /// Gathered range of the label as written (reference links: the text of
    /// the second bracket pair, or the link text).
    pub(super) label_raw: Option<(u32, u32)>,
}

/// One segment: where its bytes start in the gathered text and in the
/// document. A virtual `\n` may follow its `src_len` bytes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Seg {
    pub(super) g: u32,
    pub(super) src: usize,
    pub(super) src_len: u32,
}

/// The segment map: one segment inline (most leaves are one line), or
/// several on the heap.
#[derive(Clone, Debug)]
pub(super) enum Segs {
    One(Seg),
    Many(Vec<Seg>),
}

impl Default for Segs {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl Segs {
    pub(super) fn as_slice(&self) -> &[Seg] {
        match self {
            Self::One(seg) => std::slice::from_ref(seg),
            Self::Many(segs) => segs,
        }
    }
}

/// The longest gathered text a tree holds. Longer leaves (over 2 GiB) are
/// truncated at a segment boundary.
const MAX_GATHERED: usize = (u32::MAX / 2) as usize;

/// Copies the segments into one buffer and builds the segment map.
pub(super) fn gather<S: TextSource + ?Sized>(src: &S, segments: &[Range<usize>]) -> (String, Segs) {
    let total: usize = segments
        .iter()
        .map(|r| r.end.saturating_sub(r.start).saturating_add(1))
        .fold(0usize, usize::saturating_add);
    let mut buf: Vec<u8> = Vec::with_capacity(total.min(MAX_GATHERED));
    let mut segs = Vec::with_capacity(if segments.len() > 1 {
        segments.len()
    } else {
        0
    });
    let mut one = None;
    let src_len = src.len();
    for (i, r) in segments.iter().enumerate() {
        let start = r.start.min(src_len);
        let end = r.end.clamp(start, src_len);
        if buf.len() + (end - start) + 1 > MAX_GATHERED {
            break;
        }
        let g = buf.len();
        src.copy_range(start..end, &mut buf);
        let len = buf.len() - g;
        let seg = Seg {
            g: g as u32,
            src: start,
            src_len: len as u32,
        };
        if segments.len() == 1 {
            one = Some(seg);
        } else {
            segs.push(seg);
        }
        let ends_line = len > 0 && matches!(buf.last(), Some(b'\n' | b'\r'));
        if i + 1 < segments.len() && !ends_line {
            buf.push(b'\n');
        }
    }
    let segs = match one {
        Some(seg) => Segs::One(seg),
        None => Segs::Many(segs),
    };
    (into_string(buf), segs)
}

/// Converts the gathered bytes to a `String`, replacing each byte of an
/// invalid UTF-8 sequence with `?` so that offsets stay unchanged. Text
/// sources hold valid UTF-8 and segments start and end on character
/// boundaries, so this only guards against misuse.
fn into_string(buf: Vec<u8>) -> String {
    match String::from_utf8(buf) {
        Ok(s) => s,
        Err(e) => {
            let mut bytes = e.into_bytes();
            let mut i = 0;
            while i < bytes.len() {
                match std::str::from_utf8(&bytes[i..]) {
                    Ok(_) => break,
                    Err(err) => {
                        let bad = i + err.valid_up_to();
                        let n = err.error_len().unwrap_or(bytes.len() - bad);
                        for b in &mut bytes[bad..bad + n] {
                            *b = b'?';
                        }
                        i = bad + n;
                    }
                }
            }
            String::from_utf8(bytes).unwrap_or_default()
        }
    }
}

/// A node of the parser's working arena: children are doubly linked lists so
/// that emphasis and links can wrap a run of siblings in O(1).
#[derive(Clone, Copy, Debug)]
pub(super) struct ANode {
    pub(super) kind: InlineKind,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) next: u32,
    pub(super) prev: u32,
    pub(super) first: u32,
    pub(super) last: u32,
    pub(super) data: u32,
}

/// "No node" in the arena's links.
pub(super) const NIL: u32 = u32::MAX;

/// A token waiting to be written by [`flatten`] (adjacent text merges).
#[derive(Clone, Copy)]
struct Pending {
    kind: InlineKind,
    start: u32,
    end: u32,
    data: u32,
}

/// A composite node being written by [`flatten`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Frame {
    /// The arena sibling to visit after this node.
    next: u32,
    /// The node's index in the output.
    index: usize,
    /// The parent's end (restored when this node is done).
    parent_end: u32,
}

/// Turns the arena (node 0 is the root) into the preorder array: fills the
/// gaps between nodes with text tokens (plain text gets no arena node),
/// drops empty tokens, merges adjacent text, and splits tokens at segment
/// boundaries.
pub(super) fn flatten(
    arena: &[ANode],
    segs: &[Seg],
    text_len: u32,
    stack: &mut Vec<Frame>,
) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::with_capacity(arena.len() * 2);
    stack.clear();
    let mut pending: Option<Pending> = None;
    let mut seg_cursor = SegCursor::new(segs);
    let mut cur = arena.first().map_or(NIL, |root| root.first);
    // Everything before `cursor` in the current parent has been written.
    let mut cursor = 0u32;
    let mut parent_end = text_len;
    loop {
        if cur == NIL {
            push_token(
                &mut out,
                &mut pending,
                segs,
                &mut seg_cursor,
                InlineKind::Text,
                cursor,
                parent_end,
                0,
            );
            flush(&mut out, &mut pending, segs, &mut seg_cursor);
            match stack.pop() {
                Some(frame) => {
                    out[frame.index].next = out.len() as u32;
                    cursor = parent_end;
                    parent_end = frame.parent_end;
                    cur = frame.next;
                    continue;
                }
                None => break,
            }
        }
        let n = &arena[cur as usize];
        push_token(
            &mut out,
            &mut pending,
            segs,
            &mut seg_cursor,
            InlineKind::Text,
            cursor,
            n.start,
            0,
        );
        if n.kind.is_token() {
            push_token(
                &mut out,
                &mut pending,
                segs,
                &mut seg_cursor,
                n.kind,
                n.start,
                n.end,
                n.data,
            );
            cursor = cursor.max(n.end);
            cur = n.next;
        } else {
            flush(&mut out, &mut pending, segs, &mut seg_cursor);
            stack.push(Frame {
                next: n.next,
                index: out.len(),
                parent_end,
            });
            out.push(Node {
                kind: n.kind,
                start: n.start,
                end: n.end,
                next: 0,
                data: n.data,
            });
            cursor = n.start;
            parent_end = n.end;
            cur = n.first;
        }
    }
    // The capacity was an upper bound; trees are kept in caches.
    if out.capacity() > out.len() + out.len() / 4 + 16 {
        out.shrink_to_fit();
    }
    out
}

/// Queues the token `start..end` (nothing if empty), merging text with the
/// pending text it touches.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn push_token(
    out: &mut Vec<Node>,
    pending: &mut Option<Pending>,
    segs: &[Seg],
    cursor: &mut SegCursor,
    kind: InlineKind,
    start: u32,
    end: u32,
    data: u32,
) {
    if start >= end {
        return;
    }
    if let Some(p) = pending
        && kind == InlineKind::Text
        && p.kind == InlineKind::Text
        && p.end == start
    {
        p.end = end;
        return;
    }
    flush(out, pending, segs, cursor);
    *pending = Some(Pending {
        kind,
        start,
        end,
        data,
    });
}

/// Where [`flush`] is in the segment map.
struct SegCursor {
    /// Index of the segment holding the last token written.
    index: usize,
    /// Start of the next segment (`u32::MAX` after the last).
    next: u32,
}

impl SegCursor {
    fn new(segs: &[Seg]) -> Self {
        Self {
            index: 0,
            next: segs.get(1).map_or(u32::MAX, |s| s.g),
        }
    }

    fn advance(&mut self, segs: &[Seg]) {
        self.index += 1;
        self.next = segs.get(self.index + 1).map_or(u32::MAX, |s| s.g);
    }
}

/// Writes the pending token, split at segment starts.
#[inline(always)]
fn flush(out: &mut Vec<Node>, pending: &mut Option<Pending>, segs: &[Seg], cursor: &mut SegCursor) {
    let Some(p) = pending.take() else {
        return;
    };
    let mut start = p.start;
    // Tokens come in order: skip the segments that end before this one.
    while cursor.next <= start {
        cursor.advance(segs);
    }
    while cursor.next < p.end {
        out.push(Node {
            kind: p.kind,
            start,
            end: cursor.next,
            next: out.len() as u32 + 1,
            data: p.data,
        });
        start = cursor.next;
        cursor.advance(segs);
    }
    out.push(Node {
        kind: p.kind,
        start,
        end: p.end,
        next: out.len() as u32 + 1,
        data: p.data,
    });
}

impl InlineTree {
    /// The gathered text in `start..end` (empty if out of range).
    pub(super) fn slice(&self, start: u32, end: u32) -> &str {
        self.text.get(start as usize..end as usize).unwrap_or("")
    }

    /// The text of a string payload.
    pub(super) fn str(&self, s: Str) -> &str {
        match s {
            Str::Text(a, b) => self.slice(a, b),
            Str::Arena(a, b) => self.arena.get(a as usize..b as usize).unwrap_or(""),
        }
    }

    /// The segment holding gathered offset `g`.
    fn seg_of(&self, g: u32) -> Option<&Seg> {
        let segs = self.segs.as_slice();
        let k = segs.partition_point(|s| s.g <= g);
        segs.get(k.checked_sub(1)?)
    }

    /// The document offset of gathered offset `g` (the start of a range).
    fn src_start(&self, g: u32) -> usize {
        self.seg_of(g)
            .map_or(0, |s| s.src + ((g - s.g).min(s.src_len)) as usize)
    }

    /// The document range of the gathered range `start..end`.
    pub(super) fn src_range(&self, start: u32, end: u32) -> Range<usize> {
        let a = self.src_start(start);
        if end <= start {
            return a..a;
        }
        let b = self
            .seg_of(end - 1)
            .map_or(a, |s| s.src + ((end - s.g).min(s.src_len)) as usize);
        a..b.max(a)
    }

    /// The document range covered by the tree's segments.
    pub fn source_range(&self) -> Range<usize> {
        let segs = self.segs.as_slice();
        match (segs.first(), segs.last()) {
            (Some(first), Some(last)) => first.src..last.src + last.src_len as usize,
            _ => 0..0,
        }
    }

    /// Whether the tree has no nodes (its segments were empty).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The top-level nodes, in order.
    pub fn top_level(&self) -> Children<'_> {
        Children {
            tree: self,
            index: 0,
            end: self.nodes.len() as u32,
        }
    }

    /// Every node and token in preorder.
    pub fn nodes(&self) -> impl Iterator<Item = InlineNode<'_>> + '_ {
        (0..self.nodes.len() as u32).map(move |index| InlineNode { tree: self, index })
    }

    /// The tokens (leaves) in order with their kinds and document ranges.
    /// Their ranges cover the segments exactly; a token made of a virtual
    /// line ending between two segments is empty.
    pub fn tokens(&self) -> impl Iterator<Item = (InlineKind, Range<usize>)> + '_ {
        self.nodes
            .iter()
            .filter(|n| n.kind.is_token())
            .map(move |n| (n.kind, self.src_range(n.start, n.end)))
    }

    /// Links, images and autolinks in document order.
    pub fn links(&self) -> impl Iterator<Item = LinkRef<'_>> + '_ {
        self.nodes.iter().filter_map(move |n| self.link_ref(n))
    }

    /// The link data of `n`, if it is a link, image or autolink.
    fn link_ref(&self, n: &Node) -> Option<LinkRef<'_>> {
        if !matches!(
            n.kind,
            InlineKind::Link | InlineKind::Image | InlineKind::Autolink
        ) {
            return None;
        }
        let l = self.links.get(n.data as usize)?;
        Some(LinkRef {
            kind: l.kind,
            is_image: n.kind == InlineKind::Image,
            range: self.src_range(n.start, n.end),
            text_range: self.src_range(l.text.0, l.text.1),
            destination: self.str(l.dest),
            raw_destination: l.dest_raw.map(|(a, b)| self.src_range(a, b)),
            title: l.title.map(|t| self.str(t)),
            label: l.label.map(|t| self.str(t)),
            label_range: l.label_raw.map(|(a, b)| self.src_range(a, b)),
        })
    }

    /// Code spans in document order.
    pub fn code_spans(&self) -> impl Iterator<Item = CodeSpanRef<'_>> + '_ {
        self.nodes
            .iter()
            .filter(|n| n.kind == InlineKind::CodeSpan)
            .map(move |n| {
                let (a, b) = (n.start + n.data, n.end.saturating_sub(n.data));
                CodeSpanRef {
                    range: self.src_range(n.start, n.end),
                    content_range: self.src_range(a, b),
                    raw_content: self.slice(a, b),
                }
            })
    }

    /// Emphasis and strong emphasis in preorder.
    pub fn emphasis(&self) -> impl Iterator<Item = EmphasisRef> + '_ {
        self.nodes
            .iter()
            .filter(|n| matches!(n.kind, InlineKind::Emphasis | InlineKind::Strong))
            .map(move |n| {
                let width = if n.kind == InlineKind::Strong { 2 } else { 1 };
                EmphasisRef {
                    strong: n.kind == InlineKind::Strong,
                    range: self.src_range(n.start, n.end),
                    content_range: self.src_range(n.start + width, n.end - width),
                }
            })
    }

    /// The normalized labels this parse looked up, including the lookups that
    /// found nothing, in order (with repeats). An inline cache must be
    /// invalidated when the definition of any of them changes.
    pub fn label_lookups(&self) -> impl Iterator<Item = &str> + '_ {
        self.lookups
            .iter()
            .map(move |&(a, b)| self.arena.get(a as usize..b as usize).unwrap_or(""))
    }

    /// Appends the plain-text projection (what an image's `alt` holds: text
    /// with escapes and entities decoded, code span content, line breaks as
    /// spaces; no markup).
    pub fn plain_text(&self, out: &mut String) {
        html::plain_text(self, 0, self.nodes.len(), out, false);
    }

    /// Appends a text projection of the whole tree (see
    /// [`TextProjection`]).
    pub fn push_text(&self, projection: TextProjection, out: &mut String) {
        html::project(self, 0, self.nodes.len(), projection, out);
    }

    /// The `textContent` of the rendered tree
    /// ([`TextProjection::TextContent`]).
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        self.push_text(TextProjection::TextContent, &mut out);
        out
    }

    /// The tree's text with image descriptions
    /// ([`TextProjection::CollectText`]).
    pub fn collect_text(&self) -> String {
        let mut out = String::new();
        self.push_text(TextProjection::CollectText, &mut out);
        out
    }
}

/// A node or token of an [`InlineTree`].
#[derive(Clone, Copy, Debug)]
pub struct InlineNode<'a> {
    tree: &'a InlineTree,
    index: u32,
}

impl<'a> InlineNode<'a> {
    fn node(&self) -> &'a Node {
        &self.tree.nodes[self.index as usize]
    }

    /// The node's kind.
    pub fn kind(&self) -> InlineKind {
        self.node().kind
    }

    /// The node's document range. A node spanning several segments covers
    /// the bytes between them too (container prefixes owned by the block
    /// structure).
    pub fn range(&self) -> Range<usize> {
        let n = self.node();
        self.tree.src_range(n.start, n.end)
    }

    /// The node's text as gathered (segments joined by their line endings).
    pub fn text(&self) -> &'a str {
        let n = self.node();
        self.tree.slice(n.start, n.end)
    }

    /// The node's children (empty for tokens).
    pub fn children(&self) -> Children<'a> {
        let n = self.node();
        Children {
            tree: self.tree,
            index: self.index + 1,
            end: n.next,
        }
    }

    /// The link, image or autolink data of this node.
    pub fn link(&self) -> Option<LinkRef<'a>> {
        self.tree.link_ref(self.node())
    }

    /// Appends a text projection of this node and its descendants (see
    /// [`TextProjection`]; the projection of an image's own description
    /// with [`TextProjection::TextContent`] is empty).
    pub fn push_text(&self, projection: TextProjection, out: &mut String) {
        let n = self.node();
        let end = if n.kind.is_token() {
            self.index + 1
        } else {
            n.next
        };
        html::project(
            self.tree,
            self.index as usize,
            end as usize,
            projection,
            out,
        );
    }
}

/// Iterator over sibling nodes.
#[derive(Clone, Debug)]
pub struct Children<'a> {
    tree: &'a InlineTree,
    index: u32,
    end: u32,
}

impl<'a> Iterator for Children<'a> {
    type Item = InlineNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.end {
            return None;
        }
        let node = InlineNode {
            tree: self.tree,
            index: self.index,
        };
        self.index = self.tree.nodes.get(self.index as usize)?.next;
        Some(node)
    }
}

/// A link, image or autolink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRef<'a> {
    /// How the destination was given.
    pub kind: LinkKind,
    /// Whether this is an image.
    pub is_image: bool,
    /// The whole construct.
    pub range: Range<usize>,
    /// The link text or image description between the brackets; for
    /// autolinks, the address.
    pub text_range: Range<usize>,
    /// The destination: unescaped and entity-decoded (inline and reference
    /// links), or the address with entity references decoded (autolinks;
    /// email addresses without `mailto:`).
    pub destination: &'a str,
    /// The destination as written, including `<` and `>` (inline links and
    /// autolinks; `None` for reference links and empty destinations).
    pub raw_destination: Option<Range<usize>>,
    /// The title, unescaped and entity-decoded.
    pub title: Option<&'a str>,
    /// The normalized label (reference links).
    pub label: Option<&'a str>,
    /// The label as written (reference links: the content of the second
    /// bracket pair of a full reference, otherwise the link text).
    pub label_range: Option<Range<usize>>,
}

/// A code span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeSpanRef<'a> {
    /// The whole span, backticks included.
    pub range: Range<usize>,
    /// The content between the backtick strings.
    pub content_range: Range<usize>,
    /// The content as gathered (line endings not yet turned into spaces, no
    /// space stripped).
    pub raw_content: &'a str,
}

/// Emphasis or strong emphasis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmphasisRef {
    /// `**`/`__` rather than `*`/`_`.
    pub strong: bool,
    /// The whole span, delimiters included.
    pub range: Range<usize>,
    /// The content between the delimiters.
    pub content_range: Range<usize>,
}

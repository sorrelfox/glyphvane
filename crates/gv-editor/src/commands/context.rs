// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The block context of a line, read from the syntax tree: the containers
//! open on it (block quotes, lists, list items with their marker geometry),
//! which bytes of the line are each container's prefix, and the leaf block
//! the line belongs to.
//!
//! Columns follow CommonMark: a tab advances to the next multiple of 4. A
//! list item's **content offset** is W + N plus the indentation before the
//! marker (W the marker width, N the padding: 1–4 columns, or 1 when the
//! item starts blank or with indented code), counted from where its
//! parent's content starts.

use std::ops::Range;

use gv_syntax::{BlockId, Container, ContainerKind, Leaf, LeafKind, LeafLine, TokenKind, Visitor};

use super::doc::{Doc, Line};

/// Columns per tab stop.
pub(crate) const TAB_STOP: usize = 4;

/// The column after a tab at `col`.
pub(crate) fn tab_end(col: usize) -> usize {
    col + TAB_STOP - col % TAB_STOP
}

/// The column after `bytes`, starting at column `col`. Continuation bytes of
/// UTF-8 characters take no column.
pub(crate) fn col_after(bytes: &[u8], col: usize) -> usize {
    bytes.iter().fold(col, |col, &b| match b {
        b'\t' => tab_end(col),
        b if b & 0xC0 == 0x80 => col,
        _ => col + 1,
    })
}

/// `bytes` with every tab replaced by the spaces it spans, with the first
/// byte at column `col`.
pub(crate) fn detab(bytes: &[u8], col: usize) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut col = col;
    let text = String::from_utf8_lossy(bytes);
    for c in text.chars() {
        if c == '\t' {
            let end = tab_end(col);
            out.extend(std::iter::repeat_n(' ', end - col));
            col = end;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// `n` spaces.
pub(crate) fn spaces(n: usize) -> String {
    " ".repeat(n)
}

/// A list item marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    /// `-`, `+` or `*`.
    Bullet(u8),
    /// A number with `.` or `)`.
    Ordered {
        /// The number.
        number: u32,
        /// `.` or `)`.
        delimiter: u8,
    },
}

impl Marker {
    /// The marker text.
    pub fn text(self) -> String {
        match self {
            Marker::Bullet(c) => char::from(c).to_string(),
            Marker::Ordered { number, delimiter } => format!("{number}{}", char::from(delimiter)),
        }
    }

    /// The marker's width in columns.
    pub fn width(self) -> usize {
        self.text().len()
    }

    /// The number of an ordered marker.
    pub fn number(self) -> Option<u32> {
        match self {
            Marker::Ordered { number, .. } => Some(number),
            Marker::Bullet(_) => None,
        }
    }
}

/// A task box at the start of a list item's content: `[ ]`, `[x]`, ….
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskBox {
    /// `[`…`]`.
    pub range: Range<usize>,
    /// The status character's bytes.
    pub status: Range<usize>,
    /// Whether the status is a space.
    pub unchecked: bool,
    /// The box and the whitespace after it (up to the item's text).
    pub with_space: Range<usize>,
}

/// A list item and its marker line.
#[derive(Clone, Debug)]
pub(crate) struct ItemInfo {
    /// The line of the marker.
    pub marker_line: usize,
    /// The list-marker token: indentation, marker and padding.
    pub token: Range<usize>,
    /// The column where the token starts (the parent's content column on
    /// the marker line).
    pub base_col: usize,
    /// Columns of indentation before the marker.
    pub indent: usize,
    /// The marker bytes.
    pub marker_range: Range<usize>,
    /// The marker.
    pub marker: Marker,
    /// The padding N in columns.
    pub padding: usize,
    /// Indentation + W + N.
    pub content_offset: usize,
    /// Whether nothing follows the marker on its line.
    pub blank_start: bool,
    /// Where the item's content starts on the marker line.
    pub content_start: usize,
}

/// A list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ListInfo {
    /// Whether it is ordered.
    pub ordered: bool,
    /// The bullet character (bullet lists).
    pub bullet: u8,
    /// `.` or `)` (ordered lists).
    pub delimiter: u8,
}

/// The kind of a container level.
#[derive(Clone, Debug)]
pub(crate) enum LevelKind {
    /// A block quote.
    Quote,
    /// A list.
    List(ListInfo),
    /// A list item.
    Item(ItemInfo),
}

/// One container open on a line.
#[derive(Clone, Debug)]
pub(crate) struct Level {
    /// The container's range.
    pub range: Range<usize>,
    /// What it is.
    pub kind: LevelKind,
}

impl Level {
    /// The item data, for an item.
    pub fn item(&self) -> Option<&ItemInfo> {
        match &self.kind {
            LevelKind::Item(item) => Some(item),
            _ => None,
        }
    }

    /// The list data, for a list.
    pub fn list(&self) -> Option<ListInfo> {
        match &self.kind {
            LevelKind::List(list) => Some(*list),
            _ => None,
        }
    }

    /// Whether this is a block quote.
    pub fn is_quote(&self) -> bool {
        matches!(self.kind, LevelKind::Quote)
    }
}

/// A leaf block.
#[derive(Clone, Debug)]
pub(crate) struct LeafInfo {
    /// The block's identity (the key of the tree's inline cache).
    pub id: BlockId,
    /// What it is.
    pub kind: LeafKind,
    /// The block's range.
    pub range: Range<usize>,
    /// Its content lines.
    pub lines: Vec<LeafLine>,
    /// Its first line.
    pub first_line: usize,
    /// Its last line (the underline of a setext heading, the closing fence
    /// of a fenced block).
    pub last_line: usize,
}

impl LeafInfo {
    /// Whether inline content (emphasis, links, …) can be edited here.
    pub fn is_inline(&self) -> bool {
        matches!(self.kind, LeafKind::Paragraph | LeafKind::Heading { .. })
    }

    /// Whether this is a code, HTML or front-matter block.
    pub fn is_verbatim(&self) -> bool {
        matches!(
            self.kind,
            LeafKind::FencedCode
                | LeafKind::IndentedCode
                | LeafKind::Html { .. }
                | LeafKind::FrontMatter { .. }
        )
    }
}

/// The context of one line.
#[derive(Clone, Debug)]
pub(crate) struct LineCtx {
    /// The line.
    pub line: Line,
    /// The containers open at the line's content, outermost first.
    pub chain: Vec<Level>,
    /// Per level of `chain`: its prefix bytes on this line (lists have an
    /// empty one), or `None` once the line is lazy.
    pub parts: Vec<Option<Range<usize>>>,
    /// Where the prefixes end (for a lazy line: its first non-blank byte).
    pub content_start: usize,
    /// Whether some container's prefix is missing (lazy continuation).
    pub lazy: bool,
    /// Whether the line is blank after its prefixes.
    pub blank: bool,
    /// The leaf block the line belongs to.
    pub leaf: Option<LeafInfo>,
}

impl LineCtx {
    /// The index of the innermost list item in the chain.
    pub fn innermost_item(&self) -> Option<usize> {
        self.chain.iter().rposition(|level| level.item().is_some())
    }

    /// The index of the innermost block quote in the chain.
    pub fn innermost_quote(&self) -> Option<usize> {
        self.chain.iter().rposition(Level::is_quote)
    }

    /// Where container `index`'s region starts on this line: the end of
    /// the prefixes of the levels before it. `None` if the line is lazy
    /// before that level.
    pub fn region_start(&self, index: usize) -> Option<usize> {
        if index == 0 {
            return Some(self.line.start);
        }
        self.parts
            .get(index - 1)
            .cloned()
            .flatten()
            .map(|range| range.end)
    }

    /// What text inserted at `content_start` must start with: a space
    /// after a list marker that ends the line without one (`1.`).
    pub fn pad(&self) -> &'static str {
        match self.chain.last().and_then(Level::item) {
            Some(item)
                if item.marker_line == self.line.index
                    && item.content_start == item.marker_range.end =>
            {
                " "
            }
            _ => "",
        }
    }

    /// Whether the leaf is a code, HTML or front-matter block.
    pub fn in_verbatim(&self) -> bool {
        self.leaf.as_ref().is_some_and(LeafInfo::is_verbatim)
    }
}

/// Collects the blocks a range walk visits.
#[derive(Default)]
struct Collect {
    depth: usize,
    containers: Vec<(usize, Container)>,
    leaves: Vec<LeafInfo>,
}

impl Visitor for Collect {
    fn enter(&mut self, _: &Container) {
        self.depth += 1;
    }

    fn exit(&mut self, container: &Container) {
        self.depth = self.depth.saturating_sub(1);
        self.containers.push((self.depth, container.clone()));
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        self.leaves.push(LeafInfo {
            id: leaf.id,
            kind: leaf.kind,
            range: leaf.range.clone(),
            lines: leaf.lines.to_vec(),
            first_line: 0,
            last_line: 0,
        });
    }
}

/// The containers and leaves that intersect `range`: containers with
/// their depth.
pub(crate) fn blocks_in(
    doc: &Doc,
    range: Range<usize>,
) -> (Vec<(usize, Container)>, Vec<LeafInfo>) {
    let mut collect = Collect::default();
    doc.tree().walk_range(range, &mut collect);
    let mut leaves = collect.leaves;
    for leaf in &mut leaves {
        leaf.first_line = doc.line_of(leaf.range.start);
        leaf.last_line = doc.line_of(leaf.range.end.saturating_sub(1).max(leaf.range.start));
    }
    (collect.containers, leaves)
}

/// The context of line `index`.
pub(crate) fn line_ctx(doc: &Doc, index: usize) -> LineCtx {
    let line = doc.line(index);
    let tokens = doc
        .tree()
        .tokens(line.start..line.full_end().max(line.start + 1));
    // The first byte past the container prefixes, by the tokens.
    let probe = tokens
        .iter()
        .filter(|(range, _)| range.start >= line.start && range.start <= line.full_end())
        .find(|(_, token)| {
            !matches!(
                token.kind,
                TokenKind::ContainerPrefix | TokenKind::QuoteMarker | TokenKind::ListMarker
            )
        })
        .map_or(line.end(), |(range, _)| range.start);
    let len = doc.len();
    let (containers, leaves) = blocks_in(doc, line.start..line.full_end());
    let mut by_depth: Vec<Option<Container>> = Vec::new();
    for (depth, container) in containers {
        let open = container.range.start <= probe
            && (container.range.end > probe || (container.range.end == probe && probe == len));
        if !open {
            continue;
        }
        if by_depth.len() <= depth {
            by_depth.resize(depth + 1, None);
        }
        by_depth[depth] = Some(container);
    }
    let chain: Vec<Level> = by_depth
        .into_iter()
        .map_while(|container| container)
        .filter_map(|container| level_of(doc, container))
        .collect();
    let leaf = leaves
        .into_iter()
        .rfind(|leaf| leaf.first_line <= index && index <= leaf.last_line);
    let (parts, content_start, lazy, blank) = split_prefix(&line, &chain);
    LineCtx {
        line,
        chain,
        parts,
        content_start,
        lazy,
        blank,
        leaf,
    }
}

/// The context of the line holding `offset`.
pub(crate) fn ctx_at(doc: &Doc, offset: usize) -> LineCtx {
    line_ctx(doc, doc.line_of(offset))
}

fn level_of(doc: &Doc, container: Container) -> Option<Level> {
    let kind = match container.kind {
        ContainerKind::BlockQuote => LevelKind::Quote,
        ContainerKind::BulletList { marker, .. } => LevelKind::List(ListInfo {
            ordered: false,
            bullet: marker,
            delimiter: b'.',
        }),
        ContainerKind::OrderedList { delimiter, .. } => LevelKind::List(ListInfo {
            ordered: true,
            bullet: b'-',
            delimiter,
        }),
        ContainerKind::Item => LevelKind::Item(item_info(doc, container.range.start)?),
        // An alert continues like a block quote; a footnote definition's
        // indent is not a prefix the line commands write.
        ContainerKind::Alert { .. } => LevelKind::Quote,
        ContainerKind::FootnoteDefinition => return None,
    };
    Some(Level {
        range: container.range,
        kind,
    })
}

/// The marker geometry of the item whose list-marker token starts at
/// `start`.
pub(crate) fn item_info(doc: &Doc, start: usize) -> Option<ItemInfo> {
    let line = doc.line_at(start);
    let token_end = doc
        .tree()
        .tokens(start..start + 1)
        .into_iter()
        .find(|(range, token)| range.start == start && token.kind == TokenKind::ListMarker)
        .map(|(range, _)| range.end);
    let b = line.bytes();
    let t0 = line.rel(start);
    let base_col = col_after(&b[..t0], 0);
    let mut i = t0;
    let mut col = base_col;
    while i < b.len() && b[i] == b' ' {
        i += 1;
        col += 1;
    }
    let indent = col - base_col;
    let marker_start = i;
    let marker = match b.get(i) {
        Some(&c @ (b'-' | b'+' | b'*')) => {
            i += 1;
            Marker::Bullet(c)
        }
        Some(c) if c.is_ascii_digit() => {
            let digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            if digits > 9 {
                return None;
            }
            let number: u32 = line.text[i..i + digits].parse().ok()?;
            i += digits;
            let delimiter = *b.get(i).filter(|&&c| c == b'.' || c == b')')?;
            i += 1;
            Marker::Ordered { number, delimiter }
        }
        _ => return None,
    };
    let width = i - marker_start;
    col += width;
    let mut j = i;
    let mut pad_col = col;
    while j < b.len() && (b[j] == b' ' || b[j] == b'\t') {
        if b[j] == b'\t' {
            pad_col = tab_end(pad_col);
        } else {
            pad_col += 1;
        }
        j += 1;
    }
    let blank_start = j == b.len();
    let k = pad_col - col;
    let padding = if blank_start || k >= 5 { 1 } else { k.max(1) };
    let token_end = token_end.unwrap_or(line.start + if blank_start { b.len() } else { j });
    Some(ItemInfo {
        marker_line: line.index,
        token: start..token_end,
        base_col,
        indent,
        marker_range: line.start + marker_start..line.start + i,
        marker,
        padding,
        content_offset: indent + width + padding,
        blank_start,
        content_start: token_end,
    })
}

/// The task box at `item`'s content start, if the flavor has tasks.
pub(crate) fn task_box(doc: &Doc, item: &ItemInfo) -> Option<TaskBox> {
    if !doc.flavor.has_tasks() || item.blank_start {
        return None;
    }
    let line = doc.line(item.marker_line);
    let b = line.bytes();
    let at = line.rel(item.content_start);
    if b.get(at) != Some(&b'[') {
        return None;
    }
    let status = line.text.get(at + 1..)?.chars().next()?;
    let close = at + 1 + status.len_utf8();
    if b.get(close) != Some(&b']') {
        return None;
    }
    let known = matches!(status, ' ' | 'x' | 'X');
    if !known && !(doc.flavor.any_task_status() && !status.is_whitespace()) {
        return None;
    }
    let after = close + 1;
    if !matches!(b.get(after), None | Some(b' ' | b'\t')) {
        return None;
    }
    let mut end = after;
    while end < b.len() && (b[end] == b' ' || b[end] == b'\t') {
        end += 1;
    }
    Some(TaskBox {
        range: line.start + at..line.start + after,
        status: line.start + at + 1..line.start + close,
        unchecked: status == ' ',
        with_space: line.start + at..line.start + end,
    })
}

/// A block quote marker at `pos` (column `col`, with `virt` columns of a
/// partly consumed tab before it): up to three spaces, `>`, and an optional
/// space (or one column of a tab). Returns the position, column and
/// leftover tab columns after it.
fn quote_marker(b: &[u8], pos: usize, col: usize, virt: usize) -> Option<(usize, usize, usize)> {
    let mut p = pos;
    let mut c = col;
    let mut indent = virt;
    while p < b.len() && b[p] == b' ' && indent < 3 {
        p += 1;
        c += 1;
        indent += 1;
    }
    if b.get(p) != Some(&b'>') {
        return None;
    }
    p += 1;
    c += 1;
    let mut virt = 0;
    match b.get(p) {
        Some(b' ') => {
            p += 1;
            c += 1;
        }
        Some(b'\t') => {
            let w = tab_end(c) - c;
            p += 1;
            c += w;
            virt = w - 1;
        }
        _ => {}
    }
    Some((p, c, virt))
}

/// Splits the line's prefix by container (see [`LineCtx::parts`]).
fn split_prefix(line: &Line, chain: &[Level]) -> (Vec<Option<Range<usize>>>, usize, bool, bool) {
    let b = line.bytes();
    let mut parts = Vec::with_capacity(chain.len());
    let mut pos = 0;
    let mut col = 0;
    let mut virt = 0;
    let mut lazy = false;
    let opens_here = |range: &Range<usize>| range.start >= line.start && range.start <= line.end();
    for level in chain {
        let start = pos;
        match &level.kind {
            LevelKind::List(_) => parts.push(Some(line.start + pos..line.start + pos)),
            LevelKind::Quote => {
                let from = if opens_here(&level.range) {
                    line.rel(level.range.start).max(pos)
                } else {
                    pos
                };
                let from_col = col_after(&b[pos..from], col);
                match quote_marker(b, from, from_col, if from == pos { virt } else { 0 }) {
                    Some((p, c, v)) => {
                        parts.push(Some(line.start + start..line.start + p));
                        (pos, col, virt) = (p, c, v);
                    }
                    None => {
                        lazy = true;
                        break;
                    }
                }
            }
            LevelKind::Item(item) => {
                if item.marker_line == line.index {
                    let end = line.rel(item.token.end).max(pos);
                    let c = col_after(&b[pos..end], col);
                    let target = item.base_col + item.content_offset;
                    parts.push(Some(line.start + start..line.start + end));
                    (pos, col, virt) = (end, c, c.saturating_sub(target));
                } else if line.blank_from(pos) {
                    parts.push(Some(line.start + pos..line.start + pos));
                } else {
                    let need = item.content_offset;
                    let mut have = virt;
                    let mut p = pos;
                    let mut c = col;
                    while have < need && p < b.len() {
                        match b[p] {
                            b' ' => {
                                have += 1;
                                c += 1;
                            }
                            b'\t' => {
                                let w = tab_end(c) - c;
                                have += w;
                                c += w;
                            }
                            _ => break,
                        }
                        p += 1;
                    }
                    if have < need {
                        lazy = true;
                        break;
                    }
                    parts.push(Some(line.start + start..line.start + p));
                    (pos, col, virt) = (p, c, have - need);
                }
            }
        }
    }
    // The column state after the innermost level is not needed.
    let _ = (col, virt);
    while parts.len() < chain.len() {
        parts.push(None);
    }
    let mut content = pos;
    if lazy {
        while content < b.len() && (b[content] == b' ' || b[content] == b'\t') {
            content += 1;
        }
    }
    let blank = line.blank_from(pos);
    (parts, line.start + content, lazy, blank)
}

/// The direct child containers of the container with `range` at depth
/// `depth` (a list's items, an item's lists and quotes), in order.
pub(crate) fn child_containers(doc: &Doc, range: Range<usize>, depth: usize) -> Vec<Container> {
    let (containers, _) = blocks_in(doc, range.clone());
    let mut children: Vec<Container> = containers
        .into_iter()
        .filter(|(d, c)| {
            *d == depth + 1 && c.range.start >= range.start && c.range.end <= range.end
        })
        .map(|(_, c)| c)
        .collect();
    children.sort_by_key(|c| c.range.start);
    children
}

/// A list's items: each item's range and marker geometry.
pub(crate) type Items = Vec<(Range<usize>, ItemInfo)>;

/// The list whose range is `range`: its depth and items, in order.
pub(crate) fn list_items(doc: &Doc, range: &Range<usize>) -> Option<(usize, Items)> {
    let (containers, _) = blocks_in(doc, range.clone());
    let depth = containers
        .iter()
        .filter(|(_, c)| {
            c.range == *range
                && matches!(
                    c.kind,
                    ContainerKind::BulletList { .. } | ContainerKind::OrderedList { .. }
                )
        })
        .map(|(d, _)| *d)
        .min()?;
    let mut items: Vec<(Range<usize>, ItemInfo)> = containers
        .into_iter()
        .filter(|(d, c)| {
            *d == depth + 1
                && c.kind == ContainerKind::Item
                && c.range.start >= range.start
                && c.range.end <= range.end
        })
        .filter_map(|(_, c)| Some((c.range.clone(), item_info(doc, c.range.start)?)))
        .collect();
    items.sort_by_key(|(r, _)| r.start);
    Some((depth, items))
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The syntax tree: an immutable snapshot of the parse of one text
//! revision.
//!
//! The primary structure is a persistent sum tree over line records
//! (grouped into small chunks), each with its checkpoint and its slice of
//! the event stream ([`event`]). The nested block view, list tightness,
//! text-block resolution and the reference-definition map are derived from
//! the stream on demand ([`walk`]).

pub(crate) mod chunk;
pub mod event;
pub mod index;
pub(crate) mod range;
pub mod walk;

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

use gv_text::TextSource;
use sum_tree::{Bias, SumTree};

use crate::block::BlockParser;
use crate::block::front_matter;
use crate::block::state::{Interner, Kind, RefMode, StateId};
use crate::inline::api::{InlineInput, InlineTree, RefDef, RefLookup, parse_inlines};
use crate::options::ParseOptions;
use chunk::{Chunk, ChunkBuilder, ChunkData, Lines};
use event::{BlockId, BlockKind, Ev, Event, TokenKind};
use index::BlockIndex;

/// The link reference definitions of a document: normalized label to
/// definition, first definition wins.
#[derive(Debug, Default)]
pub struct RefMap {
    map: HashMap<String, RefDef>,
}

impl RefMap {
    /// Number of distinct labels.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether there are no definitions.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl RefLookup for RefMap {
    fn lookup(&self, normalized_label: &str) -> Option<&RefDef> {
        self.map.get(normalized_label)
    }
}

pub(crate) struct TreeData {
    pub chunks: SumTree<Chunk>,
    pub interner: Interner,
    pub next_id: u64,
    pub len: usize,
    /// Set on the interim trees of paused reparses.
    pub interim: bool,
    pub options: ParseOptions,
    pub ref_map: OnceLock<Arc<RefMap>>,
    pub block_index: OnceLock<Arc<BlockIndex>>,
    pub inlines: Mutex<HashMap<BlockId, Arc<InlineTree>>>,
}

/// The parse of one text revision. Cloning is O(1) (`Arc`); the tree is
/// immutable and can be shared between threads.
#[derive(Clone)]
pub struct SyntaxTree(pub(crate) Arc<TreeData>);

impl fmt::Debug for SyntaxTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyntaxTree")
            .field("len", &self.0.len)
            .field("lines", &self.line_count())
            .field("states", &self.0.interner.len())
            .field("interim", &self.0.interim)
            .field("options", &self.0.options)
            .finish_non_exhaustive()
    }
}

/// One line of the tree, for internal iteration.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LineView<'a> {
    pub index: usize,
    pub start: usize,
    pub len: usize,
    pub state: StateId,
    pub events: &'a [Ev],
    pub ids: &'a [BlockId],
}

/// A position in the line sequence that can be advanced cheaply.
#[derive(Clone, Debug)]
pub(crate) struct LineCursor {
    chunk: Option<Chunk>,
    chunk_first_line: usize,
    idx: usize,
    /// Global line index.
    pub global: usize,
    /// Byte offset of the line start.
    pub start: usize,
}

/// A block and its extent (see [`SyntaxTree::blocks`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BlockSpan {
    /// The block's identity.
    pub id: BlockId,
    /// Its kind as opened.
    pub kind: BlockKind,
    /// Nesting depth (0 for children of the document).
    pub depth: u32,
    /// Byte range from its `Open` to its `Close`.
    pub range: Range<usize>,
}

impl TreeData {
    pub(crate) fn new(
        chunks: SumTree<Chunk>,
        interner: Interner,
        next_id: u64,
        len: usize,
        interim: bool,
        options: ParseOptions,
    ) -> Self {
        TreeData {
            chunks,
            interner,
            next_id,
            len,
            interim,
            options,
            ref_map: OnceLock::new(),
            block_index: OnceLock::new(),
            inlines: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn line_count(&self) -> usize {
        self.chunks.summary().lines
    }

    /// A cursor at global line `line` (`line < line_count`).
    pub(crate) fn cursor_at_line(&self, line: usize) -> LineCursor {
        let mut c = self
            .chunks
            .cursor::<sum_tree::Dimensions<Lines, chunk::Bytes>>(());
        c.seek(&Lines(line), Bias::Right);
        let start = *c.start();
        let chunk = c.item().cloned();
        let mut cur = LineCursor {
            chunk,
            chunk_first_line: start.0.0,
            idx: line - start.0.0,
            global: line,
            start: start.1.0,
        };
        if let Some(ch) = &cur.chunk {
            cur.start += ch.0.lines[..cur.idx]
                .iter()
                .map(|l| l.len as usize)
                .sum::<usize>();
        }
        cur
    }

    /// A cursor at the line containing byte `offset`; the EOF line for
    /// `offset >= len`.
    pub(crate) fn cursor_at_byte(&self, offset: usize) -> LineCursor {
        if offset >= self.len {
            return self.cursor_at_line(self.line_count() - 1);
        }
        let mut c = self
            .chunks
            .cursor::<sum_tree::Dimensions<chunk::Bytes, Lines>>(());
        c.seek(&chunk::Bytes(offset), Bias::Right);
        let start = *c.start();
        let chunk = c.item().cloned();
        let mut cur = LineCursor {
            chunk,
            chunk_first_line: start.1.0,
            idx: 0,
            global: start.1.0,
            start: start.0.0,
        };
        if let Some(ch) = &cur.chunk {
            while cur.idx + 1 < ch.0.lines.len()
                && cur.start + ch.0.lines[cur.idx].len as usize <= offset
            {
                cur.start += ch.0.lines[cur.idx].len as usize;
                cur.idx += 1;
                cur.global += 1;
            }
        }
        cur
    }

    /// The line under the cursor (the cursor must be valid).
    #[inline]
    pub(crate) fn line<'a>(&self, cur: &'a LineCursor) -> Option<LineView<'a>> {
        let ch = cur.chunk.as_ref()?;
        let rec = ch.0.lines.get(cur.idx)?;
        let (events, ids) = ch.0.line_events(cur.idx);
        Some(LineView {
            index: cur.global,
            start: cur.start,
            len: rec.len as usize,
            state: rec.state,
            events,
            ids,
        })
    }

    /// Moves the cursor to the next line; false past the last line.
    pub(crate) fn advance(&self, cur: &mut LineCursor) -> bool {
        let Some(ch) = &cur.chunk else {
            return false;
        };
        cur.start += ch.0.lines[cur.idx].len as usize;
        cur.global += 1;
        cur.idx += 1;
        if cur.idx < ch.0.lines.len() {
            return true;
        }
        let next_first = cur.chunk_first_line + ch.0.lines.len();
        let mut c = self.chunks.cursor::<Lines>(());
        c.seek(&Lines(next_first), Bias::Right);
        cur.chunk = c.item().cloned();
        cur.chunk_first_line = next_first;
        cur.idx = 0;
        cur.chunk.is_some()
    }

    /// Calls `f` for every line in order.
    pub(crate) fn for_each_line(&self, mut f: impl FnMut(LineView<'_>)) {
        let mut index = 0;
        let mut start = 0;
        for ch in self.chunks.iter() {
            for (i, rec) in ch.0.lines.iter().enumerate() {
                let (events, ids) = ch.0.line_events(i);
                f(LineView {
                    index,
                    start,
                    len: rec.len as usize,
                    state: rec.state,
                    events,
                    ids,
                });
                index += 1;
                start += rec.len as usize;
            }
        }
    }
}

impl SyntaxTree {
    /// Parses `text` completely with the CommonMark options (block
    /// structure; inlines are parsed lazily, see [`SyntaxTree::inlines`]).
    pub fn parse<S: TextSource + ?Sized>(text: &S) -> SyntaxTree {
        Self::parse_with(text, ParseOptions::default())
    }

    /// Parses `text` completely with `options`. Incremental reparses of the
    /// result keep the options.
    pub fn parse_with<S: TextSource + ?Sized>(text: &S, options: ParseOptions) -> SyntaxTree {
        let mut interner = Interner::new();
        let mut p = BlockParser::new();
        let mut cb = ChunkBuilder::new(1, 0);
        let mut scratch = Vec::new();
        let n = text.len();
        let mut pos = 0;
        while pos < n {
            let state = p.checkpoint(&mut interner);
            let fm = if pos == 0 {
                front_matter::detect(text, options.front_matter)
            } else {
                None
            };
            let line = text.line(pos, &mut scratch);
            let len = line.len();
            if len == 0 {
                break;
            }
            p.process_line(line, pos == 0, fm, &mut interner, &mut cb.sink);
            cb.end_line(len, state);
            pos += len;
        }
        let state = p.checkpoint(&mut interner);
        p.finish(&mut cb.sink);
        cb.end_line(0, state);
        let (chunks, next_id) = cb.finish();
        SyntaxTree(Arc::new(TreeData::new(
            SumTree::from_iter(chunks, ()),
            interner,
            next_id,
            pos,
            false,
            options,
        )))
    }

    /// The options the tree was parsed with.
    pub fn options(&self) -> ParseOptions {
        self.0.options
    }

    /// Length of the parsed text in bytes.
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Whether the parsed text is empty.
    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// Number of lines, including the zero-length line at the end of the
    /// text whose events close the blocks still open there.
    pub fn line_count(&self) -> usize {
        self.0.line_count()
    }

    /// Whether this is the interim tree of a paused reparse (see
    /// [`crate::Reparse::Partial`]): the parse up to where the reparse
    /// stopped, followed by the previous tree's structure shifted into
    /// place.
    pub fn is_interim(&self) -> bool {
        self.0.interim
    }

    /// Number of distinct checkpoint states interned for this document.
    pub fn state_count(&self) -> usize {
        self.0.interner.len()
    }

    /// Number of chunks in the line sum tree.
    pub fn chunk_count(&self) -> usize {
        self.0.chunks.iter().count()
    }

    /// Total number of events (memory accounting).
    pub fn event_count(&self) -> usize {
        self.0.chunks.iter().map(|c| c.0.events.len()).sum()
    }

    /// Every event with the byte offset where it occurs, in order.
    pub fn events(&self) -> Vec<(usize, Event)> {
        let mut out = Vec::new();
        self.0.for_each_line(|l| {
            let mut off = l.start;
            let mut ids = l.ids.iter();
            for e in l.events {
                let id = if e.is_open() {
                    ids.next().copied().unwrap_or(BlockId(0))
                } else {
                    BlockId(0)
                };
                out.push((off, e.to_event(id)));
                off += e.tok_len();
            }
        });
        out
    }

    /// The tokens that overlap `range` (for an empty range: the token at
    /// its position), with their byte ranges, in order. For highlighting.
    pub fn tokens(&self, range: Range<usize>) -> Vec<(Range<usize>, event::Token)> {
        let end = range.end.max(range.start + 1);
        let mut out = Vec::new();
        let mut cur = self.0.cursor_at_byte(range.start);
        while let Some(l) = self.0.line(&cur) {
            if l.start >= end {
                break;
            }
            let mut off = l.start;
            for e in l.events {
                if let Some(t) = e.token() {
                    let r = off..off + t.len as usize;
                    if r.end > range.start && r.start < end {
                        out.push((r.clone(), t));
                    }
                    off = r.end;
                }
            }
            if !self.0.advance(&mut cur) {
                break;
            }
        }
        out
    }

    /// Every block with its extent, in document order (pre-order).
    pub fn blocks(&self) -> Vec<BlockSpan> {
        let mut out: Vec<BlockSpan> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        self.0.for_each_line(|l| {
            let mut off = l.start;
            let mut ids = l.ids.iter();
            for e in l.events {
                if let Some(kind) = e.open_kind() {
                    open.push(out.len());
                    out.push(BlockSpan {
                        id: ids.next().copied().unwrap_or(BlockId(0)),
                        kind,
                        depth: u32::try_from(open.len() - 1).unwrap_or(u32::MAX),
                        range: off..off,
                    });
                } else if e.is_close() {
                    if let Some(i) = open.pop() {
                        out[i].range.end = off;
                    }
                } else {
                    off += e.tok_len();
                }
            }
        });
        out
    }

    /// The link reference definitions (first definition of a label wins).
    /// Computed on first use and shared with later revisions whose edits
    /// did not touch a definition.
    pub fn ref_map<S: TextSource + ?Sized>(&self, text: &S) -> Arc<RefMap> {
        Arc::clone(
            self.0
                .ref_map
                .get_or_init(|| Arc::new(self.build_ref_map(text))),
        )
    }

    fn build_ref_map<S: TextSource + ?Sized>(&self, text: &S) -> RefMap {
        let mut map = RefMap::default();
        if self.0.chunks.summary().refdefs == 0 {
            return map;
        }
        walk::walk(
            self,
            &mut walk::LeafFn(|leaf: &walk::Leaf<'_>| {
                if leaf.kind != walk::LeafKind::LinkRefDef {
                    return;
                }
                if let Some(d) = leaf.link_ref_def(text) {
                    map.map.entry(d.label).or_insert(d.def);
                }
            }),
        );
        map
    }

    /// The inline tree of a paragraph or heading, parsed on first use and
    /// cached for this revision by BlockId.
    pub fn inlines<S: TextSource + ?Sized>(
        &self,
        leaf: &walk::Leaf<'_>,
        text: &S,
    ) -> Arc<InlineTree> {
        if let Ok(cache) = self.0.inlines.lock()
            && let Some(t) = cache.get(&leaf.id)
        {
            return Arc::clone(t);
        }
        let refs = self.ref_map(text);
        let segments = leaf.segments();
        let tree = Arc::new(parse_inlines(
            text,
            InlineInput {
                segments: &segments,
            },
            &*refs,
        ));
        if let Ok(mut cache) = self.0.inlines.lock() {
            cache.insert(leaf.id, Arc::clone(&tree));
        }
        tree
    }

    /// Checks the tree against `text`: tokens cover every byte exactly once
    /// in order, token kinds match their bytes, line records match the
    /// text's lines, blocks nest, BlockIds are unique. For tests and
    /// debugging.
    pub fn validate<S: TextSource + ?Sized>(&self, text: &S) -> Result<(), String> {
        let t = &self.0;
        if t.len != text.len() {
            return Err(format!("length {} vs text {}", t.len, text.len()));
        }
        let mut bytes = Vec::new();
        text.copy_range(0..text.len(), &mut bytes);
        let mut err = None;
        let mut depth: i64 = 0;
        let mut total = 0;
        let mut ids = std::collections::HashSet::new();
        let line_count = t.line_count();
        let mut running_depth = 0u32;
        for ch in t.chunks.iter() {
            let c: &ChunkData = &ch.0;
            if c.lines.is_empty() {
                return Err(String::from("empty chunk"));
            }
            if c.bytes != c.lines.iter().map(|l| l.len as usize).sum::<usize>() {
                return Err(String::from("chunk byte count out of sync"));
            }
            if c.start_depth != running_depth {
                return Err(format!(
                    "chunk start depth {} but the events before it end at depth {running_depth}",
                    c.start_depth
                ));
            }
            running_depth = c.check()?;
        }
        t.for_each_line(|l| {
            if err.is_some() {
                return;
            }
            let line = &bytes[l.start.min(bytes.len())..(l.start + l.len).min(bytes.len())];
            let expected_len = if l.start < bytes.len() {
                let mut scratch = Vec::new();
                text.line(l.start, &mut scratch).len()
            } else {
                0
            };
            if l.len != expected_len {
                err = Some(format!(
                    "line {}: length {} but the text's line has {}",
                    l.index, l.len, expected_len
                ));
                return;
            }
            if (l.len == 0) != (l.index + 1 == line_count) {
                err = Some(format!("line {}: empty line that is not the last", l.index));
                return;
            }
            if l.events.iter().filter(|e| e.is_open()).count() != l.ids.len() {
                err = Some(format!("line {}: id count mismatch", l.index));
                return;
            }
            for id in l.ids {
                if !ids.insert(*id) {
                    err = Some(format!("line {}: duplicate BlockId {id:?}", l.index));
                    return;
                }
            }
            let mut off = l.start;
            let mut seen_le = false;
            for e in l.events {
                if e.is_open() {
                    depth += 1;
                } else if e.is_close() {
                    depth -= 1;
                    if depth < 0 {
                        err = Some(format!("line {}: unbalanced close", l.index));
                        return;
                    }
                } else if let Some(tk) = e.token() {
                    if seen_le {
                        err = Some(format!("line {}: token after the line ending", l.index));
                        return;
                    }
                    let end = off + tk.len as usize;
                    if end > l.start + l.len {
                        err = Some(format!("line {}: tokens overrun the line", l.index));
                        return;
                    }
                    let s = &bytes[off..end];
                    if tk.len == 0 && tk.virtual_spaces == 0 {
                        err = Some(format!("line {}: empty {:?} token", l.index, tk.kind));
                        return;
                    }
                    let ok = match tk.kind {
                        TokenKind::Bom => off == 0 && s == b"\xEF\xBB\xBF",
                        TokenKind::Whitespace | TokenKind::CodeIndent => {
                            s.iter().all(|&c| c == b' ' || c == b'\t')
                        }
                        TokenKind::ContainerPrefix => {
                            s.iter().all(|&c| matches!(c, b' ' | b'\t' | b'>'))
                        }
                        TokenKind::QuoteMarker => s.contains(&b'>'),
                        TokenKind::ListMarker => s
                            .iter()
                            .any(|c| matches!(c, b'-' | b'+' | b'*' | b'.' | b')')),
                        TokenKind::Marker => s.iter().all(|c| b"#=-*_`~+. \t".contains(c)),
                        TokenKind::InfoString | TokenKind::Content => {
                            !s.contains(&b'\n') && !s.contains(&b'\r')
                        }
                        TokenKind::LineEnding => {
                            seen_le = true;
                            s == b"\n" || s == b"\r\n" || s == b"\r"
                        }
                    };
                    if !ok {
                        err = Some(format!(
                            "line {}: bad {:?} token {:?}",
                            l.index,
                            tk.kind,
                            String::from_utf8_lossy(s)
                        ));
                        return;
                    }
                    off = end;
                }
            }
            if off != l.start + l.len {
                err = Some(format!(
                    "line {}: tokens cover {} of {} bytes ({:?})",
                    l.index,
                    off - l.start,
                    l.len,
                    String::from_utf8_lossy(line)
                ));
                return;
            }
            let has_terminator = line.last().is_some_and(|&c| c == b'\n' || c == b'\r');
            if has_terminator != seen_le {
                err = Some(format!("line {}: line-ending token mismatch", l.index));
                return;
            }
            total += l.len;
        });
        if let Some(e) = err {
            return Err(e);
        }
        if total != bytes.len() {
            return Err(format!("lines cover {} of {} bytes", total, bytes.len()));
        }
        if depth != 0 {
            return Err(format!("{depth} blocks left open"));
        }
        Ok(())
    }

    /// Structural equality with another parse of the same text, ignoring
    /// BlockIds: same lines, same checkpoints (compared as resolved stacks,
    /// so trees with different interners compare), same events. For tests.
    pub fn same_structure(&self, other: &SyntaxTree) -> Result<(), String> {
        let (a, b) = (&self.0, &other.0);
        if a.options != b.options {
            return Err(format!(
                "options differ: {:?} vs {:?}",
                a.options, b.options
            ));
        }
        if a.len != b.len || a.line_count() != b.line_count() {
            return Err(format!(
                "size differs: len {} vs {}, lines {} vs {}",
                a.len,
                b.len,
                a.line_count(),
                b.line_count()
            ));
        }
        let mut xs = a.cursor_at_line(0);
        let mut ys = b.cursor_at_line(0);
        let mut cache: HashMap<(StateId, StateId), bool> = HashMap::new();
        while let (Some(x), Some(y)) = (a.line(&xs), b.line(&ys)) {
            if x.len != y.len {
                return Err(format!("line {}: length {} vs {}", x.index, x.len, y.len));
            }
            let same = *cache.entry((x.state, y.state)).or_insert_with(|| {
                comparable_state(&a.interner, x.state) == comparable_state(&b.interner, y.state)
            });
            if !same {
                return Err(format!(
                    "line {}: checkpoint differs:\n  {:?}\n  {:?}",
                    x.index,
                    a.interner.resolve(x.state).0,
                    b.interner.resolve(y.state).0
                ));
            }
            let same_events = x.events.len() == y.events.len()
                && x.events.iter().zip(y.events).all(|(p, q)| p == q);
            if !same_events {
                return Err(format!(
                    "line {}: events differ:\n  {:?}\n  {:?}",
                    x.index,
                    x.events
                        .iter()
                        .map(|e| e.to_event(BlockId(0)))
                        .collect::<Vec<_>>(),
                    y.events
                        .iter()
                        .map(|e| e.to_event(BlockId(0)))
                        .collect::<Vec<_>>()
                ));
            }
            let (ma, mb) = (a.advance(&mut xs), b.advance(&mut ys));
            if !ma || !mb {
                break;
            }
        }
        Ok(())
    }
}

/// A checkpoint in a form that compares across interners: the resolved
/// stack, with the pending definition text (whose id is interner-specific)
/// taken out of the top entry.
fn comparable_state(
    interner: &Interner,
    state: StateId,
) -> (Vec<crate::block::state::Entry>, Option<Vec<u8>>) {
    let (mut entries, _) = interner.resolve(state);
    let mut text = None;
    if let Some(top) = entries.last_mut()
        && let Kind::Paragraph {
            refs: RefMode::Pending(t),
        } = top.kind
    {
        text = Some(interner.text(t).to_vec());
        top.kind = Kind::Paragraph {
            refs: RefMode::Pending(0),
        };
    }
    (entries, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_basics() {
        let text = "# Title\n\n> - a\n>   b\n\n```rust\nfn x() {}\n```\n";
        let tree = SyntaxTree::parse(text);
        tree.validate(text).expect("valid");
        assert_eq!(tree.len(), text.len());
        assert_eq!(tree.line_count(), 9);
        let kinds: Vec<BlockKind> = tree.blocks().iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            [
                BlockKind::AtxHeading { level: 1 },
                BlockKind::BlockQuote,
                BlockKind::BulletList { marker: b'-' },
                BlockKind::Item,
                BlockKind::Text,
                BlockKind::FencedCode,
            ]
        );
        let toks = tree.tokens(18..19);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].1.kind, TokenKind::ContainerPrefix);
        assert_eq!(&text[toks[0].0.clone()], ">   ");
        let at = tree.tokens(2..2);
        assert_eq!(at[0].1.kind, TokenKind::Content);
        let all: usize = tree.tokens(0..text.len()).iter().map(|t| t.0.len()).sum();
        assert_eq!(all, text.len());
        let n_events = tree.events().len();
        assert_eq!(n_events, tree.event_count());
    }

    #[test]
    fn inline_cache_and_ref_map() {
        let text = "[a]: /u\n\nsee [a]\n";
        let tree = SyntaxTree::parse(text);
        assert_eq!(tree.ref_map(text).len(), 1);
        let mut paragraphs = 0;
        walk::walk(
            &tree,
            &mut walk::LeafFn(|leaf: &walk::Leaf<'_>| {
                if leaf.kind == walk::LeafKind::Paragraph {
                    paragraphs += 1;
                    let a = tree.inlines(leaf, text);
                    let b = tree.inlines(leaf, text);
                    assert!(Arc::ptr_eq(&a, &b), "cached");
                    let mut html = String::new();
                    crate::inline::api::render_inlines_html(&a, &mut html);
                    assert_eq!(html, "see <a href=\"/u\">a</a>");
                }
            }),
        );
        assert_eq!(paragraphs, 1);
    }
}

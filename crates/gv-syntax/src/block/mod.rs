// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The line-by-line block pass: CommonMark's block phase with cmark
//! semantics, one line at a time.
//!
//! [`BlockParser::process_line`] is a function of the parser state at the
//! line start (the open-block stack, see [`state`]) and the line's bytes.
//! Nothing else survives from one line to the next, which is what makes a
//! checkpoint comparison a sound convergence test for incremental
//! reparsing.
//!
//! Per line, events are emitted in this order:
//! 1. `Close` events for blocks the line does not continue (start-of-line
//!    closes), with the markers those closes decide;
//! 2. the container prefix of the blocks it does continue, as one token;
//! 3. `Open` events and marker tokens of new blocks, then the leaf's tokens;
//! 4. `Close` events for blocks that end on this line (closing fence, HTML
//!    end condition).
//!
//! # Linear time
//!
//! Every step is O(bytes of the line), amortized over opens and closes,
//! except the walk over open containers on a blank line (each list item
//! continues without consuming anything). That walk skips whole runs of
//! such containers through a sorted index of the entries that can stop it,
//! so a blank line costs O(log depth). `last_line_blank` flags are kept on
//! at most one open block, so updating them is O(1).
//!
//! # Nesting cap
//!
//! At most [`MAX_NESTING`] blocks can be open at once. A block quote marker
//! or list marker that would open a container beyond it is not recognized
//! and stays paragraph text. The block pass needs no cap for its own time
//! bounds (see above); the cap bounds memory per input byte, the depth of
//! the checkpoint stacks restored on every reparse, and the work of
//! consumers that walk ancestors.

pub(crate) mod front_matter;
pub(crate) mod scan;
pub(crate) mod state;

use crate::inline::api::parse_ref_def;
use crate::table;
use crate::tree::event::{BlockKind, Ev, FrontMatterKind, Sink, TokenKind};
use state::{Closed, Entry, Interner, Kind, ROOT, RefMode, StateId, TABLE_VISITED};

/// Maximum number of simultaneously open blocks, the document included.
/// Deeper block quote and list markers are paragraph text.
pub const MAX_NESTING: usize = 1 << 20;

const TAB_STOP: usize = 4;
const CODE_INDENT: usize = 4;

/// Above this size, a pending reference-definition candidate is no longer
/// checked on every line (that would be quadratic): it is decided when the
/// paragraph closes, and lines with such a candidate never serve as
/// convergence points.
const PENDING_LIMIT: usize = 1024;

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Offset and column bookkeeping within one line (cmark's parser fields).
struct Cur<'l> {
    b: &'l [u8],
    /// End of the line's content (before the terminator).
    content_end: usize,
    offset: usize,
    column: usize,
    fns: usize,
    fns_col: usize,
    indent: usize,
    blank: bool,
    partial: bool,
    /// Bytes before `tp` already belong to emitted (or pending) tokens.
    tp: usize,
}

impl<'l> Cur<'l> {
    fn new(b: &'l [u8]) -> Self {
        let mut end = b.len();
        if end > 0 && b[end - 1] == b'\n' {
            end -= 1;
        }
        if end > 0 && b[end - 1] == b'\r' {
            end -= 1;
        }
        Cur {
            b,
            content_end: end,
            offset: 0,
            column: 0,
            fns: 0,
            fns_col: 0,
            indent: 0,
            blank: false,
            partial: false,
            tp: 0,
        }
    }

    #[inline]
    fn peek(&self, i: usize) -> Option<u8> {
        self.b.get(i).copied()
    }

    fn find_first_nonspace(&mut self) {
        let mut chars_to_tab = TAB_STOP - (self.column % TAB_STOP);
        if self.fns <= self.offset {
            self.fns = self.offset;
            self.fns_col = self.column;
            loop {
                match self.b.get(self.fns) {
                    Some(b' ') => {
                        self.fns += 1;
                        self.fns_col += 1;
                        chars_to_tab -= 1;
                        if chars_to_tab == 0 {
                            chars_to_tab = TAB_STOP;
                        }
                    }
                    Some(b'\t') => {
                        self.fns += 1;
                        self.fns_col += chars_to_tab;
                        chars_to_tab = TAB_STOP;
                    }
                    _ => break,
                }
            }
        }
        self.indent = self.fns_col - self.column;
        self.blank = self.b.get(self.fns).is_none_or(|&c| scan::is_line_end(c));
    }

    fn advance(&mut self, mut count: usize, columns: bool) {
        while count > 0 && self.offset < self.b.len() {
            if self.b[self.offset] == b'\t' {
                let chars_to_tab = TAB_STOP - (self.column % TAB_STOP);
                if columns {
                    self.partial = chars_to_tab > count;
                    let adv = chars_to_tab.min(count);
                    self.column += adv;
                    if !self.partial {
                        self.offset += 1;
                    }
                    count -= adv;
                } else {
                    self.partial = false;
                    self.column += chars_to_tab;
                    self.offset += 1;
                    count -= 1;
                }
            } else {
                self.partial = false;
                self.offset += 1;
                self.column += 1;
                count -= 1;
            }
        }
    }

    /// End of the bytes consumed so far; a partially consumed tab counts as
    /// consumed.
    #[inline]
    fn prefix_end(&self) -> usize {
        if self.partial {
            self.offset + 1
        } else {
            self.offset
        }
    }

    /// Virtual spaces that precede content when a tab is partially
    /// consumed.
    #[inline]
    fn virt(&self) -> u8 {
        if self.partial {
            (TAB_STOP - self.column % TAB_STOP) as u8
        } else {
            0
        }
    }
}

fn trim_end(b: &[u8], start: usize, mut end: usize) -> usize {
    while end > start && scan::is_space_or_tab(b[end - 1]) {
        end -= 1;
    }
    end
}

fn skip_ws(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && scan::is_space_or_tab(b[i]) {
        i += 1;
    }
    i
}

/// `u32` length of a token (lines longer than 4 GiB are not supported).
#[inline]
fn len32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Number of lines in a definition's text (every line ends with `\n`; a
/// partial last line counts too).
fn count_lines(s: &[u8]) -> u32 {
    let n = memchr::memchr_iter(b'\n', s).count();
    let partial = usize::from(s.last().is_some_and(|&c| c != b'\n'));
    len32(n + partial)
}

/// Whether the first line of a paragraph rules out a link reference
/// definition at its start: the label closes on this line and is not
/// followed by `:`, holds an unescaped `[`, or is blank. Conservative: it
/// never rules out a definition that a full parse would accept.
fn label_rules_out(line: &[u8]) -> bool {
    debug_assert_eq!(line.first(), Some(&b'['));
    let mut i = 1;
    let mut nonblank = false;
    while i < line.len() {
        match line[i] {
            b'\\' => {
                nonblank = true;
                i += if line.get(i + 1).is_some_and(u8::is_ascii_punctuation) {
                    2
                } else {
                    1
                };
            }
            b'[' => return true,
            b']' => return !nonblank || line.get(i + 1) != Some(&b':'),
            c => {
                nonblank |= !scan::is_space_or_tab(c);
                i += 1;
            }
        }
    }
    false
}

/// Parses the definitions at the start of `text` (each line ends with
/// `\n`). Returns the line count of each and the bytes they cover.
fn extract_defs(text: &[u8], out: &mut Vec<u32>) -> usize {
    out.clear();
    let Ok(s) = std::str::from_utf8(text) else {
        return 0;
    };
    let mut pos = 0;
    while pos < s.len() && s.as_bytes()[pos] == b'[' {
        match parse_ref_def(&s[pos..]) {
            Some((c, ..)) if c > 0 && pos + c <= s.len() => {
                // A definition ends after a line ending; round a partial
                // line up so the next one starts at a line start.
                let mut end = pos + c;
                if text[end - 1] != b'\n' {
                    end = memchr::memchr(b'\n', &text[end..]).map_or(text.len(), |i| end + i + 1);
                }
                out.push(count_lines(&text[pos..end]));
                pos = end;
            }
            _ => break,
        }
    }
    pos
}

/// Whether `text` is exactly one complete definition.
fn is_one_complete_def(text: &[u8]) -> bool {
    std::str::from_utf8(text)
        .ok()
        .and_then(parse_ref_def)
        .is_some_and(|(c, ..)| c == text.len())
}

/// The block parser. See the module documentation.
#[derive(Clone, Debug)]
pub(crate) struct BlockParser {
    stack: Vec<Entry>,
    /// `ids[i]`: interned id of `stack[..=i]`, valid for `i < clean`.
    ids: Vec<StateId>,
    clean: usize,
    /// Ascending indices of the entries that are not blank-transparent
    /// (see [`Entry::blank_transparent`]).
    stoppers: Vec<u32>,
    /// The entry whose `lb` flag is set, if any.
    lb_index: Option<usize>,
    /// Text of the pending reference-definition candidate of the open
    /// paragraph (each line followed by `\n`).
    pending: Vec<u8>,
    defs: Vec<u32>,
    /// Container prefix tokens of the current line, held back until the
    /// start-of-line closes are known.
    pre: Vec<(TokenKind, u32)>,
    flushed: bool,
    all_closed: bool,
    last_matched: usize,
    /// Stack entries at index >= `first_new` were opened on this line.
    first_new: usize,
    /// cmark's `thematic_break_kill_pos`.
    tb_kill: usize,
    /// While unmatched blocks close before a block start: the stack index
    /// of the block that will contain the new block. cmark finalizes the
    /// blocks between it and the last matched one *before* the unmatched
    /// ones; `usize::MAX` otherwise.
    backup: usize,
    /// GFM tables are on.
    tables: bool,
}

impl Default for BlockParser {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockParser {
    pub(crate) fn new() -> Self {
        BlockParser {
            stack: vec![Entry::new(Kind::Document)],
            ids: vec![ROOT],
            clean: 1,
            stoppers: Vec::new(),
            lb_index: None,
            pending: Vec::new(),
            defs: Vec::new(),
            pre: Vec::new(),
            flushed: false,
            all_closed: true,
            last_matched: 0,
            first_new: 1,
            tb_kill: 0,
            backup: usize::MAX,
            tables: false,
        }
    }

    /// A parser at the document start; `tables` turns GFM tables on.
    pub(crate) fn with_tables(tables: bool) -> Self {
        BlockParser {
            tables,
            ..Self::new()
        }
    }

    /// Restores the parser from a checkpoint. If the state has an open
    /// paragraph with a long pending candidate
    /// ([`BlockParser::needs_pending_text`]), the caller must supply it with
    /// [`BlockParser::set_pending_text`].
    pub(crate) fn from_checkpoint(interner: &Interner, id: StateId, tables: bool) -> Self {
        let (stack, ids) = interner.resolve(id);
        let clean = stack.len();
        let mut p = BlockParser {
            stack,
            ids,
            clean,
            tables,
            ..Self::new()
        };
        for (i, e) in p.stack.iter().enumerate().skip(1) {
            if !e.blank_transparent() {
                p.stoppers.push(len32(i));
            }
            if e.lb {
                p.lb_index = Some(i);
            }
        }
        if let Some(Entry {
            kind:
                Kind::Paragraph {
                    refs: RefMode::Pending(t),
                    ..
                },
            ..
        }) = p.stack.last()
        {
            p.pending.extend_from_slice(interner.text(*t));
        }
        p
    }

    /// Whether the open paragraph's pending definition text must be rebuilt
    /// from the document (it is too long to be part of the checkpoint).
    pub(crate) fn needs_pending_text(&self) -> bool {
        matches!(
            self.stack.last().map(|e| e.kind),
            Some(Kind::Paragraph {
                refs: RefMode::Long,
                ..
            })
        )
    }

    /// Sets the pending definition text after a restart (see
    /// [`BlockParser::needs_pending_text`]).
    pub(crate) fn set_pending_text(&mut self, text: Vec<u8>) {
        self.pending = text;
    }

    /// Interns the current state and returns its id.
    pub(crate) fn checkpoint(&mut self, interner: &mut Interner) -> StateId {
        let len = self.stack.len();
        for j in self.clean.max(1)..len {
            let parent = self.ids[j - 1];
            let pk = Some(self.stack[j - 1].kind);
            self.ids[j] = interner.intern(parent, self.stack[j].canonical(pk));
        }
        self.clean = len;
        self.ids[len - 1]
    }

    #[inline]
    fn touch(&mut self, i: usize) {
        if i < self.clean {
            self.clean = i;
        }
    }

    #[inline]
    fn push_entry(&mut self, e: Entry) {
        let i = self.stack.len();
        if !e.blank_transparent() {
            self.stoppers.push(len32(i));
        }
        self.stack.push(e);
        self.ids.push(ROOT);
    }

    #[inline]
    fn pop_entry(&mut self) -> Entry {
        let i = self.stack.len() - 1;
        let e = self.stack[i];
        self.stack.pop();
        self.ids.pop();
        if self.stoppers.last() == Some(&len32(i)) {
            self.stoppers.pop();
        }
        if self.lb_index == Some(i) {
            self.lb_index = None;
        }
        self.first_new = self.first_new.min(self.stack.len());
        if self.clean > self.stack.len() {
            self.clean = self.stack.len();
        }
        e
    }

    /// Sets `lb` of an entry that stays open (tracked in `lb_index`).
    #[inline]
    fn set_lb(&mut self, i: usize, v: bool) {
        if self.stack[i].lb != v {
            self.stack[i].lb = v;
            // Only children of lists and items keep the flag in checkpoints.
            if i > 0 && self.stack[i - 1].kind.is_list_or_item() {
                self.touch(i);
            }
        }
        if v {
            self.lb_index = Some(i);
        } else if self.lb_index == Some(i) {
            self.lb_index = None;
        }
    }

    // ----- event emission -----

    fn pre_tok(&mut self, cur: &mut Cur<'_>, kind: TokenKind) {
        let end = cur.prefix_end();
        if end > cur.tp {
            let len = len32(end - cur.tp);
            match self.pre.last_mut() {
                Some((k, n)) if *k == kind => *n += len,
                _ => self.pre.push((kind, len)),
            }
            cur.tp = end;
        }
    }

    #[inline]
    fn flush(&mut self, sink: &mut Sink) {
        if !self.flushed {
            self.flushed = true;
            for (k, n) in self.pre.drain(..) {
                sink.push(Ev::tok(k, 0, n));
            }
        }
    }

    fn tok(&mut self, cur: &mut Cur<'_>, sink: &mut Sink, kind: TokenKind, end: usize) {
        self.flush(sink);
        if end > cur.tp {
            sink.push(Ev::tok(kind, 0, len32(end - cur.tp)));
            cur.tp = end;
        }
    }

    // ----- block bookkeeping -----

    fn emit_defs(&mut self, sink: &mut Sink) {
        for &d in &self.defs {
            sink.push(Ev::refdef(d));
        }
    }

    /// Decides the pending definitions of the paragraph at `idx` (it closes
    /// or meets a setext underline). Returns whether paragraph text
    /// remains.
    fn settle_paragraph(&mut self, idx: usize, sink: &mut Sink) -> bool {
        let Kind::Paragraph { refs, cells } = self.stack[idx].kind else {
            return true;
        };
        match refs {
            RefMode::Text => true,
            RefMode::Defs => false,
            RefMode::Pending(_) | RefMode::Long => {
                let mut defs = std::mem::take(&mut self.defs);
                let n = extract_defs(&self.pending, &mut defs);
                self.defs = defs;
                self.flush(sink);
                self.emit_defs(sink);
                let has_text = n < self.pending.len();
                self.pending.clear();
                self.stack[idx].kind = Kind::Paragraph {
                    refs: if has_text {
                        RefMode::Text
                    } else {
                        RefMode::Defs
                    },
                    cells,
                };
                self.touch(idx);
                if has_text {
                    self.resolve_deferred(idx, sink);
                }
                has_text
            }
        }
    }

    fn close_top(&mut self, sink: &mut Sink) {
        let top = self.stack.len() - 1;
        let vanishes = matches!(self.stack[top].kind, Kind::Paragraph { .. })
            && !self.settle_paragraph(top, sink);
        let e = self.pop_entry();
        sink.push(Ev::CLOSE);
        let p = self.stack.len() - 1;
        if vanishes {
            // A text block of only link reference definitions is no
            // structural element (cmark removes it): the item's previous
            // child is its last child again. Unless the item's list is
            // finalized first (a block start backs up past it), which then
            // still sees the text block as a sibling.
            let list = p.wrapping_sub(1);
            if matches!(self.stack[p].kind, Kind::Item { .. })
                && list <= self.last_matched
                && self.backup != usize::MAX
                && list > self.backup
            {
                if e.last.is_some_and(Closed::ebl) {
                    // The item is the innermost open block.
                    sink.push(Ev::loosen(1));
                }
            } else if let Kind::Item { .. } = self.stack[p].kind {
                self.stack[p].has_child = e.has_child;
                self.stack[p].last = e.last;
                if !e.has_child {
                    self.stoppers.push(len32(p));
                }
                self.touch(p);
            }
        } else if self.stack[p].kind.is_list_or_item() {
            let li = e.kind.is_list_or_item();
            self.stack[p].last = Some(Closed {
                lb: e.lb,
                ebl_tail: li && e.last.is_some_and(Closed::ebl),
                li,
            });
            self.touch(p);
        }
    }

    fn close_unmatched(&mut self, sink: &mut Sink) {
        if !self.all_closed {
            while self.stack.len() - 1 > self.last_matched {
                self.close_top(sink);
            }
            self.all_closed = true;
        }
    }

    /// [`BlockParser::close_unmatched`] before a block of kind `next`
    /// starts.
    fn close_unmatched_for(&mut self, next: Kind, sink: &mut Sink) {
        if self.all_closed {
            return;
        }
        let mut b = self.last_matched;
        while b > 0 && !self.stack[b].kind.can_contain(next) {
            b -= 1;
        }
        self.backup = b;
        self.close_unmatched(sink);
        self.backup = usize::MAX;
    }

    /// cmark `add_child`: closes blocks that cannot contain `kind`, records
    /// list looseness decided by the new sibling, and opens the block.
    fn add_child(&mut self, kind: Kind, bk: BlockKind, sink: &mut Sink) -> usize {
        self.add_child_ex(kind, bk, sink, false)
    }

    /// [`BlockParser::add_child`]; `defer` marks a text block that may turn
    /// out to hold only link reference definitions. In an item it does not
    /// count as a sibling until it has text: the item's previous child
    /// summary is stashed in the new entry (`has_child`, `last`), and the
    /// looseness check runs when the text appears
    /// ([`BlockParser::resolve_deferred`]).
    fn add_child_ex(&mut self, kind: Kind, bk: BlockKind, sink: &mut Sink, defer: bool) -> usize {
        while !self.stack[self.stack.len() - 1].kind.can_contain(kind) {
            self.close_top(sink);
        }
        let p = self.stack.len() - 1;
        let mut entry = Entry::new(kind);
        match self.stack[p].kind {
            Kind::Item { .. } => {
                if defer {
                    entry.has_child = self.stack[p].has_child;
                    entry.last = self.stack[p].last;
                } else if self.stack[p].last.is_some_and(Closed::ebl) {
                    // The previous child gets a next sibling: its
                    // `ends_with_blank_line` counts.
                    self.flush(sink);
                    sink.push(Ev::loosen(1));
                }
                if !self.stack[p].has_child {
                    self.stack[p].has_child = true;
                    // A childless item is a stopper and is the top entry.
                    if self.stoppers.last() == Some(&len32(p)) {
                        self.stoppers.pop();
                    }
                }
                self.stack[p].last = None;
                self.touch(p);
            }
            Kind::List { .. } => {
                // The previous item gets a next sibling: its own flag and its
                // last child's `ends_with_blank_line` count.
                if self.stack[p].last.is_some_and(|s| s.lb || s.ebl_tail) {
                    self.flush(sink);
                    sink.push(Ev::loosen(0));
                }
                self.stack[p].last = None;
                self.touch(p);
            }
            _ => {}
        }
        self.flush(sink);
        sink.open(bk);
        self.push_entry(entry);
        self.stack.len() - 1
    }

    /// The text block at `idx`, opened with a stash (see
    /// [`BlockParser::add_child_ex`]), has text: it counts as a sibling now.
    fn resolve_deferred(&mut self, idx: usize, sink: &mut Sink) {
        if idx > 0 && matches!(self.stack[idx - 1].kind, Kind::Item { .. }) {
            if self.stack[idx].last.is_some_and(Closed::ebl) {
                // The text block is the innermost open block.
                debug_assert_eq!(idx + 1, self.stack.len());
                self.flush(sink);
                sink.push(Ev::loosen(2));
            }
            if self.stack[idx].has_child || self.stack[idx].last.is_some() {
                self.stack[idx].has_child = false;
                self.stack[idx].last = None;
                self.touch(idx);
            }
        }
    }

    /// The reference-definition mode of a candidate that starts with
    /// `line` (`pending` is empty).
    fn start_candidate(&mut self, line: &[u8], interner: &mut Interner) -> RefMode {
        if line.first() == Some(&b'[') && !label_rules_out(line) {
            self.pending.clear();
            self.pending.extend_from_slice(line);
            self.pending.push(b'\n');
            self.pending_mode(interner)
        } else {
            RefMode::Text
        }
    }

    fn pending_mode(&mut self, interner: &mut Interner) -> RefMode {
        if self.pending.len() > PENDING_LIMIT {
            RefMode::Long
        } else {
            RefMode::Pending(interner.intern_text(&self.pending))
        }
    }

    /// Appends a paragraph line (content and line ending tokens) and
    /// advances the reference-definition state. `first` is true on the
    /// paragraph's first line.
    fn para_line(
        &mut self,
        cur: &mut Cur<'_>,
        sink: &mut Sink,
        idx: usize,
        first: bool,
        interner: &mut Interner,
    ) {
        let start = cur.tp;
        let end = cur.content_end.max(start);
        if let Kind::Paragraph { refs, cells } = self.stack[idx].kind {
            let line = &cur.b[start..end];
            let new = match refs {
                _ if first => refs,
                RefMode::Text => RefMode::Text,
                RefMode::Defs => self.start_candidate(line, interner),
                RefMode::Pending(_) => {
                    let first = line.iter().copied().find(|&c| !scan::is_space_or_tab(c));
                    let opener = matches!(first, Some(b'"' | b'\'' | b'('));
                    if !opener && is_one_complete_def(&self.pending) {
                        // The candidate is a definition and this line cannot
                        // extend it with a title: decided.
                        self.flush(sink);
                        sink.push(Ev::refdef(count_lines(&self.pending)));
                        self.pending.clear();
                        self.start_candidate(line, interner)
                    } else {
                        self.pending.extend_from_slice(line);
                        self.pending.push(b'\n');
                        self.pending_mode(interner)
                    }
                }
                RefMode::Long => {
                    self.pending.extend_from_slice(line);
                    self.pending.push(b'\n');
                    RefMode::Long
                }
            };
            // The cell count a delimiter row on the next line must match.
            let new_cells = if !self.tables || cells == TABLE_VISITED {
                cells
            } else {
                table::cell_count(line) as u16
            };
            if new != refs || new_cells != cells {
                self.stack[idx].kind = Kind::Paragraph {
                    refs: new,
                    cells: new_cells,
                };
                self.touch(idx);
                if new == RefMode::Text && refs != RefMode::Text {
                    self.resolve_deferred(idx, sink);
                }
            }
        }
        self.tok(cur, sink, TokenKind::Content, end);
        self.tok(cur, sink, TokenKind::LineEnding, cur.b.len());
    }

    /// Appends a code or HTML line.
    fn literal_line(&mut self, cur: &mut Cur<'_>, sink: &mut Sink) {
        self.flush(sink);
        let virt = cur.virt();
        let start = cur.tp.max(cur.prefix_end());
        if start > cur.tp {
            sink.push(Ev::tok(TokenKind::Whitespace, 0, len32(start - cur.tp)));
            cur.tp = start;
        }
        let end = cur.content_end.max(cur.tp);
        if end > cur.tp || virt > 0 {
            sink.push(Ev::tok(TokenKind::Content, virt, len32(end - cur.tp)));
            cur.tp = end;
        }
        self.tok(cur, sink, TokenKind::LineEnding, cur.b.len());
    }

    /// The text of an ATX heading line, with the optional closing `#`
    /// sequence.
    fn atx_content(&mut self, cur: &mut Cur<'_>, sink: &mut Sink) {
        let b = cur.b;
        let fns = cur.fns.max(cur.tp);
        let end = cur.content_end.max(fns);
        let e1 = trim_end(b, fns, end);
        let mut k = e1;
        while k > fns && b[k - 1] == b'#' {
            k -= 1;
        }
        self.tok(cur, sink, TokenKind::Whitespace, fns);
        if k < e1 && (k == fns || scan::is_space_or_tab(b[k - 1])) {
            let text_end = trim_end(b, fns, k);
            self.tok(cur, sink, TokenKind::Content, text_end);
            self.tok(cur, sink, TokenKind::Whitespace, k);
            self.tok(cur, sink, TokenKind::Marker, e1);
        } else {
            self.tok(cur, sink, TokenKind::Content, e1);
        }
        self.tok(cur, sink, TokenKind::Whitespace, end);
        self.tok(cur, sink, TokenKind::LineEnding, b.len());
    }

    /// Setext underline under the open paragraph at `idx`. Returns false
    /// when the paragraph holds only reference definitions; the paragraph
    /// then stays open, empty, and the line is tried for other block
    /// starts.
    fn setext(&mut self, idx: usize, level: u8, sink: &mut Sink) -> bool {
        if !self.settle_paragraph(idx, sink) {
            return false;
        }
        self.flush(sink);
        sink.push(Ev::setext(level));
        self.stack[idx].kind = Kind::Heading;
        self.touch(idx);
        true
    }

    /// A delimiter row under the open paragraph at `idx` (the top of the
    /// stack) whose column count matches its last line: that line becomes
    /// the header of a table. The definitions pending before the header
    /// line are decided. Returns false (nothing changes) when the paragraph
    /// holds only decided definitions.
    fn table_header(&mut self, idx: usize, sink: &mut Sink) -> bool {
        let Kind::Paragraph { refs, .. } = self.stack[idx].kind else {
            return false;
        };
        match refs {
            RefMode::Text => {}
            RefMode::Defs => return false,
            RefMode::Pending(_) | RefMode::Long => {
                // `pending` holds the lines since the last decided
                // definition, each followed by `\n`; the last one is the
                // header.
                let body = self.pending.len().saturating_sub(1);
                let cut = memchr::memrchr(b'\n', &self.pending[..body]).map_or(0, |i| i + 1);
                let mut defs = std::mem::take(&mut self.defs);
                extract_defs(&self.pending[..cut], &mut defs);
                self.defs = defs;
                self.flush(sink);
                self.emit_defs(sink);
                self.pending.clear();
                self.resolve_deferred(idx, sink);
            }
        }
        self.flush(sink);
        sink.push(Ev::table_header());
        self.stack[idx].kind = Kind::Table;
        self.touch(idx);
        true
    }

    /// Whether front matter is open (at a line start: the line is inside
    /// it).
    pub(crate) fn in_front_matter(&self) -> bool {
        matches!(
            self.stack.last().map(|e| e.kind),
            Some(Kind::FrontMatter { .. })
        )
    }

    /// Processes one line (`b` includes its terminator). `first_line` is
    /// true for the line at offset 0 (BOM handling). `front_matter` is
    /// `Some` only on the first line, when it opens front matter (the
    /// caller decides with [`front_matter::detect`], which looks ahead for
    /// the closing delimiter).
    pub(crate) fn process_line(
        &mut self,
        b: &[u8],
        first_line: bool,
        front_matter: Option<FrontMatterKind>,
        interner: &mut Interner,
        sink: &mut Sink,
    ) {
        let mut cur = Cur::new(b);
        self.pre.clear();
        self.flushed = false;
        self.tb_kill = 0;
        if first_line && b.starts_with(BOM) {
            cur.offset = 3;
            cur.tp = 3;
            self.pre.push((TokenKind::Bom, 3));
        }
        self.first_new = self.stack.len();
        if let Some(kind) = front_matter.filter(|_| first_line && self.stack.len() == 1) {
            self.last_matched = 0;
            self.all_closed = true;
            self.add_child(
                Kind::FrontMatter { kind },
                BlockKind::FrontMatter { kind },
                sink,
            );
            let start = cur.tp;
            self.tok(
                &mut cur,
                sink,
                TokenKind::Marker,
                start + front_matter::DELIMITER_LEN,
            );
            let end = cur.content_end.max(cur.tp);
            self.tok(&mut cur, sink, TokenKind::Whitespace, end);
            self.tok(&mut cur, sink, TokenKind::LineEnding, b.len());
            return;
        }

        // 1. Continuation of open blocks.
        let mut i = 1;
        let mut all_matched = false;
        loop {
            if i == self.stack.len() {
                all_matched = true;
                break;
            }
            cur.find_first_nonspace();
            if cur.blank && cur.offset == cur.fns && self.stack[i].blank_transparent() {
                // Lists and items with a child continue over a blank line
                // without consuming anything: skip to the next entry that
                // may not.
                let k = self.stoppers.partition_point(|&s| (s as usize) < i);
                i = self
                    .stoppers
                    .get(k)
                    .map_or(self.stack.len(), |&s| s as usize);
                continue;
            }
            let e = self.stack[i];
            let matched = match e.kind {
                Kind::BlockQuote => {
                    if cur.indent <= 3 && cur.peek(cur.fns) == Some(b'>') {
                        cur.advance(cur.indent + 1, true);
                        if cur.peek(cur.offset).is_some_and(scan::is_space_or_tab) {
                            cur.advance(1, true);
                        }
                        self.pre_tok(&mut cur, TokenKind::ContainerPrefix);
                        true
                    } else {
                        false
                    }
                }
                Kind::Item { indent } => {
                    let indent = indent as usize;
                    if cur.indent >= indent {
                        cur.advance(indent, true);
                        self.pre_tok(&mut cur, TokenKind::ContainerPrefix);
                        true
                    } else if cur.blank && e.has_child {
                        let n = cur.fns - cur.offset;
                        cur.advance(n, false);
                        self.pre_tok(&mut cur, TokenKind::ContainerPrefix);
                        true
                    } else {
                        false
                    }
                }
                Kind::FencedCode { ch, len, offset } => {
                    let m = if cur.indent <= 3 && cur.peek(cur.fns) == Some(ch) {
                        scan::close_code_fence(&b[cur.fns..]).unwrap_or(0)
                    } else {
                        0
                    };
                    if m >= len as usize {
                        // Closing fence: the code block ends with this line,
                        // and nothing else happens on it.
                        let fns = cur.fns;
                        self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                        self.tok(&mut cur, sink, TokenKind::Marker, fns + m);
                        let end = cur.content_end.max(cur.tp);
                        self.tok(&mut cur, sink, TokenKind::Whitespace, end);
                        self.tok(&mut cur, sink, TokenKind::LineEnding, b.len());
                        self.close_top(sink);
                        return;
                    }
                    let mut k = offset;
                    while k > 0 && cur.peek(cur.offset).is_some_and(scan::is_space_or_tab) {
                        cur.advance(1, true);
                        k -= 1;
                    }
                    self.pre_tok(&mut cur, TokenKind::CodeIndent);
                    true
                }
                Kind::IndentedCode => {
                    if cur.indent >= CODE_INDENT {
                        cur.advance(CODE_INDENT, true);
                        self.pre_tok(&mut cur, TokenKind::CodeIndent);
                        true
                    } else if cur.blank {
                        let n = cur.fns - cur.offset;
                        cur.advance(n, false);
                        self.pre_tok(&mut cur, TokenKind::CodeIndent);
                        true
                    } else {
                        false
                    }
                }
                Kind::FrontMatter { kind } => {
                    if front_matter::is_closer(b, kind) {
                        // The closing delimiter ends the front matter; it
                        // starts at column 0.
                        self.tok(
                            &mut cur,
                            sink,
                            TokenKind::Marker,
                            front_matter::DELIMITER_LEN,
                        );
                        let end = cur.content_end.max(cur.tp);
                        self.tok(&mut cur, sink, TokenKind::Whitespace, end);
                        self.tok(&mut cur, sink, TokenKind::LineEnding, b.len());
                        self.close_top(sink);
                        return;
                    }
                    true
                }
                Kind::Html { ty } => !(ty >= 6 && cur.blank),
                Kind::Paragraph { .. } => !cur.blank,
                Kind::Table => {
                    !cur.blank && table::cell_count(&b[cur.fns..cur.content_end.max(cur.fns)]) > 0
                }
                Kind::Heading => false,
                Kind::ThematicBreak | Kind::Document | Kind::List { .. } => true,
            };
            if !matched {
                break;
            }
            i += 1;
        }
        self.last_matched = if all_matched {
            self.stack.len() - 1
        } else {
            i - 1
        };
        self.all_closed = self.last_matched == self.stack.len() - 1;
        let mut container = self.last_matched;

        // 2. New block starts.
        let mut maybe_lazy = matches!(
            self.stack[self.stack.len() - 1].kind,
            Kind::Paragraph { .. }
        );
        loop {
            let ck = self.stack[container].kind;
            if matches!(
                ck,
                Kind::FencedCode { .. }
                    | Kind::IndentedCode
                    | Kind::Html { .. }
                    | Kind::FrontMatter { .. }
            ) {
                break;
            }
            cur.find_first_nonspace();
            let indented = cur.indent >= CODE_INDENT;
            let fns = cur.fns;
            let s = &b[fns.min(b.len())..];
            let c = s.first().copied();
            let ck_para = matches!(ck, Kind::Paragraph { .. });
            // Room for a list, its item and a leaf.
            let can_nest = self.stack.len() + 3 <= MAX_NESTING;
            let mut started = false;
            if !indented && let Some(c) = c.filter(|&c| scan::maybe_special(c)) {
                if c == b'>' && can_nest {
                    self.close_unmatched_for(Kind::BlockQuote, sink);
                    cur.advance(fns + 1 - cur.offset, false);
                    if cur.peek(cur.offset).is_some_and(scan::is_space_or_tab) {
                        cur.advance(1, true);
                    }
                    container = self.add_child(Kind::BlockQuote, BlockKind::BlockQuote, sink);
                    let e = cur.prefix_end();
                    self.tok(&mut cur, sink, TokenKind::QuoteMarker, e);
                    started = true;
                } else if let Some((level, m)) = scan::atx_heading_start(s) {
                    self.close_unmatched_for(Kind::Heading, sink);
                    container = self.add_child(
                        Kind::Heading,
                        BlockKind::AtxHeading { level: level as u8 },
                        sink,
                    );
                    self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                    self.tok(&mut cur, sink, TokenKind::Marker, fns + level);
                    cur.advance(fns + m - cur.offset, false);
                    started = true;
                } else if let Some(n) = scan::open_code_fence(s) {
                    let kind = Kind::FencedCode {
                        ch: c,
                        len: len32(n),
                        offset: len32(cur.indent),
                    };
                    self.close_unmatched_for(kind, sink);
                    container = self.add_child(kind, BlockKind::FencedCode, sink);
                    self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                    self.tok(&mut cur, sink, TokenKind::Marker, fns + n);
                    cur.advance(fns + n - cur.offset, false);
                    started = true;
                } else if let Some(ty) = scan::html_block_start(s).or_else(|| {
                    // Type 7 cannot interrupt a paragraph, including one that
                    // this line would continue lazily.
                    (!ck_para && !maybe_lazy && scan::html_block_start_7(s)).then_some(7)
                }) {
                    self.close_unmatched_for(Kind::Html { ty }, sink);
                    container =
                        self.add_child(Kind::Html { ty }, BlockKind::Html { kind: ty }, sink);
                    started = true;
                } else if ck_para && let Some((level, run)) = scan::setext_heading_line(s) {
                    // `container` is the open paragraph, on top of the stack.
                    if self.setext(container, level, sink) {
                        self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                        self.tok(&mut cur, sink, TokenKind::Marker, fns + run);
                        let adv = cur.content_end.saturating_sub(cur.offset);
                        cur.advance(adv, false);
                        started = true;
                    }
                }
                if !started
                    && !(ck_para && !all_matched)
                    && self.tb_kill <= fns
                    && matches!(c, b'*' | b'-' | b'_')
                {
                    match scan::thematic_break(s) {
                        Ok(()) => {
                            self.close_unmatched_for(Kind::ThematicBreak, sink);
                            container =
                                self.add_child(Kind::ThematicBreak, BlockKind::ThematicBreak, sink);
                            self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                            let end = trim_end(b, fns, cur.content_end.max(fns));
                            self.tok(&mut cur, sink, TokenKind::Marker, end);
                            let adv = cur.content_end.saturating_sub(cur.offset);
                            cur.advance(adv, false);
                            started = true;
                        }
                        Err(p) => self.tb_kill = fns + p,
                    }
                }
            }
            if !started
                && (!indented || matches!(ck, Kind::List { .. }))
                && cur.indent < 4
                && can_nest
                && let Some((m, marker)) = scan::parse_list_marker(b, fns, ck_para)
            {
                cur.advance(fns + m - cur.offset, false);
                let (sp, so, sc) = (cur.partial, cur.offset, cur.column);
                while cur.column - sc <= 5
                    && cur.peek(cur.offset).is_some_and(scan::is_space_or_tab)
                {
                    cur.advance(1, true);
                }
                let spaces = cur.column - sc;
                let padding = if !(1..5).contains(&spaces)
                    || cur.peek(cur.offset).is_none_or(scan::is_line_end)
                {
                    cur.offset = so;
                    cur.column = sc;
                    cur.partial = sp;
                    if spaces > 0 {
                        cur.advance(1, true);
                    }
                    m + 1
                } else {
                    m + spaces
                };
                let marker_offset = cur.indent;
                let continues = matches!(
                    ck,
                    Kind::List { ordered, ch } if ordered == marker.ordered && ch == marker.ch
                );
                let next = if continues {
                    Kind::Item { indent: 0 }
                } else {
                    Kind::List {
                        ordered: marker.ordered,
                        ch: marker.ch,
                    }
                };
                self.close_unmatched_for(next, sink);
                if !continues {
                    let bk = if marker.ordered {
                        BlockKind::OrderedList {
                            start: marker.start,
                            delimiter: marker.ch,
                        }
                    } else {
                        BlockKind::BulletList { marker: marker.ch }
                    };
                    self.add_child(
                        Kind::List {
                            ordered: marker.ordered,
                            ch: marker.ch,
                        },
                        bk,
                        sink,
                    );
                }
                container = self.add_child(
                    Kind::Item {
                        indent: len32(marker_offset + padding),
                    },
                    BlockKind::Item,
                    sink,
                );
                let e = cur.prefix_end();
                self.tok(&mut cur, sink, TokenKind::ListMarker, e);
                started = true;
            }
            if !started && indented && !maybe_lazy && !cur.blank {
                cur.advance(CODE_INDENT, true);
                self.close_unmatched_for(Kind::IndentedCode, sink);
                container = self.add_child(Kind::IndentedCode, BlockKind::IndentedCode, sink);
                let e = cur.prefix_end();
                self.tok(&mut cur, sink, TokenKind::CodeIndent, e);
                started = true;
            }
            if !started
                && self.tables
                && !indented
                && ck_para
                && let Kind::Paragraph { refs, cells } = self.stack[container].kind
                && cells != TABLE_VISITED
            {
                // A delimiter row under the paragraph (`container`, the top
                // of the stack: every block matched).
                let end = cur.content_end.max(fns);
                let columns = table::delimiter_columns(&b[fns..end]);
                if columns > 0 {
                    if columns == usize::from(cells) && self.table_header(container, sink) {
                        self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                        self.tok(&mut cur, sink, TokenKind::Marker, trim_end(b, fns, end));
                        self.tok(&mut cur, sink, TokenKind::Whitespace, end);
                        self.tok(&mut cur, sink, TokenKind::LineEnding, b.len());
                        return;
                    }
                    self.stack[container].kind = Kind::Paragraph {
                        refs,
                        cells: TABLE_VISITED,
                    };
                    self.touch(container);
                }
            }
            if !started || self.stack[container].kind.accepts_lines() {
                break;
            }
            maybe_lazy = false;
        }

        // 3. The rest of the line goes to `container` (or, lazily, to the
        //    open paragraph).
        cur.find_first_nonspace();
        let ci = container;
        if cur.blank {
            if ci + 1 < self.stack.len() {
                // The open last child closes on this line with its flag set.
                if !self.stack[ci + 1].lb {
                    self.stack[ci + 1].lb = true;
                    self.touch(ci + 1);
                }
            } else if self.stack[ci].kind.is_list_or_item()
                && let Some(mut s) = self.stack[ci].last
                && !s.lb
            {
                s.lb = true;
                self.stack[ci].last = Some(s);
                self.touch(ci);
            }
        }
        let e = self.stack[ci];
        let lb = cur.blank
            && match e.kind {
                Kind::BlockQuote
                | Kind::Heading
                | Kind::ThematicBreak
                | Kind::FencedCode { .. }
                | Kind::FrontMatter { .. } => false,
                Kind::Item { .. } => e.has_child || ci < self.first_new,
                _ => true,
            };
        // `container` gets the flag, its ancestors lose it. Deeper open
        // blocks keep theirs: they close on this line, or (lazy line) have
        // none.
        if let Some(k) = self.lb_index
            && k < ci
        {
            self.set_lb(k, false);
        }
        self.set_lb(ci, lb);

        let top = self.stack.len() - 1;
        let lazy = ci == self.last_matched
            && top > self.last_matched
            && !cur.blank
            && matches!(self.stack[top].kind, Kind::Paragraph { .. });
        if lazy {
            // Like cmark, a lazy line keeps its leading whitespace.
            self.para_line(&mut cur, sink, top, false, interner);
            return;
        }
        self.close_unmatched(sink);
        match self.stack[ci].kind {
            Kind::FencedCode { .. } if ci >= self.first_new => {
                // Opening fence line: the rest is the info string.
                let start = skip_ws(b, cur.tp, cur.content_end);
                let end = trim_end(b, start, cur.content_end.max(start));
                self.tok(&mut cur, sink, TokenKind::Whitespace, start);
                self.tok(&mut cur, sink, TokenKind::InfoString, end);
                let ce = cur.content_end.max(cur.tp);
                self.tok(&mut cur, sink, TokenKind::Whitespace, ce);
                self.tok(&mut cur, sink, TokenKind::LineEnding, b.len());
            }
            Kind::FencedCode { .. } | Kind::IndentedCode | Kind::FrontMatter { .. } => {
                self.literal_line(&mut cur, sink)
            }
            Kind::Html { ty } => {
                self.literal_line(&mut cur, sink);
                if ty <= 5 && scan::html_block_end(ty, &b[cur.fns.min(b.len())..]) {
                    self.close_top(sink);
                }
            }
            kind => {
                if cur.blank {
                    let ce = cur.content_end.max(cur.tp);
                    self.tok(&mut cur, sink, TokenKind::Whitespace, ce);
                    self.tok(&mut cur, sink, TokenKind::LineEnding, b.len());
                } else if let Kind::Heading = kind {
                    self.atx_content(&mut cur, sink);
                } else if let Kind::Paragraph { .. } = kind {
                    let fns = cur.fns;
                    self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                    self.para_line(&mut cur, sink, ci, false, interner);
                } else if let Kind::Table = kind {
                    // A table row.
                    let fns = cur.fns;
                    self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                    let end = cur.content_end.max(cur.tp);
                    self.tok(&mut cur, sink, TokenKind::Content, end);
                    self.tok(&mut cur, sink, TokenKind::LineEnding, b.len());
                } else {
                    let fns = cur.fns;
                    let refs = self.start_candidate(&b[fns..cur.content_end.max(fns)], interner);
                    let defer = refs != RefMode::Text;
                    let idx = self.add_child_ex(
                        Kind::Paragraph { refs, cells: 0 },
                        BlockKind::Text,
                        sink,
                        defer,
                    );
                    self.tok(&mut cur, sink, TokenKind::Whitespace, fns);
                    self.para_line(&mut cur, sink, idx, true, interner);
                }
            }
        }
    }

    /// Closes all open blocks at the end of the document.
    pub(crate) fn finish(&mut self, sink: &mut Sink) {
        self.pre.clear();
        self.flushed = true;
        while self.stack.len() > 1 {
            self.close_top(sink);
        }
    }
}

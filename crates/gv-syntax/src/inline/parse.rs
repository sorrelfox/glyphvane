// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The inline parser: the CommonMark appendix algorithm (as in cmark's
//! `inlines.c`) over a working arena.
//!
//! - One left-to-right scan jumps between special bytes. Code spans,
//!   autolinks, raw HTML, entities, escapes and line breaks become finished
//!   nodes on the spot; `*`/`_` runs go on the delimiter stack and `[`/`![`
//!   on the bracket stack as provisional text.
//! - `]` resolves the top bracket (inline link, full, collapsed or shortcut
//!   reference). A match wraps everything after the bracket in a link node
//!   and processes the emphasis inside it; a link deactivates the earlier
//!   link openers.
//! - At the end, emphasis is processed over the remaining delimiters, with
//!   `openers_bottom` indexed by delimiter character, closer length mod 3
//!   and whether the closer can open (0.31 / cmark master).
//!
//! Every input byte ends up in exactly one token; delimiters and brackets
//! that do not match stay text. Nothing recurses, and every search is
//! bounded or memoized so the parse is linear in the input.

use std::cell::RefCell;

use super::api::{InlineTree, RefLookup};
use super::scan;
use super::tree::{self, ANode, InlineKind as K, LinkData, LinkKind, NIL, Segs, Str};
use super::unicode;

/// Bytes that end a text run.
static SPECIAL: [bool; 256] = {
    let mut t = [false; 256];
    let specials = b"\n\r`\\&<*_[]!";
    let mut i = 0;
    while i < specials.len() {
        t[specials[i] as usize] = true;
        i += 1;
    }
    t
};

/// An entry of the delimiter stack (a doubly linked list over a vector, in
/// push order: a larger index is later in the text).
#[derive(Clone, Copy, Debug)]
struct Delim {
    /// The arena node of the run; its range shrinks as delimiters are used.
    node: u32,
    ch: u8,
    /// The run's original length (for the rule of 3).
    len: u32,
    can_open: bool,
    can_close: bool,
    prev: u32,
    next: u32,
}

/// An entry of the bracket stack.
#[derive(Clone, Copy, Debug)]
struct Bracket {
    /// The arena node of `[` or `![`.
    node: u32,
    image: bool,
    active: bool,
    /// Whether another bracket was pushed after this one.
    bracket_after: bool,
    /// Delimiters with an index at or above this were pushed after the
    /// bracket.
    delim_base: u32,
    /// Where the link text starts.
    text_start: usize,
}

/// The syntax after the `]` of a matched link.
enum Tail {
    Inline {
        open: usize,
        dest: (usize, usize),
        title: Option<(usize, usize)>,
        close: usize,
    },
    Reference {
        /// `[label]` or `[]` after the first bracket pair.
        second: Option<(usize, usize, usize)>,
    },
}

/// Working vectors kept between parses on a thread, so that a parse of a
/// short paragraph does not allocate them again.
#[derive(Default)]
struct Scratch {
    nodes: Vec<ANode>,
    delims: Vec<Delim>,
    brackets: Vec<Bracket>,
    stack: Vec<tree::Frame>,
}

/// Capacity (in arena nodes) above which the scratch vectors are released
/// after a parse instead of being kept (4096 nodes, 128 KiB: paragraphs of
/// tens of kilobytes; the scratch lives as long as its thread).
const SCRATCH_KEEP: usize = 1 << 12;

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default());
}

/// Parses gathered text into a finished tree.
pub(super) fn parse(text: String, segs: Segs, refs: &dyn RefLookup) -> InlineTree {
    SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => parse_with(&mut scratch, text, segs, refs),
        // A lookup that parses inlines itself: use fresh vectors.
        Err(_) => parse_with(&mut Scratch::default(), text, segs, refs),
    })
}

fn parse_with(scratch: &mut Scratch, text: String, segs: Segs, refs: &dyn RefLookup) -> InlineTree {
    let mut nodes = std::mem::take(&mut scratch.nodes);
    let mut delims = std::mem::take(&mut scratch.delims);
    let mut brackets = std::mem::take(&mut scratch.brackets);
    nodes.clear();
    delims.clear();
    brackets.clear();
    let mut p = Parser {
        t: &text,
        b: text.as_bytes(),
        end: 0,
        pos: 0,
        nodes,
        delims,
        last_delim: NIL,
        brackets,
        link_floor: 0,
        links: Vec::new(),
        arena: String::new(),
        lookups: Vec::new(),
        refs,
        backticks: None,
        html_skip: 0,
    };
    p.run();
    let flat = tree::flatten(
        &p.nodes,
        segs.as_slice(),
        text.len() as u32,
        &mut scratch.stack,
    );
    let Parser {
        nodes,
        delims,
        brackets,
        links,
        arena,
        lookups,
        ..
    } = p;
    if nodes.capacity() <= SCRATCH_KEEP {
        scratch.nodes = nodes;
        scratch.delims = delims;
        scratch.brackets = brackets;
    } else {
        scratch.stack = Vec::new();
    }
    InlineTree {
        text,
        segs,
        nodes: flat,
        links,
        arena,
        lookups,
    }
}

struct Parser<'a> {
    t: &'a str,
    b: &'a [u8],
    /// End of the content (trailing whitespace excluded).
    end: usize,
    pos: usize,
    /// Node 0 is the root.
    nodes: Vec<ANode>,
    delims: Vec<Delim>,
    last_delim: u32,
    brackets: Vec<Bracket>,
    /// Every link opener below this index of `brackets` is inactive.
    link_floor: usize,
    links: Vec<LinkData>,
    arena: String,
    lookups: Vec<(u32, u32)>,
    refs: &'a dyn RefLookup,
    /// Backtick runs after the first code span opener, as `(length, start)`,
    /// sorted: built on first use, so closers are found in O(log n).
    backticks: Option<Vec<(u32, u32)>>,
    html_skip: u8,
}

impl Parser<'_> {
    fn run(&mut self) {
        self.new_node(K::Text, 0, 0, 0);
        let b = self.b;
        let len = b.len();
        let mut start = 0;
        while start < len && scan::is_space_or_eol(b[start]) {
            start += 1;
        }
        let mut end = len;
        while end > start && matches!(b[end - 1], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
            end -= 1;
        }
        if start > 0 {
            self.add(K::Whitespace, 0, start, 0);
        }
        self.pos = start;
        self.end = end;
        while self.pos < self.end {
            match b[self.pos] {
                b'\n' | b'\r' => self.newline(),
                b'`' => self.backticks(),
                b'\\' => self.backslash(),
                b'&' => self.entity(),
                b'<' => self.pointy(),
                c @ (b'*' | b'_') => self.delimiter_run(c),
                b'[' => {
                    let p = self.pos;
                    let n = self.add_provisional(p, p + 1);
                    self.pos += 1;
                    self.push_bracket(n, false);
                }
                b'!' if self.pos + 1 < self.end && b[self.pos + 1] == b'[' => {
                    let p = self.pos;
                    let n = self.add_provisional(p, p + 2);
                    self.pos += 2;
                    self.push_bracket(n, true);
                }
                b']' => self.close_bracket(),
                _ => self.text(),
            }
        }
        self.process_emphasis(0);
        if end < len {
            self.add(K::Whitespace, end, len, 0);
        }
    }

    // -----------------------------------------------------------------------
    // Arena

    fn new_node(&mut self, kind: K, start: usize, end: usize, data: u32) -> u32 {
        let index = self.nodes.len() as u32;
        self.nodes.push(ANode {
            kind,
            start: start as u32,
            end: end as u32,
            next: NIL,
            prev: NIL,
            first: NIL,
            last: NIL,
            data,
        });
        index
    }

    fn node(&mut self, i: u32) -> &mut ANode {
        &mut self.nodes[i as usize]
    }

    fn append(&mut self, parent: u32, n: u32) {
        let last = self.nodes[parent as usize].last;
        self.node(n).prev = last;
        if last == NIL {
            self.node(parent).first = n;
        } else {
            self.node(last).next = n;
        }
        self.node(parent).last = n;
    }

    /// Appends a node to the root.
    fn add(&mut self, kind: K, start: usize, end: usize, data: u32) -> u32 {
        let n = self.new_node(kind, start, end, data);
        self.append(0, n);
        n
    }

    /// Appends a delimiter run or bracket: text that later steps may turn
    /// into markers. (Other text gets no node: the bytes between nodes are
    /// text, which [`tree::flatten`] fills in.)
    fn add_provisional(&mut self, start: usize, end: usize) -> u32 {
        self.add(K::Text, start, end, 0)
    }

    /// Appends a finished node with marker/content/marker children.
    fn add_wrapped(
        &mut self,
        kind: K,
        start: usize,
        end: usize,
        inner: (usize, usize),
        content: K,
        data: u32,
    ) {
        let n = self.add(kind, start, end, data);
        let open = self.new_node(K::Marker, start, inner.0, 0);
        self.append(n, open);
        if inner.0 < inner.1 {
            let c = self.new_node(content, inner.0, inner.1, 0);
            self.append(n, c);
        }
        let close = self.new_node(K::Marker, inner.1, end, 0);
        self.append(n, close);
    }

    // -----------------------------------------------------------------------
    // Simple constructs

    /// Literal text: skipped (it needs no node).
    fn text(&mut self) {
        let mut j = self.pos + 1;
        while j < self.end && !SPECIAL[self.b[j] as usize] {
            j += 1;
        }
        self.pos = j;
    }

    /// A line ending: soft or hard break. Trailing spaces and tabs join the
    /// break token; the next line's indentation becomes whitespace.
    fn newline(&mut self) {
        let nl = self.pos;
        // Trailing spaces and tabs are always text (nothing else ends in
        // them), so they start right after the last node at the latest.
        let last = self.nodes[0].last;
        let floor = if last == NIL {
            0
        } else {
            self.nodes[last as usize].end as usize
        };
        let mut ws = nl;
        while ws > floor && matches!(self.b[ws - 1], b' ' | b'\t') {
            ws -= 1;
        }
        let after = scan::line_ending(self.b, nl, self.b.len()).unwrap_or(nl + 1);
        let hard = nl >= ws + 2 && self.b[nl - 1] == b' ' && self.b[nl - 2] == b' ';
        self.add(if hard { K::HardBreak } else { K::SoftBreak }, ws, after, 0);
        self.pos = after;
        self.indentation();
    }

    /// Skips the spaces and tabs that start a line.
    fn indentation(&mut self) {
        let p = self.pos;
        let q = scan::skip_spaces(self.b, p, self.end);
        if q > p {
            self.add(K::Whitespace, p, q, 0);
        }
        self.pos = q;
    }

    fn backslash(&mut self) {
        let p = self.pos;
        if p + 1 < self.end {
            let c = self.b[p + 1];
            if scan::is_escapable(c) {
                self.add(K::Escape, p, p + 2, 0);
                self.pos = p + 2;
                return;
            }
            if let Some(after) = scan::line_ending(self.b, p + 1, self.end) {
                self.add(K::HardBreak, p, after, 0);
                self.pos = after;
                self.indentation();
                return;
            }
        }
        self.pos = p + 1;
    }

    fn entity(&mut self) {
        let p = self.pos;
        match scan::entity(self.b, p, self.end) {
            Some((len, value)) => {
                self.add(K::Entity, p, p + len, value);
                self.pos = p + len;
            }
            None => {
                self.pos = p + 1;
            }
        }
    }

    fn backticks(&mut self) {
        let start = self.pos;
        let mut after = start;
        while after < self.end && self.b[after] == b'`' {
            after += 1;
        }
        let n = after - start;
        match self.closing_backticks(after, n) {
            Some(close) => {
                let end = close + n;
                self.add_wrapped(
                    K::CodeSpan,
                    start,
                    end,
                    (after, close),
                    K::CodeText,
                    n as u32,
                );
                self.pos = end;
            }
            None => {
                self.pos = after;
            }
        }
    }

    /// The start of the first run of exactly `n` backticks at or after
    /// `from`.
    fn closing_backticks(&mut self, from: usize, n: usize) -> Option<usize> {
        let (b, end) = (self.b, self.end);
        let runs = self.backticks.get_or_insert_with(|| {
            let mut runs = Vec::new();
            let mut j = from;
            while j < end {
                if b[j] == b'`' {
                    let s = j;
                    while j < end && b[j] == b'`' {
                        j += 1;
                    }
                    runs.push(((j - s) as u32, s as u32));
                } else {
                    j += 1;
                }
            }
            runs.sort_unstable();
            runs
        });
        let key = (n as u32, from as u32);
        let i = runs.partition_point(|&r| r < key);
        runs.get(i).filter(|r| r.0 == key.0).map(|r| r.1 as usize)
    }

    /// `<`: autolink, raw HTML or a literal `<`.
    fn pointy(&mut self) {
        let p = self.pos;
        let (b, end) = (self.b, self.end);
        let auto = scan::autolink_uri(b, p, end)
            .map(|len| (len, LinkKind::UriAutolink))
            .or_else(|| scan::autolink_email(b, p, end).map(|len| (len, LinkKind::EmailAutolink)));
        if let Some((len, kind)) = auto {
            let (a, z) = ((p + 1) as u32, (p + len - 1) as u32);
            let raw = &self.t[p + 1..p + len - 1];
            let dest = if raw.contains('&') {
                let start = self.arena.len() as u32;
                scan::decode_entities_into(raw, &mut self.arena);
                Str::Arena(start, self.arena.len() as u32)
            } else {
                Str::Text(a, z)
            };
            let index = self.links.len() as u32;
            self.links.push(LinkData {
                kind,
                dest,
                title: None,
                label: None,
                text: (a, z),
                dest_raw: Some((a, z)),
                label_raw: None,
            });
            self.add_wrapped(
                K::Autolink,
                p,
                p + len,
                (p + 1, p + len - 1),
                K::AutolinkText,
                index,
            );
            self.pos = p + len;
            return;
        }
        if let Some(len) = scan::raw_html(b, p, end, &mut self.html_skip) {
            let n = self.add(K::RawHtml, p, p + len, 0);
            let t = self.new_node(K::HtmlText, p, p + len, 0);
            self.append(n, t);
            self.pos = p + len;
            return;
        }
        self.pos = p + 1;
    }

    // -----------------------------------------------------------------------
    // Emphasis

    fn delimiter_run(&mut self, ch: u8) {
        let start = self.pos;
        let mut end = start;
        while end < self.end && self.b[end] == ch {
            end += 1;
        }
        let before = self.t[..start]
            .chars()
            .next_back()
            .map_or('\n', nul_as_replacement);
        let after = if end < self.end {
            self.t[end..]
                .chars()
                .next()
                .map_or('\n', nul_as_replacement)
        } else {
            '\n'
        };
        let (ws_before, ws_after) = (
            unicode::is_whitespace(before),
            unicode::is_whitespace(after),
        );
        let (p_before, p_after) = (
            unicode::is_punctuation(before),
            unicode::is_punctuation(after),
        );
        let left = !ws_after && (!p_after || ws_before || p_before);
        let right = !ws_before && (!p_before || ws_after || p_after);
        let (can_open, can_close) = if ch == b'*' {
            (left, right)
        } else {
            (left && (!right || p_before), right && (!left || p_after))
        };
        let node = self.add_provisional(start, end);
        self.pos = end;
        if can_open || can_close {
            let index = self.delims.len() as u32;
            self.delims.push(Delim {
                node,
                ch,
                len: (end - start) as u32,
                can_open,
                can_close,
                prev: self.last_delim,
                next: NIL,
            });
            if self.last_delim != NIL {
                self.delims[self.last_delim as usize].next = index;
            }
            self.last_delim = index;
        }
    }

    fn remove_delim(&mut self, i: u32) {
        let Delim { prev, next, .. } = self.delims[i as usize];
        if prev != NIL {
            self.delims[prev as usize].next = next;
        }
        if next != NIL {
            self.delims[next as usize].prev = prev;
        } else if self.last_delim == i {
            self.last_delim = prev;
        }
    }

    /// Matches the delimiters at index `lower` and above (the spec's
    /// "process emphasis" with `stack_bottom`), then removes them.
    fn process_emphasis(&mut self, lower: u32) {
        if self.last_delim == NIL || self.last_delim < lower {
            return;
        }
        // openers_bottom[character][closer length % 3][closer can open]
        let mut openers_bottom = [[[lower; 2]; 3]; 2];
        let mut closer = self.last_delim;
        let mut first = NIL;
        while closer != NIL && closer >= lower {
            first = closer;
            closer = self.delims[closer as usize].prev;
        }
        closer = first;
        while closer != NIL {
            let c = self.delims[closer as usize];
            if !c.can_close {
                closer = c.next;
                continue;
            }
            let slot = &mut openers_bottom[usize::from(c.ch == b'_')][(c.len % 3) as usize]
                [usize::from(c.can_open)];
            let bottom = *slot;
            let mut opener = c.prev;
            let mut found = false;
            while opener != NIL && opener >= bottom {
                let o = self.delims[opener as usize];
                if o.can_open && o.ch == c.ch {
                    let odd = (c.can_open || o.can_close)
                        && (o.len + c.len).is_multiple_of(3)
                        && !(o.len.is_multiple_of(3) && c.len.is_multiple_of(3));
                    if !odd {
                        found = true;
                        break;
                    }
                }
                opener = o.prev;
            }
            if found {
                closer = self.insert_emphasis(opener, closer);
            } else {
                *slot = closer;
                if !c.can_open {
                    self.remove_delim(closer);
                }
                closer = c.next;
            }
        }
        while self.last_delim != NIL && self.last_delim >= lower {
            self.remove_delim(self.last_delim);
        }
    }

    /// Wraps the nodes between a matched opener and closer in an emphasis
    /// node and returns the next closer to look at.
    fn insert_emphasis(&mut self, opener: u32, closer: u32) -> u32 {
        let (o, c) = (self.delims[opener as usize], self.delims[closer as usize]);
        let (on, cn) = (o.node, c.node);
        let o_len = self.nodes[on as usize].end - self.nodes[on as usize].start;
        let c_len = self.nodes[cn as usize].end - self.nodes[cn as usize].start;
        let used = if o_len >= 2 && c_len >= 2 { 2 } else { 1 };
        let mut d = c.prev;
        while d != opener && d != NIL {
            let prev = self.delims[d as usize].prev;
            self.remove_delim(d);
            d = prev;
        }
        let o_end = self.nodes[on as usize].end;
        let c_start = self.nodes[cn as usize].start;
        self.node(on).end = o_end - used;
        self.node(cn).start = c_start + used;
        let open = self.new_node(K::Marker, (o_end - used) as usize, o_end as usize, 0);
        let close = self.new_node(K::Marker, c_start as usize, (c_start + used) as usize, 0);
        let kind = if used == 2 { K::Strong } else { K::Emphasis };
        let e = self.new_node(kind, (o_end - used) as usize, (c_start + used) as usize, 0);
        let first = self.nodes[on as usize].next;
        let last = self.nodes[cn as usize].prev;
        self.node(e).first = open;
        self.node(e).last = close;
        if first != cn {
            self.node(open).next = first;
            self.node(first).prev = open;
            self.node(last).next = close;
            self.node(close).prev = last;
        } else {
            self.node(open).next = close;
            self.node(close).prev = open;
        }
        self.node(on).next = e;
        self.node(e).prev = on;
        self.node(e).next = cn;
        self.node(cn).prev = e;
        if o_len == used {
            self.remove_delim(opener);
        }
        if c_len == used {
            self.remove_delim(closer);
            c.next
        } else {
            closer
        }
    }

    // -----------------------------------------------------------------------
    // Links and images

    fn push_bracket(&mut self, node: u32, image: bool) {
        // Brackets popped since the last link may have lowered the stack
        // below the floor; the new opener is active, so it must sit above.
        self.link_floor = self.link_floor.min(self.brackets.len());
        if let Some(top) = self.brackets.last_mut() {
            top.bracket_after = true;
        }
        self.brackets.push(Bracket {
            node,
            image,
            active: true,
            bracket_after: false,
            delim_base: self.delims.len() as u32,
            text_start: self.pos,
        });
    }

    fn close_bracket(&mut self) {
        let p = self.pos;
        let Some(&br) = self.brackets.last() else {
            self.pos = p + 1;
            return;
        };
        if !br.active {
            self.brackets.pop();
            self.pos = p + 1;
            return;
        }
        if let Some(tail) = self.inline_link_tail(p + 1) {
            self.make_link(br, p, tail);
            return;
        }
        if let Some(tail) = self.reference(br, p) {
            self.make_link(br, p, tail);
            return;
        }
        self.brackets.pop();
        self.pos = p + 1;
    }

    /// `(destination "title")` at `i`, if it is there.
    fn inline_link_tail(&self, i: usize) -> Option<(LinkData, Tail)> {
        let (b, end) = (self.b, self.end);
        if i >= end || b[i] != b'(' {
            return None;
        }
        let ds = scan::skip_whitespace(b, i + 1, end);
        let de = scan::link_destination(b, ds, end)?;
        let mut j = scan::skip_whitespace(b, de, end);
        let mut title = None;
        if j > de
            && let Some(te) = scan::link_title(b, j, end)
        {
            title = Some((j, te));
            j = scan::skip_whitespace(b, te, end);
        }
        if j >= end || b[j] != b')' {
            return None;
        }
        let inner = if de > ds && b[ds] == b'<' {
            scan::trim_range(b, ds + 1, de - 1)
        } else {
            (ds, de)
        };
        let data = LinkData {
            kind: LinkKind::Inline,
            dest: Str::Text(inner.0 as u32, inner.1 as u32),
            title: title.map(|(s, e)| Str::Text(s as u32 + 1, e as u32 - 1)),
            label: None,
            text: (0, 0),
            dest_raw: (de > ds).then_some((ds as u32, de as u32)),
            label_raw: None,
        };
        Some((
            data,
            Tail::Inline {
                open: i,
                dest: (ds, de),
                title,
                close: j,
            },
        ))
    }

    /// A full, collapsed or shortcut reference for the bracket closed at `p`.
    fn reference(&mut self, br: Bracket, p: usize) -> Option<(LinkData, Tail)> {
        let label = scan::link_label(self.t, p + 1, self.end);
        let (kind, raw) = match label {
            Some((s, e, _)) if !scan::is_blank(&self.b[s..e]) => (LinkKind::Full, (s, e)),
            Some(_) if !br.bracket_after => (LinkKind::Collapsed, (br.text_start, p)),
            None if !br.bracket_after => (LinkKind::Shortcut, (br.text_start, p)),
            _ => return None,
        };
        let raw_text = &self.t[raw.0..raw.1];
        if kind != LinkKind::Full && !scan::chars_at_most(raw_text, scan::MAX_LABEL_CHARS) {
            return None;
        }
        let a = self.arena.len();
        scan::normalize_label_into(raw_text, &mut self.arena);
        let z = self.arena.len();
        if a == z {
            return None;
        }
        self.lookups.push((a as u32, z as u32));
        let refs = self.refs;
        let def = refs.lookup(&self.arena[a..z])?;
        let dest = self.push_arena(&def.destination);
        let title = def.title.as_deref().map(|t| self.push_arena(t));
        let data = LinkData {
            kind,
            dest,
            title,
            label: Some(Str::Arena(a as u32, z as u32)),
            text: (0, 0),
            dest_raw: None,
            label_raw: Some((raw.0 as u32, raw.1 as u32)),
        };
        let second = if kind == LinkKind::Shortcut {
            None
        } else {
            label
        };
        Some((data, Tail::Reference { second }))
    }

    fn push_arena(&mut self, s: &str) -> Str {
        let a = self.arena.len() as u32;
        self.arena.push_str(s);
        Str::Arena(a, self.arena.len() as u32)
    }

    /// Decodes an inline link's destination or title in place in the link
    /// data when it holds escapes or entities.
    fn decode(&mut self, s: Str) -> Str {
        let Str::Text(a, z) = s else {
            return s;
        };
        let raw = &self.t[a as usize..z as usize];
        if !scan::needs_decoding(raw.as_bytes()) {
            return s;
        }
        let start = self.arena.len() as u32;
        scan::decode_into(raw, &mut self.arena);
        Str::Arena(start, self.arena.len() as u32)
    }

    /// Turns the bracket closed at `p` into a link or image.
    fn make_link(&mut self, br: Bracket, p: usize, (mut data, tail): (LinkData, Tail)) {
        self.brackets.pop();
        data.text = (br.text_start as u32, p as u32);
        if data.kind == LinkKind::Inline {
            data.dest = self.decode(data.dest);
            data.title = data.title.map(|t| self.decode(t));
        }
        let index = self.links.len() as u32;
        self.links.push(data);
        let opener = br.node;
        let start = self.nodes[opener as usize].start as usize;
        let kind = if br.image { K::Image } else { K::Link };
        let link = self.new_node(kind, start, p + 1, index);
        // The link takes the opener's place and adopts it and everything
        // after it.
        let prev = self.nodes[opener as usize].prev;
        let root_last = self.nodes[0].last;
        self.node(link).prev = prev;
        if prev == NIL {
            self.node(0).first = link;
        } else {
            self.node(prev).next = link;
        }
        self.node(link).first = opener;
        self.node(link).last = root_last;
        self.node(opener).prev = NIL;
        self.node(opener).kind = K::Marker;
        self.node(0).last = link;
        self.process_emphasis(br.delim_base);
        let close = self.new_node(K::Marker, p, p + 1, 0);
        self.append(link, close);
        let end = match tail {
            Tail::Inline {
                open,
                dest,
                title,
                close,
            } => {
                self.child(link, K::Marker, open, open + 1);
                self.child(link, K::Whitespace, open + 1, dest.0);
                self.child(link, K::LinkDestination, dest.0, dest.1);
                let after_dest = match title {
                    Some((ts, te)) => {
                        self.child(link, K::Whitespace, dest.1, ts);
                        self.child(link, K::LinkTitle, ts, te);
                        te
                    }
                    None => dest.1,
                };
                self.child(link, K::Whitespace, after_dest, close);
                self.child(link, K::Marker, close, close + 1);
                close + 1
            }
            Tail::Reference { second } => match second {
                Some((s, e, after)) => {
                    self.child(link, K::Marker, s - 1, s);
                    let inner = if scan::is_blank(&self.b[s..e]) {
                        K::Whitespace
                    } else {
                        K::LinkLabel
                    };
                    self.child(link, inner, s, e);
                    self.child(link, K::Marker, e, after);
                    after
                }
                None => p + 1,
            },
        };
        self.node(link).end = end as u32;
        self.pos = end;
        if !br.image {
            // No links in links: deactivate the earlier link openers. Below
            // `link_floor` they are inactive already, so each bracket is
            // walked over at most once.
            let floor = self.link_floor.min(self.brackets.len());
            for b in &mut self.brackets[floor..] {
                if !b.image {
                    b.active = false;
                }
            }
            self.link_floor = self.brackets.len();
        }
    }

    /// Appends a token to `parent` unless it is empty.
    fn child(&mut self, parent: u32, kind: K, start: usize, end: usize) {
        if start < end {
            let n = self.new_node(kind, start, end, 0);
            self.append(parent, n);
        }
    }
}

/// U+0000 classifies as U+FFFD, which CommonMark parsers see in its place.
fn nul_as_replacement(c: char) -> char {
    if c == '\0' { '\u{FFFD}' } else { c }
}

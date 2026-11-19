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

use super::api::{DollarMath, InlineOptions, InlineTree, RefLookup};
use super::scan;
use super::tree::{self, ANode, InlineKind as K, LinkData, LinkKind, NIL, Segs, Str};
use super::unicode;

/// Bytes that end a text run: CommonMark's, plus `~` with strikethrough,
/// `:` and `w` with extended autolinks, `$` with math and the Obsidian
/// triggers `=`, `%`, `#` and `^` with any of their constructs. `flags`:
/// bit 0 strikethrough, bit 1 autolinks, bit 2 math, bit 3 Obsidian.
const fn special(flags: usize) -> [bool; 256] {
    let mut t = [false; 256];
    let specials = b"\n\r`\\&<*_[]!";
    let mut i = 0;
    while i < specials.len() {
        t[specials[i] as usize] = true;
        i += 1;
    }
    t[b'~' as usize] = flags & 1 != 0;
    t[b':' as usize] = flags & 2 != 0;
    t[b'w' as usize] = flags & 2 != 0;
    t[b'$' as usize] = flags & 4 != 0;
    let ob = flags & 8 != 0;
    t[b'=' as usize] = ob;
    t[b'%' as usize] = ob;
    t[b'#' as usize] = ob;
    t[b'^' as usize] = ob;
    t
}

static SPECIALS: [[bool; 256]; 16] = [
    special(0),
    special(1),
    special(2),
    special(3),
    special(4),
    special(5),
    special(6),
    special(7),
    special(8),
    special(9),
    special(10),
    special(11),
    special(12),
    special(13),
    special(14),
    special(15),
];

/// Whether `c` can be part of an Obsidian tag name: ASCII letters and
/// digits, `_`, `-`, `/`, and any other character that is not whitespace
/// or punctuation (letters of any script, emoji). The exact rule is still
/// to be checked against Obsidian; punctuation outside ASCII is
/// approximated by the Latin-1, General Punctuation, CJK and full-width
/// punctuation blocks.
pub fn is_tag_char(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/');
    }
    if c.is_alphanumeric() {
        return true;
    }
    !(c.is_whitespace()
        || matches!(c as u32,
            0x80..=0xBF
            | 0x2000..=0x206F
            | 0x2E00..=0x2E7F
            | 0x3000..=0x303F
            | 0xFE10..=0xFE1F
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF0F
            | 0xFF1A..=0xFF20
            | 0xFF3B..=0xFF40
            | 0xFF5B..=0xFF65))
}

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
    /// An inline footnote's `^[`.
    note: bool,
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
pub(super) fn parse(
    text: String,
    segs: Segs,
    refs: &dyn RefLookup,
    opts: InlineOptions,
) -> InlineTree {
    SCRATCH.with(|cell| match cell.try_borrow_mut() {
        Ok(mut scratch) => parse_with(&mut scratch, text, segs, refs, opts),
        // A lookup that parses inlines itself: use fresh vectors.
        Err(_) => parse_with(&mut Scratch::default(), text, segs, refs, opts),
    })
}

fn parse_with(
    scratch: &mut Scratch,
    text: String,
    segs: Segs,
    refs: &dyn RefLookup,
    opts: InlineOptions,
) -> InlineTree {
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
        footnotes: Vec::new(),
        refs,
        backticks: None,
        html_skip: 0,
        dollars: None,
        bracket_closers: [None; 2],
        comment_fail: 0,
        wiki_fail: 0,
        opts,
        special: &SPECIALS[usize::from(opts.strikethrough)
            | usize::from(opts.autolinks) << 1
            | usize::from(opts.math != DollarMath::Off || opts.math_code) << 2
            | usize::from(
                opts.highlight
                    || opts.comments
                    || opts.tags
                    || opts.inline_footnotes
                    || opts.block_ids,
            ) << 3],
    };
    p.run();
    let flat = tree::flatten(
        &p.nodes,
        segs.lines(),
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
        footnotes,
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
        footnotes,
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
    /// Normalized labels of footnote references (arena ranges).
    footnotes: Vec<(u32, u32)>,
    refs: &'a dyn RefLookup,
    /// Backtick runs after the first code span opener, as `(length, start)`,
    /// sorted: built on first use, so closers are found in O(log n).
    backticks: Option<Vec<(u32, u32)>>,
    html_skip: u8,
    /// Runs of one or two `$` that can close math after the first opener,
    /// as `(length, start)`, sorted: built on first use, so closers are
    /// found in O(log n).
    dollars: Option<Vec<(u32, u32)>>,
    /// The last search for `\)` and `\]` (index 0 and 1): where it started
    /// and what it found. Openers come in increasing order, so the
    /// searches never overlap.
    bracket_closers: [Option<(usize, Option<usize>)>; 2],
    /// `%%` openers before this offset have no closer.
    comment_fail: usize,
    /// `[[` openers before this offset have no `]]` on their line.
    wiki_fail: usize,
    opts: InlineOptions,
    /// The bytes that end a text run.
    special: &'static [bool; 256],
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
        // A block id ends the content: `^id` after whitespace, or alone.
        let block_id = if self.opts.block_ids {
            block_id_at_end(b, start, end)
        } else {
            None
        };
        self.pos = start;
        self.end = block_id.map_or(end, |(ws, _)| ws);
        while self.pos < self.end {
            match b[self.pos] {
                b'\n' | b'\r' => self.newline(),
                b'`' => self.backticks(),
                b'\\' => self.backslash(),
                b'&' => self.entity(),
                b'<' => self.pointy(),
                c @ (b'*' | b'_') => self.delimiter_run(c),
                b'~' if self.opts.strikethrough => self.delimiter_run(b'~'),
                b'=' if self.opts.highlight => self.delimiter_run(b'='),
                b'%' if self.opts.comments => self.comment(),
                b'#' if self.opts.tags => self.tag(),
                b'^' if self.opts.inline_footnotes
                    && self.pos + 1 < self.end
                    && b[self.pos + 1] == b'[' =>
                {
                    let p = self.pos;
                    let n = self.add_provisional(p, p + 2);
                    self.pos += 2;
                    self.push_bracket(n, false);
                    if let Some(top) = self.brackets.last_mut() {
                        top.note = true;
                    }
                }
                b'$' if self.special[usize::from(b'$')] => self.dollar(),
                b'w' if self.opts.autolinks => self.www_autolink(),
                b':' if self.opts.autolinks => self.url_autolink(),
                b'[' => {
                    let p = self.pos;
                    if self.opts.wikilinks
                        && p + 1 < self.end
                        && b[p + 1] == b'['
                        && self.wikilink(p, false)
                    {
                        continue;
                    }
                    let n = self.add_provisional(p, p + 1);
                    self.pos += 1;
                    self.push_bracket(n, false);
                }
                b'!' if self.pos + 1 < self.end && b[self.pos + 1] == b'[' => {
                    let p = self.pos;
                    if self.opts.embeds
                        && p + 2 < self.end
                        && b[p + 2] == b'['
                        && self.wikilink(p, true)
                    {
                        continue;
                    }
                    let n = self.add_provisional(p, p + 2);
                    self.pos += 2;
                    self.push_bracket(n, true);
                }
                b']' => self.close_bracket(),
                _ => self.text(),
            }
        }
        self.process_emphasis(0);
        if self.opts.autolinks {
            self.email_autolinks();
        }
        if let Some((ws, caret)) = block_id {
            if ws < caret {
                self.add(K::Whitespace, ws, caret, 0);
            }
            let n = self.add(K::BlockId, caret, end, 0);
            self.child(n, K::Marker, caret, caret + 1);
            self.child(n, K::BlockIdText, caret + 1, end);
        }
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
        while j < self.end && !self.special[self.b[j] as usize] {
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
            if self.opts.math_brackets && matches!(c, b'(' | b'[') && self.bracket_math(p) {
                return;
            }
            // GitHub finds math before Markdown escapes: `\$x$` is a
            // backslash and math.
            if c == b'$' && self.opts.math == DollarMath::GitHub && self.math_at(p + 1) {
                return;
            }
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

    // -----------------------------------------------------------------------
    // Math

    /// `$`: math, or literal dollars.
    fn dollar(&mut self) {
        let p = self.pos;
        if !self.math_at(p) {
            let mut q = p;
            while q < self.end && self.b[q] == b'$' {
                q += 1;
            }
            self.pos = q;
        }
    }

    /// Math starting at the `$` at `p`: `` $`…`$ ``, `$…$` or `$$…$$`.
    /// Adds the node, moves past it and returns true; false if `p` opens
    /// none.
    fn math_at(&mut self, p: usize) -> bool {
        let (b, end) = (self.b, self.end);
        let mut q = p;
        while q < end && b[q] == b'$' {
            q += 1;
        }
        let n = q - p;
        if n == 1 && self.opts.math_code && q < end && b[q] == b'`' {
            let mut a = q;
            while a < end && b[a] == b'`' {
                a += 1;
            }
            let m = a - q;
            if let Some(close) = self.closing_backticks(a, m)
                && close + m < end
                && b[close + m] == b'$'
            {
                let z = close + m + 1;
                self.add_math(K::InlineMath, p, z, 1 + m, m + 1);
                return true;
            }
        }
        if self.opts.math == DollarMath::Off || n > 2 || q >= end {
            return false;
        }
        if n == 1 && is_cmark_space(b[q]) {
            return false;
        }
        let Some(close) = self.dollar_closer(q, n) else {
            return false;
        };
        let kind = if n == 1 {
            K::InlineMath
        } else {
            K::DisplayMath
        };
        self.add_math(kind, p, close + n, n, n);
        true
    }

    /// The first run of exactly `n` dollars at or after `from` that can
    /// close math.
    fn dollar_closer(&mut self, from: usize, n: usize) -> Option<usize> {
        let (b, end) = (self.b, self.end);
        let github = self.opts.math == DollarMath::GitHub;
        let runs = self.dollars.get_or_insert_with(|| {
            let mut runs = Vec::new();
            let mut j = from;
            while let Some(k) = memchr::memchr(b'$', &b[j..end]) {
                let s = j + k;
                let mut e = s;
                while e < end && b[e] == b'$' {
                    e += 1;
                }
                j = e;
                let mut slashes = 0;
                while s > slashes && b[s - slashes - 1] == b'\\' {
                    slashes += 1;
                }
                if slashes % 2 == 1 {
                    continue;
                }
                let closes = match e - s {
                    // Not after a blank; not before a letter or digit
                    // (GitHub) or a digit (Pandoc).
                    1 => {
                        s > 0
                            && !is_cmark_space(b[s - 1])
                            && (e >= end
                                || if github {
                                    !b[e].is_ascii_alphanumeric()
                                } else {
                                    !b[e].is_ascii_digit()
                                })
                    }
                    2 => true,
                    _ => false,
                };
                if closes {
                    runs.push(((e - s) as u32, s as u32));
                }
            }
            runs.sort_unstable();
            runs
        });
        let key = (n as u32, from as u32);
        let i = runs.partition_point(|&r| r < key);
        runs.get(i).filter(|r| r.0 == key.0).map(|r| r.1 as usize)
    }

    /// `\(…\)` or `\[…\]` at the backslash `p`: adds the node and returns
    /// true if it closes.
    fn bracket_math(&mut self, p: usize) -> bool {
        let display = self.b[p + 1] == b'[';
        let closer = if display { b']' } else { b')' };
        let slot = usize::from(display);
        let from = p + 2;
        let found = match self.bracket_closers[slot] {
            Some((_, Some(c))) if c >= from => Some(c),
            Some((start, None)) if start <= from => None,
            _ => {
                let (b, end) = (self.b, self.end);
                let mut j = from;
                let mut found = None;
                while let Some(k) = memchr::memchr(b'\\', &b[j..end]) {
                    let s = j + k;
                    if s + 1 < end && b[s + 1] == closer {
                        found = Some(s);
                        break;
                    }
                    j = s + 1;
                }
                self.bracket_closers[slot] = Some((from, found));
                found
            }
        };
        let Some(close) = found else {
            return false;
        };
        let kind = if display {
            K::DisplayMath
        } else {
            K::InlineMath
        };
        self.add_math(kind, p, close + 2, 2, 2);
        true
    }

    /// Appends a math node over `start..end` with markers of `open` and
    /// `close` bytes, and moves past it.
    fn add_math(&mut self, kind: K, start: usize, end: usize, open: usize, close: usize) {
        let data = open as u32 | (close as u32) << 16;
        self.add_wrapped(
            kind,
            start,
            end,
            (start + open, end - close),
            K::MathText,
            data,
        );
        self.pos = end;
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
                ext: false,
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
    // GFM extended autolinks (cmark-gfm's `autolink` extension)

    /// `w`: a `www.` autolink, or literal text.
    fn www_autolink(&mut self) {
        let p = self.pos;
        let b = self.b;
        let ok = self.brackets.is_empty()
            && (p == 0
                || is_cmark_space(b[p - 1])
                || matches!(b[p - 1], b'*' | b'_' | b'~' | b'('))
            && b[p..self.end].starts_with(b"www.");
        let len = if ok {
            check_domain(self.t, p, self.end, false).and_then(|d| {
                let end = link_extent(b, p + d, self.end);
                let n = autolink_delim(&b[p..end]);
                (n > 0).then_some(n)
            })
        } else {
            None
        };
        let Some(len) = len else {
            self.pos = p + 1;
            return;
        };
        let start = self.arena.len() as u32;
        self.arena.push_str("http://");
        self.arena.push_str(&self.t[p..p + len]);
        let dest = Str::Arena(start, self.arena.len() as u32);
        self.add_ext_autolink(p, p + len, LinkKind::UriAutolink, dest);
        self.pos = p + len;
    }

    /// `:`: an `http://`, `https://` or `ftp://` autolink whose scheme was
    /// just scanned as text, or literal text.
    fn url_autolink(&mut self) {
        let p = self.pos;
        let b = self.b;
        let end = self.end;
        let last = self.nodes[0].last;
        let floor = if last == NIL {
            0
        } else {
            self.nodes[last as usize].end as usize
        };
        let mut found = None;
        if self.brackets.is_empty() && end - p >= 4 && b[p + 1] == b'/' && b[p + 2] == b'/' {
            let mut rewind = 0;
            while p - rewind > floor && b[p - rewind - 1].is_ascii_alphabetic() {
                rewind += 1;
            }
            let scheme = &b[p - rewind..p];
            let safe = (scheme.eq_ignore_ascii_case(b"http")
                || scheme.eq_ignore_ascii_case(b"https")
                || scheme.eq_ignore_ascii_case(b"ftp"))
                && p + 3 < end
                && is_valid_hostchar(self.t, p + 3);
            if safe && let Some(d) = check_domain(self.t, p + 3, end, true) {
                let e = link_extent(b, p + 3 + d, end);
                let n = autolink_delim(&b[p..e]);
                if n > 0 {
                    found = Some((p - rewind, p + n));
                }
            }
        }
        let Some((a, z)) = found else {
            self.pos = p + 1;
            return;
        };
        let dest = Str::Text(a as u32, z as u32);
        self.add_ext_autolink(a, z, LinkKind::UriAutolink, dest);
        self.pos = z;
    }

    /// Appends an extended autolink over `a..z` (the text as written).
    fn add_ext_autolink(&mut self, a: usize, z: usize, kind: LinkKind, dest: Str) {
        let n = self.new_extended_autolink(a, z, kind, dest);
        self.append(0, n);
    }

    /// A detached extended autolink node over `a..z` with its text token.
    fn new_extended_autolink(&mut self, a: usize, z: usize, kind: LinkKind, dest: Str) -> u32 {
        let index = self.links.len() as u32;
        self.links.push(LinkData {
            kind,
            dest,
            title: None,
            label: None,
            text: (a as u32, z as u32),
            dest_raw: Some((a as u32, z as u32)),
            label_raw: None,
            ext: true,
        });
        let n = self.new_node(K::Autolink, a, z, index);
        let t = self.new_node(K::AutolinkText, a, z, 0);
        self.append(n, t);
        n
    }

    /// Bare email addresses (and `mailto:`/`xmpp:` addresses) in the text
    /// of the root's children and, recursively, of its emphasis,
    /// strikethrough and image descendants; links are skipped. Runs after
    /// emphasis processing, like cmark-gfm's post-processor. A run of text
    /// is the text between nodes plus unmatched delimiters and brackets
    /// (cmark-gfm's consolidated text nodes, except that escapes and
    /// entities end a run here).
    fn email_autolinks(&mut self) {
        let mut todo = vec![0u32];
        while let Some(parent) = todo.pop() {
            let pn = self.nodes[parent as usize];
            let (inner_start, inner_end) = if parent == 0 {
                (0, self.b.len())
            } else {
                (pn.start as usize, pn.end as usize)
            };
            let mut run_start = inner_start;
            let mut child = pn.first;
            loop {
                let run_end = if child == NIL {
                    inner_end
                } else {
                    let c = self.nodes[child as usize];
                    if c.kind == K::Text {
                        child = c.next;
                        continue;
                    }
                    c.start as usize
                };
                if run_end > run_start
                    && memchr::memchr(b'@', &self.b[run_start..run_end]).is_some()
                {
                    self.emails_in_run(parent, run_start, run_end);
                }
                if child == NIL {
                    break;
                }
                let c = self.nodes[child as usize];
                if matches!(
                    c.kind,
                    K::Emphasis | K::Strong | K::Strikethrough | K::Image
                ) {
                    todo.push(child);
                }
                run_start = c.end as usize;
                child = c.next;
            }
        }
    }

    /// Links the email addresses in the text run `a..z` of `parent`.
    fn emails_in_run(&mut self, parent: u32, a: usize, z: usize) {
        let mut from = a;
        while let Some(at) = memchr::memchr(b'@', &self.b[from..z]).map(|i| from + i) {
            match email_at(self.b, from, at, z) {
                Some((s, e, mailto)) => {
                    let (kind, dest) = if mailto {
                        (LinkKind::EmailAutolink, Str::Text(s as u32, e as u32))
                    } else {
                        (LinkKind::UriAutolink, Str::Text(s as u32, e as u32))
                    };
                    let link = self.new_extended_autolink(s, e, kind, dest);
                    self.splice_text(parent, link, s, e);
                    from = e;
                }
                None => from = at + 1,
            }
        }
    }

    /// Puts `link` (covering `s..e`, all text) among the children of
    /// `parent`: the unmatched delimiter or bracket nodes it covers are
    /// removed or cut.
    fn splice_text(&mut self, parent: u32, link: u32, s: usize, e: usize) {
        let (s32, e32) = (s as u32, e as u32);
        // The first child that ends after `s`.
        let mut c = self.nodes[parent as usize].first;
        while c != NIL && self.nodes[c as usize].end <= s32 {
            c = self.nodes[c as usize].next;
        }
        // Cut or drop the text nodes that overlap `s..e`.
        while c != NIL && self.nodes[c as usize].start < e32 {
            let n = self.nodes[c as usize];
            debug_assert_eq!(n.kind, K::Text);
            let next = n.next;
            if n.start < s32 && n.end > e32 {
                // Spans the link: keep the head, add the tail.
                let tail = self.new_node(K::Text, e, n.end as usize, 0);
                self.node(c).end = s32;
                self.insert_after(parent, c, tail);
                c = tail;
                break;
            } else if n.start < s32 {
                self.node(c).end = s32;
            } else if n.end > e32 {
                self.node(c).start = e32;
                break;
            } else {
                self.unlink(parent, c);
            }
            c = next;
        }
        // `c` is the first child after the link (or NIL).
        if c == NIL {
            self.append(parent, link);
        } else {
            let prev = self.nodes[c as usize].prev;
            if prev == NIL {
                self.node(parent).first = link;
            } else {
                self.node(prev).next = link;
            }
            self.node(link).prev = prev;
            self.node(link).next = c;
            self.node(c).prev = link;
        }
    }

    fn insert_after(&mut self, parent: u32, after: u32, n: u32) {
        let next = self.nodes[after as usize].next;
        self.node(n).prev = after;
        self.node(n).next = next;
        self.node(after).next = n;
        if next == NIL {
            self.node(parent).last = n;
        } else {
            self.node(next).prev = n;
        }
    }

    fn unlink(&mut self, parent: u32, n: u32) {
        let ANode { prev, next, .. } = self.nodes[n as usize];
        if prev == NIL {
            self.node(parent).first = next;
        } else {
            self.node(prev).next = next;
        }
        if next == NIL {
            self.node(parent).last = prev;
        } else {
            self.node(next).prev = prev;
        }
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
        let (can_open, can_close) = if ch == b'_' {
            (left && (!right || p_before), right && (!left || p_after))
        } else {
            (left, right)
        };
        let node = self.add_provisional(start, end);
        self.pos = end;
        // Strikethrough: runs of two tildes, or of one where allowed.
        // Highlight: runs of exactly two `=`.
        let n = end - start;
        let len_ok = match ch {
            b'~' => n == 2 || (n == 1 && self.opts.strikethrough_single),
            b'=' => n == 2,
            _ => true,
        };
        if (can_open || can_close) && len_ok {
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
        let mut openers_bottom = [[[lower; 2]; 3]; 4];
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
            let ch_index = match c.ch {
                b'*' => 0,
                b'_' => 1,
                b'~' => 2,
                _ => 3,
            };
            let slot = &mut openers_bottom[ch_index][(c.len % 3) as usize][usize::from(c.can_open)];
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
        let exact = matches!(o.ch, b'~' | b'=');
        if exact && o_len != c_len {
            // cmark-gfm: tildes of different lengths make no strikethrough;
            // the closer, the opener and every delimiter between them are
            // dropped.
            let mut d = closer;
            while d != opener && d != NIL {
                let prev = self.delims[d as usize].prev;
                self.remove_delim(d);
                d = prev;
            }
            self.remove_delim(opener);
            return c.next;
        }
        let used = if exact {
            o_len
        } else if o_len >= 2 && c_len >= 2 {
            2
        } else {
            1
        };
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
        let kind = if o.ch == b'~' {
            K::Strikethrough
        } else if o.ch == b'=' {
            K::Highlight
        } else if used == 2 {
            K::Strong
        } else {
            K::Emphasis
        };
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
            note: false,
        });
    }

    fn close_bracket(&mut self) {
        let p = self.pos;
        let Some(&br) = self.brackets.last() else {
            self.pos = p + 1;
            return;
        };
        if br.note {
            self.inline_footnote(br, p);
            return;
        }
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
        if self.footnote_ref(br, p) {
            return;
        }
        self.brackets.pop();
        self.pos = p + 1;
    }

    /// `[^label]` closed at `p` that is no link: a footnote reference if a
    /// footnote is defined for the label (cmark-gfm). The label is on one
    /// line and holds only text (no code spans, autolinks or raw HTML);
    /// delimiters inside it are text.
    fn footnote_ref(&mut self, br: Bracket, p: usize) -> bool {
        // A label holds at most 999 characters, like a link label: this
        // also keeps nested brackets linear.
        if !self.opts.footnotes
            || br.image
            || br.bracket_after
            || p - br.text_start > 4 * scan::MAX_LABEL_CHARS
        {
            return false;
        }
        let raw = &self.t[br.text_start..p];
        if !scan::chars_at_most(raw, scan::MAX_LABEL_CHARS + 1) {
            return false;
        }
        let Some(label) = raw.strip_prefix('^') else {
            return false;
        };
        if label.is_empty() || label.contains(['\n', '\r']) {
            return false;
        }
        let opener = br.node;
        let mut c = self.nodes[opener as usize].next;
        while c != NIL {
            if self.nodes[c as usize].kind != K::Text {
                return false;
            }
            c = self.nodes[c as usize].next;
        }
        let a = self.arena.len();
        scan::normalize_label_into(label, &mut self.arena);
        let z = self.arena.len();
        if a == z || !self.refs.has_footnote(&self.arena[a..z]) {
            self.arena.truncate(a);
            return false;
        }
        while self.last_delim != NIL && self.last_delim >= br.delim_base {
            self.remove_delim(self.last_delim);
        }
        // The opener and the text after it become the reference.
        let start = self.nodes[opener as usize].start as usize;
        let prev = self.nodes[opener as usize].prev;
        if prev == NIL {
            self.node(0).first = NIL;
        } else {
            self.node(prev).next = NIL;
        }
        self.node(0).last = prev;
        let index = self.footnotes.len() as u32;
        self.footnotes.push((a as u32, z as u32));
        let n = self.add(K::FootnoteRef, start, p + 1, index);
        self.child(n, K::Marker, start, start + 2);
        self.child(n, K::FootnoteLabel, start + 2, p);
        self.child(n, K::Marker, p, p + 1);
        self.brackets.pop();
        self.pos = p + 1;
        true
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
            ext: false,
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
            ext: false,
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
                if !b.image && !b.note {
                    b.active = false;
                }
            }
            self.link_floor = self.brackets.len();
        }
    }

    /// `^[note]` closed at `p`: wraps the opener and everything after it
    /// in an inline footnote.
    fn inline_footnote(&mut self, br: Bracket, p: usize) {
        self.brackets.pop();
        let opener = br.node;
        let start = self.nodes[opener as usize].start as usize;
        let note = self.new_node(K::InlineFootnote, start, p + 1, 0);
        let prev = self.nodes[opener as usize].prev;
        let root_last = self.nodes[0].last;
        self.node(note).prev = prev;
        if prev == NIL {
            self.node(0).first = note;
        } else {
            self.node(prev).next = note;
        }
        self.node(note).first = opener;
        self.node(note).last = root_last;
        self.node(opener).prev = NIL;
        self.node(opener).kind = K::Marker;
        self.node(0).last = note;
        self.process_emphasis(br.delim_base);
        let close = self.new_node(K::Marker, p, p + 1, 0);
        self.append(note, close);
        self.pos = p + 1;
    }

    // -----------------------------------------------------------------------
    // Obsidian

    /// `%%`: an inline comment if a run of exactly two `%` closes it later
    /// in the content, across soft line breaks (Obsidian); otherwise text.
    fn comment(&mut self) {
        let (b, p) = (self.b, self.pos);
        let mut q = p;
        while q < self.end && b[q] == b'%' {
            q += 1;
        }
        if q - p != 2 || p < self.comment_fail {
            self.pos = q;
            return;
        }
        let mut i = q;
        while let Some(k) = memchr::memchr(b'%', &b[i..self.end]) {
            let a = i + k;
            let mut z = a;
            while z < self.end && b[z] == b'%' {
                z += 1;
            }
            if z - a == 2 {
                self.add_wrapped(K::Comment, p, z, (q, a), K::CommentText, 0);
                self.pos = z;
                return;
            }
            i = z;
        }
        self.comment_fail = self.end;
        self.pos = q;
    }

    /// `#`: a tag after whitespace or at the start, with at least one
    /// character that is not a digit; otherwise text.
    fn tag(&mut self) {
        let p = self.pos;
        let before = self.t[..p].chars().next_back();
        let mut e = p + 1;
        let mut digits_only = true;
        if before.is_none_or(char::is_whitespace) {
            for c in self.t[p + 1..self.end].chars() {
                if !is_tag_char(c) {
                    break;
                }
                digits_only &= c.is_ascii_digit() || c == '/';
                e += c.len_utf8();
            }
        }
        if e == p + 1 || digits_only {
            self.pos = p + 1;
            return;
        }
        let n = self.add(K::Tag, p, e, 0);
        self.child(n, K::Marker, p, p + 1);
        self.child(n, K::TagText, p + 1, e);
        self.pos = e;
    }

    /// `[[` (or `![[` with `embed`) at `p`: a wikilink or embed if `]]`
    /// follows on the same line, with no `[[` before it and something
    /// between. Adds the node, moves past it and returns true.
    fn wikilink(&mut self, p: usize, embed: bool) -> bool {
        let b = self.b;
        let open = p + if embed { 3 } else { 2 };
        if p < self.wiki_fail {
            return false;
        }
        let mut i = open;
        let close = loop {
            match memchr::memchr3(b']', b'[', b'\n', &b[i..self.end]).map(|k| i + k) {
                Some(k) if b[k] == b']' && b.get(k + 1) == Some(&b']') && k + 1 < self.end => {
                    break k;
                }
                Some(k) if b[k] == b'[' && b.get(k + 1) == Some(&b'[') => return false,
                Some(k) if b[k] == b'\n' => {
                    self.wiki_fail = k;
                    return false;
                }
                Some(k) => i = k + 1,
                None => {
                    self.wiki_fail = self.end;
                    return false;
                }
            }
        };
        if b[open..close]
            .iter()
            .all(|&c| matches!(c, b' ' | b'\t' | b'\r'))
            || b[open..close].contains(&b'\r')
        {
            return false;
        }
        // The first `|` (or `\|`, as tables need it) ends the target.
        let pipe = memchr::memchr(b'|', &b[open..close]).map(|k| {
            let q = open + k;
            if q > open && b[q - 1] == b'\\' {
                (q - 1, q)
            } else {
                (q, q)
            }
        });
        let kind = if embed { K::Embed } else { K::Wikilink };
        let data = pipe.map_or(0, |(t, _)| (t - p + 1) as u32);
        let n = self.add(kind, p, close + 2, data);
        self.child(n, K::Marker, p, open);
        match pipe {
            Some((t, q)) => {
                self.child(n, K::WikiTarget, open, t);
                self.child(n, K::Marker, t, q + 1);
                self.child(n, K::WikiAlias, q + 1, close);
            }
            None => self.child(n, K::WikiTarget, open, close),
        }
        self.child(n, K::Marker, close, close + 2);
        self.pos = close + 2;
        true
    }

    /// Appends a token to `parent` unless it is empty.
    fn child(&mut self, parent: u32, kind: K, start: usize, end: usize) {
        if start < end {
            let n = self.new_node(kind, start, end, 0);
            self.append(parent, n);
        }
    }
}

/// cmark's `isspace`: space, tab, LF, VT, FF, CR.
#[inline]
/// A block id ending `b[start..end]`: `^` and one or more ASCII letters,
/// digits or `-` up to `end`, after whitespace or at `start`. Returns where
/// the whitespace before it starts and where the `^` is.
fn block_id_at_end(b: &[u8], start: usize, end: usize) -> Option<(usize, usize)> {
    let mut k = end;
    while k > start && (b[k - 1].is_ascii_alphanumeric() || b[k - 1] == b'-') {
        k -= 1;
    }
    if k == end || k == start || b[k - 1] != b'^' {
        return None;
    }
    let caret = k - 1;
    let mut ws = caret;
    while ws > start && matches!(b[ws - 1], b' ' | b'\t') {
        ws -= 1;
    }
    (ws < caret || caret == start || matches!(b[caret - 1], b'\n' | b'\r')).then_some((ws, caret))
}

fn is_cmark_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// cmark-gfm's `is_valid_hostchar` for the character at byte `i` of `t`:
/// not whitespace and not punctuation. A position inside a multi-byte
/// character is not valid (cmark-gfm decodes from each byte).
fn is_valid_hostchar(t: &str, i: usize) -> bool {
    match t.get(i..).and_then(|r| r.chars().next()) {
        Some(c) => !unicode::is_whitespace(c) && !unicode::is_punctuation(c),
        None => false,
    }
}

/// cmark-gfm's `check_domain` over `t[start..end]`: the length of the
/// domain-like prefix (scanned from its second byte, as cmark-gfm does),
/// or `None` when an underscore sits in one of its last two labels (unless
/// it has more than ten dots) or, unless `allow_short`, it has no dot.
fn check_domain(t: &str, start: usize, end: usize, allow_short: bool) -> Option<usize> {
    let b = &t.as_bytes()[start..end];
    let size = b.len();
    let (mut np, mut uscore1, mut uscore2) = (0usize, 0usize, 0usize);
    let mut i = 1;
    while i + 1 < size {
        if b[i] == b'\\' && i + 2 < size {
            i += 1;
        }
        if b[i] == b'_' {
            uscore2 += 1;
        } else if b[i] == b'.' {
            uscore1 = uscore2;
            uscore2 = 0;
            np += 1;
        } else if !is_valid_hostchar(t, start + i) && b[i] != b'-' {
            break;
        }
        i += 1;
    }
    if (uscore1 > 0 || uscore2 > 0) && np <= 10 {
        return None;
    }
    if allow_short || np > 0 {
        Some(i.max(1).min(size))
    } else {
        None
    }
}

/// Extends a link from `i` to the next whitespace or `<`.
fn link_extent(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && !is_cmark_space(b[i]) && b[i] != b'<' {
        i += 1;
    }
    i
}

/// cmark-gfm's `autolink_delim`: the length of `link` once trailing
/// punctuation, unbalanced closing parentheses and a trailing entity-like
/// `&name;` are cut, and everything from a `<` on.
fn autolink_delim(link: &[u8]) -> usize {
    let mut end = link.len();
    let (mut opening, mut closing) = (0usize, 0usize);
    for (i, &c) in link.iter().enumerate() {
        match c {
            b'<' => {
                end = i;
                break;
            }
            b'(' => opening += 1,
            b')' => closing += 1,
            _ => {}
        }
    }
    while end > 0 {
        match link[end - 1] {
            b')' => {
                if closing <= opening {
                    return end;
                }
                closing -= 1;
                end -= 1;
            }
            b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~' | b'\'' | b'"' => end -= 1,
            b';' => {
                let mut k = end.saturating_sub(2);
                while k > 0 && link[k].is_ascii_alphabetic() {
                    k -= 1;
                }
                if end >= 2 && k < end - 2 && link[k] == b'&' {
                    end = k;
                } else {
                    end -= 1;
                }
            }
            _ => return end,
        }
    }
    end
}

/// Whether `b[..i]` ends with `protocol` (which ends with `:`) preceded by
/// the start of the text (`floor`) or a non-alphanumeric byte.
fn validate_protocol(b: &[u8], floor: usize, i: usize, protocol: &[u8]) -> bool {
    i >= floor + protocol.len()
        && &b[i - protocol.len()..i] == protocol
        && (i - protocol.len() == floor || !b[i - protocol.len() - 1].is_ascii_alphanumeric())
}

/// cmark-gfm's email autolink at the `@` at `at`, in text that starts at
/// `floor` and ends at `end`: the link's range and whether it gets
/// `mailto:` (`false` when it is written with `mailto:` or `xmpp:`).
fn email_at(b: &[u8], floor: usize, at: usize, end: usize) -> Option<(usize, usize, bool)> {
    let mut auto_mailto = true;
    let mut is_xmpp = false;
    let mut s = at;
    while s > floor {
        let c = b[s - 1];
        if c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'-' | b'_') {
            s -= 1;
            continue;
        }
        if c == b':' {
            if validate_protocol(b, floor, s, b"mailto:") {
                auto_mailto = false;
                s -= 1;
                continue;
            }
            if validate_protocol(b, floor, s, b"xmpp:") {
                auto_mailto = false;
                is_xmpp = true;
                s -= 1;
                continue;
            }
        }
        break;
    }
    if s == at {
        return None;
    }
    let size = end - at;
    let mut np = 0;
    let mut n = 0;
    while n < size {
        let c = b[at + n];
        if c.is_ascii_alphanumeric() {
        } else if c == b'@' {
            // cmark-gfm counts the `@`s and fails on more than one; stopping
            // at the second keeps a run of `@`s linear.
            if n > 0 {
                return None;
            }
        } else if c == b'.' && n + 1 < size && b[at + n + 1].is_ascii_alphanumeric() {
            np += 1;
        } else if c == b'/' && is_xmpp {
        } else if c != b'-' && c != b'_' {
            break;
        }
        n += 1;
    }
    if n < 2 || np == 0 || (!b[at + n - 1].is_ascii_alphabetic() && b[at + n - 1] != b'.') {
        return None;
    }
    let n = autolink_delim(&b[at..at + n]);
    if n == 0 {
        return None;
    }
    Some((s, at + n, auto_mailto))
}

/// U+0000 classifies as U+FFFD, which CommonMark parsers see in its place.
fn nul_as_replacement(c: char) -> char {
    if c == '\0' { '\u{FFFD}' } else { c }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Intermediate document → Markdown text.
//!
//! Inline formatting arrives as flat runs and is turned back into nested
//! delimiters with a stack. Each delimiter is then checked against the
//! CommonMark flanking rules in its final context; one that would not
//! parse (`foo**bar**baz`, `**"x"**y`) falls back to the equivalent inline
//! HTML tag, which every profile renders.

use crate::profile::Profile;

use super::escape::{
    Mode, code_span, destination, escape_line_start, escape_text, html_escape, title,
};
use super::ir::{Align, Block, Cell, Inline, Item, Link, List, Run, Style};

/// Rounds of the flanking fix-up before every remaining delimiter falls
/// back to HTML.
const MAX_FIXUP_ROUNDS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Link(u32),
    Strong,
    Em,
    Strike,
    Sub,
    Sup,
    Highlight,
}

impl Mark {
    fn has(self, s: &Style) -> bool {
        match self {
            Mark::Link(id) => s.link == Some(id),
            Mark::Strong => s.strong,
            Mark::Em => s.em,
            Mark::Strike => s.strike,
            Mark::Sub => s.sub,
            Mark::Sup => s.sup,
            Mark::Highlight => s.mark,
        }
    }

    /// The Markdown delimiter, or `None` for links.
    fn delim(self) -> Option<&'static str> {
        match self {
            Mark::Link(_) => None,
            Mark::Strong => Some("**"),
            Mark::Em => Some("*"),
            Mark::Strike => Some("~~"),
            Mark::Sub => Some("~"),
            Mark::Sup => Some("^"),
            Mark::Highlight => Some("=="),
        }
    }

    fn html_tag(self) -> &'static str {
        match self {
            Mark::Link(_) => "a",
            Mark::Strong => "strong",
            Mark::Em => "em",
            Mark::Strike => "del",
            Mark::Sub => "sub",
            Mark::Sup => "sup",
            Mark::Highlight => "mark",
        }
    }
}

/// The marks of a style, in canonical order.
fn marks(s: &Style) -> Vec<Mark> {
    let mut v = Vec::with_capacity(4);
    if let Some(id) = s.link {
        v.push(Mark::Link(id));
    }
    for m in [
        Mark::Strong,
        Mark::Em,
        Mark::Strike,
        Mark::Highlight,
        Mark::Sub,
        Mark::Sup,
    ] {
        if m.has(s) {
            v.push(m);
        }
    }
    v
}

#[derive(Debug)]
enum Tok {
    Lit(String),
    Open(usize),
    Close(usize),
}

#[derive(Debug)]
struct Pair {
    mark: Mark,
    html: bool,
}

/// Writes Markdown for one profile.
#[derive(Debug)]
pub(crate) struct Emitter<'a> {
    profile: Profile,
    links: &'a [Link],
}

fn is_md_ws(c: Option<char>) -> bool {
    c.is_none_or(char::is_whitespace)
}

fn is_md_punct(c: Option<char>) -> bool {
    // Non-ASCII: anything that is not a letter, digit or space counts as
    // punctuation. That is stricter than CommonMark for marks, which only
    // causes an unneeded HTML fallback.
    c.is_some_and(|c| {
        c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace())
    })
}

fn left_flanking(prev: Option<char>, next: Option<char>) -> bool {
    !is_md_ws(next) && (!is_md_punct(next) || is_md_ws(prev) || is_md_punct(prev))
}

fn right_flanking(prev: Option<char>, next: Option<char>) -> bool {
    !is_md_ws(prev) && (!is_md_punct(prev) || is_md_ws(next) || is_md_punct(next))
}

impl<'a> Emitter<'a> {
    pub(crate) fn new(profile: Profile, links: &'a [Link]) -> Self {
        Emitter { profile, links }
    }

    /// The whole document.
    pub(crate) fn document(&self, blocks: &[Block]) -> String {
        let mut out = self.blocks(blocks).join("\n");
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        out
    }

    /// Only the flags this profile renders, so that runs whose styles
    /// differ only in unrendered flags merge (no delimiter between them).
    fn effective(&self, mut s: Style) -> Style {
        if !self.profile.highlight() {
            s.mark = false;
        }
        s
    }

    // ----- blocks -----------------------------------------------------

    fn blocks(&self, blocks: &[Block]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut prev_list: Option<(bool, bool)> = None; // (ordered, alternate)
        for b in blocks {
            let mut alt = false;
            if let Block::List(l) = b {
                alt = matches!(prev_list, Some((o, a)) if o == l.ordered && !a);
            }
            let lines = self.block(b, alt);
            if lines.is_empty() {
                continue;
            }
            prev_list = match b {
                Block::List(l) => Some((l.ordered, alt)),
                _ => None,
            };
            if !out.is_empty() {
                out.push(String::new());
            }
            out.extend(lines);
        }
        out
    }

    fn block(&self, b: &Block, alt: bool) -> Vec<String> {
        match b {
            Block::Para(runs) => self.para(runs),
            Block::Heading(level, runs) => {
                let mut text = self.inline(runs, Mode::Heading);
                if text.is_empty() {
                    return Vec::new();
                }
                if text.ends_with('#') {
                    text.insert(text.len() - 1, '\\');
                }
                vec![format!("{} {text}", "#".repeat(usize::from(*level)))]
            }
            Block::Code { lang, text } => code_block(lang.as_deref(), text),
            Block::Quote(blocks) => quote(self.blocks(blocks)),
            Block::Alert { kind, blocks } => self.alert(kind, blocks),
            Block::List(list) => self.list(list, alt),
            Block::Rule => vec!["***".to_owned()],
            Block::Table(rows) => {
                if self.profile.tables() {
                    self.table(rows)
                } else {
                    self.html_table(rows)
                }
            }
            Block::MsoItem { blocks, .. } => self.blocks(blocks),
        }
    }

    fn para(&self, runs: &[Run]) -> Vec<String> {
        let text = self.inline(runs, Mode::Para);
        if text.is_empty() {
            return Vec::new();
        }
        text.split('\n')
            .map(|l| escape_line_start(l, self.profile))
            .collect()
    }

    fn alert(&self, kind: &str, blocks: &[Block]) -> Vec<String> {
        let mut inner = self.blocks(blocks);
        if self.profile.alerts() {
            let label = if self.profile == Profile::Obsidian {
                kind.to_owned()
            } else {
                kind.to_ascii_uppercase()
            };
            inner.insert(0, format!("[!{label}]"));
        } else {
            let mut title = kind.to_owned();
            if let Some(f) = title.get_mut(..1) {
                f.make_ascii_uppercase();
            }
            if !inner.is_empty() {
                inner.insert(0, String::new());
            }
            inner.insert(0, format!("**{title}**"));
        }
        quote(inner)
    }

    fn list(&self, list: &List, alt: bool) -> Vec<String> {
        let bullet = if alt { '*' } else { '-' };
        let delim = if alt { ')' } else { '.' };
        let mut rendered: Vec<Vec<String>> = Vec::with_capacity(list.items.len());
        let mut loose = false;
        for (k, item) in list.items.iter().enumerate() {
            let marker = if list.ordered {
                let n = list.start.saturating_add(k as u64).min(999_999_999);
                format!("{n}{delim} ")
            } else {
                format!("{bullet} ")
            };
            let (mut lines, item_loose) = self.item(item);
            loose |= item_loose;
            if let Some(task) = item.task {
                let mark = match (self.profile.task_lists(), task) {
                    (true, true) => "[x]",
                    (true, false) => "[ ]",
                    (false, true) => "\\[x\\]",
                    (false, false) => "\\[ \\]",
                };
                match lines.first_mut() {
                    Some(first) => *first = format!("{mark} {first}"),
                    None => lines.push(mark.to_owned()),
                }
            }
            let indent = " ".repeat(marker.len());
            let mut out = Vec::with_capacity(lines.len().max(1));
            if lines.is_empty() {
                out.push(marker.trim_end().to_owned());
            }
            for (i, line) in lines.into_iter().enumerate() {
                if i == 0 {
                    out.push(format!("{marker}{line}"));
                } else if line.is_empty() {
                    out.push(line);
                } else {
                    out.push(format!("{indent}{line}"));
                }
            }
            rendered.push(out);
        }
        let mut out = Vec::new();
        for (i, item) in rendered.into_iter().enumerate() {
            if i > 0 && loose {
                out.push(String::new());
            }
            out.extend(item);
        }
        out
    }

    /// An item's lines, and whether the item makes its list loose.
    fn item(&self, item: &Item) -> (Vec<String>, bool) {
        let mut out: Vec<String> = Vec::new();
        let mut loose = false;
        let mut prev: Option<&Block> = None;
        let mut prev_list: Option<(bool, bool)> = None;
        for b in &item.blocks {
            let mut alt = false;
            if let Block::List(l) = b {
                alt = matches!(prev_list, Some((o, a)) if o == l.ordered && !a);
            }
            let lines = self.block(b, alt);
            if lines.is_empty() {
                continue;
            }
            if let Some(p) = prev {
                // A nested list may follow its paragraph directly (a tight
                // list); an ordered one only if it starts at 1, the only
                // number that interrupts a paragraph.
                let tight = matches!(p, Block::Para(_) | Block::Heading(..))
                    && matches!(b, Block::List(l) if !l.ordered || l.start == 1)
                    && prev_list.is_none();
                if !tight {
                    out.push(String::new());
                    loose = true;
                }
            }
            prev_list = match b {
                Block::List(l) => Some((l.ordered, alt)),
                _ => None,
            };
            prev = Some(b);
            out.extend(lines);
        }
        (out, loose)
    }

    fn table(&self, rows: &[Vec<Cell>]) -> Vec<String> {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return Vec::new();
        }
        let render_row = |row: &[Cell]| {
            let mut line = String::from("|");
            for i in 0..cols {
                let text = row
                    .get(i)
                    .map(|c| self.inline(&c.runs, Mode::Cell))
                    .unwrap_or_default();
                line.push(' ');
                line.push_str(&text);
                if !text.is_empty() {
                    line.push(' ');
                }
                line.push('|');
            }
            line
        };
        let mut out = vec![render_row(&rows[0])];
        let mut delim = String::from("|");
        for i in 0..cols {
            let align = rows[0]
                .get(i)
                .map(|c| c.align)
                .filter(|a| *a != Align::None)
                .or_else(|| rows.get(1).and_then(|r| r.get(i)).map(|c| c.align))
                .unwrap_or_default();
            delim.push_str(match align {
                Align::None => " --- |",
                Align::Left => " :--- |",
                Align::Center => " :---: |",
                Align::Right => " ---: |",
            });
        }
        out.push(delim);
        out.extend(rows[1..].iter().map(|r| render_row(r)));
        out
    }

    /// CommonMark has no tables: sanitized HTML, one row per line (an HTML
    /// block ends at a blank line).
    fn html_table(&self, rows: &[Vec<Cell>]) -> Vec<String> {
        let mut out = vec!["<table>".to_owned()];
        for row in rows {
            let mut line = String::from("<tr>");
            for cell in row {
                let tag = if cell.header { "th" } else { "td" };
                line.push('<');
                line.push_str(tag);
                match cell.align {
                    Align::None => {}
                    Align::Left => line.push_str(" align=\"left\""),
                    Align::Center => line.push_str(" align=\"center\""),
                    Align::Right => line.push_str(" align=\"right\""),
                }
                line.push('>');
                self.html_inline(&cell.runs, &mut line);
                line.push_str("</");
                line.push_str(tag);
                line.push('>');
            }
            line.push_str("</tr>");
            out.push(line);
        }
        out.push("</table>".to_owned());
        out
    }

    fn html_inline(&self, runs: &[Run], out: &mut String) {
        for run in runs {
            let ms = marks(&self.effective(run.style));
            for m in &ms {
                if let Mark::Link(id) = m {
                    out.push_str("<a href=\"");
                    if let Some(l) = self.links.get(*id as usize) {
                        html_escape(&l.dest, out);
                    }
                    out.push_str("\">");
                } else {
                    out.push('<');
                    out.push_str(m.html_tag());
                    out.push('>');
                }
            }
            if run.style.code {
                out.push_str("<code>");
            }
            match &run.item {
                Inline::Text(t) | Inline::Math(t) => html_escape(t, out),
                Inline::Break => out.push_str("<br>"),
                Inline::Image { src, alt, .. } => {
                    out.push_str("<img src=\"");
                    html_escape(src, out);
                    out.push_str("\" alt=\"");
                    html_escape(alt, out);
                    out.push_str("\">");
                }
            }
            if run.style.code {
                out.push_str("</code>");
            }
            for m in ms.iter().rev() {
                out.push_str("</");
                out.push_str(m.html_tag());
                out.push('>');
            }
        }
    }

    // ----- inlines ----------------------------------------------------

    /// Normalizes runs: white space moved out of delimiters, empty links
    /// dropped, neighbours with equal styles merged.
    fn atoms(&self, runs: &[Run]) -> Vec<Run> {
        // Split leading and trailing spaces off text runs.
        let mut atoms: Vec<(Run, bool)> = Vec::with_capacity(runs.len());
        for r in runs {
            let style = self.effective(r.style);
            match &r.item {
                Inline::Text(t) => {
                    let core = t.trim_matches(' ');
                    let ws = Style::default();
                    if core.is_empty() {
                        if !t.is_empty() {
                            atoms.push((text_run(" ", ws), true));
                        }
                        continue;
                    }
                    if t.starts_with(' ') {
                        atoms.push((text_run(" ", ws), true));
                    }
                    atoms.push((text_run(core, style), false));
                    if t.ends_with(' ') {
                        atoms.push((text_run(" ", ws), true));
                    }
                }
                item => atoms.push((
                    Run {
                        item: item.clone(),
                        style,
                    },
                    false,
                )),
            }
        }
        // A space keeps the formatting both neighbours share.
        let n = atoms.len();
        let mut styles: Vec<Option<Style>> = vec![None; n];
        let mut last: Option<Style> = None;
        for (i, (a, ws)) in atoms.iter().enumerate() {
            if *ws {
                styles[i] = last;
            } else {
                last = (a.item != Inline::Break).then_some(a.style);
            }
        }
        let mut next: Option<Style> = None;
        for i in (0..n).rev() {
            let (a, ws) = &atoms[i];
            if *ws {
                styles[i] = match (styles[i], next) {
                    (Some(l), Some(r)) => Some(Style {
                        strong: l.strong && r.strong,
                        em: l.em && r.em,
                        strike: l.strike && r.strike,
                        sub: l.sub && r.sub,
                        sup: l.sup && r.sup,
                        mark: l.mark && r.mark,
                        code: false,
                        link: if l.link == r.link { l.link } else { None },
                    }),
                    // At the start, the end or next to a break: dropped.
                    _ => None,
                };
            } else {
                next = (a.item != Inline::Break).then_some(a.style);
            }
        }
        let mut out: Vec<Run> = Vec::with_capacity(n);
        for (i, (a, ws)) in atoms.into_iter().enumerate() {
            let run = if ws {
                match styles[i] {
                    Some(style) => text_run(" ", style),
                    None => continue,
                }
            } else {
                a
            };
            // Merge with the previous run (spaces never repeat).
            if let (
                Some(Run {
                    item: Inline::Text(prev),
                    style: ps,
                }),
                Inline::Text(t),
            ) = (out.last_mut(), &run.item)
                && *ps == run.style
            {
                if !(prev.ends_with(' ') && t == " ") {
                    prev.push_str(t);
                }
                continue;
            }
            out.push(run);
        }
        // Breaks at the edges carry nothing.
        while matches!(out.first(), Some(r) if r.item == Inline::Break) {
            out.remove(0);
        }
        while matches!(out.last(), Some(r) if r.item == Inline::Break) {
            out.pop();
        }
        // Links without visible content are dropped.
        let mut visible = vec![false; self.links.len()];
        for r in &out {
            if let Some(id) = r.style.link
                && r.is_visible()
                && let Some(v) = visible.get_mut(id as usize)
            {
                *v = true;
            }
        }
        for r in &mut out {
            if let Some(id) = r.style.link
                && !visible.get(id as usize).copied().unwrap_or(false)
            {
                r.style.link = None;
            }
        }
        out
    }

    /// One atom's text.
    fn atom(&self, run: &Run, mode: Mode, out: &mut String) {
        match &run.item {
            Inline::Text(t) if run.style.code => code_span(t, mode, out),
            Inline::Text(t) => escape_text(t, self.profile, mode, out),
            Inline::Break => match mode {
                Mode::Para => out.push_str("\\\n"),
                Mode::Heading => out.push(' '),
                Mode::Cell => out.push_str("<br>"),
            },
            Inline::Image { src, alt, title: t } => {
                out.push_str("![");
                escape_text(alt, self.profile, mode, out);
                out.push_str("](");
                out.push_str(&destination(src, mode));
                if let Some(t) = t {
                    out.push(' ');
                    out.push_str(&title(t, mode));
                }
                out.push(')');
            }
            Inline::Math(tex) => {
                let tex = tex.trim();
                if self.profile.dollar_math() && !tex.contains('$') && !tex.is_empty() {
                    out.push('$');
                    out.push_str(&if mode == Mode::Cell {
                        tex.replace('|', "\\|")
                    } else {
                        tex.to_owned()
                    });
                    out.push('$');
                } else {
                    code_span(tex, mode, out);
                }
            }
        }
    }

    /// Inline content as Markdown.
    pub(crate) fn inline(&self, runs: &[Run], mode: Mode) -> String {
        let atoms = self.atoms(runs);
        if atoms.is_empty() {
            return String::new();
        }
        let (toks, mut pairs) = self.tokens(&atoms, mode);
        self.fix_flanking(&toks, &mut pairs);
        self.render(&toks, &pairs).0
    }

    /// Delimiter tokens from the flat runs (a stack; marks that continue
    /// longest are opened first).
    fn tokens(&self, atoms: &[Run], mode: Mode) -> (Vec<Tok>, Vec<Pair>) {
        let n = atoms.len();
        // How many atoms from i on carry each mark.
        let fixed = [
            Mark::Strong,
            Mark::Em,
            Mark::Strike,
            Mark::Highlight,
            Mark::Sub,
            Mark::Sup,
        ];
        let mut span = vec![[0usize; 7]; n + 1];
        for i in (0..n).rev() {
            let s = &atoms[i].style;
            for (k, m) in fixed.iter().enumerate() {
                span[i][k] = if m.has(s) { span[i + 1][k] + 1 } else { 0 };
            }
            span[i][6] = match s.link {
                Some(id) if i + 1 < n && atoms[i + 1].style.link == Some(id) => span[i + 1][6] + 1,
                Some(_) => 1,
                None => 0,
            };
        }
        let span_of = |i: usize, m: Mark| match m {
            Mark::Link(_) => span[i][6],
            m => fixed.iter().position(|f| *f == m).map_or(0, |k| span[i][k]),
        };

        let mut toks = Vec::with_capacity(n * 2);
        let mut pairs: Vec<Pair> = Vec::new();
        let mut stack: Vec<(Mark, usize)> = Vec::new();
        for (i, atom) in atoms.iter().enumerate() {
            let want = marks(&atom.style);
            // Close from the first open mark that is no longer wanted.
            if let Some(cut) = stack.iter().position(|(m, _)| !want.contains(m)) {
                while stack.len() > cut {
                    if let Some((m, p)) = stack.pop() {
                        toks.push(Tok::Close(p));
                        if let Mark::Link(id) = m {
                            toks.push(Tok::Lit(self.link_tail(id, mode)));
                        }
                    }
                }
            }
            let mut open: Vec<Mark> = want
                .into_iter()
                .filter(|m| !stack.iter().any(|(s, _)| s == m))
                .collect();
            open.sort_by_key(|m| std::cmp::Reverse(span_of(i, *m)));
            // A space where marks are (re-)opened goes before them (a
            // delimiter followed by a space cannot open).
            let mut atom = std::borrow::Cow::Borrowed(atom);
            if !open.is_empty()
                && !atom.style.code
                && let Inline::Text(t) = &atom.item
                && let Some(rest) = t.strip_prefix(' ')
            {
                toks.push(Tok::Lit(" ".to_owned()));
                atom = std::borrow::Cow::Owned(text_run(rest, atom.style));
            }
            for m in open {
                let p = pairs.len();
                pairs.push(Pair {
                    mark: m,
                    html: false,
                });
                toks.push(Tok::Open(p));
                stack.push((m, p));
            }
            let mut s = String::new();
            self.atom(&atom, mode, &mut s);
            toks.push(Tok::Lit(s));
        }
        while let Some((m, p)) = stack.pop() {
            toks.push(Tok::Close(p));
            if let Mark::Link(id) = m {
                toks.push(Tok::Lit(self.link_tail(id, mode)));
            }
        }
        // Sub- and superscript delimiters cannot hold spaces; GFM has no
        // sub/sup syntax at all.
        for p in &mut pairs {
            let md = match p.mark {
                Mark::Strike => self.profile.strikethrough(),
                Mark::Sub | Mark::Sup => self.profile.sub_sup(),
                Mark::Highlight => self.profile.highlight(),
                _ => true,
            };
            p.html = !md;
        }
        (toks, pairs)
    }

    /// `](dest "title")` closing a link.
    fn link_tail(&self, id: u32, mode: Mode) -> String {
        let mut s = String::from("](");
        if let Some(l) = self.links.get(id as usize) {
            s.push_str(&destination(&l.dest, mode));
            if let Some(t) = &l.title {
                s.push(' ');
                s.push_str(&title(t, mode));
            }
        }
        s.push(')');
        s
    }

    fn delim_text(&self, pair: &Pair, open: bool) -> String {
        match (pair.mark, pair.html) {
            (Mark::Link(_), _) => if open { "[" } else { "" }.to_owned(),
            (m, true) => {
                if open {
                    format!("<{}>", m.html_tag())
                } else {
                    format!("</{}>", m.html_tag())
                }
            }
            (m, false) => m.delim().unwrap_or("").to_owned(),
        }
    }

    /// The text, and for each token its byte range in it.
    fn render(&self, toks: &[Tok], pairs: &[Pair]) -> (String, Vec<std::ops::Range<usize>>) {
        let mut out = String::new();
        let mut ranges = Vec::with_capacity(toks.len());
        for t in toks {
            let start = out.len();
            match t {
                Tok::Lit(s) => out.push_str(s),
                Tok::Open(p) => out.push_str(&self.delim_text(&pairs[*p], true)),
                Tok::Close(p) => out.push_str(&self.delim_text(&pairs[*p], false)),
            }
            ranges.push(start..out.len());
        }
        (out, ranges)
    }

    /// Switches delimiters that would not parse to HTML tags.
    fn fix_flanking(&self, toks: &[Tok], pairs: &mut [Pair]) {
        for _ in 0..MAX_FIXUP_ROUNDS {
            let (text, ranges) = self.render(toks, pairs);
            let mut bad: Vec<usize> = Vec::new();
            let mut i = 0;
            while i < toks.len() {
                let Some(ch) = self.md_delim_char(&toks[i], pairs) else {
                    i += 1;
                    continue;
                };
                // A group: consecutive delimiter tokens of the same character
                // (one delimiter run for the parser).
                let mut j = i;
                while j + 1 < toks.len() && self.md_delim_char(&toks[j + 1], pairs) == Some(ch) {
                    j += 1;
                }
                let prev = text[..ranges[i].start].chars().next_back();
                let next = text[ranges[j].end..].chars().next();
                let opens = (i..=j).all(|k| matches!(toks[k], Tok::Open(_)));
                let closes = (i..=j).all(|k| matches!(toks[k], Tok::Close(_)));
                let left = left_flanking(prev, next);
                let right = right_flanking(prev, next);
                let kinds_ok = {
                    let first = pair_of(&toks[i]).map(|p| pairs[p].mark);
                    // `*` and `**` share a run safely; `~~` and `~` do not.
                    (i..=j).all(|k| {
                        let m = pair_of(&toks[k]).map(|p| pairs[p].mark);
                        ch == '*' || m == first
                    }) && (ch == '*' || j == i)
                };
                // Sub- and superscript (`H~2~O`, `x^2^`) work inside words;
                // only the no-space rule below applies to them.
                let subsup = pair_of(&toks[i])
                    .is_some_and(|p| matches!(pairs[p].mark, Mark::Sub | Mark::Sup));
                let flanks = (opens && left && !right) || (closes && right && !left);
                let ok = kinds_ok && (flanks || (subsup && (opens || closes)));
                if !ok {
                    bad.extend((i..=j).filter_map(|k| pair_of(&toks[k])));
                }
                i = j + 1;
            }
            // Sub/superscript content must be free of white space.
            for (k, t) in toks.iter().enumerate() {
                if let Tok::Open(p) = t
                    && matches!(pairs[*p].mark, Mark::Sub | Mark::Sup)
                    && !pairs[*p].html
                {
                    let close = toks[k..]
                        .iter()
                        .position(|t| matches!(t, Tok::Close(q) if q == p))
                        .map_or(toks.len() - 1, |c| k + c);
                    let inner = &text[ranges[k].end..ranges[close].start];
                    if inner.is_empty() || inner.chars().any(char::is_whitespace) {
                        bad.push(*p);
                    }
                }
            }
            if bad.is_empty() {
                return;
            }
            for p in bad {
                pairs[p].html = true;
            }
        }
        for p in pairs.iter_mut() {
            if !matches!(p.mark, Mark::Link(_)) {
                p.html = true;
            }
        }
    }

    fn md_delim_char(&self, t: &Tok, pairs: &[Pair]) -> Option<char> {
        let p = &pairs[pair_of(t)?];
        if p.html {
            return None;
        }
        p.mark.delim().and_then(|d| d.chars().next())
    }
}

fn pair_of(t: &Tok) -> Option<usize> {
    match t {
        Tok::Open(p) | Tok::Close(p) => Some(*p),
        Tok::Lit(_) => None,
    }
}

fn text_run(s: &str, style: Style) -> Run {
    Run {
        item: Inline::Text(s.to_owned()),
        style,
    }
}

fn quote(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|l| {
            if l.is_empty() {
                ">".to_owned()
            } else {
                format!("> {l}")
            }
        })
        .collect()
}

fn code_block(lang: Option<&str>, text: &str) -> Vec<String> {
    let mut longest = 0;
    for line in text.lines() {
        let run = line
            .trim_start_matches(' ')
            .chars()
            .take_while(|&c| c == '`')
            .count();
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    let mut out = vec![format!("{fence}{}", lang.unwrap_or(""))];
    out.extend(text.lines().map(str::to_owned));
    out.push(fence);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(t: &str, f: impl FnOnce(&mut Style)) -> Run {
        let mut s = Style::default();
        f(&mut s);
        text_run(t, s)
    }

    fn inline(runs: &[Run], p: Profile) -> String {
        let links = [Link {
            dest: "https://x.com/".into(),
            title: None,
        }];
        Emitter::new(p, &links).inline(runs, Mode::Para)
    }

    #[test]
    fn nesting_and_spaces() {
        let cm = Profile::CommonMark;
        let r = [
            run("a ", |_| {}),
            run("bold ", |s| s.strong = true),
            run("both", |s| {
                s.strong = true;
                s.em = true;
            }),
            run(" c", |_| {}),
        ];
        assert_eq!(inline(&r, cm), "a **bold *both*** c");
        let r = [
            run("x ", |s| s.link = Some(0)),
            run("y", |s| {
                s.link = Some(0);
                s.strong = true;
            }),
            run(" z", |s| s.strong = true),
        ];
        assert_eq!(inline(&r, cm), "[x **y**](https://x.com/) **z**");
    }

    #[test]
    fn flanking_falls_back_to_html() {
        let cm = Profile::CommonMark;
        let r = [
            run("foo", |_| {}),
            run("bar", |s| s.strong = true),
            run("baz", |_| {}),
        ];
        assert_eq!(inline(&r, cm), "foo<strong>bar</strong>baz");
        let r = [run("\"q\"", |s| s.em = true), run("x", |_| {})];
        assert_eq!(inline(&r, cm), "<em>\"q\"</em>x");
        let r = [
            run("a ", |_| {}),
            run("\"q\"", |s| s.em = true),
            run(" x", |_| {}),
        ];
        assert_eq!(inline(&r, cm), "a *\"q\"* x");
    }

    #[test]
    fn profile_delimiters() {
        let r = [run("gone", |s| s.strike = true)];
        assert_eq!(inline(&r, Profile::CommonMark), "<del>gone</del>");
        assert_eq!(inline(&r, Profile::GfmFile), "~~gone~~");
        let r = [
            run("H", |_| {}),
            run("2", |s| s.sub = true),
            run("O", |_| {}),
        ];
        assert_eq!(inline(&r, Profile::Extended), "H~2~O");
        assert_eq!(inline(&r, Profile::GfmFile), "H<sub>2</sub>O");
        let r = [run("a b", |s| s.sup = true)];
        assert_eq!(inline(&r, Profile::Extended), "<sup>a b</sup>");
        let r = [run("hi", |s| s.mark = true)];
        assert_eq!(inline(&r, Profile::Obsidian), "==hi==");
        assert_eq!(inline(&r, Profile::GfmFile), "hi");
    }
}

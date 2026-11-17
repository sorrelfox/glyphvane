// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Compiled queries and their evaluation against one note.

use std::borrow::Cow;
use std::cell::OnceCell;
use std::ops::Range;
use std::sync::Arc;

use aho_corasick::AhoCorasick;
use gv_vault::content::{NoteRecord, PropertyValue};
use memchr::memmem;
use regex::{Regex, RegexBuilder};

use crate::query::{Comparison, Expr, Operator, PropertyTest, TextPattern};

/// Why a query cannot run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    /// A `/regex/` does not compile.
    Regex {
        /// The pattern as written.
        pattern: String,
        /// The regex engine's message.
        message: String,
    },
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Regex { pattern, message } => {
                write!(f, "invalid regular expression /{pattern}/: {message}")
            }
        }
    }
}

impl std::error::Error for QueryError {}

/// Compiled regexes are bounded so a pathological pattern cannot use
/// unbounded memory.
const REGEX_SIZE_LIMIT: usize = 8 << 20;

/// A compiled text leaf.
#[derive(Debug)]
pub(crate) struct Needle {
    kind: NeedleKind,
    /// The literal as written (tags compare whole names with it).
    literal: Option<String>,
}

#[derive(Debug)]
enum NeedleKind {
    Exact(Box<memmem::Finder<'static>>),
    AsciiFold(AhoCorasick),
    Regex(Regex),
}

impl Needle {
    fn literal(text: &str, match_case: bool) -> Result<Self, QueryError> {
        let kind = if match_case {
            NeedleKind::Exact(Box::new(memmem::Finder::new(text.as_bytes()).into_owned()))
        } else if text.is_ascii() {
            let ac = AhoCorasick::builder()
                .ascii_case_insensitive(true)
                .build([text])
                .map_err(|e| QueryError::Regex {
                    pattern: text.to_owned(),
                    message: e.to_string(),
                })?;
            NeedleKind::AsciiFold(ac)
        } else {
            NeedleKind::Regex(build_regex(&regex::escape(text), text, false)?)
        };
        Ok(Self {
            kind,
            literal: Some(text.to_owned()),
        })
    }

    fn regex(pattern: &str, match_case: bool) -> Result<Self, QueryError> {
        Ok(Self {
            kind: NeedleKind::Regex(build_regex(pattern, pattern, match_case)?),
            literal: None,
        })
    }

    /// Whether `hay` contains a match.
    pub(crate) fn is_match(&self, hay: &str) -> bool {
        match &self.kind {
            NeedleKind::Exact(f) => f.find(hay.as_bytes()).is_some(),
            NeedleKind::AsciiFold(ac) => ac.is_match(hay),
            NeedleKind::Regex(re) => re.is_match(hay),
        }
    }

    /// Calls `each` with every non-overlapping match in `hay`; returns
    /// whether there was one (an empty regex match counts).
    fn for_each(&self, hay: &str, mut each: impl FnMut(Range<usize>)) -> bool {
        let mut any = false;
        match &self.kind {
            NeedleKind::Exact(f) => {
                let len = f.needle().len();
                for start in f.find_iter(hay.as_bytes()) {
                    any = true;
                    each(start..start + len);
                }
            }
            NeedleKind::AsciiFold(ac) => {
                for m in ac.find_iter(hay) {
                    any = true;
                    each(m.range());
                }
            }
            NeedleKind::Regex(re) => {
                for m in re.find_iter(hay) {
                    any = true;
                    if !m.is_empty() {
                        each(m.range());
                    }
                }
            }
        }
        any
    }

    /// Whether the tag `name` (without `#`) matches: a literal equals the
    /// tag or one of its parents ignoring case (`tag:#a` finds `#a/b`); a
    /// regex matches anywhere in the name.
    fn matches_tag(&self, name: &str) -> bool {
        match &self.literal {
            Some(lit) => {
                let lit = lit.strip_prefix('#').unwrap_or(lit);
                if lit.is_empty() {
                    return false;
                }
                let name = name.to_lowercase();
                let lit = lit.to_lowercase();
                name == lit
                    || name
                        .strip_prefix(lit.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            }
            None => self.is_match(name),
        }
    }
}

fn build_regex(pattern: &str, shown: &str, match_case: bool) -> Result<Regex, QueryError> {
    RegexBuilder::new(pattern)
        .case_insensitive(!match_case)
        .multi_line(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| QueryError::Regex {
            pattern: shown.to_owned(),
            message: e.to_string(),
        })
}

/// An operator after compiling (case operators are folded into leaves).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    File,
    Path,
    Content,
    Tag,
    Line,
    Block,
    Section,
    Task(TaskFilter),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskFilter {
    Any,
    Todo,
    Done,
}

#[derive(Debug)]
pub(crate) enum Node {
    All,
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Text(Arc<Needle>),
    Op(Op, Box<Node>),
    Property { name: String, test: PropTest },
}

#[derive(Debug)]
pub(crate) enum PropTest {
    Exists,
    Null,
    Compare(Comparison, String),
    Value(Box<Node>),
}

impl Node {
    /// Evaluation cost class: cheap metadata tests run first in `And`.
    fn cost(&self) -> u8 {
        match self {
            Node::All | Node::Property { .. } => 0,
            Node::Op(Op::File | Op::Path | Op::Tag, _) => 1,
            Node::Not(inner) => inner.cost(),
            Node::Text(_) | Node::Op(Op::Content, _) => 2,
            Node::And(v) | Node::Or(v) => v.iter().map(Node::cost).max().unwrap_or(0),
            Node::Op(_, _) => 3,
        }
    }
}

/// Compiles `expr`; `positive` collects the leaves outside negations
/// (the ranking checks them against the title).
pub(crate) fn compile(
    expr: &Expr,
    match_case: bool,
    positive: &mut Vec<Arc<Needle>>,
    negated: bool,
) -> Result<Node, QueryError> {
    Ok(match expr {
        Expr::All => Node::All,
        Expr::And(items) => {
            let mut nodes = items
                .iter()
                .map(|e| compile(e, match_case, positive, negated))
                .collect::<Result<Vec<_>, _>>()?;
            nodes.sort_by_key(Node::cost);
            Node::And(nodes)
        }
        Expr::Or(items) => Node::Or(
            items
                .iter()
                .map(|e| compile(e, match_case, positive, negated))
                .collect::<Result<_, _>>()?,
        ),
        Expr::Not(inner) => Node::Not(Box::new(compile(inner, match_case, positive, !negated)?)),
        Expr::Text(pattern) => {
            let needle = Arc::new(match pattern {
                TextPattern::Literal(text) => Needle::literal(text, match_case)?,
                TextPattern::Regex(re) => Needle::regex(re, match_case)?,
            });
            if !negated {
                positive.push(needle.clone());
            }
            Node::Text(needle)
        }
        Expr::Op(op, inner) => match op_of(*op) {
            None => compile(inner, *op == Operator::MatchCase, positive, negated)?,
            Some(op) => {
                // Tags are not ranked against the title.
                let mut sink = Vec::new();
                let inner_positive = if op == Op::Tag {
                    &mut sink
                } else {
                    &mut *positive
                };
                Node::Op(
                    op,
                    Box::new(compile(inner, match_case, inner_positive, negated)?),
                )
            }
        },
        Expr::Property { name, test } => Node::Property {
            name: name.to_lowercase(),
            test: match test {
                PropertyTest::Exists => PropTest::Exists,
                PropertyTest::Null => PropTest::Null,
                PropertyTest::Compare(c, v) => PropTest::Compare(*c, v.clone()),
                PropertyTest::Value(e) => {
                    let mut sink = Vec::new();
                    PropTest::Value(Box::new(compile(e, match_case, &mut sink, negated)?))
                }
            },
        },
    })
}

/// The compiled operator; `None` for the case operators.
fn op_of(op: Operator) -> Option<Op> {
    Some(match op {
        Operator::File => Op::File,
        Operator::Path => Op::Path,
        Operator::Content => Op::Content,
        Operator::Tag => Op::Tag,
        Operator::Line => Op::Line,
        Operator::Block => Op::Block,
        Operator::Section => Op::Section,
        Operator::Task => Op::Task(TaskFilter::Any),
        Operator::TaskTodo => Op::Task(TaskFilter::Todo),
        Operator::TaskDone => Op::Task(TaskFilter::Done),
        Operator::MatchCase | Operator::IgnoreCase => return None,
    })
}

/// Collected highlight ranges, capped; `count` keeps counting past the
/// cap.
#[derive(Debug, Default)]
pub(crate) struct Hits {
    pub content: Vec<Range<usize>>,
    pub path: Vec<Range<usize>>,
    pub count: usize,
    pub cap: usize,
}

#[derive(Clone, Copy)]
struct Mark {
    content: usize,
    path: usize,
    count: usize,
}

impl Hits {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            ..Self::default()
        }
    }

    fn mark(&self) -> Mark {
        Mark {
            content: self.content.len(),
            path: self.path.len(),
            count: self.count,
        }
    }

    fn reset(&mut self, m: Mark) {
        self.content.truncate(m.content);
        self.path.truncate(m.path);
        self.count = m.count;
    }

    fn push_content(&mut self, r: Range<usize>) {
        self.count += 1;
        if self.content.len() < self.cap {
            self.content.push(r);
        }
    }

    fn push_path(&mut self, r: Range<usize>) {
        if self.path.len() < self.cap {
            self.path.push(r);
        }
    }
}

/// Where a text leaf looks.
#[derive(Clone, Copy)]
enum Target<'v> {
    /// The note text and the file name.
    Default,
    /// A byte range of the note text.
    Content(usize, usize),
    FileName,
    Path,
    /// Tags; inside a scope only the tags in that range.
    Tags(Option<(usize, usize)>),
    /// The values of one property.
    Values(&'v [&'v str]),
}

/// Loads a note's text on first use.
pub(crate) type TextLoader<'a> = dyn Fn() -> Option<Cow<'a, str>> + 'a;

/// One note being evaluated.
pub(crate) struct Doc<'a> {
    pub record: &'a NoteRecord,
    load: &'a TextLoader<'a>,
    text: OnceCell<Cow<'a, str>>,
    lines: OnceCell<Vec<usize>>,
    blocks: OnceCell<Vec<(usize, usize)>>,
    sections: OnceCell<Vec<(usize, usize)>>,
}

impl<'a> Doc<'a> {
    pub(crate) fn new(record: &'a NoteRecord, load: &'a TextLoader<'a>) -> Self {
        Self {
            record,
            load,
            text: OnceCell::new(),
            lines: OnceCell::new(),
            blocks: OnceCell::new(),
            sections: OnceCell::new(),
        }
    }

    /// The note text (empty for path-only records).
    pub(crate) fn text(&self) -> &str {
        self.text
            .get_or_init(|| (self.load)().unwrap_or(Cow::Borrowed("")))
    }

    /// Whether the text was loaded.
    pub(crate) fn text_loaded(&self) -> bool {
        self.text.get().is_some()
    }

    pub(crate) fn path(&self) -> &str {
        self.record.path.as_str()
    }

    fn file_name_start(&self) -> usize {
        let p = self.path();
        p.rfind('/').map_or(0, |i| i + 1)
    }

    /// Byte offsets of line starts.
    pub(crate) fn line_starts(&self) -> &[usize] {
        self.lines.get_or_init(|| line_starts(self.text()))
    }

    /// The range of 0-based line `line`, without its line break.
    pub(crate) fn line_range(&self, line: usize) -> Option<(usize, usize)> {
        let starts = self.line_starts();
        let text = self.text();
        let start = *starts.get(line)?;
        let mut end = starts.get(line + 1).map_or(text.len(), |s| s - 1);
        if end > start && text.as_bytes()[end - 1] == b'\r' {
            end -= 1;
        }
        Some((start, end.max(start)))
    }

    /// The 0-based line holding `offset`.
    pub(crate) fn line_of(&self, offset: usize) -> usize {
        self.line_starts()
            .partition_point(|&s| s <= offset)
            .saturating_sub(1)
    }

    fn lines(&self) -> Vec<(usize, usize)> {
        (0..self.line_starts().len())
            .filter_map(|i| self.line_range(i))
            .collect()
    }

    fn blocks(&self) -> &[(usize, usize)] {
        self.blocks.get_or_init(|| {
            let text = self.text();
            let mut out = Vec::new();
            let mut cur: Option<(usize, usize)> = None;
            let mut fence: Option<(u8, usize)> = None;
            for (s, e) in self.lines() {
                let line = &text[s..e];
                let trimmed = line.trim_start();
                let fence_mark = fence_marker(trimmed);
                let blank = trimmed.is_empty();
                match fence {
                    Some((ch, n)) => {
                        cur = Some((cur.map_or(s, |c| c.0), e));
                        if fence_mark.is_some_and(|(c, m)| c == ch && m >= n)
                            && trimmed.trim_end().bytes().all(|b| b == ch)
                        {
                            fence = None;
                        }
                    }
                    None if blank => {
                        if let Some(c) = cur.take() {
                            out.push(c);
                        }
                    }
                    None => {
                        cur = Some((cur.map_or(s, |c| c.0), e));
                        fence = fence_mark;
                    }
                }
            }
            if let Some(c) = cur {
                out.push(c);
            }
            out
        })
    }

    fn sections(&self) -> &[(usize, usize)] {
        self.sections.get_or_init(|| {
            let len = self.text().len();
            let mut starts: Vec<usize> = std::iter::once(0)
                .chain(
                    self.record
                        .data
                        .headings
                        .iter()
                        .map(|h| (h.span.start as usize).min(len)),
                )
                .collect();
            starts.sort_unstable();
            starts.dedup();
            let mut out: Vec<(usize, usize)> = starts.windows(2).map(|w| (w[0], w[1])).collect();
            if let Some(&last) = starts.last() {
                out.push((last, len));
            }
            out.retain(|(s, e)| e > s);
            out
        })
    }

    fn task_lines(&self, filter: TaskFilter) -> Vec<(usize, usize)> {
        self.record
            .data
            .tasks
            .iter()
            .filter(|t| match filter {
                TaskFilter::Any => true,
                TaskFilter::Todo => t.status == ' ',
                TaskFilter::Done => t.status != ' ',
            })
            .filter_map(|t| self.line_range(t.line as usize))
            .collect()
    }

    /// Evaluates `node`; with `hits`, collects the highlight ranges of the
    /// parts that made it match.
    pub(crate) fn eval(&self, node: &Node, hits: Option<&mut Hits>) -> bool {
        self.eval_in(node, Target::Default, hits)
    }

    fn eval_in(&self, node: &Node, target: Target<'_>, mut hits: Option<&mut Hits>) -> bool {
        match node {
            Node::All => true,
            Node::And(items) => {
                let mark = hits.as_deref().map(Hits::mark);
                for item in items {
                    if !self.eval_in(item, target, hits.as_deref_mut()) {
                        if let (Some(h), Some(m)) = (hits, mark) {
                            h.reset(m);
                        }
                        return false;
                    }
                }
                true
            }
            Node::Or(items) => {
                let mut any = false;
                for item in items {
                    if self.eval_in(item, target, hits.as_deref_mut()) {
                        any = true;
                        if hits.is_none() {
                            break;
                        }
                    }
                }
                any
            }
            Node::Not(inner) => !self.eval_in(inner, target, None),
            Node::Text(needle) => self.eval_text(needle, target, hits),
            Node::Op(op, inner) => self.eval_op(*op, inner, target, hits),
            Node::Property { name, test } => self.eval_property(name, test),
        }
    }

    fn eval_text(&self, needle: &Needle, target: Target<'_>, mut hits: Option<&mut Hits>) -> bool {
        match target {
            Target::Default => {
                let len = self.text().len();
                match hits {
                    Some(h) => {
                        let c = self.find_content(needle, 0, len, Some(h));
                        let f = self.find_path(needle, self.file_name_start(), Some(h));
                        c || f
                    }
                    None => {
                        self.find_content(needle, 0, len, None)
                            || self.find_path(needle, self.file_name_start(), None)
                    }
                }
            }
            Target::Content(s, e) => self.find_content(needle, s, e, hits),
            Target::FileName => self.find_path(needle, self.file_name_start(), hits),
            Target::Path => self.find_path(needle, 0, hits),
            Target::Tags(region) => {
                let mut any = false;
                for tag in &self.record.data.tags {
                    let inside = match (region, tag.span) {
                        (None, _) => true,
                        (Some((s, e)), Some(sp)) => sp.start as usize >= s && sp.end as usize <= e,
                        (Some(_), None) => false,
                    };
                    if inside && needle.matches_tag(&tag.name) {
                        any = true;
                        match (hits.as_deref_mut(), tag.span) {
                            (Some(h), Some(sp)) => h.push_content(sp.range()),
                            (Some(_), None) => {}
                            (None, _) => return true,
                        }
                    }
                }
                any
            }
            Target::Values(values) => values.iter().any(|v| needle.is_match(v)),
        }
    }

    fn find_content(&self, needle: &Needle, s: usize, e: usize, hits: Option<&mut Hits>) -> bool {
        let text = self.text();
        let Some(hay) = text.get(s..e) else {
            return false;
        };
        match hits {
            Some(h) => needle.for_each(hay, |r| h.push_content(r.start + s..r.end + s)),
            None => needle.is_match(hay),
        }
    }

    fn find_path(&self, needle: &Needle, from: usize, hits: Option<&mut Hits>) -> bool {
        let hay = &self.path()[from..];
        match hits {
            Some(h) => needle.for_each(hay, |r| h.push_path(r.start + from..r.end + from)),
            None => needle.is_match(hay),
        }
    }

    fn eval_op(&self, op: Op, inner: &Node, target: Target<'_>, hits: Option<&mut Hits>) -> bool {
        let region = match target {
            Target::Content(s, e) => Some((s, e)),
            _ => None,
        };
        match op {
            Op::File => self.eval_in(inner, Target::FileName, hits),
            Op::Path => self.eval_in(inner, Target::Path, hits),
            Op::Content => {
                let t = match target {
                    Target::Default => Target::Content(0, self.text().len()),
                    other => other,
                };
                self.eval_in(inner, t, hits)
            }
            Op::Tag => self.eval_in(inner, Target::Tags(region), hits),
            Op::Line => self.eval_regions(&self.lines(), region, inner, hits),
            Op::Block => self.eval_regions(self.blocks(), region, inner, hits),
            Op::Section => self.eval_regions(self.sections(), region, inner, hits),
            Op::Task(filter) => self.eval_regions(&self.task_lines(filter), region, inner, hits),
        }
    }

    fn eval_regions(
        &self,
        regions: &[(usize, usize)],
        within: Option<(usize, usize)>,
        inner: &Node,
        mut hits: Option<&mut Hits>,
    ) -> bool {
        let mut any = false;
        for &(s, e) in regions {
            let (s, e) = match within {
                Some((ws, we)) => {
                    if e <= ws || s >= we {
                        continue;
                    }
                    (s.max(ws), e.min(we))
                }
                None => (s, e),
            };
            match hits.as_deref_mut() {
                Some(h) => {
                    let m = h.mark();
                    if self.eval_in(inner, Target::Content(s, e), Some(h)) {
                        any = true;
                    } else {
                        h.reset(m);
                    }
                }
                None => {
                    if self.eval_in(inner, Target::Content(s, e), None) {
                        return true;
                    }
                }
            }
        }
        any
    }

    fn eval_property(&self, name: &str, test: &PropTest) -> bool {
        let found = self
            .record
            .data
            .properties
            .iter()
            .filter(|p| p.key.to_lowercase() == name);
        let mut any = false;
        for prop in found {
            let values = values_of(&prop.value);
            let ok = match test {
                PropTest::Exists => true,
                PropTest::Null => values.is_empty(),
                PropTest::Compare(cmp, rhs) => values.iter().any(|v| compare(v, *cmp, rhs)),
                PropTest::Value(node) => {
                    // A negated value (`[status:-done]`) also holds for an
                    // empty value.
                    self.eval_in(node, Target::Values(&values), None)
                }
            };
            if ok {
                any = true;
                break;
            }
        }
        any
    }
}

fn values_of(value: &PropertyValue) -> Vec<&str> {
    match value {
        PropertyValue::Null => Vec::new(),
        PropertyValue::Bool(true) => vec!["true"],
        PropertyValue::Bool(false) => vec!["false"],
        PropertyValue::Number(s) | PropertyValue::Text(s) | PropertyValue::Other(s) => {
            if s.trim().is_empty() {
                Vec::new()
            } else {
                vec![&**s]
            }
        }
        PropertyValue::List(items) => items.iter().map(|s| &**s).collect(),
    }
}

fn compare(value: &str, cmp: Comparison, rhs: &str) -> bool {
    use std::cmp::Ordering;
    let ord = match (value.trim().parse::<f64>(), rhs.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) => a.partial_cmp(&b),
        _ => Some(value.trim().to_lowercase().cmp(&rhs.trim().to_lowercase())),
    };
    let Some(ord) = ord else {
        return false;
    };
    match cmp {
        Comparison::Lt => ord == Ordering::Less,
        Comparison::Le => ord != Ordering::Greater,
        Comparison::Gt => ord == Ordering::Greater,
        Comparison::Ge => ord != Ordering::Less,
        Comparison::Eq => ord == Ordering::Equal,
    }
}

/// A fence opener: its character and length (three or more backticks
/// or tildes).
fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let first = *line.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let n = line.bytes().take_while(|&b| b == first).count();
    (n >= 3).then_some((first, n))
}

/// Byte offsets where lines start.
pub(crate) fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(memchr::memchr_iter(b'\n', text.as_bytes()).map(|i| i + 1))
        .collect()
}

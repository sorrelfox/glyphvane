// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The query language: syntax tree and parser.
//!
//! ```text
//! query      := or_expr
//! or_expr    := and_expr ( "OR" and_expr )*
//! and_expr   := unary+                       ; juxtaposition = AND
//! unary      := "-" unary | primary
//! primary    := "(" or_expr ")" | operator | property | regex | phrase | word
//! operator   := ("file:" | "path:" | "content:" | "tag:" | "line:" | "block:"
//!                | "section:" | "task:" | "task-todo:" | "task-done:"
//!                | "match-case:" | "ignore-case:") primary
//! property   := "[" name [":" (value | "null" | comparison)] "]"
//! regex      := "/" pattern "/"
//! phrase     := '"' text '"'
//! ```
//!
//! The parser never fails: the query is typed live, so an unclosed
//! parenthesis, quote or bracket ends at the end of the input, a stray `)`
//! is skipped, and an operator without an operand (`tag:` while typing) is
//! dropped. Only compiling can fail, on an invalid regular expression
//! ([`crate::Query::new`]).

/// A parsed query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Matches everything (the empty phrase `""`, as in `task:""`).
    All,
    /// Every part matches.
    And(Vec<Expr>),
    /// Any part matches.
    Or(Vec<Expr>),
    /// The part does not match (`-term`).
    Not(Box<Expr>),
    /// A word, phrase or regular expression.
    Text(TextPattern),
    /// An operator applied to its operand.
    Op(Operator, Box<Expr>),
    /// A front matter property test (`[name]`, `[name:value]`).
    Property {
        /// The property name, as written.
        name: String,
        /// What its value must satisfy.
        test: PropertyTest,
    },
}

/// What a text leaf looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextPattern {
    /// A word or a quoted phrase, matched as a substring.
    Literal(String),
    /// A regular expression (`/…/`, Rust `regex` syntax).
    Regex(String),
}

/// A search operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operator {
    /// `file:`: the file name.
    File,
    /// `path:`: the vault-relative path.
    Path,
    /// `content:`: the note text only (not the file name).
    Content,
    /// `tag:`: the note's tags (nested tags included).
    Tag,
    /// `line:`: all terms on the same line.
    Line,
    /// `block:`: all terms in the same block (paragraph, list, fence).
    Block,
    /// `section:`: all terms in the same section (between headings).
    Section,
    /// `task:`: the line of any task.
    Task,
    /// `task-todo:`: the line of an open task (`[ ]`).
    TaskTodo,
    /// `task-done:`: the line of a task with any other status character.
    TaskDone,
    /// `match-case:`: case-sensitive operand.
    MatchCase,
    /// `ignore-case:`: case-insensitive operand.
    IgnoreCase,
}

impl Operator {
    /// Every operator with its keyword (without the colon). Longer
    /// keywords sharing a prefix come first.
    pub const ALL: [(&'static str, Operator); 12] = [
        ("task-todo", Operator::TaskTodo),
        ("task-done", Operator::TaskDone),
        ("task", Operator::Task),
        ("file", Operator::File),
        ("path", Operator::Path),
        ("content", Operator::Content),
        ("tag", Operator::Tag),
        ("line", Operator::Line),
        ("block", Operator::Block),
        ("section", Operator::Section),
        ("match-case", Operator::MatchCase),
        ("ignore-case", Operator::IgnoreCase),
    ];

    /// The keyword, without the colon.
    #[must_use]
    pub fn keyword(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, op)| *op == self)
            .map_or("", |(k, _)| k)
    }
}

/// A property test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyTest {
    /// `[name]`: the note has the property.
    Exists,
    /// `[name:null]`: the property is present and empty.
    Null,
    /// `[name:>5]`, `[due:<2026-01-01]`: compared as numbers when both
    /// sides are numbers, else as text ignoring case (ISO dates order
    /// correctly).
    Compare(Comparison, String),
    /// `[name:value]`, `[status:(draft OR review)]`: the operand matched
    /// against the value (each item of a list).
    Value(Box<Expr>),
}

/// A comparison operator in a property test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `=`: equal (numbers numerically, text ignoring case).
    Eq,
}

/// Parses `input`. An empty query (or one of only operators without
/// operands) gives `None`.
#[must_use]
pub fn parse(input: &str) -> Option<Expr> {
    let mut p = Parser { src: input, pos: 0 };
    let mut parts = Vec::new();
    loop {
        if let Some(e) = p.or_expr() {
            parts.push(e);
        }
        p.skip_ws();
        if p.peek() == Some(')') {
            // A stray `)`: skip it and go on.
            p.pos += 1;
            continue;
        }
        if p.at_end() {
            break;
        }
    }
    and_of(parts)
}

fn and_of(mut parts: Vec<Expr>) -> Option<Expr> {
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Expr::And(parts)),
    }
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn skip_ws(&mut self) {
        let rest = self.rest();
        self.pos += rest.len() - rest.trim_start().len();
    }

    /// At the keyword `OR` followed by whitespace or the end.
    fn at_or(&self) -> bool {
        let rest = self.rest();
        rest.starts_with("OR")
            && rest[2..]
                .chars()
                .next()
                .is_none_or(|c| c.is_whitespace() || c == '(' || c == '"')
    }

    fn or_expr(&mut self) -> Option<Expr> {
        let mut alts = Vec::new();
        loop {
            if let Some(e) = self.and_expr() {
                alts.push(e);
            }
            self.skip_ws();
            if self.at_or() {
                self.pos += 2;
                continue;
            }
            break;
        }
        match alts.len() {
            0 => None,
            1 => alts.pop(),
            _ => Some(Expr::Or(alts)),
        }
    }

    fn and_expr(&mut self) -> Option<Expr> {
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            if self.at_end() || self.peek() == Some(')') || self.at_or() {
                break;
            }
            let before = self.pos;
            if let Some(e) = self.unary() {
                items.push(e);
            }
            if self.pos == before {
                // Never loop without progress.
                self.pos += self.peek().map_or(1, char::len_utf8);
            }
        }
        and_of(items)
    }

    fn unary(&mut self) -> Option<Expr> {
        let rest = self.rest();
        if rest.starts_with('-')
            && rest[1..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace() && c != ')')
        {
            self.pos += 1;
            return self.unary().map(|e| Expr::Not(Box::new(e)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Option<Expr> {
        match self.peek()? {
            '(' => {
                self.pos += 1;
                let e = self.or_expr();
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.pos += 1;
                }
                e
            }
            '"' => {
                let text = self.phrase();
                Some(if text.is_empty() {
                    Expr::All
                } else {
                    Expr::Text(TextPattern::Literal(text))
                })
            }
            '/' => match self.regex() {
                Some(re) => Some(Expr::Text(TextPattern::Regex(re))),
                None => self.word_or_operator(),
            },
            '[' => self.property(),
            _ => self.word_or_operator(),
        }
    }

    /// A quoted phrase; `\"` and `\\` are escapes. Unclosed: to the end.
    fn phrase(&mut self) -> String {
        self.pos += 1;
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        let mut consumed = self.rest().len();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    consumed = i + 1;
                    break;
                }
                '\\' => match chars.next() {
                    Some((_, n @ ('"' | '\\'))) => out.push(n),
                    Some((_, n)) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => out.push('\\'),
                },
                _ => out.push(c),
            }
        }
        self.pos += consumed;
        out
    }

    /// `/pattern/` with a closing unescaped slash and a non-empty
    /// pattern; `\/` is a literal slash.
    fn regex(&mut self) -> Option<String> {
        let body = &self.rest()[1..];
        let mut escaped = false;
        for (i, c) in body.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '/' if i > 0 => {
                    let pattern = body[..i].replace("\\/", "/");
                    self.pos += 1 + i + 1;
                    return Some(pattern);
                }
                '/' => return None,
                _ => {}
            }
        }
        None
    }

    fn word_or_operator(&mut self) -> Option<Expr> {
        let rest = self.rest();
        for (keyword, op) in Operator::ALL {
            let len = keyword.len();
            if rest.len() > len
                && rest.is_char_boundary(len)
                && rest[..len].eq_ignore_ascii_case(keyword)
                && rest.as_bytes()[len] == b':'
            {
                self.pos += len + 1;
                if self.peek().is_none_or(|c| c.is_whitespace() || c == ')') {
                    // No operand yet: drop the operator.
                    return None;
                }
                let operand = self.primary()?;
                return Some(Expr::Op(op, Box::new(operand)));
            }
        }
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '(' || c == ')')
            .unwrap_or(rest.len());
        let word = &rest[..end];
        self.pos += end;
        (!word.is_empty()).then(|| Expr::Text(TextPattern::Literal(word.to_owned())))
    }

    fn property(&mut self) -> Option<Expr> {
        self.pos += 1;
        let body = self.rest();
        let mut depth = 0usize;
        let mut in_quote = false;
        let mut end = body.len();
        for (i, c) in body.char_indices() {
            match c {
                '"' => in_quote = !in_quote,
                '[' if !in_quote => depth += 1,
                ']' if !in_quote => {
                    if depth == 0 {
                        end = i;
                        break;
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        let inner = &body[..end];
        self.pos += (end + 1).min(body.len());
        parse_property(inner)
    }
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

fn parse_property(inner: &str) -> Option<Expr> {
    // The name ends at the first colon outside quotes.
    let mut in_quote = false;
    let mut colon = None;
    for (i, c) in inner.char_indices() {
        match c {
            '"' => in_quote = !in_quote,
            ':' if !in_quote => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (name, value) = match colon {
        Some(i) => (&inner[..i], Some(inner[i + 1..].trim())),
        None => (inner, None),
    };
    let name = unquote(name).to_owned();
    if name.is_empty() {
        return None;
    }
    let test = match value {
        None | Some("") => PropertyTest::Exists,
        Some(v) if v.eq_ignore_ascii_case("null") => PropertyTest::Null,
        Some(v) => {
            let cmp = [
                (">=", Comparison::Ge),
                ("<=", Comparison::Le),
                (">", Comparison::Gt),
                ("<", Comparison::Lt),
                ("=", Comparison::Eq),
            ]
            .into_iter()
            .find_map(|(sym, cmp)| v.strip_prefix(sym).map(|rest| (cmp, rest)));
            match cmp {
                Some((cmp, rest)) => PropertyTest::Compare(cmp, unquote(rest).to_owned()),
                None => match parse(v) {
                    Some(e) => PropertyTest::Value(Box::new(e)),
                    None => PropertyTest::Exists,
                },
            }
        }
    };
    Some(Expr::Property { name, test })
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Where a binding applies: context names and a small predicate language.
//!
//! The UI reports the active contexts as a set of names ([`ContextSet`]). A
//! binding's `context` is a predicate over those names:
//!
//! ```text
//! expr  := or
//! or    := and ( "||" and )*
//! and   := unary ( "&&" unary )*
//! unary := "!" unary | name | "(" expr ")"
//! name  := [a-z_][a-z0-9_]*
//! ```
//!
//! For example `editor`, `editor && !autocomplete`, `editor || reader`. A
//! binding without a context applies everywhere. The names are listed in
//! [`CONTEXTS`]. The *focus* contexts name what has keyboard focus, so at
//! most one of them is active at a time; the others are states that combine
//! freely with them. Unknown names are allowed (they are reported as
//! warnings) and behave like states.

use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;

/// A context name the UI sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextInfo {
    pub name: &'static str,
    /// What the context means, for the keymap documentation and editor.
    pub description: &'static str,
    /// Whether this names the focused area. At most one focus context is
    /// active at a time.
    pub focus: bool,
}

/// The contexts the UI knows.
pub const CONTEXTS: &[ContextInfo] = &[
    focus("editor", "The text of a tab in Edit mode has focus"),
    focus("reader", "A tab in Preview mode has focus"),
    focus("tree", "The file tree has focus"),
    focus("outline", "The outline has focus"),
    focus("search", "The vault search panel has focus"),
    focus("palette", "The command palette is open"),
    focus("switcher", "The quick switcher is open"),
    focus(
        "menu",
        "A menu is open: the application menu, a context menu or the link menu",
    ),
    focus("infobar", "An infobar has focus"),
    focus("settings", "The settings window has focus"),
    state("autocomplete", "The editor's autocomplete popup is open"),
    state(
        "split",
        "The active tab shows Edit and Preview side by side",
    ),
];

const fn focus(name: &'static str, description: &'static str) -> ContextInfo {
    ContextInfo {
        name,
        description,
        focus: true,
    }
}

const fn state(name: &'static str, description: &'static str) -> ContextInfo {
    ContextInfo {
        name,
        description,
        focus: false,
    }
}

/// The known context with this name.
pub fn context_info(name: &str) -> Option<&'static ContextInfo> {
    CONTEXTS.iter().find(|c| c.name == name)
}

fn is_focus(name: &str) -> bool {
    context_info(name).is_some_and(|c| c.focus)
}

/// The contexts active at the moment of a key press.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextSet(BTreeSet<String>);

impl ContextSet {
    /// No active context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a context.
    pub fn insert(&mut self, name: impl Into<String>) {
        self.0.insert(name.into());
    }

    /// The set with `name` added.
    pub fn with(mut self, name: impl Into<String>) -> Self {
        self.insert(name);
        self
    }

    /// Whether `name` is active.
    pub fn contains(&self, name: &str) -> bool {
        self.0.contains(name)
    }

    /// The active names, sorted.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }
}

impl<S: Into<String>> FromIterator<S> for ContextSet {
    fn from_iter<I: IntoIterator<Item = S>>(iter: I) -> Self {
        Self(iter.into_iter().map(Into::into).collect())
    }
}

/// A parsed context predicate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextExpr {
    /// Active when the named context is.
    Name(String),
    Not(Box<ContextExpr>),
    /// Active when all parts are (two or more).
    And(Vec<ContextExpr>),
    /// Active when any part is (two or more).
    Or(Vec<ContextExpr>),
}

/// Why a context predicate could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ContextParseError {
    pub message: String,
    /// The byte range of the problem in the parsed text.
    pub range: Range<usize>,
}

/// The longest context accepted, in tokens. It bounds the size of the
/// normal form used for conflict checks.
const MAX_TOKENS: usize = 64;

/// One literal of a conjunction: a context name, negated or not.
pub(crate) type Literal = (String, bool);

impl ContextExpr {
    /// Parses a predicate such as `editor && !autocomplete`.
    pub fn parse(text: &str) -> Result<Self, ContextParseError> {
        let tokens = tokenize(text)?;
        if tokens.len() > MAX_TOKENS {
            return Err(ContextParseError {
                message: format!("a context can have at most {MAX_TOKENS} names and operators"),
                range: 0..text.len(),
            });
        }
        let mut parser = Parser {
            tokens: &tokens,
            pos: 0,
            end: text.len(),
        };
        let expr = parser.or()?;
        if let Some((token, range)) = tokens.get(parser.pos) {
            return Err(ContextParseError {
                message: format!("expected `&&`, `||` or the end, found {}", token.describe()),
                range: range.clone(),
            });
        }
        Ok(expr)
    }

    /// Whether the predicate holds for the active contexts.
    pub fn matches(&self, active: &ContextSet) -> bool {
        match self {
            Self::Name(name) => active.contains(name),
            Self::Not(inner) => !inner.matches(active),
            Self::And(parts) => parts.iter().all(|p| p.matches(active)),
            Self::Or(parts) => parts.iter().any(|p| p.matches(active)),
        }
    }

    /// Every context name used, in order of first use.
    pub fn names(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_names(&mut out);
        out
    }

    fn collect_names<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Name(name) => {
                if !out.contains(&name.as_str()) {
                    out.push(name);
                }
            }
            Self::Not(inner) => inner.collect_names(out),
            Self::And(parts) | Self::Or(parts) => {
                for p in parts {
                    p.collect_names(out);
                }
            }
        }
    }

    /// The predicate in disjunctive normal form, without terms that can
    /// never hold (`x && !x`, or two focus contexts at once).
    pub(crate) fn dnf(&self) -> Vec<Vec<Literal>> {
        let mut terms = self.dnf_raw(true);
        terms.retain_mut(|term| {
            term.sort();
            term.dedup();
            consistent(term)
        });
        terms
    }

    fn dnf_raw(&self, positive: bool) -> Vec<Vec<Literal>> {
        match (self, positive) {
            (Self::Name(name), _) => vec![vec![(name.clone(), positive)]],
            (Self::Not(inner), _) => inner.dnf_raw(!positive),
            // De Morgan: a negated conjunction is a disjunction and back.
            (Self::Or(parts), true) | (Self::And(parts), false) => {
                parts.iter().flat_map(|p| p.dnf_raw(positive)).collect()
            }
            (Self::And(parts), true) | (Self::Or(parts), false) => {
                let mut acc: Vec<Vec<Literal>> = vec![Vec::new()];
                for p in parts {
                    let terms = p.dnf_raw(positive);
                    let mut next = Vec::with_capacity(acc.len() * terms.len());
                    for a in &acc {
                        for t in &terms {
                            let mut joined = a.clone();
                            joined.extend(t.iter().cloned());
                            next.push(joined);
                        }
                    }
                    acc = next;
                }
                acc
            }
        }
    }

    /// How specific the predicate is: the fewest literals in any of its
    /// alternatives. `editor && !autocomplete` (2) is more specific than
    /// `editor` (1) and `editor || reader` (1).
    pub fn specificity(&self) -> usize {
        self.dnf().iter().map(Vec::len).min().unwrap_or(0)
    }

    /// Whether the predicate can hold at all.
    pub fn is_satisfiable(&self) -> bool {
        !self.dnf().is_empty()
    }

    fn precedence(&self) -> u8 {
        match self {
            Self::Or(_) => 0,
            Self::And(_) => 1,
            Self::Not(_) | Self::Name(_) => 2,
        }
    }

    fn fmt_child(&self, child: &Self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if child.precedence() <= self.precedence() && !matches!(child, Self::Name(_)) {
            write!(f, "({child})")
        } else {
            write!(f, "{child}")
        }
    }
}

impl fmt::Display for ContextExpr {
    /// The canonical form, which [`ContextExpr::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => f.write_str(name),
            Self::Not(inner) => {
                f.write_str("!")?;
                if matches!(**inner, Self::Name(_) | Self::Not(_)) {
                    write!(f, "{inner}")
                } else {
                    write!(f, "({inner})")
                }
            }
            Self::And(parts) | Self::Or(parts) => {
                let op = if matches!(self, Self::And(_)) {
                    " && "
                } else {
                    " || "
                };
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        f.write_str(op)?;
                    }
                    self.fmt_child(p, f)?;
                }
                Ok(())
            }
        }
    }
}

/// Whether a conjunction of literals can hold: no name both required and
/// excluded, and at most one focus context required.
fn consistent(term: &[Literal]) -> bool {
    let mut focus = 0;
    for (i, (name, positive)) in term.iter().enumerate() {
        if term[i + 1..]
            .iter()
            .any(|(other, p)| other == name && p != positive)
        {
            return false;
        }
        if *positive && is_focus(name) {
            focus += 1;
        }
    }
    focus <= 1
}

fn always() -> Vec<Vec<Literal>> {
    vec![Vec::new()]
}

/// Whether two binding contexts can be active at the same time. `None`
/// means everywhere.
pub fn overlaps(a: Option<&ContextExpr>, b: Option<&ContextExpr>) -> bool {
    let a = a.map_or_else(always, ContextExpr::dnf);
    let b = b.map_or_else(always, ContextExpr::dnf);
    a.iter().any(|ta| {
        b.iter().any(|tb| {
            let mut joined: Vec<Literal> = ta.iter().chain(tb).cloned().collect();
            joined.sort();
            joined.dedup();
            consistent(&joined)
        })
    })
}

/// The most names [`covered`] enumerates before giving up.
const MAX_ENUMERATED_NAMES: usize = 16;

/// Whether `others` together hold everywhere `target` holds. `None` means
/// everywhere. Answers `false` when there are too many names to check.
pub(crate) fn covered(target: Option<&ContextExpr>, others: &[Option<&ContextExpr>]) -> bool {
    if others.iter().any(Option::is_none) {
        return true;
    }
    let mut names: Vec<&str> = target.map(ContextExpr::names).unwrap_or_default();
    for e in others.iter().flatten() {
        for n in e.names() {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    if names.len() > MAX_ENUMERATED_NAMES {
        return false;
    }
    let focus: Vec<bool> = names.iter().map(|n| is_focus(n)).collect();
    for bits in 0u32..(1 << names.len()) {
        let on = |i: usize| bits & (1 << i) != 0;
        if (0..names.len()).filter(|&i| focus[i] && on(i)).count() > 1 {
            continue;
        }
        let active: ContextSet = (0..names.len())
            .filter(|&i| on(i))
            .map(|i| names[i])
            .collect();
        let target_holds = target.is_none_or(|t| t.matches(&active));
        if target_holds && !others.iter().flatten().any(|o| o.matches(&active)) {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Name(String),
    Not,
    And,
    Or,
    Open,
    Close,
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Self::Name(name) => format!("`{name}`"),
            Self::Not => "`!`".to_owned(),
            Self::And => "`&&`".to_owned(),
            Self::Or => "`||`".to_owned(),
            Self::Open => "`(`".to_owned(),
            Self::Close => "`)`".to_owned(),
        }
    }
}

fn tokenize(text: &str) -> Result<Vec<(Token, Range<usize>)>, ContextParseError> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let start = i;
        let token = match b {
            b' ' | b'\t' => {
                i += 1;
                continue;
            }
            b'!' => Token::Not,
            b'(' => Token::Open,
            b')' => Token::Close,
            b'&' | b'|' => {
                if bytes.get(i + 1) != Some(&b) {
                    let op = if b == b'&' { "&&" } else { "||" };
                    return Err(ContextParseError {
                        message: format!("expected `{op}`"),
                        range: i..i + 1,
                    });
                }
                i += 1;
                if b == b'&' { Token::And } else { Token::Or }
            }
            b'a'..=b'z' | b'_' => {
                while i + 1 < bytes.len()
                    && matches!(bytes[i + 1], b'a'..=b'z' | b'0'..=b'9' | b'_')
                {
                    i += 1;
                }
                Token::Name(text[start..=i].to_owned())
            }
            _ => {
                let c = text[i..].chars().next().unwrap_or(' ');
                let message = if c.is_ascii_uppercase() {
                    format!("context names are lower-case; found `{c}`")
                } else {
                    format!("unexpected `{c}` in a context")
                };
                return Err(ContextParseError {
                    message,
                    range: i..i + c.len_utf8(),
                });
            }
        };
        i += 1;
        tokens.push((token, start..i));
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [(Token, Range<usize>)],
    pos: usize,
    end: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|(t, _)| t)
    }

    fn or(&mut self) -> Result<ContextExpr, ContextParseError> {
        let mut parts = vec![self.and()?];
        while self.peek() == Some(&Token::Or) {
            self.pos += 1;
            parts.push(self.and()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            ContextExpr::Or(flatten(parts, true))
        })
    }

    fn and(&mut self) -> Result<ContextExpr, ContextParseError> {
        let mut parts = vec![self.unary()?];
        while self.peek() == Some(&Token::And) {
            self.pos += 1;
            parts.push(self.unary()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            ContextExpr::And(flatten(parts, false))
        })
    }

    fn unary(&mut self) -> Result<ContextExpr, ContextParseError> {
        let Some((token, range)) = self.tokens.get(self.pos) else {
            return Err(ContextParseError {
                message: "expected a context name".to_owned(),
                range: self.end..self.end,
            });
        };
        self.pos += 1;
        match token {
            Token::Not => Ok(ContextExpr::Not(Box::new(self.unary()?))),
            Token::Name(name) => Ok(ContextExpr::Name(name.clone())),
            Token::Open => {
                let inner = self.or()?;
                match self.tokens.get(self.pos) {
                    Some((Token::Close, _)) => {
                        self.pos += 1;
                        Ok(inner)
                    }
                    Some((other, range)) => Err(ContextParseError {
                        message: format!("expected `)`, found {}", other.describe()),
                        range: range.clone(),
                    }),
                    None => Err(ContextParseError {
                        message: "expected `)`".to_owned(),
                        range: range.start..self.end,
                    }),
                }
            }
            other => Err(ContextParseError {
                message: format!("expected a context name, found {}", other.describe()),
                range: range.clone(),
            }),
        }
    }
}

/// Merges nested operators of the same kind: `(a || b) || c` is `a || b || c`.
fn flatten(parts: Vec<ContextExpr>, or: bool) -> Vec<ContextExpr> {
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        match p {
            ContextExpr::Or(inner) if or => out.extend(inner),
            ContextExpr::And(inner) if !or => out.extend(inner),
            p => out.push(p),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(text: &str) -> ContextExpr {
        ContextExpr::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn parses_and_prints_canonically() {
        for (input, canonical) in [
            ("editor", "editor"),
            ("editor&&!autocomplete", "editor && !autocomplete"),
            ("(editor || reader) && split", "(editor || reader) && split"),
            ("editor || reader && split", "editor || reader && split"),
            ("((editor))", "editor"),
            ("!(editor || reader)", "!(editor || reader)"),
            ("!!editor", "!!editor"),
            ("(a || b) || c", "a || b || c"),
        ] {
            let e = expr(input);
            assert_eq!(e.to_string(), canonical, "{input}");
            assert_eq!(expr(canonical), e, "{input}");
        }
    }

    #[test]
    fn reports_errors_with_ranges() {
        for (input, range) in [
            ("editor &", 7..8),
            ("editor && ", 10..10),
            ("Editor", 0..1),
            ("(editor", 0..7),
            ("editor reader", 7..13),
            ("", 0..0),
        ] {
            let err = ContextExpr::parse(input).err();
            assert_eq!(err.map(|e| e.range), Some(range), "{input}");
        }
    }

    #[test]
    fn overlap_respects_focus() {
        let editor = expr("editor");
        let reader = expr("reader");
        let both = expr("editor || reader");
        let popup = expr("editor && autocomplete");
        assert!(!overlaps(Some(&editor), Some(&reader)));
        assert!(overlaps(Some(&editor), Some(&both)));
        assert!(overlaps(Some(&editor), Some(&popup)));
        assert!(overlaps(None, Some(&reader)));
        assert!(!overlaps(Some(&expr("!editor")), Some(&editor)));
        assert!(!expr("editor && reader").is_satisfiable());
        assert_eq!(popup.specificity(), 2);
        assert_eq!(both.specificity(), 1);
    }

    #[test]
    fn coverage() {
        let editor = expr("editor");
        let not_editor = expr("!editor");
        let popup = expr("editor && autocomplete");
        assert!(covered(Some(&popup), &[Some(&editor)]));
        assert!(!covered(Some(&editor), &[Some(&popup)]));
        assert!(covered(None, &[Some(&editor), Some(&not_editor)]));
        assert!(covered(Some(&editor), &[None]));
    }
}

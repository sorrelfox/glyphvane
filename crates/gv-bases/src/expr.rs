// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The expression language: lexer, syntax tree and parser.
//!
//! Precedence, lowest first: `||`; `&&`; `==` `!=`; `<` `<=` `>` `>=`;
//! `+` `-`; `*` `/` `%`; unary `!` `-`; then member access (`a.b`),
//! indexing (`a[0]`, `a["key"]`) and calls (`f(x)`, `a.m(x)`). Literals:
//! numbers, strings in single or double quotes (with `\` escapes),
//! `true`, `false`, `null`, lists (`[1, 2]`) and objects
//! (`{"name": "value"}`, keys are strings or names).

use std::fmt;
use std::sync::Arc;

/// The deepest nesting the parser accepts (expressions come from files,
/// so deep nesting must not exhaust the stack).
pub const MAX_DEPTH: usize = 64;
/// The longest expression the parser accepts, in bytes.
pub const MAX_LEN: usize = 16 * 1024;
/// The deepest syntax tree the parser returns (long operator and member
/// chains nest without parentheses); keeps evaluation's recursion bounded.
pub const MAX_TREE_DEPTH: usize = 128;

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    And,
    /// `||`
    Or,
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `!`
    Not,
    /// `-`
    Neg,
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// `null`
    Null,
    /// `true`, `false`
    Bool(bool),
    /// A number.
    Number(f64),
    /// A string literal.
    String(Arc<str>),
    /// `[a, b]`
    List(Vec<Expr>),
    /// `{"key": a, other: b}`
    Object(Vec<(Arc<str>, Expr)>),
    /// A name: a property, `file`, `note`, `formula`, `this`, or a
    /// variable (`value`, `index`, `acc`, `values`).
    Ident(Arc<str>),
    /// `target.name`
    Member(Box<Expr>, Arc<str>),
    /// `target[index]`
    Index(Box<Expr>, Box<Expr>),
    /// `name(args)`: a global function.
    Call(Arc<str>, Vec<Expr>),
    /// `target.name(args)`: a method.
    Method(Box<Expr>, Arc<str>, Vec<Expr>),
    /// A unary operation.
    Unary(UnOp, Box<Expr>),
    /// A binary operation.
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

/// A syntax error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// What is wrong.
    pub message: String,
    /// Byte offset in the expression.
    pub offset: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at column {}", self.message, self.offset + 1)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(Arc<str>),
    Ident(Arc<str>),
    Op(&'static str),
    End,
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

const OPERATORS: &[&str] = &[
    "==", "!=", "<=", ">=", "&&", "||", "+", "-", "*", "/", "%", "<", ">", "!", "(", ")", "[", "]",
    ",", ".", "{", "}", ":",
];

impl Lexer<'_> {
    fn error(&self, message: impl Into<String>, offset: usize) -> ParseError {
        ParseError {
            message: message.into(),
            offset,
        }
    }

    /// The next token and its offset.
    fn next(&mut self) -> Result<(Tok, usize), ParseError> {
        let rest = &self.src[self.pos..];
        let trimmed = rest.trim_start();
        self.pos += rest.len() - trimmed.len();
        let start = self.pos;
        let Some(c) = trimmed.chars().next() else {
            return Ok((Tok::End, start));
        };
        if c.is_ascii_digit()
            || (c == '.' && trimmed[1..].starts_with(|d: char| d.is_ascii_digit()))
        {
            let mut end = 0;
            let bytes = trimmed.as_bytes();
            let mut seen_dot = false;
            while end < bytes.len() {
                let b = bytes[end];
                if b.is_ascii_digit() {
                    end += 1;
                } else if b == b'.'
                    && !seen_dot
                    && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
                {
                    seen_dot = true;
                    end += 1;
                } else if (b == b'e' || b == b'E')
                    && (bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
                        || (matches!(bytes.get(end + 1), Some(b'+' | b'-'))
                            && bytes.get(end + 2).is_some_and(u8::is_ascii_digit)))
                {
                    end += 2;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    break;
                } else {
                    break;
                }
            }
            let n: f64 = trimmed[..end]
                .parse()
                .map_err(|_| self.error("invalid number", start))?;
            self.pos += end;
            return Ok((Tok::Num(n), start));
        }
        if c == '"' || c == '\'' {
            let mut out = String::new();
            let mut chars = trimmed[1..].char_indices();
            while let Some((i, ch)) = chars.next() {
                if ch == c {
                    self.pos += 1 + i + 1;
                    return Ok((Tok::Str(out.into()), start));
                }
                if ch == '\\' {
                    match chars.next() {
                        Some((_, 'n')) => out.push('\n'),
                        Some((_, 't')) => out.push('\t'),
                        Some((_, 'r')) => out.push('\r'),
                        Some((_, other)) => out.push(other),
                        None => break,
                    }
                } else {
                    out.push(ch);
                }
            }
            return Err(self.error("unterminated string", start));
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let end = trimmed
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '$'))
                .unwrap_or(trimmed.len());
            self.pos += end;
            return Ok((Tok::Ident(trimmed[..end].into()), start));
        }
        for op in OPERATORS {
            if trimmed.starts_with(op) {
                self.pos += op.len();
                return Ok((Tok::Op(op), start));
            }
        }
        Err(self.error(format!("unexpected character `{c}`"), start))
    }
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    tok: Tok,
    at: usize,
    depth: usize,
}

/// Parses an expression.
///
/// # Errors
///
/// A syntax error, an expression longer than [`MAX_LEN`] or nested
/// deeper than [`MAX_DEPTH`].
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    if src.len() > MAX_LEN {
        return Err(ParseError {
            message: format!("expression longer than {MAX_LEN} bytes"),
            offset: 0,
        });
    }
    let mut lexer = Lexer { src, pos: 0 };
    let (tok, at) = lexer.next()?;
    if tok == Tok::End {
        return Err(ParseError {
            message: "empty expression".into(),
            offset: 0,
        });
    }
    let mut p = Parser {
        lexer,
        tok,
        at,
        depth: 0,
    };
    let e = p.expr()?;
    if p.tok != Tok::End {
        return Err(p.error(format!("unexpected {}", describe(&p.tok))));
    }
    if tree_depth(&e) > MAX_TREE_DEPTH {
        return Err(ParseError {
            message: "expression nested too deeply".into(),
            offset: 0,
        });
    }
    Ok(e)
}

/// The depth of a syntax tree, without recursion.
fn tree_depth(root: &Expr) -> usize {
    let mut max = 0;
    let mut stack = vec![(root, 1)];
    while let Some((e, d)) = stack.pop() {
        max = max.max(d);
        match e {
            Expr::Null | Expr::Bool(_) | Expr::Number(_) | Expr::String(_) | Expr::Ident(_) => {}
            Expr::List(items) | Expr::Call(_, items) => {
                stack.extend(items.iter().map(|i| (i, d + 1)));
            }
            Expr::Member(t, _) | Expr::Unary(_, t) => stack.push((t, d + 1)),
            Expr::Index(a, b) | Expr::Binary(_, a, b) => {
                stack.push((a, d + 1));
                stack.push((b, d + 1));
            }
            Expr::Method(t, _, args) => {
                stack.push((t, d + 1));
                stack.extend(args.iter().map(|i| (i, d + 1)));
            }
            Expr::Object(entries) => stack.extend(entries.iter().map(|(_, v)| (v, d + 1))),
        }
    }
    max
}

fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Num(n) => format!("number {n}"),
        Tok::Str(_) => "string".into(),
        Tok::Ident(s) => format!("`{s}`"),
        Tok::Op(o) => format!("`{o}`"),
        Tok::End => "end of expression".into(),
    }
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            message: message.into(),
            offset: self.at,
        }
    }

    fn bump(&mut self) -> Result<Tok, ParseError> {
        let (tok, at) = self.lexer.next()?;
        self.at = at;
        Ok(std::mem::replace(&mut self.tok, tok))
    }

    fn eat(&mut self, op: &str) -> Result<bool, ParseError> {
        if matches!(self.tok, Tok::Op(o) if o == op) {
            self.bump()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect(&mut self, op: &str) -> Result<(), ParseError> {
        if self.eat(op)? {
            Ok(())
        } else {
            Err(self.error(format!("expected `{op}`, found {}", describe(&self.tok))))
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("expression nested too deeply"));
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.enter()?;
        let e = self.binary(0);
        self.depth -= 1;
        e
    }

    fn binary(&mut self, level: usize) -> Result<Expr, ParseError> {
        const LEVELS: &[&[(&str, BinOp)]] = &[
            &[("||", BinOp::Or)],
            &[("&&", BinOp::And)],
            &[("==", BinOp::Eq), ("!=", BinOp::Ne)],
            &[
                ("<=", BinOp::Le),
                (">=", BinOp::Ge),
                ("<", BinOp::Lt),
                (">", BinOp::Gt),
            ],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Rem)],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Tok::Op(o) = self.tok {
            let Some(&(_, op)) = LEVELS[level].iter().find(|(s, _)| *s == o) else {
                break;
            };
            self.bump()?;
            let right = self.binary(level + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        let op = match self.tok {
            Tok::Op("!") => UnOp::Not,
            Tok::Op("-") => UnOp::Neg,
            Tok::Op("+") => {
                self.bump()?;
                return self.unary();
            }
            _ => return self.postfix(),
        };
        self.bump()?;
        self.enter()?;
        let inner = self.unary();
        self.depth -= 1;
        Ok(Expr::Unary(op, Box::new(inner?)))
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.primary()?;
        loop {
            if self.eat(".")? {
                let Tok::Ident(name) = self.bump()? else {
                    return Err(self.error("expected a name after `.`"));
                };
                if self.eat("(")? {
                    let args = self.args()?;
                    e = Expr::Method(Box::new(e), name, args);
                } else {
                    e = Expr::Member(Box::new(e), name);
                }
            } else if self.eat("[")? {
                let index = self.expr()?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(index));
            } else {
                return Ok(e);
            }
        }
    }

    /// Arguments after `(`, up to and including `)`.
    fn args(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut args = Vec::new();
        if self.eat(")")? {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat(")")? {
                return Ok(args);
            }
            self.expect(",")?;
        }
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let at = self.at;
        let fail = |message: String| ParseError {
            message,
            offset: at,
        };
        // Checked before the lexer reads on into the pattern.
        if self.tok == Tok::Op("/") {
            return Err(fail("regular expressions are not supported".into()));
        }
        match self.bump()? {
            Tok::Num(n) => Ok(Expr::Number(n)),
            Tok::Str(s) => Ok(Expr::String(s)),
            Tok::Ident(name) => match &*name {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "null" => Ok(Expr::Null),
                _ => {
                    if self.eat("(")? {
                        Ok(Expr::Call(name, self.args()?))
                    } else {
                        Ok(Expr::Ident(name))
                    }
                }
            },
            Tok::Op("(") => {
                let e = self.expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Op("[") => {
                let mut items = Vec::new();
                if !self.eat("]")? {
                    loop {
                        items.push(self.expr()?);
                        if self.eat("]")? {
                            break;
                        }
                        self.expect(",")?;
                    }
                }
                Ok(Expr::List(items))
            }
            Tok::Op("{") => {
                let mut entries = Vec::new();
                if !self.eat("}")? {
                    loop {
                        let key = match self.bump()? {
                            Tok::Str(k) | Tok::Ident(k) => k,
                            other => {
                                return Err(self
                                    .error(format!("expected a key, found {}", describe(&other))));
                            }
                        };
                        self.expect(":")?;
                        entries.push((key, self.expr()?));
                        if self.eat("}")? {
                            break;
                        }
                        self.expect(",")?;
                    }
                }
                Ok(Expr::Object(entries))
            }
            other => Err(fail(format!("unexpected {}", describe(&other)))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr {
        parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn id(s: &str) -> Box<Expr> {
        Box::new(Expr::Ident(s.into()))
    }

    #[test]
    fn precedence() {
        assert_eq!(
            p("1 + 2 * 3"),
            Expr::Binary(
                BinOp::Add,
                Box::new(Expr::Number(1.0)),
                Box::new(Expr::Binary(
                    BinOp::Mul,
                    Box::new(Expr::Number(2.0)),
                    Box::new(Expr::Number(3.0))
                ))
            )
        );
        assert_eq!(
            p("a || b && !c"),
            Expr::Binary(
                BinOp::Or,
                id("a"),
                Box::new(Expr::Binary(
                    BinOp::And,
                    id("b"),
                    Box::new(Expr::Unary(UnOp::Not, id("c")))
                ))
            )
        );
        assert!(
            matches!(p("a - b - c"), Expr::Binary(BinOp::Sub, l, _) if matches!(*l, Expr::Binary(BinOp::Sub, _, _)))
        );
        assert!(matches!(p("x >= 1 == true"), Expr::Binary(BinOp::Eq, _, _)));
        assert!(matches!(p("10 % 3 * 2"), Expr::Binary(BinOp::Mul, _, _)));
    }

    #[test]
    fn postfix_chains() {
        assert_eq!(
            p("file.hasTag(\"a\", 'b')"),
            Expr::Method(
                id("file"),
                "hasTag".into(),
                vec![Expr::String("a".into()), Expr::String("b".into())]
            )
        );
        assert_eq!(
            p("note[\"my prop\"][0].length"),
            Expr::Member(
                Box::new(Expr::Index(
                    Box::new(Expr::Index(
                        id("note"),
                        Box::new(Expr::String("my prop".into()))
                    )),
                    Box::new(Expr::Number(0.0))
                )),
                "length".into()
            )
        );
        assert_eq!(p("now()"), Expr::Call("now".into(), vec![]));
        assert!(matches!(
            p("(price / age).toFixed(2)"),
            Expr::Method(_, _, _)
        ));
        assert_eq!(
            p("[1, 'a']"),
            Expr::List(vec![Expr::Number(1.0), Expr::String("a".into())])
        );
        assert_eq!(p("[]"), Expr::List(vec![]));
        assert_eq!(
            p(r#"{"name": "value", n: 1}"#),
            Expr::Object(vec![
                ("name".into(), Expr::String("value".into())),
                ("n".into(), Expr::Number(1.0)),
            ])
        );
        assert_eq!(p("{}"), Expr::Object(vec![]));
        assert!(parse("{1: 2}").is_err());
        assert!(parse("{\"a\" 2}").is_err());
    }

    #[test]
    fn regular_expressions_are_named() {
        for src in [
            r#"/abc/.matches("x")"#,
            r#""a:b".replace(/:/, "-")"#,
            r#""a".split(/,/g)"#,
        ] {
            let e = parse(src).expect_err(src);
            assert_eq!(e.message, "regular expressions are not supported", "{src}");
        }
    }

    #[test]
    fn literals() {
        assert_eq!(p("2.5"), Expr::Number(2.5));
        assert_eq!(p(".5"), Expr::Number(0.5));
        assert_eq!(p("1e3"), Expr::Number(1000.0));
        assert_eq!(p("'it\\'s'"), Expr::String("it's".into()));
        assert_eq!(p("\"a\\\"b\""), Expr::String("a\"b".into()));
        assert_eq!(p("null"), Expr::Null);
        assert_eq!(p("-1"), Expr::Unary(UnOp::Neg, Box::new(Expr::Number(1.0))));
        assert_eq!(p("größe"), Expr::Ident("größe".into()));
        // A member after a number literal needs parentheses: `(1).x`.
        assert!(matches!(p("(123).toString()"), Expr::Method(_, _, _)));
    }

    #[test]
    fn errors() {
        for bad in [
            "",
            "1 +",
            "(1",
            "a.",
            "'open",
            "a ? b : c",
            "f(1,)",
            "1 2",
            "/re/",
            "a[1",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should fail");
        }
        let e = parse("a +* b").expect_err("bad");
        assert_eq!(e.offset, 3);
    }

    #[test]
    fn depth_and_length_limits() {
        let deep = format!("{}1{}", "(".repeat(200), ")".repeat(200));
        assert!(parse(&deep).is_err());
        let nots = format!("{}x", "!".repeat(200));
        assert!(parse(&nots).is_err());
        let ok = format!("{}1{}", "(".repeat(20), ")".repeat(20));
        assert!(parse(&ok).is_ok());
        assert!(parse(&"1+".repeat(MAX_LEN)).is_err());
        let chain = format!("1{}", "+1".repeat(1000));
        assert!(parse(&chain).is_err());
        let members = format!("a{}", ".b".repeat(1000));
        assert!(parse(&members).is_err());
    }
}

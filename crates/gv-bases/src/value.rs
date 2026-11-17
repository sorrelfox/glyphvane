// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Values of the expression language.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use crate::date::{Date, Duration};

/// A link (`[[Note]]` in front matter, `link("Note")`, `file.asLink()`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Link {
    /// The link text as written (`Note`, `folder/Note#Heading`).
    pub text: Arc<str>,
    /// The vault path it resolves to, if any.
    pub path: Option<Arc<str>>,
    /// Display text (`[[Note|display]]`, the second `link()` argument).
    pub display: Option<Arc<str>>,
}

impl Link {
    /// What a view shows: the display text, else the link text.
    #[must_use]
    pub fn label(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.text)
    }
}

/// An external URL (`https://…` in front matter, `link("https://…")`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Url {
    /// The URL.
    pub url: Arc<str>,
    /// Display text.
    pub display: Option<Arc<str>>,
}

/// A value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Nothing (a missing property, `null`).
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(f64),
    /// Text.
    String(Arc<str>),
    /// A date, with or without a time.
    Date(Date),
    /// A length of time.
    Duration(Duration),
    /// A list.
    List(Arc<[Value]>),
    /// A mapping (nested front matter, `file.properties`), in order.
    Object(Arc<[(Arc<str>, Value)]>),
    /// A link to a note or file.
    Link(Arc<Link>),
    /// An external URL.
    Url(Arc<Url>),
    /// A file of the vault, by path.
    File(Arc<str>),
    /// `html(…)`: markup the view sanitizes before showing it.
    Html(Arc<str>),
    /// `image(…)`: an image path, link target or URL.
    Image(Arc<str>),
    /// `icon(…)`: a Lucide icon name.
    Icon(Arc<str>),
}

impl Value {
    /// A string value.
    #[must_use]
    pub fn string(s: impl Into<Arc<str>>) -> Self {
        Self::String(s.into())
    }

    /// A list value.
    #[must_use]
    pub fn list(items: Vec<Value>) -> Self {
        Self::List(items.into())
    }

    /// The type name `isType()` compares with: `null`, `boolean`,
    /// `number`, `string`, `date`, `duration`, `list`, `object`, `link`,
    /// `url`, `file`, `html`, `image`, `icon`.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Date(_) => "date",
            Self::Duration(_) => "duration",
            Self::List(_) => "list",
            Self::Object(_) => "object",
            Self::Link(_) => "link",
            Self::Url(_) => "url",
            Self::File(_) => "file",
            Self::Html(_) => "html",
            Self::Image(_) => "image",
            Self::Icon(_) => "icon",
        }
    }

    /// Truthiness: `null`, `false`, `0`, `NaN`, `""`, an empty list and an
    /// empty object are false; everything else is true.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::String(s) => !s.is_empty(),
            Self::List(l) => !l.is_empty(),
            Self::Object(o) => !o.is_empty(),
            _ => true,
        }
    }

    /// Whether the value is empty: `null`, `""`, an empty list or object.
    /// Numbers, dates and booleans are never empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Null => true,
            Self::String(s) => s.is_empty(),
            Self::List(l) => l.is_empty(),
            Self::Object(o) => o.is_empty(),
            _ => false,
        }
    }

    /// The text a cell shows and `toString()` returns.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => format_number(*n),
            Self::String(s) | Self::Html(s) | Self::Image(s) | Self::Icon(s) => s.to_string(),
            Self::Date(d) => d.display(),
            Self::Duration(d) => d.display(),
            Self::List(items) => items
                .iter()
                .map(Value::display)
                .collect::<Vec<_>>()
                .join(", "),
            Self::Object(entries) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.display()))
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            Self::Link(l) => l.label().to_owned(),
            Self::Url(u) => u.display.as_deref().unwrap_or(&u.url).to_owned(),
            Self::File(path) => path.to_string(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// A number as JavaScript prints it: integers without `.0`, `NaN`,
/// `Infinity`.
#[must_use]
pub fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else if n == 0.0 {
        "0".into()
    } else if n == n.trunc() && n.abs() < 1e21 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

/// Rank of a type in mixed-type sorting: numbers, dates, durations,
/// strings and the rest, booleans; empty values always sort last.
fn rank(v: &Value) -> u8 {
    match v {
        Value::Number(_) => 0,
        Value::Date(_) => 1,
        Value::Duration(_) => 2,
        Value::Bool(_) => 4,
        Value::Null => 9,
        _ => 3,
    }
}

/// The order used to sort rows and groups. Text compares ignoring case
/// and with numbers inside compared by value (`2` before `10`). Empty
/// values (`null`, `""`, empty lists) sort after everything else in
/// both directions, so callers sort them separately (see
/// [`Value::is_empty`]).
#[must_use]
pub fn sort_cmp(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.total_cmp(y),
        (Value::Date(x), Value::Date(y)) => x.ms.cmp(&y.ms),
        (Value::Duration(x), Value::Duration(y)) => x.approx_ms().total_cmp(&y.approx_ms()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::List(x), Value::List(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let o = sort_cmp(p, q);
                if o != Ordering::Equal {
                    return o;
                }
            }
            x.len().cmp(&y.len())
        }
        _ => match rank(a).cmp(&rank(b)) {
            Ordering::Equal => natural_cmp(&a.display(), &b.display()),
            o => o,
        },
    }
}

/// Case-insensitive comparison with digit runs compared by value.
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut x = a.chars().peekable();
    let mut y = b.chars().peekable();
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) if p.is_ascii_digit() && q.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (mt, nt) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let o = mt.len().cmp(&nt.len()).then_with(|| mt.cmp(nt));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(p), Some(q)) => {
                let o = p.to_lowercase().cmp(q.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_javascript() {
        assert_eq!(format_number(3.0), "3");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(2.5), "2.5");
        assert_eq!(format_number(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(format_number(f64::NAN), "NaN");
        assert_eq!(format_number(f64::INFINITY), "Infinity");
    }

    #[test]
    fn truthiness() {
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Number(0.0).is_truthy());
        assert!(!Value::string("").is_truthy());
        assert!(!Value::list(vec![]).is_truthy());
        assert!(Value::string("x").is_truthy());
        assert!(Value::File("a.md".into()).is_truthy());
    }

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "File10"), Ordering::Less);
        assert_eq!(natural_cmp("b", "A"), Ordering::Greater);
        assert_eq!(natural_cmp("a01", "a1"), Ordering::Less);
        assert_eq!(
            sort_cmp(&Value::Number(2.0), &Value::string("1")),
            Ordering::Less
        );
    }

    #[test]
    fn display() {
        let l = Value::list(vec![Value::Number(1.0), Value::string("a")]);
        assert_eq!(l.display(), "1, a");
        let link = Value::Link(Arc::new(Link {
            text: "Note".into(),
            path: None,
            display: Some("Shown".into()),
        }));
        assert_eq!(link.display(), "Shown");
        assert_eq!(Value::Null.display(), "");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Front matter properties for the content index.
//!
//! YAML front matter is read with a small parser for the subset notes use
//! (Obsidian writes nothing else): top-level `key: value` pairs whose
//! values are scalars (plain, single- or double-quoted, `|` and `>`
//! blocks), flow lists (`[a, b]`) or block lists (`- a`). Anything else
//! (nested maps, anchors) is kept as [`PropertyValue::Other`] with its
//! text. TOML front matter goes through the `toml` crate.
//!
//! Obsidian semantics (≥ 1.9): `tags`, `aliases` and `cssclasses` are
//! lists; the singular keys (`tag`, `alias`, `cssclass`) are ordinary
//! properties. A plain string is accepted as a one-item list (`tags` is
//! split at commas and spaces). Hashtags inside text properties are not
//! tags. `[[links]]` in string values are links.

use std::sync::Arc;

use super::record::{Property, PropertyValue, Span};

/// What the front matter says.
#[derive(Debug, Default)]
pub(crate) struct FrontMatterFacts {
    pub title: Option<Arc<str>>,
    pub aliases: Vec<Arc<str>>,
    pub tags: Vec<Arc<str>>,
    pub cssclasses: Vec<Arc<str>>,
    pub properties: Vec<Property>,
    /// `[[…]]` inside string values: the text between the brackets, and
    /// where the brackets are in the document.
    pub links: Vec<(Arc<str>, Span)>,
}

/// A scalar with its document offset.
struct Scalar<'a> {
    text: std::borrow::Cow<'a, str>,
    at: usize,
    quoted: bool,
}

/// Parses YAML front matter `content` that starts at document offset `base`.
pub(crate) fn parse_yaml(content: &str, base: usize) -> FrontMatterFacts {
    let mut facts = FrontMatterFacts::default();
    let lines: Vec<(usize, &str)> = line_offsets(content);
    let mut i = 0;
    while i < lines.len() {
        let (off, raw) = lines[i];
        let line = raw.trim_end_matches(['\r', '\n']);
        i += 1;
        if line.trim().is_empty()
            || line.starts_with([' ', '\t', '#', '-'])
            || line.trim_start().starts_with('#')
        {
            continue;
        }
        let Some((key, rest_at)) = split_key(line) else {
            continue;
        };
        let rest = strip_comment(line[rest_at..].trim());
        let rest_off =
            base + off + rest_at + (line[rest_at..].len() - line[rest_at..].trim_start().len());
        let value = if rest.is_empty() {
            // A block list, a nested map or nothing.
            let start = i;
            while i < lines.len() {
                let l = lines[i].1.trim_end_matches(['\r', '\n']);
                if l.trim().is_empty() || l.starts_with([' ', '\t', '-']) {
                    i += 1;
                } else {
                    break;
                }
            }
            block_value(&lines[start..i], base)
        } else if rest.starts_with('|') || rest.starts_with('>') {
            let start = i;
            while i < lines.len() {
                let l = lines[i].1.trim_end_matches(['\r', '\n']);
                if l.trim().is_empty() || l.starts_with([' ', '\t']) {
                    i += 1;
                } else {
                    break;
                }
            }
            let folded = rest.starts_with('>');
            let parts: Vec<&str> = lines[start..i]
                .iter()
                .map(|(_, l)| l.trim())
                .filter(|l| !l.is_empty())
                .collect();
            let text = parts.join(if folded { " " } else { "\n" });
            let at = lines.get(start).map_or(rest_off, |(o, _)| base + o);
            Value::Scalar(Scalar {
                text: text.into(),
                at,
                quoted: true,
            })
        } else if rest.starts_with('[') {
            Value::List(flow_list(rest, rest_off))
        } else if rest.starts_with('{') {
            Value::Other(rest.to_owned())
        } else {
            Value::Scalar(scalar(rest, rest_off))
        };
        facts.add(key, value);
    }
    facts
}

/// Parses TOML front matter.
pub(crate) fn parse_toml(content: &str) -> FrontMatterFacts {
    let mut facts = FrontMatterFacts::default();
    let Ok(table) = content.parse::<toml::Table>() else {
        return facts;
    };
    for (key, value) in table {
        let value = match value {
            toml::Value::String(s) => Value::Scalar(Scalar {
                text: s.into(),
                at: 0,
                quoted: true,
            }),
            toml::Value::Integer(n) => Value::Typed(PropertyValue::Number(n.to_string().into())),
            toml::Value::Float(n) => Value::Typed(PropertyValue::Number(n.to_string().into())),
            toml::Value::Boolean(b) => Value::Typed(PropertyValue::Bool(b)),
            toml::Value::Array(items) if items.iter().all(toml::Value::is_str) => Value::List(
                items
                    .into_iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .map(|s| Scalar {
                        text: s.into(),
                        at: 0,
                        quoted: true,
                    })
                    .collect(),
            ),
            other => Value::Other(other.to_string()),
        };
        facts.add(&key, value);
    }
    // TOML values have no reliable offsets: no link spans.
    facts.links.clear();
    facts
}

enum Value<'a> {
    Scalar(Scalar<'a>),
    List(Vec<Scalar<'a>>),
    Typed(PropertyValue),
    Other(String),
}

impl FrontMatterFacts {
    fn add(&mut self, key: &str, value: Value<'_>) {
        // Links in string values.
        match &value {
            Value::Scalar(s) => self.collect_links(s),
            Value::List(items) => {
                for s in items {
                    self.collect_links(s);
                }
            }
            Value::Typed(_) | Value::Other(_) => {}
        }
        let as_list = |value: &Value<'_>| -> Vec<Arc<str>> {
            match value {
                Value::Scalar(s) if !s.text.is_empty() => vec![Arc::from(&*s.text)],
                Value::List(items) => items
                    .iter()
                    .filter(|s| !s.text.is_empty())
                    .map(|s| Arc::from(&*s.text))
                    .collect(),
                _ => Vec::new(),
            }
        };
        match key {
            "title" => {
                if let Value::Scalar(s) = &value
                    && !s.text.trim().is_empty()
                {
                    self.title = Some(s.text.trim().into());
                } else if let Value::Typed(PropertyValue::Number(n)) = &value {
                    self.title = Some(n.clone());
                }
            }
            "aliases" => self.aliases = as_list(&value),
            "cssclasses" => self.cssclasses = as_list(&value),
            "tags" => {
                let items: Vec<Arc<str>> = match &value {
                    Value::Scalar(s) => s
                        .text
                        .split([',', ' '])
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(Arc::from)
                        .collect(),
                    _ => as_list(&value),
                };
                self.tags = items
                    .into_iter()
                    .map(|t| {
                        let t = t.trim_start_matches('#');
                        Arc::from(t.trim())
                    })
                    .filter(|t: &Arc<str>| !t.is_empty())
                    .collect();
            }
            _ => {}
        }
        let value = match value {
            Value::Scalar(s) => classify(&s),
            Value::List(items) => {
                PropertyValue::List(items.iter().map(|s| Arc::from(&*s.text)).collect())
            }
            Value::Typed(v) => v,
            Value::Other(text) => PropertyValue::Other(text.into()),
        };
        self.properties.push(Property {
            key: key.into(),
            value,
        });
    }

    fn collect_links(&mut self, s: &Scalar<'_>) {
        let text = &*s.text;
        let mut from = 0;
        while let Some(open) = text[from..].find("[[").map(|i| i + from) {
            let Some(close) = text[open + 2..].find("]]").map(|i| i + open + 2) else {
                break;
            };
            let inner = &text[open + 2..close];
            if !inner.trim().is_empty() {
                // Quoted scalars may have escapes; their offsets are then
                // approximate.
                let embed = open > 0 && text.as_bytes()[open - 1] == b'!';
                let start = if embed { open - 1 } else { open };
                self.links.push((
                    inner.into(),
                    Span::of(
                        s.at + start + usize::from(s.quoted)
                            ..s.at + close + 2 + usize::from(s.quoted),
                    ),
                ));
            }
            from = close + 2;
        }
    }
}

fn classify(s: &Scalar<'_>) -> PropertyValue {
    let t = s.text.trim();
    if s.quoted {
        return PropertyValue::Text(t.into());
    }
    match t {
        "" | "~" | "null" | "Null" | "NULL" => PropertyValue::Null,
        "true" | "True" | "TRUE" => PropertyValue::Bool(true),
        "false" | "False" | "FALSE" => PropertyValue::Bool(false),
        _ if t.parse::<f64>().is_ok() && !t.ends_with('.') && !t.starts_with('.') => {
            PropertyValue::Number(t.into())
        }
        _ => PropertyValue::Text(t.into()),
    }
}

/// `(offset, line with its ending)` for each line of `s`.
fn line_offsets(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in s.split_inclusive('\n') {
        out.push((at, line));
        at += line.len();
    }
    out
}

/// The key of `key: …` (unquoted) and where the value starts.
fn split_key(line: &str) -> Option<(&str, usize)> {
    let (key, colon) = if let Some(rest) = line.strip_prefix('"') {
        let end = rest.find('"')?;
        (&rest[..end], 1 + end + 1)
    } else if let Some(rest) = line.strip_prefix('\'') {
        let end = rest.find('\'')?;
        (&rest[..end], 1 + end + 1)
    } else {
        // `key:` followed by a space or the end of the line.
        let mut found = None;
        for (i, _) in line.match_indices(':') {
            let next = line[i + 1..].chars().next();
            if next.is_none_or(char::is_whitespace) {
                found = Some(i);
                break;
            }
        }
        let i = found?;
        (line[..i].trim_end(), i)
    };
    let rest = &line[colon..];
    let rest = rest
        .strip_prefix(':')
        .or_else(|| rest.trim_start().strip_prefix(':'))?;
    if key.is_empty() {
        return None;
    }
    Some((key, line.len() - rest.len()))
}

/// Drops a ` # comment` after an unquoted value.
fn strip_comment(value: &str) -> &str {
    if value.starts_with(['"', '\'']) {
        return value;
    }
    if value.starts_with('#') {
        return "";
    }
    match value.find(" #") {
        Some(i) => value[..i].trim_end(),
        None => value,
    }
}

/// A plain or quoted scalar.
fn scalar(text: &str, at: usize) -> Scalar<'_> {
    let t = text.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        let inner = &t[1..t.len() - 1];
        let text = if inner.contains('\\') {
            unescape_double(inner).into()
        } else {
            inner.into()
        };
        return Scalar {
            text,
            at,
            quoted: true,
        };
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        let inner = &t[1..t.len() - 1];
        let text = if inner.contains("''") {
            inner.replace("''", "'").into()
        } else {
            inner.into()
        };
        return Scalar {
            text,
            at,
            quoted: true,
        };
    }
    Scalar {
        text: t.into(),
        at,
        quoted: false,
    }
}

fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// `[a, "b", c]`.
fn flow_list(text: &str, at: usize) -> Vec<Scalar<'_>> {
    let inner_end = text.rfind(']').unwrap_or(text.len());
    let inner = &text[1..inner_end.max(1)];
    let mut items = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    for (i, c) in inner.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[') => depth += 1,
            (None, ']') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                push_item(&mut items, inner, start..i, at + 1);
                start = i + 1;
            }
            _ => {}
        }
    }
    push_item(&mut items, inner, start..inner.len(), at + 1);
    items
}

fn push_item<'a>(
    items: &mut Vec<Scalar<'a>>,
    inner: &'a str,
    range: std::ops::Range<usize>,
    at: usize,
) {
    let piece = &inner[range.clone()];
    let lead = piece.len() - piece.trim_start().len();
    if piece.trim().is_empty() {
        return;
    }
    items.push(scalar(piece, at + range.start + lead));
}

/// The lines under a key with an empty value: a block list, or something
/// else kept as text.
fn block_value<'a>(lines: &[(usize, &'a str)], base: usize) -> Value<'a> {
    let content: Vec<(usize, &str)> = lines
        .iter()
        .map(|&(o, l)| (o, l.trim_end_matches(['\r', '\n'])))
        .filter(|(_, l)| !l.trim().is_empty())
        .collect();
    if content.is_empty() {
        return Value::Typed(PropertyValue::Null);
    }
    let all_items = content.iter().all(|(_, l)| l.trim_start().starts_with('-'));
    if !all_items {
        let text: Vec<&str> = content.iter().map(|(_, l)| *l).collect();
        return Value::Other(text.join("\n"));
    }
    let mut items = Vec::new();
    for (off, l) in content {
        let indent = l.len() - l.trim_start().len();
        let item = &l[indent + 1..];
        if item.trim_start().starts_with(['-', '{']) || item.contains(": ") {
            let text: Vec<&str> = lines
                .iter()
                .map(|(_, l)| l.trim_end_matches(['\r', '\n']))
                .collect();
            return Value::Other(text.join("\n"));
        }
        let lead = item.len() - item.trim_start().len();
        let item = strip_comment(item.trim());
        if item.is_empty() {
            continue;
        }
        items.push(scalar(item, base + off + indent + 1 + lead));
    }
    Value::List(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_obsidian_keys() {
        let fm = "title: My Note\naliases:\n  - Alpha\n  - \"Beta\"\ntags: [one, '#two']\ncssclasses: wide\nrating: 4.5\ndone: true\nempty:\nnested:\n  a: 1\n";
        let f = parse_yaml(fm, 4);
        assert_eq!(f.title.as_deref(), Some("My Note"));
        assert_eq!(f.aliases, vec![Arc::from("Alpha"), Arc::from("Beta")]);
        assert_eq!(f.tags, vec![Arc::from("one"), Arc::from("two")]);
        assert_eq!(f.cssclasses, vec![Arc::from("wide")]);
        let get = |k: &str| {
            f.properties
                .iter()
                .find(|p| &*p.key == k)
                .map(|p| p.value.clone())
        };
        assert_eq!(get("rating"), Some(PropertyValue::Number("4.5".into())));
        assert_eq!(get("done"), Some(PropertyValue::Bool(true)));
        assert_eq!(get("empty"), Some(PropertyValue::Null));
        assert!(matches!(get("nested"), Some(PropertyValue::Other(_))));
    }

    #[test]
    fn singular_keys_are_plain_properties() {
        let f = parse_yaml("tag: a\nalias: b\n", 0);
        assert!(f.tags.is_empty() && f.aliases.is_empty());
        assert_eq!(f.properties.len(), 2);
    }

    #[test]
    fn links_in_values_have_document_spans() {
        let text = "---\nup: \"[[Parent]]\"\nrel:\n  - \"[[A]]\"\n---\n";
        let content = &text[4..text.len() - 4];
        let f = parse_yaml(content, 4);
        assert_eq!(f.links.len(), 2);
        for (inner, span) in &f.links {
            assert_eq!(&text[span.range()], format!("[[{inner}]]"));
        }
    }

    #[test]
    fn comments_and_hash_values() {
        let f = parse_yaml("tags: # none\nx: a # note\n", 0);
        assert!(f.tags.is_empty());
        let x = f
            .properties
            .iter()
            .find(|p| &*p.key == "x")
            .map(|p| p.value.clone());
        assert_eq!(x, Some(PropertyValue::Text("a".into())));
    }

    #[test]
    fn toml_front_matter() {
        let f = parse_toml("title = \"T\"\ntags = [\"a\", \"b\"]\ndraft = true\n");
        assert_eq!(f.title.as_deref(), Some("T"));
        assert_eq!(f.tags.len(), 2);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Front matter as properties: the top-level keys of YAML or TOML front
//! matter with their values as display text, for the properties block.
//!
//! This reads the shapes front matter takes in notes, not YAML or TOML in
//! full: `key: value` (quotes removed), flow lists `[a, b]`, block lists
//! (`- item` lines under a key), block scalars (`|`, `>`), and nested
//! mappings shown as `k: v` pairs; TOML `key = value` and arrays. A line
//! it cannot read becomes part of the property above it, or a property of
//! its own with an empty key. Typed values (dates, links, numbers) come
//! with the document model's properties.

use std::sync::Arc;

use gv_syntax::FrontMatterKind;

use crate::node::CodeBlock;

/// One property of front matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    pub key: Arc<str>,
    /// The values, in order (one for a scalar).
    pub values: Box<[Arc<str>]>,
    /// The first line of the front matter it is written on.
    pub line: u32,
    /// Its tag pills: the values of a `tags`, `tag` or `aliases`-like list
    /// key are shown as pills when the key is `tags` or `tag`.
    pub tags: bool,
}

/// The properties of front matter of kind `kind` whose lines are `code`.
pub(crate) fn parse(kind: FrontMatterKind, code: &CodeBlock) -> Vec<Property> {
    match kind {
        FrontMatterKind::Yaml => yaml(code),
        FrontMatterKind::Toml => toml(code),
    }
}

fn property(key: &str, values: Vec<String>, line: usize) -> Property {
    let k = key.trim();
    Property {
        key: Arc::from(k),
        values: values.into_iter().map(|v| Arc::from(v.as_str())).collect(),
        line: u32::try_from(line).unwrap_or(u32::MAX),
        tags: matches!(k.to_ascii_lowercase().as_str(), "tags" | "tag"),
    }
}

/// A scalar without its quotes and trailing comment.
fn scalar(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        return t[1..t.len() - 1].replace("\\\"", "\"").replace("''", "'");
    }
    match t.find(" #") {
        Some(i) => t[..i].trim_end().to_owned(),
        None => t.to_owned(),
    }
}

/// The items of a flow list `[a, b]` (quotes removed; no nesting).
fn flow_list(s: &str) -> Option<Vec<String>> {
    let inner = s.trim().strip_prefix('[')?.strip_suffix(']')?;
    Some(
        inner
            .split(',')
            .map(scalar)
            .filter(|v| !v.is_empty())
            .collect(),
    )
}

fn yaml(code: &CodeBlock) -> Vec<Property> {
    let mut out: Vec<Property> = Vec::new();
    let mut block_scalar = false;
    for i in 0..code.lines.len() {
        let line = code.line(i);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = line.starts_with([' ', '\t']);
        if !indented && !trimmed.starts_with("- ") && trimmed != "-" {
            block_scalar = false;
            if let Some((key, rest)) = line.split_once(':') {
                let rest = rest.trim();
                let values = if rest.is_empty() {
                    Vec::new()
                } else if rest == "|"
                    || rest == ">"
                    || rest.starts_with("|-")
                    || rest.starts_with(">-")
                {
                    block_scalar = true;
                    Vec::new()
                } else if let Some(items) = flow_list(rest) {
                    items
                } else {
                    vec![scalar(rest)]
                };
                out.push(property(key, values, i));
                continue;
            }
            out.push(property("", vec![trimmed.to_owned()], i));
            continue;
        }
        let Some(last) = out.last_mut() else {
            out.push(property("", vec![trimmed.to_owned()], i));
            continue;
        };
        let mut values: Vec<String> = last.values.iter().map(|v| v.to_string()).collect();
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or((trimmed == "-").then_some(""))
        {
            values.push(scalar(item));
        } else if block_scalar {
            match values.last_mut() {
                Some(v) => {
                    v.push(' ');
                    v.push_str(trimmed);
                }
                None => values.push(trimmed.to_owned()),
            }
        } else if let Some((k, v)) = trimmed.split_once(':') {
            values.push(format!("{}: {}", k.trim(), scalar(v)));
        } else {
            values.push(scalar(trimmed));
        }
        last.values = values.into_iter().map(|v| Arc::from(v.as_str())).collect();
    }
    out
}

fn toml(code: &CodeBlock) -> Vec<Property> {
    let mut out = Vec::new();
    let mut table = String::new();
    for i in 0..code.lines.len() {
        let trimmed = code.line(i).trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains('=') {
            table = trimmed.trim_matches(['[', ']']).trim().to_owned();
            continue;
        }
        let Some((key, rest)) = trimmed.split_once('=') else {
            out.push(property("", vec![trimmed.to_owned()], i));
            continue;
        };
        let key = if table.is_empty() {
            key.trim().to_owned()
        } else {
            format!("{table}.{}", key.trim())
        };
        let values = flow_list(rest).unwrap_or_else(|| vec![scalar(rest)]);
        out.push(property(&key, values, i));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::CodeLine;

    fn code(text: &str) -> CodeBlock {
        let mut lines = Vec::new();
        let mut at = 0;
        for l in text.split('\n') {
            lines.push(CodeLine {
                text: at..at + l.len(),
                source: at..at + l.len(),
                start: at,
                indent: 0,
                units: 0,
            });
            at += l.len() + 1;
        }
        CodeBlock {
            fenced: false,
            info: None,
            language: None,
            text: text.to_owned(),
            lines: lines.into_boxed_slice(),
        }
    }

    fn rows(props: &[Property]) -> Vec<(String, Vec<String>)> {
        props
            .iter()
            .map(|p| {
                (
                    p.key.to_string(),
                    p.values.iter().map(|v| v.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn yaml_shapes() {
        let p = parse(
            FrontMatterKind::Yaml,
            &code(
                "title: \"Following links\"\naliases: [Navigation, 'Link following']\ntags:\n  - links\n  - navigation\nsummary: >\n  one\n  two\nauthor:\n  name: Ann\n  mail: a@b.c\nempty:\n# comment\nodd line",
            ),
        );
        assert_eq!(
            rows(&p),
            [
                ("title".to_owned(), vec!["Following links".to_owned()]),
                (
                    "aliases".to_owned(),
                    vec!["Navigation".to_owned(), "Link following".to_owned()]
                ),
                (
                    "tags".to_owned(),
                    vec!["links".to_owned(), "navigation".to_owned()]
                ),
                ("summary".to_owned(), vec!["one two".to_owned()]),
                (
                    "author".to_owned(),
                    vec!["name: Ann".to_owned(), "mail: a@b.c".to_owned()]
                ),
                ("empty".to_owned(), vec![]),
                (String::new(), vec!["odd line".to_owned()]),
            ]
        );
        assert!(p[2].tags && !p[0].tags);
        assert_eq!(p[2].line, 2);
    }

    #[test]
    fn toml_shapes() {
        let p = parse(
            FrontMatterKind::Toml,
            &code("title = 'Notes'\ntags = [\"a\", \"b\"]\n[extra]\nlevel = 3"),
        );
        assert_eq!(
            rows(&p),
            [
                ("title".to_owned(), vec!["Notes".to_owned()]),
                ("tags".to_owned(), vec!["a".to_owned(), "b".to_owned()]),
                ("extra.level".to_owned(), vec!["3".to_owned()]),
            ]
        );
    }
}

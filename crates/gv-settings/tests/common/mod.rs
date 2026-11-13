// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Generators shared by the integration tests.

#![allow(dead_code)]

use gv_settings::{Kind, Spec, specs};
use proptest::prelude::*;

/// A valid value for `spec`: its TOML source and the normalized value the
/// layer should hold.
pub fn value_for(spec: &'static Spec) -> BoxedStrategy<(String, toml::Value)> {
    match spec.kind {
        Kind::Bool => any::<bool>()
            .prop_map(|b| (b.to_string(), toml::Value::Boolean(b)))
            .boxed(),
        Kind::Integer { min, max } => {
            let max = max.min(min.saturating_add(1_000_000));
            (min..=max)
                .prop_map(|i| (i.to_string(), toml::Value::Integer(i)))
                .boxed()
        }
        Kind::Float { min, max } => {
            let (lo, hi) = ((min * 2.0) as i64, (max * 2.0) as i64);
            (lo..=hi, any::<bool>())
                .prop_map(|(halves, as_int)| {
                    let f = halves as f64 / 2.0;
                    let raw = if as_int && f.fract() == 0.0 {
                        format!("{}", f as i64)
                    } else {
                        format!("{f:?}")
                    };
                    (raw, toml::Value::Float(f))
                })
                .boxed()
        }
        Kind::String => ("[a-zA-Z0-9 _-]{0,12}", any::<bool>())
            .prop_map(|(s, literal)| (quote(&s, literal), toml::Value::String(s)))
            .boxed(),
        Kind::StringList => (
            prop::collection::vec("[a-z][a-z.]{0,8}", 0..4),
            prop::sample::select(vec!["", " "]),
        )
            .prop_map(|(items, pad)| {
                let raw = format!(
                    "[{pad}{}{pad}]",
                    items
                        .iter()
                        .map(|s| quote(s, false))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let value =
                    toml::Value::Array(items.into_iter().map(toml::Value::String).collect());
                (raw, value)
            })
            .boxed(),
        Kind::Enum(variants) => (prop::sample::select(variants.to_vec()), any::<bool>())
            .prop_map(|(v, literal)| (quote(v, literal), toml::Value::String(v.to_owned())))
            .boxed(),
    }
}

fn quote(s: &str, literal: bool) -> String {
    if literal {
        format!("'{s}'")
    } else {
        format!("\"{s}\"")
    }
}

/// Any setting.
pub fn any_spec() -> impl Strategy<Value = &'static Spec> {
    prop::sample::select(specs().iter().collect::<Vec<_>>())
}

/// How one section is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Header,
    Dotted,
    Inline,
}

/// A piece of a generated document. Values are kept apart so tests can
/// rebuild the expected text after an edit.
#[derive(Debug, Clone)]
pub enum Piece {
    Text(String),
    Value { path: &'static str, raw: String },
}

/// A generated settings document.
#[derive(Debug, Clone)]
pub struct Doc {
    pub pieces: Vec<Piece>,
    /// The settings set in the document and their normalized values.
    pub values: Vec<(&'static str, toml::Value)>,
}

impl Doc {
    pub fn text(&self) -> String {
        self.render(None)
    }

    /// The text with the value at `path` replaced by `raw`.
    pub fn render(&self, replace: Option<(&str, &str)>) -> String {
        let mut out = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text(t) => out.push_str(t),
                Piece::Value { path, raw } => match replace {
                    Some((p, r)) if p == *path => out.push_str(r),
                    _ => out.push_str(raw),
                },
            }
        }
        out
    }

    pub fn has(&self, path: &str) -> bool {
        self.values.iter().any(|(p, _)| *p == path)
    }
}

fn comment() -> impl Strategy<Value = String> {
    prop::option::of("[ a-zA-Z0-9äö#'\"=\\[\\]]{0,20}").prop_map(|c| match c {
        Some(c) => format!(" # {c}"),
        None => String::new(),
    })
}

fn spacing() -> impl Strategy<Value = (&'static str, &'static str)> {
    (
        prop::sample::select(vec!["", " ", "  ", "\t"]),
        prop::sample::select(vec!["", " ", "   "]),
    )
}

fn filler() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "",
        "\n",
        "# a comment line\n",
        "\n# two\n# comment lines\n\n",
        "   \n",
    ])
    .prop_map(str::to_owned)
}

type Line = (
    &'static Spec,
    (String, toml::Value),
    String,
    (&'static str, &'static str),
    String,
);

fn line() -> impl Strategy<Value = Line> {
    any_spec().prop_flat_map(|spec| (Just(spec), value_for(spec), comment(), spacing(), filler()))
}

/// A whole document: an optional `version` line, then sections in random
/// styles with comments, blank lines and unknown keys.
pub fn doc() -> impl Strategy<Value = Doc> {
    (
        prop::collection::vec(line(), 0..14),
        prop::collection::vec(
            prop::sample::select(vec![Style::Header, Style::Dotted, Style::Inline]),
            12,
        ),
        any::<bool>(),
        filler(),
        any::<bool>(),
    )
        .prop_map(|(lines, styles, with_version, head, unknown)| {
            build(lines, &styles, with_version, &head, unknown)
        })
}

fn build(lines: Vec<Line>, styles: &[Style], with_version: bool, head: &str, unknown: bool) -> Doc {
    let mut sections: Vec<(&'static str, Vec<Line>)> = Vec::new();
    for line in lines {
        let section = line.0.section;
        let entry = match sections.iter_mut().find(|(s, _)| *s == section) {
            Some(entry) => entry,
            None => {
                sections.push((section, Vec::new()));
                let last = sections.len() - 1;
                &mut sections[last]
            }
        };
        if !entry.1.iter().any(|l| l.0.path == line.0.path) {
            entry.1.push(line);
        }
    }
    let style_of = |i: usize| styles.get(i).copied().unwrap_or(Style::Header);

    let mut pieces = vec![Piece::Text(head.to_owned())];
    let mut values = Vec::new();
    if with_version {
        pieces.push(Piece::Text("version = 1 # file format\n".to_owned()));
    }
    if unknown {
        pieces.push(Piece::Text("future_root_key = \"kept\"\n".to_owned()));
    }
    // Dotted and inline sections are root keys, so they come before headers.
    for (i, (section, lines)) in sections.iter().enumerate() {
        match style_of(i) {
            Style::Dotted => {
                for (spec, (raw, value), comment, (before, after), filler) in lines {
                    pieces.push(Piece::Text(format!(
                        "{section}.{}{before}={after}",
                        spec.key
                    )));
                    pieces.push(Piece::Value {
                        path: spec.path,
                        raw: raw.clone(),
                    });
                    pieces.push(Piece::Text(format!("{comment}\n{filler}")));
                    values.push((spec.path, value.clone()));
                }
            }
            Style::Inline => {
                pieces.push(Piece::Text(format!("{section} = {{ ")));
                for (j, (spec, (raw, value), _, (before, after), _)) in lines.iter().enumerate() {
                    if j > 0 {
                        pieces.push(Piece::Text(", ".to_owned()));
                    }
                    pieces.push(Piece::Text(format!("{}{before}={after}", spec.key)));
                    pieces.push(Piece::Value {
                        path: spec.path,
                        raw: raw.clone(),
                    });
                    values.push((spec.path, value.clone()));
                }
                pieces.push(Piece::Text(" } # inline\n".to_owned()));
            }
            Style::Header => {}
        }
    }
    for (i, (section, lines)) in sections.iter().enumerate() {
        if style_of(i) != Style::Header {
            continue;
        }
        pieces.push(Piece::Text(format!("\n[{section}] # section\n")));
        for (spec, (raw, value), comment, (before, after), filler) in lines {
            pieces.push(Piece::Text(format!("{filler}{}{before}={after}", spec.key)));
            pieces.push(Piece::Value {
                path: spec.path,
                raw: raw.clone(),
            });
            pieces.push(Piece::Text(format!("{comment}\n")));
            values.push((spec.path, value.clone()));
        }
        if unknown {
            pieces.push(Piece::Text(
                "unknown_in_section = [1, 2] # kept\n".to_owned(),
            ));
        }
    }
    Doc { pieces, values }
}

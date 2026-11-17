// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! YAML as plain data: the parsed form of a base before it is read into
//! the model ([`crate::Base`]), and the home of keys the model does not
//! know (kept in document order, so nothing is lost).

use saphyr::{LoadableYamlNode as _, ScalarOwned, YamlOwned};

/// A YAML node. Mapping keys are kept in document order.
#[derive(Debug, Clone, PartialEq)]
pub enum YamlValue {
    /// `null`, `~` or nothing.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A string.
    String(String),
    /// A sequence.
    Seq(Vec<YamlValue>),
    /// A mapping, in document order. Scalar keys are written as text;
    /// complex keys (rare) as their YAML flow form.
    Map(Vec<(String, YamlValue)>),
}

impl YamlValue {
    /// The string, if this is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// A scalar as text (`true`, `3`, `2.5`, the string itself); `None`
    /// for null and collections.
    #[must_use]
    pub fn scalar_text(&self) -> Option<String> {
        match self {
            Self::Bool(b) => Some(b.to_string()),
            Self::Int(i) => Some(i.to_string()),
            Self::Float(f) => Some(format_float(*f)),
            Self::String(s) => Some(s.clone()),
            Self::Null | Self::Seq(_) | Self::Map(_) => None,
        }
    }

    /// A number, if this is one.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            #[allow(clippy::cast_precision_loss)]
            Self::Int(i) => Some(*i as f64),
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// The value of `key` in a mapping.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&YamlValue> {
        match self {
            Self::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The YAML kind, for messages.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Int(_) | Self::Float(_) => "number",
            Self::String(_) => "string",
            Self::Seq(_) => "list",
            Self::Map(_) => "mapping",
        }
    }
}

fn format_float(f: f64) -> String {
    if f.is_nan() {
        ".nan".into()
    } else if f.is_infinite() {
        if f > 0.0 {
            ".inf".into()
        } else {
            "-.inf".into()
        }
    } else {
        f.to_string()
    }
}

/// A YAML syntax error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    /// What is wrong.
    pub message: String,
    /// 1-based line.
    pub line: usize,
    /// 0-based column.
    pub column: usize,
}

impl std::fmt::Display for YamlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (line {}, column {})",
            self.message,
            self.line,
            self.column + 1
        )
    }
}

impl std::error::Error for YamlError {}

/// Parses the first YAML document of `text`. An empty text is `Null`.
///
/// # Errors
///
/// A YAML syntax error.
pub fn parse_yaml(text: &str) -> Result<YamlValue, YamlError> {
    let docs = YamlOwned::load_from_str(text).map_err(|e| YamlError {
        message: e.info().to_owned(),
        line: e.marker().line(),
        column: e.marker().col(),
    })?;
    Ok(docs.into_iter().next().map_or(YamlValue::Null, convert))
}

fn convert(node: YamlOwned) -> YamlValue {
    match node {
        YamlOwned::Value(scalar) => convert_scalar(scalar),
        YamlOwned::Representation(text, _, _) => YamlValue::String(text),
        YamlOwned::Sequence(items) => YamlValue::Seq(items.into_iter().map(convert).collect()),
        YamlOwned::Mapping(map) => YamlValue::Map(
            map.into_iter()
                .map(|(k, v)| (key_text(k), convert(v)))
                .collect(),
        ),
        YamlOwned::Tagged(_, inner) => convert(*inner),
        YamlOwned::Alias(_) | YamlOwned::BadValue => YamlValue::Null,
    }
}

fn convert_scalar(scalar: ScalarOwned) -> YamlValue {
    match scalar {
        ScalarOwned::Null => YamlValue::Null,
        ScalarOwned::Boolean(b) => YamlValue::Bool(b),
        ScalarOwned::Integer(i) => YamlValue::Int(i),
        ScalarOwned::FloatingPoint(f) => YamlValue::Float(f.0),
        ScalarOwned::String(s) => YamlValue::String(s),
    }
}

fn key_text(key: YamlOwned) -> String {
    match convert(key) {
        YamlValue::Null => "null".into(),
        v @ (YamlValue::Bool(_)
        | YamlValue::Int(_)
        | YamlValue::Float(_)
        | YamlValue::String(_)) => v.scalar_text().unwrap_or_default(),
        other => flow(&other),
    }
}

/// A compact YAML flow form, for complex keys and messages.
#[must_use]
pub fn flow(value: &YamlValue) -> String {
    match value {
        YamlValue::Null => "null".into(),
        YamlValue::String(s) => format!("{s:?}"),
        YamlValue::Seq(items) => {
            let parts: Vec<String> = items.iter().map(flow).collect();
            format!("[{}]", parts.join(", "))
        }
        YamlValue::Map(entries) => {
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{k}: {}", flow(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        other => other.scalar_text().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_and_order() {
        let v = parse_yaml("b: 1\na: true\nc: 2.5\nd: ~\ne: 'x'\nf: [1, two]\n").expect("yaml");
        let YamlValue::Map(entries) = &v else {
            panic!("map expected");
        };
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["b", "a", "c", "d", "e", "f"]);
        assert_eq!(v.get("b"), Some(&YamlValue::Int(1)));
        assert_eq!(v.get("a"), Some(&YamlValue::Bool(true)));
        assert_eq!(v.get("c"), Some(&YamlValue::Float(2.5)));
        assert_eq!(v.get("d"), Some(&YamlValue::Null));
        assert_eq!(v.get("e"), Some(&YamlValue::String("x".into())));
        assert_eq!(
            v.get("f"),
            Some(&YamlValue::Seq(vec![
                YamlValue::Int(1),
                YamlValue::String("two".into())
            ]))
        );
    }

    #[test]
    fn empty_and_errors() {
        assert_eq!(parse_yaml("").expect("empty"), YamlValue::Null);
        let err = parse_yaml("a: [1, 2\nb: 3").expect_err("unclosed");
        assert!(err.line >= 1);
    }

    #[test]
    fn non_string_keys() {
        let v = parse_yaml("1: one\ntrue: yes\n").expect("yaml");
        assert_eq!(v.get("1"), Some(&YamlValue::String("one".into())));
        assert!(v.get("true").is_some());
    }
}

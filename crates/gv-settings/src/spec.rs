// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Per-setting metadata: value kind, range, default, scope and the strictness
//! order of security-relevant settings. The registry built from
//! [`crate::schema`] is the single source for validation, layering and the
//! JSON Schema.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::OnceLock;

use toml::Value;

/// The shape of a setting's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// `true` or `false`.
    Bool,
    /// A whole number in `min..=max`.
    Integer { min: i64, max: i64 },
    /// A number in `min..=max`. Integers are accepted and widened.
    Float { min: f64, max: f64 },
    /// Any string.
    String,
    /// An array of strings.
    StringList,
    /// One of the listed strings. For security-relevant enums the list runs
    /// from the least to the most strict value.
    Enum(&'static [&'static str]),
}

impl Kind {
    /// A phrase for error messages, such as "an integer from 40 to 240".
    pub fn expected(&self) -> String {
        match *self {
            Self::Bool => "a boolean (true or false)".to_owned(),
            Self::Integer { min, max } => format!("an integer from {min} to {max}"),
            Self::Float { min, max } => format!("a number from {min} to {max}"),
            Self::String => "a string".to_owned(),
            Self::StringList => "an array of strings".to_owned(),
            Self::Enum(variants) => {
                let list: Vec<String> = variants.iter().map(|v| format!("\"{v}\"")).collect();
                format!("one of {}", list.join(", "))
            }
        }
    }
}

/// Implemented by every Rust type a setting can have.
pub trait SettingType {
    /// The value kind before any range narrowing.
    const KIND: Kind;
}

impl SettingType for bool {
    const KIND: Kind = Kind::Bool;
}

impl SettingType for u32 {
    const KIND: Kind = Kind::Integer {
        min: 0,
        max: u32::MAX as i64,
    };
}

impl SettingType for u64 {
    const KIND: Kind = Kind::Integer {
        min: 0,
        max: i64::MAX,
    };
}

impl SettingType for f32 {
    const KIND: Kind = Kind::Float {
        min: f32::MIN as f64,
        max: f32::MAX as f64,
    };
}

impl SettingType for String {
    const KIND: Kind = Kind::String;
}

impl SettingType for Vec<String> {
    const KIND: Kind = Kind::StringList;
}

/// The strictness order of a security-relevant setting. Values read from
/// in-vault files are applied only when the result is at least as strict as
/// the value below them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tighten {
    /// `true` is stricter than `false`.
    TrueIsStricter,
    /// `false` is stricter than `true`.
    FalseIsStricter,
    /// For enums: a value later in the variant list is stricter.
    LaterVariantIsStricter,
    /// For numbers: a lower value is stricter.
    LowerIsStricter,
    /// For numbers: a higher value is stricter.
    HigherIsStricter,
    /// For lists: a subset is stricter (an allow list).
    Subset,
    /// For lists: a superset is stricter (a block list).
    Superset,
}

impl Tighten {
    /// Whether `candidate` is at least as strict as `base`. Values of the wrong
    /// shape are never "at least as strict", so a malformed value can't relax a
    /// setting.
    pub fn at_least_as_strict(self, kind: &Kind, candidate: &Value, base: &Value) -> bool {
        match self {
            Self::TrueIsStricter => match (candidate.as_bool(), base.as_bool()) {
                (Some(c), Some(b)) => c || !b,
                _ => false,
            },
            Self::FalseIsStricter => match (candidate.as_bool(), base.as_bool()) {
                (Some(c), Some(b)) => !c || b,
                _ => false,
            },
            Self::LaterVariantIsStricter => {
                let Kind::Enum(variants) = kind else {
                    return false;
                };
                let index = |v: &Value| {
                    v.as_str()
                        .and_then(|s| variants.iter().position(|variant| *variant == s))
                };
                match (index(candidate), index(base)) {
                    (Some(c), Some(b)) => c >= b,
                    _ => false,
                }
            }
            Self::LowerIsStricter => match (number(candidate), number(base)) {
                (Some(c), Some(b)) => c <= b,
                _ => false,
            },
            Self::HigherIsStricter => match (number(candidate), number(base)) {
                (Some(c), Some(b)) => c >= b,
                _ => false,
            },
            Self::Subset => match (strings(candidate), strings(base)) {
                (Some(c), Some(b)) => c.iter().all(|item| b.contains(item)),
                _ => false,
            },
            Self::Superset => match (strings(candidate), strings(base)) {
                (Some(c), Some(b)) => b.iter().all(|item| c.contains(item)),
                _ => false,
            },
        }
    }
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn strings(value: &Value) -> Option<Vec<&str>> {
    value.as_array()?.iter().map(Value::as_str).collect()
}

/// Which layers may set a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Any layer: global, per vault, per file.
    Any,
    /// Only the global settings file. Vault layers that set it get a warning.
    GlobalOnly,
}

/// How a layer's value combines with the value below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// The higher layer's value replaces the lower one.
    Replace,
    /// For lists: the union of both, lower items first.
    Union,
}

/// Metadata for one setting.
#[derive(Debug, Clone)]
pub struct Spec {
    /// The section table, such as `appearance`.
    pub section: &'static str,
    /// The key within the section, such as `theme`.
    pub key: &'static str,
    /// The dotted path, such as `appearance.theme`.
    pub path: &'static str,
    /// The documentation, one paragraph.
    pub doc: String,
    /// The value kind, with any range applied.
    pub kind: Kind,
    /// The built-in default.
    pub default: Value,
    /// Set for security-relevant settings: the order that in-vault values
    /// must respect.
    pub tighten: Option<Tighten>,
    /// For security-relevant settings: a value that, when the global layer
    /// sets it, no vault layer may change (remote content `block`).
    pub absolute_in_global: Option<&'static str>,
    /// Which layers may set it.
    pub scope: Scope,
    /// How layers combine.
    pub merge: Merge,
}

impl Spec {
    pub(crate) fn new(
        section: &'static str,
        key: &'static str,
        path: &'static str,
        doc: &[&str],
        kind: Kind,
        default: Value,
    ) -> Self {
        let doc = doc
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            section,
            key,
            path,
            doc,
            kind,
            default,
            tighten: None,
            absolute_in_global: None,
            scope: Scope::Any,
            merge: Merge::Replace,
        }
    }

    /// Narrows a numeric kind to `min..=max`.
    pub(crate) fn range(mut self, min: impl Into<f64>, max: impl Into<f64>) -> Self {
        let (min, max) = (min.into(), max.into());
        self.kind = match self.kind {
            Kind::Integer { .. } => Kind::Integer {
                min: min as i64,
                max: max as i64,
            },
            Kind::Float { .. } => Kind::Float { min, max },
            other => other,
        };
        self
    }

    /// Marks the setting security-relevant with the given order.
    pub(crate) fn security(mut self, tighten: Tighten) -> Self {
        self.tighten = Some(tighten);
        self
    }

    pub(crate) fn absolute_in_global(mut self, value: &'static str) -> Self {
        self.absolute_in_global = Some(value);
        self
    }

    pub(crate) fn global_only(mut self) -> Self {
        self.scope = Scope::GlobalOnly;
        self
    }

    pub(crate) fn union(mut self) -> Self {
        self.merge = Merge::Union;
        self
    }

    /// Whether in-vault files may only tighten this setting.
    pub fn is_security_relevant(&self) -> bool {
        self.tighten.is_some()
    }

    /// Checks a parsed TOML value and converts it to the normalized form
    /// (integers widened to floats for number settings).
    pub fn check(&self, value: &toml_edit::Value) -> Result<Value, ValueError> {
        let mismatch = || ValueError {
            message: format!(
                "expected {}, found {}",
                self.kind.expected(),
                describe(value)
            ),
            span: value.span(),
        };
        match self.kind {
            Kind::Bool => value.as_bool().map(Value::Boolean).ok_or_else(mismatch),
            Kind::Integer { min, max } => {
                let i = value.as_integer().ok_or_else(mismatch)?;
                if (min..=max).contains(&i) {
                    Ok(Value::Integer(i))
                } else {
                    Err(mismatch())
                }
            }
            Kind::Float { min, max } => {
                let f = match value {
                    toml_edit::Value::Float(f) => *f.value(),
                    toml_edit::Value::Integer(i) => *i.value() as f64,
                    _ => return Err(mismatch()),
                };
                if f.is_finite() && (min..=max).contains(&f) {
                    Ok(Value::Float(f))
                } else {
                    Err(mismatch())
                }
            }
            Kind::String => value
                .as_str()
                .map(|s| Value::String(s.to_owned()))
                .ok_or_else(mismatch),
            Kind::StringList => {
                let array = value.as_array().ok_or_else(mismatch)?;
                let mut items = Vec::with_capacity(array.len());
                for (index, item) in array.iter().enumerate() {
                    match item.as_str() {
                        Some(s) => items.push(Value::String(s.to_owned())),
                        None => {
                            return Err(ValueError {
                                message: format!(
                                    "expected {}, but item {} is {}",
                                    self.kind.expected(),
                                    index + 1,
                                    describe(item)
                                ),
                                span: item.span().or_else(|| value.span()),
                            });
                        }
                    }
                }
                Ok(Value::Array(items))
            }
            Kind::Enum(variants) => {
                let s = value.as_str().ok_or_else(mismatch)?;
                if variants.contains(&s) {
                    Ok(Value::String(s.to_owned()))
                } else {
                    Err(mismatch())
                }
            }
        }
    }

    /// Checks an owned value, as used for programmatic edits.
    pub fn check_owned(&self, value: &Value) -> Result<Value, ValueError> {
        let text = value.to_string();
        let parsed: toml_edit::Value = text.parse().map_err(|_| ValueError {
            message: format!(
                "expected {}, found an unsupported value",
                self.kind.expected()
            ),
            span: None,
        })?;
        self.check(&parsed).map_err(|e| ValueError {
            message: e.message,
            span: None,
        })
    }
}

/// A value that does not match its setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError {
    /// "expected …, found …".
    pub message: String,
    /// The byte range of the offending value in its file, when known.
    pub span: Option<Range<usize>>,
}

/// "the string \"blue\"", "the integer 500", …, for error messages.
pub(crate) fn describe(value: &toml_edit::Value) -> String {
    fn clip(text: &str) -> String {
        const MAX: usize = 40;
        if text.chars().count() > MAX {
            let head: String = text.chars().take(MAX).collect();
            format!("{head}…")
        } else {
            text.to_owned()
        }
    }
    match value {
        toml_edit::Value::String(s) => format!("the string {}", clip(&format!("{:?}", s.value()))),
        toml_edit::Value::Integer(i) => format!("the integer {}", i.value()),
        toml_edit::Value::Float(f) => format!("the number {}", f.value()),
        toml_edit::Value::Boolean(b) => format!("the boolean {}", b.value()),
        toml_edit::Value::Datetime(d) => format!("the date-time {}", d.value()),
        toml_edit::Value::Array(_) => "an array".to_owned(),
        toml_edit::Value::InlineTable(_) => "a table".to_owned(),
    }
}

/// One section of the settings file.
#[derive(Debug, Clone)]
pub struct Section {
    /// The table name, such as `privacy_and_security`.
    pub name: &'static str,
    /// The documentation, one paragraph.
    pub doc: String,
}

pub(crate) struct Registry {
    pub(crate) specs: Vec<Spec>,
    pub(crate) sections: Vec<Section>,
    by_path: HashMap<&'static str, usize>,
}

impl Registry {
    fn get() -> &'static Self {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let (sections, specs) = crate::schema::build_registry();
            let by_path = specs.iter().enumerate().map(|(i, s)| (s.path, i)).collect();
            Registry {
                specs,
                sections,
                by_path,
            }
        })
    }
}

/// Every setting, in section and declaration order.
pub fn specs() -> &'static [Spec] {
    &Registry::get().specs
}

/// The setting at a dotted path such as `appearance.theme`.
pub fn spec(path: &str) -> Option<&'static Spec> {
    let registry = Registry::get();
    registry.by_path.get(path).map(|&i| &registry.specs[i])
}

/// The sections, in the order the settings window lists them.
pub fn sections() -> &'static [Section] {
    &Registry::get().sections
}

/// Whether `name` is a section table.
pub(crate) fn is_section(name: &str) -> bool {
    sections().iter().any(|s| s.name == name)
}

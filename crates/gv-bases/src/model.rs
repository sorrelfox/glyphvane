// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The base model: what a `.base` file or a `base` code block says.
//!
//! Top-level keys: `filters`, `formulas`, `properties`, `summaries`,
//! `views`. Every key the model does not know, at the top level, in
//! property settings and in views, is kept in [`Base::extra`],
//! [`PropertyConfig::extra`] and [`View::extra`] in document order.
//!
//! Reading is lenient: a key with the wrong shape is reported in
//! [`Base::diagnostics`], kept as an unknown key, and the rest of the
//! base still loads. Only invalid YAML, or a document that is not a
//! mapping, fails ([`BaseError`]).

use std::fmt;

use crate::yaml::{YamlError, YamlValue, flow, parse_yaml};

/// A property a base refers to: `note.x` (or bare `x`), `file.x` or
/// `formula.x`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PropertyId {
    /// A front matter property of the note.
    Note(String),
    /// A file property (`name`, `path`, `mtime`, ...).
    File(String),
    /// A formula of the base.
    Formula(String),
}

impl PropertyId {
    /// Reads `note.status`, `status`, `file.name` or `formula.ppu`.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix("note.") {
            Self::Note(rest.to_owned())
        } else if let Some(rest) = text.strip_prefix("file.") {
            Self::File(rest.to_owned())
        } else if let Some(rest) = text.strip_prefix("formula.") {
            Self::Formula(rest.to_owned())
        } else {
            Self::Note(text.to_owned())
        }
    }

    /// The name without the namespace.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Note(n) | Self::File(n) | Self::Formula(n) => n,
        }
    }

    /// The default column label: the note property's name, `file name`
    /// for `file.name`, the formula's name.
    #[must_use]
    pub fn default_label(&self) -> String {
        match self {
            Self::Note(n) | Self::Formula(n) => n.clone(),
            Self::File(n) => format!("file {n}"),
        }
    }
}

impl fmt::Display for PropertyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Note(n) => write!(f, "note.{n}"),
            Self::File(n) => write!(f, "file.{n}"),
            Self::Formula(n) => write!(f, "formula.{n}"),
        }
    }
}

/// A filter: a statement, or `and`, `or`, `not` over a list of filters.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// An expression; the file passes when it is truthy.
    Expr(String),
    /// Every item passes.
    And(Vec<Filter>),
    /// At least one item passes.
    Or(Vec<Filter>),
    /// No item passes.
    Not(Vec<Filter>),
    /// Not a filter (reported in [`Base::diagnostics`]); excludes every
    /// file, so a broken filter never shows more than it should.
    Invalid(YamlValue),
}

/// Display settings of one property (`properties:`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PropertyConfig {
    /// `displayName`: the column header. Not used in expressions.
    pub display_name: Option<String>,
    /// Other keys, in order.
    pub extra: Vec<(String, YamlValue)>,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Smallest first.
    #[default]
    Asc,
    /// Largest first.
    Desc,
}

impl Direction {
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_uppercase().as_str() {
            "ASC" => Some(Self::Asc),
            "DESC" => Some(Self::Desc),
            _ => None,
        }
    }
}

/// One sort key of a view.
#[derive(Debug, Clone, PartialEq)]
pub struct SortKey {
    /// The property sorted by.
    pub property: PropertyId,
    /// The direction.
    pub direction: Direction,
}

/// The layout of a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewKind {
    /// Rows and columns.
    Table,
    /// A grid of cards.
    Cards,
    /// A list.
    List,
    /// A map (not supported: the view shows a notice).
    Map,
    /// A kanban board (not supported: the view shows a notice).
    Kanban,
    /// A layout from a plugin (not supported: the view shows a notice).
    Other(String),
}

impl ViewKind {
    fn parse(text: &str) -> Self {
        match text {
            "table" => Self::Table,
            "cards" => Self::Cards,
            "list" => Self::List,
            "map" => Self::Map,
            "kanban" => Self::Kanban,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Whether Glyphvane draws this layout.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Table | Self::Cards | Self::List)
    }

    /// The name as written in YAML.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Table => "table",
            Self::Cards => "cards",
            Self::List => "list",
            Self::Map => "map",
            Self::Kanban => "kanban",
            Self::Other(s) => s,
        }
    }
}

/// How the cover image fills a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFit {
    /// Fills the box, cropping overflow.
    #[default]
    Cover,
    /// Scales to fit, without cropping.
    Contain,
}

/// `groupBy`.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupBy {
    /// The property whose value groups the rows.
    pub property: PropertyId,
    /// Group order.
    pub direction: Direction,
}

/// One view.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// `type`.
    pub kind: ViewKind,
    /// `name`.
    pub name: Option<String>,
    /// View filters, combined with the base's filters by AND.
    pub filters: Option<Filter>,
    /// `order`: the columns (table) or shown properties (cards, list).
    pub order: Vec<PropertyId>,
    /// `sort`: sort keys, highest priority first.
    pub sort: Vec<SortKey>,
    /// `groupBy`.
    pub group_by: Option<GroupBy>,
    /// `groupOrder`: only these groups, in this order (`null` is the
    /// group of files without a value). `None`: every group.
    pub group_order: Option<Vec<YamlValue>>,
    /// `limit`: the most rows shown.
    pub limit: Option<usize>,
    /// `summaries`: property → summary name (`Average`, or a custom one).
    pub summaries: Vec<(PropertyId, String)>,
    /// `columnSize`: column widths in pixels, as written.
    pub column_size: Vec<(PropertyId, f64)>,
    /// Cards: `image`, the cover image property.
    pub image: Option<PropertyId>,
    /// Cards: `imageFit`.
    pub image_fit: Option<ImageFit>,
    /// Cards: `imageAspectRatio` (height / width).
    pub image_aspect_ratio: Option<f64>,
    /// Cards: `cardSize` (width in pixels).
    pub card_size: Option<f64>,
    /// Other keys, in order.
    pub extra: Vec<(String, YamlValue)>,
}

impl View {
    /// A table of file names: the view of a base without `views`.
    #[must_use]
    pub fn default_table() -> Self {
        Self {
            kind: ViewKind::Table,
            name: None,
            filters: None,
            order: vec![PropertyId::File("name".into())],
            sort: Vec::new(),
            group_by: None,
            group_order: None,
            limit: None,
            summaries: Vec::new(),
            column_size: Vec::new(),
            image: None,
            image_fit: None,
            image_aspect_ratio: None,
            card_size: None,
            extra: Vec::new(),
        }
    }
}

/// A problem in the base that did not stop it from loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDiagnostic {
    /// Where (`views[0].limit`).
    pub at: String,
    /// What.
    pub message: String,
}

/// Why a base could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseError {
    /// Invalid YAML.
    Yaml(YamlError),
    /// The document is not a mapping.
    NotAMapping(&'static str),
}

impl fmt::Display for BaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml(e) => write!(f, "invalid YAML: {e}"),
            Self::NotAMapping(kind) => write!(f, "a base must be a YAML mapping, not a {kind}"),
        }
    }
}

impl std::error::Error for BaseError {}

/// A base.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Base {
    /// Filters of every view.
    pub filters: Option<Filter>,
    /// `formulas`: name → expression, in order.
    pub formulas: Vec<(String, String)>,
    /// `properties`: display settings, in order.
    pub properties: Vec<(PropertyId, PropertyConfig)>,
    /// `summaries`: custom summary formulas (name → expression over
    /// `values`), in order.
    pub summaries: Vec<(String, String)>,
    /// `views`, in order. The first is the default view.
    pub views: Vec<View>,
    /// Unknown top-level keys, in order.
    pub extra: Vec<(String, YamlValue)>,
    /// Problems found while reading.
    pub diagnostics: Vec<ModelDiagnostic>,
}

impl Base {
    /// Reads the text of a `.base` file or of a `base` code block.
    ///
    /// # Errors
    ///
    /// Invalid YAML, or a document that is not a mapping (an empty
    /// document is an empty base).
    pub fn parse(text: &str) -> Result<Self, BaseError> {
        let yaml = parse_yaml(text).map_err(BaseError::Yaml)?;
        Self::from_yaml(yaml)
    }

    /// Reads parsed YAML.
    ///
    /// # Errors
    ///
    /// The document is not a mapping (null is an empty base).
    pub fn from_yaml(yaml: YamlValue) -> Result<Self, BaseError> {
        let entries = match yaml {
            YamlValue::Null => Vec::new(),
            YamlValue::Map(entries) => entries,
            other => return Err(BaseError::NotAMapping(other.kind())),
        };
        let mut base = Self::default();
        let mut diags = Vec::new();
        for (key, value) in entries {
            match key.as_str() {
                "filters" => base.filters = read_filter(&value, "filters", &mut diags),
                "formulas" => {
                    base.formulas = read_string_map(&value, "formulas", &mut diags);
                }
                "summaries" => {
                    base.summaries = read_string_map(&value, "summaries", &mut diags);
                }
                "properties" => base.properties = read_properties(value, &mut diags),
                "views" => match value {
                    YamlValue::Seq(items) => {
                        for (i, item) in items.into_iter().enumerate() {
                            if let Some(view) = read_view(item, i, &mut diags) {
                                base.views.push(view);
                            }
                        }
                    }
                    YamlValue::Null => {}
                    other => {
                        diags.push(diag(
                            "views",
                            format!("expected a list, found a {}", other.kind()),
                        ));
                        base.extra.push((key, other));
                    }
                },
                _ => base.extra.push((key, value)),
            }
        }
        base.diagnostics = diags;
        Ok(base)
    }

    /// The index of the view named `name` (embeds: `![[File.base#View]]`).
    #[must_use]
    pub fn view_index(&self, name: &str) -> Option<usize> {
        self.views
            .iter()
            .position(|v| v.name.as_deref() == Some(name))
    }

    /// The display settings of `property`.
    #[must_use]
    pub fn property_config(&self, property: &PropertyId) -> Option<&PropertyConfig> {
        self.properties
            .iter()
            .find(|(p, _)| p == property)
            .map(|(_, c)| c)
    }

    /// The label of `property`: its `displayName`, else the default.
    #[must_use]
    pub fn label(&self, property: &PropertyId) -> String {
        self.property_config(property)
            .and_then(|c| c.display_name.clone())
            .unwrap_or_else(|| property.default_label())
    }

    /// The formula named `name`.
    #[must_use]
    pub fn formula(&self, name: &str) -> Option<&str> {
        self.formulas
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, f)| f.as_str())
    }
}

fn diag(at: impl Into<String>, message: impl Into<String>) -> ModelDiagnostic {
    ModelDiagnostic {
        at: at.into(),
        message: message.into(),
    }
}

/// Reads a filter. `None` for null.
fn read_filter(value: &YamlValue, at: &str, diags: &mut Vec<ModelDiagnostic>) -> Option<Filter> {
    match value {
        YamlValue::Null => None,
        YamlValue::String(s) => Some(Filter::Expr(s.clone())),
        YamlValue::Bool(_) | YamlValue::Int(_) | YamlValue::Float(_) => {
            Some(Filter::Expr(value.scalar_text().unwrap_or_default()))
        }
        YamlValue::Map(entries) if entries.len() == 1 => {
            let (key, items) = &entries[0];
            let make: fn(Vec<Filter>) -> Filter = match key.as_str() {
                "and" => Filter::And,
                "or" => Filter::Or,
                "not" => Filter::Not,
                _ => {
                    diags.push(diag(
                        at,
                        format!("expected `and`, `or` or `not`, found `{key}`"),
                    ));
                    return Some(Filter::Invalid(value.clone()));
                }
            };
            let list = match items {
                YamlValue::Seq(list) => list.as_slice(),
                YamlValue::Null => &[],
                // One statement instead of a list.
                single => std::slice::from_ref(single),
            };
            let children = list
                .iter()
                .enumerate()
                .filter_map(|(i, item)| read_filter(item, &format!("{at}.{key}[{i}]"), diags))
                .collect();
            Some(make(children))
        }
        other => {
            diags.push(diag(at, format!("not a filter: {}", flow(other))));
            Some(Filter::Invalid(other.clone()))
        }
    }
}

fn read_string_map(
    value: &YamlValue,
    at: &str,
    diags: &mut Vec<ModelDiagnostic>,
) -> Vec<(String, String)> {
    match value {
        YamlValue::Map(entries) => entries
            .iter()
            .filter_map(|(k, v)| match v.scalar_text() {
                Some(text) => Some((k.clone(), text)),
                None if *v == YamlValue::Null => Some((k.clone(), String::new())),
                None => {
                    diags.push(diag(
                        format!("{at}.{k}"),
                        format!("expected an expression, found a {}", v.kind()),
                    ));
                    None
                }
            })
            .collect(),
        YamlValue::Null => Vec::new(),
        other => {
            diags.push(diag(
                at,
                format!("expected a mapping, found a {}", other.kind()),
            ));
            Vec::new()
        }
    }
}

fn read_properties(
    value: YamlValue,
    diags: &mut Vec<ModelDiagnostic>,
) -> Vec<(PropertyId, PropertyConfig)> {
    let entries = match value {
        YamlValue::Map(entries) => entries,
        YamlValue::Null => return Vec::new(),
        other => {
            diags.push(diag(
                "properties",
                format!("expected a mapping, found a {}", other.kind()),
            ));
            return Vec::new();
        }
    };
    entries
        .into_iter()
        .map(|(key, settings)| {
            let mut config = PropertyConfig::default();
            match settings {
                YamlValue::Map(fields) => {
                    for (field, v) in fields {
                        if field == "displayName" {
                            match v.scalar_text() {
                                Some(text) => config.display_name = Some(text),
                                None => config.extra.push((field, v)),
                            }
                        } else {
                            config.extra.push((field, v));
                        }
                    }
                }
                YamlValue::Null => {}
                other => {
                    diags.push(diag(
                        format!("properties.{key}"),
                        format!("expected a mapping, found a {}", other.kind()),
                    ));
                }
            }
            (PropertyId::parse(&key), config)
        })
        .collect()
}

fn read_view(value: YamlValue, index: usize, diags: &mut Vec<ModelDiagnostic>) -> Option<View> {
    let at = format!("views[{index}]");
    let YamlValue::Map(entries) = value else {
        diags.push(diag(
            at,
            format!("expected a mapping, found a {}", value.kind()),
        ));
        return None;
    };
    let mut view = View::default_table();
    view.order.clear();
    let mut has_type = false;
    for (key, value) in entries {
        let here = format!("{at}.{key}");
        let mut keep = false;
        match key.as_str() {
            "type" => match value.scalar_text() {
                Some(t) => {
                    view.kind = ViewKind::parse(&t);
                    has_type = true;
                }
                None => keep = true,
            },
            "name" => match value.scalar_text() {
                Some(n) => view.name = Some(n),
                None => keep = value != YamlValue::Null,
            },
            "filters" => view.filters = read_filter(&value, &here, diags),
            "order" => match &value {
                YamlValue::Seq(items) => {
                    view.order = items
                        .iter()
                        .filter_map(YamlValue::scalar_text)
                        .map(|s| PropertyId::parse(&s))
                        .collect();
                }
                YamlValue::Null => {}
                _ => keep = true,
            },
            "sort" => match &value {
                YamlValue::Seq(items) => {
                    for (i, item) in items.iter().enumerate() {
                        match read_sort_key(item) {
                            Some(k) => view.sort.push(k),
                            None => diags.push(diag(
                                format!("{here}[{i}]"),
                                "expected `property` and `direction`",
                            )),
                        }
                    }
                }
                YamlValue::Null => {}
                _ => keep = true,
            },
            "groupBy" => match read_sort_key(&value) {
                Some(k) => {
                    view.group_by = Some(GroupBy {
                        property: k.property,
                        direction: k.direction,
                    });
                }
                None => keep = value != YamlValue::Null,
            },
            "groupOrder" => match value {
                YamlValue::Seq(ref items) => view.group_order = Some(items.clone()),
                YamlValue::Null => {}
                _ => keep = true,
            },
            "limit" => match value.as_f64() {
                Some(n) if n >= 0.0 => {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let n = n.min(usize::MAX as f64) as usize;
                    view.limit = Some(n);
                }
                _ => keep = value != YamlValue::Null,
            },
            "summaries" => match &value {
                YamlValue::Map(fields) => {
                    for (prop, name) in fields {
                        if let Some(name) = name.scalar_text() {
                            view.summaries.push((PropertyId::parse(prop), name));
                        }
                    }
                }
                YamlValue::Null => {}
                _ => keep = true,
            },
            "columnSize" => match &value {
                YamlValue::Map(fields) => {
                    for (prop, size) in fields {
                        if let Some(px) = size.as_f64() {
                            view.column_size.push((PropertyId::parse(prop), px));
                        }
                    }
                }
                _ => keep = true,
            },
            "image" => match value.scalar_text() {
                Some(p) => view.image = Some(PropertyId::parse(&p)),
                None => keep = value != YamlValue::Null,
            },
            "imageFit" => match value.as_str() {
                Some("cover") => view.image_fit = Some(ImageFit::Cover),
                Some("contain") => view.image_fit = Some(ImageFit::Contain),
                _ => keep = true,
            },
            "imageAspectRatio" => match value.as_f64() {
                Some(r) => view.image_aspect_ratio = Some(r),
                None => keep = true,
            },
            "cardSize" => match value.as_f64() {
                Some(r) => view.card_size = Some(r),
                None => keep = true,
            },
            _ => {
                view.extra.push((key, value));
                continue;
            }
        }
        if keep {
            diags.push(diag(here, format!("unexpected {}", value.kind())));
            view.extra.push((key, value));
        }
    }
    if !has_type {
        diags.push(diag(at, "no `type`: shown as a table"));
    }
    Some(view)
}

fn read_sort_key(value: &YamlValue) -> Option<SortKey> {
    let property = value.get("property")?.scalar_text()?;
    let direction = match value.get("direction") {
        None | Some(YamlValue::Null) => Direction::Asc,
        Some(d) => Direction::parse(&d.scalar_text()?)?,
    };
    Some(SortKey {
        property: PropertyId::parse(&property),
        direction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../tests/fixtures/sample.base");

    #[test]
    fn property_ids() {
        assert_eq!(
            PropertyId::parse("status"),
            PropertyId::Note("status".into())
        );
        assert_eq!(
            PropertyId::parse("note.age"),
            PropertyId::Note("age".into())
        );
        assert_eq!(
            PropertyId::parse("file.ext"),
            PropertyId::File("ext".into())
        );
        assert_eq!(
            PropertyId::parse("formula.ppu"),
            PropertyId::Formula("ppu".into())
        );
        assert_eq!(PropertyId::parse("formula.ppu").to_string(), "formula.ppu");
        assert_eq!(PropertyId::File("name".into()).default_label(), "file name");
    }

    #[test]
    fn sample_fixture() {
        let base = Base::parse(SAMPLE).expect("base");
        let Some(Filter::Or(items)) = &base.filters else {
            panic!("or filter expected: {:?}", base.filters);
        };
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], Filter::Expr("file.hasTag(\"project\")".into()));
        assert!(matches!(&items[1], Filter::And(v) if v.len() == 2));
        assert!(matches!(&items[2], Filter::Not(v) if v.len() == 2));
        assert_eq!(base.formulas.len(), 2);
        assert_eq!(base.formula("per_page"), Some("(price / pages).toFixed(2)"));
        assert_eq!(
            base.label(&PropertyId::parse("formula.price_label")),
            "Price"
        );
        assert_eq!(base.label(&PropertyId::parse("note.status")), "Status");
        assert_eq!(
            base.summaries,
            [("mean3".into(), "values.mean().round(3)".into())]
        );
        let view = &base.views[0];
        assert_eq!(view.kind, ViewKind::Table);
        assert_eq!(view.name.as_deref(), Some("Reading"));
        assert_eq!(view.limit, Some(10));
        assert_eq!(
            view.group_by,
            Some(GroupBy {
                property: PropertyId::Note("pages".into()),
                direction: Direction::Desc
            })
        );
        assert_eq!(view.order.len(), 5);
        assert_eq!(
            view.summaries,
            [(PropertyId::Formula("per_page".into()), "Average".into())]
        );
        assert!(base.diagnostics.is_empty(), "{:?}", base.diagnostics);
        assert_eq!(base.view_index("Reading"), Some(0));
    }

    #[test]
    fn unknown_keys_are_kept() {
        let base = Base::parse(
            "zeta: 1\nfilters: 'x > 1'\nalpha: {a: b}\nproperties:\n  p:\n    displayName: P\n    width: 3\nviews:\n  - type: map\n    name: M\n    center: [1, 2]\n    zoom: 4\n",
        )
        .expect("base");
        let keys: Vec<&str> = base.extra.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["zeta", "alpha"]);
        let (_, config) = &base.properties[0];
        assert_eq!(config.extra, [("width".into(), YamlValue::Int(3))]);
        let view = &base.views[0];
        assert_eq!(view.kind, ViewKind::Map);
        assert!(!view.kind.is_supported());
        let keys: Vec<&str> = view.extra.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["center", "zoom"]);
    }

    #[test]
    fn wrong_shapes_are_diagnosed_not_fatal() {
        let base = Base::parse(
            "filters:\n  nand: [a]\nformulas: [1]\nviews:\n  - 3\n  - type: table\n    limit: lots\n    sort:\n      - property: file.name\n        direction: sideways\n",
        )
        .expect("base");
        assert!(matches!(base.filters, Some(Filter::Invalid(_))));
        assert!(base.formulas.is_empty());
        assert_eq!(base.views.len(), 1);
        assert_eq!(base.views[0].limit, None);
        assert_eq!(base.views[0].extra[0].0, "limit");
        assert!(base.views[0].sort.is_empty());
        assert!(base.diagnostics.len() >= 4, "{:?}", base.diagnostics);
    }

    #[test]
    fn not_a_mapping() {
        assert!(matches!(
            Base::parse("- a\n- b"),
            Err(BaseError::NotAMapping("list"))
        ));
        assert!(matches!(Base::parse("a: [b"), Err(BaseError::Yaml(_))));
        assert_eq!(Base::parse("").expect("empty"), Base::default());
    }

    #[test]
    fn cards_settings_and_sort() {
        let base = Base::parse(
            "views:\n  - type: cards\n    image: note.cover\n    imageFit: contain\n    imageAspectRatio: 0.75\n    cardSize: 220\n    sort:\n      - property: file.mtime\n        direction: DESC\n      - property: title\n    columnSize:\n      file.name: 180\n    groupOrder: [Done, null]\n",
        )
        .expect("base");
        let v = &base.views[0];
        assert_eq!(v.kind, ViewKind::Cards);
        assert_eq!(v.image, Some(PropertyId::Note("cover".into())));
        assert_eq!(v.image_fit, Some(ImageFit::Contain));
        assert_eq!(v.image_aspect_ratio, Some(0.75));
        assert_eq!(v.card_size, Some(220.0));
        assert_eq!(v.sort.len(), 2);
        assert_eq!(v.sort[0].direction, Direction::Desc);
        assert_eq!(v.sort[1].direction, Direction::Asc);
        assert_eq!(v.column_size, [(PropertyId::File("name".into()), 180.0)]);
        assert_eq!(
            v.group_order,
            Some(vec![YamlValue::String("Done".into()), YamlValue::Null])
        );
    }

    #[test]
    fn single_statement_under_and() {
        let base = Base::parse("filters:\n  and: 'a == 1'\n").expect("base");
        assert_eq!(
            base.filters,
            Some(Filter::And(vec![Filter::Expr("a == 1".into())]))
        );
    }
}

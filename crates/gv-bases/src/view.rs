// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Evaluating a view: filters, columns, sort, limit, groups, summaries.
//!
//! A view never fails as a whole. A filter that cannot be evaluated for
//! a file excludes that file and is counted in
//! [`ViewResult::diagnostics`]; a cell that cannot be evaluated holds its
//! [`EvalError`].

use std::cmp::Ordering;
use std::sync::Arc;

use crate::eval::{EvalContext, EvalError, Evaluator, Program, RowState};
use crate::expr::{Expr, ParseError, parse};
use crate::model::{Base, BaseError, Direction, Filter, ImageFit, PropertyId, View, ViewKind};
use crate::source::VaultData;
use crate::value::{Value, natural_cmp, sort_cmp};
use crate::yaml::YamlValue;

/// A cell: a value, or why there is none.
pub type CellValue = Result<Value, EvalError>;

/// A filter with parsed statements.
#[derive(Debug)]
enum Compiled {
    Expr(Result<Expr, ParseError>, String),
    And(Vec<Compiled>),
    Or(Vec<Compiled>),
    Not(Vec<Compiled>),
    Invalid,
}

impl Compiled {
    fn new(filter: &Filter) -> Self {
        let all = |items: &[Filter]| items.iter().map(Compiled::new).collect();
        match filter {
            Filter::Expr(src) => Self::Expr(parse(src), src.clone()),
            Filter::And(items) => Self::And(all(items)),
            Filter::Or(items) => Self::Or(all(items)),
            Filter::Not(items) => Self::Not(all(items)),
            Filter::Invalid(_) => Self::Invalid,
        }
    }

    fn syntax_errors<'a>(&'a self, out: &mut Vec<(&'a str, &'a ParseError)>) {
        match self {
            Self::Expr(Err(e), src) => out.push((src, e)),
            Self::Expr(Ok(_), _) | Self::Invalid => {}
            Self::And(items) | Self::Or(items) | Self::Not(items) => {
                for i in items {
                    i.syntax_errors(out);
                }
            }
        }
    }
}

/// A base ready to evaluate: the model with every expression parsed.
#[derive(Debug)]
pub struct CompiledBase {
    base: Base,
    program: Program,
    filters: Option<Compiled>,
    view_filters: Vec<Option<Compiled>>,
}

/// One column (table) or shown property (cards, list).
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// The property.
    pub property: PropertyId,
    /// The header: `displayName`, else the default label.
    pub label: String,
    /// `columnSize`, in pixels.
    pub width: Option<f64>,
}

/// One file of the result.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Its vault path.
    pub path: Arc<str>,
    /// One cell per column.
    pub cells: Vec<CellValue>,
    /// Cards: the cover image property's value (see [`Cover::of`]).
    pub cover: Option<CellValue>,
}

/// A summary cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// The summary's name (`Average`, a custom one).
    pub name: String,
    /// Its value.
    pub value: CellValue,
}

/// Rows with the same `groupBy` value.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// The value (`Null` for files without one). [`Value::Null`] for the
    /// single group of an ungrouped view.
    pub key: Value,
    /// The rows.
    pub rows: Vec<Row>,
    /// One per column: the summary of this group, where the view has one.
    pub summaries: Vec<Option<Summary>>,
}

/// Why a result stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incomplete {
    /// [`EvalContext::deadline`] passed.
    TimedOut,
    /// [`EvalContext::cancel`] was set.
    Cancelled,
    /// More files than [`EvalContext::max_rows`] passed the filters.
    RowCap,
}

/// A problem met while evaluating, counted over files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Where: `filters`, `views[0].filters`, `formula.x`, `summary x`.
    pub source: String,
    /// What.
    pub message: String,
    /// How many files it affected.
    pub count: usize,
}

/// Cards view settings, with defaults applied.
#[derive(Debug, Clone, PartialEq)]
pub struct CardsLayout {
    /// The cover image property.
    pub image: Option<PropertyId>,
    /// How the cover fills the card.
    pub fit: ImageFit,
    /// Cover height / width (1.0: square).
    pub aspect_ratio: f64,
    /// Card width in pixels, as written.
    pub card_size: Option<f64>,
}

/// What a cover image value points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cover {
    /// A file of the vault.
    Path(Arc<str>),
    /// An external image.
    Url(Arc<str>),
    /// A colour (`#rrggbb`, `#rgb`).
    Color(Arc<str>),
}

impl Cover {
    /// The cover a cell value stands for: a link or file, an image, a URL,
    /// or a hex colour. `None` for anything else.
    #[must_use]
    pub fn of(value: &Value) -> Option<Self> {
        match value {
            Value::Link(l) => l.path.clone().map(Self::Path),
            Value::File(p) => Some(Self::Path(p.clone())),
            Value::Url(u) => Some(Self::Url(u.url.clone())),
            Value::Image(p) if p.contains("://") => Some(Self::Url(p.clone())),
            Value::Image(p) => Some(Self::Path(p.clone())),
            Value::String(s) => {
                let hex = s.strip_prefix('#')?;
                (matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                    .then(|| Self::Color(s.clone()))
            }
            Value::List(items) => items.first().and_then(Self::of),
            _ => None,
        }
    }
}

/// The evaluated view.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewResult {
    /// The view's index in [`Base::views`].
    pub view: usize,
    /// Its name.
    pub name: Option<String>,
    /// Its layout.
    pub kind: ViewKind,
    /// Set for layouts Glyphvane does not draw (map, kanban, plugin
    /// layouts): the text of the notice to show. Rows are still computed.
    pub notice: Option<String>,
    /// Columns (table) or shown properties (cards, list).
    pub columns: Vec<Column>,
    /// Whether the view has `groupBy`.
    pub grouped: bool,
    /// The groups in order; one group with key `Null` when not grouped.
    pub groups: Vec<Group>,
    /// One per column: the summary over every shown row.
    pub summaries: Vec<Option<Summary>>,
    /// Files that passed the filters.
    pub matched: usize,
    /// Rows shown (after `limit` and `groupOrder`).
    pub shown: usize,
    /// Set when evaluation stopped early.
    pub incomplete: Option<Incomplete>,
    /// Filter, formula and summary problems.
    pub diagnostics: Vec<Diagnostic>,
    /// Cards settings (cards views only).
    pub cards: Option<CardsLayout>,
}

impl ViewResult {
    /// Every row in display order.
    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.groups.iter().flat_map(|g| g.rows.iter())
    }
}

/// Collects diagnostics, counted by (source, message).
#[derive(Default)]
struct Diags(Vec<Diagnostic>);

impl Diags {
    fn add(&mut self, source: &str, message: String) {
        if let Some(d) = self
            .0
            .iter_mut()
            .find(|d| d.source == source && d.message == message)
        {
            d.count += 1;
        } else if self.0.len() < 100 {
            self.0.push(Diagnostic {
                source: source.to_owned(),
                message,
                count: 1,
            });
        }
    }
}

impl CompiledBase {
    /// Parses every expression of `base`.
    #[must_use]
    pub fn new(mut base: Base) -> Self {
        if base.views.is_empty() {
            base.views.push(View::default_table());
        }
        Self {
            program: Program::new(&base.formulas, &base.summaries),
            filters: base.filters.as_ref().map(Compiled::new),
            view_filters: base
                .views
                .iter()
                .map(|v| v.filters.as_ref().map(Compiled::new))
                .collect(),
            base,
        }
    }

    /// Reads and compiles the text of a `.base` file or `base` block.
    ///
    /// # Errors
    ///
    /// See [`Base::parse`].
    pub fn parse(text: &str) -> Result<Self, BaseError> {
        Base::parse(text).map(Self::new)
    }

    /// The model. A base without views has one default table view.
    #[must_use]
    pub fn base(&self) -> &Base {
        &self.base
    }

    /// Syntax errors of filters and formulas: where, the source text and
    /// the error. For a diagnostics panel; evaluation reports them too.
    #[must_use]
    pub fn syntax_errors(&self) -> Vec<(String, String, ParseError)> {
        let mut out = Vec::new();
        let mut collect = |at: String, c: &Compiled| {
            let mut list = Vec::new();
            c.syntax_errors(&mut list);
            for (src, e) in list {
                out.push((at.clone(), src.to_owned(), e.clone()));
            }
        };
        if let Some(f) = &self.filters {
            collect("filters".into(), f);
        }
        for (i, f) in self.view_filters.iter().enumerate() {
            if let Some(f) = f {
                collect(format!("views[{i}].filters"), f);
            }
        }
        for (name, src) in &self.base.formulas {
            if let Some(Err(e)) = self.program.formulas.get(name) {
                out.push((format!("formula.{name}"), src.clone(), e.clone()));
            }
        }
        for (name, src) in &self.base.summaries {
            if let Some(Err(e)) = self.program.summaries.get(name) {
                out.push((format!("summaries.{name}"), src.clone(), e.clone()));
            }
        }
        out
    }

    /// Evaluates one expression for the file at `path` (or for no file):
    /// the formula bar, tests.
    pub fn evaluate_expr(
        &self,
        expr: &str,
        path: Option<&str>,
        data: &dyn VaultData,
        ctx: &EvalContext,
    ) -> CellValue {
        let expr = parse(expr).map_err(EvalError::Syntax)?;
        let ev = Evaluator::new(data, ctx, &self.program);
        let row = RowState::new(path.and_then(|p| data.file(p)));
        ev.eval_row(&expr, &row)
    }

    /// Evaluates view `view` (out of range: the first view) over `data`.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn evaluate(&self, view: usize, data: &dyn VaultData, ctx: &EvalContext) -> ViewResult {
        let view_index = if view < self.base.views.len() {
            view
        } else {
            0
        };
        let v = &self.base.views[view_index];
        let ev = Evaluator::new(data, ctx, &self.program);
        let mut diags = Diags::default();
        for (at, src, e) in self.syntax_errors() {
            if at == "filters" || at == format!("views[{view_index}].filters") {
                diags.add(&at, format!("`{src}`: {e}"));
            }
        }

        let order: Vec<PropertyId> = if v.order.is_empty() {
            vec![PropertyId::File("name".into())]
        } else {
            v.order.clone()
        };
        let columns: Vec<Column> = order
            .iter()
            .map(|p| Column {
                property: p.clone(),
                label: self.base.label(p),
                width: v.column_size.iter().find(|(q, _)| q == p).map(|(_, w)| *w),
            })
            .collect();

        // Filter.
        let mut files = data.files();
        files.sort_by(|a, b| natural_cmp(&a.path, &b.path));
        let view_at = format!("views[{view_index}].filters");
        let mut incomplete = None;
        let mut passed: Vec<RowState> = Vec::new();
        for (i, file) in files.into_iter().enumerate() {
            if i % 64 == 0 && ctx.should_stop() {
                incomplete = Some(
                    if ctx
                        .cancel
                        .as_ref()
                        .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                    {
                        Incomplete::Cancelled
                    } else {
                        Incomplete::TimedOut
                    },
                );
                break;
            }
            let row = RowState::new(Some(file));
            let ok = self
                .filters
                .as_ref()
                .is_none_or(|f| passes(f, &ev, &row, "filters", &mut diags))
                && self.view_filters[view_index]
                    .as_ref()
                    .is_none_or(|f| passes(f, &ev, &row, &view_at, &mut diags));
            if ok {
                if passed.len() >= ctx.max_rows {
                    incomplete = Some(Incomplete::RowCap);
                    break;
                }
                passed.push(row);
            }
        }
        let matched = passed.len();

        // Sort.
        let sort_keys = |row: &RowState| -> Vec<Value> {
            v.sort
                .iter()
                .map(|k| ev.property(&k.property, row).unwrap_or(Value::Null))
                .collect()
        };
        let mut keyed: Vec<(Vec<Value>, RowState)> =
            passed.into_iter().map(|r| (sort_keys(&r), r)).collect();
        if !v.sort.is_empty() {
            keyed.sort_by(|(a, ra), (b, rb)| {
                for ((x, y), key) in a.iter().zip(b.iter()).zip(v.sort.iter()) {
                    let o = directed(x, y, key.direction);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                let pa = ra.file.as_ref().map_or("", |f| &f.path);
                let pb = rb.file.as_ref().map_or("", |f| &f.path);
                natural_cmp(pa, pb)
            });
        }
        if let Some(limit) = v.limit {
            keyed.truncate(limit);
        }

        // Cells.
        let cards = (v.kind == ViewKind::Cards).then(|| CardsLayout {
            image: v.image.clone(),
            fit: v.image_fit.unwrap_or_default(),
            aspect_ratio: v.image_aspect_ratio.filter(|r| *r > 0.0).unwrap_or(1.0),
            card_size: v.card_size,
        });
        let mut rows: Vec<(Value, Row)> = Vec::with_capacity(keyed.len());
        for (_, state) in &keyed {
            let path = state
                .file
                .as_ref()
                .map_or_else(|| Arc::from(""), |f| f.path.clone());
            let cells: Vec<CellValue> = order.iter().map(|p| ev.property(p, state)).collect();
            for (cell, p) in cells.iter().zip(order.iter()) {
                if let (Err(e), PropertyId::Formula(name)) = (cell, p) {
                    diags.add(&format!("formula.{name}"), e.to_string());
                }
            }
            let cover = cards
                .as_ref()
                .and_then(|c| c.image.as_ref())
                .map(|p| ev.property(p, state));
            let key = match &v.group_by {
                Some(g) => ev.property(&g.property, state).unwrap_or(Value::Null),
                None => Value::Null,
            };
            rows.push((key, Row { path, cells, cover }));
        }

        // Group.
        let mut groups: Vec<Group> = Vec::new();
        if let Some(g) = &v.group_by {
            for (key, row) in rows {
                match groups.iter_mut().find(|grp| ev.equal(&grp.key, &key)) {
                    Some(grp) => grp.rows.push(row),
                    None => groups.push(Group {
                        key,
                        rows: vec![row],
                        summaries: Vec::new(),
                    }),
                }
            }
            groups.sort_by(|a, b| directed(&a.key, &b.key, g.direction));
            if let Some(wanted) = &v.group_order {
                groups = order_groups(groups, wanted);
            }
        } else {
            groups.push(Group {
                key: Value::Null,
                rows: rows.into_iter().map(|(_, r)| r).collect(),
                summaries: Vec::new(),
            });
        }

        // Summaries.
        let summarize = |group_rows: &[&Row], diags: &mut Diags| -> Vec<Option<Summary>> {
            columns
                .iter()
                .enumerate()
                .map(|(ci, col)| {
                    let name = v
                        .summaries
                        .iter()
                        .find(|(p, _)| *p == col.property)
                        .map(|(_, n)| n)?;
                    let values: Vec<Value> = group_rows
                        .iter()
                        .filter_map(|r| r.cells[ci].as_ref().ok().cloned())
                        .collect();
                    let value = self.summary(name, values, &ev);
                    if let Err(e) = &value {
                        diags.add(&format!("summary {name}"), e.to_string());
                    }
                    Some(Summary {
                        name: name.clone(),
                        value,
                    })
                })
                .collect()
        };
        if !v.summaries.is_empty() {
            for grp in &mut groups {
                let refs: Vec<&Row> = grp.rows.iter().collect();
                grp.summaries = summarize(&refs, &mut diags);
            }
        } else {
            for grp in &mut groups {
                grp.summaries = vec![None; columns.len()];
            }
        }
        let all: Vec<&Row> = groups.iter().flat_map(|g| g.rows.iter()).collect();
        let shown = all.len();
        let summaries = if v.summaries.is_empty() {
            vec![None; columns.len()]
        } else {
            summarize(&all, &mut diags)
        };

        ViewResult {
            view: view_index,
            name: v.name.clone(),
            kind: v.kind.clone(),
            notice: (!v.kind.is_supported()).then(|| {
                format!(
                    "The {} layout is not supported. The results are listed as a table.",
                    v.kind.as_str()
                )
            }),
            columns,
            grouped: v.group_by.is_some(),
            groups,
            summaries,
            matched,
            shown,
            incomplete,
            diagnostics: diags.0,
            cards,
        }
    }

    /// A named summary over `values`: a default one (`Average`, `Min`,
    /// `Max`, `Sum`, `Range`, `Median`, `Stddev`, `Earliest`, `Latest`,
    /// `Checked`, `Unchecked`, `Empty`, `Filled`, `Unique`), else a custom
    /// one from the base's `summaries`.
    fn summary(&self, name: &str, values: Vec<Value>, ev: &Evaluator<'_>) -> CellValue {
        if let Some(r) = default_summary(name, &values, ev) {
            return r;
        }
        match self.program.summaries.get(name) {
            Some(Ok(expr)) => ev.eval_summary(expr, Value::list(values)),
            Some(Err(e)) => Err(EvalError::Syntax(e.clone())),
            None => Err(EvalError::Argument(format!("Unknown summary: {name}"))),
        }
    }
}

/// Orders with empty values last in both directions.
fn directed(a: &Value, b: &Value, dir: Direction) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => {
            let o = sort_cmp(a, b);
            if dir == Direction::Desc {
                o.reverse()
            } else {
                o
            }
        }
    }
}

fn order_groups(groups: Vec<Group>, wanted: &[YamlValue]) -> Vec<Group> {
    let mut slots: Vec<Option<Group>> = groups.into_iter().map(Some).collect();
    let mut out = Vec::new();
    for w in wanted {
        let found = slots.iter_mut().find(|g| {
            g.as_ref().is_some_and(|g| match w {
                YamlValue::Null => g.key.is_empty(),
                other => other
                    .scalar_text()
                    .is_some_and(|t| !g.key.is_empty() && g.key.display() == t),
            })
        });
        if let Some(slot) = found
            && let Some(g) = slot.take()
        {
            out.push(g);
        }
    }
    out
}

fn passes(f: &Compiled, ev: &Evaluator<'_>, row: &RowState, at: &str, diags: &mut Diags) -> bool {
    match f {
        Compiled::Expr(Ok(expr), src) => match ev.eval_row(expr, row) {
            Ok(v) => v.is_truthy(),
            Err(e) => {
                diags.add(at, format!("`{src}`: {e}"));
                false
            }
        },
        Compiled::Expr(Err(_), _) | Compiled::Invalid => false,
        Compiled::And(items) => items.iter().all(|i| passes(i, ev, row, at, diags)),
        Compiled::Or(items) => {
            items.is_empty() || items.iter().any(|i| passes(i, ev, row, at, diags))
        }
        Compiled::Not(items) => !items.iter().any(|i| passes(i, ev, row, at, diags)),
    }
}

#[allow(clippy::cast_precision_loss)]
fn default_summary(name: &str, values: &[Value], ev: &Evaluator<'_>) -> Option<CellValue> {
    let nums: Vec<f64> = values
        .iter()
        .filter_map(|v| match v {
            Value::Number(n) => Some(*n),
            _ => None,
        })
        .collect();
    let dates: Vec<i64> = values
        .iter()
        .filter_map(|v| match v {
            Value::Date(d) => Some(d.ms),
            _ => None,
        })
        .collect();
    let date_of = |ms: i64| {
        values
            .iter()
            .find_map(|v| match v {
                Value::Date(d) if d.ms == ms => Some(Value::Date(*d)),
                _ => None,
            })
            .unwrap_or(Value::Null)
    };
    let num = |f: &dyn Fn(&[f64]) -> f64| {
        if nums.is_empty() {
            Value::Null
        } else {
            Value::Number(f(&nums))
        }
    };
    let min = |n: &[f64]| n.iter().copied().fold(f64::INFINITY, f64::min);
    let max = |n: &[f64]| n.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean = |n: &[f64]| n.iter().sum::<f64>() / n.len() as f64;
    let count = |pred: &dyn Fn(&Value) -> bool| {
        Value::Number(values.iter().filter(|v| pred(v)).count() as f64)
    };
    Some(Ok(match name.to_ascii_lowercase().as_str() {
        "average" => num(&mean),
        "min" => num(&min),
        "max" => num(&max),
        "sum" => Value::Number(nums.iter().sum()),
        "range" => {
            if nums.is_empty() && !dates.is_empty() {
                let (lo, hi) = (dates.iter().min()?, dates.iter().max()?);
                Value::Duration(crate::date::Duration::from_ms(hi - lo))
            } else {
                num(&|n| max(n) - min(n))
            }
        }
        "median" => num(&|n| {
            let mut s = n.to_vec();
            s.sort_by(f64::total_cmp);
            let mid = s.len() / 2;
            if s.len() % 2 == 0 {
                f64::midpoint(s[mid - 1], s[mid])
            } else {
                s[mid]
            }
        }),
        "stddev" => num(&|n| {
            let m = mean(n);
            (n.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n.len() as f64).sqrt()
        }),
        "earliest" => dates.iter().min().map_or(Value::Null, |ms| date_of(*ms)),
        "latest" => dates.iter().max().map_or(Value::Null, |ms| date_of(*ms)),
        "checked" => count(&|v| matches!(v, Value::Bool(true))),
        "unchecked" => count(&|v| matches!(v, Value::Bool(false))),
        "empty" => count(&Value::is_empty),
        "filled" => count(&|v| !v.is_empty()),
        "unique" => {
            let mut seen: Vec<&Value> = Vec::new();
            for v in values {
                if !seen.iter().any(|s| ev.equal(s, v)) {
                    seen.push(v);
                }
            }
            Value::Number(seen.len() as f64)
        }
        _ => return None,
    }))
}

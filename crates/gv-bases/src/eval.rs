// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The evaluator: operators, names, formulas and the function subset.
//!
//! Errors never escape a cell: every failure is an [`EvalError`] value
//! that the view shows in place of the cell.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use crate::date::{Date, Duration};
use crate::expr::{BinOp, Expr, ParseError, UnOp, parse};
use crate::source::{FileInfo, VaultData};
use crate::value::{Link, Url, Value, sort_cmp};

/// How deep evaluation may recurse (formulas calling formulas included).
const MAX_EVAL_DEPTH: usize = 200;

/// Where and when a base is evaluated.
#[derive(Debug, Clone)]
pub struct EvalContext {
    /// The file `this` refers to: the base file when the base is opened
    /// on its own, the embedding note for an embedded base or a `base`
    /// block, the active file for a base in a sidebar. `None`: `this`
    /// is `null`.
    pub this: Option<Arc<str>>,
    /// The current local time (`now()`, `today()`, `relative()`).
    pub now: Date,
    /// The local time zone's offset from UTC in minutes (east positive),
    /// applied to file times and zoned date strings.
    pub utc_offset_minutes: i32,
    /// Stop evaluating after this instant; the result is then marked
    /// incomplete.
    pub deadline: Option<Instant>,
    /// Stop evaluating when set; the result is then marked incomplete.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The most files that may pass the filters; later ones are not
    /// evaluated and the result is marked incomplete.
    pub max_rows: usize,
}

impl EvalContext {
    /// A context at `now_utc_ms` (milliseconds since the Unix epoch) in a
    /// zone `utc_offset_minutes` east of UTC, without `this`, a deadline
    /// or cancellation, capped at 100,000 rows.
    #[must_use]
    pub fn new(now_utc_ms: i64, utc_offset_minutes: i32) -> Self {
        Self {
            this: None,
            now: Date {
                ms: now_utc_ms + i64::from(utc_offset_minutes) * 60_000,
                has_time: true,
            },
            utc_offset_minutes,
            deadline: None,
            cancel: None,
            max_rows: 100_000,
        }
    }

    /// [`new`](Self::new) at the system clock's current time.
    #[must_use]
    pub fn now(utc_offset_minutes: i32) -> Self {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        Self::new(ms, utc_offset_minutes)
    }

    /// With `this` set to the file at `path`.
    #[must_use]
    pub fn with_this(mut self, path: impl Into<Arc<str>>) -> Self {
        self.this = Some(path.into());
        self
    }

    /// A UTC time as a local [`Date`].
    #[must_use]
    pub fn local(&self, utc_ms: i64) -> Date {
        Date {
            ms: utc_ms + i64::from(self.utc_offset_minutes) * 60_000,
            has_time: true,
        }
    }

    pub(crate) fn should_stop(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            || self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// Why a cell (or a filter, or a summary) has no value.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    /// The expression does not parse.
    Syntax(ParseError),
    /// A function or method outside the supported subset.
    UnsupportedFunction(String),
    /// `formula.x` where the base has no formula `x`.
    UnknownFormula(String),
    /// A formula that depends on itself.
    CircularFormula(String),
    /// An operation on values of the wrong type.
    Type(String),
    /// A bad argument (count, type or content).
    Argument(String),
    /// Nesting too deep.
    TooDeep,
}

impl EvalError {
    /// The function's name, for [`EvalError::UnsupportedFunction`].
    #[must_use]
    pub fn function(&self) -> Option<&str> {
        match self {
            Self::UnsupportedFunction(name) => Some(name),
            _ => None,
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(e) => write!(f, "Syntax error: {e}"),
            Self::UnsupportedFunction(name) => write!(f, "Unsupported function: {name}()"),
            Self::UnknownFormula(name) => write!(f, "Unknown formula: {name}"),
            Self::CircularFormula(name) => write!(f, "Circular formula: {name}"),
            Self::Type(m) | Self::Argument(m) => f.write_str(m),
            Self::TooDeep => f.write_str("Expression nested too deeply"),
        }
    }
}

impl std::error::Error for EvalError {}

type R = Result<Value, EvalError>;

/// Parsed formulas and summaries of a base.
#[derive(Debug, Default)]
pub(crate) struct Program {
    pub formulas: HashMap<String, Result<Expr, ParseError>>,
    pub summaries: HashMap<String, Result<Expr, ParseError>>,
}

impl Program {
    pub fn new(formulas: &[(String, String)], summaries: &[(String, String)]) -> Self {
        Self {
            formulas: formulas
                .iter()
                .map(|(k, v)| (k.clone(), parse(v)))
                .collect(),
            summaries: summaries
                .iter()
                .map(|(k, v)| (k.clone(), parse(v)))
                .collect(),
        }
    }
}

enum Slot {
    Busy,
    Done(R),
}

/// One file being evaluated, with its formula results.
pub(crate) struct RowState {
    pub file: Option<Arc<FileInfo>>,
    formulas: RefCell<HashMap<String, Slot>>,
}

impl RowState {
    pub fn new(file: Option<Arc<FileInfo>>) -> Self {
        Self {
            file,
            formulas: RefCell::new(HashMap::new()),
        }
    }
}

/// Variables of `filter`, `map`, `reduce` and summaries.
struct Vars<'p> {
    names: Vec<(&'static str, Value)>,
    parent: Option<&'p Vars<'p>>,
}

impl Vars<'_> {
    fn get(&self, name: &str) -> Option<&Value> {
        self.names
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v)
            .or_else(|| self.parent.and_then(|p| p.get(name)))
    }
}

/// Evaluates expressions for one row.
pub(crate) struct Evaluator<'a> {
    pub data: &'a dyn VaultData,
    pub ctx: &'a EvalContext,
    pub program: &'a Program,
    depth: Cell<usize>,
}

/// Method names of the subset, for telling "unsupported" from "does
/// not apply to this type".
const KNOWN_METHODS: &[&str] = &[
    "isTruthy",
    "isType",
    "toString",
    "isEmpty",
    "contains",
    "containsAll",
    "containsAny",
    "startsWith",
    "endsWith",
    "lower",
    "upper",
    "title",
    "trim",
    "replace",
    "repeat",
    "reverse",
    "slice",
    "split",
    "length",
    "abs",
    "ceil",
    "floor",
    "round",
    "toFixed",
    "filter",
    "map",
    "reduce",
    "flat",
    "join",
    "sort",
    "unique",
    "mean",
    "median",
    "sum",
    "min",
    "max",
    "date",
    "time",
    "format",
    "relative",
    "year",
    "month",
    "day",
    "asLink",
    "hasLink",
    "hasProperty",
    "hasTag",
    "inFolder",
    "asFile",
    "linksTo",
    "keys",
    "values",
];

fn arg_count(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), EvalError> {
    if args.len() < min || args.len() > max {
        let want = if min == max {
            format!("{min}")
        } else {
            format!("{min} to {max}")
        };
        return Err(EvalError::Argument(format!(
            "{name}() takes {want} argument{}, got {}",
            if max == 1 { "" } else { "s" },
            args.len()
        )));
    }
    Ok(())
}

fn want_string<'v>(name: &str, v: &'v Value) -> Result<&'v str, EvalError> {
    match v {
        Value::String(s) => Ok(s),
        Value::Url(u) => Ok(&u.url),
        other => Err(EvalError::Argument(format!(
            "{name}() expects a string, got a {}",
            other.type_name()
        ))),
    }
}

fn want_number(name: &str, v: &Value) -> Result<f64, EvalError> {
    match v {
        Value::Number(n) => Ok(*n),
        other => Err(EvalError::Argument(format!(
            "{name}() expects a number, got a {}",
            other.type_name()
        ))),
    }
}

/// JavaScript's `Math.round`: halves round up.
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// Character index arguments of `slice()`: negative counts from the end.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn slice_bounds(len: usize, start: f64, end: Option<f64>) -> (usize, usize) {
    let norm = |i: f64| -> usize {
        let i = i.trunc();
        if i < 0.0 {
            (len as f64 + i).max(0.0) as usize
        } else {
            (i as usize).min(len)
        }
    };
    let s = norm(start);
    let e = end.map_or(len, norm);
    (s, e.max(s))
}

impl<'a> Evaluator<'a> {
    pub fn new(data: &'a dyn VaultData, ctx: &'a EvalContext, program: &'a Program) -> Self {
        Self {
            data,
            ctx,
            program,
            depth: Cell::new(0),
        }
    }

    /// Evaluates `expr` for `row`.
    pub fn eval_row(&self, expr: &Expr, row: &RowState) -> R {
        let vars = Vars {
            names: Vec::new(),
            parent: None,
        };
        self.eval(expr, row, &vars)
    }

    /// Evaluates a summary formula over `values`.
    pub fn eval_summary(&self, expr: &Expr, values: Value) -> R {
        let row = RowState::new(None);
        let vars = Vars {
            names: vec![("values", values)],
            parent: None,
        };
        self.eval(expr, &row, &vars)
    }

    /// The value of a property id of `row`.
    pub fn property(&self, id: &crate::model::PropertyId, row: &RowState) -> R {
        use crate::model::PropertyId;
        match id {
            PropertyId::Note(name) => Ok(row
                .file
                .as_ref()
                .and_then(|f| f.property(name).cloned())
                .unwrap_or(Value::Null)),
            PropertyId::File(name) => match &row.file {
                Some(f) => self.file_field(&f.path, name),
                None => Ok(Value::Null),
            },
            PropertyId::Formula(name) => self.formula(name, row),
        }
    }

    pub fn formula(&self, name: &str, row: &RowState) -> R {
        match row.formulas.borrow().get(name) {
            Some(Slot::Busy) => return Err(EvalError::CircularFormula(name.to_owned())),
            Some(Slot::Done(r)) => return r.clone(),
            None => {}
        }
        let expr = match self.program.formulas.get(name) {
            None => return Err(EvalError::UnknownFormula(name.to_owned())),
            Some(Err(e)) => return Err(EvalError::Syntax(e.clone())),
            Some(Ok(expr)) => expr,
        };
        row.formulas
            .borrow_mut()
            .insert(name.to_owned(), Slot::Busy);
        let result = self.eval_row(expr, row);
        row.formulas
            .borrow_mut()
            .insert(name.to_owned(), Slot::Done(result.clone()));
        result
    }

    fn eval(&self, e: &Expr, row: &RowState, vars: &Vars<'_>) -> R {
        let depth = self.depth.get();
        if depth >= MAX_EVAL_DEPTH {
            return Err(EvalError::TooDeep);
        }
        self.depth.set(depth + 1);
        let result = self.eval_inner(e, row, vars);
        self.depth.set(depth);
        result
    }

    fn eval_inner(&self, e: &Expr, row: &RowState, vars: &Vars<'_>) -> R {
        match e {
            Expr::Null => Ok(Value::Null),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Number(n) => Ok(Value::Number(*n)),
            Expr::String(s) => Ok(Value::String(s.clone())),
            Expr::List(items) => Ok(Value::list(
                items
                    .iter()
                    .map(|i| self.eval(i, row, vars))
                    .collect::<Result<_, _>>()?,
            )),
            Expr::Object(entries) => Ok(Value::Object(
                entries
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), self.eval(v, row, vars)?)))
                    .collect::<Result<Vec<_>, _>>()?
                    .into(),
            )),
            Expr::Ident(name) => self.ident(name, row, vars),
            Expr::Member(target, name) => {
                if let Some(ns) = self.namespace(target, vars) {
                    return self.in_namespace(ns, name, row);
                }
                let v = self.eval(target, row, vars)?;
                self.member(&v, name)
            }
            Expr::Index(target, index) => {
                let i = self.eval(index, row, vars)?;
                if let Some(ns) = self.namespace(target, vars) {
                    let Value::String(name) = &i else {
                        return Err(EvalError::Type(format!(
                            "{ns}[…] needs a property name, got a {}",
                            i.type_name()
                        )));
                    };
                    return self.in_namespace(ns, name, row);
                }
                let v = self.eval(target, row, vars)?;
                self.index(&v, &i)
            }
            Expr::Call(name, args) => self.call(name, args, row, vars),
            Expr::Method(target, name, args) => self.method_call(target, name, args, row, vars),
            Expr::Unary(op, inner) => {
                let v = self.eval(inner, row, vars)?;
                match op {
                    UnOp::Not => Ok(Value::Bool(!v.is_truthy())),
                    UnOp::Neg => match v {
                        Value::Number(n) => Ok(Value::Number(-n)),
                        Value::Duration(d) => Ok(Value::Duration(d.negated())),
                        Value::Null => Ok(Value::Null),
                        other => Err(EvalError::Type(format!(
                            "cannot negate a {}",
                            other.type_name()
                        ))),
                    },
                }
            }
            Expr::Binary(BinOp::And, l, r) => {
                let a = self.eval(l, row, vars)?;
                if !a.is_truthy() {
                    return Ok(Value::Bool(false));
                }
                Ok(Value::Bool(self.eval(r, row, vars)?.is_truthy()))
            }
            Expr::Binary(BinOp::Or, l, r) => {
                let a = self.eval(l, row, vars)?;
                if a.is_truthy() {
                    return Ok(Value::Bool(true));
                }
                Ok(Value::Bool(self.eval(r, row, vars)?.is_truthy()))
            }
            Expr::Binary(op, l, r) => {
                let a = self.eval(l, row, vars)?;
                let b = self.eval(r, row, vars)?;
                self.binary(*op, &a, &b)
            }
        }
    }

    /// `note` or `formula` used as a namespace (unless shadowed).
    fn namespace(&self, target: &Expr, vars: &Vars<'_>) -> Option<&'static str> {
        match target {
            Expr::Ident(n) if &**n == "note" && vars.get("note").is_none() => Some("note"),
            Expr::Ident(n) if &**n == "formula" && vars.get("formula").is_none() => Some("formula"),
            _ => None,
        }
    }

    fn in_namespace(&self, ns: &str, name: &str, row: &RowState) -> R {
        if ns == "formula" {
            self.formula(name, row)
        } else {
            Ok(row
                .file
                .as_ref()
                .and_then(|f| f.property(name).cloned())
                .unwrap_or(Value::Null))
        }
    }

    fn ident(&self, name: &str, row: &RowState, vars: &Vars<'_>) -> R {
        if let Some(v) = vars.get(name) {
            return Ok(v.clone());
        }
        match name {
            "file" => Ok(row
                .file
                .as_ref()
                .map_or(Value::Null, |f| Value::File(f.path.clone()))),
            "this" => Ok(self
                .ctx
                .this
                .as_ref()
                .map_or(Value::Null, |p| Value::File(p.clone()))),
            "note" => Ok(row.file.as_ref().map_or(Value::Null, |f| {
                Value::Object(f.properties.iter().cloned().collect())
            })),
            "formula" => Err(EvalError::Type("`formula` needs a name: formula.x".into())),
            _ => Ok(row
                .file
                .as_ref()
                .and_then(|f| f.property(name).cloned())
                .unwrap_or(Value::Null)),
        }
    }

    /// A field of a file (`name`, `mtime`, …), else its note property.
    fn file_field(&self, path: &Arc<str>, name: &str) -> R {
        let info = self.data.file(path);
        let Some(f) = info.as_deref() else {
            let tmp = FileInfo::new(path.clone());
            return Ok(match name {
                "name" => Value::string(tmp.name()),
                "basename" => Value::string(tmp.basename()),
                "path" => Value::String(path.clone()),
                "folder" => Value::string(tmp.folder()),
                "ext" => Value::string(tmp.ext()),
                "file" => Value::File(path.clone()),
                _ => Value::Null,
            });
        };
        let links = |list: &[crate::source::LinkInfo]| {
            Value::list(
                list.iter()
                    .map(|l| {
                        Value::Link(Arc::new(Link {
                            text: l.text.clone(),
                            path: l.target.clone(),
                            display: None,
                        }))
                    })
                    .collect(),
            )
        };
        #[allow(clippy::cast_precision_loss)]
        Ok(match name {
            "name" => Value::string(f.name()),
            "basename" => Value::string(f.basename()),
            "path" => Value::String(f.path.clone()),
            "folder" => Value::string(f.folder()),
            "ext" => Value::string(f.ext()),
            "size" => f.size.map_or(Value::Null, |s| Value::Number(s as f64)),
            "ctime" => f
                .ctime_ms
                .map_or(Value::Null, |t| Value::Date(self.ctx.local(t))),
            "mtime" => f
                .mtime_ms
                .map_or(Value::Null, |t| Value::Date(self.ctx.local(t))),
            "tags" => Value::list(f.tags.iter().map(|t| Value::String(t.clone())).collect()),
            "links" => links(&f.links),
            "embeds" => links(&f.embeds),
            "backlinks" => Value::list(
                self.data
                    .backlinks(&f.path)
                    .into_iter()
                    .map(|p| Value::Link(Arc::new(self.link_to_file(&p, None))))
                    .collect(),
            ),
            "properties" => Value::Object(f.properties.iter().cloned().collect()),
            "file" => Value::File(f.path.clone()),
            other => f.property(other).cloned().unwrap_or(Value::Null),
        })
    }

    fn link_to_file(&self, path: &Arc<str>, display: Option<Arc<str>>) -> Link {
        let tmp = FileInfo::new(path.clone());
        let text: Arc<str> = if tmp.ext() == "md" {
            tmp.basename().into()
        } else {
            tmp.name().into()
        };
        Link {
            text,
            path: Some(path.clone()),
            display,
        }
    }

    fn member(&self, v: &Value, name: &str) -> R {
        #[allow(clippy::cast_precision_loss)]
        match v {
            Value::Null => Ok(Value::Null),
            Value::File(path) => self.file_field(path, name),
            Value::Link(l) => match &l.path {
                Some(path) => self.file_field(path, name),
                None => Ok(Value::Null),
            },
            Value::Date(d) => {
                let c = d.civil();
                let n = match name {
                    "year" => c.year as f64,
                    "month" => f64::from(c.month),
                    "day" => f64::from(c.day),
                    "hour" => f64::from(c.hour),
                    "minute" => f64::from(c.minute),
                    "second" => f64::from(c.second),
                    "millisecond" => f64::from(c.millisecond),
                    _ => return Err(EvalError::Type(format!("a date has no field `{name}`"))),
                };
                Ok(Value::Number(n))
            }
            // The whole length in a unit, cut toward zero (`(due -
            // today()).days`); a month counts as 30.4375 days.
            Value::Duration(d) => {
                let unit = match name {
                    "days" => 86_400_000.0,
                    "hours" => 3_600_000.0,
                    "minutes" => 60_000.0,
                    "seconds" => 1_000.0,
                    "milliseconds" => 1.0,
                    _ => {
                        return Err(EvalError::Type(format!("a duration has no field `{name}`")));
                    }
                };
                Ok(Value::Number((d.approx_ms() / unit).trunc()))
            }
            Value::String(s) if name == "length" => Ok(Value::Number(s.chars().count() as f64)),
            Value::Url(u) if name == "length" => Ok(Value::Number(u.url.chars().count() as f64)),
            Value::List(items) if name == "length" => Ok(Value::Number(items.len() as f64)),
            Value::Object(entries) => Ok(entries
                .iter()
                .find(|(k, _)| &**k == name)
                .map_or(Value::Null, |(_, v)| v.clone())),
            other => Err(EvalError::Type(format!(
                "a {} has no field `{name}`",
                other.type_name()
            ))),
        }
    }

    fn index(&self, v: &Value, i: &Value) -> R {
        match (v, i) {
            (Value::Null, _) => Ok(Value::Null),
            (Value::List(items), Value::Number(n)) => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )]
                let idx = if *n < 0.0 {
                    items.len() as f64 + n.trunc()
                } else {
                    n.trunc()
                };
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Ok(if idx >= 0.0 {
                    items.get(idx as usize).cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                })
            }
            (Value::String(s), Value::Number(n)) if *n >= 0.0 =>
            {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Ok(s.chars()
                    .nth(n.trunc() as usize)
                    .map_or(Value::Null, |c| Value::string(c.to_string())))
            }
            (_, Value::String(name)) => self.member(v, name),
            _ => Err(EvalError::Type(format!(
                "cannot index a {} with a {}",
                v.type_name(),
                i.type_name()
            ))),
        }
    }

    // ----- operators -------------------------------------------------

    fn binary(&self, op: BinOp, a: &Value, b: &Value) -> R {
        match op {
            BinOp::Eq => Ok(Value::Bool(self.equal(a, b))),
            BinOp::Ne => Ok(Value::Bool(!self.equal(a, b))),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let ord = self.compare(a, b);
                Ok(Value::Bool(match (op, ord) {
                    (_, None) => false,
                    (BinOp::Lt, Some(o)) => o == Ordering::Less,
                    (BinOp::Le, Some(o)) => o != Ordering::Greater,
                    (BinOp::Gt, Some(o)) => o == Ordering::Greater,
                    (_, Some(o)) => o != Ordering::Less,
                }))
            }
            _ => self.arith(op, a, b),
        }
    }

    fn duration_of(v: &Value) -> Option<Duration> {
        match v {
            Value::Duration(d) => Some(*d),
            Value::String(s) => Duration::parse(s),
            _ => None,
        }
    }

    fn arith(&self, op: BinOp, a: &Value, b: &Value) -> R {
        let type_error = || {
            let sym = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                _ => "%",
            };
            EvalError::Type(format!(
                "cannot compute {} {sym} {}",
                a.type_name(),
                b.type_name()
            ))
        };
        if matches!(a, Value::Null) || matches!(b, Value::Null) {
            return Ok(Value::Null);
        }
        match (op, a, b) {
            (_, Value::Number(x), Value::Number(y)) => {
                let n = match op {
                    BinOp::Add => x + y,
                    BinOp::Sub => x - y,
                    BinOp::Mul => x * y,
                    BinOp::Div | BinOp::Rem if *y == 0.0 => {
                        return Err(EvalError::Type("division by zero".into()));
                    }
                    BinOp::Div => x / y,
                    _ => x % y,
                };
                Ok(Value::Number(n))
            }
            (BinOp::Add | BinOp::Sub, Value::Date(d), other) => {
                if let (BinOp::Sub, Value::Date(e)) = (op, other) {
                    return Ok(Value::Duration(Duration::from_ms(d.ms - e.ms)));
                }
                let dur = Self::duration_of(other).ok_or_else(|| {
                    EvalError::Type(format!("`{}` is not a duration", other.display()))
                })?;
                let dur = if op == BinOp::Sub { dur.negated() } else { dur };
                Ok(Value::Date(d.shifted(dur)))
            }
            (BinOp::Add, Value::Duration(dur), Value::Date(d)) => Ok(Value::Date(d.shifted(*dur))),
            (BinOp::Add | BinOp::Sub, Value::Duration(x), other)
                if Self::duration_of(other).is_some() =>
            {
                let y = Self::duration_of(other).unwrap_or_default();
                Ok(Value::Duration(if op == BinOp::Add {
                    x.plus(y)
                } else {
                    x.plus(y.negated())
                }))
            }
            (BinOp::Mul, Value::Duration(d), Value::Number(n)) => Ok(Value::Duration(d.times(*n))),
            (BinOp::Div, Value::Duration(d), Value::Number(n)) if *n != 0.0 => {
                Ok(Value::Duration(d.times(1.0 / n)))
            }
            (BinOp::Div, Value::Duration(x), Value::Duration(y)) if y.approx_ms() != 0.0 => {
                Ok(Value::Number(x.approx_ms() / y.approx_ms()))
            }
            (BinOp::Add, Value::List(x), Value::List(y)) => {
                Ok(Value::list(x.iter().chain(y.iter()).cloned().collect()))
            }
            (BinOp::Add, x, y)
                if matches!(x, Value::String(_) | Value::Url(_))
                    || matches!(y, Value::String(_) | Value::Url(_)) =>
            {
                Ok(Value::string(format!("{}{}", x.display(), y.display())))
            }
            _ => Err(type_error()),
        }
    }

    /// The path a value stands for, when it is a file or a resolved link.
    fn path_of(v: &Value) -> Option<&Arc<str>> {
        match v {
            Value::File(p) => Some(p),
            Value::Link(l) => l.path.as_ref(),
            _ => None,
        }
    }

    /// `==`: by value; links equal files and links that resolve to the
    /// same file, unresolved links compare by text.
    pub fn equal(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x == y,
            (Value::Date(x), Value::Date(y)) => x.ms == y.ms,
            (Value::Date(d), Value::String(s)) | (Value::String(s), Value::Date(d)) => {
                Date::parse(s, self.ctx.utc_offset_minutes).is_some_and(|e| e.ms == d.ms)
            }
            (Value::Link(x), Value::Link(y)) => match (&x.path, &y.path) {
                (Some(p), Some(q)) => p == q,
                _ => x.text == y.text,
            },
            (Value::Link(_) | Value::File(_), Value::Link(_) | Value::File(_)) => {
                Self::path_of(a).is_some() && Self::path_of(a) == Self::path_of(b)
            }
            (Value::Link(l), Value::String(s)) | (Value::String(s), Value::Link(l)) => {
                *l.text == **s
                    || l.path.as_deref() == Some(&**s)
                    || s.strip_prefix("[[")
                        .and_then(|r| r.strip_suffix("]]"))
                        .is_some_and(|t| *l.text == *t.split('|').next().unwrap_or(t))
            }
            (Value::Url(u), Value::String(s)) | (Value::String(s), Value::Url(u)) => *u.url == **s,
            (Value::Url(x), Value::Url(y)) => x.url == y.url,
            (Value::List(x), Value::List(y)) => {
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| self.equal(p, q))
            }
            _ => a == b,
        }
    }

    /// `<` and friends: numbers, dates (and date strings), durations,
    /// strings, booleans. Anything else is unordered.
    fn compare(&self, a: &Value, b: &Value) -> Option<Ordering> {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.partial_cmp(y),
            (Value::Date(x), Value::Date(y)) => Some(x.ms.cmp(&y.ms)),
            (Value::Date(x), Value::String(s)) => {
                Date::parse(s, self.ctx.utc_offset_minutes).map(|y| x.ms.cmp(&y.ms))
            }
            (Value::String(s), Value::Date(y)) => {
                Date::parse(s, self.ctx.utc_offset_minutes).map(|x| x.ms.cmp(&y.ms))
            }
            (Value::Duration(x), other) => {
                Self::duration_of(other).and_then(|y| x.approx_ms().partial_cmp(&y.approx_ms()))
            }
            (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
            (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
            _ => None,
        }
    }

    // ----- global functions ---------------------------------------------

    fn call(&self, name: &str, args: &[Expr], row: &RowState, vars: &Vars<'_>) -> R {
        if name == "if" {
            if args.len() < 2 || args.len() > 3 {
                return Err(EvalError::Argument(format!(
                    "if() takes 2 or 3 arguments, got {}",
                    args.len()
                )));
            }
            return if self.eval(&args[0], row, vars)?.is_truthy() {
                self.eval(&args[1], row, vars)
            } else if let Some(other) = args.get(2) {
                self.eval(other, row, vars)
            } else {
                Ok(Value::Null)
            };
        }
        if !matches!(
            name,
            "now"
                | "today"
                | "date"
                | "duration"
                | "number"
                | "list"
                | "link"
                | "file"
                | "html"
                | "image"
                | "icon"
                | "max"
                | "min"
                | "escapeHTML"
        ) {
            return Err(EvalError::UnsupportedFunction(name.to_owned()));
        }
        let vals: Vec<Value> = args
            .iter()
            .map(|a| self.eval(a, row, vars))
            .collect::<Result<_, _>>()?;
        let from = row.file.as_ref().map(|f| f.path.clone());
        self.global(name, &vals, from.as_deref())
    }

    #[allow(clippy::too_many_lines)]
    fn global(&self, name: &str, args: &[Value], from: Option<&str>) -> R {
        match name {
            "now" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::Date(self.ctx.now))
            }
            "today" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::Date(self.ctx.now.day_start()))
            }
            "date" => {
                arg_count(name, args, 1, 1)?;
                match &args[0] {
                    Value::Date(d) => Ok(Value::Date(*d)),
                    Value::String(s) => Date::parse(s, self.ctx.utc_offset_minutes)
                        .map(Value::Date)
                        .ok_or_else(|| EvalError::Argument(format!("date(): `{s}` is not a date"))),
                    #[allow(clippy::cast_possible_truncation)]
                    Value::Number(n) => Ok(Value::Date(self.ctx.local(*n as i64))),
                    Value::Null => Ok(Value::Null),
                    other => Err(EvalError::Argument(format!(
                        "date() expects a string, got a {}",
                        other.type_name()
                    ))),
                }
            }
            "duration" => {
                arg_count(name, args, 1, 1)?;
                match &args[0] {
                    Value::Duration(d) => Ok(Value::Duration(*d)),
                    Value::String(s) => Duration::parse(s).map(Value::Duration).ok_or_else(|| {
                        EvalError::Argument(format!("duration(): `{s}` is not a duration"))
                    }),
                    #[allow(clippy::cast_possible_truncation)]
                    Value::Number(n) => Ok(Value::Duration(Duration::from_ms(*n as i64))),
                    other => Err(EvalError::Argument(format!(
                        "duration() expects a string, got a {}",
                        other.type_name()
                    ))),
                }
            }
            "number" => {
                arg_count(name, args, 1, 1)?;
                #[allow(clippy::cast_precision_loss)]
                match &args[0] {
                    Value::Number(n) => Ok(Value::Number(*n)),
                    Value::Bool(b) => Ok(Value::Number(if *b { 1.0 } else { 0.0 })),
                    Value::Date(d) => Ok(Value::Number(
                        (d.ms - i64::from(self.ctx.utc_offset_minutes) * 60_000) as f64,
                    )),
                    Value::Duration(d) => Ok(Value::Number(d.approx_ms())),
                    Value::String(s) => s.trim().parse::<f64>().map(Value::Number).map_err(|_| {
                        EvalError::Argument(format!("number(): `{s}` is not a number"))
                    }),
                    Value::Null => Ok(Value::Null),
                    other => Err(EvalError::Argument(format!(
                        "number() cannot convert a {}",
                        other.type_name()
                    ))),
                }
            }
            "list" => {
                arg_count(name, args, 1, 1)?;
                Ok(match &args[0] {
                    Value::List(_) => args[0].clone(),
                    Value::Null => Value::list(Vec::new()),
                    other => Value::list(vec![other.clone()]),
                })
            }
            "link" => {
                arg_count(name, args, 1, 2)?;
                let display: Option<Arc<str>> = args.get(1).map(|d| d.display().into());
                match &args[0] {
                    Value::File(p) => Ok(Value::Link(Arc::new(self.link_to_file(p, display)))),
                    Value::Link(l) => Ok(Value::Link(Arc::new(Link {
                        display: display.or_else(|| l.display.clone()),
                        ..(**l).clone()
                    }))),
                    Value::Url(u) => Ok(Value::Url(Arc::new(Url {
                        url: u.url.clone(),
                        display: display.or_else(|| u.display.clone()),
                    }))),
                    Value::Null => Ok(Value::Null),
                    other => {
                        let text = other.display();
                        if text.contains("://") || text.starts_with("mailto:") {
                            return Ok(Value::Url(Arc::new(Url {
                                url: text.into(),
                                display,
                            })));
                        }
                        let target = text
                            .strip_prefix("[[")
                            .and_then(|t| t.strip_suffix("]]"))
                            .unwrap_or(&text);
                        let path = self.resolve(target, from);
                        Ok(Value::Link(Arc::new(Link {
                            text: target.into(),
                            path,
                            display,
                        })))
                    }
                }
            }
            "file" => {
                arg_count(name, args, 1, 1)?;
                Ok(match &args[0] {
                    Value::File(_) => args[0].clone(),
                    Value::Link(l) => l.path.clone().map_or(Value::Null, Value::File),
                    Value::String(s) => self.resolve(s, from).map_or(Value::Null, Value::File),
                    _ => Value::Null,
                })
            }
            "html" | "icon" => {
                arg_count(name, args, 1, 1)?;
                let text: Arc<str> = args[0].display().into();
                Ok(if name == "html" {
                    Value::Html(text)
                } else {
                    Value::Icon(text)
                })
            }
            "image" => {
                arg_count(name, args, 1, 1)?;
                Ok(match &args[0] {
                    Value::File(p) => Value::Image(p.clone()),
                    Value::Link(l) => {
                        Value::Image(l.path.clone().unwrap_or_else(|| l.text.clone()))
                    }
                    Value::Url(u) => Value::Image(u.url.clone()),
                    Value::Null => Value::Null,
                    other => Value::Image(other.display().into()),
                })
            }
            "max" | "min" => {
                let nums: Vec<f64> = args
                    .iter()
                    .filter(|v| !matches!(v, Value::Null))
                    .map(|v| want_number(name, v))
                    .collect::<Result<_, _>>()?;
                Ok(extreme(&nums, name == "max"))
            }
            "escapeHTML" => {
                arg_count(name, args, 1, 1)?;
                let s = args[0].display();
                let mut out = String::with_capacity(s.len());
                for c in s.chars() {
                    match c {
                        '&' => out.push_str("&amp;"),
                        '<' => out.push_str("&lt;"),
                        '>' => out.push_str("&gt;"),
                        '"' => out.push_str("&quot;"),
                        '\'' => out.push_str("&#39;"),
                        c => out.push(c),
                    }
                }
                Ok(Value::string(out))
            }
            _ => Err(EvalError::UnsupportedFunction(name.to_owned())),
        }
    }

    /// A link target as a path: an existing path as is, else resolved
    /// like a wikilink written in `from` (or in `this`).
    fn resolve(&self, text: &str, from: Option<&str>) -> Option<Arc<str>> {
        if let Some(f) = self.data.file(text) {
            return Some(f.path.clone());
        }
        let from = from.or(self.ctx.this.as_deref()).unwrap_or("");
        self.data.resolve_link(text, from)
    }

    // ----- methods ---------------------------------------------------------

    fn method_call(
        &self,
        target: &Expr,
        name: &str,
        args: &[Expr],
        row: &RowState,
        vars: &Vars<'_>,
    ) -> R {
        if !KNOWN_METHODS.contains(&name) {
            return Err(EvalError::UnsupportedFunction(name.to_owned()));
        }
        let recv = self.eval(target, row, vars)?;
        if let (Value::List(items), "filter" | "map" | "reduce") = (&recv, name) {
            return self.lambda(items, name, args, row, vars);
        }
        if matches!(name, "filter" | "map" | "reduce") && matches!(recv, Value::Null) {
            return Ok(Value::Null);
        }
        let vals: Vec<Value> = args
            .iter()
            .map(|a| self.eval(a, row, vars))
            .collect::<Result<_, _>>()?;
        self.method(&recv, name, &vals)
    }

    fn lambda(
        &self,
        items: &[Value],
        name: &str,
        args: &[Expr],
        row: &RowState,
        vars: &Vars<'_>,
    ) -> R {
        let want = if name == "reduce" { 2 } else { 1 };
        if args.len() != want {
            return Err(EvalError::Argument(format!(
                "{name}() takes {want} argument{}, got {}",
                if want == 1 { "" } else { "s" },
                args.len()
            )));
        }
        let body = &args[0];
        #[allow(clippy::cast_precision_loss)]
        match name {
            "filter" | "map" => {
                let mut out = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let scope = Vars {
                        names: vec![("value", item.clone()), ("index", Value::Number(i as f64))],
                        parent: Some(vars),
                    };
                    let v = self.eval(body, row, &scope)?;
                    if name == "map" {
                        out.push(v);
                    } else if v.is_truthy() {
                        out.push(item.clone());
                    }
                }
                Ok(Value::list(out))
            }
            _ => {
                let mut acc = self.eval(&args[1], row, vars)?;
                for (i, item) in items.iter().enumerate() {
                    let scope = Vars {
                        names: vec![
                            ("value", item.clone()),
                            ("index", Value::Number(i as f64)),
                            ("acc", acc),
                        ],
                        parent: Some(vars),
                    };
                    acc = self.eval(body, row, &scope)?;
                }
                Ok(acc)
            }
        }
    }

    fn not_for(name: &str, v: &Value) -> EvalError {
        EvalError::Type(format!("{name}() does not apply to a {}", v.type_name()))
    }

    fn method(&self, recv: &Value, name: &str, args: &[Value]) -> R {
        // Methods of every type.
        match name {
            "isTruthy" => {
                arg_count(name, args, 0, 0)?;
                return Ok(Value::Bool(recv.is_truthy()));
            }
            "isType" => {
                arg_count(name, args, 1, 1)?;
                let t = want_string(name, &args[0])?;
                return Ok(Value::Bool(recv.type_name().eq_ignore_ascii_case(t)));
            }
            "toString" => {
                arg_count(name, args, 0, 0)?;
                return Ok(Value::string(recv.display()));
            }
            "isEmpty" => {
                arg_count(name, args, 0, 0)?;
                return Ok(Value::Bool(recv.is_empty()));
            }
            _ => {}
        }
        match recv {
            Value::Null => Ok(Value::Null),
            Value::String(s) => self.string_method(s, recv, name, args),
            Value::Url(u) => self.string_method(&u.url, recv, name, args),
            Value::Number(n) => Self::number_method(*n, recv, name, args),
            Value::List(items) => self.list_method(items, recv, name, args),
            Value::Date(d) => self.date_method(*d, recv, name, args),
            Value::File(path) => self.file_method(path, recv, name, args),
            Value::Link(l) => match name {
                "asFile" => {
                    arg_count(name, args, 0, 0)?;
                    Ok(l.path.clone().map_or(Value::Null, Value::File))
                }
                "linksTo" => {
                    arg_count(name, args, 1, 1)?;
                    match &l.path {
                        Some(p) => self.file_method(p, recv, "hasLink", args),
                        None => Ok(Value::Bool(false)),
                    }
                }
                _ => Err(Self::not_for(name, recv)),
            },
            Value::Object(entries) => match name {
                "keys" => {
                    arg_count(name, args, 0, 0)?;
                    Ok(Value::list(
                        entries
                            .iter()
                            .map(|(k, _)| Value::String(k.clone()))
                            .collect(),
                    ))
                }
                "values" => {
                    arg_count(name, args, 0, 0)?;
                    Ok(Value::list(
                        entries.iter().map(|(_, v)| v.clone()).collect(),
                    ))
                }
                _ => Err(Self::not_for(name, recv)),
            },
            _ => Err(Self::not_for(name, recv)),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn string_method(&self, s: &str, recv: &Value, name: &str, args: &[Value]) -> R {
        #[allow(clippy::cast_precision_loss)]
        match name {
            "contains" | "startsWith" | "endsWith" => {
                arg_count(name, args, 1, 1)?;
                let q = want_string(name, &args[0])?;
                Ok(Value::Bool(match name {
                    "contains" => s.contains(q),
                    "startsWith" => s.starts_with(q),
                    _ => s.ends_with(q),
                }))
            }
            "containsAll" | "containsAny" => {
                let qs: Vec<&str> = args
                    .iter()
                    .map(|a| want_string(name, a))
                    .collect::<Result<_, _>>()?;
                Ok(Value::Bool(if name == "containsAll" {
                    qs.iter().all(|q| s.contains(q))
                } else {
                    qs.iter().any(|q| s.contains(q))
                }))
            }
            "lower" | "upper" | "title" | "trim" | "reverse" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::string(match name {
                    "lower" => s.to_lowercase(),
                    "upper" => s.to_uppercase(),
                    "trim" => s.trim().to_owned(),
                    "reverse" => s.chars().rev().collect(),
                    _ => title_case(s),
                }))
            }
            "length" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::Number(s.chars().count() as f64))
            }
            "replace" => {
                arg_count(name, args, 2, 2)?;
                let pat = want_string(name, &args[0])?;
                let rep = args[1].display();
                if pat.is_empty() {
                    return Ok(Value::string(format!("{rep}{s}")));
                }
                Ok(Value::string(s.replace(pat, &rep)))
            }
            "repeat" => {
                arg_count(name, args, 1, 1)?;
                let n = want_number(name, &args[0])?;
                if !(0.0..=10_000.0).contains(&n) || s.len() as f64 * n > 1_000_000.0 {
                    return Err(EvalError::Argument("repeat(): count out of range".into()));
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Ok(Value::string(s.repeat(n as usize)))
            }
            "slice" => {
                arg_count(name, args, 1, 2)?;
                let chars: Vec<char> = s.chars().collect();
                let start = want_number(name, &args[0])?;
                let end = args.get(1).map(|e| want_number(name, e)).transpose()?;
                let (a, b) = slice_bounds(chars.len(), start, end);
                Ok(Value::string(chars[a..b].iter().collect::<String>()))
            }
            "split" => {
                arg_count(name, args, 1, 2)?;
                let sep = want_string(name, &args[0])?;
                let mut parts: Vec<Value> = if sep.is_empty() {
                    s.chars().map(|c| Value::string(c.to_string())).collect()
                } else {
                    s.split(sep).map(Value::string).collect()
                };
                if let Some(n) = args.get(1) {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    parts.truncate(want_number(name, n)?.max(0.0) as usize);
                }
                Ok(Value::list(parts))
            }
            _ => Err(Self::not_for(name, recv)),
        }
    }

    fn number_method(n: f64, recv: &Value, name: &str, args: &[Value]) -> R {
        match name {
            "abs" | "ceil" | "floor" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::Number(match name {
                    "abs" => n.abs(),
                    "ceil" => n.ceil(),
                    _ => n.floor(),
                }))
            }
            "round" => {
                arg_count(name, args, 0, 1)?;
                let digits = match args.first() {
                    Some(d) => want_number(name, d)?.trunc().clamp(0.0, 15.0),
                    None => 0.0,
                };
                let f = 10f64.powf(digits);
                Ok(Value::Number(js_round(n * f) / f))
            }
            "toFixed" => {
                arg_count(name, args, 1, 1)?;
                let p = want_number(name, &args[0])?;
                if !(0.0..=100.0).contains(&p) {
                    return Err(EvalError::Argument(
                        "toFixed(): precision out of range".into(),
                    ));
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let p = p as usize;
                Ok(Value::string(format!("{n:.p$}")))
            }
            _ => Err(Self::not_for(name, recv)),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn list_method(&self, items: &Arc<[Value]>, recv: &Value, name: &str, args: &[Value]) -> R {
        let numbers = || -> Vec<f64> {
            items
                .iter()
                .filter_map(|v| match v {
                    Value::Number(n) => Some(*n),
                    _ => None,
                })
                .collect()
        };
        #[allow(clippy::cast_precision_loss)]
        match name {
            "contains" => {
                arg_count(name, args, 1, 1)?;
                Ok(Value::Bool(items.iter().any(|v| self.equal(v, &args[0]))))
            }
            "containsAll" => Ok(Value::Bool(
                args.iter().all(|a| items.iter().any(|v| self.equal(v, a))),
            )),
            "containsAny" => Ok(Value::Bool(
                args.iter().any(|a| items.iter().any(|v| self.equal(v, a))),
            )),
            "length" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::Number(items.len() as f64))
            }
            "flat" => {
                arg_count(name, args, 0, 0)?;
                let mut out = Vec::new();
                for v in items.iter() {
                    match v {
                        Value::List(inner) => out.extend(inner.iter().cloned()),
                        other => out.push(other.clone()),
                    }
                }
                Ok(Value::list(out))
            }
            "join" => {
                arg_count(name, args, 0, 1)?;
                let sep = match args.first() {
                    Some(s) => s.display(),
                    None => ",".into(),
                };
                Ok(Value::string(
                    items
                        .iter()
                        .map(Value::display)
                        .collect::<Vec<_>>()
                        .join(&sep),
                ))
            }
            "reverse" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::list(items.iter().rev().cloned().collect()))
            }
            "slice" => {
                arg_count(name, args, 1, 2)?;
                let start = want_number(name, &args[0])?;
                let end = args.get(1).map(|e| want_number(name, e)).transpose()?;
                let (a, b) = slice_bounds(items.len(), start, end);
                Ok(Value::list(items[a..b].to_vec()))
            }
            "sort" => {
                arg_count(name, args, 0, 0)?;
                let mut v = items.to_vec();
                v.sort_by(|a, b| match (a.is_empty(), b.is_empty()) {
                    (true, false) => Ordering::Greater,
                    (false, true) => Ordering::Less,
                    _ => sort_cmp(a, b),
                });
                Ok(Value::list(v))
            }
            "unique" => {
                arg_count(name, args, 0, 0)?;
                let mut out: Vec<Value> = Vec::new();
                for v in items.iter() {
                    if !out.iter().any(|o| self.equal(o, v)) {
                        out.push(v.clone());
                    }
                }
                Ok(Value::list(out))
            }
            "sum" | "mean" | "median" | "min" | "max" => {
                arg_count(name, args, 0, 0)?;
                let mut nums = numbers();
                if nums.is_empty() {
                    return Ok(if name == "sum" {
                        Value::Number(0.0)
                    } else {
                        Value::Null
                    });
                }
                Ok(match name {
                    "sum" => Value::Number(nums.iter().sum()),
                    "mean" => Value::Number(nums.iter().sum::<f64>() / nums.len() as f64),
                    "median" => {
                        nums.sort_by(f64::total_cmp);
                        let mid = nums.len() / 2;
                        Value::Number(if nums.len() % 2 == 0 {
                            f64::midpoint(nums[mid - 1], nums[mid])
                        } else {
                            nums[mid]
                        })
                    }
                    _ => extreme(&nums, name == "max"),
                })
            }
            _ => Err(Self::not_for(name, recv)),
        }
    }

    fn date_method(&self, d: Date, recv: &Value, name: &str, args: &[Value]) -> R {
        match name {
            "date" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::Date(d.day_start()))
            }
            "time" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::string(d.time_text()))
            }
            "format" => {
                arg_count(name, args, 1, 1)?;
                Ok(Value::string(d.format(want_string(name, &args[0])?)))
            }
            "relative" => {
                arg_count(name, args, 0, 0)?;
                Ok(Value::string(d.relative(self.ctx.now)))
            }
            "year" | "month" | "day" => {
                arg_count(name, args, 0, 0)?;
                self.member(recv, name)
            }
            _ => Err(Self::not_for(name, recv)),
        }
    }

    fn file_method(&self, path: &Arc<str>, recv: &Value, name: &str, args: &[Value]) -> R {
        let info = self.data.file(path);
        match name {
            "asLink" => {
                arg_count(name, args, 0, 1)?;
                let display = args.first().map(|d| Arc::from(d.display()));
                Ok(Value::Link(Arc::new(self.link_to_file(path, display))))
            }
            "hasProperty" => {
                arg_count(name, args, 1, 1)?;
                let key = want_string(name, &args[0])?;
                Ok(Value::Bool(info.is_some_and(|f| f.property(key).is_some())))
            }
            "hasTag" => {
                let wanted: Vec<String> = args
                    .iter()
                    .map(|a| want_string(name, a).map(|s| s.trim_start_matches('#').to_lowercase()))
                    .collect::<Result<_, _>>()?;
                Ok(Value::Bool(info.is_some_and(|f| {
                    f.tags.iter().any(|t| {
                        let t = t.trim_start_matches('#').to_lowercase();
                        wanted.iter().any(|w| {
                            t == *w
                                || (t.starts_with(w.as_str())
                                    && t.as_bytes().get(w.len()) == Some(&b'/'))
                        })
                    })
                })))
            }
            "inFolder" => {
                arg_count(name, args, 1, 1)?;
                let folder = want_string(name, &args[0])?.trim_matches('/');
                Ok(Value::Bool(
                    folder.is_empty()
                        || (path.starts_with(folder)
                            && path.as_bytes().get(folder.len()) == Some(&b'/')),
                ))
            }
            "hasLink" => {
                arg_count(name, args, 1, 1)?;
                let Some(f) = info else {
                    return Ok(Value::Bool(false));
                };
                let from = Some(&*f.path);
                let (target, text): (Option<Arc<str>>, Option<Arc<str>>) = match &args[0] {
                    Value::File(p) => (Some(p.clone()), None),
                    Value::Link(l) => (l.path.clone(), Some(l.text.clone())),
                    Value::String(s) => (self.resolve(s, from), Some(s.clone())),
                    Value::Null => return Ok(Value::Bool(false)),
                    other => {
                        return Err(EvalError::Argument(format!(
                            "hasLink() expects a file, link or string, got a {}",
                            other.type_name()
                        )));
                    }
                };
                Ok(Value::Bool(f.links.iter().chain(f.embeds.iter()).any(
                    |l| match (&target, &l.target) {
                        (Some(t), Some(lt)) => t == lt,
                        _ => text.as_deref().is_some_and(|x| *l.text == *x),
                    },
                )))
            }
            _ => Err(Self::not_for(name, recv)),
        }
    }
}

fn extreme(nums: &[f64], max: bool) -> Value {
    let pick = nums.iter().copied().reduce(|a, b| {
        if (max && b > a) || (!max && b < a) {
            b
        } else {
            a
        }
    });
    pick.map_or(Value::Null, Value::Number)
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for c in s.chars() {
        if start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            start = false;
        } else {
            if c.is_whitespace() {
                start = true;
            } else if c.is_alphanumeric() {
                start = false;
            }
            out.push(c);
        }
    }
    out
}

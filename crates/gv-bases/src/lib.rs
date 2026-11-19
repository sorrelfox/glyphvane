// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Bases model, expression language and evaluation over the content index.
//!
//! A base (a `.base` file or a ` ```base ` code block) is YAML with the
//! top-level keys `filters`, `formulas`, `properties`, `summaries` and
//! `views`. It lists the files of the vault that pass its filters, with
//! columns of note properties, file properties and formulas.
//!
//! # Use
//!
//! ```
//! use gv_bases::{CompiledBase, EvalContext, FileInfo, MemoryData, Value};
//!
//! let base = CompiledBase::parse(
//!     "filters: 'file.hasTag(\"book\")'\n\
//!      formulas:\n  pages_left: 'pages - read'\n\
//!      views:\n  - type: table\n    order: [file.name, formula.pages_left]\n",
//! )
//! .expect("valid base");
//! let mut note = FileInfo::new("Books/Dune.md");
//! note.tags.push("book".into());
//! note.properties.push(("pages".into(), Value::Number(412.0)));
//! note.properties.push(("read".into(), Value::Number(100.0)));
//! let data = MemoryData::new([note, FileInfo::new("Other.md")]);
//! let result = base.evaluate(0, &data, &EvalContext::new(0, 0));
//! let row = result.rows().next().expect("one row");
//! assert_eq!(row.cells[0], Ok(Value::string("Dune.md")));
//! assert_eq!(row.cells[1], Ok(Value::Number(312.0)));
//! ```
//!
//! # Parts
//!
//! | Part | What |
//! |---|---|
//! | [`Base`] | The model. Unknown keys are kept in order; wrong shapes become [`ModelDiagnostic`]s, not failures |
//! | [`expr`] | Lexer and parser of the expression language |
//! | [`Value`] | string, number, boolean, date, duration, list, object, link, URL, file, null, and the rich `html`/`image`/`icon` values |
//! | [`CompiledBase`] | A base with parsed expressions; [`CompiledBase::evaluate`] gives a [`ViewResult`] |
//! | [`VaultData`] | The files a base evaluates over: [`IndexData`] over the content index, [`MemoryData`] in memory |
//! | [`EvalContext`] | `this`, the clock and zone, the deadline, cancellation and the row cap |
//!
//! # Expression language
//!
//! Operators `+ - * / %`, `== != > < >= <=`, `! && ||`, parentheses,
//! member access, indexing (`list[0]`, `note["a b"]`) and list literals.
//! Names: `note.x` or bare `x` (front matter), `file.*` (`name`,
//! `basename`, `path`, `folder`, `ext`, `size`, `ctime`, `mtime`, `tags`,
//! `links`, `embeds`, `backlinks`, `properties`, `file`), `formula.x`,
//! `this` (a file: the base file, the embedding note, or the active file;
//! see [`EvalContext::this`]). Dates take durations: `now() - "2h"`,
//! `date("2025-01-01") + "1M"`; two dates subtract to a duration.
//!
//! # Function subset
//!
//! | Receiver | Functions |
//! |---|---|
//! | Global | `if`, `link`, `list`, `date`, `duration`, `now`, `today`, `number`, `file`, `max`, `min`, `escapeHTML`, `html`, `image`, `icon` |
//! | Any | `isTruthy`, `isType`, `toString`, `isEmpty` |
//! | String (and URL) | `contains`, `containsAll`, `containsAny`, `startsWith`, `endsWith`, `lower`, `upper`, `title`, `trim`, `replace` (text patterns), `repeat`, `reverse`, `slice`, `split` (text separators), `length` |
//! | Number | `abs`, `ceil`, `floor`, `round`, `toFixed` |
//! | List | `contains`, `containsAll`, `containsAny`, `filter`, `map`, `reduce`, `flat`, `join`, `reverse`, `slice`, `sort`, `unique`, `length`, `mean`, `median`, `sum`, `min`, `max` |
//! | Date | `date`, `time`, `format` (Moment.js tokens), `relative`, `year`, `month`, `day`; fields `year` … `millisecond` |
//! | File | `asLink`, `hasLink`, `hasProperty`, `hasTag`, `inFolder` |
//! | Link | `asFile`, `linksTo`; fields of the linked file |
//! | Duration | fields `days`, `hours`, `minutes`, `seconds`, `milliseconds` (the whole length in that unit) |
//! | Object | `keys`, `values` |
//!
//! Anything else is an [`EvalError::UnsupportedFunction`] in the cell
//! that uses it. Regular expressions are not supported.

pub mod date;
mod eval;
pub mod expr;
mod index;
mod model;
mod source;
mod value;
mod view;
pub mod yaml;

pub use date::{Date, Duration};
pub use eval::{EvalContext, EvalError};
pub use expr::ParseError;
pub use index::IndexData;
pub use model::{
    Base, BaseError, Direction, Filter, GroupBy, ImageFit, ModelDiagnostic, PropertyConfig,
    PropertyId, SortKey, View, ViewKind,
};
pub use source::{
    FileInfo, LinkInfo, MemoryData, VaultData, number_value, property_value, text_value,
};
pub use value::{Link, Url, Value, format_number, natural_cmp, sort_cmp};
pub use view::{
    CardsLayout, CellValue, Column, CompiledBase, Cover, Diagnostic, Group, Incomplete, Row,
    Summary, ViewResult,
};
pub use yaml::{YamlError, YamlValue};

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Expression and view tests over files in memory.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::date::Date;
use crate::{
    CompiledBase, Cover, EvalContext, EvalError, FileInfo, Incomplete, LinkInfo, MemoryData,
    PropertyId, Value, ViewKind, text_value,
};

/// 2025-03-10 12:00 UTC.
const NOW: i64 = 1_741_608_000_000;

fn ctx() -> EvalContext {
    EvalContext::new(NOW, 0)
}

fn link(text: &str) -> LinkInfo {
    LinkInfo {
        text: text.into(),
        target: None,
    }
}

fn note(path: &str, props: &[(&str, Value)], tags: &[&str], links: &[&str]) -> FileInfo {
    let mut f = FileInfo::new(path);
    f.size = Some(100);
    f.mtime_ms = Some(NOW - 86_400_000);
    f.ctime_ms = Some(NOW - 10 * 86_400_000);
    f.properties = props
        .iter()
        .map(|(k, v)| (Arc::from(*k), v.clone()))
        .collect();
    f.tags = tags.iter().map(|t| Arc::from(*t)).collect();
    f.links = links.iter().map(|l| link(l)).collect();
    f
}

fn s(x: &str) -> Value {
    Value::string(x)
}

fn n(x: f64) -> Value {
    Value::Number(x)
}

fn vault() -> MemoryData {
    MemoryData::new([
        note(
            "Books/Dune.md",
            &[
                ("pages", n(412.0)),
                ("price", n(9.5)),
                ("status", s("reading")),
                ("author", text_value("[[Frank Herbert]]", 0)),
                ("started", text_value("2025-03-01", 0)),
                ("genres", Value::list(vec![s("sf"), s("classic")])),
                ("done", Value::Bool(false)),
            ],
            &["book", "sf/classic"],
            &["Frank Herbert", "Reading list"],
        ),
        note(
            "Books/Emma.md",
            &[
                ("pages", n(474.0)),
                ("price", n(4.0)),
                ("status", s("done")),
                ("author", text_value("[[Jane Austen]]", 0)),
                ("done", Value::Bool(true)),
            ],
            &["book"],
            &["Jane Austen"],
        ),
        note(
            "Books/Notes on Dune.md",
            &[("status", s("draft"))],
            &["draft"],
            &["Dune"],
        ),
        note(
            "People/Frank Herbert.md",
            &[("born", n(1920.0))],
            &["person"],
            &[],
        ),
        note(
            "People/Jane Austen.md",
            &[("born", n(1775.0))],
            &["person"],
            &["Emma"],
        ),
        note("Reading list.md", &[], &[], &["Dune", "Emma"]),
        FileInfo::new("Attachments/cover.png"),
        FileInfo::new("Library.base"),
    ])
}

fn eval_on(expr: &str, path: Option<&str>) -> Result<Value, EvalError> {
    let base = CompiledBase::parse(
        "formulas:\n  per_page: 'price / pages'\n  loop_a: 'formula.loop_b + 1'\n  loop_b: 'formula.loop_a'\n  rounded: 'formula.per_page.round(4)'\n  bad: 'pages +'\n",
    )
    .expect("base");
    let ctx = ctx().with_this("Reading list.md");
    base.evaluate_expr(expr, path, &vault(), &ctx)
}

fn ev(expr: &str) -> Value {
    eval_on(expr, Some("Books/Dune.md")).unwrap_or_else(|e| panic!("{expr}: {e}"))
}

#[test]
fn arithmetic_and_logic() {
    assert_eq!(ev("1 + 2 * 3"), n(7.0));
    assert_eq!(ev("(1 + 2) * 3"), n(9.0));
    assert_eq!(ev("7 % 4"), n(3.0));
    assert_eq!(ev("10 / 4"), n(2.5));
    assert_eq!(ev("-pages"), n(-412.0));
    assert_eq!(ev("'a' + 1"), s("a1"));
    assert_eq!(ev("1 + 'a'"), s("1a"));
    assert_eq!(ev("true && 0"), Value::Bool(false));
    assert_eq!(ev("0 || 'x'"), Value::Bool(true));
    assert_eq!(ev("!missing"), Value::Bool(true));
    assert_eq!(ev("missing + 1"), Value::Null);
    assert_eq!(ev("pages > 400 && price < 10"), Value::Bool(true));
    assert_eq!(ev("status == 'reading'"), Value::Bool(true));
    assert_eq!(ev("status != 'done'"), Value::Bool(true));
    assert_eq!(ev("'b' > 'a'"), Value::Bool(true));
    assert_eq!(ev("missing > 3"), Value::Bool(false));
    assert_eq!(ev("[1, 2] == [1, 2]"), Value::Bool(true));
    assert!(matches!(eval_on("1 / 0", None), Err(EvalError::Type(_))));
    assert!(matches!(eval_on("true * 2", None), Err(EvalError::Type(_))));
}

#[test]
fn names_and_namespaces() {
    assert_eq!(ev("note.pages"), n(412.0));
    assert_eq!(ev("note[\"pages\"]"), n(412.0));
    assert_eq!(ev("pages"), n(412.0));
    assert_eq!(ev("missing"), Value::Null);
    assert_eq!(ev("file.name"), s("Dune.md"));
    assert_eq!(ev("file.basename"), s("Dune"));
    assert_eq!(ev("file.path"), s("Books/Dune.md"));
    assert_eq!(ev("file.folder"), s("Books"));
    assert_eq!(ev("file.ext"), s("md"));
    assert_eq!(ev("file.size"), n(100.0));
    assert_eq!(
        ev("file.tags"),
        Value::list(vec![s("book"), s("sf/classic")])
    );
    assert_eq!(ev("file.links.length"), n(2.0));
    assert_eq!(ev("file.backlinks.length"), n(2.0));
    assert_eq!(ev("file.properties.status"), s("reading"));
    assert_eq!(ev("genres[1]"), s("classic"));
    assert_eq!(ev("genres[-1]"), s("classic"));
    assert_eq!(ev("genres[5]"), Value::Null);
    assert_eq!(ev("status.length"), n(7.0));
    assert_eq!(ev("file.mtime.date() == today() - '1d'"), Value::Bool(true));
}

#[test]
fn formulas() {
    assert_eq!(ev("formula.per_page.toFixed(3)"), s("0.023"));
    assert_eq!(ev("formula.rounded"), n(0.0231));
    assert_eq!(ev("formula['per_page'] > 0"), Value::Bool(true));
    assert!(matches!(
        eval_on("formula.loop_a", Some("Books/Dune.md")),
        Err(EvalError::CircularFormula(_))
    ));
    assert!(matches!(
        eval_on("formula.nope", Some("Books/Dune.md")),
        Err(EvalError::UnknownFormula(_))
    ));
    assert!(matches!(
        eval_on("formula.bad", Some("Books/Dune.md")),
        Err(EvalError::Syntax(_))
    ));
    assert!(matches!(
        eval_on("pages +", Some("Books/Dune.md")),
        Err(EvalError::Syntax(_))
    ));
}

#[test]
fn this_semantics() {
    // `this` is the embedding note (set in the context).
    assert_eq!(ev("this.file.name"), s("Reading list.md"));
    assert_eq!(ev("this.name"), s("Reading list.md"));
    assert_eq!(ev("file.hasLink(this)"), Value::Bool(true));
    assert_eq!(
        eval_on("this.file.hasLink(file)", Some("Books/Dune.md")),
        Ok(Value::Bool(true))
    );
    assert_eq!(ev("file.backlinks.contains(this)"), Value::Bool(true));
    // Without `this`, it is null.
    let base = CompiledBase::parse("").expect("base");
    assert_eq!(
        base.evaluate_expr("this", None, &vault(), &ctx()),
        Ok(Value::Null)
    );
}

#[test]
fn links_and_files() {
    assert_eq!(ev("author == link('Frank Herbert')"), Value::Bool(true));
    assert_eq!(
        ev("author == file('People/Frank Herbert.md')"),
        Value::Bool(true)
    );
    assert_eq!(ev("author.asFile().born"), n(1920.0));
    assert_eq!(ev("author.born"), n(1920.0));
    assert_eq!(ev("author.asFile().name"), s("Frank Herbert.md"));
    assert_eq!(ev("author.linksTo(file)"), Value::Bool(false));
    assert_eq!(
        eval_on(
            "author.linksTo(file('Books/Emma.md'))",
            Some("Books/Emma.md")
        ),
        Ok(Value::Bool(true))
    );
    assert_eq!(ev("file.asLink().toString()"), s("Dune"));
    assert_eq!(ev("file.asLink('D').toString()"), s("D"));
    assert_eq!(ev("link('Dune', 'Shown').toString()"), s("Shown"));
    assert!(matches!(ev("link('https://example.org')"), Value::Url(_)));
    assert_eq!(ev("link('Missing') == link('Missing')"), Value::Bool(true));
    assert_eq!(ev("file('nope')"), Value::Null);
    assert_eq!(ev("author == '[[Frank Herbert]]'"), Value::Bool(true));
    assert_eq!(ev("author.isType('link')"), Value::Bool(true));
}

#[test]
fn file_functions() {
    assert_eq!(ev("file.hasTag('book')"), Value::Bool(true));
    assert_eq!(ev("file.hasTag('#BOOK')"), Value::Bool(true));
    assert_eq!(ev("file.hasTag('sf')"), Value::Bool(true));
    assert_eq!(ev("file.hasTag('s')"), Value::Bool(false));
    assert_eq!(ev("file.hasTag('x', 'book')"), Value::Bool(true));
    assert_eq!(ev("file.inFolder('Books')"), Value::Bool(true));
    assert_eq!(ev("file.inFolder('Books/')"), Value::Bool(true));
    assert_eq!(ev("file.inFolder('Book')"), Value::Bool(false));
    assert_eq!(ev("file.hasProperty('pages')"), Value::Bool(true));
    assert_eq!(ev("file.hasProperty('nope')"), Value::Bool(false));
    assert_eq!(ev("file.hasLink('Reading list')"), Value::Bool(true));
    assert_eq!(ev("file.hasLink(link('Frank Herbert'))"), Value::Bool(true));
    assert_eq!(ev("file.hasLink('Jane Austen')"), Value::Bool(false));
}

#[test]
fn string_functions() {
    assert_eq!(ev("'Hello'.contains('ell')"), Value::Bool(true));
    assert_eq!(ev("'Hello'.containsAll('H', 'o')"), Value::Bool(true));
    assert_eq!(ev("'Hello'.containsAny('x', 'z')"), Value::Bool(false));
    assert_eq!(ev("'Hello'.startsWith('He')"), Value::Bool(true));
    assert_eq!(ev("'Hello'.endsWith('lo')"), Value::Bool(true));
    assert_eq!(ev("'Hello'.lower()"), s("hello"));
    assert_eq!(ev("'Hello'.upper()"), s("HELLO"));
    assert_eq!(ev("'the old man'.title()"), s("The Old Man"));
    assert_eq!(ev("'  x '.trim()"), s("x"));
    assert_eq!(ev("'a-b-c'.replace('-', '+')"), s("a+b+c"));
    assert_eq!(ev("'ab'.repeat(3)"), s("ababab"));
    assert_eq!(ev("'abc'.reverse()"), s("cba"));
    assert_eq!(ev("'abcdef'.slice(1, 3)"), s("bc"));
    assert_eq!(ev("'abcdef'.slice(-2)"), s("ef"));
    assert_eq!(
        ev("'a,b,c'.split(',')"),
        Value::list(vec![s("a"), s("b"), s("c")])
    );
    assert_eq!(
        ev("'a,b,c'.split(',', 2)"),
        Value::list(vec![s("a"), s("b")])
    );
    assert_eq!(ev("'héllo'.length"), n(5.0));
    assert_eq!(ev("'héllo'.length()"), n(5.0));
    assert_eq!(ev("''.isEmpty()"), Value::Bool(true));
    assert_eq!(ev("missing.isEmpty()"), Value::Bool(true));
    assert!(matches!(
        eval_on("'a'.repeat(-1)", None),
        Err(EvalError::Argument(_))
    ));
    assert!(matches!(
        eval_on("'a'.contains()", None),
        Err(EvalError::Argument(_))
    ));
}

#[test]
fn number_functions() {
    assert_eq!(ev("(-2.5).abs()"), n(2.5));
    assert_eq!(ev("(2.1).ceil()"), n(3.0));
    assert_eq!(ev("(2.9).floor()"), n(2.0));
    assert_eq!(ev("(2.5).round()"), n(3.0));
    assert_eq!(ev("(-2.5).round()"), n(-2.0));
    assert_eq!(ev("(2.71828).round(2)"), n(2.72));
    assert_eq!(ev("(2.71828).toFixed(2)"), s("2.72"));
    assert_eq!(ev("(123).toString()"), s("123"));
    assert_eq!(ev("number('42')"), n(42.0));
    assert_eq!(ev("number(true)"), n(1.0));
    assert!(matches!(
        eval_on("number('x')", None),
        Err(EvalError::Argument(_))
    ));
    assert_eq!(ev("max(1, 5, 3)"), n(5.0));
    assert_eq!(ev("min(4, 2)"), n(2.0));
}

#[test]
fn list_functions() {
    assert_eq!(ev("genres.contains('sf')"), Value::Bool(true));
    assert_eq!(ev("genres.containsAll('sf', 'classic')"), Value::Bool(true));
    assert_eq!(ev("genres.containsAny('x', 'sf')"), Value::Bool(true));
    assert_eq!(ev("genres.join(' / ')"), s("sf / classic"));
    assert_eq!(
        ev("[3, 1, 2].sort()"),
        Value::list(vec![n(1.0), n(2.0), n(3.0)])
    );
    assert_eq!(
        ev("['b', 'A', 'c'].sort()"),
        Value::list(vec![s("A"), s("b"), s("c")])
    );
    assert_eq!(ev("[1, 1, 2].unique()"), Value::list(vec![n(1.0), n(2.0)]));
    assert_eq!(
        ev("[1, 2, 3].reverse()"),
        Value::list(vec![n(3.0), n(2.0), n(1.0)])
    );
    assert_eq!(
        ev("[1, 2, 3, 4].slice(1, 3)"),
        Value::list(vec![n(2.0), n(3.0)])
    );
    assert_eq!(ev("[[1], 2, [3, 4]].flat().length"), n(4.0));
    assert_eq!(
        ev("[1, 2, 3, 4].filter(value > 2)"),
        Value::list(vec![n(3.0), n(4.0)])
    );
    assert_eq!(
        ev("[1, 2, 3].map(value * 10 + index)"),
        Value::list(vec![n(10.0), n(21.0), n(32.0)])
    );
    assert_eq!(
        ev("[1, 5, 3].reduce(if(acc == null || value > acc, value, acc), null)"),
        n(5.0)
    );
    assert_eq!(ev("[1, 2, 3, 4].sum()"), n(10.0));
    assert_eq!(ev("[1, 2, 3, 4].mean()"), n(2.5));
    assert_eq!(ev("[5, 1, 3].median()"), n(3.0));
    assert_eq!(ev("[4, 1, 3, 2].median()"), n(2.5));
    assert_eq!(ev("[4, 'x', 9].max()"), n(9.0));
    assert_eq!(ev("[4, 9].min()"), n(4.0));
    assert_eq!(ev("[].mean()"), Value::Null);
    assert_eq!(ev("list('a')"), Value::list(vec![s("a")]));
    assert_eq!(ev("list(genres).length"), n(2.0));
    // Nested lambdas see their own `value`.
    assert_eq!(
        ev("[[1, 2], [3]].map(value.map(value * 2))"),
        Value::list(vec![
            Value::list(vec![n(2.0), n(4.0)]),
            Value::list(vec![n(6.0)])
        ])
    );
    assert_eq!(ev("file.properties.keys().length"), n(7.0));
}

#[test]
fn dates() {
    assert_eq!(
        ev("now().format('YYYY-MM-DD HH:mm')"),
        s("2025-03-10 12:00")
    );
    assert_eq!(
        ev("today().format('YYYY-MM-DD HH:mm')"),
        s("2025-03-10 00:00")
    );
    assert_eq!(
        ev("date('2024-12-01') + '1M' + '4h' + '3m'"),
        Value::Date(Date::parse("2025-01-01 04:03", 0).expect("d"))
    );
    assert_eq!(ev("(now() - '1 week').format('MM-DD')"), s("03-03"));
    assert_eq!(ev("started < today()"), Value::Bool(true));
    assert_eq!(ev("started == '2025-03-01'"), Value::Bool(true));
    assert_eq!(ev("(today() - started).toString()"), s("9 days"));
    assert_eq!(ev("today() - started > duration('1w')"), Value::Bool(true));
    assert_eq!(ev("(now() + duration('1d') * 2).day"), n(12.0));
    assert_eq!(ev("started.year"), n(2025.0));
    assert_eq!(ev("started.month()"), n(3.0));
    assert_eq!(ev("now().time()"), s("12:00:00"));
    assert_eq!(ev("file.mtime.relative()"), s("a day ago"));
    assert_eq!(ev("file.mtime > now() - '1 week'"), Value::Bool(true));
    assert_eq!(ev("started.isEmpty()"), Value::Bool(false));
    assert!(matches!(
        eval_on("date('soon')", None),
        Err(EvalError::Argument(_))
    ));
    assert!(matches!(
        eval_on("now() + 'soon'", None),
        Err(EvalError::Type(_))
    ));
}

#[test]
fn any_and_rich() {
    assert_eq!(ev("pages.isType('number')"), Value::Bool(true));
    assert_eq!(ev("'x'.isType('string')"), Value::Bool(true));
    assert_eq!(ev("0.isTruthy()"), Value::Bool(false));
    assert_eq!(ev("if(price > 5, 'dear', 'cheap')"), s("dear"));
    assert_eq!(ev("if(false, 'x')"), Value::Null);
    // `if` is lazy: the branch not taken may fail.
    assert_eq!(ev("if(true, 1, 1 / 0)"), n(1.0));
    assert_eq!(ev("html('<b>x</b>')"), Value::Html("<b>x</b>".into()));
    assert_eq!(ev("icon('star')"), Value::Icon("star".into()));
    assert_eq!(
        ev("image('Attachments/cover.png')"),
        Value::Image("Attachments/cover.png".into())
    );
    assert_eq!(ev("escapeHTML('<a&b>')"), s("&lt;a&amp;b&gt;"));
}

#[test]
fn unsupported_functions_name_themselves() {
    for (expr, name) in [
        ("random()", "random"),
        ("'x'.matches('y')", "matches"),
        ("missing.frobnicate()", "frobnicate"),
        ("pages.foo(1)", "foo"),
    ] {
        let err = eval_on(expr, Some("Books/Dune.md")).expect_err(expr);
        assert_eq!(err.function(), Some(name), "{expr}");
        assert_eq!(err.to_string(), format!("Unsupported function: {name}()"));
    }
    // A known method on the wrong type is a type error, not "unsupported".
    assert!(matches!(
        eval_on("pages.lower()", Some("Books/Dune.md")),
        Err(EvalError::Type(_))
    ));
}

#[test]
fn deep_formula_chains_stop() {
    let mut yaml = String::from("formulas:\n");
    for i in 0..300 {
        yaml.push_str(&format!("  f{i}: 'formula.f{} + 1'\n", i + 1));
    }
    yaml.push_str("  f300: '1'\n");
    let base = CompiledBase::parse(&yaml).expect("base");
    let r = base.evaluate_expr("formula.f0", Some("Books/Dune.md"), &vault(), &ctx());
    assert_eq!(r, Err(EvalError::TooDeep));
}

const LIBRARY: &str = r#"
filters:
  and:
    - file.inFolder("Books")
    - 'file.ext == "md"'
formulas:
  per_page: '(price / pages).toFixed(3)'
  label: 'if(price, price.toFixed(2) + " EUR", "free")'
  broken: 'pages.nope()'
properties:
  formula.label:
    displayName: Price
views:
  - type: table
    name: All
    order: [file.name, pages, formula.label, formula.broken]
    sort:
      - property: pages
        direction: DESC
    summaries:
      pages: Sum
      formula.label: Filled
  - type: table
    name: Grouped
    groupBy:
      property: status
      direction: ASC
    order: [file.name, pages]
    summaries:
      pages: Average
  - type: cards
    name: Cards
    image: note.cover
    imageAspectRatio: 1.5
    limit: 1
    order: [file.basename]
  - type: kanban
    name: Board
  - type: table
    name: Bad filter
    filters: 'pages +'
  - type: table
    name: Failing filter
    filters: 'pages.nope()'
  - type: list
    name: Ordered groups
    groupBy:
      property: status
    groupOrder: [done, null]
"#;

#[test]
fn table_view() {
    let base = CompiledBase::parse(LIBRARY).expect("base");
    let r = base.evaluate(0, &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::Table);
    assert_eq!(r.notice, None);
    assert_eq!(r.matched, 3);
    assert_eq!(r.shown, 3);
    let labels: Vec<&str> = r.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["file name", "pages", "Price", "broken"]);
    let names: Vec<&str> = r.rows().map(|row| &*row.path).collect();
    // Sorted by pages, descending; the note without pages last.
    assert_eq!(
        names,
        ["Books/Emma.md", "Books/Dune.md", "Books/Notes on Dune.md"]
    );
    let first = r.rows().next().expect("row");
    assert_eq!(first.cells[2], Ok(s("4.00 EUR")));
    // Per-cell error, the rest of the row intact.
    assert_eq!(
        first.cells[3].as_ref().err().and_then(EvalError::function),
        Some("nope")
    );
    assert_eq!(first.cells[1], Ok(n(474.0)));
    let last = r.rows().last().expect("row");
    assert_eq!(last.cells[2], Ok(s("free")));
    assert_eq!(last.cells[1], Ok(Value::Null));
    // Summaries: Sum of pages, Filled of the label.
    assert_eq!(
        r.summaries[1].as_ref().map(|s| s.value.clone()),
        Some(Ok(n(886.0)))
    );
    assert_eq!(
        r.summaries[2].as_ref().map(|s| s.value.clone()),
        Some(Ok(n(3.0)))
    );
    assert_eq!(r.summaries[0], None);
    // The broken formula is reported once per file.
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.source == "formula.broken")
        .expect("diagnostic");
    assert_eq!(d.count, 3);
}

#[test]
fn grouped_view() {
    let base = CompiledBase::parse(LIBRARY).expect("base");
    let r = base.evaluate(1, &vault(), &ctx());
    assert!(r.grouped);
    let keys: Vec<String> = r.groups.iter().map(|g| g.key.display()).collect();
    assert_eq!(keys, ["done", "draft", "reading"]);
    assert_eq!(
        r.groups[0].summaries[1].as_ref().map(|s| s.value.clone()),
        Some(Ok(n(474.0)))
    );
    // Average over the group without numbers is empty.
    assert_eq!(
        r.groups[1].summaries[1].as_ref().map(|s| s.value.clone()),
        Some(Ok(Value::Null))
    );
    assert_eq!(
        r.summaries[1].as_ref().map(|s| s.value.clone()),
        Some(Ok(n(443.0)))
    );
}

#[test]
fn cards_view_and_limit() {
    let base = CompiledBase::parse(LIBRARY).expect("base");
    let r = base.evaluate(2, &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::Cards);
    let cards = r.cards.as_ref().expect("cards");
    assert_eq!(cards.image, Some(PropertyId::Note("cover".into())));
    assert!((cards.aspect_ratio - 1.5).abs() < f64::EPSILON);
    assert_eq!(r.matched, 3);
    assert_eq!(r.shown, 1);
    let row = r.rows().next().expect("row");
    assert_eq!(row.cells[0], Ok(s("Dune")));
    assert_eq!(row.cover, Some(Ok(Value::Null)));
}

#[test]
fn unsupported_layout_has_notice() {
    let base = CompiledBase::parse(LIBRARY).expect("base");
    let r = base.evaluate(3, &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::Kanban);
    assert!(r.notice.as_deref().is_some_and(|n| n.contains("kanban")));
    assert_eq!(r.matched, 3);
}

#[test]
fn bad_filters_exclude_and_report() {
    let base = CompiledBase::parse(LIBRARY).expect("base");
    let r = base.evaluate(4, &vault(), &ctx());
    assert_eq!(r.matched, 0);
    assert!(r.diagnostics.iter().any(|d| d.source == "views[4].filters"));
    let r = base.evaluate(5, &vault(), &ctx());
    assert_eq!(r.matched, 0);
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.source == "views[5].filters")
        .expect("diag");
    assert_eq!(d.count, 3);
    assert!(d.message.contains("Unsupported function: nope()"));
    assert_eq!(base.syntax_errors().len(), 1);
}

#[test]
fn group_order() {
    let base = CompiledBase::parse(LIBRARY).expect("base");
    let r = base.evaluate(6, &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::List);
    let keys: Vec<String> = r.groups.iter().map(|g| g.key.display()).collect();
    assert_eq!(keys, ["done"]);
    assert_eq!(r.shown, 1);
    assert_eq!(r.matched, 3);
}

#[test]
fn default_view_lists_every_file() {
    let base = CompiledBase::parse("").expect("base");
    let r = base.evaluate(0, &vault(), &ctx());
    assert_eq!(r.matched, 8);
    assert_eq!(r.columns.len(), 1);
    assert_eq!(r.columns[0].property, PropertyId::File("name".into()));
    // Natural path order.
    assert_eq!(
        r.rows().next().map(|row| &*row.path),
        Some("Attachments/cover.png")
    );
}

#[test]
fn global_and_view_filters_combine() {
    let base = CompiledBase::parse(
        "filters:\n  or:\n    - file.hasTag('book')\n    - file.hasTag('person')\nviews:\n  - type: table\n    filters:\n      not:\n        - 'born < 1800'\n        - status == 'done'\n",
    )
    .expect("base");
    let r = base.evaluate(0, &vault(), &ctx());
    let names: Vec<&str> = r.rows().map(|row| &*row.path).collect();
    assert_eq!(names, ["Books/Dune.md", "People/Frank Herbert.md"]);
}

#[test]
fn embedded_this_filter() {
    // A `base` block in "Reading list.md": the notes it links to.
    let base = CompiledBase::parse("filters: 'this.file.hasLink(file)'\n").expect("base");
    let r = base.evaluate(0, &vault(), &ctx().with_this("Reading list.md"));
    let names: Vec<&str> = r.rows().map(|row| &*row.path).collect();
    assert_eq!(names, ["Books/Dune.md", "Books/Emma.md"]);
    // Backlinks pane: the files linking to the active file.
    let base = CompiledBase::parse("filters: 'file.hasLink(this.file)'\n").expect("base");
    let r = base.evaluate(0, &vault(), &ctx().with_this("Books/Dune.md"));
    let names: Vec<&str> = r.rows().map(|row| &*row.path).collect();
    assert_eq!(names, ["Books/Notes on Dune.md", "Reading list.md"]);
}

#[test]
fn caps_and_cancellation() {
    let base = CompiledBase::parse("").expect("base");
    let mut c = ctx();
    c.max_rows = 3;
    let r = base.evaluate(0, &vault(), &c);
    assert_eq!(r.incomplete, Some(Incomplete::RowCap));
    assert_eq!(r.matched, 3);
    let mut c = ctx();
    c.cancel = Some(Arc::new(AtomicBool::new(true)));
    let r = base.evaluate(0, &vault(), &c);
    assert_eq!(r.incomplete, Some(Incomplete::Cancelled));
    assert_eq!(r.matched, 0);
    let mut c = ctx();
    c.deadline = Some(std::time::Instant::now());
    assert_eq!(
        base.evaluate(0, &vault(), &c).incomplete,
        Some(Incomplete::TimedOut)
    );
}

#[test]
fn custom_and_default_summaries() {
    let base = CompiledBase::parse(
        "summaries:\n  mean3: 'values.mean().round(3)'\n  oops: 'values.nope()'\nviews:\n  - type: table\n    order: [pages, born, done, status, started, file.mtime]\n    summaries:\n      pages: mean3\n      born: Range\n      done: Checked\n      status: Unique\n      started: Earliest\n      file.mtime: Nonsense\n",
    )
    .expect("base");
    let r = base.evaluate(0, &vault(), &ctx());
    let val = |i: usize| r.summaries[i].as_ref().map(|s| s.value.clone());
    assert_eq!(val(0), Some(Ok(n(443.0))));
    assert_eq!(val(1), Some(Ok(n(145.0))));
    assert_eq!(val(2), Some(Ok(n(1.0))));
    // reading, done, draft, and the empty value.
    assert_eq!(val(3), Some(Ok(n(4.0))));
    assert!(matches!(val(4), Some(Ok(Value::Date(_)))));
    assert!(matches!(val(5), Some(Err(EvalError::Argument(_)))));
}

#[test]
fn covers() {
    assert_eq!(
        Cover::of(&s("#ff0000")),
        Some(Cover::Color("#ff0000".into()))
    );
    assert_eq!(Cover::of(&s("#xyz")), None);
    assert_eq!(
        Cover::of(&Value::Image("https://x/y.png".into())),
        Some(Cover::Url("https://x/y.png".into()))
    );
    assert_eq!(
        Cover::of(&Value::File("a.png".into())),
        Some(Cover::Path("a.png".into()))
    );
}

#[test]
fn local_offset_applies_to_file_times() {
    let base = CompiledBase::parse("").expect("base");
    // UTC+2: the file time (yesterday 12:00 UTC) shows as 14:00.
    let c = EvalContext::new(NOW, 120);
    let r = base.evaluate_expr(
        "file.mtime.format('HH:mm')",
        Some("Books/Dune.md"),
        &vault(),
        &c,
    );
    assert_eq!(r, Ok(s("14:00")));
    let r = base.evaluate_expr("now().format('HH:mm')", None, &vault(), &c);
    assert_eq!(r, Ok(s("14:00")));
}

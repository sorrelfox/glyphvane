// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Bases written the way Obsidian writes them, and the examples of its
//! documentation, evaluated over a small vault: every layout, `this`,
//! unsupported functions reported per cell, unknown keys kept.

use std::sync::Arc;

use gv_bases::{
    CompiledBase, EvalContext, EvalError, FileInfo, LinkInfo, MemoryData, PropertyId, Value,
    ViewKind, ViewResult, YamlValue, text_value,
};

const READING_LIST: &str = include_str!("fixtures/real/reading-list.base");
const BACKLINKS: &str = include_str!("fixtures/real/backlinks.base");
const NEW: &str = include_str!("fixtures/real/new.base");
const EMPTY: &str = include_str!("fixtures/real/empty.base");
const DOCS: &str = include_str!("fixtures/sample.base");

/// 2025-03-10 12:00 UTC.
const NOW: i64 = 1_741_608_000_000;
const DAY: i64 = 86_400_000;

fn ctx() -> EvalContext {
    EvalContext::new(NOW, 0)
}

fn prop(k: &str, v: Value) -> (Arc<str>, Value) {
    (Arc::from(k), v)
}

fn link(text: &str) -> LinkInfo {
    LinkInfo {
        text: text.into(),
        target: None,
    }
}

fn book(path: &str, props: Vec<(Arc<str>, Value)>, links: &[&str]) -> FileInfo {
    let mut f = FileInfo::new(path);
    f.size = Some(2_000);
    f.ctime_ms = Some(NOW - 40 * DAY);
    f.mtime_ms = Some(NOW - 2 * DAY);
    f.properties = props;
    f.tags = vec!["book".into()];
    f.links = links.iter().map(|l| link(l)).collect();
    f
}

fn s(x: &str) -> Value {
    Value::string(x)
}

fn n(x: f64) -> Value {
    Value::Number(x)
}

/// The vault the fixtures run over: front matter as Obsidian stores it
/// (dates and links as text).
fn vault() -> MemoryData {
    MemoryData::new([
        book(
            "Books/Fiction/Dune.md",
            vec![
                prop("author", text_value("[[Frank Herbert]]", 0)),
                prop("pages", n(412.0)),
                prop("pages read", n(103.0)),
                prop("status", s("reading")),
                prop("rating", n(5.0)),
                prop("started", text_value("2025-03-03", 0)),
                prop("cover", s("covers/dune.jpg")),
            ],
            &["Frank Herbert", "Reading list"],
        ),
        book(
            "Books/Fiction/Emma.md",
            vec![
                prop("author", text_value("[[Jane Austen]]", 0)),
                prop("pages", n(474.0)),
                prop("pages read", n(474.0)),
                prop("status", s("done")),
                prop("rating", n(4.0)),
                prop("started", text_value("2025-01-02", 0)),
                prop("finished", text_value("2025-02-14", 0)),
            ],
            &["Jane Austen"],
        ),
        book(
            "Books/Essays/Walden.md",
            vec![
                prop("author", text_value("[[Henry Thoreau]]", 0)),
                prop("pages", n(352.0)),
                prop("pages read", n(88.0)),
                prop("status", s("reading")),
                prop("started", text_value("2025-02-20", 0)),
            ],
            &["Reading list"],
        ),
        book("Books/To read.md", vec![prop("status", s("to read"))], &[]),
        book("Books/cover.png", Vec::new(), &[]),
        book(
            "People/Frank Herbert.md",
            vec![prop("born", n(1920.0))],
            &[],
        ),
        book("People/Jane Austen.md", vec![prop("born", n(1775.0))], &[]),
        book("Reading list.md", Vec::new(), &["Dune"]),
    ])
}

fn compiled(text: &str) -> CompiledBase {
    CompiledBase::parse(text).expect("the fixture is a base")
}

fn view(base: &CompiledBase, name: &str) -> usize {
    base.base().view_index(name).expect("view")
}

fn names(r: &ViewResult) -> Vec<String> {
    r.rows()
        .map(|row| row.path.rsplit('/').next().unwrap_or(&row.path).to_owned())
        .collect()
}

fn column(r: &ViewResult, property: &str) -> usize {
    r.columns
        .iter()
        .position(|c| c.property.to_string() == property)
        .unwrap_or_else(|| panic!("column {property}"))
}

fn cell<'a>(r: &'a ViewResult, row: &str, property: &str) -> &'a Result<Value, EvalError> {
    let c = column(r, property);
    let row = r
        .rows()
        .find(|x| x.path.ends_with(&format!("/{row}")) || &*x.path == row)
        .unwrap_or_else(|| panic!("row {row}"));
    &row.cells[c]
}

#[test]
fn reading_list_loads_without_model_problems() {
    let base = compiled(READING_LIST);
    let b = base.base();
    assert!(b.diagnostics.is_empty(), "{:?}", b.diagnostics);
    let kinds: Vec<&str> = b.views.iter().map(|v| v.kind.as_str()).collect();
    assert_eq!(kinds, ["table", "table", "cards", "list", "kanban", "map"]);
    // The only syntax error is the regular expression.
    let syntax: Vec<String> = base
        .syntax_errors()
        .into_iter()
        .map(|(at, _, e)| format!("{at}: {}", e.message))
        .collect();
    assert_eq!(
        syntax,
        ["formula.initials: regular expressions are not supported"]
    );
}

#[test]
fn unknown_keys_are_kept_in_order() {
    let base = compiled(READING_LIST);
    let b = base.base();
    let top: Vec<&str> = b.extra.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(top, ["x-plugin-state"]);
    let YamlValue::Map(state) = &b.extra[0].1 else {
        panic!("a mapping: {:?}", b.extra[0].1);
    };
    let keys: Vec<&str> = state.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["lastOpened", "pinned"]);
    let reading = &b.views[view(&base, "Reading")];
    let extra: Vec<&str> = reading.extra.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(extra, ["rowHeight"]);
    let map = &b.views[view(&base, "Where")];
    let extra: Vec<&str> = map.extra.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(extra, ["coordinates", "markerIcon"]);
    assert_eq!(map.kind.as_str(), "map");
}

#[test]
fn table_view_evaluates_like_obsidian() {
    let base = compiled(READING_LIST);
    let r = base.evaluate(view(&base, "Reading"), &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::Table);
    // Sorted by progress, highest first (a tie keeps path order).
    assert_eq!(names(&r), ["Walden.md", "Dune.md"]);
    let labels: Vec<&str> = r.columns.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels[..3], ["Title", "Author", "pages"]);
    assert_eq!(r.columns[0].width, Some(260.0));
    assert_eq!(r.columns[1].width, Some(160.0));
    assert_eq!(cell(&r, "Dune.md", "formula.pages_left"), &Ok(n(309.0)));
    assert_eq!(cell(&r, "Dune.md", "formula.progress"), &Ok(n(25.0)));
    assert_eq!(cell(&r, "Walden.md", "formula.progress"), &Ok(n(25.0)));
    assert_eq!(cell(&r, "Dune.md", "formula.age"), &Ok(s("8 days ago")));
    // A link in front matter, followed to the linked note's properties.
    assert_eq!(cell(&r, "Dune.md", "formula.author_born"), &Ok(n(1920.0)));
    assert_eq!(
        cell(&r, "Walden.md", "formula.author_born"),
        &Ok(Value::Null)
    );
    // Summaries of the view.
    let sums: Vec<_> = r
        .summaries
        .iter()
        .flatten()
        .map(|s| (s.name.clone(), s.value.clone()))
        .collect();
    assert_eq!(
        sums,
        [
            ("Sum".into(), Ok(n(764.0))),
            ("Average".into(), Ok(n(25.0)))
        ]
    );
}

#[test]
fn unsupported_functions_are_reported_per_cell() {
    let base = compiled(READING_LIST);
    let r = base.evaluate(view(&base, "Reading"), &vault(), &ctx());
    for row in ["Dune.md", "Walden.md"] {
        match cell(&r, row, "formula.lucky") {
            Err(e) => assert_eq!(e.function(), Some("random"), "{e}"),
            other => panic!("random() is not in the subset: {other:?}"),
        }
        match cell(&r, row, "formula.initials") {
            Err(EvalError::Syntax(e)) => {
                assert_eq!(e.message, "regular expressions are not supported");
            }
            other => panic!("a syntax error: {other:?}"),
        }
        // The other cells of the row are evaluated.
        assert!(cell(&r, row, "formula.progress").is_ok());
    }
    let messages: Vec<&str> = r.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("random")),
        "the problems list names the function: {messages:?}"
    );
    let random = r
        .diagnostics
        .iter()
        .find(|d| d.message.contains("random"))
        .expect("listed");
    assert_eq!(random.count, 2, "once per affected file");
}

#[test]
fn grouped_view_with_summaries() {
    let base = compiled(READING_LIST);
    let r = base.evaluate(view(&base, "By status"), &vault(), &ctx());
    assert!(r.grouped);
    let keys: Vec<String> = r.groups.iter().map(|g| g.key.display()).collect();
    assert_eq!(keys, ["done", "reading", "to read"]);
    let sizes: Vec<usize> = r.groups.iter().map(|g| g.rows.len()).collect();
    assert_eq!(sizes, [1, 2, 1]);
    let reading_avg = r.groups[1].summaries[column(&r, "note.rating")]
        .as_ref()
        .expect("summary");
    assert_eq!(reading_avg.value, Ok(n(5.0)));
}

#[test]
fn cards_view_with_cover_and_size() {
    let base = compiled(READING_LIST);
    let r = base.evaluate(view(&base, "Shelf"), &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::Cards);
    let cards = r.cards.as_ref().expect("cards layout");
    assert_eq!(cards.card_size, Some(180.0));
    assert!((cards.aspect_ratio - 1.5).abs() < 1e-9);
    assert_eq!(cards.image, Some(PropertyId::Note("cover".into())));
    let dune = r
        .rows()
        .find(|row| row.path.ends_with("Dune.md"))
        .expect("row");
    assert_eq!(dune.cover, Some(Ok(s("covers/dune.jpg"))));
    assert_eq!(cell(&r, "Dune.md", "formula.shelf"), &Ok(s("Fiction")));
    assert_eq!(cell(&r, "Walden.md", "formula.shelf"), &Ok(s("Essays")));
}

#[test]
fn list_view_honours_the_limit() {
    let base = compiled(READING_LIST);
    let r = base.evaluate(view(&base, "Titles"), &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::List);
    assert_eq!(r.shown, 2);
    assert_eq!(r.matched, 4);
}

#[test]
fn kanban_and_map_show_a_notice_with_the_rows() {
    let base = compiled(READING_LIST);
    for name in ["Board", "Where"] {
        let r = base.evaluate(view(&base, name), &vault(), &ctx());
        let notice = r.notice.as_deref().expect("a notice");
        assert!(notice.contains("not supported"), "{notice}");
        assert_eq!(r.matched, 4, "{name}");
    }
    let board = base.evaluate(view(&base, "Board"), &vault(), &ctx());
    let keys: Vec<String> = board.groups.iter().map(|g| g.key.display()).collect();
    assert_eq!(keys, ["to read", "reading", "done"], "groupOrder");
}

#[test]
fn object_literals_evaluate() {
    let base = compiled(READING_LIST);
    let v = base.evaluate_expr(
        "formula.meta",
        Some("Books/Fiction/Dune.md"),
        &vault(),
        &ctx(),
    );
    assert_eq!(
        v,
        Ok(Value::Object(
            vec![
                (Arc::from("pages"), n(412.0)),
                (Arc::from("kind"), s("book"))
            ]
            .into()
        ))
    );
    let v = base.evaluate_expr(r#"{"a": 1, b: [2, 3]}.b[1]"#, None, &vault(), &ctx());
    assert_eq!(v, Ok(n(3.0)));
}

#[test]
fn this_is_the_file_given() {
    let base = compiled(BACKLINKS);
    // A sidebar base: `this` is the active note.
    let r = base.evaluate(0, &vault(), &ctx().with_this("Reading list.md"));
    assert_eq!(names(&r), ["Walden.md", "Dune.md"]);
    assert_eq!(
        cell(&r, "Dune.md", "formula.this_name"),
        &Ok(s("Reading list.md"))
    );
    let r = base.evaluate(0, &vault(), &ctx().with_this("Books/Fiction/Dune.md"));
    assert_eq!(names(&r), ["Reading list.md"]);
    // Without `this`, nothing links to it.
    let r = base.evaluate(0, &vault(), &ctx());
    assert_eq!(r.matched, 0);
}

#[test]
fn new_and_empty_bases() {
    let base = compiled(NEW);
    assert_eq!(base.base().views.len(), 1);
    let r = base.evaluate(0, &vault(), &ctx());
    assert_eq!(r.matched, 8, "every file");
    assert_eq!(r.columns.len(), 1, "the file name");
    // An empty file is a base of every file's name.
    let base = compiled(EMPTY);
    assert!(base.base().diagnostics.is_empty());
    let r = base.evaluate(0, &vault(), &ctx());
    assert_eq!(r.kind, ViewKind::Table);
    assert_eq!(r.matched, 8);
}

#[test]
fn documentation_example_runs() {
    let base = compiled(DOCS);
    assert!(base.base().diagnostics.is_empty());
    assert!(base.syntax_errors().is_empty());
    let r = base.evaluate(0, &vault(), &ctx());
    assert!(r.grouped);
    assert_eq!(r.columns.len(), 5);
}

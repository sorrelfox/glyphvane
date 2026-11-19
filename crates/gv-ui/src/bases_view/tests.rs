// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Bases view in headless GPUI windows: reading and evaluating a base,
//! the table, cards and list layouts, view tabs and keys, clicks that
//! open files and follow links, error cells, problems, notices, empty
//! states, covers, stale runs and the content index; what assistive
//! technology is told; the sidebar base panel.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyWindowHandle, Bounds, Entity, Modifiers, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_bases::{FileInfo, Link, MemoryData, Value, ViewKind};

use super::cards::{CardLine, card_lines};
use super::cells::html_text;
use super::table::{Line, lines};
use super::{BaseData, BasesEvent, BasesPanel, BasesStatus, BasesView, count_text, vault_path};
use crate::a11y::{Role, testing};
use crate::reader::Disposition;
use crate::{AppSettings, ThemeChoice};

const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 700.0;

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0xd, 0xa, 0x1a, 0xa, 0x0, 0x0, 0x0, 0xd, 0x49, 0x48, 0x44, 0x52, 0x0,
    0x0, 0x0, 0x1, 0x0, 0x0, 0x0, 0x1, 0x8, 0x4, 0x0, 0x0, 0x0, 0xb5, 0x1c, 0xc, 0x2, 0x0, 0x0,
    0x0, 0xb, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0x0, 0x0, 0x0, 0x6, 0x0, 0x2,
    0x30, 0x81, 0xd0, 0x2f, 0x0, 0x0, 0x0, 0x0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const LIBRARY: &str = r#"
filters: 'file.inFolder("Books")'
formulas:
  left: 'pages - read'
  broken: 'pages.nope()'
  blurb: 'html("<b>Big</b> &amp; <script>alert(1)</script>bold")'
properties:
  formula.left:
    displayName: Pages left
views:
  - type: table
    name: All
    order: [file.name, pages, formula.left, author, tags, done, formula.broken, formula.blurb]
    sort:
      - property: pages
        direction: DESC
    columnSize:
      file.name: 240
    summaries:
      pages: Sum
  - type: table
    name: By status
    groupBy:
      property: status
      direction: ASC
    order: [file.name, pages]
    summaries:
      pages: Sum
  - type: cards
    name: Shelf
    image: note.cover
    imageAspectRatio: 1.5
    cardSize: 180
    order: [file.basename, author]
  - type: list
    name: Plain
    order: [file.basename, author]
  - type: map
    name: Where
"#;

fn s(x: &str) -> Value {
    Value::string(x)
}

fn book(path: &str, pages: f64, read: f64, status: &str, author: Option<&str>) -> FileInfo {
    let mut f = FileInfo::new(path);
    f.properties.push(("pages".into(), Value::Number(pages)));
    f.properties.push(("read".into(), Value::Number(read)));
    f.properties.push(("status".into(), s(status)));
    f.properties
        .push(("done".into(), Value::Bool(status == "done")));
    f.properties
        .push(("tags".into(), Value::list(vec![s("book"), s(status)])));
    if let Some(a) = author {
        f.properties.push((
            "author".into(),
            Value::Link(Arc::new(Link {
                text: a.into(),
                path: (a == "Herbert").then(|| Arc::from("People/Herbert.md")),
                display: None,
            })),
        ));
    }
    f.properties
        .push(("cover".into(), Value::Image("img.png".into())));
    f
}

fn library() -> BaseData {
    Arc::new(MemoryData::new([
        book("Books/Dune.md", 412.0, 100.0, "reading", Some("Herbert")),
        book("Books/Emma.md", 474.0, 474.0, "done", Some("Austen")),
        book("Books/Notes.md", 20.0, 0.0, "draft", None),
        FileInfo::new("People/Herbert.md"),
        FileInfo::new("Other.md"),
    ]))
}

fn fixture(base: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("Bases")).expect("folder");
    std::fs::write(root.join("Bases/Library.base"), base).expect("write");
    std::fs::write(root.join("img.png"), PNG).expect("write");
    dir
}

fn open(cx: &mut TestAppContext, file: &Path, root: &Path) -> (AnyWindowHandle, Entity<BasesView>) {
    let (file, root) = (file.to_path_buf(), root.to_path_buf());
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(WIDTH), px(HEIGHT)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| BasesView::open(file, Some(root), cx))
            .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

/// Opens `base` in a fixture vault, gives it `data` and draws it.
fn shown(
    cx: &mut TestAppContext,
    base: &str,
    data: Option<BaseData>,
) -> (tempfile::TempDir, Entity<BasesView>, VisualTestContext) {
    let dir = fixture(base);
    let (window, view) = open(cx, &dir.path().join("Bases/Library.base"), dir.path());
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    if let Some(data) = data {
        view.update(&mut vcx, |v, cx| v.set_data(data, cx));
    }
    for _ in 0..3 {
        draw(&mut vcx);
    }
    vcx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    (dir, view, vcx)
}

fn events(view: &Entity<BasesView>, vcx: &mut VisualTestContext) -> Rc<RefCell<Vec<BasesEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    vcx.update(|_, cx| {
        cx.subscribe(view, move |_, e: &BasesEvent, _| {
            sink.borrow_mut().push(e.clone())
        })
        .detach();
    });
    seen
}

fn click(vcx: &mut VisualTestContext, selector: &'static str, modifiers: Modifiers) {
    let b = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    vcx.simulate_click(b.center(), modifiers);
    draw(vcx);
}

fn mod_key() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers::command()
    } else {
        Modifiers::control()
    }
}

#[gpui::test]
fn the_table_shows_the_result(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    let (status, this, result) = view.read_with(&vcx, |v, _| {
        (
            v.status().clone(),
            v.this().map(str::to_owned),
            v.result().cloned().expect("evaluated"),
        )
    });
    assert_eq!(status, BasesStatus::Ready);
    assert_eq!(this.as_deref(), Some("Bases/Library.base"));
    assert_eq!(result.kind, ViewKind::Table);
    assert_eq!((result.matched, result.shown), (3, 3));
    assert_eq!(count_text(&result), "3 results");
    let names: Vec<&str> = result.rows().map(|r| &*r.path).collect();
    assert_eq!(names, ["Books/Emma.md", "Books/Dune.md", "Books/Notes.md"]);
    assert_eq!(result.columns[2].label, "Pages left");

    // The header, the rows and the footer are drawn; the name column is
    // as wide as `columnSize` says.
    let name_col = vcx.debug_bounds("bases-header:0").expect("header");
    assert_eq!(name_col.size.width, px(240.0));
    assert!(
        vcx.debug_bounds("bases-header:1")
            .expect("header")
            .size
            .width
            == px(180.0)
    );
    assert!(vcx.debug_bounds("bases-footer").is_some());
    assert!(vcx.debug_bounds("bases-count").is_some());
    // The three rows (the list measures its first item too).
    let drawn = view.read_with(&vcx, |v, _| v.rows_drawn.get());
    assert!((3..=5).contains(&drawn), "{drawn}");
    // The unsupported function's cells say so; the problems chip opens
    // the panel.
    assert!(vcx.debug_bounds("bases-error-cell").is_some());
    let problems = view.read_with(&vcx, |v, _| v.problems());
    assert!(
        problems
            .iter()
            .any(|(at, what)| at == "formula.broken" && what.contains("3 files")),
        "{problems:?}"
    );
    assert!(vcx.debug_bounds("bases-problems").is_none());
    click(&mut vcx, "bases-problems-chip", Modifiers::none());
    assert!(vcx.debug_bounds("bases-problems").is_some());
}

#[gpui::test]
fn clicks_open_files_and_follow_links(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    let seen = events(&view, &mut vcx);
    // Line 0 is Emma: its name (column 0) opens the note.
    click(&mut vcx, "bases-link:0:0:0", Modifiers::none());
    // Line 1 is Dune: its author (column 3) is a link.
    click(&mut vcx, "bases-link:1:3:0", mod_key());
    let seen = seen.borrow().clone();
    assert_eq!(
        seen[0],
        BasesEvent::OpenFile {
            path: dir.path().join("Books").join("Emma.md"),
            fragment: None,
            disposition: Disposition::InPlace,
        }
    );
    match &seen[1] {
        BasesEvent::FollowLink(req) => {
            assert_eq!(&*req.destination, "Herbert");
            assert_eq!(req.kind, gv_model::LinkKind::Wikilink);
            assert_eq!(req.disposition, Disposition::NewTab);
        }
        other => panic!("not a link: {other:?}"),
    }
}

#[gpui::test]
fn tabs_and_keys_switch_views(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    click(&mut vcx, "bases-view-tab:1", Modifiers::none());
    draw(&mut vcx);
    let (index, result) = view.read_with(&vcx, |v, _| (v.view_index(), v.result().cloned()));
    assert_eq!(index, 1);
    let result = result.expect("evaluated");
    assert!(result.grouped);
    // Group headers, rows and group summaries.
    let l = lines(&result);
    assert_eq!(l[0], Line::Group(0));
    assert!(l.contains(&Line::GroupSummary(0)));
    assert_eq!(l.len(), 3 * 3);
    assert!(vcx.debug_bounds("bases-group:0").is_some());

    vcx.simulate_keystrokes("]");
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |v, _| v.view_index()), 2);
    vcx.simulate_keystrokes("[ [ [");
    draw(&mut vcx);
    // Wraps around: 2 → 1 → 0 → 4.
    assert_eq!(view.read_with(&vcx, |v, _| v.view_index()), 4);
}

#[gpui::test]
fn cards_show_covers_and_open_their_file(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    view.update(&mut vcx, |v, cx| assert!(v.select_view_named("Shelf", cx)));
    for _ in 0..4 {
        draw(&mut vcx);
    }
    let (kind, columns, started) = view.read_with(&vcx, |v, _| {
        (
            v.result().map(|r| r.kind.clone()),
            v.card_columns,
            v.covers.started,
        )
    });
    assert_eq!(kind, Some(ViewKind::Cards));
    // 1000 px wide: (1000 - 16) / (180 + 16) = 5 cards a line.
    assert_eq!(columns, 5);
    // One cover file, decoded once.
    assert_eq!(started, 1);
    let card = vcx.debug_bounds("bases-card:0").expect("card");
    assert_eq!(card.size.width, px(180.0));
    let shown = view.update(&mut vcx, |v, cx| {
        v.cover_shown(&gv_bases::Cover::Path("img.png".into()), (180.0, 270.0), cx)
    });
    assert!(matches!(shown, super::cards::CoverShown::Image(_)));
    let seen = events(&view, &mut vcx);
    // Cards are in path order: Dune, Emma, Notes.
    click(&mut vcx, "bases-card:1", Modifiers::none());
    let seen = seen.borrow().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(matches!(
        &seen[0],
        BasesEvent::OpenFile { path, .. } if *path == dir.path().join("Books").join("Emma.md")
    ));
}

#[gpui::test]
fn list_and_unsupported_layouts(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    view.update(&mut vcx, |v, cx| v.select_view_named("Plain", cx));
    draw(&mut vcx);
    let drawn = view.read_with(&vcx, |v, _| v.rows_drawn.get());
    assert!((3..=5).contains(&drawn), "{drawn}");
    assert!(vcx.debug_bounds("bases-notice").is_none());
    view.update(&mut vcx, |v, cx| v.select_view_named("Where", cx));
    draw(&mut vcx);
    // A map: the notice, then the rows as a table.
    assert!(vcx.debug_bounds("bases-notice").is_some());
    assert!(vcx.debug_bounds("bases-header:0").is_some());
}

#[gpui::test]
fn many_rows_are_virtualized(cx: &mut TestAppContext) {
    let files: Vec<FileInfo> = (0..5_000)
        .map(|i| {
            book(
                &format!("Books/B{i:05}.md"),
                f64::from(i),
                0.0,
                "reading",
                None,
            )
        })
        .collect();
    let (_dir, view, vcx) = shown(cx, LIBRARY, Some(Arc::new(MemoryData::new(files))));
    let (matched, drawn) = view.read_with(&vcx, |v, _| {
        (v.result().map(|r| r.matched), v.rows_drawn.get())
    });
    assert_eq!(matched, Some(5_000));
    assert!(drawn > 10 && drawn < 60, "{drawn} rows drawn");
}

#[gpui::test]
fn empty_and_invalid_states(cx: &mut TestAppContext) {
    // No files yet: waiting for the index.
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, None);
    assert!(view.read_with(&vcx, |v, _| v.result().is_none()));
    assert!(vcx.debug_bounds("bases-empty").is_some());
    // Files, none matching.
    view.update(&mut vcx, |v, cx| {
        v.set_data(Arc::new(MemoryData::new([FileInfo::new("Other.md")])), cx);
    });
    draw(&mut vcx);
    assert_eq!(
        view.read_with(&vcx, |v, _| v.result().map(|r| r.matched)),
        Some(0)
    );
    assert!(vcx.debug_bounds("bases-empty").is_some());

    let (_dir, view, _vcx) = shown(cx, "views: [1, 2\n", Some(library()));
    let status = view.read_with(cx, |v, _| v.status().clone());
    assert!(matches!(status, BasesStatus::Invalid(_)), "{status:?}");
}

#[gpui::test]
fn a_newer_run_replaces_a_stale_one(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, None);
    view.update(&mut vcx, |v, cx| {
        v.set_data(library(), cx);
        v.set_data(
            Arc::new(MemoryData::new([book(
                "Books/Only.md",
                1.0,
                0.0,
                "x",
                None,
            )])),
            cx,
        );
    });
    draw(&mut vcx);
    let (evaluations, matched) =
        view.read_with(&vcx, |v, _| (v.evaluations, v.result().map(|r| r.matched)));
    assert_eq!(evaluations, 1);
    assert_eq!(matched, Some(1));
}

#[gpui::test]
fn reloading_keeps_the_view_shown(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    view.update(&mut vcx, |v, cx| v.select_view_named("Plain", cx));
    draw(&mut vcx);
    // The views are reordered on disk.
    let reordered = LIBRARY
        .replace("  - type: map\n    name: Where\n", "")
        .replacen("views:\n", "views:\n  - type: map\n    name: Where\n", 1);
    let path = dir.path().join("Bases/Library.base");
    std::fs::write(&path, reordered).expect("write");
    view.update(&mut vcx, |v, cx| v.load(path, cx));
    draw(&mut vcx);
    let name = view.read_with(&vcx, |v, _| {
        v.base()
            .and_then(|b| b.base().views.get(v.view_index()))
            .and_then(|view| view.name.clone())
    });
    assert_eq!(name.as_deref(), Some("Plain"));

    // Another vault root: `this` follows.
    let root = dir.path().join("Bases");
    view.update(&mut vcx, |v, cx| v.set_vault_root(Some(root), cx));
    draw(&mut vcx);
    let (this, shown) = view.read_with(&vcx, |v, _| {
        (v.this().map(str::to_owned), v.path().map(Path::to_path_buf))
    });
    assert_eq!(this.as_deref(), Some("Library.base"));
    assert_eq!(shown, Some(dir.path().join("Bases/Library.base")));
}

#[gpui::test]
fn embedded_bases_use_the_embedding_note(cx: &mut TestAppContext) {
    let text: Arc<str> = Arc::from(
        "filters: 'file.hasLink(this.file)'\nviews:\n  - type: table\n  - type: list\n    name: Linked\n",
    );
    let mut linker = FileInfo::new("A.md");
    linker.links.push(gv_bases::LinkInfo {
        text: "Home".into(),
        target: None,
    });
    let data: BaseData = Arc::new(MemoryData::new([
        linker,
        FileInfo::new("B.md"),
        FileInfo::new("Home.md"),
    ]));
    let view = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        BasesView::embedded(
            text,
            Some("Home.md".into()),
            Some("Linked".into()),
            None,
            cx,
        )
    });
    view.update(cx, |v, cx| v.set_data(data, cx));
    cx.run_until_parked();
    let (index, paths) = view.read_with(cx, |v, _| {
        (
            v.view_index(),
            v.result()
                .map(|r| r.rows().map(|row| row.path.to_string()).collect::<Vec<_>>()),
        )
    });
    assert_eq!(index, 1);
    assert_eq!(paths, Some(vec!["A.md".to_owned()]));
}

#[gpui::test]
fn the_content_index_feeds_the_view(cx: &mut TestAppContext) {
    use std::time::Duration;

    use gv_vault::content::{ContentIndexer, ContentOptions};
    use gv_vault::{IndexConfig, PathIndex, ProfileId};

    let dir = fixture("filters: 'file.hasTag(\"book\")'\n");
    std::fs::create_dir_all(dir.path().join("Books")).expect("folder");
    std::fs::write(
        dir.path().join("Books/Dune.md"),
        "---\ntags: [book]\npages: 412\n---\n",
    )
    .expect("write");
    std::fs::write(dir.path().join("Other.md"), "nothing\n").expect("write");
    let paths = PathIndex::build(dir.path(), IndexConfig::default())
        .expect("index")
        .0;
    let indexer = ContentIndexer::start(
        paths,
        ContentOptions::for_profile(ProfileId::Obsidian),
        |_| {},
    )
    .expect("starts");
    assert!(indexer.flush(Duration::from_secs(10)));
    let snapshot = indexer.snapshot().expect("snapshot");
    let (window, view) = open(cx, &dir.path().join("Bases/Library.base"), dir.path());
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    view.update(&mut vcx, |v, cx| v.set_content(snapshot, cx));
    draw(&mut vcx);
    let paths = view.read_with(&vcx, |v, _| {
        v.result()
            .map(|r| r.rows().map(|row| row.path.to_string()).collect::<Vec<_>>())
    });
    assert_eq!(paths, Some(vec!["Books/Dune.md".to_owned()]));
}

#[test]
fn html_cells_show_text_only() {
    assert_eq!(
        html_text("<b>Big</b> &amp; <script>alert(1)</script>bold"),
        "Big & bold"
    );
    assert_eq!(
        html_text("<p>a</p><p>b&#x21;</p><style>p{}</style><!-- c -->"),
        "a b!"
    );
    assert_eq!(html_text("<img src=x onerror=alert(1)>"), "");
    assert_eq!(html_text("1 < 2"), "1");
    assert_eq!(html_text("&bogus; &lt;i&gt;"), "&bogus; <i>");
}

#[test]
fn counts_paths_and_card_lines() {
    let result = gv_bases::CompiledBase::parse(LIBRARY)
        .expect("base")
        .evaluate(2, &*library(), &gv_bases::EvalContext::new(0, 0));
    assert_eq!(count_text(&result), "3 results");
    let mut limited = result.clone();
    limited.shown = 1;
    assert_eq!(count_text(&limited), "1 of 3 results");
    assert_eq!(
        card_lines(&result, 2),
        [CardLine::Cards(0, 0, 2), CardLine::Cards(0, 2, 3)]
    );
    assert_eq!(
        &*vault_path(Path::new("/v/a/b.base"), Some(Path::new("/v"))),
        "a/b.base"
    );
    assert_eq!(&*vault_path(&PathBuf::from("/x/b.base"), None), "b.base");
}

/// The one element with `role` named `label`.
fn role_named(vcx: &mut VisualTestContext, role: Role, label: &str) -> testing::Node {
    let found: Vec<testing::Node> = testing::with_role(vcx, role)
        .into_iter()
        .filter(|n| n.label.as_deref() == Some(label))
        .collect();
    assert_eq!(found.len(), 1, "one {role:?} named {label:?}");
    found.into_iter().next().expect("checked")
}

fn labels(nodes: &[testing::Node]) -> Vec<String> {
    nodes.iter().filter_map(|n| n.label.clone()).collect()
}

#[gpui::test]
fn the_table_is_a_table_for_assistive_technology(cx: &mut TestAppContext) {
    let (_dir, _view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    // The table is named after its view, as its tab is.
    role_named(&mut vcx, Role::Table, "All");
    // The views are tabs.
    let tabs = testing::named(&mut vcx, "Views");
    assert_eq!(tabs.role, Some(Role::TabList));
    let tab_nodes = testing::with_role(&mut vcx, Role::Tab);
    assert_eq!(
        labels(&tab_nodes),
        ["All", "By status", "Shelf", "Plain", "Where"]
    );
    assert_eq!(tab_nodes[0].selected, Some(true));
    assert_eq!(tab_nodes[0].position, Some((1, 5)));
    assert_eq!(tab_nodes[1].selected, Some(false));
    assert!(tab_nodes.iter().all(|t| t.focusable && t.clickable));
    // Column headers, then a row per file with a cell per column.
    let headers = labels(&testing::with_role(&mut vcx, Role::ColumnHeader));
    assert_eq!(headers[..3], ["file name", "pages", "Pages left"]);
    let rows = labels(&testing::with_role(&mut vcx, Role::Row));
    assert!(
        rows.starts_with(&["Emma.md".to_owned(), "Dune.md".to_owned()]),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.starts_with("Summaries: pages Sum 906")),
        "{rows:?}"
    );
    let cells = labels(&testing::with_role(&mut vcx, Role::Cell));
    assert_eq!(cells[..4], ["Emma.md", "474", "0", "Austen"]);
    assert!(cells.contains(&"book, done".to_owned()), "{cells:?}");
    assert!(cells.contains(&"checked".to_owned()));
    assert!(cells.contains(&"not checked".to_owned()));
    assert!(
        cells
            .iter()
            .any(|c| c.starts_with("Error: Unsupported function: nope()")),
        "{cells:?}"
    );
    // `html()` is read as its text.
    assert!(cells.contains(&"Big & bold".to_owned()), "{cells:?}");
    // The count is a polite status; the problems chip a button.
    let count = testing::named(&mut vcx, "3 results");
    assert_eq!(count.role, Some(Role::Status));
    assert!(count.live.is_some());
    let chip = testing::with_role(&mut vcx, Role::Button)
        .into_iter()
        .find(|b| {
            b.label
                .as_deref()
                .is_some_and(|l| l.ends_with("problems") || l.ends_with("problem"))
        })
        .expect("the problems chip");
    assert_eq!(chip.expanded, Some(false));
    assert!(chip.focusable);
    click(&mut vcx, "bases-problems-chip", Modifiers::none());
    let chip_label = chip.label.expect("named");
    assert_eq!(testing::named(&mut vcx, &chip_label).expanded, Some(true));
    let panel = testing::named(&mut vcx, "Problems");
    assert_eq!(panel.role, Some(Role::List));
}

#[gpui::test]
fn grouped_tables_name_their_groups(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    view.update(&mut vcx, |v, cx| v.select_view(1, cx));
    draw(&mut vcx);
    draw(&mut vcx);
    let rows = labels(&testing::with_role(&mut vcx, Role::Row));
    assert!(rows.contains(&"Group done, 1 file".to_owned()), "{rows:?}");
    assert!(
        rows.iter().any(|r| r.starts_with("Summaries: pages Sum")),
        "{rows:?}"
    );
    assert_eq!(
        testing::with_role(&mut vcx, Role::Tab)[1].selected,
        Some(true)
    );
}

#[gpui::test]
fn cards_and_lists_are_lists(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    view.update(&mut vcx, |v, cx| v.select_view(2, cx));
    for _ in 0..3 {
        draw(&mut vcx);
    }
    role_named(&mut vcx, Role::List, "Shelf");
    let cards = testing::with_role(&mut vcx, Role::ListItem);
    assert_eq!(labels(&cards), ["Dune", "Emma", "Notes"]);
    assert_eq!(cards[1].position, Some((2, 3)));
    assert_eq!(cards[0].description.as_deref(), Some("author: Herbert"));
    assert!(cards.iter().all(|c| c.focusable && c.clickable));

    view.update(&mut vcx, |v, cx| v.select_view(3, cx));
    for _ in 0..3 {
        draw(&mut vcx);
    }
    role_named(&mut vcx, Role::List, "Plain");
    let items = testing::with_role(&mut vcx, Role::ListItem);
    assert_eq!(labels(&items), ["Dune, Herbert", "Emma, Austen", "Notes"]);
    assert_eq!(items[2].position, Some((3, 3)));
}

const BACKLINKS: &str = r#"
filters:
  and:
    - 'file.hasLink(this.file)'
views:
  - type: table
    name: Linked here
    order: [file.name]
  - type: list
    name: As a list
    order: [file.name]
"#;

fn linked_vault() -> BaseData {
    let mut a = FileInfo::new("Notes/A.md");
    a.links.push(gv_bases::LinkInfo {
        text: "Hub".into(),
        target: Some("Hub.md".into()),
    });
    let mut b = FileInfo::new("Notes/B.md");
    b.links.push(gv_bases::LinkInfo {
        text: "Hub".into(),
        target: Some("Hub.md".into()),
    });
    b.links.push(gv_bases::LinkInfo {
        text: "Other".into(),
        target: Some("Other.md".into()),
    });
    Arc::new(MemoryData::new([
        a,
        b,
        FileInfo::new("Hub.md"),
        FileInfo::new("Other.md"),
        FileInfo::new("Bases/Backlinks.base"),
        FileInfo::new("Bases/Library.base"),
    ]))
}

fn open_panel(
    cx: &mut TestAppContext,
    root: &Path,
) -> (AnyWindowHandle, Entity<BasesPanel>, VisualTestContext) {
    let root = root.to_path_buf();
    let (window, panel) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(320.0), px(HEIGHT)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| BasesPanel::new(Some(root), cx))
            .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    (window, panel, vcx)
}

fn shown_names(panel: &Entity<BasesPanel>, vcx: &mut VisualTestContext) -> Vec<String> {
    panel.read_with(vcx, |p, cx| {
        p.view()
            .and_then(|v| v.read(cx).result().cloned())
            .map(|r| r.rows().map(|row| row.path.to_string()).collect())
            .unwrap_or_default()
    })
}

#[gpui::test]
fn the_sidebar_base_follows_the_active_file(cx: &mut TestAppContext) {
    let dir = fixture(LIBRARY);
    std::fs::write(dir.path().join("Bases/Backlinks.base"), BACKLINKS).expect("write");
    let (_window, panel, mut vcx) = open_panel(cx, dir.path());
    // Nothing chosen yet.
    assert!(vcx.debug_bounds("bases-panel-empty").is_some());
    panel.update(&mut vcx, |p, cx| {
        p.set_bases(
            vec!["Bases/Library.base".into(), "Bases/Backlinks.base".into()],
            cx,
        );
        p.set_data(linked_vault(), cx);
        p.set_active_path(Some(&dir.path().join("Hub.md")), cx);
    });
    draw(&mut vcx);
    assert_eq!(
        panel.read_with(&vcx, |p, _| p.bases().to_vec()),
        [
            Arc::from("Bases/Backlinks.base"),
            Arc::from("Bases/Library.base")
        ]
    );
    assert_eq!(
        panel
            .read_with(&vcx, |p, _| p.active().map(str::to_owned))
            .as_deref(),
        Some("Hub.md")
    );

    // The picker lists the bases; choosing one shows it for the active file.
    let picker = testing::named(&mut vcx, "Choose a base");
    assert_eq!(
        (picker.role, picker.expanded),
        (Some(Role::Button), Some(false))
    );
    click(&mut vcx, "bases-panel-picker", Modifiers::none());
    let options = testing::with_role(&mut vcx, Role::ListBoxOption);
    assert_eq!(
        labels(&options),
        ["Bases/Backlinks.base", "Bases/Library.base"]
    );
    click(&mut vcx, "bases-panel-option:0", Modifiers::none());
    for _ in 0..3 {
        draw(&mut vcx);
    }
    assert!(!panel.read_with(&vcx, |p, _| p.picker_open()));
    assert_eq!(
        panel
            .read_with(&vcx, |p, _| p.chosen().map(str::to_owned))
            .as_deref(),
        Some("Bases/Backlinks.base")
    );
    assert!(testing::has(&mut vcx, "Base: Backlinks"));
    assert!(testing::has(&mut vcx, "For Hub.md"));
    let this = panel.read_with(&vcx, |p, cx| {
        p.view().and_then(|v| v.read(cx).this().map(str::to_owned))
    });
    assert_eq!(this.as_deref(), Some("Hub.md"), "not the base file");
    assert_eq!(shown_names(&panel, &mut vcx), ["Notes/A.md", "Notes/B.md"]);

    // Another active file: evaluated again for it.
    panel.update(&mut vcx, |p, cx| {
        p.set_active_path(Some(&dir.path().join("Other.md")), cx);
    });
    for _ in 0..3 {
        draw(&mut vcx);
    }
    assert_eq!(shown_names(&panel, &mut vcx), ["Notes/B.md"]);
    assert!(testing::has(&mut vcx, "For Other.md"));
    // A file outside the vault, or none: `this` is empty.
    panel.update(&mut vcx, |p, cx| {
        p.set_active_path(Some(Path::new("/elsewhere/x.md")), cx)
    });
    for _ in 0..3 {
        draw(&mut vcx);
    }
    assert!(shown_names(&panel, &mut vcx).is_empty());
    assert!(testing::has(&mut vcx, "No active file"));

    // The base's views are its tabs; choosing one through the panel works.
    panel.update(&mut vcx, |p, cx| p.choose_view(1, cx));
    let index = panel.read_with(&vcx, |p, cx| p.view().map(|v| v.read(cx).view_index()));
    assert_eq!(index, Some(1));
}

#[gpui::test]
fn the_sidebar_base_passes_on_open_requests(cx: &mut TestAppContext) {
    let dir = fixture(LIBRARY);
    std::fs::write(dir.path().join("Bases/Backlinks.base"), BACKLINKS).expect("write");
    let (_window, panel, mut vcx) = open_panel(cx, dir.path());
    panel.update(&mut vcx, |p, cx| {
        p.set_data(linked_vault(), cx);
        p.set_active(Some("Hub.md".into()), cx);
        p.choose_base("Bases/Backlinks.base".into(), cx);
    });
    for _ in 0..3 {
        draw(&mut vcx);
    }
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&panel, move |_, e: &BasesEvent, _| {
            sink.borrow_mut().push(e.clone())
        })
        .detach();
    });
    click(&mut vcx, "bases-link:0:0:0", Modifiers::none());
    assert_eq!(
        seen.borrow().clone(),
        [BasesEvent::OpenFile {
            path: dir.path().join("Notes").join("A.md"),
            fragment: None,
            disposition: Disposition::InPlace,
        }]
    );
}

#[gpui::test]
fn a_vault_without_bases_says_so(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let (_window, panel, mut vcx) = open_panel(cx, dir.path());
    click(&mut vcx, "bases-panel-picker", Modifiers::none());
    assert!(panel.read_with(&vcx, |p, _| p.picker_open()));
    assert!(vcx.debug_bounds("bases-panel-list").is_some());
    assert!(testing::with_role(&mut vcx, Role::ListBoxOption).is_empty());
}

// ----- icons and remote covers ------------------------------------------------------

const ICONS: &str = r#"
formulas:
  up: 'icon("arrow-up")'
  down: 'icon("Lucide-Arrow-Down")'
  odd: 'icon("no-such-icon")'
views:
  - type: table
    name: Icons
    order: [file.name, formula.up, formula.down, formula.odd]
"#;

#[test]
fn icon_names_are_found_in_the_icon_set() {
    use super::cells::icon_path;
    assert_eq!(icon_path("arrow-up").as_deref(), Some("icons/arrow-up.svg"));
    assert_eq!(
        icon_path(" Lucide-Arrow-Down ").as_deref(),
        Some("icons/arrow-down.svg")
    );
    for unknown in ["no-such-icon", "", "../x", "arrow up", "icons/arrow-up.svg"] {
        assert_eq!(icon_path(unknown), None, "{unknown}");
    }
}

#[gpui::test]
fn icon_cells_draw_icons_or_their_name(cx: &mut TestAppContext) {
    let data: BaseData = Arc::new(MemoryData::new([FileInfo::new("A.md")]));
    let (_dir, _view, mut vcx) = shown(cx, ICONS, Some(data));
    assert!(vcx.debug_bounds("bases-icon:arrow-up").is_some());
    assert!(vcx.debug_bounds("bases-icon:Lucide-Arrow-Down").is_some());
    assert!(
        vcx.debug_bounds("bases-icon:no-such-icon").is_none(),
        "an unknown name is text"
    );
    assert!(testing::has(&mut vcx, "no-such-icon icon"));
}

/// Serves one PNG on the loopback interface and counts the requests.
fn png_server() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("address").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                    break;
                }
            }
            count.fetch_add(1, Ordering::SeqCst);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                PNG.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(PNG);
        }
    });
    (format!("http://127.0.0.1:{port}/cover.png"), hits)
}

const REMOTE_SHELF: &str = r#"
views:
  - type: cards
    name: Shelf
    image: note.cover
    order: [file.basename]
"#;

#[gpui::test]
fn remote_covers_wait_for_load_once(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    let (url, hits) = png_server();
    let cache = tempfile::tempdir().expect("cache dir");
    cx.update(|cx| {
        crate::reader::remote::RemoteSession::use_cache_dir(cx, cache.path().to_path_buf());
    });
    let mut f = FileInfo::new("Dune.md");
    f.properties
        .push(("cover".into(), Value::Image(url.as_str().into())));
    let data: BaseData = Arc::new(MemoryData::new([f]));
    let (_dir, view, mut vcx) = shown(cx, REMOTE_SHELF, Some(data));
    // Ask (the default): the cover waits, nothing is fetched.
    let cover = gv_bases::Cover::Url(url.as_str().into());
    let waiting = view.update(&mut vcx, |v, cx| v.cover_shown(&cover, (200.0, 200.0), cx));
    assert!(matches!(
        &waiting,
        super::cards::CoverShown::Waiting { host, .. } if host.as_ref() == "127.0.0.1"
    ));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert!(testing::has(&mut vcx, "Load once from 127.0.0.1"));

    // Load once: fetched for this base, and shown; the card is not opened.
    let seen = events(&view, &mut vcx);
    click(&mut vcx, "bases-load-once:0:0", Modifiers::none());
    for _ in 0..6 {
        draw(&mut vcx);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let shown = view.update(&mut vcx, |v, cx| v.cover_shown(&cover, (200.0, 200.0), cx));
    assert!(matches!(shown, super::cards::CoverShown::Image(_)));
    assert!(seen.borrow().is_empty(), "{:?}", seen.borrow());
    assert!(vcx.debug_bounds("bases-load-once:0:0").is_none());
    // Nothing was written to the settings.
    vcx.update(|_, cx| {
        let allow = cx
            .global::<AppSettings>()
            .settings()
            .privacy_and_security
            .remote_allow_sites
            .clone();
        assert!(allow.is_empty());
    });
}

#[test]
fn the_cursor_moves_over_rows_only() {
    use super::cursor::{PAGE, Step, step_column, step_row};
    let lines = [
        Line::Group(0),
        Line::Row(0, 0),
        Line::Row(0, 1),
        Line::GroupSummary(0),
        Line::Group(1),
        Line::Row(1, 0),
    ];
    // The first move goes to the first row (End to the last).
    assert_eq!(step_row(&lines, None, Step::Next), Some(1));
    assert_eq!(step_row(&lines, None, Step::Previous), Some(1));
    assert_eq!(step_row(&lines, None, Step::Last), Some(5));
    // Group headers and summaries are passed over; the ends stop it.
    assert_eq!(step_row(&lines, Some(1), Step::Next), Some(2));
    assert_eq!(step_row(&lines, Some(2), Step::Next), Some(5));
    assert_eq!(step_row(&lines, Some(5), Step::Next), Some(5));
    assert_eq!(step_row(&lines, Some(5), Step::Previous), Some(2));
    assert_eq!(step_row(&lines, Some(1), Step::Previous), Some(1));
    assert_eq!(step_row(&lines, Some(5), Step::First), Some(1));
    assert_eq!(step_row(&lines, Some(1), Step::PageDown), Some(5));
    assert_eq!(step_row(&lines, Some(5), Step::PageUp), Some(1));
    // From a line that is no longer a row: the next row.
    assert_eq!(step_row(&lines, Some(3), Step::Previous), Some(2));
    // No rows, no cursor.
    assert_eq!(step_row(&[Line::Group(0)], None, Step::Next), None);
    // Pages of PAGE rows.
    let many: Vec<Line> = (0..30).map(|r| Line::Row(0, r)).collect();
    assert_eq!(step_row(&many, Some(0), Step::PageDown), Some(PAGE));
    assert_eq!(step_row(&many, Some(25), Step::PageDown), Some(29));
    // Columns stop at the ends.
    assert_eq!(step_column(0, 3, false), 0);
    assert_eq!(step_column(0, 3, true), 1);
    assert_eq!(step_column(2, 3, true), 2);
    assert_eq!(step_column(0, 0, true), 0);
}

/// The selected cells' names.
fn selected_cells(vcx: &mut VisualTestContext) -> Vec<String> {
    labels(
        &testing::with_role(vcx, Role::Cell)
            .into_iter()
            .filter(|c| c.selected == Some(true))
            .collect::<Vec<_>>(),
    )
}

/// The selected rows' names.
fn selected_rows(vcx: &mut VisualTestContext, role: Role) -> Vec<String> {
    labels(
        &testing::with_role(vcx, role)
            .into_iter()
            .filter(|c| c.selected == Some(true))
            .collect::<Vec<_>>(),
    )
}

#[gpui::test]
fn the_keyboard_moves_over_the_table(cx: &mut TestAppContext) {
    let (dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    let seen = events(&view, &mut vcx);
    // The view is a named group that takes the keyboard.
    let root = role_named(&mut vcx, Role::Group, "Base Library");
    assert!(root.focusable);
    // No row is selected before a key moves the cursor.
    assert!(selected_rows(&mut vcx, Role::Row).is_empty());
    assert!(selected_cells(&mut vcx).is_empty());

    vcx.simulate_keystrokes("down");
    draw(&mut vcx);
    assert_eq!(selected_rows(&mut vcx, Role::Row), ["Emma.md"]);
    assert_eq!(selected_cells(&mut vcx), ["Emma.md"]);
    // The cell knows its row and column.
    let cell = testing::with_role(&mut vcx, Role::Cell)
        .into_iter()
        .find(|c| c.selected == Some(true))
        .expect("the cursor's cell");
    assert_eq!(cell.label.as_deref(), Some("Emma.md"));

    vcx.simulate_keystrokes("right");
    draw(&mut vcx);
    assert_eq!(selected_cells(&mut vcx), ["474"]);
    vcx.simulate_keystrokes("down");
    draw(&mut vcx);
    assert_eq!(selected_rows(&mut vcx, Role::Row), ["Dune.md"]);
    assert_eq!(selected_cells(&mut vcx), ["412"]);
    vcx.simulate_keystrokes("left left left");
    draw(&mut vcx);
    assert_eq!(selected_cells(&mut vcx), ["Dune.md"]);
    vcx.simulate_keystrokes("end");
    draw(&mut vcx);
    assert_eq!(selected_rows(&mut vcx, Role::Row), ["Notes.md"]);
    vcx.simulate_keystrokes("down");
    draw(&mut vcx);
    assert_eq!(selected_rows(&mut vcx, Role::Row), ["Notes.md"]);

    // Enter opens the row's file.
    vcx.simulate_keystrokes("enter");
    draw(&mut vcx);
    assert_eq!(
        seen.borrow().last(),
        Some(&BasesEvent::OpenFile {
            path: dir.path().join("Books").join("Notes.md"),
            fragment: None,
            disposition: Disposition::InPlace,
        })
    );

    // A new evaluation keeps the cursor on its file.
    vcx.simulate_keystrokes("home");
    draw(&mut vcx);
    assert_eq!(selected_rows(&mut vcx, Role::Row), ["Emma.md"]);
    view.update(&mut vcx, |v, cx| v.set_data(library(), cx));
    for _ in 0..3 {
        draw(&mut vcx);
    }
    assert_eq!(selected_rows(&mut vcx, Role::Row), ["Emma.md"]);

    // Another view starts afresh; a list's item is selected.
    view.update(&mut vcx, |v, cx| v.select_view_named("Plain", cx));
    for _ in 0..3 {
        draw(&mut vcx);
    }
    assert_eq!(view.read_with(&vcx, |v, _| v.cursor()), None);
    assert!(selected_rows(&mut vcx, Role::ListItem).is_empty());
    vcx.simulate_keystrokes("down down");
    draw(&mut vcx);
    assert_eq!(selected_rows(&mut vcx, Role::ListItem), ["Emma, Austen"]);
    // Left and Right do nothing in a list.
    vcx.simulate_keystrokes("right");
    draw(&mut vcx);
    assert_eq!(view.read_with(&vcx, |v, _| v.cursor()), Some((1, 0)));
}

#[gpui::test]
fn row_keys_leave_the_tabs_alone(cx: &mut TestAppContext) {
    let (_dir, view, mut vcx) = shown(cx, LIBRARY, Some(library()));
    let seen = events(&view, &mut vcx);
    vcx.simulate_keystrokes("down");
    draw(&mut vcx);
    // Tab to the view tabs: Enter there is the tab's, not the row's.
    vcx.simulate_keystrokes("tab");
    draw(&mut vcx);
    assert_eq!(testing::focused(&mut vcx), Some(Some("All".to_owned())));
    vcx.simulate_keystrokes("enter down");
    draw(&mut vcx);
    assert!(seen.borrow().is_empty(), "{:?}", seen.borrow());
    assert_eq!(view.read_with(&vcx, |v, _| v.cursor()), Some((0, 0)));
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Bases view in headless GPUI windows: reading and evaluating a base,
//! the table, cards and list layouts, view tabs and keys, clicks that
//! open files and follow links, error cells, problems, notices, empty
//! states, covers, stale runs and the content index.

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
use super::{BaseData, BasesEvent, BasesStatus, BasesView, count_text, vault_path};
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

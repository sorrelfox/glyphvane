// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Search panel: its results model, and the panel in a headless GPUI
//! window over a temporary vault.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_search::{CancelToken, Query, SearchOptions, VaultSource};
use gv_vault::content::{ContentOptions, ContentSnapshot, DiskSource};
use gv_vault::index::{IndexConfig, PathIndex};
use gv_vault::profile::ProfileId;

use super::model::{Progress, Results, Row};
use super::{
    Collapse, Dismiss, Expand, OpenSelected, OpenSelectedInNewTab, SearchPanel, SearchPanelEvent,
    SearchScope, SearchToggle, SelectNext, SelectPrevious,
};
use crate::reader::Disposition;
use crate::{AppSettings, ThemeChoice};

const NOTES: &[(&str, &str)] = &[
    (
        "Apples.md",
        "# Fruit\n\n## Red\n\nApples are red.\n\nGreen apples too.\n",
    ),
    (
        "Pears.md",
        "# Pears\n\nPears are not apples, said the apple.\n",
    ),
    ("Other.md", "Nothing to see. Apple pie\n"),
    ("Code.md", "Text with a / slash and a(b) call.\n"),
];

fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    dir
}

fn snapshot(root: &Path) -> ContentSnapshot {
    let paths = PathIndex::build(root, IndexConfig::default())
        .expect("index")
        .0;
    let options = ContentOptions::for_profile(ProfileId::Obsidian);
    let source = DiskSource::new(root, &options);
    ContentSnapshot::build(paths, options, &source, &AtomicBool::new(false)).expect("content")
}

fn results_for(root: &Path, content: &ContentSnapshot, query: &str) -> Results {
    let query = Query::new(query, false).expect("query");
    let source = VaultSource::new(
        content.clone(),
        std::sync::Arc::new(DiskSource::new(root, content.options())),
    );
    let (files, summary) = gv_search::search_all(
        &query,
        &source,
        &SearchOptions::default(),
        &CancelToken::new(),
    );
    let mut results = Results::default();
    results.add(files, Some(content));
    results.progress = Progress::Done(summary);
    results
}

// ----- the model ------------------------------------------------------------------

#[test]
fn results_are_ranked_grouped_and_folded() {
    let dir = vault(NOTES);
    let content = snapshot(dir.path());
    let mut results = results_for(dir.path(), &content, "apple");
    let names: Vec<&str> = results.files().iter().map(|f| f.path.as_str()).collect();
    // The file name matches first.
    assert_eq!(names[0], "Apples.md");
    assert_eq!(names.len(), 3);
    assert_eq!(results.summary(), "5 results in 3 files");
    // A row per note, then one per context line.
    assert_eq!(results.rows()[0], Row::File(0));
    assert!(matches!(results.rows()[1], Row::Match(0, 0)));
    let all = results.rows().len();

    results.toggle(0);
    assert!(results.is_folded(0));
    assert_eq!(
        results.rows().len(),
        all - results.files()[0].snippets.len()
    );
    results.toggle_all();
    assert!(results.all_folded());
    assert_eq!(results.rows().len(), 3);
    results.toggle_all();
    assert_eq!(results.rows().len(), all);

    // Heading paths of the lines, outermost first.
    let red = results.files()[0]
        .snippets
        .iter()
        .position(|s| s.text.contains("are red"))
        .expect("a line");
    assert_eq!(
        results.heading(0, red).map(|h| h.to_string()),
        Some("Fruit › Red".to_owned())
    );
    assert_eq!(
        results.as_markdown_links().lines().next(),
        Some("- [[Apples]]")
    );
}

#[test]
fn keyboard_selection_walks_rows_and_survives_new_batches() {
    let dir = vault(NOTES);
    let content = snapshot(dir.path());
    let mut results = results_for(dir.path(), &content, "apple");
    // Enter with nothing selected opens the first context line.
    assert_eq!(results.target(), Some(Row::Match(0, 0)));
    assert_eq!(results.step(true), Some(0));
    assert_eq!(results.step(true), Some(1));
    assert_eq!(results.target(), Some(Row::Match(0, 0)));
    // Left from a line selects its note; Left again folds it; Right
    // unfolds it.
    results.collapse();
    assert_eq!(results.selected_row(), Some(0));
    results.collapse();
    assert!(results.is_folded(0));
    results.expand();
    assert!(!results.is_folded(0));
    assert_eq!(results.step(false), Some(0));
    assert_eq!(results.step(false), Some(0));
    // The selection follows its note when the list is ranked again.
    let last = results.rows().len() - 1;
    results.select(last);
    let key = results.key(results.rows()[last]);
    results.add(Vec::new(), Some(&content));
    assert_eq!(
        results
            .selected_row()
            .map(|r| results.key(results.rows()[r])),
        Some(key)
    );
}

#[test]
fn the_summary_tells_progress_and_the_limit() {
    let mut results = Results::default();
    assert_eq!(results.summary(), "");
    results.progress = Progress::Running {
        scanned: 1200,
        total: 10_000,
    };
    assert_eq!(results.summary(), "Searching · 1,200 of 10,000 notes");
    results.progress = Progress::Done(gv_search::SearchSummary::default());
    assert_eq!(results.summary(), "No results");
}

// ----- the panel ------------------------------------------------------------------

struct Panel {
    _dir: tempfile::TempDir,
    panel: Entity<SearchPanel>,
    vcx: VisualTestContext,
    events: Rc<RefCell<Vec<SearchPanelEvent>>>,
}

fn panel(cx: &mut TestAppContext, files: &[(&str, &str)]) -> Panel {
    let dir = vault(files);
    let content = snapshot(dir.path());
    let root = dir.path().to_path_buf();
    let (window, panel) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(232.0), px(600.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| SearchPanel::new(window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&panel, move |_, e: &SearchPanelEvent, _| {
            seen.borrow_mut().push(e.clone());
        })
        .detach();
        panel.update(cx, |p, cx| {
            p.set_scope(
                Some(SearchScope {
                    root,
                    content: Some(content),
                }),
                cx,
            );
        });
    });
    Panel {
        _dir: dir,
        panel,
        vcx,
        events,
    }
}

impl Panel {
    fn draw(&mut self) {
        for _ in 0..2 {
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    fn search(&mut self, query: &str) {
        let panel = self.panel.clone();
        self.vcx
            .update(|window, cx| panel.update(cx, |p, cx| p.set_query(query, window, cx)));
        self.draw();
    }

    fn summary(&self) -> String {
        self.panel.read_with(&self.vcx, |p, _| p.summary())
    }

    fn focus_field(&mut self) {
        let panel = self.panel.clone();
        self.vcx
            .update(|window, cx| panel.update(cx, |p, cx| p.focus_field(window, cx)));
        self.draw();
    }
}

#[gpui::test]
fn typing_runs_the_search_and_draws_grouped_results(cx: &mut TestAppContext) {
    let mut p = panel(cx, NOTES);
    p.search("apple");
    assert!(matches!(
        p.panel.read_with(&p.vcx, |p, _| p.progress()),
        Progress::Done(_)
    ));
    assert_eq!(p.summary(), "5 results in 3 files");
    assert!(p.vcx.debug_bounds("search-file-0").is_some());
    assert!(p.vcx.debug_bounds("search-line-0-0").is_some());
    assert!(p.vcx.debug_bounds("search-summary").is_some());

    // Match case.
    p.panel
        .update(&mut p.vcx, |p, cx| p.toggle(SearchToggle::MatchCase, cx));
    p.draw();
    // "Apples" and "Apple pie" no longer match.
    assert_eq!(p.summary(), "3 results in 2 files");
    p.panel
        .update(&mut p.vcx, |p, cx| p.toggle(SearchToggle::MatchCase, cx));
    p.draw();

    // Folding with a click on the note's row.
    let at = p.vcx.debug_bounds("search-file-0").expect("row").center();
    p.vcx.simulate_click(at, Modifiers::none());
    p.draw();
    assert!(p.vcx.debug_bounds("search-line-0-0").is_none());
    // An empty query clears the list.
    p.search("");
    assert_eq!(p.summary(), "");
    assert!(p.vcx.debug_bounds("search-file-0").is_none());
}

#[gpui::test]
fn regex_errors_and_the_regex_toggle(cx: &mut TestAppContext) {
    let mut p = panel(cx, NOTES);
    p.search("/a(b/");
    let error = p.panel.read_with(&p.vcx, |p, _| p.error().cloned());
    assert!(
        error
            .as_ref()
            .is_some_and(|e| e.contains("invalid regular expression")),
        "{error:?}"
    );
    // The whole query as one regular expression, its slash kept.
    p.panel
        .update(&mut p.vcx, |p, cx| p.toggle(SearchToggle::Regex, cx));
    p.search("a / s");
    assert_eq!(p.summary(), "1 result in 1 file");
    p.search("a\\(b");
    assert_eq!(p.summary(), "1 result in 1 file");
    p.search("a(b");
    assert!(p.panel.read_with(&p.vcx, |p, _| p.error().is_some()));
}

#[gpui::test]
fn keys_walk_the_results_and_open_them(cx: &mut TestAppContext) {
    let mut p = panel(cx, NOTES);
    p.search("pears");
    p.focus_field();
    assert!(
        p.vcx
            .update(|window, cx| { p.panel.read(cx).focus_handle(cx).is_focused(window) })
    );
    // Enter with nothing selected: the first match.
    p.vcx.dispatch_action(OpenSelected);
    p.draw();
    let first = p.events.borrow_mut().drain(..).collect::<Vec<_>>();
    match first.as_slice() {
        [
            SearchPanelEvent::Open {
                path,
                offset: Some(_),
                disposition: Disposition::InPlace,
            },
        ] => assert!(path.ends_with("Pears.md")),
        other => panic!("{other:?}"),
    }
    // Down twice, Up once, Mod+Enter: the note's row, in a new tab.
    p.vcx.dispatch_action(SelectNext);
    p.vcx.dispatch_action(SelectNext);
    p.vcx.dispatch_action(SelectPrevious);
    p.vcx.dispatch_action(OpenSelectedInNewTab);
    p.draw();
    let opened = p.events.borrow_mut().drain(..).collect::<Vec<_>>();
    assert!(matches!(
        opened.as_slice(),
        [SearchPanelEvent::Open {
            offset: None,
            disposition: Disposition::NewTab,
            ..
        }]
    ));
    p.vcx.dispatch_action(Collapse);
    p.draw();
    assert!(p.vcx.debug_bounds("search-line-0-0").is_none());
    p.vcx.dispatch_action(Expand);
    p.draw();
    assert!(p.vcx.debug_bounds("search-line-0-0").is_some());
    // A click on a line opens it.
    let at = p
        .vcx
        .debug_bounds("search-line-0-0")
        .expect("line")
        .center();
    p.vcx.simulate_click(at, Modifiers::none());
    p.draw();
    assert!(matches!(
        p.events.borrow().as_slice(),
        [SearchPanelEvent::Open {
            offset: Some(_),
            disposition: Disposition::InPlace,
            ..
        }]
    ));
    p.events.borrow_mut().clear();
    p.focus_field();
    p.vcx.dispatch_action(Dismiss);
    p.draw();
    assert_eq!(p.events.borrow().as_slice(), [SearchPanelEvent::Dismiss]);
}

#[gpui::test]
fn without_an_index_the_panel_waits_for_it(cx: &mut TestAppContext) {
    let mut p = panel(cx, NOTES);
    let root = p._dir.path().to_path_buf();
    p.panel.update(&mut p.vcx, |p, cx| {
        p.set_scope(
            Some(SearchScope {
                root: root.clone(),
                content: None,
            }),
            cx,
        );
    });
    p.search("apple");
    assert_eq!(p.summary(), "");
    assert!(p.vcx.debug_bounds("search-file-0").is_none());
    // The index's first snapshot runs the query.
    let content = snapshot(&root);
    p.panel.update(&mut p.vcx, |p, cx| {
        p.set_scope(
            Some(SearchScope {
                root,
                content: Some(content),
            }),
            cx,
        );
    });
    p.draw();
    assert_eq!(p.summary(), "5 results in 3 files");
}

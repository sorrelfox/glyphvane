// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Backlinks, unlinked mentions, *Links in this note* and the status bar's
//! link findings in headless GPUI windows over temporary vaults.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};

use gv_vault::files::journal::JournalStore;

use super::links_data::Severity;
use super::links_panel::{LinksPanel, MENTIONS_DELAY, Phase};
use super::vault::AutoIndexLimit;
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::workspace as commands;
use crate::{AppSettings, ThemeChoice, TogglePanel};

struct Fixture {
    dir: tempfile::TempDir,
    _journal: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

/// An Obsidian vault with `files`, a window showing `open` (an empty
/// window with `None`), and the right panel shown.
fn fixture(cx: &mut TestAppContext, files: &[(&str, &str)], open: Option<&str>) -> Fixture {
    fixture_with(cx, files, open, None)
}

fn fixture_with(
    cx: &mut TestAppContext,
    files: &[(&str, &str)],
    open: Option<&str>,
    limit: Option<usize>,
) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    for (name, text) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folder");
        }
        std::fs::write(path, text).expect("write");
    }
    let target = open.map(|name| OpenTarget::new(dir.path().join(name)));
    // Edits reach the content index through the recovery journal's worker.
    let journal = tempfile::tempdir().expect("journal dir");
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let store = JournalStore::open(journal.path().join("journal")).expect("journal store");
        crate::files::install_store(cx, Some(store), false);
        if let Some(limit) = limit {
            cx.set_global(AutoIndexLimit(limit));
        }
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    vcx.dispatch_action(TogglePanel);
    draw(&mut vcx);
    Fixture {
        dir,
        _journal: journal,
        workspace,
        vcx,
    }
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn panel(&self) -> Entity<LinksPanel> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.links_panel())
            .expect("the links sections exist")
    }

    /// Announces the content index's snapshots (GPUI's test scheduler does
    /// not let the index's thread wake the window), lets the unlinked
    /// mentions' delay pass, then draws.
    fn poll(&mut self) {
        self.vcx.executor().advance_clock(MENTIONS_DELAY);
        let vault = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
        }
        draw(&mut self.vcx);
    }

    /// Waits (in real time: the indexes have their own threads) until `f`
    /// holds for the sections.
    fn until(&mut self, what: &str, f: impl Fn(&LinksPanel) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            self.poll();
            if self.panel().read_with(&self.vcx, |p, _| f(p)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{what} did not happen in time");
    }

    fn has(&mut self, selector: &'static str) -> bool {
        self.vcx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        let at = self
            .vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        draw(&mut self.vcx);
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn rows(&self) -> Vec<(String, Vec<Option<String>>)> {
        self.panel().read_with(&self.vcx, |p, _| p.backlink_rows())
    }
}

fn line(s: &str) -> Option<String> {
    Some(s.to_string())
}

const HUB: &str = "Hub text.\n\n[[Missing]] and [[Gone]] and [[Dup]] and [[A]].\n";

fn vault_files() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Hub.md", HUB),
        ("A.md", "## Intro\n\n- See [[Hub]] here.\n"),
        (
            "B.md",
            "## Part\n\nAn item [[Hub|the hub]].\n\n## Later\n\nMore ![[Hub#Sec]].\n",
        ),
        ("C.md", "Talking about hub without a link.\n"),
        ("x/Dup.md", "one\n"),
        ("y/Dup.md", "two\n"),
    ]
}

#[gpui::test]
fn backlinks_list_sources_with_their_context_lines(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    f.until("backlinks", |p| p.settled() && p.backlink_count() == 3);
    assert_eq!(
        f.rows(),
        vec![
            ("A".to_string(), vec![line("See [Hub] here.")]),
            (
                "B".to_string(),
                vec![
                    line("An item [the hub]."),
                    line("More [Hub > Sec]. (embed)")
                ]
            ),
        ]
    );
    assert!(f.has("backlinks-head"));
    assert!(f.has("backlink-head-0"));
    assert!(f.has("backlink-line-1-1"));
    assert!(f.has("unlinked-mentions-head"));

    // A source folds away on a click on its row.
    f.click("backlink-head-1");
    assert!(!f.has("backlink-line-1-0"));
    f.click("backlink-head-1");
    assert!(f.has("backlink-line-1-0"));

    // The section folds with its head.
    f.click("backlinks-head");
    assert!(!f.has("backlink-head-0"));
    f.click("backlinks-head");
    assert!(f.has("backlink-head-0"));
}

#[gpui::test]
fn backlinks_follow_unsaved_edits_of_other_notes(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    f.until("backlinks", |p| p.settled() && p.backlink_count() == 3);
    let c = f.path("C.md");
    crate::files::overlay::dirty(&c, &gv_text::TextSnapshot::new("Now a [[Hub]] link.\n"));
    f.until("the edit", |p| p.settled() && p.backlink_count() == 4);
    let rows = f.rows();
    assert_eq!(rows[2], ("C".to_string(), vec![line("Now a [Hub] link.")]));
    crate::files::overlay::clean(&c);
    f.until("the edit undone", |p| {
        p.settled() && p.backlink_count() == 3
    });
}

#[gpui::test]
fn a_context_line_opens_its_source_at_the_link(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    f.until("backlinks", |p| p.settled() && p.backlink_count() == 3);
    f.click("backlink-line-1-1");
    assert_eq!(f.active_path(), Some(f.path("B.md")));
    let can_go_back = f.workspace.read_with(&f.vcx, |ws, _| ws.can_go_back());
    assert!(can_go_back, "opening from Backlinks is history");
    // The section now lists B's backlinks: none.
    f.until("B's backlinks", |p| p.settled() && p.backlink_count() == 0);
    assert!(f.has("links-message:No backlinks"));
}

#[gpui::test]
fn the_keyboard_moves_folds_and_opens(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    f.until("backlinks", |p| p.settled() && p.backlink_count() == 3);
    f.click("backlink-head-0");
    f.click("backlink-head-0"); // unfolded again, selected
    let selected = |f: &Fixture| f.panel().read_with(&f.vcx, |p, _| p.selected_selector());
    assert_eq!(selected(&f).as_deref(), Some("backlink-head-0"));
    f.vcx.simulate_keystrokes("down");
    assert_eq!(selected(&f).as_deref(), Some("backlink-line-0-0"));
    f.vcx.simulate_keystrokes("left");
    assert_eq!(selected(&f).as_deref(), Some("backlink-head-0"));
    f.vcx.simulate_keystrokes("left");
    draw(&mut f.vcx);
    assert!(!f.has("backlink-line-0-0"), "Left folds the source");
    f.vcx.simulate_keystrokes("right");
    draw(&mut f.vcx);
    assert!(f.has("backlink-line-0-0"), "Right unfolds it");
    f.vcx.simulate_keystrokes("down down down");
    assert_eq!(selected(&f).as_deref(), Some("backlink-line-1-0"));
    f.vcx.simulate_keystrokes("enter");
    draw(&mut f.vcx);
    assert_eq!(f.active_path(), Some(f.path("B.md")));
}

#[gpui::test]
fn the_link_action_writes_an_undoable_link(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    // Tests have no file watcher: an autosave would hide the edit from the
    // index, which sees it as an unsaved buffer instead.
    f.vcx
        .update(|_, cx| crate::files::override_autosave(cx, Some(false)));
    f.until("mentions", |p| {
        p.settled() && p.mention_rows().is_some_and(|m| m.len() == 1)
    });
    let mentions = f.panel().read_with(&f.vcx, |p, _| p.mention_rows());
    assert_eq!(
        mentions,
        Some(vec![(
            "C".to_string(),
            vec![line("Talking about [hub] without a link.")]
        )])
    );
    assert!(!f.has("mention-line-0-0"), "unlinked mentions start folded");
    f.click("unlinked-mentions-head");
    assert!(f.has("mention-line-0-0"));
    f.click("mention-link-0-0");

    // C opened in a background tab, its buffer holding the link.
    let c = f.path("C.md");
    assert_eq!(f.active_path(), Some(f.path("Hub.md")));
    let editor = f.workspace.read_with(&f.vcx, |ws, _| {
        ws.tabs()
            .iter()
            .find(|t| t.path() == Some(c.as_path()))
            .and_then(|t| t.editor().cloned())
    });
    let editor = editor.expect("C has a Source view");
    let text = |f: &mut Fixture| {
        editor.read_with(&f.vcx, |e, _| {
            e.editor().map(|e| e.text().to_string()).unwrap_or_default()
        })
    };
    assert_eq!(text(&mut f), "Talking about [[Hub|hub]] without a link.\n");

    // The index sees the buffer: the mention is a backlink now.
    f.until("the new backlink", |p| {
        p.settled() && p.backlink_count() == 4 && p.mention_rows().is_some_and(|m| m.is_empty())
    });

    // One undo step takes it back.
    editor.update(&mut f.vcx, |e, cx| e.undo_for_tests(cx));
    draw(&mut f.vcx);
    assert_eq!(text(&mut f), "Talking about hub without a link.\n");
    f.until("the mention again", |p| {
        p.settled() && p.backlink_count() == 3 && p.mention_rows().is_some_and(|m| m.len() == 1)
    });
    crate::files::overlay::clean(&c);
}

#[gpui::test]
fn link_findings_show_in_the_head_and_the_status_bar(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    f.until("findings", |p| p.findings().is_some_and(|f| f.links == 4));
    let findings = f
        .panel()
        .read_with(&f.vcx, |p, _| p.findings())
        .expect("found");
    assert_eq!((findings.errors, findings.warnings), (2, 1));
    assert!(f.has("status-link-findings"));

    // A click shows Links in this note, with the links and their statuses.
    assert!(!f.has("outgoing-row-0"));
    f.click("status-link-findings");
    assert!(f.has("outgoing-row-3"));
    let labels = f.panel().read_with(&f.vcx, |p, _| p.outgoing_labels());
    assert_eq!(
        labels,
        vec![
            ("Missing".to_string(), Some(Severity::Error)),
            ("Gone".to_string(), Some(Severity::Error)),
            ("Dup".to_string(), Some(Severity::Warning)),
            ("A".to_string(), None),
        ]
    );

    // A resolved link opens its target.
    f.click("outgoing-row-3");
    assert_eq!(f.active_path(), Some(f.path("A.md")));

    // Only Preview shows findings; A has none anyway.
    f.until("A's findings", |p| {
        p.findings().is_some_and(|f| f.links == 1)
    });
    assert!(!f.has("status-link-findings"));
}

#[gpui::test]
fn the_status_bar_shows_findings_in_preview_only(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    f.until("findings", |p| p.findings().is_some_and(|f| f.links == 4));
    assert!(f.has("status-link-findings"));
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.set_mode(ViewMode::Edit, cx));
    draw(&mut f.vcx);
    assert!(!f.has("status-link-findings"));
}

#[gpui::test]
fn empty_states(cx: &mut TestAppContext) {
    // No document.
    let mut f = fixture(cx, &[], None);
    assert!(f.has("links-message:No document"));
    let phase = f.panel().read_with(&f.vcx, |p, _| p.phase());
    assert_eq!(phase, Phase::NoDocument);
}

#[gpui::test]
fn a_folder_that_is_not_indexed_has_no_backlinks(cx: &mut TestAppContext) {
    let mut f = fixture_with(cx, &vault_files(), Some("Hub.md"), Some(2));
    f.until("the question", |p| p.phase() == Phase::NoVault);
    assert!(f.has("links-message:Not in an indexed folder"));
    assert!(!f.has("status-link-findings"));
}

#[gpui::test]
fn view_commands_expand_the_sections(cx: &mut TestAppContext) {
    let mut f = fixture(cx, &vault_files(), Some("Hub.md"));
    f.until("backlinks", |p| p.settled());
    f.click("backlinks-head");
    assert!(!f.has("backlink-head-0"));
    f.vcx.dispatch_action(commands::ShowBacklinks);
    draw(&mut f.vcx);
    assert!(f.has("backlink-head-0"));
    f.vcx.dispatch_action(commands::ShowOutgoingLinks);
    draw(&mut f.vcx);
    assert!(f.has("outgoing-row-0"));
}

/// A hub note with 15,000 backlinks (one per source note): the query runs
/// off the window's thread, the rows are drawn lazily and a frame stays
/// short.
#[gpui::test]
fn a_hub_with_15k_backlinks_stays_responsive(cx: &mut TestAppContext) {
    const SOURCES: usize = 15_000;
    let mut files: Vec<(String, String)> = vec![("Hub.md".into(), "The hub.\n".into())];
    for i in 0..SOURCES {
        files.push((
            format!("n/{:02}/note{i}.md", i % 50),
            format!("# Note {i}\n\n## Section\n\nA line about [[Hub]] number {i}.\n"),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let mut f = fixture(cx, &refs, Some("Hub.md"));
    let started = Instant::now();
    f.until("the hub's backlinks", |p| {
        p.settled() && p.backlink_count() == SOURCES
    });
    let ready = started.elapsed();
    f.until("the hub's unlinked mentions", |p| {
        p.mention_rows().is_some()
    });
    let timings = f.panel().read_with(&f.vcx, |p, _| p.timings());

    // The query itself, as the background task runs it.
    let (snapshot, hub) = f.workspace.read_with(&f.vcx, |ws, cx| {
        let vault = ws.vault.as_ref().expect("vault").read(cx);
        (
            vault.content().expect("content"),
            gv_vault::VaultPath::new("Hub.md").expect("path"),
        )
    });
    let t = Instant::now();
    let groups = super::links_data::backlink_groups(&snapshot, &hub);
    let query = t.elapsed();
    assert_eq!(groups.len(), SOURCES);

    // A frame with the list on screen.
    let mut frames = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        f.vcx.update(|window, cx| window.draw(cx).clear(cx));
        frames.push(t.elapsed());
    }
    frames.sort();
    let frame = frames[frames.len() / 2];
    assert!(f.has("backlink-head-0"));
    assert!(!f.has("backlink-head-14999"), "rows are drawn lazily");

    // Folding a source re-lays out 30,000 rows.
    let t = Instant::now();
    f.click("backlink-head-0");
    let fold = t.elapsed();

    eprintln!(
        "15k backlinks: groups {query:?}; panel query {:?}; context lines {:?}; \
         mentions {:?}; frame (median of 5) {frame:?}; fold + frame {fold:?}; \
         index + panel ready {ready:?}",
        timings.query, timings.snippets, timings.mentions
    );
    // Debug builds are several times slower than release; these bounds
    // only catch work that grows out of proportion.
    assert!(query < Duration::from_secs(2), "{query:?}");
    assert!(frame < Duration::from_millis(500), "{frame:?}");
}

#[gpui::test]
fn the_backlinks_section_grows_to_its_wrapped_lines(cx: &mut TestAppContext) {
    // Context lines that wrap: the section is as tall as they are, not as
    // one line each.
    let long = "A long context line that wraps over several lines in the narrow \
                panel, with [[Hub]] in the middle of it and more words after the \
                link so that it wraps again.\n";
    let files = vec![
        ("Hub.md", "Hub.\n"),
        ("A.md", long),
        ("B.md", long),
        ("C.md", long),
        ("D.md", long),
    ];
    let mut f = fixture(cx, &files, Some("Hub.md"));
    f.until("backlinks", |p| p.settled() && p.backlink_count() == 4);
    draw(&mut f.vcx);
    let list = f.vcx.debug_bounds("backlinks-list").expect("the list");
    let first = f.vcx.debug_bounds("backlink-line-0-0").expect("a line");
    assert!(
        first.size.height > px(2.0 * crate::metrics::text::UI_SECTION.line),
        "the lines wrap: {first:?}"
    );
    let last = f
        .vcx
        .debug_bounds("backlink-line-3-0")
        .expect("the last line is drawn");
    assert!(
        last.bottom() <= list.bottom() + px(0.5),
        "{last:?} in {list:?}"
    );
}

#[gpui::test]
fn a_context_line_typesets_its_formulas(cx: &mut TestAppContext) {
    let files = vec![
        ("Hub.md", "Hub.\n"),
        (
            "A.md",
            "Area $\\pi r^2$ near [[Hub]] and $x$ costs $5 and $6.\n",
        ),
        ("B.md", "The [[Hub|$y$ hub]] is here.\n"),
    ];
    let mut f = fixture(cx, &files, Some("Hub.md"));
    f.until("backlinks", |p| p.settled() && p.backlink_count() == 2);
    let line = f
        .panel()
        .read_with(&f.vcx, |p, _| p.backlink_line(0, 0))
        .expect("a line");
    let runs = crate::search_panel::segments(&line);
    let formulas: Vec<&str> = runs
        .iter()
        .filter_map(|r| match r {
            crate::search_panel::Segment::Math { tex, .. } => Some(tex.as_str()),
            crate::search_panel::Segment::Text { .. } => None,
        })
        .collect();
    assert_eq!(formulas, ["\\pi r^2", "x"], "{runs:?}");
    // The link stays marked; the currency stays text.
    let text: Vec<(&str, Vec<&str>)> = runs
        .iter()
        .filter_map(|r| match r {
            crate::search_panel::Segment::Text { text, marks } => Some((
                text.as_str(),
                marks.iter().map(|m| &text[m.clone()]).collect(),
            )),
            crate::search_panel::Segment::Math { .. } => None,
        })
        .collect();
    assert_eq!(
        text,
        [
            ("Area ", vec![]),
            (" near Hub and ", vec!["Hub"]),
            (" costs $5 and $6.", vec![]),
        ]
    );
    assert!(f.has("backlink-line-0-0"));

    // A formula in the link's text stays as its TeX, so the mark shows.
    let line = f
        .panel()
        .read_with(&f.vcx, |p, _| p.backlink_line(1, 0))
        .expect("a line");
    let runs = crate::search_panel::segments(&line);
    assert!(
        matches!(runs.as_slice(), [crate::search_panel::Segment::Text { text, marks }]
            if marks.len() == 1 && text[marks[0].clone()].contains('y')),
        "{runs:?}"
    );
    assert!(f.has("backlink-line-1-0"));
}

#[gpui::test]
fn a_context_line_typesets_with_the_source_notes_macros(cx: &mut TestAppContext) {
    let files = vec![
        ("Hub.md", "Hub.\n"),
        (
            "A.md",
            "$\\Pln$ first, then $\\def\\Pln{\\mathbb{P}}$ defined.\n\nIn $\\Pln^2$ near [[Hub]] and $\\Pln$ [[Hub]].\n",
        ),
    ];
    let mut f = fixture(cx, &files, Some("Hub.md"));
    f.until("backlinks", |p| p.settled() && p.backlink_count() == 2);
    let formulas = |item: usize| -> Vec<String> {
        let line = f
            .panel()
            .read_with(&f.vcx, |p, _| p.backlink_line(0, item))
            .expect("a line");
        crate::search_panel::segments(&line)
            .into_iter()
            .filter_map(|r| match r {
                crate::search_panel::Segment::Math { tex, .. } => Some(tex),
                crate::search_panel::Segment::Text { .. } => None,
            })
            .collect()
    };
    // Defined before the line: typeset.
    assert_eq!(formulas(0), ["\\Pln^2", "\\Pln"]);
    assert_eq!(formulas(1), ["\\Pln^2", "\\Pln"]);
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The quick switcher in headless GPUI windows over temporary folders:
//! opening and closing, what it lists and in which order, Enter, Mod+Enter
//! and Shift+Enter, windows without a vault, and the content index changing
//! while it is open.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_vault::{IndexConfig, PathIndex};

use super::session::{self, Session, SessionStore};
use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::workspace as workspace_commands;
use crate::switcher::{QuickSwitcher, switcher_of};
use crate::{AppSettings, ThemeChoice};

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn options() -> WindowOptions {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1280.0), px(800.0)));
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    }
}

/// A vault folder (marked as an Obsidian vault) with `files`, a session
/// with `recent` files, and a window showing `open` (an empty window with
/// `None`).
fn fixture(
    cx: &mut TestAppContext,
    files: &[(&str, &str)],
    open: Option<&str>,
    recent: &[PathBuf],
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
    let session = Session {
        recent_files: recent.to_vec(),
        ..Session::default()
    };
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        session::install(cx, SessionStore::new(session, None));
        gpui::open_window(options(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        dir,
        workspace,
        vcx,
    }
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

impl Fixture {
    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn draw(&mut self) {
        draw(&mut self.vcx);
    }

    /// Announces the content index's new snapshots (GPUI's test scheduler
    /// does not let the index's thread wake the window), then draws.
    fn poll(&mut self) {
        let vault = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
        }
        self.draw();
    }

    fn switcher(&mut self) -> Entity<QuickSwitcher> {
        self.vcx
            .update(|window, cx| switcher_of(window, cx))
            .expect("the window root has a switcher")
    }

    fn toggle(&mut self) {
        self.vcx.dispatch_action(workspace_commands::QuickSwitcher);
        self.draw();
    }

    fn is_open(&mut self) -> bool {
        let open = self.switcher().read_with(&self.vcx, |s, _| s.is_open());
        assert_eq!(open, self.vcx.debug_bounds("quick-switcher").is_some());
        open
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        self.draw();
    }

    fn type_text(&mut self, text: &str) {
        self.vcx.simulate_input(text);
        self.draw();
    }

    fn listed(&mut self) -> Vec<String> {
        self.switcher().read_with(&self.vcx, |s, _| s.listed())
    }

    fn selected(&mut self) -> Option<String> {
        self.switcher().read_with(&self.vcx, |s, _| s.selected())
    }

    fn tab_paths(&self) -> Vec<Option<PathBuf>> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.tabs()
                .iter()
                .map(|t| t.path().map(Path::to_path_buf))
                .collect()
        })
    }

    fn active_path(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    /// Waits (in real time: the indexes have their own threads) until the
    /// window's content index is built and `f` holds for it.
    fn until_content(&mut self, f: impl Fn(&gv_vault::content::ContentSnapshot) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.poll();
            let ok = self.workspace.read_with(&self.vcx, |ws, cx| {
                ws.vault
                    .as_ref()
                    .and_then(|v| v.read(cx).content())
                    .is_some_and(|s| f(&s))
            });
            if ok {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the content index did not get there in time");
    }

    /// Waits (in real time) until the open switcher lists `row`.
    fn until_listed(&mut self, row: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.poll();
            if self.listed().iter().any(|r| r == row) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{row} was not listed in time: {:?}", self.listed());
    }

    /// Waits (in real time: files are created off the window's thread)
    /// until `f` holds.
    fn until(&mut self, f: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            self.poll();
            if f(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("did not happen in time");
    }

    fn reader_focused(&mut self) -> bool {
        let workspace = self.workspace.clone();
        self.vcx.update(|window, cx| {
            let ws = workspace.read(cx);
            ws.active_tab()
                .reader()
                .is_some_and(|r| r.read(cx).focus_handle(cx).is_focused(window))
        })
    }
}

const VAULT: &[(&str, &str)] = &[
    (
        "Start.md",
        "# Start\n\nSee [[Release process]] and [[Release process]].\n",
    ),
    (
        "Profiles/Flavor profiles.md",
        "---\naliases: [Profiles]\n---\n# Flavor profiles\n\n## Pro mode\n\nText ^blk1\n",
    ),
    ("Vault/Properties.md", "# Properties\n"),
    (
        "Editing/Proofreading.md",
        "# Proofreading and spell check\n",
    ),
    ("Editing/Paste and drop.md", "# Paste and drop\n"),
    ("assets/diagram.png", "not really a picture"),
];

/// The switcher opens with Mod+O and the command, takes the keyboard, and
/// gives it back when Esc or Mod+O closes it.
#[gpui::test]
fn opens_and_closes(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    assert!(!f.is_open());
    assert!(f.reader_focused());
    f.keys("ctrl-o");
    assert!(f.is_open());
    assert!(!f.reader_focused(), "the field has the keyboard");
    assert!(f.vcx.debug_bounds("quick-switcher-footer").is_some());
    f.keys("escape");
    assert!(!f.is_open());
    assert!(f.reader_focused(), "the keyboard goes back");

    f.toggle();
    assert!(f.is_open());
    f.keys("ctrl-o");
    assert!(!f.is_open(), "Mod+O again closes");
    assert!(f.reader_focused());

    // A click outside closes too.
    f.toggle();
    f.vcx
        .simulate_click(point(px(20.0), px(700.0)), gpui::Modifiers::none());
    f.draw();
    assert!(!f.is_open());
}

/// Titles, aliases and notes not created yet, ranked as the design's
/// screen shows "pro".
#[gpui::test]
fn lists_titles_aliases_and_missing_notes(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    f.toggle();
    f.until_content(|s| s.note_count() == 5 && !s.unresolved_targets().is_empty());
    f.until_listed("Flavor profiles");
    f.type_text("pro");
    let rows = f.listed();
    assert!(rows.contains(&"Flavor profiles".to_owned()), "{rows:?}");
    assert!(rows.contains(&"Profiles → Flavor profiles".to_owned()));
    assert!(rows.contains(&"Properties".to_owned()));
    assert!(rows.contains(&"Proofreading and spell check".to_owned()));
    assert_eq!(
        rows.last().map(String::as_str),
        Some("Release process · not created yet"),
        "notes not created yet come last"
    );
    assert!(
        f.vcx
            .debug_bounds("quick-switcher-row:Release process · not created yet")
            .is_some()
    );

    // Arrows move the highlight, wrapping around.
    assert_eq!(f.selected().as_deref(), Some(rows[0].as_str()));
    f.keys("down");
    assert_eq!(f.selected().as_deref(), Some(rows[1].as_str()));
    f.keys("up up");
    assert_eq!(f.selected().as_deref(), rows.last().map(String::as_str));

    // An exact alias comes first.
    f.keys("ctrl-a backspace");
    f.type_text("profiles");
    assert_eq!(f.listed()[0], "Profiles → Flavor profiles");
}

/// Enter opens the file in the active tab; on an alias, its note.
#[gpui::test]
fn enter_opens_in_the_active_tab(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    f.toggle();
    f.until_content(|s| s.note_count() == 5);
    f.until_listed("Flavor profiles");
    f.type_text("profiles");
    f.keys("enter");
    assert!(!f.is_open());
    assert_eq!(f.tab_paths(), [Some(f.path("Profiles/Flavor profiles.md"))]);
    // Back returns to where the tab was.
    let back = f.workspace.read_with(&f.vcx, |ws, _| ws.can_go_back());
    assert!(back, "opening from the switcher is remembered");

    // A file shown by another tab activates that tab.
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        let target = OpenTarget::new(f.dir.path().join("Vault/Properties.md"));
        ws.open_tab(target, window, cx);
    });
    f.draw();
    f.toggle();
    f.type_text("flavor");
    f.keys("enter");
    assert_eq!(f.tab_paths().len(), 2);
    assert_eq!(f.active_path(), Some(f.path("Profiles/Flavor profiles.md")));
}

/// Mod+Enter opens a new tab and leaves the others.
#[gpui::test]
fn mod_enter_opens_a_new_tab(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    f.toggle();
    f.until_content(|s| s.note_count() == 5);
    f.until_listed("Properties");
    f.type_text("properties");
    f.keys("ctrl-enter");
    assert!(!f.is_open());
    assert_eq!(
        f.tab_paths(),
        [
            Some(f.path("Start.md")),
            Some(f.path("Vault/Properties.md"))
        ]
    );
    assert_eq!(f.active_path(), Some(f.path("Vault/Properties.md")));
}

/// `#` lists headings; Enter opens the note there.
#[gpui::test]
fn headings_mode_opens_at_the_heading(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    f.toggle();
    f.until_content(|s| s.note_count() == 5);
    f.until_listed("Properties");
    f.type_text("#pro mode");
    assert_eq!(f.listed(), ["Pro mode · Flavor profiles"]);
    f.keys("enter");
    assert_eq!(f.active_path(), Some(f.path("Profiles/Flavor profiles.md")));
    f.toggle();
    f.type_text("^blk");
    assert_eq!(f.listed(), ["^blk1 · Flavor profiles"]);
    f.keys("escape");
}

/// Enter on a note not created yet creates it; with nothing matching, the
/// create row does; Shift+Enter creates the query in the folders it names.
#[gpui::test]
fn creates_notes(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    f.toggle();
    f.until_content(|s| !s.unresolved_targets().is_empty());
    f.until_listed("Properties");

    // Nothing matches: the only row creates the query.
    f.type_text("Brand new idea");
    assert_eq!(f.listed(), ["+ Brand new idea.md"]);
    assert!(f.vcx.debug_bounds("quick-switcher-create").is_some());
    f.keys("enter");
    let created = f.path("Brand new idea.md");
    f.until(|f| f.active_path() == Some(f.path("Brand new idea.md")));
    assert!(created.is_file());
    assert_eq!(f.tab_paths(), [Some(created.clone())]);
    let mode = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.active_tab().mode());
    assert_eq!(mode, ViewMode::Edit, "a new note opens for editing");

    // A note that links point to.
    f.toggle();
    f.type_text("release");
    assert_eq!(f.listed()[0], "Release process · not created yet");
    f.keys("ctrl-enter");
    let release = f.path("Release process.md");
    f.until(|f| f.tab_paths().len() == 2);
    assert!(release.is_file());
    assert_eq!(f.active_path(), Some(release));

    // Shift+Enter creates the query even though rows match.
    f.toggle();
    f.type_text("Profiles/Pro: plan");
    assert!(!f.listed().is_empty());
    f.keys("shift-enter");
    let plan = f.path("Profiles/Pro plan.md");
    f.until(|f| f.active_path() == Some(f.path("Profiles/Pro plan.md")));
    assert!(plan.is_file());
    assert_eq!(f.active_path(), Some(plan));

    // A name that exists (in any letter case) opens it instead of making
    // a twin.
    f.toggle();
    f.type_text("START");
    f.keys("shift-enter");
    f.until(|f| f.active_path() == Some(f.path("Start.md")));
    let twins = std::fs::read_dir(f.dir.path())
        .expect("list")
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case("start.md")
        })
        .count();
    assert_eq!(twins, 1, "no case twin is made");
}

/// A bare name is created where the vault's `newFileLocation` says; a
/// query that names folders keeps them.
#[gpui::test]
fn creates_notes_where_the_vault_says(cx: &mut TestAppContext) {
    let files = [
        (
            ".obsidian/app.json",
            "{\"newFileLocation\": \"folder\", \"newFileFolderPath\": \"Inbox/\"}",
        ),
        ("Start.md", "# Start\n"),
    ];
    let mut f = fixture(cx, &files, Some("Start.md"), &[]);
    f.toggle();
    f.until_listed("Start");
    f.type_text("Fresh idea");
    f.keys("shift-enter");
    let made = f.path("Inbox/Fresh idea.md");
    f.until(|f| f.active_path() == Some(f.path("Inbox/Fresh idea.md")));
    assert!(made.is_file());

    f.toggle();
    f.type_text("Elsewhere/Other");
    f.keys("shift-enter");
    f.until(|f| f.active_path() == Some(f.path("Elsewhere/Other.md")));
    assert!(f.path("Elsewhere/Other.md").is_file());
}

/// A window without a vault lists its open tabs and the recent files, and
/// opens them; it offers to create nothing.
#[gpui::test]
fn works_without_a_vault(cx: &mut TestAppContext) {
    let elsewhere = tempfile::tempdir().expect("temp dir");
    let one = elsewhere.path().join("One.md");
    let two = elsewhere.path().join("Two.md");
    std::fs::write(&one, "# One\n").expect("write");
    std::fs::write(&two, "# Two\n").expect("write");
    let mut f = fixture(cx, &[], None, &[two.clone(), one.clone()]);
    f.toggle();
    assert!(f.is_open());
    assert_eq!(f.listed(), ["Two", "One"]);
    f.type_text("one");
    assert_eq!(f.listed(), ["One"]);
    f.keys("enter");
    // The tab shows the file once the window has heard the choice.
    f.until(|f| f.tab_paths() == [Some(one.clone())]);

    f.toggle();
    f.type_text("nothing like it");
    assert!(f.listed().is_empty(), "no vault, nothing to create");
    assert!(f.vcx.debug_bounds("quick-switcher-empty").is_some());
    f.keys("shift-enter");
    assert!(f.is_open());
    f.keys("escape");
}

/// While the switcher is open, a new snapshot of the content index
/// refreshes the list and keeps the highlighted row.
#[gpui::test]
fn follows_the_index_while_open(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    f.toggle();
    f.until_content(|s| s.note_count() == 5);
    f.until_listed("Properties");
    f.type_text("p");
    f.keys("down");
    let highlighted = f.selected();

    std::fs::write(f.path("Pancakes.md"), "# Pancakes\n").expect("write");
    let index = PathIndex::build(f.dir.path(), IndexConfig::default())
        .expect("index")
        .0;
    let vault = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.vault.clone())
        .expect("a vault");
    vault.update(&mut f.vcx, |v, cx| v.publish(index, cx));
    f.until_listed("Pancakes");
    assert!(f.is_open());
    assert_eq!(f.selected(), highlighted, "the highlight stays on its row");

    // A title change shows too.
    std::fs::write(f.path("Pancakes.md"), "---\ntitle: Crêpes\n---\n").expect("write");
    let index = PathIndex::build(f.dir.path(), IndexConfig::default())
        .expect("index")
        .0;
    vault.update(&mut f.vcx, |v, cx| v.publish(index, cx));
    f.keys("ctrl-a backspace");
    f.type_text("crepes");
    f.until_listed("Crêpes");
}

/// The open tab with unsaved changes is marked.
#[gpui::test]
fn unsaved_files_are_marked(cx: &mut TestAppContext) {
    let mut f = fixture(cx, VAULT, Some("Start.md"), &[]);
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        ws.edit_active_tab(None, window, cx);
    });
    f.draw();
    f.vcx.simulate_input("x");
    f.draw();
    f.toggle();
    f.until_listed("Start");
    f.type_text("start");
    assert!(f.vcx.debug_bounds("quick-switcher-unsaved:Start").is_some());
}

/// The time to gather and rank a real 10,000-note vault (run by hand:
/// `cargo test -p gv-ui --profile measure -- --ignored ten_thousand`).
#[gpui::test]
#[ignore = "builds a 10,000-note vault; a measurement"]
fn ten_thousand_notes(cx: &mut TestAppContext) {
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..10_000 {
        files.push((
            format!("Folder {}/Note {i}.md", i % 100),
            format!(
                "---\naliases: [alias {i}]\n---\n# Title {i}\n\n## Part\n\nLinks [[Note {}]] [[Missing {}]]\n",
                (i + 1) % 10_000,
                i % 500
            ),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let mut f = fixture(cx, &refs, Some("Folder 0/Note 0.md"), &[]);
    f.toggle();
    f.until_content(|s| s.note_count() == 10_000);
    let started = Instant::now();
    let sources = f
        .workspace
        .read_with(&f.vcx, |ws, cx| ws.switcher_sources(cx));
    let gathered = started.elapsed();
    let mut worst = Duration::ZERO;
    for query in [
        "t", "ti", "tit", "titl", "title", "title 9", "title 99", "fo 7/no",
    ] {
        let started = Instant::now();
        let _ = crate::switcher::model::rank(&sources.entries, query);
        worst = worst.max(started.elapsed());
    }
    let (root, snapshot) = sources.content.clone().expect("content");
    let started = Instant::now();
    let headings = crate::switcher::model::sections(&snapshot, &root, false);
    let sections = started.elapsed();
    let started = Instant::now();
    let _ = crate::switcher::model::rank(&headings, "part");
    let heading_rank = started.elapsed();
    eprintln!(
        "switcher, 10,000 notes: {} entries gathered in {gathered:?}; worst keystroke \
         {worst:?}; {} headings in {sections:?}, ranked in {heading_rank:?}",
        sources.entries.len(),
        headings.len()
    );
}

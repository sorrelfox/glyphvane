// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Bookmarks panel in a window, in headless GPUI windows over a
//! temporary vault with Obsidian's `bookmarks.json`: the rows of both
//! lists, opening, `bookmarks.toggle`, Obsidian's file left alone, and
//! renames.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};
use gv_settings::Locations;
use gv_vault::VaultPath;

use super::bookmarks::{self, contains_note};
use super::bookmarks_panel::{BookmarkTarget, BookmarksPanel, RowIcon};
use super::panels::SidebarPanel;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::{bookmarks as bookmark_commands, workspace as commands};
use crate::{AppSettings, ThemeChoice};

const OBSIDIAN: &str = r##"{
  "items": [
    {"type": "file", "ctime": 1, "path": "Notes/A.md", "subpath": "#Second"},
    {"type": "group", "ctime": 2, "title": "Work", "items": [
      {"type": "file", "ctime": 3, "path": "B.md"},
      {"type": "folder", "ctime": 4, "path": "Notes"},
      {"type": "file", "ctime": 5, "path": "Gone.md"}
    ]},
    {"type": "search", "ctime": 6, "query": "needle"},
    {"type": "graph", "ctime": 7, "title": "Graph"}
  ]
}"##;

struct Fixture {
    dir: tempfile::TempDir,
    data: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let data = tempfile::tempdir().expect("data dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join(".obsidian")).expect("marker");
    std::fs::create_dir_all(root.join("Notes")).expect("folder");
    std::fs::write(root.join(".obsidian/bookmarks.json"), OBSIDIAN).expect("bookmarks");
    std::fs::write(
        root.join("Notes/A.md"),
        "# First\n\none\n\n# Second\n\ntwo\n",
    )
    .expect("write");
    std::fs::write(root.join("B.md"), "# B\n").expect("write");
    std::fs::write(root.join("C.md"), "# C\n").expect("write");
    let target = Some(OpenTarget::new(root.join("C.md")));
    let locations = Locations::with_root(data.path());
    let (window, workspace) = cx.update(|cx| {
        crate::init(
            cx,
            AppSettings::new(None, Some(ThemeChoice::Light)).with_locations(locations),
        );
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
    Fixture {
        dir,
        data,
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
    fn root(&self) -> PathBuf {
        std::path::absolute(self.dir.path()).expect("absolute")
    }

    fn panel(&self) -> Entity<BookmarksPanel> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.bookmarks_panel())
            .expect("the Bookmarks panel exists")
    }

    fn labels(&self) -> Vec<String> {
        self.panel().read_with(&self.vcx, |p, _| {
            p.rows().iter().map(|r| r.label.to_string()).collect()
        })
    }

    fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            draw(&mut self.vcx);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn own_file(&self) -> PathBuf {
        let id = crate::settings::vault_id(&self.root());
        Locations::with_root(self.data.path())
            .vault_data_dir(&id)
            .expect("vault dir")
            .join(bookmarks::OWN_FILE)
    }

    fn own_has(&self, path: &str) -> bool {
        bookmarks::read_file(&self.own_file())
            .ok()
            .flatten()
            .is_some_and(|items| contains_note(&items, path))
    }

    fn active_file(&self) -> Option<PathBuf> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().path().map(Path::to_path_buf)
        })
    }

    fn notice_title(&self) -> Option<String> {
        self.workspace.read_with(&self.vcx, |ws, _| {
            ws.active_tab().notice().map(|n| n.title.to_string())
        })
    }
}

#[gpui::test]
fn the_panel_lists_obsidians_bookmarks_and_opens_them(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowBookmarks);
    draw(&mut f.vcx);
    let panel = f.workspace.read_with(&f.vcx, |ws, _| ws.sidebar_panel());
    assert_eq!(panel, SidebarPanel::Bookmarks);
    f.until("the rows", |f| !f.labels().is_empty());
    assert_eq!(
        f.labels(),
        [
            "A › Second",
            "Work",
            "B",
            "Notes",
            "Gone",
            "needle",
            "Graph"
        ]
    );
    let rows = f.panel().read_with(&f.vcx, |p, _| p.rows().to_vec());
    assert_eq!(rows[0].icon, RowIcon::Heading);
    assert_eq!(rows[2].depth, 1, "group items are indented");
    assert!(rows[4].missing && rows[4].target.is_none());
    assert!(rows[6].target.is_none(), "graph views do not open");
    assert_eq!(
        rows[5].target,
        Some(BookmarkTarget::Search("needle".into()))
    );

    // A heading bookmark opens its note.
    f.panel().update(&mut f.vcx, |p, cx| p.open_row(0, cx));
    draw(&mut f.vcx);
    assert_eq!(f.active_file(), Some(f.root().join("Notes/A.md")));

    // A search bookmark runs in the Search panel.
    f.panel().update(&mut f.vcx, |p, cx| p.open_row(5, cx));
    draw(&mut f.vcx);
    let panel = f.workspace.read_with(&f.vcx, |ws, _| ws.sidebar_panel());
    assert_eq!(panel, SidebarPanel::Search);
}

#[gpui::test]
fn toggle_adds_and_removes_own_bookmarks_and_leaves_obsidians_file(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowBookmarks);
    draw(&mut f.vcx);
    f.until("the rows", |f| !f.labels().is_empty());

    // C.md is active: bookmarked in the own list, shown first.
    f.vcx.dispatch_action(bookmark_commands::Toggle);
    f.until("C bookmarked", |f| f.own_has("C.md"));
    f.until("C listed", |f| {
        f.labels().first().map(String::as_str) == Some("C")
    });
    assert_eq!(
        f.labels().get(1).map(String::as_str),
        Some("From Obsidian"),
        "{:?}",
        f.labels()
    );
    // Again: removed.
    f.vcx.dispatch_action(bookmark_commands::Toggle);
    f.until("C removed", |f| !f.own_has("C.md"));
    f.until("C not listed", |f| {
        f.labels().first().map(String::as_str) != Some("C")
    });

    // B.md is bookmarked in Obsidian only: not removed, Obsidian's file
    // untouched.
    let b = f.root().join("B.md");
    f.workspace.update_in(&mut f.vcx, |ws, window, cx| {
        ws.open_tab(OpenTarget::new(b), window, cx);
    });
    draw(&mut f.vcx);
    f.vcx.dispatch_action(bookmark_commands::Toggle);
    f.until("the infobar", |f| {
        f.notice_title().as_deref() == Some("Bookmark not removed.")
    });
    assert!(!f.own_has("B.md"));
    let obsidian =
        std::fs::read_to_string(f.root().join(".obsidian/bookmarks.json")).expect("read");
    assert_eq!(obsidian, OBSIDIAN);
}

#[gpui::test]
fn own_bookmarks_follow_renames(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowBookmarks);
    draw(&mut f.vcx);
    f.vcx.dispatch_action(bookmark_commands::Toggle);
    f.until("C bookmarked", |f| f.own_has("C.md"));
    let root = f.root();
    std::fs::rename(root.join("C.md"), root.join("D.md")).expect("renamed");
    let vp = |s: &str| VaultPath::new(s).expect("vault path");
    f.workspace.update(&mut f.vcx, |ws, cx| {
        ws.carry_history(&root, &[(vp("C.md"), vp("D.md"))], cx);
    });
    f.until("D bookmarked", |f| f.own_has("D.md") && !f.own_has("C.md"));
    f.until("D listed", |f| {
        f.labels().first().map(String::as_str) == Some("D")
    });
}

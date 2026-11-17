// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Tags panel in a window, in headless GPUI windows over a temporary
//! vault: View ▸ Tags, the rows, a click running the tag's search, the
//! keys, sorting, the filter, and new content snapshots.

use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};

use super::panels::SidebarPanel;
use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::workspace as commands;
use crate::tags_panel::TagsPanel;
use crate::tags_panel::model::TagSort;
use crate::{AppSettings, ThemeChoice};

struct Fixture {
    dir: tempfile::TempDir,
    workspace: Entity<Workspace>,
    vcx: VisualTestContext,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    let files = [
        ("A.md", "# A\n\n#editing/lists and #editing/tables\n"),
        ("B.md", "---\ntags: [editing, vault]\n---\n# B\n"),
        ("C.md", "#vault\n"),
    ];
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    let target = Some(OpenTarget::new(dir.path().join("A.md")));
    let (window, workspace) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
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

fn row(depth: usize, tag: &str, notes: usize) -> (usize, String, usize) {
    (depth, tag.to_owned(), notes)
}

impl Fixture {
    fn panel(&self) -> Entity<TagsPanel> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.tags_panel())
            .expect("the tags panel exists")
    }

    /// Announces the content index's snapshots (its thread cannot wake
    /// the window under GPUI's test scheduler) and draws.
    fn poll(&mut self) {
        let vault = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
        }
        draw(&mut self.vcx);
    }

    fn rows(&self) -> Vec<(usize, String, usize)> {
        self.panel().read_with(&self.vcx, |p, _| p.visible_rows())
    }

    /// Waits (in real time: the index has its own thread) until the panel
    /// lists `rows`.
    fn until_rows(&mut self, rows: &[(usize, String, usize)]) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.poll();
            if self.rows() == rows {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "the tags panel did not list {rows:?} in time: {:?}",
            self.rows()
        );
    }

    fn search_query(&self) -> Option<String> {
        self.workspace
            .read_with(&self.vcx, |ws, cx| ws.search_query(cx))
    }
}

#[gpui::test]
fn view_tags_lists_the_tags_and_a_click_searches_one(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowTags);
    draw(&mut f.vcx);
    f.workspace.read_with(&f.vcx, |ws, _| {
        assert_eq!(ws.sidebar_panel(), SidebarPanel::Tags);
    });
    assert!(f.vcx.debug_bounds("sidebar-panel:tags").is_some());
    let panel = f.panel();
    assert!(
        f.vcx
            .update(|window, cx| panel.read(cx).focus_handle(cx).is_focused(window)),
        "View ▸ Tags gives the panel the keyboard"
    );
    f.until_rows(&[
        row(0, "editing", 2),
        row(1, "editing/lists", 1),
        row(1, "editing/tables", 1),
        row(0, "vault", 2),
    ]);
    assert!(f.vcx.debug_bounds("tag-row-0").is_some());
    assert!(f.vcx.debug_bounds("tag-chevron-0").is_some());
    assert!(
        f.vcx.debug_bounds("tag-chevron-1").is_none(),
        "a leaf has none"
    );

    // A click on a nested tag runs its search in the Search panel.
    let at = f.vcx.debug_bounds("tag-row-1").expect("row").center();
    f.vcx.simulate_click(at, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.search_query().as_deref(), Some("tag:#editing/lists"));
    f.workspace.read_with(&f.vcx, |ws, _| {
        assert_eq!(ws.sidebar_panel(), SidebarPanel::Search);
    });
    let search = f
        .workspace
        .read_with(&f.vcx, |ws, _| ws.panels.search.clone())
        .expect("search panel");
    assert!(
        f.vcx.update(|window, cx| search
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)),
        "the Search panel's field has the keyboard"
    );
}

#[gpui::test]
fn keys_chevrons_sort_and_filter(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowTags);
    draw(&mut f.vcx);
    f.until_rows(&[
        row(0, "editing", 2),
        row(1, "editing/lists", 1),
        row(1, "editing/tables", 1),
        row(0, "vault", 2),
    ]);
    let panel = f.panel();

    // Down selects the first tag; Left closes it; Right opens it again.
    f.vcx.simulate_keystrokes("down");
    draw(&mut f.vcx);
    assert_eq!(
        panel.read_with(&f.vcx, |p, _| p.selected_tag()).as_deref(),
        Some("editing")
    );
    f.vcx.simulate_keystrokes("left");
    draw(&mut f.vcx);
    assert_eq!(f.rows(), [row(0, "editing", 2), row(0, "vault", 2)]);
    f.vcx.simulate_keystrokes("right");
    draw(&mut f.vcx);
    assert_eq!(f.rows().len(), 4);
    // Right on an open tag goes to its first nested tag; Left goes back.
    f.vcx.simulate_keystrokes("right");
    draw(&mut f.vcx);
    assert_eq!(
        panel.read_with(&f.vcx, |p, _| p.selected_tag()).as_deref(),
        Some("editing/lists")
    );
    f.vcx.simulate_keystrokes("left");
    draw(&mut f.vcx);
    assert_eq!(
        panel.read_with(&f.vcx, |p, _| p.selected_tag()).as_deref(),
        Some("editing")
    );

    // The chevron closes a tag without searching.
    let at = f
        .vcx
        .debug_bounds("tag-chevron-0")
        .expect("chevron")
        .center();
    f.vcx.simulate_click(at, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.rows(), [row(0, "editing", 2), row(0, "vault", 2)]);
    assert_eq!(f.search_query(), None);
    f.workspace.read_with(&f.vcx, |ws, _| {
        assert_eq!(ws.sidebar_panel(), SidebarPanel::Tags);
    });

    // Another order, then Collapse all.
    panel.update(&mut f.vcx, |p, cx| p.set_sort(TagSort::NameDescending, cx));
    draw(&mut f.vcx);
    assert_eq!(f.rows(), [row(0, "vault", 2), row(0, "editing", 2)]);
    assert!(f.vcx.debug_bounds("sort-tags").is_some());
    let at = f
        .vcx
        .debug_bounds("collapse-tags")
        .expect("button")
        .center();
    f.vcx.simulate_click(at, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.rows(), [row(0, "vault", 2), row(0, "editing", 2)]);

    // The filter keeps matches and the tags above them, open.
    f.vcx
        .update(|window, cx| panel.update(cx, |p, cx| p.set_filter_text("tab", window, cx)));
    draw(&mut f.vcx);
    assert!(f.vcx.debug_bounds("tag-filter").is_some());
    assert_eq!(
        f.rows(),
        [row(0, "editing", 2), row(1, "editing/tables", 1)]
    );
    f.vcx
        .update(|window, cx| panel.update(cx, |p, cx| p.set_filter_text("zzz", window, cx)));
    draw(&mut f.vcx);
    assert_eq!(
        panel.read_with(&f.vcx, |p, _| p.message()),
        Some("No matching tags")
    );
    // Esc in the field clears it.
    f.vcx.simulate_keystrokes("escape");
    draw(&mut f.vcx);
    assert!(f.vcx.debug_bounds("tag-filter").is_none());
    assert_eq!(f.rows().len(), 2);

    // Enter on the selected tag searches it.
    f.vcx.simulate_keystrokes("up enter");
    draw(&mut f.vcx);
    assert_eq!(f.search_query().as_deref(), Some("tag:#vault"));
}

#[gpui::test]
fn new_snapshots_update_the_tags(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    f.vcx.dispatch_action(commands::ShowTags);
    draw(&mut f.vcx);
    f.until_rows(&[
        row(0, "editing", 2),
        row(1, "editing/lists", 1),
        row(1, "editing/tables", 1),
        row(0, "vault", 2),
    ]);
    // Unsaved text of a note in the vault reaches the content index.
    let c = f.dir.path().join("C.md");
    crate::files::overlay::dirty(&c, &gv_text::TextSnapshot::new("#vault #new/tag\n"));
    f.until_rows(&[
        row(0, "editing", 2),
        row(1, "editing/lists", 1),
        row(1, "editing/tables", 1),
        row(0, "new", 1),
        row(1, "new/tag", 1),
        row(0, "vault", 2),
    ]);
    crate::files::overlay::clean(&c);
    f.until_rows(&[
        row(0, "editing", 2),
        row(1, "editing/lists", 1),
        row(1, "editing/tables", 1),
        row(0, "vault", 2),
    ]);
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window hands its vault's content snapshots to the Reading views
//! (live blocks use them), in headless GPUI windows over a temporary
//! vault.

use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds,
    WindowOptions, point, px, size,
};

use super::{OpenTarget, ViewMode, Workspace};
use crate::chrome::WindowChrome;
use crate::keymap::actions::workspace as commands;
use crate::reader::Reader;
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
        ("A.md", "# A\n\n```query\nneedle\n```\n"),
        ("B.md", "A needle.\n"),
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

impl Fixture {
    fn poll(&mut self) {
        let vault = self
            .workspace
            .read_with(&self.vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(&mut self.vcx, |v, cx| v.poll_content(cx));
        }
        draw(&mut self.vcx);
    }

    /// The generation of the vault's content snapshot.
    fn vault_generation(&self) -> Option<u64> {
        self.workspace.read_with(&self.vcx, |ws, cx| {
            ws.vault
                .as_ref()
                .and_then(|v| v.read(cx).content())
                .map(|c| c.generation())
        })
    }

    /// Waits (in real time: the index has its own thread) until `reader`
    /// holds the vault's current snapshot, given by the window, and that
    /// snapshot satisfies `f`.
    fn until_given(&mut self, reader: &Entity<Reader>, f: impl Fn(Option<u64>) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.poll();
            let vault = self.vault_generation();
            let (held, given) = reader.read_with(&self.vcx, |r, _| r.content_for_tests());
            if given && vault.is_some() && held == vault && f(held) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "the reader did not get the vault's snapshot in time: {:?}",
            reader.read_with(&self.vcx, |r, _| r.content_for_tests())
        );
    }

    fn reader(&self) -> Entity<Reader> {
        self.workspace
            .read_with(&self.vcx, |ws, _| ws.active_tab().reader().cloned())
            .expect("reader")
    }
}

#[gpui::test]
fn reading_views_get_each_content_snapshot(cx: &mut TestAppContext) {
    let mut f = fixture(cx);
    let reader = f.reader();
    // No vault yet (a window opened for a file): the view looks for one
    // itself.
    assert!(!reader.read_with(&f.vcx, |r, _| r.content_for_tests()).1);

    // Showing the sidebar makes the vault; its snapshots reach the view.
    f.vcx.dispatch_action(commands::ShowFiles);
    f.until_given(&reader, |_| true);
    let first = f.vault_generation();

    // A newer snapshot (unsaved text of a note) follows.
    let b = f.dir.path().join("B.md");
    crate::files::overlay::dirty(&b, &gv_text::TextSnapshot::new("No more.\n"));
    f.until_given(&reader, |g| g != first);
    crate::files::overlay::clean(&b);

    // The Split view's preview pane gets them too.
    f.workspace
        .update(&mut f.vcx, |ws, cx| ws.set_mode(ViewMode::Split, cx));
    draw(&mut f.vcx);
    let preview = f
        .workspace
        .read_with(&f.vcx, |ws, cx| ws.active_tab().split_reader(cx))
        .expect("split preview");
    f.until_given(&preview, |_| true);
}

#[gpui::test]
fn a_view_without_a_window_vault_is_woken_by_an_index_opening(cx: &mut TestAppContext) {
    use gv_vault::content::{ContentIndexer, ContentOptions};
    use gv_vault::{IndexConfig, PathIndex, ProfileId};

    let mut f = fixture(cx);
    let reader = f.reader();
    assert_eq!(
        reader.read_with(&f.vcx, |r, _| r.content_for_tests()),
        (None, false)
    );
    // Another window's vault holding the file opens its content index.
    // No time passes on the test clock: the view is woken, not polling.
    let root = f.dir.path().to_path_buf();
    let paths = PathIndex::build(&root, IndexConfig::default())
        .expect("index")
        .0;
    let mut options = ContentOptions::for_profile(ProfileId::Obsidian);
    options.overlay_debounce = Duration::ZERO;
    let indexer = ContentIndexer::start(paths, options, |_| {}).expect("starts");
    assert!(indexer.flush(Duration::from_secs(10)));
    let built = indexer.snapshot().map(|s| s.generation());
    assert!(built.is_some());
    crate::files::overlay::register(&root, indexer.handle());
    draw(&mut f.vcx);
    assert_eq!(
        reader.read_with(&f.vcx, |r, _| r.content_for_tests()),
        (built, false)
    );
    // A newer snapshot, announced as that window's vault does.
    let b = root.join("B.md");
    crate::files::overlay::dirty(&b, &gv_text::TextSnapshot::new("No more.\n"));
    assert!(indexer.flush(Duration::from_secs(10)));
    let newer = indexer.snapshot().map(|s| s.generation());
    assert_ne!(newer, built);
    crate::files::overlay::content_changed();
    draw(&mut f.vcx);
    assert_eq!(
        reader.read_with(&f.vcx, |r, _| r.content_for_tests()),
        (newer, false)
    );
    crate::files::overlay::clean(&b);
}

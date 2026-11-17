// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Sync conflicts panel in a headless window over a temporary vault,
//! with a trash folder instead of the system trash.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::{
    AppContext as _, Bounds, Entity, Focusable as _, Modifiers, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_vault::content::{ContentOptions, ContentSnapshot, DiskSource};
use gv_vault::files::SyncChoice;
use gv_vault::index::{IndexConfig, PathIndex};
use gv_vault::profile::ProfileId;
use gv_vault::refactor::Trash;

use super::{
    CompareSelected, ConflictsScope, SelectNext, SyncConflictsEvent, SyncConflictsPanel,
    entry_detail,
};
use crate::{AppSettings, ThemeChoice};

const FILES: &[(&str, &str)] = &[
    ("Note.md", "mine\n"),
    ("Note (conflicted copy 2026-01-02).md", "theirs\n"),
    ("Plans/Trip.md", "a\n"),
    ("Plans/Trip.sync-conflict-20260102-101010-ABCDEFG.md", "b\n"),
    ("Other.md", "x\n"),
];

/// Moves files into a folder of its own.
struct FolderTrash(PathBuf);

impl Trash for FolderTrash {
    fn delete(&self, path: &Path) -> Result<(), String> {
        let name = path.file_name().ok_or("no name")?;
        std::fs::rename(path, self.0.join(name)).map_err(|e| e.to_string())
    }
}

/// A trash that always fails.
struct BrokenTrash;

impl Trash for BrokenTrash {
    fn delete(&self, _: &Path) -> Result<(), String> {
        Err("the trash is full".to_owned())
    }
}

fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (name, text) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, text).expect("write");
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

struct Panel {
    dir: tempfile::TempDir,
    bin: tempfile::TempDir,
    panel: Entity<SyncConflictsPanel>,
    vcx: VisualTestContext,
    events: Rc<RefCell<Vec<SyncConflictsEvent>>>,
}

fn panel(cx: &mut TestAppContext, indexed: bool) -> Panel {
    let dir = vault(FILES);
    let bin = tempfile::tempdir().expect("trash dir");
    let content = indexed.then(|| snapshot(dir.path()));
    let root = dir.path().to_path_buf();
    let trash = Arc::new(FolderTrash(bin.path().to_path_buf()));
    let (window, panel) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(260.0), px(600.0)));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        gpui::open_window(options, cx, |_, cx| cx.new(SyncConflictsPanel::new))
            .expect("test window opens")
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = events.clone();
    vcx.update(|_, cx| {
        cx.subscribe(&panel, move |_, e: &SyncConflictsEvent, _| {
            seen.borrow_mut().push(e.clone());
        })
        .detach();
        panel.update(cx, |p, cx| {
            p.set_trash(trash);
            p.set_scope(Some(ConflictsScope { root, content }), cx);
        });
    });
    let mut p = Panel {
        dir,
        bin,
        panel,
        vcx,
        events,
    };
    p.draw();
    p
}

impl Panel {
    fn draw(&mut self) {
        for _ in 0..2 {
            self.vcx.run_until_parked();
            self.vcx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }

    fn click(&mut self, selector: &'static str) {
        let at = self
            .vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.vcx.simulate_click(at, Modifiers::none());
        self.draw();
    }

    fn names(&self) -> Vec<String> {
        self.panel.read_with(&self.vcx, |p, _| {
            p.entries()
                .iter()
                .map(|e| e.copy.as_str().to_owned())
                .collect()
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

#[gpui::test]
fn lists_the_copies_with_their_service_and_original(cx: &mut TestAppContext) {
    let mut p = panel(cx, true);
    assert_eq!(
        p.names(),
        [
            "Note (conflicted copy 2026-01-02).md",
            "Plans/Trip.sync-conflict-20260102-101010-ABCDEFG.md"
        ]
    );
    let details: Vec<String> = p.panel.read_with(&p.vcx, |p, _| {
        p.entries().iter().map(entry_detail).collect()
    });
    assert_eq!(
        details,
        [
            "Dropbox copy of Note.md",
            "Syncthing copy of Trip.md \u{b7} Plans"
        ]
    );
    assert!(p.vcx.debug_bounds("conflict-0").is_some());
    assert!(p.vcx.debug_bounds("conflict-compare-1").is_some());
    assert!(p.vcx.debug_bounds("conflicts-count").is_some());
}

#[gpui::test]
fn without_an_index_the_panel_says_so(cx: &mut TestAppContext) {
    let mut p = panel(cx, false);
    assert!(p.names().is_empty());
    assert!(p.vcx.debug_bounds("conflicts-message").is_some());
    assert!(p.vcx.debug_bounds("conflict-0").is_none());
}

#[gpui::test]
fn compare_and_open_ask_the_window(cx: &mut TestAppContext) {
    let mut p = panel(cx, true);
    p.click("conflict-compare-1");
    p.click("conflict-open-0");
    assert_eq!(
        *p.events.borrow(),
        [
            SyncConflictsEvent::Compare {
                original: p.path("Plans/Trip.md"),
                copy: p.path("Plans/Trip.sync-conflict-20260102-101010-ABCDEFG.md"),
            },
            SyncConflictsEvent::Open(p.path("Note (conflicted copy 2026-01-02).md")),
        ]
    );
    // Keys: down selects the first entry, Enter compares it.
    p.events.borrow_mut().clear();
    let panel = p.panel.clone();
    p.vcx.update(|window, cx| {
        panel.update(cx, |p, _| p.selected = None);
        panel.read(cx).focus_handle(cx).focus(window, cx);
    });
    p.vcx.dispatch_action(SelectNext);
    p.vcx.dispatch_action(CompareSelected);
    p.draw();
    assert_eq!(
        *p.events.borrow(),
        [SyncConflictsEvent::Compare {
            original: p.path("Note.md"),
            copy: p.path("Note (conflicted copy 2026-01-02).md"),
        }]
    );
}

#[gpui::test]
fn keep_original_and_use_copy_settle_through_the_trash(cx: &mut TestAppContext) {
    let mut p = panel(cx, true);
    p.click("conflict-keep-0");
    assert!(!p.path("Note (conflicted copy 2026-01-02).md").exists());
    assert_eq!(
        std::fs::read_to_string(p.path("Note.md")).ok().as_deref(),
        Some("mine\n")
    );
    assert!(
        p.bin
            .path()
            .join("Note (conflicted copy 2026-01-02).md")
            .exists()
    );
    assert_eq!(
        p.names(),
        ["Plans/Trip.sync-conflict-20260102-101010-ABCDEFG.md"]
    );
    p.click("conflict-use-0");
    assert_eq!(
        std::fs::read_to_string(p.path("Plans/Trip.md"))
            .ok()
            .as_deref(),
        Some("b\n")
    );
    assert_eq!(
        std::fs::read_to_string(p.bin.path().join("Trip.md"))
            .ok()
            .as_deref(),
        Some("a\n")
    );
    assert!(p.names().is_empty());
    assert!(matches!(
        p.events.borrow().as_slice(),
        [
            SyncConflictsEvent::Resolved {
                choice: SyncChoice::KeepOriginal,
                ..
            },
            SyncConflictsEvent::Resolved {
                choice: SyncChoice::UseCopy,
                ..
            }
        ]
    ));
    // A snapshot taken before the files moved does not bring them back.
    let stale = {
        let dir = vault(FILES);
        snapshot(dir.path())
    };
    let root = p.dir.path().to_path_buf();
    let panel = p.panel.clone();
    p.vcx.update(|_, cx| {
        panel.update(cx, |pn, cx| {
            pn.set_scope(
                Some(ConflictsScope {
                    root: root.clone(),
                    content: Some(stale),
                }),
                cx,
            );
        });
    });
    assert!(p.names().is_empty());
    // A fresh one has none.
    let fresh = snapshot(&root);
    p.vcx.update(|_, cx| {
        panel.update(cx, |pn, cx| {
            pn.set_scope(
                Some(ConflictsScope {
                    root,
                    content: Some(fresh),
                }),
                cx,
            );
        });
    });
    p.draw();
    assert!(p.names().is_empty());
    assert!(p.vcx.debug_bounds("conflicts-message").is_some());
}

#[gpui::test]
fn a_failed_trash_keeps_the_files_and_says_why(cx: &mut TestAppContext) {
    let mut p = panel(cx, true);
    let panel = p.panel.clone();
    p.vcx
        .update(|_, cx| panel.update(cx, |pn, _| pn.set_trash(Arc::new(BrokenTrash))));
    p.click("conflict-use-0");
    assert!(p.path("Note (conflicted copy 2026-01-02).md").exists());
    assert_eq!(
        std::fs::read_to_string(p.path("Note.md")).ok().as_deref(),
        Some("mine\n")
    );
    assert_eq!(p.names().len(), 2);
    let error = p.panel.read_with(&p.vcx, |pn, _| pn.error().cloned());
    assert_eq!(
        error.as_deref(),
        Some("Cannot move \u{201c}Note.md\u{201d} to the trash: the trash is full.")
    );
    assert!(p.vcx.debug_bounds("conflicts-error").is_some());
    assert!(matches!(
        p.events.borrow().as_slice(),
        [SyncConflictsEvent::Failed(_)]
    ));
}

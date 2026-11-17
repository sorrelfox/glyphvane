// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Git awareness in a headless window: the branch in the status bar and the
//! badges in the Files panel. Repositories are made with `git`; without it
//! the tests are skipped.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use gpui::{
    AnyWindowHandle, AppContext as _, Entity, TestAppContext, VisualTestContext, WindowOptions,
};
use gv_vault::VaultPath;
use gv_vault::git::{Branch, FileStatus};

use super::{OpenTarget, Workspace};
use crate::chrome::WindowChrome;
use crate::{AppSettings, ThemeChoice, ToggleSidebar};

/// Runs `git` in `dir` without the user's or the system's configuration.
fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A repository on branch `topic` with `a.md` and `Notes/b.md` committed,
/// then `a.md` changed, `Notes/new.md` added untracked and `build/`
/// ignored; `None` without `git`.
fn repo() -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    if !git(root, &["init", "-q", "-b", "topic"]) {
        eprintln!("git is not available: skipped");
        return None;
    }
    std::fs::create_dir_all(root.join("Notes")).expect("folder");
    std::fs::write(root.join("a.md"), "# A\n").expect("write");
    std::fs::write(root.join("Notes/b.md"), "# B\n").expect("write");
    std::fs::write(root.join(".gitignore"), "build/\n").expect("write");
    assert!(git(root, &["add", "-A"]));
    assert!(git(root, &["commit", "-q", "-m", "first"]));
    std::fs::write(root.join("a.md"), "# A changed\n").expect("write");
    std::fs::write(root.join("Notes/new.md"), "new\n").expect("write");
    std::fs::create_dir_all(root.join("build")).expect("folder");
    std::fs::write(root.join("build/out.md"), "out\n").expect("write");
    Some(dir)
}

fn open_window(cx: &mut TestAppContext, file: &Path) -> (AnyWindowHandle, Entity<Workspace>) {
    cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        let target = Some(OpenTarget::new(file));
        gpui::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::with_chrome(target, WindowChrome::Linux, window, cx))
        })
        .expect("test window opens")
    })
}

fn draw(vcx: &mut VisualTestContext) {
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
}

/// Waits (in real time: the watcher has its own thread) until the window's
/// vault has a Git status.
fn until_git(workspace: &Entity<Workspace>, vcx: &mut VisualTestContext) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        draw(vcx);
        let vault = workspace.read_with(vcx, |ws, _| ws.vault.clone());
        if let Some(vault) = vault {
            vault.update(vcx, |v, cx| {
                v.poll_git(cx);
            });
            if vault.read_with(vcx, |v, _| v.git().is_some()) {
                draw(vcx);
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the Git status did not arrive in time");
}

#[gpui::test]
fn the_branch_and_the_badges_show(cx: &mut TestAppContext) {
    let Some(dir) = repo() else {
        return;
    };
    let root = dir.path();
    let (window, workspace) = open_window(cx, &root.join("a.md"));
    cx.dispatch_action(window, ToggleSidebar);
    let mut vcx = VisualTestContext::from_window(window, cx);
    until_git(&workspace, &mut vcx);

    let vault = workspace
        .read_with(&vcx, |ws, _| ws.vault.clone())
        .expect("vault");
    let status = vault.read_with(&vcx, |v, _| v.git().cloned().expect("status"));
    assert_eq!(status.branch, Branch::Named("topic".into()));
    let path = |p: &str| VaultPath::new(p).expect("path");
    assert_eq!(status.file(&path("a.md")), Some(FileStatus::Modified));
    assert_eq!(
        status.file(&path("Notes/new.md")),
        Some(FileStatus::Untracked)
    );
    assert_eq!(
        status.file(&path("build/out.md")),
        Some(FileStatus::Ignored)
    );

    // The status bar shows the branch.
    assert!(vcx.debug_bounds("status-git-branch").is_some());
    let branch = workspace.read_with(&vcx, |ws, cx| ws.git_branch(cx));
    assert_eq!(branch.as_deref(), Some("topic"));

    // Rows: Notes (folder: untracked inside), build (ignored: no badge),
    // Notes/… collapsed; a.md modified.
    let rows: Vec<String> = workspace
        .read_with(&vcx, |ws, _| ws.files.clone())
        .expect("files panel")
        .read_with(&vcx, |p, _| {
            p.visible_rows().into_iter().map(|(_, n)| n).collect()
        });
    let badge = |name: &str, vcx: &mut VisualTestContext| {
        let ix = rows.iter().position(|r| r == name).expect("row");
        vcx.debug_bounds(Box::leak(format!("git-badge-{ix}").into_boxed_str()))
            .is_some()
    };
    assert!(badge("a.md", &mut vcx), "{rows:?}");
    assert!(
        badge("Notes", &mut vcx),
        "a folder sums up its files: {rows:?}"
    );
    if rows.iter().any(|r| r == "build") {
        assert!(!badge("build", &mut vcx), "ignored entries are dimmed only");
    }
}

#[gpui::test]
fn a_vault_outside_a_work_tree_has_no_branch(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("a.md"), "# A\n").expect("write");
    let (window, workspace) = open_window(cx, &dir.path().join("a.md"));
    cx.dispatch_action(window, ToggleSidebar);
    let mut vcx = VisualTestContext::from_window(window, cx);
    for _ in 0..5 {
        draw(&mut vcx);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(vcx.debug_bounds("status-git-branch").is_none());
    assert_eq!(workspace.read_with(&vcx, |ws, cx| ws.git_branch(cx)), None);
}

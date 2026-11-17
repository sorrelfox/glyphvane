// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Each test makes a repository with the `git` program and is skipped
//! (passes without checking) where `git` is not installed.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::*;

fn p(s: &str) -> VaultPath {
    VaultPath::new(s).expect("path")
}

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

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("parent")).expect("folders");
    fs::write(full, text).expect("write");
}

/// A repository on branch `main` with `files` committed, or `None`
/// without `git`.
fn repo(files: &[(&str, &str)]) -> Option<tempfile::TempDir> {
    let dir = tempfile::tempdir().expect("temp dir");
    if !git(dir.path(), &["init", "-q", "-b", "main"]) {
        eprintln!("git is not available: skipped");
        return None;
    }
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    assert!(git(dir.path(), &["add", "-A"]));
    assert!(git(dir.path(), &["commit", "-q", "-m", "first"]));
    Some(dir)
}

fn status(root: &Path) -> GitStatus {
    read_status(root, &Arc::new(AtomicBool::new(false)))
        .expect("status")
        .expect("a work tree")
}

#[test]
fn outside_a_work_tree_there_is_no_status() {
    let dir = tempfile::tempdir().expect("temp dir");
    if find_work_tree(dir.path()).is_some() {
        // The temporary folder is itself inside a repository here.
        return;
    }
    assert_eq!(
        read_status(dir.path(), &Arc::new(AtomicBool::new(false))),
        Ok(None)
    );
    assert!(GitWatcher::start(dir.path(), GitWatchOptions::default(), |_| {}).is_none());
}

#[test]
fn branch_and_file_statuses() {
    let Some(dir) = repo(&[
        ("a.md", "a\n"),
        ("b.md", "b\n"),
        ("gone.md", "x\n"),
        (".gitignore", "build/\n*.tmp\n"),
    ]) else {
        return;
    };
    let root = dir.path();
    write(root, "a.md", "a changed\n");
    write(root, "c.md", "staged\n");
    assert!(git(root, &["add", "c.md"]));
    write(root, "new/d.md", "untracked\n");
    write(root, "new/e.md", "untracked\n");
    write(root, "loose.md", "untracked\n");
    write(root, "build/out.md", "ignored\n");
    write(root, "x.tmp", "ignored\n");
    fs::remove_file(root.join("gone.md")).expect("remove");
    let s = status(root);
    assert_eq!(s.branch, Branch::Named("main".into()));
    assert_eq!(s.file(&p("a.md")), Some(FileStatus::Modified));
    assert_eq!(s.file(&p("b.md")), None);
    assert_eq!(s.file(&p("c.md")), Some(FileStatus::Added));
    assert_eq!(s.file(&p("gone.md")), Some(FileStatus::Deleted));
    assert_eq!(s.file(&p("loose.md")), Some(FileStatus::Untracked));
    // A new folder is untracked as a whole.
    assert_eq!(s.file(&p("new/d.md")), Some(FileStatus::Untracked));
    assert_eq!(s.folder(&p("new")), Some(FileStatus::Untracked));
    assert_eq!(s.file(&p("build/out.md")), Some(FileStatus::Ignored));
    assert_eq!(s.file(&p("x.tmp")), Some(FileStatus::Ignored));
    // The vault folder sums up: modified wins over the rest.
    assert_eq!(s.folder(&VaultPath::root()), Some(FileStatus::Modified));
    assert_eq!(FileStatus::Modified.badge(), Some('M'));
    assert_eq!(FileStatus::Untracked.badge(), Some('U'));
    assert!(s.changed_count() >= 5);
}

#[test]
fn a_vault_in_a_subfolder_gets_vault_paths() {
    let Some(dir) = repo(&[("docs/a.md", "a\n"), ("src/main.rs", "fn main() {}\n")]) else {
        return;
    };
    let root = dir.path();
    write(root, "docs/a.md", "changed\n");
    write(root, "src/main.rs", "changed\n");
    write(root, "docs/n.md", "new\n");
    let s = status(&root.join("docs"));
    assert_eq!(s.file(&p("a.md")), Some(FileStatus::Modified));
    assert_eq!(s.file(&p("n.md")), Some(FileStatus::Untracked));
    let all: Vec<&str> = s.entries().map(|(p, _)| p.as_str()).collect();
    assert_eq!(all, ["a.md", "n.md"]);
    assert_eq!(
        find_work_tree(&root.join("docs")).map(|(_, prefix)| prefix),
        Some("docs/".to_owned())
    );
}

#[test]
fn detached_and_unborn_heads() {
    let dir = tempfile::tempdir().expect("temp dir");
    if !git(dir.path(), &["init", "-q", "-b", "trunk"]) {
        return;
    }
    assert_eq!(status(dir.path()).branch, Branch::Unborn("trunk".into()));
    write(dir.path(), "a.md", "a\n");
    assert!(git(dir.path(), &["add", "-A"]));
    assert!(git(dir.path(), &["commit", "-q", "-m", "one"]));
    assert!(git(dir.path(), &["checkout", "-q", "--detach"]));
    let Branch::Detached(id) = status(dir.path()).branch else {
        panic!("detached");
    };
    assert_eq!(id.len(), 7);
}

#[test]
fn the_watcher_reports_changes_after_a_refresh() {
    let Some(dir) = repo(&[("a.md", "a\n")]) else {
        return;
    };
    let root = dir.path();
    let options = GitWatchOptions {
        debounce: Duration::from_millis(50),
        ..GitWatchOptions::default()
    };
    let (watcher, events) = GitWatcher::start_channel(root, options).expect("watcher");
    let first = events.recv_timeout(Duration::from_secs(10)).expect("first");
    let GitEvent::Updated(first) = first else {
        panic!("{first:?}");
    };
    assert_eq!(first.changed_count(), 0);
    write(root, "a.md", "changed\n");
    watcher.refresh();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match events.recv_timeout(left) {
            Ok(GitEvent::Updated(s)) if s.file(&p("a.md")) == Some(FileStatus::Modified) => break,
            Ok(_) => {}
            Err(e) => panic!("no update: {e}"),
        }
    }
    assert_eq!(
        watcher.snapshot().and_then(|s| s.file(&p("a.md"))),
        Some(FileStatus::Modified)
    );
    // A branch switch is seen through the repository folder alone.
    assert!(git(root, &["checkout", "-q", "-b", "topic"]));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match events.recv_timeout(left) {
            Ok(GitEvent::Updated(s)) if s.branch == Branch::Named("topic".into()) => break,
            Ok(_) => {}
            Err(e) => panic!("no branch update: {e}"),
        }
    }
}

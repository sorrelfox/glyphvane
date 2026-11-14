// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The vault watcher on a real file system (temporary directories).
//!
//! Every wait is bounded; the debounce is shortened so the tests stay fast.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use gv_vault::files::{
    Backup, DiskState, ExternalChange, WriteStrategy, sync_conflict_copies, write_file,
};
use gv_vault::index::conflict::SyncService;
use gv_vault::watch::{ActiveMode, PollReason, RescanReason, UpdateCause, WatchNotice};
use gv_vault::{
    Change, IndexConfig, PathIndex, SharedPathIndex, VaultPath, VaultWatcher, WatchEvent,
    WatchMode, WatchOptions,
};

/// The longest any single wait may take before the test fails.
const TIMEOUT: Duration = Duration::from_secs(10);

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    shared: Arc<SharedPathIndex>,
    watcher: Option<VaultWatcher>,
    rx: Receiver<WatchEvent>,
    seen: Vec<WatchEvent>,
}

fn options() -> WatchOptions {
    WatchOptions {
        debounce: Duration::from_millis(50),
        ..WatchOptions::default()
    }
}

fn polling() -> WatchOptions {
    WatchOptions {
        mode: WatchMode::Polling,
        poll_interval: Duration::from_millis(100),
        ..options()
    }
}

impl Harness {
    fn start(setup: impl FnOnce(&Path), options: WatchOptions) -> Self {
        Self::start_with(setup, options, IndexConfig::default())
    }

    fn start_with(setup: impl FnOnce(&Path), options: WatchOptions, config: IndexConfig) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        setup(&root);
        let (index, _) = PathIndex::build(&root, config).expect("build");
        let shared = Arc::new(SharedPathIndex::new(index));
        let (watcher, rx) =
            VaultWatcher::start_channel(shared.clone(), options).expect("watcher starts");
        let mut h = Self {
            _dir: dir,
            root,
            shared,
            watcher: Some(watcher),
            rx,
            seen: Vec::new(),
        };
        h.wait("ready", |e| {
            matches!(e, WatchEvent::Ready { .. }).then_some(())
        });
        h
    }

    fn watcher(&self) -> &VaultWatcher {
        self.watcher.as_ref().expect("running")
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn index(&self) -> PathIndex {
        self.shared.load()
    }

    /// Waits for an event `f` accepts.
    fn wait<T>(&mut self, what: &str, mut f: impl FnMut(&WatchEvent) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(event) => {
                    let found = f(&event);
                    self.seen.push(event);
                    if let Some(found) = found {
                        return found;
                    }
                }
                Err(_) => panic!("timed out waiting for {what}; seen: {:#?}", self.summary()),
            }
        }
    }

    /// Waits until the published index satisfies `pred`.
    fn wait_index(&mut self, what: &str, pred: impl Fn(&PathIndex) -> bool) {
        if pred(&self.index()) {
            return;
        }
        self.wait(what, |e| match e {
            WatchEvent::IndexUpdated { index, .. } if pred(index) => Some(()),
            _ => None,
        });
    }

    /// Collects events for `d`.
    fn drain(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while let Ok(event) = self
            .rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            self.seen.push(event);
        }
    }

    fn summary(&self) -> Vec<String> {
        self.seen
            .iter()
            .map(|e| match e {
                WatchEvent::IndexUpdated {
                    changes,
                    cause,
                    own_writes,
                    ..
                } => format!("IndexUpdated {cause:?} {changes:?} own={own_writes:?}"),
                WatchEvent::IndexBuilt { .. } => "IndexBuilt".to_owned(),
                WatchEvent::DocumentChanged { path, change } => {
                    format!("DocumentChanged {} {}", path.display(), change_name(change))
                }
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn changes(&self) -> Vec<Change> {
        self.seen
            .iter()
            .filter_map(|e| match e {
                WatchEvent::IndexUpdated { changes, .. } => Some(changes.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn doc_changes(&self, path: &Path) -> Vec<&ExternalChange> {
        self.seen
            .iter()
            .filter_map(|e| match e {
                WatchEvent::DocumentChanged { path: p, change } if p == path => Some(change),
                _ => None,
            })
            .collect()
    }

    fn track(&self, rel: &str) -> PathBuf {
        let path = self.path(rel);
        let (_, baseline) = DiskState::read(&path).expect("read baseline");
        self.watcher().track(path.clone(), baseline);
        path
    }

    fn wait_modified(&mut self, path: &Path, bytes: &[u8]) {
        self.wait("document modified", |e| match e {
            WatchEvent::DocumentChanged {
                path: p,
                change: ExternalChange::Modified { bytes: b, .. },
            } if p == path && b == bytes => Some(()),
            _ => None,
        });
    }
}

fn change_name(change: &ExternalChange) -> &'static str {
    match change {
        ExternalChange::Unchanged => "Unchanged",
        ExternalChange::Touched(_) => "Touched",
        ExternalChange::Modified { .. } => "Modified",
        ExternalChange::Deleted => "Deleted",
    }
}

fn touches(changes: &[Change], path: &str) -> bool {
    changes.iter().any(|c| match c {
        Change::Upsert { path: p, .. } | Change::Remove { path: p } => {
            p.as_str() == path || p.as_str().starts_with(&format!("{path}/"))
        }
        Change::Rename { from, to } => from.as_str() == path || to.as_str() == path,
    })
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

#[test]
fn create_modify_and_delete_a_file() {
    let mut h = Harness::start(|_| {}, options());
    assert_eq!(h.watcher().mode(), ActiveMode::Native);

    write(&h.root, "a.md", "one\n");
    h.wait_index("a.md indexed", |i| {
        i.entry_by_path("a.md").is_some_and(|e| e.meta.size == 4)
    });
    let id = h.index().id_of("a.md");

    let path = h.track("a.md");
    write(&h.root, "a.md", "one two\n");
    h.wait_modified(&path, b"one two\n");
    h.wait_index("new size", |i| {
        i.entry_by_path("a.md").is_some_and(|e| e.meta.size == 8)
    });
    assert_eq!(h.index().id_of("a.md"), id, "a modification keeps the id");

    fs::remove_file(&path).expect("remove");
    h.wait("document deleted", |e| match e {
        WatchEvent::DocumentChanged {
            change: ExternalChange::Deleted,
            ..
        } => Some(()),
        _ => None,
    });
    h.wait_index("a.md removed", |i| i.entry_by_path("a.md").is_none());
    h.index().check_consistency().expect("consistent");
}

#[test]
fn rename_keeps_the_file_id_and_follows_the_document() {
    let mut h = Harness::start(|root| write(root, "dir/note.md", "text\n"), options());
    let id = h.index().id_of("dir/note.md").expect("indexed");
    let from = h.track("dir/note.md");
    let to = h.path("renamed.md");
    fs::rename(&from, &to).expect("rename");
    h.wait("document moved", |e| match e {
        WatchEvent::DocumentMoved { from: f, to: t } if *f == from && *t == to => Some(()),
        _ => None,
    });
    h.wait_index("renamed", |i| i.id_of("renamed.md") == Some(id));
    assert!(h.index().entry_by_path("dir/note.md").is_none());
    assert!(
        h.changes()
            .iter()
            .any(|c| matches!(c, Change::Rename { to, .. } if to.as_str() == "renamed.md"))
    );
    // The document is tracked under its new path.
    write(&h.root, "renamed.md", "changed\n");
    h.wait_modified(&to, b"changed\n");
    assert!(
        h.doc_changes(&from).is_empty(),
        "no change reported for the old path: {:#?}",
        h.summary()
    );
}

#[test]
fn folders_are_created_moved_and_deleted() {
    let mut h = Harness::start(|_| {}, options());
    write(&h.root, "dir/sub/x.md", "x\n");
    write(&h.root, "dir/y.md", "y\n");
    h.wait_index("folder contents indexed", |i| {
        i.entry_by_path("dir/sub/x.md").is_some() && i.entry_by_path("dir/y.md").is_some()
    });
    let index = h.index();
    let ids = (
        index.id_of("dir"),
        index.id_of("dir/sub/x.md"),
        index.id_of("dir/y.md"),
    );

    fs::rename(h.path("dir"), h.path("moved")).expect("rename folder");
    h.wait_index("folder moved", |i| {
        i.entry_by_path("moved/sub/x.md").is_some() && i.entry_by_path("dir").is_none()
    });
    let index = h.index();
    assert_eq!(
        (
            index.id_of("moved"),
            index.id_of("moved/sub/x.md"),
            index.id_of("moved/y.md")
        ),
        ids,
        "a folder move keeps every id"
    );

    // A folder moved in from outside the vault is read in full.
    let outside = tempfile::tempdir_in(h.root.parent().expect("parent")).expect("temp dir");
    write(outside.path(), "incoming/a/b.md", "b\n");
    write(outside.path(), "incoming/c.md", "c\n");
    fs::rename(outside.path().join("incoming"), h.path("incoming")).expect("move in");
    h.wait_index("moved-in folder indexed", |i| {
        i.entry_by_path("incoming/a/b.md").is_some() && i.entry_by_path("incoming/c.md").is_some()
    });

    fs::remove_dir_all(h.path("moved")).expect("remove folder");
    h.wait_index("folder removed", |i| {
        i.entry_by_path("moved").is_none() && i.entry_by_path("moved/sub/x.md").is_none()
    });
    h.index().check_consistency().expect("consistent");
}

/// An editor's atomic save: write a temporary file, rename it over the
/// original. Seen as one modification of the target, whether the steps land
/// in one debounce window or two.
#[test]
fn atomic_save_is_one_modification() {
    let mut h = Harness::start(|root| write(root, "Note.md", "v1\n"), options());
    let id = h.index().id_of("Note.md").expect("indexed");
    let path = h.track("Note.md");

    // One window.
    write(&h.root, "Note.md.tmp", "v2\n");
    fs::rename(h.path("Note.md.tmp"), &path).expect("replace");
    h.wait_modified(&path, b"v2\n");
    h.wait_index("temporary file gone", |i| {
        i.entry_by_path("Note.md.tmp").is_none()
    });

    // Two windows: the temporary file is indexed first.
    write(&h.root, "Note.md.tmp", "v3\n");
    h.wait_index("temporary file indexed", |i| {
        i.entry_by_path("Note.md.tmp").is_some()
    });
    fs::rename(h.path("Note.md.tmp"), &path).expect("replace");
    h.wait_modified(&path, b"v3\n");
    h.wait_index("temporary file gone", |i| {
        i.entry_by_path("Note.md.tmp").is_none()
    });

    assert_eq!(
        h.index().id_of("Note.md"),
        Some(id),
        "the target keeps its id"
    );
    let changes = h.changes();
    assert!(
        !changes
            .iter()
            .any(|c| matches!(c, Change::Remove { path } if path.as_str() == "Note.md")),
        "never removed: {changes:?}"
    );
    assert!(
        h.doc_changes(&path)
            .iter()
            .all(|c| matches!(c, ExternalChange::Modified { .. })),
        "only modifications: {:?}",
        h.summary()
    );
}

struct NoBackup;

impl Backup for NoBackup {
    fn store(&mut self, _: &Path, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn clear(&mut self, _: &Path) -> io::Result<()> {
        Ok(())
    }
}

fn own_save(h: &mut Harness, path: &Path, bytes: &[u8]) {
    h.watcher().begin_write(path);
    let report =
        write_file(path, bytes, WriteStrategy::Auto, &mut NoBackup, None).expect("write_file");
    h.watcher().expect_write(path, report.state);
}

#[test]
fn own_saves_are_not_external_changes() {
    let mut h = Harness::start(|root| write(root, "Note.md", "v1\n"), options());
    let path = h.track("Note.md");

    own_save(&mut h, &path, b"mine\n");
    h.wait_index("own save indexed", |i| {
        i.entry_by_path("Note.md").is_some_and(|e| e.meta.size == 5)
    });
    // Anything the save raised arrives within a few debounce windows.
    h.drain(Duration::from_millis(300));
    assert!(
        h.doc_changes(&path).is_empty(),
        "own save reported: {:?}",
        h.summary()
    );

    // A later external write is reported against the saved state.
    write(&h.root, "Note.md", "theirs\n");
    h.wait_modified(&path, b"theirs\n");
}

#[test]
fn own_saves_are_not_external_changes_when_polling() {
    let mut h = Harness::start(|root| write(root, "Note.md", "v1\n"), polling());
    let path = h.track("Note.md");
    own_save(&mut h, &path, b"mine\n");
    h.wait_index("own save indexed", |i| {
        i.entry_by_path("Note.md").is_some_and(|e| e.meta.size == 5)
    });
    h.drain(Duration::from_millis(300));
    assert!(
        h.doc_changes(&path).is_empty(),
        "own save reported: {:?}",
        h.summary()
    );
    write(&h.root, "Note.md", "theirs\n");
    h.wait_modified(&path, b"theirs\n");
}

#[test]
fn overflow_rescans_without_id_churn() {
    let mut h = Harness::start(
        |root| {
            write(root, "keep.md", "k\n");
            write(root, "dir/lost.md", "l\n");
        },
        options(),
    );
    let keep = h.index().id_of("keep.md").expect("indexed");
    let dir = h.index().id_of("dir").expect("indexed");
    // Make the index disagree with the disk, as missed events would.
    h.shared
        .apply(&[
            Change::Remove {
                path: VaultPath::new("dir/lost.md").expect("path"),
            },
            Change::Upsert {
                path: VaultPath::new("ghost.md").expect("path"),
                kind: gv_vault::EntryKind::File,
                meta: gv_vault::FileMeta::default(),
            },
        ])
        .expect("apply");

    h.watcher().inject_overflow();
    h.wait("events-lost notice", |e| {
        matches!(e, WatchEvent::Notice(WatchNotice::EventsLost)).then_some(())
    });
    let changes = h.wait("rescan", |e| match e {
        WatchEvent::IndexUpdated {
            cause: UpdateCause::Rescan(RescanReason::Overflow),
            changes,
            ..
        } => Some(changes.clone()),
        _ => None,
    });
    let index = h.index();
    assert!(index.entry_by_path("ghost.md").is_none());
    assert!(index.entry_by_path("dir/lost.md").is_some());
    assert_eq!(
        index.id_of("keep.md"),
        Some(keep),
        "unchanged paths keep ids"
    );
    assert_eq!(index.id_of("dir"), Some(dir));
    assert!(!touches(&changes, "keep.md"), "{changes:?}");
    assert_eq!(changes.len(), 2, "only the differences: {changes:?}");

    // A watcher error also rescans (nothing to change now) and is noticed.
    h.watcher().inject_error("simulated failure");
    h.wait("watcher-error notice", |e| match e {
        WatchEvent::Notice(WatchNotice::WatcherError(m)) if m.contains("simulated") => Some(()),
        _ => None,
    });
    // The watcher keeps working.
    write(&h.root, "after.md", "a\n");
    h.wait_index("after.md indexed", |i| {
        i.entry_by_path("after.md").is_some()
    });
}

#[test]
fn polling_detects_changes() {
    let mut h = Harness::start(|root| write(root, "a.md", "one\n"), polling());
    assert_eq!(h.watcher().mode(), ActiveMode::Polling(PollReason::Forced));
    let id = h.index().id_of("a.md").expect("indexed");
    let path = h.track("a.md");

    write(&h.root, "new/b.md", "b\n");
    h.wait_index("b.md indexed", |i| i.entry_by_path("new/b.md").is_some());

    // A different size: detected by metadata, confirmed by hash.
    write(&h.root, "a.md", "one two\n");
    h.wait_modified(&path, b"one two\n");

    // A rename is recognised by file identity.
    let to = h.path("c.md");
    fs::rename(&path, &to).expect("rename");
    h.wait("document moved", |e| match e {
        WatchEvent::DocumentMoved { to: t, .. } if *t == to => Some(()),
        _ => None,
    });
    assert_eq!(h.index().id_of("c.md"), Some(id));

    fs::remove_file(h.path("new/b.md")).expect("remove");
    h.wait_index("b.md removed", |i| i.entry_by_path("new/b.md").is_none());
    assert!(h.seen.iter().all(|e| !matches!(
        e,
        WatchEvent::IndexUpdated {
            cause: UpdateCause::Events,
            ..
        }
    )));
}

#[test]
fn excluded_paths_are_ignored() {
    let mut h = Harness::start(|_| {}, options());
    for rel in [
        "node_modules/pkg/readme.md",
        ".hidden/x.md",
        ".git/HEAD",
        ".obsidian/app.json",
        "dir/.Note.md.gv-tmp-0a1b2c",
        ".dotfile.md",
    ] {
        write(&h.root, rel, "x\n");
    }
    write(&h.root, "sentinel.md", "s\n");
    h.wait_index("sentinel indexed", |i| {
        i.entry_by_path("sentinel.md").is_some()
    });
    h.drain(Duration::from_millis(150));
    let index = h.index();
    for rel in [
        "node_modules",
        ".hidden",
        ".git",
        ".obsidian",
        "dir/.Note.md.gv-tmp-0a1b2c",
        ".dotfile.md",
    ] {
        assert!(index.entry_by_path(rel).is_none(), "{rel} indexed");
        assert!(!touches(&h.changes(), rel), "{rel} in changes");
    }
}

#[test]
fn gitignored_paths_are_ignored_in_repositories() {
    let mut h = Harness::start_with(
        |root| write(root, ".gitignore", "build/\n*.log\n"),
        options(),
        IndexConfig::git_repo(),
    );
    write(&h.root, "build/out.md", "x\n");
    write(&h.root, "debug.log", "x\n");
    write(&h.root, ".github/README.md", "x\n");
    write(&h.root, "sentinel.md", "s\n");
    h.wait_index("sentinel indexed", |i| {
        i.entry_by_path("sentinel.md").is_some() && i.entry_by_path(".github/README.md").is_some()
    });
    h.drain(Duration::from_millis(150));
    let index = h.index();
    assert!(index.entry_by_path("build").is_none());
    assert!(index.entry_by_path("debug.log").is_none());
}

#[test]
fn sync_conflict_copies_are_reported() {
    let mut h = Harness::start(|root| write(root, "Note.md", "mine\n"), options());
    let name = "Note (conflicted copy 2026-01-02).md";
    write(&h.root, name, "theirs\n");
    let (copy, original, service) = h.wait("conflict copy", |e| match e {
        WatchEvent::SyncConflict {
            copy,
            original,
            service,
        } => Some((copy.clone(), original.clone(), *service)),
        _ => None,
    });
    assert_eq!(copy, h.path(name));
    assert_eq!(original, h.path("Note.md"));
    assert_eq!(service, SyncService::Dropbox);
    // The same rules as the per-file listing.
    let listed = sync_conflict_copies(&h.path("Note.md")).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, copy);

    // Syncthing writes a hidden temporary file and renames it into place.
    let syncthing = "Note.sync-conflict-20260102-101010-ABCDEFG.md";
    write(&h.root, ".syncthing.Note.md.tmp", "other\n");
    fs::rename(h.path(".syncthing.Note.md.tmp"), h.path(syncthing)).expect("rename");
    h.wait("syncthing copy", |e| match e {
        WatchEvent::SyncConflict {
            service: SyncService::Syncthing,
            copy,
            ..
        } if copy.ends_with(syncthing) => Some(()),
        _ => None,
    });

    // An ordinary new file is not a conflict copy.
    write(&h.root, "Other.md", "x\n");
    h.wait_index("Other.md indexed", |i| {
        i.entry_by_path("Other.md").is_some()
    });
    let conflicts = h
        .seen
        .iter()
        .filter(|e| matches!(e, WatchEvent::SyncConflict { .. }))
        .count();
    assert_eq!(conflicts, 2);
}

#[test]
fn building_index_is_walked_at_start() {
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), "a/b.md", "b\n");
    let shared = Arc::new(SharedPathIndex::building(
        dir.path(),
        IndexConfig::default(),
    ));
    let (watcher, rx) = VaultWatcher::start_channel(shared.clone(), options()).expect("start");
    let deadline = Instant::now() + TIMEOUT;
    let mut built = false;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(WatchEvent::IndexBuilt { .. }) => built = true,
            Ok(WatchEvent::Ready { .. }) => break,
            Ok(_) => {}
            Err(e) => panic!("no ready event: {e:?}"),
        }
    }
    assert!(built);
    assert!(shared.load().is_complete());
    assert!(shared.load().entry_by_path("a/b.md").is_some());
    drop(watcher);
}

#[test]
fn a_missing_root_is_an_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let shared = Arc::new(SharedPathIndex::building(
        dir.path().join("missing"),
        IndexConfig::default(),
    ));
    assert!(VaultWatcher::start_channel(shared, options()).is_err());
}

#[test]
fn dropping_the_watcher_stops_it() {
    for options in [options(), polling()] {
        let mut h = Harness::start(|_| {}, options);
        let started = Instant::now();
        drop(h.watcher.take());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown took {:?}",
            started.elapsed()
        );
        // The worker thread has exited (and dropped the event sink): the
        // channel is closed once its backlog is read.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match h
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(_) => {}
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => panic!("sink still alive after drop"),
            }
        }
        // Changes after the drop go nowhere and nothing panics.
        write(&h.root, "late.md", "x\n");
    }
}

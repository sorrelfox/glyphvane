// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The content index service on a real file system (temporary
//! directories): the cache round trip, rebuilds of unusable caches,
//! watcher-driven updates, overlays and cancellation.
//!
//! Every wait is bounded.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gv_text::TextSnapshot;
use gv_vault::content::{
    CacheUse, ContentEvent, ContentIndexer, ContentOptions, ContentSnapshot, FileSource,
    RecordKind, store,
};
use gv_vault::{
    IndexConfig, PathIndex, ProfileId, SharedPathIndex, VaultPath, VaultWatcher, WatchEvent,
    WatchOptions,
};

const TIMEOUT: Duration = Duration::from_secs(10);

fn vp(s: &str) -> VaultPath {
    VaultPath::new(s).expect("valid path")
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
    fs::write(full, text).expect("write");
}

fn sample(root: &Path) {
    write(root, "A.md", "# Alpha\n\n[[B]] and [[C]] #one\n");
    write(root, "B.md", "---\naliases: [Bee]\n---\n[[A]]\n");
    write(root, "sub/C.md", "- [ ] task [[A#Alpha]]\n");
}

fn options(cache: Option<PathBuf>) -> ContentOptions {
    let mut o = ContentOptions::for_profile(ProfileId::Obsidian);
    o.threads = 2;
    o.cache_file = cache;
    o.overlay_debounce = Duration::from_millis(20);
    o
}

fn build(root: &Path) -> PathIndex {
    PathIndex::build(root, IndexConfig::obsidian(Vec::new()))
        .expect("path index")
        .0
}

fn start(root: &Path, options: ContentOptions) -> (ContentIndexer, Receiver<ContentEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let indexer = ContentIndexer::start(build(root), options, move |e| {
        let _ = tx.lock().expect("lock").send(e);
    })
    .expect("indexer starts");
    (indexer, rx)
}

fn ready(rx: &Receiver<ContentEvent>) -> (ContentSnapshot, gv_vault::content::ColdStats) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left).expect("ready in time") {
            ContentEvent::Ready { snapshot, stats } => return (snapshot, stats),
            _ => continue,
        }
    }
}

/// Polls `f` on the indexer's snapshots until it holds.
fn eventually(indexer: &ContentIndexer, what: &str, f: impl Fn(&ContentSnapshot) -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        if indexer.snapshot().is_some_and(|s| f(&s)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for: {what}");
}

#[test]
fn cache_round_trip_gives_a_warm_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vault");
    let cache_dir = dir.path().join("cache");
    sample(&root);
    let cache = store::cache_file(&cache_dir, &root);

    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    let (cold, stats) = ready(&rx);
    assert!(
        matches!(stats.cache, CacheUse::NotUsed(_)),
        "{:?}",
        stats.cache
    );
    assert_eq!(stats.notes, 3);
    assert_eq!(stats.read, 3);
    assert_eq!(
        cold.backlink_sources(&vp("A.md")),
        vec![vp("B.md"), vp("sub/C.md")]
    );
    indexer.stop();
    assert!(cache.is_file());

    // Nothing changed: no file is read again.
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    let (warm, stats) = ready(&rx);
    assert_eq!(stats.cache, CacheUse::Loaded(3));
    // Files written within the racy window of the cache are compared by
    // bytes; none were parsed again.
    assert_eq!(stats.parsed, 0);
    assert_eq!(warm.debug_dump(), cold.debug_dump());
    indexer.stop();

    // A note changed while the app was closed is read again.
    std::thread::sleep(Duration::from_millis(20));
    write(&root, "B.md", "no links now, a longer text\n");
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    let (s, stats) = ready(&rx);
    assert_eq!(stats.cache, CacheUse::Loaded(3));
    assert!(stats.parsed >= 1);
    assert_eq!(s.backlink_sources(&vp("A.md")), vec![vp("sub/C.md")]);
    indexer.stop();
}

#[test]
fn a_warm_start_after_changes_matches_a_cold_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vault");
    sample(&root);
    write(&root, "notes/Target.md", "t\n");
    write(&root, "Links.md", "[[Target]] [[Gone]] [[C]]\n");
    write(&root, "Gone.md", "soon gone\n");
    let cache = store::cache_file(&dir.path().join("cache"), &root);
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    ready(&rx);
    indexer.stop();

    // While the app is closed: a note is deleted, a second `Target`
    // makes `[[Target]]` ambiguous, a root `C.md` takes over `[[C]]`.
    fs::remove_file(root.join("Gone.md")).expect("delete");
    write(&root, "other/Target.md", "t2\n");
    write(&root, "C.md", "root c\n");
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    let (warm, stats) = ready(&rx);
    assert_eq!(stats.cache, CacheUse::Loaded(6));
    indexer.stop();
    let (indexer, rx) = start(&root, options(None));
    let (cold, _) = ready(&rx);
    indexer.stop();
    assert_eq!(warm.debug_dump(), cold.debug_dump());
    assert!(warm.unresolved().iter().any(|u| &*u.link.raw == "Gone"));
    assert_eq!(
        warm.backlink_sources(&vp("C.md")),
        vec![vp("A.md"), vp("Links.md")]
    );
}

#[test]
fn unusable_caches_are_rebuilt() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("vault");
    sample(&root);
    let cache = store::cache_file(&dir.path().join("cache"), &root);
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    let (reference, _) = ready(&rx);
    indexer.stop();

    // A newer format version.
    let mut bytes = fs::read(&cache).expect("cache");
    bytes[8..12].copy_from_slice(&(store::FORMAT_VERSION + 1).to_le_bytes());
    fs::write(&cache, &bytes).expect("write");
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    let (s, stats) = ready(&rx);
    match &stats.cache {
        CacheUse::NotUsed(why) => assert!(why.contains("version"), "{why}"),
        other => panic!("cache used: {other:?}"),
    }
    assert_eq!(stats.read, 3);
    assert_eq!(s.debug_dump(), reference.debug_dump());
    indexer.stop();
    // The rebuilt cache is current again.
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    assert_eq!(ready(&rx).1.cache, CacheUse::Loaded(3));
    indexer.stop();

    // A damaged file.
    let mut bytes = fs::read(&cache).expect("cache");
    let n = bytes.len();
    bytes.truncate(n - 7);
    fs::write(&cache, &bytes).expect("write");
    let (indexer, rx) = start(&root, options(Some(cache.clone())));
    let (s, stats) = ready(&rx);
    assert!(matches!(stats.cache, CacheUse::NotUsed(_)));
    assert_eq!(s.debug_dump(), reference.debug_dump());
    indexer.stop();

    // Another profile's cache is not used.
    let mut gfm = options(Some(cache.clone()));
    gfm.profile = ProfileId::GfmFile;
    gfm.params = gv_vault::ProfileParams::for_profile(ProfileId::GfmFile);
    let (indexer, rx) = start(&root, gfm);
    assert!(matches!(ready(&rx).1.cache, CacheUse::NotUsed(_)));
    indexer.stop();
}

#[test]
fn watcher_updates_reach_the_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    sample(&root);
    let shared = Arc::new(SharedPathIndex::new(build(&root)));
    let indexer = ContentIndexer::start(shared.load(), options(None), |_| {}).expect("starts");
    let handle = indexer.handle();
    let (watcher_ready_tx, watcher_ready) = mpsc::channel();
    let watcher_ready_tx = Mutex::new(watcher_ready_tx);
    let watcher = VaultWatcher::start(
        shared,
        WatchOptions {
            debounce: Duration::from_millis(50),
            ..WatchOptions::default()
        },
        move |event| match event {
            WatchEvent::IndexUpdated { index, .. } | WatchEvent::IndexBuilt { index, .. } => {
                handle.update_paths(index);
            }
            WatchEvent::Ready { .. } => {
                let _ = watcher_ready_tx.lock().expect("lock").send(());
            }
            _ => {}
        },
    )
    .expect("watcher");
    watcher_ready.recv_timeout(TIMEOUT).expect("watcher ready");
    eventually(&indexer, "first build", |s| s.note_count() == 3);

    write(&root, "D.md", "new note linking [[A]]\n");
    eventually(&indexer, "new backlink", |s| {
        s.backlink_sources(&vp("A.md")).contains(&vp("D.md"))
    });
    write(&root, "A.md", "# Alpha\n\nnow links [[D]] only\n");
    eventually(&indexer, "edited links", |s| {
        s.backlink_sources(&vp("D.md")) == vec![vp("A.md")]
            && s.backlink_sources(&vp("B.md")).is_empty()
    });
    fs::rename(root.join("D.md"), root.join("E.md")).expect("rename");
    eventually(&indexer, "rename", |s| {
        s.note(&vp("E.md")).is_some()
            && s.note(&vp("D.md")).is_none()
            && s.unresolved().iter().any(|u| &*u.link.raw == "D")
    });
    fs::remove_file(root.join("B.md")).expect("delete");
    eventually(&indexer, "delete", |s| s.note(&vp("B.md")).is_none());
    // An overflow rescan changes nothing.
    watcher.inject_overflow();
    std::thread::sleep(Duration::from_millis(200));
    assert!(indexer.flush(TIMEOUT));
    let s = indexer.snapshot().expect("snapshot");
    assert_eq!(s.note_count(), 3);
    drop(watcher);
    indexer.stop();
}

#[test]
fn overlays_through_the_service() {
    let dir = tempfile::tempdir().expect("tempdir");
    sample(dir.path());
    let (indexer, rx) = start(dir.path(), options(None));
    ready(&rx);
    indexer.set_overlay(vp("sub/C.md"), TextSnapshot::new("typing [[B]] #wip\n"));
    // Several keystrokes in a row are indexed once.
    indexer.set_overlay(vp("sub/C.md"), TextSnapshot::new("typing [[B]] #wip2\n"));
    assert!(indexer.flush(TIMEOUT));
    let s = indexer.snapshot().expect("snapshot");
    assert_eq!(
        s.backlink_sources(&vp("B.md")),
        vec![vp("A.md"), vp("sub/C.md")]
    );
    assert!(s.backlink_sources(&vp("A.md")) == vec![vp("B.md")]);
    assert_eq!(
        s.note(&vp("sub/C.md")).map(|r| r.kind),
        Some(RecordKind::Overlay)
    );
    assert!(s.tags().iter().any(|t| &*t.name == "wip2"));
    indexer.clear_overlay(vp("sub/C.md"));
    assert!(indexer.flush(TIMEOUT));
    let s = indexer.snapshot().expect("snapshot");
    assert_eq!(
        s.backlink_sources(&vp("A.md")),
        vec![vp("B.md"), vp("sub/C.md")]
    );
    indexer.stop();
}

/// Reads slowly and counts.
struct Slow {
    root: PathBuf,
    reads: Arc<AtomicUsize>,
}

impl FileSource for Slow {
    fn is_placeholder(&self, _: &VaultPath) -> std::io::Result<bool> {
        Ok(false)
    }
    fn read(&self, path: &VaultPath) -> std::io::Result<Vec<u8>> {
        std::thread::sleep(Duration::from_millis(5));
        self.reads.fetch_add(1, Ordering::Relaxed);
        fs::read(path.to_fs_path(&self.root))
    }
}

#[test]
fn dropping_the_indexer_cancels_the_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..400 {
        write(dir.path(), &format!("n{i}.md"), "[[n0]]\n");
    }
    let reads = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(Slow {
        root: dir.path().to_path_buf(),
        reads: reads.clone(),
    });
    let mut o = options(None);
    o.threads = 1;
    let indexer =
        ContentIndexer::start_with_source(build(dir.path()), o, source, |_| {}).expect("starts");
    let handle = indexer.handle();
    std::thread::sleep(Duration::from_millis(100));
    drop(indexer);
    assert!(handle.is_stopped());
    std::thread::sleep(Duration::from_millis(100));
    let after = reads.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(reads.load(Ordering::Relaxed), after, "reading went on");
    assert!(after < 400, "the build was not cancelled ({after} reads)");
    assert!(handle.snapshot().is_none());
}

#[test]
fn placeholders_by_hook_are_never_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    sample(dir.path());
    write(dir.path(), "online only.md", "[[A]]\n");
    let mut o = options(None);
    o.placeholder = Some(Arc::new(|path: &Path, _: &fs::Metadata| {
        path.to_string_lossy().contains("online only")
    }));
    let (indexer, rx) = start(dir.path(), o);
    let (s, stats) = ready(&rx);
    assert_eq!(stats.read, 4);
    assert_eq!(stats.parsed, 3);
    assert_eq!(s.placeholders(), vec![vp("online only.md")]);
    assert!(
        !s.backlink_sources(&vp("A.md"))
            .contains(&vp("online only.md"))
    );
    indexer.stop();
}

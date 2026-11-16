// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Content index scale measurement on a generated 10k-note, ~100 MB vault
//! (NFR-PERF-10: ≤ 3 s cold, ≤ 300 ms warm; index memory ≤ 50 MB; a
//! per-file update ≤ 10 ms).
//!
//! Ignored by default (it writes about 100 MB). Run with
//! `cargo test -p gv-vault --profile measure --test content_scale -- --ignored --nocapture`.
//! `GV_CONTENT_NOTES` and `GV_CONTENT_THREADS` change the note count and
//! the worker threads. The vault is created under Cargo's target tmp dir
//! and removed afterwards.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use gv_text::TextSnapshot;
use gv_vault::content::{
    CacheUse, ContentEvent, ContentIndexer, ContentOptions, DiskSource, store,
};
use gv_vault::{Change, EntryKind, FileMeta, IndexConfig, PathIndex, ProfileId, VaultPath};

const FOLDERS: usize = 100;
const TIMEOUT: Duration = Duration::from_secs(120);

struct TempTree(PathBuf);

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A small deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next()).unwrap_or(0) % n
    }
}

const WORDS: &[&str] = &[
    "the", "index", "vault", "note", "link", "graph", "alpha", "beta", "gamma", "delta", "quick",
    "brown", "fox", "jumps", "over", "lazy", "dog", "markdown", "editor", "render", "parse",
    "token", "block", "heading", "list", "table", "search", "query", "result", "panel",
];

fn name(i: usize) -> String {
    format!("Note {i:05}")
}

fn path_of(i: usize) -> String {
    format!("area-{:02}/{}.md", i % FOLDERS, name(i))
}

fn note(i: usize, n: usize, rate: usize, rng: &mut Lcg) -> String {
    let mut s = String::with_capacity(11_000);
    if i.is_multiple_of(3) {
        s.push_str(&format!(
            "---\ntags: [area{}, topic/{}]\naliases: [Alias {i}]\nstatus: draft\n---\n",
            i % 10,
            i % 7
        ));
    }
    s.push_str(&format!("# Title of note {i}\n\n"));
    let mut section = 0;
    while s.len() < 10_000 {
        match rng.below(12) {
            0 => {
                section += 1;
                s.push_str(&format!("\n## Section {section}\n\n"));
            }
            1 => s.push_str(&format!(
                "- [ ] task {} [[{}]]\n",
                rng.below(100),
                name(rng.below(n))
            )),
            2 => s.push_str("```rust\nlet x = \"[[not a link]]\"; // #not-a-tag\n```\n\n"),
            _ => {
                for _ in 0..40 {
                    // About 40 links and 10 tags per note; the dense mode
                    // has about ten times as many.
                    match rng.below(rate) {
                        0 => s.push_str(&format!("[[{}]] ", name(rng.below(n)))),
                        1 => s.push_str(&format!("[[{}#Section 1|see]] ", name(rng.below(n)))),
                        2 => s.push_str(&format!(
                            "[md](../area-{:02}/{}.md) ",
                            rng.below(FOLDERS),
                            name(rng.below(n)).replace(' ', "%20")
                        )),
                        3 => s.push_str(&format!("#tag{} ", rng.below(50))),
                        4 if rng.below(4) == 0 => s.push_str("[[Note 00000]] "),
                        5 if rng.below(4) == 0 => {
                            s.push_str(&format!("[[Missing {}]] ", rng.below(200)))
                        }
                        _ => {
                            s.push_str(WORDS[rng.below(WORDS.len())]);
                            s.push(' ');
                        }
                    }
                }
                s.push_str("\n\n");
            }
        }
    }
    s
}

fn generate(root: &Path, n: usize, rate: usize) -> u64 {
    let mut rng = Lcg(42);
    let mut bytes = 0;
    for f in 0..FOLDERS {
        fs::create_dir_all(root.join(format!("area-{f:02}"))).expect("mkdir");
    }
    for i in 0..n {
        let text = note(i, n, rate, &mut rng);
        bytes += text.len() as u64;
        fs::write(root.join(path_of(i)), text).expect("write");
    }
    bytes
}

fn rss_kib() -> Option<i64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn percentile(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i]
}

fn start(paths: PathIndex, o: ContentOptions) -> (ContentIndexer, Receiver<ContentEvent>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let indexer = ContentIndexer::start(paths, o, move |e| {
        let _ = tx.lock().expect("lock").send(e);
    })
    .expect("start");
    (indexer, rx)
}

fn wait<T>(rx: &Receiver<ContentEvent>, f: impl Fn(ContentEvent) -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let e = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("event in time");
        if let Some(t) = f(e) {
            return t;
        }
    }
}

#[test]
#[ignore = "writes ~100 MB; run explicitly with --ignored"]
fn content_index_scale() {
    let n: usize = std::env::var("GV_CONTENT_NOTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("content-scale-{}", std::process::id()));
    let _tree = TempTree(base.clone());
    let root = base.join("vault");
    let cache = store::cache_file(&base.join("cache"), &root);
    let t = Instant::now();
    // GV_CONTENT_DENSE=1: about 190 links per note (a stress case).
    let rate = if std::env::var_os("GV_CONTENT_DENSE").is_some() {
        30
    } else {
        250
    };
    let bytes = generate(&root, n, rate);
    println!(
        "generated {n} notes, {:.1} MB in {:.0} ms",
        bytes as f64 / 1e6,
        ms(t.elapsed())
    );
    // Let the newest files leave the racy window before the cache is
    // written, as on a vault that was not just generated.
    std::thread::sleep(Duration::from_millis(2_200));

    let mut o = ContentOptions::for_profile(ProfileId::Obsidian);
    if let Some(t) = std::env::var("GV_CONTENT_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        o.threads = t;
    }
    o.cache_file = Some(cache.clone());
    o.overlay_debounce = Duration::ZERO;
    println!("threads: {}", o.threads);

    let t = Instant::now();
    let (paths, _) = PathIndex::build(&root, IndexConfig::obsidian(Vec::new())).expect("walk");
    println!("path index: {:.0} ms", ms(t.elapsed()));

    // Cold: no cache.
    let rss0 = rss_kib();
    let t = Instant::now();
    let (indexer, rx) = start(paths.clone(), o.clone());
    let (snapshot, stats) = wait(&rx, |e| match e {
        ContentEvent::Ready { snapshot, stats } => Some((snapshot, stats)),
        _ => None,
    });
    let cold_wall = t.elapsed();
    assert!(matches!(stats.cache, CacheUse::NotUsed(_)));
    assert_eq!(stats.notes, n);
    let (persist_bytes, persist_time) = wait(&rx, |e| match e {
        ContentEvent::Persisted { bytes, elapsed } => Some((bytes, elapsed)),
        _ => None,
    });
    let rss1 = rss_kib();
    let links: usize = snapshot.notes().iter().map(|r| r.data.links.len()).sum();
    let tags: usize = snapshot.notes().iter().map(|r| r.data.tags.len()).sum();
    println!(
        "cold: {:.0} ms in the indexer ({:.0} ms wall; read+parse {:.0} ms, resolve {:.0} ms), {} notes, {links} links, {tags} tags, read {} parsed {}",
        ms(stats.elapsed),
        ms(cold_wall),
        ms(stats.read_time),
        ms(stats.resolve),
        stats.notes,
        stats.read,
        stats.parsed
    );
    println!(
        "estimated index heap: {:.1} MiB",
        snapshot.estimated_bytes() as f64 / 1_048_576.0
    );
    println!(
        "cache: {:.1} MB written in {:.0} ms",
        persist_bytes as f64 / 1e6,
        ms(persist_time)
    );
    if let (Some(a), Some(b)) = (rss0, rss1) {
        println!("RSS after cold build: +{:.1} MiB", (b - a) as f64 / 1024.0);
    }

    // Queries.
    let hub = VaultPath::new(&path_of(0)).expect("path");
    let t = Instant::now();
    let back = snapshot.backlinks(&hub);
    println!(
        "backlinks of the hub: {} links in {:.2} ms",
        back.len(),
        ms(t.elapsed())
    );
    let some = VaultPath::new(&path_of(n / 2)).expect("path");
    let t = Instant::now();
    let back = snapshot.backlinks(&some);
    println!(
        "backlinks of a note: {} links in {:.3} ms",
        back.len(),
        ms(t.elapsed())
    );
    let t = Instant::now();
    let tags = snapshot.tags();
    println!("tags: {} tags in {:.1} ms", tags.len(), ms(t.elapsed()));
    let t = Instant::now();
    let unresolved = snapshot.unresolved();
    println!(
        "unresolved: {} links in {:.1} ms",
        unresolved.len(),
        ms(t.elapsed())
    );
    let t = Instant::now();
    let entries = snapshot.switcher_entries();
    println!(
        "switcher entries: {} in {:.1} ms",
        entries.len(),
        ms(t.elapsed())
    );
    let t = Instant::now();
    let mentions = snapshot
        .unlinked_mentions(&some, &DiskSource::new(&root, &o), &AtomicBool::new(false))
        .expect("not cancelled");
    println!(
        "unlinked mentions: {} in {:.0} ms",
        mentions.len(),
        ms(t.elapsed())
    );

    // Per-file updates: edit one note, hand the index a new path snapshot.
    let mut current = paths.clone();
    let mut updates = Vec::new();
    let mut walls = Vec::new();
    for k in 0..30 {
        let i = (k * 331) % n;
        let rel = path_of(i);
        let full = root.join(&rel);
        let mut text = fs::read_to_string(&full).expect("read");
        text.push_str(&format!("\nedit {k} [[{}]]\n", name((i + 1) % n)));
        fs::write(&full, &text).expect("write");
        let md = fs::metadata(&full).expect("meta");
        let change = Change::Upsert {
            path: VaultPath::new(&rel).expect("path"),
            kind: EntryKind::File,
            meta: FileMeta {
                size: md.len(),
                mtime: md.modified().ok(),
                identity: None,
            },
        };
        current = current.apply(&[change]).expect("apply");
        let t = Instant::now();
        indexer.update_paths(current.clone());
        let elapsed = wait(&rx, |e| match e {
            ContentEvent::Updated { elapsed, .. } => Some(elapsed),
            _ => None,
        });
        walls.push(t.elapsed());
        updates.push(elapsed);
    }
    println!(
        "file update: median {:.2} ms, p95 {:.2} ms (indexer); wall p95 {:.2} ms",
        ms(percentile(&mut updates, 0.5)),
        ms(percentile(&mut updates, 0.95)),
        ms(percentile(&mut walls, 0.95))
    );

    // Overlay updates (typing in a note).
    let mut overlays = Vec::new();
    let base_text = fs::read_to_string(root.join(path_of(7))).expect("read");
    for k in 0..30 {
        let text = format!("{base_text}\ntyping {k} [[{}]]\n", name(k));
        indexer.set_overlay(
            VaultPath::new(&path_of(7)).expect("path"),
            TextSnapshot::new(&text),
        );
        overlays.push(wait(&rx, |e| match e {
            ContentEvent::Updated { elapsed, .. } => Some(elapsed),
            _ => None,
        }));
    }
    println!(
        "overlay update: median {:.2} ms, p95 {:.2} ms",
        ms(percentile(&mut overlays, 0.5)),
        ms(percentile(&mut overlays, 0.95))
    );
    // A new file (re-resolves the links with its key).
    let rel = "area-00/Missing 7.md";
    fs::write(root.join(rel), "now exists\n").expect("write");
    current = current
        .apply(&[Change::Upsert {
            path: VaultPath::new(rel).expect("path"),
            kind: EntryKind::File,
            meta: FileMeta {
                size: 11,
                mtime: Some(SystemTime::now()),
                identity: None,
            },
        }])
        .expect("apply");
    indexer.update_paths(current.clone());
    let (elapsed, resolved) = wait(&rx, |e| match e {
        ContentEvent::Updated {
            elapsed, report, ..
        } => Some((elapsed, report.resolved)),
        _ => None,
    });
    println!(
        "new file resolving a missing name: {:.2} ms ({resolved} notes re-resolved)",
        ms(elapsed)
    );
    indexer.stop();
    drop(snapshot);

    // Warm: a valid cache. The edits above were written to it on stop.
    std::thread::sleep(Duration::from_millis(2_200));
    let (paths, _) = PathIndex::build(&root, IndexConfig::obsidian(Vec::new())).expect("walk");
    let t = Instant::now();
    let (indexer, rx) = start(paths, o.clone());
    let stats = wait(&rx, |e| match e {
        ContentEvent::Ready { stats, .. } => Some(stats),
        _ => None,
    });
    println!(
        "warm: {:.0} ms in the indexer ({:.0} ms wall; load {:.0} ms, diff+read {:.0} ms, resolve {:.0} ms), cache {:?}, read {} parsed {}",
        ms(stats.elapsed),
        ms(t.elapsed()),
        ms(stats.load),
        ms(stats.read_time),
        ms(stats.resolve),
        stats.cache,
        stats.read,
        stats.parsed
    );
    assert!(matches!(stats.cache, CacheUse::Loaded(_)));
    indexer.stop();
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Path-index scale measurement on a synthetic 50k-file tree.
//!
//! Ignored by default (it writes 50,000 files). Run with
//! `cargo test -p gv-vault --release --test scale -- --ignored --nocapture`.
//! The tree is created under Cargo's target tmp dir and removed afterwards.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gv_vault::{
    Change, EntryKind, FileMeta, IndexConfig, LinkKind, LinkRef, PathIndex, ProfileId,
    ProfileParams, SharedPathIndex, Status, VaultPath, resolve,
};

const TOP: usize = 50;
const SUB: usize = 10;
const FILES: usize = 100;

struct TempTree(PathBuf);

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn generate(root: &Path) -> usize {
    let mut n = 0;
    for t in 0..TOP {
        for s in 0..SUB {
            let dir = root.join(format!("area-{t:02}")).join(format!("Topic {s}"));
            fs::create_dir_all(&dir).expect("test setup");
            for f in 0..FILES {
                // Every 10th name repeats across folders (basename collisions).
                let name = if f % 10 == 0 {
                    format!("Index {f}.md")
                } else {
                    format!("note-{t}-{s}-{f}.md")
                };
                fs::write(dir.join(name), b"").expect("test setup");
                n += 1;
            }
        }
    }
    n
}

/// Resident set size in KiB (Linux only).
fn rss_kib() -> Option<i64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn per_op(total: Duration, n: usize) -> f64 {
    total.as_secs_f64() * 1e6 / n as f64
}

#[test]
#[ignore = "writes 50,000 files; run explicitly"]
fn index_50k_files() {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("gv-vault-scale-{}", std::process::id()));
    let _guard = TempTree(base.clone());
    let started = Instant::now();
    let files = generate(&base);
    eprintln!("generated {files} files in {:.2?}", started.elapsed());

    let cfg = IndexConfig {
        entry_limit: None,
        ..IndexConfig::default()
    };
    let rss_before = rss_kib();
    let mut builds = Vec::new();
    let mut last = None;
    let mut rss_growth = None;
    for run in 0..5 {
        let t = Instant::now();
        let (idx, stats) = PathIndex::build(&base, cfg.clone()).unwrap();
        builds.push(t.elapsed());
        eprintln!(
            "build: total {:.1?} (walk {:.1?}, maps {:.1?}), {} entries",
            t.elapsed(),
            stats.walk_time,
            stats.index_time,
            stats.entries
        );
        if run == 0 {
            rss_growth = rss_before.zip(rss_kib()).map(|(a, b)| b - a);
        }
        last = Some(idx);
    }
    let idx = last.unwrap();
    assert_eq!(idx.file_count(), files);
    if let Some(kib) = rss_growth {
        eprintln!(
            "RSS growth after the first build: {:.1} MiB",
            kib as f64 / 1024.0
        );
    }
    eprintln!("build median: {:.1?} over {} runs", median(builds), 5);
    idx.check_consistency().unwrap();

    // Incremental updates (each produces a new snapshot).
    let n = 1000;
    let t = Instant::now();
    let mut cur = idx.clone();
    for i in 0..n {
        cur = cur
            .apply(&[Change::Upsert {
                path: VaultPath::new(&format!("area-07/Topic 3/new-{i}.md")).unwrap(),
                kind: EntryKind::File,
                meta: FileMeta::default(),
            }])
            .unwrap();
    }
    let add = t.elapsed();
    let t = Instant::now();
    for i in 0..n {
        cur = cur
            .apply(&[Change::Rename {
                from: VaultPath::new(&format!("area-07/Topic 3/new-{i}.md")).unwrap(),
                to: VaultPath::new(&format!("area-08/Topic 4/renamed-{i}.md")).unwrap(),
            }])
            .unwrap();
    }
    let rename = t.elapsed();
    let t = Instant::now();
    for i in 0..n {
        cur = cur
            .apply(&[Change::Remove {
                path: VaultPath::new(&format!("area-08/Topic 4/renamed-{i}.md")).unwrap(),
            }])
            .unwrap();
    }
    let remove = t.elapsed();
    let t = Instant::now();
    let dirs = 20;
    for i in 0..dirs {
        let (from, to) = if i % 2 == 0 {
            ("area-09/Topic 1", "area-09/Moved")
        } else {
            ("area-09/Moved", "area-09/Topic 1")
        };
        cur = cur
            .apply(&[Change::Rename {
                from: VaultPath::new(from).unwrap(),
                to: VaultPath::new(to).unwrap(),
            }])
            .unwrap();
    }
    let dir_rename = t.elapsed();
    cur.check_consistency().unwrap();
    eprintln!(
        "update per op: add {:.1} us, rename {:.1} us, remove {:.1} us, folder rename (100 files) {:.1} us",
        per_op(add, n),
        per_op(rename, n),
        per_op(remove, n),
        per_op(dir_rename, dirs)
    );

    // Publishing and loading snapshots.
    let shared = SharedPathIndex::new(idx.clone());
    let t = Instant::now();
    for _ in 0..10_000 {
        std::hint::black_box(shared.load());
    }
    eprintln!("snapshot load: {:.3} us", per_op(t.elapsed(), 10_000));

    // Resolution throughput.
    let ob = ProfileParams::for_profile(ProfileId::Obsidian);
    // A source outside the area folders, so `Index 50` needs the basename lookup.
    let source = VaultPath::new("inbox/today.md").unwrap();
    let cases = [
        ("note-31-7-42", Status::Exact),
        ("Index 50", Status::Ambiguous(TOP * SUB)),
        ("area-12/Topic 2/note-12-2-3", Status::Exact),
        ("NOTE-5-5-5", Status::Exact),
        ("missing note", Status::Unresolved),
    ];
    for (text, want) in cases {
        let link = LinkRef::new(
            LinkKind::Wikilink,
            text,
            source.clone(),
            ProfileId::Obsidian,
        );
        let reps = if matches!(want, Status::Ambiguous(_)) {
            200
        } else {
            5000
        };
        let t = Instant::now();
        let mut status = Status::Pending;
        for _ in 0..reps {
            status = std::hint::black_box(resolve(&idx, &link, &ob)).status;
        }
        assert_eq!(status, want, "{text}");
        eprintln!(
            "resolve [[{text}]] -> {status}: {:.1} us",
            per_op(t.elapsed(), reps)
        );
    }
}

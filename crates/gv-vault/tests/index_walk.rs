// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The directory walk, the building state, shared snapshots and the
//! file-system probe, on real temporary directories.

use std::fs;
use std::path::Path;

use gv_vault::{
    Change, EntryKind, FsProbe, IndexConfig, IndexError, IndexState, LinkKind, LinkRef, PathIndex,
    PathProbe, PlatformRules, ProbeResult, ProfileId, ProfileParams, SharedPathIndex, Status,
    VaultPath, resolve, resolve_with_probe,
};

fn vp(s: &str) -> VaultPath {
    VaultPath::new(s).expect("test setup")
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().expect("test setup")).expect("test setup");
    fs::write(p, text).expect("test setup");
}

fn sample_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("test setup");
    let r = dir.path();
    write(r, "notes/a.md", "# A\n");
    write(r, "notes/B.md", "# B\n");
    write(r, "Archive/old.md", "");
    write(r, ".hidden/h.md", "");
    write(r, ".github/CONTRIBUTING.md", "");
    write(r, "node_modules/m/readme.md", "");
    write(r, "target/t.md", "");
    write(r, ".obsidian/app.json", "{}");
    write(r, ".git/HEAD", "ref: refs/heads/main\n");
    write(r, "ignored/i.md", "");
    write(r, ".gitignore", "ignored/\n");
    dir
}

fn paths(idx: &PathIndex) -> Vec<String> {
    let mut v: Vec<String> = idx.iter().map(|e| e.path.as_str().to_owned()).collect();
    v.sort();
    v
}

#[test]
fn default_walk_excludes_and_hides() {
    let dir = sample_tree();
    let (idx, stats) = PathIndex::build(dir.path(), IndexConfig::default()).unwrap();
    assert_eq!(idx.state(), IndexState::Complete);
    assert_eq!(
        paths(&idx),
        [
            "",
            "Archive",
            "Archive/old.md",
            "ignored",
            "ignored/i.md",
            "notes",
            "notes/B.md",
            "notes/a.md"
        ]
    );
    assert_eq!(stats.entries, 7);
    assert_eq!(stats.errors, 0);
    idx.check_consistency().unwrap();
    let a = idx.entry_by_path("notes/a.md").unwrap();
    assert_eq!(a.meta.size, 4);
    assert!(a.meta.mtime.is_some());
    // The creation time where the file system keeps one, else the mtime.
    let md = fs::metadata(dir.path().join("notes/a.md")).unwrap();
    let created = md.created().ok().or(a.meta.mtime);
    assert_eq!(a.meta.created, created);
    #[cfg(unix)]
    assert!(a.meta.identity.is_some());
}

#[test]
fn git_repo_walk_honours_gitignore_and_shows_hidden_files() {
    let dir = sample_tree();
    let (idx, _) = PathIndex::build(dir.path(), IndexConfig::git_repo()).unwrap();
    let p = paths(&idx);
    assert!(p.contains(&".github/CONTRIBUTING.md".to_owned()), "{p:?}");
    assert!(p.contains(&".gitignore".to_owned()));
    assert!(!p.iter().any(|x| x.starts_with("ignored")), "{p:?}");
    assert!(!p.iter().any(|x| x.starts_with(".git/") || x == ".git"));
    assert!(!p.iter().any(|x| x.starts_with("node_modules")
        || x.starts_with("target")
        || x.starts_with(".obsidian")));
}

#[test]
fn user_ignore_filters_are_flagged_not_excluded() {
    let dir = sample_tree();
    let (idx, _) =
        PathIndex::build(dir.path(), IndexConfig::obsidian(vec!["Archive/".into()])).unwrap();
    assert!(idx.entry_by_path("Archive/old.md").unwrap().user_ignored);
    assert!(!idx.entry_by_path("notes/a.md").unwrap().user_ignored);
}

#[test]
fn entry_limit_stops_the_walk() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..20 {
        write(dir.path(), &format!("n{i}.md"), "");
    }
    let cfg = IndexConfig {
        entry_limit: Some(5),
        ..IndexConfig::default()
    };
    assert!(matches!(
        PathIndex::build(dir.path(), cfg),
        Err(IndexError::TooManyEntries { limit: 5 })
    ));
}

#[test]
fn incremental_updates_on_a_built_index() {
    let dir = sample_tree();
    let (idx, _) = PathIndex::build(dir.path(), IndexConfig::default()).unwrap();
    let a = idx.id_of("notes/a.md").unwrap();
    let next = idx
        .apply(&[
            Change::Rename {
                from: vp("notes"),
                to: vp("Notes 2"),
            },
            Change::Remove {
                path: vp("ignored"),
            },
            Change::Upsert {
                path: vp("new/deep/x.md"),
                kind: EntryKind::File,
                meta: Default::default(),
            },
        ])
        .unwrap();
    next.check_consistency().unwrap();
    assert_eq!(next.id_of("Notes 2/a.md"), Some(a));
    assert!(next.generation() > idx.generation());
    let link = LinkRef::new(LinkKind::Wikilink, "a", vp("x.md"), ProfileId::Obsidian);
    let r = resolve(
        &next,
        &link,
        &ProfileParams::for_profile(ProfileId::Obsidian),
    );
    assert_eq!((r.status, r.target), (Status::Exact, Some(a)));
}

#[test]
fn shared_index_goes_from_pending_to_resolved() {
    let dir = sample_tree();
    let shared = SharedPathIndex::building(dir.path(), IndexConfig::default());
    let link = LinkRef::new(LinkKind::Wikilink, "a", vp("x.md"), ProfileId::Obsidian);
    let params = ProfileParams::for_profile(ProfileId::Obsidian);
    let before = shared.load();
    assert_eq!(resolve(&before, &link, &params).status, Status::Pending);
    let stats = shared.build(dir.path(), IndexConfig::default()).unwrap();
    assert!(stats.entries > 0);
    assert_eq!(
        resolve(&shared.load(), &link, &params).status,
        Status::Exact
    );
    // Readers keep their snapshot.
    assert_eq!(resolve(&before, &link, &params).status, Status::Pending);
    // Writers publish.
    shared
        .apply(&[Change::Remove {
            path: vp("notes/a.md"),
        }])
        .unwrap();
    assert_eq!(
        resolve(&shared.load(), &link, &params).status,
        Status::Unresolved
    );
}

#[test]
fn probe_decides_exact_names_while_building() {
    let dir = sample_tree();
    let building = PathIndex::new_building(dir.path(), IndexConfig::default());
    let probe = FsProbe::new(dir.path(), PlatformRules::host());
    let params = ProfileParams::for_profile(ProfileId::GfmFile);
    let r = |raw: &str| {
        resolve_with_probe(
            &building,
            &LinkRef::new(LinkKind::Inline, raw, vp("notes/x.md"), ProfileId::GfmFile),
            &params,
            &probe,
        )
    };
    let hit = r("a.md");
    assert_eq!(
        (hit.status, hit.target, hit.target_path),
        (Status::Exact, None, Some(vp("notes/a.md")))
    );
    // `.md` appended, exact name: decidable.
    assert_eq!(r("a").status, Status::Loose);
    // A case variant needs the fold map: Pending, not Unresolved.
    assert_eq!(r("b.md").status, Status::Pending);
    // Obsidian bare paths: the vault-root miss is decided by the probe, the
    // case/NFC variants are not.
    let ob = ProfileParams::for_profile(ProfileId::Obsidian);
    let link = LinkRef::new(
        LinkKind::Wikilink,
        "notes/a",
        vp("x.md"),
        ProfileId::Obsidian,
    );
    assert_eq!(
        resolve_with_probe(&building, &link, &ob, &probe).status,
        Status::Exact
    );
    assert_eq!(probe.probe(&vp("notes")), ProbeResult::Dir);
    assert_eq!(probe.probe(&vp("notes/zzz.md")), ProbeResult::Missing);
    // Names a platform treats specially are never touched.
    let win = FsProbe::new(dir.path(), PlatformRules::WINDOWS);
    assert_eq!(win.probe(&vp("notes/a.md:stream")), ProbeResult::Missing);
    assert_eq!(win.probe(&vp("CON")), ProbeResult::Missing);
}

#[cfg(unix)]
#[test]
fn symlinks_inside_and_outside_the_root() {
    use std::os::unix::fs::symlink;
    let dir = sample_tree();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.md", "");
    let r = dir.path();
    symlink(r.join("notes"), r.join("linked-notes")).unwrap();
    symlink(r.join("notes/a.md"), r.join("alias.md")).unwrap();
    symlink(outside.path().join("secret.md"), r.join("escape.md")).unwrap();
    symlink(r.join("nowhere.md"), r.join("dangling.md")).unwrap();
    symlink(r.join("loop-b"), r.join("loop-a")).unwrap();
    symlink(r.join("loop-a"), r.join("loop-b")).unwrap();
    let (idx, _) = PathIndex::build(r, IndexConfig::default()).unwrap();
    idx.check_consistency().unwrap();
    let kind = |p: &str| idx.entry_by_path(p).unwrap().kind.clone();
    assert_eq!(
        kind("linked-notes"),
        EntryKind::Symlink(gv_vault::SymlinkTarget::Inside(vp("notes")))
    );
    assert_eq!(
        kind("alias.md"),
        EntryKind::Symlink(gv_vault::SymlinkTarget::Inside(vp("notes/a.md")))
    );
    assert!(matches!(
        kind("escape.md"),
        EntryKind::Symlink(gv_vault::SymlinkTarget::Outside(_))
    ));
    assert_eq!(
        kind("dangling.md"),
        EntryKind::Symlink(gv_vault::SymlinkTarget::Broken)
    );
    assert_eq!(
        kind("loop-a"),
        EntryKind::Symlink(gv_vault::SymlinkTarget::Broken)
    );

    let params = ProfileParams::for_profile(ProfileId::GfmFile);
    let res = |raw: &str| {
        resolve(
            &idx,
            &LinkRef::new(LinkKind::Inline, raw, vp("x.md"), ProfileId::GfmFile),
            &params,
        )
    };
    let a = idx.id_of("notes/a.md");
    assert_eq!(res("alias.md").target, a);
    assert_eq!(res("linked-notes/a.md").target, a);
    assert_eq!(res("escape.md").status, Status::OutsideRoot);
    assert_eq!(res("dangling.md").status, Status::Unresolved);
    // An embed through an outside symlink is blocked by default.
    let embed = resolve(
        &idx,
        &LinkRef::new(LinkKind::Image, "escape.md", vp("x.md"), ProfileId::GfmFile),
        &params,
    );
    assert_eq!(
        embed.block,
        Some(gv_vault::url::BlockReason::OutsideRootEmbed)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_are_skipped_and_counted() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(OsStr::from_bytes(b"bad\xff.md")), "").unwrap();
    write(dir.path(), "good.md", "");
    let (idx, stats) = PathIndex::build(dir.path(), IndexConfig::default()).unwrap();
    assert_eq!(stats.skipped_non_utf8, 1);
    assert_eq!(idx.file_count(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn case_and_normalization_twins_coexist_on_linux() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["Case.md", "case.md", "Caf\u{e9}.md", "Cafe\u{301}.md"] {
        write(dir.path(), name, "");
    }
    let (idx, _) = PathIndex::build(dir.path(), IndexConfig::default()).unwrap();
    assert_eq!(idx.file_count(), 4);
    assert_eq!(idx.by_fold("case.md").len(), 2);
    assert_eq!(idx.by_nfc("Caf\u{e9}.md").len(), 2);
    // Each twin pair would collide on macOS (APFS); only the case pair on Windows.
    let mac = PlatformRules::MACOS;
    assert!(mac.same_name("Case.md", "case.md") && mac.same_name("Caf\u{e9}.md", "Cafe\u{301}.md"));
    assert!(!PlatformRules::WINDOWS.same_name("Caf\u{e9}.md", "Cafe\u{301}.md"));
}

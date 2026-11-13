// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests of the path index and the resolver.
//!
//! - Resolution is deterministic, and independent of the order in which
//!   the index was filled (FileIds differ, results do not).
//! - A case-only rename never loses a target under case-insensitive rules.
//! - NFC and NFD spellings of a link resolve to the same target.
//! - Incremental updates give the same index as a rebuild.

use std::collections::BTreeSet;

use gv_vault::keys::{nfc_key, nfd};
use gv_vault::platform::PlatformRules;
use gv_vault::{
    Change, EntryKind, FileMeta, LinkKind, LinkRef, PathIndex, ProfileId, ProfileParams,
    Resolution, VaultPath, resolve,
};
use proptest::prelude::*;
use proptest::test_runner::Config;

const PROFILES: [ProfileId; 5] = [
    ProfileId::CommonMark,
    ProfileId::GfmFile,
    ProfileId::GfmComment,
    ProfileId::Obsidian,
    ProfileId::Extended,
];

fn name() -> impl Strategy<Value = String> {
    let chars = prop::sample::select(vec![
        "a", "b", "A", "B", "\u{e9}", "e\u{301}", "\u{c9}", "-", "1", "\u{df}",
    ]);
    prop::collection::vec(chars, 1..4).prop_map(|v| v.concat())
}

/// A file path: 1-3 segments, `.md` on the last.
fn file_path() -> impl Strategy<Value = String> {
    prop::collection::vec(name(), 1..4).prop_map(|segs| format!("{}.md", segs.join("/")))
}

fn vault() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(file_path(), 1..24).prop_map(|v| {
        let set: BTreeSet<String> = v.into_iter().collect();
        set.into_iter().collect()
    })
}

fn index_of(paths: &[String]) -> PathIndex {
    PathIndex::from_paths(paths).expect("valid paths")
}

/// The same files, inserted one by one in reverse order (different FileIds).
fn index_reversed(paths: &[String]) -> PathIndex {
    let mut idx = PathIndex::from_paths(Vec::<&str>::new()).expect("empty");
    for p in paths.iter().rev() {
        idx = idx
            .apply(&[Change::Upsert {
                path: VaultPath::new(p).expect("valid"),
                kind: EntryKind::File,
                meta: FileMeta::default(),
            }])
            .expect("upsert");
    }
    idx
}

/// Everything in a resolution except FileIds.
fn projection(r: &Resolution) -> String {
    let cands: Vec<&str> = r.candidates.iter().map(|c| c.path.as_str()).collect();
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
        r.status, r.target_path, r.target_kind, cands, r.notes, r.trace, r.fragment, r.outside_path
    )
}

#[derive(Debug, Clone)]
enum Variant {
    AsIs,
    Upper,
    Lower,
    Nfd,
    Nfc,
}

fn variant() -> impl Strategy<Value = Variant> {
    prop_oneof![
        Just(Variant::AsIs),
        Just(Variant::Upper),
        Just(Variant::Lower),
        Just(Variant::Nfd),
        Just(Variant::Nfc)
    ]
}

fn apply_variant(s: &str, v: &Variant) -> String {
    match v {
        Variant::AsIs => s.to_owned(),
        Variant::Upper => s.to_uppercase(),
        Variant::Lower => s.to_lowercase(),
        Variant::Nfd => nfd(s),
        Variant::Nfc => nfc_key(s).into_owned(),
    }
}

/// Link text derived from a vault path: the full path or its last
/// segment, with or without `.md`.
fn link_text(path: &str, basename: bool, keep_ext: bool) -> String {
    let p = if basename {
        path.rsplit('/').next().unwrap_or(path)
    } else {
        path
    };
    if keep_ext {
        p.to_owned()
    } else {
        p.trim_end_matches(".md").to_owned()
    }
}

/// 96 cases per property in `cargo test`; set `PROPTEST_CASES` for more.
fn config() -> Config {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(96);
    Config {
        cases,
        failure_persistence: None,
        max_global_rejects: cases.saturating_mul(4).max(1024),
        ..Config::default()
    }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn resolution_is_deterministic_and_order_independent(
        paths in vault(),
        pick in any::<prop::sample::Index>(),
        src in any::<prop::sample::Index>(),
        basename in any::<bool>(),
        keep_ext in any::<bool>(),
        v in variant(),
        wiki in any::<bool>(),
        profile in prop::sample::select(PROFILES.to_vec()),
        extra in name(),
        use_extra in any::<bool>(),
    ) {
        let a = index_of(&paths);
        let b = index_reversed(&paths);
        let target = pick.get(&paths);
        let text = if use_extra { extra } else { apply_variant(&link_text(target, basename, keep_ext), &v) };
        let kind = if wiki { LinkKind::Wikilink } else { LinkKind::Inline };
        let source = VaultPath::new(src.get(&paths)).unwrap();
        let link = LinkRef::new(kind, text, source, profile);
        let params = ProfileParams::for_profile(profile);
        let r1 = resolve(&a, &link, &params);
        let r2 = resolve(&a, &link, &params);
        prop_assert_eq!(&r1, &r2);
        let r3 = resolve(&b, &link, &params);
        prop_assert_eq!(projection(&r1), projection(&r3));
    }

    #[test]
    fn case_only_rename_never_loses_a_target(
        paths in vault(),
        pick in any::<prop::sample::Index>(),
        src in any::<prop::sample::Index>(),
        basename in any::<bool>(),
        upper in any::<bool>(),
        profile in prop::sample::select(vec![ProfileId::Obsidian, ProfileId::GfmFile, ProfileId::Extended]),
    ) {
        // Case-insensitive rules: no two paths may collide on macOS (APFS).
        let mac = PlatformRules::MACOS;
        let mut seen = BTreeSet::new();
        let paths: Vec<String> = paths.into_iter().filter(|p| seen.insert(mac.collision_key(p))).collect();
        let idx = index_of(&paths);
        let target = pick.get(&paths);
        let target_id = idx.id_of(target).unwrap();
        let source = src.get(&paths);
        // GitHub-like profiles only resolve paths relative to the source.
        let text = if profile == ProfileId::Obsidian || profile == ProfileId::Extended {
            link_text(target, basename, false)
        } else {
            relative_path(source, target)
        };
        let kind = if profile == ProfileId::GfmFile { LinkKind::Inline } else { LinkKind::Wikilink };
        let link = LinkRef::new(kind, text, VaultPath::new(source).unwrap(), profile);
        let params = ProfileParams::for_profile(profile);
        let before = resolve(&idx, &link, &params);
        prop_assume!(before.target == Some(target_id) && before.candidates.is_empty());

        let (dir, file) = match target.rsplit_once('/') {
            Some((d, f)) => (format!("{d}/"), f),
            None => (String::new(), target.as_str()),
        };
        let stem = file.trim_end_matches(".md");
        let renamed_stem = if upper { swap_case(stem) } else { stem.to_lowercase() };
        let renamed = format!("{dir}{renamed_stem}.md");
        prop_assume!(renamed != *target && mac.same_name(&renamed, target));
        let moved = idx
            .apply(&[Change::Rename { from: VaultPath::new(target).unwrap(), to: VaultPath::new(&renamed).unwrap() }])
            .unwrap();
        moved.check_consistency().map_err(TestCaseError::fail)?;
        let after = resolve(&moved, &link, &params);
        prop_assert!(after.status.is_resolved(), "{}", after.trace_text());
        prop_assert_eq!(after.target, Some(target_id), "{}", after.trace_text());
    }

    #[test]
    fn nfc_and_nfd_links_resolve_the_same(
        paths in vault(),
        pick in any::<prop::sample::Index>(),
        src in any::<prop::sample::Index>(),
        basename in any::<bool>(),
        profile in prop::sample::select(PROFILES.to_vec()),
        wiki in any::<bool>(),
    ) {
        // No NFC/NFD twins (they are different files and legitimately differ).
        let mut seen = BTreeSet::new();
        let paths: Vec<String> = paths.into_iter().filter(|p| seen.insert(nfc_key(p).into_owned())).collect();
        let idx = index_of(&paths);
        let target = pick.get(&paths);
        let text = link_text(target, basename, !wiki);
        let source = VaultPath::new(src.get(&paths)).unwrap();
        let kind = if wiki { LinkKind::Wikilink } else { LinkKind::Inline };
        let params = ProfileParams::for_profile(profile);
        let as_nfc = resolve(&idx, &LinkRef::new(kind, nfc_key(&text).into_owned(), source.clone(), profile), &params);
        let as_nfd = resolve(&idx, &LinkRef::new(kind, nfd(&text), source, profile), &params);
        prop_assert_eq!(as_nfc.target_path, as_nfd.target_path);
        prop_assert_eq!(as_nfc.status.is_resolved(), as_nfd.status.is_resolved());
        prop_assert_eq!(as_nfc.candidates.len(), as_nfd.candidates.len());
    }

    #[test]
    fn incremental_updates_equal_a_rebuild(
        paths in vault(),
        ops in prop::collection::vec((0u8..3, any::<prop::sample::Index>(), file_path()), 0..16),
        probe in file_path(),
    ) {
        let mut idx = index_of(&paths);
        let mut files: BTreeSet<String> = paths.iter().cloned().collect();
        for (op, which, new_path) in ops {
            let current: Vec<String> = files.iter().cloned().collect();
            let change = match op {
                0 => Change::Upsert { path: VaultPath::new(&new_path).unwrap(), kind: EntryKind::File, meta: FileMeta::default() },
                1 if !current.is_empty() => Change::Remove { path: VaultPath::new(which.get(&current)).unwrap() },
                _ if !current.is_empty() => Change::Rename {
                    from: VaultPath::new(which.get(&current)).unwrap(),
                    to: VaultPath::new(&new_path).unwrap(),
                },
                _ => continue,
            };
            // Skip changes that would put a file under a file (not possible here:
            // directory names never end in `.md`).
            idx = idx.apply(std::slice::from_ref(&change)).unwrap();
            match change {
                Change::Upsert { path, .. } => { files.insert(path.as_str().to_owned()); }
                Change::Remove { path } => { files.remove(path.as_str()); }
                Change::Rename { from, to } => { files.remove(from.as_str()); files.insert(to.as_str().to_owned()); }
            }
        }
        idx.check_consistency().map_err(TestCaseError::fail)?;
        let have: BTreeSet<String> = idx.iter().filter(|e| !e.kind.is_dir()).map(|e| e.path.as_str().to_owned()).collect();
        prop_assert_eq!(&have, &files);
        // A fresh index of the same entries (directories emptied by removals
        // stay, as on disk) resolves every link the same way.
        let rebuilt = PathIndex::from_entries(
            "/vault",
            gv_vault::IndexConfig::default(),
            gv_vault::IndexState::Complete,
            idx.iter().filter(|e| !e.path.is_root()).map(|e| (e.path.clone(), e.kind.clone(), FileMeta::default())),
        )
        .unwrap();
        let params = ProfileParams::for_profile(ProfileId::Obsidian);
        let text = link_text(&probe, true, false);
        let link = LinkRef::new(LinkKind::Wikilink, text, VaultPath::root().join("src.md").unwrap(), ProfileId::Obsidian);
        let a = resolve(&idx, &link, &params);
        let b = resolve(&rebuilt, &link, &params);
        prop_assert_eq!(a.status, b.status);
        prop_assert_eq!(&a.target_path, &b.target_path);
        prop_assert_eq!(projection(&a), projection(&b));
    }
}

fn swap_case(s: &str) -> String {
    s.chars()
        .flat_map(|c| -> Vec<char> {
            if c.is_lowercase() {
                c.to_uppercase().collect()
            } else {
                c.to_lowercase().collect()
            }
        })
        .collect()
}

/// A `./`-free relative path from `source`'s folder to `target`.
fn relative_path(source: &str, target: &str) -> String {
    let src_dir: Vec<&str> =
        source.split('/').collect::<Vec<_>>()[..source.split('/').count() - 1].to_vec();
    let tgt: Vec<&str> = target.split('/').collect();
    let common = src_dir.iter().zip(&tgt).take_while(|(a, b)| a == b).count();
    let mut out: Vec<String> = vec!["..".to_owned(); src_dir.len() - common];
    out.extend(tgt[common..].iter().map(|s| (*s).to_owned()));
    out.join("/")
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Gathering, ranking, highlighting and new-note names, without a window.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gv_vault::VaultPath;
use gv_vault::content::SwitcherEntry;

use super::*;

const ROOT: &str = "/vault";

fn file(path: &str, title: Option<&str>) -> SwitcherEntry {
    SwitcherEntry {
        path: VaultPath::new(path).expect("path"),
        title: title.map(Arc::from),
        alias: None,
        is_note: path.ends_with(".md"),
    }
}

fn alias(path: &str, title: &str, alias: &str) -> SwitcherEntry {
    SwitcherEntry {
        alias: Some(Arc::from(alias)),
        ..file(path, Some(title))
    }
}

fn vault(entries: Vec<SwitcherEntry>) -> VaultSource {
    VaultSource {
        root: PathBuf::from(ROOT),
        entries,
        ..VaultSource::default()
    }
}

fn gather(entries: Vec<SwitcherEntry>) -> Gather {
    Gather {
        vault: Some(vault(entries)),
        ..Gather::default()
    }
}

fn abs(rel: &str) -> PathBuf {
    Path::new(ROOT).join(rel)
}

/// The rows for `query` as the switcher describes them.
fn ranked(entries: &[Entry], query: &str) -> Vec<String> {
    rank(entries, query)
        .into_iter()
        .map(|i| {
            let e = &entries[i];
            match (&e.kind, &e.target) {
                (EntryKind::Alias, Some(t)) => format!("{} → {t}", e.label),
                (EntryKind::Unresolved { .. }, _) => format!("+{}", e.label),
                _ => e.label.to_string(),
            }
        })
        .collect()
}

fn sample() -> Gather {
    gather(vec![
        file("Profiles/Flavor profiles.md", Some("Flavor profiles")),
        alias("Profiles/Flavor profiles.md", "Flavor profiles", "Profiles"),
        file("Vault/Properties.md", Some("Properties")),
        file(
            "Editing/Proofreading.md",
            Some("Proofreading and spell check"),
        ),
        file("Editing/Paste and drop.md", Some("Paste and drop")),
        file("Projects/Readme.md", Some("Read me first")),
        file("assets/diagram.png", None),
    ])
}

#[test]
fn notes_show_their_titles_and_files_their_names() {
    let entries = collect(&sample());
    let labels: Vec<&str> = entries.iter().map(|e| &*e.label).collect();
    assert_eq!(
        labels,
        [
            "Flavor profiles",
            "Profiles",
            "Properties",
            "Proofreading and spell check",
            "Paste and drop",
            "Read me first",
            "diagram.png"
        ]
    );
    assert_eq!(entries[0].kind, EntryKind::Note);
    assert_eq!(&*entries[0].folder, "Profiles");
    assert_eq!(
        entries[0].file.as_deref(),
        Some(abs("Profiles/Flavor profiles.md").as_path())
    );
    assert_eq!(entries[1].kind, EntryKind::Alias);
    assert_eq!(entries[1].target.as_deref(), Some("Flavor profiles"));
    assert_eq!(entries[6].kind, EntryKind::File);
}

#[test]
fn titles_rank_before_paths_and_an_exact_title_comes_first() {
    let entries = collect(&sample());
    let rows = ranked(&entries, "pro");
    let at = |row: &str| {
        rows.iter()
            .position(|r| r == row)
            .unwrap_or_else(|| panic!("{row} is listed: {rows:?}"))
    };
    // Titles that start with "pro" come first. "Read me first" matches
    // only in its path (Projects/), so it comes after them, but before
    // "Paste and drop", whose title has the letters scattered.
    for title in [
        "Properties",
        "Proofreading and spell check",
        "Profiles → Flavor profiles",
        "Flavor profiles",
    ] {
        assert!(at(title) < at("Read me first"), "{rows:?}");
    }
    assert_eq!(rows.last().map(String::as_str), Some("Paste and drop"));
    assert!(!rows.contains(&"diagram.png".to_owned()));
    // A title equal to the query wins over better-scattered matches.
    let rows = ranked(&entries, "properties");
    assert_eq!(rows[0], "Properties");
}

#[test]
fn aliases_find_their_note() {
    let entries = collect(&sample());
    let rows = ranked(&entries, "profiles");
    // The alias is an exact match, so it comes first; its note follows.
    assert_eq!(rows[0], "Profiles → Flavor profiles");
    assert_eq!(rows[1], "Flavor profiles");
}

#[test]
fn path_segments_match() {
    let entries = collect(&sample());
    assert_eq!(ranked(&entries, "proj/read"), ["Read me first"]);
    assert_eq!(ranked(&entries, "editing paste")[0], "Paste and drop");
    // Words match in any order.
    assert_eq!(ranked(&entries, "drop paste"), ["Paste and drop"]);
}

#[test]
fn case_and_accents_are_folded() {
    let entries = collect(&gather(vec![
        file("Café menu.md", Some("Café menu")),
        file("Résumé.md", Some("Résumé")),
        file("Other.md", Some("Other")),
    ]));
    assert_eq!(ranked(&entries, "cafe"), ["Café menu"]);
    assert_eq!(ranked(&entries, "CAFE MENU"), ["Café menu"]);
    assert_eq!(ranked(&entries, "resume"), ["Résumé"]);
    // A query with an accent asks for it.
    assert_eq!(ranked(&entries, "café"), ["Café menu"]);
}

#[test]
fn recent_files_are_boosted() {
    let mut g = gather(vec![
        file("Notes/Plan.md", Some("Plan")),
        file("Notes/Planet.md", Some("Planet")),
        file("Notes/Plane.md", Some("Plane")),
    ]);
    let entries = collect(&g);
    assert_eq!(ranked(&entries, "plan")[0], "Plan");
    assert_eq!(ranked(&entries, "plane"), ["Plane", "Planet"]);
    // "Planet" was opened recently: among equal matches it comes first,
    // but an exact title still wins.
    g.recent = vec![abs("Notes/Planet.md")];
    let entries = collect(&g);
    assert_eq!(ranked(&entries, "plan")[..2], ["Plan", "Planet"]);
    assert_eq!(ranked(&entries, "pla")[0], "Planet");
}

#[test]
fn an_empty_query_lists_open_and_recent_files_first() {
    let mut g = sample();
    g.recent = vec![
        abs("Vault/Properties.md"),
        PathBuf::from("/elsewhere/Outside.md"),
    ];
    g.open = vec![(abs("Editing/Paste and drop.md"), true)];
    let entries = collect(&g);
    let rows = ranked(&entries, "");
    assert_eq!(rows[..3], ["Properties", "Outside", "Paste and drop"]);
    // Then the vault's files by path; no aliases.
    assert!(!rows.iter().any(|r| r.contains('→')));
    assert_eq!(rows.len(), 7);
    let paste = entries
        .iter()
        .find(|e| &*e.label == "Paste and drop")
        .expect("listed");
    assert!(paste.unsaved, "the open tab has unsaved changes");
}

#[test]
fn without_a_vault_the_open_and_recent_files_are_listed() {
    let g = Gather {
        vault: None,
        open: vec![(PathBuf::from("/home/u/notes/Today.md"), false)],
        recent: vec![
            PathBuf::from("/home/u/notes/Yesterday.md"),
            PathBuf::from("/home/u/notes/Today.md"),
            PathBuf::from("/home/u/pics/cat.png"),
        ],
        home: Some(PathBuf::from("/home/u")),
    };
    let entries = collect(&g);
    assert_eq!(ranked(&entries, ""), ["Yesterday", "Today", "cat.png"]);
    assert_eq!(&*entries[0].path_text, "~/notes/Yesterday.md");
    assert_eq!(&*entries[0].folder, "notes");
    assert_eq!(ranked(&entries, "notes/tod"), ["Today"]);
}

#[test]
fn notes_not_created_yet_come_last() {
    let mut g = sample();
    if let Some(v) = &mut g.vault {
        v.unresolved = vec![(Arc::from("Release process"), 2)];
    }
    let entries = collect(&g);
    let rows = ranked(&entries, "pro");
    assert_eq!(rows.last().map(String::as_str), Some("+Release process"));
    assert!(!ranked(&entries, "").contains(&"+Release process".to_owned()));
}

#[test]
fn unsaved_notes_are_marked() {
    let mut g = gather(vec![file("Draft.md", Some("Draft")), file("Done.md", None)]);
    if let Some(v) = &mut g.vault {
        v.unsaved.insert(VaultPath::new("Draft.md").expect("path"));
    }
    let entries = collect(&g);
    assert!(entries[0].unsaved);
    assert!(!entries[1].unsaved);
    assert_eq!(&*entries[1].label, "Done");
}

#[test]
fn modes_take_their_character_off() {
    assert_eq!(mode_of("#intro"), (Mode::Headings, "intro"));
    assert_eq!(mode_of("^abc"), (Mode::Blocks, "abc"));
    assert_eq!(mode_of("plan"), (Mode::Files, "plan"));
}

#[test]
fn matched_letters_are_marked() {
    assert_eq!(highlight("Flavor profiles", "pro"), vec![7..10]);
    assert_eq!(highlight("Café menu", "fe"), vec![2..5]);
    assert!(highlight("Plan", "xyz").is_empty());
    assert!(highlight("Plan", " ").is_empty());
}

#[test]
fn new_note_names_are_made_safe() {
    assert_eq!(
        new_note_path("Release process").as_deref(),
        Some("Release process.md")
    );
    assert_eq!(
        new_note_path("Projects/Plan").as_deref(),
        Some("Projects/Plan.md")
    );
    assert_eq!(new_note_path("notes.md").as_deref(), Some("notes.md"));
    assert_eq!(
        new_note_path("a: b? [c] #d|e^").as_deref(),
        Some("a b c de.md")
    );
    assert_eq!(new_note_path("Trailing.. ").as_deref(), Some("Trailing.md"));
    assert_eq!(new_note_path("con").as_deref(), Some("con_.md"));
    assert_eq!(new_note_path("../x").as_deref(), Some("x.md"));
    // NFD is stored as NFC.
    assert_eq!(
        new_note_path("Cafe\u{301}").as_deref(),
        Some("Caf\u{e9}.md")
    );
    assert_eq!(new_note_path("  "), None);
    assert_eq!(new_note_path("???"), None);
}

/// A vault of `notes` notes, a fifth with two aliases, in 100 folders.
fn large_vault(notes: usize) -> Gather {
    let words = [
        "plan", "meeting", "draft", "review", "journal", "idea", "project", "summary", "report",
        "reading",
    ];
    let mut entries = Vec::with_capacity(notes * 7 / 5);
    for i in 0..notes {
        let title = format!(
            "{} {} {i}",
            words[i % words.len()],
            words[(i / words.len()) % words.len()]
        );
        let path = format!("Folder {}/Sub {}/{title}.md", i % 100, i % 7);
        entries.push(file(&path, Some(&title)));
        if i % 5 == 0 {
            entries.push(alias(&path, &title, &format!("alias {i}")));
            entries.push(alias(&path, &title, &format!("other name {i}")));
        }
    }
    let mut g = gather(entries);
    g.recent = (0..30)
        .map(|i| abs(&format!("Folder {}/Sub {}/x {i}.md", i % 100, i % 7)))
        .collect();
    g
}

/// NFR-PERF-12: ranking 10,000 notes with their aliases per keystroke
/// stays within a frame (16 ms) in an optimised build. Debug builds are
/// several times slower, so they only check for gross regressions.
#[test]
fn ranking_ten_thousand_notes_fits_in_a_frame() {
    let started = Instant::now();
    let entries = collect(&large_vault(10_000));
    let gathered = started.elapsed();
    assert_eq!(entries.len(), 14_000);
    let budget = if cfg!(debug_assertions) {
        Duration::from_millis(250)
    } else {
        Duration::from_millis(16)
    };
    let mut worst = Duration::ZERO;
    // As typed, one keystroke at a time, and a few harder queries.
    let mut queries: Vec<String> = (1..="meeting review".len())
        .map(|n| "meeting review"[..n].to_owned())
        .collect();
    queries.extend(["fo 4/sub", "zzz", "a", "e", "dr 99"].map(str::to_owned));
    for query in &queries {
        let started = Instant::now();
        let rows = rank(&entries, query);
        let took = started.elapsed();
        worst = worst.max(took);
        assert!(query != "meeting" || !rows.is_empty());
    }
    eprintln!(
        "switcher: gathered 14,000 entries in {gathered:?}; worst keystroke {worst:?} \
         over {} queries",
        queries.len()
    );
    assert!(worst < budget, "worst keystroke took {worst:?}");
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Tags panel's tree: nesting, counts, casing, sort and filter, from
//! content index snapshots of temporary vaults.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gv_vault::content::{ContentOptions, ContentSnapshot, DiskSource};
use gv_vault::{IndexConfig, PathIndex, ProfileId};

use super::model::{self, TagSort, TagTree};

fn snapshot(root: &Path) -> ContentSnapshot {
    let paths = PathIndex::build(root, IndexConfig::default())
        .expect("index")
        .0;
    let options = ContentOptions::for_profile(ProfileId::Obsidian);
    let source = DiskSource::new(root, &options);
    ContentSnapshot::build(paths, options, &source, &AtomicBool::new(false)).expect("content")
}

fn tree(files: &[(&str, &str)]) -> TagTree {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join(".obsidian")).expect("marker");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("write");
    }
    model::build(&snapshot(dir.path()))
}

fn listed(tree: &TagTree, sort: TagSort, filter: &str) -> Vec<(usize, String, usize)> {
    model::rows(tree, sort, filter, &HashSet::new())
        .into_iter()
        .map(|r| (r.depth, r.tag.to_string(), r.notes))
        .collect()
}

fn row(depth: usize, tag: &str, notes: usize) -> (usize, String, usize) {
    (depth, tag.to_owned(), notes)
}

#[test]
fn nested_tags_count_their_notes_once() {
    let t = tree(&[
        (
            "A.md",
            "#editing/lists and #editing/tables #editing/lists\n",
        ),
        (
            "B.md",
            "---\ntags: [editing, vault]\n---\nBody #Editing/Lists\n",
        ),
        ("C.md", "#vault #links/checker\n"),
    ]);
    assert_eq!(
        listed(&t, TagSort::NameAscending, ""),
        [
            // A (two nested tags) and B (the tag itself) count once each.
            row(0, "editing", 2),
            row(1, "editing/lists", 2),
            row(1, "editing/tables", 1),
            // Never written alone: a parent all the same.
            row(0, "links", 1),
            row(1, "links/checker", 1),
            row(0, "vault", 2),
        ]
    );
    // Tags and their parents.
    assert_eq!(t.len, 6);
}

#[test]
fn a_tag_shows_as_first_written_in_path_order() {
    let t = tree(&[
        ("A.md", "#Project/Alpha\n"),
        ("B.md", "#project/alpha #PROJECT\n"),
    ]);
    assert_eq!(
        listed(&t, TagSort::NameAscending, ""),
        [row(0, "Project", 2), row(1, "Project/Alpha", 2)]
    );
}

#[test]
fn sort_orders() {
    let t = tree(&[
        ("A.md", "#beta #alpha\n"),
        ("B.md", "#beta #gamma\n"),
        ("C.md", "#beta #gamma\n"),
    ]);
    let names = |sort| {
        listed(&t, sort, "")
            .into_iter()
            .map(|r| r.1)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(TagSort::NameAscending), ["alpha", "beta", "gamma"]);
    assert_eq!(names(TagSort::NameDescending), ["gamma", "beta", "alpha"]);
    assert_eq!(names(TagSort::CountDescending), ["beta", "gamma", "alpha"]);
    assert_eq!(names(TagSort::CountAscending), ["alpha", "gamma", "beta"]);
}

#[test]
fn the_filter_keeps_matches_and_the_tags_above_them() {
    let t = tree(&[
        ("A.md", "#editing/lists #editing/tables #links/checker\n"),
        ("B.md", "#vault\n"),
    ]);
    assert_eq!(
        listed(&t, TagSort::NameAscending, "TAB"),
        [row(0, "editing", 1), row(1, "editing/tables", 1)]
    );
    // A leading `#` is the tag's, not part of the text.
    assert_eq!(
        listed(&t, TagSort::NameAscending, "#vau"),
        [row(0, "vault", 1)]
    );
    assert!(listed(&t, TagSort::NameAscending, "nothing").is_empty());
}

#[test]
fn closed_tags_hide_their_nested_tags_unless_filtering() {
    let t = tree(&[("A.md", "#editing/lists #vault\n")]);
    let closed: HashSet<Arc<str>> = [Arc::from("editing")].into_iter().collect();
    let rows = |filter| {
        model::rows(&t, TagSort::NameAscending, filter, &closed)
            .into_iter()
            .map(|r| (r.tag.to_string(), r.expanded))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows(""),
        [("editing".to_owned(), false), ("vault".to_owned(), true)]
    );
    assert_eq!(
        rows("list"),
        [
            ("editing".to_owned(), true),
            ("editing/lists".to_owned(), true)
        ]
    );
}

#[test]
fn code_and_text_properties_are_not_tags() {
    let t = tree(&[(
        "A.md",
        "---\nsummary: \"about #notatag\"\n---\n`#code` #real\n\n```\n#fenced\n```\n",
    )]);
    assert_eq!(listed(&t, TagSort::NameAscending, ""), [row(0, "real", 1)]);
}

#[test]
fn a_tag_search_names_the_whole_tag() {
    assert_eq!(model::search_for("editing/lists"), "tag:#editing/lists");
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use super::*;
use crate::content::{ContentOptions, ContentSnapshot, DiskSource, extract};
use crate::index::{IndexConfig, PathIndex};
use crate::profile::{ProfileId, ProfileParams};

fn vault(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    for (path, text) in files {
        let full = dir.path().join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("folders");
        fs::write(full, text).expect("write");
    }
    dir
}

fn p(s: &str) -> VaultPath {
    VaultPath::new(s).expect("path")
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).expect("read")
}

/// Plans `target` of `note` renamed to `new`, with the note's text on disk.
fn plan(
    root: &Path,
    profile: ProfileId,
    note: &str,
    target: AnchorTarget,
    new: &str,
) -> Result<AnchorPlan, RefactorError> {
    let paths = PathIndex::build(root, IndexConfig::default())
        .expect("index")
        .0;
    let options = ContentOptions::for_profile(profile);
    let source = DiskSource::new(root, &options);
    let content = ContentSnapshot::build(paths.clone(), options, &source, &AtomicBool::new(false))
        .expect("content");
    let text = read(root, note);
    plan_anchor_rename(
        root,
        &paths,
        Some(&content),
        &ProfileParams::for_profile(profile),
        &text,
        &AnchorRename {
            note: p(note),
            target,
            new_name: new.to_owned(),
        },
    )
}

/// Applies the plan: the note's own edit written like a save, the others
/// through the journal.
fn apply_all(root: &Path, plan: &AnchorPlan) -> AppliedRefactor {
    fs::write(plan.own.path.to_fs_path(root), &plan.own.after).expect("write note");
    apply(&plan.others, None).expect("apply")
}

#[test]
fn heading_rename_rewrites_wikilinks_embeds_and_markdown_links() {
    let note = "# Top\n\n## Old Head\n\ntext [[#Old Head]] and [[#Other]]\n\n## Other\n";
    let dir = vault(&[
        ("Note.md", note),
        (
            "a.md",
            "[[Note#Old Head]] [[Note#Old Head|shown]] ![[Note#old head]]\n\
             [md](Note.md#Old%20Head) `[[Note#Old Head]]` [[Note#Other]]\n\
             [[Note#Top#Old Head]]\n",
        ),
        ("b.md", "[[Note]] only\n"),
    ]);
    let root = dir.path();
    let plan = plan(
        root,
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Heading(1),
        "New: [Name]",
    )
    .expect("plan");
    assert_eq!(plan.old_name, "Old Head");
    assert_eq!(plan.new_name, "New: [Name]");
    assert_eq!(plan.others.edits.len(), 1);
    assert_eq!(plan.link_count(), 6);
    assert!(plan.others.skipped.is_empty(), "{:?}", plan.others.skipped);
    assert_eq!(
        plan.own.after,
        "# Top\n\n## New: [Name]\n\ntext [[#New: Name]] and [[#Other]]\n\n## Other\n"
    );
    let applied = apply_all(root, &plan);
    assert_eq!(
        read(root, "a.md"),
        "[[Note#New: Name]] [[Note#New: Name|shown]] ![[Note#New: Name]]\n\
         [md](Note.md#New:%20Name) `[[Note#Old Head]]` [[Note#Other]]\n\
         [[Note#Top#New: Name]]\n"
    );
    assert_eq!(read(root, "b.md"), "[[Note]] only\n");
    undo(&applied, None).expect("undo");
    assert!(read(root, "a.md").starts_with("[[Note#Old Head]] [[Note#Old Head|shown]]"));
}

#[test]
fn slug_links_follow_and_duplicate_suffixes_are_recomputed() {
    let note = "# Intro\n\nx\n\n## Intro\n\ny\n";
    let dir = vault(&[
        ("note.md", note),
        (
            "a.md",
            "[first](note.md#intro) [second](note.md#intro-1) [gh](note.md#user-content-intro)\n",
        ),
    ]);
    let root = dir.path();
    let plan = plan(
        root,
        ProfileId::GfmFile,
        "note.md",
        AnchorTarget::Heading(0),
        "Getting started",
    )
    .expect("plan");
    apply_all(root, &plan);
    assert_eq!(
        read(root, "a.md"),
        "[first](note.md#getting-started) [second](note.md#intro) [gh](note.md#user-content-getting-started)\n"
    );
    assert_eq!(
        read(root, "note.md"),
        "# Getting started\n\nx\n\n## Intro\n\ny\n"
    );
}

#[test]
fn block_id_rename_rewrites_fragments_and_the_marker() {
    let note = "Para one ^abc\n\n- item\n\n^list1\n";
    let dir = vault(&[
        ("Note.md", note),
        (
            "a.md",
            "[[Note#^abc]] ![[Note#^abc]] [x](Note.md#^abc) [[Note#^list1]]\n",
        ),
    ]);
    let root = dir.path();
    let plan_ok = plan(
        root,
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Block("abc".into()),
        "^new-id",
    )
    .expect("plan");
    assert_eq!(plan_ok.own.after, "Para one ^new-id\n\n- item\n\n^list1\n");
    assert_eq!(plan_ok.link_count(), 3);
    apply_all(root, &plan_ok);
    assert_eq!(
        read(root, "a.md"),
        "[[Note#^new-id]] ![[Note#^new-id]] [x](Note.md#^new-id) [[Note#^list1]]\n"
    );
    // The own-line marker of a list.
    let list = plan(
        root,
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Block("list1".into()),
        "items",
    )
    .expect("plan");
    assert_eq!(list.own.after, "Para one ^new-id\n\n- item\n\n^items\n");
    // Taken and invalid ids are refused.
    assert!(matches!(
        plan(
            root,
            ProfileId::Obsidian,
            "Note.md",
            AnchorTarget::Block("list1".into()),
            "NEW-ID"
        ),
        Err(RefactorError::InvalidName(_))
    ));
    assert!(matches!(
        plan(
            root,
            ProfileId::Obsidian,
            "Note.md",
            AnchorTarget::Block("list1".into()),
            "a b"
        ),
        Err(RefactorError::InvalidName(_))
    ));
}

#[test]
fn canvas_subpaths_follow() {
    let canvas = "{\n\t\"nodes\":[\n\t\t{\"id\":\"1\",\"type\":\"file\",\"file\":\"Note.md\",\"subpath\":\"#Old\"},\n\t\t{\"id\":\"2\",\"type\":\"file\",\"file\":\"Other.md\",\"subpath\":\"#Old\"},\n\t\t{\"id\":\"3\",\"type\":\"file\",\"file\":\"Note.md\",\"subpath\":\"#^blk\"}\n\t]\n}";
    let dir = vault(&[
        ("Note.md", "# Old\n\ntext ^blk\n"),
        ("Other.md", "# Old\n"),
        ("board.canvas", canvas),
    ]);
    let root = dir.path();
    let heading = plan(
        root,
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Heading(0),
        "Fresh \"name\"",
    )
    .expect("plan");
    let applied = apply_all(root, &heading);
    let after = read(root, "board.canvas");
    assert_eq!(
        after,
        canvas.replacen("\"#Old\"", "\"#Fresh \\\"name\\\"\"", 1)
    );
    undo(&applied, None).expect("undo");
    assert_eq!(read(root, "board.canvas"), canvas);
    fs::write(root.join("Note.md"), "# Old\n\ntext ^blk\n").expect("restore");
    let block = plan(
        root,
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Block("blk".into()),
        "b2",
    )
    .expect("plan");
    apply_all(root, &block);
    assert!(read(root, "board.canvas").contains("\"subpath\":\"#^b2\""));
}

#[test]
fn base_link_literals_follow() {
    let base = "filters:\n  or:\n    - file.hasLink(link(\"Note#Old\"))\n    - file.hasLink(link('Other#Old'))\n    - file.hasLink(link(\"Note#^blk\"))\n    - \"file.hasLink(link(\\\"Note#Old\\\"))\"\n";
    let dir = vault(&[
        ("Note.md", "# Old\n\ntext ^blk\n"),
        ("Other.md", "# Old\n"),
        ("tasks.base", base),
    ]);
    let root = dir.path();
    let heading = plan(
        root,
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Heading(0),
        "Fresh name",
    )
    .expect("plan");
    // The literal inside an escaped YAML string is listed, not rewritten.
    assert_eq!(
        heading.others.skipped.len(),
        1,
        "{:?}",
        heading.others.skipped
    );
    let applied = apply_all(root, &heading);
    assert_eq!(
        read(root, "tasks.base"),
        base.replacen("link(\"Note#Old\")", "link(\"Note#Fresh name\")", 1)
    );
    undo(&applied, None).expect("undo");
    assert_eq!(read(root, "tasks.base"), base);
    fs::write(root.join("Note.md"), "# Old\n\ntext ^blk\n").expect("restore");
    let block = plan(
        root,
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Block("blk".into()),
        "b2",
    )
    .expect("plan");
    apply_all(root, &block);
    assert!(read(root, "tasks.base").contains("link(\"Note#^b2\")"));
}

#[test]
fn unchanged_missing_and_bad_headings_are_refused() {
    let dir = vault(&[("Note.md", "# Same\n\nSetext\n======\n")]);
    let root = dir.path();
    let go = |target, new: &str| plan(root, ProfileId::Obsidian, "Note.md", target, new);
    assert!(matches!(
        go(AnchorTarget::Heading(0), "Same"),
        Err(RefactorError::Unchanged)
    ));
    assert!(matches!(
        go(AnchorTarget::Heading(5), "x"),
        Err(RefactorError::NoAnchor(_))
    ));
    assert!(matches!(
        go(AnchorTarget::Heading(0), "  "),
        Err(RefactorError::InvalidName(_))
    ));
    assert!(matches!(
        go(AnchorTarget::Block("nope".into()), "x"),
        Err(RefactorError::NoAnchor(_))
    ));
    let setext = go(AnchorTarget::Heading(1), "Renamed").expect("setext");
    assert_eq!(setext.own.after, "# Same\n\nRenamed\n======\n");
}

#[test]
fn a_setext_heading_over_several_lines_is_renamed_whole() {
    let note = "# Top\n\nFirst line\nsecond line\n---\n\n> Quoted one\n> quoted two\n> ===\n";
    let dir = vault(&[("Note.md", note)]);
    let root = dir.path();
    let go = |i, new: &str| {
        plan(
            root,
            ProfileId::Obsidian,
            "Note.md",
            AnchorTarget::Heading(i),
            new,
        )
        .expect("plan")
    };
    let plain = go(1, "One line");
    assert_eq!(
        plain.own.after,
        "# Top\n\nOne line\n---\n\n> Quoted one\n> quoted two\n> ===\n"
    );
    let quoted = go(2, "Quoted");
    assert_eq!(
        quoted.own.after,
        "# Top\n\nFirst line\nsecond line\n---\n\n> Quoted\n> ===\n"
    );
}

#[test]
fn heading_text_ranges() {
    for (text, want) in [
        ("## Foo bar ##\n", "Foo bar"),
        ("#   Spaced\n", "Spaced"),
        ("> ## Quoted\n", "Quoted"),
        ("Setext\n---\n", "Setext"),
        ("# A #hash tag\n", "A #hash tag"),
    ] {
        let data = extract(text, ProfileId::Obsidian);
        let h = &data.headings[0];
        let range = heading_text_range(text, h).expect("range");
        assert_eq!(&text[range], want, "{text:?}");
    }
}

#[test]
fn a_second_heading_with_the_same_text_warns() {
    let dir = vault(&[("Note.md", "# A\n\n# B\n"), ("x.md", "[[Note#A]]\n")]);
    let plan = plan(
        dir.path(),
        ProfileId::Obsidian,
        "Note.md",
        AnchorTarget::Heading(0),
        "B",
    )
    .expect("plan");
    assert_eq!(plan.others.warnings.len(), 1);
    // The link would land on the first "B", the renamed heading: kept.
    assert_eq!(plan.others.edits[0].after, "[[Note#B]]\n");
}

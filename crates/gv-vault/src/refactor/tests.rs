// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use super::*;
use crate::content::{ContentOptions, ContentSnapshot, DiskSource};
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

fn mv(from: &str, to: &str) -> Move {
    Move {
        from: p(from),
        to: p(to),
    }
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).expect("read")
}

/// Plans `moves` with the content index of `profile`.
fn plan(root: &Path, profile: ProfileId, moves: &[Move]) -> Result<RefactorPlan, RefactorError> {
    let paths = PathIndex::build(root, IndexConfig::default())
        .expect("index")
        .0;
    let options = ContentOptions::for_profile(profile);
    let source = DiskSource::new(root, &options);
    let content = ContentSnapshot::build(paths.clone(), options, &source, &AtomicBool::new(false))
        .expect("content");
    plan_moves(
        root,
        &paths,
        Some(&content),
        &ProfileParams::for_profile(profile),
        moves,
    )
}

fn rename_and_apply(root: &Path, profile: ProfileId, moves: &[Move]) -> AppliedRefactor {
    let plan = plan(root, profile, moves).expect("plan");
    apply(&plan, None).expect("apply")
}

#[test]
fn renaming_a_note_rewrites_links_in_their_own_style() {
    let dir = vault(&[
        ("Old.md", "# Old\n"),
        (
            "a.md",
            "[[Old]] and [[Old#Old|shown]] and ![[Old]]\n[md](Old.md#old) `[[Old]]`\n",
        ),
        ("sub/b.md", "[up](../Old.md) [[Old]]\n```\n[[Old]]\n```\n"),
        ("c.md", "[[Other]]\n"),
    ]);
    let root = dir.path();
    let plan = plan(root, ProfileId::Obsidian, &[mv("Old.md", "New name.md")]).expect("plan");
    assert_eq!(plan.edits.len(), 2);
    assert_eq!(plan.link_count(), 6);
    apply(&plan, None).expect("apply");
    assert_eq!(
        read(root, "a.md"),
        "[[New name]] and [[New name#Old|shown]] and ![[New name]]\n[md](New%20name.md#old) `[[Old]]`\n"
    );
    assert_eq!(
        read(root, "sub/b.md"),
        "[up](../New%20name.md) [[New name]]\n```\n[[Old]]\n```\n"
    );
    assert_eq!(read(root, "c.md"), "[[Other]]\n");
    assert!(root.join("New name.md").exists());
    assert!(!root.join("Old.md").exists());
}

#[test]
fn moving_a_note_updates_its_own_relative_links() {
    let dir = vault(&[
        (
            "notes/n.md",
            "[a](a.md) ![img](./pic.png) [top](/notes/a.md)\n",
        ),
        ("notes/a.md", "[back](n.md)\n"),
        ("notes/pic.png", "png"),
        ("archive/x.txt", ""),
    ]);
    let root = dir.path();
    rename_and_apply(
        root,
        ProfileId::GfmFile,
        &[mv("notes/n.md", "archive/n.md")],
    );
    assert_eq!(
        read(root, "archive/n.md"),
        "[a](../notes/a.md) ![img](../notes/pic.png) [top](/notes/a.md)\n"
    );
    assert_eq!(read(root, "notes/a.md"), "[back](../archive/n.md)\n");
}

#[test]
fn obsidian_links_by_name_keep_working_after_a_move() {
    let dir = vault(&[
        ("n.md", "[[n2]] [m](n2.md)\n"),
        ("n2.md", "x\n"),
        ("src.md", "[[n]] [l](n.md)\n"),
        ("deep/.keep", ""),
    ]);
    let root = dir.path();
    let plan = plan(root, ProfileId::Obsidian, &[mv("n.md", "deep/n.md")]).expect("plan");
    assert!(plan.edits.is_empty(), "{:?}", plan.edits);
}

/// Links in front matter are rewritten as their YAML scalar needs: a
/// `'` doubled in single quotes.
#[test]
fn front_matter_links_keep_their_yaml_valid() {
    let dir = vault(&[
        ("Old.md", "# Old"),
        (
            "a.md",
            "---\nup: '[[Old]]'\ndown: \"[[Old]]\"\nrel: ['[[Old]]', \"[[Old]]\"]\n---\n[[Old]]\n",
        ),
    ]);
    let root = dir.path();
    rename_and_apply(root, ProfileId::Obsidian, &[mv("Old.md", "Bob's.md")]);
    assert_eq!(
        read(root, "a.md"),
        "---\nup: '[[Bob''s]]'\ndown: \"[[Bob's]]\"\nrel: ['[[Bob''s]]', \"[[Bob's]]\"]\n---\n[[Bob's]]\n"
    );
}

#[test]
fn yaml_quotes_are_read_from_the_line_start() {
    let quote = |line: &str| plan::yaml_quote_at(line, line.find("[[").expect("a link"));
    assert_eq!(quote("up: '[[A]]'"), Some('\''));
    assert_eq!(quote("up: \"see [[A]]\""), Some('"'));
    assert_eq!(quote("- '[[A]]'"), Some('\''));
    assert_eq!(quote("up: it's [[A]]"), None);
    assert_eq!(quote("x: ['a''b', \"c\\\"d\", [[A]]]"), None);
    assert_eq!(quote("\"k\": '[[A]]'"), Some('\''));
}

#[test]
fn renaming_a_folder_updates_links_into_and_out_of_it() {
    let dir = vault(&[
        ("Projects/p.md", "[top](../top.md) [same](q.md)\n"),
        ("Projects/q.md", "q\n"),
        ("top.md", "[p](Projects/p.md) [[Projects/q]]\n"),
    ]);
    let root = dir.path();
    rename_and_apply(root, ProfileId::Obsidian, &[mv("Projects", "Work")]);
    // Relative links inside the folder still resolve: unchanged.
    assert_eq!(read(root, "Work/p.md"), "[top](../top.md) [same](q.md)\n");
    assert_eq!(read(root, "top.md"), "[p](Work/p.md) [[Work/q]]\n");
}

#[test]
fn a_case_only_rename_goes_through_a_temporary_name() {
    let dir = vault(&[("Note.md", "n\n"), ("a.md", "[[Note]] [x](Note.md)\n")]);
    let root = dir.path();
    let m = mv("Note.md", "note.md");
    assert!(m.is_case_only());
    let applied = rename_and_apply(root, ProfileId::Obsidian, &[m]);
    let names: Vec<String> = fs::read_dir(root)
        .expect("list")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&"note.md".to_owned()), "{names:?}");
    assert!(!names.contains(&"Note.md".to_owned()), "{names:?}");
    assert!(!names.iter().any(|n| n.contains("gv-tmp")), "{names:?}");
    assert_eq!(read(root, "a.md"), "[[note]] [x](note.md)\n");
    undo(&applied, None).expect("undo");
    assert!(root.join("Note.md").exists());
    assert_eq!(read(root, "a.md"), "[[Note]] [x](Note.md)\n");
}

#[test]
fn collisions_are_refused_in_any_letter_case() {
    let dir = vault(&[("a.md", ""), ("B.md", ""), ("dir/x.md", "")]);
    let root = dir.path();
    assert!(matches!(
        plan(root, ProfileId::Obsidian, &[mv("a.md", "b.md")]),
        Err(RefactorError::Exists(_))
    ));
    assert!(matches!(
        plan(root, ProfileId::Obsidian, &[mv("dir", "dir/sub")]),
        Err(RefactorError::IntoItself)
    ));
    assert!(matches!(
        plan(root, ProfileId::Obsidian, &[mv("a.md", "a.md")]),
        Err(RefactorError::Unchanged)
    ));
    assert!(matches!(
        plan(root, ProfileId::Obsidian, &[mv("a.md", "a?.md")]),
        Err(RefactorError::InvalidName(_))
    ));
    assert!(matches!(
        plan(root, ProfileId::Obsidian, &[mv("a.md", "nowhere/a.md")]),
        Err(RefactorError::NoFolder(_))
    ));
    assert!(matches!(
        plan(root, ProfileId::Obsidian, &[mv("gone.md", "x.md")]),
        Err(RefactorError::Missing(_))
    ));
}

#[test]
fn undo_reverts_every_file_in_one_step() {
    let dir = vault(&[
        ("Old.md", "[[x]]\n"),
        ("x.md", "[[Old]]\n"),
        ("y.md", "[o](Old.md)\n"),
    ]);
    let root = dir.path();
    let applied = rename_and_apply(root, ProfileId::Obsidian, &[mv("Old.md", "New.md")]);
    assert_eq!(applied.edited_notes(), 2);
    undo(&applied, None).expect("undo");
    assert_eq!(read(root, "x.md"), "[[Old]]\n");
    assert_eq!(read(root, "y.md"), "[o](Old.md)\n");
    assert!(root.join("Old.md").exists());
    assert!(!root.join("New.md").exists());
}

#[test]
fn undo_is_refused_after_a_later_edit() {
    let dir = vault(&[("Old.md", ""), ("x.md", "[[Old]]\n")]);
    let root = dir.path();
    let applied = rename_and_apply(root, ProfileId::Obsidian, &[mv("Old.md", "New.md")]);
    fs::write(root.join("x.md"), "[[New]] edited\n").expect("write");
    assert!(matches!(
        undo(&applied, None),
        Err(RefactorError::Changed(_))
    ));
    // Nothing moved back.
    assert!(root.join("New.md").exists());
    assert_eq!(read(root, "x.md"), "[[New]] edited\n");
}

#[test]
fn a_note_changed_after_the_preview_stops_the_refactor() {
    let dir = vault(&[("Old.md", ""), ("x.md", "[[Old]]\n")]);
    let root = dir.path();
    let plan = plan(root, ProfileId::Obsidian, &[mv("Old.md", "New.md")]).expect("plan");
    fs::write(root.join("x.md"), "[[Old]] typed\n").expect("write");
    assert!(matches!(apply(&plan, None), Err(RefactorError::Changed(_))));
    assert!(root.join("Old.md").exists());
    assert_eq!(read(root, "x.md"), "[[Old]] typed\n");
}

#[test]
fn a_failed_move_rolls_the_edits_back() {
    let dir = vault(&[("Old.md", ""), ("x.md", "[[Old]]\n")]);
    let root = dir.path();
    let journal = tempfile::tempdir().expect("journal");
    let plan = plan(root, ProfileId::Obsidian, &[mv("Old.md", "New.md")]).expect("plan");
    // Something takes the name between the preview and the move.
    fs::write(root.join("New.md"), "someone else\n").expect("write");
    assert!(matches!(
        apply(&plan, Some(journal.path())),
        Err(RefactorError::Exists(_))
    ));
    assert_eq!(read(root, "x.md"), "[[Old]]\n");
    assert_eq!(read(root, "New.md"), "someone else\n");
    assert!(root.join("Old.md").exists());
    assert_eq!(fs::read_dir(journal.path()).expect("list").count(), 0);
}

#[test]
fn recovery_rolls_back_an_interrupted_refactor() {
    let dir = vault(&[("Old.md", ""), ("x.md", "[[Old]]\n")]);
    let root = dir.path();
    let journal = tempfile::tempdir().expect("journal");
    let plan = plan(root, ProfileId::Obsidian, &[mv("Old.md", "New.md")]).expect("plan");
    // As if the process died after the steps but before the journal went.
    let steps = super::apply::tests_support::steps_of(&plan);
    super::apply::tests_support::journal(journal.path(), &steps);
    fs::write(root.join("x.md"), &plan.edits[0].after).expect("edit");
    fs::rename(root.join("Old.md"), root.join("New.md")).expect("move");

    let recovered = recover(root, journal.path());
    assert_eq!(recovered.len(), 1);
    assert!(recovered[0].problems.is_empty(), "{recovered:?}");
    assert_eq!(read(root, "x.md"), "[[Old]]\n");
    assert!(root.join("Old.md").exists());
    assert!(!root.join("New.md").exists());
    assert_eq!(fs::read_dir(journal.path()).expect("list").count(), 0);
}

#[test]
fn recovery_skips_steps_never_done() {
    let dir = vault(&[("Old.md", ""), ("x.md", "[[Old]]\n")]);
    let root = dir.path();
    let journal = tempfile::tempdir().expect("journal");
    let plan = plan(root, ProfileId::Obsidian, &[mv("Old.md", "New.md")]).expect("plan");
    let steps = super::apply::tests_support::steps_of(&plan);
    super::apply::tests_support::journal(journal.path(), &steps);
    let recovered = recover(root, journal.path());
    assert!(recovered[0].problems.is_empty(), "{recovered:?}");
    assert_eq!(read(root, "x.md"), "[[Old]]\n");
    assert!(root.join("Old.md").exists());
}

#[test]
fn links_that_still_resolve_are_left_alone() {
    let dir = vault(&[
        ("a/n.md", "[s](s.md)\n"),
        ("a/s.md", "[n](n.md)\n"),
        ("src.md", "[n](a/n.md) [s](a/s.md)\n"),
    ]);
    let root = dir.path();
    let plan = plan(root, ProfileId::GfmFile, &[mv("a", "b")]).expect("plan");
    // Inside the folder the relative links still resolve.
    assert_eq!(plan.edits.len(), 1);
    assert_eq!(plan.edits[0].path.as_str(), "src.md");
    assert_eq!(plan.edits[0].after, "[n](b/n.md) [s](b/s.md)\n");
}

#[test]
fn a_reference_links_definition_is_rewritten() {
    let dir = vault(&[
        ("Old.md", ""),
        (
            "x.md",
            "[a][r] and [r] and ![i][r]\n\n[r]: Old.md \"Title\"\n",
        ),
    ]);
    let root = dir.path();
    let plan = plan(root, ProfileId::Obsidian, &[mv("Old.md", "New.md")]).expect("plan");
    assert!(plan.skipped.is_empty(), "{:?}", plan.skipped);
    assert_eq!(plan.edits.len(), 1);
    assert_eq!(
        plan.edits[0].after,
        "[a][r] and [r] and ![i][r]\n\n[r]: New.md \"Title\"\n"
    );
    assert_eq!(plan.edits[0].changes.len(), 1);
    assert_eq!(plan.edits[0].changes[0].old, "[r]: Old.md \"Title\"");
    assert_eq!(plan.edits[0].changes[0].line, 2);
}

#[test]
fn reference_definitions_follow_a_move_in_their_written_style() {
    let dir = vault(&[
        ("a/T x.md", ""),
        ("a/n.md", "[t]\n\n[t]: <T x.md#Part>\n[u]: ./T%20x.md\n"),
        ("src.md", "> [t]\n>\n> [t]: a/T%20x.md\n"),
    ]);
    let root = dir.path();
    rename_and_apply(root, ProfileId::GfmFile, &[mv("a", "c")]);
    // The moved note's own definitions are relative to its folder and keep
    // resolving; the other note's definition (inside a quote) follows.
    assert_eq!(
        read(root, "c/n.md"),
        "[t]\n\n[t]: <T x.md#Part>\n[u]: ./T%20x.md\n"
    );
    assert_eq!(read(root, "src.md"), "> [t]\n>\n> [t]: c/T%20x.md\n");
}

#[test]
fn a_moved_notes_own_reference_definition_is_rewritten() {
    let dir = vault(&[
        ("t.md", ""),
        ("n.md", "[t]\n\n[t]: t.md\n"),
        ("d/other.md", ""),
    ]);
    let root = dir.path();
    rename_and_apply(root, ProfileId::GfmFile, &[mv("n.md", "d/n.md")]);
    assert_eq!(read(root, "d/n.md"), "[t]\n\n[t]: ../t.md\n");
}

#[test]
fn names_are_checked_for_every_platform() {
    assert!(validate_name("Note.md").is_ok());
    assert!(validate_name("Ünïcode name.md").is_ok());
    for bad in [
        "", "  ", "a/b", "a\\b", "con.md", "a:b", "a?", "end.", "end ", "a#b", "x|y", "..",
    ] {
        assert!(validate_name(bad).is_err(), "{bad:?}");
    }
    assert_eq!(clean_name("  Cafe\u{301}.md "), "Café.md");
}

#[test]
fn new_names_avoid_every_letter_case() {
    let dir = vault(&[("Untitled", ""), ("note.md", ""), ("Note 1.md", "")]);
    let root = dir.path();
    assert_eq!(
        unique_name(root, "untitled", None, false).expect("name"),
        "untitled 1"
    );
    assert_eq!(
        duplicate_name(root, "Note.md", false).expect("name"),
        "Note 2.md"
    );
    let folder = create_folder(root, &VaultPath::root(), "Untitled").expect("folder");
    assert_eq!(folder.as_str(), "Untitled 1");
    assert!(root.join("Untitled 1").is_dir());
    assert!(create_folder(root, &VaultPath::root(), "a|b").is_err());
}

#[test]
fn duplicates_copy_files_and_folders() {
    let dir = vault(&[
        ("Note.md", "text\n"),
        ("Folder/a.md", "a\n"),
        ("Folder/sub/b.md", "b\n"),
    ]);
    let root = dir.path();
    let copy = duplicate(root, &p("Note.md")).expect("copy");
    assert_eq!(copy.as_str(), "Note 1.md");
    assert_eq!(read(root, "Note 1.md"), "text\n");
    let copy = duplicate(root, &p("Folder")).expect("copy");
    assert_eq!(copy.as_str(), "Folder 1");
    assert_eq!(read(root, "Folder 1/sub/b.md"), "b\n");
    assert_eq!(read(root, "Folder/a.md"), "a\n");
}

#[derive(Default)]
struct FakeTrash(Mutex<Vec<std::path::PathBuf>>);

impl Trash for FakeTrash {
    fn delete(&self, path: &Path) -> Result<(), String> {
        fs::remove_file(path)
            .or_else(|_| fs::remove_dir_all(path))
            .map_err(|e| e.to_string())?;
        self.0.lock().expect("lock").push(path.to_path_buf());
        Ok(())
    }
}

#[test]
fn deleting_goes_through_the_trash() {
    let dir = vault(&[("a.md", ""), ("f/b.md", "")]);
    let root = dir.path();
    let trash = FakeTrash::default();
    let done = move_to_trash(root, &[p("a.md"), p("f")], &trash).expect("trash");
    assert_eq!(done.len(), 2);
    assert!(!root.join("a.md").exists());
    assert!(!root.join("f").exists());
    assert_eq!(trash.0.lock().expect("lock").len(), 2);
    assert!(matches!(
        move_to_trash(root, &[VaultPath::root()], &trash),
        Err(RefactorError::Root)
    ));
    assert!(matches!(
        move_to_trash(root, &[p("gone.md")], &trash),
        Err(RefactorError::Missing(_))
    ));
}

const BOARD: &str = r#"{
  "nodes": [
    {"id": "1", "type": "file", "file": "notes/a.md", "x": 0, "y": 0, "width": 10, "height": 10},
    {"id": "2", "type": "file", "file": "other.md", "x": 0, "y": 0, "width": 10, "height": 10},
    {"id": "3", "type": "group", "background": "notes/pic.png", "x": 0, "y": 0, "width": 10, "height": 10}
  ],
  "edges": [],
  "extra": {"kept": true}
}"#;

#[test]
fn renaming_a_note_rewrites_canvas_file_cards_in_place() {
    let dir = vault(&[
        ("notes/a.md", "# A"),
        ("other.md", ""),
        ("notes/pic.png", ""),
        ("board.canvas", BOARD),
    ]);
    let root = dir.path();
    let plan = plan(root, ProfileId::Obsidian, &[mv("notes/a.md", "notes/b.md")]).expect("plan");
    let edit = plan
        .edits
        .iter()
        .find(|e| e.path == p("board.canvas"))
        .expect("canvas edit");
    assert_eq!(edit.changes.len(), 1);
    assert_eq!(edit.changes[0].old, "\"notes/a.md\"");
    assert_eq!(edit.changes[0].new, "\"notes/b.md\"");
    assert_eq!(edit.changes[0].line, 2);
    apply(&plan, None).expect("apply");
    // Only the path string changed: formatting and unknown keys stay.
    assert_eq!(
        read(root, "board.canvas"),
        BOARD.replace("notes/a.md", "notes/b.md")
    );
}

#[test]
fn moving_a_folder_rewrites_every_canvas_path_below_it() {
    let dir = vault(&[
        ("notes/a.md", ""),
        ("notes/pic.png", ""),
        ("other.md", ""),
        ("archive/x.txt", ""),
        ("board.canvas", BOARD),
    ]);
    let root = dir.path();
    let applied = rename_and_apply(root, ProfileId::Obsidian, &[mv("notes", "archive/notes")]);
    let text = read(root, "board.canvas");
    assert!(text.contains("\"archive/notes/a.md\""));
    assert!(text.contains("\"archive/notes/pic.png\""));
    assert!(text.contains("\"other.md\""));
    undo(&applied, None).expect("undo");
    assert_eq!(read(root, "board.canvas"), BOARD);
}

#[test]
fn a_moved_canvas_keeps_its_vault_paths_and_a_broken_one_is_reported() {
    let dir = vault(&[
        ("notes/a.md", ""),
        ("f/x.txt", ""),
        ("board.canvas", BOARD),
        ("bad.canvas", "{\"nodes\": [ {"),
    ]);
    let root = dir.path();
    let plan = plan(
        root,
        ProfileId::Obsidian,
        &[mv("board.canvas", "f/board.canvas")],
    )
    .expect("plan");
    // Canvas paths are from the vault root, so moving the canvas changes
    // nothing in it.
    assert!(plan.edits.iter().all(|e| e.path != p("board.canvas")));
    let plan = plan_for_rename(root);
    assert!(
        plan.skipped
            .iter()
            .any(|s| s.source == p("bad.canvas") && s.reason.contains("not a valid canvas"))
    );
}

fn plan_for_rename(root: &Path) -> RefactorPlan {
    plan(root, ProfileId::Obsidian, &[mv("notes/a.md", "notes/c.md")]).expect("plan")
}

const BASE: &str = r#"filters:
  and:
    - 'file.hasLink(link("notes/a"))'
    - "file == file(\"notes/a.md\")"
    - file.inFolder("notes")
formulas:
  same: 'link( "notes/a.md#Top" )'
  other: 'link("other")'
  doubled: 'link(''notes/a'')'
views:
  - type: table
    name: All
"#;

#[test]
fn renaming_a_note_rewrites_base_literals_by_byte_range() {
    let dir = vault(&[
        ("notes/a.md", "# A"),
        ("other.md", ""),
        ("books.base", BASE),
    ]);
    let root = dir.path();
    let plan = plan(root, ProfileId::Obsidian, &[mv("notes/a.md", "notes/b.md")]).expect("plan");
    let edit = plan
        .edits
        .iter()
        .find(|e| e.path == p("books.base"))
        .expect("base edit");
    let olds: Vec<_> = edit.changes.iter().map(|c| c.old.as_str()).collect();
    assert_eq!(olds, ["notes/a", "notes/a.md#Top"]);
    assert_eq!(edit.changes[0].new, "notes/b");
    assert_eq!(edit.changes[1].new, "notes/b.md#Top");
    // An escaped literal and a YAML-doubled one are left, with a reason.
    let skipped: Vec<_> = plan
        .skipped
        .iter()
        .filter(|s| s.source == p("books.base"))
        .map(|s| s.link.as_str())
        .collect();
    assert_eq!(skipped, ["notes/a.md", "notes/a"]);
    apply(&plan, None).expect("apply");
    let expected = BASE
        .replace("link(\"notes/a\")", "link(\"notes/b\")")
        .replace("\"notes/a.md#Top\"", "\"notes/b.md#Top\"");
    assert_eq!(read(root, "books.base"), expected);
}

#[test]
fn base_refs_finds_link_and_file_literals_only() {
    let text = r#"a: 'link("x") + file('y') + mylink("z") + f.file("w") + link(other) + file.inFolder("d") + inFolder("e")'"#;
    let refs = plan::base_refs(text);
    let values: Vec<_> = refs
        .iter()
        .map(|r| (&text[r.range.clone()], r.folder))
        .collect();
    assert_eq!(values, [("x", false), ("y", false), ("d", true)]);
    assert!(refs.iter().all(|r| r.unsafe_reason.is_none()));
}

/// Moving a folder rewrites the `inFolder` literals that name it or a
/// folder inside it; renaming a note leaves them.
#[test]
fn moving_a_folder_rewrites_base_in_folder_literals() {
    let base = "filters:\n  or:\n    - file.inFolder(\"notes\")\n    - 'file.inFolder(\"notes/sub/\")'\n    - file.inFolder(\"other\")\n    - file.inFolder(\"note\")\n";
    let dir = vault(&[
        ("notes/a.md", "# A"),
        ("notes/sub/b.md", "# B"),
        ("other/c.md", ""),
        ("archive/d.md", ""),
        ("books.base", base),
    ]);
    let root = dir.path();
    let renamed = plan(root, ProfileId::Obsidian, &[mv("notes/a.md", "notes/z.md")]).expect("plan");
    assert!(renamed.edits.iter().all(|e| e.path != p("books.base")));
    let moved = plan(root, ProfileId::Obsidian, &[mv("notes", "archive/notes")]).expect("plan");
    let edit = moved
        .edits
        .iter()
        .find(|e| e.path == p("books.base"))
        .expect("base edit");
    let changes: Vec<_> = edit
        .changes
        .iter()
        .map(|c| (c.old.as_str(), c.new.as_str()))
        .collect();
    assert_eq!(
        changes,
        [
            ("notes", "archive/notes"),
            ("notes/sub/", "archive/notes/sub/")
        ]
    );
}

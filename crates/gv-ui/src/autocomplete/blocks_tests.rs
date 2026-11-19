// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The blocks a block link lists, where a new id goes in each, and adding
//! one to another note's file.

use std::sync::Arc;

use gv_syntax::ParseOptions;
use gv_vault::VaultPath;
use gv_vault::profile::ProfileId;

use super::blocks::{BlockIdRequest, BlockKind, BlockRef, NoteBlock, blocks, insertion};
use super::write_block_id;

fn parse(text: &str) -> Vec<NoteBlock> {
    let tree = gv_syntax::parse_with(text, ParseOptions::builtin(ProfileId::Obsidian));
    blocks(&tree, text)
}

/// `text` with a new id `x1y2z3` added to its block `n`.
fn with_id(text: &str, n: usize) -> String {
    let found = parse(text);
    let block = BlockRef {
        ordinal: n,
        hash: found[n].hash,
        taken: Arc::from(Vec::new()),
    };
    let (at, insert) =
        insertion(text, ProfileId::Obsidian, &block, "x1y2z3").expect("the id goes in");
    format!("{}{insert}{}", &text[..at], &text[at..])
}

fn kinds(text: &str) -> Vec<(BlockKind, String, Option<String>)> {
    parse(text)
        .into_iter()
        .map(|b| (b.kind, b.text, b.id))
        .collect()
}

#[test]
fn paragraphs_and_list_items_get_the_id_at_their_end() {
    let text = "First line\nsecond line\n\nNext.\n";
    assert_eq!(
        with_id(text, 0),
        "First line\nsecond line ^x1y2z3\n\nNext.\n"
    );
    let list = "- one\n- two\n  more\n- three\n";
    let found = kinds(list);
    assert_eq!(found.len(), 3, "{found:?}");
    assert_eq!(with_id(list, 1), "- one\n- two\n  more ^x1y2z3\n- three\n");
}

#[test]
fn tables_code_and_math_get_the_id_on_a_line_of_their_own() {
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\nText after.\n";
    // GFM: the line without a pipe is a row; a blank line ends the table.
    let table = table.replace("Text after.", "\nText after.");
    assert_eq!(kinds(&table)[0].0, BlockKind::Table);
    assert_eq!(kinds(&table)[0].1, "| a | b |");
    assert_eq!(
        with_id(&table, 0),
        "| a | b |\n|---|---|\n| 1 | 2 |\n\n^x1y2z3\n\nText after.\n"
    );
    let code = "```rust\nfn main() {}\n```\nText right after.";
    assert_eq!(kinds(code)[0].0, BlockKind::Code);
    assert_eq!(
        with_id(code, 0),
        "```rust\nfn main() {}\n```\n\n^x1y2z3\n\nText right after."
    );
    // At the end of the note.
    let math = "$$\nx^2\n$$";
    assert_eq!(kinds(math)[0].0, BlockKind::Math);
    assert_eq!(with_id(math, 0), "$$\nx^2\n$$\n\n^x1y2z3");
    // CR LF lines stay CR LF.
    let crlf = "```\ncode\n```\r\n\r\nMore\r\n";
    assert_eq!(
        with_id(crlf, 0),
        "```\ncode\n```\r\n\r\n^x1y2z3\r\n\r\nMore\r\n"
    );
}

#[test]
fn a_quote_is_one_block_with_its_id_after_it() {
    let text = "> A quote\n> on two lines\n\n> [!note] Title\n> Body\n";
    let found = kinds(text);
    assert_eq!(
        found,
        vec![
            (BlockKind::Quote, "A quote".to_owned(), None),
            (BlockKind::Quote, "Title".to_owned(), None),
        ]
    );
    assert_eq!(
        with_id(text, 0),
        "> A quote\n> on two lines\n\n^x1y2z3\n\n> [!note] Title\n> Body\n"
    );
    assert_eq!(
        with_id(text, 1),
        "> A quote\n> on two lines\n\n> [!note] Title\n> Body\n\n^x1y2z3\n"
    );
}

#[test]
fn headings_are_listed_without_a_place_for_an_id() {
    let found = parse("# Intro\n\nText\n\n## Details ##\n");
    assert_eq!(found[0].kind, BlockKind::Heading(0));
    assert_eq!(found[0].text, "Intro");
    assert_eq!(found[2].kind, BlockKind::Heading(1));
    assert_eq!(found[2].text, "Details");
    assert!(found[0].new_id.is_none() && found[2].new_id.is_none());
    let block = BlockRef {
        ordinal: 0,
        hash: found[0].hash,
        taken: Arc::from(Vec::new()),
    };
    assert!(
        insertion(
            "# Intro\n\nText\n\n## Details ##\n",
            ProfileId::Obsidian,
            &block,
            "abc"
        )
        .is_err()
    );
}

#[test]
fn ids_at_the_end_and_on_their_own_line_are_read() {
    let text = "Para ^p1\n\n| a |\n|---|\n\n^tbl\n\n- item ^li\n";
    assert_eq!(
        kinds(text),
        vec![
            (
                BlockKind::Paragraph,
                "Para".to_owned(),
                Some("p1".to_owned())
            ),
            (BlockKind::Table, "| a |".to_owned(), Some("tbl".to_owned())),
            (
                BlockKind::Paragraph,
                "item".to_owned(),
                Some("li".to_owned())
            ),
        ]
    );
    // A block with an id gets no other.
    assert!(parse(text).iter().all(|b| b.new_id.is_none()));
}

#[test]
fn a_new_id_must_be_new_and_the_block_unchanged() {
    let text = "Some text\n\nOther ^taken\n";
    let found = parse(text);
    let block = BlockRef {
        ordinal: 0,
        hash: found[0].hash,
        taken: Arc::from(Vec::new()),
    };
    let err = insertion(text, ProfileId::Obsidian, &block, "TAKEN").expect_err("taken");
    assert!(err.contains("^TAKEN"), "{err}");
    // Edited since it was listed.
    let edited = "Some other text\n\nOther ^taken\n";
    assert!(insertion(edited, ProfileId::Obsidian, &block, "new123").is_err());
    // Moved, but the same text: found again.
    let moved = "New first.\n\nSome text\n\nOther ^taken\n";
    let (at, insert) = insertion(moved, ProfileId::Obsidian, &block, "new123").expect("found");
    assert_eq!(&moved[..at], "New first.\n\nSome text");
    assert_eq!(insert, " ^new123");
}

fn request(root: &std::path::Path, text: &str, n: usize) -> BlockIdRequest {
    let found = parse(text);
    BlockIdRequest {
        file: root.join("Plan.md"),
        root: root.to_path_buf(),
        note: VaultPath::new("Plan.md").expect("path"),
        block: BlockRef {
            ordinal: n,
            hash: found[n].hash,
            taken: Arc::from(Vec::new()),
        },
        id: "k3x9q0".into(),
        profile: ProfileId::Obsidian,
    }
}

#[test]
fn the_file_gets_the_id_through_the_journal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let journal = tempfile::tempdir().expect("journal");
    let text = "# Plan\n\nThe paragraph.\n";
    std::fs::write(dir.path().join("Plan.md"), text).expect("write");
    let req = request(dir.path(), text, 1);
    write_block_id(&req, Some(journal.path())).expect("written");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Plan.md")).expect("read"),
        "# Plan\n\nThe paragraph. ^k3x9q0\n"
    );
}

#[test]
fn a_file_changed_since_it_was_listed_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text = "# Plan\n\nThe paragraph.\n";
    let req = request(dir.path(), text, 1);
    let changed = "# Plan\n\nThe paragraph, edited elsewhere.\n";
    std::fs::write(dir.path().join("Plan.md"), changed).expect("write");
    let err = write_block_id(&req, None).expect_err("refused");
    assert!(err.contains("changed"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Plan.md")).expect("read"),
        changed
    );
    // Not UTF-8: refused too.
    std::fs::write(dir.path().join("Plan.md"), b"\xff\xfe").expect("write");
    assert!(write_block_id(&req, None).is_err());
}

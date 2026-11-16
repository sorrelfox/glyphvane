// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GFM tables (one item per row, cells after gap boxes) and strikethrough
//! in profiles that have them.

mod common;

use gv_inline::ItemKind as RunKind;
use gv_render_tree::{
    BoxRef, CopyOptions, InlineRole, ItemKind, NodeKind, RenderOptions, Sources, TextRole,
    decode_style_id,
};
use gv_syntax::table::Alignment;
use gv_syntax::{ParseOptions, ProfileId};

fn build_in(text: &str, profile: ProfileId) -> common::Doc {
    let doc = common::build_with(
        text,
        ParseOptions::builtin(profile),
        RenderOptions::builtin(profile),
        Sources::default(),
    );
    common::check(&doc);
    doc
}

const TABLE: &str =
    "Before\n| a | *b* | c |\n|:--|:-:|--:|\n| 1 | 2 |\n| x | y | z | w |\n\nAfter\n";

#[test]
fn a_table_is_one_item_per_row() {
    let doc = build_in(TABLE, ProfileId::GfmFile);
    let rt = &doc.render;
    let kinds: Vec<&ItemKind> = rt.items().iter().map(|i| &i.kind).collect();
    assert_eq!(
        kinds,
        [
            &ItemKind::Paragraph,
            &ItemKind::TableRow { row: 0, rows: 3 },
            &ItemKind::TableRow { row: 1, rows: 3 },
            &ItemKind::TableRow { row: 2, rows: 3 },
            &ItemKind::Paragraph,
        ]
    );
    // The paragraph and the table share a text block: distinct item ids.
    assert_eq!(rt.items()[0].id.block, rt.items()[1].id.block);
    assert_eq!(rt.items()[1].id.chunk, 1);
    let NodeKind::Table(t) = &rt.nodes()[1].kind else {
        panic!("table node")
    };
    assert_eq!(
        &*t.aligns,
        [Alignment::Left, Alignment::Center, Alignment::Right]
    );
    assert!(t.rows[0].header && !t.rows[1].header);
    // The header row covers the delimiter row; rows tile the table.
    assert_eq!(
        &TABLE[rt.items()[1].source.clone()],
        "| a | *b* | c |\n|:--|:-:|--:|\n"
    );
    assert_eq!(&TABLE[rt.items()[2].source.clone()], "| 1 | 2 |\n");
    assert_eq!(&TABLE[rt.items()[3].source.clone()], "| x | y | z | w |\n");
    // Header cells in the header role, body cells in the cell role.
    let roles = |row: usize| -> Vec<TextRole> {
        t.rows[row]
            .content
            .paragraph
            .items()
            .iter()
            .filter_map(|it| match it.kind() {
                RunKind::Text(s) => decode_style_id(s.font).map(|(r, _)| r),
                _ => None,
            })
            .collect()
    };
    assert!(roles(0).iter().all(|&r| r == TextRole::TableHeader));
    assert!(roles(1).iter().all(|&r| r == TextRole::TableCell));
}

#[test]
fn cells_follow_gap_boxes() {
    let doc = build_in(TABLE, ProfileId::GfmFile);
    let NodeKind::Table(t) = &doc.render.nodes()[1].kind else {
        panic!("table node")
    };
    let gaps = |row: usize| -> Vec<u32> {
        let c = &t.rows[row].content;
        c.paragraph
            .items()
            .iter()
            .zip(c.spans.iter())
            .filter_map(|(it, s)| match (it.inline_box(), s.role) {
                (Some(b), InlineRole::CellGap(col)) => {
                    assert_eq!(BoxRef::from_id(b.id), BoxRef::Cell(col));
                    assert_eq!(b.width, 0.0);
                    Some(col)
                }
                _ => None,
            })
            .collect()
    };
    // Every row has a gap per column and one after the last cell: short
    // rows are padded, long rows truncated.
    for row in 0..3 {
        assert_eq!(gaps(row), [0, 1, 2, 3], "row {row}");
    }
    // Copy: cells separated by tabs.
    assert_eq!(doc.render.item_text(1).text, "a\tb\tc");
    assert_eq!(doc.render.item_text(2).text, "1\t2\t");
    assert_eq!(doc.render.item_text(3).text, "x\ty\tz");
    let all = doc
        .render
        .plain_text(0..TABLE.len(), CopyOptions::default());
    assert!(all.contains("a\tb\tc\n1\t2\t\nx\ty\tz"), "{all:?}");
}

#[test]
fn commonmark_has_no_tables() {
    let doc = build_in(TABLE, ProfileId::CommonMark);
    assert!(
        doc.render
            .items()
            .iter()
            .all(|i| matches!(i.kind, ItemKind::Paragraph))
    );
}

#[test]
fn obsidian_needs_two_dashes() {
    let doc = build_in("| a |\n| - |\n\n| b |\n| -- |\n", ProfileId::Obsidian);
    let kinds: Vec<&str> = doc.render.items().iter().map(|i| i.kind.name()).collect();
    assert_eq!(kinds, ["paragraph", "table-row"]);
}

#[test]
fn table_rows_follow_edits() {
    let mut doc = build_in(TABLE, ProfileId::GfmFile);
    let at = TABLE.find("| 1").expect("row");
    for (range, insert) in [
        (at + 2..at + 3, "one"),
        (at..at, "| new | row |\n"),
        (0..7, ""),
    ] {
        doc.edit(range, insert, Sources::default());
        common::check(&doc);
        // A full build of the same (incrementally reparsed) syntax tree:
        // block ids of a fresh parse differ.
        let full = gv_render_tree::RenderTree::build(
            &doc.tree,
            &doc.model,
            doc.text.as_str(),
            RenderOptions::builtin(ProfileId::GfmFile),
            Sources::default(),
        );
        assert_eq!(common::canonical(&doc.render), common::canonical(&full));
    }
}

#[test]
fn strikethrough_draws_a_line() {
    let md = "a ~~b~~ c\n";
    let struck = |profile| -> Vec<(String, bool)> {
        let doc = build_in(md, profile);
        let c = doc.render.nodes()[0].inline().expect("inline").clone();
        let p = &c.paragraph;
        p.items()
            .iter()
            .enumerate()
            .filter_map(|(ix, it)| match it.kind() {
                RunKind::Text(s) => Some((p.item_text(ix).to_owned(), s.strikethrough.is_some())),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        struck(ProfileId::GfmFile),
        [
            ("a ".to_owned(), false),
            ("b".to_owned(), true),
            (" c".to_owned(), false)
        ]
    );
    assert_eq!(
        struck(ProfileId::CommonMark),
        [("a ~~b~~ c".to_owned(), false)]
    );
}

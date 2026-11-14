// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Copy as Markdown: [`RenderTree::markdown_for`] on representative
//! selections, and on every spec example.

mod common;

use std::path::PathBuf;

use gv_render_tree::{ItemKind, RenderOptions, RenderTree, Sources};
use gv_syntax::ParseOptions;

/// The Markdown of the selection from the first `from` to the end of the
/// first `to` at or after it.
fn copy(text: &str, from: &str, to: &str) -> String {
    let doc = common::build(text);
    copy_in(&doc, from, to)
}

fn copy_in(doc: &common::Doc, from: &str, to: &str) -> String {
    let start = doc.text.find(from).expect("start in the text");
    let end = start + doc.text[start..].find(to).expect("end in the text") + to.len();
    doc.render.markdown_for(&doc.tree, &doc.text, start..end)
}

#[test]
fn inside_a_paragraph_is_the_slice() {
    let text = "Hello brave new world.\n\nNext.\n";
    assert_eq!(copy(text, "brave", "new"), "brave new");
    assert_eq!(copy(text, "Hello", "world."), "Hello brave new world.");
    // Escapes and entities are copied as written.
    let text = "One \\*two\\* &amp; three\n";
    assert_eq!(copy(text, "\\*two", "&amp;"), "\\*two\\* &amp;");
}

#[test]
fn partial_inline_constructs_grow_to_whole_ones() {
    let text = "See [the docs](a.md \"T\") now.\n";
    // Into the link text, out of it, and inside it.
    assert_eq!(copy(text, "See", "the d"), "See [the docs](a.md \"T\")");
    assert_eq!(copy(text, "docs", "now"), "[the docs](a.md \"T\") now");
    assert_eq!(copy(text, "he", "do"), "[the docs](a.md \"T\")");
    let text = "A **bold *and* nested** word, `some code` and <https://x.org/y>.\n";
    assert_eq!(copy(text, "old", "and"), "**bold *and* nested**");
    assert_eq!(copy(text, "word", "some"), "word, `some code`");
    assert_eq!(
        copy(text, "code", "x.org"),
        "`some code` and <https://x.org/y>"
    );
    // A reference link and an image.
    let text = "Go [here][r] or ![alt text](i.png).\n\n[r]: https://example.com\n";
    assert_eq!(copy(text, "ere", "alt"), "[here][r] or ![alt text](i.png)");
    // A selection that already covers whole constructs is unchanged.
    let text = "x *a* y\n";
    assert_eq!(copy(text, "x", "y"), "x *a* y");
    assert_eq!(copy(text, "a", "a"), "*a*");
}

#[test]
fn across_list_items_each_line_keeps_its_markers() {
    let text = "- one\n- two\n- three\n";
    assert_eq!(copy(text, "one", "two"), "- one\n- two");
    assert_eq!(copy(text, "ne", "thr"), "- ne\n- two\n- thr");
    // Within one item's line: just the text.
    assert_eq!(copy(text, "tw", "wo"), "two");
    let text = "1. first\n2. second\n   - nested a\n   - nested b\n";
    assert_eq!(copy(text, "second", "a"), "2. second\n   - nested a");
    assert_eq!(copy(text, "nested a", "b"), "   - nested a\n   - nested b");
    // A continuation line keeps its indentation.
    let text = "- one\n  more\n- two\n";
    assert_eq!(copy(text, "more", "two"), "  more\n- two");
}

#[test]
fn inside_a_quote_lines_keep_their_markers() {
    let text = "> first line\n> second *line*\n\nAfter.\n";
    assert_eq!(copy(text, "line", "*li"), "> line\n> second *line*");
    assert_eq!(copy(text, "second", "After"), "> second *line*\n\nAfter");
    // One line: no prefix.
    assert_eq!(copy(text, "second", "second"), "second");
    // Nested containers: quote in a list item.
    let text = "- item\n  > quoted\n  > more\n";
    assert_eq!(copy(text, "quoted", "more"), "  > quoted\n  > more");
    let text = "> - a\n> - b\n";
    assert_eq!(copy(text, "a", "b"), "> - a\n> - b");
}

#[test]
fn across_a_code_block_chunk_boundary_is_the_slice() {
    let lines: Vec<String> = (0..9).map(|i| format!("    let x{i} = {i};")).collect();
    let text = format!("Intro.\n\n```rust\n{}\n```\n", lines.join("\n"));
    let opts = RenderOptions {
        chunk_lines: 3,
        ..RenderOptions::default()
    };
    let doc = common::build_with(&text, ParseOptions::default(), opts, Sources::default());
    let chunks = doc
        .render
        .items()
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Code { .. }))
        .count();
    assert_eq!(chunks, 3);
    // From the middle of line 2 to the middle of line 7 crosses two chunk
    // boundaries; the first line's code indentation is not a container
    // prefix of a fenced block, so the copy is the slice.
    let got = copy_in(&doc, "x1 =", "x7");
    let start = text.find("x1 =").expect("start");
    let end = text.find("x7").expect("end") + 2;
    assert_eq!(got, &text[start..end]);
    // An indented code block in a list item keeps the first line's prefix.
    let text = "- item\n\n      code a\n      code b\n";
    assert_eq!(copy(text, "a", "code b"), "      a\n      code b");
}

#[test]
fn crlf_documents_keep_crlf() {
    let text = "- one\r\n- two\r\n\r\n> a\r\n> b *c*\r\n";
    assert_eq!(copy(text, "one", "two"), "- one\r\n- two");
    assert_eq!(copy(text, "two", "c"), "- two\r\n\r\n> a\r\n> b *c*");
    assert_eq!(copy(text, "a", "b"), "> a\r\n> b");
}

#[test]
fn everything_is_the_whole_text() {
    let text = "# T\n\n> q\n\n- a\n- b\n\n```\nc\n```\n";
    let doc = common::build(text);
    assert_eq!(
        doc.render.markdown_for(&doc.tree, text, 0..text.len()),
        text
    );
    // Without the byte order mark.
    let text = "\u{feff}- a\n- b\n";
    let doc = common::build(text);
    assert_eq!(
        doc.render.markdown_for(&doc.tree, text, 3..text.len()),
        "- a\n- b\n"
    );
    // Empty and out-of-range selections.
    assert_eq!(doc.render.markdown_for(&doc.tree, text, 5..5), "");
    assert_eq!(doc.render.markdown_for(&doc.tree, text, 99..120), "");
}

fn spec() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/spec/commonmark-0.31.2.json");
    let text = std::fs::read_to_string(&path).expect("spec fixture");
    let json: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    json.as_array()
        .expect("array")
        .iter()
        .map(|e| e["markdown"].as_str().expect("markdown").to_owned())
        .collect()
}

/// On every spec example, every item's source and a few ranges across
/// items: the copy never panics and holds the selected bytes.
#[test]
fn every_example_copies_its_selection() {
    for md in spec() {
        let doc = common::build(&md);
        let rt: &RenderTree = &doc.render;
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        ranges.push(0..md.len());
        for it in rt.items() {
            ranges.push(it.source.clone());
            let mid = it.source.start + (it.source.end - it.source.start) / 2;
            ranges.push(mid..md.len());
            ranges.push(0..mid);
        }
        for r in ranges {
            if !(md.is_char_boundary(r.start) && md.is_char_boundary(r.end)) {
                continue;
            }
            let got = rt.markdown_for(&doc.tree, &md, r.clone());
            assert!(got.contains(&md[r.clone()]), "{md:?} {r:?}: {got:?}");
        }
    }
}

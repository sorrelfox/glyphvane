// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Block-structure comparison with comrak.
//!
//! An *outline* is a compact, comparable rendering of a document's block
//! tree: one line per block, pre-order, indented two spaces per level.
//! Inlines are not compared; link reference definitions have no line (comrak
//! has no nodes for them).
//!
//! ```text
//! document
//!   list bullet - start=1 tight @1
//!     item @1
//!       paragraph @1-1
//!   block_quote @3
//!     heading h2 setext @3-4
//!   code_block fenced info="rust" @6 "fn main() {}\n"
//!   html_block @9 "<div>\n"
//!   thematic_break @11
//! ```
//!
//! - `@N` is the 1-based start line; leaves with text add `-M`, the end line.
//!   For a paragraph or setext heading that follows link reference
//!   definitions in the same text block, the start line is the first line of
//!   the text block (cmark's sourcepos).
//! - Lists: `bullet` with the bullet character, or `ordered` with the
//!   delimiter (`.` or `)`) and start number; `tight` or `loose`.
//! - Code blocks carry the info string and literal content; HTML blocks
//!   their literal content. A fenced info string whose *raw* text contains
//!   `&` or `\` is written as `<escaped>`: comrak reports it decoded, and
//!   the block pass alone does not decode it.
//!
//! Comparing block structure, rather than HTML, separates block-pass bugs
//! from inline bugs and works before the HTML emitter exists.
//!
//! TODO(M1, block structure): the gv-syntax side. Once `SyntaxTree::blocks()`
//! exists, add `gv_outline(tree, text) -> Vec<String>` producing this format
//! from the CST (resolving setext headings, reference definitions and list
//! tightness), then:
//! 1. assert `gv_outline == comrak_outline` on every CommonMark spec example
//!    (all 652, not only the 296 in block sections);
//! 2. run the same comparison on `random_document` inputs (100k per nightly
//!    run) and minimize mismatches with `minimize`;
//! 3. pin the cases in `tests/fixtures/differential/known-divergences.toml`
//!    whose `decision` is `match-comrak` so they cannot drift.

use comrak::nodes::{AstNode, ListDelimType, ListType, NodeValue};

/// The outline of comrak's block tree for `md` (CommonMark options).
///
/// `raw_infos` holds the raw info strings of the fenced code blocks of the
/// tree being compared, in document order; it decides which info strings are
/// written as `<escaped>`. Pass `&[]` when there is no other tree.
pub fn comrak_outline(md: &str, raw_infos: &[String]) -> Vec<String> {
    let arena = comrak::Arena::new();
    let opts = comrak::Options::default();
    let root = comrak::parse_document(&arena, md, &opts);
    let mut out = Vec::new();
    let mut infos = raw_infos.iter();
    outline_node(root, 0, &mut out, &mut infos);
    out
}

/// The outline line for an info string (shared with the gv-syntax side).
pub fn info_repr(info: &str, raw_has_escapes: bool) -> String {
    if raw_has_escapes {
        String::from("<escaped>")
    } else {
        format!("{info:?}")
    }
}

fn outline_node<'a>(
    n: &'a AstNode<'a>,
    depth: usize,
    out: &mut Vec<String>,
    raw_infos: &mut std::slice::Iter<'_, String>,
) {
    let d = n.data();
    let pad = "  ".repeat(depth);
    let line = d.sourcepos.start.line;
    let (s, recurse) = match &d.value {
        NodeValue::Document => (String::from("document"), true),
        NodeValue::BlockQuote => (format!("block_quote @{line}"), true),
        NodeValue::List(nl) => {
            let ordered = nl.list_type == ListType::Ordered;
            let ch = if ordered {
                if nl.delimiter == ListDelimType::Paren {
                    ')'
                } else {
                    '.'
                }
            } else {
                nl.bullet_char as char
            };
            (
                format!(
                    "list {} {ch} start={} {} @{line}",
                    if ordered { "ordered" } else { "bullet" },
                    if ordered { nl.start } else { 1 },
                    if nl.tight { "tight" } else { "loose" },
                ),
                true,
            )
        }
        NodeValue::Item(_) => (format!("item @{line}"), true),
        NodeValue::Paragraph => (format!("paragraph @{line}-{}", d.sourcepos.end.line), false),
        NodeValue::Heading(h) => (
            format!(
                "heading h{} {} @{line}-{}",
                h.level,
                if h.setext { "setext" } else { "atx" },
                d.sourcepos.end.line
            ),
            false,
        ),
        NodeValue::ThematicBreak => (format!("thematic_break @{line}"), false),
        NodeValue::CodeBlock(cb) => {
            let raw = if cb.fenced {
                raw_infos.next().map_or("", String::as_str)
            } else {
                ""
            };
            let escaped = raw.contains('&') || raw.contains('\\');
            (
                format!(
                    "code_block {} info={} @{line} {:?}",
                    if cb.fenced { "fenced" } else { "indented" },
                    info_repr(&cb.info, escaped),
                    cb.literal
                ),
                false,
            )
        }
        NodeValue::HtmlBlock(h) => (format!("html_block @{line} {:?}", h.literal), false),
        other => (format!("<unexpected {other:?}>"), false),
    };
    out.push(format!("{pad}{s}"));
    if recurse {
        for c in n.children() {
            outline_node(c, depth + 1, out, raw_infos);
        }
    }
}

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
//! [`gv_outline`] produces the same format from a gv-syntax tree (text
//! blocks resolved into definitions and a paragraph or setext heading,
//! lists resolved to tight or loose). `tests/blocks.rs` compares the two on
//! every CommonMark spec example and on random documents.

use comrak::nodes::{AstNode, ListDelimType, ListType, NodeValue};
use gv_syntax::{Container, ContainerKind, Leaf, LeafKind, SyntaxTree, Visitor};
use gv_text::TextSource;

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

/// The outline of a gv-syntax tree of `text`, plus the raw info strings of
/// its fenced code blocks in document order (for [`comrak_outline`]).
pub fn gv_outline<S: TextSource + ?Sized>(
    tree: &SyntaxTree,
    text: &S,
) -> (Vec<String>, Vec<String>) {
    struct V<'s, S: TextSource + ?Sized> {
        text: &'s S,
        depth: usize,
        out: Vec<String>,
        infos: Vec<String>,
    }
    impl<S: TextSource + ?Sized> V<'_, S> {
        fn read(&self, r: std::ops::Range<usize>) -> String {
            let mut b = Vec::new();
            self.text.copy_range(r, &mut b);
            String::from_utf8_lossy(&b).into_owned()
        }
        fn push(&mut self, s: String) {
            self.out.push(format!("{}{s}", "  ".repeat(self.depth)));
        }
    }
    impl<S: TextSource + ?Sized> Visitor for V<'_, S> {
        fn enter(&mut self, c: &Container) {
            let line = c.line + 1;
            let s = match c.kind {
                ContainerKind::BlockQuote => format!("block_quote @{line}"),
                ContainerKind::BulletList { marker, tight } => format!(
                    "list bullet {} start=1 {} @{line}",
                    char::from(marker),
                    if tight { "tight" } else { "loose" }
                ),
                ContainerKind::OrderedList {
                    start,
                    delimiter,
                    tight,
                } => format!(
                    "list ordered {} start={start} {} @{line}",
                    char::from(delimiter),
                    if tight { "tight" } else { "loose" }
                ),
                ContainerKind::Item => format!("item @{line}"),
                ContainerKind::Alert { kind } => format!("alert {} @{line}", kind.name()),
                ContainerKind::FootnoteDefinition => format!("footnote_definition @{line}"),
                ContainerKind::Callout { fold } => format!("callout{} @{line}", fold.name()),
            };
            self.push(s);
            self.depth += 1;
        }
        fn exit(&mut self, _c: &Container) {
            self.depth -= 1;
        }
        fn leaf(&mut self, leaf: &Leaf<'_>) {
            let s = match leaf.kind {
                LeafKind::Paragraph => {
                    format!("paragraph @{}-{}", leaf.open_line + 1, leaf.end_line + 1)
                }
                LeafKind::Heading { level, setext } => {
                    let start = if setext {
                        leaf.open_line
                    } else {
                        leaf.start_line
                    };
                    format!(
                        "heading h{level} {} @{}-{}",
                        if setext { "setext" } else { "atx" },
                        start + 1,
                        leaf.end_line + 1
                    )
                }
                LeafKind::Table => format!("table @{}-{}", leaf.start_line + 1, leaf.end_line + 1),
                LeafKind::MathBlock => {
                    format!("math_block @{}-{}", leaf.start_line + 1, leaf.end_line + 1)
                }
                LeafKind::CalloutTitle => format!("callout_title @{}", leaf.start_line + 1),
                LeafKind::Comment => {
                    format!("comment @{}-{}", leaf.start_line + 1, leaf.end_line + 1)
                }
                LeafKind::ThematicBreak => format!("thematic_break @{}", leaf.open_line + 1),
                LeafKind::FencedCode | LeafKind::IndentedCode => {
                    let fenced = leaf.kind == LeafKind::FencedCode;
                    let raw = leaf.info.clone().map(|r| self.read(r)).unwrap_or_default();
                    if fenced {
                        self.infos.push(raw.clone());
                    }
                    let mut lit = String::new();
                    for l in leaf.lines {
                        lit.push_str(&" ".repeat(usize::from(l.virtual_spaces)));
                        lit.push_str(&self.read(l.content.clone()));
                        lit.push_str(&self.read(l.line_ending.clone()));
                    }
                    if !fenced {
                        lit = trim_trailing_blank_lines(&lit);
                    }
                    let escaped = raw.contains('&') || raw.contains('\\');
                    format!(
                        "code_block {} info={} @{} {lit:?}",
                        if fenced { "fenced" } else { "indented" },
                        if fenced {
                            info_repr(&info_text(&raw), escaped)
                        } else {
                            info_repr("", false)
                        },
                        leaf.open_line + 1
                    )
                }
                LeafKind::Html { .. } => {
                    let mut lit = String::new();
                    for l in leaf.lines {
                        lit.push_str(&" ".repeat(usize::from(l.virtual_spaces)));
                        lit.push_str(&self.read(l.content.clone()));
                        lit.push_str(&self.read(l.line_ending.clone()));
                    }
                    format!("html_block @{} {lit:?}", leaf.open_line + 1)
                }
                // Not part of comrak's outline (front matter is off in the
                // CommonMark profile).
                LeafKind::LinkRefDef | LeafKind::FrontMatter { .. } => return,
            };
            self.push(s);
        }
    }
    let mut v = V {
        text,
        depth: 1,
        out: vec![String::from("document")],
        infos: Vec::new(),
    };
    gv_syntax::walk(tree, &mut v);
    (v.out, v.infos)
}

/// comrak's info string for an info string without escapes: trimmed of
/// ASCII whitespace.
fn info_text(raw: &str) -> String {
    raw.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}'))
        .to_owned()
}

/// comrak's `remove_trailing_blank_lines`, then a final newline.
fn trim_trailing_blank_lines(s: &str) -> String {
    let b = s.as_bytes();
    let Some(last) = b
        .iter()
        .rposition(|&c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
    else {
        return String::from("\n");
    };
    let end = b[last..]
        .iter()
        .position(|&c| c == b'\n' || c == b'\r')
        .map_or(b.len(), |i| last + i);
    format!("{}\n", &s[..end])
}

/// Whether the difference between our outline and comrak's for a
/// minimized input is one of the decided divergences
/// (`tests/fixtures/differential/known-divergences.toml`, decided
/// `match-spec` in M1; each is pinned exactly in `tests/blocks.rs`):
///
/// - `html-type-7-after-lazy-paragraph`: comrak opens an HTML block on a
///   line that continues one of our paragraphs lazily;
/// - `ordered-marker-at-eof`: an ordered marker on the last line without a
///   line ending is our list item;
/// - `refdef-then-dash-underline`: our thematic break after a paragraph of
///   definitions is comrak's paragraph text.
pub fn decided(md: &str, ours: &[String], theirs: &[String]) -> bool {
    let line_of = |s: &str, prefix: &str| -> Option<usize> {
        let rest = s.trim_start().strip_prefix(prefix)?;
        rest.split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    let paragraph_covers = |n: usize| {
        ours.iter().any(|o| {
            o.trim_start()
                .strip_prefix("paragraph @")
                .and_then(|r| r.split_once('-'))
                .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
                .is_some_and(|(a, b)| a <= n && n <= b)
        })
    };
    if theirs
        .iter()
        .filter_map(|t| line_of(t, "html_block @"))
        .any(paragraph_covers)
    {
        return true;
    }
    let last = md.rsplit(['\n', '\r']).next().unwrap_or("").trim();
    if !md.ends_with(['\n', '\r'])
        && last.len() >= 2
        && last[..last.len() - 1].bytes().all(|b| b.is_ascii_digit())
        && last.ends_with(['.', ')'])
        && ours.iter().any(|o| o.contains("list ordered"))
    {
        return true;
    }
    // Not (yet) in the divergences file: comrak strips the info line of a
    // fenced code block as if a lone `\r` ending it and the `\n` of the next
    // line were one CRLF (cmark normalizes line endings first and agrees
    // with us).
    if md.contains('\r')
        && ours.len() == theirs.len()
        && ours.iter().zip(theirs).any(|(o, t)| o != t)
        && ours.iter().zip(theirs).all(|(o, t)| {
            o == t || (o.contains("code_block fenced") && o.replacen("\"\\n", "\"", 1) == *t)
        })
    {
        return true;
    }
    md.contains("]:")
        && ours.iter().any(|o| o.contains("thematic_break"))
        && theirs
            .iter()
            .filter(|t| t.contains("thematic_break"))
            .count()
            < ours.iter().filter(|o| o.contains("thematic_break")).count()
}

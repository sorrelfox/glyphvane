// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The deterministic S-expression dump of a CST (`glyphvane render
//! --format cst`, snapshot tests, the incremental-equals-full fuzz target).
//!
//! # Format
//!
//! One node per line, children indented by two spaces, the closing `)` of
//! a node appended to its last line (Lisp style). Byte ranges are
//! `start..end` in the document. BlockIds are left out, so two parses of
//! the same text dump identically iff their structure is identical.
//!
//! ```text
//! (document 0..14
//!   (bullet-list "-" tight 0..14
//!     (item 0..14
//!       (list-marker 0..2 "- ")
//!       (text paragraph 2..14
//!         (content 2..5 "foo")
//!         (line-ending 5..6 "\n")
//!         (prefix 6..8 "  ")
//!         (content 8..11 "bar")
//!         (line-ending 11..12 "\n")
//!         (inlines
//!           ...)))))
//! ```
//!
//! - **Blocks** are written as `(kind attributes start..end` with these
//!   kinds and attributes: `block-quote`; `bullet-list "-" tight|loose`;
//!   `ordered-list START "." tight|loose`; `item`; `text` followed by what
//!   the text block resolves to (`paragraph`, `heading N`, or `empty` when
//!   it holds only link reference definitions); `atx-heading N`;
//!   `thematic-break`; `fenced-code`; `indented-code`; `html N` (the start
//!   condition).
//! - **Tokens** are `(kind start..end "bytes")`, with `+N` before the bytes
//!   when the token has N virtual spaces (a partially consumed tab). Kinds:
//!   `bom`, `ws`, `prefix` (container prefixes of the line), `quote-marker`,
//!   `list-marker`, `code-indent`, `marker`, `info`, `content`,
//!   `line-ending`. Bytes are written as a Rust string literal (`{:?}`).
//! - **Markers** are `(refdef N)` (the next N content lines of the text
//!   block are one link reference definition), `(setext N)` and `(loosen)`.
//! - **Inlines**: the paragraph or heading of a text block, and an ATX
//!   heading, end with `(inlines` and the inline module's dump of their
//!   content, one S-expression per line.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;

use gv_text::TextSource;

use crate::inline::api::{InlineInput, cst_dump_inlines, parse_inlines};
use crate::tree::SyntaxTree;
use crate::tree::event::{BlockId, BlockKind, Event, TokenKind};
use crate::tree::walk::{self, ContainerKind, LeafKind};

/// Resolution of one block, from the walker.
#[derive(Default)]
struct Resolved {
    /// Lists: tight.
    tight: bool,
    /// Text blocks: `paragraph`, `heading N` or `empty`.
    text: Option<String>,
    /// Inline segments of the paragraph or heading part.
    segments: Option<Vec<Range<usize>>>,
}

/// Writes the CST dump of `tree` (parsed from `text`).
pub fn cst_dump<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S) -> String {
    // Resolutions by BlockId.
    let mut res: HashMap<BlockId, Resolved> = HashMap::new();
    struct V<'a>(&'a mut HashMap<BlockId, Resolved>);
    impl walk::Visitor for V<'_> {
        fn enter(&mut self, c: &walk::Container) {
            let tight = matches!(
                c.kind,
                ContainerKind::BulletList { tight: true, .. }
                    | ContainerKind::OrderedList { tight: true, .. }
            );
            self.0.entry(c.id).or_default().tight = tight;
        }
        fn leaf(&mut self, leaf: &walk::Leaf<'_>) {
            let r = self.0.entry(leaf.id).or_default();
            match leaf.kind {
                LeafKind::Paragraph => r.text = Some(String::from("paragraph")),
                LeafKind::Heading { level, .. } => r.text = Some(format!("heading {level}")),
                LeafKind::LinkRefDef => {
                    if r.text.is_none() {
                        r.text = Some(String::from("empty"));
                    }
                    return;
                }
                _ => return,
            }
            r.segments = Some(leaf.segments());
        }
    }
    walk::walk(tree, &mut V(&mut res));
    let refs = tree.ref_map(text);

    let events = tree.events();
    // End offset of every block, by the index of its `Open` event.
    let mut ends = vec![0usize; events.len()];
    let mut open: Vec<usize> = Vec::new();
    for (i, (off, e)) in events.iter().enumerate() {
        match e {
            Event::Open { .. } => open.push(i),
            Event::Close => {
                if let Some(j) = open.pop() {
                    ends[j] = *off;
                }
            }
            _ => {}
        }
    }

    let mut out = format!("(document 0..{}\n", tree.len());
    let mut stack: Vec<BlockId> = Vec::new();
    let mut bytes = Vec::new();
    for (i, (off, e)) in events.iter().enumerate() {
        let indent = "  ".repeat(stack.len() + 1);
        match *e {
            Event::Open { id, kind } => {
                let r = res.get(&id);
                let tight = r.is_some_and(|r| r.tight);
                let lt = if tight { "tight" } else { "loose" };
                let head = match kind {
                    BlockKind::BlockQuote => String::from("block-quote"),
                    BlockKind::BulletList { marker } => {
                        format!("bullet-list {:?} {lt}", char::from(marker))
                    }
                    BlockKind::OrderedList { start, delimiter } => {
                        format!("ordered-list {start} {:?} {lt}", char::from(delimiter))
                    }
                    BlockKind::Item => String::from("item"),
                    BlockKind::Text => format!(
                        "text {}",
                        r.and_then(|r| r.text.as_deref()).unwrap_or("empty")
                    ),
                    BlockKind::AtxHeading { level } => format!("atx-heading {level}"),
                    BlockKind::ThematicBreak => String::from("thematic-break"),
                    BlockKind::FencedCode => String::from("fenced-code"),
                    BlockKind::IndentedCode => String::from("indented-code"),
                    BlockKind::Html { kind } => format!("html {kind}"),
                };
                let _ = writeln!(out, "{indent}({head} {off}..{}", ends[i]);
                stack.push(id);
            }
            Event::Close => {
                if let Some(id) = stack.pop()
                    && let Some(segments) = res.get(&id).and_then(|r| r.segments.as_ref())
                {
                    let tree = parse_inlines(text, InlineInput { segments }, &*refs);
                    let mut s = String::new();
                    cst_dump_inlines(&tree, &mut s);
                    let _ = writeln!(out, "{indent}(inlines");
                    for line in s.lines() {
                        let _ = writeln!(out, "{indent}  {line}");
                    }
                    close_last_line(&mut out);
                }
                close_last_line(&mut out);
            }
            Event::Token(t) => {
                bytes.clear();
                text.copy_range(*off..*off + t.len as usize, &mut bytes);
                let virt = if t.virtual_spaces > 0 {
                    format!(" +{}", t.virtual_spaces)
                } else {
                    String::new()
                };
                let _ = writeln!(
                    out,
                    "{indent}({} {off}..{}{virt} {:?})",
                    token_name(t.kind),
                    *off + t.len as usize,
                    String::from_utf8_lossy(&bytes)
                );
            }
            Event::RefDef { lines } => {
                let _ = writeln!(out, "{indent}(refdef {lines})");
            }
            Event::Setext { level } => {
                let _ = writeln!(out, "{indent}(setext {level})");
            }
            Event::Loosen => {
                let _ = writeln!(out, "{indent}(loosen)");
            }
        }
    }
    close_last_line(&mut out);
    out
}

fn token_name(k: TokenKind) -> &'static str {
    k.name()
}

/// Appends `)` to the last line.
fn close_last_line(out: &mut String) {
    if out.ends_with('\n') {
        out.pop();
    }
    out.push_str(")\n");
}

/// The concatenated bytes of every token of `tree`, in order. The CST is
/// lossless iff this equals the text.
pub fn cst_text<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S) -> Vec<u8> {
    let mut out = Vec::with_capacity(tree.len());
    for (off, e) in tree.events() {
        if let Event::Token(t) = e {
            text.copy_range(off..off + t.len as usize, &mut out);
        }
    }
    out
}

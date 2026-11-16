// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Benchmark documents and helpers shared by the wall-clock (`parse`) and
//! instruction-count (`icount`) benchmarks, so both measure the same inputs.
//! Everything is generated deterministically from the in-repo GFM spec
//! fixture or a fixed-seed generator: nothing is downloaded.

use std::hint::black_box;

use gv_syntax::SyntaxTree;

pub fn spec_text() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/spec/gfm-0.29.0.gfm.13.txt"
    );
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `unit` repeated until the result is at least `bytes` long.
pub fn repeat_to(unit: &str, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + unit.len());
    while s.len() < bytes {
        s.push_str(unit);
        if !s.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

/// One huge, tight, nested list of at least `bytes` bytes.
pub fn outliner(bytes: usize) -> String {
    let words = [
        "parser",
        "checkpoint",
        "milestone",
        "review",
        "latency",
        "budget",
        "notes",
        "design",
        "rope",
        "editor",
        "render",
        "tight",
        "list",
        "block",
        "quote",
        "fence",
        "table",
        "vault",
        "link",
        "index",
    ];
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut s = String::with_capacity(bytes + 200);
    let mut depth = 0usize;
    while s.len() < bytes {
        let r = next();
        depth = match r % 4 {
            0 => 0,
            1 => depth.saturating_sub(1),
            _ => (depth + 1).min(4),
        };
        s.push_str(&"  ".repeat(depth));
        s.push_str(if r % 7 == 0 { "- [ ] " } else { "- " });
        let n = 3 + (r >> 8) as usize % 8;
        for i in 0..n {
            if i > 0 {
                s.push(' ');
            }
            s.push_str(words[(r >> (12 + i * 3)) as usize % words.len()]);
        }
        if r % 5 == 0 {
            s.push_str(" [link](https://example.com/x) and *emphasis*");
        }
        s.push('\n');
    }
    s
}

/// The end of a text line (paragraph or list item text, not code) near
/// `at`: where a user typing would be.
pub fn text_line_end(text: &str, mut at: usize) -> usize {
    loop {
        let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
        let line = &text[line_start..line_end];
        let trimmed = line.trim_start_matches([' ', '-', '*']);
        if line.len() > 20
            && !line.starts_with("    ")
            && trimmed.chars().next().is_some_and(char::is_alphabetic)
            && line
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '.')
        {
            return line_end;
        }
        at = (line_end + 1).min(text.len() - 1);
    }
}

pub fn full_inlines(tree: &SyntaxTree, text: &str) {
    let refs = tree.ref_map(text);
    gv_syntax::walk(
        tree,
        &mut gv_syntax::tree::walk::LeafFn(|leaf: &gv_syntax::Leaf<'_>| {
            if matches!(
                leaf.kind,
                gv_syntax::LeafKind::Paragraph | gv_syntax::LeafKind::Heading { .. }
            ) {
                let segments = leaf.segments();
                black_box(gv_syntax::inline::api::parse_inlines(
                    text,
                    gv_syntax::inline::api::InlineInput {
                        segments: &segments,
                    },
                    &*refs,
                ));
            }
        }),
    );
}

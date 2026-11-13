// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Inline parser throughput on paragraph-heavy documents. Ignored by
//! default (timings in a debug build mean little); run in release:
//!
//! ```sh
//! cargo test --release -p gv-syntax --test inline_throughput -- --ignored --nocapture
//! ```
//!
//! Documents: generated prose with a realistic inline mix (emphasis, code,
//! links, entities, autolinks, line breaks) and the CommonMark spec
//! examples concatenated (a dense mix of every inline construct). Each is
//! about 8 MB, split into paragraphs at blank lines; every line is one
//! segment. Reported: parse only, and parse plus HTML rendering.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use gv_syntax::inline::{InlineInput, RefDef, parse_inlines, render_inlines_html};

/// Document size; `GV_INLINE_BENCH_BYTES` overrides it.
fn target_bytes() -> usize {
    std::env::var("GV_INLINE_BENCH_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8 << 20)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const WORDS: &[&str] = &[
    "the",
    "parser",
    "reads",
    "each",
    "line",
    "of",
    "text",
    "and",
    "builds",
    "a",
    "tree",
    "with",
    "every",
    "byte",
    "kept",
    "in",
    "order",
    "so",
    "that",
    "editing",
    "stays",
    "exact",
    "while",
    "rendering",
    "remains",
    "fast",
    "for",
    "large",
    "documents",
    "über",
    "naïve",
    "café",
];

fn prose(rng: &mut Rng) -> String {
    let target = target_bytes();
    let mut doc = String::with_capacity(target + 4096);
    while doc.len() < target {
        let lines = 1 + rng.below(6);
        for _ in 0..lines {
            let words = 6 + rng.below(10);
            for w in 0..words {
                if w > 0 {
                    doc.push(' ');
                }
                let word = WORDS[rng.below(WORDS.len())];
                match rng.below(40) {
                    0 => {
                        doc.push('*');
                        doc.push_str(word);
                        doc.push('*');
                    }
                    1 => {
                        doc.push_str("**");
                        doc.push_str(word);
                        doc.push_str("**");
                    }
                    2 => {
                        doc.push('`');
                        doc.push_str(word);
                        doc.push_str("()`");
                    }
                    3 => {
                        doc.push('[');
                        doc.push_str(word);
                        doc.push_str("](https://example.com/");
                        doc.push_str(word);
                        doc.push(')');
                    }
                    4 => doc.push_str("&amp;"),
                    5 => {
                        doc.push_str(word);
                        doc.push_str("_name");
                    }
                    6 => doc.push_str("<https://example.org>"),
                    7 => {
                        doc.push('[');
                        doc.push_str(word);
                        doc.push(']');
                    }
                    _ => doc.push_str(word),
                }
            }
            doc.push_str(if rng.below(10) == 0 { "  \n" } else { "\n" });
        }
        doc.push('\n');
    }
    doc
}

fn spec_examples() -> String {
    let path = std::env::var_os("GV_SPEC_JSON").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/spec/commonmark-0.31.2.json")
        },
        PathBuf::from,
    );
    let text = std::fs::read_to_string(path).expect("spec fixture");
    let json: serde_json::Value = serde_json::from_str(&text).expect("spec JSON");
    let mut one = String::new();
    for e in json.as_array().expect("array") {
        one.push_str(e["markdown"].as_str().unwrap_or_default());
        one.push('\n');
    }
    let target = target_bytes();
    let mut doc = String::with_capacity(target + one.len());
    while doc.len() < target {
        doc.push_str(&one);
    }
    doc
}

/// Paragraphs (runs of non-blank lines) as segment lists, each line with
/// leading spaces and tabs stripped and its line ending excluded.
fn paragraphs(doc: &str) -> (Vec<Vec<Range<usize>>>, usize) {
    let mut paras = Vec::new();
    let mut cur = Vec::new();
    let mut bytes = 0;
    let mut start = 0;
    for line in doc.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start_matches([' ', '\t']);
        let s = start + (content.len() - trimmed.len());
        if trimmed.trim().is_empty() {
            if !cur.is_empty() {
                paras.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(s..start + content.len());
            bytes += content.len() - (content.len() - trimmed.len());
        }
        start += line.len();
    }
    if !cur.is_empty() {
        paras.push(cur);
    }
    (paras, bytes)
}

fn measure(name: &str, doc: &str) {
    let mut refs: HashMap<String, RefDef> = HashMap::new();
    refs.insert(
        "foo".into(),
        RefDef {
            destination: "/url".into(),
            title: Some("title".into()),
        },
    );
    let (paras, bytes) = paragraphs(doc);
    let mb = bytes as f64 / (1024.0 * 1024.0);
    let mut best_parse = f64::MAX;
    let mut best_render = f64::MAX;
    let mut nodes = 0usize;
    let mut html = String::with_capacity(doc.len() * 2);
    for _ in 0..5 {
        let t = Instant::now();
        nodes = 0;
        for p in &paras {
            let tree = parse_inlines(doc, InlineInput { segments: p }, &refs);
            nodes += tree.nodes().count();
            std::hint::black_box(&tree);
        }
        best_parse = best_parse.min(t.elapsed().as_secs_f64());
        html.clear();
        let t = Instant::now();
        for p in &paras {
            let tree = parse_inlines(doc, InlineInput { segments: p }, &refs);
            render_inlines_html(&tree, &mut html);
        }
        best_render = best_render.min(t.elapsed().as_secs_f64());
    }
    println!(
        "{name:28} {mb:6.2} MiB in {:6} paragraphs, {:8} nodes: parse {:7.1} MiB/s ({:6.2} ms), parse+render {:7.1} MiB/s",
        paras.len(),
        nodes,
        mb / best_parse,
        best_parse * 1e3,
        mb / best_render
    );
}

#[test]
#[ignore = "benchmark: run in release with --ignored --nocapture"]
fn inline_throughput() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    measure("generated prose", &prose(&mut rng));
    measure("spec examples (dense mix)", &spec_examples());
}

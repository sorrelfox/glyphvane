// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests: incremental reparse == full parse, for random documents
//! built from Markdown fragments and random edits, including long edit
//! traces. Every step is checked against a full parse (lines, checkpoints,
//! events), for losslessness and for the BlockId rules. Variations: a
//! chunked text source with 1-31-byte chunks, reparses paused after every
//! slice and resumed (with the interim tree validated), multi-edit
//! transactions through `gv_text::Buffer`.
//!
//! Case counts: `GV_PROP_SINGLE` (default 2000), `GV_PROP_TRACES` (default
//! 100); run with larger values in release mode for a deep check.

mod support;

use gv_syntax::{Budget, Reparse, SyntaxTree};
use proptest::prelude::*;
use proptest::sample::{Index, select};
use support::edits::{Mode, apply, check_edit};

fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Markdown building blocks, weighted toward the constructs whose
/// incremental behavior is subtle (lazy continuation, setext, reference
/// definitions, list tightness, fences, HTML blocks, tabs, CR/CRLF, BOM).
const FRAGMENTS: &[&str] = &[
    "Hello world.\n",
    "lazy continuation\n",
    "  indented para\n",
    "foo *bar*\n",
    "text",
    "\n",
    "\n\n",
    "   \n",
    "\t\n",
    "# Heading\n",
    "## Heading ##\n",
    "####### not\n",
    "#\n",
    "Setext\n===\n",
    "---\n",
    "***\n",
    "_ _ _\n",
    "===\n",
    "- - -\n",
    "--\n",
    "```\n",
    "```rust\n",
    "~~~\n",
    "````\n",
    "```\ncode\n```\n",
    "  ```\n  x\n  ```\n",
    "``` a`b\n",
    "    indented code\n",
    "\tcode tab\n",
    "      deeper\n",
    "> quote\n",
    ">quote\n",
    "> > nested\n",
    ">\n",
    "> - in quote\n",
    ">\tcode in quote\n",
    "> ```\n",
    "- item\n",
    "* item\n",
    "+ item\n",
    "-\n",
    "- \n",
    "  - nested\n",
    "    - deep\n",
    "-    five spaces\n",
    "- [ ] task\n",
    "-\tfoo\n",
    " -  bar\n",
    "- a\n\n- b\n",
    "- a\n  b\n\n  c\n",
    "1. one\n",
    "2) two\n",
    "10. ten\n",
    "1.\n",
    "  1. nested\n",
    "3.  three\n",
    "<div>\n",
    "</div>\n",
    "<!-- comment\n",
    "-->\n",
    "<pre>\n",
    "</pre>\n",
    "<?php\n",
    "?>\n",
    "<!DOCTYPE x>\n",
    "<![CDATA[\n",
    "]]>\n",
    "<a href=\"x\">\n",
    "<custom-tag>\n",
    "<script>\n",
    "</script>\n",
    "[foo]: /url\n",
    "[foo]: /url \"title\"\n",
    "[bar]:\n",
    "/dest\n",
    "\"title\"\n",
    "'t'\n",
    "[a]: <b> 'c'\n",
    "[x\n",
    "y]: /z\n",
    "[a]: /u \"ti\n",
    "tle\"\n",
    "[link](/x) text\n",
    "line with trailing  \n",
    "crlf line\r\n",
    "cr line\r",
    "tab\tinside\n",
    "\u{fc}n\u{ef}c\u{f6}d\u{e9} \u{2713}\n",
    "\u{feff}bom\n",
];

/// Strings typed or pasted by edits.
const TYPED: &[&str] = &[
    "x", " ", "\n", "\r", "\r\n", "\t", ">", "-", "*", "+", "#", "`", "```", "~~~", "1.", "2)",
    "[", "]", ":", "=", "<", "<div>", "    ", "> ", "- ", "\u{e9}", "\n\n", "]: /u", "-->", "\"",
];

fn fragment() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => select(FRAGMENTS).prop_map(String::from),
        1 => "[a-z#>*`~ \\t\\n\\r0-9.<\\[\\]:=_)\"-]{0,10}",
    ]
}

fn document(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(fragment(), 0..max).prop_map(|v| v.concat())
}

#[derive(Clone, Debug)]
enum Op {
    /// Replace `del` chars at a random position with the string.
    Replace(Index, usize, String),
    /// Type the string right after the previous edit.
    TypeNext(String),
    /// Delete the char before the previous edit's end.
    Backspace,
}

fn op() -> impl Strategy<Value = Op> {
    let ins = prop_oneof![
        3 => select(TYPED).prop_map(String::from),
        2 => fragment(),
        1 => Just(String::new()),
    ];
    prop_oneof![
        4 => (any::<Index>(), prop_oneof![3 => Just(0usize), 2 => 1usize..4, 1 => 4usize..40], ins)
            .prop_map(|(i, d, s)| Op::Replace(i, d, s)),
        3 => select(TYPED).prop_map(|s| Op::TypeNext(s.to_owned())),
        1 => Just(Op::Backspace),
    ]
}

fn snap_down(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn snap_up(s: &str, mut i: usize) -> usize {
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// The concrete `(start, end, insert)` of an op against `text`; `cursor` is
/// where the previous edit ended.
fn concretize(text: &str, cursor: usize, op: &Op) -> (usize, usize, String) {
    match op {
        Op::Replace(i, d, s) => {
            let start = snap_down(text, i.index(text.len() + 1));
            let end = snap_up(text, (start + d).min(text.len()));
            (start, end, s.clone())
        }
        Op::TypeNext(s) => {
            let c = snap_down(text, cursor.min(text.len()));
            (c, c, s.clone())
        }
        Op::Backspace => {
            let c = snap_down(text, cursor.min(text.len()));
            if c == 0 {
                (0, 0, String::new())
            } else {
                (snap_down(text, c - 1), c, String::new())
            }
        }
    }
}

static CHECKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_trace(initial: &str, ops: &[Op], mode_seed: usize) -> Result<(), TestCaseError> {
    use std::sync::atomic::Ordering::Relaxed;
    let n = CHECKS.fetch_add(ops.len(), Relaxed) + ops.len();
    let b = BYTES.fetch_add(initial.len(), Relaxed) + initial.len();
    if n / 50_000 != (n - ops.len()) / 50_000 {
        eprintln!("incremental checks so far: {n} (initial documents: {b} bytes in total)");
    }
    let mut text = initial.to_owned();
    let mut tree = SyntaxTree::parse(text.as_str());
    tree.validate(text.as_str()).map_err(TestCaseError::fail)?;
    let mut cursor = 0;
    for (step, op) in ops.iter().enumerate() {
        let (s, e, ins) = concretize(&text, cursor, op);
        let (new_text, change) = apply(&text, s, e, &ins);
        let mode = match (mode_seed + step) % 3 {
            0 => Mode::Direct,
            k => Mode::Sliced(1 + (mode_seed * 7 + step * k) % 31),
        };
        let (new, _) = check_edit(&tree, &text, &new_text, change, mode)
            .map_err(|m| TestCaseError::fail(format!("step {step}: {m}")))?;
        tree = new;
        cursor = s + ins.len();
        text = new_text;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_SINGLE", 2000), ..ProptestConfig::default() })]

    /// One random edit on a random document.
    #[test]
    fn single_edit(doc in document(40), op in op(), seed in 0usize..1000) {
        run_trace(&doc, &[op], seed)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_TRACES", 100), ..ProptestConfig::default() })]

    /// Traces of 100-200 consecutive edits, each checked against a full
    /// parse.
    #[test]
    fn edit_trace(doc in document(60), ops in prop::collection::vec(op(), 100..200), seed in 0usize..1000) {
        run_trace(&doc, &ops, seed)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_TRACES", 100), ..ProptestConfig::default() })]

    /// Multi-edit transactions (sorted, non-overlapping replacements)
    /// through `gv_text::Buffer` and `SyntaxTree::reparse`.
    #[test]
    fn transactions(doc in document(40), edits in prop::collection::vec((any::<Index>(), 0usize..6, select(TYPED)), 1..5)) {
        let mut buffer = gv_text::Buffer::new(&doc);
        let tree = SyntaxTree::parse(buffer.text());
        let len = doc.len();
        let mut ranges: Vec<(usize, usize, &str)> = edits
            .iter()
            .map(|(i, d, s)| {
                let start = snap_down(&doc, i.index(len + 1));
                (start, snap_up(&doc, (start + d).min(len)), *s)
            })
            .collect();
        ranges.sort_unstable();
        let mut tx = gv_text::Transaction::new();
        let mut last_end = 0;
        for (s, e, ins) in ranges {
            if s < last_end {
                continue;
            }
            tx.push(s..e, ins);
            last_end = e;
        }
        let summary = buffer.apply(&tx).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let snapshot = buffer.snapshot();
        let new = match tree.reparse(&snapshot, &summary, Budget::Unlimited) {
            Reparse::Done { tree, .. } => tree,
            Reparse::Partial { .. } => return Err(TestCaseError::fail("partial with unlimited budget")),
        };
        let new_text = snapshot.to_string();
        let full = SyntaxTree::parse(new_text.as_str());
        new.same_structure(&full).map_err(TestCaseError::fail)?;
        new.validate(new_text.as_str()).map_err(TestCaseError::fail)?;
    }
}

/// A long deterministic trace over a larger document, reporting how much
/// each reparse scanned.
#[test]
fn long_trace() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, RngSeed, TestRunner};
    let mut runner = TestRunner::new(Config {
        rng_seed: RngSeed::Fixed(7),
        ..Config::default()
    });
    let (mut steps, mut converged, mut lines, mut doc_lines) = (0usize, 0usize, 0usize, 0usize);
    for _ in 0..2 {
        let initial = document(1500).new_tree(&mut runner).expect("doc").current();
        let ops = prop::collection::vec(op(), 300..301)
            .new_tree(&mut runner)
            .expect("ops")
            .current();
        let mut text = initial.clone();
        let mut tree = SyntaxTree::parse(text.as_str());
        let mut cursor = 0;
        for (i, op) in ops.iter().enumerate() {
            let (s, e, ins) = concretize(&text, cursor, op);
            let (new_text, change) = apply(&text, s, e, &ins);
            let (new, st) = if i % 10 == 0 {
                check_edit(&tree, &text, &new_text, change, Mode::Direct)
                    .expect("incremental == full")
            } else {
                tree.reparse_full(new_text.as_str(), change)
            };
            steps += 1;
            converged += usize::from(st.converged);
            lines += st.lines_parsed;
            doc_lines += new.line_count();
            tree = new;
            cursor = s + ins.len();
            text = new_text;
        }
        let full = SyntaxTree::parse(text.as_str());
        tree.same_structure(&full)
            .expect("final tree == full parse");
    }
    println!(
        "long traces: {steps} edits, {converged} converged before the end, {:.1} lines scanned per edit (documents of {:.0} lines)",
        lines as f64 / steps as f64,
        doc_lines as f64 / steps as f64
    );
    assert!(converged * 10 > steps * 8, "{converged}/{steps} converged");
}

/// Two reparses of the same tree (as when a background task is abandoned
/// for a newer edit) do not interfere: trees are immutable snapshots.
#[test]
fn snapshots_are_independent() {
    let text = "# A\n\n- one\n- two\n\npara\n";
    let tree = SyntaxTree::parse(text);
    let (t1, c1) = apply(text, 2, 2, "x");
    let (t2, c2) = apply(text, 0, 0, "```\n");
    let a = tree.reparse_full(t1.as_str(), c1).0;
    let b = tree.reparse_full(t2.as_str(), c2).0;
    a.same_structure(&SyntaxTree::parse(t1.as_str()))
        .expect("a");
    b.same_structure(&SyntaxTree::parse(t2.as_str()))
        .expect("b");
    tree.same_structure(&SyntaxTree::parse(text))
        .expect("old tree unchanged");
    std::thread::scope(|s| {
        s.spawn(|| {
            let c = tree.reparse_full(t1.as_str(), c1).0;
            c.same_structure(&a).expect("same on another thread");
        });
    });
}

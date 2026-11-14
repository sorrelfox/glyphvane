// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests: incremental reparse == full parse, for random documents
//! built from Markdown fragments and random edits, including long edit
//! traces. Every step is checked against a full parse (lines, checkpoints,
//! events), for losslessness and for the BlockId rules. Variations: a
//! chunked text source with 1-31-byte chunks, reparses paused after every
//! slice and resumed (with the interim tree validated), multi-edit
//! transactions through `gv_text::Buffer`, front matter enabled (documents
//! that start with front matter or something close to it).
//!
//! Also randomized here, on the same documents and edits: range walks
//! equal the full walk restricted to the range (on fresh, reparsed and
//! interim trees), and composed reparse reports describe the composed
//! edit.
//!
//! Case counts: `GV_PROP_SINGLE` (default 2000), `GV_PROP_TRACES` (default
//! 100); run with larger values in release mode for a deep check.

mod support;

use gv_syntax::{Budget, Changes, ParseOptions, Reparse, SyntaxTree};
use proptest::prelude::*;
use proptest::sample::{Index, select};
use support::edits::{Mode, apply, check_edit};
use support::views::{filtered_walk, record_walk_range};

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
    "...\n",
    "+++\n",
    "title: x\n",
];

/// Starts of documents with front matter enabled: front matter, openers
/// without a closing line, and near misses.
const FRONT_MATTER: &[&str] = &[
    "",
    "---\n",
    "---\ntitle: x\n",
    "---\ntitle: x\n---\n",
    "---\n---\n",
    "---\na: b\n...\n",
    "--- \r\na: 1\r\n---\r\n",
    "+++\n",
    "+++\na = 1\n+++\n",
    "\u{feff}---\na\n---\n",
    "----\n",
    " ---\n",
    "--\n",
];

/// Strings typed or pasted by edits.
const TYPED: &[&str] = &[
    "x", " ", "\n", "\r", "\r\n", "\t", ">", "-", "*", "+", "#", "`", "```", "~~~", "1.", "2)",
    "[", "]", ":", "=", "<", "<div>", "    ", "> ", "- ", "\u{e9}", "\n\n", "]: /u", "-->", "\"",
    "---", "\n---\n", "...", "+++\n",
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

/// A document for front matter tests.
fn fm_document(max: usize) -> impl Strategy<Value = String> {
    (select(FRONT_MATTER), document(max)).prop_map(|(start, rest)| format!("{start}{rest}"))
}

/// The options of the `extended` profile (YAML and TOML front matter).
fn extended() -> ParseOptions {
    ParseOptions::for_profile("extended").expect("built-in profile")
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
    run_trace_with(initial, ops, mode_seed, ParseOptions::default())
}

fn run_trace_with(
    initial: &str,
    ops: &[Op],
    mode_seed: usize,
    options: ParseOptions,
) -> Result<(), TestCaseError> {
    use std::sync::atomic::Ordering::Relaxed;
    let n = CHECKS.fetch_add(ops.len(), Relaxed) + ops.len();
    let b = BYTES.fetch_add(initial.len(), Relaxed) + initial.len();
    if n / 50_000 != (n - ops.len()) / 50_000 {
        eprintln!("incremental checks so far: {n} (initial documents: {b} bytes in total)");
    }
    let mut text = initial.to_owned();
    let mut tree = SyntaxTree::parse_with(text.as_str(), options);
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
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_SINGLE", 2000), ..ProptestConfig::default() })]

    /// One random edit on a random document, front matter enabled.
    #[test]
    fn front_matter_single_edit(doc in fm_document(20), op in op(), seed in 0usize..1000) {
        run_trace_with(&doc, &[op], seed, extended())?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_TRACES", 100), ..ProptestConfig::default() })]

    /// Traces of consecutive edits with front matter enabled.
    #[test]
    fn front_matter_trace(doc in fm_document(30), ops in prop::collection::vec(op(), 50..120), seed in 0usize..1000) {
        run_trace_with(&doc, &ops, seed, extended())?;
    }
}

/// The start offset of every line of a tree of `text`, including the
/// zero-length line at the end.
fn line_starts(text: &str) -> Vec<usize> {
    let b = text.as_bytes();
    let mut out = vec![0];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => {
                i += 2;
                out.push(i);
            }
            b'\r' | b'\n' => {
                i += 1;
                out.push(i);
            }
            _ => i += 1,
        }
    }
    if out.last() != Some(&b.len()) {
        out.push(b.len());
    }
    out
}

/// Checks that `c` describes the change from `(t0, s0)` to `(t1, s1)`:
/// outside its window the texts, the line structure and the blocks agree.
fn check_window(
    t0: &SyntaxTree,
    s0: &str,
    t1: &SyntaxTree,
    s1: &str,
    c: &Changes,
) -> Result<(), String> {
    let (l0, l1) = (line_starts(s0), line_starts(s1));
    if l0.len() != t0.line_count() || l1.len() != t1.line_count() {
        return Err(String::from("line model mismatch"));
    }
    let (o, n) = (&c.old_range, &c.new_range);
    let ok = o.start == n.start
        && o.start <= o.end
        && n.start <= n.end
        && c.restart_line <= c.old_end_line
        && c.restart_line <= c.new_end_line
        && s0.get(..o.start) == s1.get(..n.start)
        && s0.get(o.end..) == s1.get(n.end..)
        && l0.len() - c.old_end_line == l1.len() - c.new_end_line
        && l0[c.restart_line] == o.start
        && l1[c.restart_line] == n.start
        && l0.get(c.old_end_line).is_none_or(|&x| x == o.end)
        && l1.get(c.new_end_line).is_none_or(|&x| x == n.end)
        && (c.old_end_line < l0.len() || o.end == s0.len())
        && (c.new_end_line < l1.len() || n.end == s1.len());
    if !ok {
        return Err(format!("window does not fit: {c:?}"));
    }
    let delta = s1.len() as isize - s0.len() as isize;
    let old: std::collections::HashMap<_, _> = t0.blocks().into_iter().map(|b| (b.id, b)).collect();
    for b in t1.blocks() {
        let shift = if b.range.end < n.start {
            0
        } else if b.range.start >= n.end {
            delta
        } else {
            continue;
        };
        let expected =
            (b.range.start as isize - shift) as usize..(b.range.end as isize - shift) as usize;
        match old.get(&b.id) {
            Some(a) if a.kind == b.kind && a.depth == b.depth && a.range == expected => {}
            other => {
                return Err(format!(
                    "block {b:?} outside the window has no twin: {other:?}\n{c:?}"
                ));
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_TRACES", 100) * 3, ..ProptestConfig::default() })]

    /// The reports of consecutive reparses compose into one report of the
    /// whole sequence, associatively.
    #[test]
    fn composed_changes(
        doc in fm_document(30),
        ops in prop::collection::vec(op(), 1..12),
        fm in any::<bool>(),
        split in any::<(Index, Index)>(),
    ) {
        let options = if fm { extended() } else { ParseOptions::default() };
        let mut text = doc.clone();
        let t0 = SyntaxTree::parse_with(text.as_str(), options);
        let mut tree = t0.clone();
        let mut all = Vec::new();
        let mut cursor = 0;
        for op in &ops {
            let (s, e, ins) = concretize(&text, cursor, op);
            let (new_text, change) = apply(&text, s, e, &ins);
            let (new, changes) = tree.reparse_full(new_text.as_str(), change);
            check_window(&tree, &text, &new, &new_text, &changes).map_err(TestCaseError::fail)?;
            all.push(changes);
            tree = new;
            cursor = s + ins.len();
            text = new_text;
        }
        let c = Changes::compose_all(&all);
        check_window(&t0, &doc, &tree, &text, &c)
            .map_err(|e| TestCaseError::fail(format!("{e}\nparts: {all:?}")))?;
        // Associativity on a three-way split.
        let (mut i, mut j) = (split.0.index(all.len() + 1), split.1.index(all.len() + 1));
        if i > j {
            std::mem::swap(&mut i, &mut j);
        }
        let (a, b, d) = (
            Changes::compose_all(&all[..i]),
            Changes::compose_all(&all[i..j]),
            Changes::compose_all(&all[j..]),
        );
        prop_assert_eq!(a.compose(&b).compose(&d), a.compose(&b.compose(&d)));
        prop_assert_eq!(a.compose(&b).compose(&d), c);
    }
}

/// Random ranges of `text`: empty, one byte, a few lines, all of it.
fn ranges(text: &str, picks: &[(Index, usize)]) -> Vec<std::ops::Range<usize>> {
    let n = text.len();
    let mut out = vec![0..n, n..n];
    for (i, len) in picks {
        let start = i.index(n + 1);
        out.push(start..(start + len).min(n));
    }
    out
}

fn check_range_walks(
    tree: &SyntaxTree,
    text: &str,
    picks: &[(Index, usize)],
) -> Result<(), TestCaseError> {
    for r in ranges(text, picks) {
        let ours = record_walk_range(tree, r.clone());
        let expected = filtered_walk(tree, r.clone());
        if ours != expected {
            return Err(TestCaseError::fail(format!(
                "range walk {r:?} differs\ntext: {text:?}\nrange walk: {ours:#?}\nfull walk: {expected:#?}"
            )));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_TRACES", 100), ..ProptestConfig::default() })]

    /// Range walks on documents of several chunks (64 lines each), so that
    /// the summary searches cross chunks.
    #[test]
    fn range_walks_big(
        doc in fm_document(400),
        ops in prop::collection::vec(op(), 0..3),
        picks in prop::collection::vec((any::<Index>(), prop_oneof![Just(0usize), 2usize..200, 200usize..3000]), 1..12),
    ) {
        let mut text = doc;
        let mut tree = SyntaxTree::parse_with(text.as_str(), extended());
        check_range_walks(&tree, &text, &picks)?;
        let mut cursor = 0;
        for op in &ops {
            let (s, e, ins) = concretize(&text, cursor, op);
            let (new_text, change) = apply(&text, s, e, &ins);
            tree = tree.reparse_full(new_text.as_str(), change).0;
            cursor = s + ins.len();
            text = new_text;
            check_range_walks(&tree, &text, &picks)?;
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: env_u32("GV_PROP_SINGLE", 2000), ..ProptestConfig::default() })]

    /// A range walk visits exactly what the full walk visits within the
    /// range, on fresh trees, reparsed trees (spliced summaries) and
    /// interim trees of paused reparses.
    #[test]
    fn range_walks(
        doc in fm_document(40),
        ops in prop::collection::vec(op(), 0..4),
        picks in prop::collection::vec((any::<Index>(), prop_oneof![Just(0usize), Just(1), 2usize..40, 40usize..400]), 1..8),
        fm in any::<bool>(),
    ) {
        let options = if fm { extended() } else { ParseOptions::default() };
        let mut text = doc;
        let mut tree = SyntaxTree::parse_with(text.as_str(), options);
        check_range_walks(&tree, &text, &picks)?;
        let mut cursor = 0;
        for (k, op) in ops.iter().enumerate() {
            let (s, e, ins) = concretize(&text, cursor, op);
            let (new_text, change) = apply(&text, s, e, &ins);
            if k % 2 == 1 {
                let mut task = tree.begin_reparse(new_text.as_str(), change);
                if !task.run(new_text.as_str(), Budget::Until(std::time::Instant::now())) {
                    let interim = task.interim(new_text.as_str());
                    interim.validate(new_text.as_str()).map_err(TestCaseError::fail)?;
                    check_range_walks(&interim, &new_text, &picks)?;
                }
            }
            tree = tree.reparse_full(new_text.as_str(), change).0;
            cursor = s + ins.len();
            text = new_text;
            check_range_walks(&tree, &text, &picks)?;
        }
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
                let r = check_edit(&tree, &text, &new_text, change, Mode::Direct)
                    .expect("incremental == full");
                // Range walks on a document of many chunks, around the edit
                // and elsewhere.
                let at = s.min(new_text.len());
                for range in [
                    at..at,
                    at.saturating_sub(300)..at + 300,
                    i * 37..i * 37 + 2000,
                ] {
                    assert_eq!(
                        record_walk_range(&r.0, range.clone()),
                        filtered_walk(&r.0, range.clone()),
                        "range walk {range:?} after edit {i}"
                    );
                }
                r
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

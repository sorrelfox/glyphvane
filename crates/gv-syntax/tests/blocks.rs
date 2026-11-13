// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Block structure: the outline of every CommonMark spec example and of
//! random documents compared with comrak's, incremental reparsing at every
//! position of every spec example, the decisions on oracle quirks, the
//! nesting cap, and the cases where convergence is subtle.
//!
//! `GV_DIFF_CASES` sets the number of random documents (default 2000);
//! `GV_SPEC_EDIT_STRIDE` tests every Nth position of the spec examples
//! (default 1 in release builds, 3 in debug builds).

mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use gv_syntax::{Change, SyntaxTree};
use support::comrak_outline::{comrak_outline, decided, gv_outline};
use support::edits::{Mode, apply, boundaries, check_edit};
use support::{COMMONMARK_SPEC, Rng, load_spec, minimize, random_document};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn outlines(md: &str) -> (Vec<String>, Vec<String>) {
    let tree = SyntaxTree::parse(md);
    let (ours, infos) = gv_outline(&tree, md);
    (ours, comrak_outline(md, &infos))
}

#[test]
fn spec_examples_block_structure_matches_comrak() {
    let mut per_section: BTreeMap<(usize, String), (usize, usize)> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut failures = String::new();
    for ex in load_spec(COMMONMARK_SPEC) {
        if !order.contains(&ex.section) {
            order.push(ex.section.clone());
        }
        let idx = order.iter().position(|s| *s == ex.section).unwrap_or(0);
        let tree = SyntaxTree::parse(ex.markdown.as_str());
        tree.validate(ex.markdown.as_str())
            .unwrap_or_else(|e| panic!("example {}: {e}", ex.number));
        let (ours, theirs) = outlines(&ex.markdown);
        let entry = per_section.entry((idx, ex.section.clone())).or_default();
        entry.1 += 1;
        if ours == theirs {
            entry.0 += 1;
        } else {
            let _ = writeln!(
                failures,
                "--- example {} ({}) {:?}\nours:\n  {}\ncomrak:\n  {}",
                ex.number,
                ex.section,
                ex.markdown,
                ours.join("\n  "),
                theirs.join("\n  ")
            );
        }
    }
    let mut report = String::new();
    for ((_, section), (pass, total)) in &per_section {
        let _ = writeln!(report, "{section}: {pass}/{total}");
    }
    println!("{report}");
    assert!(failures.is_empty(), "block outline mismatches:\n{failures}");
}

#[test]
fn random_documents_block_structure_matches_comrak() {
    let cases = env_usize("GV_DIFF_CASES", 2000);
    let mut rng = Rng::new(env_usize("GV_DIFF_SEED", 11) as u64);
    let differs = |md: &str| {
        let (a, b) = outlines(md);
        a != b
    };
    let mut new_failures: BTreeMap<String, usize> = BTreeMap::new();
    for _ in 0..cases {
        let doc = random_document(&mut rng, 60);
        let tree = SyntaxTree::parse(doc.as_str());
        tree.validate(doc.as_str())
            .unwrap_or_else(|e| panic!("{doc:?}: {e}"));
        if !differs(&doc) {
            continue;
        }
        let small = minimize(&doc, differs);
        let (a, b) = outlines(&small);
        if !decided(&small, &a, &b) {
            *new_failures.entry(small).or_default() += 1;
        }
    }
    let report: Vec<String> = new_failures
        .iter()
        .take(10)
        .map(|(md, n)| {
            let (a, b) = outlines(md);
            format!("{md:?} ({n}x)\n  ours:   {a:?}\n  comrak: {b:?}")
        })
        .collect();
    assert!(
        new_failures.is_empty(),
        "divergences:\n{}",
        report.join("\n")
    );
}

/// Each decision on a comrak/cmark quirk (see the known-divergences file),
/// made against the spec text.
#[test]
fn quirk_decisions() {
    for md in ["[foo]: /url\n---\n", "1.", "> foo\n<custom-tag>\n"] {
        let (a, b) = outlines(md);
        assert!(a != b && decided(md, &a, &b), "{md:?}: {a:?} vs {b:?}");
    }
    let html = gv_syntax::harness::render_commonmark_html;
    // After the definition is removed the paragraph is empty, so `---`
    // cannot be a setext underline: it is a thematic break (commonmark.js).
    assert_eq!(html("[foo]: /url\n---\n"), "<hr />\n");
    // The same inside a list item (comrak: the paragraph `---`).
    let md = "- [a]: /u\n  ---\n- b\n";
    assert_eq!(html(md), "<ul>\n<li>\n<hr />\n</li>\n<li>b</li>\n</ul>\n");
    let (a, b) = outlines(md);
    assert!(a != b && decided(md, &a, &b), "{md:?}: {a:?} vs {b:?}");
    // `===` cannot be a thematic break: paragraph text (spec example 216).
    assert_eq!(html("[foo]: /url\n===\n"), "<p>===</p>\n");
    // An empty item cannot interrupt the (empty) paragraph either.
    assert_eq!(html("[foo]: /url\n-\n"), "<p>-</p>\n");
    // An ordered marker at the end of the input is an empty item, like `-`.
    assert_eq!(html("1."), "<ol>\n<li></li>\n</ol>\n");
    assert_eq!(html("-"), "<ul>\n<li></li>\n</ul>\n");
    // HTML block type 7 cannot interrupt a paragraph, including a lazy one.
    assert_eq!(
        html("> foo\n<custom-tag>\n"),
        "<blockquote>\n<p>foo\n<custom-tag></p>\n</blockquote>\n"
    );
    // No limit on list starts per line (comrak stops at 100).
    let deep = format!("{}x\n", "- ".repeat(150));
    assert_eq!(html(&deep).matches("<ul>").count(), 150);
}

#[test]
fn nesting_cap() {
    // The cap is far above anything a document needs; check the mechanism
    // on a small scale through the depth that block quotes reach.
    let md = format!("{}a\n", "> ".repeat(2000));
    let tree = SyntaxTree::parse(md.as_str());
    tree.validate(md.as_str()).expect("valid");
    let depth = tree.blocks().iter().map(|b| b.depth).max().unwrap_or(0);
    assert_eq!(depth, 2000, "2000 quotes and a paragraph");
    const { assert!(gv_syntax::MAX_NESTING >= 1 << 20) };
}

/// Incremental == full for one-character edits at every position of every
/// spec example: insert `x`, `\n`, `>` or a backtick, or delete one char.
#[test]
fn spec_examples_incremental_equals_full() {
    let stride = env_usize(
        "GV_SPEC_EDIT_STRIDE",
        if cfg!(debug_assertions) { 3 } else { 1 },
    );
    let mut checks = 0;
    let mut failures = Vec::new();
    for ex in load_spec(COMMONMARK_SPEC) {
        let md = ex.markdown.as_str();
        let tree = SyntaxTree::parse(md);
        for (k, &p) in boundaries(md).iter().enumerate() {
            if k % stride != 0 {
                continue;
            }
            let mut edits: Vec<(usize, usize, &str)> =
                vec![(p, p, "x"), (p, p, "\n"), (p, p, ">"), (p, p, "`")];
            if p < md.len() {
                let mut q = p + 1;
                while !md.is_char_boundary(q) {
                    q += 1;
                }
                edits.push((p, q, ""));
            }
            for (s, e, ins) in edits {
                let (new_text, change) = apply(md, s, e, ins);
                checks += 1;
                let mode = if checks % 7 == 0 {
                    Mode::Sliced(1 + checks % 13)
                } else {
                    Mode::Direct
                };
                if let Err(err) = check_edit(&tree, md, &new_text, change, mode) {
                    failures.push(format!("example {}: {err}", ex.number));
                }
            }
        }
    }
    println!(
        "spec incremental checks: {checks}, failures: {}",
        failures.len()
    );
    for f in failures.iter().take(5) {
        println!("{f}");
    }
    assert!(
        failures.is_empty(),
        "{} incremental failures",
        failures.len()
    );
}

// ---------------------------------------------------------------------------
// Cases where convergence is subtle: each checked against a full parse and
// for how much it rescans.

fn step(
    text: &str,
    start: usize,
    end: usize,
    ins: &str,
) -> (gv_syntax::Changes, String, Vec<String>) {
    let tree = SyntaxTree::parse(text);
    let (new_text, change) = apply(text, start, end, ins);
    let (new, st) =
        check_edit(&tree, text, &new_text, change, Mode::Direct).expect("incremental == full");
    let (outline, _) = gv_outline(&new, new_text.as_str());
    (st, new_text, outline)
}

fn big_tight_list() -> String {
    (0..2000).map(|i| format!("- item {i}\n")).collect()
}

#[test]
fn typing_in_a_tight_list_restarts_at_the_edited_line() {
    let text = big_tight_list();
    let at = text.find("- item 1000\n").expect("item") + "- item 1000".len();
    let (st, _, _) = step(&text, at, at, "x");
    assert!(st.converged && st.lines_parsed <= 2, "{st:?}");
}

#[test]
fn loosening_a_list_is_local() {
    let text = big_tight_list();
    let at = text.find("- item 1000\n").expect("item");
    let (st, new_text, outline) = step(&text, at, at, "\n");
    assert!(st.converged && st.lines_parsed <= 3, "{st:?}");
    assert!(outline[1].contains("loose"), "{:?}", &outline[..2]);
    let (st, _, outline) = step(&new_text, at, at + 1, "");
    assert!(st.converged && st.lines_parsed <= 3 && outline[1].contains("tight"));
}

#[test]
fn setext_underline_turns_the_paragraph_above_into_a_heading() {
    let text = format!(
        "{}para line 1\npara line 2\n\n{}",
        "intro\n\n".repeat(500),
        "after\n\n".repeat(500)
    );
    let at = text.find("para line 2\n").expect("para") + "para line 2\n".len();
    let (st, new_text, outline) = step(&text, at, at, "---\n");
    assert!(
        outline.iter().any(|l| l.contains("heading h2 setext")),
        "{outline:?}"
    );
    assert!(st.converged && st.lines_parsed <= 3, "{st:?}");
    let (st, _, outline) = step(&new_text, at, at + 4, "");
    assert!(!outline.iter().any(|l| l.contains("heading")) && st.converged && st.lines_parsed <= 3);
}

#[test]
fn reference_definition_state_is_exact() {
    // `foo\n===` is a heading, `[foo]: /url\n===` a definition plus the
    // paragraph `===`: at line 2 both are "paragraph open"; the pending
    // definition text tells them apart.
    let text = format!(
        "{}[foo]: /url\n===\n\n{}",
        "x\n\n".repeat(300),
        "y\n\n".repeat(300)
    );
    let at = text.find("[foo]: /url").expect("def");
    let (st, _, outline) = step(&text, at, at + 1, "");
    assert!(
        outline.iter().any(|l| l.contains("heading h1 setext")),
        "{outline:?}"
    );
    assert!(st.converged && st.lines_parsed <= 4, "{st:?}");
}

#[test]
fn definition_blocks_converge_and_keep_few_states() {
    let defs: String = (0..500)
        .map(|i| format!("[label{i}]: /url/{i}\n"))
        .collect();
    let text = format!("intro\n\n{defs}\nafter [label7] text\n");
    let tree = SyntaxTree::parse(text.as_str());
    let at = text.find("/url/250").expect("def") + 4;
    let (_, st) = check_edit(
        &tree,
        &text,
        &apply(&text, at, at, "x").0,
        Change {
            start: at,
            old_end: at,
            new_len: 1,
        },
        Mode::Direct,
    )
    .expect("incremental == full");
    assert!(st.converged && st.lines_parsed <= 3, "{st:?}");
    // Task items and links at a paragraph start are ruled out on their
    // first line: no per-line states.
    let tasks: String = (0..500)
        .map(|i| format!("- [ ] task {i}\n- [link](/x) {i}\n"))
        .collect();
    let tree = SyntaxTree::parse(tasks.as_str());
    assert!(tree.state_count() < 50, "{} states", tree.state_count());
}

#[test]
fn restart_inside_a_definition_paragraph() {
    let text = "intro\n\n[a]: /a\n[b]:\n/b\n\"title\nstill title\"\n[c]: /c\nrest\n\nend\n";
    let at = text.find("still").expect("title");
    for ins in ["x", "\"", "\n", "]", "\n\n", "[d]: /d\n"] {
        let (st, _, _) = step(text, at, at, ins);
        assert_eq!(st.restart_line, 5, "restart next to the edit: {st:?}");
    }
    // A long unclosed title: the candidate outgrows the per-line check, so
    // a restart inside it rebuilds the candidate from the text.
    let words: Vec<String> = (0..400).map(|i| format!("word{i}")).collect();
    let long = format!("[a]: /u \"{}\n\nend\n", words.join("\n"));
    for at in [10, long.len() / 2, long.len() - 8] {
        for ins in ["x", "\"", "\n", "\"\n"] {
            step(&long, at, at, ins);
        }
    }
}

#[test]
fn lazy_continuation_is_decided_per_line() {
    let text = format!(
        "{}> quote line\nlazy line\n\n{}",
        "a\n\n".repeat(200),
        "b\n\n".repeat(200)
    );
    let at = text.find("lazy line").expect("lazy");
    for ins in ["> ", "- ", "# ", "    ", "<div>\n", "<custom-tag>\n"] {
        let (st, _, _) = step(&text, at, at, ins);
        assert!(st.converged && st.lines_parsed <= 4, "{ins:?}: {st:?}");
    }
}

#[test]
fn opening_a_fence_runs_to_the_end_and_closing_it_converges_back() {
    let text = "para\n\n".repeat(1000);
    let (st, new_text, outline) = step(&text, 0, 0, "```\n");
    assert!(!st.converged && st.lines_parsed > 2000, "{st:?}");
    assert_eq!(outline.len(), 2, "everything is code: {:?}", &outline[..3]);
    let (st, _, _) = step(&new_text, 0, 4, "");
    assert!(!st.converged, "closing it again also rescans everything");
    let at = new_text.len() / 2;
    let (st, _, _) = step(&new_text, at, at, "x");
    assert!(st.converged && st.lines_parsed <= 2, "{st:?}");
}

#[test]
fn typing_keeps_block_ids() {
    let text = "# Title\n\nSome paragraph text\nmore text\n\n- a\n- b\n";
    let tree = SyntaxTree::parse(text);
    let before = tree.blocks();
    let at = text.find("more text").expect("text") + 4;
    let (new_text, change) = apply(text, at, at, "x");
    let (new, _) = check_edit(&tree, text, &new_text, change, Mode::Direct).expect("ok");
    let after = new.blocks();
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(&after) {
        assert_eq!(a.id, b.id, "{a:?} vs {b:?}");
    }
}

#[test]
fn deep_lists_with_blank_lines_are_linear() {
    // Blank lines continue every open item; runs of such items are skipped
    // in one step.
    let md = format!("{}x{}", "- ".repeat(20_000), "\n".repeat(20_000));
    let t = std::time::Instant::now();
    let tree = SyntaxTree::parse(md.as_str());
    let elapsed = t.elapsed();
    tree.validate(md.as_str()).expect("valid");
    assert!(elapsed.as_secs_f64() < 5.0, "{elapsed:?}");
    let html = gv_syntax::harness::render_commonmark_html(&md);
    assert_eq!(html.matches("<ul>").count(), 20_000);
}

/// A text block of only link reference definitions is no structural
/// element (cmark removes it): it neither loosens a list nor keeps an item
/// non-empty.
#[test]
fn definition_only_text_blocks_are_not_siblings() {
    let cases = [
        "- )\n\n  [f]:\"\n",
        "- a\n\n  [f]: /u\n- b\n",
        "- [a]: /u\n\n\n  foo\n",
        "- [a]: /u\n- b\n",
        "- a\n\n  [f]: /u\n\n  b\n",
        "1. [a]: /u\n   [b]: /v\n\n   c\n",
        "- a\n  [f]: /u\n\n- b\n",
        "- [a]: /u\n  ===\n\n- b\n",
        "> - a\n>\n>   [f]: /u\n> - b\n",
        "- k\n\n  [f]: /u\n> x\n",
        "- k\n\n  [f]: /u\n\n> x\n",
        "- k\n\n  [f]: /u\n# h\n",
        "- k\n\n  [f]: /u\n* b\n",
        "- k\n\n  [f]: /u\n    code\n",
        "- k\n\n  [f]: /u\nlazy\n",
        "- k\n\n  [f]: /u\n  # h\n",
        "> - k\n>\n>   [f]: /u\n# h\n",
    ];
    for md in cases {
        let (ours, theirs) = outlines(md);
        assert_eq!(ours, theirs, "{md:?}");
        let tree = SyntaxTree::parse(md);
        for &p in &boundaries(md) {
            for ins in ["x", "\n", "[", " "] {
                let (new_text, change) = apply(md, p, p, ins);
                check_edit(&tree, md, &new_text, change, Mode::Direct)
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        }
    }
}

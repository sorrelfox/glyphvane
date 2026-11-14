// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Build and update timings on large documents. Ignored by default
//! (timings in a debug build mean little); run in release:
//!
//! ```sh
//! cargo test --release -p gv-render-tree --test perf -- --ignored --nocapture
//! ```
//!
//! Documents of 1 MB: the GFM spec repeated (prose, code, lists, links),
//! and a generated outliner (one deep tight list: the worst case of an
//! update, which rebuilds the whole top-level block). Reported, median of
//! the runs: the parse and model build (for scale), a full render build
//! with flattening, the total estimated height, and a keystroke in the
//! middle (reparse, model update and render update; and the render update
//! alone).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gv_model::DocModel;
use gv_render_tree::{RenderOptions, RenderTree, Sources};
use gv_syntax::{Change, SyntaxTree};

fn repeat_to(unit: &str, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + unit.len());
    while s.len() < bytes {
        s.push_str(unit);
        if !s.ends_with('\n') {
            s.push('\n');
        }
    }
    s
}

fn spec(bytes: usize) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/spec/gfm-0.29.0.gfm.13.txt");
    repeat_to(&std::fs::read_to_string(path).expect("spec text"), bytes)
}

fn outliner(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 100);
    let mut i = 0usize;
    while s.len() < bytes {
        let depth = (i * 7 + i / 3) % 6;
        s.push_str(&"  ".repeat(depth));
        s.push_str(&format!(
            "- item {i} with some *text* and a [link](n{i}.md)\n"
        ));
        i += 1;
    }
    s
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> Duration {
    median(
        (0..runs)
            .map(|_| {
                let t = Instant::now();
                std::hint::black_box(f());
                t.elapsed()
            })
            .collect(),
    )
}

fn report(name: &str, text: &str) {
    let runs = 11;
    let tree = SyntaxTree::parse(text);
    let model = DocModel::build(&tree, text);
    let parse = time(runs, || SyntaxTree::parse(text));
    let model_build = time(runs, || DocModel::build(&tree, text));
    let build = time(runs, || {
        RenderTree::build(
            &tree,
            &model,
            text,
            RenderOptions::default(),
            Sources::default(),
        )
    });
    let rt = RenderTree::build(
        &tree,
        &model,
        text,
        RenderOptions::default(),
        Sources::default(),
    );
    let height = time(runs, || rt.height_estimate(600.0));
    let bytes: usize = rt
        .nodes()
        .iter()
        .filter_map(|n| n.inline())
        .map(|c| c.paragraph.heap_bytes())
        .sum();

    // A keystroke at the end of a text line in the middle.
    let mid = text[..text.len() / 2].rfind('\n').unwrap_or(0);
    let mut edited = text.to_owned();
    edited.insert(mid, 'x');
    let change = Change {
        start: mid,
        old_end: mid,
        new_len: 1,
    };
    let (next, changes) = tree.reparse_full(edited.as_str(), change);
    let next_model = model.update(&next, edited.as_str(), &changes);
    // Each timed update gets its own copy of the tree, cloned beforehand.
    let mut copies: Vec<RenderTree> = (0..runs).map(|_| rt.clone()).collect();
    let keystroke = median(
        copies
            .iter_mut()
            .map(|r| {
                let t = Instant::now();
                let (next, changes) = tree.reparse_full(edited.as_str(), change);
                let m = model.update(&next, edited.as_str(), &changes);
                std::hint::black_box(r.update(
                    &next,
                    &m,
                    edited.as_str(),
                    &changes,
                    Sources::default(),
                ));
                t.elapsed()
            })
            .collect(),
    );
    let mut copies: Vec<RenderTree> = (0..runs).map(|_| rt.clone()).collect();
    let update = median(
        copies
            .iter_mut()
            .map(|r| {
                let t = Instant::now();
                std::hint::black_box(r.update(
                    &next,
                    &next_model,
                    edited.as_str(),
                    &changes,
                    Sources::default(),
                ));
                t.elapsed()
            })
            .collect(),
    );
    let clone = time(runs, || rt.clone());
    let mut r = rt.clone();
    let rep = r.update(
        &next,
        &next_model,
        edited.as_str(),
        &changes,
        Sources::default(),
    );
    println!(
        "{name:<10} {:>5.2} MB  {} nodes, {} items  parse {:>8.2?}  model {:>8.2?}  \
         render build {:>8.2?}  height(600) {:>8.2?} = {:.0} px  paragraphs {:.1} MB\n\
         {:<10} keystroke {:>8.2?} (render update {:>8.2?}; {} built, {} reused, \
         {} items replaced); tree clone {:>8.2?}",
        text.len() as f64 / 1e6,
        rt.nodes().len(),
        rt.items().len(),
        parse,
        model_build,
        build,
        height,
        rt.height_estimate(600.0),
        bytes as f64 / 1e6,
        "",
        keystroke,
        update,
        rep.nodes_built,
        rep.nodes_reused,
        rep.removed.len(),
        clone,
    );
}

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn perf() {
    report("spec", &spec(1 << 20));
    report("outliner", &outliner(1 << 20));
}

/// About `bytes` of mixed prose, lists, quotes and code (the Reading
/// view's generated timing document).
fn generated(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 4096);
    let mut i = 0;
    while s.len() < bytes {
        match i % 10 {
            0 => s.push_str(&format!("## Section {i}\n\n")),
            1..=5 => s.push_str(&format!(
                "Paragraph {i} with **strong** and *emphasised* words, `inline code` and a \
                 [link](other-{i}.md#part) to another note, written long enough to wrap \
                 over two or three lines of the text column.\n\n"
            )),
            6 => s.push_str(&format!("- item {i} one\n- item two\n  - nested three\n\n")),
            7 => s.push_str(&format!(
                "> A quote {i} that also has some words in it.\n\n"
            )),
            _ => s.push_str(&format!(
                "```rust\nfn f{i}(x: u32) -> u32 {{\n    x * 2 + {i}\n}}\n```\n\n"
            )),
        }
        i += 1;
    }
    s
}

/// Find in page on the generated 10 MB document: the find index, and
/// searches over it (all matches, the first 10,000, whole word, regex),
/// against projecting every item's text on the way.
#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn find_perf() {
    use gv_render_tree::{FindOptions, Finder};

    let text = generated(10 << 20);
    let tree = SyntaxTree::parse(&text);
    let model = DocModel::build(&tree, &text);
    let rt = RenderTree::build(
        &tree,
        &model,
        text.as_str(),
        RenderOptions::default(),
        Sources::default(),
    );
    let runs = 5;
    let index = rt.find_index();
    let build = time(runs, || rt.find_index());
    let finder = |needle: &str, o: FindOptions| Finder::new(needle, o).expect("compiles");
    let plain = finder("the", FindOptions::default());
    let (all, _) = index.find(&plain, usize::MAX);
    let all_t = time(runs, || index.find(&plain, usize::MAX));
    let capped_t = time(runs, || index.find(&plain, 10_000));
    // The first match: the first chunk of items that holds one.
    let first_t = time(runs, || {
        let mut out = Vec::new();
        let mut start = 0;
        while out.is_empty() && start < index.len() {
            index.find_in(&plain, start..start + 1024, 1, &mut out);
            start += 1024;
        }
        out
    });
    let word = finder(
        "the",
        FindOptions {
            whole_word: true,
            ..FindOptions::default()
        },
    );
    let (words, _) = index.find(&word, usize::MAX);
    let word_t = time(runs, || index.find(&word, usize::MAX));
    let re = finder(
        r"wr\w+",
        FindOptions {
            regex: true,
            ..FindOptions::default()
        },
    );
    let (res, _) = index.find(&re, usize::MAX);
    let re_t = time(runs, || index.find(&re, usize::MAX));
    let rare = finder("Section 99990", FindOptions::default());
    let rare_t = time(runs, || index.find(&rare, usize::MAX));
    let direct_t = time(runs, || {
        let mut out = Vec::new();
        rt.find_with(&plain, 0..rt.items().len(), usize::MAX, &mut out);
        out
    });
    let bytes: usize = (0..index.len())
        .filter_map(|ix| index.item(ix))
        .map(|t| t.text.len())
        .sum();
    println!(
        "find, generated {:.1} MB, {} items ({:.1} MB of text): index build {build:.2?}\n\
         \"the\": {} matches, all {all_t:.2?}, first 10,000 {capped_t:.2?}, first match {first_t:.2?}\n\
         whole word \"the\": {} matches {word_t:.2?}; regex wr\\w+: {} matches {re_t:.2?}; \
         rare literal (full scan) {rare_t:.2?}\n\
         without the index (projecting every item): {direct_t:.2?}",
        text.len() as f64 / 1e6,
        rt.items().len(),
        bytes as f64 / 1e6,
        all.len(),
        words.len(),
        res.len(),
    );
}

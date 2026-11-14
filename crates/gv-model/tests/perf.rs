// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Build and update timings on large documents. Ignored by default
//! (timings in a debug build mean little); run in release:
//!
//! ```sh
//! cargo test --release -p gv-model --test perf -- --ignored --nocapture
//! ```
//!
//! Documents, 1 MB and 10 MB each: the GFM spec repeated (prose, code,
//! headings, some links); a generated link list (an "awesome" README: a
//! heading every 20 lines, every line a link); a generated outliner (one
//! deep tight list). Reported, median of the runs: the block parse (for
//! scale), a full model build, the first GitHub slug pass, and a
//! keystroke in the middle (reparse plus model update, and the update
//! alone).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gv_model::{DocModel, SlugAlgorithm};
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

fn link_list(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 100);
    let mut i = 0usize;
    while s.len() < bytes {
        if i.is_multiple_of(20) {
            s.push_str(&format!("\n## Category {}\n\n", i / 20));
        }
        s.push_str(&format!(
            "- [Project {i}](https://github.com/owner/project-{i}) - A *useful* tool for `task {i}`. [![Badge](https://img.shields.io/b/{i}.svg)](https://ci.example.org/{i})\n"
        ));
        i += 1;
    }
    s
}

fn outliner(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 100);
    let mut i = 0usize;
    while s.len() < bytes {
        let depth = (i * 7 + i / 3) % 6;
        s.push_str(&"  ".repeat(depth));
        s.push_str(&format!(
            "- item {i} with some text and a [[note]] or [link](n{i}.md)\n"
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
    let runs = if text.len() > 4 << 20 { 5 } else { 15 };
    let tree = SyntaxTree::parse(text);
    let parse = time(runs, || SyntaxTree::parse(text));
    let build = time(runs, || DocModel::build(&tree, text));
    struct Noop;
    impl gv_syntax::Visitor for Noop {}
    let walk = time(runs, || gv_syntax::walk(&tree, &mut Noop));
    let model = DocModel::build(&tree, text);
    let slug_times: Vec<Duration> = (0..runs)
        .map(|_| {
            let m = DocModel::build(&tree, text);
            let t = Instant::now();
            std::hint::black_box(m.slugs(SlugAlgorithm::GitHub).len());
            t.elapsed()
        })
        .collect();
    // A keystroke at the end of a line in the middle.
    let mid = text[..text.len() / 2].rfind('\n').unwrap_or(0);
    let mut edited = text.to_owned();
    edited.insert(mid, 'x');
    let change = Change {
        start: mid,
        old_end: mid,
        new_len: 1,
    };
    let (next, changes) = tree.reparse_full(edited.as_str(), change);
    let keystroke = time(runs, || {
        let (next, changes) = tree.reparse_full(edited.as_str(), change);
        model.update(&next, edited.as_str(), &changes)
    });
    let update = time(runs, || model.update(&next, edited.as_str(), &changes));
    let updated = model.update(&next, edited.as_str(), &changes);
    let s = updated.stats();
    println!(
        "{name:<14} {:>6.2} MB  parse {:>8.2?}  walk {:>8.2?}  build {:>8.2?}  github slugs {:>8.2?}  \
         keystroke {:>8.2?} (update {:>8.2?}; {} blocks kept, {} leaves walked, {} inline parses)  \
         {} headings, {} links",
        text.len() as f64 / 1e6,
        parse,
        walk,
        build,
        median(slug_times),
        keystroke,
        update,
        s.reused,
        s.leaves,
        s.inline_parses,
        model.headings().len(),
        model.links().len(),
    );
}

#[test]
#[ignore = "timing; run in release with --ignored --nocapture"]
fn timings() {
    for (mb, label) in [(1usize, "1 MB"), (10, "10 MB")] {
        let bytes = mb << 20;
        println!("-- {label}");
        report("gfm spec", &spec(bytes));
        report("link list", &link_list(bytes));
        report("outliner", &outliner(bytes));
    }
}

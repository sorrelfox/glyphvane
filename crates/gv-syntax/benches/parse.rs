// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parser benchmarks.
//!
//! - `full_parse/*`: the block pass alone (`block`), block pass plus every
//!   inline (`full`), and parse plus CommonMark HTML (`html`).
//! - `keystroke/*`: type one character at the end of a text line in the
//!   middle of the document, then delete it; time per reparse.
//! - `fence_at_top/*`: open a fence at the top (the reparse runs to the
//!   end), then close it again; time per reparse.
//! - `walk/*`: block views. `range_keystroke` walks the blocks of a 40-byte
//!   range at a text line in the middle (what a keystroke there touches),
//!   `range_screen` those of 4 KB from there (about a screenful), `full`
//!   the whole document; `block_index` builds the per-revision index.
//! - After the criterion groups, a latency report times single edits one
//!   by one (p50/p95/p99/max) and the fence-at-top case in 1 ms slices.
//!
//! Documents: the GFM spec (`tests/fixtures/spec/gfm-0.29.0.gfm.13.txt`,
//! real prose, lists and code) repeated to 1 MB and 10 MB, and a generated
//! 1 MB outliner (one huge nested tight list).

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput};
use gv_syntax::{Budget, Change, SyntaxTree};

mod common;
use common::{full_inlines, outliner, repeat_to, spec_text, text_line_end};

fn docs() -> Vec<(&'static str, String)> {
    let spec = spec_text();
    vec![
        ("spec_1MB", repeat_to(&spec, 1 << 20)),
        ("spec_10MB", repeat_to(&spec, 10 << 20)),
        ("outliner_1MB", outliner(1 << 20)),
    ]
}

fn full_parse(c: &mut Criterion, docs: &[(&'static str, String)]) {
    let mut g = c.benchmark_group("full_parse");
    g.sample_size(10)
        .measurement_time(Duration::from_secs(4))
        .warm_up_time(Duration::from_secs(1));
    for (name, text) in docs {
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(BenchmarkId::new("block", name), text, |b, t| {
            b.iter(|| black_box(SyntaxTree::parse(t.as_str())));
        });
        g.bench_with_input(BenchmarkId::new("full", name), text, |b, t| {
            b.iter(|| {
                let tree = SyntaxTree::parse(t.as_str());
                full_inlines(&tree, t);
                black_box(tree)
            });
        });
        g.bench_with_input(BenchmarkId::new("html", name), text, |b, t| {
            b.iter(|| black_box(gv_syntax::harness::render_commonmark_html(t)));
        });
    }
    g.finish();
}

fn keystroke(c: &mut Criterion, docs: &[(&'static str, String)]) {
    let mut g = c.benchmark_group("keystroke");
    g.sample_size(30)
        .measurement_time(Duration::from_secs(3))
        .warm_up_time(Duration::from_secs(1));
    for (name, text) in docs {
        let at = text_line_end(text, text.len() / 2);
        let with = format!("{}x{}", &text[..at], &text[at..]);
        let tree = SyntaxTree::parse(text.as_str());
        let insert = Change {
            start: at,
            old_end: at,
            new_len: 1,
        };
        let delete = Change {
            start: at,
            old_end: at + 1,
            new_len: 0,
        };
        g.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                let mut tree = tree.clone();
                for _ in 0..iters {
                    let t = Instant::now();
                    let (t1, _) = tree.reparse_full(with.as_str(), insert);
                    let (t2, _) = t1.reparse_full(text.as_str(), delete);
                    total += t.elapsed();
                    tree = t2;
                }
                total / 2
            });
        });
    }
    g.finish();
}

fn fence_at_top(c: &mut Criterion, docs: &[(&'static str, String)]) {
    let mut g = c.benchmark_group("fence_at_top");
    g.sample_size(10)
        .measurement_time(Duration::from_secs(4))
        .warm_up_time(Duration::from_secs(1));
    for (name, text) in docs {
        let with = format!("```\n{text}");
        let tree = SyntaxTree::parse(text.as_str());
        let open = Change {
            start: 0,
            old_end: 0,
            new_len: 4,
        };
        let close = Change {
            start: 0,
            old_end: 4,
            new_len: 0,
        };
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                let mut tree = tree.clone();
                for _ in 0..iters {
                    let t = Instant::now();
                    let (t1, _) = tree.reparse_full(with.as_str(), open);
                    let (t2, _) = t1.reparse_full(text.as_str(), close);
                    total += t.elapsed();
                    tree = t2;
                }
                total / 2
            });
        });
    }
    g.finish();
}

/// Counts what it visits, so the walk cannot be optimized away.
#[derive(Default)]
struct Count(usize);

impl gv_syntax::Visitor for Count {
    fn enter(&mut self, c: &gv_syntax::Container) {
        self.0 += c.line;
    }
    fn leaf(&mut self, l: &gv_syntax::Leaf<'_>) {
        self.0 += l.lines.len();
    }
}

fn walks(c: &mut Criterion, docs: &[(&'static str, String)]) {
    let mut g = c.benchmark_group("walk");
    g.sample_size(30)
        .measurement_time(Duration::from_secs(2))
        .warm_up_time(Duration::from_millis(500));
    for (name, text) in docs {
        let tree = SyntaxTree::parse(text.as_str());
        let at = text_line_end(text, text.len() / 2);
        g.bench_function(BenchmarkId::new("range_keystroke", name), |b| {
            b.iter(|| {
                let mut v = Count::default();
                tree.walk_range(at - 40..at, &mut v);
                black_box(v.0)
            });
        });
        g.bench_function(BenchmarkId::new("range_screen", name), |b| {
            b.iter(|| {
                let mut v = Count::default();
                tree.walk_range(at..(at + 4096).min(text.len()), &mut v);
                black_box(v.0)
            });
        });
    }
    g.sample_size(10).measurement_time(Duration::from_secs(4));
    for (name, text) in docs {
        let tree = SyntaxTree::parse(text.as_str());
        g.bench_function(BenchmarkId::new("full", name), |b| {
            b.iter(|| {
                let mut v = Count::default();
                tree.walk(&mut v);
                black_box(v.0)
            });
        });
        g.bench_function(BenchmarkId::new("block_index", name), |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    // A fresh revision each time: the index is cached per
                    // tree. Only building the index is timed.
                    let fresh = tree.clone().reparse_full(
                        text.as_str(),
                        Change {
                            start: 0,
                            old_end: 0,
                            new_len: 0,
                        },
                    );
                    let t = Instant::now();
                    black_box(fresh.0.block_index().len());
                    total += t.elapsed();
                }
                total
            });
        });
    }
    g.finish();
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Times single edits one by one: typing sessions (10 characters, then 10
/// backspaces, at random text-line ends) and structural edits at random
/// line starts, each undone. Every session ends with a check against a
/// full parse.
fn latency_report(docs: &[(&'static str, String)]) {
    println!("\nLatency (each edit timed on its own; microseconds)\n");
    println!("| Session | Edits | p50 | p95 | p99 | max | mean lines | converged |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut rnd = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for (name, doc) in docs {
        for structural in [false, true] {
            let mut text = doc.clone();
            let mut tree = SyntaxTree::parse(text.as_str());
            let mut times = Vec::new();
            let (mut lines, mut converged) = (0usize, 0usize);
            let spots = if name.contains("10MB") { 100 } else { 300 };
            for _ in 0..spots {
                let edits: Vec<(usize, usize, String)> = if structural {
                    let at = text[..rnd(text.len())].rfind('\n').map_or(0, |i| i + 1);
                    let ins = [
                        "> ",
                        "- ",
                        "# ",
                        "1. ",
                        "    ",
                        "\n",
                        "---\n",
                        "<div>\n",
                        "[x]: /y\n",
                        "===\n",
                    ][rnd(10)];
                    vec![
                        (at, at, ins.to_owned()),
                        (at, at + ins.len(), String::new()),
                    ]
                } else {
                    let at = text_line_end(&text, rnd(text.len() - 1));
                    let mut v: Vec<(usize, usize, String)> = (0..10)
                        .map(|i| (at + i, at + i, String::from("x")))
                        .collect();
                    v.extend((0..10).rev().map(|i| (at + i, at + i + 1, String::new())));
                    v
                };
                for (s, e, ins) in edits {
                    let mut new_text = String::with_capacity(text.len() + ins.len());
                    new_text.push_str(&text[..s]);
                    new_text.push_str(&ins);
                    new_text.push_str(&text[e..]);
                    let change = Change {
                        start: s,
                        old_end: e,
                        new_len: ins.len(),
                    };
                    let t = Instant::now();
                    let (new, st) = tree.reparse_full(new_text.as_str(), change);
                    times.push(t.elapsed().as_secs_f64() * 1e6);
                    lines += st.lines_parsed;
                    converged += usize::from(st.converged);
                    tree = new;
                    text = new_text;
                }
            }
            if let Err(e) = tree.same_structure(&SyntaxTree::parse(text.as_str())) {
                panic!("{name}: incremental != full after the session: {e}");
            }
            let n = times.len();
            let mean_lines = lines as f64 / n as f64;
            times.sort_by(f64::total_cmp);
            println!(
                "| {name} {} | {n} | {:.1} | {:.1} | {:.1} | {:.0} | {mean_lines:.1} | {:.1}% |",
                if structural { "structural" } else { "typing" },
                percentile(&times, 0.5),
                percentile(&times, 0.95),
                percentile(&times, 0.99),
                percentile(&times, 1.0),
                100.0 * converged as f64 / n as f64,
            );
        }
    }

    println!("\nFence at the top, 1 ms slices (milliseconds)\n");
    println!("| Document | First slice | Slices | Total (all slices + finish) | Interim tree |");
    println!("|---|---:|---:|---:|---:|");
    for (name, doc) in docs {
        let tree = SyntaxTree::parse(doc.as_str());
        let with = format!("````````````````````````````````````````\n{doc}");
        let change = Change {
            start: 0,
            old_end: 0,
            new_len: 41,
        };
        let t0 = Instant::now();
        let mut task = tree.begin_reparse(with.as_str(), change);
        let done = task.run(with.as_str(), Budget::from_now(Duration::from_millis(1)));
        let first = t0.elapsed();
        let mut slices = 1;
        let mut interim_ms = 0.0;
        if !done {
            let ti = Instant::now();
            black_box(task.interim(with.as_str()));
            interim_ms = ti.elapsed().as_secs_f64() * 1e3;
            while !task.run(with.as_str(), Budget::from_now(Duration::from_millis(1))) {
                slices += 1;
            }
            slices += 1;
        }
        black_box(task.finish());
        let total = t0.elapsed();
        println!(
            "| {name} | {:.2} | {slices} | {:.2} | {interim_ms:.3} |",
            first.as_secs_f64() * 1e3,
            total.as_secs_f64() * 1e3
        );
    }
}

fn main() {
    let docs = docs();
    let mut c = Criterion::default().configure_from_args();
    full_parse(&mut c, &docs);
    keystroke(&mut c, &docs);
    fence_at_top(&mut c, &docs);
    walks(&mut c, &docs);
    c.final_summary();
    if std::env::args().all(|a| a != "--list") {
        latency_report(&docs);
    }
}

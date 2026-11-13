// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Benchmarks on a 10 MiB Markdown-like text: position conversions, line
//! lookups, and transactions of 1, 100 and 1 000 edits (one pass each).
//! Not run in CI. Run with `cargo bench -p gv-text`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use gv_text::{Bias, Buffer, OffsetUtf16, Point, TextSnapshot, Transaction};

const TEXT_LEN: usize = 10 * 1024 * 1024;
const LOOKUPS: usize = 4096;

/// Deterministic pseudo-random numbers (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn markdown_text(len: usize) -> String {
    const PIECES: &[&str] = &[
        "# Heading with **bold** text\n\n",
        "A paragraph with *emphasis*, `code`, a [[wikilink]] and a [link](https://example.com).\n",
        "- [ ] task item with émoji 🎉 and ünïcödé\n",
        "> quoted line 引用文\r\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "```rust\nfn main() {}\n```\n",
        "Plain prose continues here without anything special at all.\n",
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut text = String::with_capacity(len + 128);
    while text.len() < len {
        text.push_str(PIECES[rng.below(PIECES.len())]);
    }
    text
}

fn offsets(snapshot: &TextSnapshot, rng: &mut Rng) -> Vec<usize> {
    (0..LOOKUPS)
        .map(|_| snapshot.clip_offset(rng.below(snapshot.len()), Bias::Left))
        .collect()
}

fn conversions(c: &mut Criterion) {
    let text = markdown_text(TEXT_LEN);
    let snapshot = TextSnapshot::new(&text);
    let mut rng = Rng(42);
    let offsets = offsets(&snapshot, &mut rng);
    let points: Vec<Point> = offsets
        .iter()
        .map(|&o| snapshot.offset_to_point(o).expect("valid"))
        .collect();
    let utf16: Vec<OffsetUtf16> = offsets
        .iter()
        .map(|&o| snapshot.offset_to_utf16(o).expect("valid"))
        .collect();

    let mut group = c.benchmark_group("conversions_10MiB");
    group.sample_size(20);
    group.bench_function("build", |b| b.iter(|| TextSnapshot::new(black_box(&text))));
    group.bench_function("snapshot_clone", |b| b.iter(|| black_box(snapshot.clone())));
    group.bench_function("offset_to_point", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % LOOKUPS;
            snapshot.offset_to_point(black_box(offsets[i]))
        });
    });
    group.bench_function("point_to_offset", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % LOOKUPS;
            snapshot.point_to_offset(black_box(points[i]))
        });
    });
    group.bench_function("offset_to_utf16", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % LOOKUPS;
            snapshot.offset_to_utf16(black_box(offsets[i]))
        });
    });
    group.bench_function("utf16_to_offset", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % LOOKUPS;
            snapshot.utf16_to_offset(black_box(utf16[i]))
        });
    });
    group.bench_function("offset_to_point_utf16", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % LOOKUPS;
            snapshot.offset_to_point_utf16(black_box(offsets[i]))
        });
    });
    group.bench_function("line_range", |b| {
        let mut i = 0;
        b.iter(|| {
            i = (i + 1) % LOOKUPS;
            snapshot.line_range(black_box(points[i].line))
        });
    });
    group.bench_function("chunk_walk", |b| {
        b.iter(|| snapshot.all_chunks().map(str::len).sum::<usize>());
    });
    group.finish();
}

/// `count` sorted single-character edits spread over the text.
fn transaction(snapshot: &TextSnapshot, count: usize, rng: &mut Rng) -> Transaction {
    let mut starts: Vec<usize> = (0..count)
        .map(|_| snapshot.clip_offset(rng.below(snapshot.len()), Bias::Left))
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let mut transaction = Transaction::new();
    for start in starts {
        let end = snapshot.clip_offset(start + 1, Bias::Right);
        transaction.push(start..end, "x\n");
    }
    transaction
}

fn transactions(c: &mut Criterion) {
    let text = markdown_text(TEXT_LEN);
    let buffer = Buffer::new(&text);
    let mut rng = Rng(7);
    let mut group = c.benchmark_group("transactions_10MiB");
    group.sample_size(20);
    for count in [1, 100, 1000] {
        let tx = transaction(buffer.text(), count, &mut rng);
        group.bench_function(format!("{count}_edits"), |b| {
            b.iter_batched(
                || buffer.clone(),
                |mut buffer| buffer.apply(&tx).map(|summary| summary.edits().len()),
                criterion::BatchSize::SmallInput,
            );
        });
    }
    let typing = Transaction::new().insert(buffer.len() / 2, "a");
    group.bench_function("type_one_char", |b| {
        b.iter_batched(
            || buffer.clone(),
            |mut buffer| buffer.apply(&typing).map(|summary| summary.edits().len()),
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, conversions, transactions);
criterion_main!(benches);

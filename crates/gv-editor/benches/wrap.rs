// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Soft-wrap benchmarks on a 10 MiB Markdown-like document:
//!
//! - wrapping it all: the first wrap, a new width, a proportional measure,
//!   and the layer without a width;
//! - one keystroke synced through every layer (buffer, replace, tab, wrap),
//!   against the same without the wrap layer, and through the editor;
//! - moving down one wrap row in the editor.
//!
//! With `--bench`, per-keystroke and re-wrap percentiles are printed after
//! the Criterion runs. Not run in CI. Run with
//! `cargo bench -p gv-editor --bench wrap`.

use std::hint::black_box;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group};
use gv_editor::display_map::{
    MonospaceMeasure, ReplaceMap, TabMap, TabSnapshot, TextMeasure, WrapMap,
};
use gv_editor::{Editor, Motion};
use gv_text::{Bias, Buffer};

const DOC_LEN: usize = 10 * 1024 * 1024;
const TAB_SIZE: NonZeroU32 = NonZeroU32::new(4).unwrap();
/// The wrap width in columns of the monospace measure.
const COLUMNS: f32 = 80.0;

/// A Markdown-like document of about `len` bytes, with short lines, lines
/// a little over 80 columns and long paragraphs.
fn document(len: usize) -> String {
    const PIECES: &[&str] = &[
        "# Heading with **bold** text\n\n",
        "A paragraph with *emphasis*, `code`, a [[wikilink]] and a [link](https://example.com).\n",
        "- [ ] task item with émoji 🎉 and ünïcödé that goes on long enough to wrap once\n",
        "> quoted line 引用文 with 日本語のテキストが続きます and more words after it\r\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "Plain prose continues here without anything special at all.\n\n",
        "A long paragraph line, as in prose written without hard breaks: it runs on \
         for several hundred characters, so soft wrap breaks it into a few rows at \
         every width, with words of different lengths, punctuation, and the odd \
         extraordinarily-long-hyphenated-compound that has to be broken somewhere.\n",
    ];
    let mut text = String::with_capacity(len + 512);
    let mut i = 0usize;
    while text.len() < len {
        text.push_str(PIECES[i % PIECES.len()]);
        i = i.wrapping_mul(31).wrapping_add(7);
    }
    text
}

/// Proportional widths in columns: narrow and wide Latin letters, wide
/// CJK, as a prose font would measure them.
#[derive(Clone, Copy, Debug)]
struct Proportional;

impl TextMeasure for Proportional {
    fn char_width(&mut self, c: char) -> f32 {
        match c {
            'i' | 'l' | 'j' | '.' | ',' | ':' | ';' | '|' | '!' | '\'' => 0.45,
            'm' | 'w' | 'M' | 'W' => 1.4,
            ' ' => 0.55,
            c if c.is_ascii_uppercase() => 1.2,
            c if (c as u32) >= 0x3000 => 2.0,
            _ => 1.0,
        }
    }
}

fn tab_snapshot(text: &str) -> TabSnapshot {
    let (_, replace) = ReplaceMap::new(gv_text::TextSnapshot::new(text));
    TabMap::new(replace, TAB_SIZE).1
}

/// A buffer and every layer over it, wrapped at 80 columns or not.
struct Layers {
    buffer: Buffer,
    replace_map: ReplaceMap,
    tab_map: TabMap,
    wrap_map: Option<WrapMap>,
}

impl Layers {
    fn new(text: &str, wrapped: bool) -> Self {
        let buffer = Buffer::new(text);
        let (replace_map, replace) = ReplaceMap::new(buffer.snapshot());
        let (tab_map, tabs) = TabMap::new(replace, TAB_SIZE);
        let wrap_map = wrapped
            .then(|| WrapMap::new(tabs, Box::new(MonospaceMeasure::new(1.0)), Some(COLUMNS)).0);
        Self {
            buffer,
            replace_map,
            tab_map,
            wrap_map,
        }
    }

    /// Types one character at a different place each time (so no line
    /// grows long), syncing every layer.
    fn keystroke(&mut self, i: usize) {
        let offset = self
            .buffer
            .clip_offset((i * 104_729) % self.buffer.len(), Bias::Left);
        let Ok(summary) = self.buffer.edit([(offset..offset, "x")]) else {
            return;
        };
        let (replace, replace_edits) = self.replace_map.sync(self.buffer.snapshot(), &summary);
        let (tabs, tab_edits) = self.tab_map.sync(replace, replace_edits, TAB_SIZE);
        if let Some(wrap_map) = &mut self.wrap_map {
            black_box(wrap_map.sync(tabs, tab_edits));
        } else {
            black_box((tabs, tab_edits));
        }
    }
}

/// An editor in the middle of `text`, wrapped at 80 columns or not.
fn make_editor(text: &str, wrapped: bool) -> Editor {
    let mut editor = Editor::new(Buffer::new(text));
    if wrapped {
        editor.set_wrap_width(Some(COLUMNS));
    }
    editor.set_cursor(text.len() / 2);
    editor.move_selections(Motion::LineEnd, false);
    editor
}

/// Prose with line breaks, so typed lines stay short (as in the typing
/// benchmark).
const TYPED: &str = "the quick brown fox jumps over the lazy dog\n";

fn type_char(editor: &mut Editor, i: usize) {
    let at = i % TYPED.len();
    editor.insert_at_selections(&TYPED[at..at + 1]);
}

fn rewrap(c: &mut Criterion) {
    let text = document(DOC_LEN);
    let tabs = tab_snapshot(&text);

    let mut group = c.benchmark_group("rewrap 10 MiB");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(8));
    group.bench_function("first wrap at 80 columns", |b| {
        b.iter_batched(
            || tabs.clone(),
            |tabs| {
                black_box(WrapMap::new(
                    tabs,
                    Box::new(MonospaceMeasure::new(1.0)),
                    Some(COLUMNS),
                ))
            },
            BatchSize::LargeInput,
        );
    });
    let (mut wrap_map, _) = WrapMap::new(
        tabs.clone(),
        Box::new(MonospaceMeasure::new(1.0)),
        Some(COLUMNS),
    );
    let mut wide = false;
    group.bench_function("new width (80 <-> 100 columns)", |b| {
        b.iter(|| {
            wide = !wide;
            black_box(wrap_map.set_wrap_width(Some(if wide { 100.0 } else { COLUMNS })))
        });
    });
    let (mut wrap_map, _) = WrapMap::new(tabs.clone(), Box::new(Proportional), Some(COLUMNS));
    group.bench_function("new width, proportional measure", |b| {
        b.iter(|| {
            wide = !wide;
            black_box(wrap_map.set_wrap_width(Some(if wide { 100.0 } else { COLUMNS })))
        });
    });
    group.bench_function("no width", |b| {
        b.iter_batched(
            || tabs.clone(),
            |tabs| {
                black_box(WrapMap::new(
                    tabs,
                    Box::new(MonospaceMeasure::new(1.0)),
                    None,
                ))
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn keystrokes(c: &mut Criterion) {
    let text = document(DOC_LEN);
    let mut group = c.benchmark_group("sync 10 MiB");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    let mut layers = Layers::new(&text, false);
    let mut i = 0;
    group.bench_function("keystroke, buffer + replace + tab", |b| {
        b.iter(|| {
            layers.keystroke(i);
            i += 1;
        });
    });
    let mut layers = Layers::new(&text, true);
    group.bench_function("keystroke, buffer + replace + tab + wrap", |b| {
        b.iter(|| {
            layers.keystroke(i);
            i += 1;
        });
    });
    let mut editor = make_editor(&text, false);
    group.bench_function("editor keystroke, unwrapped", |b| {
        b.iter(|| {
            type_char(&mut editor, i);
            i += 1;
        });
    });
    let mut editor = make_editor(&text, true);
    group.bench_function("editor keystroke, wrapped", |b| {
        b.iter(|| {
            type_char(&mut editor, i);
            i += 1;
        });
    });
    let mut editor = make_editor(&text, true);
    let mut down = true;
    group.bench_function("editor Down, wrapped", |b| {
        b.iter(|| {
            // Back and forth, so the caret stays in the middle.
            down = !down;
            editor.move_selections(if down { Motion::Down } else { Motion::Up }, false);
        });
    });
    group.finish();
}

fn quantiles(mut times: Vec<Duration>) -> (f64, f64, f64) {
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e6;
    (at(0.5), at(0.99), at(1.0))
}

fn latency_report() {
    let text = document(DOC_LEN);
    eprintln!("\nPer-keystroke latency (10 MiB, wrapped at 80 columns):");
    let mut layers = Layers::new(&text, true);
    let times = (0..2000)
        .map(|i| {
            let start = Instant::now();
            layers.keystroke(i);
            start.elapsed()
        })
        .collect();
    let (p50, p99, max) = quantiles(times);
    eprintln!("layers: p50 {p50:.1} µs, p99 {p99:.1} µs, max {max:.1} µs (2000 keystrokes)");
    let mut editor = make_editor(&text, true);
    let times = (0..2000)
        .map(|i| {
            let start = Instant::now();
            type_char(&mut editor, i);
            start.elapsed()
        })
        .collect();
    let (p50, p99, max) = quantiles(times);
    eprintln!("editor: p50 {p50:.1} µs, p99 {p99:.1} µs, max {max:.1} µs (2000 keystrokes)");

    let tabs = tab_snapshot(&text);
    let (mut wrap_map, snapshot) =
        WrapMap::new(tabs, Box::new(MonospaceMeasure::new(1.0)), Some(COLUMNS));
    eprintln!(
        "\n10 MiB: {} tab rows, {} wrap rows at 80 columns",
        snapshot.tab_snapshot().max_point().row() + 1,
        snapshot.max_point().row().0 + 1
    );
    let mut wrapper =
        gv_editor::display_map::LineWrapper::new(Box::new(MonospaceMeasure::new(1.0)));
    let mut job = wrap_map.defer_wrap_width(Some(100.0)).expect("a new width");
    let mut steps = Vec::new();
    while !job.is_done() {
        let start = Instant::now();
        job.run(&mut wrapper, 1000);
        steps.push(start.elapsed());
    }
    let total: Duration = steps.iter().sum();
    let (p50, p99, max) = quantiles(steps.clone());
    eprintln!(
        "re-wrap job in steps of 1000 rows: {} steps, {:.1} ms in all, step p50 {p50:.1} µs, p99 {p99:.1} µs, max {max:.1} µs",
        steps.len(),
        total.as_secs_f64() * 1e3,
    );
    let start = Instant::now();
    black_box(wrap_map.finish_rewrap(job));
    eprintln!(
        "finish_rewrap: {:.1} µs",
        start.elapsed().as_secs_f64() * 1e6
    );
}

criterion_group!(benches, rewrap, keystrokes);

fn main() {
    benches();
    if std::env::args().any(|arg| arg == "--bench") {
        latency_report();
    }
    Criterion::default().configure_from_args().final_summary();
}

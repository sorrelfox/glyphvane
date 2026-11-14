// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Block-layer benchmarks on a 10 MiB Markdown-like document wrapped at 80
//! columns, with 1,000 blocks (images below their line, tables replacing
//! theirs) spread over it:
//!
//! - one keystroke synced through every layer and the block layer, against
//!   the same without blocks, and through the editor;
//! - resizing one block (an image loaded);
//! - y lookups: the item at a y, the top of a row, the items of a viewport;
//! - inserting the 1,000 blocks, and rebuilding the layer from them.
//!
//! With `--bench`, per-operation percentiles are printed after the
//! Criterion runs. Not run in CI. Run with
//! `cargo bench -p gv-editor --bench blocks`.

use std::hint::black_box;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group};
use gv_editor::display_map::{
    BlockId, BlockMap, BlockPlacement, BlockProperties, Height, MonospaceMeasure, ReplaceMap,
    TabMap, WrapMap, Y,
};
use gv_editor::{Editor, Motion};
use gv_text::{Anchor, Bias, Buffer};

const DOC_LEN: usize = 10 * 1024 * 1024;
const BLOCKS: usize = 1000;
const TAB_SIZE: NonZeroU32 = NonZeroU32::new(4).unwrap();
/// The wrap width in columns of the monospace measure.
const COLUMNS: f32 = 80.0;
const ROW_HEIGHT: f32 = 24.0;
const VIEWPORT: f64 = 1000.0;

/// A Markdown-like document of about `len` bytes (as in the wrap
/// benchmark).
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

/// `BLOCKS` blocks spread evenly over the lines of `buffer`: images
/// (120–220 px) below a line and tables (rows × 30 + 20 px) instead of one.
fn blocks(buffer: &Buffer) -> Vec<BlockProperties> {
    let text = buffer.text();
    let lines = text.line_count();
    (0..BLOCKS)
        .map(|i| {
            let line = (i * lines / BLOCKS).min(lines - 1);
            let range = text.line_range(line).expect("a line");
            if i % 2 == 0 {
                let placement = BlockPlacement::Below(Anchor::after(range.start));
                BlockProperties::new(placement, Height::from_px(120.0 + (i % 101) as f32))
            } else {
                let placement =
                    BlockPlacement::Replace(Anchor::after(range.start)..Anchor::before(range.end));
                BlockProperties::new(placement, Height::from_px(3.0 * 30.0 + 20.0))
            }
        })
        .collect()
}

/// A buffer, every layer over it wrapped at 80 columns, and a block layer
/// with `BLOCKS` blocks or none.
struct Layers {
    buffer: Buffer,
    replace_map: ReplaceMap,
    tab_map: TabMap,
    wrap_map: WrapMap,
    blocks: Option<BlockMap>,
    ids: Vec<BlockId>,
}

impl Layers {
    fn new(text: &str, with_blocks: bool) -> Self {
        let buffer = Buffer::new(text);
        let (replace_map, replace) = ReplaceMap::new(buffer.snapshot());
        let (tab_map, tabs) = TabMap::new(replace, TAB_SIZE);
        let (wrap_map, wrap) =
            WrapMap::new(tabs, Box::new(MonospaceMeasure::new(1.0)), Some(COLUMNS));
        let (blocks, ids) = if with_blocks {
            let mut map = BlockMap::new(wrap, Height::from_px(ROW_HEIGHT));
            let ids = map.insert(blocks(&buffer));
            (Some(map), ids)
        } else {
            (None, Vec::new())
        };
        Self {
            buffer,
            replace_map,
            tab_map,
            wrap_map,
            blocks,
            ids,
        }
    }

    /// Types one character at a different place each time, syncing every
    /// layer.
    fn keystroke(&mut self, i: usize) {
        let offset = self
            .buffer
            .clip_offset((i * 104_729) % self.buffer.len(), Bias::Left);
        let Ok(summary) = self.buffer.edit([(offset..offset, "x")]) else {
            return;
        };
        let (replace, replace_edits) = self.replace_map.sync(self.buffer.snapshot(), &summary);
        let (tabs, tab_edits) = self.tab_map.sync(replace, replace_edits, TAB_SIZE);
        let (wrap, wrap_edits) = self.wrap_map.sync(tabs, tab_edits);
        match &mut self.blocks {
            Some(blocks) => {
                black_box(blocks.sync_wrap(wrap, &wrap_edits, Some(&summary)));
            }
            None => {
                black_box((wrap, wrap_edits));
            }
        }
    }

    fn block_map(&mut self) -> &mut BlockMap {
        self.blocks.as_mut().expect("with blocks")
    }

    /// Resizes block `i % BLOCKS` to a new height.
    fn resize(&mut self, i: usize) {
        let id = self.ids[(i * 7919) % self.ids.len()];
        let height = Height::from_px(100.0 + (i % 97) as f32);
        black_box(self.block_map().resize([(id, height)]));
    }
}

/// An editor in the middle of `text`, wrapped at 80 columns, with the
/// blocks or not.
fn make_editor(text: &str, with_blocks: bool) -> Editor {
    let mut editor = Editor::new(Buffer::new(text));
    editor.set_wrap_width(Some(COLUMNS));
    editor.set_row_height(Height::from_px(ROW_HEIGHT));
    if with_blocks {
        let blocks = blocks(editor.buffer());
        editor.insert_blocks(blocks);
    }
    editor.set_cursor(text.len() / 2);
    editor.move_selections(Motion::LineEnd, false);
    editor
}

const TYPED: &str = "the quick brown fox jumps over the lazy dog\n";

fn type_char(editor: &mut Editor, i: usize) {
    let at = i % TYPED.len();
    editor.insert_at_selections(&TYPED[at..at + 1]);
}

/// A y in the document for iteration `i`.
fn y_at(total: Y, i: usize) -> Y {
    Y((i as u64).wrapping_mul(2_654_435_761) % total.0.max(1))
}

fn keystrokes(c: &mut Criterion) {
    let text = document(DOC_LEN);
    let mut group = c.benchmark_group("blocks 10 MiB, sync");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    let mut i = 0;
    let mut layers = Layers::new(&text, false);
    group.bench_function("keystroke, every layer, no blocks", |b| {
        b.iter(|| {
            layers.keystroke(i);
            i += 1;
        });
    });
    let mut layers = Layers::new(&text, true);
    group.bench_function("keystroke, every layer, 1000 blocks", |b| {
        b.iter(|| {
            layers.keystroke(i);
            i += 1;
        });
    });
    group.bench_function("resize one block", |b| {
        b.iter(|| {
            layers.resize(i);
            i += 1;
        });
    });
    let mut editor = make_editor(&text, false);
    group.bench_function("editor keystroke, wrapped, no blocks", |b| {
        b.iter(|| {
            type_char(&mut editor, i);
            i += 1;
        });
    });
    let mut editor = make_editor(&text, true);
    group.bench_function("editor keystroke, wrapped, 1000 blocks", |b| {
        b.iter(|| {
            type_char(&mut editor, i);
            i += 1;
        });
    });
    group.finish();
}

fn lookups(c: &mut Criterion) {
    let text = document(DOC_LEN);
    let layers = Layers::new(&text, true);
    let blocks = layers.blocks.as_ref().expect("with blocks").snapshot();
    let total = blocks.total_height();
    let rows = blocks.row_count();
    let mut group = c.benchmark_group("blocks 10 MiB, lookups");
    group.sample_size(30);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    let mut i = 0usize;
    group.bench_function("item at y", |b| {
        b.iter(|| {
            i += 1;
            black_box(blocks.item_at(y_at(total, i)))
        });
    });
    group.bench_function("top of row", |b| {
        b.iter(|| {
            i += 1;
            black_box(blocks.row_top((i as u32).wrapping_mul(2_654_435_761) % rows))
        });
    });
    group.bench_function("items of a 1000 px viewport", |b| {
        b.iter(|| {
            i += 1;
            let top = y_at(total, i);
            let bottom = Y(top.0 + Y::from_px(VIEWPORT).0);
            black_box(blocks.items(top..bottom).count())
        });
    });
    group.finish();

    let mut group = c.benchmark_group("blocks 10 MiB, build");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));
    let wrap = layers.wrap_map.snapshot();
    let properties = blocks_of(&layers);
    group.bench_function("insert 1000 blocks", |b| {
        b.iter_batched(
            || BlockMap::new(wrap.clone(), Height::from_px(ROW_HEIGHT)),
            |mut map| black_box(map.insert(properties.clone())),
            BatchSize::LargeInput,
        );
    });
    let mut map = layers.blocks.clone().expect("with blocks");
    group.bench_function("rebuild with 1000 blocks", |b| {
        b.iter(|| map.rebuild());
    });
    group.finish();
}

fn blocks_of(layers: &Layers) -> Vec<BlockProperties> {
    blocks(&layers.buffer)
}

fn quantiles(mut times: Vec<Duration>) -> (f64, f64, f64) {
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e6;
    (at(0.5), at(0.99), at(1.0))
}

fn report(name: &str, times: Vec<Duration>) {
    let count = times.len();
    let (p50, p99, max) = quantiles(times);
    eprintln!("{name}: p50 {p50:.2} µs, p99 {p99:.2} µs, max {max:.2} µs ({count} runs)");
}

fn timed(runs: usize, mut f: impl FnMut(usize)) -> Vec<Duration> {
    (0..runs)
        .map(|i| {
            let start = Instant::now();
            f(i);
            start.elapsed()
        })
        .collect()
}

fn latency_report() {
    let text = document(DOC_LEN);
    let mut layers = Layers::new(&text, true);
    {
        let blocks = layers.block_map().snapshot();
        eprintln!(
            "\n10 MiB: {} wrap rows, {} blocks shown, {:.0} px tall",
            blocks.row_count(),
            blocks.block_count(),
            blocks.total_height().to_px()
        );
    }
    let mut plain = Layers::new(&text, false);
    report(
        "keystroke, every layer, no blocks",
        timed(2000, |i| plain.keystroke(i)),
    );
    report(
        "keystroke, every layer, 1000 blocks",
        timed(2000, |i| layers.keystroke(i)),
    );
    report("resize one block", timed(2000, |i| layers.resize(i)));
    let blocks = layers.block_map().snapshot().clone();
    let total = blocks.total_height();
    report(
        "item at y",
        timed(10_000, |i| {
            black_box(blocks.item_at(y_at(total, i)));
        }),
    );
    report(
        "items of a 1000 px viewport",
        timed(10_000, |i| {
            let top = y_at(total, i);
            black_box(blocks.items(top..Y(top.0 + Y::from_px(VIEWPORT).0)).count());
        }),
    );
    let mut editor = make_editor(&text, false);
    report(
        "editor keystroke, no blocks",
        timed(2000, |i| type_char(&mut editor, i)),
    );
    let mut editor = make_editor(&text, true);
    report(
        "editor keystroke, 1000 blocks",
        timed(2000, |i| type_char(&mut editor, i)),
    );
    let mut down = true;
    report(
        "editor Down/Up, 1000 blocks",
        timed(2000, |_| {
            down = !down;
            editor.move_selections(if down { Motion::Down } else { Motion::Up }, false);
        }),
    );
    report(
        "editor page by 1000 px, 1000 blocks",
        timed(2000, |i| {
            let dy = if i % 2 == 0 { VIEWPORT } else { -VIEWPORT };
            editor.move_selections_by_pixels(dy as f32, false);
        }),
    );
    let wrap = layers.wrap_map.snapshot();
    let properties = blocks_of(&layers);
    let start = Instant::now();
    let mut map = BlockMap::new(wrap, Height::from_px(ROW_HEIGHT));
    map.insert(properties);
    eprintln!(
        "insert 1000 blocks: {:.2} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
    let start = Instant::now();
    map.rebuild();
    eprintln!(
        "rebuild with 1000 blocks: {:.2} ms",
        start.elapsed().as_secs_f64() * 1e3
    );
}

criterion_group!(benches, keystrokes, lookups);

fn main() {
    benches();
    if std::env::args().any(|arg| arg == "--bench") {
        latency_report();
    }
    Criterion::default().configure_from_args().final_summary();
}

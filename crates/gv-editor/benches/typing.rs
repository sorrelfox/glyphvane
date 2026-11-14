// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Typing benchmarks: the cost of one keystroke in the editor model (rope
//! edit, selections, undo grouping, display-map sync), alone and with the
//! incremental reparse attached as an observer.
//!
//! - 1,000 characters typed at 100 cursors (one transaction per keystroke),
//!   and the same transactions on a bare buffer, and on a buffer with the
//!   display map, as baselines;
//! - single-cursor typing in the middle of a 10 MiB document;
//! - the same with the parser observer (the edit path without rendering);
//! - undoing a 100-cursor typing step.
//!
//! With `--bench`, per-keystroke percentiles are printed after the
//! Criterion runs. Not run in CI. Run with `cargo bench -p gv-editor`.

use std::cell::RefCell;
use std::hint::black_box;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use criterion::{BatchSize, Criterion, criterion_group};
use gv_editor::display_map::{ReplaceMap, TabMap};
use gv_editor::{Editor, EditorEvent, Motion};
use gv_syntax::{Budget, Reparse, SyntaxTree};
use gv_text::{Buffer, Transaction};

const DOC_LEN: usize = 10 * 1024 * 1024;

/// A Markdown-like document of about `len` bytes.
fn document(len: usize) -> String {
    const PIECES: &[&str] = &[
        "# Heading with **bold** text\n\n",
        "A paragraph with *emphasis*, `code`, a [[wikilink]] and a [link](https://example.com).\n",
        "- [ ] task item with émoji 🎉 and ünïcödé\n",
        "> quoted line 引用文\r\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "Plain prose continues here without anything special at all.\n\n",
    ];
    let mut text = String::with_capacity(len + 128);
    let mut i = 0usize;
    while text.len() < len {
        text.push_str(PIECES[i % PIECES.len()]);
        i = i.wrapping_mul(31).wrapping_add(7);
    }
    text
}

/// 100 lines with a caret at the end of each.
fn hundred_cursors() -> Editor {
    let text: String = (0..100)
        .map(|i| format!("line {i} with some words in it\n"))
        .collect();
    let mut editor = Editor::new(Buffer::new(&text));
    for line in 0..100 {
        let end = editor.text().line_range(line).map_or(0, |range| range.end);
        editor.add_cursor(end);
    }
    editor
}

const TAB_SIZE: NonZeroU32 = NonZeroU32::new(4).unwrap();

/// The buffer and caret offsets of [`hundred_cursors`].
fn bare_hundred_cursors() -> (Buffer, Vec<usize>) {
    let editor = hundred_cursors();
    let carets = editor.selections().iter().map(|s| s.head()).collect();
    (editor.into_buffer(), carets)
}

/// The transaction of keystroke `i` at `carets`, which move past it.
fn bare_keystroke(carets: &mut [usize], i: usize) -> Transaction {
    let at = i % TYPED.len();
    let mut transaction = Transaction::new();
    for &caret in carets.iter() {
        transaction.push(caret..caret, &TYPED[at..at + 1]);
    }
    for (k, caret) in carets.iter_mut().enumerate() {
        *caret += k + 1;
    }
    transaction
}

fn big_document() -> Editor {
    let mut editor = Editor::new(Buffer::new(&document(DOC_LEN)));
    editor.set_cursor(DOC_LEN / 2);
    editor.move_selections(Motion::LineEnd, false);
    editor
}

/// Attaches the incremental parser as an observer.
fn with_parser(editor: &mut Editor) {
    let tree = Rc::new(RefCell::new(SyntaxTree::parse(&editor.snapshot())));
    editor.observe(move |event| {
        if let EditorEvent::Edited(edit) = event {
            let mut tree = tree.borrow_mut();
            let next = match tree.reparse(
                edit.text,
                edit.summary,
                Budget::from_now(Duration::from_millis(1)),
            ) {
                Reparse::Done { tree, .. } => tree,
                Reparse::Partial { mut task, .. } => {
                    task.run(edit.text, Budget::Unlimited);
                    task.finish().0
                }
            };
            *tree = next;
        }
    });
}

/// Prose with line breaks, so typed lines stay short (the parser rescans
/// the edited line).
const TYPED: &str = "the quick brown fox jumps over the lazy dog\n";

fn keystroke(editor: &mut Editor, i: usize) {
    let c = TYPED.as_bytes()[i % TYPED.len()] as char;
    let mut buf = [0u8; 4];
    editor.insert_at_selections(c.encode_utf8(&mut buf));
}

fn typing(c: &mut Criterion) {
    let mut group = c.benchmark_group("typing");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("100 cursors x 1000 chars", |b| {
        b.iter_batched(
            hundred_cursors,
            |mut editor| {
                for i in 0..1000 {
                    keystroke(&mut editor, i);
                }
                black_box(editor.text().len())
            },
            BatchSize::LargeInput,
        );
    });

    // Baselines: the same keystrokes as bare buffer transactions (rope and
    // line index), then with the display map synced as well; carets are
    // moved by hand.
    group.bench_function("100 cursors x 1000 chars, buffer only", |b| {
        b.iter_batched(
            bare_hundred_cursors,
            |(mut buffer, mut carets)| {
                for i in 0..1000 {
                    let _ = buffer.apply(&bare_keystroke(&mut carets, i));
                }
                black_box(buffer.len())
            },
            BatchSize::LargeInput,
        );
    });
    group.bench_function("100 cursors x 1000 chars, buffer + display map", |b| {
        b.iter_batched(
            || {
                let (buffer, carets) = bare_hundred_cursors();
                let (replace_map, replace) = ReplaceMap::new(buffer.snapshot());
                let (tab_map, _) = TabMap::new(replace, TAB_SIZE);
                (buffer, carets, replace_map, tab_map)
            },
            |(mut buffer, mut carets, mut replace_map, mut tab_map)| {
                for i in 0..1000 {
                    let Ok(summary) = buffer.apply(&bare_keystroke(&mut carets, i)) else {
                        continue;
                    };
                    let (replace, edits) = replace_map.sync(buffer.snapshot(), &summary);
                    black_box(tab_map.sync(replace, edits, TAB_SIZE));
                }
                black_box(buffer.len())
            },
            BatchSize::LargeInput,
        );
    });

    let mut editor = big_document();
    let mut i = 0;
    group.bench_function("10 MiB single cursor, per keystroke", |b| {
        b.iter(|| {
            keystroke(&mut editor, i);
            i += 1;
        });
    });

    let mut editor = big_document();
    with_parser(&mut editor);
    let mut i = 0;
    group.bench_function("10 MiB single cursor + reparse, per keystroke", |b| {
        b.iter(|| {
            keystroke(&mut editor, i);
            i += 1;
        });
    });
    group.finish();

    let mut group = c.benchmark_group("undo");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.bench_function("100-cursor step of 40 chars", |b| {
        b.iter_batched(
            || {
                let mut editor = hundred_cursors();
                for i in 0..40 {
                    keystroke(&mut editor, i % 3);
                }
                editor
            },
            |mut editor| black_box(editor.undo()),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

/// Per-keystroke latency percentiles in microseconds.
fn percentiles(name: &str, editor: &mut Editor, keystrokes: usize) {
    let mut times: Vec<Duration> = (0..keystrokes)
        .map(|i| {
            let start = Instant::now();
            keystroke(editor, i);
            start.elapsed()
        })
        .collect();
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e6;
    eprintln!(
        "{name}: p50 {:.1} µs, p99 {:.1} µs, max {:.1} µs ({keystrokes} keystrokes)",
        at(0.5),
        at(0.99),
        at(1.0)
    );
}

fn latency_report() {
    eprintln!("\nPer-keystroke latency:");
    percentiles("100 cursors", &mut hundred_cursors(), 1000);
    percentiles("10 MiB single cursor", &mut big_document(), 2000);
    let mut editor = big_document();
    with_parser(&mut editor);
    percentiles("10 MiB single cursor + reparse", &mut editor, 2000);
}

criterion_group!(benches, typing);

fn main() {
    benches();
    if std::env::args().any(|arg| arg == "--bench") {
        latency_report();
    }
    Criterion::default().configure_from_args().final_summary();
}

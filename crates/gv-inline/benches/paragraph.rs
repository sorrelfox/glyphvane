// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Layout of a 1,000-word paragraph with the monospace fake measurer:
//! cold (build, shape through a fresh run cache, lay out) and re-wrapped
//! from the cached shaping at new widths, plus hit testing and selection
//! rectangles. With a real text system, cold shaping costs more; the other
//! numbers are this crate's own cost. Not run in CI. Run with
//! `cargo bench -p gv-inline`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use gv_inline::{
    Background, BoxKind, ColorRole, FakeMono, InlineBox, Layout, LineHeight, Paragraph,
    ParagraphBuilder, ParagraphStyle, RunCache, RunStyle, Shaped, StyleId, Underline,
    UnderlineStyle,
};

const WORDS: &[&str] = &[
    "the",
    "inline",
    "formatting",
    "context",
    "lays",
    "out",
    "styled",
    "runs",
    "and",
    "atomic",
    "boxes",
    "on",
    "lines",
    "breaking",
    "at",
    "opportunities",
    "found",
    "by",
    "unicode",
    "rules",
    "while",
    "keeping",
    "a",
    "shared",
    "baseline",
    "for",
    "math",
    "images",
    "checkboxes",
    "with",
    "carets",
    "selection",
    "hit",
    "testing",
    "paint",
];

const WIDTHS: [f32; 6] = [680.0, 600.0, 520.0, 440.0, 360.0, 300.0];

/// Body 15/24, as in the preview. `mixed` switches style every six words
/// (bold, italic, link, inline code) and adds a math box every 50 words.
fn paragraph(words: usize, mixed: bool) -> Paragraph {
    let body = RunStyle::new(StyleId(0), 15.0);
    let bold = RunStyle::new(StyleId(1), 15.0);
    let italic = RunStyle::new(StyleId(2), 15.0);
    let link = body.with_underline(Underline::new(
        UnderlineStyle::Solid,
        ColorRole(2),
        3.0,
        1.0,
    ));
    let code = RunStyle::new(StyleId(3), 13.0).with_background(Background {
        role: ColorRole(3),
        padding_x: 5.0,
        padding_y: 1.0,
        radius: 4.0,
    });
    let styles = [body, bold, body, italic, body, link, body, code];
    let mut b = ParagraphBuilder::new(ParagraphStyle::new(StyleId(0), 15.0, LineHeight::Px(24.0)));
    let mut run = String::new();
    let mut style = body;
    let mut src = 0;
    let flush = |b: &mut ParagraphBuilder, run: &mut String, style: RunStyle, src: &mut usize| {
        let text = std::mem::take(run);
        b.text(&text, style, *src..*src + text.len());
        *src += text.len() + 2;
    };
    for i in 0..words {
        let word = WORDS[(i * 7 + i / 3) % WORDS.len()];
        if mixed && i > 0 && i % 6 == 0 {
            flush(&mut b, &mut run, style, &mut src);
            style = styles[(i / 6) % styles.len()];
            if i % 50 == 0 {
                b.inline_box(InlineBox::new(BoxKind::Math, 30.0, 12.0, 4.0), src..src + 5);
                src += 5;
                run.push(' ');
            }
        }
        run.push_str(word);
        run.push(if i + 1 == words { '.' } else { ' ' });
    }
    flush(&mut b, &mut run, style, &mut src);
    b.build()
}

fn shaped(p: &Arc<Paragraph>) -> Arc<Shaped> {
    let mut m = FakeMono::default();
    let mut cache = RunCache::default();
    Arc::new(Shaped::new(p.clone(), &mut m, &mut cache))
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("paragraph_1000_words");
    group.sample_size(30);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    for mixed in [false, true] {
        let name = if mixed { "mixed" } else { "one_run" };
        let p = Arc::new(paragraph(1000, mixed));

        group.bench_function(format!("{name}/build"), |b| {
            b.iter(|| black_box(paragraph(1000, mixed)));
        });
        group.bench_function(format!("{name}/cold_build_shape_layout"), |b| {
            b.iter(|| {
                let p = Arc::new(paragraph(1000, mixed));
                let mut m = FakeMono::default();
                let mut cache = RunCache::default();
                let s = Arc::new(Shaped::new(p, &mut m, &mut cache));
                black_box(Layout::new(s, 680.0))
            });
        });
        let s = shaped(&p);
        let mut i = 0;
        group.bench_function(format!("{name}/rewrap"), |b| {
            b.iter(|| {
                i += 1;
                black_box(Layout::new(s.clone(), WIDTHS[i % WIDTHS.len()]))
            });
        });
        let l = Layout::new(s.clone(), 680.0);
        let mut k = 0usize;
        group.bench_function(format!("{name}/hit"), |b| {
            b.iter(|| {
                k += 1;
                let x = (k * 37 % 700) as f32;
                let y = (k * 53) as f32 % l.height();
                black_box(l.hit(x, y))
            });
        });
        group.bench_function(format!("{name}/selection_rects_all"), |b| {
            b.iter(|| black_box(l.selection_rects(1, p.len() - 1)));
        });
        group.bench_function(format!("{name}/caret_for_every_stop"), |b| {
            let stops: Vec<usize> = s.caret_stops().collect();
            b.iter(|| {
                for &o in &stops {
                    black_box(l.caret(o, gv_inline::Affinity::Downstream));
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);

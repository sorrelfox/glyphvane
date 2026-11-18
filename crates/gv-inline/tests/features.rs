// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Alignment, affinity, decorations, caret stops, source mapping, box
//! alignment, metrics rules and the caches.

mod common;

use std::sync::Arc;

use common::*;
use gv_inline::{
    Affinity, Align, Bias, BlockCache, BoxAlign, BoxKind, ColorRole, DecorationKind, FakeMono,
    InlineBox, Layout, ParagraphBuilder, RunCache, Strikethrough, Underline, UnderlineStyle,
};

fn link(style: UnderlineStyle) -> gv_inline::RunStyle {
    body().with_underline(Underline::new(style, ColorRole(3), 3.0, 1.0))
}

#[test]
fn center_and_right_alignment_ignore_hanging_space() {
    let pieces = || vec![text("aaa bbb ccc", body())];
    let center = Fixture::with_style(pieces(), paragraph_style().with_align(Align::Center));
    let l = center.layout(50.0); // "aaa bbb " (42 + hanging space) | "ccc" (18)
    assert_eq!(l.lines()[0].x, 4.0);
    assert_eq!(l.lines()[1].x, 16.0);
    assert_eq!(l.line_fragments(1)[0].x, 16.0);
    let right = Fixture::with_style(pieces(), paragraph_style().with_align(Align::Right));
    let l = right.layout(50.0);
    assert_eq!((l.lines()[0].x, l.lines()[1].x), (8.0, 32.0));
    // Carets and hits follow the offset.
    assert_eq!(l.caret(8, Affinity::Downstream).x, 32.0);
    assert_eq!(l.hit(33.0, 20.0).offset, 8);
    assert_eq!(l.hit(0.0, 20.0).offset, 8, "left of the line: its start");
    // An empty line sits at the alignment offset; max-content is not aligned.
    let empty = Fixture::with_style(vec![], paragraph_style().with_align(Align::Center));
    assert_eq!(empty.layout(50.0).caret(0, Affinity::Downstream).x, 25.0);
    assert_eq!(right.layout(f32::INFINITY).lines()[0].x, 0.0);
}

#[test]
fn affinity_picks_the_line_at_a_soft_wrap_but_not_after_a_hard_break() {
    let f = Fixture::new(vec![text("aaa bbb", body())]);
    let l = f.layout(30.0); // "aaa " | "bbb"
    let up = l.caret(4, Affinity::Upstream);
    let down = l.caret(4, Affinity::Downstream);
    assert_eq!((up.line, up.x), (0, 24.0), "after the hanging space");
    assert_eq!((down.line, down.x), (1, 0.0));
    let h = l.hit(500.0, 1.0);
    assert_eq!((h.offset, h.affinity, h.line), (4, Affinity::Upstream, 0));

    let f = Fixture::new(vec![text("ab", body()), Piece::Break, text("cd", body())]);
    let l = f.layout(1000.0);
    // After the break: always the next line.
    assert_eq!(l.caret(3, Affinity::Upstream).line, 1);
    // Past the end of the first line: before the break.
    let h = l.hit(500.0, 1.0);
    assert_eq!((h.offset, h.line), (2, 0));
    assert_eq!(l.line_end(0), 2);
}

#[test]
fn affinity_at_an_item_boundary_picks_the_side_of_the_padding() {
    let f = Fixture::new(vec![text("ab", body()), text("cd", code())]);
    let l = f.layout(1000.0);
    // "ab" ends at 12; the chip's padding runs to 17 where "cd" starts.
    assert_eq!(l.caret(2, Affinity::Upstream).x, 12.0);
    assert_eq!(l.caret(2, Affinity::Downstream).x, 17.0);
    // A click in the padding takes the nearer of the two carets.
    let h = l.hit(14.0, 1.0);
    assert_eq!(
        (h.offset, h.affinity, h.item),
        (2, Affinity::Upstream, Some(0))
    );
    let h = l.hit(15.0, 1.0);
    assert_eq!(
        (h.offset, h.affinity, h.item),
        (2, Affinity::Downstream, Some(1))
    );
}

#[test]
fn decorations_merge_across_runs_and_skip_hanging_space() {
    let solid = link(UnderlineStyle::Solid);
    let mut bold_link = solid;
    bold_link.font = bold().font;
    let f = Fixture::new(vec![
        text("see ", body()),
        text("my ", solid),
        text("link", bold_link),
        text(" now ", link(UnderlineStyle::Dashed)),
        text(
            "gone",
            body().with_strikethrough(Strikethrough {
                color: ColorRole(4),
                thickness: 1.0,
            }),
        ),
    ]);
    let l = f.layout(1000.0);
    let d = l.decorations(0);
    let underlines: Vec<_> = d
        .iter()
        .filter_map(|d| match d.kind {
            DecorationKind::Underline { style, .. } => Some((style, d.rect.x, d.rect.width)),
            _ => None,
        })
        .collect();
    // "my link" is one solid segment across two runs; " now " is dashed.
    assert_eq!(
        underlines,
        [
            (UnderlineStyle::Solid, 24.0, 42.0),
            (UnderlineStyle::Dashed, 66.0, 30.0)
        ]
    );
    let u = d
        .iter()
        .find(|d| matches!(d.kind, DecorationKind::Underline { .. }));
    assert_eq!(
        u.map(|u| (u.rect.y, u.rect.height)),
        Some((11.0 + 3.0, 1.0))
    );
    let s = d
        .iter()
        .find(|d| matches!(d.kind, DecorationKind::Strikethrough { .. }));
    // Middle of the x-height (5 px) above the baseline, minus half the thickness.
    assert_eq!(
        s.map(|s| (s.rect.x, s.rect.y)),
        Some((96.0, 11.0 - 2.5 - 0.5))
    );

    // Wrapped after "my ": the hanging space is not underlined.
    let l = f.layout(45.0);
    let first: Vec<_> = l.decorations(0).iter().map(|d| d.rect.right()).collect();
    assert_eq!(first, [36.0], "{:?}", l.decorations(0));
}

#[test]
fn backgrounds_round_only_the_outer_ends() {
    let f = Fixture::new(vec![
        text("a ", body()),
        text("xy zw", code()),
        text(" b", body()),
    ]);
    let l = f.layout(40.0); // "a " + "xy " | "zw" + " b"
    let ends = |line| {
        l.decorations(line)
            .iter()
            .filter_map(|d| match d.kind {
                DecorationKind::Background {
                    round_left,
                    round_right,
                    ..
                } => Some((round_left, round_right, d.rect.x, d.rect.right())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // The first piece ends before its hanging space; the second gets the
    // right padding.
    assert_eq!(ends(0), [(true, false, 12.0, 29.0)]);
    assert_eq!(ends(1), [(false, true, 0.0, 17.0)]);
    let bg = l.decorations(0)[0].rect;
    // ascent 8 + descent 2 + 2 × padding 1, from 8 + 1 above the baseline.
    assert_eq!((bg.y, bg.height), (11.0 - 9.0, 12.0));
}

#[test]
fn caret_stops_are_graphemes_items_and_box_sides() {
    let f = Fixture::new(vec![
        text("e\u{301}x", body()),
        math(10.0, 5.0, 1.0),
        Piece::Break,
        text("👍🏽!", bold()),
    ]);
    let stops: Vec<usize> = f.shaped.caret_stops().collect();
    // é (3 bytes) | x | box (3) | \n | 👍🏽 (8) | !
    assert_eq!(stops, [0, 3, 4, 7, 8, 16, 17]);
    let mut forward = vec![0];
    while *forward.last().unwrap_or(&0) < f.paragraph.len() {
        let next = f.shaped.next_stop(*forward.last().unwrap_or(&0));
        forward.push(next);
    }
    assert_eq!(forward, stops);
    let mut backward = vec![f.paragraph.len()];
    while *backward.last().unwrap_or(&0) > 0 {
        let prev = f.shaped.prev_stop(*backward.last().unwrap_or(&0));
        backward.push(prev);
    }
    backward.reverse();
    assert_eq!(backward, stops);
    assert_eq!(f.shaped.snap(1, Bias::Left), 0);
    assert_eq!(f.shaped.snap(1, Bias::Right), 3);
    assert_eq!(f.shaped.snap(5, Bias::Right), 7, "inside the box");
    assert_eq!(f.shaped.snap(10, Bias::Left), 8, "inside the emoji");
}

#[test]
fn caret_inside_a_ligature_snaps_to_its_edges() {
    let measure = FakeMono {
        ligatures: true,
        ..FakeMono::default()
    };
    let p = build(&[text("a fine", body())], paragraph_style());
    let f = Fixture::from_paragraph(p, measure);
    let stops: Vec<usize> = f.shaped.caret_stops().collect();
    assert_eq!(stops, [0, 1, 2, 4, 5, 6], "no stop between f and i");
    let l = f.layout(1000.0);
    assert_eq!(
        l.caret(3, Affinity::Downstream).x,
        l.caret(2, Affinity::Downstream).x
    );
    assert_eq!(
        l.hit(18.0, 1.0).offset,
        2,
        "middle of the ligature: nearer edge"
    );
    assert_eq!(l.hit(19.0, 1.0).offset, 4);
}

#[test]
fn zero_width_space_breaks_and_nbsp_does_not() {
    let f = Fixture::new(vec![text("aaaa\u{200B}bbbb", body())]);
    assert_eq!(f.lines(30.0), ["aaaa\u{200B}", "bbbb"]);
    let f = Fixture::new(vec![text("aa\u{A0}bb cc", body())]);
    assert_eq!(f.lines(40.0), ["aa\u{A0}bb", "cc"]);
    // Only overflow-wrap splits at the no-break space, which stays on the
    // first line (`lines` trims it: it is white space to `str::trim_end`).
    assert_eq!(f.lines(20.0), ["aa", "bb", "cc"]);
    assert_eq!(f.layout(20.0).lines()[0].range, 0..4);
}

#[test]
fn source_offsets_through_the_layout() {
    // Source: `a **b** \*c` → "a " 0..2, "b" 4..5, " *c" from ` \*c` 7..11.
    let mut b = ParagraphBuilder::new(paragraph_style());
    b.text("a ", body(), 0..2);
    b.text("b", bold(), 4..5);
    b.text_mapped(" *c", body(), 7..11, &[(1, 8), (2, 10)]);
    let f = Fixture::from_paragraph(b.build(), FakeMono::default());
    let l = f.layout(1000.0);
    // Left half of "b": its start, from the bold side of the `**`.
    let h = l.hit(13.0, 1.0);
    assert_eq!((h.offset, h.item, h.source), (2, Some(1), 4));
    // Right half of " ": end of the first item, before the `**`.
    let h = l.hit(11.0, 1.0);
    assert_eq!((h.offset, h.item, h.source), (2, Some(0), 2));
    // Right half of "*": after the escape.
    let h = l.hit(28.0, 1.0);
    assert_eq!((h.offset, h.source), (5, 10));
    // Source → caret: inside the `**` the caret is at the boundary; inside
    // the escape, at either edge of "*".
    assert_eq!(l.caret_for_source(3, Bias::Left).x, 12.0);
    assert_eq!(l.caret_for_source(9, Bias::Left).x, 24.0);
    assert_eq!(l.caret_for_source(9, Bias::Right).x, 30.0);
    // A selection over the source of "b" and the escape.
    let rects = l.selection_rects_for_source(4..9);
    assert_eq!(rects.len(), 1);
    assert_eq!((rects[0].x, rects[0].right()), (12.0, 30.0));
}

#[test]
fn box_alignment_against_the_strut() {
    let boxed = |align| {
        Fixture::new(vec![
            text("x", body()),
            Piece::Box(InlineBox::new(BoxKind::Image, 10.0, 12.0, 0.0).with_align(align)),
        ])
    };
    let extent = |f: &Fixture| {
        let l = f.layout(1000.0);
        let b = &l.fragments()[1];
        (b.ascent, b.descent)
    };
    assert_eq!(extent(&boxed(BoxAlign::Baseline)), (12.0, 0.0));
    // x-height 5: the middle sits 2.5 px above the baseline.
    assert_eq!(extent(&boxed(BoxAlign::Middle)), (8.5, 3.5));
    assert_eq!(extent(&boxed(BoxAlign::TextTop)), (8.0, 4.0));
    assert_eq!(extent(&boxed(BoxAlign::TextBottom)), (10.0, 2.0));
}

#[test]
fn line_metrics_come_from_the_primary_font_only() {
    // Emoji and CJK come from fallback faces in a real text system; their
    // runs still use the style's primary metrics, so the line keeps the
    // strut's height. A smaller run (inline code at 8 px) does not grow it.
    let mut small = code();
    small.size = 8.0;
    let f = Fixture::new(vec![
        text("a 👩\u{200D}👧 日本 ", body()),
        text("x()", small),
    ]);
    let l = f.layout(1000.0);
    assert_eq!((l.lines()[0].height, l.lines()[0].baseline), (16.0, 11.0));
}

#[test]
fn hover_regions_follow_fragments() {
    let f = Fixture::new(vec![
        text("go ", body()),
        text("to the place", link(UnderlineStyle::Solid)),
    ]);
    let l = f.layout(60.0); // "go to the " | "place"
    assert_eq!(l.item_at_point(5.0, 3.0), Some(0));
    assert_eq!(l.item_at_point(20.0, 3.0), Some(1));
    assert_eq!(l.item_at_point(40.0, 20.0), None, "right of the last line");
    assert_eq!(l.item_at_point(5.0, -1.0), None);
    let rects = l.item_rects(1);
    assert_eq!(rects.len(), 2);
    assert_eq!((rects[1].x, rects[1].y, rects[1].width), (0.0, 16.0, 30.0));
}

#[test]
fn intrinsic_widths() {
    let f = Fixture::new(vec![
        text("ab abcdef ", body()),
        math(50.0, 5.0, 1.0),
        Piece::Break,
        text("xy", body()),
    ]);
    // Widest unbreakable piece: the box (50); widest line: "ab abcdef ☐".
    assert_eq!(f.shaped.min_content_width(), 50.0);
    assert_eq!(f.shaped.max_content_width(), 110.0);
    assert_eq!(f.layout(f32::INFINITY).width(), 110.0);
}

#[test]
fn empty_paragraph_has_one_strut_line() {
    let f = Fixture::new(vec![]);
    let l = f.layout(100.0);
    assert_eq!(l.lines().len(), 1);
    assert_eq!(l.height(), 16.0);
    let h = l.hit(50.0, 50.0);
    assert_eq!((h.offset, h.item), (0, None));
    let c = l.caret(0, Affinity::Downstream);
    assert_eq!((c.x, c.top, c.height), (0.0, 3.0, 10.0));
    assert!(l.selection_rects(0, 0).is_empty());
}

#[test]
fn culling_range_and_run_origin() {
    let f = Fixture::new(vec![text("aa bb cc dd", body().with_letter_spacing(1.0))]);
    let l = f.layout(20.0); // four lines of 16 px
    assert_eq!(l.lines().len(), 4);
    assert_eq!(l.lines_between(20.0, 40.0), 1..3);
    assert_eq!(l.lines_between(-10.0, 0.0), 0..0);
    assert_eq!(l.lines_between(60.0, 1000.0), 3..4);
    // The second line's run starts 3 clusters × (6 + 1) to the left.
    let frag = &l.line_fragments(1)[0];
    assert_eq!(l.run_origin_x(frag), -21.0);
}

#[test]
fn block_cache_serves_the_probe_and_real_widths() {
    let paragraph = Arc::new(build(
        &[text("one two three four five six", body())],
        paragraph_style(),
    ));
    let mut block = BlockCache::new(paragraph);
    let mut measure = FakeMono::default();
    let mut runs = RunCache::default();
    // Three frames, each measuring at max-content and at the real width.
    for _ in 0..3 {
        let probe = block.layout(f32::INFINITY, &mut measure, &mut runs);
        let real = block.layout(60.0, &mut measure, &mut runs);
        assert_eq!(probe.lines().len(), 1);
        assert!(real.lines().len() > 1);
    }
    let stats = block.stats();
    assert_eq!((stats.shapings, stats.layouts, stats.hits), (1, 2, 4));
    assert_eq!(measure.shaped, 1);
    // Another block with the same text reuses the shaped run.
    let mut other = BlockCache::new(block.paragraph().clone());
    other.layout(80.0, &mut measure, &mut runs);
    assert_eq!(measure.shaped, 1);
    assert_eq!(runs.stats().hits, 1);
    // NaN widths are treated as 0 and cached as such.
    let a: Arc<Layout> = block.layout(f32::NAN, &mut measure, &mut runs);
    let b = block.layout(0.0, &mut measure, &mut runs);
    assert!(Arc::ptr_eq(&a, &b));
    assert!(block.heap_bytes() > 0);
}

#[test]
fn bad_lengths_are_sanitized() {
    let nan = f32::NAN;
    let style = gv_inline::ParagraphStyle::new(
        gv_inline::StyleId(0),
        nan,
        gv_inline::LineHeight::Px(f32::INFINITY),
    );
    let mut run = body()
        .with_letter_spacing(nan)
        .with_baseline_shift(f32::INFINITY);
    run.size = -3.0;
    let mut b = ParagraphBuilder::new(style);
    b.text("ab cd", run, 0..5);
    b.inline_box(
        InlineBox::new(BoxKind::Math, nan, nan, 2.0).with_margins(-5.0, f32::INFINITY),
        5..8,
    );
    b.text("ef", body(), 8..10);
    let f = Fixture::from_paragraph(b.build(), FakeMono::default());
    for width in [nan, -1.0, 0.0, 30.0, f32::INFINITY] {
        let l = f.layout(width);
        assert!(l.height().is_finite() && l.width().is_finite());
        for frag in l.fragments() {
            let values = [
                frag.x,
                frag.width,
                frag.ascent,
                frag.descent,
                frag.pad_right,
            ];
            assert!(values.iter().all(|v| v.is_finite()), "{frag:?}");
        }
        for o in f.shaped.caret_stops() {
            let c = l.caret(o, Affinity::Downstream);
            assert!(c.x.is_finite() && c.top.is_finite() && c.height.is_finite());
            assert!(l.hit(c.x, c.top).offset <= f.paragraph.len());
        }
    }
}

/// [`FakeMono`] with a font that reports no x-height (no OS/2 `sxHeight`).
struct NoXHeight(FakeMono);

impl gv_inline::Measure for NoXHeight {
    type Glyphs = ();

    fn shape(&mut self, text: &str, font: gv_inline::StyleId, size: f32) -> gv_inline::ShapedRun {
        gv_inline::Measure::shape(&mut self.0, text, font, size)
    }

    fn metrics(&mut self, font: gv_inline::StyleId, size: f32) -> gv_inline::FontMetrics {
        gv_inline::FontMetrics {
            x_height: 0.0,
            ..gv_inline::Measure::metrics(&mut self.0, font, size)
        }
    }
}

#[test]
fn a_font_without_x_height_gets_half_an_em() {
    let mut runs = RunCache::default();
    let mut measure = NoXHeight(FakeMono::default());
    assert_eq!(
        runs.metrics(&mut measure, body().font, SIZE).x_height,
        0.5 * SIZE
    );
    // The middle of a box and the strikethrough sit as with a reported
    // x-height of half an em (`FakeMono`'s), not on the baseline.
    let paragraph = build(
        &[
            text(
                "x",
                body().with_strikethrough(Strikethrough {
                    color: ColorRole(4),
                    thickness: 1.0,
                }),
            ),
            Piece::Box(
                InlineBox::new(BoxKind::Image, 10.0, 12.0, 0.0).with_align(BoxAlign::Middle),
            ),
        ],
        paragraph_style(),
    );
    let shaped = Arc::new(gv_inline::Shaped::new(
        Arc::new(paragraph),
        &mut measure,
        &mut runs,
    ));
    let l = Layout::new(shaped, 1000.0);
    let b = &l.fragments()[1];
    assert_eq!((b.ascent, b.descent), (8.5, 3.5));
    let s = l
        .decorations(0)
        .iter()
        .find(|d| matches!(d.kind, DecorationKind::Strikethrough { .. }))
        .map(|s| s.rect.y);
    assert_eq!(s, Some(11.0 - 2.5 - 0.5));
}

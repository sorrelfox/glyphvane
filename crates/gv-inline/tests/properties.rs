// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests on random paragraphs: mixed scripts, emoji sequences,
//! ligature-free and zero-width clusters, letter spacing, baseline shifts,
//! padded runs, boxes, hard breaks, delimiter gaps and escapes in the
//! source, every alignment and line height.
//!
//! Each property runs a moderate number of cases in `cargo test`. The
//! `*_heavy` variants run many more and are `#[ignore]`d; run them with
//! `cargo test -p gv-inline --test properties -- --ignored`.

use std::collections::HashSet;
use std::sync::Arc;

use gv_inline::{
    Affinity, Align, Background, Bias, BoxAlign, BoxKind, ColorRole, FakeMono, InlineBox, ItemKind,
    Layout, LineHeight, Paragraph, ParagraphBuilder, ParagraphStyle, RunCache, RunStyle, Shaped,
    StyleId, Underline, UnderlineStyle,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError};
use unicode_segmentation::UnicodeSegmentation;

const CASES: u32 = 96;
const HEAVY_CASES: u32 = 4096;

fn config(cases: u32) -> Config {
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

// ----- generators -------------------------------------------------------

/// Pieces of text. None starts with a combining character, so item
/// boundaries are grapheme boundaries of the logical text.
fn piece() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "a",
        "bc",
        "word",
        "xyzzy",
        " ",
        " ",
        "  ",
        "日本",
        "語",
        "。",
        "、",
        ",",
        ".",
        "(",
        ")",
        "/",
        "-",
        "e\u{301}",
        "👩\u{200D}👧",
        "👍🏽",
        "🇯🇵",
        "fi",
        "\u{200B}",
        "\u{A0}",
        "\t",
        "longerword",
        "supercalifragilistic",
        "\n",
        "\r\n",
        "\u{2028}",
    ])
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(piece(), 1..6).prop_map(|v| v.concat())
}

fn style() -> impl Strategy<Value = RunStyle> {
    (
        0u32..3,
        prop::sample::select(vec![8.0f32, 10.0, 13.0]),
        prop::sample::select(vec![0.0f32, 0.0, 1.5, -0.5]),
        prop::sample::select(vec![0.0f32, 0.0, 3.0, -2.0]),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(font, size, spacing, shift, padded, underlined)| {
            let mut s = RunStyle::new(StyleId(font), size)
                .with_letter_spacing(spacing)
                .with_baseline_shift(shift);
            if padded {
                s = s.with_background(Background {
                    role: ColorRole(1),
                    padding_x: 3.0,
                    padding_y: 1.0,
                    radius: 2.0,
                });
            }
            if underlined {
                s = s.with_underline(Underline::new(
                    UnderlineStyle::Dotted,
                    ColorRole(2),
                    2.0,
                    1.0,
                ));
            }
            s
        })
}

fn inline_box() -> impl Strategy<Value = InlineBox> {
    (
        prop::sample::select(vec![0.0f32, 7.5, 20.0, 45.0]),
        0.0f32..20.0,
        0.0f32..8.0,
        prop::sample::select(vec![0.0f32, 2.0]),
        prop::sample::select(vec![
            BoxAlign::Baseline,
            BoxAlign::Middle,
            BoxAlign::TextTop,
            BoxAlign::TextBottom,
        ]),
    )
        .prop_map(|(w, a, d, m, align)| {
            InlineBox::new(BoxKind::Other, w, a, d)
                .with_margins(m, m)
                .with_align(align)
        })
}

#[derive(Debug, Clone)]
enum Gen {
    /// Text, its style, the gap before it in the source, and the char index
    /// of an escape (`\x` in the source for `x` in the text).
    Text(String, RunStyle, usize, Option<usize>),
    Box(InlineBox, usize, usize),
    Break(usize, usize),
}

fn gen_item() -> impl Strategy<Value = Gen> {
    prop_oneof![
        8 => (text(), style(), 0usize..3, prop::option::of(0usize..8))
            .prop_map(|(t, s, gap, esc)| Gen::Text(t, s, gap, esc)),
        1 => (style(), 0usize..3).prop_map(|(s, gap)| Gen::Text(String::new(), s, gap, None)),
        3 => (inline_box(), 0usize..3, 1usize..9).prop_map(|(b, gap, len)| Gen::Box(b, gap, len)),
        1 => (0usize..3, 1usize..3).prop_map(|(gap, len)| Gen::Break(gap, len)),
    ]
}

/// The paragraph style, and whether the measurer forms `fi` / `fl`
/// ligatures (no caret stop inside them).
fn setup() -> impl Strategy<Value = (ParagraphStyle, bool)> {
    (
        prop::sample::select(vec![Align::Left, Align::Center, Align::Right]),
        prop::sample::select(vec![
            LineHeight::Px(16.0),
            LineHeight::Px(8.0),
            LineHeight::Normal,
        ]),
        any::<bool>(),
    )
        .prop_map(|(align, lh, ligatures)| {
            let style = ParagraphStyle::new(StyleId(0), 10.0, lh).with_align(align);
            (style, ligatures)
        })
}

fn build(items: &[Gen], style: ParagraphStyle) -> Paragraph {
    let mut b = ParagraphBuilder::new(style);
    let mut at = 100;
    b.source_anchor(at);
    for item in items {
        match item {
            Gen::Text(t, s, gap, esc) => {
                at += gap;
                let escape = esc.and_then(|n| t.char_indices().nth(n));
                match escape {
                    Some((i, c)) => {
                        let end = i + c.len_utf8();
                        let anchors = [(i, at + i), (end, at + end + 1)];
                        b.text_mapped(t, *s, at..at + t.len() + 1, &anchors);
                        at += t.len() + 1;
                    }
                    None => {
                        b.text(t, *s, at..at + t.len());
                        at += t.len();
                    }
                }
            }
            Gen::Box(bx, gap, len) => {
                at += gap;
                b.inline_box(*bx, at..at + len);
                at += len;
            }
            Gen::Break(gap, len) => {
                at += gap;
                b.hard_break(at..at + len);
                at += len;
            }
        }
    }
    b.build()
}

fn width() -> impl Strategy<Value = f32> {
    prop_oneof![0.5f32..30.0, 30.0f32..400.0]
}

fn case() -> impl Strategy<Value = (Vec<Gen>, (ParagraphStyle, bool), f32)> {
    (prop::collection::vec(gen_item(), 0..20), setup(), width())
}

fn shape(items: &[Gen], (style, ligatures): (ParagraphStyle, bool)) -> Arc<Shaped> {
    let p = Arc::new(build(items, style));
    let mut m = FakeMono {
        ligatures,
        ..FakeMono::default()
    };
    let mut runs = RunCache::default();
    Arc::new(Shaped::new(p, &mut m, &mut runs))
}

// ----- properties -------------------------------------------------------

/// Lines tile the text; every soft or hard line boundary is a grapheme
/// boundary and a caret stop, so no line ends inside a cluster, a ligature
/// or a box.
fn lines_tile_at_graphemes(shaped: &Arc<Shaped>, width: f32) -> Result<(), TestCaseError> {
    let l = Layout::new(shaped.clone(), width);
    let text = shaped.paragraph().text();
    let graphemes: HashSet<usize> = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let stops: HashSet<usize> = shaped.caret_stops().collect();
    let mut expect = 0;
    for line in l.lines() {
        prop_assert_eq!(line.range.start, expect);
        prop_assert!(line.range.end >= line.range.start);
        expect = line.range.end;
        prop_assert!(graphemes.contains(&line.range.end), "{:?}", line.range);
        prop_assert!(stops.contains(&line.range.end), "{:?}", line.range);
    }
    prop_assert_eq!(expect, text.len());
    Ok(())
}

/// A line is wider than the width only when it holds a single unit (one
/// cluster or one box) that cannot be broken.
fn lines_fit_unless_one_unit(shaped: &Arc<Shaped>, width: f32) -> Result<(), TestCaseError> {
    let l = Layout::new(shaped.clone(), width);
    let stops: Vec<usize> = shaped.caret_stops().collect();
    for line in l.lines() {
        if line.width <= width + 0.02 {
            continue;
        }
        let text = &shaped.paragraph().text()[line.range.clone()];
        let hanging = [
            ' ', '\t', '\n', '\r', '\u{0B}', '\u{0C}', '\u{85}', '\u{2028}', '\u{2029}',
        ];
        let trimmed = line.range.start + text.trim_end_matches(hanging).len();
        let units = stops
            .windows(2)
            .filter(|w| w[0] >= line.range.start && w[1] <= trimmed)
            .filter(|w| shaped.width(w[0], w[1]) > 0.0)
            .count();
        prop_assert!(
            units <= 1,
            "line {:?} width {} of {width}",
            line,
            line.width
        );
    }
    Ok(())
}

fn widening_never_adds_lines(shaped: &Arc<Shaped>, w1: f32, w2: f32) -> Result<(), TestCaseError> {
    let (narrow, wide) = (w1.min(w2), w1.max(w2));
    let a = Layout::new(shaped.clone(), narrow).lines().len();
    let b = Layout::new(shaped.clone(), wide).lines().len();
    prop_assert!(b <= a, "{narrow} → {a} lines, {wide} → {b} lines");
    Ok(())
}

/// Hit testing at every caret returns its offset, or an offset whose caret
/// is at the same place (zero-width clusters and boxes share an x).
fn carets_round_trip(shaped: &Arc<Shaped>, width: f32) -> Result<(), TestCaseError> {
    let l = Layout::new(shaped.clone(), width);
    for offset in shaped.caret_stops() {
        for affinity in [Affinity::Upstream, Affinity::Downstream] {
            let c = l.caret(offset, affinity);
            prop_assert!(c.x.is_finite() && c.height >= 0.0);
            let line = &l.lines()[c.line];
            let h = l.hit(c.x, line.top + line.height / 2.0);
            let back = l.caret(h.offset, h.affinity);
            prop_assert!(
                h.offset == offset || (back.line == c.line && (back.x - c.x).abs() < 1e-3),
                "offset {offset} {affinity:?} caret {c:?} → hit {h:?} caret {back:?}"
            );
            prop_assert_eq!(back.line, c.line, "{:?} → {:?}", c, h);
            prop_assert!(
                (back.x - c.x).abs() < 1e-3,
                "{:?} → {:?} → {:?}",
                c,
                h,
                back
            );
        }
    }
    Ok(())
}

/// Every source offset (and some outside the paragraph) maps to a caret;
/// the hit at that caret maps back to the same logical position.
fn source_offsets_round_trip(shaped: &Arc<Shaped>, width: f32) -> Result<(), TestCaseError> {
    let l = Layout::new(shaped.clone(), width);
    let p = shaped.paragraph();
    let span = p.source_span();
    for src in span.start.saturating_sub(2)..span.end + 2 {
        for bias in [Bias::Left, Bias::Right] {
            let (raw, affinity) = p.logical_offset(src, bias);
            prop_assert!(p.text().is_char_boundary(raw));
            let offset = shaped.snap(raw, bias);
            let c = l.caret_for_source(src, bias);
            prop_assert_eq!(c, l.caret(offset, affinity));
            let line = &l.lines()[c.line];
            let h = l.hit(c.x, line.top + line.height / 2.0);
            let again = shaped.snap(p.logical_offset(h.source, Bias::Left).0, Bias::Left);
            prop_assert_eq!(again, h.offset, "src {} {:?} → {:?}", src, bias, h);
            prop_assert!(span.contains(&h.source) || h.source == span.end);
        }
    }
    Ok(())
}

/// Selection rectangles cover a cluster (or box) exactly when it is
/// selected.
fn selection_covers_selected_clusters(
    shaped: &Arc<Shaped>,
    width: f32,
    pick: (usize, usize),
) -> Result<(), TestCaseError> {
    let l = Layout::new(shaped.clone(), width);
    let stops: Vec<usize> = shaped.caret_stops().collect();
    let (i, j) = (pick.0 % stops.len(), pick.1 % stops.len());
    let (a, b) = (stops[i.min(j)], stops[i.max(j)]);
    let rects = l.selection_rects(a, b);
    let items = shaped.paragraph().items();
    for (li, line) in l.lines().iter().enumerate() {
        let on_line: Vec<_> = rects.iter().filter(|r| r.y == line.top).collect();
        for f in l.line_fragments(li) {
            let start = items[f.item].range().start;
            let mut units: Vec<(usize, usize, f32, f32)> = Vec::new();
            match items[f.item].kind() {
                ItemKind::Text(_) => {
                    let base = f.x - shaped.x_at(f.item, f.range.start);
                    let local: Vec<usize> = stops
                        .iter()
                        .filter(|&&s| s >= start + f.range.start && s <= start + f.range.end)
                        .map(|s| s - start)
                        .collect();
                    for w in local.windows(2) {
                        let x0 = base + shaped.x_at(f.item, w[0]);
                        let x1 = base + shaped.x_at(f.item, w[1]);
                        units.push((start + w[0], start + w[1], x0, x1));
                    }
                }
                _ => units.push((start, start + f.range.end, f.x, f.x + f.width)),
            }
            for (s0, s1, x0, x1) in units {
                if x1 - x0 < 0.05 {
                    continue;
                }
                let mid = (x0 + x1) / 2.0;
                let covered = on_line.iter().any(|r| r.x <= mid && mid < r.right());
                let selected = a <= s0 && s1 <= b;
                prop_assert_eq!(
                    covered,
                    selected,
                    "unit {}..{} of selection {}..{}",
                    s0,
                    s1,
                    a,
                    b
                );
            }
        }
    }
    Ok(())
}

// ----- drivers ----------------------------------------------------------

proptest! {
    #![proptest_config(config(CASES))]

    #[test]
    fn lines_tile_and_never_split_a_grapheme_or_a_box((items, style, width) in case()) {
        lines_tile_at_graphemes(&shape(&items, style), width)?;
    }

    #[test]
    fn lines_fit_unless_they_hold_one_unit((items, style, width) in case()) {
        lines_fit_unless_one_unit(&shape(&items, style), width)?;
    }

    #[test]
    fn widening_never_increases_the_line_count(
        (items, style, w1) in case(),
        w2 in width(),
    ) {
        widening_never_adds_lines(&shape(&items, style), w1, w2)?;
    }

    #[test]
    fn hit_testing_at_every_caret_returns_its_offset((items, style, width) in case()) {
        carets_round_trip(&shape(&items, style), width)?;
    }

    #[test]
    fn every_source_offset_maps_to_a_caret_and_back((items, style, width) in case()) {
        source_offsets_round_trip(&shape(&items, style), width)?;
    }

    #[test]
    fn selection_rects_cover_exactly_the_selected_clusters(
        (items, style, width) in case(),
        pick in (any::<usize>(), any::<usize>()),
    ) {
        selection_covers_selected_clusters(&shape(&items, style), width, pick)?;
    }
}

proptest! {
    #![proptest_config(config(HEAVY_CASES))]

    #[test]
    #[ignore = "slow; run with --ignored"]
    fn all_properties_heavy(
        (items, style, w1) in case(),
        w2 in width(),
        pick in (any::<usize>(), any::<usize>()),
    ) {
        let shaped = shape(&items, style);
        lines_tile_at_graphemes(&shaped, w1)?;
        lines_fit_unless_one_unit(&shaped, w1)?;
        widening_never_adds_lines(&shaped, w1, w2)?;
        carets_round_trip(&shaped, w1)?;
        source_offsets_round_trip(&shaped, w1)?;
        selection_covers_selected_clusters(&shaped, w1, pick)?;
    }
}

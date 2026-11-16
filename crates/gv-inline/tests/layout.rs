// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Line breaking, line boxes, hit testing, carets and selection with the
//! monospace fake measurer (see `common`).

mod common;

use common::*;
use gv_inline::{
    Affinity, Bias, BoxKind, InlineBox, LineHeight, OBJECT_LEN, ParagraphBuilder, ParagraphStyle,
    StyleId,
};

#[test]
fn english_breaks_at_spaces_and_hangs_trailing_space() {
    let f = Fixture::new(vec![text("aaa bbb ccc", body())]);
    // "aaa bbb" is 7 chars = 42 px; the space after it hangs.
    assert_eq!(f.lines(42.0), ["aaa bbb", "ccc"]);
    assert_eq!(f.lines(41.0), ["aaa", "bbb", "ccc"]);
    assert_eq!(f.lines(1000.0), ["aaa bbb ccc"]);
    let l = f.layout(42.0);
    assert_eq!(l.lines()[0].width, 42.0, "hanging space not counted");
}

#[test]
fn no_break_at_style_boundary_inside_a_word() {
    // "un**bold**ed" is one word across three runs.
    let f = Fixture::new(vec![
        text("xx un", body()),
        text("bold", bold()),
        text("ed yy", body()),
    ]);
    assert_eq!(f.lines(60.0), ["xx", "unbolded", "yy"]);
}

#[test]
fn cjk_breaks_between_ideographs_but_not_before_closing_punctuation() {
    // Each ideograph is 10 px.
    let f = Fixture::new(vec![text("日本語の文章。次", body())]);
    let lines = f.lines(30.0);
    assert_eq!(lines[0], "日本語");
    // "。" (CL) may not start a line: "章。" stay together.
    assert!(lines.iter().all(|l| !l.starts_with('。')), "{lines:?}");
    assert_eq!(lines.concat(), "日本語の文章。次");
}

#[test]
fn emoji_zwj_sequence_is_never_split() {
    let family = "👩\u{200D}👩\u{200D}👧\u{200D}👦";
    let s = format!("ab {family}{family}{family} cd");
    let f = Fixture::new(vec![text(s, body())]);
    // Narrow enough to force overflow-wrap inside the emoji run.
    for width in [5.0, 10.0, 15.0, 25.0] {
        for line in f.lines(width) {
            for (i, c) in line.char_indices() {
                if c == '\u{200D}' {
                    assert!(i > 0 && i + 3 < line.len(), "ZWJ at a line edge: {line:?}");
                }
            }
        }
    }
    // Each family is one grapheme of 10 px; emoji are ID class, so lines
    // may break between two families.
    assert_eq!(
        f.lines(30.0),
        [
            format!("ab {family}"),
            format!("{family}{family}"),
            "cd".to_owned()
        ]
    );
}

#[test]
fn long_word_breaks_anywhere_when_it_cannot_fit() {
    let f = Fixture::new(vec![text("ab abcdefghij cd", body())]);
    // 24 px = 4 chars. The long word moves to its own line, then breaks.
    assert_eq!(f.lines(24.0), ["ab", "abcd", "efgh", "ij", "cd"]);
    // Narrower than one character: one grapheme per line, no endless loop.
    assert_eq!(f.lines(1.0).len(), 14);
    assert_eq!(f.lines(0.0).len(), 14);
}

#[test]
fn boxes_are_atomic_and_break_opportunities_around_them() {
    let f = Fixture::new(vec![
        text("aaa ", body()),
        math(30.0, 12.0, 6.0),
        text(" bbb", body()),
    ]);
    assert_eq!(f.lines(1000.0), ["aaa ☐ bbb"]);
    // Box 30 px does not fit after "aaa " (24 px) in 40 px, and " bbb"
    // does not fit after it.
    assert_eq!(f.lines(40.0), ["aaa", "☐", "bbb"]);
    assert_eq!(f.lines(56.0), ["aaa ☐", "bbb"]);
    // A box wider than the line still takes a line of its own, unbroken.
    let wide = Fixture::new(vec![text("a ", body()), math(100.0, 8.0, 2.0)]);
    assert_eq!(wide.lines(50.0), ["a", "☐"]);
}

#[test]
fn box_glued_to_punctuation_does_not_break_before_comma() {
    // "$x$, then": no break between the box and the comma.
    let f = Fixture::new(vec![
        text("see ", body()),
        math(20.0, 8.0, 2.0),
        text(", then", body()),
    ]);
    for width in [30.0, 40.0, 50.0] {
        for line in f.lines(width) {
            assert!(
                !line.starts_with(','),
                "comma starts a line at {width}: {line:?}"
            );
        }
    }
}

#[test]
fn hard_break_forces_a_line_and_empty_lines_keep_strut_height() {
    let f = Fixture::new(vec![
        text("ab", body()),
        Piece::Break,
        Piece::Break,
        text("cd", body()),
    ]);
    let l = f.layout(1000.0);
    assert_eq!(l.lines().len(), 3);
    assert!(l.lines().iter().all(|line| line.height == 16.0));
    assert!(l.lines()[0].hard_break && l.lines()[1].hard_break);
    assert!(!l.lines()[2].hard_break);
    assert_eq!(l.height(), 48.0);
}

#[test]
fn shared_baseline_and_line_height_grow_for_tall_boxes() {
    // Strut: ascent 8, descent 2, line height 16 → half leading 3,
    // so a plain line has ascent 11 and descent 5.
    let f = Fixture::new(vec![
        text("ab ", body()),
        math(20.0, 20.0, 9.0),
        text(" cd", body()),
    ]);
    let l = f.layout(1000.0);
    let line = &l.lines()[0];
    assert_eq!(line.baseline, 20.0, "tall box ascent sets the baseline");
    assert_eq!(line.height, 29.0, "20 above + 9 below");
    // A small box keeps the strut's height.
    let small = Fixture::new(vec![text("ab ", body()), math(10.0, 9.0, 3.0)]);
    let l = small.layout(1000.0);
    assert_eq!((l.lines()[0].baseline, l.lines()[0].height), (11.0, 16.0));
}

#[test]
fn second_line_top_follows_the_first_lines_height() {
    let f = Fixture::new(vec![
        text("ab ", body()),
        math(20.0, 20.0, 9.0),
        text(" cd efgh", body()),
    ]);
    // "ab ☐ cd" = 18 + 20 + 18 = 56 px; "efgh" wraps.
    let l = f.layout(60.0);
    assert_eq!(l.lines().len(), 2);
    assert_eq!(l.lines()[1].top, 29.0);
    assert_eq!(l.lines()[1].baseline, 29.0 + 11.0);
}

#[test]
fn chip_padding_takes_space_only_at_item_edges() {
    let f = Fixture::new(vec![
        text("a ", body()),
        text("xy zw", code()),
        text(" b", body()),
    ]);
    let l = f.layout(1000.0);
    let frag = &l.fragments()[1];
    assert_eq!(
        (frag.x, frag.width, frag.pad_left, frag.pad_right),
        (17.0, 30.0, 5.0, 5.0)
    );
    // Wrapped inside the code span: left padding on the first piece only.
    let l = f.layout(40.0);
    let pieces: Vec<_> = l
        .fragments()
        .iter()
        .filter(|fr| fr.item == 1)
        .map(|fr| (fr.pad_left, fr.pad_right))
        .collect();
    assert_eq!(pieces, [(5.0, 0.0), (0.0, 5.0)]);
}

#[test]
fn hit_testing_maps_points_to_offsets_in_text_and_around_boxes() {
    let f = Fixture::new(vec![
        text("abcd ", body()),
        math(20.0, 8.0, 2.0),
        text(" xyz", body()),
    ]);
    let l = f.layout(1000.0);
    // 'a' 0..6, 'b' 6..12: x 7 is closest to the stop at 6.
    let h = l.hit(7.0, 5.0);
    assert_eq!((h.item, h.offset), (Some(0), 1));
    assert_eq!(l.hit(10.0, 5.0).offset, 2);
    // The box spans 30..50: left half → before, right half → after.
    let before = l.hit(35.0, 5.0);
    assert_eq!(
        (before.offset, before.affinity, before.item),
        (5, Affinity::Downstream, Some(1))
    );
    let after = l.hit(45.0, 5.0);
    assert_eq!(
        (after.offset, after.affinity, after.item),
        (5 + OBJECT_LEN, Affinity::Upstream, Some(1))
    );
    // Source offsets: here the source equals the logical text.
    assert_eq!((before.source, after.source), (5, 8));
    // Past the end of the line, and above/below the block (clamped).
    assert_eq!(l.hit(999.0, 5.0).offset, f.paragraph.len());
    assert_eq!(l.hit(0.0, -50.0).offset, 0);
    assert_eq!(l.hit(999.0, 500.0).offset, f.paragraph.len());
}

#[test]
fn caret_round_trips_through_hit_testing() {
    let f = Fixture::new(vec![
        text("hello ", body()),
        text("bold", bold()),
        text(" 日本 ", body()),
        math(20.0, 14.0, 4.0),
        text(" end of the line", body()),
    ]);
    let l = f.layout(70.0);
    let mut checked = 0;
    for offset in f.shaped.caret_stops() {
        for affinity in [Affinity::Upstream, Affinity::Downstream] {
            let c = l.caret(offset, affinity);
            let y = l.lines()[c.line].top + 1.0;
            let h = l.hit(c.x, y);
            assert_eq!(h.offset, offset, "offset {offset} {affinity:?} at {c:?}");
            assert_eq!(l.caret(h.offset, h.affinity), c, "same caret for {h:?}");
            checked += 1;
        }
    }
    assert!(checked > 40, "{checked}");
}

#[test]
fn selection_rects_cover_items_and_lines() {
    let f = Fixture::new(vec![
        text("aaa ", body()),
        math(20.0, 8.0, 2.0),
        text(" bbb ccc", body()),
    ]);
    let l = f.layout(60.0); // "aaa ☐" (44) | "bbb ccc" (42)
    // From inside "aaa" across the box to inside "bbb".
    let rects = l.selection_rects(1, 4 + OBJECT_LEN + 3);
    assert_eq!(rects.len(), 2, "{rects:?}");
    assert_eq!((rects[0].x, rects[0].y), (6.0, 0.0));
    // "aaa ☐ " = 24 + 20 + 6: the box and the hanging space after it.
    assert_eq!(rects[0].right(), 50.0, "box included");
    assert_eq!((rects[1].x, rects[1].width, rects[1].y), (0.0, 12.0, 16.0));
}

#[test]
fn source_slice_of_a_selection_takes_boxes_whole() {
    // Ported from the plain-text copy test: copy as Markdown is the source
    // slice, and half a box is not part of it.
    let source = "[x] area $\\pi r^2$\\\nnext";
    let mut b = ParagraphBuilder::new(paragraph_style());
    b.inline_box(
        InlineBox::new(BoxKind::Checkbox { checked: true }, 14.0, 11.0, 3.0).with_margins(0.0, 9.0),
        0..3,
    );
    b.text("area ", body(), 4..9);
    b.inline_box(InlineBox::new(BoxKind::Math, 30.0, 10.0, 2.0), 9..18);
    b.hard_break(18..20);
    b.text("next", body(), 20..24);
    let f = Fixture::from_paragraph(b.build(), gv_inline::FakeMono::default());
    let p = &f.paragraph;
    let all =
        p.source_offset(0, Affinity::Downstream)..p.source_offset(p.len(), Affinity::Upstream);
    assert_eq!(&source[all], source);
    // Logical 3..9 ends inside the math box: snapped left, it is left out.
    let end = f.shaped.snap(3 + 6, Bias::Left);
    let half = p.source_offset(3, Affinity::Downstream)..p.source_offset(end, Affinity::Upstream);
    assert_eq!(&source[half], "area ");
    // Snapped right, the whole box is in.
    let end = f.shaped.snap(3 + 6, Bias::Right);
    let whole = p.source_offset(3, Affinity::Downstream)..p.source_offset(end, Affinity::Upstream);
    assert_eq!(&source[whole], "area $\\pi r^2$");
}

#[test]
fn rewrap_reuses_shaping() {
    let f = Fixture::new(vec![
        text("one two three four five", body()),
        text(" six", bold()),
    ]);
    let before = f.measure.shaped;
    let counts: Vec<usize> = [30.0, 60.0, 120.0, 1000.0]
        .iter()
        .map(|&w| f.layout(w).lines().len())
        .collect();
    assert_eq!(f.measure.shaped, before, "layout never shapes");
    assert!(counts.windows(2).all(|w| w[0] >= w[1]), "{counts:?}");
}

#[test]
fn layout_math_is_consistent_for_random_widths() {
    // Every line fits unless it is a single unbreakable piece; line ranges
    // tile the text; fragments are in x order.
    let f = Fixture::new(vec![
        text("Lorem ipsum ", body()),
        text("dolor", bold()),
        text(" sit amet, 日本語の文章 ", body()),
        math(25.0, 15.0, 5.0),
        text(", consectetur ", body()),
        text("adipiscing_elit()", code()),
        text(" 👍🏽 sed do eiusmod.", body()),
    ]);
    for w in (5..400).step_by(7) {
        let width = w as f32;
        let l = f.layout(width);
        let mut expect = 0;
        for (ix, line) in l.lines().iter().enumerate() {
            assert_eq!(line.range.start, expect);
            expect = line.range.end;
            let frags = l.line_fragments(ix);
            assert!(frags.windows(2).all(|p| p[0].x <= p[1].x));
            if frags.len() > 1 {
                assert!(
                    line.width <= width + 0.01 || frags.len() <= 2,
                    "line too wide at {width}"
                );
            }
        }
        assert_eq!(expect, f.paragraph.len());
    }
}

#[test]
fn baseline_shift_raises_or_lowers_a_run_and_grows_the_line_if_needed() {
    let mut sup = body().with_baseline_shift(6.0);
    sup.size = 8.0;
    let sub = sup.with_baseline_shift(-5.0);
    // Strut: ascent 11, descent 5 (see shared_baseline test).
    let f = Fixture::new(vec![text("x", body()), text("2", sup)]);
    let l = f.layout(1000.0);
    // sup ascent 6.4 + 6 = 12.4 > 11: the line grows above the baseline.
    assert!(
        (l.lines()[0].baseline - 12.4).abs() < 1e-4,
        "{:?}",
        l.lines()[0]
    );
    assert_eq!(l.fragments()[1].shift, 6.0);
    let f = Fixture::new(vec![text("H", body()), text("2", sub)]);
    let l = f.layout(1000.0);
    // sub descent 1.6 + 5 = 6.6 > 5: the line grows below the baseline.
    assert!(
        (l.lines()[0].height - (11.0 + 6.6)).abs() < 1e-4,
        "{:?}",
        l.lines()[0]
    );
}

#[test]
fn tracking_adds_space_after_every_cluster_and_wraps_accordingly() {
    let tracked = body().with_letter_spacing(2.0);
    let family = "👩\u{200D}👧";
    let f = Fixture::new(vec![text(format!("ab{family} cd"), tracked)]);
    let l = f.layout(1000.0);
    // a(6+2) b(6+2) family(10+2) space(6+2) c(6+2) d(6+2) = 52.
    assert_eq!(l.lines()[0].width, 52.0);
    // The caret after the family cluster is at 8 + 8 + 12 = 28.
    let after = "ab".len() + family.len();
    let c = l.caret(after, Affinity::Downstream);
    assert_eq!((c.line, c.x), (0, 28.0));
    // Painting: a glyph of the cluster at byte 2 moves by two spacings.
    assert_eq!(f.shaped.glyph_shift(0, 2), 4.0);
    // "ab👩‍👧" is 28 px and "cd" 16 px with tracking: at 40 px they no
    // longer share a line.
    assert_eq!(f.lines(52.0).len(), 1);
    assert_eq!(f.lines(40.0).len(), 2);
}

#[test]
fn normal_and_tight_line_heights() {
    let normal = ParagraphStyle::new(StyleId(0), SIZE, LineHeight::Normal);
    let f = Fixture::with_style(vec![text("ab cd", body())], normal);
    // 0.8 + 0.2 + 0.2 em.
    assert_eq!(f.layout(1000.0).lines()[0].height, 12.0);
    // A line height tighter than the font trims text as it trims the
    // strut: the line stays 8 px.
    let tight = ParagraphStyle::new(StyleId(0), SIZE, LineHeight::Px(8.0));
    let f = Fixture::with_style(vec![text("ab ", body()), text("cd", bold())], tight);
    let l = f.layout(1000.0);
    assert_eq!((l.lines()[0].height, l.lines()[0].baseline), (8.0, 7.0));
}

#[test]
fn columns_wrap_side_by_side_and_hit_test_per_column() {
    // Two cells, each after a zero-width gap box: "aaa bbb ccc" (66 px
    // unwrapped) in a 42 px column at x 10, "dd ee" right-aligned in a
    // 20 px column at x 70.
    let gap = || Piece::Box(InlineBox::new(BoxKind::Other, 0.0, 0.0, 0.0));
    let mut p = build(
        &[
            gap(),
            text("aaa bbb ccc", body()),
            gap(),
            text("dd ee", body()),
            gap(),
        ],
        paragraph_style(),
    );
    let second = 3 + "aaa bbb ccc".len();
    p.set_columns([
        gv_inline::Column {
            start: 0,
            x: 10.0,
            width: 42.0,
            align: gv_inline::Align::Left,
        },
        gv_inline::Column {
            start: second,
            x: 70.0,
            width: 20.0,
            align: gv_inline::Align::Right,
        },
    ]);
    let f = Fixture::from_paragraph(p, gv_inline::FakeMono::default());
    // The width passed is ignored: each column wraps in its own.
    let l = f.layout(1.0e5);
    assert!(l.is_columnar());
    let texts: Vec<String> = l
        .lines()
        .iter()
        .map(|line| {
            f.paragraph.text()[line.range.clone()]
                .trim_end()
                .replace(gv_inline::OBJECT, "")
        })
        .collect();
    assert_eq!(texts, ["aaa bbb", "ccc", "dd", "ee"]);
    // Each column's lines start at the top; the block is as tall as the
    // tallest column.
    assert_eq!(l.lines()[0].top, 0.0);
    assert_eq!(l.lines()[2].top, 0.0);
    assert_eq!(l.height(), 32.0);
    // Left column at x 10; right column right-aligned: "dd" ends at 90.
    assert_eq!(l.lines()[0].x, 10.0);
    assert!((l.lines()[2].x + l.lines()[2].width - 90.0).abs() < 0.01);
    assert!((l.width() - 90.0).abs() < 0.01);
    // Hits pick the column by x, then the line by y.
    let hit = l.hit(85.0, 20.0);
    assert_eq!(hit.line, 3);
    let hit = l.hit(12.0, 20.0);
    assert_eq!(hit.line, 1);
    assert_eq!(f.paragraph.source_offset(hit.offset, hit.affinity), 3 + 8);
    // Carets and selection follow the columns.
    let c = l.caret(second + 3, Affinity::Downstream);
    assert_eq!(c.line, 2);
    assert!(c.x >= 70.0);
    let rects = l.selection_rects(3, second + 8);
    assert!(rects.iter().any(|r| r.x >= 70.0 && r.y == 0.0), "{rects:?}");
    assert_eq!(l.lines_between(20.0, 21.0), 0..4);
    assert_eq!(l.item_at_point(85.0, 20.0), Some(3));
    // The cells' own content widths.
    assert_eq!(f.shaped.max_content_width_in(3..second), 66.0);
    assert_eq!(f.shaped.min_content_width_in(3..second), 18.0);
    assert_eq!(f.shaped.min_content_width_in(second + 3..second + 8), 12.0);
}

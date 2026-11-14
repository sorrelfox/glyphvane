// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors
//
// The tests from `test_basic_folds` to `test_random_chunk_bitmaps` are
// ported from Zed (https://github.com/zed-industries/zed),
// crates/editor/src/display_map/fold_map.rs at commit
// d2a4fb6ded7c470be25d1541281da3882495b65b.
// Copyright (C) Zed Industries, Inc. and the Zed contributors.
// Licensed under the GNU General Public License, version 3 or later.
//
// Modified by the Glyphvane contributors (2026-11-14): they run on
// gv-text buffers without GPUI or an inlay map, take byte ranges and
// points instead of multi-buffer anchors and fold offsets, and expect an
// edit inside a fold to remove it and folds that only touch to keep a
// placeholder each; see third_party/zed/PROVENANCE.md.

// The fold commands take lists of ranges.
#![allow(clippy::single_range_in_vec_init)]

use super::*;
use crate::display_map::test_support::{
    TestText, for_each_seed, normalize, random_edit, random_text, replay_edits,
};
use gv_text::Buffer;
use rand::{Rng, SeedableRng, rngs::StdRng};

fn snapshot(text: &str) -> ReplaceSnapshot {
    ReplaceMap::new(TextSnapshot::new(text)).1
}

/// Zed's `util::test::sample_text`: `rows` rows of `cols` copies of a
/// letter, from `start` on.
fn sample_text(rows: usize, cols: usize, start: char) -> String {
    (0..rows)
        .map(|row| {
            let c = char::from_u32(start as u32 + row as u32).unwrap_or(start);
            c.to_string().repeat(cols)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn offset(buffer: &TextSnapshot, line: usize, column: usize) -> usize {
    buffer
        .point_to_offset(BufferPoint::new(line, column))
        .unwrap()
}

/// A byte range from two buffer points.
fn points(buffer: &TextSnapshot, start: (usize, usize), end: (usize, usize)) -> Range<usize> {
    offset(buffer, start.0, start.1)..offset(buffer, end.0, end.1)
}

fn edit(old: Range<(u32, u32)>, new: Range<(u32, u32)>) -> ReplaceEdit {
    let point = |(row, column)| ReplacePoint::new(row, column);
    Edit {
        old: point(old.start)..point(old.end),
        new: point(new.start)..point(new.end),
    }
}

/// The buffer text with every run of overlapping folds replaced by the
/// placeholder, then normalized.
fn expected_text(buffer: &TextSnapshot, folds: &[Fold]) -> String {
    let mut text = buffer.to_string();
    for (_, range) in merged_folds(folds).into_iter().rev() {
        text.replace_range(range, PLACEHOLDER);
    }
    normalize(&text)
}

// ----- the identity part: line terminators --------------------------------------

#[test]
fn terminators_become_newlines() {
    let snapshot = snapshot("a\r\nbc\rd\n\r\n\r");
    assert_eq!(snapshot.text(), "a\nbc\nd\n\n\n");
    assert_eq!(snapshot.max_point(), ReplacePoint::new(5, 0));
    assert_eq!(snapshot.line_len(0), 1);
    assert_eq!(snapshot.line_len(1), 2);
    assert_eq!(snapshot.line_len(3), 0);
    assert_eq!(snapshot.line_len(9), 0);
}

#[test]
fn crlf_across_buffer_chunks() {
    // Long enough that some CRLFs straddle the rope's chunk boundaries.
    for prefix in 0..8 {
        let text = format!("{}{}", "x".repeat(prefix), "ab\r\n\r\tc\r".repeat(100));
        let snapshot = snapshot(&text);
        assert_eq!(snapshot.text(), normalize(&text));
        for chunk in snapshot.chunks_at(ReplacePoint::zero()) {
            assert!(!chunk.text.is_empty());
            assert!(chunk.text.len() <= MAX_CHUNK_LEN);
            assert_eq!(chunk, Chunk::new(chunk.text));
        }
    }
}

#[test]
fn chunks_of_ranges() {
    let text = "αβ\r\n\tγ\rδ";
    let snapshot = snapshot(text);
    let expected = TestText::new(&normalize(text));
    let max = snapshot.max_point();
    for start_row in 0..=max.row() {
        for start_column in 0..=snapshot.line_len(start_row) {
            let start = snapshot.clip_point(ReplacePoint::new(start_row, start_column), Bias::Left);
            let actual: String = snapshot
                .chunks(start..max)
                .map(|chunk| chunk.text)
                .collect();
            let offset = expected.point_to_offset(start.0);
            assert_eq!(actual, &expected.text()[offset..], "from {start:?}");
        }
    }
}

#[test]
fn clipping_and_buffer_points() {
    let snapshot = snapshot("aé\r\nb");
    // Inside `é` (bytes 1..3).
    assert_eq!(
        snapshot.clip_point(ReplacePoint::new(0, 2), Bias::Left),
        ReplacePoint::new(0, 1)
    );
    assert_eq!(
        snapshot.clip_point(ReplacePoint::new(0, 2), Bias::Right),
        ReplacePoint::new(0, 3)
    );
    // Past the end of the row (the CRLF interior is not a column here).
    assert_eq!(
        snapshot.clip_point(ReplacePoint::new(0, 4), Bias::Right),
        ReplacePoint::new(0, 3)
    );
    assert_eq!(
        snapshot.clip_point(ReplacePoint::new(7, 0), Bias::Left),
        ReplacePoint::new(1, 1)
    );
    // Between CR and LF in the buffer.
    assert_eq!(
        snapshot.to_replace_point(BufferPoint::new(0, 4), Bias::Left),
        ReplacePoint::new(0, 3)
    );
    assert_eq!(
        snapshot.to_replace_point(BufferPoint::new(0, 4), Bias::Right),
        ReplacePoint::new(1, 0)
    );
    assert_eq!(
        snapshot.to_buffer_point(ReplacePoint::new(1, 1), Bias::Left),
        BufferPoint::new(1, 1)
    );
}

/// A text and the replacements applied to it.
type Case = (&'static str, &'static [(Range<usize>, &'static str)]);

/// Edits around CR and LF, where a CRLF is created or split.
#[test]
fn edits_that_join_or_split_crlf() {
    let cases: &[Case] = &[
        ("a\r\nb", &[(2..2, "x")]),
        ("a\r\nb", &[(2..3, "")]),
        ("a\r\nb", &[(1..2, "")]),
        ("a\rb", &[(2..2, "\n")]),
        ("a\nb", &[(1..1, "\r")]),
        ("a\r", &[(2..2, "\n")]),
        ("a\rX\nb", &[(2..3, "")]),
        ("\r\n\r\n", &[(1..1, "x"), (2..3, "\r"), (3..3, "\n")]),
        ("\r\r\n\n", &[(0..1, ""), (1..2, ""), (3..4, "\r")]),
    ];
    for (text, edits) in cases {
        let mut buffer = Buffer::new(text);
        let (mut map, old) = ReplaceMap::new(buffer.snapshot());
        let summary = buffer
            .edit(edits.iter().map(|(range, text)| (range.clone(), *text)))
            .unwrap();
        let (new, replace_edits) = map.sync(buffer.snapshot(), &summary);
        assert_eq!(new.text(), normalize(&buffer.to_string()));
        assert_eq!(
            replay_edits(&old.text(), &new.text(), &replace_edits, |p| p.0),
            new.text(),
            "{text:?} with {edits:?}: {replace_edits:?}"
        );
    }
}

/// Typing at the end of a line is not widened to the terminator.
#[test]
fn edits_at_line_ends_stay_tight() {
    for text in ["ab\r\ncd", "ab\ncd", "ab\rcd"] {
        let mut buffer = Buffer::new(text);
        let (mut map, _) = ReplaceMap::new(buffer.snapshot());
        let summary = buffer.edit([(2..2, "x")]).unwrap();
        let (_, edits) = map.sync(buffer.snapshot(), &summary);
        assert_eq!(
            edits,
            vec![Edit {
                old: ReplacePoint::new(0, 2)..ReplacePoint::new(0, 2),
                new: ReplacePoint::new(0, 2)..ReplacePoint::new(0, 3),
            }],
            "{text:?}"
        );
    }
}

#[test]
fn mismatched_revisions_report_everything() {
    let mut buffer = Buffer::new("a\nb");
    let (mut map, _) = ReplaceMap::new(buffer.snapshot());
    map.fold([0..2]);
    let first = buffer.edit([(0..0, "x")]).unwrap();
    let _second = buffer.edit([(0..0, "y")]).unwrap();
    let (snapshot, edits, unfolded) = map.sync_with_unfolded(buffer.snapshot(), &first);
    assert_eq!(snapshot.text(), "yxa\nb");
    assert_eq!(
        edits,
        vec![Edit {
            old: ReplacePoint::zero()..ReplacePoint::new(0, 4),
            new: ReplacePoint::zero()..ReplacePoint::new(1, 1),
        }]
    );
    assert_eq!(unfolded.len(), 1);
    assert!(!snapshot.has_folds());
}

#[test]
fn random_edits() {
    for seed in 0..200 {
        let mut rng = StdRng::seed_from_u64(seed);
        let len = rng.random_range(0..60);
        let mut buffer = Buffer::new(&random_text(&mut rng, len));
        let (mut map, mut old) = ReplaceMap::new(buffer.snapshot());
        for _ in 0..5 {
            let summary = random_edit(&mut rng, &mut buffer);
            let (new, edits) = map.sync(buffer.snapshot(), &summary);
            assert_eq!(new.text(), normalize(&buffer.to_string()), "seed {seed}");
            assert_eq!(
                replay_edits(&old.text(), &new.text(), &edits, |p| p.0),
                new.text(),
                "seed {seed}: {edits:?}"
            );
            old = new;
        }
    }
}

// ----- folds: ported from Zed ---------------------------------------------------

#[test]
fn test_basic_folds() {
    let mut buffer = Buffer::new(&sample_text(5, 6, 'a'));
    let (mut map, _) = ReplaceMap::new(buffer.snapshot());

    let text = buffer.snapshot();
    let (snapshot2, edits) =
        map.fold([points(&text, (0, 2), (2, 2)), points(&text, (2, 4), (4, 1))]);
    assert_eq!(snapshot2.text(), "aa⋯cc⋯eeeee");
    assert_eq!(
        edits,
        &[
            edit((0, 2)..(2, 2), (0, 2)..(0, 5)),
            edit((2, 4)..(4, 1), (0, 7)..(0, 10)),
        ]
    );

    let text = buffer.snapshot();
    let summary = buffer
        .edit([
            (points(&text, (0, 0), (0, 1)), "123"),
            (points(&text, (2, 3), (2, 3)), "123"),
        ])
        .unwrap();
    let (snapshot3, edits) = map.sync(buffer.snapshot(), &summary);
    assert_eq!(snapshot3.text(), "123a⋯c123c⋯eeeee");
    assert_eq!(
        edits,
        &[
            edit((0, 0)..(0, 1), (0, 0)..(0, 3)),
            edit((0, 6)..(0, 6), (0, 8)..(0, 11)),
        ]
    );

    let text = buffer.snapshot();
    let summary = buffer
        .edit([(points(&text, (2, 6), (4, 3)), "456")])
        .unwrap();
    let (snapshot4, _) = map.sync(buffer.snapshot(), &summary);
    assert_eq!(snapshot4.text(), "123a⋯c123456eee");

    let text = buffer.snapshot();
    let at = offset(&text, 0, 4);
    map.unfold_intersecting([at..at], false);
    assert_eq!(map.snapshot().text(), "123a⋯c123456eee");

    map.unfold_intersecting([at..at], true);
    assert_eq!(map.snapshot().text(), "123aaaaa\nbbbbbb\nccc123456eee");
}

#[test]
fn test_adjacent_folds() {
    let mut buffer = Buffer::new("abcdefghijkl");

    {
        let (mut map, _) = ReplaceMap::new(buffer.snapshot());

        map.fold([5..8]);
        assert_eq!(map.snapshot().text(), "abcde⋯ijkl");

        // Create an fold adjacent to the start of the first fold. (Upstream
        // joins folds that touch: `⋯b⋯ijkl`, then `⋯b⋯kl`.)
        map.fold([0..1, 2..5]);
        assert_eq!(map.snapshot().text(), "⋯b⋯⋯ijkl");

        // Create an fold adjacent to the end of the first fold.
        map.fold([11..11, 8..10]);
        assert_eq!(map.snapshot().text(), "⋯b⋯⋯⋯kl");
    }

    {
        let (mut map, _) = ReplaceMap::new(buffer.snapshot());

        // Create two adjacent folds.
        map.fold([0..2, 2..5]);
        assert_eq!(map.snapshot().text(), "⋯⋯fghijkl");

        // Edit within one of the folds. Upstream keeps the fold (and the
        // text shows `12345⋯fghijkl`); here the edit removes it.
        let summary = buffer.edit([(0..1, "12345")]).unwrap();
        let (snapshot, _) = map.sync(buffer.snapshot(), &summary);
        assert_eq!(snapshot.text(), "12345b⋯fghijkl");
    }
}

#[test]
fn test_overlapping_folds() {
    let buffer = TextSnapshot::new(&sample_text(5, 6, 'a'));
    let (mut map, _) = ReplaceMap::new(buffer.clone());
    map.fold([
        points(&buffer, (0, 2), (2, 2)),
        points(&buffer, (0, 4), (1, 0)),
        points(&buffer, (1, 2), (3, 2)),
        points(&buffer, (3, 1), (4, 1)),
    ]);
    assert_eq!(map.snapshot().text(), "aa⋯eeeee");
}

#[test]
fn test_merging_folds_via_edit() {
    let mut buffer = Buffer::new(&sample_text(5, 6, 'a'));
    let (mut map, _) = ReplaceMap::new(buffer.snapshot());

    let text = buffer.snapshot();
    map.fold([points(&text, (0, 2), (2, 2)), points(&text, (3, 1), (4, 1))]);
    assert_eq!(map.snapshot().text(), "aa⋯cccc\nd⋯eeeee");

    // Upstream joins the folds that now touch: `aa⋯eeeee`.
    let summary = buffer.edit([(points(&text, (2, 2), (3, 1)), "")]).unwrap();
    let (snapshot, _) = map.sync(buffer.snapshot(), &summary);
    assert_eq!(snapshot.text(), "aa⋯⋯eeeee");
}

#[test]
fn test_folds_in_range() {
    let buffer = TextSnapshot::new(&sample_text(5, 6, 'a'));
    let (mut map, _) = ReplaceMap::new(buffer.clone());
    map.fold([
        points(&buffer, (0, 2), (2, 2)),
        points(&buffer, (0, 4), (1, 0)),
        points(&buffer, (1, 2), (3, 2)),
        points(&buffer, (3, 1), (4, 1)),
    ]);
    let snapshot = map.snapshot();
    let fold_ranges = snapshot
        .folds_in_range(points(&buffer, (1, 0), (1, 3)))
        .map(|fold| fold.range.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        fold_ranges,
        vec![
            points(&buffer, (0, 2), (2, 2)),
            points(&buffer, (1, 2), (3, 2)),
        ]
    );
}

/// Zed's `FoldMap::randomly_mutate`: folds or unfolds random ranges.
fn randomly_mutate(map: &mut ReplaceMap, rng: &mut StdRng) -> (ReplaceSnapshot, Vec<ReplaceEdit>) {
    let buffer = map.snapshot().buffer().clone();
    let random_range = |rng: &mut StdRng| {
        let end = buffer.clip_offset(rng.random_range(0..=buffer.len()), Bias::Right);
        let start = buffer.clip_offset(rng.random_range(0..=end), Bias::Left);
        start..end
    };
    match rng.random_range(0..=100) {
        0..=39 if map.snapshot().has_folds() => {
            let to_unfold: Vec<Range<usize>> = (0..rng.random_range(1..=3))
                .map(|_| random_range(rng))
                .collect();
            let inclusive = rng.random();
            map.unfold_intersecting(to_unfold, inclusive)
        }
        _ => {
            let to_fold: Vec<Range<usize>> = (0..rng.random_range(1..=2))
                .map(|_| random_range(rng))
                .collect();
            map.fold(to_fold)
        }
    }
}

#[test]
fn test_random_folds() {
    let operations = std::env::var("OPERATIONS")
        .ok()
        .and_then(|operations| operations.parse().ok())
        .unwrap_or(10);
    for_each_seed(100, |mut rng| {
        let len = if rng.random() {
            rng.random_range(0..10)
        } else {
            rng.random_range(0..200)
        };
        let mut buffer = Buffer::new(&random_text(&mut rng, len));
        let (mut map, mut initial_snapshot) = ReplaceMap::new(buffer.snapshot());
        let mut snapshot_edits = Vec::new();

        for _ in 0..operations {
            match rng.random_range(0..=100) {
                0..=59 => snapshot_edits.push(randomly_mutate(&mut map, &mut rng)),
                _ => {
                    let summary = random_edit(&mut rng, &mut buffer);
                    snapshot_edits.push(map.sync(buffer.snapshot(), &summary));
                }
            }
            let snapshot = map.snapshot();
            let buffer_snapshot = buffer.snapshot();

            let expected_text = expected_text(&buffer_snapshot, snapshot.folds());
            assert_eq!(snapshot.text(), expected_text);

            // The buffer line of every row: the rows outside folds, with
            // the lines a run of folds covers skipped.
            let merged = merged_folds(snapshot.folds());
            let mut expected_buffer_rows = Vec::new();
            let mut line = 0;
            for (_, range) in &merged {
                let start = buffer_snapshot.offset_to_point(range.start).unwrap().line;
                let end = buffer_snapshot.offset_to_point(range.end).unwrap().line;
                expected_buffer_rows.extend(line..=start);
                line = end + 1;
            }
            expected_buffer_rows.extend(line..buffer_snapshot.line_count());
            assert_eq!(
                expected_buffer_rows.len(),
                expected_text.matches('\n').count() + 1,
                "wrong expected buffer rows {expected_buffer_rows:?}. text: {expected_text:?}"
            );

            for (output_row, line) in expected_text.split('\n').enumerate() {
                assert_eq!(snapshot.line_len(output_row as u32), line.len() as u32);
            }

            let longest = TextSummary::from(expected_text.as_str());
            let summary = snapshot.text_summary();
            assert_eq!(summary.longest_row, longest.longest_row);
            assert_eq!(summary.longest_row_chars, longest.longest_row_chars);

            let mut replace_point = ReplacePoint::zero();
            for c in expected_text.chars() {
                for bias in [Bias::Left, Bias::Right] {
                    let buffer_point = snapshot.to_buffer_point(replace_point, bias);
                    assert_eq!(
                        snapshot.to_replace_point(buffer_point, bias),
                        replace_point,
                        "{buffer_point:?} -> replace point",
                    );
                    assert_eq!(snapshot.clip_point(replace_point, bias), replace_point);
                }
                if c == '\n' {
                    replace_point.0 += Point::new(1, 0);
                } else {
                    replace_point.0.column += c.len_utf8() as u32;
                }
            }

            let expected = TestText::new(&expected_text);
            for _ in 0..5 {
                let row = rng.random_range(0..=snapshot.max_point().row());
                let column = rng.random_range(0..=snapshot.line_len(row) + 2);
                let mut start = snapshot.clip_point(ReplacePoint::new(row, column), Bias::Left);
                let row = rng.random_range(0..=snapshot.max_point().row());
                let column = rng.random_range(0..=snapshot.line_len(row) + 2);
                let mut end = snapshot.clip_point(ReplacePoint::new(row, column), Bias::Right);
                if start > end {
                    std::mem::swap(&mut start, &mut end);
                }
                let text = expected.slice(start.0..end.0);
                assert_eq!(
                    snapshot
                        .chunks(start..end)
                        .map(|chunk| chunk.text)
                        .collect::<String>(),
                    text,
                );
                assert_eq!(
                    snapshot.text_summary_for_range(start..end),
                    TextSummary::from(text)
                );
            }

            for row in 0..expected_buffer_rows.len() as u32 {
                assert_eq!(
                    snapshot.buffer_lines(row).collect::<Vec<_>>(),
                    expected_buffer_rows[row as usize..],
                    "wrong buffer rows starting at row {row}",
                );
            }

            let mut folded_lines = std::collections::HashSet::new();
            for (_, range) in &merged {
                let start = buffer_snapshot.offset_to_point(range.start).unwrap();
                let end = buffer_snapshot.offset_to_point(range.end).unwrap();
                if end.column == 0 {
                    folded_lines.extend(start.line..end.line);
                } else {
                    folded_lines.extend(start.line..=end.line);
                }
            }
            for line in 0..buffer_snapshot.line_count() {
                assert_eq!(
                    snapshot.is_line_folded(line),
                    folded_lines.contains(&line),
                    "expected buffer line {line}{} to be folded",
                    if folded_lines.contains(&line) {
                        ""
                    } else {
                        " not"
                    }
                );
            }

            for _ in 0..5 {
                let end = buffer_snapshot
                    .clip_offset(rng.random_range(0..=buffer_snapshot.len()), Bias::Right);
                let start = buffer_snapshot.clip_offset(rng.random_range(0..=end), Bias::Left);
                let expected_folds: Vec<&Fold> = snapshot
                    .folds()
                    .iter()
                    .filter(|fold| start < fold.range.end && end > fold.range.start)
                    .collect();
                assert_eq!(
                    snapshot.folds_in_range(start..end).collect::<Vec<_>>(),
                    expected_folds
                );
            }

            // Every buffer offset strictly inside a run of folds is hidden.
            for offset in 0..=buffer_snapshot.len() {
                let inside = merged
                    .iter()
                    .find(|(_, range)| range.start < offset && offset < range.end)
                    .map(|(_, range)| range.clone());
                assert_eq!(snapshot.folded_range_at(offset), inside, "offset {offset}");
            }

            // Rebuilding from the folds gives the same layer.
            let (mut rebuilt, _) = ReplaceMap::new(buffer_snapshot.clone());
            let (rebuilt, _) = rebuilt.fold(snapshot.folds().iter().map(|fold| fold.range.clone()));
            assert_eq!(rebuilt.text(), snapshot.text());
            let ranges = |snapshot: &ReplaceSnapshot| {
                snapshot
                    .placeholders(0..u32::MAX)
                    .into_iter()
                    .map(|placeholder| (placeholder.range, placeholder.buffer_range))
                    .collect::<Vec<_>>()
            };
            assert_eq!(ranges(&rebuilt), ranges(&snapshot));

            let mut text = initial_snapshot.text();
            for (snapshot, edits) in snapshot_edits.drain(..) {
                let new_text = snapshot.text();
                text = replay_edits(&text, &new_text, &edits, |p| p.0);
                assert_eq!(text, new_text, "{edits:?}");
                initial_snapshot = snapshot;
            }
        }
    });
}

#[test]
fn test_buffer_rows() {
    let text = sample_text(6, 6, 'a') + "\n";
    let buffer = TextSnapshot::new(&text);
    let (mut map, _) = ReplaceMap::new(buffer.clone());
    map.fold([
        points(&buffer, (0, 2), (2, 2)),
        points(&buffer, (3, 1), (4, 1)),
    ]);

    let snapshot = map.snapshot();
    assert_eq!(snapshot.text(), "aa⋯cccc\nd⋯eeeee\nffffff\n");
    assert_eq!(snapshot.buffer_lines(0).collect::<Vec<_>>(), [0, 3, 5, 6]);
    assert_eq!(snapshot.buffer_lines(3).collect::<Vec<_>>(), [6]);
}

#[test]
fn test_random_chunk_bitmaps() {
    for_each_seed(100, |mut rng| {
        let text_len = rng.random_range(0..10000);
        let text = random_text(&mut rng, text_len);
        let (mut map, _) = ReplaceMap::new(TextSnapshot::new(&text));

        let mutation_count = rng.random_range(1..10);
        for _ in 0..mutation_count {
            randomly_mutate(&mut map, &mut rng);
        }

        let snapshot = map.snapshot();
        for chunk in snapshot.chunks(ReplacePoint::zero()..snapshot.max_point()) {
            let chunk_text = chunk.text;
            assert!(!chunk_text.is_empty(), "empty chunk");
            assert!(
                chunk_text.len() <= 128,
                "Chunk text length {} exceeds 128 bytes",
                chunk_text.len()
            );
            assert_eq!(chunk, Chunk::new(chunk_text), "bitmaps of {chunk_text:?}");
        }
    });
}

// ----- folds: Glyphvane's rules ----------------------------------------------------

#[test]
fn edits_inside_a_fold_unfold_it_and_edits_beside_it_keep_it() {
    // The fold covers `bc\ndef`.
    let text = "abc\ndefg\nh";
    let fold = 1..7;
    let cases: &[(Range<usize>, &str, &str)] = &[
        // Insertions at the fold's ends stay outside it.
        (1..1, "X", "aX⋯g\nh"),
        (7..7, "X", "a⋯Xg\nh"),
        // Deleting next to it.
        (0..1, "", "⋯g\nh"),
        (7..8, "", "a⋯\nh"),
        // Replacing the same text inside it changes nothing.
        (2..4, "c\n", "a⋯g\nh"),
    ];
    for (range, insert, expected) in cases {
        let mut buffer = Buffer::new(text);
        let (mut map, _) = ReplaceMap::new(buffer.snapshot());
        map.fold([fold.clone()]);
        let summary = buffer.edit([(range.clone(), *insert)]).unwrap();
        let (snapshot, _, unfolded) = map.sync_with_unfolded(buffer.snapshot(), &summary);
        assert_eq!(snapshot.text(), *expected, "{range:?} -> {insert:?}");
        assert!(unfolded.is_empty());
    }

    let removing: &[(Range<usize>, &str)] = &[
        (3..3, "X"),
        (2..3, ""),
        (0..2, ""),
        (6..8, "Z"),
        (0..10, ""),
        (1..7, "new"),
        (4..5, "D"),
    ];
    for (range, insert) in removing {
        let mut buffer = Buffer::new(text);
        let (mut map, _) = ReplaceMap::new(buffer.snapshot());
        map.fold([fold.clone()]);
        let summary = buffer.edit([(range.clone(), *insert)]).unwrap();
        let (snapshot, edits, unfolded) = map.sync_with_unfolded(buffer.snapshot(), &summary);
        assert_eq!(snapshot.text(), normalize(&buffer.to_string()), "{range:?}");
        assert_eq!(unfolded.len(), 1, "{range:?}");
        assert_eq!(unfolded[0].range, fold, "{range:?}");
        assert_eq!(
            replay_edits("a⋯g\nh", &snapshot.text(), &edits, |p| p.0),
            snapshot.text()
        );
    }
}

#[test]
fn folds_keep_out_of_crlfs() {
    let mut buffer = Buffer::new("a\r\nb\r\nc");
    let (mut map, _) = ReplaceMap::new(buffer.snapshot());
    // The start inside the first CRLF moves after it, the end inside the
    // second before it.
    let (snapshot, _) = map.fold([2..5]);
    assert_eq!(snapshot.folds()[0].range, 3..4);
    assert_eq!(snapshot.text(), "a\n⋯\nc");

    // A CR typed at the fold's start stays outside it.
    let (mut map, _) = ReplaceMap::new(buffer.snapshot());
    map.fold([1..4]);
    assert_eq!(map.snapshot().text(), "a⋯\nc");
    let summary = buffer.edit([(1..1, "\r")]).unwrap();
    let (snapshot, edits) = map.sync(buffer.snapshot(), &summary);
    assert_eq!(buffer.to_string(), "a\r\r\nb\r\nc");
    assert_eq!(
        snapshot.text(),
        expected_text(snapshot.buffer(), snapshot.folds())
    );
    assert_eq!(
        replay_edits("a⋯\nc", &snapshot.text(), &edits, |p| p.0),
        snapshot.text()
    );
}

#[test]
fn placeholders_and_lines() {
    let text = "# A\nbody\n\n# B\nmore\n";
    let buffer = TextSnapshot::new(text);
    let (mut map, _) = ReplaceMap::new(buffer.clone());
    let (snapshot, _) = map.fold([3..8, 13..18]);
    assert_eq!(snapshot.text(), "# A⋯\n\n# B⋯\n");
    let placeholders = snapshot.placeholders(0..4);
    assert_eq!(placeholders.len(), 2);
    assert_eq!(
        placeholders[0].range,
        ReplacePoint::new(0, 3)..ReplacePoint::new(0, 6)
    );
    assert_eq!(placeholders[0].buffer_range, 3..8);
    assert_eq!(
        placeholders[1].range,
        ReplacePoint::new(2, 3)..ReplacePoint::new(2, 6)
    );
    assert_eq!(snapshot.placeholders(1..2), Vec::new());
    assert_eq!(
        snapshot.placeholder_at(ReplacePoint::new(0, 4)),
        Some(placeholders[0].clone())
    );
    assert_eq!(snapshot.placeholder_at(ReplacePoint::new(0, 6)), None);
    assert_eq!(snapshot.buffer_lines(0).collect::<Vec<_>>(), [0, 2, 3, 5]);
    assert_eq!(
        (0..6)
            .map(|line| snapshot.is_line_folded(line))
            .collect::<Vec<_>>(),
        [true, true, false, true, true, false]
    );
    // Buffer positions inside a fold go to the placeholder's ends.
    for (point, bias, expected) in [
        (BufferPoint::new(0, 3), Bias::Right, ReplacePoint::new(0, 3)),
        (BufferPoint::new(1, 2), Bias::Left, ReplacePoint::new(0, 3)),
        (BufferPoint::new(1, 2), Bias::Right, ReplacePoint::new(0, 6)),
        (BufferPoint::new(1, 4), Bias::Left, ReplacePoint::new(0, 6)),
    ] {
        assert_eq!(
            snapshot.to_replace_point(point, bias),
            expected,
            "{point:?}"
        );
    }
    assert_eq!(
        snapshot.to_buffer_point(ReplacePoint::new(0, 5), Bias::Left),
        BufferPoint::new(0, 3)
    );
    assert_eq!(
        snapshot.to_buffer_point(ReplacePoint::new(0, 5), Bias::Right),
        BufferPoint::new(1, 4)
    );
    // Removing folds by id and all at once.
    let first = snapshot.folds()[0].id;
    let (snapshot, edits) = map.remove_folds([first]);
    assert_eq!(snapshot.text(), "# A\nbody\n\n# B⋯\n");
    assert_eq!(edits, vec![edit((0, 3)..(0, 6), (0, 3)..(1, 4))]);
    let (snapshot, _) = map.unfold_all();
    assert_eq!(snapshot.text(), text);
}

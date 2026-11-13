// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Example-based tests of positions, lines, edits, anchors and chunks.

use gv_text::{
    Anchor, Bias, Buffer, EditError, LineEnding, LineInfo, OffsetUtf16, Point, PointUtf16,
    PositionError, Revision, TextSnapshot, Transaction,
};

fn snap(text: &str) -> TextSnapshot {
    TextSnapshot::new(text)
}

#[test]
fn empty_text() {
    let t = snap("");
    assert_eq!(t.len(), 0);
    assert!(t.is_empty());
    assert_eq!(t.line_count(), 1);
    assert_eq!(t.offset_to_point(0), Ok(Point::new(0, 0)));
    assert_eq!(t.point_to_offset(Point::new(0, 0)), Ok(0));
    assert_eq!(
        t.point_to_offset(Point::new(0, 1)),
        Err(PositionError::ColumnOutOfBounds {
            line: 0,
            column: 1,
            max: 0
        })
    );
    assert_eq!(
        t.point_to_offset(Point::new(1, 0)),
        Err(PositionError::LineOutOfBounds {
            line: 1,
            line_count: 1
        })
    );
    assert_eq!(
        t.offset_to_point(1),
        Err(PositionError::OffsetOutOfBounds { offset: 1, len: 0 })
    );
    assert_eq!(t.offset_to_utf16(0), Ok(OffsetUtf16(0)));
    assert_eq!(t.utf16_to_offset(OffsetUtf16(0)), Ok(0));
    assert_eq!(t.max_point(), Point::new(0, 0));
    assert_eq!(t.max_point_utf16(), PointUtf16::new(0, 0));
    assert_eq!(t.line_range(0), Ok(0..0));
    assert_eq!(t.line_terminator(0), Ok(None));
    assert_eq!(t.clip_offset(5, Bias::Right), 0);
    assert_eq!(t.clip_point(Point::new(3, 3), Bias::Left), Point::new(0, 0));
    assert_eq!(t.text_for_range(0..0).as_deref(), Ok(""));
}

#[test]
fn emoji_and_surrogate_pairs() {
    // 😀 is 4 bytes in UTF-8 and a surrogate pair in UTF-16.
    let t = snap("a😀b");
    assert_eq!(t.len(), 6);
    assert_eq!(t.len_utf16(), OffsetUtf16(4));
    assert_eq!(t.char_count(), 3);
    assert_eq!(t.offset_to_utf16(1), Ok(OffsetUtf16(1)));
    assert_eq!(t.offset_to_utf16(5), Ok(OffsetUtf16(3)));
    assert_eq!(t.offset_to_utf16(6), Ok(OffsetUtf16(4)));
    for inside in 2..5 {
        assert_eq!(
            t.offset_to_utf16(inside),
            Err(PositionError::NotCharBoundary { offset: inside })
        );
        assert_eq!(t.clip_offset(inside, Bias::Left), 1);
        assert_eq!(t.clip_offset(inside, Bias::Right), 5);
    }
    assert_eq!(t.utf16_to_offset(OffsetUtf16(1)), Ok(1));
    assert_eq!(t.utf16_to_offset(OffsetUtf16(3)), Ok(5));
    assert_eq!(
        t.utf16_to_offset(OffsetUtf16(2)),
        Err(PositionError::InsideSurrogatePair { offset: 2 })
    );
    assert_eq!(
        t.utf16_to_offset(OffsetUtf16(5)),
        Err(PositionError::Utf16OutOfBounds { offset: 5, len: 4 })
    );
    assert_eq!(
        t.clip_offset_utf16(OffsetUtf16(2), Bias::Left),
        OffsetUtf16(1)
    );
    assert_eq!(
        t.clip_offset_utf16(OffsetUtf16(2), Bias::Right),
        OffsetUtf16(3)
    );
    assert_eq!(
        t.clip_offset_utf16(OffsetUtf16(9), Bias::Left),
        OffsetUtf16(4)
    );
    assert_eq!(
        t.point_utf16_to_offset(PointUtf16::new(0, 2)),
        Err(PositionError::InsideSurrogatePair { offset: 2 })
    );
    assert_eq!(t.point_utf16_to_offset(PointUtf16::new(0, 3)), Ok(5));
    assert_eq!(
        t.clip_point_utf16(PointUtf16::new(0, 2), Bias::Right),
        PointUtf16::new(0, 3)
    );
    assert_eq!(
        t.clip_point_utf16(PointUtf16::new(0, 2), Bias::Left),
        PointUtf16::new(0, 1)
    );
    assert_eq!(t.offset_to_point_utf16(6), Ok(PointUtf16::new(0, 4)));
    assert_eq!(t.clip_point(Point::new(0, 3), Bias::Left), Point::new(0, 1));
    assert_eq!(
        t.clip_point(Point::new(0, 3), Bias::Right),
        Point::new(0, 5)
    );
}

#[test]
fn surrogate_pair_at_text_end() {
    let t = snap("😀");
    assert_eq!(
        t.utf16_to_offset(OffsetUtf16(1)),
        Err(PositionError::InsideSurrogatePair { offset: 1 })
    );
    assert_eq!(
        t.clip_offset_utf16(OffsetUtf16(1), Bias::Right),
        OffsetUtf16(2)
    );
    assert_eq!(
        t.clip_offset_utf16(OffsetUtf16(1), Bias::Left),
        OffsetUtf16(0)
    );
    assert_eq!(t.clip_utf16_range(OffsetUtf16(1)..OffsetUtf16(1)), 0..4);
}

#[test]
fn cjk() {
    let t = snap("日本語\n漢字");
    assert_eq!(t.offset_to_point(10), Ok(Point::new(1, 0)));
    assert_eq!(t.offset_to_point_utf16(13), Ok(PointUtf16::new(1, 1)));
    assert_eq!(t.point_to_offset(Point::new(1, 3)), Ok(13));
    assert_eq!(
        t.point_to_offset(Point::new(1, 1)),
        Err(PositionError::NotCharBoundary { offset: 11 })
    );
    assert_eq!(t.point_utf16_to_offset(PointUtf16::new(1, 2)), Ok(16));
    assert_eq!(t.point_utf16_to_offset(PointUtf16::new(0, 3)), Ok(9));
    assert_eq!(
        t.point_utf16_to_offset(PointUtf16::new(0, 4)),
        Err(PositionError::ColumnOutOfBounds {
            line: 0,
            column: 4,
            max: 3
        })
    );
    assert_eq!(t.max_point_utf16(), PointUtf16::new(1, 2));
}

#[test]
fn combining_marks_are_separate_chars() {
    // "e" + COMBINING ACUTE ACCENT (2 bytes). Grapheme handling belongs to
    // the editor; here every char boundary is a valid position.
    let t = snap("e\u{301}x");
    assert!(t.is_char_boundary(1));
    assert!(!t.is_char_boundary(2));
    assert_eq!(t.offset_to_utf16(3), Ok(OffsetUtf16(2)));
    assert_eq!(t.utf16_to_offset(OffsetUtf16(1)), Ok(1));
    assert_eq!(t.clip_offset(2, Bias::Left), 1);
}

#[test]
fn mixed_line_endings() {
    let t = snap("a\r\nb\rc\nd");
    assert_eq!(t.line_count(), 4);
    let lines: Vec<LineInfo> = t.lines_from(0).collect();
    assert_eq!(
        lines,
        vec![
            LineInfo {
                line: 0,
                content: 0..1,
                terminator: Some(LineEnding::CrLf)
            },
            LineInfo {
                line: 1,
                content: 3..4,
                terminator: Some(LineEnding::Cr)
            },
            LineInfo {
                line: 2,
                content: 5..6,
                terminator: Some(LineEnding::Lf)
            },
            LineInfo {
                line: 3,
                content: 7..8,
                terminator: None
            },
        ]
    );
    assert_eq!(lines[0].range_with_terminator(), 0..3);
    assert_eq!(t.line_range_with_terminator(1), Ok(3..5));
    assert_eq!(t.line_info(2).map(|l| l.content), Ok(5..6));
    assert_eq!(t.lines_from(3).count(), 1);
    assert_eq!(t.lines_from(4).count(), 0);
    // Between CR and LF.
    assert!(t.is_inside_crlf(2));
    assert!(!t.is_inside_crlf(1));
    assert!(!t.is_inside_crlf(4));
    assert_eq!(t.offset_to_point(2), Ok(Point::new(0, 2)));
    assert_eq!(t.point_to_offset(Point::new(0, 2)), Ok(2));
    assert_eq!(
        t.point_to_offset(Point::new(0, 3)),
        Err(PositionError::ColumnOutOfBounds {
            line: 0,
            column: 3,
            max: 2
        })
    );
    // A lone CR line has no inner position.
    assert_eq!(
        t.point_to_offset(Point::new(1, 2)),
        Err(PositionError::ColumnOutOfBounds {
            line: 1,
            column: 2,
            max: 1
        })
    );
    assert_eq!(t.offset_to_point(3), Ok(Point::new(1, 0)));
    assert_eq!(t.offset_to_point(4), Ok(Point::new(1, 1)));
    assert_eq!(t.offset_to_point(5), Ok(Point::new(2, 0)));
    assert_eq!(t.offset_to_point_utf16(2), Ok(PointUtf16::new(0, 2)));
    assert_eq!(t.point_utf16_to_offset(PointUtf16::new(0, 2)), Ok(2));
    // Clipping past the end of a line stops before the terminator.
    assert_eq!(
        t.clip_point(Point::new(0, 9), Bias::Right),
        Point::new(0, 1)
    );
    assert_eq!(
        t.clip_point_utf16(PointUtf16::new(1, 9), Bias::Right),
        PointUtf16::new(1, 1)
    );
    let stats = t.line_ending_stats();
    assert_eq!((stats.lf, stats.crlf, stats.cr), (1, 1, 1));
    assert!(stats.is_mixed());
}

#[test]
fn trailing_terminators_make_an_empty_last_line() {
    for text in ["x\n", "x\r", "x\r\n"] {
        let t = snap(text);
        assert_eq!(t.line_count(), 2, "{text:?}");
        assert_eq!(t.offset_to_point(t.len()), Ok(Point::new(1, 0)));
        assert_eq!(t.max_point(), Point::new(1, 0));
        assert_eq!(t.line_range(1), Ok(t.len()..t.len()));
    }
    let t = snap("\r\r");
    assert_eq!(t.line_count(), 3);
}

#[test]
#[expect(
    clippy::reversed_empty_ranges,
    reason = "inverted ranges are under test"
)]
fn ranges_and_chunks() {
    let text = "αβγ 😀 ".repeat(100);
    let t = snap(&text);
    assert_eq!(
        t.chunks(0..t.len()).map(|c| c.collect::<String>()).ok(),
        Some(text.clone())
    );
    assert!(t.all_chunks().all(|c| c.len() <= 128));
    assert_eq!(t.text_for_range(2..6).as_deref(), Ok("βγ"));
    assert_eq!(
        t.chunks(1..4).err(),
        Some(PositionError::NotCharBoundary { offset: 1 })
    );
    assert_eq!(
        t.chunks(4..2).err(),
        Some(PositionError::InvertedRange { start: 4, end: 2 })
    );
    assert_eq!(t.range_to_utf16(2..6), Ok(OffsetUtf16(1)..OffsetUtf16(3)));
    assert_eq!(t.utf16_to_range(OffsetUtf16(1)..OffsetUtf16(3)), Ok(2..6));
    // An untrusted, inverted UTF-16 range cutting a surrogate pair.
    assert_eq!(t.clip_utf16_range(OffsetUtf16(5)..OffsetUtf16(1)), 2..11);
    assert_eq!(t.chars_at(7).map(|mut c| c.next()), Ok(Some('😀')));
    assert_eq!(t.reversed_chars_at(7).map(|mut c| c.next()), Ok(Some(' ')));
}

#[test]
fn transactions_apply_in_one_pass() {
    let mut buffer = Buffer::new("one\ntwo\nthree\n");
    assert_eq!(buffer.revision(), Revision::ZERO);
    let summary = buffer
        .apply(
            &Transaction::new()
                .replace(0..3, "1")
                .insert(4, "> ")
                .delete(8..14),
        )
        .expect("valid");
    assert_eq!(buffer.to_string(), "1\n> two\n");
    assert_eq!(buffer.revision(), Revision(1));
    assert_eq!(summary.old_revision(), Revision::ZERO);
    assert_eq!(summary.new_revision(), Revision(1));
    let ranges: Vec<_> = summary
        .edits()
        .iter()
        .map(|e| (e.old.clone(), e.new.clone()))
        .collect();
    assert_eq!(ranges, vec![(0..3, 0..1), (4..4, 2..4), (8..14, 8..8)]);
    assert_eq!(buffer.line_count(), 3);
    assert_eq!(buffer.line_range(1), Ok(2..7));
}

#[test]
#[expect(
    clippy::reversed_empty_ranges,
    reason = "inverted ranges are under test"
)]
fn invalid_transactions_change_nothing() {
    let mut buffer = Buffer::new("aé\nb");
    let before = buffer.snapshot();
    let cases = [
        (
            Transaction::new().replace(3..1, ""),
            EditError::InvertedRange {
                index: 0,
                range: 3..1,
            },
        ),
        (
            Transaction::new().insert(9, "x"),
            EditError::OutOfBounds {
                index: 0,
                range: 9..9,
                len: 5,
            },
        ),
        (
            Transaction::new().insert(0, "x").insert(2, "y"),
            EditError::NotCharBoundary {
                index: 1,
                offset: 2,
            },
        ),
        (
            Transaction::new().insert(3, "x").insert(1, "y"),
            EditError::Unsorted { index: 1 },
        ),
        (
            Transaction::new().delete(0..3).delete(1..4),
            EditError::Overlapping { index: 1 },
        ),
    ];
    for (transaction, error) in cases {
        assert_eq!(buffer.apply(&transaction), Err(error));
        assert!(buffer.text_eq(&before));
        assert_eq!(buffer.revision(), Revision::ZERO);
    }
}

#[test]
fn noop_transactions_keep_the_revision() {
    let mut buffer = Buffer::new("abc");
    let summary = buffer
        .apply(&Transaction::new().insert(1, "").replace(2..2, ""))
        .expect("valid");
    assert!(summary.is_empty());
    assert_eq!(buffer.revision(), Revision::ZERO);
    let summary = buffer.apply(&Transaction::new()).expect("valid");
    assert!(summary.is_empty());
}

#[test]
fn insertions_at_the_same_offset_apply_in_order() {
    let mut buffer = Buffer::new("ab");
    let summary = buffer
        .edit([(1..1, "x"), (1..1, "y"), (1..2, "Z")])
        .expect("valid");
    assert_eq!(buffer.to_string(), "axyZ");
    let news: Vec<_> = summary.edits().iter().map(|e| e.new.clone()).collect();
    assert_eq!(news, vec![1..2, 2..3, 3..4]);
}

#[test]
fn edits_join_and_split_crlf() {
    // CR + inserted LF becomes one CRLF terminator.
    let mut buffer = Buffer::new("a\rb");
    buffer.edit([(2..2, "\n")]).expect("valid");
    assert_eq!(buffer.line_count(), 2);
    assert_eq!(buffer.line_terminator(0), Ok(Some(LineEnding::CrLf)));
    // Inserting between CR and LF splits it into CR + LF lines.
    buffer.edit([(2..2, "x")]).expect("valid");
    assert_eq!(buffer.to_string(), "a\rx\nb");
    assert_eq!(buffer.line_count(), 3);
    assert_eq!(buffer.line_terminator(0), Ok(Some(LineEnding::Cr)));
    assert_eq!(buffer.line_terminator(1), Ok(Some(LineEnding::Lf)));
    // Deleting the text between them joins them again.
    buffer.edit([(2..3, "")]).expect("valid");
    assert_eq!(buffer.line_count(), 2);
    assert_eq!(buffer.line_terminator(0), Ok(Some(LineEnding::CrLf)));
    // Deleting the LF of a CRLF leaves a lone CR.
    buffer.edit([(2..3, "")]).expect("valid");
    assert_eq!(buffer.to_string(), "a\rb");
    assert_eq!(buffer.line_terminator(0), Ok(Some(LineEnding::Cr)));
    // An LF typed before a CR: "\n\r" is two terminators.
    buffer.edit([(1..1, "\n")]).expect("valid");
    assert_eq!(buffer.line_count(), 3);
}

#[test]
fn edits_on_a_long_line_keep_the_index() {
    let line = "x".repeat(100_000);
    let mut buffer = Buffer::new(&format!("{line}\r\n{line}"));
    buffer
        .edit([(50_000..50_000, "\r"), (100_001..100_001, "\n")])
        .expect("valid");
    assert_eq!(buffer.line_count(), 4);
    let terminators: Vec<_> = buffer.lines_from(0).map(|l| l.terminator).collect();
    assert_eq!(
        terminators,
        vec![
            Some(LineEnding::Cr),
            Some(LineEnding::CrLf),
            Some(LineEnding::Lf),
            None
        ]
    );
}

#[test]
fn anchors_follow_edits() {
    let mut buffer = Buffer::new("hello world");
    let anchors = [
        Anchor::before(0),
        Anchor::after(0),
        Anchor::before(6),
        Anchor::after(6),
        Anchor::before(8),
        Anchor::after(8),
        Anchor::before(11),
    ];
    // Insert at 0, replace "wor" (6..9) with "WOR!".
    let summary = buffer.edit([(0..0, ">> "), (6..9, "WOR!")]).expect("valid");
    assert_eq!(buffer.to_string(), ">> hello WOR!ld");
    let moved: Vec<usize> = anchors
        .iter()
        .map(|a| a.transform(&summary).offset)
        .collect();
    assert_eq!(moved, vec![0, 3, 9, 9, 9, 13, 15]);
}

#[test]
fn clipping_a_valid_position_is_identity() {
    let t = snap("a\r\né😀\rz");
    for offset in 0..=t.len() {
        if t.is_char_boundary(offset) {
            for bias in [Bias::Left, Bias::Right] {
                assert_eq!(t.clip_offset(offset, bias), offset);
                let point = t.offset_to_point(offset).expect("valid");
                assert_eq!(t.clip_point(point, bias), point);
                let p16 = t.offset_to_point_utf16(offset).expect("valid");
                assert_eq!(t.clip_point_utf16(p16, bias), p16);
            }
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Property tests against naive `String` models.
//!
//! Each property runs a moderate number of cases in `cargo test`. The
//! `*_heavy` variants run many more and are `#[ignore]`d; run them with
//! `cargo test -p gv-text --test properties -- --ignored`.

use gv_text::encoding::{decode_with, encode, encode_chunks};
use gv_text::{
    Bias, Bom, Buffer, DecodeStatus, Encoding, LineEnding, OffsetUtf16, Point, PointUtf16,
    PositionError, TextSnapshot, Transaction, decode,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError};

const CASES: u32 = 64;
const HEAVY_CASES: u32 = 4096;

fn config(cases: u32) -> Config {
    Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    }
}

// ----- generators -------------------------------------------------------

/// Characters that exercise every code path: ASCII, every terminator,
/// 2-, 3- and 4-byte UTF-8, a combining mark and NUL.
fn text_char() -> impl Strategy<Value = char> {
    prop_oneof![
        6 => prop::sample::select(vec!['a', 'b', ' ', '#', '*']),
        3 => Just('\n'),
        2 => Just('\r'),
        1 => prop::sample::select(vec!['é', 'ß', '\u{301}', '\0']),
        1 => prop::sample::select(vec!['日', '語', '€']),
        1 => prop::sample::select(vec!['😀', '🍐', '\u{10FFFF}']),
    ]
}

fn text(max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(text_char(), 0..max).prop_map(|chars| chars.into_iter().collect())
}

/// A replacement as random numbers; turned into a valid edit by `build`.
type RawEdit = (usize, usize, String);

fn raw_edits() -> impl Strategy<Value = Vec<RawEdit>> {
    prop::collection::vec(
        (
            any::<usize>(),
            prop_oneof![4 => 0usize..8, 1 => 0usize..300],
            prop_oneof![4 => text(6), 1 => text(300)],
        ),
        0..6,
    )
}

/// Turns random numbers into a sorted, non-overlapping transaction on
/// char boundaries.
fn build(text: &str, raw: &[RawEdit]) -> Vec<(std::ops::Range<usize>, String)> {
    let floor = |mut i: usize| {
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let mut starts: Vec<(usize, usize, &String)> = raw
        .iter()
        .map(|(pos, len, s)| (floor(pos % (text.len() + 1)), *len, s))
        .collect();
    starts.sort_by_key(|(start, _, _)| *start);
    let mut edits = Vec::new();
    let mut prev_end = 0;
    for (start, len, s) in starts {
        let start = start.max(prev_end);
        let mut end = (start + len).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        edits.push((start..end, s.clone()));
        prev_end = end;
    }
    edits
}

fn apply_naive(text: &str, edits: &[(std::ops::Range<usize>, String)]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for (range, s) in edits {
        out.push_str(&text[pos..range.start]);
        out.push_str(s);
        pos = range.end;
    }
    out.push_str(&text[pos..]);
    out
}

// ----- naive models -----------------------------------------------------

/// `(start, content end, terminator)` for every line.
fn naive_lines(text: &str) -> Vec<(usize, usize, Option<LineEnding>)> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push((start, i, Some(LineEnding::Lf)));
                start = i + 1;
            }
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                lines.push((start, i, Some(LineEnding::CrLf)));
                i += 1;
                start = i + 1;
            }
            b'\r' => {
                lines.push((start, i, Some(LineEnding::Cr)));
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    lines.push((start, bytes.len(), None));
    lines
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Checks every query and conversion of `snapshot` against `text`.
fn check_snapshot(snapshot: &TextSnapshot, text: &str) -> Result<(), TestCaseError> {
    prop_assert_eq!(snapshot.to_string(), text);
    prop_assert_eq!(snapshot.len(), text.len());
    prop_assert_eq!(snapshot.len_utf16(), OffsetUtf16(utf16_len(text)));
    prop_assert_eq!(snapshot.char_count(), text.chars().count());

    let lines = naive_lines(text);
    prop_assert_eq!(snapshot.line_count(), lines.len());
    let stats = snapshot.line_ending_stats();
    let count = |kind| lines.iter().filter(|l| l.2 == Some(kind)).count();
    prop_assert_eq!(
        (stats.lf, stats.crlf, stats.cr),
        (
            count(LineEnding::Lf),
            count(LineEnding::CrLf),
            count(LineEnding::Cr)
        )
    );
    let iterated: Vec<_> = snapshot
        .lines_from(0)
        .map(|l| (l.content.start, l.content.end, l.terminator))
        .collect();
    prop_assert_eq!(&iterated, &lines);
    let line_count = lines.len();
    prop_assert_eq!(
        snapshot.line_range(line_count),
        Err(PositionError::LineOutOfBounds {
            line: line_count,
            line_count
        })
    );

    // UTF-16 offset of every char boundary, and the line of every offset.
    let mut u16_at = vec![usize::MAX; text.len() + 1];
    let mut units = 0;
    for (i, c) in text.char_indices() {
        u16_at[i] = units;
        units += c.len_utf16();
    }
    u16_at[text.len()] = units;
    let mut line_at = Vec::with_capacity(text.len() + 1);
    let mut line = 0;
    for offset in 0..=text.len() {
        while line + 1 < lines.len() && lines[line + 1].0 <= offset {
            line += 1;
        }
        line_at.push(line);
    }
    for offset in 0..=text.len() + 1 {
        if offset > text.len() {
            prop_assert_eq!(
                snapshot.offset_to_point(offset),
                Err(PositionError::OffsetOutOfBounds {
                    offset,
                    len: text.len()
                })
            );
            prop_assert_eq!(snapshot.clip_offset(offset, Bias::Left), text.len());
            continue;
        }
        if !text.is_char_boundary(offset) {
            let err = Err(PositionError::NotCharBoundary { offset });
            prop_assert_eq!(snapshot.offset_to_point(offset), err);
            prop_assert_eq!(
                snapshot.offset_to_utf16(offset),
                err.map(|_| OffsetUtf16(0))
            );
            prop_assert_eq!(
                snapshot.offset_to_point_utf16(offset),
                err.map(|_| PointUtf16::default())
            );
            let floor = (0..offset).rev().find(|&i| text.is_char_boundary(i));
            let ceil = (offset..=text.len()).find(|&i| text.is_char_boundary(i));
            prop_assert_eq!(Some(snapshot.clip_offset(offset, Bias::Left)), floor);
            prop_assert_eq!(Some(snapshot.clip_offset(offset, Bias::Right)), ceil);
            continue;
        }
        let line = line_at[offset];
        let start = lines[line].0;
        let point = Point::new(line, offset - start);
        prop_assert_eq!(snapshot.offset_to_point(offset), Ok(point));
        prop_assert_eq!(snapshot.point_to_offset(point), Ok(offset));
        let u16 = OffsetUtf16(u16_at[offset]);
        prop_assert_eq!(snapshot.offset_to_utf16(offset), Ok(u16));
        prop_assert_eq!(snapshot.utf16_to_offset(u16), Ok(offset));
        let p16 = PointUtf16::new(line, u16_at[offset] - u16_at[start]);
        prop_assert_eq!(snapshot.offset_to_point_utf16(offset), Ok(p16));
        prop_assert_eq!(snapshot.point_utf16_to_offset(p16), Ok(offset));
        prop_assert_eq!(snapshot.is_inside_crlf(offset), {
            let b = text.as_bytes();
            offset > 0 && b[offset - 1] == b'\r' && b.get(offset) == Some(&b'\n')
        });
        for bias in [Bias::Left, Bias::Right] {
            prop_assert_eq!(snapshot.clip_offset(offset, bias), offset);
            prop_assert_eq!(snapshot.clip_point(point, bias), point);
            prop_assert_eq!(snapshot.clip_point_utf16(p16, bias), p16);
            prop_assert_eq!(snapshot.clip_offset_utf16(u16, bias), u16);
        }
    }

    // Every UTF-16 offset: valid iff it is a char boundary in UTF-16.
    let len16 = utf16_len(text);
    let mut boundary16 = vec![false; len16 + 1];
    for &u in u16_at.iter().filter(|&&u| u != usize::MAX) {
        boundary16[u] = true;
    }
    for u in 0..=len16 + 1 {
        let result = snapshot.utf16_to_offset(OffsetUtf16(u));
        if u > len16 {
            prop_assert_eq!(
                result,
                Err(PositionError::Utf16OutOfBounds {
                    offset: u,
                    len: len16
                })
            );
        } else if boundary16.get(u) == Some(&true) {
            prop_assert!(result.is_ok());
        } else {
            prop_assert_eq!(
                result,
                Err(PositionError::InsideSurrogatePair { offset: u })
            );
            let left = snapshot.clip_offset_utf16(OffsetUtf16(u), Bias::Left);
            let right = snapshot.clip_offset_utf16(OffsetUtf16(u), Bias::Right);
            prop_assert_eq!((left.0, right.0), (u - 1, u + 1));
        }
    }

    // Every column on every line, including out-of-range ones.
    for (line, &(start, content_end, terminator)) in lines.iter().enumerate() {
        let max = content_end - start + usize::from(terminator == Some(LineEnding::CrLf));
        for column in 0..=max + 2 {
            let result = snapshot.point_to_offset(Point::new(line, column));
            if column > max {
                prop_assert_eq!(
                    result,
                    Err(PositionError::ColumnOutOfBounds { line, column, max })
                );
                prop_assert_eq!(
                    snapshot.clip_point(Point::new(line, column), Bias::Right),
                    Point::new(line, content_end - start)
                );
            } else if text.is_char_boundary(start + column) {
                prop_assert_eq!(result, Ok(start + column));
            } else {
                prop_assert_eq!(
                    result,
                    Err(PositionError::NotCharBoundary {
                        offset: start + column
                    })
                );
            }
        }
    }
    Ok(())
}

// ----- properties -------------------------------------------------------

/// (a) + (b): random transactions equal the same edits on a `String`, and
/// every conversion on the result matches the naive model.
fn transactions_match_string(initial: &str, rounds: &[Vec<RawEdit>]) -> Result<(), TestCaseError> {
    let mut buffer = Buffer::new(initial);
    let mut model = initial.to_owned();
    check_snapshot(buffer.text(), &model)?;
    for raw in rounds {
        let edits = build(&model, raw);
        let mut transaction = Transaction::new();
        for (range, s) in &edits {
            transaction.push(range.clone(), s.as_str());
        }
        let before = buffer.revision();
        let summary = buffer
            .apply(&transaction)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        model = apply_naive(&model, &edits);
        check_snapshot(buffer.text(), &model)?;
        // The summary's new ranges hold the inserted text.
        for (edit, (_, s)) in summary.edits().iter().zip(
            edits
                .iter()
                .filter(|(r, s)| !(r.is_empty() && s.is_empty())),
        ) {
            prop_assert_eq!(&model[edit.new.clone()], s.as_str());
        }
        let changed = !summary.is_empty();
        prop_assert_eq!(buffer.revision().0, before.0 + u64::from(changed));
    }
    Ok(())
}

/// Reference anchor transform for one edit at a time.
fn transform_one(offset: usize, bias: Bias, old: &std::ops::Range<usize>, new_len: usize) -> usize {
    let new_end = old.start + new_len;
    if offset < old.start {
        offset
    } else if old.is_empty() && offset == old.start {
        if bias == Bias::Left {
            old.start
        } else {
            new_end
        }
    } else if offset == old.start {
        old.start
    } else if offset < old.end {
        if bias == Bias::Left {
            old.start
        } else {
            new_end
        }
    } else {
        offset - old.end + new_end
    }
}

/// (c): anchor transformation through a whole transaction matches applying
/// the edits one at a time with the single-edit rule, and keeps anchor order.
fn anchors_match_reference(initial: &str, raw: &[RawEdit]) -> Result<(), TestCaseError> {
    let mut buffer = Buffer::new(initial);
    let edits = build(initial, raw);
    let summary = buffer
        .edit(edits.iter().map(|(r, s)| (r.clone(), s.clone())))
        .map_err(|e| TestCaseError::fail(e.to_string()))?;
    for bias in [Bias::Left, Bias::Right] {
        let mut prev = 0;
        for offset in (0..=initial.len()).filter(|&o| initial.is_char_boundary(o)) {
            // Sequential reference: each edit shifted by the earlier ones.
            let mut expected = offset;
            let mut delta: isize = 0;
            for (range, s) in &edits {
                let shifted = range.start.saturating_add_signed(delta)
                    ..range.end.saturating_add_signed(delta);
                expected = transform_one(expected, bias, &shifted, s.len());
                delta += s.len() as isize - range.len() as isize;
            }
            let actual = summary.transform_offset(offset, bias);
            prop_assert_eq!(actual, expected, "offset {} bias {:?}", offset, bias);
            prop_assert!(buffer.is_char_boundary(actual));
            prop_assert!(actual >= prev, "anchors keep their order");
            prev = actual;
        }
    }
    Ok(())
}

fn utf16_formats() -> Vec<(Encoding, Bom)> {
    vec![
        (Encoding::Utf8, Bom::Absent),
        (Encoding::Utf8, Bom::Present),
        (Encoding::Utf16Le, Bom::Absent),
        (Encoding::Utf16Le, Bom::Present),
        (Encoding::Utf16Be, Bom::Absent),
        (Encoding::Utf16Be, Bom::Present),
    ]
}

/// Joins lines with the given line-ending style (`None` mixes them).
fn with_line_endings(lines: &[String], style: Option<LineEnding>) -> String {
    let all = [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr];
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        if i + 1 < lines.len() {
            out.push_str(style.unwrap_or(all[i % 3]).as_str());
        }
    }
    out
}

/// (d): encode → decode → encode is byte-identical for Unicode encodings,
/// with and without BOM, for every line-ending style.
fn encoding_round_trips(lines: &[String], style: Option<LineEnding>) -> Result<(), TestCaseError> {
    let text = with_line_endings(lines, style);
    for (encoding, bom) in utf16_formats() {
        let format = gv_text::TextFormat {
            encoding,
            bom,
            line_ending: LineEnding::Lf,
        };
        let bytes = encode(&text, &format).map_err(|e| TestCaseError::fail(e.to_string()))?;
        // Without a BOM, UTF-16 is not detected; the user picks it.
        let decoded = if bom == Bom::Absent && encoding != Encoding::Utf8 {
            decode_with(&bytes, encoding)
        } else {
            decode(&bytes).map_err(|e| TestCaseError::fail(e.to_string()))?
        };
        prop_assert_eq!(&decoded.text, &text);
        prop_assert_eq!(decoded.status, DecodeStatus::Exact);
        prop_assert_eq!(decoded.format.encoding, encoding);
        prop_assert_eq!(decoded.format.bom, bom);
        if let Some(style) = style
            && text.contains(['\n', '\r'])
        {
            prop_assert_eq!(decoded.format.line_ending, style);
        }
        let again = encode(&decoded.text, &decoded.format)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(&again, &bytes);
        // Through a buffer and its chunks as well.
        let buffer = Buffer::from_decoded(&decoded);
        prop_assert_eq!(
            buffer
                .encode()
                .map_err(|e| TestCaseError::fail(e.to_string()))?,
            bytes
        );
    }
    Ok(())
}

/// (d), arbitrary bytes: whenever a decode claims to be exact, encoding the
/// text gives back exactly the input.
fn exact_decodes_round_trip(bytes: &[u8]) -> Result<(), TestCaseError> {
    let mut decodes = Vec::new();
    if let Ok(decoded) = decode(bytes) {
        decodes.push(decoded);
    }
    for label in [
        "utf-8",
        "utf-16le",
        "utf-16be",
        "windows-1252",
        "shift_jis",
        "gbk",
    ] {
        if let Some(encoding) = Encoding::for_label(label) {
            decodes.push(decode_with(bytes, encoding));
        }
    }
    for decoded in decodes {
        if decoded.status.is_exact() {
            let again = encode_chunks([decoded.text.as_str()], &decoded.format)
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            prop_assert_eq!(&again[..], bytes, "{:?}", decoded.format);
        }
    }
    Ok(())
}

fn line_ending_style() -> impl Strategy<Value = Option<LineEnding>> {
    prop::sample::select(vec![
        Some(LineEnding::Lf),
        Some(LineEnding::CrLf),
        Some(LineEnding::Cr),
        None,
    ])
}

/// Lines without terminators.
fn plain_lines() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(text(20).prop_map(|s| s.replace(['\n', '\r'], "")), 0..8)
}

proptest! {
    #![proptest_config(config(CASES))]

    #[test]
    fn prop_transactions_match_string(initial in text(300), rounds in prop::collection::vec(raw_edits(), 1..4)) {
        transactions_match_string(&initial, &rounds)?;
    }

    #[test]
    fn prop_anchors_match_reference(initial in text(80), raw in raw_edits()) {
        anchors_match_reference(&initial, &raw)?;
    }

    #[test]
    fn prop_encoding_round_trips(lines in plain_lines(), style in line_ending_style()) {
        encoding_round_trips(&lines, style)?;
    }

    #[test]
    fn prop_exact_decodes_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        exact_decodes_round_trip(&bytes)?;
    }
}

proptest! {
    #![proptest_config(config(HEAVY_CASES))]

    #[test]
    #[ignore = "slow; run with --ignored"]
    fn prop_transactions_match_string_heavy(initial in text(1500), rounds in prop::collection::vec(raw_edits(), 1..8)) {
        transactions_match_string(&initial, &rounds)?;
    }

    #[test]
    #[ignore = "slow; run with --ignored"]
    fn prop_anchors_match_reference_heavy(initial in text(300), raw in raw_edits()) {
        anchors_match_reference(&initial, &raw)?;
    }

    #[test]
    #[ignore = "slow; run with --ignored"]
    fn prop_encoding_round_trips_heavy(lines in plain_lines(), style in line_ending_style()) {
        encoding_round_trips(&lines, style)?;
    }

    #[test]
    #[ignore = "slow; run with --ignored"]
    fn prop_exact_decodes_round_trip_heavy(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        exact_decodes_round_trip(&bytes)?;
    }
}

/// Windows-1252 text survives detection and the round trip.
#[test]
fn windows_1252_round_trip() {
    let encoding = Encoding::for_label("windows-1252").expect("known");
    let text = "Größe: 5 µm — «café» naïve façade\r\n".repeat(3);
    let format = gv_text::TextFormat {
        encoding,
        bom: Bom::Absent,
        line_ending: LineEnding::CrLf,
    };
    let bytes = encode(&text, &format).expect("encodable");
    let decoded = decode(&bytes).expect("text");
    assert_eq!(decoded.text, text);
    assert!(decoded.status.is_exact());
    assert_eq!(
        encode(&decoded.text, &decoded.format).expect("encodable"),
        bytes
    );
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Text boundaries on a [`TextSnapshot`]: grapheme clusters and words
//! (Unicode UAX #29), line starts and ends, indentation.
//!
//! Every function takes any offset and treats it as clipped to the text
//! (clamped, and moved left out of a multi-byte character), so none of them
//! panics on a bad position. They read only a bounded window of text around
//! the position, growing it when a cluster or word runs past the window, so
//! the cost does not depend on the length of the document or of the line.

use gv_text::{Bias, TextSnapshot};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete, UnicodeSegmentation};

/// Bytes read per step when looking for a grapheme boundary.
const GRAPHEME_WINDOW: usize = 64;
/// Initial window for word boundaries; doubled while the word runs past it.
const WORD_WINDOW: usize = 256;
/// A word longer than this is cut: word motion stops after this many bytes.
const MAX_WORD_WINDOW: usize = 1 << 20;

/// The text in `start..end`, both clipped to character boundaries
/// (`start` to the left, `end` to the right).
fn window(text: &TextSnapshot, start: usize, end: usize) -> (usize, String) {
    let start = text.clip_offset(start, Bias::Left);
    let end = text.clip_offset(end, Bias::Right).max(start);
    (start, text.text_for_range(start..end).unwrap_or_default())
}

/// The offset clipped to the text and to a character boundary (left).
fn clip(text: &TextSnapshot, offset: usize) -> usize {
    text.clip_offset(offset, Bias::Left)
}

/// Feeds the text before `end` to a grapheme cursor. The cursor only asks
/// for context ending at a chunk start, which is a character boundary, so
/// the window ends exactly at `end` as the cursor requires.
fn provide_context(text: &TextSnapshot, cursor: &mut GraphemeCursor, end: usize) {
    let (start, context) = window(text, end.saturating_sub(GRAPHEME_WINDOW), end);
    debug_assert_eq!(start + context.len(), end);
    cursor.provide_context(&context, start);
}

/// The first grapheme cluster boundary after `offset`, or the end of the
/// text. A CRLF is one cluster.
pub fn next_grapheme_boundary(text: &TextSnapshot, offset: usize) -> usize {
    let len = text.len();
    let offset = clip(text, offset);
    if offset >= len {
        return len;
    }
    let mut cursor = GraphemeCursor::new(offset, len, true);
    let (mut chunk_start, mut chunk) = window(text, offset, offset + GRAPHEME_WINDOW);
    loop {
        match cursor.next_boundary(&chunk, chunk_start) {
            Ok(Some(boundary)) => return boundary,
            Ok(None) => return len,
            Err(GraphemeIncomplete::NextChunk) => {
                let next = chunk_start + chunk.len();
                (chunk_start, chunk) = window(text, next, next + GRAPHEME_WINDOW);
                if chunk.is_empty() {
                    return len;
                }
            }
            Err(GraphemeIncomplete::PreContext(end)) => provide_context(text, &mut cursor, end),
            Err(_) => return next_char_boundary(text, offset),
        }
    }
}

/// The last grapheme cluster boundary before `offset`, or 0.
pub fn prev_grapheme_boundary(text: &TextSnapshot, offset: usize) -> usize {
    let len = text.len();
    let offset = clip(text, offset);
    if offset == 0 {
        return 0;
    }
    let mut cursor = GraphemeCursor::new(offset, len, true);
    let (mut chunk_start, mut chunk) = window(text, offset.saturating_sub(GRAPHEME_WINDOW), offset);
    loop {
        match cursor.prev_boundary(&chunk, chunk_start) {
            Ok(Some(boundary)) => return boundary,
            Ok(None) => return 0,
            Err(GraphemeIncomplete::PrevChunk) => {
                let end = chunk_start;
                (chunk_start, chunk) = window(text, end.saturating_sub(GRAPHEME_WINDOW), end);
                if chunk.is_empty() {
                    return 0;
                }
            }
            Err(GraphemeIncomplete::PreContext(end)) => provide_context(text, &mut cursor, end),
            Err(_) => return prev_char_boundary(text, offset),
        }
    }
}

/// Whether `offset` is a grapheme cluster boundary. Offsets past the end
/// and inside a character are not.
pub fn is_grapheme_boundary(text: &TextSnapshot, offset: usize) -> bool {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return false;
    }
    if offset == 0 || offset == text.len() {
        return true;
    }
    let mut cursor = GraphemeCursor::new(offset, text.len(), true);
    let (mut chunk_start, mut chunk) = window(
        text,
        offset.saturating_sub(GRAPHEME_WINDOW),
        offset + GRAPHEME_WINDOW,
    );
    loop {
        match cursor.is_boundary(&chunk, chunk_start) {
            Ok(is_boundary) => return is_boundary,
            Err(GraphemeIncomplete::PreContext(end)) => provide_context(text, &mut cursor, end),
            Err(_) => {
                // Should not happen with a chunk around the offset; read a
                // wider one before giving up.
                let wider = chunk.len() * 2 + GRAPHEME_WINDOW;
                if wider > MAX_WORD_WINDOW {
                    return true;
                }
                (chunk_start, chunk) = window(
                    text,
                    offset.saturating_sub(wider),
                    offset.saturating_add(wider),
                );
            }
        }
    }
}

/// `offset` if it is a grapheme boundary, else the boundary before it.
pub fn snap_to_grapheme(text: &TextSnapshot, offset: usize) -> usize {
    let offset = clip(text, offset);
    if is_grapheme_boundary(text, offset) {
        offset
    } else {
        prev_grapheme_boundary(text, offset)
    }
}

fn next_char_boundary(text: &TextSnapshot, offset: usize) -> usize {
    text.clip_offset(offset + 1, Bias::Right)
}

fn prev_char_boundary(text: &TextSnapshot, offset: usize) -> usize {
    text.clip_offset(offset.saturating_sub(1), Bias::Left)
}

/// The class of a word-boundary segment, for word motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordClass {
    /// Spaces, tabs and line terminators.
    Whitespace,
    /// A segment with a letter or digit (UAX #29 keeps `can't`, `3.14` and
    /// `snake_case` whole).
    Word,
    /// Anything else; consecutive punctuation segments form one run.
    Punctuation,
}

fn classify(segment: &str) -> WordClass {
    if segment.chars().all(char::is_whitespace) {
        WordClass::Whitespace
    } else if segment.chars().any(char::is_alphanumeric) {
        WordClass::Word
    } else {
        WordClass::Punctuation
    }
}

/// The end of the next word after `offset`: whitespace (line terminators
/// included) is skipped, then a word or a run of punctuation is crossed.
/// Returns the end of the text if only whitespace follows.
pub fn next_word_end(text: &TextSnapshot, offset: usize) -> usize {
    let offset = clip(text, offset);
    let mut size = WORD_WINDOW;
    loop {
        let (start, chunk) = window(text, offset, offset.saturating_add(size));
        let window_end = start + chunk.len();
        let at_text_end = window_end >= text.len();
        let mut found: Option<(WordClass, usize)> = None;
        let mut cut = false;
        for (index, segment) in chunk.split_word_bound_indices() {
            let class = classify(segment);
            let end = start + index + segment.len();
            match found {
                None if class == WordClass::Whitespace => {}
                None => found = Some((class, end)),
                Some((WordClass::Punctuation, _)) if class == WordClass::Punctuation => {
                    found = Some((class, end));
                }
                Some(_) => break,
            }
            cut = end == window_end;
        }
        match found {
            // The word (or punctuation run) reaches the window end: it may
            // go on past it.
            Some(_) if cut && !at_text_end && size < MAX_WORD_WINDOW => size *= 2,
            Some((_, end)) => return snap_to_grapheme(text, end),
            None if at_text_end || size >= MAX_WORD_WINDOW => {
                return snap_to_grapheme(text, window_end);
            }
            None => size *= 2,
        }
    }
}

/// The start of the previous word before `offset`: whitespace (line
/// terminators included) is skipped backwards, then a word or a run of
/// punctuation is crossed. Returns 0 if only whitespace precedes.
pub fn prev_word_start(text: &TextSnapshot, offset: usize) -> usize {
    let offset = clip(text, offset);
    let mut size = WORD_WINDOW;
    loop {
        let (start, chunk) = window(text, offset.saturating_sub(size), offset);
        let at_text_start = start == 0;
        let mut found: Option<(WordClass, usize)> = None;
        let mut cut = false;
        for (index, segment) in chunk.split_word_bound_indices().rev() {
            let class = classify(segment);
            let seg_start = start + index;
            match found {
                None if class == WordClass::Whitespace => {}
                None => found = Some((class, seg_start)),
                Some((WordClass::Punctuation, _)) if class == WordClass::Punctuation => {
                    found = Some((class, seg_start));
                }
                Some(_) => break,
            }
            cut = index == 0;
        }
        match found {
            Some(_) if cut && !at_text_start && size < MAX_WORD_WINDOW => size *= 2,
            Some((_, word_start)) => return snap_to_grapheme(text, word_start),
            None if at_text_start || size >= MAX_WORD_WINDOW => {
                return snap_to_grapheme(text, start);
            }
            None => size *= 2,
        }
    }
}

/// The line holding `offset` (clipped).
pub fn line_of(text: &TextSnapshot, offset: usize) -> usize {
    text.line_of_offset(clip(text, offset)).unwrap_or(0)
}

/// The start of the line holding `offset`.
pub fn line_start(text: &TextSnapshot, offset: usize) -> usize {
    text.line_start(line_of(text, offset)).unwrap_or(0)
}

/// The end of the content of the line holding `offset` (before its
/// terminator).
pub fn line_end(text: &TextSnapshot, offset: usize) -> usize {
    text.line_range(line_of(text, offset))
        .map_or(text.len(), |range| range.end)
}

/// The end of the indentation (spaces and tabs) of the line holding
/// `offset`: the first non-blank character, or the end of the content on a
/// blank line.
pub fn indent_end(text: &TextSnapshot, offset: usize) -> usize {
    let Ok(range) = text.line_range(line_of(text, offset)) else {
        return text.len();
    };
    let mut end = range.start;
    let Ok(chars) = text.chars_at(range.start) else {
        return end;
    };
    for c in chars {
        if end >= range.end || !matches!(c, ' ' | '\t') {
            break;
        }
        end += 1;
    }
    end.min(range.end)
}

/// `offset` clipped to the text, out of a multi-byte character (by `bias`)
/// and out of a CRLF (always before the CR): a position where a caret may
/// rest.
pub fn clip_caret(text: &TextSnapshot, offset: usize, bias: Bias) -> usize {
    let offset = text.clip_offset(offset, bias);
    if text.is_inside_crlf(offset) {
        offset - 1
    } else {
        offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(text: &str) -> TextSnapshot {
        TextSnapshot::new(text)
    }

    #[test]
    fn grapheme_steps() {
        let text = "a\u{301}b🇩🇪c\r\nd👨‍👩‍👧e";
        let t = snap(text);
        let expected: Vec<usize> = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let mut forward = vec![0];
        let mut at = 0;
        while at < text.len() {
            at = next_grapheme_boundary(&t, at);
            forward.push(at);
        }
        assert_eq!(forward, expected);
        let mut backward = vec![text.len()];
        let mut at = text.len();
        while at > 0 {
            at = prev_grapheme_boundary(&t, at);
            backward.push(at);
        }
        backward.reverse();
        assert_eq!(backward, expected);
        for offset in 0..=text.len() {
            assert_eq!(
                is_grapheme_boundary(&t, offset),
                expected.contains(&offset),
                "{offset}"
            );
        }
    }

    #[test]
    fn long_cluster_crosses_windows() {
        // A base letter with 500 combining marks is one cluster of 1001
        // bytes, far longer than the window.
        let text = format!("x{}y", "\u{301}".repeat(500));
        let t = snap(&text);
        assert_eq!(next_grapheme_boundary(&t, 0), 1001);
        assert_eq!(prev_grapheme_boundary(&t, 1001), 0);
        assert!(!is_grapheme_boundary(&t, 501));
        assert_eq!(snap_to_grapheme(&t, 501), 0);
        // An offset inside a character is clipped left first.
        assert_eq!(next_grapheme_boundary(&t, 2), 1001);
    }

    #[test]
    fn regional_indicators_need_context() {
        // Flags pair up from the start: the boundary parity depends on the
        // context before the window.
        let text = "🇩🇪".repeat(40);
        let t = snap(&text);
        let end = text.len();
        assert_eq!(prev_grapheme_boundary(&t, end), end - 8);
        assert_eq!(next_grapheme_boundary(&t, end - 8), end);
        assert!(!is_grapheme_boundary(&t, end - 4));
    }

    #[test]
    fn out_of_range_offsets_are_clipped() {
        let t = snap("aé");
        assert_eq!(next_grapheme_boundary(&t, 99), 3);
        assert_eq!(prev_grapheme_boundary(&t, 99), 1);
        assert_eq!(prev_grapheme_boundary(&t, 2), 0);
        assert!(!is_grapheme_boundary(&t, 2));
        assert!(!is_grapheme_boundary(&t, 4));
        let empty = snap("");
        assert_eq!(next_grapheme_boundary(&empty, 5), 0);
        assert_eq!(prev_grapheme_boundary(&empty, 5), 0);
        assert_eq!(next_word_end(&empty, 0), 0);
        assert_eq!(prev_word_start(&empty, 0), 0);
    }

    #[test]
    fn word_motion() {
        let text = "foo bar_baz,  can't... 3.14\r\n\r\n  next";
        let t = snap(text);
        let mut stops = vec![];
        let mut at = 0;
        while at < text.len() {
            at = next_word_end(&t, at);
            stops.push(&text[..at]);
        }
        assert_eq!(
            stops,
            [
                "foo",
                "foo bar_baz",
                "foo bar_baz,",
                "foo bar_baz,  can't",
                "foo bar_baz,  can't...",
                "foo bar_baz,  can't... 3.14",
                text,
            ]
        );
        let mut starts = vec![];
        let mut at = text.len();
        while at > 0 {
            at = prev_word_start(&t, at);
            starts.push(&text[at..]);
        }
        assert_eq!(
            starts,
            [
                "next",
                "3.14\r\n\r\n  next",
                "... 3.14\r\n\r\n  next",
                "can't... 3.14\r\n\r\n  next",
                ",  can't... 3.14\r\n\r\n  next",
                "bar_baz,  can't... 3.14\r\n\r\n  next",
                text,
            ]
        );
    }

    #[test]
    fn long_words_grow_the_window() {
        let word = "w".repeat(5000);
        let text = format!("  {word}  ");
        let t = snap(&text);
        assert_eq!(next_word_end(&t, 0), 5002);
        assert_eq!(prev_word_start(&t, text.len()), 2);
        assert_eq!(next_word_end(&t, 5002), text.len());
        assert_eq!(prev_word_start(&t, 2), 0);
    }

    #[test]
    fn lines_and_indentation() {
        let t = snap("  \tab\r\n    \rx");
        assert_eq!(line_start(&t, 4), 0);
        assert_eq!(line_end(&t, 0), 5);
        assert_eq!(indent_end(&t, 0), 3);
        assert_eq!(indent_end(&t, 8), 11);
        assert_eq!(line_end(&t, 8), 11);
        assert_eq!(line_start(&t, 12), 12);
        assert_eq!(indent_end(&t, 12), 12);
        assert_eq!(line_end(&t, 99), 13);
    }

    #[test]
    fn carets_stay_out_of_crlf() {
        let t = snap("a\r\nb");
        assert_eq!(clip_caret(&t, 2, Bias::Left), 1);
        assert_eq!(clip_caret(&t, 2, Bias::Right), 1);
        assert_eq!(clip_caret(&t, 3, Bias::Left), 3);
        assert_eq!(clip_caret(&t, 9, Bias::Left), 4);
        let t = snap("é");
        assert_eq!(clip_caret(&t, 1, Bias::Left), 0);
        assert_eq!(clip_caret(&t, 1, Bias::Right), 2);
    }
}

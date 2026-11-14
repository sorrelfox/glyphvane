// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors
//
// Ported from GPUI, part of Zed (https://github.com/zed-industries/zed),
// crates/gpui/src/text_system/line_wrapper.rs at commit
// d2a4fb6ded7c470be25d1541281da3882495b65b.
// Copyright (C) Zed Industries, Inc. and the Zed contributors.
// The upstream file is licensed under the Apache License, Version 2.0
// (http://www.apache.org/licenses/LICENSE-2.0); this modified version is
// distributed as part of Glyphvane under GPL-3.0-or-later.
//
// Modified by the Glyphvane contributors (2026-11-14): widths come from an
// injected `TextMeasure` instead of GPUI's text system, pixels are `f32`,
// UAX #14 break opportunities come from `unicode-linebreak` instead of ICU4X,
// an absolute `IndentAdjustment::Columns` is added, and the truncation and
// shaped-line helpers are left out. The full list is in
// third_party/zed/PROVENANCE.md.

//! Finds soft-wrap boundaries in one line of text.
//!
//! [`LineWrapper::wrap_line`] sums per-character advance widths (cached,
//! from a [`TextMeasure`]) and breaks a line that gets wider than the wrap
//! width:
//!
//! - at the last **UAX #14 line-break opportunity** that is also a grapheme
//!   boundary, after the first non-blank character: after spaces, after a
//!   hyphen or slash, and between CJK ideographs, but never before closing
//!   punctuation such as `！` or inside `a\u{a0}b`;
//! - where there is none (a long word), before the grapheme that overflows.
//!   A grapheme wider than the line stays whole.
//!
//! Spaces count toward the width and stay at the end of the row they
//! follow. Each boundary carries the indent of the next row, in columns
//! (multiples of the width of a space), chosen by an [`IndentAdjustment`].

use std::collections::HashMap;
use std::fmt;
use std::iter;

use unicode_segmentation::UnicodeSegmentation;

use super::text_measure::TextMeasure;

/// Controls how soft-wrapped continuation lines are indented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum IndentAdjustment {
    /// No indent - continuation lines start at column 0.
    NoIndent,
    /// Match the original line's leading whitespace (none where the text
    /// carried over to the continuation row would not fit after it).
    #[default]
    SameIndent,
    /// Add N extra columns of indent (in space-character widths).
    ExtraColumns(u32),
    /// Indent continuation lines to exactly N columns, whatever the line's
    /// leading whitespace (for a hanging indent computed from the syntax
    /// tree). The same overflow guard as [`Self::ExtraColumns`] applies.
    Columns(u32),
}

/// Wraps lines of text to a given width, measuring characters with a
/// [`TextMeasure`].
pub struct LineWrapper {
    measure: Box<dyn TextMeasure>,
    char_widths: CharWidthCache,
    text_buffer: String,
}

impl fmt::Debug for LineWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LineWrapper")
            .field("cached_widths", &self.char_widths.len())
            .finish_non_exhaustive()
    }
}

/// Cached character widths for one font and size.
#[derive(Debug)]
struct CharWidthCache {
    ascii: [Option<f32>; 128],
    other: HashMap<char, f32>,
}

impl Default for CharWidthCache {
    fn default() -> Self {
        Self {
            ascii: [None; 128],
            other: HashMap::default(),
        }
    }
}

impl CharWidthCache {
    fn len(&self) -> usize {
        self.ascii.iter().flatten().count() + self.other.len()
    }
}

impl LineWrapper {
    /// The maximum indent that can be applied to a line.
    pub const MAX_INDENT: u32 = 256;

    /// A wrapper that measures with `measure`. Widths are cached, so a
    /// wrapper must be replaced (not reused) when the font changes.
    pub fn new(measure: Box<dyn TextMeasure>) -> Self {
        Self {
            measure,
            char_widths: CharWidthCache::default(),
            text_buffer: String::new(),
        }
    }

    /// The measure, giving up the wrapper.
    pub fn into_measure(self) -> Box<dyn TextMeasure> {
        self.measure
    }

    /// Wrap a line of text to the given width with this wrapper's measure.
    ///
    /// Yields the boundaries in order: the byte index (in the concatenated
    /// fragments) where each continuation row starts, and that row's
    /// indent in columns. A `\n` takes no width.
    pub fn wrap_line<'a>(
        &'a mut self,
        fragments: &'a [LineFragment<'a>],
        wrap_width: f32,
        indent_adjustment: IndentAdjustment,
    ) -> impl Iterator<Item = Boundary> + 'a {
        let mut width = 0.0;
        let mut first_non_whitespace_ix = None;
        let mut base_indent = None;
        let mut last_candidate_ix = 0;
        let mut last_candidate_width = 0.0;
        let mut last_wrap_ix = 0;
        let mut index = 0;
        let Self {
            text_buffer: text,
            char_widths,
            measure,
        } = self;
        text.clear();
        for fragment in fragments {
            match fragment {
                LineFragment::Text {
                    text: fragment_text,
                } => text.push_str(fragment_text),
                // This placeholder keeps inline elements attached to adjacent words
                LineFragment::Element { len_utf8, .. } => {
                    text.extend(iter::repeat_n('a', *len_utf8));
                }
            }
        }
        let text: &'a str = text;
        let mut break_indices = break_indices(text).peekable();
        let mut grapheme_indices = text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .peekable();
        let mut width_for_char =
            move |character| width_for_char(char_widths, character, &mut **measure);
        let mut grapheme_index = 0;
        let mut width_before_grapheme = 0.0;
        let mut candidates = fragments
            .iter()
            .flat_map(move |fragment| fragment.wrap_boundary_candidates())
            .peekable();
        iter::from_fn(move || {
            for candidate in candidates.by_ref() {
                let ix = index;
                index += candidate.len_utf8();
                while grapheme_indices
                    .next_if(|position| *position < ix)
                    .is_some()
                {}
                let is_grapheme_boundary = grapheme_indices.next_if_eq(&ix).is_some();
                if is_grapheme_boundary {
                    grapheme_index = ix;
                    width_before_grapheme = width;
                }
                // Glyphvane: drop opportunities that no candidate starts at
                // (inside a multi-byte element placeholder), so they cannot
                // hold back the later ones.
                while break_indices.next_if(|position| *position < ix).is_some() {}
                if break_indices.next_if_eq(&ix).is_some()
                    && is_grapheme_boundary
                    && first_non_whitespace_ix.is_some()
                {
                    last_candidate_ix = ix;
                    last_candidate_width = width;
                }
                let item_width = match candidate {
                    WrapBoundaryCandidate::Char { character: c } => {
                        if c == '\n' {
                            continue;
                        }

                        if c != ' ' && first_non_whitespace_ix.is_none() {
                            first_non_whitespace_ix = Some(ix);
                        }

                        width_for_char(c)
                    }
                    WrapBoundaryCandidate::Element {
                        width: element_width,
                        ..
                    } => {
                        if first_non_whitespace_ix.is_none() {
                            first_non_whitespace_ix = Some(ix);
                        }

                        element_width
                    }
                };

                width += item_width;
                if width > wrap_width && grapheme_index > last_wrap_ix {
                    let wrap_at_candidate = last_candidate_ix > last_wrap_ix
                        && width - last_candidate_width <= wrap_width;

                    let carried_width = if wrap_at_candidate {
                        width - last_candidate_width
                    } else {
                        width - width_before_grapheme
                    };

                    // Compute base indentation from the first non-whitespace character on the line
                    // and retain it for all subsequent wrap rows. If the line begins with leading
                    // whitespace that wraps before any non-whitespace character (or is all whitespace),
                    // base_indent remains None so continuation rows within the leading whitespace
                    // do not receive ExtraColumns indentation and degrade into cascading short rows.
                    if base_indent.is_none()
                        && let Some(first_non_whitespace_ix) = first_non_whitespace_ix
                    {
                        base_indent = Some(
                            Self::MAX_INDENT
                                .min(first_non_whitespace_ix.saturating_sub(last_wrap_ix) as u32),
                        );
                    }

                    // Reserve headroom for any carried suffix from an earlier word boundary
                    // (and at least 2 columns for a full-width character) so the continuation
                    // line does not immediately exceed wrap width.
                    let mut guarded = |candidate: u32| {
                        let candidate_indent_width = width_for_char(' ') * candidate as f32;
                        let min_headroom = carried_width.max(width_for_char(' ') * 2.0);
                        if candidate_indent_width + min_headroom > wrap_width {
                            0
                        } else {
                            Self::MAX_INDENT.min(candidate)
                        }
                    };
                    let next_indent = match indent_adjustment {
                        IndentAdjustment::NoIndent => 0,
                        // Glyphvane: no indent when the carried text would
                        // not fit after it (upstream keeps it, and the row
                        // overflows with more than one grapheme).
                        IndentAdjustment::SameIndent => match base_indent {
                            Some(base_indent)
                                if width_for_char(' ') * base_indent as f32 + carried_width
                                    <= wrap_width =>
                            {
                                base_indent
                            }
                            _ => 0,
                        },
                        IndentAdjustment::ExtraColumns(extra) => match base_indent {
                            Some(base_indent) => guarded(base_indent.saturating_add(extra)),
                            None => 0,
                        },
                        // Glyphvane: an absolute indent, guarded the same way.
                        IndentAdjustment::Columns(columns) => match base_indent {
                            Some(_) => guarded(columns),
                            None => 0,
                        },
                    };

                    if wrap_at_candidate {
                        last_wrap_ix = last_candidate_ix;
                        width -= last_candidate_width;
                        width_before_grapheme -= last_candidate_width;
                    } else {
                        // An oversized grapheme stays intact, even if it exceeds the line width.
                        last_wrap_ix = grapheme_index;
                        width -= width_before_grapheme;
                        width_before_grapheme = 0.0;
                    }
                    last_candidate_ix = 0;

                    let indent_width = width_for_char(' ') * next_indent as f32;
                    width += indent_width;
                    width_before_grapheme += indent_width;

                    return Some(Boundary::new(last_wrap_ix, next_indent));
                }
            }

            None
        })
    }

    /// The width of `c`, cached.
    #[inline]
    pub fn width_for_char(&mut self, c: char) -> f32 {
        width_for_char(&mut self.char_widths, c, &mut *self.measure)
    }
}

/// The byte offsets in `text` at which a line break is allowed (UAX #14,
/// with the default resolution of the context-dependent classes; complex
/// scripts break like letters, as in ICU4X's segmenter for non-complex
/// scripts).
pub fn break_indices(text: &str) -> impl Iterator<Item = usize> + '_ {
    unicode_linebreak::linebreaks(text).map(|(index, _)| index)
}

#[inline]
fn width_for_char(char_widths: &mut CharWidthCache, c: char, measure: &mut dyn TextMeasure) -> f32 {
    if (c as u32) < 128 {
        if let Some(cached_width) = char_widths.ascii[c as usize] {
            cached_width
        } else {
            let width = measure.char_width(c);
            char_widths.ascii[c as usize] = Some(width);
            width
        }
    } else if let Some(cached_width) = char_widths.other.get(&c) {
        *cached_width
    } else {
        let width = measure.char_width(c);
        char_widths.other.insert(c, width);
        width
    }
}

/// A fragment of a line that can be wrapped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineFragment<'a> {
    /// A text fragment consisting of characters.
    Text {
        /// The text content of the fragment.
        text: &'a str,
    },
    /// A non-text element with a fixed width.
    Element {
        /// The width of the element in pixels.
        width: f32,
        /// The UTF-8 encoded length of the element.
        len_utf8: usize,
    },
}

impl<'a> LineFragment<'a> {
    /// Creates a new text fragment from the given text.
    pub fn text(text: &'a str) -> Self {
        LineFragment::Text { text }
    }

    /// Creates a new non-text element with the given width and UTF-8 encoded length.
    pub fn element(width: f32, len_utf8: usize) -> Self {
        LineFragment::Element { width, len_utf8 }
    }

    fn wrap_boundary_candidates(&self) -> impl Iterator<Item = WrapBoundaryCandidate> {
        let text = match self {
            LineFragment::Text { text } => text,
            LineFragment::Element { .. } => "\0",
        };
        text.chars().map(move |character| {
            if let LineFragment::Element { width, len_utf8 } = self {
                WrapBoundaryCandidate::Element {
                    width: *width,
                    len_utf8: *len_utf8,
                }
            } else {
                WrapBoundaryCandidate::Char { character }
            }
        })
    }
}

enum WrapBoundaryCandidate {
    Char { character: char },
    Element { width: f32, len_utf8: usize },
}

impl WrapBoundaryCandidate {
    fn len_utf8(&self) -> usize {
        match self {
            WrapBoundaryCandidate::Char { character } => character.len_utf8(),
            WrapBoundaryCandidate::Element { len_utf8: len, .. } => *len,
        }
    }
}

/// A boundary between two lines of text.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Boundary {
    /// The index of the last character in a line
    pub ix: usize,
    /// The indent of the next line.
    pub next_indent: u32,
}

impl Boundary {
    /// A boundary at byte `ix` whose next row is indented `next_indent`
    /// columns.
    pub fn new(ix: usize, next_indent: u32) -> Self {
        Self { ix, next_indent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_map::test_support::ColumnMeasure;
    use crate::display_map::text_measure::MonospaceMeasure;

    // Upstream measures `.ZedMono` at 16 px through GPUI's text system: an
    // advance of 9.6 px for the ASCII used in these tests.
    fn build_wrapper() -> LineWrapper {
        LineWrapper::new(Box::new(MonospaceMeasure::new(9.6)))
    }

    // Upstream fills the width cache with 1 px for ASCII and U+0301; here
    // every character is one pixel wide.
    fn unit_wrapper() -> LineWrapper {
        LineWrapper::new(Box::new(MonospaceMeasure::new(1.0)))
    }

    fn assert_emergency_wrap_preserves_graphemes(wrapper: &mut LineWrapper) {
        let mut failures = Vec::new();
        for cluster in ["e\u{301}", "か\u{3099}", "👍🏽", "🇯🇵", "👩\u{200d}💻"] {
            let cluster_width: f32 = cluster
                .chars()
                .map(|character| wrapper.width_for_char(character))
                .sum();
            let first_width = cluster
                .chars()
                .next()
                .map(|character| wrapper.width_for_char(character))
                .expect("nonempty fixture");
            for text in [cluster.to_owned(), format!("x{cluster}y")] {
                let grapheme_boundaries: Vec<_> = text
                    .grapheme_indices(true)
                    .map(|(index, _)| index)
                    .chain(std::iter::once(text.len()))
                    .collect();
                assert!(
                    break_indices(&text).all(|index| grapheme_boundaries.contains(&index)),
                    "UAX #14 boundary inside {text:?}"
                );
                let fragments = [LineFragment::text(&text)];
                let full_width: f32 = text
                    .chars()
                    .map(|character| wrapper.width_for_char(character))
                    .sum();
                assert!(
                    wrapper
                        .wrap_line(&fragments, full_width + 1., IndentAdjustment::NoIndent)
                        .next()
                        .is_none()
                );
                for wrap_width in [
                    first_width - 0.1,
                    first_width,
                    first_width + 0.1,
                    cluster_width - 0.1,
                    cluster_width,
                    cluster_width + 0.1,
                ] {
                    let boundaries: Vec<_> = wrapper
                        .wrap_line(&fragments, wrap_width, IndentAdjustment::NoIndent)
                        .map(|boundary| boundary.ix)
                        .collect();
                    let split_fragments: Vec<_> = text
                        .split_inclusive(|_: char| true)
                        .map(LineFragment::text)
                        .collect();
                    let split_boundaries: Vec<_> = wrapper
                        .wrap_line(&split_fragments, wrap_width, IndentAdjustment::NoIndent)
                        .map(|boundary| boundary.ix)
                        .collect();
                    assert_eq!(
                        boundaries, split_boundaries,
                        "fragmentation changed {text:?}"
                    );
                    let invalid: Vec<_> = boundaries
                        .iter()
                        .copied()
                        .filter(|index| !grapheme_boundaries.contains(index))
                        .collect();
                    if !invalid.is_empty() {
                        failures.push(format!(
                            "text={text:?}, width={wrap_width:?}, cluster_width={cluster_width:?}, breaks={boundaries:?}, inside_grapheme={invalid:?}"
                        ));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn test_emergency_wrap_preserves_graphemes() {
        assert_emergency_wrap_preserves_graphemes(&mut build_wrapper());
        // Glyphvane: also with wide and zero-width characters.
        assert_emergency_wrap_preserves_graphemes(&mut LineWrapper::new(Box::new(ColumnMeasure)));
    }

    #[test]
    fn test_emergency_wrap_exact_boundaries() {
        let mut wrapper = unit_wrapper();

        for (text, width, adjustment, expected) in [
            (
                "xe\u{301}y",
                1.,
                IndentAdjustment::NoIndent,
                vec![Boundary::new(1, 0), Boundary::new(4, 0)],
            ),
            (
                "  xe\u{301}yze\u{301}q",
                5.,
                IndentAdjustment::SameIndent,
                vec![Boundary::new(6, 2), Boundary::new(8, 2)],
            ),
            (
                "  xe\u{301}yze\u{301}q",
                5.,
                IndentAdjustment::ExtraColumns(1),
                vec![
                    Boundary::new(6, 3),
                    Boundary::new(8, 3),
                    Boundary::new(11, 3),
                ],
            ),
        ] {
            let fragments = [LineFragment::text(text)];
            assert_eq!(
                wrapper
                    .wrap_line(&fragments, width, adjustment)
                    .collect::<Vec<_>>(),
                expected,
                "{text:?}, {adjustment:?}",
            );
        }
    }

    #[test]
    fn test_emergency_wrap_elements_and_graphemes() {
        let mut wrapper = unit_wrapper();

        for element_length in [1, 3, 32] {
            let fragments = [
                LineFragment::text("xe\u{301}"),
                LineFragment::element(4., element_length),
                LineFragment::text("e\u{301}yz"),
            ];
            assert_eq!(
                wrapper
                    .wrap_line(&fragments, 2., IndentAdjustment::NoIndent)
                    .collect::<Vec<_>>(),
                vec![
                    Boundary::new(1, 0),
                    Boundary::new(4, 0),
                    Boundary::new(4 + element_length, 0),
                    Boundary::new(7 + element_length, 0),
                ],
                "element_length={element_length}",
            );
        }
    }

    #[test]
    fn test_wrap_line() {
        let mut wrapper = build_wrapper();

        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("aa bbb cccc ddddd eeee")],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(12, 0),
                Boundary::new(18, 0)
            ],
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("aaa aaaaaaaaaaaaaaaaaa")],
                    72.0,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(4, 0),
                Boundary::new(11, 0),
                Boundary::new(18, 0)
            ],
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("     aaaaaaa")],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 5),
                Boundary::new(9, 5),
                Boundary::new(11, 5),
            ]
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("                            ")],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(14, 0),
                Boundary::new(21, 0)
            ]
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("          aaaaaaaaaaaaaa")],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(14, 3),
                Boundary::new(18, 3),
                Boundary::new(22, 3),
            ]
        );

        // Test wrapping multiple text fragments
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::text("aa bbb "),
                        LineFragment::text("cccc ddddd eeee")
                    ],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(12, 0),
                Boundary::new(18, 0)
            ],
        );

        // Test wrapping with a mix of text and element fragments
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::text("aa "),
                        LineFragment::element(20., 1),
                        LineFragment::text(" bbb "),
                        LineFragment::element(30., 1),
                        LineFragment::text(" cccc")
                    ],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(5, 0),
                Boundary::new(9, 0),
                Boundary::new(11, 0)
            ],
        );

        // Test with element at the beginning and text afterward
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::element(50., 1),
                        LineFragment::text(" aaaa bbbb cccc dddd")
                    ],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(2, 0),
                Boundary::new(7, 0),
                Boundary::new(12, 0),
                Boundary::new(17, 0)
            ],
        );

        // Test with a large element that forces wrapping by itself
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::text("short text "),
                        LineFragment::element(100., 1),
                        LineFragment::text(" more text")
                    ],
                    72.,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(6, 0),
                Boundary::new(11, 0),
                Boundary::new(12, 0),
                Boundary::new(18, 0)
            ],
        );

        // Test with non-breaking glue characters
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("a\u{202F}b\u{00A0}c\u{2011}d e")],
                    72.0,
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(12, 0),], // special chars above take up 3, 2 and 3 bytes, so boundary ends up at 12
        );
    }

    #[test]
    fn test_wrap_line_break_opportunities() {
        // Glyphvane: run with a monospace and a variable-width measure.
        for mut wrapper in [build_wrapper(), LineWrapper::new(Box::new(ColumnMeasure))] {
            for (text, fitting_prefix, expected_index) in [
                ("aaa required!", "aaa required", 4),
                ("aaa foo/bar", "aaa foo/", 8),
                ("aaa (hello)", "aaa (hello", 4),
                ("aaa αβγδε", "aaa αβγδ", 4),
                ("aaa a\u{a0}b", "aaa a\u{a0}", 4),
                ("aaa 你好！", "aaa 你好", 7),
            ] {
                let wrap_width = fitting_prefix
                    .chars()
                    .map(|character| wrapper.width_for_char(character))
                    .sum();
                assert_eq!(
                    wrapper
                        .wrap_line(
                            &[LineFragment::text(text)],
                            wrap_width,
                            IndentAdjustment::default()
                        )
                        .collect::<Vec<_>>(),
                    [Boundary::new(expected_index, 0)],
                    "{text:?}",
                );
            }
        }
    }

    #[test]
    fn test_extra_columns_overflow_guard() {
        // Upstream measures `.ZedMono`, where the crab is two columns wide.
        let mut wrapper = LineWrapper::new(Box::new(ColumnMeasure));
        let space_width = wrapper.width_for_char(' ');

        // 6 spaces indent, wrap width 10 columns.
        let text = "      ab cd ef gh";
        let wrap_width = space_width * 10.0;

        // When base_indent + extra overflows wrap width (6 + 8 + 2 > 10),
        // indent must fall back to 0 instead of degrading to one character per row.
        //
        // Expected wrapped lines (10 columns):
        //   |      ab  |  (row 0: 6 spaces + "ab ", len 9)
        //   |cd ef gh  |  (row 1: 0 indent + "cd ef gh", len 8)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(9, 0)]
        );

        // When base_indent + extra fits within wrap width (6 + 2 + 2 <= 10),
        // the extra indent is applied and not clamped.
        // Spaces count toward the width, so the separators occupy their own rows
        // rather than overflowing the preceding row or splitting the following word.
        //
        // Expected wrapped lines (10 columns):
        //   |      ab  |  (row 0: 6 spaces + "ab ", len 9)
        //   |        cd|  (row 1: 8 spaces + "cd", len 10)
        //   |          |  (row 2: 8 spaces + " ", len 9)
        //   |        ef|  (row 3: 8 spaces + "ef", len 10)
        //   |          |  (row 4: 8 spaces + " ", len 9)
        //   |        gh|  (row 5: 8 spaces + "gh", len 10)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(9, 8),
                Boundary::new(11, 8),
                Boundary::new(12, 8),
                Boundary::new(14, 8),
                Boundary::new(15, 8),
            ]
        );

        // When text contains full-width (two-column) glyphs, reserving headroom for a
        // two-column character (candidate + 2 > wrap_width) ensures that candidate
        // indents leaving only 1 column cannot accept the indent and overflow.
        //
        // 1 space indent, wrap width 10 columns.
        // " 🦀🦀🦀🦀🦀🦀🦀🦀" with extra=8 (extra_two with tab_size=4):
        // candidate = 1 + 8 = 9.
        // With +2 headroom (9 + 2 > 10), indent falls back to 0.
        //
        // Expected wrapped lines (10 columns):
        //   | 🦀🦀🦀🦀 |  (row 0: 1 space + 4 two-column glyphs, 9 cols)
        //   |🦀🦀🦀🦀  |  (row 1: 0 indent + 4 two-column glyphs, 8 cols)
        let full_width_text = " 🦀🦀🦀🦀🦀🦀🦀🦀";
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(full_width_text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(17, 0)]
        );

        // When the carried suffix fits within wrap width alongside extra indent
        // (1 indent + 9 carried suffix = 10 <= 10), the extra indent is applied.
        //
        // Expected wrapped lines (10 columns):
        //   |a         |  (row 0: "a ", len 2)
        //   | abcdefghi|  (row 1: 1 indent + "abcdefghi", len 10)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("a abcdefghi")],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(1),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(2, 1)]
        );

        // When extra indent exceeds wrap width by even 1 column
        // (2 indent + 9 carried suffix = 11 > 10), indent falls back to 0.
        //
        // Expected wrapped lines (10 columns):
        //   |a         |  (row 0: "a ", len 2)
        //   |abcdefghi |  (row 1: 0 indent + "abcdefghi", len 9)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("a abcdefghi")],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(2, 0)]
        );

        // When text wraps at an earlier word boundary, the carried suffix
        // must be accounted for so indent + carried_suffix <= wrap_width.
        //
        // "a abcdefghij" with wrap_width 10 columns, ExtraColumns(8):
        // candidate = 0 + 8 = 8.
        // carried suffix = "abcdefghi" (9 columns).
        // 8 indent + 9 carried suffix = 17 > 10, so indent falls back to 0.
        //
        // Expected wrapped lines (10 columns):
        //   |a         |  (row 0: "a ", len 2)
        //   |abcdefghij|  (row 1: 0 indent + "abcdefghij", len 10)
        let carried_text = "a abcdefghij";
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(carried_text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(2, 0)]
        );

        // When a wider inline element is encountered after the first wrap,
        // the overflow guard must evaluate whether the element fits with the extra indent.
        // If text continues after the element row and wraps again, subsequent continuation
        // lines resume the extra indent if their carried content fits.
        //
        // "abcdefghijk " followed by an 8-column element and "z 12":
        // Row 0: "abcdefghij" (len 10)
        // Row 1: "k " (2 cols) with 8 indent (len 10)
        // Row 2: element (8 cols) cannot fit with 8 indent (8 + 8 = 16 > 10),
        //        so indent falls back to 0. Element (8 cols) + "z " (2 cols) = len 10.
        // Row 3: "12" (2 cols) fits with 8 indent (8 + 2 = 10 <= 10).
        //
        // Expected wrapped lines (10 columns):
        //   |abcdefghij|  (row 0: 10 cols)
        //   |        k |  (row 1: 8 indent + "k ", len 10)
        //   |[ELEMENT]z|  (row 2: 0 indent + [ELEMENT (8)] + "z ", len 10)
        //   |        12|  (row 3: 8 indent + "12", len 10)
        let element_fragments = [
            LineFragment::text("abcdefghijk "),
            LineFragment::element(space_width * 8.0, 1),
            LineFragment::text("z 12"),
        ];
        assert_eq!(
            wrapper
                .wrap_line(
                    &element_fragments,
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(10, 8),
                Boundary::new(12, 0),
                Boundary::new(15, 8),
            ]
        );

        // A line of only whitespace wrapping before any non-whitespace character
        // must not have extra columns added to subsequent rows.
        let spaces = "                    "; // 20 spaces
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(spaces)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(10, 0)]
        );

        // When leading whitespace wraps before the first non-whitespace character,
        // base_indent should reflect the leading whitespace on the row where non-whitespace begins.
        // 14 spaces followed by "ab cd ef gh", wrap width 10:
        // Row 0: 10 spaces (len 10) -> wraps at ix 10 with indent 0
        // Row 1: 4 spaces + "ab cd" (len 9) -> wraps at ix 20
        //        base_indent is 14 - 10 = 4.
        //        With ExtraColumns(2), candidate = 4 + 2 = 6. 6 + 2 (headroom) = 8 <= 10.
        //        Row 2 and subsequent rows get indent 6.
        let multiline_indent_text = "              ab cd ef gh";
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(multiline_indent_text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(10, 0),
                Boundary::new(20, 6),
                Boundary::new(23, 6),
            ]
        );

        // When a word boundary precedes an oversized carried suffix that itself
        // exceeds wrap_width (e.g. text followed by an inline element), wrapping
        // at the earlier candidate would force the continuation row to immediately
        // overflow even with 0 indent. The wrapper must reject the candidate and
        // break before the overflowing item instead.
        //
        // "aaaaaaaaaaaaaaaaaaa a" (21 chars: 19 'a's, space, 'a') followed by 10-column element:
        // Row 0: "aaaaaaaaaa" (10 chars, len 10) -> wraps at ix 10
        // Row 1: "aaaaaaaa" (8 chars) with 2 indent (len 10) -> wraps at ix 18
        // Row 2: "a a" (3 chars) with 2 indent (len 5)
        //        candidate boundary at ix 20 (space)
        //        Then element (10 cols): width becomes 5 + 10 = 15 > wrap_width (10).
        //        Candidate would carry 'a' (1 col) + element (10 cols) = 11 cols > 10.
        //        Because carried suffix (11) > wrap_width (10), candidate is rejected.
        //        Wrapper breaks at ix 21 (before element) with indent 0.
        // Row 3: element (10 cols, len 10)
        let oversized_suffix_fragments = [
            LineFragment::text("aaaaaaaaaaaaaaaaaaa a"),
            LineFragment::element(space_width * 10.0, 1),
        ];
        assert_eq!(
            wrapper
                .wrap_line(
                    &oversized_suffix_fragments,
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(10, 2),
                Boundary::new(18, 2),
                Boundary::new(21, 0),
            ]
        );

        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("  aaaaaaaa")],
                    space_width * 4.0,
                    IndentAdjustment::ExtraColumns(u32::MAX),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(4, 0), Boundary::new(8, 0)]
        );
    }

    // Glyphvane: `SameIndent` drops the indent where the carried word
    // would not fit after it, so no row overflows with more than one
    // grapheme.
    #[test]
    fn same_indent_overflow_guard() {
        let mut wrapper = LineWrapper::new(Box::new(ColumnMeasure));
        // Upstream gives `| a-|`, `| aa|`, `|  |`, `| a、|` (4 columns at
        // 3.6) and `| a|`.
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(" a-aa a、a")],
                    3.6,
                    IndentAdjustment::SameIndent,
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(3, 1),
                Boundary::new(5, 1),
                Boundary::new(6, 0),
                Boundary::new(10, 1),
            ]
        );
        // An indent that leaves room for the carried text is kept.
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("  ab cd")],
                    5.,
                    IndentAdjustment::SameIndent,
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(5, 2)]
        );
    }

    // Glyphvane: the absolute indent of `IndentAdjustment::Columns`.
    #[test]
    fn absolute_indent_columns() {
        let mut wrapper = unit_wrapper();
        // A list item: continuation rows hang under the text after `- `.
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("- aaa bbb ccc")],
                    6.,
                    IndentAdjustment::Columns(2),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(6, 2), Boundary::new(10, 2)]
        );
        // Ignores the leading whitespace (SameIndent would give 4).
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("    aaa bbb")],
                    8.,
                    IndentAdjustment::Columns(1),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(8, 1)]
        );
        // Too wide for the row (3 + 2 columns of headroom > 4): no indent.
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("aaaaaaaa")],
                    4.,
                    IndentAdjustment::Columns(3),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(4, 0)]
        );
        // Wrapping inside the leading whitespace gets no indent.
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("        ")],
                    4.,
                    IndentAdjustment::Columns(1),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(4, 0)]
        );
    }

    // Glyphvane: CJK text breaks between ideographs, but not before closing
    // punctuation or after opening punctuation.
    #[test]
    fn cjk_breaks_between_ideographs() {
        let mut wrapper = LineWrapper::new(Box::new(ColumnMeasure));
        // Each ideograph is two columns: three fit in seven columns.
        let boundaries: Vec<_> = wrapper
            .wrap_line(
                &[LineFragment::text("日本語の文章です")],
                7.,
                IndentAdjustment::NoIndent,
            )
            .collect();
        assert_eq!(
            boundaries,
            [Boundary::new(9, 0), Boundary::new(18, 0)],
            "after 日本語 and の文章"
        );
        // `「` stays with the following character and `」` with the previous:
        // rows `一二`, `「三`, `四」五`.
        let boundaries: Vec<_> = wrapper
            .wrap_line(
                &[LineFragment::text("一二「三四」五")],
                6.,
                IndentAdjustment::NoIndent,
            )
            .collect();
        assert_eq!(boundaries, [Boundary::new(6, 0), Boundary::new(12, 0)]);
    }
}

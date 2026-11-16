// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Greedy line breaking over the UAX #14 opportunities, with hanging white
//! space and the `overflow-wrap: anywhere` fallback.

use std::ops::Range;

use crate::paragraph::is_break_char;
use crate::shaped::{EPS, Shaped};

/// One line's logical range (trailing white space and the hard break
/// included) and whether it ends with a forced break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineBreak {
    pub(crate) range: Range<usize>,
    pub(crate) hard: bool,
}

/// Breaks the paragraph into lines no wider than `max` where possible.
///
/// - Lines end only at UAX #14 opportunities (style runs do not matter;
///   boxes are U+FFFC, so the rules around objects apply).
/// - White space at a line end hangs: it does not count when fitting.
/// - A piece wider than the line moves to a line of its own, then breaks at
///   caret stops (grapheme clusters that start a glyph cluster), never
///   inside a box: CSS `overflow-wrap: anywhere`. At least one cluster goes
///   on every line, so there is no endless loop at tiny widths.
pub(crate) fn break_lines<G>(shaped: &Shaped<G>, max: f32) -> Vec<LineBreak> {
    break_lines_in(shaped, 0..shaped.paragraph().text().len(), max)
}

/// [`break_lines`] for the logical range `range` only (a column), which
/// ends as if a break were allowed at its end.
pub(crate) fn break_lines_in<G>(
    shaped: &Shaped<G>,
    range: Range<usize>,
    max: f32,
) -> Vec<LineBreak> {
    let text = shaped.paragraph().text();
    let len = range.end.min(text.len());
    let begin = range.start.min(len);
    let mut lines = Vec::new();
    let push = |lines: &mut Vec<LineBreak>, range: Range<usize>| {
        let hard = text[..range.end].ends_with(is_break_char);
        lines.push(LineBreak { range, hard });
    };
    let inside = shaped
        .paragraph()
        .breaks()
        .iter()
        .map(|b| (b.offset as usize, b.mandatory))
        .filter(|&(pos, _)| begin < pos && pos < len);
    // The range's end is an opportunity (the paragraph's end is a
    // mandatory one already).
    let end = (len > begin).then_some((len, len == text.len()));
    // Line start, end of the last segment placed, width placed so far.
    let (mut ls, mut cur, mut w) = (begin, begin, 0.0f32);
    for (pos, mandatory) in inside.chain(end) {
        let trail = shaped.trim_hanging(cur, pos);
        let (seg, seg_full) = shaped.width_split(cur, trail, pos);
        if cur > ls && w + seg > max + EPS {
            push(&mut lines, ls..cur);
            ls = cur;
            w = 0.0;
        }
        if cur == ls && seg > max + EPS {
            // The rest of the piece stays on the line, with its trailing
            // white space, once it fits.
            let mut a = cur;
            loop {
                let e = if shaped.width(a, trail) <= max + EPS {
                    trail
                } else {
                    shaped.fit(a, trail, max)
                };
                if e >= trail {
                    w = shaped.width(a, pos);
                    break;
                }
                push(&mut lines, ls..e);
                ls = e;
                a = e;
            }
        } else {
            w += seg_full;
        }
        cur = pos;
        if mandatory && pos > ls {
            push(&mut lines, ls..pos);
            ls = pos;
            w = 0.0;
        }
    }
    if lines.is_empty() || ls < len {
        push(&mut lines, ls..len);
    }
    lines
}

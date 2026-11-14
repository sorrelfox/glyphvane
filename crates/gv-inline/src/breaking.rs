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
    let text = shaped.paragraph().text();
    let len = text.len();
    let mut lines = Vec::new();
    let push = |lines: &mut Vec<LineBreak>, range: Range<usize>| {
        let hard = text[..range.end].ends_with(is_break_char);
        lines.push(LineBreak { range, hard });
    };
    // Line start, end of the last segment placed, width placed so far.
    let (mut ls, mut cur, mut w) = (0usize, 0usize, 0.0f32);
    for b in shaped.paragraph().breaks() {
        let pos = b.offset as usize;
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
        if b.mandatory && pos > ls {
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

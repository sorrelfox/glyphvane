// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Toggle comment: `%%…%%` in Obsidian, `<!-- … -->` elsewhere.
//!
//! - A selection or caret inside a comment: the comment's delimiters are
//!   removed (with the space next to each, for HTML comments).
//! - A selection on one line: it is wrapped, and stays selected inside the
//!   delimiters.
//! - A caret, or a selection over several lines: the lines are wrapped,
//!   from the first line's content (after its `>` markers and list marker)
//!   to the end of the last line's text. On a blank line an empty comment
//!   is inserted with the caret inside.

use std::ops::Range;

use crate::selection::SelectionSpec;

use super::context::line_ctx;
use super::doc::Doc;
use super::flavor::Flavor;
use super::list::selected_lines;

/// The comment delimiters of a flavor and the padding written inside them.
fn delimiters(flavor: Flavor) -> (&'static str, &'static str, &'static str) {
    if flavor == Flavor::Obsidian {
        ("%%", "%%", "")
    } else {
        ("<!--", "-->", " ")
    }
}

/// The comment around `s..e`: its opening and closing delimiter ranges.
/// Obsidian comments pair `%%` runs from the start of the text; HTML
/// comments run from `<!--` to the next `-->`.
fn enclosing(
    text: &str,
    flavor: Flavor,
    s: usize,
    e: usize,
) -> Option<(Range<usize>, Range<usize>)> {
    let (open, close, _) = delimiters(flavor);
    if flavor == Flavor::Obsidian {
        let marks: Vec<usize> = text.match_indices(open).map(|(i, _)| i).collect();
        // Non-overlapping `%%` runs pair up in order.
        let mut pairs = Vec::new();
        let mut last_end = 0;
        let mut pending: Option<usize> = None;
        for i in marks {
            if i < last_end {
                continue;
            }
            last_end = i + open.len();
            match pending.take() {
                None => pending = Some(i),
                Some(o) => pairs.push((o, i)),
            }
        }
        return pairs
            .into_iter()
            .find(|&(o, c)| o <= s && e <= c + close.len() && (s < c || s == e))
            .map(|(o, c)| (o..o + open.len(), c..c + close.len()));
    }
    let o = text[..s.min(text.len())]
        .rfind(open)
        .or_else(|| text[s..].starts_with(open).then_some(s))?;
    let after = o + open.len();
    let c = after + text[after..].find(close)?;
    // The comment must not have ended before the selection.
    (c + close.len() >= e && c + close.len() > s.max(o)).then(|| (o..after, c..c + close.len()))
}

/// Toggles a comment at every selection.
pub(crate) fn toggle_comment(doc: &mut Doc) {
    let text = doc.slice(0..doc.len());
    let (open, close, pad) = delimiters(doc.flavor);
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut claimed: Vec<Range<usize>> = Vec::new();
    // Per selection: where to put it afterwards, as old offsets and how
    // far into the inserted text.
    let mut placements: Vec<Option<(usize, usize)>> = vec![None; doc.sels.len()];
    // Where closing delimiters go: a selection end there stays before it.
    let mut closers: Vec<usize> = Vec::new();
    let before = doc.sels.clone();
    for (i, spec) in doc.sels.clone().into_iter().enumerate() {
        let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
        if let Some((o, c)) = enclosing(&text, doc.flavor, s, e) {
            let span = o.start..c.end;
            if claimed
                .iter()
                .any(|r| r.start < span.end && span.start < r.end)
            {
                continue;
            }
            claimed.push(span);
            let o_end = if !pad.is_empty()
                && text[o.end..].starts_with(pad)
                && o.end + pad.len() <= c.start
            {
                o.end + pad.len()
            } else {
                o.end
            };
            let c_start = if !pad.is_empty()
                && c.start >= o_end + pad.len()
                && text[..c.start].ends_with(pad)
            {
                c.start - pad.len()
            } else {
                c.start
            };
            edits.push((o.start..o_end, String::new()));
            edits.push((c_start..c.end, String::new()));
            continue;
        }
        let lines = selected_lines(doc, spec);
        let (first, last) = (lines.start, lines.end - 1);
        if s < e && first == last {
            let span = s..e;
            if claimed
                .iter()
                .any(|r| r.start < span.end && span.start < r.end)
            {
                continue;
            }
            claimed.push(span);
            edits.push((s..s, format!("{open}{pad}")));
            edits.push((e..e, format!("{pad}{close}")));
            continue;
        }
        let start = line_ctx(doc, first).content_start;
        let end_line = doc.line(last);
        let end = end_line.start + end_line.text.trim_end_matches([' ', '\t']).len();
        let end = end.max(start);
        let span = start..end;
        if claimed
            .iter()
            .any(|r| r.start <= span.end && span.start <= r.end)
        {
            continue;
        }
        claimed.push(span.clone());
        if start == end {
            edits.push((start..start, format!("{open}{pad}{pad}{close}")));
            placements[i] = Some((start, open.len() + pad.len()));
        } else {
            edits.push((start..start, format!("{open}{pad}")));
            edits.push((end..end, format!("{pad}{close}")));
            closers.push(end);
        }
    }
    let Some(summary) = doc.apply(edits) else {
        return;
    };
    for (i, spec) in before.iter().enumerate() {
        let left = |p: usize| summary.transform_offset(p, gv_text::Bias::Left);
        let mut new = doc.sels[i];
        if closers.contains(&spec.tail) && spec.tail >= spec.head {
            new.tail = left(spec.tail);
        }
        if closers.contains(&spec.head) && spec.head >= spec.tail {
            new.head = left(spec.head);
        }
        doc.sels[i] = SelectionSpec::new(new.tail, new.head);
    }
    for (i, place) in placements.into_iter().enumerate() {
        if let Some((at, into)) = place {
            let caret = summary.transform_offset(at, gv_text::Bias::Left) + into;
            doc.sels[i] = SelectionSpec::caret(caret);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_html_comments() {
        let text = "a <!-- b --> c";
        assert_eq!(enclosing(text, Flavor::GfmFile, 8, 8), Some((2..6, 9..12)));
        assert_eq!(enclosing(text, Flavor::GfmFile, 13, 13), None);
        assert_eq!(enclosing(text, Flavor::GfmFile, 2, 12), Some((2..6, 9..12)));
        assert_eq!(enclosing("a b", Flavor::GfmFile, 1, 1), None);
    }

    #[test]
    fn finds_obsidian_comments() {
        let text = "a %%b%% c %%d%%";
        assert_eq!(enclosing(text, Flavor::Obsidian, 5, 5), Some((2..4, 5..7)));
        assert_eq!(enclosing(text, Flavor::Obsidian, 9, 9), None);
        assert_eq!(
            enclosing(text, Flavor::Obsidian, 12, 12),
            Some((10..12, 13..15))
        );
    }
}

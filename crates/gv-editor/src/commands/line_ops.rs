// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Line commands: move a line (or a list item, or a fenced block) up or
//! down, duplicate lines, join lines, sort lines, change case.
//!
//! Lines keep their line endings in place: when lines change places, the
//! ending after the n-th line of the region stays the n-th ending, so the
//! last line of the document never gains one and a CRLF document stays
//! CRLF.

use std::cmp::Ordering;
use std::ops::Range;

use gv_syntax::LeafKind;
use gv_text::LineEnding;
use unicode_segmentation::UnicodeSegmentation;

use crate::selection::SelectionSpec;

use super::context::{line_ctx, list_items};
use super::doc::{Doc, Line};
use super::lines::minimal;
use super::list::selected_lines;

/// The lines one selection moves: the lines it covers, or, for a caret or
/// selection on one line, the whole list item whose marker is on that line
/// (with its children) or the whole fenced block whose opening fence it is.
#[derive(Clone, Debug)]
struct Unit {
    /// First and last line (inclusive).
    first: usize,
    last: usize,
    /// The list item it is (the item's range), when it is one.
    item: Option<Range<usize>>,
}

/// The last line before `last` (inclusive, not before `first`) that is not
/// blank.
fn trim_blank_tail(doc: &Doc, first: usize, mut last: usize) -> usize {
    while last > first && doc.line(last).text.trim().is_empty() {
        last -= 1;
    }
    last
}

/// The unit of selection `spec`.
fn unit_of(doc: &Doc, spec: SelectionSpec) -> Unit {
    let lines = selected_lines(doc, spec);
    let (first, last) = (lines.start, lines.end - 1);
    if first != last {
        return Unit {
            first,
            last,
            item: None,
        };
    }
    let ctx = line_ctx(doc, first);
    // The innermost item whose marker is on this line.
    let item = ctx
        .chain
        .iter()
        .rev()
        .find(|level| level.item().is_some_and(|item| item.marker_line == first));
    if let Some(level) = item {
        let end = doc.line_of(level.range.end.saturating_sub(1).max(level.range.start));
        // An item that ends at a line start ends on the line before.
        let end = if end > first && doc.line(end).start == level.range.end {
            end - 1
        } else {
            end
        };
        return Unit {
            first,
            last: trim_blank_tail(doc, first, end),
            item: Some(level.range.clone()),
        };
    }
    if let Some(leaf) = &ctx.leaf
        && matches!(leaf.kind, LeafKind::FencedCode | LeafKind::MathBlock)
        && leaf.first_line == first
    {
        return Unit {
            first,
            last: leaf.last_line.max(first),
            item: None,
        };
    }
    Unit {
        first,
        last,
        item: None,
    }
}

/// The units of every selection, sorted; units that overlap or touch are
/// merged (and are then plain lines). Each unit lists the selections in it.
fn units(doc: &Doc) -> Vec<(Unit, Vec<usize>)> {
    let mut all: Vec<(Unit, usize)> = doc
        .sels
        .iter()
        .enumerate()
        .map(|(i, spec)| (unit_of(doc, *spec), i))
        .collect();
    all.sort_by_key(|(unit, _)| (unit.first, unit.last));
    let mut merged: Vec<(Unit, Vec<usize>)> = Vec::new();
    for (unit, i) in all {
        if let Some((prev, sels)) = merged.last_mut()
            && unit.first <= prev.last + 1
        {
            let same = prev.first == unit.first && prev.last == unit.last && prev.item == unit.item;
            if !same {
                prev.item = None;
            }
            prev.last = prev.last.max(unit.last);
            sels.push(i);
            continue;
        }
        merged.push((unit, vec![i]));
    }
    merged
}

/// The sibling item before (or after) the item with `range`: its first and
/// last line (trailing blank lines left out).
fn sibling(doc: &Doc, item: &Range<usize>, before: bool) -> Option<(usize, usize)> {
    let ctx = line_ctx(doc, doc.line_of(item.start));
    let ii = ctx.chain.iter().rposition(|level| level.range == *item)?;
    let list = ctx.chain.get(ii.checked_sub(1)?)?;
    list.list()?;
    let (_, items) = list_items(doc, &list.range)?;
    let k = items.iter().position(|(range, _)| range == item)?;
    let other = if before {
        items.get(k.checked_sub(1)?)?
    } else {
        items.get(k + 1)?
    };
    let first = other.1.marker_line;
    let end = doc.line_of(other.0.end.saturating_sub(1).max(other.0.start));
    let end = if end > first && doc.line(end).start == other.0.end {
        end - 1
    } else {
        end
    };
    Some((first, trim_blank_tail(doc, first, end)))
}

/// Lines `first..=last` rewritten with their contents in `order` (indices
/// into the region), each line ending staying in its place. Returns the
/// edit and, per region line, where its content starts in the new text.
fn reorder(
    doc: &Doc,
    first: usize,
    last: usize,
    order: &[usize],
) -> (Range<usize>, String, Vec<usize>) {
    let lines: Vec<Line> = (first..=last).map(|i| doc.line(i)).collect();
    let start = lines[0].start;
    let end = lines[lines.len() - 1].end();
    let mut text = String::new();
    let mut starts = vec![0; lines.len()];
    for (k, &src) in order.iter().enumerate() {
        starts[src] = start + text.len();
        text.push_str(&lines[src].text);
        if k + 1 < lines.len() {
            text.push_str(lines[k].terminator.map_or("", LineEnding::as_str));
        }
    }
    (start..end, text, starts)
}

/// Where a selection end at `offset` in a unit ending on line `last`
/// lands, the region's lines starting at `starts`. An end at the start of
/// the line after the unit (a selection of whole lines) stays after the
/// unit's last line and its line ending.
fn moved(
    doc: &Doc,
    offset: usize,
    first: usize,
    last: usize,
    starts: &[usize],
    order: &[usize],
) -> usize {
    let line = doc.line_at(offset);
    if line.index > last {
        let k = last - first;
        let end_line = doc.line(last);
        let position = order.iter().position(|&src| src == k).unwrap_or(k);
        let ending = doc
            .line(first + position)
            .terminator
            .map_or(0, LineEnding::byte_len);
        return starts[k] + end_line.text.len() + ending;
    }
    let k = line.index - first;
    starts[k] + (offset - line.start)
}

/// Moves the lines (items, blocks) at the selections one place up or down.
/// Nothing moves if one of them is at the edge of the document.
pub(crate) fn move_lines(doc: &mut Doc, up: bool) {
    let units = units(doc);
    let count = doc.line_count();
    if units.is_empty() {
        return;
    }
    let at_edge = if up {
        units[0].0.first == 0
    } else {
        units[units.len() - 1].0.last + 1 >= count
    };
    if at_edge {
        return;
    }
    // The neighbour each unit changes places with: the sibling item for an
    // item, else one line.
    let mut regions: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (unit, _) in &units {
        let single = if up {
            (unit.first - 1, unit.first - 1)
        } else {
            (unit.last + 1, unit.last + 1)
        };
        let neighbour = unit
            .item
            .as_ref()
            .and_then(|item| sibling(doc, item, up))
            .filter(|&(a, b)| if up { b < unit.first } else { a > unit.last })
            .unwrap_or(single);
        regions.push((unit.first, unit.last, neighbour.0, neighbour.1));
    }
    let overlaps = |regions: &[(usize, usize, usize, usize)]| {
        let spans: Vec<(usize, usize)> = regions
            .iter()
            .map(|&(a, b, c, d)| (a.min(c), b.max(d)))
            .collect();
        spans.windows(2).any(|w| w[1].0 <= w[0].1)
    };
    if overlaps(&regions) {
        for (region, (unit, _)) in regions.iter_mut().zip(&units) {
            let line = if up { unit.first - 1 } else { unit.last + 1 };
            *region = (unit.first, unit.last, line, line);
        }
    }
    let mut edits = Vec::new();
    let mut sels = doc.sels.clone();
    for (&(a, b, c, d), (_, members)) in regions.iter().zip(&units) {
        // The region: the neighbour, the gap, the unit (or the other way).
        let (first, last) = (a.min(c), b.max(d));
        let unit: Vec<usize> = (a - first..=b - first).collect();
        let neighbour: Vec<usize> = (c - first..=d - first).collect();
        let gap: Vec<usize> = if up {
            (d + 1 - first..a - first).collect()
        } else {
            (b + 1 - first..c - first).collect()
        };
        let order: Vec<usize> = if up {
            unit.iter().chain(&gap).chain(&neighbour).copied().collect()
        } else {
            neighbour.iter().chain(&gap).chain(&unit).copied().collect()
        };
        let (range, text, starts) = reorder(doc, first, last, &order);
        for &i in members {
            let spec = doc.sels[i];
            sels[i] = SelectionSpec::new(
                moved(doc, spec.tail, first, b, &starts, &order),
                moved(doc, spec.head, first, b, &starts, &order),
            );
        }
        let old = doc.slice(range.clone());
        edits.extend(minimal(&old, range.start, text));
    }
    if doc.apply(edits).is_some() {
        doc.sels = sels;
    }
}

/// Copies the lines the selections cover below them; the selections move
/// onto the copy.
pub(crate) fn duplicate_lines(doc: &mut Doc) {
    let mut blocks: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    let mut order: Vec<(Range<usize>, usize)> = doc
        .sels
        .iter()
        .enumerate()
        .map(|(i, spec)| (selected_lines(doc, *spec), i))
        .collect();
    order.sort_by_key(|(lines, _)| (lines.start, lines.end));
    for (lines, i) in order {
        let (first, last) = (lines.start, lines.end - 1);
        if let Some(prev) = blocks.last_mut()
            && first <= prev.1
        {
            prev.1 = prev.1.max(last);
            prev.2.push(i);
            continue;
        }
        blocks.push((first, last, vec![i]));
    }
    let mut edits = Vec::new();
    let mut sels = doc.sels.clone();
    let mut shift = 0;
    for (first, last, members) in blocks {
        let end_line = doc.line(last);
        let start = doc.line(first).start;
        let copy = doc.slice(start..end_line.end());
        let text = format!("{}{copy}", doc.line_ending_for(&end_line));
        let len = text.len();
        for i in members {
            let spec = doc.sels[i];
            sels[i] = SelectionSpec::new(spec.tail + shift + len, spec.head + shift + len);
        }
        shift += len;
        edits.push((end_line.end()..end_line.end(), text));
    }
    if doc.apply(edits).is_some() {
        doc.sels = sels;
    }
}

/// Joins the lines each selection covers (a caret or a selection on one
/// line: its line and the next) with one space, dropping the next lines'
/// container prefixes (`> `, item indentation) and leading whitespace and
/// the trailing whitespace before them. A caret goes to the join.
pub(crate) fn join_lines(doc: &mut Doc) {
    let count = doc.line_count();
    let mut joins: Vec<usize> = Vec::new();
    let mut carets: Vec<(usize, usize)> = Vec::new();
    for (i, spec) in doc.sels.iter().enumerate() {
        let lines = selected_lines(doc, *spec);
        let (first, mut last) = (lines.start, lines.end - 1);
        if last == first {
            last = first + 1;
        }
        if last >= count {
            continue;
        }
        joins.extend(first..last);
        if spec.tail == spec.head {
            carets.push((i, first));
        }
    }
    joins.sort_unstable();
    joins.dedup();
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut join_at: Vec<(usize, usize)> = Vec::new();
    for index in joins {
        let line = doc.line(index);
        let next = line_ctx(doc, index + 1);
        let trimmed = line.text.trim_end_matches([' ', '\t']).len();
        let from = line.start + trimmed;
        let rest = &next.line.text[next.line.rel(next.content_start)..];
        let lead = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let to = next.content_start + lead;
        let sep = if trimmed == 0
            || rest.trim().is_empty()
            || line.text[..trimmed].ends_with(char::is_whitespace)
        {
            ""
        } else {
            " "
        };
        join_at.push((index, from));
        edits.push((from..to, sep.to_owned()));
    }
    // Carets go to the first join of their line, before the space.
    let mut placed: Vec<(usize, usize)> = Vec::new();
    for (i, first) in carets {
        if let Some(&(_, at)) = join_at.iter().find(|(index, _)| *index == first) {
            placed.push((i, at));
        }
    }
    let Some(summary) = doc.apply(edits) else {
        return;
    };
    for (i, at) in placed {
        let caret = summary.transform_offset(at, gv_text::Bias::Left);
        doc.sels[i] = SelectionSpec::caret(caret);
    }
}

/// A line's structural head (indentation, `>` markers, a list marker and
/// its padding, a task box) and the rest.
fn split_head(text: &str) -> (&str, &str) {
    let b = text.as_bytes();
    let ws = |c: u8| c == b' ' || c == b'\t';
    let mut i = 0;
    while i < b.len() && (ws(b[i]) || b[i] == b'>') {
        i += 1;
    }
    let mut j = i;
    if b.get(j).is_some_and(|c| b"-+*".contains(c)) {
        j += 1;
    } else {
        let digits = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        if (1..=9).contains(&digits) && matches!(b.get(j + digits), Some(b'.' | b')')) {
            j += digits + 1;
        }
    }
    if j > i && b.get(j).is_none_or(|&c| ws(c)) {
        i = j;
        while i < b.len() && ws(b[i]) {
            i += 1;
        }
        if b.len() >= i + 3
            && b[i] == b'['
            && b[i + 2] == b']'
            && b.get(i + 3).is_none_or(|&c| ws(c))
        {
            i += 3;
            while i < b.len() && ws(b[i]) {
                i += 1;
            }
        }
    }
    // A list marker with nothing after it is all head.
    text.split_at(i.min(text.len()))
}

/// Whether `head` is an ordered list marker (with its prefix).
fn ordered_head(head: &str) -> Option<String> {
    let trimmed = head.trim_end();
    let last = trimmed.chars().last()?;
    if !matches!(last, '.' | ')') {
        return None;
    }
    let body = &trimmed[..trimmed.len() - 1];
    let digits = body.bytes().rev().take_while(u8::is_ascii_digit).count();
    (digits > 0).then(|| format!("{}#{last}", &body[..body.len() - digits]))
}

/// Sorts the lines each selection covers by their text after the
/// structural head, ignoring case. A caret, or a selection on one line,
/// sorts the list it is in (one line per item: items with continuation
/// lines are sorted as lines), else its paragraph. When every line is an
/// item of one ordered list, the numbers stay where they are.
pub(crate) fn sort_lines(doc: &mut Doc) {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for spec in doc.sels.clone() {
        let lines = selected_lines(doc, spec);
        let (mut first, mut last) = (lines.start, lines.end - 1);
        if first == last {
            let ctx = line_ctx(doc, first);
            let list = ctx.chain.iter().rev().find(|level| level.list().is_some());
            if let Some(list) = list {
                first = doc.line_of(list.range.start);
                let end = doc.line_of(list.range.end.saturating_sub(1).max(list.range.start));
                let end = if end > first && doc.line(end).start == list.range.end {
                    end - 1
                } else {
                    end
                };
                last = trim_blank_tail(doc, first, end);
            } else if let Some(leaf) = &ctx.leaf
                && leaf.kind == LeafKind::Paragraph
            {
                first = leaf.first_line;
                last = leaf.last_line;
            }
        }
        if last > first {
            ranges.push((first, last));
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (first, last) in ranges {
        if let Some(prev) = merged.last_mut()
            && first <= prev.1
        {
            prev.1 = prev.1.max(last);
            continue;
        }
        merged.push((first, last));
    }
    let mut edits = Vec::new();
    // Selections keep their lines and columns.
    let keep: Vec<(usize, usize, usize, usize)> = doc
        .sels
        .iter()
        .map(|spec| {
            let (t, h) = (doc.line_at(spec.tail), doc.line_at(spec.head));
            (t.index, spec.tail - t.start, h.index, spec.head - h.start)
        })
        .collect();
    for (first, last) in merged {
        let lines: Vec<Line> = (first..=last).map(|i| doc.line(i)).collect();
        let parts: Vec<(&str, &str)> = lines.iter().map(|line| split_head(&line.text)).collect();
        let shapes: Vec<Option<String>> =
            parts.iter().map(|(head, _)| ordered_head(head)).collect();
        let keep_heads = shapes[0].is_some() && shapes.iter().all(|shape| *shape == shapes[0]);
        let mut order: Vec<usize> = (0..lines.len()).collect();
        order.sort_by(|&a, &b| compare(parts[a].1, parts[b].1));
        if order.iter().enumerate().all(|(k, &src)| k == src) {
            continue;
        }
        let mut text = String::new();
        for (k, &src) in order.iter().enumerate() {
            if keep_heads {
                text.push_str(parts[k].0);
                text.push_str(parts[src].1);
            } else {
                text.push_str(&lines[src].text);
            }
            if k + 1 < lines.len() {
                text.push_str(lines[k].terminator.map_or("", LineEnding::as_str));
            }
        }
        let range = lines[0].start..lines[lines.len() - 1].end();
        let old = doc.slice(range.clone());
        edits.extend(minimal(&old, range.start, text));
    }
    if doc.apply(edits).is_some() {
        let at = |doc: &Doc, index: usize, col: usize| {
            let line = doc.line(index);
            let rel = col.min(line.text.len());
            let rel = (0..=rel)
                .rev()
                .find(|&r| line.text.is_char_boundary(r))
                .unwrap_or(0);
            line.start + rel
        };
        doc.sels = keep
            .into_iter()
            .map(|(tl, tc, hl, hc)| SelectionSpec::new(at(doc, tl, tc), at(doc, hl, hc)))
            .collect();
    }
}

/// Sort order: case-insensitive, then by the exact text.
fn compare(a: &str, b: &str) -> Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// The case a text is changed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Case {
    Upper,
    Lower,
    Title,
}

/// `text` in `case`.
fn recase(text: &str, case: Case) -> String {
    match case {
        Case::Upper => text.to_uppercase(),
        Case::Lower => text.to_lowercase(),
        Case::Title => {
            let mut out = String::with_capacity(text.len());
            for word in text.split_word_bounds() {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) if first.is_alphanumeric() => {
                        out.extend(first.to_uppercase());
                        out.push_str(&chars.as_str().to_lowercase());
                    }
                    _ => out.push_str(word),
                }
            }
            out
        }
    }
}

/// The next case in the cycle lower → UPPER → Title → lower, from the
/// case `text` is in (mixed text goes to lower case).
fn next_case(text: &str) -> Case {
    let lower = text.to_lowercase();
    let upper = text.to_uppercase();
    if text == lower && text != upper {
        Case::Upper
    } else if text == upper && text != lower {
        Case::Title
    } else {
        Case::Lower
    }
}

/// The word around `offset` (a caret at either end of a word is in it).
pub(crate) fn word_at(doc: &Doc, offset: usize) -> Option<Range<usize>> {
    let line = doc.line_at(offset);
    let rel = line.rel(offset);
    let mut best = None;
    for (start, word) in line.text.split_word_bound_indices() {
        let end = start + word.len();
        if start > rel {
            break;
        }
        if word.chars().any(char::is_alphanumeric) && rel <= end {
            best = Some(line.start + start..line.start + end);
            if rel < end {
                break;
            }
        }
    }
    best
}

/// Changes the case of every selection (a caret: the word at it), cycling
/// lower → UPPER → Title Case, decided by the primary selection's text.
/// Selections keep covering the changed text.
pub(crate) fn change_case(doc: &mut Doc) {
    let targets: Vec<(usize, Range<usize>)> = doc
        .sels
        .iter()
        .enumerate()
        .filter_map(|(i, spec)| {
            let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
            if s < e {
                Some((i, s..e))
            } else {
                word_at(doc, s).map(|word| (i, word))
            }
        })
        .collect();
    let Some(sample) = targets
        .iter()
        .find(|(i, _)| *i == doc.primary)
        .or_else(|| targets.first())
        .map(|(_, range)| doc.slice(range.clone()))
    else {
        return;
    };
    let case = next_case(&sample);
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut claimed: Vec<Range<usize>> = Vec::new();
    for (_, range) in &targets {
        if claimed
            .iter()
            .any(|c| c.start < range.end && range.start < c.end)
        {
            continue;
        }
        claimed.push(range.clone());
        let old = doc.slice(range.clone());
        let new = recase(&old, case);
        if new != old {
            edits.push((range.clone(), new));
        }
    }
    edits.sort_by_key(|(range, _)| range.start);
    // Where each old offset lands: an edit's ends stay its ends, an
    // offset inside it keeps its distance from the start.
    let map = |offset: usize| -> usize {
        let mut delta: isize = 0;
        for (range, text) in &edits {
            if offset < range.start {
                break;
            }
            let start = range.start.saturating_add_signed(delta);
            if offset == range.end {
                return start + text.len();
            }
            if offset < range.end {
                return start + (offset - range.start).min(text.len());
            }
            delta += text.len() as isize - range.len() as isize;
        }
        offset.saturating_add_signed(delta)
    };
    let sels: Vec<SelectionSpec> = doc
        .sels
        .iter()
        .map(|spec| {
            let forward = spec.tail <= spec.head;
            let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
            let (ns, ne) = (map(s), map(e));
            if forward {
                SelectionSpec::new(ns, ne)
            } else {
                SelectionSpec::new(ne, ns)
            }
        })
        .collect();
    if doc.apply(edits).is_some() {
        doc.sels = sels;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads() {
        assert_eq!(split_head("- a"), ("- ", "a"));
        assert_eq!(split_head("  12. b"), ("  12. ", "b"));
        assert_eq!(split_head("> - [x] c"), ("> - [x] ", "c"));
        assert_eq!(split_head("-a"), ("", "-a"));
        assert_eq!(split_head("text"), ("", "text"));
        assert_eq!(ordered_head("  12. "), Some("  #.".to_owned()));
        assert_eq!(ordered_head("- "), None);
    }

    #[test]
    fn cases() {
        assert_eq!(next_case("abc"), Case::Upper);
        assert_eq!(next_case("ABC"), Case::Title);
        assert_eq!(next_case("Abc"), Case::Lower);
        assert_eq!(next_case("123"), Case::Lower);
        assert_eq!(recase("hello wORLD-x", Case::Title), "Hello World-X");
        assert_eq!(recase("straße", Case::Upper), "STRASSE");
    }
}

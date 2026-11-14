// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! List and block quote structure: continuation on Enter, indent and
//! outdent, renumbering, task boxes.
//!
//! Every function here edits a [`Doc`] in steps; the steps never add or
//! remove lines except the new line of a continuation, so line indices are
//! stable across the steps of indent, outdent and renumbering.

use std::collections::BTreeSet;
use std::ops::Range;

use gv_text::{Bias, EditSummary};

use crate::selection::SelectionSpec;

use super::context::{
    ItemInfo, LevelKind, LineCtx, ListInfo, Marker, child_containers, ctx_at, line_ctx, list_items,
    spaces, task_box,
};
use super::doc::Doc;
use super::lines::{replace_in_prefix, shift_edit};
use super::settings::{CommandSettings, IndentUnit, Numbering};

/// The highest list item number CommonMark allows (nine digits).
const MAX_NUMBER: u32 = 999_999_999;

/// How an ordered list is numbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    /// Start, start + 1, start + 2, …
    Sequential,
    /// Every item has the start number (`1.` `1.` `1.`).
    Same,
}

/// The numbering a list uses: all items equal (two or more) is
/// [`Style::Same`], other lists of two or more are sequential, a single
/// item follows the settings.
pub(crate) fn detect_style(
    items: &[(Range<usize>, ItemInfo)],
    settings: &CommandSettings,
) -> Style {
    let numbers: Vec<u32> = items
        .iter()
        .filter_map(|(_, item)| item.marker.number())
        .collect();
    match numbers.as_slice() {
        [first, _, ..] if numbers.iter().all(|n| n == first) => Style::Same,
        [_, _, ..] => Style::Sequential,
        _ => match settings.numbering {
            Numbering::Sequential => Style::Sequential,
            Numbering::Ones => Style::Same,
        },
    }
}

/// The level index of the innermost list item, if the line is in a list
/// more deeply than in any block quote.
pub(crate) fn list_level(ctx: &LineCtx) -> Option<usize> {
    let item = ctx.innermost_item()?;
    match ctx.innermost_quote() {
        Some(quote) if quote > item => None,
        _ => Some(item),
    }
}

/// The level index of the innermost item whose marker is on the line.
fn marker_level(ctx: &LineCtx) -> Option<usize> {
    ctx.chain.iter().rposition(|level| {
        level
            .item()
            .is_some_and(|item| item.marker_line == ctx.line.index)
    })
}

/// The prefix that continues `ctx.chain[..upto]` on a new line: `> ` per
/// block quote (keeping its indentation), the content offset in spaces per
/// list item.
pub(crate) fn continuation_prefix(ctx: &LineCtx, upto: usize) -> String {
    let mut out = String::new();
    for (k, level) in ctx.chain.iter().take(upto).enumerate() {
        match &level.kind {
            LevelKind::Quote => {
                let indent = ctx.parts.get(k).cloned().flatten().map_or(0, |range| {
                    ctx.line.text[ctx.line.rel(range.start)..ctx.line.rel(range.end)]
                        .bytes()
                        .take_while(|&b| b == b' ')
                        .count()
                });
                out.push_str(&spaces(indent));
                out.push_str("> ");
            }
            LevelKind::List(_) => {}
            LevelKind::Item(item) => out.push_str(&spaces(item.content_offset)),
        }
    }
    out
}

/// Text inserted on a blank line of `ctx`: at the content start after a
/// marker or quote opened on the line (with a space after a bare marker),
/// else replacing the line with the continuation prefix of its containers
/// (a blank line has no item indentation of its own). Returns the range,
/// the replacement, and where `text` starts in it.
pub(crate) fn blank_line_insert(ctx: &LineCtx, text: &str) -> (Range<usize>, String, usize) {
    let opens_here = ctx.chain.iter().any(|level| match level.item() {
        Some(item) => item.marker_line == ctx.line.index,
        None => level.range.start >= ctx.line.start && level.range.start <= ctx.line.end(),
    });
    if opens_here || ctx.chain.is_empty() {
        let pad = ctx.pad();
        let at = ctx.content_start;
        return (at..at, format!("{pad}{text}"), pad.len());
    }
    let prefix = continuation_prefix(ctx, ctx.chain.len());
    let offset = prefix.len();
    (ctx.line.start..ctx.line.end(), prefix + text, offset)
}

/// The last line of a block with `range`.
fn last_line(doc: &Doc, range: &Range<usize>) -> usize {
    doc.line_of(range.end.saturating_sub(1).max(range.start))
}

/// Replaces `range` with `text` as one step and puts selection `i` as a
/// caret after the text. Returns the caret.
fn replace_and_place(doc: &mut Doc, i: usize, range: Range<usize>, text: String) -> usize {
    let start = range.start;
    let len = text.len();
    match doc.apply(vec![(range, text)]) {
        Some(summary) => {
            let caret = summary.transform_offset(start, Bias::Left) + len;
            doc.sels[i] = SelectionSpec::caret(caret);
            caret
        }
        None => start,
    }
}

/// What a container found by its range is (a list and its only item have
/// the same range).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    List,
    Item,
}

/// The index of the container with `range` and kind `kind` in `ctx`'s
/// chain.
fn find_level(ctx: &LineCtx, range: &Range<usize>, kind: Anchor) -> Option<usize> {
    ctx.chain.iter().position(|level| {
        level.range == *range
            && match kind {
                Anchor::List => level.list().is_some(),
                Anchor::Item => level.item().is_some(),
            }
    })
}

/// Shifts lines `lines` by `delta` columns where the region of the
/// container with `anchor` starts on each of them. Blank lines and lines
/// lazy before or at that container are left alone.
fn shift_lines(
    doc: &Doc,
    lines: Range<usize>,
    anchor: (&Range<usize>, Anchor),
    delta: isize,
) -> Vec<(Range<usize>, String)> {
    let (anchor, kind) = anchor;
    let mut edits = Vec::new();
    if delta == 0 {
        return edits;
    }
    for index in lines {
        let ctx = line_ctx(doc, index);
        if ctx.blank {
            continue;
        }
        let Some(k) = find_level(&ctx, anchor, kind) else {
            continue;
        };
        if ctx.parts.get(k).cloned().flatten().is_none() {
            continue;
        }
        if let Some(at) = ctx.region_start(k) {
            edits.extend(shift_edit(doc, &ctx.line, at, delta));
        }
    }
    edits
}

// ----- renumbering --------------------------------------------------------

/// Gives `items` the numbers `numbers`, keeping each item's content
/// aligned when a marker's width changes. One step.
fn renumber_items(
    doc: &mut Doc,
    items: &[(Range<usize>, ItemInfo)],
    numbers: impl Iterator<Item = u32>,
) -> bool {
    let mut edits = Vec::new();
    for ((range, item), number) in items.iter().zip(numbers) {
        let Marker::Ordered {
            number: old,
            delimiter,
        } = item.marker
        else {
            continue;
        };
        let number = number.min(MAX_NUMBER);
        if old == number {
            continue;
        }
        let marker = Marker::Ordered { number, delimiter };
        let line = doc.line(item.marker_line);
        edits.extend(replace_in_prefix(
            doc,
            &line,
            item.marker_range.clone(),
            &marker.text(),
        ));
        let delta = marker.width() as isize - item.marker.width() as isize;
        let body = item.marker_line + 1..last_line(doc, range) + 1;
        edits.extend(shift_lines(doc, body, (range, Anchor::Item), delta));
    }
    doc.apply(edits).is_some()
}

/// Renumbers the ordered list with `range` from its first number. One
/// step.
pub(crate) fn renumber_list(doc: &mut Doc, range: &Range<usize>, style: Style) -> bool {
    renumber_list_from(doc, range, style, None)
}

/// Renumbers the ordered list with `range` from `start`, or from its first
/// number when `start` is `None`. One step.
fn renumber_list_from(
    doc: &mut Doc,
    range: &Range<usize>,
    style: Style,
    start: Option<u32>,
) -> bool {
    let Some((_, items)) = list_items(doc, range) else {
        return false;
    };
    let Some(start) = start.or_else(|| items.first().and_then(|(_, item)| item.marker.number()))
    else {
        return false;
    };
    let numbers = (0..).map(|k: u32| match style {
        Style::Sequential => start.saturating_add(k),
        Style::Same => start,
    });
    renumber_items(doc, &items, numbers)
}

/// Renumbers the list of the item that starts at `item_start`, if it is
/// ordered.
fn renumber_list_of_item(doc: &mut Doc, item_start: usize, style: Style) -> bool {
    renumber_list_of_item_from(doc, item_start, style, None)
}

/// [`renumber_list_of_item`] from `start` (see [`renumber_list_from`]).
fn renumber_list_of_item_from(
    doc: &mut Doc,
    item_start: usize,
    style: Style,
    start: Option<u32>,
) -> bool {
    let ctx = ctx_at(doc, item_start);
    let Some(k) = ctx
        .chain
        .iter()
        .position(|level| level.range.start == item_start && level.item().is_some())
    else {
        return false;
    };
    match k.checked_sub(1).map(|list| &ctx.chain[list]) {
        Some(list) if list.list().is_some_and(|info| info.ordered) => {
            let range = list.range.clone();
            renumber_list_from(doc, &range, style, start)
        }
        _ => false,
    }
}

/// The explicit renumber command: the innermost ordered list at each
/// caret, numbered per the settings.
pub(crate) fn renumber_command(doc: &mut Doc, settings: &CommandSettings) {
    let style = match settings.numbering {
        Numbering::Sequential => Style::Sequential,
        Numbering::Ones => Style::Same,
    };
    // Renumbering changes no line's block structure, so a list is found
    // again by its first line and its level there.
    let mut lists: BTreeSet<(usize, usize)> = BTreeSet::new();
    for spec in doc.sels.clone() {
        let ctx = ctx_at(doc, spec.head);
        let Some(ii) = ctx.innermost_item() else {
            continue;
        };
        if let Some(list) = ii.checked_sub(1).map(|k| &ctx.chain[k])
            && list.list().is_some_and(|info| info.ordered)
        {
            lists.insert((doc.line_of(list.range.start), ii - 1));
        }
    }
    for (line, k) in lists {
        let ctx = line_ctx(doc, line);
        if let Some(list) = ctx.chain.get(k)
            && list.list().is_some_and(|info| info.ordered)
        {
            let range = list.range.clone();
            renumber_list(doc, &range, style);
        }
    }
}

// ----- Enter -----------------------------------------------------------------

/// Whether the item at level `ii` of `ctx` (on its marker line) has no
/// content: nothing after the marker (or the task box) and only blank lines
/// after it.
fn is_empty_item(doc: &Doc, ctx: &LineCtx, ii: usize) -> bool {
    let level = &ctx.chain[ii];
    let Some(item) = level.item() else {
        return false;
    };
    let rest = task_box(doc, item).map_or(item.content_start, |task| task.with_space.end);
    if !ctx.line.blank_from(ctx.line.rel(rest)) {
        return false;
    }
    let last = last_line(doc, &level.range);
    (item.marker_line + 1..=last).all(|index| line_ctx(doc, index).blank)
}

/// Enter (or Shift+Enter with `plain`) at every selection.
pub(crate) fn newline(doc: &mut Doc, settings: &CommandSettings, plain: bool) {
    for i in 0..doc.sels.len() {
        newline_one(doc, settings, i, plain);
    }
}

fn newline_one(doc: &mut Doc, settings: &CommandSettings, i: usize, plain: bool) {
    let spec = doc.sels[i];
    let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
    let ctx = ctx_at(doc, s);
    let le = doc.line_ending_for(&ctx.line);
    let one_line = doc.line_of(e) == ctx.line.index;
    // A caret in the container prefixes or a marker: a blank line above
    // that keeps the containers continuing from the lines before. A
    // selection from there starts where the content does.
    let in_prefix = s < ctx.content_start && !ctx.lazy;
    let opens_list_here = list_level(&ctx).is_some() && !plain && !ctx.in_verbatim();
    if in_prefix && s == e && !opens_list_here {
        let continuing = ctx
            .chain
            .iter()
            .take_while(|level| level.range.start < ctx.line.start)
            .count();
        let text = format!("{}{le}", continuation_prefix(&ctx, continuing));
        let at = ctx.line.start;
        if let Some(summary) = doc.apply(vec![(at..at, text)]) {
            doc.sels[i] = SelectionSpec::caret(summary.transform_offset(s, Bias::Right));
        }
        return;
    }
    let s = if in_prefix { ctx.content_start } else { s };
    let e = e.max(s);
    if plain || ctx.in_verbatim() {
        let prefix = continuation_prefix(&ctx, ctx.chain.len());
        let lazy_tail = lazy_tail(doc, &ctx, doc.line_of(e));
        if let Some(summary) = replace_and_place_summary(doc, i, s..e, format!("{le}{prefix}")) {
            reprefix(doc, &summary, &lazy_tail, &prefix);
        }
        return;
    }
    if let Some(ii) = list_level(&ctx) {
        continue_list(doc, settings, i, &ctx, ii, s..e, one_line);
        return;
    }
    if let Some(qi) = ctx.innermost_quote() {
        if ctx.blank
            && !ctx.lazy
            && one_line
            && let Some(part) = ctx.parts.get(qi).cloned().flatten()
        {
            // An empty quote line: leave the innermost quote.
            let start = part.start;
            replace_and_place(doc, i, part, String::new());
            doc.sels[i] = SelectionSpec::caret(start);
            return;
        }
        let prefix = continuation_prefix(&ctx, ctx.chain.len());
        let lazy_tail = lazy_tail(doc, &ctx, doc.line_of(e));
        if let Some(summary) = replace_and_place_summary(doc, i, s..e, format!("{le}{prefix}")) {
            reprefix(doc, &summary, &lazy_tail, &prefix);
        }
        return;
    }
    replace_and_place(doc, i, s..e, le.to_owned());
}

/// The content starts of the lazy lines of `ctx`'s paragraph after line
/// `after`.
fn lazy_tail(doc: &Doc, ctx: &LineCtx, after: usize) -> Vec<usize> {
    let Some(leaf) = ctx
        .leaf
        .as_ref()
        .filter(|leaf| leaf.kind == gv_syntax::LeafKind::Paragraph)
    else {
        return Vec::new();
    };
    leaf.lines
        .iter()
        .filter(|line| line.has_content && line.line > after && line_ctx(doc, line.line).lazy)
        .map(|line| line.content.start)
        .collect()
}

/// Gives the lines whose text starts at `contents` (offsets before the
/// step `summary`) the container prefix `prefix`. One step.
fn reprefix(doc: &mut Doc, summary: &EditSummary, contents: &[usize], prefix: &str) {
    let edits: Vec<(Range<usize>, String)> = contents
        .iter()
        .map(|&content| summary.transform_offset(content, Bias::Left))
        .filter_map(|content| {
            let line = doc.line_at(content);
            (doc.slice(line.start..content) != prefix)
                .then(|| (line.start..content, prefix.to_owned()))
        })
        .collect();
    doc.apply(edits);
}

/// Like [`replace_and_place`], returning the step's summary.
fn replace_and_place_summary(
    doc: &mut Doc,
    i: usize,
    range: Range<usize>,
    text: String,
) -> Option<EditSummary> {
    let start = range.start;
    let len = text.len();
    let summary = doc.apply(vec![(range, text)])?;
    doc.sels[i] = SelectionSpec::caret(summary.transform_offset(start, Bias::Left) + len);
    Some(summary)
}

fn continue_list(
    doc: &mut Doc,
    settings: &CommandSettings,
    i: usize,
    ctx: &LineCtx,
    ii: usize,
    range: Range<usize>,
    one_line: bool,
) {
    let le = doc.line_ending_for(&ctx.line);
    let Some(item) = ctx.chain[ii].item().cloned() else {
        return;
    };
    let on_marker = ctx.line.index == item.marker_line;
    if on_marker && one_line && is_empty_item(doc, ctx, ii) {
        let nested = ii >= 2 && ctx.chain[ii - 2].item().is_some();
        if nested {
            if outdent_item(doc, settings, item.marker_line) {
                let line = doc.line(item.marker_line);
                doc.sels[i] = SelectionSpec::caret(line.end());
            }
            return;
        }
        // An empty item at the top level ends the list: its marker is
        // removed. In the middle of a list the blank line left behind does
        // not split it (CommonMark keeps one, now loose, list), so the
        // following items are renumbered from the list's start number to
        // stay sequential.
        let following = following_renumber(doc, settings, ctx, ii);
        let start = item.token.start;
        let Some(summary) = doc.apply(vec![(start..ctx.line.end(), String::new())]) else {
            return;
        };
        doc.sels[i] = SelectionSpec::caret(summary.transform_offset(start, Bias::Left));
        if let Some((next, first)) = following {
            let next = summary.transform_offset(next, Bias::Right);
            renumber_list_of_item_from(doc, next, Style::Sequential, Some(first));
        }
        return;
    }
    if ctx.blank && !on_marker {
        replace_and_place(doc, i, range, le.to_owned());
        return;
    }
    let list_range = ctx.chain[ii - 1].range.clone();
    let style = match item.marker {
        Marker::Ordered { .. } => list_items(doc, &list_range)
            .map_or(Style::Sequential, |(_, items)| {
                detect_style(&items, settings)
            }),
        Marker::Bullet(_) => Style::Sequential,
    };
    let marker = match item.marker {
        Marker::Bullet(c) => Marker::Bullet(c),
        Marker::Ordered { number, delimiter } => Marker::Ordered {
            number: match style {
                Style::Same => number,
                Style::Sequential => number.saturating_add(1).min(MAX_NUMBER),
            },
            delimiter,
        },
    };
    let outer = continuation_prefix(ctx, ii - 1);
    let sibling = format!(
        "{outer}{}{}{}",
        spaces(item.indent),
        marker.text(),
        spaces(item.padding)
    );
    let renumber = matches!(marker, Marker::Ordered { .. }) && style == Style::Sequential;
    if !on_marker
        && one_line
        && !ctx.lazy
        && is_bare_marker(&ctx.line.text[ctx.line.rel(ctx.content_start)..])
    {
        // An empty item that CommonMark reads as paragraph text or a setext
        // underline (an empty item cannot interrupt a paragraph): it becomes
        // the next sibling, as an empty nested item would.
        let line_start = ctx.line.start;
        let caret = replace_and_place(doc, i, line_start..ctx.line.end(), sibling);
        if renumber {
            renumber_list_of_item(doc, doc.line_at(caret).start + outer.len(), style);
        }
        return;
    }
    let task = if on_marker {
        task_box(doc, &item)
    } else {
        None
    };
    let mut bound = if on_marker {
        item.content_start
    } else {
        ctx.content_start
    };
    if let Some(task) = &task
        && range.start < task.with_space.end
    {
        bound = task.with_space.end;
    }
    let start = range.start.max(bound);
    let mut end = range.end.max(start);
    let end_line = doc.line_at(end);
    let b = end_line.bytes();
    let mut rel = end_line.rel(end);
    while rel < b.len() && (b[rel] == b' ' || b[rel] == b'\t') {
        rel += 1;
    }
    end = end.max(end_line.start + rel);
    let text = format!("{le}{sibling}{}", if task.is_some() { "[ ] " } else { "" });
    // Everything of the item after the split follows the text after the
    // caret into the new item: lazy lines of the paragraph get the new
    // item's prefix, other lines move by the difference of the content
    // offsets (a wider marker, `9.` → `10.`). Same step: these lines lie
    // after the split.
    let end_index = doc.line_of(end);
    let new_offset = item.indent + marker.width() + item.padding;
    let delta = new_offset as isize - item.content_offset as isize;
    let paragraph_lines: Vec<usize> = ctx
        .leaf
        .as_ref()
        .filter(|leaf| leaf.kind == gv_syntax::LeafKind::Paragraph)
        .map(|leaf| leaf.lines.iter().map(|line| line.line).collect())
        .unwrap_or_default();
    let item_range = ctx.chain[ii].range.clone();
    let new_prefix = format!("{outer}{}", spaces(new_offset));
    let mut edits = vec![(start..end, text.clone())];
    for index in end_index + 1..=last_line(doc, &item_range) {
        let line_ctx = line_ctx(doc, index);
        if line_ctx.blank {
            continue;
        }
        let Some(k) = find_level(&line_ctx, &item_range, Anchor::Item) else {
            if line_ctx.lazy && paragraph_lines.contains(&index) {
                edits.push((
                    line_ctx.line.start..line_ctx.content_start,
                    new_prefix.clone(),
                ));
            }
            continue;
        };
        match line_ctx.parts.get(k).cloned().flatten() {
            Some(_) => {
                if let Some(at) = line_ctx.region_start(k) {
                    edits.extend(shift_edit(doc, &line_ctx.line, at, delta));
                }
            }
            None if paragraph_lines.contains(&index) => {
                edits.push((
                    line_ctx.line.start..line_ctx.content_start,
                    new_prefix.clone(),
                ));
            }
            None => {}
        }
    }
    let len = text.len();
    let Some(summary) = doc.apply(edits) else {
        return;
    };
    let new_start = summary.transform_offset(start, Bias::Left);
    doc.sels[i] = SelectionSpec::caret(new_start + len);
    let item_start = new_start + le.len() + outer.len();
    if renumber {
        renumber_list_of_item(doc, item_start, style);
    }
}

/// For the empty item at level `ii` of `ctx` in a sequentially numbered
/// ordered list: the start of the item after it and the list's start
/// number, if there is a following item.
fn following_renumber(
    doc: &Doc,
    settings: &CommandSettings,
    ctx: &LineCtx,
    ii: usize,
) -> Option<(usize, u32)> {
    let list = ctx.chain.get(ii.checked_sub(1)?)?;
    if !list.list().is_some_and(|info| info.ordered) {
        return None;
    }
    let (_, items) = list_items(doc, &list.range)?;
    if detect_style(&items, settings) != Style::Sequential {
        return None;
    }
    let here = items
        .iter()
        .position(|(range, _)| *range == ctx.chain[ii].range)?;
    let (next, _) = items.get(here + 1)?;
    let first = items.first()?.1.marker.number()?;
    Some((next.start, first))
}

/// Whether `rest` is a list marker alone (with trailing whitespace).
fn is_bare_marker(rest: &str) -> bool {
    let rest = rest.trim_end_matches([' ', '\t']);
    let b = rest.as_bytes();
    match b {
        [b'-' | b'+' | b'*'] => true,
        [digits @ .., b'.' | b')'] => {
            (1..=9).contains(&digits.len()) && digits.iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

// ----- indent and outdent ----------------------------------------------------

/// The marker lines of the items the selections cover, the quote leaves
/// (by first line) they cover, and the selections that cover neither.
struct Targets {
    items: BTreeSet<usize>,
    quotes: BTreeSet<usize>,
    other: Vec<usize>,
}

/// The lines selection `spec` covers (a selection ending at the start of a
/// line does not cover it).
pub(crate) fn selected_lines(doc: &Doc, spec: SelectionSpec) -> Range<usize> {
    let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
    let first = doc.line_of(s);
    let mut last = doc.line_of(e);
    if e > s && last > first && doc.line(last).start == e {
        last -= 1;
    }
    first..last + 1
}

fn targets(doc: &Doc) -> Targets {
    let mut targets = Targets {
        items: BTreeSet::new(),
        quotes: BTreeSet::new(),
        other: Vec::new(),
    };
    for (i, spec) in doc.sels.iter().enumerate() {
        let mut any = false;
        for index in selected_lines(doc, *spec) {
            let ctx = line_ctx(doc, index);
            if ctx.in_verbatim() {
                continue;
            }
            if let Some(ii) = list_level(&ctx) {
                if let Some(item) = ctx.chain[ii].item() {
                    targets.items.insert(item.marker_line);
                    any = true;
                }
            } else if ctx.innermost_quote().is_some()
                && let Some(leaf) = &ctx.leaf
            {
                targets.quotes.insert(leaf.first_line);
                any = true;
            }
        }
        if !any {
            targets.other.push(i);
        }
    }
    targets
}

/// Tab: indent the list items and quotes the selections cover; elsewhere
/// insert the indent unit.
pub(crate) fn indent(doc: &mut Doc, settings: &CommandSettings) {
    let Targets {
        mut items,
        quotes,
        other,
    } = targets(doc);
    while let Some(&first) = items.first() {
        let ctx = line_ctx(doc, first);
        let Some(xi) = marker_level(&ctx) else {
            items.remove(&first);
            continue;
        };
        let list = ctx.chain[xi - 1].range.clone();
        let Some((_, siblings)) = list_items(doc, &list) else {
            items.remove(&first);
            continue;
        };
        let run: Vec<usize> = siblings
            .iter()
            .skip_while(|(_, item)| item.marker_line != first)
            .take_while(|(_, item)| items.contains(&item.marker_line))
            .map(|(_, item)| item.marker_line)
            .collect();
        let covered_to = siblings
            .iter()
            .filter(|(_, item)| run.contains(&item.marker_line))
            .map(|(range, _)| last_line(doc, range))
            .max()
            .unwrap_or(first);
        items.retain(|&line| line < first || line > covered_to);
        indent_run(doc, settings, &run);
    }
    for first in quotes {
        nest_quote(doc, first);
    }
    for i in other {
        insert_indent(doc, settings, i);
    }
}

/// Shift+Tab: outdent the list items and quotes the selections cover;
/// elsewhere remove one indent unit from the line starts.
pub(crate) fn outdent(doc: &mut Doc, settings: &CommandSettings) {
    let Targets {
        mut items,
        quotes,
        other,
    } = targets(doc);
    while let Some(first) = items.pop_first() {
        let ctx = line_ctx(doc, first);
        if let Some(xi) = marker_level(&ctx) {
            let last = last_line(doc, &ctx.chain[xi].range);
            items.retain(|&line| line > last);
        }
        outdent_item(doc, settings, first);
    }
    for first in quotes {
        unnest_quote(doc, first);
    }
    for i in other {
        remove_indent(doc, settings, i);
    }
}

/// Indents the consecutive sibling items with marker lines `run` under the
/// item before the first of them. One step plus renumbering.
fn indent_run(doc: &mut Doc, settings: &CommandSettings, run: &[usize]) -> bool {
    let Some(&first) = run.first() else {
        return false;
    };
    let ctx = line_ctx(doc, first);
    let Some(xi) = marker_level(&ctx) else {
        return false;
    };
    let list = ctx.chain[xi - 1].clone();
    let Some(list_info) = list.list() else {
        return false;
    };
    let Some((list_depth, siblings)) = list_items(doc, &list.range) else {
        return false;
    };
    let Some(pos) = siblings
        .iter()
        .position(|(_, item)| item.marker_line == first)
    else {
        return false;
    };
    if pos == 0 {
        return false;
    }
    let (parent_range, parent) = siblings[pos - 1].clone();
    let moved: Vec<(Range<usize>, ItemInfo)> =
        siblings[pos..].iter().take(run.len()).cloned().collect();
    let list_style = detect_style(&siblings, settings);
    // A list at the end of the new parent: the items join it.
    let nested = child_containers(doc, parent_range.clone(), list_depth + 1)
        .into_iter()
        .last()
        .filter(|child| {
            matches!(
                child.kind,
                gv_syntax::ContainerKind::BulletList { .. }
                    | gv_syntax::ContainerKind::OrderedList { .. }
            ) && (last_line(doc, &child.range) + 1..=last_line(doc, &parent_range))
                .all(|index| line_ctx(doc, index).blank)
        })
        .and_then(|child| {
            let (_, items) = list_items(doc, &child.range)?;
            let info = match child.kind {
                gv_syntax::ContainerKind::OrderedList { delimiter, .. } => ListInfo {
                    ordered: true,
                    bullet: b'-',
                    delimiter,
                },
                gv_syntax::ContainerKind::BulletList { marker, .. } => ListInfo {
                    ordered: false,
                    bullet: marker,
                    delimiter: b'.',
                },
                _ => return None,
            };
            Some((info, items))
        });
    let (relative, target_style) = match &nested {
        Some((_, items)) => (
            items.first().map_or(0, |(_, item)| item.indent),
            detect_style(items, settings),
        ),
        None => (0, list_style),
    };
    let target_col = parent.content_offset + relative;
    let mut edits = Vec::new();
    for (j, (range, item)) in moved.iter().enumerate() {
        let j = u32::try_from(j).unwrap_or(u32::MAX);
        let marker = match &nested {
            Some((info, items)) if info.ordered => {
                let last = items
                    .last()
                    .and_then(|(_, item)| item.marker.number())
                    .unwrap_or(1);
                Marker::Ordered {
                    number: match target_style {
                        Style::Same => last,
                        Style::Sequential => last.saturating_add(j + 1),
                    },
                    delimiter: info.delimiter,
                }
            }
            Some((info, _)) => Marker::Bullet(info.bullet),
            None => match item.marker {
                Marker::Ordered { delimiter, .. } => Marker::Ordered {
                    number: match target_style {
                        Style::Same => 1,
                        Style::Sequential => 1 + j,
                    },
                    delimiter,
                },
                bullet => bullet,
            },
        };
        let line_ctx = line_ctx(doc, item.marker_line);
        let Some(k) = find_level(&line_ctx, &list.range, Anchor::List) else {
            continue;
        };
        let Some(region) = line_ctx.region_start(k) else {
            continue;
        };
        let delta = target_col as isize - item.indent as isize;
        let widen = marker.width() as isize - item.marker.width() as isize;
        edits.extend(replace_in_prefix(
            doc,
            &line_ctx.line,
            region..item.marker_range.end,
            &format!("{}{}", spaces(target_col), marker.text()),
        ));
        let body = item.marker_line + 1..last_line(doc, range) + 1;
        edits.extend(shift_lines(
            doc,
            body,
            (&list.range, Anchor::List),
            delta + widen,
        ));
    }
    if doc.apply(edits).is_none() {
        return false;
    }
    if list_info.ordered {
        renumber_list_of_item(doc, parent_range.start, list_style);
    }
    let moved_ctx = line_ctx(doc, first);
    if let Some(k) = marker_level(&moved_ctx) {
        let start = moved_ctx.chain[k].range.start;
        renumber_list_of_item(doc, start, target_style);
    }
    true
}

/// Outdents the nested item whose marker is on `marker_line`: it becomes
/// the next sibling of its parent item, taking the parent's list marker;
/// the items and blocks after it in the parent become its children.
pub(crate) fn outdent_item(doc: &mut Doc, settings: &CommandSettings, marker_line: usize) -> bool {
    let ctx = line_ctx(doc, marker_line);
    let Some(xi) = marker_level(&ctx) else {
        return false;
    };
    if xi < 3 {
        return false;
    }
    let (Some(outer), Some(_), Some(inner)) = (
        ctx.chain[xi - 3].list(),
        ctx.chain[xi - 2].item(),
        ctx.chain[xi - 1].list(),
    ) else {
        return false;
    };
    let outer_range = ctx.chain[xi - 3].range.clone();
    let inner_range = ctx.chain[xi - 1].range.clone();
    let item_range = ctx.chain[xi].range.clone();
    let Some((_, outer_items)) = list_items(doc, &outer_range) else {
        return false;
    };
    let outer_style = detect_style(&outer_items, settings);
    let Some((_, inner_items)) = list_items(doc, &inner_range) else {
        return false;
    };
    let Some(pos) = inner_items
        .iter()
        .position(|(range, _)| *range == item_range)
    else {
        return false;
    };
    // The items after it become a list of their own under it: an ordered
    // one must start at 1 to follow its paragraph.
    let following = &inner_items[pos + 1..];
    if inner.ordered && !following.is_empty() {
        let style = detect_style(&inner_items, settings);
        renumber_items(
            doc,
            following,
            (0..).map(|k: u32| match style {
                Style::Same => 1,
                Style::Sequential => 1 + k,
            }),
        );
    }
    let ctx = line_ctx(doc, marker_line);
    let (Some(parent), Some(item)) = (ctx.chain[xi - 2].item(), ctx.chain[xi].item()) else {
        return false;
    };
    let parent_range = ctx.chain[xi - 2].range.clone();
    let item_range = ctx.chain[xi].range.clone();
    let marker = if outer.ordered {
        let number = parent.marker.number().unwrap_or(1);
        Marker::Ordered {
            number: match outer_style {
                Style::Same => number,
                Style::Sequential => number.saturating_add(1).min(MAX_NUMBER),
            },
            delimiter: outer.delimiter,
        }
    } else {
        Marker::Bullet(outer.bullet)
    };
    let Some(region) = ctx.region_start(xi - 2) else {
        return false;
    };
    let delta = parent.indent as isize - (parent.content_offset + item.indent) as isize;
    let widen = marker.width() as isize - item.marker.width() as isize;
    let item_last = last_line(doc, &item_range);
    let parent_last = last_line(doc, &parent_range);
    let rest =
        (parent.indent + marker.width() + item.padding) as isize - parent.content_offset as isize;
    let mut edits = Vec::new();
    edits.extend(replace_in_prefix(
        doc,
        &ctx.line,
        region..item.marker_range.end,
        &format!("{}{}", spaces(parent.indent), marker.text()),
    ));
    edits.extend(shift_lines(
        doc,
        marker_line + 1..item_last + 1,
        (&parent_range, Anchor::Item),
        delta + widen,
    ));
    edits.extend(shift_lines(
        doc,
        item_last + 1..parent_last + 1,
        (&parent_range, Anchor::Item),
        rest,
    ));
    if doc.apply(edits).is_none() {
        return false;
    }
    if outer.ordered {
        renumber_list_of_item(doc, region, outer_style);
    }
    true
}

// ----- block quotes ----------------------------------------------------------

/// The lines of the leaf that starts on `first`, with their contexts.
fn leaf_lines(doc: &Doc, first: usize) -> Vec<LineCtx> {
    let ctx = line_ctx(doc, first);
    let last = ctx.leaf.as_ref().map_or(first, |leaf| leaf.last_line);
    let mut out = vec![ctx];
    out.extend((first + 1..=last).map(|index| line_ctx(doc, index)));
    out
}

/// Adds a block quote level inside the innermost quote of the leaf that
/// starts on line `first`.
fn nest_quote(doc: &mut Doc, first: usize) {
    let mut edits = Vec::new();
    for ctx in leaf_lines(doc, first) {
        let Some(qi) = ctx.innermost_quote() else {
            continue;
        };
        if let Some(part) = ctx.parts.get(qi).cloned().flatten() {
            edits.push((part.end..part.end, "> ".to_owned()));
        }
    }
    doc.apply(edits);
}

/// Removes the innermost block quote level from the lines of the leaf that
/// starts on line `first`.
pub(crate) fn unnest_quote(doc: &mut Doc, first: usize) {
    let mut edits = Vec::new();
    let Some(quote) = line_ctx(doc, first)
        .innermost_quote()
        .map(|qi| (qi, line_ctx(doc, first).chain[qi].range.clone()))
    else {
        return;
    };
    for ctx in leaf_lines(doc, first) {
        let Some(k) = ctx.chain.iter().position(|level| level.range == quote.1) else {
            continue;
        };
        if let Some(part) = ctx.parts.get(k).cloned().flatten() {
            edits.push((part, String::new()));
        }
    }
    doc.apply(edits);
}

// ----- plain indentation -------------------------------------------------------

/// Inserts the indent unit at selection `i` (a selection within one line is
/// replaced), or at the start of every non-blank line it covers.
fn insert_indent(doc: &mut Doc, settings: &CommandSettings, i: usize) {
    let unit = match settings.indent {
        IndentUnit::Tab => "\t".to_owned(),
        IndentUnit::Spaces(n) => spaces(usize::from(n.clamp(1, 16))),
    };
    let spec = doc.sels[i];
    let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
    if doc.line_of(s) == doc.line_of(e) {
        replace_and_place(doc, i, s..e, unit);
        return;
    }
    let lines = selected_lines(doc, spec);
    let edits: Vec<(Range<usize>, String)> = lines
        .map(|index| line_ctx(doc, index))
        .filter(|ctx| !ctx.blank)
        .map(|ctx| (ctx.content_start..ctx.content_start, unit.clone()))
        .collect();
    doc.apply(edits);
}

/// Removes up to one indent unit of whitespace after the container
/// prefixes of every line selection `i` covers.
fn remove_indent(doc: &mut Doc, settings: &CommandSettings, i: usize) {
    let width = match settings.indent {
        IndentUnit::Tab => 4,
        IndentUnit::Spaces(n) => usize::from(n.clamp(1, 16)),
    };
    let spec = doc.sels[i];
    let mut edits = Vec::new();
    for index in selected_lines(doc, spec) {
        let ctx = line_ctx(doc, index);
        let b = ctx.line.bytes();
        let at = ctx.line.rel(ctx.content_start);
        let mut end = at;
        let mut cols = 0;
        while end < b.len() && cols < width {
            match b[end] {
                b' ' => cols += 1,
                b'\t' => cols = width,
                _ => break,
            }
            end += 1;
        }
        if end > at {
            edits.push((ctx.line.start + at..ctx.line.start + end, String::new()));
        }
    }
    doc.apply(edits);
}

// ----- tasks -----------------------------------------------------------------

/// Toggles the task box of every list item the selections cover; adds one
/// to items without; turns paragraph and blank lines into task items.
pub(crate) fn toggle_task(doc: &mut Doc) {
    let mut edits = Vec::new();
    let mut seen = BTreeSet::new();
    for spec in doc.sels.clone() {
        for index in selected_lines(doc, spec) {
            let ctx = line_ctx(doc, index);
            if ctx.in_verbatim() {
                continue;
            }
            if let Some(ii) = list_level(&ctx) {
                let Some(item) = ctx.chain[ii].item() else {
                    continue;
                };
                if !seen.insert(item.marker_line) {
                    continue;
                }
                match task_box(doc, item) {
                    Some(task) => {
                        let status = if task.unchecked { "x" } else { " " };
                        edits.push((task.status, status.to_owned()));
                    }
                    None if item.content_start == item.marker_range.end => {
                        let at = item.marker_range.end;
                        edits.push((at..at, " [ ] ".to_owned()));
                    }
                    None => {
                        let at = item.content_start;
                        edits.push((at..at, "[ ] ".to_owned()));
                    }
                }
                continue;
            }
            let paragraph = ctx
                .leaf
                .as_ref()
                .is_some_and(|leaf| leaf.kind == gv_syntax::LeafKind::Paragraph);
            if ctx.leaf.is_none() && ctx.blank && seen.insert(index) {
                let (range, text, _) = blank_line_insert(&ctx, "- [ ] ");
                edits.push((range, text));
                continue;
            }
            if paragraph && seen.insert(index) {
                let b = ctx.line.bytes();
                let mut at = ctx.line.rel(ctx.content_start);
                while at < b.len() && (b[at] == b' ' || b[at] == b'\t') {
                    at += 1;
                }
                let at = ctx.line.start + at;
                edits.push((at..at, "- [ ] ".to_owned()));
            }
        }
    }
    doc.apply(edits);
}

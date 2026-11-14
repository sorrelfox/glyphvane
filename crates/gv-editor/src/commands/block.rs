// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Block formatting: heading levels and block quotes.

use std::collections::BTreeSet;
use std::ops::Range;

use gv_syntax::{LeafKind, TokenKind};

use super::context::{LeafInfo, LineCtx, line_ctx};
use super::doc::Doc;
use super::list::{blank_line_insert, continuation_prefix, selected_lines};

/// Where a backslash keeps `content`, at the start of a line, from
/// starting a block (a list item, quote, heading, fence, thematic break or
/// setext underline): the byte offset to insert it at.
pub(crate) fn block_start_escape(content: &str) -> Option<usize> {
    let b = content.as_bytes();
    let first = *b.first()?;
    let space_or_end = |i: usize| matches!(b.get(i), None | Some(b' ' | b'\t'));
    let hashes = b.iter().take_while(|&&c| c == b'#').count();
    if (1..=6).contains(&hashes) && space_or_end(hashes) {
        return Some(0);
    }
    if first == b'>' || b.starts_with(b"```") || b.starts_with(b"~~~") {
        return Some(0);
    }
    if matches!(first, b'-' | b'+' | b'*') && space_or_end(1) {
        return Some(0);
    }
    let only = |c: u8| b.iter().all(|&x| x == c || x == b' ' || x == b'\t');
    if matches!(first, b'-' | b'*' | b'_')
        && only(first)
        && b.iter().filter(|&&x| x == first).count() >= 3
    {
        return Some(0);
    }
    if first == b'=' && only(b'=') {
        return Some(0);
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if (1..=9).contains(&digits)
        && matches!(b.get(digits), Some(b'.' | b')'))
        && space_or_end(digits + 1)
    {
        return Some(digits);
    }
    None
}

/// The content range of `leaf` on line `index`.
fn content_on(leaf: &LeafInfo, index: usize) -> Option<Range<usize>> {
    leaf.lines
        .iter()
        .find(|line| line.line == index && line.has_content)
        .map(|line| line.content.clone())
}

/// Sets every line the selections cover to an ATX heading of `level`, or
/// to a paragraph for level 0.
pub(crate) fn set_heading(doc: &mut Doc, level: u8) {
    let hashes = "#".repeat(usize::from(level.min(6)));
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    // Paragraph lines made headings, with the paragraph's last line.
    let mut converted: Vec<(usize, usize)> = Vec::new();
    for spec in doc.sels.clone() {
        let lines = selected_lines(doc, spec);
        let single = lines.len() == 1;
        for index in lines {
            let ctx = line_ctx(doc, index);
            let key = ctx.leaf.as_ref().map_or(index, |leaf| leaf.first_line);
            let Some(leaf) = ctx.leaf.clone() else {
                if level > 0 && single && ctx.blank && !ctx.lazy && seen.insert(index) {
                    let (range, text, _) = blank_line_insert(&ctx, &format!("{hashes} "));
                    let range = range.start..range.end.max(ctx.line.end());
                    edits.push((range, text));
                }
                continue;
            };
            match leaf.kind {
                LeafKind::Paragraph if level > 0 && seen.insert(index) => {
                    edits.extend(paragraph_line(&ctx, &leaf, &hashes));
                    converted.push((index, leaf.last_line));
                }
                LeafKind::Heading { setext: false, .. } if seen.insert(index) => {
                    edits.extend(atx_line(doc, &ctx, &leaf, level));
                }
                LeafKind::Heading {
                    setext: true,
                    level: current,
                } if seen.insert(key) && level != current => {
                    edits.extend(setext(doc, &leaf, level));
                }
                _ => {}
            }
        }
    }
    // The rest of a paragraph after a line made a heading is a paragraph of
    // its own: its first line is escaped if it would now start a block (it
    // was continuation text), and its lazy lines get their container
    // prefixes, so it stays text in its containers.
    for (line, last) in converted {
        for index in line + 1..=last {
            let ctx = line_ctx(doc, index);
            if ctx.blank || !seen.insert(index) {
                break;
            }
            if ctx.lazy {
                let prefix = continuation_prefix(&ctx, ctx.chain.len());
                edits.push((ctx.line.start..ctx.content_start, prefix));
            }
            let content = ctx.leaf.as_ref().and_then(|leaf| content_on(leaf, index));
            if index == line + 1
                && let Some(content) = content
            {
                // Leading whitespace no longer belongs to a continuation
                // line: four columns of it would make indented code.
                if !ctx.lazy && ctx.content_start < content.start {
                    edits.push((ctx.content_start..content.start, String::new()));
                }
                if let Some(k) = block_start_escape(&doc.slice(content.clone())) {
                    let at = content.start + k;
                    edits.push((at..at, "\\".to_owned()));
                }
            }
        }
    }
    doc.apply(edits);
}

/// A paragraph line turned into a heading: the whitespace before its text
/// replaced with the `#` run (and the missing container prefixes of a lazy
/// line added, so the heading stays in its containers).
fn paragraph_line(ctx: &LineCtx, leaf: &LeafInfo, hashes: &str) -> Option<(Range<usize>, String)> {
    let content = content_on(leaf, ctx.line.index)?;
    if ctx.lazy {
        let prefix = continuation_prefix(ctx, ctx.chain.len());
        return Some((ctx.line.start..content.start, format!("{prefix}{hashes} ")));
    }
    Some((ctx.content_start..content.start, format!("{hashes} ")))
}

/// An ATX heading line set to `level` (0: a paragraph, escaped if its text
/// would start another block).
fn atx_line(doc: &Doc, ctx: &LineCtx, leaf: &LeafInfo, level: u8) -> Vec<(Range<usize>, String)> {
    let tokens = doc
        .tree()
        .tokens(ctx.line.start..ctx.line.full_end().max(ctx.line.start + 1));
    let markers: Vec<Range<usize>> = tokens
        .iter()
        .filter(|(range, token)| {
            token.kind == TokenKind::Marker && range.start >= ctx.content_start
        })
        .map(|(range, _)| range.clone())
        .collect();
    let Some(open) = markers.first().cloned() else {
        return Vec::new();
    };
    if level > 0 {
        if open.len() == usize::from(level) {
            return Vec::new();
        }
        return vec![(open, "#".repeat(usize::from(level.min(6))))];
    }
    let Some(content) = content_on(leaf, ctx.line.index) else {
        return vec![(ctx.content_start..ctx.line.end(), String::new())];
    };
    let mut edits = Vec::new();
    let text = doc.slice(content.clone());
    let escape = if block_start_escape(&text) == Some(0) {
        "\\"
    } else {
        ""
    };
    edits.push((ctx.content_start..content.start, escape.to_owned()));
    if let Some(k) = block_start_escape(&text).filter(|&k| k > 0) {
        edits.push((content.start + k..content.start + k, "\\".to_owned()));
    }
    if markers.len() > 1
        && markers
            .last()
            .is_some_and(|close| close.start >= content.end)
    {
        edits.push((content.end..ctx.line.end(), String::new()));
    }
    edits
}

/// A setext heading rewritten: as an ATX heading of `level` (its lines
/// joined), or as a paragraph for level 0 (the underline removed, or
/// emptied when a line follows that would otherwise join the paragraph).
fn setext(doc: &Doc, leaf: &LeafInfo, level: u8) -> Vec<(Range<usize>, String)> {
    let underline = doc.line(leaf.last_line);
    let Some(last_text) = leaf.lines.iter().rfind(|line| line.has_content) else {
        return Vec::new();
    };
    if level == 0 {
        let next_blank =
            leaf.last_line + 1 >= doc.line_count() || line_ctx(doc, leaf.last_line + 1).blank;
        if next_blank {
            return vec![(last_text.content.end..underline.end(), String::new())];
        }
        let uctx = line_ctx(doc, leaf.last_line);
        return vec![(uctx.content_start..underline.end(), String::new())];
    }
    let first = line_ctx(doc, leaf.first_line);
    let joined: Vec<String> = leaf
        .lines
        .iter()
        .filter(|line| line.has_content)
        .map(|line| doc.slice(line.content.clone()).trim().to_owned())
        .collect();
    vec![(
        first.content_start..underline.end(),
        format!(
            "{} {}",
            "#".repeat(usize::from(level.min(6))),
            joined.join(" ")
        ),
    )]
}

/// Toggles a block quote around the blocks every selection covers: removes
/// the innermost quote level if they are directly in a quote, else adds one
/// inside their innermost container.
pub(crate) fn toggle_quote(doc: &mut Doc) {
    let mut done = BTreeSet::new();
    for i in 0..doc.sels.len() {
        let lines: Vec<LineCtx> = selected_lines(doc, doc.sels[i])
            .map(|index| line_ctx(doc, index))
            .collect();
        let Some(first) = lines.first() else {
            continue;
        };
        let quoted = first
            .innermost_quote()
            .filter(|&qi| qi + 1 == first.chain.len())
            .map(|qi| first.chain[qi].range.clone());
        // Every line of the leaves the selection touches, and the
        // selection's other lines.
        let mut indices: BTreeSet<usize> = BTreeSet::new();
        for ctx in &lines {
            match &ctx.leaf {
                Some(leaf) => indices.extend(leaf.first_line..=leaf.last_line),
                None => {
                    indices.insert(ctx.line.index);
                }
            }
        }
        indices.retain(|index| !done.contains(index));
        done.extend(indices.iter().copied());
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for index in indices {
            let ctx = line_ctx(doc, index);
            if ctx
                .leaf
                .as_ref()
                .is_some_and(|leaf| matches!(leaf.kind, LeafKind::FrontMatter { .. }))
            {
                continue;
            }
            match &quoted {
                Some(quote) => {
                    let Some(k) = ctx.chain.iter().position(|level| level.range == *quote) else {
                        continue;
                    };
                    if let Some(part) = ctx.parts.get(k).cloned().flatten() {
                        edits.push((part, String::new()));
                    }
                }
                None if ctx.lazy => {}
                None if ctx.blank && ctx.leaf.is_none() => {
                    let text = if index == first.line.index { "> " } else { ">" };
                    let (range, text, _) = blank_line_insert(&ctx, text);
                    edits.push((range, text));
                }
                None => {
                    let at = ctx.content_start;
                    edits.push((at..at, "> ".to_owned()));
                }
            }
        }
        doc.apply(edits);
    }
}

#[cfg(test)]
mod tests {
    use super::block_start_escape;

    #[test]
    fn escapes() {
        assert_eq!(block_start_escape("- a"), Some(0));
        assert_eq!(block_start_escape("-a"), None);
        assert_eq!(block_start_escape("## a"), Some(0));
        assert_eq!(block_start_escape("#tag"), None);
        assert_eq!(block_start_escape("12. a"), Some(2));
        assert_eq!(block_start_escape("12.5"), None);
        assert_eq!(block_start_escape("***"), Some(0));
        assert_eq!(block_start_escape("*a*"), None);
        assert_eq!(block_start_escape("==="), Some(0));
        assert_eq!(block_start_escape("> q"), Some(0));
        assert_eq!(block_start_escape("text"), None);
    }
}

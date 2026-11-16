// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Footnotes: insert a footnote, and go between a reference and its
//! definition.
//!
//! The parser does not read footnotes yet, so they are found in the text:
//! `[^label]` (a label without whitespace) is a reference, and a definition
//! when it starts a line's content (after block-quote markers and
//! indentation) followed by `:`. Lines of code, HTML, math and front-matter
//! blocks are skipped. Labels match ignoring case, as GFM does.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use gv_syntax::LeafKind;

use super::context::{blocks_in, ctx_at};
use super::doc::Doc;
use super::settings::FootnotePlacement;
use crate::selection::SelectionSpec;

/// A `[^label]` in the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Occurrence {
    /// The label as written.
    pub label: String,
    /// `[^label]`.
    pub range: Range<usize>,
    /// Whether it is a definition (`[^label]:` at the start of a line).
    pub definition: bool,
}

/// The lines (first, last) of the blocks whose text is not Markdown.
fn verbatim_lines(doc: &Doc) -> Vec<(usize, usize)> {
    let (_, leaves) = blocks_in(doc, 0..doc.len());
    leaves
        .into_iter()
        .filter(|leaf| {
            leaf.is_verbatim() || matches!(leaf.kind, LeafKind::MathBlock | LeafKind::IndentedCode)
        })
        .map(|leaf| (leaf.first_line, leaf.last_line))
        .collect()
}

/// The `[^label]`s of line `text` (starting at `start`).
fn scan_line(text: &str, start: usize, out: &mut Vec<Occurrence>) {
    let mut from = 0;
    while let Some(found) = text[from..].find("[^") {
        let open = from + found;
        let Some(close) = text[open + 2..].find(']') else {
            break;
        };
        let end = open + 2 + close + 1;
        let label = &text[open + 2..end - 1];
        if label.is_empty() || label.contains(char::is_whitespace) || label.contains('[') {
            from = open + 2;
            continue;
        }
        let lead = text[..open].trim_start_matches([' ', '\t', '>']).is_empty();
        out.push(Occurrence {
            label: label.to_owned(),
            range: start + open..start + end,
            definition: lead && text[end..].starts_with(':'),
        });
        from = end;
    }
}

/// Every footnote reference and definition, in order.
pub(crate) fn occurrences(doc: &Doc) -> Vec<Occurrence> {
    let skip = verbatim_lines(doc);
    let mut out = Vec::new();
    for index in 0..doc.line_count() {
        if skip.iter().any(|&(a, b)| a <= index && index <= b) {
            continue;
        }
        let line = doc.line(index);
        if line.text.contains("[^") {
            scan_line(&line.text, line.start, &mut out);
        }
    }
    out
}

/// Go to footnote from the caret at `offset`: from a reference to its
/// definition, from a definition (its label or the rest of its first line)
/// to the first reference. The target's
/// `[^label]` range.
pub(crate) fn target(doc: &Doc, offset: usize) -> Option<Range<usize>> {
    let all = occurrences(doc);
    let line = doc.line_of(offset);
    let here = all
        .iter()
        .find(|o| o.range.start <= offset && offset <= o.range.end)
        .or_else(|| {
            // Anywhere in a definition's first line.
            all.iter()
                .find(|o| o.definition && doc.line_of(o.range.start) == line)
        })?;
    let label = here.label.to_lowercase();
    all.iter()
        .find(|o| o.definition != here.definition && o.label.to_lowercase() == label)
        .map(|o| o.range.clone())
}

/// The next free numeric label: one more than the largest number in use,
/// skipping labels taken.
fn next_label(used: &HashSet<String>) -> u64 {
    let max = used
        .iter()
        .filter_map(|label| label.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    let mut next = max.saturating_add(1);
    while used.contains(&next.to_string()) {
        next = next.saturating_add(1);
    }
    next
}

/// Whether line `index` is blank (or past the end).
fn blank_line(doc: &Doc, index: usize) -> bool {
    index >= doc.line_count() || doc.line(index).text.trim().is_empty()
}

/// Insert footnote at every selection: a reference `[^n]` after the
/// selection (the next free number, in document order), and its
/// definition `[^n]: ` at the end of the document or after the top-level
/// block the caret is in. The carets go to the definitions.
pub(crate) fn insert_footnote(doc: &mut Doc, placement: FootnotePlacement) {
    let mut used: HashSet<String> = occurrences(doc)
        .into_iter()
        .map(|o| o.label.to_lowercase())
        .collect();
    let mut points: Vec<usize> = doc
        .sels
        .iter()
        .map(|spec| spec.tail.max(spec.head))
        .collect();
    points.sort_unstable();
    points.dedup();
    // Insertions by offset: references first, then definitions; and where
    // each caret goes (offset of the insertion, end within its text).
    let mut inserts: BTreeMap<usize, String> = BTreeMap::new();
    let mut carets: Vec<(usize, usize)> = Vec::new();
    let len = doc.len();
    let text = doc.slice(0..len);
    let last = doc.line(doc.line_count().saturating_sub(1));
    let doc_eol = doc.line_ending_for(&last);
    let mut definitions: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for &point in &points {
        let label = next_label(&used).to_string();
        used.insert(label.clone());
        inserts
            .entry(point)
            .or_default()
            .push_str(&format!("[^{label}]"));
        let at = match placement {
            FootnotePlacement::EndOfDocument => len,
            FootnotePlacement::AfterParagraph => {
                let ctx = ctx_at(doc, point);
                let end = ctx
                    .chain
                    .first()
                    .map(|level| level.range.end)
                    .or(ctx.leaf.as_ref().map(|leaf| leaf.range.end))
                    .unwrap_or(ctx.line.end());
                let index = doc
                    .line_of(end.saturating_sub(1).max(ctx.line.start))
                    .max(ctx.line.index);
                doc.line(index).end()
            }
        };
        definitions.entry(at).or_default().push(label);
    }
    for (at, labels) in definitions {
        let line = doc.line_at(at);
        let eol = doc.line_ending_for(&line);
        let mut block = String::new();
        let at_end = at == len;
        if at_end && placement == FootnotePlacement::EndOfDocument {
            // One line break after a definition, a blank line after other
            // text; a reference just inserted at the end is other text.
            let trimmed = text.trim_end_matches(['\r', '\n']);
            let last_line = trimmed.rsplit('\n').next().unwrap_or("");
            let (breaks, after_definition) = if inserts.contains_key(&len) {
                (0, false)
            } else {
                let breaks = text[trimmed.len()..].matches('\n').count();
                let definition =
                    last_line.trim_start().starts_with("[^") && last_line.contains("]:");
                (breaks, definition)
            };
            if !text.is_empty() || inserts.contains_key(&len) {
                let needed: usize = if after_definition { 1 } else { 2 };
                for _ in breaks..needed {
                    block.push_str(doc_eol);
                }
            }
        } else {
            block.push_str(eol);
            block.push_str(eol);
        }
        let ends_with_eol = at_end && text.ends_with('\n');
        let entry = inserts.entry(at).or_default();
        let base = entry.len();
        for (k, label) in labels.iter().enumerate() {
            if k > 0 {
                block.push_str(eol);
            }
            block.push_str(&format!("[^{label}]: "));
            carets.push((at, base + block.len()));
        }
        if ends_with_eol {
            block.push_str(doc_eol);
        } else if !at_end && !blank_line(doc, line.index + 1) {
            block.push_str(eol);
        }
        entry.push_str(&block);
    }
    let mut shift: BTreeMap<usize, usize> = BTreeMap::new();
    let mut delta = 0;
    for (&at, text) in &inserts {
        shift.insert(at, delta);
        delta += text.len();
    }
    let specs: Vec<SelectionSpec> = carets
        .into_iter()
        .map(|(at, end)| SelectionSpec::caret(at + shift[&at] + end))
        .collect();
    let edits = inserts
        .into_iter()
        .map(|(at, text)| (at..at, text))
        .collect();
    if doc.apply(edits).is_some() {
        doc.sels = specs;
        doc.primary = doc.sels.len() - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let used: HashSet<String> = ["1", "2", "note", "4"].map(String::from).into();
        assert_eq!(next_label(&used), 5);
        assert_eq!(next_label(&HashSet::new()), 1);
        let mut out = Vec::new();
        scan_line("> [^a]: x [^b] [^ c] [^]", 10, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out[0].definition);
        assert_eq!(out[0].range, 12..16);
        assert!(!out[1].definition);
    }
}

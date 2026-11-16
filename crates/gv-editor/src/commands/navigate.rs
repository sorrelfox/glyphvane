// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the Go commands need from a document: its headings (Go to
//! heading, sections for Expand selection), line targets (Go to line) and
//! footnote references and definitions (Go to footnote).

use std::ops::Range;

use gv_syntax::{Leaf, LeafKind, Visitor};
use gv_text::{Bias, TextSnapshot};

use super::doc::Doc;

/// A heading of the document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadingEntry {
    /// 1 to 6.
    pub level: u8,
    /// Its text, as written (inline syntax kept), lines joined by spaces.
    pub text: String,
    /// The start of its first line.
    pub offset: usize,
    /// Its 0-based first line.
    pub line: usize,
    /// Where its text starts (after the `#` marker).
    pub text_start: usize,
}

struct Headings {
    out: Vec<(u8, Range<usize>, Vec<Range<usize>>)>,
}

impl Visitor for Headings {
    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if let LeafKind::Heading { level, .. } = leaf.kind {
            let content: Vec<Range<usize>> = leaf
                .lines
                .iter()
                .filter(|line| line.has_content)
                .map(|line| line.content.clone())
                .collect();
            self.out.push((level, leaf.range.clone(), content));
        }
    }
}

/// The headings of `doc`, in order.
pub(crate) fn headings(doc: &Doc) -> Vec<HeadingEntry> {
    let mut visitor = Headings { out: Vec::new() };
    doc.tree().walk(&mut visitor);
    visitor
        .out
        .into_iter()
        .map(|(level, range, content)| {
            let text = content
                .iter()
                .map(|r| doc.slice(r.clone()).trim().to_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let first = content.first().map_or(range.start, |r| r.start);
            let line = doc.line_of(first);
            HeadingEntry {
                level,
                text,
                offset: doc.line(line).start,
                line,
                text_start: first,
            }
        })
        .collect()
}

/// The sections of `doc` around `offset`, innermost first: each from its
/// heading line to the next heading of the same or a higher level.
pub(crate) fn sections_at(doc: &Doc, offset: usize) -> Vec<Range<usize>> {
    let headings = headings(doc);
    let mut out = Vec::new();
    for (k, heading) in headings.iter().enumerate() {
        if heading.offset > offset {
            break;
        }
        let end = headings[k + 1..]
            .iter()
            .find(|h| h.level <= heading.level)
            .map_or(doc.len(), |h| h.offset);
        if offset <= end {
            out.push(heading.offset..end);
        }
    }
    out.sort_by_key(|range| range.len());
    out
}

/// A "line" or "line:column" typed in Go to line (1-based; spaces allowed
/// around the parts, `,` also separates the column): the 1-based line and
/// column. `None` if it is not one.
pub fn parse_line_target(input: &str) -> Option<(usize, Option<usize>)> {
    let input = input.trim();
    let (line, column) = match input.split_once([':', ',']) {
        Some((line, column)) => (line.trim(), Some(column.trim())),
        None => (input, None),
    };
    let line: usize = line.parse().ok()?;
    let column = match column {
        Some("") | None => None,
        Some(column) => Some(column.parse::<usize>().ok()?),
    };
    Some((line, column))
}

/// The offset of 1-based `line` and `column` in `text`: a line past the
/// end is the last line, line 0 the first, a column past the end of the
/// line its end (columns count characters).
pub fn line_target_offset(text: &TextSnapshot, line: usize, column: Option<usize>) -> usize {
    let count = text.line_count().max(1);
    let index = line.clamp(1, count) - 1;
    let Ok(info) = text.line_info(index) else {
        return text.len();
    };
    let content = info.content;
    let Some(column) = column.filter(|&c| c > 1) else {
        return content.start;
    };
    let line_text = text.text_for_range(content.clone()).unwrap_or_default();
    let rel = line_text
        .char_indices()
        .nth(column - 1)
        .map_or(line_text.len(), |(i, _)| i);
    text.clip_offset(content.start + rel, Bias::Left)
}

/// The footnote label (`[^label]`) at `offset` on its line: the label's
/// range (without brackets) and whether it is a definition (`[^label]:`
/// at the start of the line's content).
fn footnote_at(doc: &Doc, offset: usize) -> Option<(String, bool)> {
    let line = doc.line_at(offset);
    let rel = line.rel(offset);
    let text = &line.text;
    let mut from = 0;
    while let Some(found) = text[from..].find("[^") {
        let start = from + found;
        let Some(close) = text[start + 2..].find(']') else {
            break;
        };
        let end = start + 2 + close + 1;
        let label = &text[start + 2..end - 1];
        if !label.is_empty() && !label.contains(char::is_whitespace) && start <= rel && rel <= end {
            let lead = text[..start]
                .trim_start_matches([' ', '\t', '>'])
                .is_empty();
            let definition = lead && text[end..].starts_with(':');
            return Some((label.to_owned(), definition));
        }
        from = end;
    }
    None
}

/// Where Go to footnote goes from the caret at `offset`: from a reference
/// to its definition, from a definition to the first reference. The
/// target's `[^label]` range.
pub(crate) fn footnote_target(doc: &Doc, offset: usize) -> Option<Range<usize>> {
    let (label, definition) = footnote_at(doc, offset)?;
    let needle = format!("[^{label}]");
    let text = doc.slice(0..doc.len());
    let mut at = 0;
    while let Some(found) = text[at..].find(&needle) {
        let start = at + found;
        let end = start + needle.len();
        let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
        let lead = text[line_start..start]
            .trim_start_matches([' ', '\t', '>', '\r'])
            .is_empty();
        let is_definition = lead && text[end..].starts_with(':');
        if is_definition != definition {
            return Some(start..end);
        }
        at = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_targets() {
        assert_eq!(parse_line_target("12"), Some((12, None)));
        assert_eq!(parse_line_target(" 3:4 "), Some((3, Some(4))));
        assert_eq!(parse_line_target("3,4"), Some((3, Some(4))));
        assert_eq!(parse_line_target("3:"), Some((3, None)));
        assert_eq!(parse_line_target("x"), None);
        assert_eq!(parse_line_target(""), None);
        let text = gv_text::Buffer::new("ab\ncdé\n").snapshot();
        assert_eq!(line_target_offset(&text, 2, None), 3);
        assert_eq!(line_target_offset(&text, 2, Some(3)), 5);
        assert_eq!(line_target_offset(&text, 2, Some(9)), 7);
        assert_eq!(line_target_offset(&text, 99, None), 8);
        assert_eq!(line_target_offset(&text, 0, None), 0);
    }
}

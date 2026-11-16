// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Insert callout: `> [!type]` around the blocks at the selections.
//!
//! Obsidian has callouts (13 types, written in lower case); GitHub and
//! Extended have alerts (5 types, upper case). CommonMark has neither.
//!
//! - A caret on a blank line inserts a callout header and an empty body
//!   line, with the caret in the body.
//! - Otherwise the blocks the selection covers are quoted (as
//!   [`Quote`](super::MarkdownCommand::Quote) does) under a new header line
//!   at the same container level.
//! - A caret or selection already in a callout or alert changes its type.

use std::ops::Range;

use crate::selection::SelectionSpec;

use super::context::{LineCtx, line_ctx};
use super::doc::Doc;
use super::flavor::Flavor;
use super::list::{blank_line_insert, continuation_prefix, selected_lines};

/// Obsidian's callout types (the main name of each).
pub const OBSIDIAN_CALLOUTS: [&str; 13] = [
    "note", "abstract", "info", "todo", "tip", "success", "question", "warning", "failure",
    "danger", "bug", "example", "quote",
];

/// GitHub's alert types.
pub const ALERTS: [&str; 5] = ["NOTE", "TIP", "IMPORTANT", "WARNING", "CAUTION"];

/// The callout or alert types of `flavor`, in menu order (empty when it has
/// neither).
pub fn callout_types(flavor: Flavor) -> &'static [&'static str] {
    match flavor {
        Flavor::Obsidian => &OBSIDIAN_CALLOUTS,
        Flavor::GfmFile | Flavor::GfmComment | Flavor::Extended => &ALERTS,
        Flavor::CommonMark => &[],
    }
}

/// The `[!type]` marker at the start of `text`: its range in `text`.
fn marker(text: &str) -> Option<Range<usize>> {
    let rest = text.strip_prefix("[!")?;
    let end = rest.find(']')?;
    let name = &rest[..end];
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_'))
    .then_some(0..end + 3)
}

/// The type name range of the callout the line of `ctx` is in, if its
/// innermost quote starts with a `[!type]` marker.
fn existing(doc: &Doc, ctx: &LineCtx) -> Option<Range<usize>> {
    let qi = ctx.innermost_quote()?;
    let quote = &ctx.chain[qi];
    let first = line_ctx(doc, doc.line_of(quote.range.start));
    let rest = &first.line.text[first.line.rel(first.content_start)..];
    let found = marker(rest)?;
    let at = first.content_start + found.start;
    Some(at + 2..at + found.end - 1)
}

/// Inserts a callout of type `kind` at every selection.
pub(crate) fn insert_callout(doc: &mut Doc, kind: &str) {
    let mut done: Vec<usize> = Vec::new();
    for i in 0..doc.sels.len() {
        let spec = doc.sels[i];
        let lines = selected_lines(doc, spec);
        if lines.clone().any(|line| done.contains(&line)) {
            continue;
        }
        done.extend(lines.clone());
        let ctx = line_ctx(doc, lines.start);
        if let Some(name) = existing(doc, &ctx) {
            doc.apply(vec![(name, kind.to_owned())]);
            continue;
        }
        let header = format!("[!{kind}]");
        if spec.tail == spec.head && ctx.blank && ctx.leaf.is_none() {
            let ending = doc.line_ending_for(&ctx.line);
            let (range, prefix_and, at) = blank_line_insert(&ctx, "");
            let prefix_before = &prefix_and[..at];
            let next_prefix = continuation_prefix(&ctx, ctx.chain.len());
            let text = format!("{prefix_before}> {header}{ending}{next_prefix}> ");
            let caret = range.start + text.len();
            if doc.apply(vec![(range, text)]).is_some() {
                doc.sels[i] = SelectionSpec::caret(caret);
            }
            continue;
        }
        // Quote the lines (whole leaves), then put the header above.
        let mut indices: Vec<usize> = Vec::new();
        for index in lines.clone() {
            let ctx = line_ctx(doc, index);
            match &ctx.leaf {
                Some(leaf) => indices.extend(leaf.first_line..=leaf.last_line),
                None => indices.push(index),
            }
        }
        indices.sort_unstable();
        indices.dedup();
        done.extend(indices.iter().copied());
        let Some(&top) = indices.first() else {
            continue;
        };
        let top_ctx = line_ctx(doc, top);
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for &index in &indices {
            let ctx = line_ctx(doc, index);
            if ctx.lazy && index != top {
                let prefix = continuation_prefix(&ctx, ctx.chain.len());
                edits.push((ctx.line.start..ctx.content_start, format!("{prefix}> ")));
            } else if ctx.blank && ctx.leaf.is_none() {
                let (range, text, _) = blank_line_insert(&ctx, ">");
                edits.push((range, text));
            } else if index != top {
                let at = ctx.content_start;
                edits.push((at..at, "> ".to_owned()));
            }
        }
        let ending = doc.line_ending_for(&top_ctx.line);
        let prefix = continuation_prefix(&top_ctx, top_ctx.chain.len());
        let at = top_ctx.content_start;
        let pad = top_ctx.pad();
        edits.push((at..at, format!("{pad}> {header}{ending}{prefix}> ")));
        doc.apply(edits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers() {
        assert_eq!(marker("[!note] Title"), Some(0..7));
        assert_eq!(marker("[!NOTE]"), Some(0..7));
        assert_eq!(marker("[note]"), None);
        assert_eq!(marker("[!] x"), None);
        assert!(callout_types(Flavor::CommonMark).is_empty());
        assert_eq!(callout_types(Flavor::Obsidian)[0], "note");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Inline formatting: emphasis, strikethrough, highlight and code spans
//! wrapped or unwrapped, and link insertion.
//!
//! Emphasis and code spans are found in the inline tree. Strikethrough and
//! highlight, which the parser does not read yet, are found by pairing
//! their delimiter runs in the leaf's text (outside code spans), with the
//! flanking rule that an opener is not followed by whitespace and a closer
//! not preceded by it.

use std::ops::Range;
use std::sync::Arc;

use gv_syntax::Leaf;
use gv_syntax::inline::InlineTree;
use gv_text::Bias;
use unicode_segmentation::UnicodeSegmentation;

use crate::selection::SelectionSpec;

use super::context::{LeafInfo, LineCtx, ctx_at};
use super::doc::{Doc, Line};
use super::list::blank_line_insert;
use super::settings::{CommandSettings, EmphasisStyle, LinkStyle};

/// An inline format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Span {
    /// Strong emphasis.
    Bold,
    /// Emphasis.
    Italic,
    /// `~~strikethrough~~`.
    Strike,
    /// `==highlight==`.
    Highlight,
    /// A code span.
    Code,
}

/// The inline tree of `leaf`.
pub(crate) fn inlines(doc: &Doc, leaf: &LeafInfo) -> Arc<InlineTree> {
    let view = Leaf {
        id: leaf.id,
        kind: leaf.kind,
        lines: &leaf.lines,
        info: None,
        open_line: leaf.first_line,
        start_line: leaf.first_line,
        end_line: leaf.last_line,
        range: leaf.range.clone(),
    };
    doc.tree().inlines(&view, doc.text())
}

/// Whether `offset` lies strictly inside a code span of `tree`.
pub(crate) fn in_code_span(tree: &InlineTree, offset: usize) -> bool {
    tree.code_spans()
        .any(|span| span.range.start < offset && offset < span.range.end)
}

/// The word around `offset` on `line` (a caret at either end of a word is
/// in it).
fn word_at(line: &Line, offset: usize) -> Option<Range<usize>> {
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

/// `range` clamped to the content of `leaf` and trimmed of whitespace.
fn trimmed_in_leaf(doc: &Doc, leaf: &LeafInfo, range: Range<usize>) -> Range<usize> {
    let content = leaf
        .lines
        .iter()
        .filter(|line| line.has_content)
        .map(|line| line.content.clone());
    let (first, last) = {
        let mut content = content;
        let first = content.next();
        let last = content.next_back().or_else(|| first.clone());
        (first, last)
    };
    let (Some(first), Some(last)) = (first, last) else {
        return range.start..range.start;
    };
    let start = range.start.clamp(first.start, last.end);
    let end = range.end.clamp(start, last.end);
    let text = doc.slice(start..end);
    let lead = text.len() - text.trim_start().len();
    let trail = text.len() - text.trim_end().len();
    if lead + trail >= text.len() {
        return start..start;
    }
    start + lead..end - trail
}

/// The opening and closing delimiters of the innermost `span` of `tree`
/// around `s..e` (a caret strictly inside, a selection inside or equal).
fn find_span(
    doc: &Doc,
    leaf: &LeafInfo,
    tree: &InlineTree,
    span: Span,
    s: usize,
    e: usize,
) -> Option<(Range<usize>, Range<usize>)> {
    let around = |range: &Range<usize>| {
        if s == e {
            range.start < s && s < range.end
        } else {
            range.start <= s && e <= range.end
        }
    };
    match span {
        Span::Bold | Span::Italic => tree
            .emphasis()
            .filter(|emphasis| emphasis.strong == (span == Span::Bold) && around(&emphasis.range))
            .min_by_key(|emphasis| emphasis.range.len())
            .map(|emphasis| {
                (
                    emphasis.range.start..emphasis.content_range.start,
                    emphasis.content_range.end..emphasis.range.end,
                )
            }),
        Span::Code => tree
            .code_spans()
            .find(|code| around(&code.range))
            .map(|code| {
                let content = doc.slice(code.content_range.clone());
                let padded = content.len() >= 2
                    && content.starts_with(' ')
                    && content.ends_with(' ')
                    && !content.bytes().all(|b| b == b' ');
                let pad = usize::from(padded);
                (
                    code.range.start..code.content_range.start + pad,
                    code.content_range.end - pad..code.range.end,
                )
            }),
        Span::Strike | Span::Highlight => {
            let marker = if span == Span::Strike { b'~' } else { b'=' };
            if s < e {
                let before = doc.slice(s.saturating_sub(2)..s);
                let after = doc.slice(e..e + 2);
                let pair = [marker, marker];
                if before.as_bytes() == pair && after.as_bytes() == pair {
                    return Some((s - 2..s, e..e + 2));
                }
            }
            delimited_pairs(doc, leaf, tree, marker)
                .into_iter()
                .filter(|(open, close)| around(&(open.start..close.end)))
                .min_by_key(|(open, close)| close.end - open.start)
        }
    }
}

/// The pairs of two-character `marker` runs in `leaf`, outside code spans.
fn delimited_pairs(
    doc: &Doc,
    leaf: &LeafInfo,
    tree: &InlineTree,
    marker: u8,
) -> Vec<(Range<usize>, Range<usize>)> {
    let code: Vec<Range<usize>> = tree.code_spans().map(|code| code.range).collect();
    let mut pairs = Vec::new();
    let mut open: Option<Range<usize>> = None;
    for line in leaf.lines.iter().filter(|line| line.has_content) {
        let text = doc.slice(line.content.clone());
        let b = text.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] != marker {
                i += 1;
                continue;
            }
            let run = b[i..].iter().take_while(|&&c| c == marker).count();
            let at = line.content.start + i;
            let inside_code = code.iter().any(|range| range.start <= at && at < range.end);
            if run == 2 && !inside_code {
                let before = i.checked_sub(1).map(|k| b[k]);
                let after = b.get(i + 2).copied();
                let space = |c: Option<u8>| c.is_none_or(|c| c.is_ascii_whitespace());
                let range = at..at + 2;
                match open.take() {
                    Some(start) if !space(before) => pairs.push((start, range)),
                    Some(start) => open = Some(if space(after) { start } else { range }),
                    None if !space(after) => open = Some(range),
                    None => {}
                }
            }
            i += run;
        }
    }
    pairs
}

/// The delimiters that wrap `text` in `span`.
fn delimiters(
    span: Span,
    settings: &CommandSettings,
    text: &str,
    intraword: bool,
) -> (String, String) {
    let underscore = settings.emphasis == EmphasisStyle::Underscore && !intraword;
    let simple = |marker: &str| (marker.to_owned(), marker.to_owned());
    match span {
        Span::Bold if underscore => simple("__"),
        Span::Bold => simple("**"),
        Span::Italic if underscore => simple("_"),
        Span::Italic => simple("*"),
        Span::Strike => simple("~~"),
        Span::Highlight => simple("=="),
        Span::Code => {
            let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest + 1);
            let pad = text.starts_with('`')
                || text.ends_with('`')
                || (text.len() >= 2
                    && text.starts_with(' ')
                    && text.ends_with(' ')
                    && !text.bytes().all(|b| b == b' '));
            if pad {
                (format!("{fence} "), format!(" {fence}"))
            } else {
                (fence.clone(), fence)
            }
        }
    }
}

/// Whether the characters around `range` make it part of a word.
fn is_intraword(doc: &Doc, range: &Range<usize>) -> bool {
    let before = doc.slice(range.start.saturating_sub(4)..range.start);
    let after = doc.slice(range.end..range.end + 4);
    before.chars().last().is_some_and(char::is_alphanumeric)
        || after.chars().next().is_some_and(char::is_alphanumeric)
}

/// The edits of one step and the carets to place after it: selection
/// index, an offset of the old text, and a distance after its new place.
#[derive(Default)]
struct Plan {
    edits: Vec<(Range<usize>, String)>,
    claimed: Vec<Range<usize>>,
    carets: Vec<(usize, usize, usize)>,
    /// Wrapped words: their range and the opening delimiter's length, for
    /// other carets in the same word.
    wrapped: Vec<(Range<usize>, usize)>,
}

impl Plan {
    /// Claims `range` for one selection; false if another took it.
    fn claim(&mut self, range: Range<usize>) -> bool {
        let taken = self
            .claimed
            .iter()
            .any(|other| (other.start < range.end && range.start < other.end) || *other == range);
        if !taken {
            self.claimed.push(range);
        }
        !taken
    }

    fn apply(self, doc: &mut Doc) {
        if let Some(summary) = doc.apply(self.edits) {
            for (i, at, offset) in self.carets {
                let caret = summary.transform_offset(at, Bias::Left) + offset;
                doc.sels[i] = SelectionSpec::caret(caret);
            }
        }
    }
}

/// The inline leaf at `s` and its tree, or `None` (with the context) on a
/// line without one.
fn inline_context(doc: &Doc, s: usize) -> (LineCtx, Option<(LeafInfo, Arc<InlineTree>)>) {
    let ctx = ctx_at(doc, s);
    let leaf = ctx.leaf.clone().filter(LeafInfo::is_inline);
    let tree = leaf.map(|leaf| {
        let tree = inlines(doc, &leaf);
        (leaf, tree)
    });
    (ctx, tree)
}

/// `offset` moved into the leaf's content on its line (out of the
/// container prefixes and markers before it).
fn into_content(ctx: &LineCtx, leaf: &LeafInfo, offset: usize) -> usize {
    match leaf
        .lines
        .iter()
        .find(|line| line.line == ctx.line.index && line.has_content)
    {
        Some(line) => offset.clamp(line.content.start, line.content.end),
        None => offset.max(ctx.content_start),
    }
}

/// Wraps or unwraps every selection (or the word at a caret) in `span`.
pub(crate) fn toggle_span(doc: &mut Doc, span: Span, settings: &CommandSettings) {
    let mut plan = Plan::default();
    for (i, spec) in doc.sels.clone().into_iter().enumerate() {
        let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
        let (ctx, inline) = inline_context(doc, s);
        let Some((leaf, tree)) = inline else {
            if ctx.leaf.is_none()
                && ctx.blank
                && !ctx.in_verbatim()
                && doc.line_of(e) == ctx.line.index
            {
                let (open, close) = delimiters(span, settings, "", false);
                let (range, text, offset) = blank_line_insert(&ctx, &format!("{open}{close}"));
                if plan.claim(range.clone()) {
                    plan.carets.push((i, range.start, offset + open.len()));
                    plan.edits.push((range, text));
                }
            }
            continue;
        };
        let s = into_content(&ctx, &leaf, s);
        let e = e.max(s);
        if span != Span::Code && (in_code_span(&tree, s) || in_code_span(&tree, e)) {
            continue;
        }
        if let Some((open, close)) = find_span(doc, &leaf, &tree, span, s, e) {
            if plan.claim(open.start..close.end) {
                plan.edits.push((open, String::new()));
                plan.edits.push((close, String::new()));
            }
            continue;
        }
        let target = if s == e {
            word_at(&ctx.line, s).unwrap_or(s..s)
        } else {
            trimmed_in_leaf(doc, &leaf, s..e)
        };
        if !plan.claim(target.clone()) {
            let same = plan.wrapped.iter().find(|(range, _)| *range == target);
            if let Some(&(_, open)) = same.filter(|_| s == e && !target.is_empty()) {
                plan.carets
                    .push((i, target.start, open + (s - target.start)));
            }
            continue;
        }
        let text = doc.slice(target.clone());
        let (open, close) = delimiters(span, settings, &text, is_intraword(doc, &target));
        plan.wrapped.push((target.clone(), open.len()));
        if target.is_empty() {
            plan.carets.push((i, s, open.len()));
            plan.edits.push((s..s, open + &close));
        } else {
            if s == e {
                plan.carets
                    .push((i, target.start, open.len() + (s - target.start)));
            }
            plan.edits.push((target.start..target.start, open));
            plan.edits.push((target.end..target.end, close));
        }
    }
    plan.apply(doc);
}

/// Whether `text` is an absolute URL (`scheme:rest` with a scheme of two or
/// more characters and no whitespace) or starts with `www.`.
pub(crate) fn is_url(text: &str) -> bool {
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return false;
    }
    if text.starts_with("www.") && text.len() > 4 {
        return true;
    }
    let Some((scheme, rest)) = text.split_once(':') else {
        return false;
    };
    (2..=32).contains(&scheme.len())
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && !rest.is_empty()
}

/// `url` as a link destination: in angle brackets if it has characters a
/// bare destination cannot hold.
fn destination(url: &str) -> String {
    let mut depth: i32 = 0;
    let mut balanced = true;
    for c in url.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                balanced &= depth >= 0;
            }
            _ => {}
        }
    }
    balanced &= depth == 0;
    if balanced && !url.contains(['<', '>', ' ']) {
        url.to_owned()
    } else {
        format!("<{}>", url.replace('<', "%3C").replace('>', "%3E"))
    }
}

/// Insert link at every selection: `[text](url)` with a URL (from the
/// clipboard or the selection itself), else `[text]()` with the caret in
/// the destination, or `[[text]]` in wikilink style.
pub(crate) fn insert_link(doc: &mut Doc, settings: &CommandSettings, url: Option<&str>) {
    let url = url.map(str::trim).filter(|url| is_url(url));
    let wikilinks = settings.link_style == LinkStyle::Wikilink && doc.flavor.has_wikilinks();
    let mut plan = Plan::default();
    for (i, spec) in doc.sels.clone().into_iter().enumerate() {
        let (s, e) = (spec.tail.min(spec.head), spec.tail.max(spec.head));
        let (ctx, inline) = inline_context(doc, s);
        if ctx.in_verbatim() {
            continue;
        }
        // The range replaced, and text that goes before the link (the
        // container prefix of a blank line).
        let (target, lead) = match &inline {
            Some((leaf, tree)) => {
                let s = into_content(&ctx, leaf, s);
                let e = e.max(s);
                if in_code_span(tree, s) || in_code_span(tree, e) {
                    continue;
                }
                let target = if s == e {
                    word_at(&ctx.line, s).unwrap_or(s..s)
                } else {
                    trimmed_in_leaf(doc, leaf, s..e)
                };
                (target, String::new())
            }
            None if s == e && ctx.blank => {
                let (range, lead, _) = blank_line_insert(&ctx, "");
                (range, lead)
            }
            None => continue,
        };
        if !plan.claim(target.clone()) {
            continue;
        }
        let text = if lead.is_empty() {
            doc.slice(target.clone())
        } else {
            String::new()
        };
        let (insert, caret) = if let Some(url) = url {
            if text.is_empty() {
                (format!("[]({})", destination(url)), 1)
            } else {
                let link = format!("[{text}]({})", destination(url));
                let len = link.len();
                (link, len)
            }
        } else if is_url(&text) {
            (format!("[]({})", destination(&text)), 1)
        } else if wikilinks {
            (format!("[[{text}]]"), 2 + text.len())
        } else if text.is_empty() {
            ("[]()".to_owned(), 1)
        } else {
            (format!("[{text}]()"), text.len() + 3)
        };
        plan.carets.push((i, target.start, lead.len() + caret));
        plan.edits.push((target, lead + &insert));
    }
    plan.apply(doc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert!(is_url("https://example.com/a?b"));
        assert!(is_url("mailto:me@example.com"));
        assert!(is_url("www.example.com"));
        assert!(!is_url("C:\\path"));
        assert!(!is_url("not a url"));
        assert!(!is_url("word"));
        assert_eq!(destination("https://a.b/(x)"), "https://a.b/(x)");
        assert_eq!(destination("https://a.b/x)"), "<https://a.b/x)>");
        assert_eq!(destination("file:a b"), "<file:a b>");
    }

    #[test]
    fn code_delimiters() {
        let settings = CommandSettings::default();
        assert_eq!(
            delimiters(Span::Code, &settings, "a", false),
            ("`".into(), "`".into())
        );
        assert_eq!(
            delimiters(Span::Code, &settings, "a`b", false),
            ("``".into(), "``".into())
        );
        assert_eq!(
            delimiters(Span::Code, &settings, "`a", false),
            ("`` ".into(), " ``".into())
        );
        let underscore = CommandSettings {
            emphasis: EmphasisStyle::Underscore,
            ..settings
        };
        assert_eq!(delimiters(Span::Bold, &underscore, "a", false).0, "__");
        assert_eq!(delimiters(Span::Bold, &underscore, "a", true).0, "**");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What is prose in a parsed document.
//!
//! Prose comes from the CST: the text runs of paragraphs, headings and
//! table cells (inside emphasis, strikethrough, link text and image
//! descriptions too), and the values of front matter. Everything else is
//! skipped: code spans and blocks, math, raw HTML and HTML blocks, URLs
//! and autolinks, link destinations, titles and labels, footnote labels,
//! link reference definitions, entities and escapes, front-matter keys.
//!
//! Constructs the CST keeps as text are then cut out of the runs
//! ([`refine`]): wikilink and embed targets (`[[target|alias]]` keeps the
//! alias), callout types (`[!note]`), tags (`#tag`), emoji shortcodes
//! (`:smile:`), block ids (`^id`), bare URLs, email addresses and file
//! paths.

use std::ops::Range;

use gv_syntax::inline::{InlineKind, InlineNode, InlineTree, parse_table_cell};
use gv_syntax::{Leaf, LeafKind, SyntaxTree};
use gv_text::TextSource;

/// Front-matter keys whose values are names, not prose.
const IDENTIFIER_KEYS: &[&str] = &[
    "tags",
    "tag",
    "cssclass",
    "cssclasses",
    "lang",
    "language",
    "layout",
    "slug",
    "permalink",
    "id",
    "uid",
    "url",
    "link",
    "image",
    "cover",
    "banner",
    "template",
    "type",
    "date",
    "created",
    "modified",
    "updated",
];

/// The prose of `leaf`, as document ranges in order. `text` is the text
/// `tree` was parsed from.
pub fn leaf_prose<S: TextSource + ?Sized>(
    tree: &SyntaxTree,
    text: &S,
    leaf: &Leaf<'_>,
) -> Vec<Range<usize>> {
    let mut runs = Runs::default();
    match leaf.kind {
        LeafKind::Paragraph | LeafKind::Heading { .. } | LeafKind::CalloutTitle => {
            let inlines = tree.inlines(leaf, text);
            runs.tree(&inlines);
        }
        LeafKind::Table => {
            let refs = tree.ref_map(text);
            let options = tree.options().inline();
            let mut buf = Vec::new();
            for line in leaf.lines.iter().filter(|l| l.has_content && !l.marker) {
                buf.clear();
                text.copy_range(line.content.clone(), &mut buf);
                let Some(cells) = gv_syntax::table::row_cells(&buf) else {
                    continue;
                };
                for cell in cells {
                    let range = line.content.start + cell.start..line.content.start + cell.end;
                    let inlines = parse_table_cell(text, range, &*refs, options);
                    runs.tree(&inlines);
                }
            }
        }
        LeafKind::FrontMatter { kind } => {
            front_matter(
                text,
                leaf,
                kind == gv_syntax::FrontMatterKind::Toml,
                &mut runs,
            );
        }
        LeafKind::ThematicBreak
        | LeafKind::FencedCode
        | LeafKind::IndentedCode
        | LeafKind::Html { .. }
        | LeafKind::LinkRefDef
        | LeafKind::MathBlock
        | LeafKind::Comment => {}
    }
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for run in runs.0 {
        buf.clear();
        text.copy_range(run.clone(), &mut buf);
        let s = String::from_utf8_lossy(&buf);
        for piece in refine(&s) {
            out.push(run.start + piece.start..run.start + piece.end);
        }
    }
    out
}

/// Text runs, adjacent ones merged.
#[derive(Default)]
struct Runs(Vec<Range<usize>>);

impl Runs {
    fn push(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => self.0.push(range),
        }
    }

    /// Adds `range` as a run of its own.
    fn push_alone(&mut self, range: Range<usize>) {
        if !range.is_empty() {
            self.0.push(range);
        }
    }

    fn tree(&mut self, tree: &InlineTree) {
        for node in tree.top_level() {
            self.node(node);
        }
    }

    fn node(&mut self, node: InlineNode<'_>) {
        match node.kind() {
            InlineKind::Text => self.push(node.range()),
            InlineKind::Emphasis
            | InlineKind::Strong
            | InlineKind::Strikethrough
            | InlineKind::Link
            | InlineKind::Image
            | InlineKind::Highlight
            | InlineKind::InlineFootnote => node.children().for_each(|c| self.node(c)),
            // Code, math, HTML, autolinks, footnote references, wikilinks,
            // tags, comments, block ids, escapes, entities, breaks and
            // syntax are not prose.
            _ => {}
        }
    }
}

/// The values of front matter, without keys and without the values of
/// [`IDENTIFIER_KEYS`]. YAML: `key: value` and list items under a key;
/// TOML: quoted strings.
fn front_matter<S: TextSource + ?Sized>(text: &S, leaf: &Leaf<'_>, toml: bool, runs: &mut Runs) {
    let mut buf = Vec::new();
    // Whether the values of the current key are prose.
    let mut prose_key = false;
    for line in leaf.lines {
        buf.clear();
        text.copy_range(line.content.clone(), &mut buf);
        let s = String::from_utf8_lossy(&buf);
        let base = line.content.start;
        if toml {
            let Some(eq) = s.find('=') else {
                continue;
            };
            let key = s[..eq].trim().trim_matches(['"', '\'']);
            if s.trim_start().starts_with(['[', '#']) || is_identifier_key(key) {
                continue;
            }
            for quoted in quoted_strings(&s[eq + 1..]) {
                runs.push_alone(base + eq + 1 + quoted.start..base + eq + 1 + quoted.end);
            }
            continue;
        }
        let indented = s.starts_with([' ', '\t']);
        let trimmed = s.trim_start();
        let lead = s.len() - trimmed.len();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ")
            && (indented || !trimmed.contains(": "))
        {
            if prose_key {
                let start = s.len() - item.len();
                push_value(runs, base, start, &s[start..]);
            }
            continue;
        }
        let Some(colon) = trimmed.find(':') else {
            // A continuation of a multi-line value.
            if indented && prose_key {
                push_value(runs, base, lead, trimmed);
            }
            continue;
        };
        let key = trimmed[..colon].trim().trim_matches(['"', '\'']);
        prose_key = !is_identifier_key(key);
        if prose_key {
            let start = lead + colon + 1;
            push_value(runs, base, start, &s[start..]);
        }
    }
}

fn is_identifier_key(key: &str) -> bool {
    IDENTIFIER_KEYS.iter().any(|k| k.eq_ignore_ascii_case(key))
}

/// A YAML value from byte `start` of the line: its trimmed text, without
/// surrounding quotes and flow brackets.
fn push_value(runs: &mut Runs, base: usize, start: usize, value: &str) {
    let lead = value.len() - value.trim_start().len();
    let value = value.trim();
    let mut a = start + lead;
    let mut b = a + value.len();
    let bytes = value.as_bytes();
    if let (Some(first), Some(last)) = (bytes.first(), bytes.last())
        && bytes.len() >= 2
        && ((*first == b'"' && *last == b'"')
            || (*first == b'\'' && *last == b'\'')
            || (*first == b'[' && *last == b']'))
    {
        a += 1;
        b -= 1;
    }
    if matches!(value, "|" | ">" | "|-" | ">-") {
        return;
    }
    runs.push_alone(base + a..base + b);
}

/// The insides of the quoted strings of a TOML value.
fn quoted_strings(value: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut open: Option<(char, usize)> = None;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        match open {
            Some((q, start)) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    out.push(start..i);
                    open = None;
                }
            }
            None if c == '"' || c == '\'' => open = Some((c, i + 1)),
            None if c == '#' => break,
            None => {}
        }
    }
    out
}

/// The pieces of a text run left after cutting out what the CST keeps as
/// text but is not prose (see the module docs). Byte ranges into `s`.
pub fn refine(s: &str) -> Vec<Range<usize>> {
    let mut cut: Vec<Range<usize>> = Vec::new();
    let bytes = s.as_bytes();
    // Wikilinks and embeds: the target, and the brackets.
    let mut from = 0;
    while let Some(open) = s[from..].find("[[").map(|i| from + i) {
        let Some(close) = s[open + 2..].find("]]").map(|i| open + 2 + i) else {
            break;
        };
        let start = if open > 0 && bytes[open - 1] == b'!' {
            open - 1
        } else {
            open
        };
        match s[open + 2..close].find('|') {
            Some(pipe) => {
                cut.push(start..open + 2 + pipe + 1);
                cut.push(close..close + 2);
            }
            None => cut.push(start..close + 2),
        }
        from = close + 2;
    }
    // Callout types: `[!note]`, `[!tip]-`.
    let mut from = 0;
    while let Some(open) = s[from..].find("[!").map(|i| from + i) {
        let Some(close) = s[open..].find(']').map(|i| open + i) else {
            break;
        };
        if s[open + 2..close]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            cut.push(open..close + 1);
        }
        from = close + 1;
    }
    // Whitespace-separated chunks: URLs, addresses, paths, tags, block ids,
    // shortcodes.
    let mut at = 0;
    for chunk in s.split(char::is_whitespace) {
        let start = at;
        at += chunk.len()
            + s[start + chunk.len()..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
        if chunk.is_empty() {
            continue;
        }
        let end = start + chunk.len();
        let lower = chunk.to_ascii_lowercase();
        let trimmed = chunk.trim_start_matches(['(', '<', '"', '\'', '[']);
        let path_like = chunk.contains(['/', '\\'])
            && (chunk.contains(['.', '~']) || trimmed.starts_with(['/', '\\']));
        if lower.contains("://")
            || trimmed.to_ascii_lowercase().starts_with("www.")
            || lower.starts_with("mailto:")
            || chunk.contains('@')
            || path_like
        {
            cut.push(start..end);
            continue;
        }
        if let Some(id) = chunk.strip_prefix('^')
            && !id.is_empty()
            && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            cut.push(start..end);
            continue;
        }
        // Tags: `#` then tag characters, at the chunk's start (after
        // opening punctuation).
        let lead = chunk.len() - trimmed.len();
        if let Some(rest) = trimmed.strip_prefix('#')
            && rest
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            let len = rest
                .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
                .unwrap_or(rest.len());
            cut.push(start + lead..start + lead + 1 + len);
        }
        // Shortcodes: `:name:` anywhere in the chunk.
        let mut from = 0;
        while let Some(open) = chunk[from..].find(':').map(|i| from + i) {
            let name_len = chunk[open + 1..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')))
                .unwrap_or(chunk.len() - open - 1);
            let close = open + 1 + name_len;
            if name_len > 0 && chunk[close..].starts_with(':') {
                cut.push(start + open..start + close + 1);
                from = close + 1;
            } else {
                from = open + 1;
            }
        }
    }
    cut.sort_by_key(|r| r.start);
    let mut out = Vec::new();
    let mut pos = 0;
    for r in cut {
        if r.start > pos {
            out.push(pos..r.start);
        }
        pos = pos.max(r.end);
    }
    if pos < s.len() {
        out.push(pos..s.len());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(s: &str) -> String {
        refine(s)
            .into_iter()
            .map(|r| &s[r])
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn refine_cuts_what_is_not_prose() {
        assert_eq!(
            kept("see [[Target note#Head|the alias]] now"),
            "see |the alias| now"
        );
        assert_eq!(kept("![[image.png]] after"), " after");
        assert_eq!(kept("[!warning]- Title"), "- Title");
        assert_eq!(kept("a #tagg/sub, b"), "a |, b");
        assert_eq!(kept("C# and #1"), "C# and ");
        assert_eq!(kept("hi :smiley: there"), "hi | there");
        assert_eq!(kept("time 10:30 ok"), "time 10:30 ok");
        assert_eq!(kept("end ^blockid"), "end ");
        assert_eq!(kept("go https://exampel.org/x now"), "go | now");
        assert_eq!(kept("(www.exampel.com) mail me@exampel.org"), " mail ");
        assert_eq!(kept("open ./dirr/fille.md or ~/x"), "open | or ");
        assert_eq!(kept("and/or"), "and/or");
    }
}

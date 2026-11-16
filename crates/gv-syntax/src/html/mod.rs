// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The CommonMark-exact and GFM-exact HTML emitters.
//!
//! Block structure follows cmark's `html.c` byte for byte (with raw HTML
//! passed through, as `cmark --unsafe` does): XHTML-style void elements,
//! `& < > "` escaped, `class="language-X"` from the first word of a code
//! block's info string, `<p>` omitted in tight lists. Inline content is
//! rendered by the inline module, with the inline switches of the tree's
//! [`ParseOptions`](crate::ParseOptions).
//!
//! [`to_gfm_html`] is cmark-gfm's output instead: tables
//! (`extensions/table.c`), `<del>` for strikethrough, no tags for a strong
//! emphasis directly inside another, and optionally the tagfilter.
//!
//! Code and HTML block literals keep the document's line endings (the
//! block pass never rewrites text); U+0000 is replaced by U+FFFD.

use std::sync::Arc;

use gv_text::TextSource;

use crate::inline::api::{
    InlineHtmlOptions, InlineInput, InlineOptions, parse_inlines_with, parse_table_cell,
    push_tagfiltered, render_inlines_html_with, unescape_string,
};
use crate::table::{self, Alignment};
use crate::tree::walk::{self, Container, ContainerKind, Leaf, LeafKind, Visitor};
use crate::tree::{RefMap, SyntaxTree};

/// Switches of [`to_gfm_html`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GfmHtmlOptions {
    /// The tagfilter extension: in raw HTML (blocks and inline), the `<` of
    /// `title`, `textarea`, `style`, `xmp`, `iframe`, `noembed`,
    /// `noframes`, `script` and `plaintext` tags becomes `&lt;`.
    pub tagfilter: bool,
}

/// Renders `tree` (parsed from `text`) as CommonMark HTML.
pub fn to_commonmark_html<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S) -> String {
    render(tree, text, InlineHtmlOptions::default())
}

/// Renders `tree` (parsed from `text`) as cmark-gfm does: with the inline
/// switches of the tree's options, GFM tables, and cmark-gfm's renderer
/// quirks (see the [module documentation](self)).
pub fn to_gfm_html<S: TextSource + ?Sized>(
    tree: &SyntaxTree,
    text: &S,
    options: GfmHtmlOptions,
) -> String {
    render(
        tree,
        text,
        InlineHtmlOptions {
            gfm: true,
            tagfilter: options.tagfilter,
        },
    )
}

fn render<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S, html: InlineHtmlOptions) -> String {
    let mut r = Renderer {
        out: String::with_capacity(tree.len() + tree.len() / 4),
        text,
        refs: tree.ref_map(text),
        stack: Vec::new(),
        buf: Vec::new(),
        segments: Vec::new(),
        inline: tree.options().inline(),
        html,
        padded: 0,
    };
    walk::walk(tree, &mut r);
    r.out
}

struct Renderer<'s, S: TextSource + ?Sized> {
    out: String,
    text: &'s S,
    refs: Arc<RefMap>,
    stack: Vec<ContainerKind>,
    buf: Vec<u8>,
    segments: Vec<std::ops::Range<usize>>,
    inline: InlineOptions,
    html: InlineHtmlOptions,
    /// Empty cells written so far to pad short table rows.
    padded: usize,
}

/// At most this many empty cells pad short table rows in one document
/// (cmark-gfm's guard against output blow-up, `MAX_AUTOCOMPLETED_CELLS`);
/// rows after that are written as they are.
const MAX_PADDED_CELLS: usize = 500_000;

/// Appends `s` with `& < > "` escaped and U+0000 replaced.
pub(crate) fn escape_html(out: &mut String, s: &str) {
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            0 => "\u{fffd}",
            _ => continue,
        };
        out.push_str(&s[last..i]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&s[last..]);
}

impl<S: TextSource + ?Sized> Renderer<'_, S> {
    /// cmark's `cr()`: a newline unless the output is empty or ends with
    /// one.
    fn cr(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn inlines(&mut self, leaf: &Leaf<'_>) {
        self.segments.clear();
        self.segments.extend(
            leaf.lines
                .iter()
                .filter(|l| l.has_content)
                .map(|l| l.content.clone()),
        );
        let tree = parse_inlines_with(
            self.text,
            InlineInput {
                segments: &self.segments,
            },
            &*self.refs,
            self.inline,
        );
        render_inlines_html_with(&tree, &mut self.out, self.html);
    }

    /// The cells of a table line (document ranges); `None` if it is not a
    /// row.
    fn row(&mut self, content: &std::ops::Range<usize>) -> Option<Vec<std::ops::Range<usize>>> {
        self.buf.clear();
        self.text.copy_range(content.clone(), &mut self.buf);
        let cells = table::row_cells(&self.buf)?;
        Some(
            cells
                .into_iter()
                .map(|r| content.start + r.start..content.start + r.end)
                .collect(),
        )
    }

    /// A GFM table, as cmark-gfm's table extension writes it.
    fn table(&mut self, leaf: &Leaf<'_>) {
        let (Some(header), Some(delim)) = (leaf.lines.first(), leaf.lines.get(1)) else {
            return;
        };
        self.buf.clear();
        self.text.copy_range(delim.content.clone(), &mut self.buf);
        let aligns = table::delimiter_row(&self.buf).unwrap_or_default();
        let columns = aligns.len();
        self.cr();
        self.out.push_str("<table>");
        let header_cells = self.row(&header.content).unwrap_or_default();
        self.cr();
        self.out.push_str("<thead>");
        self.table_row(&header_cells, &aligns, "th");
        self.cr();
        self.out.push_str("</thead>");
        let body = &leaf.lines[2..];
        for line in body {
            let mut cells = self.row(&line.content).unwrap_or_default();
            cells.truncate(columns);
            self.cr();
            if std::ptr::eq(line, &body[0]) {
                self.out.push_str("<tbody>");
                self.cr();
            }
            self.table_row(&cells, &aligns, "td");
        }
        if !body.is_empty() {
            self.cr();
            self.out.push_str("</tbody>");
            self.cr();
        }
        self.cr();
        self.out.push_str("</table>");
        self.cr();
    }

    /// One `<tr>`: `cells` padded with empty cells to the column count
    /// (within [`MAX_PADDED_CELLS`]).
    fn table_row(&mut self, cells: &[std::ops::Range<usize>], aligns: &[Alignment], tag: &str) {
        self.cr();
        self.out.push_str("<tr>");
        let missing = aligns.len().saturating_sub(cells.len());
        let n = if self.padded + missing <= MAX_PADDED_CELLS {
            self.padded += missing;
            aligns.len()
        } else {
            cells.len().min(aligns.len())
        };
        for (i, align) in aligns.iter().take(n).enumerate() {
            self.cr();
            self.out.push('<');
            self.out.push_str(tag);
            if let Some(a) = align.html() {
                self.out.push_str(" align=\"");
                self.out.push_str(a);
                self.out.push('"');
            }
            self.out.push('>');
            if let Some(cell) = cells.get(i) {
                let tree = parse_table_cell(self.text, cell.clone(), &*self.refs, self.inline);
                render_inlines_html_with(&tree, &mut self.out, self.html);
            }
            self.out.push_str("</");
            self.out.push_str(tag);
            self.out.push('>');
        }
        self.cr();
        self.out.push_str("</tr>");
    }

    /// The literal text of a code or HTML block.
    fn literal(&mut self, leaf: &Leaf<'_>) {
        self.buf.clear();
        for l in leaf.lines {
            self.buf
                .extend(std::iter::repeat_n(b' ', usize::from(l.virtual_spaces)));
            self.text.copy_range(l.content.clone(), &mut self.buf);
            self.text.copy_range(l.line_ending.clone(), &mut self.buf);
        }
    }

    fn code_block(&mut self, leaf: &Leaf<'_>) {
        self.cr();
        let info = leaf.info.as_ref().map(|r| {
            let mut raw = Vec::new();
            self.text.copy_range(r.clone(), &mut raw);
            info_word(&raw)
        });
        match info {
            Some(word) if !word.is_empty() => {
                self.out.push_str("<pre><code class=\"language-");
                escape_html(&mut self.out, &word);
                self.out.push_str("\">");
            }
            _ => self.out.push_str("<pre><code>"),
        }
        self.literal(leaf);
        if matches!(leaf.kind, LeafKind::IndentedCode) {
            trim_trailing_blank_lines(&mut self.buf);
            self.buf.push(b'\n');
        }
        let lit = String::from_utf8_lossy(&self.buf).into_owned();
        escape_html(&mut self.out, &lit);
        self.out.push_str("</code></pre>\n");
    }
}

/// comrak/cmark `remove_trailing_blank_lines`: drops trailing lines that
/// hold only spaces and tabs, and the last line's terminator.
fn trim_trailing_blank_lines(s: &mut Vec<u8>) {
    let Some(last) = s
        .iter()
        .rposition(|&c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
    else {
        s.clear();
        return;
    };
    let end = s[last..]
        .iter()
        .position(|&c| c == b'\n' || c == b'\r')
        .map_or(s.len(), |i| last + i);
    s.truncate(end);
}

/// The first word of an info string after decoding backslash escapes and
/// character references (one left-to-right pass, as in link destinations)
/// and trimming.
fn info_word(raw: &[u8]) -> String {
    const SPACE: [char; 6] = [' ', '\t', '\n', '\r', '\u{b}', '\u{c}'];
    let decoded = unescape_string(&String::from_utf8_lossy(raw));
    let s = decoded.trim_matches(SPACE);
    s[..s.find(SPACE).unwrap_or(s.len())].to_owned()
}

impl<S: TextSource + ?Sized> Visitor for Renderer<'_, S> {
    fn enter(&mut self, c: &Container) {
        match c.kind {
            ContainerKind::BlockQuote => {
                self.cr();
                self.out.push_str("<blockquote>\n");
            }
            ContainerKind::BulletList { .. } => {
                self.cr();
                self.out.push_str("<ul>\n");
            }
            ContainerKind::OrderedList { start, .. } => {
                self.cr();
                if start == 1 {
                    self.out.push_str("<ol>\n");
                } else {
                    self.out.push_str("<ol start=\"");
                    self.out.push_str(&start.to_string());
                    self.out.push_str("\">\n");
                }
            }
            ContainerKind::Item => {
                self.cr();
                self.out.push_str("<li>");
            }
        }
        self.stack.push(c.kind);
    }

    fn exit(&mut self, c: &Container) {
        self.stack.pop();
        match c.kind {
            ContainerKind::BlockQuote => {
                self.cr();
                self.out.push_str("</blockquote>\n");
            }
            ContainerKind::BulletList { .. } => self.out.push_str("</ul>\n"),
            ContainerKind::OrderedList { .. } => self.out.push_str("</ol>\n"),
            ContainerKind::Item => self.out.push_str("</li>\n"),
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        match leaf.kind {
            LeafKind::Paragraph => {
                let n = self.stack.len();
                let tight = n >= 2
                    && self.stack[n - 1] == ContainerKind::Item
                    && matches!(
                        self.stack[n - 2],
                        ContainerKind::BulletList { tight: true, .. }
                            | ContainerKind::OrderedList { tight: true, .. }
                    );
                if tight {
                    self.inlines(leaf);
                } else {
                    self.cr();
                    self.out.push_str("<p>");
                    self.inlines(leaf);
                    self.out.push_str("</p>\n");
                }
            }
            LeafKind::Heading { level, .. } => {
                self.cr();
                let tag = char::from(b'0' + level.clamp(1, 6));
                self.out.push_str("<h");
                self.out.push(tag);
                self.out.push('>');
                self.inlines(leaf);
                self.out.push_str("</h");
                self.out.push(tag);
                self.out.push_str(">\n");
            }
            LeafKind::ThematicBreak => {
                self.cr();
                self.out.push_str("<hr />\n");
            }
            LeafKind::FencedCode | LeafKind::IndentedCode => self.code_block(leaf),
            LeafKind::Html { .. } => {
                self.cr();
                self.literal(leaf);
                let lit = String::from_utf8_lossy(&self.buf).into_owned();
                if self.html.tagfilter {
                    push_tagfiltered(&mut self.out, &lit);
                } else {
                    self.out.push_str(&lit.replace('\0', "\u{fffd}"));
                }
                self.cr();
            }
            LeafKind::Table => self.table(leaf),
            // Front matter is metadata; this emitter renders nothing for it.
            LeafKind::LinkRefDef | LeafKind::FrontMatter { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_strings() {
        assert_eq!(info_word(b"rust"), "rust");
        assert_eq!(info_word(b"  rust  x"), "rust");
        assert_eq!(info_word(br"foo\+bar"), "foo+bar");
        assert_eq!(info_word(b"f&ouml;&ouml;"), "f\u{f6}\u{f6}");
        assert_eq!(info_word(b"a&#x41;&#66;&#0;"), "aAB\u{fffd}");
        assert_eq!(info_word(b"&bogus;"), "&bogus;");
        assert_eq!(info_word(b""), "");
    }

    #[test]
    fn trailing_blank_lines() {
        let mut s = b"a\n  \n\t\n".to_vec();
        trim_trailing_blank_lines(&mut s);
        assert_eq!(s, b"a");
        let mut s = b"a\r\nb\r\n\r\n".to_vec();
        trim_trailing_blank_lines(&mut s);
        assert_eq!(s, b"a\r\nb");
        let mut s = b" \n".to_vec();
        trim_trailing_blank_lines(&mut s);
        assert!(s.is_empty());
    }

    #[test]
    fn escaping() {
        let mut out = String::new();
        escape_html(&mut out, "a<b>&\"c\"\0'");
        assert_eq!(out, "a&lt;b&gt;&amp;&quot;c&quot;\u{fffd}'");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The CommonMark-exact HTML emitter.
//!
//! Block structure follows cmark's `html.c` byte for byte (with raw HTML
//! passed through, as `cmark --unsafe` does): XHTML-style void elements,
//! `& < > "` escaped, `class="language-X"` from the first word of a code
//! block's info string, `<p>` omitted in tight lists. Inline content is
//! rendered by the inline module.
//!
//! Code and HTML block literals keep the document's line endings (the
//! block pass never rewrites text); U+0000 is replaced by U+FFFD.

use std::sync::Arc;

use gv_text::TextSource;

use crate::inline::api::{InlineInput, parse_inlines, render_inlines_html, unescape_string};
use crate::tree::walk::{self, Container, ContainerKind, Leaf, LeafKind, Visitor};
use crate::tree::{RefMap, SyntaxTree};

/// Renders `tree` (parsed from `text`) as CommonMark HTML.
pub fn to_commonmark_html<S: TextSource + ?Sized>(tree: &SyntaxTree, text: &S) -> String {
    let mut r = Renderer {
        out: String::with_capacity(tree.len() + tree.len() / 4),
        text,
        refs: tree.ref_map(text),
        stack: Vec::new(),
        buf: Vec::new(),
        segments: Vec::new(),
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
}

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
        let tree = parse_inlines(
            self.text,
            InlineInput {
                segments: &self.segments,
            },
            &*self.refs,
        );
        render_inlines_html(&tree, &mut self.out);
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
                let lit = String::from_utf8_lossy(&self.buf).replace('\0', "\u{fffd}");
                self.out.push_str(&lit);
                self.cr();
            }
            LeafKind::LinkRefDef => {}
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

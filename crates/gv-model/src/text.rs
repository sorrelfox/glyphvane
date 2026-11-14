// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Plain-text projections of a heading's inline tree (see
//! [`Projection`](crate::slug::Projection)).
//!
//! Both projections keep text (escapes and entities decoded), code span
//! content (normalized as CommonMark renders it), link text and autolink
//! addresses, and drop markup: emphasis markers, raw HTML tags (the text
//! between them stays), destinations and titles. They differ in images and
//! line breaks:
//!
//! - **rendered text** (a browser's `textContent`): images contribute
//!   nothing (their description is an attribute), and soft and hard line
//!   breaks are the `\n` the HTML holds;
//! - **text with descriptions** (comrak's `collect_text`, pandoc's
//!   `stringify`): image descriptions are included, line breaks are
//!   spaces.

use gv_syntax::inline::{Children, InlineKind, InlineTree, unescape_string};

/// The two projections of one inline tree.
#[derive(Debug, Default)]
pub(crate) struct Projections {
    /// The rendered text.
    pub text: String,
    /// The text with image descriptions and spaces for line breaks.
    pub with_alt: String,
}

/// Projects `tree`.
pub(crate) fn project(tree: &InlineTree) -> Projections {
    let mut p = Projections::default();
    walk(tree.top_level(), false, &mut p);
    p
}

fn walk(nodes: Children<'_>, in_image: bool, p: &mut Projections) {
    for node in nodes {
        let push = |p: &mut Projections, s: &str| {
            if !in_image {
                p.text.push_str(s);
            }
            p.with_alt.push_str(s);
        };
        match node.kind() {
            InlineKind::Text => push(p, node.text()),
            InlineKind::Escape => push(p, node.text().get(1..).unwrap_or("")),
            InlineKind::Entity => push(p, &unescape_string(node.text())),
            InlineKind::SoftBreak | InlineKind::HardBreak => {
                if !in_image {
                    p.text.push('\n');
                }
                p.with_alt.push(' ');
            }
            InlineKind::CodeSpan => push(p, &code_span_content(node.text())),
            InlineKind::Autolink => {
                if let Some(link) = node.link() {
                    push(p, link.destination);
                }
            }
            InlineKind::Emphasis | InlineKind::Strong | InlineKind::Link => {
                walk(node.children(), in_image, p);
            }
            InlineKind::Image => walk(node.children(), true, p),
            InlineKind::RawHtml
            | InlineKind::Whitespace
            | InlineKind::Marker
            | InlineKind::CodeText
            | InlineKind::HtmlText
            | InlineKind::AutolinkText
            | InlineKind::LinkDestination
            | InlineKind::LinkTitle
            | InlineKind::LinkLabel => {}
        }
    }
}

/// The content of a code span as CommonMark renders it, from the span's
/// text with its backtick strings: line endings become spaces, then one
/// space is stripped from each end if both ends have one and the content
/// is not all spaces.
pub(crate) fn code_span_content(span: &str) -> String {
    let ticks = span.bytes().take_while(|&b| b == b'`').count();
    let inner = span
        .get(ticks..span.len().saturating_sub(ticks).max(ticks))
        .unwrap_or("");
    let mut s = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                s.push(' ');
            }
            '\n' => s.push(' '),
            c => s.push(c),
        }
    }
    if s.len() >= 2 && s.starts_with(' ') && s.ends_with(' ') && s.bytes().any(|b| b != b' ') {
        s.pop();
        s.remove(0);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_syntax::inline::{InlineInput, parse_inlines};

    fn proj(src: &str) -> (String, String) {
        let segments: Vec<_> = {
            let mut v = Vec::new();
            let mut start = 0;
            for line in src.split_inclusive('\n') {
                let end = start + line.trim_end_matches('\n').len();
                v.push(start..end);
                start += line.len();
            }
            v
        };
        let tree = parse_inlines(
            src,
            InlineInput {
                segments: &segments,
            },
            &(),
        );
        let p = project(&tree);
        (p.text, p.with_alt)
    }

    #[test]
    fn projections() {
        assert_eq!(proj("Foo *bar* `x`").0, "Foo bar x");
        assert_eq!(proj("a ![img *x*](p.png) b").0, "a  b");
        assert_eq!(proj("a ![img *x*](p.png) b").1, "a img x b");
        assert_eq!(
            proj("Foo <b>Bar</b> &amp; \\* [l](u \"t\")").0,
            "Foo Bar & * l"
        );
        assert_eq!(
            proj("<https://x.org/&amp;> <a@b.c>").0,
            "https://x.org/& a@b.c"
        );
        assert_eq!(proj("a\nb").0, "a\nb");
        assert_eq!(proj("a\nb").1, "a b");
        assert_eq!(proj("a  \nb").0, "a\nb");
        assert_eq!(proj("`` ` `` and ` a\nb `").0, "` and a b");
        assert_eq!(proj("&#0; &copy;").0, "\u{fffd} ©");
    }

    #[test]
    fn code_spans() {
        assert_eq!(code_span_content("` a `"), "a");
        assert_eq!(code_span_content("`  `"), "  ");
        assert_eq!(code_span_content("``a\r\nb``"), "a b");
        assert_eq!(code_span_content("` a`"), " a");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! CommonMark-exact HTML for inline trees (cmark's output: XHTML-style
//! `<br />` and `<img … />`, `& < > "` escaped in text, cmark-style URL
//! percent-encoding), the plain-text projection used for image `alt`, and
//! the text projections ([`TextProjection`]).

use super::api::InlineTree;
use super::scan;
use super::tree::{InlineKind as K, LinkKind, Node, TextProjection};

/// Switches of [`render`].
#[derive(Clone, Copy, Debug)]
pub(super) struct RenderOptions {
    /// cmark-gfm's renderer: no tags for a strong emphasis whose parent is
    /// a strong emphasis.
    pub(super) gfm: bool,
    /// The GFM tagfilter on raw HTML.
    pub(super) tagfilter: bool,
}

impl RenderOptions {
    pub(super) const COMMONMARK: RenderOptions = RenderOptions {
        gfm: false,
        tagfilter: false,
    };
}

/// Renders `tree` as HTML into `out`.
pub(super) fn render(tree: &InlineTree, out: &mut String, opts: RenderOptions) {
    render_with(tree, out, opts, None);
}

/// Writes the HTML of a footnote reference, given its normalized label.
pub(super) type FootnoteHtml<'f> = &'f mut dyn FnMut(&str, &mut String);

/// [`render`] with the footnote references written by `footnotes` (as
/// their source text without it).
pub(super) fn render_with(
    tree: &InlineTree,
    out: &mut String,
    opts: RenderOptions,
    mut footnotes: Option<FootnoteHtml<'_>>,
) {
    let nodes = &tree.nodes;
    // Open elements: where they end, their closing tag, their kind.
    let mut closers: Vec<(u32, &'static str, K)> = Vec::new();
    let mut i = 0usize;
    while i < nodes.len() {
        while let Some(&(end, tag, _)) = closers.last() {
            if end as usize > i {
                break;
            }
            out.push_str(tag);
            closers.pop();
        }
        let n = nodes[i];
        let mut next = i + 1;
        match n.kind {
            K::Text => escape_html(out, tree.slice(n.start, n.end)),
            K::Escape => escape_html(out, tree.slice(n.start + 1, n.end)),
            K::Entity => push_entity_escaped(n.data, out),
            K::SoftBreak => out.push('\n'),
            K::HardBreak => out.push_str("<br />\n"),
            K::Emphasis => {
                out.push_str("<em>");
                closers.push((n.next, "</em>", K::Emphasis));
            }
            K::Strong => {
                if opts.gfm && closers.last().is_some_and(|c| c.2 == K::Strong) {
                    closers.push((n.next, "", K::Strong));
                } else {
                    out.push_str("<strong>");
                    closers.push((n.next, "</strong>", K::Strong));
                }
            }
            K::Strikethrough => {
                out.push_str("<del>");
                closers.push((n.next, "</del>", K::Strikethrough));
            }
            K::Highlight => {
                out.push_str("<mark>");
                closers.push((n.next, "</mark>", K::Highlight));
            }
            K::InlineFootnote => {
                out.push_str("<sup class=\"footnote-inline\">");
                closers.push((n.next, "</sup>", K::InlineFootnote));
            }
            K::Wikilink | K::Embed => {
                if let Some(w) = tree.wiki_ref(&n) {
                    if w.embed {
                        out.push_str("<span class=\"internal-embed\" src=\"");
                        escape_html(out, w.target.trim());
                        out.push_str("\"></span>");
                    } else {
                        out.push_str("<a href=\"");
                        escape_href(out, w.target.trim());
                        out.push_str("\" class=\"internal-link\">");
                        escape_html(out, &w.display());
                        out.push_str("</a>");
                    }
                }
                next = n.next as usize;
            }
            K::Tag => {
                let text = tree.slice(n.start, n.end);
                out.push_str("<a href=\"");
                escape_href(out, text);
                out.push_str("\" class=\"tag\">");
                escape_html(out, text);
                out.push_str("</a>");
                next = n.next as usize;
            }
            K::Comment | K::BlockId => next = n.next as usize,
            K::CodeSpan => {
                out.push_str("<code>");
                code_content(tree, &n, out, true);
                out.push_str("</code>");
                next = n.next as usize;
            }
            K::Link => {
                if let Some(l) = tree.links.get(n.data as usize) {
                    out.push_str("<a href=\"");
                    escape_href(out, tree.str(l.dest));
                    out.push('"');
                    if let Some(title) = l.title.map(|t| tree.str(t)).filter(|t| !t.is_empty()) {
                        out.push_str(" title=\"");
                        escape_html(out, title);
                        out.push('"');
                    }
                    out.push('>');
                    closers.push((n.next, "</a>", K::Link));
                }
            }
            K::Image => {
                if let Some(l) = tree.links.get(n.data as usize) {
                    out.push_str("<img src=\"");
                    escape_href(out, tree.str(l.dest));
                    out.push_str("\" alt=\"");
                    plain_text(tree, i + 1, n.next as usize, out, true);
                    out.push('"');
                    if let Some(title) = l.title.map(|t| tree.str(t)).filter(|t| !t.is_empty()) {
                        out.push_str(" title=\"");
                        escape_html(out, title);
                        out.push('"');
                    }
                    out.push_str(" />");
                }
                next = n.next as usize;
            }
            K::Autolink => {
                if let Some(l) = tree.links.get(n.data as usize) {
                    out.push_str("<a href=\"");
                    if l.kind == LinkKind::EmailAutolink {
                        out.push_str("mailto:");
                    }
                    escape_href(out, tree.str(l.dest));
                    out.push_str("\">");
                    escape_html(out, tree.autolink_text(l));
                    out.push_str("</a>");
                }
                next = n.next as usize;
            }
            K::InlineMath | K::DisplayMath => {
                // comrak's math markup.
                out.push_str(if n.kind == K::InlineMath {
                    "<span data-math-style=\"inline\">"
                } else {
                    "<span data-math-style=\"display\">"
                });
                escape_html(out, super::tree::math_tex(tree, &n));
                out.push_str("</span>");
                next = n.next as usize;
            }
            K::FootnoteRef => {
                match (&mut footnotes, tree.footnote_label(n.data)) {
                    (Some(f), Some(label)) => f(label, out),
                    _ => escape_html(out, tree.slice(n.start, n.end)),
                }
                next = n.next as usize;
            }
            K::RawHtml => {
                let raw = tree.slice(n.start, n.end);
                if opts.tagfilter {
                    push_filtered(out, raw);
                } else {
                    push_raw(out, raw);
                }
                next = n.next as usize;
            }
            K::Whitespace
            | K::Marker
            | K::CodeText
            | K::HtmlText
            | K::AutolinkText
            | K::MathText
            | K::LinkDestination
            | K::LinkTitle
            | K::LinkLabel
            | K::FootnoteLabel
            | K::CommentText
            | K::TagText
            | K::WikiTarget
            | K::WikiAlias
            | K::BlockIdText => {}
        }
        i = next;
    }
    while let Some((_, tag, _)) = closers.pop() {
        out.push_str(tag);
    }
}

/// The tags the GFM tagfilter disallows.
const DISALLOWED_TAGS: [&[u8]; 9] = [
    b"title",
    b"textarea",
    b"style",
    b"xmp",
    b"iframe",
    b"noembed",
    b"noframes",
    b"script",
    b"plaintext",
];

/// cmark-gfm's `is_tag` of the tagfilter: whether `s` (from a `<` to the
/// end of the raw HTML) opens or closes a disallowed tag.
fn is_disallowed_tag(s: &[u8]) -> bool {
    if s.len() < 3 || s[0] != b'<' {
        return false;
    }
    let start = if s[1] == b'/' { 2 } else { 1 };
    DISALLOWED_TAGS.iter().any(|name| {
        let end = start + name.len();
        if end >= s.len() || !s[start..end].eq_ignore_ascii_case(name) {
            return false;
        }
        match s[end] {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C | b'>' => true,
            b'/' => s.get(end + 1) == Some(&b'>'),
            _ => false,
        }
    })
}

/// Appends raw HTML with the GFM tagfilter applied: the `<` of every
/// disallowed tag becomes `&lt;`. U+0000 is replaced.
pub(crate) fn push_filtered(out: &mut String, s: &str) {
    let b = s.as_bytes();
    let mut copied = 0;
    for i in memchr::memchr_iter(b'<', b) {
        if is_disallowed_tag(&b[i..]) {
            push_raw(out, &s[copied..i]);
            out.push_str("&lt;");
            copied = i + 1;
        }
    }
    push_raw(out, &s[copied..]);
}

/// Appends the plain text of the nodes `from..to` (cmark's rendering of an
/// image description): text, decoded escapes and entities, code span
/// content and raw HTML as text, line breaks as spaces. With `escape`, the
/// text is HTML-escaped.
pub(super) fn plain_text(
    tree: &InlineTree,
    from: usize,
    to: usize,
    out: &mut String,
    escape: bool,
) {
    let push = |out: &mut String, s: &str| {
        if escape {
            escape_html(out, s);
        } else {
            push_raw(out, s);
        }
    };
    let mut i = from;
    while i < to.min(tree.nodes.len()) {
        let n = tree.nodes[i];
        let mut next = i + 1;
        match n.kind {
            K::Text => push(out, tree.slice(n.start, n.end)),
            K::Escape => push(out, tree.slice(n.start + 1, n.end)),
            K::Entity => {
                let mut s = String::new();
                scan::push_entity(n.data, &mut s);
                push(out, &s);
            }
            K::SoftBreak | K::HardBreak => out.push(' '),
            K::CodeSpan => {
                code_content(tree, &n, out, escape);
                next = n.next as usize;
            }
            K::RawHtml => {
                push(out, tree.slice(n.start, n.end));
                next = n.next as usize;
            }
            K::InlineMath | K::DisplayMath => {
                push(out, super::tree::math_tex(tree, &n));
                next = n.next as usize;
            }
            K::Autolink => {
                if let Some(l) = tree.links.get(n.data as usize) {
                    push(out, tree.autolink_text(l));
                }
                next = n.next as usize;
            }
            K::Wikilink => {
                if let Some(w) = tree.wiki_ref(&n) {
                    push(out, &w.display());
                }
                next = n.next as usize;
            }
            K::Tag => {
                push(out, tree.slice(n.start, n.end));
                next = n.next as usize;
            }
            // Its number is document-global: no text here. Embeds, comments
            // and block ids have none either.
            K::FootnoteRef | K::InlineFootnote | K::Embed | K::Comment | K::BlockId => {
                next = n.next as usize;
            }
            K::Highlight
            | K::Emphasis
            | K::Strong
            | K::Strikethrough
            | K::Link
            | K::Image
            | K::Whitespace
            | K::Marker
            | K::CodeText
            | K::HtmlText
            | K::AutolinkText
            | K::MathText
            | K::LinkDestination
            | K::LinkTitle
            | K::LinkLabel
            | K::FootnoteLabel
            | K::CommentText
            | K::TagText
            | K::WikiTarget
            | K::WikiAlias
            | K::BlockIdText => {}
        }
        i = next;
    }
}

/// Appends the text projection of the nodes `from..to` (see
/// [`TextProjection`]).
pub(super) fn project(
    tree: &InlineTree,
    from: usize,
    to: usize,
    projection: TextProjection,
    out: &mut String,
) {
    let with_alt = projection == TextProjection::CollectText;
    let mut i = from;
    while i < to.min(tree.nodes.len()) {
        let n = tree.nodes[i];
        let mut next = i + 1;
        match n.kind {
            K::Text => push_raw(out, tree.slice(n.start, n.end)),
            K::Escape => push_raw(out, tree.slice(n.start + 1, n.end)),
            K::Entity => scan::push_entity(n.data, out),
            K::SoftBreak | K::HardBreak => out.push(if with_alt { ' ' } else { '\n' }),
            K::CodeSpan => {
                code_content(tree, &n, out, false);
                next = n.next as usize;
            }
            K::Autolink => {
                if let Some(l) = tree.links.get(n.data as usize) {
                    push_raw(out, tree.autolink_text(l));
                }
                next = n.next as usize;
            }
            K::Image if !with_alt => next = n.next as usize,
            // The TeX as written.
            K::InlineMath | K::DisplayMath => {
                push_raw(out, super::tree::math_tex(tree, &n));
                next = n.next as usize;
            }
            K::Wikilink => {
                if let Some(w) = tree.wiki_ref(&n) {
                    push_raw(out, &w.display());
                }
                next = n.next as usize;
            }
            K::Tag => {
                push_raw(out, tree.slice(n.start, n.end));
                next = n.next as usize;
            }
            K::RawHtml
            | K::FootnoteRef
            | K::InlineFootnote
            | K::Embed
            | K::Comment
            | K::BlockId => next = n.next as usize,
            // Containers: their text children follow; syntax tokens render
            // nothing.
            K::Highlight
            | K::Emphasis
            | K::Strong
            | K::Strikethrough
            | K::Link
            | K::Image
            | K::Whitespace
            | K::Marker
            | K::CodeText
            | K::HtmlText
            | K::AutolinkText
            | K::MathText
            | K::LinkDestination
            | K::LinkTitle
            | K::LinkLabel
            | K::FootnoteLabel
            | K::CommentText
            | K::TagText
            | K::WikiTarget
            | K::WikiAlias
            | K::BlockIdText => {}
        }
        i = next;
    }
}

/// Appends a code span's content: line endings become spaces, and one space
/// is stripped from each end when both ends are spaces and the content is
/// not all spaces.
fn code_content(tree: &InlineTree, n: &Node, out: &mut String, escape: bool) {
    let s = tree.slice(n.start + n.data, n.end.saturating_sub(n.data));
    let b = s.as_bytes();
    let space = |c: u8| matches!(c, b' ' | b'\n' | b'\r');
    let mut lo = 0;
    let mut hi = b.len();
    if b.len() >= 2 && space(b[0]) && space(b[hi - 1]) && !b.iter().all(|&c| space(c)) {
        lo = if b.starts_with(b"\r\n") { 2 } else { 1 };
        hi -= if b.ends_with(b"\r\n") { 2 } else { 1 };
    }
    let content = s.get(lo..hi).unwrap_or("");
    let mut rest = content;
    while let Some(k) = rest.find(['\n', '\r']) {
        let (line, tail) = rest.split_at(k);
        if escape {
            escape_html(out, line);
        } else {
            push_raw(out, line);
        }
        out.push(' ');
        rest = tail.strip_prefix("\r\n").unwrap_or(&tail[1..]);
    }
    if escape {
        escape_html(out, rest);
    } else {
        push_raw(out, rest);
    }
}

/// Appends `s` with `&`, `<`, `>` and `"` escaped and U+0000 replaced.
pub(super) fn escape_html(out: &mut String, s: &str) {
    let b = s.as_bytes();
    let mut copied = 0;
    for (i, &c) in b.iter().enumerate() {
        let rep = match c {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            0 => "\u{FFFD}",
            _ => continue,
        };
        out.push_str(&s[copied..i]);
        out.push_str(rep);
        copied = i + 1;
    }
    out.push_str(&s[copied..]);
}

/// Appends `s` unescaped, with U+0000 replaced.
pub(crate) fn push_raw(out: &mut String, s: &str) {
    let mut parts = s.split('\0');
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for part in parts {
        out.push('\u{FFFD}');
        out.push_str(part);
    }
}

/// Appends the decoded text of an entity, HTML-escaped.
fn push_entity_escaped(value: u32, out: &mut String) {
    let mut s = String::new();
    scan::push_entity(value, &mut s);
    escape_html(out, &s);
}

/// Bytes kept as they are in URLs; the others are percent-encoded.
static HREF_SAFE: [bool; 256] = {
    let mut t = [false; 256];
    let safe = b"-_.+!*(),%#@?=;:/$~";
    let mut i = 0;
    while i < safe.len() {
        t[safe[i] as usize] = true;
        i += 1;
    }
    let mut c = 0;
    while c < 256 {
        if (c as u8).is_ascii_alphanumeric() {
            t[c] = true;
        }
        c += 1;
    }
    t
};

/// Appends a URL for an `href` or `src` attribute: unsafe bytes are
/// percent-encoded, `&` and `'` become entities, a `%` that does not start a
/// percent-escape is encoded, U+0000 is encoded as U+FFFD.
pub(crate) fn escape_href(out: &mut String, s: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let b = s.as_bytes();
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if HREF_SAFE[c as usize] && c != b'%' {
            i += 1;
            continue;
        }
        if copied < i {
            out.push_str(&s[copied..i]);
        }
        match c {
            b'&' => out.push_str("&amp;"),
            b'\'' => out.push_str("&#x27;"),
            b'%' if b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit) =>
            {
                out.push('%');
            }
            0 => out.push_str("%EF%BF%BD"),
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(c >> 4)]));
                out.push(char::from(HEX[usize::from(c & 15)]));
            }
        }
        i += 1;
        copied = i;
    }
    if copied < b.len() {
        out.push_str(&s[copied..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hrefs() {
        let h = |s: &str| {
            let mut out = String::new();
            escape_href(&mut out, s);
            out
        };
        assert_eq!(h("foo%20bä"), "foo%20b%C3%A4");
        assert_eq!(h("a b&c'd\"e"), "a%20b&amp;c&#x27;d%22e");
        assert_eq!(h("100%"), "100%25");
        assert_eq!(h("%zz"), "%25zz");
        assert_eq!(h("/[x]\\"), "/%5Bx%5D%5C");
        assert_eq!(h("\0"), "%EF%BF%BD");
    }

    #[test]
    fn tagfilter() {
        let f = |s: &str| {
            let mut out = String::new();
            push_filtered(&mut out, s);
            out
        };
        assert_eq!(f("<title>"), "&lt;title>");
        assert_eq!(f("</SCRIPT >"), "&lt;/SCRIPT >");
        assert_eq!(f("<xmp/>"), "&lt;xmp/>");
        assert_eq!(f("<xmp/"), "<xmp/");
        assert_eq!(f("<titles>"), "<titles>");
        assert_eq!(f("<style"), "<style");
        assert_eq!(f("a <em> <iframe\n>"), "a <em> &lt;iframe\n>");
    }

    #[test]
    fn escaping() {
        let mut out = String::new();
        escape_html(&mut out, "a<b>&\"'\0é");
        assert_eq!(out, "a&lt;b&gt;&amp;&quot;'\u{FFFD}é");
    }
}

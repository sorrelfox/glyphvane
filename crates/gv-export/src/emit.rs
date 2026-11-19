// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The rich emitter: a render tree as HTML, before sanitizing.
//!
//! Nodes come flat, each with its containers ([`Frame`]s, outermost
//! first). The emitter keeps the frames it has open and, at each node,
//! closes those the node is not in and opens its new ones, so the nesting
//! comes back. Inline content is a styled run list; the emitter turns the
//! styles back into properly nested elements (link, strong, emphasis,
//! strikethrough, highlight, code).
//!
//! Trusted fragments (formula and diagram SVG, task checkboxes) never pass
//! the sanitizer: they are held in [`Body::trusted`] and stand in the
//! output as `U+E000 nonce index U+E001`, with a random nonce, until
//! [`Body::restore`] puts them back.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use gv_highlight::CodeHighlighter;
use gv_inline::{Bias, ItemKind as RunKind};
use gv_model::LinkKind;
use gv_render_tree::{
    BlockNode, CodeBlock, Frame, FrameKind, InlineContent, InlineRole, LinkInfo, LinkStatus,
    ListMarker, NodeKind, RenderTree, TableBlock, Variant, decode_style_id,
};
use gv_syntax::BlockId;
use gv_syntax::table::Alignment;

use crate::escape::{escaped, text as esc};
use crate::sanitize::{ID_PREFIX, rewrite_link};
use crate::{ExportOptions, math};

/// A heading, for the title and the table of contents.
#[derive(Debug, Clone)]
pub(crate) struct Heading {
    pub level: u8,
    pub anchor: Option<String>,
    pub text: String,
}

/// The emitted body: HTML with placeholders, and what they stand for.
#[derive(Debug)]
pub(crate) struct Body {
    pub html: String,
    /// Where `html` may be cut into parts sanitized on their own, and
    /// whether the part before the cut holds raw HTML (see
    /// [`Body::parts`]).
    cuts: Vec<(usize, bool)>,
    trusted: Vec<String>,
    nonce: String,
    pub title: Option<String>,
    pub headings: Vec<Heading>,
}

impl Body {
    /// The body in parts that are sanitized one by one: each raw HTML
    /// region on its own, with the Markdown between regions in one part.
    /// A region is a top-level block with raw HTML and the blocks after it
    /// while its elements stay open (capped at [`REGION_BYTES`]), as the
    /// Reading view finds them; so an unclosed `<style>` or `<textarea>`
    /// in one region cannot swallow the rest of the document.
    pub(crate) fn parts(&self) -> Vec<&str> {
        let html = self.html.as_str();
        let mut parts = Vec::new();
        let (mut part, mut segment) = (0, 0);
        for &(at, raw) in &self.cuts {
            if at <= segment {
                continue;
            }
            if raw {
                if part < segment {
                    parts.push(&html[part..segment]);
                }
                parts.push(&html[segment..at]);
                part = at;
            }
            segment = at;
        }
        if part < html.len() {
            parts.push(&html[part..]);
        }
        parts
    }

    /// `clean` (the sanitized body) with the trusted fragments back in
    /// place. A placeholder that is not ours becomes U+FFFD.
    pub(crate) fn restore(&self, clean: &str) -> String {
        let mut out = String::with_capacity(
            clean.len() + self.trusted.iter().map(String::len).sum::<usize>(),
        );
        let mut rest = clean;
        while let Some(i) = rest.find('\u{E000}') {
            out.push_str(&rest[..i]);
            let after = &rest[i + '\u{E000}'.len_utf8()..];
            if let Some(j) = after.find('\u{E001}')
                && let Some(fragment) = after[..j]
                    .strip_prefix(self.nonce.as_str())
                    .and_then(|n| n.parse::<usize>().ok())
                    .and_then(|n| self.trusted.get(n))
            {
                out.push_str(fragment);
                rest = &after[j + '\u{E001}'.len_utf8()..];
                continue;
            }
            out.push('\u{FFFD}');
            rest = after;
        }
        out.push_str(rest);
        out
    }
}

/// An inline element the emitter keeps open, in nesting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Link(u32),
    Strong,
    Em,
    Del,
    Mark,
    Code,
}

struct Emitter<'a> {
    tree: &'a RenderTree,
    opts: &'a ExportOptions,
    range: Option<Range<usize>>,
    out: String,
    trusted: Vec<String>,
    nonce: String,
    /// The containers open now.
    stack: Vec<Frame>,
    footnotes_open: bool,
    /// Raw HTML elements open now (lower-case names), for region cuts.
    html_open: Vec<String>,
    /// Where the current region (or segment) started in `out`, and whether
    /// it holds raw HTML.
    segment: (usize, bool),
    cuts: Vec<(usize, bool)>,
    /// Diagrams emitted so far.
    diagrams: usize,
    /// `<details>` elements opened by their summary row.
    details: HashSet<BlockId>,
    /// References emitted so far, per footnote number.
    fn_refs: HashMap<u32, u32>,
    headings: Vec<Heading>,
}

/// Emits `tree`, or only what intersects `range` (source offsets) for a
/// copy.
pub(crate) fn emit(tree: &RenderTree, opts: &ExportOptions, range: Option<Range<usize>>) -> Body {
    let nonce = {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(tree.len());
        format!("gv{:016x}:", h.finish())
    };
    let mut e = Emitter {
        tree,
        opts,
        range,
        out: String::new(),
        trusted: Vec::new(),
        nonce,
        stack: Vec::new(),
        footnotes_open: false,
        html_open: Vec::new(),
        segment: (0, false),
        cuts: Vec::new(),
        diagrams: 0,
        details: HashSet::new(),
        fn_refs: HashMap::new(),
        headings: Vec::new(),
    };
    e.document();
    let title = e
        .headings
        .iter()
        .find(|h| h.level == 1)
        .or(e.headings.first())
        .map(|h| h.text.clone())
        .filter(|t| !t.trim().is_empty());
    Body {
        html: e.out,
        cuts: e.cuts,
        trusted: e.trusted,
        nonce: e.nonce,
        title,
        headings: e.headings,
    }
}

/// The footnote section's rule (the render tree's own block id for it).
fn is_footnote_rule(node: &BlockNode) -> bool {
    node.id.0 == u64::MAX
}

impl Emitter<'_> {
    fn trusted(&mut self, html: String) {
        self.out.push('\u{E000}');
        self.out.push_str(&self.nonce);
        self.out.push_str(&self.trusted.len().to_string());
        self.out.push('\u{E001}');
        self.trusted.push(html);
    }

    fn wanted(&self, node: &BlockNode) -> bool {
        let Some(r) = &self.range else {
            return true;
        };
        let s = &node.source;
        if s.is_empty() {
            r.start <= s.start && s.start < r.end
        } else {
            s.start < r.end && r.start < s.end
        }
    }

    /// Raw HTML goes out as written; its tags are followed for region
    /// cuts.
    fn raw_html(&mut self, html: &str) {
        self.out.push_str(html);
        self.segment.1 = true;
        // A raw text element left open here would take everything after
        // it as its text: it ends with the snippet.
        let closers = track_tags(html, &mut self.html_open);
        self.out.push_str(&closers);
    }

    /// At a top-level block boundary: ends the segment there when no raw
    /// HTML element is open (or the region is over its size cap).
    fn boundary(&mut self) {
        if !self.stack.is_empty() || self.footnotes_open {
            return;
        }
        let at = self.out.len();
        if self.html_open.is_empty() || at - self.segment.0 > REGION_BYTES {
            self.html_open.clear();
            self.cuts.push((at, self.segment.1));
            self.segment = (at, false);
        }
    }

    fn document(&mut self) {
        let tree = self.tree;
        for (ix, node) in tree.nodes().iter().enumerate() {
            if node.context.is_empty() {
                self.sync(&[]);
                self.boundary();
            }
            if is_footnote_rule(node) {
                if self.range.is_some() {
                    // A copy leaves the footnote section out.
                    break;
                }
                self.sync(&[]);
                self.out
                    .push_str("<section class=\"gv-footnotes\">\n<hr>\n<ol>\n");
                self.footnotes_open = true;
                continue;
            }
            if !self.wanted(node) {
                continue;
            }
            self.sync(&node.context);
            self.node(ix, node);
        }
        self.sync(&[]);
        self.boundary();
        if self.footnotes_open {
            self.out.push_str("</ol>\n</section>\n");
        }
    }

    /// Closes the open containers `context` is not in and opens its new
    /// ones.
    fn sync(&mut self, context: &[Frame]) {
        let common = self
            .stack
            .iter()
            .zip(context)
            .take_while(|(a, b)| a == b)
            .count();
        while self.stack.len() > common {
            if let Some(frame) = self.stack.pop() {
                self.close(&frame);
            }
        }
        for frame in &context[common..] {
            self.open(frame);
            self.stack.push(frame.clone());
        }
    }

    fn open(&mut self, frame: &Frame) {
        match &frame.kind {
            FrameKind::Quote { .. } => self.out.push_str("<blockquote>\n"),
            FrameKind::List {
                ordered: true,
                start,
                ..
            } => {
                if *start == 1 {
                    self.out.push_str("<ol>\n");
                } else {
                    self.out.push_str(&format!("<ol start=\"{start}\">\n"));
                }
            }
            FrameKind::List { .. } => self.out.push_str("<ul>\n"),
            FrameKind::Item {
                marker: ListMarker::Task { checked, .. },
            } => {
                self.out.push_str("<li class=\"gv-task\">");
                let checkbox = if *checked {
                    "<input type=\"checkbox\" disabled checked>"
                } else {
                    "<input type=\"checkbox\" disabled>"
                };
                self.trusted(checkbox.to_owned());
                self.out.push(' ');
            }
            FrameKind::Item { .. } => self.out.push_str("<li>"),
            FrameKind::Alert { kind } => {
                let name = kind.name();
                let mut title = name.to_owned();
                if let Some(first) = title.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                self.out.push_str(&format!(
                    "<div class=\"gv-callout gv-callout-{name}\">\n<p class=\"gv-callout-title\">{title}</p>\n"
                ));
            }
            FrameKind::Footnote { number, .. } => {
                self.out.push_str(&format!("<li id=\"fn-{number}\">\n"));
            }
            // Folded content without its own `<details>` element (a folded
            // callout): shown open, as a print-friendly page has no folds.
            FrameKind::Details => self.out.push_str("<div>\n"),
            FrameKind::Callout { name, .. } => self.out.push_str(&format!(
                "<div class=\"gv-callout gv-callout-{}\">\n",
                escaped(name)
            )),
            FrameKind::Embed { .. } => self.out.push_str("<div class=\"gv-embed\">\n"),
        }
    }

    fn close(&mut self, frame: &Frame) {
        self.out.push_str(match &frame.kind {
            FrameKind::Quote { .. } => "</blockquote>\n",
            FrameKind::List { ordered: true, .. } => "</ol>\n",
            FrameKind::List { .. } => "</ul>\n",
            FrameKind::Item { .. } | FrameKind::Footnote { .. } => "</li>\n",
            FrameKind::Alert { .. } | FrameKind::Callout { .. } | FrameKind::Embed { .. } => {
                "</div>\n"
            }
            FrameKind::Details if self.details.contains(&frame.id) => "</details>\n",
            FrameKind::Details => "</div>\n",
        });
    }

    /// A paragraph directly in an item of a tight list has no `<p>`.
    fn tight(node: &BlockNode) -> bool {
        let ctx = &node.context;
        matches!(ctx.last().map(|f| &f.kind), Some(FrameKind::Item { .. }))
            && ctx
                .iter()
                .rev()
                .find_map(|f| match f.kind {
                    FrameKind::List { tight, .. } => Some(tight),
                    _ => None,
                })
                .unwrap_or(false)
    }

    fn node(&mut self, ix: usize, node: &BlockNode) {
        match &node.kind {
            NodeKind::Paragraph(c) => {
                if Self::tight(node) {
                    self.inline(ix, node, c, 0..c.spans.len(), 0);
                    self.out.push('\n');
                } else {
                    self.out.push_str("<p>");
                    self.inline(ix, node, c, 0..c.spans.len(), 0);
                    self.out.push_str("</p>\n");
                }
            }
            NodeKind::Heading {
                level,
                anchor,
                content,
                ..
            } => {
                let level = (*level).clamp(1, 6);
                match anchor {
                    Some(a) => self
                        .out
                        .push_str(&format!("<h{level} id=\"{}\">", escaped(a))),
                    None => self.out.push_str(&format!("<h{level}>")),
                }
                self.inline(ix, node, content, 0..content.spans.len(), 0);
                self.out.push_str(&format!("</h{level}>\n"));
                self.headings.push(Heading {
                    level,
                    anchor: anchor.as_deref().map(str::to_owned),
                    text: plain(content),
                });
            }
            NodeKind::CalloutTitle { title, .. } => {
                self.out.push_str("<p class=\"gv-callout-title\">");
                self.inline(ix, node, title, 0..title.spans.len(), 0);
                self.out.push_str("</p>\n");
            }
            NodeKind::Code(c) => self.code(node, c),
            NodeKind::Math(c) => {
                let macros = self.tree.math_macros(ix, 0);
                match math::svg(&c.text, true, &macros) {
                    Ok(svg) => {
                        self.out.push_str("<div class=\"gv-math-block\">");
                        self.trusted(svg);
                        self.out.push_str("</div>\n");
                    }
                    Err(_) => {
                        self.out.push_str("<pre class=\"gv-math-error\"><code>");
                        esc(&mut self.out, &c.text);
                        self.out.push_str("</code></pre>\n");
                    }
                }
            }
            NodeKind::Diagram(c) => {
                match self.opts.diagrams.as_ref().and_then(|d| d.svg(&c.text)) {
                    Some(svg) => {
                        // Inline SVGs share the page's ids: each diagram's
                        // get their own prefix.
                        self.diagrams += 1;
                        let prefix = format!("gv-d{}-", self.diagrams);
                        let svg = svg
                            .replace("id=\"", &format!("id=\"{prefix}"))
                            .replace("url(#", &format!("url(#{prefix}"))
                            .replace("href=\"#", &format!("href=\"#{prefix}"));
                        self.out.push_str("<div class=\"gv-diagram\">");
                        self.trusted(svg);
                        self.out.push_str("</div>\n");
                    }
                    None => {
                        self.out.push_str("<pre class=\"gv-diagram-source\"><code>");
                        esc(&mut self.out, &c.text);
                        self.out.push_str("</code></pre>\n");
                    }
                }
            }
            NodeKind::Table(t) => self.table(ix, node, t),
            NodeKind::ThematicBreak => self.out.push_str("<hr>\n"),
            NodeKind::Html { code, from, .. } => {
                // Raw HTML as written; the sanitizer decides what stays.
                // Inside a note embed its relative URLs are the embedded
                // note's.
                let html = self.embedded_html(&code.text, from.as_deref());
                self.raw_html(&html);
                self.out.push('\n');
            }
            NodeKind::Properties { props, .. } => {
                if props.is_empty() {
                    return;
                }
                self.out
                    .push_str("<table class=\"gv-properties\">\n<tbody>\n");
                for p in props.iter() {
                    self.out.push_str("<tr><th>");
                    esc(&mut self.out, &p.key);
                    self.out.push_str("</th><td>");
                    let values: Vec<&str> = p.values.iter().map(|v| &**v).collect();
                    esc(&mut self.out, &values.join(", "));
                    self.out.push_str("</td></tr>\n");
                }
                self.out.push_str("</tbody>\n</table>\n");
            }
            NodeKind::EmptyItem | NodeKind::EmptyQuote => {}
            NodeKind::Details { summary, open } => {
                self.out.push_str(if *open {
                    "<details open>\n<summary>"
                } else {
                    "<details>\n<summary>"
                });
                self.inline(ix, node, summary, 0..summary.spans.len(), 0);
                self.out.push_str("</summary>\n");
                self.details.insert(node.id);
                self.stack.push(Frame {
                    id: node.id,
                    kind: FrameKind::Details,
                });
            }
            NodeKind::Picture(p) => {
                // Exports are light: the light source, else the fallback.
                if let Some(src) = p.pick(false) {
                    let src = embedded_image_src(self.opts, p.from.as_deref(), src);
                    self.out.push_str("<p><img src=\"");
                    esc(&mut self.out, &src);
                    self.out.push_str("\" alt=\"");
                    esc(&mut self.out, &p.alt);
                    self.out.push_str("\"></p>\n");
                }
            }
            // A live block (canvas or base embed, base or query fence) is
            // not drawn in exports: the embed's line as a paragraph (its
            // link), or the fence as a code block.
            NodeKind::Live(live) => {
                if let (Some(_), Some(c)) = (&live.html, &live.code) {
                    // An `<audio>`, `<video>` or `<iframe>` block: raw
                    // HTML, as written; the sanitizer decides what stays.
                    let html = self.embedded_html(&c.text, live.host.as_deref());
                    self.raw_html(&html);
                    self.out.push('\n');
                } else if let Some(c) = &live.label {
                    self.out.push_str("<p>");
                    self.inline(ix, node, c, 0..c.spans.len(), 0);
                    self.out.push_str("</p>\n");
                } else if let Some(c) = &live.code {
                    self.code(node, c);
                }
            }
        }
    }

    /// Raw HTML of the note `from` (inside a note embed; `None`: the
    /// document's) with its relative URLs rebased onto the document.
    fn embedded_html<'h>(&self, html: &'h str, from: Option<&str>) -> std::borrow::Cow<'h, str> {
        match from {
            None => std::borrow::Cow::Borrowed(html),
            Some(from) => std::borrow::Cow::Owned(rebase_html(html, |url| {
                embedded_image_src(self.opts, Some(from), url)
            })),
        }
    }

    fn code(&mut self, node: &BlockNode, c: &CodeBlock) {
        // A copy takes the selected lines.
        let text_range = match &self.range {
            None => 0..c.text.len(),
            Some(r) => {
                let local = node.to_local(r.start)..node.to_local(r.end);
                let lines: Vec<_> = c
                    .lines
                    .iter()
                    .filter(|l| l.source.start <= local.end && local.start <= l.source.end)
                    .collect();
                match (lines.first(), lines.last()) {
                    (Some(a), Some(b)) => a.text.start..b.text.end,
                    _ => 0..c.text.len(),
                }
            }
        };
        let info = c.info.as_deref().unwrap_or("");
        let highlight = CodeHighlighter::shared().highlight(&c.text, info);
        self.out.push_str("<pre class=\"gv-code\"><code>");
        let mut at = text_range.start;
        for span in &highlight.spans {
            let (a, b) = (
                span.start.max(text_range.start),
                span.end.min(text_range.end),
            );
            if a >= b || span.class.name() == "plain" {
                continue;
            }
            esc(&mut self.out, &c.text[at..a]);
            self.out
                .push_str(&format!("<span class=\"hl-{}\">", span.class.name()));
            esc(&mut self.out, &c.text[a..b]);
            self.out.push_str("</span>");
            at = b;
        }
        if at < text_range.end {
            esc(&mut self.out, &c.text[at..text_range.end]);
        }
        self.out.push_str("</code></pre>\n");
    }

    fn table(&mut self, ix: usize, node: &BlockNode, t: &TableBlock) {
        self.out.push_str("<table>\n");
        let mut math_base = 0;
        let mut head_open = false;
        let mut body_open = false;
        for row in t.rows.iter() {
            let base = math_base;
            math_base += row.content.math.len();
            if let Some(r) = &self.range {
                let src = node.to_document(row.source.start)..node.to_document(row.source.end);
                if !(src.start < r.end && r.start < src.end) {
                    continue;
                }
            }
            if row.header && !head_open {
                self.out.push_str("<thead>\n");
                head_open = true;
            }
            if !row.header && !body_open {
                if head_open {
                    self.out.push_str("</thead>\n");
                    head_open = false;
                }
                self.out.push_str("<tbody>\n");
                body_open = true;
            }
            let tag = if row.header { "th" } else { "td" };
            self.out.push_str("<tr>");
            let spans = &row.content.spans;
            // Cells sit between the gap boxes.
            let gaps: Vec<(usize, u32)> = spans
                .iter()
                .enumerate()
                .filter_map(|(i, s)| match s.role {
                    InlineRole::CellGap(col) => Some((i, col)),
                    _ => None,
                })
                .collect();
            for pair in gaps.windows(2) {
                let ((a, col), (b, _)) = (pair[0], pair[1]);
                let align = match t.aligns.get(col as usize) {
                    Some(Alignment::Left) => " align=\"left\"",
                    Some(Alignment::Center) => " align=\"center\"",
                    Some(Alignment::Right) => " align=\"right\"",
                    _ => "",
                };
                self.out.push_str(&format!("<{tag}{align}>"));
                self.inline(ix, node, &row.content, a + 1..b, base);
                self.out.push_str(&format!("</{tag}>"));
            }
            self.out.push_str("</tr>\n");
        }
        if head_open {
            self.out.push_str("</thead>\n");
        }
        if body_open {
            self.out.push_str("</tbody>\n");
        }
        self.out.push_str("</table>\n");
    }

    /// The href of a link, or `None` when it is shown as plain text:
    /// unresolved links, tags, and targets an export cannot link to.
    fn link_target(&self, link: &LinkInfo) -> Option<String> {
        if matches!(
            link.status,
            LinkStatus::Unresolved | LinkStatus::OutsideRoot | LinkStatus::Blocked
        ) {
            return None;
        }
        if let Some(from) = &link.from {
            return self.embedded_link_target(link, from);
        }
        match link.kind {
            LinkKind::Wikilink | LinkKind::Embed => {
                let target = link.destination.split('|').next().unwrap_or_default();
                let slugs = self.tree.options().slugs;
                wikilink_href(target, self.opts, |heading| slugs.slug(heading))
            }
            LinkKind::Tag => None,
            _ => rewrite_link(&link.destination),
        }
    }

    /// The href of a link inside a note embed, resolved from the embedded
    /// note `from` (its key: a path below the vault root, or below the
    /// document's folder without one) and made relative to the exported
    /// document: links to the note itself (`[[#Heading]]`, `#id`) and
    /// relative links point to the note's `.html` (or the file) where it
    /// is beside the export; wikilinks resolve by name as in the
    /// document. `None` (plain text) when that place is not known.
    fn embedded_link_target(&self, link: &LinkInfo, from: &str) -> Option<String> {
        // The embedded note's folder seen from the document's (`""`: the
        // same folder), and its file name.
        let place = || -> Option<(String, String)> {
            let host = self.opts.base_dir()?;
            let note = self.opts.root.as_deref().unwrap_or(host).join(from);
            let note_dir = note.parent()?;
            let prefix = if same_dir(host, note_dir) {
                String::new()
            } else {
                format!("{}/", relative_path(host, note_dir)?)
            };
            Some((prefix, note.file_name()?.to_str()?.to_owned()))
        };
        let slugs = self.tree.options().slugs;
        let heading = |anchor: &str| {
            (!anchor.is_empty() && !anchor.starts_with('^')).then(|| {
                let last = anchor.rsplit('#').next().unwrap_or(anchor);
                format!("#{ID_PREFIX}{}", slugs.slug(last))
            })
        };
        match link.kind {
            LinkKind::Tag => None,
            LinkKind::Wikilink | LinkKind::Embed => {
                let target = link.destination.split('|').next().unwrap_or_default();
                let (file, anchor) = target.split_once('#').unwrap_or((target, ""));
                if file.trim().is_empty() {
                    let (prefix, name) = place()?;
                    return Some(note_href(format!("{prefix}{name}"), heading(anchor)));
                }
                if self.opts.wiki.is_some() {
                    return wikilink_href(target, self.opts, |h| slugs.slug(h));
                }
                // Without a vault: the note of that name beside the
                // embedded note.
                let (prefix, _) = place()?;
                let file = if Path::new(file).extension().is_some() {
                    file.to_owned()
                } else {
                    format!("{file}.md")
                };
                Some(note_href(format!("{prefix}{file}"), heading(anchor)))
            }
            _ => {
                let dest = link.destination.trim();
                match gv_vault::url::classify(dest) {
                    gv_vault::url::UrlClass::Fragment(id) => {
                        let (prefix, name) = place()?;
                        let id = id.strip_prefix(ID_PREFIX).unwrap_or(&id);
                        let fragment = (!id.is_empty()).then(|| format!("#{ID_PREFIX}{id}"));
                        Some(note_href(format!("{prefix}{name}"), fragment))
                    }
                    gv_vault::url::UrlClass::Relative(path) if !path.starts_with('/') => {
                        let (prefix, _) = place()?;
                        rewrite_link(&format!("{prefix}{dest}"))
                    }
                    _ => rewrite_link(dest),
                }
            }
        }
    }

    fn open_tag(&mut self, tag: Tag, c: &InlineContent) {
        match tag {
            Tag::Link(i) => {
                let Some(link) = c.links.get(i as usize) else {
                    self.out.push_str("<span>");
                    return;
                };
                match self.link_target(link) {
                    Some(href) => {
                        self.out.push_str("<a href=\"");
                        esc(&mut self.out, &href);
                        self.out.push('"');
                        if let Some(title) = &link.title {
                            self.out.push_str(" title=\"");
                            esc(&mut self.out, title);
                            self.out.push('"');
                        }
                        self.out.push('>');
                    }
                    None if link.kind == LinkKind::Tag => {
                        self.out.push_str("<span class=\"gv-tag\">");
                    }
                    None => self.out.push_str("<span class=\"gv-unresolved\">"),
                }
            }
            Tag::Strong => self.out.push_str("<strong>"),
            Tag::Em => self.out.push_str("<em>"),
            Tag::Del => self.out.push_str("<del>"),
            Tag::Mark => self.out.push_str("<mark>"),
            Tag::Code => self.out.push_str("<code>"),
        }
    }

    fn close_tag(&mut self, tag: Tag, c: &InlineContent) {
        self.out.push_str(match tag {
            Tag::Link(i) => match c.links.get(i as usize).and_then(|l| self.link_target(l)) {
                Some(_) => "</a>",
                None => "</span>",
            },
            Tag::Strong => "</strong>",
            Tag::Em => "</em>",
            Tag::Del => "</del>",
            Tag::Mark => "</mark>",
            Tag::Code => "</code>",
        });
    }

    /// Opens and closes inline elements so that exactly `want` is open.
    fn set_tags(&mut self, open: &mut Vec<Tag>, want: &[Tag], c: &InlineContent) {
        let common = open.iter().zip(want).take_while(|(a, b)| a == b).count();
        while open.len() > common {
            if let Some(tag) = open.pop() {
                self.close_tag(tag, c);
            }
        }
        for &tag in &want[common..] {
            self.open_tag(tag, c);
            open.push(tag);
        }
    }

    /// The logical text range of item `k` to emit (all of it, or the part
    /// inside the copied range); `None` when nothing of it is wanted.
    fn clip(&self, node: &BlockNode, c: &InlineContent, k: usize) -> Option<Range<usize>> {
        let para = &c.paragraph;
        let item = &para.items()[k];
        let Some(r) = &self.range else {
            return Some(item.range());
        };
        let src = item.source();
        let doc = node.to_document(src.start)..node.to_document(src.end);
        let inside = if doc.is_empty() {
            r.start <= doc.start && doc.start < r.end
        } else {
            doc.start < r.end && r.start < doc.end
        };
        if !inside {
            return None;
        }
        if !matches!(item.kind(), RunKind::Text(_)) || (r.start <= doc.start && doc.end <= r.end) {
            return Some(item.range());
        }
        let (a, _) = para.logical_offset(node.to_local(r.start), Bias::Left);
        let (b, _) = para.logical_offset(node.to_local(r.end), Bias::Right);
        let range = item.range();
        let (a, b) = (a.max(range.start), b.min(range.end));
        (a < b).then_some(a..b)
    }

    /// Emits items `items` of `c` (all, or one table cell's). `math_base`
    /// is the formula number of the first formula of `c` in its node.
    fn inline(
        &mut self,
        ix: usize,
        node: &BlockNode,
        c: &InlineContent,
        items: Range<usize>,
        math_base: usize,
    ) {
        let para = c.paragraph.clone();
        let mut open: Vec<Tag> = Vec::new();
        let mut images_done: HashSet<u32> = HashSet::new();
        for k in items {
            let Some(span) = c.spans.get(k) else {
                break;
            };
            let Some(range) = self.clip(node, c, k) else {
                continue;
            };
            let item = &para.items()[k];
            let mut want: Vec<Tag> = Vec::new();
            let link = span.link;
            match span.role {
                InlineRole::Text | InlineRole::Code | InlineRole::Tag => {
                    if let Some(l) = link {
                        want.push(Tag::Link(l));
                    }
                    let style = item.style();
                    let variant = style
                        .and_then(|s| decode_style_id(s.font))
                        .map_or(Variant::PLAIN, |(_, v)| v);
                    let mono = variant.contains(Variant::MONO) || span.role == InlineRole::Code;
                    if variant.contains(Variant::STRONG) {
                        want.push(Tag::Strong);
                    }
                    if variant.contains(Variant::ITALIC) {
                        want.push(Tag::Em);
                    }
                    if style.is_some_and(|s| s.strikethrough.is_some()) {
                        want.push(Tag::Del);
                    }
                    // A tag's pill is not a highlight.
                    if !mono
                        && span.role != InlineRole::Tag
                        && style.is_some_and(|s| s.background.is_some())
                    {
                        want.push(Tag::Mark);
                    }
                    if mono {
                        want.push(Tag::Code);
                    }
                    self.set_tags(&mut open, &want, c);
                    esc(&mut self.out, &para.text()[range]);
                }
                InlineRole::Break => {
                    self.set_tags(&mut open, &[], c);
                    self.out.push_str("<br>\n");
                }
                InlineRole::RawHtml => {
                    // As written; the sanitizer pairs and filters it.
                    self.raw_html(&para.text()[range]);
                }
                InlineRole::Image(i) | InlineRole::ImageAlt(i) => {
                    if !images_done.insert(i) {
                        continue;
                    }
                    let Some(image) = c.images.get(i as usize) else {
                        continue;
                    };
                    if let Some(l) = image.link.or(link) {
                        want.push(Tag::Link(l));
                    }
                    self.set_tags(&mut open, &want, c);
                    let src =
                        embedded_image_src(self.opts, image.from.as_deref(), &image.destination);
                    self.out.push_str("<img src=\"");
                    esc(&mut self.out, &src);
                    self.out.push_str("\" alt=\"");
                    esc(&mut self.out, &image.alt);
                    self.out.push('"');
                    if let Some(title) = &image.title {
                        self.out.push_str(" title=\"");
                        esc(&mut self.out, title);
                        self.out.push('"');
                    }
                    self.out.push('>');
                }
                InlineRole::LinkSuffix | InlineRole::CellGap(_) => {}
                InlineRole::Math(i) => {
                    let Some(m) = c.math.get(i as usize) else {
                        continue;
                    };
                    if let Some(l) = link {
                        want.push(Tag::Link(l));
                    }
                    self.set_tags(&mut open, &want, c);
                    let macros = self.tree.math_macros(ix, math_base + i as usize);
                    match math::svg(&m.tex, m.display, &macros) {
                        Ok(svg) => {
                            self.out.push_str("<span class=\"gv-math\">");
                            self.trusted(svg);
                            self.out.push_str("</span>");
                        }
                        Err(_) => {
                            self.out.push_str("<code class=\"gv-math-error\">");
                            esc(&mut self.out, &m.tex);
                            self.out.push_str("</code>");
                        }
                    }
                }
                InlineRole::FootnoteRef(i) => {
                    let Some(f) = c.footnotes.get(i as usize) else {
                        continue;
                    };
                    // Never inside a link.
                    let keep: Vec<Tag> = open
                        .iter()
                        .copied()
                        .filter(|t| !matches!(t, Tag::Link(_)))
                        .collect();
                    self.set_tags(&mut open, &keep, c);
                    let n = f.number;
                    if f.embedded {
                        // A note embed's footnote: listed at the embed's
                        // end, not in the document's footnotes.
                        self.out
                            .push_str(&format!("<sup class=\"gv-fnref\">{n}</sup>"));
                        continue;
                    }
                    let k = {
                        let count = self.fn_refs.entry(n).or_insert(0);
                        *count += 1;
                        *count
                    };
                    self.out.push_str(&format!(
                        "<sup class=\"gv-fnref\"><a href=\"#{ID_PREFIX}fn-{n}\" id=\"fnref-{n}-{k}\">{n}</a></sup>"
                    ));
                }
                InlineRole::FootnoteBackref(k) => {
                    let number = self.stack.iter().rev().find_map(|f| match f.kind {
                        FrameKind::Footnote { number, .. } => Some(number),
                        _ => None,
                    });
                    let Some(n) = number else {
                        continue;
                    };
                    self.set_tags(&mut open, &[], c);
                    self.out.push_str(&format!(
                        " <a href=\"#{ID_PREFIX}fnref-{n}-{}\" class=\"gv-backref\" aria-label=\"Back to reference {}\">\u{21a9}</a>",
                        k + 1,
                        k + 1
                    ));
                }
            }
        }
        self.set_tags(&mut open, &[], c);
    }
}

/// The largest raw HTML region sanitized as one part.
const REGION_BYTES: usize = 1 << 20;

/// Elements that hold a region open: the sanitizer's allowlist, without
/// void elements and `p` (which the HTML parser closes by itself).
const BALANCED: &[&str] = &[
    "div",
    "span",
    "center",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "main",
    "nav",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "blockquote",
    "pre",
    "code",
    "kbd",
    "samp",
    "var",
    "tt",
    "b",
    "strong",
    "i",
    "em",
    "u",
    "s",
    "strike",
    "del",
    "ins",
    "mark",
    "small",
    "sub",
    "sup",
    "q",
    "cite",
    "dfn",
    "abbr",
    "font",
    "a",
    "picture",
    "details",
    "summary",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "th",
    "td",
    "caption",
    "colgroup",
    "figure",
    "figcaption",
    "ruby",
    "rt",
    "rp",
    "audio",
    "video",
];

/// Elements whose content is text up to their end tag.
const RAW_TEXT: &[&str] = &[
    "script",
    "style",
    "textarea",
    "title",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "noscript",
    "plaintext",
];

/// Follows the start and end tags of `html` on `open` (the simplified
/// open-element stack of a region): a start tag of a [`BALANCED`] element
/// opens it, an end tag closes up to the nearest open element of its name;
/// other elements are ignored. Comments and the text of raw text elements
/// are skipped. Returns the end tags that close what `html` leaves open
/// and must not reach past it: a raw text element, then foreign content
/// (`svg`, `math`), whose elements no longer count for the region.
fn track_tags(html: &str, open: &mut Vec<String>) -> String {
    let b = html.as_bytes();
    let mut i = 0;
    // Foreign roots open: name and the stack depth when they opened.
    let mut foreign: Vec<(&'static str, usize)> = Vec::new();
    let mut closers = String::new();
    while let Some(k) = html[i..].find('<').map(|k| i + k) {
        i = k + 1;
        if html[i..].starts_with("!--") {
            i = html[i..].find("-->").map_or(b.len(), |e| i + e + 3);
            continue;
        }
        let end = b.get(i) == Some(&b'/');
        let start = i + usize::from(end);
        let mut j = start;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        if j == start || !b[start].is_ascii_alphabetic() {
            continue;
        }
        let name = html[start..j].to_ascii_lowercase();
        let gt = html[j..].find('>').map_or(b.len(), |e| j + e);
        let self_closing = gt > j && b[gt - 1] == b'/';
        i = gt.min(b.len());
        if end {
            if let Some(p) = foreign.iter().rposition(|(n, _)| *n == name) {
                open.truncate(foreign[p].1);
                foreign.truncate(p);
            } else if let Some(p) = open.iter().rposition(|n| *n == name) {
                open.truncate(p);
            }
        } else if let Some(raw) = RAW_TEXT.iter().find(|r| **r == name) {
            let close = format!("</{name}");
            match html[i..].to_ascii_lowercase().find(&close) {
                Some(e) => i += e,
                None => {
                    closers.push_str(&format!("</{raw}>"));
                    break;
                }
            }
        } else if let Some(root) = ["svg", "math"].iter().find(|r| **r == name) {
            if !self_closing {
                foreign.push((root, open.len()));
            }
        } else if BALANCED.contains(&name.as_str()) && !self_closing {
            open.push(name);
        }
    }
    while let Some((root, depth)) = foreign.pop() {
        open.truncate(depth);
        closers.push_str(&format!("</{root}>"));
    }
    closers
}

/// The text of inline content, as the outline shows it.
fn plain(c: &InlineContent) -> String {
    let para = &c.paragraph;
    let mut s = String::new();
    for (k, span) in c.spans.iter().enumerate() {
        match span.role {
            InlineRole::Text | InlineRole::Code | InlineRole::ImageAlt(_) => {
                s.push_str(para.item_text(k))
            }
            InlineRole::Math(i) => {
                if let Some(m) = c.math.get(i as usize) {
                    s.push_str(&m.tex);
                }
            }
            InlineRole::Break => s.push(' '),
            _ => {}
        }
    }
    s.trim().to_owned()
}

/// A table of contents for `headings` (escaped; links carry the id
/// prefix, as the sanitized headings do).
pub(crate) fn toc(headings: &[Heading]) -> String {
    let min = headings.iter().map(|h| h.level).min().unwrap_or(1);
    let mut s = String::from("<nav class=\"gv-toc\">\n");
    let mut depth = 0u8;
    for h in headings {
        let level = h.level - min + 1;
        while depth < level {
            s.push_str("<ul>\n");
            depth += 1;
        }
        while depth > level {
            s.push_str("</ul>\n");
            depth -= 1;
        }
        match &h.anchor {
            Some(a) => s.push_str(&format!(
                "<li><a href=\"#{}\">{}</a></li>\n",
                escaped(&format!("{ID_PREFIX}{a}")),
                escaped(&h.text)
            )),
            None => s.push_str(&format!("<li>{}</li>\n", escaped(&h.text))),
        }
    }
    while depth > 0 {
        s.push_str("</ul>\n");
        depth -= 1;
    }
    s.push_str("</nav>\n");
    s
}

/// The href of a wikilink `[[target#anchor]]` in an export of the
/// document at `opts.doc_path`: the resolved note's `.html`, relative to
/// the document's folder, with the heading's anchor (by `slug`); `None`
/// when the target does not resolve (the link becomes plain text).
pub fn wikilink_href(
    target: &str,
    opts: &ExportOptions,
    slug: impl Fn(&str) -> String,
) -> Option<String> {
    let (file, anchor) = match target.split_once('#') {
        Some((f, a)) => (f, Some(a)),
        None => (target, None),
    };
    let fragment = anchor
        .filter(|a| !a.starts_with('^') && !a.is_empty())
        .map(|a| format!("#{ID_PREFIX}{}", slug(a.rsplit('#').next().unwrap_or(a))));
    if file.is_empty() {
        return fragment;
    }
    let rel = match &opts.wiki {
        Some(wiki) => {
            let resolved = wiki.resolve(file)?;
            relative_path(opts.base_dir()?, &resolved)?
        }
        // Without a vault: the note of that name beside the document.
        None if Path::new(file).extension().is_some() => file.to_owned(),
        None => format!("{file}.md"),
    };
    Some(note_href(rel, fragment))
}

/// The href of a note at `rel` (a path relative to the export, `/`
/// separated): a Markdown file becomes its `.html`, the characters a URL
/// path cannot hold are encoded, then `fragment` (with its `#`).
fn note_href(mut rel: String, fragment: Option<String>) -> String {
    if let Some(stem) = [".md", ".markdown"].iter().find_map(|ext| {
        rel.to_ascii_lowercase()
            .ends_with(ext)
            .then(|| rel[..rel.len() - ext.len()].to_owned())
    }) {
        rel = format!("{stem}.html");
    }
    let mut href: String = rel
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_owned(),
            '#' => "%23".to_owned(),
            '?' => "%3F".to_owned(),
            '%' => "%25".to_owned(),
            c => c.to_string(),
        })
        .collect();
    if let Some(f) = fragment {
        href.push_str(&f);
    }
    href
}

/// The address of image `dest` of the embedded note `from` (its key: a
/// path below the vault root, or below the document's folder without one)
/// seen from the exported document: a relative path goes through the
/// note's folder; anything else, and an image of the document (`from`
/// `None`), stays as written.
pub(crate) fn embedded_image_src(opts: &ExportOptions, from: Option<&str>, dest: &str) -> String {
    let Some(from) = from else {
        return dest.to_owned();
    };
    let trimmed = dest.trim();
    match gv_vault::url::classify(trimmed) {
        gv_vault::url::UrlClass::Relative(path) if !path.starts_with('/') => {}
        _ => return dest.to_owned(),
    }
    let Some(host) = opts.base_dir() else {
        return dest.to_owned();
    };
    let note = opts.root.as_deref().unwrap_or(host).join(from);
    let Some(note_dir) = note.parent() else {
        return dest.to_owned();
    };
    if same_dir(host, note_dir) {
        return trimmed.to_owned();
    }
    match relative_path(host, note_dir) {
        Some(prefix) => format!("{prefix}/{trimmed}"),
        None => dest.to_owned(),
    }
}

/// `html` with the URLs of its `src`, `href`, `poster` and `srcset`
/// attributes passed through `rebase` (which gets each URL as written,
/// entities included, and returns it unchanged or with a path before it).
pub(crate) fn rebase_html(html: &str, rebase: impl Fn(&str) -> String) -> String {
    let b = html.as_bytes();
    let mut out = String::with_capacity(html.len() + 16);
    let mut copied = 0;
    let mut i = 0;
    while let Some(lt) = html[i..].find('<').map(|p| i + p) {
        if html[lt..].starts_with("<!--") {
            i = html[lt..].find("-->").map_or(b.len(), |p| lt + p + 3);
            continue;
        }
        let mut j = lt + 1;
        if !b.get(j).is_some_and(u8::is_ascii_alphabetic) {
            i = j;
            continue;
        }
        while b
            .get(j)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-')
        {
            j += 1;
        }
        // Attributes up to the tag's end.
        loop {
            while b
                .get(j)
                .is_some_and(|c| c.is_ascii_whitespace() || *c == b'/')
            {
                j += 1;
            }
            match b.get(j) {
                None | Some(b'>') => break,
                _ => {}
            }
            let name_start = j;
            while b
                .get(j)
                .is_some_and(|c| !c.is_ascii_whitespace() && !matches!(c, b'=' | b'>' | b'/'))
            {
                j += 1;
            }
            let name = html[name_start..j].to_ascii_lowercase();
            if name_start == j {
                j += 1;
                continue;
            }
            let mut k = j;
            while b.get(k).is_some_and(u8::is_ascii_whitespace) {
                k += 1;
            }
            if b.get(k) != Some(&b'=') {
                continue;
            }
            k += 1;
            while b.get(k).is_some_and(u8::is_ascii_whitespace) {
                k += 1;
            }
            let (value, end) = match b.get(k) {
                Some(&q) if q == b'"' || q == b'\'' => {
                    let close = html[k + 1..].find(q as char).map_or(b.len(), |p| k + 1 + p);
                    (k + 1..close, (close + 1).min(b.len()))
                }
                _ => {
                    let mut e = k;
                    while b
                        .get(e)
                        .is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>')
                    {
                        e += 1;
                    }
                    (k..e, e)
                }
            };
            j = end;
            let value_text = &html[value.clone()];
            let new = match name.as_str() {
                "src" | "href" | "poster" => rebase_one(value_text, &rebase),
                "srcset" => value_text
                    .split(',')
                    .map(|c| {
                        let lead = c.len() - c.trim_start().len();
                        let t = c.trim_start();
                        let url_end = t.find(char::is_whitespace).unwrap_or(t.len());
                        format!(
                            "{}{}{}",
                            &c[..lead],
                            rebase_one(&t[..url_end], &rebase),
                            &t[url_end..]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                _ => continue,
            };
            if new != value_text {
                out.push_str(&html[copied..value.start]);
                out.push_str(&new);
                copied = value.end;
            }
        }
        i = j;
    }
    out.push_str(&html[copied..]);
    out
}

/// One URL of [`rebase_html`]: what `rebase` puts before it, escaped.
fn rebase_one(url: &str, rebase: &impl Fn(&str) -> String) -> String {
    let trimmed = url.trim();
    let new = rebase(url);
    match new.strip_suffix(trimmed) {
        Some(prefix) if new != url => format!("{}{trimmed}", escaped(prefix)),
        _ => url.to_owned(),
    }
}

/// Whether two folders are the same (after resolving links when they
/// exist).
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    norm(a) == norm(b)
}

/// `to` relative to the folder `from`, with `/` separators.
fn relative_path(from: &Path, to: &Path) -> Option<String> {
    let norm =
        |p: &Path| -> PathBuf { std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()) };
    let (from, to) = (norm(from), norm(to));
    let a: Vec<Component<'_>> = from.components().collect();
    let b: Vec<Component<'_>> = to.components().collect();
    if a.first() != b.first() {
        return None;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".to_owned(); a.len() - common];
    parts.extend(
        b[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    (!parts.is_empty()).then(|| parts.join("/"))
}

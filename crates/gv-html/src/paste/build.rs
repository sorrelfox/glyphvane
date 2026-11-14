// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! HTML tree → intermediate document.
//!
//! The walker is the sanitizer: elements without a mapping are either
//! transparent (their children are converted) or dropped with their
//! content (scripts, styles, form controls, embedded objects, SVG). URL
//! attributes go through the classifier; nothing else of the input
//! survives.

use crate::dom::{Dom, NodeData, NodeId};
use crate::url::{Dest, classify_with_base};

use super::ir::{Align, Block, Cell, Inline, Item, Link, List, Run, Style};

/// Deepest HTML nesting walked recursively; deeper content becomes text.
const MAX_WALK_DEPTH: usize = 192;
/// Deepest list / quote nesting produced; deeper ones are flattened.
const MAX_CONTAINER_DEPTH: usize = 24;
/// Widest table (columns), after `colspan` expansion.
const MAX_COLUMNS: usize = 64;

/// Dropped together with their content.
const DROPPED: &[&str] = &[
    "applet", "area", "audio", "base", "basefont", "bgsound", "button", "canvas", "datalist",
    "dialog", "embed", "frame", "frameset", "head", "iframe", "link", "map", "meta", "meter",
    "noembed", "noframes", "noscript", "object", "optgroup", "option", "param", "progress", "rp",
    "script", "select", "slot", "source", "style", "template", "textarea", "title", "track",
    "video",
];

/// Block-level elements without a Markdown construct of their own: a
/// paragraph boundary before and after.
const PLAIN_BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "body",
    "center",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hgroup",
    "html",
    "legend",
    "li",
    "main",
    "nav",
    "p",
    "search",
    "section",
    "summary",
    "caption",
];

/// What a `style` attribute says (only the properties the converter uses).
#[derive(Debug, Default)]
struct Decl {
    bold: Option<bool>,
    italic: Option<bool>,
    strike: bool,
    sub: bool,
    sup: bool,
    mono: bool,
    hidden: bool,
    align: Align,
    /// `mso-list: l0 level2 lfo1` → `Some(2)`.
    mso_level: Option<u32>,
    /// `mso-list: l0 level2 lfo1` → `l0 lfo1` (which list).
    mso_key: String,
    /// `mso-list: Ignore` (Word's bullet text).
    mso_ignore: bool,
}

const MONO_FAMILIES: &[&str] = &[
    "monospace",
    "courier",
    "consolas",
    "menlo",
    "monaco",
    "source code",
    "roboto mono",
    "fira code",
    "fira mono",
    "jetbrains mono",
    "sf mono",
    "sfmono",
    "ubuntu mono",
    "dejavu sans mono",
    "liberation mono",
    "lucida console",
    "andale mono",
    "cascadia",
    "inconsolata",
];

fn parse_style(style: &str) -> Decl {
    let mut d = Decl::default();
    for decl in style.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().to_ascii_lowercase();
        let v = v.trim_end_matches("!important").trim();
        match k.as_str() {
            "font-weight" => {
                d.bold = match v {
                    "bold" | "bolder" => Some(true),
                    "normal" | "lighter" => Some(false),
                    n => n.parse::<u32>().ok().map(|w| w >= 600),
                }
            }
            "font-style" => {
                d.italic = match v {
                    "italic" | "oblique" => Some(true),
                    "normal" => Some(false),
                    _ => None,
                }
            }
            "text-decoration" | "text-decoration-line" => {
                d.strike |= v.contains("line-through");
            }
            "vertical-align" => match v {
                "super" => d.sup = true,
                "sub" => d.sub = true,
                _ => {}
            },
            "font-family" => d.mono |= MONO_FAMILIES.iter().any(|f| v.contains(f)),
            "display" => d.hidden |= v == "none",
            "visibility" => d.hidden |= v == "hidden" || v == "collapse",
            "text-align" => {
                d.align = match v {
                    "left" | "start" => Align::Left,
                    "center" => Align::Center,
                    "right" | "end" => Align::Right,
                    _ => d.align,
                }
            }
            "mso-list" => {
                if v == "ignore" {
                    d.mso_ignore = true;
                } else if let Some(i) = v.find("level") {
                    let digits: String = v[i + 5..]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    d.mso_level = Some(digits.parse::<u32>().unwrap_or(1).clamp(1, 9));
                    d.mso_key = v
                        .split_ascii_whitespace()
                        .filter(|w| !w.starts_with("level"))
                        .collect::<Vec<_>>()
                        .join(" ");
                }
            }
            _ => {}
        }
    }
    d
}

fn parse_align(v: Option<&str>) -> Align {
    match v.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("left") => Align::Left,
        Some("center" | "middle") => Align::Center,
        Some("right") => Align::Right,
        _ => Align::None,
    }
}

/// Keeps a code-fence info string to `[A-Za-z0-9_+#.-]{1,32}`.
fn sanitize_lang(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty()
        || s.len() > 32
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '#' | '.' | '-'))
    {
        return None;
    }
    Some(s.to_owned())
}

/// The language named by a `class` value: `language-x`, `lang-x`,
/// `highlight-source-x` (GitHub) or `brush: x` (MDN, SyntaxHighlighter).
fn lang_from_class(class: &str) -> Option<String> {
    let mut words = class.split_ascii_whitespace();
    while let Some(w) = words.next() {
        for prefix in ["language-", "lang-", "highlight-source-"] {
            if let Some(l) = w.strip_prefix(prefix) {
                return sanitize_lang(l);
            }
        }
        if w == "brush:" {
            return words
                .next()
                .and_then(|l| sanitize_lang(l.trim_end_matches(';')));
        }
        if let Some(l) = w.strip_prefix("brush:") {
            return sanitize_lang(l.trim_end_matches(';'));
        }
    }
    None
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c' | '\u{a0}')
}

/// Content being collected for one container (the body, a list item, a
/// quote, a cell, …).
#[derive(Debug, Default)]
struct Sink {
    blocks: Vec<Block>,
    runs: Vec<Run>,
    /// Set by a leading checkbox in a list item.
    task: Option<bool>,
}

impl Sink {
    fn ends_with_space(&self) -> bool {
        match self.runs.last() {
            None => true,
            Some(Run {
                item: Inline::Text(t),
                ..
            }) => t.ends_with(' '),
            Some(Run {
                item: Inline::Break,
                ..
            }) => true,
            Some(_) => false,
        }
    }

    fn push_text(&mut self, text: &str, style: Style) {
        let mut s = String::with_capacity(text.len());
        let mut space = self.ends_with_space();
        for c in text.chars() {
            if is_ws(c) {
                if !space {
                    s.push(' ');
                    space = true;
                }
            } else if c.is_control() || c == '\u{feff}' {
                // Other control characters never reach the Markdown.
            } else {
                s.push(c);
                space = false;
            }
        }
        if s.is_empty() {
            return;
        }
        if let Some(Run {
            item: Inline::Text(t),
            style: st,
        }) = self.runs.last_mut()
            && *st == style
        {
            t.push_str(&s);
            return;
        }
        self.runs.push(Run {
            item: Inline::Text(s),
            style,
        });
    }

    fn trim_trailing_space(&mut self) {
        while let Some(Run {
            item: Inline::Text(t),
            ..
        }) = self.runs.last_mut()
        {
            let trimmed = t.trim_end_matches(' ').len();
            t.truncate(trimmed);
            if t.is_empty() {
                self.runs.pop();
            } else {
                break;
            }
        }
    }

    fn push_break(&mut self, style: Style) {
        self.trim_trailing_space();
        if matches!(
            self.runs.last(),
            Some(Run {
                item: Inline::Break,
                ..
            })
        ) {
            // Two breaks in a row separate paragraphs.
            self.runs.pop();
            self.flush();
            return;
        }
        if self.runs.iter().any(Run::is_visible) {
            self.runs.push(Run {
                item: Inline::Break,
                style,
            });
        }
    }

    fn push_inline(&mut self, item: Inline, style: Style) {
        self.runs.push(Run { item, style });
    }

    fn has_content(&self) -> bool {
        !self.blocks.is_empty() || self.runs.iter().any(Run::is_visible)
    }

    fn flush(&mut self) {
        self.trim_trailing_space();
        while matches!(
            self.runs.last(),
            Some(Run {
                item: Inline::Break,
                ..
            })
        ) {
            self.runs.pop();
            self.trim_trailing_space();
        }
        let runs = std::mem::take(&mut self.runs);
        if runs.iter().any(Run::is_visible) {
            self.blocks.push(Block::Para(runs));
        }
    }

    fn push_block(&mut self, block: Block) {
        self.flush();
        self.blocks.push(block);
    }

    fn finish(mut self) -> Vec<Block> {
        self.flush();
        group_mso(self.blocks)
    }
}

/// Inherited state while walking.
#[derive(Debug, Clone, Copy, Default)]
struct Ctx<'d> {
    style: Style,
    /// Inside a heading or header cell: bold is implied, so `strong` is
    /// not produced.
    no_strong: bool,
    /// A language named by an enclosing element (GitHub puts
    /// `highlight-source-x` on a `div` around the `pre`).
    lang: Option<&'d str>,
}

/// Converts a parsed document to blocks.
#[derive(Debug)]
pub(crate) struct Builder<'d> {
    dom: &'d Dom,
    base: Option<url::Url>,
    links: Vec<Link>,
    depth: usize,
    containers: usize,
    /// The bullet text of the Word list paragraph being converted.
    mso_marker: Option<String>,
}

impl<'d> Builder<'d> {
    pub(crate) fn new(dom: &'d Dom, base: Option<url::Url>) -> Self {
        Builder {
            dom,
            base,
            links: Vec::new(),
            depth: 0,
            containers: 0,
            mso_marker: None,
        }
    }

    /// Converts the document body.
    pub(crate) fn run(mut self) -> (Vec<Block>, Vec<Link>) {
        let root = self.dom.document();
        let body = self.dom.find_descendant(root, "body").unwrap_or(root);
        let blocks = self.collect(body, Ctx::default());
        (blocks, self.links)
    }

    fn collect(&mut self, node: NodeId, ctx: Ctx<'d>) -> Vec<Block> {
        let mut sink = Sink::default();
        self.children(node, &mut sink, ctx);
        sink.finish()
    }

    fn children(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>) {
        let dom = self.dom;
        let kids = dom.children(node);
        // Word wraps list bullets in `<![if !supportLists]>` … `<![endif]>`,
        // which HTML parses as two comments around ordinary content. The
        // content is skipped only when the closing comment exists.
        let comment = |n: NodeId, prefix: &str| matches!(&dom.node(n).data, NodeData::Comment(c) if c.trim().starts_with(prefix));
        let mut endif_after = vec![false; kids.len()];
        if kids.iter().any(|&k| comment(k, "[if !supportLists]")) {
            let mut seen = false;
            for i in (0..kids.len()).rev() {
                endif_after[i] = seen;
                seen |= comment(kids[i], "[endif]");
            }
        }
        let mut marker: Option<String> = None;
        for (i, &child) in kids.iter().enumerate() {
            if let NodeData::Comment(_) = &dom.node(child).data {
                if comment(child, "[if !supportLists]") && endif_after[i] {
                    marker = Some(String::new());
                } else if comment(child, "[endif]")
                    && let Some(m) = marker.take()
                {
                    self.mso_marker = Some(m);
                }
                continue;
            }
            if let Some(m) = &mut marker {
                if m.len() < 64 {
                    dom.text_content(child, m, &|_| false);
                }
                continue;
            }
            self.walk(child, sink, ctx);
        }
    }

    fn walk(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>) {
        match &self.dom.node(node).data {
            NodeData::Text(t) => sink.push_text(t, ctx.style),
            NodeData::Element { .. } => {
                if self.depth >= MAX_WALK_DEPTH {
                    let mut text = String::new();
                    self.dom
                        .text_content(node, &mut text, &|n| self.is_dropped(n));
                    sink.push_text(&text, ctx.style);
                    return;
                }
                self.depth += 1;
                self.element(node, sink, ctx);
                self.depth -= 1;
            }
            NodeData::Document => self.children(node, sink, ctx),
            NodeData::Comment(_) | NodeData::Other => {}
        }
    }

    fn is_dropped(&self, node: NodeId) -> bool {
        match self.dom.html_name(node) {
            Some(name) => DROPPED.contains(&name),
            None => self.dom.any_name(node).is_some(),
        }
    }

    fn element(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>) {
        let dom = self.dom;
        let Some(name) = dom.html_name(node) else {
            if dom.any_name(node) == Some("math") {
                self.math(node, sink, ctx);
            }
            // SVG and other foreign content are dropped.
            return;
        };
        if DROPPED.contains(&name) || dom.attr(node, "hidden").is_some() {
            return;
        }
        let decl = dom.attr(node, "style").map(parse_style).unwrap_or_default();
        if decl.hidden {
            return;
        }
        if decl.mso_ignore {
            let mut m = String::new();
            dom.text_content(node, &mut m, &|_| false);
            self.mso_marker = Some(m);
            return;
        }
        let mut ctx = ctx;
        // A monospace font on a block (a whole paragraph in Courier) is
        // not code; only inline elements become code spans.
        let inline_style = !PLAIN_BLOCKS.contains(&name) && !crate::guard::BLOCK.contains(&name);
        self.apply_tag_style(name, &mut ctx);
        if let Some(b) = decl.bold {
            ctx.style.strong = b && !ctx.no_strong;
        }
        if let Some(i) = decl.italic {
            ctx.style.em = i;
        }
        ctx.style.strike |= decl.strike;
        ctx.style.sub |= decl.sub;
        ctx.style.sup |= decl.sup;
        if inline_style {
            ctx.style.code |= decl.mono;
        }
        if let Some(lang) = dom
            .attr(node, "class")
            .filter(|c| c.contains("highlight-source-"))
        {
            ctx.lang = Some(lang);
        }

        match name {
            "br" => sink.push_break(ctx.style),
            "hr" => sink.push_block(Block::Rule),
            "img" => self.image(node, sink, ctx),
            "input" => {
                let checkbox = dom
                    .attr(node, "type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("checkbox"));
                if checkbox && !sink.has_content() && sink.task.is_none() {
                    sink.task = Some(dom.attr(node, "checked").is_some());
                }
            }
            "a" => {
                if let Some(link) = self.link(node) {
                    ctx.style.link = Some(link);
                }
                self.children(node, sink, ctx);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.as_bytes()[1] - b'0';
                let mut hctx = ctx;
                hctx.no_strong = true;
                hctx.style.strong = false;
                let blocks = self.collect(node, hctx);
                let runs = flatten(&blocks);
                sink.flush();
                if runs.iter().any(Run::is_visible) {
                    sink.push_block(Block::Heading(level, runs));
                }
            }
            "pre" | "listing" | "xmp" | "plaintext" => self.code_block(node, sink, ctx),
            "blockquote" => self.container(node, sink, ctx, Block::Quote),
            "ul" | "ol" | "menu" | "dir" => self.list(node, sink, ctx, name == "ol"),
            "table" => self.table(node, sink, ctx),
            "div" if alert_kind(dom.attr(node, "class")).is_some() => {
                let kind = alert_kind(dom.attr(node, "class")).unwrap_or("note");
                self.alert(node, sink, ctx, kind);
            }
            _ if PLAIN_BLOCKS.contains(&name) => {
                if let Some(level) = decl.mso_level {
                    self.mso_item(node, sink, ctx, level, decl.mso_key);
                    return;
                }
                sink.flush();
                self.children(node, sink, ctx);
                sink.flush();
            }
            _ => self.children(node, sink, ctx),
        }
    }

    fn apply_tag_style(&self, name: &str, ctx: &mut Ctx<'d>) {
        let s = &mut ctx.style;
        match name {
            "b" | "strong" => s.strong = !ctx.no_strong,
            "i" | "em" | "cite" | "dfn" | "var" => s.em = true,
            "s" | "strike" | "del" => s.strike = true,
            "sub" => s.sub = true,
            "sup" => s.sup = true,
            "mark" => s.mark = true,
            "code" | "kbd" | "samp" | "tt" => s.code = true,
            "th" => {
                ctx.no_strong = true;
                s.strong = false;
            }
            _ => {}
        }
    }

    /// Adds a link for an `a[href]`, or `None` if it has no usable target.
    fn link(&mut self, node: NodeId) -> Option<u32> {
        let href = self.dom.attr(node, "href")?;
        let (class, url) = classify_with_base(href, self.base.as_ref());
        let keep = matches!(
            class,
            Dest::Web(_)
                | Dest::Mailto
                | Dest::Fragment
                | Dest::Relative
                | Dest::LocalFile
                | Dest::OtherScheme
        );
        // Local files only as `file:` URLs: a bare `C:\…` path is not a
        // link a page would carry.
        let bare_drive = class == Dest::LocalFile
            && !href
                .trim_start()
                .get(..5)
                .is_some_and(|p| p.eq_ignore_ascii_case("file:"));
        if !keep || bare_drive || url.is_empty() {
            return None;
        }
        let title = self
            .dom
            .attr(node, "title")
            .map(collapse)
            .filter(|t| !t.is_empty());
        self.links.push(Link { dest: url, title });
        u32::try_from(self.links.len() - 1).ok()
    }

    fn image(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>) {
        let dom = self.dom;
        let alt = dom.attr(node, "alt").map(collapse).unwrap_or_default();
        let title = dom
            .attr(node, "title")
            .map(collapse)
            .filter(|t| !t.is_empty());
        let mut src = dom.attr(node, "src");
        // Lazy loading: a placeholder `data:` source and the real one in
        // `data-src`.
        if src.is_none_or(|s| s.trim_start().to_ascii_lowercase().starts_with("data:")) {
            src = dom.attr(node, "data-src").or(src);
        }
        let usable = src.and_then(|s| {
            let (class, url) = classify_with_base(s, self.base.as_ref());
            // Images load automatically: only web URLs and relative paths.
            matches!(class, Dest::Web(_) | Dest::Relative).then_some(url)
        });
        match usable {
            Some(src) if !src.is_empty() => {
                sink.push_inline(Inline::Image { src, alt, title }, ctx.style);
            }
            _ => sink.push_text(&alt, ctx.style),
        }
    }

    fn code_block(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>) {
        let dom = self.dom;
        let mut lang = dom.attr(node, "class").and_then(lang_from_class);
        if lang.is_none()
            && let Some(code) = dom.find_descendant(node, "code")
        {
            lang = dom.attr(code, "class").and_then(lang_from_class);
        }
        if lang.is_none() {
            lang = dom.attr(node, "lang").and_then(sanitize_lang);
        }
        if lang.is_none() {
            lang = ctx.lang.and_then(lang_from_class);
        }
        let text = self.pre_text(node);
        let text = text.trim_end_matches(['\n', ' ', '\t']);
        if text.trim().is_empty() {
            return;
        }
        sink.push_block(Block::Code {
            lang,
            text: text.to_owned(),
        });
    }

    /// The text of a `pre`, white space kept, `br` as a newline.
    fn pre_text(&self, node: NodeId) -> String {
        let dom = self.dom;
        let mut out = String::new();
        let mut stack: Vec<NodeId> = dom.children(node).iter().rev().copied().collect();
        while let Some(n) = stack.pop() {
            match &dom.node(n).data {
                NodeData::Text(t) => out.extend(
                    t.chars()
                        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
                        .map(|c| if c == '\u{a0}' { ' ' } else { c }),
                ),
                NodeData::Element { .. } => {
                    if dom.html_name(n) == Some("br") {
                        out.push('\n');
                    } else if !self.is_dropped(n) {
                        stack.extend(dom.children(n).iter().rev());
                    }
                }
                _ => {}
            }
        }
        out.replace("\r\n", "\n").replace('\r', "\n")
    }

    fn container(
        &mut self,
        node: NodeId,
        sink: &mut Sink,
        ctx: Ctx<'d>,
        make: impl FnOnce(Vec<Block>) -> Block,
    ) {
        if self.containers >= MAX_CONTAINER_DEPTH {
            sink.flush();
            self.children(node, sink, ctx);
            sink.flush();
            return;
        }
        self.containers += 1;
        let blocks = self.collect(node, ctx);
        self.containers -= 1;
        if !blocks.is_empty() {
            sink.push_block(make(blocks));
        }
    }

    fn alert(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>, kind: &'static str) {
        if self.containers >= MAX_CONTAINER_DEPTH {
            sink.flush();
            self.children(node, sink, ctx);
            sink.flush();
            return;
        }
        self.containers += 1;
        let mut inner = Sink::default();
        for &child in self.dom.children(node) {
            let is_title = self
                .dom
                .attr(child, "class")
                .is_some_and(|c| c.contains("markdown-alert-title"));
            if !is_title {
                self.walk(child, &mut inner, ctx);
            }
        }
        self.containers -= 1;
        let blocks = inner.finish();
        sink.push_block(Block::Alert { kind, blocks });
    }

    fn list(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>, ordered: bool) {
        if self.containers >= MAX_CONTAINER_DEPTH {
            sink.flush();
            self.children(node, sink, ctx);
            sink.flush();
            return;
        }
        let dom = self.dom;
        let start = if ordered {
            dom.attr(node, "start")
                .and_then(|s| s.trim().parse::<i64>().ok())
                .map_or(1, |n| n.clamp(0, 999_999_999).unsigned_abs())
        } else {
            1
        };
        self.containers += 1;
        let mut items: Vec<Item> = Vec::new();
        for &child in dom.children(node) {
            match &dom.node(child).data {
                NodeData::Element { .. } if dom.html_name(child) == Some("li") => {
                    let mut item = Sink::default();
                    if self.depth < MAX_WALK_DEPTH {
                        self.depth += 1;
                        self.children(child, &mut item, ctx);
                        self.depth -= 1;
                    } else {
                        self.walk(child, &mut item, ctx);
                    }
                    let task = item.task;
                    items.push(Item {
                        task,
                        blocks: item.finish(),
                    });
                }
                NodeData::Text(t) if t.trim().is_empty() => {}
                NodeData::Comment(_) | NodeData::Other => {}
                _ => {
                    // A list nested directly in a list, or stray content:
                    // it belongs to the previous item.
                    let mut extra = Sink::default();
                    self.walk(child, &mut extra, ctx);
                    let blocks = extra.finish();
                    if blocks.is_empty() {
                        continue;
                    }
                    match items.last_mut() {
                        Some(last) => last.blocks.extend(blocks),
                        None => items.push(Item { task: None, blocks }),
                    }
                }
            }
        }
        self.containers -= 1;
        if !items.is_empty() {
            sink.push_block(Block::List(List {
                ordered,
                start,
                items,
            }));
        }
    }

    fn table(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>) {
        let dom = self.dom;
        let mut rows: Vec<NodeId> = Vec::new();
        let mut captions: Vec<NodeId> = Vec::new();
        for &child in dom.children(node) {
            match dom.html_name(child) {
                Some("tr") => rows.push(child),
                Some("thead" | "tbody" | "tfoot") => rows.extend(
                    dom.children(child)
                        .iter()
                        .filter(|&&r| dom.html_name(r) == Some("tr")),
                ),
                Some("caption") => captions.push(child),
                _ => {}
            }
        }
        for c in captions {
            self.walk(c, sink, ctx);
        }
        let mut grid: Vec<Vec<(Vec<Block>, Align, bool)>> = Vec::new();
        let mut nested_table = false;
        for row in rows {
            let row_align = parse_align(dom.attr(row, "align"));
            let mut cells = Vec::new();
            for &cell in dom.children(row) {
                let Some(name @ ("td" | "th")) = dom.html_name(cell) else {
                    continue;
                };
                let header = name == "th";
                let decl = dom.attr(cell, "style").map(parse_style).unwrap_or_default();
                let mut align = parse_align(dom.attr(cell, "align"));
                if align == Align::None {
                    align = decl.align;
                }
                if align == Align::None {
                    align = row_align;
                }
                let mut cctx = ctx;
                if header {
                    cctx.no_strong = true;
                    cctx.style.strong = false;
                }
                let blocks = if self.depth < MAX_WALK_DEPTH {
                    self.depth += 1;
                    let b = self.collect(cell, cctx);
                    self.depth -= 1;
                    b
                } else {
                    let mut s = Sink::default();
                    self.walk(cell, &mut s, cctx);
                    s.finish()
                };
                nested_table |= blocks.iter().any(contains_table);
                let span = dom
                    .attr(cell, "colspan")
                    .and_then(|s| s.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, MAX_COLUMNS);
                cells.push((blocks, align, header));
                for _ in 1..span {
                    cells.push((Vec::new(), Align::None, header));
                }
            }
            cells.truncate(MAX_COLUMNS);
            if !cells.is_empty() {
                grid.push(cells);
            }
        }
        let single_cell = grid.len() == 1 && grid[0].len() == 1;
        if grid.is_empty() {
            return;
        }
        if single_cell || nested_table {
            // A layout table: its content, in order.
            sink.flush();
            for row in grid {
                for (blocks, _, _) in row {
                    sink.blocks.extend(blocks);
                }
            }
            return;
        }
        let rows: Vec<Vec<Cell>> = grid
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|(blocks, align, header)| Cell {
                        runs: flatten(&blocks),
                        align,
                        header,
                    })
                    .collect()
            })
            .collect();
        sink.push_block(Block::Table(rows));
    }

    fn mso_item(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>, level: u32, key: String) {
        self.mso_marker = None;
        let blocks = self.collect(node, ctx);
        let marker = self.mso_marker.take().unwrap_or_default();
        let marker = marker.trim_matches(|c: char| is_ws(c));
        let body = marker.trim_end_matches(['.', ')']);
        let ordered = marker.len() > body.len()
            && !body.is_empty()
            && body.len() <= 6
            && body.chars().all(|c| c.is_ascii_alphanumeric());
        let start = if ordered {
            body.parse::<u64>().ok()
        } else {
            None
        };
        sink.push_block(Block::MsoItem {
            level,
            key,
            ordered,
            start,
            blocks,
        });
    }

    fn math(&mut self, node: NodeId, sink: &mut Sink, ctx: Ctx<'d>) {
        let dom = self.dom;
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if dom.any_name(n) == Some("annotation")
                && dom
                    .attr(n, "encoding")
                    .is_some_and(|e| e.eq_ignore_ascii_case("application/x-tex"))
            {
                let mut tex = String::new();
                dom.text_content(n, &mut tex, &|_| false);
                let tex = collapse(&tex);
                if !tex.is_empty() {
                    sink.push_inline(Inline::Math(tex), ctx.style);
                }
                return;
            }
            stack.extend(dom.children(n).iter().rev());
        }
        // No TeX: the visible text, without annotations.
        let mut text = String::new();
        dom.text_content(node, &mut text, &|n| {
            matches!(dom.any_name(n), Some("annotation" | "annotation-xml"))
        });
        sink.push_text(&text, ctx.style);
    }
}

fn alert_kind(class: Option<&str>) -> Option<&'static str> {
    let class = class?;
    if !class
        .split_ascii_whitespace()
        .any(|w| w == "markdown-alert")
    {
        return None;
    }
    for kind in ["note", "tip", "important", "warning", "caution"] {
        let want = format!("markdown-alert-{kind}");
        if class.split_ascii_whitespace().any(|w| w == want) {
            return Some(kind);
        }
    }
    None
}

fn contains_table(b: &Block) -> bool {
    match b {
        Block::Table(_) => true,
        Block::Quote(bs) | Block::Alert { blocks: bs, .. } | Block::MsoItem { blocks: bs, .. } => {
            bs.iter().any(contains_table)
        }
        Block::List(l) => l.items.iter().any(|i| i.blocks.iter().any(contains_table)),
        _ => false,
    }
}

/// Collapses white space (attribute values: titles, alt text).
fn collapse(s: &str) -> String {
    s.split(is_ws)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// The inline content of `blocks` as one run list, blocks separated by
/// breaks (headings and table cells hold a single line).
pub(crate) fn flatten(blocks: &[Block]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let sep = |out: &mut Vec<Run>| {
        if out.iter().any(Run::is_visible)
            && !matches!(
                out.last(),
                Some(Run {
                    item: Inline::Break,
                    ..
                })
            )
        {
            out.push(Run {
                item: Inline::Break,
                style: Style::default(),
            });
        }
    };
    let mut stack: Vec<&Block> = blocks.iter().rev().collect();
    while let Some(b) = stack.pop() {
        match b {
            Block::Para(runs) | Block::Heading(_, runs) => {
                sep(&mut out);
                out.extend(runs.iter().cloned());
            }
            Block::Code { text, .. } => {
                for line in text.lines() {
                    sep(&mut out);
                    out.push(Run {
                        item: Inline::Text(line.to_owned()),
                        style: Style {
                            code: true,
                            ..Style::default()
                        },
                    });
                }
            }
            Block::Quote(bs)
            | Block::Alert { blocks: bs, .. }
            | Block::MsoItem { blocks: bs, .. } => {
                stack.extend(bs.iter().rev());
            }
            Block::List(l) => {
                for item in l.items.iter().rev() {
                    stack.extend(item.blocks.iter().rev());
                }
            }
            Block::Table(rows) => {
                for row in rows {
                    sep(&mut out);
                    for (i, cell) in row.iter().enumerate() {
                        if i > 0 {
                            out.push(Run {
                                item: Inline::Text(" ".into()),
                                style: Style::default(),
                            });
                        }
                        out.extend(
                            cell.runs
                                .iter()
                                .filter(|r| r.item != Inline::Break)
                                .cloned(),
                        );
                    }
                }
            }
            Block::Rule => {}
        }
    }
    while matches!(
        out.last(),
        Some(Run {
            item: Inline::Break,
            ..
        })
    ) {
        out.pop();
    }
    out
}

/// Groups consecutive Word list paragraphs into nested lists.
fn group_mso(blocks: Vec<Block>) -> Vec<Block> {
    if !blocks.iter().any(|b| matches!(b, Block::MsoItem { .. })) {
        return blocks;
    }
    let mut out = Vec::new();
    let mut run: Vec<(u32, bool, Option<u64>, Vec<Block>)> = Vec::new();
    let mut run_key = String::new();
    for b in blocks {
        match b {
            Block::MsoItem {
                level,
                key,
                ordered,
                start,
                blocks,
            } => {
                // Another Word list (a different `l…`/`lfo…` id) starts a
                // new Markdown list.
                if !run.is_empty() && key != run_key {
                    out.push(mso_list(std::mem::take(&mut run)));
                }
                run_key = key;
                run.push((level, ordered, start, blocks));
            }
            other => {
                if !run.is_empty() {
                    out.push(mso_list(std::mem::take(&mut run)));
                }
                out.push(other);
            }
        }
    }
    if !run.is_empty() {
        out.push(mso_list(run));
    }
    out
}

fn mso_list(items: Vec<(u32, bool, Option<u64>, Vec<Block>)>) -> Block {
    // Levels are at most 9, so the explicit stack stays tiny.
    struct Open {
        level: u32,
        list: List,
    }
    let mut stack: Vec<Open> = Vec::new();
    for (level, ordered, start, blocks) in items {
        while stack.len() > 1 && stack.last().is_some_and(|o| o.level > level) {
            close_level(&mut stack);
        }
        match stack.last_mut() {
            Some(top) if level <= top.level => top.list.items.push(Item { task: None, blocks }),
            _ => stack.push(Open {
                level,
                list: List {
                    ordered,
                    start: start.unwrap_or(1),
                    items: vec![Item { task: None, blocks }],
                },
            }),
        }
    }
    while stack.len() > 1 {
        close_level(&mut stack);
    }
    fn close_level(stack: &mut Vec<Open>) {
        if let Some(done) = stack.pop()
            && let Some(parent) = stack.last_mut()
        {
            match parent.list.items.last_mut() {
                Some(item) => item.blocks.push(Block::List(done.list)),
                None => parent.list.items.push(Item {
                    task: None,
                    blocks: vec![Block::List(done.list)],
                }),
            }
        }
    }
    match stack.pop() {
        Some(o) => Block::List(o.list),
        None => Block::Para(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_attribute() {
        let d = parse_style("font-weight:700; font-style: italic ;text-decoration:line-through");
        assert_eq!(d.bold, Some(true));
        assert_eq!(d.italic, Some(true));
        assert!(d.strike);
        let d = parse_style("font-weight:normal;font-family:'Courier New'");
        assert_eq!(d.bold, Some(false));
        assert!(d.mono);
        let d = parse_style("mso-list:l0 level3 lfo1");
        assert_eq!(d.mso_level, Some(3));
        assert!(parse_style("mso-list:Ignore").mso_ignore);
        assert!(parse_style("DISPLAY: none !important").hidden);
    }

    #[test]
    fn languages() {
        assert_eq!(lang_from_class("language-rust").as_deref(), Some("rust"));
        assert_eq!(
            lang_from_class("highlight highlight-source-js notranslate").as_deref(),
            Some("js")
        );
        assert_eq!(lang_from_class("brush: py; gutter").as_deref(), Some("py"));
        assert_eq!(lang_from_class("language-a`b"), None);
        assert_eq!(lang_from_class("plain"), None);
    }
}

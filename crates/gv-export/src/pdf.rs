// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The PDF exporter's half: a render tree as a **generated Typst
//! document**, which `glyphvane-helper` compiles to PDF.
//!
//! No user text is ever Typst markup. Every piece of it becomes a Typst
//! string literal (`#"…";`) in which only `\` and `"` are special; control
//! characters are dropped. Labels are hex-encoded anchors, asset paths are
//! generated (`/assets/N.ext`), link targets pass a scheme allowlist
//! (http, https, mailto) and option values come from fixed lists or
//! clamped numbers. The document reads nothing but its assets.
//!
//! Before an image becomes an asset it is checked: local files only
//! (within the vault, under the HTML export's byte caps), a format Typst
//! reads, at most [`MAX_PIXELS`] pixels, and SVGs without external
//! references (Typst fails the whole document on one). Formulas are
//! RaTeX SVGs on a box at their baseline; diagrams are SVGs with their
//! text as outlines; code keeps the highlighter's colours.
//!
//! The PDF gets an outline (bookmarks) from the headings, link annotations
//! for links that resolve inside the document or to the web, and no date,
//! so the same document gives the same PDF.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use gv_highlight::{CodeHighlighter, TokenClass};
use gv_inline::ItemKind as RunKind;
use gv_model::LinkKind;
use gv_render_tree::{
    BlockNode, CalloutType, CodeBlock, Frame, FrameKind, InlineContent, InlineRole, LinkInfo,
    LinkStatus, ListMarker, NodeKind, RenderTree, TableBlock, Variant, decode_style_id,
};
use gv_syntax::table::Alignment;

use crate::assets::{ImageMode, ImageNote, Store};
use crate::{ExportOptions, ExportTheme, math};

/// The most pixels an embedded raster image may have (Typst decodes images
/// at full size).
pub const MAX_PIXELS: u64 = 40_000_000;

/// The most images and formulas one document embeds.
pub const MAX_ASSETS: usize = 4096;

/// Paper sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Paper {
    #[default]
    A4,
    A5,
    A3,
    Letter,
    Legal,
}

impl Paper {
    pub const ALL: [Paper; 5] = [Paper::A4, Paper::A5, Paper::A3, Paper::Letter, Paper::Legal];

    /// The name options use (`a4`, `letter`, …).
    pub fn name(self) -> &'static str {
        match self {
            Self::A4 => "a4",
            Self::A5 => "a5",
            Self::A3 => "a3",
            Self::Letter => "letter",
            Self::Legal => "legal",
        }
    }

    /// The paper named `name` (any case).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(name))
    }

    fn typst(self) -> &'static str {
        match self {
            Self::A4 => "a4",
            Self::A5 => "a5",
            Self::A3 => "a3",
            Self::Letter => "us-letter",
            Self::Legal => "us-legal",
        }
    }
}

/// The page of a PDF export (FR-EXPORT-05).
#[derive(Debug, Clone, PartialEq)]
pub struct PageOptions {
    pub paper: Paper,
    /// A page size in millimetres (from a print dialog), instead of
    /// `paper`.
    pub size_mm: Option<(f32, f32)>,
    pub landscape: bool,
    /// The margin on every side, in millimetres (5–60).
    pub margin_mm: f32,
    /// The title at the top of every page after the first.
    pub header: bool,
    /// "page / pages" at the bottom of every page.
    pub page_numbers: bool,
    /// Each level-1 heading starts a page.
    pub break_before_h1: bool,
    /// The most pages the document may have.
    pub max_pages: u32,
}

impl Default for PageOptions {
    fn default() -> Self {
        Self {
            paper: Paper::A4,
            size_mm: None,
            landscape: false,
            margin_mm: 20.0,
            header: true,
            page_numbers: true,
            break_before_h1: false,
            max_pages: 2000,
        }
    }
}

/// A generated Typst document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypstDocument {
    /// The main file.
    pub source: String,
    /// Virtual paths and their bytes.
    pub assets: Vec<(String, Vec<u8>)>,
    /// The font families the document names, in order of preference.
    pub fonts: Vec<String>,
    pub title: String,
    /// A stable identifier of the document (a hash of its source).
    pub ident: String,
    /// Images that were not embedded, and why.
    pub notes: Vec<ImageNote>,
}

/// Body text, in order of preference; the last ones cover symbols, CJK
/// and emoji.
pub const SANS_FONTS: &[&str] = &[
    "Inter",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Arial",
    "Helvetica",
    "Segoe UI",
    "Noto Sans CJK SC",
    "Noto Sans Symbols 2",
    "Noto Color Emoji",
    "Apple Color Emoji",
    "Segoe UI Emoji",
];

/// Code.
pub const MONO_FONTS: &[&str] = &[
    "JetBrains Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Menlo",
    "Consolas",
];

/// A Typst string literal of `s`, quotes included. Control characters are
/// dropped (tabs and line breaks become spaces).
pub fn str_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' | '\n' | '\r' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A label for anchor `anchor`, valid Typst label syntax whatever it holds.
pub fn label(anchor: &str) -> String {
    let mut out = String::from("gv-");
    for b in anchor.bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' | b'-' => out.push(b as char),
            _ => {
                let _ = write!(out, "_{b:02x}");
            }
        }
    }
    out
}

/// Whether `url` may become a link annotation.
pub fn url_allowed(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// A number for Typst source (finite, short).
fn num(v: f32) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".to_owned()
    } else {
        s.to_owned()
    }
}

/// Typst text for the end of a content block that is only text: `#"…";`.
fn text(out: &mut String, s: &str) {
    if s.is_empty() {
        return;
    }
    out.push('#');
    out.push_str(&str_lit(s));
    out.push(';');
}

/// The colours the document uses, as Typst expressions.
struct Colors {
    text: String,
    text2: String,
    text3: String,
    accent: String,
    sunken: String,
    border: String,
    border_strong: String,
    theme: ExportTheme,
}

impl Colors {
    fn new(theme: &ExportTheme) -> Self {
        let c = |name: &str, fallback: u8| color(theme, name, fallback);
        Self {
            text: c("text.primary", 20),
            text2: c("text.secondary", 90),
            text3: c("text.tertiary", 130),
            accent: c("accent.default", 60),
            sunken: c("surface.sunken", 245),
            border: c("border.default", 210),
            border_strong: c("border.strong", 170),
            theme: theme.clone(),
        }
    }

    fn token(&self, name: &str) -> String {
        color(&self.theme, name, 120)
    }
}

/// Token `name` of `theme` as a Typst colour; a grey without it.
fn color(theme: &ExportTheme, name: &str, grey: u8) -> String {
    match theme.color(name) {
        Some([r, g, b, 255]) => format!("rgb(\"#{r:02x}{g:02x}{b:02x}\")"),
        Some([r, g, b, a]) => format!("rgb(\"#{r:02x}{g:02x}{b:02x}{a:02x}\")"),
        None => format!("luma({grey})"),
    }
}

/// An inline element kept open, in nesting order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Link(u32),
    Strong,
    Em,
    Del,
    Mark,
}

struct Emitter<'a> {
    tree: &'a RenderTree,
    opts: &'a ExportOptions,
    colors: Colors,
    store: Store,
    out: String,
    /// Containers open now.
    stack: Vec<Frame>,
    /// Anchors that name exactly one heading: link targets.
    targets: HashSet<String>,
    /// Footnote bodies by number, and whether they were referenced yet.
    footnotes: HashMap<u32, String>,
    referenced: HashSet<u32>,
    /// Output saved while a footnote body is emitted.
    saved: Vec<String>,
    assets: Vec<(String, Vec<u8>)>,
    /// Asset path of each image, by the store's key.
    images: HashMap<usize, String>,
    title: Option<String>,
}

/// Generates the Typst document of `tree`.
pub fn export_typst(tree: &RenderTree, opts: &ExportOptions, page: &PageOptions) -> TypstDocument {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for node in tree.nodes() {
        if let NodeKind::Heading {
            anchor: Some(a), ..
        } = &node.kind
        {
            *counts.entry(a).or_default() += 1;
        }
    }
    let targets = counts
        .into_iter()
        .filter(|(_, n)| *n == 1)
        .map(|(a, _)| a.to_owned())
        .collect();
    let mut e = Emitter {
        tree,
        opts,
        colors: Colors::new(&opts.theme),
        store: Store::new(opts, ImageMode::Embed),
        out: String::new(),
        stack: Vec::new(),
        targets,
        footnotes: HashMap::new(),
        referenced: HashSet::new(),
        saved: Vec::new(),
        assets: Vec::new(),
        images: HashMap::new(),
        title: None,
    };
    e.footnote_bodies();
    e.body();
    let title = opts
        .title
        .clone()
        .or_else(|| e.title.clone())
        .or_else(|| {
            opts.doc_path
                .as_deref()
                .and_then(Path::file_stem)
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Untitled".to_owned());
    let mut source = preamble(&e.colors, &title, page, opts.toc);
    source.push_str(&e.out);
    let ident = format!("glyphvane-{:016x}", fnv(source.as_bytes()));
    let (_, notes) = e.store.finish();
    TypstDocument {
        source,
        assets: e.assets,
        fonts: SANS_FONTS
            .iter()
            .chain(MONO_FONTS)
            .map(|s| (*s).to_owned())
            .collect(),
        title,
        ident,
        notes,
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn font_list(fonts: &[&str]) -> String {
    let list: Vec<String> = fonts.iter().map(|f| str_lit(f)).collect();
    format!("({},)", list.join(", "))
}

/// The page set-up, the styles and the helper functions the body calls.
fn preamble(c: &Colors, title: &str, page: &PageOptions, toc: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "#set document(title: {}, author: ())", str_lit(title));
    let size = match page.size_mm {
        Some((w, h)) if w.is_finite() && h.is_finite() => {
            let (w, h) = (w.clamp(50.0, 2000.0), h.clamp(50.0, 2000.0));
            format!("width: {}mm, height: {}mm", num(w), num(h))
        }
        _ => format!("paper: {}", str_lit(page.paper.typst())),
    };
    let margin = if page.margin_mm.is_finite() {
        page.margin_mm.clamp(5.0, 60.0)
    } else {
        20.0
    };
    let header = if page.header {
        format!(
            "context if counter(page).get().first() > 1 {{ align(right, text(size: 8pt, fill: {}, {})) }}",
            c.text3,
            str_lit(title)
        )
    } else {
        "none".to_owned()
    };
    let footer = if page.page_numbers {
        format!(
            "context align(center, text(size: 8pt, fill: {}, counter(page).display(\"1 / 1\", both: true)))",
            c.text3
        )
    } else {
        "none".to_owned()
    };
    let _ = writeln!(
        s,
        "#set page({size}, flipped: {}, margin: {}mm, header: {header}, footer: {footer})",
        page.landscape,
        num(margin)
    );
    let _ = writeln!(
        s,
        "#set text(font: {}, size: 10.5pt, fill: {}, hyphenate: false)",
        font_list(SANS_FONTS),
        c.text
    );
    s.push_str("#set par(justify: false, leading: 0.7em, spacing: 1.1em)\n");
    let _ = writeln!(
        s,
        "#show raw: set text(font: {}, size: 0.9em)",
        font_list(MONO_FONTS)
    );
    let _ = writeln!(s, "#show link: set text(fill: {})", c.accent);
    s.push_str("#show heading: set block(above: 1.4em, below: 0.8em)\n");
    if page.break_before_h1 {
        s.push_str("#show heading.where(level: 1): it => { pagebreak(weak: true); it }\n");
    }
    let _ = writeln!(
        s,
        "#let gvimg(path, alt) = layout(size => {{ let img = image(path, alt: alt); let w = measure(img).width; if w > size.width {{ image(path, alt: alt, width: size.width) }} else {{ img }} }})"
    );
    s.push_str("#let gvitem(m, body) = grid(columns: (1.4em, 1fr), column-gutter: 0.35em, align(right, m), body)\n");
    s.push_str("#let gvlist(tight, body) = block(width: 100%, { if tight { set par(spacing: 0.6em); set block(spacing: 0.6em); body } else { body } })\n");
    let _ = writeln!(
        s,
        "#let gvtask(done) = box(width: 0.75em, height: 0.75em, baseline: 0.05em, radius: 1.5pt, stroke: 0.6pt + {}, fill: if done {{ {} }} else {{ none }})",
        c.border_strong, c.accent
    );
    let _ = writeln!(
        s,
        "#let gvquote(body) = block(width: 100%, inset: (left: 10pt, y: 2pt), stroke: (left: 2.5pt + {}), text(fill: {}, body))",
        c.border_strong, c.text2
    );
    s.push_str("#let gvcallout(tint, color, body) = block(width: 100%, inset: (x: 10pt, y: 8pt), radius: 3pt, fill: tint, stroke: (left: 3pt + color), body)\n");
    s.push_str("#let gvctitle(color, body) = text(weight: \"semibold\", fill: color, body)\n");
    let _ = writeln!(
        s,
        "#let gvembed(body) = block(width: 100%, inset: (left: 10pt), stroke: (left: 1.5pt + {}), body)",
        c.border_strong
    );
    let _ = writeln!(
        s,
        "#let gvcode(body) = block(width: 100%, fill: {}, stroke: 0.5pt + {}, radius: 3pt, inset: 8pt, {{ set par(leading: 0.5em); body }})",
        c.sunken, c.border
    );
    let _ = writeln!(
        s,
        "#let gvic(t) = box(fill: {}, inset: (x: 2pt), outset: (y: 2pt), radius: 2pt, raw(t))",
        c.sunken
    );
    let _ = writeln!(
        s,
        "#let gvrule = line(length: 100%, stroke: 0.5pt + {})",
        c.border
    );
    let _ = writeln!(
        s,
        "#set table(stroke: 0.5pt + {}, inset: (x: 6pt, y: 4pt))",
        c.border
    );
    let _ = writeln!(s, "#let gvhead = {}", c.sunken);
    let _ = writeln!(
        s,
        "#let gverror(t) = text(fill: {}, raw(t))",
        c.token("status.error")
    );
    if toc {
        s.push_str("#outline()\n");
    }
    s.push('\n');
    s
}

impl Emitter<'_> {
    /// Emits the footnote section into [`Emitter::footnotes`], so each
    /// footnote's text can go where it is first referenced.
    fn footnote_bodies(&mut self) {
        let tree = self.tree;
        let Some(start) = tree.nodes().iter().position(|n| n.id.0 == u64::MAX) else {
            return;
        };
        for (ix, node) in tree.nodes().iter().enumerate().skip(start + 1) {
            self.sync(&node.context);
            self.node(ix, node);
        }
        self.sync(&[]);
    }

    fn body(&mut self) {
        let tree = self.tree;
        for (ix, node) in tree.nodes().iter().enumerate() {
            if node.id.0 == u64::MAX {
                break;
            }
            self.sync(&node.context);
            self.node(ix, node);
        }
        self.sync(&[]);
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
            FrameKind::Quote { .. } => self.out.push_str("#gvquote[\n"),
            FrameKind::List { tight, .. } => {
                let _ = writeln!(self.out, "#gvlist({tight})[");
            }
            FrameKind::Item {
                marker: ListMarker::Task { checked, .. },
            } => {
                let _ = writeln!(self.out, "#gvitem(gvtask({checked}))[");
            }
            FrameKind::Item { marker } => {
                let _ = writeln!(self.out, "#gvitem({})[", str_lit(&marker.text()));
            }
            FrameKind::Alert { kind } => {
                let (tint, color) = match kind.name() {
                    "note" => ("status.info_tint", "status.info"),
                    "tip" => ("status.success_tint", "status.success"),
                    "warning" => ("status.warning_tint", "status.warning"),
                    "caution" => ("status.error_tint", "status.error"),
                    _ => ("accent.tint", "accent.default"),
                };
                let (tint, color) = (self.colors.token(tint), self.colors.token(color));
                let mut title = kind.name().to_owned();
                if let Some(first) = title.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                let _ = write!(
                    self.out,
                    "#gvcallout({tint}, {color})[#gvctitle({color})[#{};]\n\n",
                    str_lit(&title)
                );
            }
            FrameKind::Callout { name, .. } => {
                let (tint, color) = CalloutType::of(name).colors();
                let _ = writeln!(
                    self.out,
                    "#gvcallout({}, {})[",
                    self.colors.token(tint.name()),
                    self.colors.token(color.name())
                );
            }
            FrameKind::Footnote { .. } => {
                self.saved.push(std::mem::take(&mut self.out));
            }
            FrameKind::Details => {}
            FrameKind::Embed { .. } => self.out.push_str("#gvembed[\n"),
        }
    }

    fn close(&mut self, frame: &Frame) {
        match &frame.kind {
            FrameKind::Footnote { number, .. } => {
                let body = std::mem::replace(&mut self.out, self.saved.pop().unwrap_or_default());
                self.footnotes.insert(*number, body.trim().to_owned());
            }
            FrameKind::Details => {}
            _ => self.out.push_str("]\n\n"),
        }
    }

    /// The colour of the callout `node` is the title of.
    fn callout_color(&self, node: &BlockNode) -> String {
        let name = node.context.iter().rev().find_map(|f| match &f.kind {
            FrameKind::Callout { name, .. } => Some(name.clone()),
            _ => None,
        });
        match name {
            Some(name) => self.colors.token(CalloutType::of(&name).colors().1.name()),
            None => self.colors.text.clone(),
        }
    }

    fn node(&mut self, ix: usize, node: &BlockNode) {
        match &node.kind {
            NodeKind::Paragraph(c) => {
                self.inline(c, 0..c.spans.len(), ix, 0);
                self.out.push_str("\n\n");
            }
            NodeKind::Heading {
                level,
                anchor,
                content,
                ..
            } => {
                let level = (*level).clamp(1, 6);
                if self.title.is_none() && level == 1 {
                    let t = plain(content);
                    if !t.trim().is_empty() {
                        self.title = Some(t);
                    }
                }
                let _ = write!(self.out, "#heading(level: {level})[");
                self.inline(content, 0..content.spans.len(), ix, 0);
                self.out.push(']');
                if let Some(a) = anchor
                    && self.targets.contains(&**a)
                {
                    let _ = write!(self.out, " <{}>", label(a));
                }
                self.out.push_str("\n\n");
            }
            NodeKind::CalloutTitle { title, .. } => {
                let color = self.callout_color(node);
                let _ = write!(self.out, "#gvctitle({color})[");
                self.inline(title, 0..title.spans.len(), ix, 0);
                self.out.push_str("]\n\n");
            }
            NodeKind::Code(c) => self.code(c),
            NodeKind::Math(c) => {
                let macros = self.tree.math_macros(ix, 0);
                match math::svg_box(&c.text, true, &macros) {
                    Ok(b) => match self.svg_asset(b.svg) {
                        Some(path) => {
                            let _ = write!(
                                self.out,
                                "#align(center, image({}, height: {}em))\n\n",
                                str_lit(&path),
                                num(b.height)
                            );
                        }
                        None => self.error_text(&c.text),
                    },
                    Err(_) => self.error_text(&c.text),
                }
            }
            NodeKind::Diagram(c) => {
                let svg = self.opts.diagrams.as_ref().and_then(|d| d.svg(&c.text));
                match svg.filter(|s| svg_is_self_contained(s)) {
                    Some(svg) => match self.svg_asset(svg) {
                        Some(path) => {
                            let _ = write!(
                                self.out,
                                "#align(center, gvimg({}, \"diagram\"))\n\n",
                                str_lit(&path)
                            );
                        }
                        None => self.source_block(&c.text),
                    },
                    None => self.source_block(&c.text),
                }
            }
            NodeKind::Table(t) => self.table(ix, t),
            NodeKind::ThematicBreak => self.out.push_str("#gvrule\n\n"),
            NodeKind::Html { code, .. } => {
                // PDF has no HTML: its text, without the markup.
                let t = html_text(&code.text);
                if !t.is_empty() {
                    text(&mut self.out, &t);
                    self.out.push_str("\n\n");
                }
            }
            NodeKind::Properties { props, .. } => {
                if props.is_empty() {
                    return;
                }
                self.out.push_str(
                    "#table(columns: (auto, 1fr), fill: (x, y) => if x == 0 { gvhead },\n",
                );
                for p in props.iter() {
                    let values: Vec<&str> = p.values.iter().map(|v| &**v).collect();
                    let _ = writeln!(
                        self.out,
                        "  [#{};], [#{};],",
                        str_lit(&p.key),
                        str_lit(&values.join(", "))
                    );
                }
                self.out.push_str(")\n\n");
            }
            NodeKind::EmptyItem | NodeKind::EmptyQuote => {}
            NodeKind::Details { summary, .. } => {
                self.out.push_str("#strong[");
                self.inline(summary, 0..summary.spans.len(), ix, 0);
                self.out.push_str("]\n\n");
            }
            NodeKind::Picture(p) => {
                if let Some(src) = p.pick(false) {
                    let src = src.to_owned();
                    self.image(&src, &p.alt, false);
                    self.out.push_str("\n\n");
                }
            }
            // A live block (canvas or base embed, base or query fence) is
            // not drawn in exports: the embed's line, or the fence as code.
            NodeKind::Live(live) => {
                if let (Some(_), Some(c)) = (&live.html, &live.code) {
                    // An HTML media block: its text, as for raw HTML.
                    let t = html_text(&c.text);
                    if !t.is_empty() {
                        text(&mut self.out, &t);
                        self.out.push_str("\n\n");
                    }
                } else if let Some(c) = &live.label {
                    self.inline(c, 0..c.spans.len(), ix, 0);
                    self.out.push_str("\n\n");
                } else if let Some(c) = &live.code {
                    self.code(c);
                }
            }
        }
    }

    fn error_text(&mut self, tex: &str) {
        let _ = write!(self.out, "#gverror({})\n\n", str_lit(tex));
    }

    fn source_block(&mut self, src: &str) {
        self.out.push_str("#gvcode[");
        self.code_lines(src, &[]);
        self.out.push_str("]\n\n");
    }

    fn code(&mut self, c: &CodeBlock) {
        let info = c.info.as_deref().unwrap_or("");
        let highlight = CodeHighlighter::shared().highlight(&c.text, info);
        let spans: Vec<(usize, usize, TokenClass)> = highlight
            .spans
            .iter()
            .map(|s| (s.start, s.end, s.class))
            .collect();
        self.out.push_str("#gvcode[");
        self.code_lines(&c.text, &spans);
        self.out.push_str("]\n\n");
    }

    /// `text` line by line, its runs coloured by `spans` (byte ranges in
    /// order).
    fn code_lines(&mut self, text: &str, spans: &[(usize, usize, TokenClass)]) {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let mut line_start = 0;
        let mut first = true;
        for line in text.split('\n') {
            let line_end = line_start + line.len();
            if !first {
                self.out.push_str("#linebreak()");
            }
            first = false;
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.trim().is_empty() {
                // An empty line still takes its height.
                self.out.push_str("#raw(\" \");");
            } else {
                let mut at = line_start;
                let end = line_start + line.len();
                for &(a, b, class) in spans {
                    let (a, b) = (a.max(line_start), b.min(end));
                    if a >= b || a < at || class == TokenClass::Plain {
                        continue;
                    }
                    self.code_run(&text[at..a], None);
                    self.code_run(&text[a..b], Some(class));
                    at = b;
                }
                self.code_run(&text[at..end], None);
            }
            line_start = line_end + 1;
        }
    }

    fn code_run(&mut self, run: &str, class: Option<TokenClass>) {
        if run.is_empty() {
            return;
        }
        match class {
            Some(class) => {
                let style = class.font_style();
                let _ = write!(
                    self.out,
                    "#text(fill: {}{}{}, raw({}));",
                    self.colors.token(class.token()),
                    if style.bold {
                        ", weight: \"semibold\""
                    } else {
                        ""
                    },
                    if style.italic {
                        ", style: \"italic\""
                    } else {
                        ""
                    },
                    str_lit(run)
                );
            }
            None => {
                let _ = write!(self.out, "#raw({});", str_lit(run));
            }
        }
    }

    fn table(&mut self, ix: usize, t: &TableBlock) {
        let columns = t.aligns.len().max(1);
        let aligns: Vec<&str> = (0..columns)
            .map(|i| match t.aligns.get(i) {
                Some(Alignment::Center) => "center",
                Some(Alignment::Right) => "right",
                _ => "left",
            })
            .collect();
        let _ = writeln!(
            self.out,
            "#table(columns: {columns}, align: ({},), fill: (x, y) => if {} and y == 0 {{ gvhead }},",
            aligns.join(", "),
            t.rows.first().is_some_and(|r| r.header)
        );
        let mut math_base = 0;
        let mut header_open = false;
        for (r, row) in t.rows.iter().enumerate() {
            let base = math_base;
            math_base += row.content.math.len();
            if row.header && !header_open && r == 0 {
                self.out.push_str("  table.header(");
                header_open = true;
            } else if !row.header && header_open {
                self.out.push_str("),\n");
                header_open = false;
            }
            if !header_open {
                self.out.push_str("  ");
            }
            let spans = &row.content.spans;
            let gaps: Vec<usize> = spans
                .iter()
                .enumerate()
                .filter_map(|(i, s)| matches!(s.role, InlineRole::CellGap(_)).then_some(i))
                .collect();
            let mut cells = 0;
            for pair in gaps.windows(2) {
                if cells == columns {
                    break;
                }
                self.out
                    .push_str(if row.header { "[#strong[" } else { "[" });
                self.inline(&row.content, pair[0] + 1..pair[1], ix, base);
                self.out.push_str(if row.header { "]], " } else { "], " });
                cells += 1;
            }
            for _ in cells..columns {
                self.out.push_str("[], ");
            }
            if !header_open {
                self.out.push('\n');
            }
        }
        if header_open {
            self.out.push_str("),\n");
        }
        self.out.push_str(")\n\n");
    }

    /// The asset path of a generated SVG, or `None` when the document has
    /// too many assets.
    fn svg_asset(&mut self, svg: String) -> Option<String> {
        if self.assets.len() >= MAX_ASSETS {
            return None;
        }
        let path = format!("/assets/{}.svg", self.assets.len());
        self.assets.push((path.clone(), svg.into_bytes()));
        Some(path)
    }

    /// An image: embedded if it may be, else its alt text in brackets.
    fn image(&mut self, src: &str, alt: &str, inline: bool) {
        match self.image_asset(src) {
            Some(path) => {
                let call = format!("gvimg({}, {})", str_lit(&path), str_lit(alt));
                if inline {
                    let _ = write!(self.out, "#box({call});");
                } else {
                    let _ = write!(self.out, "#{call}");
                }
            }
            None => {
                let shown = if alt.trim().is_empty() { src } else { alt };
                text(&mut self.out, &format!("[{shown}]"));
            }
        }
    }

    /// The asset path of image `src`, after its checks.
    fn image_asset(&mut self, src: &str) -> Option<String> {
        let trimmed = src.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http:") || lower.starts_with("https:") || lower.starts_with("//") {
            self.store
                .skip(src, "remote images are not embedded in PDF");
            return None;
        }
        if lower.contains(':') && !lower.starts_with("./") {
            self.store.skip(src, "not a plain relative path");
            return None;
        }
        let (key, bytes, mime) = self.store.bytes(trimmed)?;
        if let Some(path) = self.images.get(&key) {
            return Some(path.clone());
        }
        let bytes = bytes?;
        let ext = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            _ => {
                self.store.skip(src, "a format PDF export cannot embed");
                return None;
            }
        };
        if ext == "svg" {
            if !std::str::from_utf8(&bytes).is_ok_and(svg_is_self_contained) {
                self.store
                    .skip(src, "an SVG that refers to other files or images");
                return None;
            }
        } else {
            match imagesize::blob_size(&bytes) {
                Ok(size)
                    if (size.width as u64).saturating_mul(size.height as u64) <= MAX_PIXELS => {}
                Ok(_) => {
                    self.store.skip(src, "too many pixels");
                    return None;
                }
                Err(_) => {
                    self.store.skip(src, "cannot be read");
                    return None;
                }
            }
        }
        if self.assets.len() >= MAX_ASSETS {
            self.store.skip(src, "the export has too many images");
            return None;
        }
        let path = format!("/assets/{}.{ext}", self.assets.len());
        self.assets.push((path.clone(), bytes));
        self.images.insert(key, path.clone());
        Some(path)
    }

    /// Where link `link` goes: an external URL or a label of this
    /// document; `None` when it is plain text in the PDF.
    fn link_target(&self, link: &LinkInfo) -> Option<String> {
        if matches!(
            link.status,
            LinkStatus::Unresolved | LinkStatus::OutsideRoot | LinkStatus::Blocked
        ) {
            return None;
        }
        let dest = link.destination.trim();
        let anchor = match link.kind {
            LinkKind::Tag => return None,
            LinkKind::Wikilink | LinkKind::Embed => {
                let target = dest.split('|').next().unwrap_or_default();
                let (file, heading) = target.split_once('#')?;
                let this = self
                    .opts
                    .doc_path
                    .as_deref()
                    .and_then(Path::file_stem)
                    .is_some_and(|s| s.to_string_lossy() == file);
                if !file.is_empty() && !this {
                    return None;
                }
                let heading = heading.rsplit('#').next().unwrap_or(heading);
                self.tree.options().slugs.slug(heading)
            }
            LinkKind::EmailAutolink if !dest.to_ascii_lowercase().starts_with("mailto:") => {
                let url = format!("mailto:{dest}");
                return url_allowed(&url).then(|| str_lit(&url));
            }
            _ => match dest.strip_prefix('#') {
                Some(fragment) => fragment.to_owned(),
                None => return url_allowed(dest).then(|| str_lit(dest)),
            },
        };
        self.targets
            .contains(&anchor)
            .then(|| format!("<{}>", label(&anchor)))
    }

    fn open_tag(&mut self, tag: Tag, c: &InlineContent) {
        match tag {
            Tag::Link(i) => match c.links.get(i as usize).and_then(|l| self.link_target(l)) {
                Some(target) => {
                    let _ = write!(self.out, "#link({target})[");
                }
                None => self.out.push_str("#["),
            },
            Tag::Strong => self.out.push_str("#strong["),
            Tag::Em => self.out.push_str("#emph["),
            Tag::Del => self.out.push_str("#strike["),
            Tag::Mark => {
                let fill = self.colors.token("status.warning_tint");
                let _ = write!(self.out, "#highlight(fill: {fill})[");
            }
        }
    }

    fn set_tags(&mut self, open: &mut Vec<Tag>, want: &[Tag], c: &InlineContent) {
        let common = open.iter().zip(want).take_while(|(a, b)| a == b).count();
        while open.len() > common {
            open.pop();
            self.out.push(']');
        }
        for &tag in &want[common..] {
            self.open_tag(tag, c);
            open.push(tag);
        }
    }

    /// Emits items `items` of `c`. `math_base` is the number of the first
    /// formula of `c` in its node `ix`.
    fn inline(
        &mut self,
        c: &InlineContent,
        items: std::ops::Range<usize>,
        ix: usize,
        math_base: usize,
    ) {
        let para = c.paragraph.clone();
        let mut open: Vec<Tag> = Vec::new();
        let mut images_done: HashSet<u32> = HashSet::new();
        for k in items {
            let Some(span) = c.spans.get(k) else {
                break;
            };
            let item = &para.items()[k];
            let range = item.range();
            let mut want: Vec<Tag> = Vec::new();
            let link = span.link;
            match span.role {
                InlineRole::Text | InlineRole::Code | InlineRole::Tag => {
                    if !matches!(item.kind(), RunKind::Text(_)) {
                        continue;
                    }
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
                    if !mono
                        && span.role != InlineRole::Tag
                        && style.is_some_and(|s| s.background.is_some())
                    {
                        want.push(Tag::Mark);
                    }
                    self.set_tags(&mut open, &want, c);
                    let t = &para.text()[range];
                    if mono {
                        let _ = write!(self.out, "#gvic({});", str_lit(t));
                    } else {
                        text(&mut self.out, t);
                    }
                }
                InlineRole::Break => {
                    self.set_tags(&mut open, &[], c);
                    self.out.push_str("#linebreak();");
                }
                InlineRole::RawHtml => {
                    let raw = para.text()[range].to_ascii_lowercase();
                    if raw.starts_with("<br") {
                        self.set_tags(&mut open, &[], c);
                        self.out.push_str("#linebreak();");
                    }
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
                    let (dest, alt) = (image.destination.to_string(), image.alt.to_string());
                    self.image(&dest, &alt, true);
                }
                InlineRole::LinkSuffix
                | InlineRole::CellGap(_)
                | InlineRole::FootnoteBackref(_) => {}
                InlineRole::Math(i) => {
                    let Some(m) = c.math.get(i as usize) else {
                        continue;
                    };
                    if let Some(l) = link {
                        want.push(Tag::Link(l));
                    }
                    self.set_tags(&mut open, &want, c);
                    let macros = self.tree.math_macros(ix, math_base + i as usize);
                    let tex = m.tex.to_string();
                    match math::svg_box(&tex, m.display, &macros)
                        .ok()
                        .and_then(|b| Some((b.height, b.depth, self.svg_asset(b.svg)?)))
                    {
                        Some((height, depth, path)) => {
                            let _ = write!(
                                self.out,
                                "#box(baseline: {}em, image({}, height: {}em));",
                                num(depth),
                                str_lit(&path),
                                num(height)
                            );
                        }
                        None => {
                            let _ = write!(self.out, "#gverror({});", str_lit(&tex));
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
                    let fn_label = format!("gv-fn-{n}");
                    if self.referenced.insert(n) {
                        let body = self.footnotes.get(&n).cloned().unwrap_or_default();
                        let _ = write!(self.out, "#footnote[{body}] <{fn_label}>");
                    } else if self.footnotes.contains_key(&n) {
                        let _ = write!(self.out, "#footnote(<{fn_label}>)");
                    } else {
                        let _ = write!(self.out, "#super[#{};]", str_lit(&n.to_string()));
                    }
                }
            }
        }
        self.set_tags(&mut open, &[], c);
    }
}

/// Whether an SVG refers to nothing outside itself (Typst fails the whole
/// document on an external reference).
fn svg_is_self_contained(svg: &str) -> bool {
    let lower = svg.to_ascii_lowercase();
    if lower.contains("<image") || lower.contains("@import") || lower.contains("<foreignobject") {
        return false;
    }
    for attr in ["href=\"", "href='"] {
        let mut rest = lower.as_str();
        while let Some(i) = rest.find(attr) {
            rest = &rest[i + attr.len()..];
            if !rest.starts_with('#') {
                return false;
            }
        }
    }
    true
}

/// The text of an HTML fragment: tags, comments and the content of script
/// and style elements dropped, a few entities decoded, white space
/// collapsed.
fn html_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    let b = html.as_bytes();
    while i < b.len() {
        if b[i] == b'<' {
            if lower[i..].starts_with("<!--") {
                i = lower[i..].find("-->").map_or(b.len(), |e| i + e + 3);
                continue;
            }
            let skip = [
                "script", "style", "textarea", "title", "noscript", "template",
            ]
            .iter()
            .find(|t| {
                lower[i + 1..].starts_with(*t)
                    && !lower[i + 1 + t.len()..].starts_with(|c: char| c.is_ascii_alphanumeric())
            });
            if let Some(t) = skip {
                let close = format!("</{t}");
                i = lower[i..].find(&close).map_or(b.len(), |e| i + e);
                i = lower[i..].find('>').map_or(b.len(), |e| i + e + 1);
                continue;
            }
            i = lower[i..].find('>').map_or(b.len(), |e| i + e + 1);
            out.push(' ');
            continue;
        }
        let next = html[i..].find('<').map_or(html.len(), |e| i + e);
        out.push_str(&decode_entities(&html[i..next]));
        i = next;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|e| *e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => name
                    .strip_prefix("#x")
                    .or_else(|| name.strip_prefix("#X"))
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .and_then(char::from_u32),
            }
            .map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of inline content.
fn plain(c: &InlineContent) -> String {
    let para = &c.paragraph;
    let mut s = String::new();
    for (k, span) in c.spans.iter().enumerate() {
        match span.role {
            InlineRole::Text | InlineRole::Code | InlineRole::ImageAlt(_) => {
                s.push_str(para.item_text(k));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_escape_everything_typst_reads() {
        assert_eq!(str_lit("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(str_lit("x\u{0}\u{1b}y\tz\n"), "\"xy z \"");
        assert_eq!(
            str_lit("#read(\"/etc/passwd\")"),
            "\"#read(\\\"/etc/passwd\\\")\""
        );
    }

    #[test]
    fn labels_are_always_valid() {
        assert_eq!(label("intro"), "gv-intro");
        assert_eq!(label("a b>c"), "gv-a_20b_3ec");
        assert_eq!(label("été"), "gv-_c3_a9t_c3_a9");
    }

    #[test]
    fn only_web_and_mail_links() {
        assert!(url_allowed("https://x.org"));
        assert!(url_allowed("mailto:a@b.c"));
        assert!(!url_allowed("javascript:alert(1)"));
        assert!(!url_allowed("file:///etc/passwd"));
    }

    #[test]
    fn svgs_must_be_self_contained() {
        assert!(svg_is_self_contained("<svg><use href=\"#a\"/></svg>"));
        assert!(!svg_is_self_contained("<svg><image href=\"x.png\"/></svg>"));
        assert!(!svg_is_self_contained("<svg><a href=\"/etc\"/></svg>"));
    }

    #[test]
    fn html_becomes_its_text() {
        assert_eq!(
            html_text(
                "<div align=\"center\"><b>bold</b> &amp; <script>alert(1)</script>x &#x41;</div>"
            ),
            "bold & x A"
        );
        assert_eq!(html_text("<!-- c --><p>a<br>b</p>"), "a b");
    }

    #[test]
    fn numbers_are_short_and_finite() {
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(2.0), "2");
        assert_eq!(num(f32::NAN), "0");
        assert_eq!(num(-0.0001), "0");
    }
}

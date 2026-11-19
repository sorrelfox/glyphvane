// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Highlighting of the Source view from the syntax tree.
//!
//! Lines are highlighted on demand, a window around the rows on screen at a
//! time, in three layers that each paint over the one before:
//!
//! 1. **Block tokens** ([`SyntaxTree::tokens`]): container prefixes, list
//!    and quote markers, `#` runs, fences, setext underlines and thematic
//!    breaks are markers; a fence's info string is a tag.
//! 2. **Leaves** ([`walk_range`]): heading text is bold; fenced code lines
//!    get the code fill, and fences with a known language the classes of
//!    the code highlighter ([`FenceCode`]: results come from background
//!    jobs, never from the UI thread);
//!    HTML blocks and comments, link reference definitions, front matter
//!    (YAML and TOML keys and values), callout markers on a quote's first
//!    line, and pipe tables inside paragraphs (rows whose second line is a
//!    delimiter row) are recognised here.
//! 3. **Inlines** ([`SyntaxTree::inlines`]): emphasis, code spans, links,
//!    images, autolinks and raw HTML. Link targets take the style of their
//!    resolution status (SPEC § Link status, editor column).
//!
//! The colours are design tokens ([`Token`]); the table in SPEC § Editor
//! decides which.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;

use gv_highlight::HighlightKey;
use gv_render_tree::{LinkStatus, LinkStatusSource, LinkTarget};
use gv_syntax::inline::{InlineKind, InlineNode, LinkKind as SyntaxLinkKind, LinkRef};
use gv_syntax::{
    BlockId, Container, ContainerKind, FrontMatterKind, Leaf, LeafKind, SyntaxTree, TokenKind,
    Visitor, walk_range,
};
use gv_text::TextSnapshot;
use gv_vault::{PathIndex, ProfileId};

use super::syntax::Syntax;
use crate::inline::Token;
use crate::reader::Resolver;
use crate::reader::code_highlight::{self, Jobs, LineSpans};

/// How a link target is underlined (SPEC § Link status). Dotted and dashed
/// lines take the text's colour, the wavy line `status.warning`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Underline {
    Dotted,
    Dashed,
    Wavy,
}

/// The look of a run of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Style {
    pub color: Token,
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<Underline>,
    pub strike: bool,
}

impl Style {
    /// Body text.
    pub const PLAIN: Style = Style {
        color: Token::TextPrimary,
        bold: false,
        italic: false,
        underline: None,
        strike: false,
    };

    /// Markdown markers: `#`, `**`, `>`, `-`, pipes, backticks, brackets,
    /// fences.
    pub const MARKER: Style = Style::of(Token::TextTertiary);

    /// Comments.
    pub const COMMENT: Style = Style {
        italic: true,
        ..Style::of(Token::TextSecondary)
    };

    pub const fn of(color: Token) -> Style {
        Style {
            color,
            ..Style::PLAIN
        }
    }
}

/// The highlighting of one line.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LineStyle {
    /// Runs over the line's content, in line-relative bytes, in order and
    /// without gaps. Empty for an empty line.
    pub runs: Vec<(Range<usize>, Style)>,
    /// A fenced code line: drawn on the code fill.
    pub code: bool,
}

impl LineStyle {
    /// The style at line-relative byte `at`.
    #[cfg(test)]
    pub fn style_at(&self, at: usize) -> Style {
        self.runs
            .iter()
            .find(|(r, _)| r.contains(&at))
            .map_or(Style::PLAIN, |(_, s)| *s)
    }

    /// The runs clamped to a line of `len` bytes (when the text moved on
    /// since the line was highlighted), extended plain to the end.
    pub fn clamped(&self, len: usize) -> Vec<(Range<usize>, Style)> {
        let mut out: Vec<(Range<usize>, Style)> = self
            .runs
            .iter()
            .filter(|(r, _)| r.start < len)
            .map(|(r, s)| (r.start..r.end.min(len), *s))
            .collect();
        let end = out.last().map_or(0, |(r, _)| r.end);
        if end < len {
            out.push((end..len, Style::PLAIN));
        }
        out
    }
}

/// The resolution statuses of link targets, asked once per destination.
pub(crate) struct LinkStatuses {
    resolver: Option<Resolver>,
    /// The document's profile (link resolution mode).
    profile: ProfileId,
    cache: HashMap<(gv_model::LinkKind, bool, String), LinkStatus>,
    /// Bumped when the statuses may have changed (a new index).
    generation: u64,
}

impl std::fmt::Debug for LinkStatuses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkStatuses")
            .field("resolver", &self.resolver)
            .field("cached", &self.cache.len())
            .field("generation", &self.generation)
            .finish()
    }
}

impl LinkStatuses {
    /// The statuses of the links of `file` in `profile`, through the
    /// window's `index` when there is one (see [`Resolver::statuses`]).
    /// Without a file every link is pending.
    pub fn new(file: Option<&Path>, index: Option<&PathIndex>, profile: ProfileId) -> Self {
        Self {
            resolver: file.map(|f| Resolver::statuses(f, index, profile)),
            profile,
            cache: HashMap::new(),
            generation: 0,
        }
    }

    /// Resolves again with a new index.
    pub fn reset(&mut self, file: Option<&Path>, index: Option<&PathIndex>) {
        self.resolver = file.map(|f| Resolver::statuses(f, index, self.profile));
        self.cache.clear();
        self.generation += 1;
    }

    /// The resolver links are resolved with, once the document has a file.
    pub(crate) fn resolver(&self) -> Option<&Resolver> {
        self.resolver.as_ref()
    }

    /// The profile links are resolved in.
    pub fn profile(&self) -> ProfileId {
        self.profile
    }

    /// Resolves again in `profile`.
    pub fn set_profile(&mut self, profile: ProfileId) {
        self.profile = profile;
        self.resolver = self.resolver.take().map(|r| r.with_profile(profile));
        self.cache.clear();
        self.generation += 1;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The status of a link (cached).
    pub(crate) fn status_of(
        &mut self,
        kind: gv_model::LinkKind,
        image: bool,
        destination: &str,
    ) -> LinkStatus {
        self.status(kind, image, destination)
    }

    /// The link resolved again, for its target and candidates.
    pub(crate) fn resolution(
        &self,
        kind: gv_model::LinkKind,
        image: bool,
        destination: &str,
    ) -> Option<gv_vault::Resolution> {
        let resolver = self.resolver.as_ref()?;
        Some(resolver.resolve(crate::reader::resolver_kind(kind, image), destination))
    }

    fn status(&mut self, kind: gv_model::LinkKind, image: bool, destination: &str) -> LinkStatus {
        let Some(resolver) = &self.resolver else {
            return LinkStatus::Pending;
        };
        if let Some(status) = self.cache.get(&(kind, image, destination.to_owned())) {
            return *status;
        }
        let status = resolver.status(&LinkTarget {
            kind,
            image,
            destination,
            title: None,
            range: 0..0,
            block: BlockId(0),
            from: None,
        });
        if self.cache.len() > 4096 {
            self.cache.clear();
        }
        self.cache
            .insert((kind, image, destination.to_owned()), status);
        status
    }
}

/// The style of a link target with `status` (SPEC § Link status, editor
/// column).
pub(crate) fn link_style(status: LinkStatus) -> Style {
    let accent = Style::of(Token::AccentDefault);
    match status {
        LinkStatus::Exact | LinkStatus::Pending => accent,
        LinkStatus::CaseFold | LinkStatus::Loose | LinkStatus::OutsideRoot => Style {
            underline: Some(Underline::Dotted),
            ..accent
        },
        LinkStatus::Ambiguous(_) => Style {
            underline: Some(Underline::Wavy),
            ..accent
        },
        LinkStatus::Unresolved => Style {
            underline: Some(Underline::Dashed),
            ..accent
        },
        LinkStatus::External => Style::of(Token::SyntaxUrl),
        LinkStatus::Blocked => Style {
            strike: true,
            ..Style::of(Token::TextSecondary)
        },
    }
}

fn model_kind(kind: SyntaxLinkKind) -> gv_model::LinkKind {
    match kind {
        SyntaxLinkKind::Inline => gv_model::LinkKind::Inline,
        SyntaxLinkKind::Full => gv_model::LinkKind::Full,
        SyntaxLinkKind::Collapsed => gv_model::LinkKind::Collapsed,
        SyntaxLinkKind::Shortcut => gv_model::LinkKind::Shortcut,
        SyntaxLinkKind::UriAutolink => gv_model::LinkKind::UriAutolink,
        SyntaxLinkKind::EmailAutolink => gv_model::LinkKind::EmailAutolink,
    }
}

/// Highlighted lines, for one shown tree and one set of link statuses.
#[derive(Debug, Default)]
pub(crate) struct HighlightCache {
    key: Option<(u64, u64)>,
    lines: HashMap<usize, Rc<LineStyle>>,
    /// The fences' code highlighting.
    pub code: FenceCode,
    /// Plain-text mode ([`super::plain`]): no Markdown roles; the whole
    /// text is code of this language, if any.
    plain: Option<Option<String>>,
    /// Live Preview's styles ([`Self::set_live`]).
    live: bool,
}

/// Code highlighting of fenced code in the Source view.
///
/// A fence's content lines are joined with `\n` and looked up in the
/// shared cache by content ([`code_highlight::key_for`]), so an edit
/// re-highlights only the fence it changes. A fence not highlighted yet
/// is coloured with its previous result line by line, or drawn plain, and
/// a background job is asked for ([`Self::take_requests`]).
#[derive(Debug, Default)]
pub(crate) struct FenceCode {
    jobs: Jobs,
    /// Jobs to start: the key and the joined text.
    requests: Vec<(HighlightKey, String)>,
    /// The last result drawn per fence (by its opening line).
    last: HashMap<usize, LineSpans>,
}

/// Fences remembered at most (all are forgotten beyond).
const FENCES_CAP: usize = 4_096;

impl FenceCode {
    /// The jobs to start (each key once).
    pub fn take_requests(&mut self) -> Vec<(HighlightKey, String)> {
        std::mem::take(&mut self.requests)
    }

    /// The highlighting of fence `leaf` (opened by info string `info`), if
    /// there is one to draw now.
    fn spans(&mut self, text: &TextSnapshot, leaf: &Leaf<'_>, info: &str) -> Option<LineSpans> {
        let contents = leaf.lines.iter().map(|l| l.content.clone());
        self.spans_of(text, contents, leaf.open_line, info)
    }

    /// The highlighting of the whole of `text` as code of `info` (plain-text
    /// mode), if there is one to draw now.
    fn whole(&mut self, text: &TextSnapshot, info: &str) -> Option<LineSpans> {
        let contents = (0..text.line_count()).filter_map(|line| text.line_range(line).ok());
        self.spans_of(text, contents, 0, info)
    }

    /// The highlighting of the lines of `text` whose contents are
    /// `contents`, remembered under `open_line`.
    fn spans_of(
        &mut self,
        text: &TextSnapshot,
        contents: impl Iterator<Item = Range<usize>> + Clone,
        open_line: usize,
        info: &str,
    ) -> Option<LineSpans> {
        // The content, unless it is over the cap anyway.
        let bytes: usize = contents.clone().map(|c| c.len() + 1).sum();
        if bytes > gv_highlight::Limits::default().max_bytes {
            return None;
        }
        let mut code = String::with_capacity(bytes);
        let mut lines: Vec<Range<usize>> = Vec::new();
        for (i, content) in contents.enumerate() {
            if i > 0 {
                code.push('\n');
            }
            let start = code.len();
            if let Ok(t) = text.text_for_range(content) {
                code.push_str(&t);
            }
            lines.push(start..code.len());
        }
        let key = code_highlight::key_for(&code, info)?;
        if self.last.len() > FENCES_CAP {
            self.last.clear();
        }
        if let Some(h) = code_highlight::cached(&key) {
            let spans = LineSpans::new(h, lines.into());
            self.last.insert(open_line, spans.clone());
            return Some(spans);
        }
        if self.jobs.start(key) {
            self.requests.push((key, code));
        }
        let last = self.last.get(&open_line).cloned();
        #[cfg(test)]
        if last.is_none() {
            code_highlight::tests::count_pending_draw();
        }
        last
    }
}

/// Lines highlighted around the ones asked for, so that scrolling a few
/// rows does not walk the tree again.
const MARGIN: usize = 32;
/// Cached lines kept at most.
const MAX_LINES: usize = 20_000;

impl HighlightCache {
    /// Plain-text mode (`Some`), with the code language of the whole text,
    /// or Markdown (`None`).
    pub fn set_plain(&mut self, plain: Option<Option<String>>) {
        self.plain = plain;
        self.key = None;
        self.lines.clear();
    }

    /// Live Preview's styles (`true`: link text as links, struck
    /// strikethrough, headings by level) or the Source view's.
    pub fn set_live(&mut self, live: bool) {
        if self.live != live {
            self.live = live;
            self.key = None;
            self.lines.clear();
        }
    }

    /// Makes sure `lines` are highlighted with the shown tree of `syntax`.
    pub fn ensure(&mut self, syntax: &Syntax, links: &mut LinkStatuses, lines: Range<usize>) {
        let key = (syntax.generation(), links.generation());
        if self.key != Some(key) || self.lines.len() > MAX_LINES {
            self.key = Some(key);
            self.lines.clear();
        }
        let missing: Vec<usize> = lines
            .clone()
            .filter(|l| !self.lines.contains_key(l))
            .collect();
        let (Some(&first), Some(&last)) = (missing.first(), missing.last()) else {
            return;
        };
        let (tree, text) = syntax.shown();
        let count = text.line_count();
        let window = first.saturating_sub(MARGIN)..(last + 1 + MARGIN).min(count);
        if window.is_empty() {
            return;
        }
        let styles = match &self.plain {
            Some(info) => highlight_plain(text, window, info.as_deref(), &mut self.code),
            None => highlight_with(tree, text, window, links, &mut self.code, self.live),
        };
        for (line, style) in styles {
            self.lines.entry(line).or_insert_with(|| Rc::new(style));
        }
    }

    /// The job for `key` ended: its fence is highlighted again.
    pub fn landed(&mut self, key: &HighlightKey) {
        self.code.jobs.finish(key);
        self.lines.clear();
    }

    /// The highlighting of `line`, once [`Self::ensure`]d.
    pub fn get(&self, line: usize) -> Option<&Rc<LineStyle>> {
        self.lines.get(&line)
    }

    /// Caches `style` for `line`.
    #[cfg(test)]
    pub fn insert_for_test(&mut self, line: usize, style: LineStyle) {
        self.lines.insert(line, Rc::new(style));
    }
}

/// Per-byte styles of a window of lines, painted layer by layer.
struct Painter<'t> {
    text: &'t TextSnapshot,
    first: usize,
    lines: Vec<LineBuf>,
}

struct LineBuf {
    start: usize,
    styles: Vec<Style>,
    code: bool,
}

impl<'t> Painter<'t> {
    fn new(text: &'t TextSnapshot, lines: Range<usize>) -> Self {
        let bufs = lines
            .clone()
            .map(|line| {
                let range = text.line_range(line).unwrap_or(0..0);
                LineBuf {
                    start: range.start,
                    styles: vec![Style::PLAIN; range.len()],
                    code: false,
                }
            })
            .collect();
        Self {
            text,
            first: lines.start,
            lines: bufs,
        }
    }

    /// The document bytes the window covers.
    fn range(&self) -> Range<usize> {
        match (self.lines.first(), self.lines.last()) {
            (Some(a), Some(b)) => a.start..b.start + b.styles.len(),
            _ => 0..0,
        }
    }

    /// Applies `f` to the style of every byte of `range` in the window.
    fn paint(&mut self, range: Range<usize>, f: impl Fn(&mut Style)) {
        let window = self.range();
        let start = range.start.max(window.start);
        let end = range.end.min(window.end);
        if start >= end {
            return;
        }
        let Ok(mut line) = self.text.line_of_offset(start) else {
            return;
        };
        while let Some(buf) = line
            .checked_sub(self.first)
            .and_then(|i| self.lines.get_mut(i))
        {
            if buf.start >= end {
                break;
            }
            let a = start.saturating_sub(buf.start).min(buf.styles.len());
            let b = (end - buf.start).min(buf.styles.len());
            for style in &mut buf.styles[a..b] {
                f(style);
            }
            line += 1;
        }
    }

    fn set(&mut self, range: Range<usize>, style: Style) {
        self.paint(range, |s| *s = style);
    }

    /// Marks the lines holding bytes `range` as fenced code.
    fn code(&mut self, range: Range<usize>) {
        let (Ok(first), Ok(last)) = (
            self.text.line_of_offset(range.start),
            self.text
                .line_of_offset(range.end.saturating_sub(1).max(range.start)),
        ) else {
            return;
        };
        for line in first..=last {
            if let Some(buf) = line
                .checked_sub(self.first)
                .and_then(|i| self.lines.get_mut(i))
            {
                buf.code = true;
            }
        }
    }

    fn finish(self) -> Vec<(usize, LineStyle)> {
        let first = self.first;
        self.lines
            .into_iter()
            .enumerate()
            .map(|(i, buf)| {
                let mut runs: Vec<(Range<usize>, Style)> = Vec::new();
                for (at, style) in buf.styles.iter().enumerate() {
                    match runs.last_mut() {
                        Some((r, s)) if s == style => r.end = at + 1,
                        _ => runs.push((at..at + 1, *style)),
                    }
                }
                (
                    first + i,
                    LineStyle {
                        runs,
                        code: buf.code,
                    },
                )
            })
            .collect()
    }
}

/// Highlights `lines` of `text` with `tree` (the tree of `text`).
#[cfg(test)]
pub(crate) fn highlight(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    lines: Range<usize>,
    links: &mut LinkStatuses,
    code: &mut FenceCode,
) -> Vec<(usize, LineStyle)> {
    highlight_with(tree, text, lines, links, code, false)
}

/// Highlights `lines` of `text` with `tree` (the tree of `text`), with
/// Live Preview's styles when `live`.
pub(crate) fn highlight_with(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    lines: Range<usize>,
    links: &mut LinkStatuses,
    code: &mut FenceCode,
    live: bool,
) -> Vec<(usize, LineStyle)> {
    let mut painter = Painter::new(text, lines);
    let range = painter.range();
    if tree.len() != text.len() {
        return painter.finish();
    }
    // 1. Block tokens.
    for (r, token) in tree.tokens(range.clone()) {
        match token.kind {
            TokenKind::ContainerPrefix
            | TokenKind::QuoteMarker
            | TokenKind::ListMarker
            | TokenKind::Marker => painter.set(r, Style::MARKER),
            // Fence languages and the markers of task boxes, footnote
            // labels and alert types.
            TokenKind::InfoString | TokenKind::ExtMarker => {
                painter.set(r, Style::of(Token::SyntaxTag));
            }
            TokenKind::Bom
            | TokenKind::Whitespace
            | TokenKind::CodeIndent
            | TokenKind::Content
            | TokenKind::LineEnding => {}
        }
    }
    // 2 and 3. Leaves and their inlines.
    let mut leaves = Leaves {
        tree,
        text,
        painter: &mut painter,
        links,
        code,
        containers: Vec::new(),
        live,
    };
    walk_range(tree, range, &mut leaves);
    painter.finish()
}

/// Highlights `lines` of `text` in plain-text mode: plain text, coloured
/// by the code highlighter as language `info` when there is one.
pub(crate) fn highlight_plain(
    text: &TextSnapshot,
    lines: Range<usize>,
    info: Option<&str>,
    code: &mut FenceCode,
) -> Vec<(usize, LineStyle)> {
    let mut painter = Painter::new(text, lines.clone());
    let spans = info.and_then(|info| code.whole(text, info));
    if let Some(spans) = spans {
        for line in lines {
            let Ok(c) = text.line_range(line) else {
                continue;
            };
            for (r, class) in spans.line(line, c.len()) {
                let font = class.font_style();
                let style = Style {
                    bold: font.bold,
                    italic: font.italic,
                    ..Style::of(code_highlight::token_of(class))
                };
                painter.set(c.start + r.start..c.start + r.end, style);
            }
        }
    }
    painter.finish()
}

struct Leaves<'a, 't> {
    tree: &'a SyntaxTree,
    text: &'t TextSnapshot,
    painter: &'a mut Painter<'t>,
    links: &'a mut LinkStatuses,
    code: &'a mut FenceCode,
    containers: Vec<Container>,
    /// Live Preview's styles.
    live: bool,
}

impl Visitor for Leaves<'_, '_> {
    fn enter(&mut self, container: &Container) {
        self.containers.push(container.clone());
    }

    fn exit(&mut self, _: &Container) {
        self.containers.pop();
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        match leaf.kind {
            LeafKind::Heading { level, .. } => {
                // Live Preview: a style per level (not designed).
                let color = match level {
                    1 if self.live => Some(Token::AccentDefault),
                    3.. if self.live => Some(Token::TextSecondary),
                    _ => None,
                };
                let italic = self.live && level >= 4;
                for line in leaf.lines {
                    self.painter.paint(line.content.clone(), |s| {
                        s.bold = true;
                        s.italic |= italic;
                        if let Some(color) = color {
                            s.color = color;
                        }
                    });
                }
                self.inlines(leaf);
            }
            LeafKind::Paragraph => {
                self.inlines(leaf);
                self.table(leaf);
                self.callout(leaf);
            }
            // An Obsidian callout's title (its `[!type]` is an extension
            // marker, painted with the block tokens).
            LeafKind::CalloutTitle => {
                for line in leaf.lines {
                    self.painter.paint(line.content.clone(), |s| s.bold = true);
                }
                self.inlines(leaf);
            }
            // An Obsidian `%%` block comment.
            LeafKind::Comment => self.painter.set(leaf.range.clone(), Style::COMMENT),
            // A GFM table: its lines are the header, the delimiter row and
            // the body rows, as `table` expects.
            LeafKind::Table => self.table(leaf),
            LeafKind::FencedCode => {
                self.painter.code(leaf.range.clone());
                let info = leaf
                    .info
                    .clone()
                    .and_then(|r| self.text.text_for_range(r).ok())
                    .unwrap_or_default();
                if let Some(spans) = self.code.spans(self.text, leaf, &info) {
                    self.fence(leaf, &spans);
                }
            }
            // `$$` math: TeX, like code.
            LeafKind::MathBlock => self.painter.code(leaf.range.clone()),
            LeafKind::IndentedCode | LeafKind::ThematicBreak => {}
            LeafKind::Html { kind } => {
                let style = if kind == 2 {
                    Style::COMMENT
                } else {
                    Style::of(Token::TextSecondary)
                };
                for line in leaf.lines {
                    self.painter.set(line.content.clone(), style);
                }
            }
            LeafKind::LinkRefDef => self.definition(leaf),
            LeafKind::FrontMatter { kind } => {
                let lex = match kind {
                    FrontMatterKind::Yaml => yaml_line,
                    FrontMatterKind::Toml => toml_line,
                };
                for line in leaf.lines {
                    self.lexed(line.content.clone(), lex);
                }
            }
        }
    }
}

/// The context of an inline node: what its ancestors make of its text.
#[derive(Clone, Copy, Debug, Default)]
struct Ctx {
    bold: bool,
    italic: bool,
    /// The style of a link target, inside a link, image or autolink.
    target: Option<Style>,
}

impl Leaves<'_, '_> {
    fn inlines(&mut self, leaf: &Leaf<'_>) {
        let tree = self.tree.inlines(leaf, self.text);
        for node in tree.top_level() {
            self.inline(node, Ctx::default());
        }
    }

    fn inline(&mut self, node: InlineNode<'_>, ctx: Ctx) {
        let range = node.range();
        match node.kind() {
            InlineKind::Strong => {
                let ctx = Ctx { bold: true, ..ctx };
                node.children().for_each(|c| self.inline(c, ctx));
            }
            InlineKind::Emphasis => {
                let ctx = Ctx {
                    italic: true,
                    ..ctx
                };
                node.children().for_each(|c| self.inline(c, ctx));
            }
            InlineKind::Strikethrough if self.live => {
                node.children().for_each(|c| self.inline(c, ctx));
                let markers: Vec<Range<usize>> = node
                    .children()
                    .filter(|c| c.kind() == InlineKind::Marker)
                    .map(|c| c.range())
                    .collect();
                if let (Some(open), Some(close)) = (markers.first(), markers.last()) {
                    self.painter
                        .paint(open.end..close.start, |s| s.strike = true);
                }
            }
            InlineKind::CodeSpan
            | InlineKind::Strikethrough
            | InlineKind::InlineMath
            | InlineKind::DisplayMath
            | InlineKind::FootnoteRef => {
                node.children().for_each(|c| self.inline(c, ctx));
            }
            // Raw HTML, or a comment (which may run over several lines).
            InlineKind::RawHtml => {
                let style = if node.text().starts_with("<!--") {
                    Style::COMMENT
                } else {
                    Style::of(Token::TextSecondary)
                };
                for child in node.children() {
                    self.painter.set(child.range(), style);
                }
            }
            InlineKind::Highlight | InlineKind::InlineFootnote => {
                node.children().for_each(|c| self.inline(c, ctx));
            }
            // Obsidian: wikilinks and embeds (the target in its status's
            // style), `%%` comments, tags and block ids.
            InlineKind::Wikilink | InlineKind::Embed => {
                let target = node.wikilink().map(|w| {
                    let kind = if w.embed {
                        gv_model::LinkKind::Embed
                    } else {
                        gv_model::LinkKind::Wikilink
                    };
                    link_style(self.links.status(kind, false, &w.inner))
                });
                let ctx = Ctx { target, ..ctx };
                node.children().for_each(|c| self.inline(c, ctx));
            }
            InlineKind::Comment => self.painter.set(range, Style::COMMENT),
            InlineKind::Tag => self.painter.set(range, Style::of(Token::AccentDefault)),
            InlineKind::BlockId => self.painter.set(range, Style::MARKER),
            InlineKind::WikiTarget => {
                let style = ctx.target.unwrap_or(Style::of(Token::AccentDefault));
                self.painter.set(range, style);
            }
            InlineKind::WikiAlias => self.painter.paint(range, |s| {
                s.bold |= ctx.bold;
                s.italic |= ctx.italic;
                // Live Preview shows only the alias: in the link's style.
                if let Some(target) = ctx.target.filter(|_| self.live) {
                    s.color = target.color;
                    s.underline = target.underline;
                }
            }),
            InlineKind::CommentText => self.painter.set(range, Style::COMMENT),
            InlineKind::TagText => self.painter.set(range, Style::of(Token::AccentDefault)),
            InlineKind::BlockIdText => self.painter.set(range, Style::MARKER),
            InlineKind::Link | InlineKind::Image | InlineKind::Autolink => {
                let target = node.link().map(|link| self.target_style(&link));
                let ctx = Ctx { target, ..ctx };
                node.children().for_each(|c| self.inline(c, ctx));
                // Live Preview shows a link's text in its target's style.
                if self.live
                    && node.kind() == InlineKind::Link
                    && let (Some(target), Some(text)) =
                        (target, super::live_preview::link_text(&node))
                {
                    self.painter.paint(text, |s| {
                        s.color = target.color;
                        s.underline = target.underline;
                    });
                }
            }
            InlineKind::Text
            | InlineKind::Entity
            | InlineKind::SoftBreak
            | InlineKind::Whitespace => self.painter.paint(range, |s| {
                s.bold |= ctx.bold;
                s.italic |= ctx.italic;
            }),
            InlineKind::Escape | InlineKind::HardBreak => {
                let backslash = node.text().starts_with('\\');
                if backslash {
                    self.painter
                        .set(range.start..range.start + 1, Style::MARKER);
                    self.painter.paint(range.start + 1..range.end, |s| {
                        s.bold |= ctx.bold;
                        s.italic |= ctx.italic;
                    });
                }
            }
            InlineKind::Marker => self.painter.set(range, Style::MARKER),
            InlineKind::CodeText | InlineKind::MathText => {
                self.painter.paint(range, |s| s.color = Token::SyntaxCode);
            }
            InlineKind::HtmlText => self.painter.set(range, Style::of(Token::TextSecondary)),
            InlineKind::AutolinkText
            | InlineKind::LinkDestination
            | InlineKind::LinkLabel
            | InlineKind::FootnoteLabel => {
                let style = ctx.target.unwrap_or(Style::of(Token::AccentDefault));
                self.painter.set(range, style);
            }
            InlineKind::LinkTitle => self.painter.set(range, Style::of(Token::SyntaxString)),
        }
    }

    fn target_style(&mut self, link: &LinkRef<'_>) -> Style {
        let kind = model_kind(link.kind);
        let destination = if link.kind == SyntaxLinkKind::EmailAutolink {
            format!("mailto:{}", link.destination)
        } else {
            link.destination.to_owned()
        };
        link_style(self.links.status(kind, link.is_image, &destination))
    }

    /// `[label]: destination "title"`.
    fn definition(&mut self, leaf: &Leaf<'_>) {
        let Some(def) = leaf.link_ref_def(self.text) else {
            return;
        };
        self.painter
            .set(def.range.start..def.label_range.start, Style::MARKER);
        self.painter.set(
            def.label_range.end..def.destination_range.start,
            Style::MARKER,
        );
        let status = self
            .links
            .status(gv_model::LinkKind::Full, false, &def.def.destination);
        self.painter
            .set(def.destination_range.clone(), link_style(status));
        if let Some(title) = def.title_range {
            self.painter.set(title, Style::of(Token::SyntaxString));
        }
    }

    /// A paragraph whose second line is a delimiter row is a pipe table:
    /// the header cells are bold, the delimiter row and the pipes markers.
    fn table(&mut self, leaf: &Leaf<'_>) {
        let lines: Vec<(Range<usize>, String)> = leaf
            .lines
            .iter()
            .map(|l| {
                let text = self
                    .text
                    .text_for_range(l.content.clone())
                    .unwrap_or_default();
                (l.content.clone(), text)
            })
            .collect();
        let [(head_range, head), (delimiter_range, delimiter), ..] = lines.as_slice() else {
            return;
        };
        if !is_delimiter_row(delimiter) || !(head.contains('|') || delimiter.contains('|')) {
            return;
        }
        self.painter.paint(head_range.clone(), |s| {
            if s.color != Token::TextTertiary {
                s.bold = true;
            }
        });
        self.painter.set(delimiter_range.clone(), Style::MARKER);
        for (range, text) in &lines {
            let mut escaped = false;
            for (at, c) in text.char_indices() {
                if c == '|' && !escaped {
                    let start = range.start + at;
                    self.painter.set(start..start + 1, Style::MARKER);
                }
                escaped = c == '\\' && !escaped;
            }
        }
    }

    /// `[!NOTE]` at the start of a block quote's first paragraph.
    fn callout(&mut self, leaf: &Leaf<'_>) {
        let Some(quote) = self.containers.last() else {
            return;
        };
        if quote.kind != ContainerKind::BlockQuote || quote.line != leaf.start_line {
            return;
        }
        let Some(first) = leaf.lines.first() else {
            return;
        };
        let text = self
            .text
            .text_for_range(first.content.clone())
            .unwrap_or_default();
        if let Some(len) = callout_marker(&text) {
            let start = first.content.start;
            self.painter
                .set(start..start + len, Style::of(Token::SyntaxTag));
        }
    }

    /// Paints the lines of fence `leaf` with the classes of `spans`.
    fn fence(&mut self, leaf: &Leaf<'_>, spans: &LineSpans) {
        let window = self.painter.range();
        for (i, line) in leaf.lines.iter().enumerate() {
            let c = &line.content;
            if c.end < window.start || c.start > window.end {
                continue;
            }
            for (r, class) in spans.line(i, c.len()) {
                let font = class.font_style();
                let style = Style {
                    bold: font.bold,
                    italic: font.italic,
                    ..Style::of(code_highlight::token_of(class))
                };
                self.painter.set(c.start + r.start..c.start + r.end, style);
            }
        }
    }

    /// Paints `range` (one line) with the spans `lex` finds in its text.
    fn lexed(&mut self, range: Range<usize>, lex: Lexer) {
        let text = self.text.text_for_range(range.clone()).unwrap_or_default();
        for (r, style) in lex(&text) {
            self.painter
                .set(range.start + r.start..range.start + r.end, style);
        }
    }
}

/// Finds the styled spans of one line of a small language.
type Lexer = fn(&str) -> Vec<(Range<usize>, Style)>;

/// A GFM delimiter row: cells of dashes with optional colons.
fn is_delimiter_row(line: &str) -> bool {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    !line.is_empty()
        && line.split('|').all(|cell| {
            let cell = cell.trim();
            let cell = cell.strip_prefix(':').unwrap_or(cell);
            let cell = cell.strip_suffix(':').unwrap_or(cell);
            !cell.is_empty() && cell.bytes().all(|b| b == b'-')
        })
}

/// The length of a callout marker (`[!TYPE]`, with an optional `+` or `-`)
/// at the start of `text`.
fn callout_marker(text: &str) -> Option<usize> {
    let rest = text.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = &rest[..close];
    if kind.is_empty()
        || !kind
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let mut len = 2 + close + 1;
    if matches!(text.as_bytes().get(len), Some(b'+' | b'-')) {
        len += 1;
    }
    Some(len)
}

/// The style of a value: a quoted string, a number or flag, or a bare
/// string.
fn value_style(word: &str) -> Style {
    let lower = word.to_ascii_lowercase();
    if word.parse::<f64>().is_ok() || matches!(lower.as_str(), "true" | "false" | "null" | "~") {
        Style::of(Token::SyntaxNumber)
    } else {
        Style::of(Token::SyntaxString)
    }
}

/// The spans of a value after a key or a list dash.
fn value_spans(line: &str, start: usize, out: &mut Vec<(Range<usize>, Style)>) {
    let rest = &line[start..];
    let trimmed = rest.trim_start();
    let at = start + (rest.len() - trimmed.len());
    if trimmed.is_empty() {
        return;
    }
    if let Some(comment) = trimmed.find(" #") {
        let value = trimmed[..comment].trim_end();
        if !value.is_empty() {
            out.push((at..at + value.len(), value_style(value)));
        }
        out.push((at + comment + 1..line.len(), Style::COMMENT));
        return;
    }
    if trimmed.starts_with('#') {
        out.push((at..line.len(), Style::COMMENT));
        return;
    }
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        // A flow sequence or map: brackets and commas are markers.
        let mut item = at;
        for (i, c) in trimmed.char_indices() {
            let pos = at + i;
            if matches!(c, '[' | ']' | '{' | '}' | ',') {
                let value = line[item..pos].trim();
                if !value.is_empty() {
                    let offset =
                        item + (line[item..pos].len() - line[item..pos].trim_start().len());
                    out.push((offset..offset + value.len(), value_style(value)));
                }
                out.push((pos..pos + 1, Style::MARKER));
                item = pos + 1;
            }
        }
        return;
    }
    let value = trimmed.trim_end();
    out.push((at..at + value.len(), value_style(value)));
}

/// The spans of one line of YAML front matter.
fn yaml_line(line: &str) -> Vec<(Range<usize>, Style)> {
    let mut out = Vec::new();
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if trimmed.starts_with('#') {
        out.push((indent..line.len(), Style::COMMENT));
        return out;
    }
    let mut at = indent;
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or(trimmed.strip_prefix('-').filter(|r| r.is_empty()))
    {
        out.push((at..at + 1, Style::MARKER));
        at = line.len() - rest.len();
    }
    let rest = &line[at..];
    let key_end = rest
        .find(": ")
        .or_else(|| rest.strip_suffix(':').map(str::len))
        .filter(|&end| end > 0 && !rest.starts_with(['"', '\'', '[', '{']));
    match key_end {
        Some(end) => {
            out.push((at..at + end, Style::of(Token::SyntaxKey)));
            out.push((at + end..at + end + 1, Style::MARKER));
            value_spans(line, at + end + 1, &mut out);
        }
        None => value_spans(line, at, &mut out),
    }
    out
}

/// The spans of one line of TOML front matter.
fn toml_line(line: &str) -> Vec<(Range<usize>, Style)> {
    let mut out = Vec::new();
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if trimmed.starts_with('#') {
        out.push((indent..line.len(), Style::COMMENT));
    } else if trimmed.starts_with('[') {
        out.push((
            indent..indent + trimmed.trim_end().len(),
            Style::of(Token::SyntaxNumber),
        ));
    } else if let Some(eq) = line.find('=') {
        let key = line[..eq].trim();
        if !key.is_empty() {
            out.push((indent..indent + key.len(), Style::of(Token::SyntaxKey)));
        }
        out.push((eq..eq + 1, Style::MARKER));
        value_spans(line, eq + 1, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_syntax::ParseOptions;

    fn styles(text: &str, options: ParseOptions) -> Vec<LineStyle> {
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, options);
        let mut links = LinkStatuses::new(None, None, ProfileId::CommonMark);
        let mut code = FenceCode::default();
        highlight(
            &tree,
            &snapshot,
            0..snapshot.line_count(),
            &mut links,
            &mut code,
        )
        .into_iter()
        .map(|(_, s)| s)
        .collect()
    }

    /// The style of the first occurrence of `needle` on line `line`.
    fn at(lines: &[LineStyle], text: &str, line: usize, needle: &str) -> Style {
        let content = text.lines().nth(line).unwrap();
        let col = content
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} on line {line}"));
        lines[line].style_at(col)
    }

    #[test]
    fn markdown_roles() {
        let text = "# Title **x**\n\nSome **strong** and `code` and [a](b.md) <https://x.org>.\n\n> [!NOTE]\n> body\n\n```sh\nglyphvane notes/todo.md:12 --wait\n```\n<!-- note -->\n";
        let lines = styles(text, ParseOptions::default());
        let title = at(&lines, text, 0, "Title");
        assert!(title.bold && title.color == Token::TextPrimary);
        assert_eq!(at(&lines, text, 0, "#"), Style::MARKER);
        assert_eq!(at(&lines, text, 2, "**"), Style::MARKER);
        assert!(at(&lines, text, 2, "strong").bold);
        assert_eq!(at(&lines, text, 2, "`"), Style::MARKER);
        assert_eq!(at(&lines, text, 2, "code").color, Token::SyntaxCode);
        assert_eq!(at(&lines, text, 2, "b.md").color, Token::AccentDefault);
        assert_eq!(at(&lines, text, 2, "https").color, Token::AccentDefault);
        assert_eq!(at(&lines, text, 4, ">"), Style::MARKER);
        assert_eq!(at(&lines, text, 4, "[!NOTE]").color, Token::SyntaxTag);
        assert_eq!(at(&lines, text, 7, "sh").color, Token::SyntaxTag);
        assert_eq!(at(&lines, text, 7, "```"), Style::MARKER);
        assert!(lines[7].code && lines[8].code && lines[9].code);
        assert!(!lines[10].code);
        // Not highlighted yet: the code is plain until its job lands.
        assert_eq!(at(&lines, text, 8, "glyphvane"), Style::PLAIN);
        assert_eq!(at(&lines, text, 10, "note"), Style::COMMENT);
    }

    #[test]
    fn front_matter_keys_and_values() {
        let text = "---\ntitle: Notes\ncount: 3\ntags: [a, b]\n---\n# H\n";
        let options = ParseOptions::builtin(gv_profile::ProfileId::GfmFile);
        let lines = styles(text, options);
        assert_eq!(at(&lines, text, 0, "---"), Style::MARKER);
        assert_eq!(at(&lines, text, 1, "title").color, Token::SyntaxKey);
        assert_eq!(at(&lines, text, 1, ":"), Style::MARKER);
        assert_eq!(at(&lines, text, 1, "Notes").color, Token::SyntaxString);
        assert_eq!(at(&lines, text, 2, "3").color, Token::SyntaxNumber);
        assert_eq!(at(&lines, text, 3, "[").color, Token::TextTertiary);
        assert_eq!(at(&lines, text, 3, "a,").color, Token::SyntaxString);
        assert!(at(&lines, text, 5, "H").bold);
    }

    #[test]
    fn tables_in_paragraphs() {
        let text = "| Key | Action |\n| --- | :-: |\n| `a` | b \\| c |\n";
        let lines = styles(text, ParseOptions::default());
        assert_eq!(at(&lines, text, 0, "|"), Style::MARKER);
        assert!(at(&lines, text, 0, "Key").bold);
        assert_eq!(lines[1].runs, vec![(0..13, Style::MARKER)]);
        assert_eq!(at(&lines, text, 2, "a").color, Token::SyntaxCode);
        // The escaped pipe stays text.
        assert_eq!(at(&lines, text, 2, "\\|").color, Token::TextTertiary);
        assert_eq!(
            lines[2].style_at(text.lines().nth(2).unwrap().find("\\|").unwrap() + 1),
            Style::PLAIN
        );
    }

    #[test]
    fn link_statuses_have_their_line_styles() {
        assert_eq!(
            link_style(LinkStatus::Exact),
            Style::of(Token::AccentDefault)
        );
        assert_eq!(
            link_style(LinkStatus::Unresolved).underline,
            Some(Underline::Dashed)
        );
        assert_eq!(
            link_style(LinkStatus::CaseFold).underline,
            Some(Underline::Dotted)
        );
        assert_eq!(
            link_style(LinkStatus::Ambiguous(2)).underline,
            Some(Underline::Wavy)
        );
        assert_eq!(link_style(LinkStatus::External).color, Token::SyntaxUrl);
        let blocked = link_style(LinkStatus::Blocked);
        assert!(blocked.strike && blocked.color == Token::TextSecondary);
    }

    #[test]
    fn inline_comments_over_several_lines() {
        let text = "Text <!-- a\ncomment --> and <b>bold</b>.\n";
        let lines = styles(text, ParseOptions::default());
        assert_eq!(at(&lines, text, 0, "<!--"), Style::COMMENT);
        assert_eq!(at(&lines, text, 1, "comment"), Style::COMMENT);
        assert_eq!(at(&lines, text, 1, "<b>"), Style::of(Token::TextSecondary));
        assert_eq!(at(&lines, text, 1, "bold"), Style::PLAIN);
    }

    #[test]
    fn callout_markers() {
        assert_eq!(callout_marker("[!NOTE] title"), Some(7));
        assert_eq!(callout_marker("[!tip]-"), Some(7));
        assert_eq!(callout_marker("[!]"), None);
        assert_eq!(callout_marker("[note]"), None);
    }

    /// Highlights `text` with `code`, and the requests it made.
    fn styles_with(text: &str, code: &mut FenceCode) -> Vec<LineStyle> {
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, ParseOptions::default());
        let mut links = LinkStatuses::new(None, None, ProfileId::CommonMark);
        highlight(&tree, &snapshot, 0..snapshot.line_count(), &mut links, code)
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    /// Runs the jobs `code` asks for, as the background executor would.
    fn run_jobs(code: &mut FenceCode) -> usize {
        let requests = code.take_requests();
        for (key, text) in &requests {
            code_highlight::highlight(*key, text);
            code.jobs.finish(key);
        }
        requests.len()
    }

    #[test]
    fn fences_take_the_highlighter_classes_once_their_job_lands() {
        let text = "Text\n\n```rust\nfn fences_take() -> u32 {\n    \"s\" // c\n}\n```\n\n```nosuchlanguage\nfn x() {}\n```\n";
        let mut code = FenceCode::default();
        let lines = styles_with(text, &mut code);
        // Pending: plain on the code fill, one job asked for (the unknown
        // language asks for none).
        assert!(lines[3].code);
        assert_eq!(at(&lines, text, 3, "fn"), Style::PLAIN);
        assert_eq!(run_jobs(&mut code), 1);
        let lines = styles_with(text, &mut code);
        assert_eq!(code.take_requests().len(), 0, "cached now");
        let keyword = at(&lines, text, 3, "fn");
        assert_eq!(keyword.color, Token::SyntaxTag);
        assert_eq!(at(&lines, text, 3, "fences_take").color, Token::SyntaxKey);
        assert_eq!(at(&lines, text, 4, "\"s\"").color, Token::SyntaxString);
        let comment = at(&lines, text, 4, "// c");
        assert!(comment.italic && comment.color == Token::TextTertiary);
        // The fence line and the language stay Markdown's.
        assert_eq!(at(&lines, text, 2, "```"), Style::MARKER);
        assert_eq!(at(&lines, text, 2, "rust").color, Token::SyntaxTag);
        // An unknown language stays plain.
        assert_eq!(at(&lines, text, 9, "fn"), Style::PLAIN);
        assert!(lines[9].code);
    }

    #[test]
    fn an_edit_in_a_fence_rehighlights_that_fence_only() {
        let one = "```python\ndef an_edit_one(): pass\n```\n\n```js\nconst an_edit = 1;\n```\n";
        let mut code = FenceCode::default();
        styles_with(one, &mut code);
        assert_eq!(run_jobs(&mut code), 2);
        styles_with(one, &mut code);
        assert_eq!(code.take_requests().len(), 0);
        // Typing in the second fence: one job, for that fence; meanwhile it
        // keeps its previous colours line by line.
        let two = one.replace("const an_edit = 1;", "const an_edit = 12;");
        let lines = styles_with(&two, &mut code);
        let requests = code.take_requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].1.contains("= 12;"));
        assert_eq!(at(&lines, &two, 5, "const").color, Token::SyntaxTag);
        assert_eq!(at(&lines, &two, 1, "def").color, Token::SyntaxTag);
    }
}

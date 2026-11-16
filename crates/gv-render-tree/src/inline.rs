// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Inline content: an [`InlineTree`] becomes a `gv-inline`
//! [`Paragraph`] with a side table of spans, links and images.
//!
//! - Text, escapes, entities and soft breaks of one style merge into one
//!   run; the anchors of [`ParagraphBuilder::text_mapped`] keep the
//!   mapping to the source exact (an escape or entity is one unit).
//! - Syntax (emphasis delimiters, brackets, destinations, titles) is a gap
//!   that belongs to no item.
//! - Soft breaks render as a space (CommonMark profile), hard breaks as
//!   breaks.
//! - Code spans keep their content as rendered (line endings as spaces,
//!   one space stripped from each end).
//! - Links get the decoration of their status and, after the link text,
//!   the status suffix (`≈` or an icon box).
//! - Images of known size are boxes, sized to fit the column; others show
//!   their alt text.
//! - Raw inline HTML is shown as text until the HTML pipeline exists.
//! - Strikethrough is a line through the text in its colour.
//! - Inline and display math are formula boxes ([`BoxRef::Math`]) with an
//!   estimated size; the view typesets them and sets the real size.
//! - A table row is one paragraph: before each cell a gap box
//!   ([`BoxRef::Cell`], 0 wide until the view knows the column widths),
//!   then the cell's inlines, and a last gap box after the last cell.

use std::ops::Range;
use std::sync::Arc;

use gv_inline::{
    Background, BoxAlign, BoxKind, InlineBox, ParagraphBuilder, RunStyle, Strikethrough,
};
use gv_model::LinkKind;
use gv_syntax::BlockId;
use gv_syntax::inline::{
    Children, InlineKind, InlineNode, InlineTree, LinkKind as SynLinkKind, TextProjection,
};

use crate::footnote::FootnoteIndex;
use crate::metrics::Metrics;
use crate::node::{
    BoxRef, FootnoteRefInfo, ImageInfo, InlineContent, InlineRole, InlineSpan, LinkInfo, MathInfo,
    Measure, Units,
};
use crate::status::{ImageSize, LinkTarget, Sources};
use crate::style::{ColorToken, Family, LinkSuffix, StyleTable, TextRole, Variant, link_decor};

/// What an inline build needs besides the tree.
pub(crate) struct InlineCx<'a> {
    pub styles: &'a StyleTable,
    pub metrics: &'a Metrics,
    pub sources: Sources<'a>,
    pub block: BlockId,
    /// The document's footnote numbers.
    pub footnotes: Option<&'a FootnoteIndex>,
    /// Soft line breaks are line breaks.
    pub soft_breaks: bool,
    /// A callout's title: bold, in this colour, after a gutter of this
    /// width for the type's icon.
    pub title: Option<(ColorToken, f32)>,
}

/// Size of a tag pill's text relative to the text around it (11.5 px in
/// 14 px body text).
const TAG_SCALE: f32 = 11.5 / 14.0;
/// A tag pill's horizontal padding, in px at zoom 1.
const TAG_PADDING_X: f32 = 8.0;

/// Size of a footnote reference's number relative to the text around it.
const FOOTNOTE_SCALE: f32 = 0.75;
/// How far a footnote reference's number is raised, in em of the text
/// around it.
const FOOTNOTE_RAISE: f32 = 0.35;

/// Builds the inline content of a block whose node starts at `base`.
pub(crate) fn build(
    tree: &InlineTree,
    base: usize,
    role: TextRole,
    cx: &InlineCx<'_>,
) -> InlineContent {
    let mut b = Builder::new(cx, role, base);
    b.para
        .source_anchor(tree.source_range().start.saturating_sub(base));
    b.title_gutter(tree.source_range().start);
    b.walk(tree.top_level());
    b.finish()
}

/// [`build`], then the backreferences of a footnote referenced `count`
/// times after the content (at `end`, absolute): `↩`, `↩²`, ….
pub(crate) fn build_with_backrefs(
    tree: &InlineTree,
    base: usize,
    end: usize,
    count: u32,
    cx: &InlineCx<'_>,
) -> InlineContent {
    let mut b = Builder::new(cx, TextRole::Body, base);
    b.para
        .source_anchor(tree.source_range().start.saturating_sub(base));
    b.walk(tree.top_level());
    b.backrefs(end, count);
    b.finish()
}

/// Inline content of only the backreferences of a footnote referenced
/// `count` times, at `at` (absolute; the node starts there).
pub(crate) fn build_backrefs_only(at: usize, count: u32, cx: &InlineCx<'_>) -> InlineContent {
    let mut b = Builder::new(cx, TextRole::Body, at);
    b.para.source_anchor(0);
    b.backrefs(at, count);
    b.finish()
}

/// Inline content of plain text (no Markdown): a `<details>` summary.
pub(crate) fn build_plain(
    text: &str,
    source: Range<usize>,
    base: usize,
    role: TextRole,
    cx: &InlineCx<'_>,
) -> InlineContent {
    let mut b = Builder::new(cx, role, base);
    b.para.source_anchor(source.start.saturating_sub(base));
    b.title_gutter(source.start);
    let state = State {
        variant: if cx.title.is_some() {
            Variant::STRONG
        } else {
            Variant::PLAIN
        },
        link: None,
        alt: None,
        strike: false,
        mark: None,
        note: false,
    };
    let key = b.key(state, InlineRole::Text);
    let exact = text.len() == source.len();
    b.piece(text, source, exact, key);
    b.finish()
}

/// Builds the content of a table row whose node starts at `base`: per
/// column a gap box and the cell's inlines (`cells[i]` is the cell's
/// document range and tree; missing cells are empty), then the last gap
/// at `end` (absolute).
pub(crate) fn build_row(
    cells: &[(Range<usize>, Option<InlineTree>)],
    columns: usize,
    end: usize,
    base: usize,
    role: TextRole,
    cx: &InlineCx<'_>,
) -> InlineContent {
    let mut b = Builder::new(cx, role, base);
    let first = cells.first().map_or(end, |c| c.0.start);
    b.para.source_anchor(first.saturating_sub(base));
    let mut at = first;
    for col in 0..columns {
        let (range, tree) = match cells.get(col) {
            Some((r, t)) => (r.clone(), t.as_ref()),
            None => (at..at, None),
        };
        at = range.end.max(at);
        b.cell_gap(col, range.start);
        if let Some(tree) = tree {
            b.walk(tree.top_level());
        }
    }
    b.cell_gap(columns, end.max(at));
    b.finish()
}

/// The style state at a point of the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    variant: Variant,
    link: Option<u32>,
    /// Inside the alt text of this image (shown in place of the image).
    alt: Option<u32>,
    /// Struck through.
    strike: bool,
    /// Highlighted (`==x==`), in this colour.
    mark: Option<ColorToken>,
    /// Inside an inline footnote (`^[…]`).
    note: bool,
}

/// What ends when a subtree ends.
enum Exit {
    /// Restore the state.
    Restore(State),
    /// A link ends: restore, then add its suffix.
    Link(State, u32),
}

/// Identifies runs that may merge: same style inputs and role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunKey {
    state: State,
    role: InlineRole,
}

/// A text run being accumulated (relative source offsets).
struct Run {
    key: RunKey,
    text: String,
    start: usize,
    end: usize,
    anchors: Vec<(usize, usize)>,
    /// Whether the last piece maps one to one.
    exact: bool,
}

struct Builder<'c, 'a> {
    cx: &'c InlineCx<'a>,
    role: TextRole,
    base: usize,
    para: ParagraphBuilder,
    spans: Vec<InlineSpan>,
    links: Vec<LinkInfo>,
    images: Vec<ImageInfo>,
    run: Option<Run>,
    /// Units of the forced lines done, and of the current one.
    lines: Vec<Units>,
    line: Units,
    extra_height: f32,
    line_height: f32,
    raw_html: bool,
    math: Vec<MathInfo>,
    footnotes: Vec<FootnoteRefInfo>,
}

impl<'c, 'a> Builder<'c, 'a> {
    fn new(cx: &'c InlineCx<'a>, role: TextRole, base: usize) -> Self {
        Builder {
            cx,
            role,
            base,
            para: ParagraphBuilder::new(cx.styles.paragraph_style(role)),
            spans: Vec::new(),
            links: Vec::new(),
            images: Vec::new(),
            run: None,
            lines: Vec::new(),
            line: Units::default(),
            extra_height: 0.0,
            line_height: cx.styles.role(role).line_height,
            raw_html: false,
            math: Vec::new(),
            footnotes: Vec::new(),
        }
    }

    fn finish(mut self) -> InlineContent {
        self.flush();
        self.lines.push(self.line);
        InlineContent {
            paragraph: Arc::new(self.para.build()),
            spans: self.spans.into_boxed_slice(),
            links: self.links.into_boxed_slice(),
            images: self.images.into_boxed_slice(),
            math: self.math.into_boxed_slice(),
            footnotes: self.footnotes.into_boxed_slice(),
            raw_html_pending: self.raw_html,
            measure: Measure {
                lines: self.lines.into_boxed_slice(),
                extra_height: self.extra_height,
            },
        }
    }
}

impl Builder<'_, '_> {
    fn rel(&self, r: Range<usize>) -> Range<usize> {
        r.start.saturating_sub(self.base)..r.end.saturating_sub(self.base)
    }

    /// Walks a sibling sequence and everything below it, iteratively (deep
    /// emphasis nesting must not overflow the stack).
    fn walk(&mut self, top: Children<'_>) {
        let mut state = State {
            variant: if self.cx.title.is_some() {
                Variant::STRONG
            } else {
                Variant::PLAIN
            },
            link: None,
            alt: None,
            strike: false,
            mark: None,
            note: false,
        };
        let mut stack: Vec<(Children<'_>, Option<Exit>)> = vec![(top, None)];
        while let Some((iter, _)) = stack.last_mut() {
            let Some(node) = iter.next() else {
                if let Some((_, exit)) = stack.pop() {
                    match exit {
                        Some(Exit::Restore(s)) => state = s,
                        Some(Exit::Link(s, link)) => {
                            state = s;
                            self.link_suffix(link, state);
                        }
                        None => {}
                    }
                }
                continue;
            };
            match node.kind() {
                InlineKind::Emphasis | InlineKind::Strong => {
                    let saved = state;
                    state.variant = state.variant.with(if node.kind() == InlineKind::Strong {
                        Variant::STRONG
                    } else {
                        Variant::ITALIC
                    });
                    stack.push((node.children(), Some(Exit::Restore(saved))));
                }
                InlineKind::Strikethrough => {
                    let saved = state;
                    state.strike = true;
                    stack.push((node.children(), Some(Exit::Restore(saved))));
                }
                InlineKind::Highlight => {
                    let saved = state;
                    state.mark = Some(mark_color(&node));
                    stack.push((node.children(), Some(Exit::Restore(saved))));
                }
                InlineKind::InlineFootnote => {
                    let saved = state;
                    state.note = true;
                    stack.push((node.children(), Some(Exit::Restore(saved))));
                }
                InlineKind::Wikilink => self.wikilink(&node, state),
                InlineKind::Embed => self.embed(&node, state),
                InlineKind::Tag => self.tag(&node, state),
                // Hidden in a view: shown in the Source view only.
                InlineKind::Comment | InlineKind::BlockId => {}
                InlineKind::InlineMath | InlineKind::DisplayMath => self.math_box(&node, state),
                InlineKind::Link if state.alt.is_none() => {
                    let saved = state;
                    let link = self.link(&node);
                    state.link = Some(link);
                    stack.push((node.children(), Some(Exit::Link(saved, link))));
                }
                InlineKind::Link => {
                    // Link text inside an image's alt text: plain alt text.
                    let saved = state;
                    stack.push((node.children(), Some(Exit::Restore(saved))));
                }
                InlineKind::Image => {
                    if state.alt.is_some() {
                        let saved = state;
                        stack.push((node.children(), Some(Exit::Restore(saved))));
                    } else if let Some(image) = self.image(&node, state) {
                        let saved = state;
                        state.alt = Some(image);
                        stack.push((node.children(), Some(Exit::Restore(saved))));
                    }
                }
                InlineKind::Autolink => self.autolink(&node, state),
                InlineKind::FootnoteRef => self.footnote_ref(&node, state),
                InlineKind::CodeSpan => self.code_span(&node, state),
                InlineKind::RawHtml => self.raw_html(&node, state),
                InlineKind::Text => {
                    let r = node.range();
                    let text = node.text();
                    if text.contains('\0') {
                        let fixed = text.replace('\0', "\u{fffd}");
                        self.piece(&fixed, r, false, self.key(state, InlineRole::Text));
                    } else {
                        let exact = text.len() == r.len();
                        self.piece(text, r, exact, self.key(state, InlineRole::Text));
                    }
                }
                InlineKind::Escape | InlineKind::Entity => {
                    let mut s = String::new();
                    node.push_text(TextProjection::TextContent, &mut s);
                    self.piece(&s, node.range(), false, self.key(state, InlineRole::Text));
                }
                InlineKind::SoftBreak if self.cx.soft_breaks => {
                    self.flush();
                    let r = self.rel(node.range());
                    self.para.hard_break(r);
                    self.spans.push(InlineSpan {
                        role: InlineRole::Break,
                        link: state.link,
                    });
                    self.lines.push(std::mem::take(&mut self.line));
                }
                InlineKind::SoftBreak => {
                    let r = node.range();
                    let exact = r.len() == 1;
                    self.piece(" ", r, exact, self.key(state, InlineRole::Text));
                }
                InlineKind::HardBreak => {
                    self.flush();
                    let r = self.rel(node.range());
                    self.para.hard_break(r);
                    self.spans.push(InlineSpan {
                        role: InlineRole::Break,
                        link: state.link,
                    });
                    self.lines.push(std::mem::take(&mut self.line));
                }
                // Syntax and the tokens of nodes handled whole.
                InlineKind::Whitespace
                | InlineKind::Marker
                | InlineKind::CodeText
                | InlineKind::HtmlText
                | InlineKind::AutolinkText
                | InlineKind::MathText
                | InlineKind::LinkDestination
                | InlineKind::LinkTitle
                | InlineKind::LinkLabel
                | InlineKind::FootnoteLabel
                | InlineKind::CommentText
                | InlineKind::TagText
                | InlineKind::WikiTarget
                | InlineKind::WikiAlias
                | InlineKind::BlockIdText => {}
            }
        }
    }

    /// A footnote reference: its number, raised (its source text when the
    /// footnote has no number).
    fn footnote_ref(&mut self, node: &InlineNode<'_>, state: State) {
        let label = node.footnote_label().unwrap_or("");
        let number = self.cx.footnotes.and_then(|f| f.number(label));
        let source = node.range();
        let Some(number) = number.filter(|_| state.alt.is_none()) else {
            let key = self.key(state, InlineRole::Text);
            self.piece(node.text(), source, false, key);
            return;
        };
        let index = u32::try_from(self.footnotes.len()).unwrap_or(u32::MAX);
        self.footnotes.push(FootnoteRefInfo {
            label: Arc::from(label),
            number,
            source: self.rel(source.clone()),
        });
        self.flush();
        let key = RunKey {
            state,
            role: InlineRole::FootnoteRef(index),
        };
        self.piece(&number.to_string(), source, false, key);
        self.flush();
    }

    /// The backreferences of a footnote after its last paragraph, at `at`
    /// (absolute): one `↩` per reference, the later ones numbered.
    fn backrefs(&mut self, at: usize, count: u32) {
        const SUPERSCRIPT: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
        let state = State {
            variant: Variant::PLAIN,
            link: None,
            alt: None,
            strike: false,
            mark: None,
            note: false,
        };
        for k in 0..count {
            let mut text = String::from("\u{a0}\u{21a9}");
            if k > 0 {
                for d in (k + 1).to_string().bytes() {
                    text.push(SUPERSCRIPT[usize::from(d - b'0')]);
                }
            }
            self.flush();
            let key = RunKey {
                state,
                role: InlineRole::FootnoteBackref(k),
            };
            self.piece(&text, at..at, false, key);
            self.flush();
        }
    }

    fn key(&self, state: State, role: InlineRole) -> RunKey {
        let role = match (role, state.alt) {
            (InlineRole::Text, Some(image)) => InlineRole::ImageAlt(image),
            _ => role,
        };
        RunKey { state, role }
    }

    /// Adds `text`, rendered from the (absolute) `source`, to the current
    /// run or a new one. `exact`: the text maps to the source byte for
    /// byte.
    fn piece(&mut self, text: &str, source: Range<usize>, exact: bool, key: RunKey) {
        if self.run.as_ref().is_some_and(|r| r.key != key) {
            self.flush();
        }
        let src = self.rel(source);
        self.count(text, key);
        let run = self.run.get_or_insert_with(|| Run {
            key,
            text: String::new(),
            start: src.start,
            end: src.start,
            anchors: Vec::new(),
            exact: true,
        });
        let (t0, s0) = (run.text.len(), run.end);
        if src.start != s0 {
            // A gap (syntax between the pieces): end the previous segment,
            // then start this piece after the gap.
            run.anchors.push((t0, s0));
            run.anchors.push((t0, src.start));
        } else if !exact || !run.exact {
            run.anchors.push((t0, s0));
        }
        run.text.push_str(text);
        run.end = src.end.max(src.start);
        run.exact = exact;
        if !exact {
            run.anchors.push((run.text.len(), run.end));
        }
    }

    /// Adds the width units of `text` in the style of `key`.
    fn count(&mut self, text: &str, key: RunKey) {
        let spec = self.cx.styles.variant(self.role, key.state.variant);
        let units = width_units(text) as f32;
        match spec.family {
            Family::Text => self.line.text += units * spec.size,
            Family::Mono => self.line.mono += units * spec.size,
        }
        self.line.px += spec.letter_spacing() * text.chars().count() as f32;
        if key.role == InlineRole::Code && self.run.as_ref().is_none_or(|r| r.key != key) {
            // The chip's padding at both ends.
            self.line.px += 10.0 * self.cx.styles.zoom();
        }
        if key.role == InlineRole::Tag && self.run.as_ref().is_none_or(|r| r.key != key) {
            // The pill's padding at both ends.
            self.line.px += 2.0 * TAG_PADDING_X * self.cx.styles.zoom();
        }
    }

    /// Ends the current run.
    fn flush(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        let style = self.style(run.key);
        self.para
            .text_mapped(&run.text, style, run.start..run.end, &run.anchors);
        self.spans.push(InlineSpan {
            role: run.key.role,
            link: run.key.state.link,
        });
    }

    /// The run style of a key.
    fn style(&self, key: RunKey) -> RunStyle {
        let styles = self.cx.styles;
        let mut variant = key.state.variant;
        if key.role == InlineRole::Code {
            variant = variant.with(Variant::MONO);
        }
        let mut style = styles.run_style(self.role, variant);
        match key.role {
            InlineRole::RawHtml => style = style.with_color(ColorToken::TextTertiary.role()),
            InlineRole::ImageAlt(_) => {
                style = style.with_color(ColorToken::TextSecondary.role());
            }
            InlineRole::LinkSuffix => {
                return style.with_color(ColorToken::TextSecondary.role());
            }
            InlineRole::FootnoteRef(_) => {
                let size = style.size;
                style = style.with_color(ColorToken::AccentDefault.role());
                style.size = size * FOOTNOTE_SCALE;
                return style.with_baseline_shift(size * FOOTNOTE_RAISE);
            }
            InlineRole::FootnoteBackref(_) => {
                return style.with_color(ColorToken::AccentDefault.role());
            }
            InlineRole::Tag => {
                let zoom = styles.zoom();
                style.size *= TAG_SCALE;
                return style
                    .with_color(ColorToken::AccentDefault.role())
                    .with_background(Background {
                        role: ColorToken::AccentTint.role(),
                        padding_x: TAG_PADDING_X * zoom,
                        padding_y: 2.0 * zoom,
                        radius: 9.0 * zoom,
                    });
            }
            _ => {}
        }
        if key.state.note {
            style = style.with_color(ColorToken::TextSecondary.role());
        }
        if let Some((title, _)) = self.cx.title
            && key.state.link.is_none()
        {
            style = style.with_color(title.role());
        }
        if let Some(mark) = key.state.mark
            && key.role != InlineRole::Code
        {
            style = style.with_background(Background {
                role: mark.role(),
                padding_x: 0.0,
                padding_y: 1.0 * styles.zoom(),
                radius: 2.0 * styles.zoom(),
            });
        }
        if let Some(link) = key.state.link
            && let Some(info) = self.links.get(link as usize)
        {
            style = styles.link_style(style, info.status);
        }
        if key.state.strike && style.strikethrough.is_none() {
            style = style.with_strikethrough(Strikethrough {
                color: style.color,
                thickness: styles.zoom(),
            });
        }
        style
    }

    /// A formula: a box of estimated size (the view typesets it).
    fn math_box(&mut self, node: &InlineNode<'_>, state: State) {
        let Some(tex) = node.math() else {
            return;
        };
        let display = node.kind() == InlineKind::DisplayMath;
        if state.alt.is_some() {
            // In an image's alt text: the TeX as alt text.
            let key = self.key(state, InlineRole::Text);
            self.piece(tex, node.range(), false, key);
            return;
        }
        let em =
            self.cx.styles.role(self.role).size * if display { MATH_DISPLAY_SCALE } else { 1.0 };
        let chars = tex.chars().filter(|c| !c.is_whitespace()).count().max(1);
        #[allow(clippy::cast_precision_loss)]
        let width = (chars as f32 * MATH_EM_PER_CHAR * em).min(self.cx.metrics.image_max_width);
        let (ascent, descent) = if display {
            (1.2 * em, 0.6 * em)
        } else {
            (0.8 * em, 0.25 * em)
        };
        let index = u32::try_from(self.math.len()).unwrap_or(u32::MAX);
        let source = node.range();
        self.math.push(MathInfo {
            tex: Arc::from(tex),
            display,
            source: self.rel(source.clone()),
        });
        let b =
            InlineBox::new(BoxKind::Math, width, ascent, descent).with_id(BoxRef::Math(index).id());
        self.push_box(b, source, InlineRole::Math(index), state.link);
    }

    /// The space for a callout title's icon, at `at` (absolute): a gap box
    /// of the gutter's width (no content: copy and find skip it).
    fn title_gutter(&mut self, at: usize) {
        if let Some((_, width)) = self.cx.title
            && width > 0.0
        {
            let b = InlineBox::new(BoxKind::Other, width, 0.0, 0.0).with_id(BoxRef::Cell(0).id());
            self.push_box(b, at..at, InlineRole::CellGap(0), None);
        }
    }

    /// The gap box before table column `col` (`col` = the column count:
    /// after the last cell), at `at` (absolute).
    fn cell_gap(&mut self, col: usize, at: usize) {
        let col = u32::try_from(col).unwrap_or(u32::MAX);
        let b = InlineBox::new(BoxKind::Other, 0.0, 0.0, 0.0).with_id(BoxRef::Cell(col).id());
        self.push_box(b, at..at, InlineRole::CellGap(col), None);
    }

    fn push_box(
        &mut self,
        b: InlineBox,
        source: Range<usize>,
        role: InlineRole,
        link: Option<u32>,
    ) {
        self.flush();
        self.line.px += b.advance();
        self.extra_height += (b.height() - self.line_height).max(0.0);
        let r = self.rel(source);
        self.para.inline_box(b, r);
        self.spans.push(InlineSpan { role, link });
    }

    /// Records a link (or autolink) and asks for its status.
    fn link(&mut self, node: &InlineNode<'_>) -> u32 {
        let Some(l) = node.link() else {
            return self.add_link(LinkKind::Inline, Arc::from(""), None, node.range());
        };
        let kind = link_kind(l.kind);
        let destination: Arc<str> = if kind == LinkKind::EmailAutolink {
            format!("mailto:{}", l.destination).into()
        } else {
            l.destination.into()
        };
        self.add_link(kind, destination, l.title.map(Arc::from), l.range)
    }

    fn add_link(
        &mut self,
        kind: LinkKind,
        destination: Arc<str>,
        title: Option<Arc<str>>,
        range: Range<usize>,
    ) -> u32 {
        let status = self.cx.sources.links.status(&LinkTarget {
            kind,
            image: false,
            destination: &destination,
            title: title.as_deref(),
            range: range.clone(),
            block: self.cx.block,
        });
        let source = self.rel(range);
        self.links.push(LinkInfo {
            kind,
            destination,
            title,
            status,
            source,
        });
        u32::try_from(self.links.len() - 1).unwrap_or(u32::MAX)
    }

    /// What trails link `link` (its status suffix), at the link's end.
    fn link_suffix(&mut self, link: u32, state: State) {
        let Some(info) = self.links.get(link as usize) else {
            return;
        };
        let at = self.base + info.source.end;
        let styles = self.cx.styles;
        let zoom = styles.zoom();
        match link_decor(info.status).suffix {
            None => {}
            Some(LinkSuffix::Approx) => {
                let key = RunKey {
                    state: State {
                        link: Some(link),
                        ..state
                    },
                    role: InlineRole::LinkSuffix,
                };
                self.flush();
                self.piece("\u{a0}\u{2248}", at..at, false, key);
                self.flush();
            }
            Some(LinkSuffix::Icon { icon, count, .. }) => {
                let size = icon.size() * zoom;
                let mut width = size;
                let mut margin = 3.0 * zoom;
                if let Some(n) = count {
                    let digits = n.max(1).ilog10() + 1;
                    let badge = styles.role(TextRole::Badge);
                    width +=
                        2.0 * zoom + digits as f32 * badge.size * self.cx.metrics.text_advance_em;
                    margin = 5.0 * zoom;
                }
                let b = InlineBox::new(BoxKind::Other, width, size, 0.0)
                    .with_id(BoxRef::LinkSuffix(link).id())
                    .with_margins(margin, 0.0)
                    .with_align(BoxAlign::Middle);
                self.push_box(b, at..at, InlineRole::LinkSuffix, Some(link));
            }
        }
    }

    /// An image: a box if its size is known (returns `None`), else its
    /// index, to render the description in its place.
    fn image(&mut self, node: &InlineNode<'_>, state: State) -> Option<u32> {
        let l = node.link()?;
        let mut alt = String::new();
        node.push_text(TextProjection::CollectText, &mut alt);
        let kind = link_kind(l.kind);
        let size = self.cx.sources.images.size(&LinkTarget {
            kind,
            image: true,
            destination: l.destination,
            title: l.title,
            range: l.range.clone(),
            block: self.cx.block,
        });
        let index = u32::try_from(self.images.len()).unwrap_or(u32::MAX);
        let source = self.rel(l.range.clone());
        self.images.push(ImageInfo {
            kind,
            destination: l.destination.into(),
            title: l.title.map(Arc::from),
            alt: alt.into(),
            size,
            source,
            link: state.link,
        });
        match size {
            ImageSize::Known { width, height }
                if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 =>
            {
                let zoom = self.cx.styles.zoom();
                let (mut w, mut h) = (width * zoom, height * zoom);
                let max = self.cx.metrics.image_max_width;
                if w > max && max > 0.0 {
                    h *= max / w;
                    w = max;
                }
                let b =
                    InlineBox::new(BoxKind::Image, w, h, 0.0).with_id(BoxRef::Image(index).id());
                self.push_box(b, l.range, InlineRole::Image(index), state.link);
                None
            }
            _ => Some(index),
        }
    }

    /// A wikilink: its display text as a link (status from the resolver),
    /// then the status suffix.
    fn wikilink(&mut self, node: &InlineNode<'_>, state: State) {
        let Some(w) = node.wikilink() else {
            return;
        };
        let display = w.display();
        let mut inner = state;
        let link = if state.alt.is_none() {
            let link = self.add_link(LinkKind::Wikilink, Arc::from(&*w.inner), None, node.range());
            inner.link = Some(link);
            Some(link)
        } else {
            None
        };
        let key = self.key(inner, InlineRole::Text);
        match (&w.alias, &w.alias_range) {
            (Some(alias), Some(r)) if !alias.trim().is_empty() => {
                self.piece(alias, r.clone(), alias.len() == r.len(), key);
            }
            _ => {
                let r = w.target_range.clone();
                let exact = display == w.target && display.len() == r.len();
                self.piece(&display, r, exact, key);
            }
        }
        if let Some(link) = link {
            self.link_suffix(link, state);
        }
    }

    /// An embed inside a paragraph: an image embed is an image (sized by
    /// `|100` or `|100x145`); another embed is a link to its target (a
    /// paragraph that is only a note embed shows the note; see the block
    /// builder).
    fn embed(&mut self, node: &InlineNode<'_>, state: State) {
        let Some(w) = node.wikilink() else {
            return;
        };
        let path = w.path().trim();
        if state.alt.is_some() || !is_image_path(path) {
            self.wikilink(node, state);
            return;
        }
        let range = node.range();
        let size = self.cx.sources.images.size(&LinkTarget {
            kind: LinkKind::Embed,
            image: true,
            destination: path,
            title: None,
            range: range.clone(),
            block: self.cx.block,
        });
        let index = u32::try_from(self.images.len()).unwrap_or(u32::MAX);
        self.images.push(ImageInfo {
            kind: LinkKind::Embed,
            destination: path.into(),
            title: None,
            alt: Arc::from(path),
            size,
            source: self.rel(range.clone()),
            link: state.link,
        });
        let wanted = w.size();
        match size {
            ImageSize::Known { width, height }
                if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 =>
            {
                let zoom = self.cx.styles.zoom();
                #[allow(clippy::cast_precision_loss)]
                let (mut w, mut h) = match wanted {
                    Some((ww, Some(hh))) => (ww as f32, hh as f32),
                    Some((ww, None)) => (ww as f32, ww as f32 * height / width),
                    None => (width, height),
                };
                w *= zoom;
                h *= zoom;
                let max = self.cx.metrics.image_max_width;
                if w > max && max > 0.0 {
                    h *= max / w;
                    w = max;
                }
                let b =
                    InlineBox::new(BoxKind::Image, w, h, 0.0).with_id(BoxRef::Image(index).id());
                self.push_box(b, range, InlineRole::Image(index), state.link);
            }
            _ => {
                let key = RunKey {
                    state,
                    role: InlineRole::ImageAlt(index),
                };
                self.piece(path, w.target_range.clone(), false, key);
            }
        }
    }

    /// A `#tag`: a pill whose click target is a [`LinkKind::Tag`] link.
    fn tag(&mut self, node: &InlineNode<'_>, state: State) {
        let range = node.range();
        let text = node.text();
        let mut inner = state;
        if state.alt.is_none() && state.link.is_none() {
            let index = u32::try_from(self.links.len()).unwrap_or(u32::MAX);
            self.links.push(LinkInfo {
                kind: LinkKind::Tag,
                destination: Arc::from(text.get(1..).unwrap_or("")),
                title: None,
                status: crate::status::LinkStatus::Exact,
                source: self.rel(range.clone()),
            });
            inner.link = Some(index);
        }
        self.flush();
        let key = RunKey {
            state: inner,
            role: InlineRole::Tag,
        };
        self.piece(text, range.clone(), text.len() == range.len(), key);
        self.flush();
    }

    fn autolink(&mut self, node: &InlineNode<'_>, state: State) {
        let mut inner = state;
        let link = if state.alt.is_none() {
            let link = self.link(node);
            inner.link = Some(link);
            Some(link)
        } else {
            None
        };
        for child in node.children() {
            if child.kind() == InlineKind::AutolinkText {
                let r = child.range();
                let text = child.text();
                let exact = text.len() == r.len();
                self.piece(text, r, exact, self.key(inner, InlineRole::Text));
            }
        }
        if let Some(link) = link {
            self.link_suffix(link, state);
        }
    }

    fn code_span(&mut self, node: &InlineNode<'_>, state: State) {
        let mut rendered = String::new();
        node.push_text(TextProjection::TextContent, &mut rendered);
        let key = RunKey {
            state,
            role: InlineRole::Code,
        };
        // Pieces: text between line endings (exact), a space per line
        // ending (not exact).
        let mut pieces: Vec<(String, Range<usize>, bool)> = Vec::new();
        for tok in node.children().filter(|c| c.kind() == InlineKind::CodeText) {
            split_line_endings(tok.text(), tok.range(), &mut pieces);
        }
        strip_code_space(&mut pieces);
        let joined: String = pieces.iter().map(|p| p.0.as_str()).collect();
        if joined != rendered || pieces.is_empty() {
            // Fall back to one unit over the whole span.
            let r = node.range();
            if !rendered.is_empty() {
                self.piece(&rendered, r, false, key);
                self.flush();
            }
            return;
        }
        if self.run.as_ref().is_some_and(|r| r.key == key) {
            // Two adjacent code spans stay two chips.
            self.flush();
        }
        for (text, r, exact) in pieces {
            self.piece(&text, r, exact, key);
        }
        self.flush();
    }

    fn raw_html(&mut self, node: &InlineNode<'_>, state: State) {
        self.raw_html = true;
        // In an image's alt text, raw HTML is part of the alt text (as in
        // the `alt` attribute).
        let role = match state.alt {
            Some(image) => InlineRole::ImageAlt(image),
            None => InlineRole::RawHtml,
        };
        let key = RunKey { state, role };
        let mut pieces: Vec<(String, Range<usize>, bool)> = Vec::new();
        for tok in node.children() {
            split_line_endings(tok.text(), tok.range(), &mut pieces);
        }
        for (text, r, exact) in pieces {
            self.piece(&text, r, exact, key);
        }
    }
}

/// Display math is this much larger than the text around it (KaTeX's
/// 1.21).
pub const MATH_DISPLAY_SCALE: f32 = 1.21;

/// Estimated formula width per character of TeX, in em (until the view
/// typesets it).
const MATH_EM_PER_CHAR: f32 = 0.45;

/// Splits a token's gathered text at line endings into exact pieces and a
/// space for each line ending. A token is either all real (its text is its
/// source) or a virtual line ending (empty source).
fn split_line_endings(
    text: &str,
    source: Range<usize>,
    out: &mut Vec<(String, Range<usize>, bool)>,
) {
    let real = text.len() == source.len();
    let at = |off: usize| {
        if real {
            source.start + off
        } else {
            source.start
        }
    };
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'\n' | b'\r') {
            let le = if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            if start < i {
                out.push((
                    text[start..i].replace('\0', "\u{fffd}"),
                    at(start)..at(i),
                    real && !text[start..i].contains('\0'),
                ));
            }
            out.push((" ".to_owned(), at(i)..at(i + le), real && le == 1));
            i += le;
            start = i;
        } else {
            i += 1;
        }
    }
    if start < bytes.len() {
        let piece = &text[start..];
        out.push((
            piece.replace('\0', "\u{fffd}"),
            at(start)..at(bytes.len()),
            real && !piece.contains('\0'),
        ));
    }
}

/// CommonMark's code span rule: one space is stripped from each end when
/// both ends are spaces and the content is not all spaces.
fn strip_code_space(pieces: &mut Vec<(String, Range<usize>, bool)>) {
    let all: String = pieces.iter().map(|p| p.0.as_str()).collect();
    let b = all.as_bytes();
    if b.len() < 2 || b[0] != b' ' || b[b.len() - 1] != b' ' || b.iter().all(|&c| c == b' ') {
        return;
    }
    if let Some(first) = pieces.first_mut() {
        if first.0.len() == 1 {
            pieces.remove(0);
        } else {
            first.0.remove(0);
            if first.2 {
                first.1.start += 1;
            }
        }
    }
    if let Some(last) = pieces.last_mut() {
        if last.0.len() == 1 {
            pieces.pop();
        } else {
            last.0.pop();
            if last.2 {
                last.1.end -= 1;
            }
        }
    }
}

/// The background of a highlight: Obsidian's colour emoji right after
/// `==` pick a colour (🔴🟠🟡🟢🔵🟣), else the default highlight.
fn mark_color(node: &InlineNode<'_>) -> ColorToken {
    let text = node.text();
    let first = text.get(2..).and_then(|t| t.chars().next());
    match first {
        Some('\u{1f534}') => ColorToken::StatusErrorTint,
        Some('\u{1f7e2}') => ColorToken::StatusSuccessTint,
        Some('\u{1f535}') => ColorToken::StatusInfoTint,
        Some('\u{1f7e3}') => ColorToken::AccentTint,
        _ => ColorToken::StatusWarningTint,
    }
}

/// Whether `path` names an image Obsidian embeds as one.
pub(crate) fn is_image_path(path: &str) -> bool {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    ["png", "jpg", "jpeg", "gif", "bmp", "svg", "webp", "avif"]
        .iter()
        .any(|e| ext.eq_ignore_ascii_case(e))
}

fn link_kind(kind: SynLinkKind) -> LinkKind {
    match kind {
        SynLinkKind::Inline => LinkKind::Inline,
        SynLinkKind::Full => LinkKind::Full,
        SynLinkKind::Collapsed => LinkKind::Collapsed,
        SynLinkKind::Shortcut => LinkKind::Shortcut,
        SynLinkKind::UriAutolink => LinkKind::UriAutolink,
        SynLinkKind::EmailAutolink => LinkKind::EmailAutolink,
    }
}

/// Width units of `text`: one per character, two for wide (East Asian
/// wide and emoji) characters, four for a tab.
pub(crate) fn width_units(text: &str) -> u32 {
    if text.is_ascii() {
        let tabs = text.bytes().filter(|&b| b == b'\t').count();
        return u32::try_from(text.len() + 3 * tabs).unwrap_or(u32::MAX);
    }
    text.chars()
        .map(|c| match c {
            '\t' => 4,
            c if is_wide(c) => 2,
            _ => 1,
        })
        .sum()
}

fn is_wide(c: char) -> bool {
    matches!(u32::from(c),
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(width_units("abc"), 3);
        assert_eq!(width_units("a\tb"), 6);
        assert_eq!(width_units("日本"), 4);
        assert_eq!(width_units("é"), 1);
    }

    #[test]
    fn line_endings_split() {
        let mut out = Vec::new();
        split_line_endings("a\r\nb", 10..14, &mut out);
        assert_eq!(
            out,
            vec![
                ("a".to_owned(), 10..11, true),
                (" ".to_owned(), 11..13, false),
                ("b".to_owned(), 13..14, true)
            ]
        );
        out.clear();
        split_line_endings("\n", 7..7, &mut out);
        assert_eq!(out, vec![(" ".to_owned(), 7..7, false)]);
    }

    #[test]
    fn code_space_strip() {
        let mut p = vec![(" a ".to_owned(), 0..3, true)];
        strip_code_space(&mut p);
        assert_eq!(p, vec![("a".to_owned(), 1..2, true)]);
        let mut p = vec![("  ".to_owned(), 0..2, true)];
        strip_code_space(&mut p);
        assert_eq!(p, vec![("  ".to_owned(), 0..2, true)]);
    }
}

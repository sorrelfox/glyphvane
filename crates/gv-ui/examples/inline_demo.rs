// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Inline layout demo: paragraphs of styled text and atomic boxes in the
//! preview column, laid out by `gv-inline` and painted by `gv-ui`.
//!
//! ```text
//! cargo run -p gv-ui --example inline_demo -- [options]
//!
//!   --theme light|semi-light|dark   starting theme (default light)
//!   --width PX --height PX          window size (default 900 × 1040)
//!   --column PX                     text column width (default 680)
//!   --select A-B                    preset selection, source offsets
//!   --no-selection                  start without a selection
//!   --paragraphs N                  repeat the body paragraphs to N blocks
//!   --frames N                      draw N frames, print timings, quit
//!   --no-cull                       paint lines outside the window too
//! ```
//!
//! Keys: `t` cycles Light → Semi-light → Dark, `Escape` clears the
//! selection. Drag with the mouse to select across paragraphs.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::assets::IconName;
use gpui::{
    App, AppContext as _, Bounds, Context, FocusHandle, Focusable, ImageSource,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Point, Render, Resource, SharedString,
    Styled as _, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, px, size,
};
use gv_inline::{
    Background, Bias, BoxAlign, BoxKind, InlineBox, LineHeight, Paragraph, ParagraphBuilder,
    ParagraphStyle, RunStyle, Strikethrough, StyleId, Underline, UnderlineStyle,
};
use gv_ui::icons::FILE_PLUS_DASHED;
use gv_ui::inline::{
    BoxContent, FontFamilies, FontTable, InlineBlock, InlineParagraph, InlineText, Palette, Token,
    font_of,
};
use gv_ui::metrics::{HAIRLINE, icon, radius, size::document, spacing, text};
use gv_ui::theme::ThemeId;

// ----- styles ----------------------------------------------------------------

const BODY: StyleId = StyleId(0);
const ITALIC: StyleId = StyleId(1);
const BOLD: StyleId = StyleId(2);
const BOLD_ITALIC: StyleId = StyleId(3);
const SEMIBOLD: StyleId = StyleId(4);
const MONO: StyleId = StyleId(5);

/// Sub- and superscripts: a smaller run on a shifted baseline, as browsers
/// draw `<sub>` and `<sup>` (`font-size: smaller`).
const SCRIPT_SCALE: f32 = 0.75;

fn fonts(f: &FontFamilies) -> FontTable {
    FontTable::new()
        .with(BODY, font_of(f.text.clone(), text::weight::REGULAR, false))
        .with(ITALIC, font_of(f.text.clone(), text::weight::REGULAR, true))
        .with(BOLD, font_of(f.text.clone(), text::weight::BOLD, false))
        .with(
            BOLD_ITALIC,
            font_of(f.text.clone(), text::weight::BOLD, true),
        )
        .with(
            SEMIBOLD,
            font_of(f.text.clone(), text::weight::STRONG, false),
        )
        .with(MONO, font_of(f.mono.clone(), text::weight::REGULAR, false))
}

fn run(style: StyleId) -> RunStyle {
    RunStyle::new(style, text::PREVIEW_BODY.size)
}

fn body() -> RunStyle {
    run(BODY)
}

fn secondary() -> RunStyle {
    body().with_color(Token::TextSecondary.role())
}

/// SPEC: inline code is mono 13, padding 1 5, radius 4, `surface.sunken`.
fn code() -> RunStyle {
    RunStyle::new(MONO, text::INLINE_CODE_SIZE).with_background(Background {
        role: Token::SurfaceSunken.role(),
        padding_x: spacing::inline::CODE_PAD_X,
        padding_y: spacing::inline::CODE_PAD_Y,
        radius: radius::CHIP,
    })
}

fn underline(style: UnderlineStyle, color: Token) -> Underline {
    Underline::new(
        style,
        color.role(),
        spacing::inline::UNDERLINE_OFFSET,
        HAIRLINE,
    )
}

/// SPEC § Link status, Exact (and Pending, Case only, Loose, Ambiguous,
/// Outside root, External): `accent.default`, solid 1 px `accent.line`
/// underline, offset 3.
fn link() -> RunStyle {
    body()
        .with_color(Token::AccentDefault.role())
        .with_underline(underline(UnderlineStyle::Solid, Token::AccentLine))
}

/// Unresolved: a dashed underline in the link colour.
fn unresolved() -> RunStyle {
    body()
        .with_color(Token::AccentDefault.role())
        .with_underline(underline(UnderlineStyle::Dashed, Token::AccentDefault))
}

/// Blocked: `text.secondary`, struck through.
fn blocked() -> RunStyle {
    secondary().with_strikethrough(Strikethrough {
        color: Token::TextSecondary.role(),
        thickness: HAIRLINE,
    })
}

/// The editor's link styles, shown here for their underlines: dotted (case
/// only, loose, outside root) and wavy in `status.warning` (ambiguous).
fn dotted() -> RunStyle {
    body()
        .with_color(Token::AccentDefault.role())
        .with_underline(underline(UnderlineStyle::Dotted, Token::AccentDefault))
}

fn wavy() -> RunStyle {
    body()
        .with_color(Token::AccentDefault.role())
        .with_underline(underline(UnderlineStyle::Wavy, Token::StatusWarning))
}

fn script(raise: f32) -> RunStyle {
    let size = text::PREVIEW_BODY.size;
    RunStyle::new(BODY, size * SCRIPT_SCALE).with_baseline_shift(raise * size)
}

fn body_paragraph() -> ParagraphStyle {
    ParagraphStyle::new(
        BODY,
        text::PREVIEW_BODY.size,
        LineHeight::Px(text::PREVIEW_BODY.line),
    )
}

// ----- the document ------------------------------------------------------------

/// A block of the demo: its paragraph, the content of its boxes, and the
/// space after it.
struct Built {
    paragraph: Paragraph,
    boxes: Vec<(u32, BoxContent)>,
    gap: f32,
}

/// Builds paragraphs with document-wide source offsets: one selection range
/// spans them all.
struct DocBuilder {
    at: usize,
    blocks: Vec<Built>,
    image: Option<Arc<Path>>,
}

struct Para<'a> {
    doc: &'a mut DocBuilder,
    b: ParagraphBuilder,
    boxes: Vec<(u32, BoxContent)>,
    next_id: u32,
}

impl DocBuilder {
    fn new(image: Option<Arc<Path>>) -> Self {
        Self {
            at: 0,
            blocks: Vec::new(),
            image,
        }
    }

    fn para(&mut self, style: ParagraphStyle) -> Para<'_> {
        let mut b = ParagraphBuilder::new(style);
        b.source_anchor(self.at);
        Para {
            doc: self,
            b,
            boxes: Vec::new(),
            next_id: 1,
        }
    }
}

impl Para<'_> {
    fn t(mut self, text: &str, style: RunStyle) -> Self {
        let at = self.doc.at;
        self.b.text(text, style, at..at + text.len());
        self.doc.at += text.len();
        self
    }

    /// A box standing for `source_len` bytes of source.
    fn boxed(
        mut self,
        inline_box: InlineBox,
        source_len: usize,
        content: Option<BoxContent>,
    ) -> Self {
        let id = self.next_id;
        self.next_id += 1;
        let at = self.doc.at;
        self.b
            .inline_box(inline_box.with_id(id), at..at + source_len);
        self.doc.at += source_len;
        if let Some(content) = content {
            self.boxes.push((id, content));
        }
        self
    }

    fn math(self, width: f32, ascent: f32, descent: f32, source: &str) -> Self {
        self.boxed(
            InlineBox::new(BoxKind::Math, width, ascent, descent).with_margins(1.0, 1.0),
            source.len(),
            None,
        )
    }

    /// An image whose bottom sits `descent` below the baseline.
    fn image(self, width: f32, height: f32, descent: f32) -> Self {
        let content = self
            .doc
            .image
            .clone()
            .map(|p| BoxContent::Image(ImageSource::Resource(Resource::Path(p))));
        self.boxed(
            InlineBox::new(BoxKind::Image, width, height - descent, descent),
            "![](icon.png)".len(),
            content,
        )
    }

    /// SPEC: 14 px checkbox, 9 px gap, centred on the x-height.
    fn checkbox(self, checked: bool) -> Self {
        let side = gv_ui::metrics::size::CHECKBOX;
        self.boxed(
            InlineBox::new(BoxKind::Checkbox { checked }, side, side / 2.0, side / 2.0)
                .with_align(BoxAlign::Middle)
                .with_margins(0.0, spacing::inline::CHECKBOX_GAP),
            "- [x] ".len(),
            None,
        )
    }

    /// A link status icon (SPEC § Link status) after a link: `size` px,
    /// `gap` before it, its bottom `drop` px below the baseline.
    fn status_icon(self, path: SharedString, color: Token, size: f32, gap: f32, drop: f32) -> Self {
        self.boxed(
            InlineBox::new(BoxKind::Other, size, size - drop, drop).with_margins(gap, 0.0),
            0,
            Some(BoxContent::Icon {
                path,
                color: color.role(),
            }),
        )
    }

    /// A fixed space (a box that draws nothing).
    fn space(self, width: f32) -> Self {
        self.boxed(
            InlineBox::new(BoxKind::Other, 0.0, 0.0, 0.0).with_margins(width, 0.0),
            0,
            Some(BoxContent::Blank),
        )
    }

    /// The `≈` of a case-only or loose match: `text.secondary`, weight 600,
    /// 4 px after the link.
    fn approx(self) -> Self {
        self.space(spacing::inline::STATUS_MARKER_GAP)
            .t("≈", run(SEMIBOLD).with_color(Token::TextSecondary.role()))
    }

    fn hard_break(mut self) -> Self {
        let at = self.doc.at;
        self.b.hard_break(at..at + 2);
        self.doc.at += 2;
        self
    }

    fn done(self, gap: f32) -> usize {
        let Para { doc, b, boxes, .. } = self;
        doc.blocks.push(Built {
            paragraph: b.build(),
            boxes,
            gap,
        });
        // A blank line between blocks in the source.
        doc.at += 2;
        doc.blocks.len() - 1
    }
}

fn heading(doc: &mut DocBuilder, words: &str, h1: bool) {
    let (scale, style, tracking) = if h1 {
        (text::PREVIEW_H1, BOLD, text::H1_TRACKING)
    } else {
        (text::PREVIEW_H2, SEMIBOLD, text::H2_TRACKING)
    };
    // SPEC: H1 26/32 weight 700, tracking -0.02em; H2 18/24 weight 600,
    // tracking -0.01em. GPUI has no letter spacing; the layout applies it.
    let run = RunStyle::new(style, scale.size).with_letter_spacing(tracking * scale.size);
    doc.para(ParagraphStyle::new(
        style,
        scale.size,
        LineHeight::Px(scale.line),
    ))
    .t(words, run)
    .done(spacing::document::HEADING_GAP);
}

/// The body paragraphs. Returns `(index of the inline-code paragraph, index
/// of the image paragraph)` for the preset selection.
fn body_paragraphs(doc: &mut DocBuilder) -> (usize, usize) {
    let gap = spacing::document::PARAGRAPH_GAP;
    let intro = doc
        .para(body_paragraph())
        .t(
            "Glyphvane lays out inline content on one shared baseline: regular text, ",
            body(),
        )
        .t("italic", run(ITALIC))
        .t(", ", body())
        .t("bold", run(BOLD))
        .t(", ", body())
        .t("both", run(BOLD_ITALIC))
        .t(", ", body())
        .t("inline_code()", code())
        .t(" in a sunken chip, and a ", body())
        .t("resolved link", link())
        .t(" with a solid underline. Inline math ", body())
        .math(70.0, 15.0, 4.0, "$\\sqrt{a^2+b^2}$")
        .t(
            " sits on the baseline like any word; runs can shift it, as in H",
            body(),
        )
        .t("2", script(-0.2))
        .t("O and x", body())
        .t("2", script(0.4))
        .t(".", body())
        .done(gap);
    doc.para(body_paragraph())
        .checkbox(true)
        .t(
            "Ship the inline layout: the checkbox and this text share a baseline.",
            secondary(),
        )
        .hard_break()
        .checkbox(false)
        .t(
            "A task after a hard line break, with a tall fraction ",
            body(),
        )
        .math(58.0, 22.0, 12.0, "$\\frac{n(n+1)}{2}$")
        .t(" that makes its line taller than 24 px.", body())
        .done(gap);
    let images = doc
        .para(body_paragraph())
        .t("An inline image ", body())
        .image(32.0, 32.0, 0.0)
        .t(
            " sits on the baseline and grows its line; a small one ",
            body(),
        )
        .image(16.0, 16.0, 3.0)
        .t(" does not. Editor underlines: ", body())
        .t("dotted", dotted())
        .t(" and ", body())
        .t("wavy", wavy())
        .t(".", body())
        .done(gap);
    heading(doc, "Link status", false);
    let warning_icon = IconName::TriangleAlert.path();
    doc.para(body_paragraph())
        .t("Exact ", body())
        .t("Flavor profiles", link())
        .t(", case only ", body())
        .t("setup", link())
        .approx()
        .t(", loose ", body())
        .t("export", link())
        .approx()
        .t(", ambiguous ", body())
        .t("Shortcuts", link())
        .status_icon(
            warning_icon,
            Token::StatusWarning,
            icon::INLINE,
            spacing::inline::STATUS_ICON_GAP,
            1.0,
        )
        .space(spacing::inline::STATUS_COUNT_GAP)
        .t(
            "2",
            RunStyle::new(SEMIBOLD, text::KEYCAP_SIZE).with_color(Token::StatusWarning.role()),
        )
        .t(", pending ", body())
        .t("Tags", link())
        .t(", unresolved ", body())
        .t("Release checklist", unresolved())
        .status_icon(
            FILE_PLUS_DASHED.into(),
            Token::TextSecondary,
            icon::INLINE,
            spacing::inline::STATUS_ICON_GAP,
            2.0,
        )
        .t(", outside the root ", body())
        .t("shared notes", link())
        .status_icon(
            IconName::LogOut.path(),
            Token::TextSecondary,
            icon::INLINE,
            spacing::inline::STATUS_ICON_GAP,
            2.0,
        )
        .t(", external ", body())
        .t("CommonMark spec", link())
        .status_icon(
            IconName::ArrowUpRight.path(),
            Token::AccentDefault,
            icon::SMALL,
            spacing::inline::STATUS_MARKER_GAP,
            1.0,
        )
        .t(" and blocked ", body())
        .t("\\\\server\\share\\x.md", blocked())
        .status_icon(
            IconName::Lock.path(),
            Token::TextSecondary,
            icon::SMALL,
            spacing::inline::STATUS_ICON_GAP,
            1.0,
        )
        .t(".", body())
        .done(gap);
    doc.para(body_paragraph())
        .t("日本語の文章は単語の間にスペースを入れません。行は漢字と仮名の間で折り返されますが、句読点「。」や「、」は行頭に来ません。中文也一样：每个汉字之间都可以换行，", body())
        .t("粗体也可以", run(BOLD))
        .t("，mixed with English words and ", body())
        .t("コード", code())
        .t(" too.", body())
        .done(gap);
    doc.para(body_paragraph())
        .t("Emoji clusters stay whole when lines wrap: family 👩\u{200D}👩\u{200D}👧\u{200D}👦, thumbs up 👍🏽, rainbow flag 🏳\u{FE0F}\u{200D}🌈, keycap 1\u{FE0F}\u{20E3}, and 🎉🚀✨ in a row; ", body())
        .t("bold 🎉 too", run(BOLD))
        .t(".", body())
        .done(gap);
    doc.para(body_paragraph())
        .t("A long URL ", body())
        .t("https://example.com/a/very/long/path/without/any/spaces/that/cannot/fit/on/one/line.html", link())
        .t(" and the word ", body())
        .t("Pneumonoultramicroscopicsilicovolcanoconiosis", run(ITALIC))
        .t(" break anywhere only when nothing else fits.", body())
        .done(gap);
    doc.para(body_paragraph())
        .t("Code spans wrap like text: ", body())
        .t("let layout = paragraph.layout(width);", code())
        .t(" keeps its padding at both ends, even across a line break. A box glued to punctuation, ", body())
        .math(52.0, 13.0, 1.0, "$E=mc^2$")
        .t(", never starts a line with the comma; nor does (", body())
        .math(64.0, 19.0, 9.0, "$\\sum_{i=1}^n x_i$")
        .t(") lose its brackets.", body())
        .done(gap);
    (intro, images)
}

// ----- the view ------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Opts {
    theme: ThemeId,
    width: f32,
    height: f32,
    column: f32,
    select: Option<(usize, usize)>,
    no_selection: bool,
    paragraphs: Option<usize>,
    frames: Option<usize>,
    cull: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            theme: ThemeId::Light,
            width: 900.0,
            height: 1040.0,
            column: document::COLUMN_MAX,
            select: None,
            no_selection: false,
            paragraphs: None,
            frames: None,
            cull: true,
        }
    }
}

/// Frame timings: from `render` to the end of the last paint (frame CPU),
/// and from just before the first paragraph paints to just after the last
/// (paint).
#[derive(Debug, Default)]
struct Bench {
    frame_start: Option<Instant>,
    paint_start: Option<Instant>,
    frames: Vec<Duration>,
    paints: Vec<Duration>,
    lines: u64,
    glyphs: u64,
}

struct Demo {
    blocks: Vec<Rc<RefCell<InlineBlock>>>,
    gaps: Vec<f32>,
    text: Rc<RefCell<InlineText>>,
    /// The family of the status line.
    ui_font: SharedString,
    theme: ThemeId,
    /// Anchor and head of the selection, in source offsets.
    selection: Option<(usize, usize)>,
    dragging: bool,
    focus: FocusHandle,
    opts: Opts,
    bench: Rc<RefCell<Bench>>,
}

impl Demo {
    fn new(opts: Opts, cx: &mut Context<'_, Self>) -> Self {
        let families = FontFamilies::from_app(cx);
        let image: Option<Arc<Path>> = Some(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/icons/48x48.png")
                .into(),
        )
        .filter(|p: &Arc<Path>| p.exists());
        let mut doc = DocBuilder::new(image);
        heading(&mut doc, "Inline formatting context", true);
        let (intro, images) = body_paragraphs(&mut doc);
        let preset = {
            let source = |ix: usize, item: usize| doc.blocks[ix].paragraph.items()[item].source();
            // From inside `inline_code()` into the image paragraph.
            (source(intro, 7).start + 7, source(images, 2).start + 21)
        };
        if let Some(n) = opts.paragraphs {
            // Repeat the body paragraphs (not the title) up to n blocks.
            while doc.blocks.len() < n {
                body_paragraphs(&mut doc);
            }
            doc.blocks.truncate(n.max(1));
        }
        let selection = match (opts.no_selection, opts.select) {
            (true, _) => None,
            (false, Some(sel)) => Some(sel),
            (false, None) => Some(preset),
        };
        let gaps = doc.blocks.iter().map(|b| b.gap).collect();
        let blocks = doc
            .blocks
            .into_iter()
            .map(|built| {
                let mut block = InlineBlock::new(Arc::new(built.paragraph));
                for (id, content) in built.boxes {
                    block.set_box_content(id, content);
                }
                Rc::new(RefCell::new(block))
            })
            .collect();
        Self {
            blocks,
            gaps,
            text: Rc::new(RefCell::new(InlineText::new(fonts(&families)))),
            ui_font: families.text.clone(),
            theme: opts.theme,
            selection,
            dragging: false,
            focus: cx.focus_handle(),
            opts,
            bench: Rc::default(),
        }
    }

    /// The source offset under a window position: the block whose top is
    /// above the position (the first block when none is), and in it the
    /// nearest caret stop; below a block's last line, its end.
    fn hit(&self, position: Point<gpui::Pixels>) -> Option<usize> {
        let mut found = None;
        for block in &self.blocks {
            let b = block.borrow();
            let Some(bounds) = b.bounds() else {
                continue;
            };
            if found.is_none() || bounds.top() <= position.y {
                found = Some(if position.y >= bounds.bottom() {
                    b.paragraph().source_span().end
                } else {
                    b.hit(position)?.source
                });
            }
        }
        found
    }

    fn status(&self) -> String {
        let text = self.text.borrow();
        let runs = text.run_stats();
        let (count, bytes) = text.run_cache_size();
        let selection = match self.selection {
            Some((a, b)) => format!("selection {}–{}", a.min(b), a.max(b)),
            None => "no selection".to_owned(),
        };
        format!(
            "{} · {selection} · {} blocks · shaped runs {count} ({} KB, {} hits, {} misses) · t: theme, Esc: clear",
            self.theme.theme().name,
            self.blocks.len(),
            bytes / 1024,
            runs.hits,
            runs.misses,
        )
    }
}

impl Focusable for Demo {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.bench.borrow_mut().frame_start = Some(Instant::now());
        let theme = self.theme.theme();
        let palette = Palette::of(self.theme);
        let selection = self
            .selection
            .map(|(a, b)| a.min(b)..a.max(b))
            .filter(|r| !r.is_empty());
        let caret = self
            .selection
            .filter(|_| !self.dragging)
            .map(|(_, head)| head);
        let paragraphs = self.blocks.iter().zip(&self.gaps).map(|(block, gap)| {
            let mut p = InlineParagraph::new(block.clone(), self.text.clone(), palette.clone())
                .cull(self.opts.cull);
            if let Some(s) = &selection {
                p = p.selection(s.clone());
            }
            if let Some(head) = caret {
                p = p.caret(head, Bias::Left);
            }
            div().w_full().mb(px(*gap)).child(p)
        });
        let start = {
            let bench = self.bench.clone();
            canvas(
                |_, _, _| (),
                move |_, _, _, _| {
                    bench.borrow_mut().paint_start = Some(Instant::now());
                },
            )
            .absolute()
            .size(px(0.0))
        };
        let end = {
            let bench = self.bench.clone();
            let blocks = self.blocks.clone();
            let frames = self.opts.frames;
            canvas(
                |_, _, _| (),
                move |_, _, window, cx| {
                    after_paint(&bench, &blocks, frames, window, cx);
                },
            )
            .absolute()
            .size(px(0.0))
        };

        div()
            .id("demo")
            .key_context("InlineDemo")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .font_family(self.ui_font.clone())
            .cursor_text()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "t" => {
                        let ix = ThemeId::ALL
                            .iter()
                            .position(|t| *t == this.theme)
                            .unwrap_or(0);
                        this.theme = ThemeId::ALL[(ix + 1) % ThemeId::ALL.len()];
                    }
                    "escape" => this.selection = None,
                    _ => return,
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    if let Some(at) = this.hit(ev.position) {
                        let anchor = match (ev.modifiers.shift, this.selection) {
                            (true, Some((anchor, _))) => anchor,
                            _ => at,
                        };
                        this.selection = Some((anchor, at));
                        this.dragging = true;
                    }
                    window.focus(&this.focus, cx);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if !this.dragging || ev.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                if let (Some(at), Some((anchor, _))) = (this.hit(ev.position), this.selection) {
                    this.selection = Some((anchor, at));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.dragging = false;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(self.opts.column + 2.0 * document::PAD_X))
                            .px(px(document::PAD_X))
                            .pt(px(document::PAD_TOP))
                            .flex()
                            .flex_col()
                            .child(start)
                            .children(paragraphs)
                            .child(end),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(gv_ui::metrics::size::STATUS_BAR))
                    .px(px(spacing::status_bar::PAD_X))
                    .flex()
                    .items_center()
                    .bg(theme.surface.window)
                    .border_t_1()
                    .border_color(theme.border.default)
                    .text_size(px(text::UI_SMALL.size))
                    .line_height(px(text::UI_SMALL.line))
                    .text_color(theme.text.secondary)
                    .child(self.status()),
            )
    }
}

/// Runs after the last paragraph painted: records the frame and, in
/// benchmark mode, asks for the next frame or reports and quits.
fn after_paint(
    bench: &Rc<RefCell<Bench>>,
    blocks: &[Rc<RefCell<InlineBlock>>],
    frames: Option<usize>,
    window: &mut Window,
    cx: &mut App,
) {
    let now = Instant::now();
    let count = {
        let mut b = bench.borrow_mut();
        if let (Some(start), Some(paint)) = (b.frame_start.take(), b.paint_start.take()) {
            b.frames.push(now - start);
            b.paints.push(now - paint);
        }
        for block in blocks {
            let stats = block.borrow().paint_stats();
            b.lines += u64::from(stats.lines);
            b.glyphs += u64::from(stats.glyphs);
        }
        b.frames.len()
    };
    let Some(total) = frames else {
        return;
    };
    if count < total {
        window.request_animation_frame();
    } else if count == total {
        report(&bench.borrow(), blocks.len());
        cx.defer(|cx| cx.quit());
    }
}

fn report(bench: &Bench, blocks: usize) {
    // The first frames create the window and shape every run: skipped.
    let skip = 10.min(bench.frames.len());
    let stats = |name: &str, v: &[Duration]| {
        let mut ms: Vec<f64> = v[skip..].iter().map(|d| d.as_secs_f64() * 1e3).collect();
        ms.sort_by(f64::total_cmp);
        if ms.is_empty() {
            return;
        }
        let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
        println!(
            "{name}: median {:.3} ms, p95 {:.3} ms, max {:.3} ms (n {})",
            at(0.5),
            at(0.95),
            at(1.0),
            ms.len()
        );
    };
    let n = bench.frames.len().max(1) as u64;
    println!(
        "{blocks} blocks, {} frames; per frame {} lines and {} glyphs painted",
        bench.frames.len(),
        bench.lines / n,
        bench.glyphs / n
    );
    stats("frame CPU (render → last paint)", &bench.frames);
    stats("paragraph paint", &bench.paints);
    if let Some(first) = bench.frames.first() {
        println!("first frame {:.3} ms", first.as_secs_f64() * 1e3);
    }
}

// ----- main -------------------------------------------------------------------------

fn parse() -> Result<Opts, String> {
    let mut opts = Opts::default();
    let mut args = std::env::args().skip(1);
    let number = |v: Option<String>, name: &str| -> Result<f32, String> {
        v.ok_or(format!("{name} needs a value"))?
            .parse()
            .map_err(|e| format!("{name}: {e}"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--theme" => {
                opts.theme = match args.next().as_deref() {
                    Some("light") => ThemeId::Light,
                    Some("semi-light") => ThemeId::SemiLight,
                    Some("dark") => ThemeId::Dark,
                    other => return Err(format!("unknown theme {other:?}")),
                }
            }
            "--width" => opts.width = number(args.next(), "--width")?,
            "--height" => opts.height = number(args.next(), "--height")?,
            "--column" => opts.column = number(args.next(), "--column")?,
            "--select" => {
                let v = args.next().ok_or("--select needs A-B")?;
                let (a, b) = v.split_once('-').ok_or("--select needs A-B")?;
                opts.select = Some((
                    a.parse().map_err(|e| format!("--select: {e}"))?,
                    b.parse().map_err(|e| format!("--select: {e}"))?,
                ));
            }
            "--no-selection" => opts.no_selection = true,
            "--paragraphs" => opts.paragraphs = Some(number(args.next(), "--paragraphs")? as usize),
            "--frames" => opts.frames = Some(number(args.next(), "--frames")? as usize),
            "--no-cull" => opts.cull = false,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(opts)
}

fn main() {
    let opts = match parse() {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("inline_demo: {err}");
            std::process::exit(2);
        }
    };
    gpui::application()
        .with_assets(gv_ui::Assets)
        .run(move |cx| {
            let bounds = Bounds::centered(None, size(px(opts.width), px(opts.height)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Inline layout demo".into()),
                    ..TitlebarOptions::default()
                }),
                ..WindowOptions::default()
            };
            let opened = cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| Demo::new(opts.clone(), cx));
                window.focus(&view.focus_handle(cx), cx);
                view
            });
            if let Err(err) = opened {
                eprintln!("inline_demo: cannot open a window: {err:#}");
                cx.quit();
            }
            cx.on_window_closed(|cx, _| cx.quit()).detach();
        });
}

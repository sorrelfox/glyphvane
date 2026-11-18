// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Tests of the GPUI side of inline layout.
//!
//! - With the platform's real text system and no window: shaping widths,
//!   primary-font metrics, letter spacing, and the caret ⇄ hit round trip.
//! - In headless GPUI windows: what the element paints (selection, caret,
//!   decorations, boxes) with GPUI's no-op text system (0.6 em per
//!   character), and hit testing through real mouse events with the real
//!   text system.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Background as Fill, BorderStyle, Bounds, Context, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, Quad, Render, ScaledPixels, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px,
};
use gv_inline::{
    Affinity, Background, BoxKind, DecorationKind, InlineBox, LineHeight, Paragraph,
    ParagraphBuilder, ParagraphStyle, RunStyle, Strikethrough, StyleId, Underline, UnderlineStyle,
};

use super::*;
use crate::metrics::spacing::inline as spacing;
use crate::metrics::text;
use crate::theme::ThemeId;

const BODY: StyleId = StyleId(0);
const BOLD: StyleId = StyleId(1);
const MONO: StyleId = StyleId(2);

// ----- fixture ---------------------------------------------------------------

fn table(families: &FontFamilies) -> FontTable {
    FontTable::new()
        .with(BODY, font_of(families.text.clone(), 400.0, false))
        .with(BOLD, font_of(families.text.clone(), 700.0, false))
        .with(MONO, font_of(families.mono.clone(), 400.0, false))
}

fn body() -> RunStyle {
    RunStyle::new(BODY, text::PREVIEW_BODY.size)
}

fn bold() -> RunStyle {
    RunStyle::new(BOLD, text::PREVIEW_BODY.size)
}

fn code() -> RunStyle {
    RunStyle::new(MONO, text::INLINE_CODE_SIZE).with_background(Background {
        role: Token::SurfaceSunken.role(),
        padding_x: spacing::CODE_PAD_X,
        padding_y: spacing::CODE_PAD_Y,
        radius: crate::metrics::radius::CHIP,
    })
}

fn link(style: UnderlineStyle, line: Token) -> RunStyle {
    body()
        .with_color(Token::AccentDefault.role())
        .with_underline(Underline::new(
            style,
            line.role(),
            spacing::UNDERLINE_OFFSET,
            crate::metrics::HAIRLINE,
        ))
}

fn paragraph_style() -> ParagraphStyle {
    ParagraphStyle::new(
        BODY,
        text::PREVIEW_BODY.size,
        LineHeight::Px(text::PREVIEW_BODY.line),
    )
}

enum Piece {
    Text(&'static str, RunStyle),
    Box(InlineBox),
}

use Piece::{Box as B, Text as T};

/// A paragraph whose source starts at `source` and equals its text byte for
/// byte; a box takes 3 source bytes.
fn build(pieces: &[Piece], source: usize) -> Paragraph {
    let mut b = ParagraphBuilder::new(paragraph_style());
    b.source_anchor(source);
    let mut at = source;
    for p in pieces {
        match p {
            T(t, s) => {
                b.text(t, *s, at..at + t.len());
                at += t.len();
            }
            B(bx) => {
                b.inline_box(*bx, at..at + 3);
                at += 3;
            }
        }
    }
    b.build()
}

fn mixed() -> Vec<Piece> {
    vec![
        T("Glyphvane lays out ", body()),
        T("bold", bold()),
        T(" text, ", body()),
        T("inline_code()", code()),
        T(" and a formula ", body()),
        B(InlineBox::new(BoxKind::Math, 40.0, 13.0, 4.0).with_margins(1.0, 1.0)),
        T(" on one baseline; 日本語の文章、", body()),
        T("a link", link(UnderlineStyle::Solid, Token::AccentLine)),
        T(" and 👍🏽 at the end.", body()),
    ]
}

// ----- headless windows ---------------------------------------------------------

struct View {
    blocks: Vec<Rc<RefCell<InlineBlock>>>,
    text: Rc<RefCell<InlineText>>,
    palette: Arc<Palette>,
    width: f32,
    selection: Option<Range<usize>>,
    caret: Option<(usize, gv_inline::Bias)>,
    /// Source offsets of mouse-downs, hit tested against the blocks.
    clicks: Vec<usize>,
}

impl View {
    fn new(paragraphs: Vec<Paragraph>, text: InlineText, width: f32) -> Self {
        Self {
            blocks: paragraphs
                .into_iter()
                .map(|p| Rc::new(RefCell::new(InlineBlock::new(Arc::new(p)))))
                .collect(),
            text: Rc::new(RefCell::new(text)),
            palette: Palette::of(ThemeId::Light),
            width,
            selection: None,
            caret: None,
            clicks: Vec::new(),
        }
    }
}

impl Render for View {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .id("root")
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, _| {
                    for block in &this.blocks {
                        let block = block.borrow();
                        if block.bounds().is_some_and(|b| b.contains(&ev.position))
                            && let Some(hit) = block.hit(ev.position)
                        {
                            this.clicks.push(hit.source);
                        }
                    }
                }),
            )
            .child(
                div()
                    .w(px(self.width))
                    .flex()
                    .flex_col()
                    .gap(px(crate::metrics::spacing::document::PARAGRAPH_GAP))
                    .children(self.blocks.iter().map(|b| {
                        let mut p = InlineParagraph::new(
                            b.clone(),
                            self.text.clone(),
                            self.palette.clone(),
                        );
                        if let Some(s) = &self.selection {
                            p = p.selection(s.clone());
                        }
                        if let Some((offset, bias)) = self.caret {
                            p = p.caret(offset, bias);
                        }
                        p
                    })),
            )
    }
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
}

fn noop_text() -> InlineText {
    // The no-op text system lists no fonts: every family falls back.
    InlineText::new(table(&FontFamilies::detect(&[])))
}

fn quads(cx: &mut VisualTestContext) -> (Vec<Quad>, f32) {
    cx.update(|window, _| (window.painted_quads(), window.scale_factor()))
}

fn logical(b: Bounds<ScaledPixels>, scale: f32) -> (f32, f32, f32, f32) {
    (
        b.origin.x.0 / scale,
        b.origin.y.0 / scale,
        b.size.width.0 / scale,
        b.size.height.0 / scale,
    )
}

fn close(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    (a.0 - b.0).abs() < 1.01
        && (a.1 - b.1).abs() < 1.01
        && (a.2 - b.2).abs() < 1.01
        && (a.3 - b.3).abs() < 1.01
}

fn color(token: Token) -> Fill {
    Palette::of(ThemeId::Light).color(token).into()
}

#[gpui::test]
fn selection_and_caret_paint_in_their_tokens(cx: &mut TestAppContext) {
    let paragraphs = vec![
        build(&mixed(), 0),
        build(&[T("second block of text", body())], 500),
    ];
    let (view, cx) = cx.add_window_view(|_, _| View::new(paragraphs, noop_text(), 300.0));
    draw(cx);
    // From inside the code span into the second block, caret at its end.
    let start = "Glyphvane lays out bold text, inline".len();
    view.update(cx, |v, cx| {
        v.selection = Some(start..506);
        v.caret = Some((506, gv_inline::Bias::Left));
        cx.notify();
    });
    draw(cx);
    let (painted, scale) = quads(cx);
    let (first, second) = view.read_with(cx, |v, _| (v.blocks[0].clone(), v.blocks[1].clone()));
    let selection = color(Token::Selection);
    let selected: Vec<_> = painted
        .iter()
        .filter(|q| q.background == selection)
        .map(|q| logical(q.bounds, scale))
        .collect();
    // Every rectangle the layouts give is painted, in both blocks.
    let mut expected = Vec::new();
    for (block, range) in [(&first, start..506), (&second, start..506)] {
        let block = block.borrow();
        let (bounds, layout) = (
            block.bounds().expect("painted"),
            block.layout().expect("laid out"),
        );
        for r in layout.selection_rects_for_source(range) {
            expected.push((
                f32::from(bounds.origin.x) + r.x,
                f32::from(bounds.origin.y) + r.y,
                r.width,
                r.height,
            ));
        }
    }
    assert!(expected.len() >= 3, "{expected:?}");
    for e in &expected {
        assert!(
            selected.iter().any(|s| close(*s, *e)),
            "{e:?} not in {selected:?}"
        );
    }
    assert_eq!(
        first.borrow().paint_stats().selection_rects
            + second.borrow().paint_stats().selection_rects,
        u32::try_from(expected.len()).unwrap_or(0)
    );
    // The selected formula gets the translucent tint on top.
    let tint: Fill = Palette::of(ThemeId::Light)
        .color(Token::Selection)
        .opacity(crate::metrics::SELECTED_BOX_TINT_OPACITY)
        .into();
    assert_eq!(painted.iter().filter(|q| q.background == tint).count(), 1);
    // The caret: 2 px of accent.default at the end of the second block.
    let caret: Vec<_> = painted
        .iter()
        .filter(|q| q.background == color(Token::AccentDefault))
        .map(|q| logical(q.bounds, scale))
        .collect();
    let want = second
        .borrow()
        .caret_bounds(506, gv_inline::Bias::Left)
        .expect("caret");
    assert_eq!(caret.len(), 1, "{caret:?}");
    assert!((caret[0].2 - crate::metrics::size::CARET_WIDTH).abs() < 0.01);
    assert!((caret[0].0 + caret[0].2 / 2.0 - f32::from(want.origin.x)).abs() < 1.01);
    assert!(second.borrow().paint_stats().caret);
    assert!(
        !first.borrow().paint_stats().caret,
        "only the block holding the offset"
    );
    // The code chip, under the selection.
    let chips = painted
        .iter()
        .filter(|q| q.background == color(Token::SurfaceSunken) && q.border_widths.bottom.0 == 0.0)
        .count();
    assert!(chips >= 1);
    let first_chip = painted
        .iter()
        .position(|q| q.background == color(Token::SurfaceSunken));
    let first_selection = painted.iter().position(|q| q.background == selection);
    assert!(
        first_chip < first_selection,
        "chips paint before the selection"
    );
    assert!(first.borrow().paint_stats().glyphs > 50);
}

#[gpui::test]
fn every_underline_style_and_strikethrough_is_painted(cx: &mut TestAppContext) {
    let blocked = body()
        .with_color(Token::TextSecondary.role())
        .with_strikethrough(Strikethrough {
            color: Token::TextSecondary.role(),
            thickness: crate::metrics::HAIRLINE,
        });
    let pieces = [
        T("solid", link(UnderlineStyle::Solid, Token::AccentLine)),
        T(" ", body()),
        T("dashed", link(UnderlineStyle::Dashed, Token::AccentDefault)),
        T(" ", body()),
        T("dotted", link(UnderlineStyle::Dotted, Token::AccentDefault)),
        T(" ", body()),
        T("wavy", link(UnderlineStyle::Wavy, Token::StatusWarning)),
        T(" ", body()),
        T("blocked", blocked),
    ];
    let paragraph = build(&pieces, 0);
    let (view, cx) = cx.add_window_view(|_, _| View::new(vec![paragraph], noop_text(), 600.0));
    draw(cx);
    let block = view.read_with(cx, |v, _| v.blocks[0].clone());
    let layout = block.borrow().layout().cloned().expect("laid out");
    assert_eq!(layout.lines().len(), 1);
    // The layout produces one decoration per style.
    let kinds: Vec<_> = layout.decorations(0).iter().map(|d| d.kind).collect();
    for style in [
        UnderlineStyle::Solid,
        UnderlineStyle::Dashed,
        UnderlineStyle::Dotted,
        UnderlineStyle::Wavy,
    ] {
        assert_eq!(
            kinds
                .iter()
                .filter(|k| matches!(k, DecorationKind::Underline { style: s, .. } if *s == style))
                .count(),
            1,
            "{style:?} in {kinds:?}"
        );
    }
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, DecorationKind::Strikethrough { .. }))
    );
    assert_eq!(block.borrow().paint_stats().decorations, 5);

    // Solid and wavy: GPUI underlines in their token colours.
    let underlines = cx.update(|window, _| window.painted_underlines());
    let palette = Palette::of(ThemeId::Light);
    let solid = underlines
        .iter()
        .find(|u| u.color == palette.color(Token::AccentLine))
        .expect("solid underline");
    let wavy = underlines
        .iter()
        .find(|u| u.color == palette.color(Token::StatusWarning))
        .expect("wavy underline");
    assert_eq!(underlines.len(), 2);
    assert_ne!(
        format!("{:?}", solid.wavy),
        format!("{:?}", wavy.wavy),
        "one solid, one wavy"
    );
    let (painted, scale) = quads(cx);
    // Dashed (3 on, 2 off) and dotted (1 on, 1 off): one quad per piece, in
    // accent.default, on the underline's row.
    let pieces = |width: f32| {
        painted
            .iter()
            .filter(|q| {
                q.background == color(Token::AccentDefault)
                    && (q.bounds.size.width.0 / scale - width).abs() < 0.01
                    && (q.bounds.size.height.0 / scale - 1.0).abs() < 0.01
            })
            .count()
    };
    let word = 6.0 * 0.6 * text::PREVIEW_BODY.size;
    assert_eq!(pieces(3.0), (word / 5.0).ceil() as usize, "dashes");
    assert_eq!(pieces(1.0), (word / 2.0).round() as usize, "dots");
    assert!(
        painted
            .iter()
            .all(|q| q.border_style != BorderStyle::Dashed)
    );
    // Strikethrough: a hairline in text.secondary through the x-height.
    let strike: Vec<_> = painted
        .iter()
        .filter(|q| q.background == color(Token::TextSecondary))
        .collect();
    assert_eq!(strike.len(), 1);
    let line = &layout.lines()[0];
    let origin_y = f32::from(block.borrow().bounds().expect("painted").origin.y);
    let y = strike[0].bounds.origin.y.0 / scale - origin_y;
    assert!(
        y < line.baseline && y > line.baseline - 0.5 * text::PREVIEW_BODY.size,
        "{y}"
    );
}

#[gpui::test]
fn boxes_paint_as_checkboxes_placeholders_and_icons(cx: &mut TestAppContext) {
    let pieces = [
        B(
            InlineBox::new(BoxKind::Checkbox { checked: true }, 14.0, 11.0, 3.0)
                .with_margins(0.0, spacing::CHECKBOX_GAP),
        ),
        T("done ", body()),
        B(InlineBox::new(
            BoxKind::Checkbox { checked: false },
            14.0,
            11.0,
            3.0,
        )),
        T(" math ", body()),
        B(InlineBox::new(BoxKind::Math, 30.0, 12.0, 4.0)),
        T(" icon ", body()),
        B(InlineBox::new(BoxKind::Other, 13.0, 11.0, 2.0).with_id(7)),
    ];
    let paragraph = build(&pieces, 0);
    let (view, cx) = cx.add_window_view(|_, _| View::new(vec![paragraph], noop_text(), 600.0));
    view.update(cx, |v, _| {
        v.blocks[0].borrow_mut().set_box_content(
            7,
            BoxContent::Icon {
                path: "icons/triangle-alert.svg".into(),
                color: Token::StatusWarning.role(),
            },
        );
    });
    draw(cx);
    let block = view.read_with(cx, |v, _| v.blocks[0].clone());
    assert_eq!(block.borrow().paint_stats().boxes, 4);
    let (painted, scale) = quads(cx);
    let checked: Vec<_> = painted
        .iter()
        .filter(|q| q.background == color(Token::AccentDefault))
        .map(|q| logical(q.bounds, scale))
        .collect();
    assert_eq!(checked.len(), 1);
    assert!((checked[0].2 - 14.0).abs() < 0.01 && (checked[0].3 - 14.0).abs() < 0.01);
    let bordered = |fill: Token| {
        painted
            .iter()
            .filter(|q| {
                q.background == color(fill)
                    && q.border_color == Palette::of(ThemeId::Light).color(Token::BorderStrong)
                    && q.border_widths.left.0 > 0.0
            })
            .count()
    };
    assert_eq!(bordered(Token::SurfaceRaised), 1, "unchecked box");
    assert_eq!(bordered(Token::SurfaceSunken), 1, "math placeholder");
    // The checkbox sits on the line's baseline: its bottom 3 px below.
    let layout = block.borrow().layout().cloned().expect("laid out");
    let top = f32::from(block.borrow().bounds().expect("painted").origin.y);
    assert!((checked[0].1 + 14.0 - top - (layout.lines()[0].baseline + 3.0)).abs() < 1.01);
}

#[gpui::test]
fn resizing_rewraps_from_cached_shaping(cx: &mut TestAppContext) {
    let paragraph = build(&mixed(), 0);
    let (view, cx) = cx.add_window_view(|_, _| View::new(vec![paragraph], noop_text(), 600.0));
    draw(cx);
    let (block, text) = view.read_with(cx, |v, _| (v.blocks[0].clone(), v.text.clone()));
    let lines = block.borrow().layout().map(|l| l.lines().len());
    let misses = text.borrow().run_stats().misses;
    let layouts = block.borrow().cache_stats().layouts;
    // Another frame at the same width: every measurement is a cache hit.
    draw(cx);
    assert_eq!(block.borrow().cache_stats().layouts, layouts, "no relayout");
    view.update(cx, |v, cx| {
        v.width = 250.0;
        cx.notify();
    });
    draw(cx);
    let narrow = block.borrow().layout().map(|l| l.lines().len());
    assert!(narrow > lines, "{narrow:?} vs {lines:?}");
    assert_eq!(text.borrow().run_stats().misses, misses, "no reshaping");
    assert_eq!(block.borrow().cache_stats().shapings, 1);
    // Changing the fonts reshapes on the next frame.
    text.borrow_mut()
        .set_fonts(table(&FontFamilies::detect(&[])));
    draw(cx);
    assert_eq!(block.borrow().cache_stats().shapings, 2);
}

/// Tests with the platform's real text system. Linux only for now: the
/// headless platform has not been tried on macOS and Windows.
#[cfg(target_os = "linux")]
mod real {
    use std::sync::OnceLock;

    use gpui::{
        Modifiers, PlatformTextSystem, TestDispatcher, TextSystem, WindowTextSystem, point,
    };
    use unicode_segmentation::UnicodeSegmentation as _;

    use super::*;

    /// The platform's real text system, made once per test process.
    fn platform_text_system() -> Arc<dyn PlatformTextSystem> {
        static TEXT: OnceLock<Arc<dyn PlatformTextSystem>> = OnceLock::new();
        TEXT.get_or_init(|| gpui::platform::current_platform(true).text_system())
            .clone()
    }

    fn real_text_system() -> WindowTextSystem {
        WindowTextSystem::new(Arc::new(TextSystem::new(platform_text_system())))
    }

    fn real_text(ts: &WindowTextSystem) -> InlineText {
        InlineText::new(table(&FontFamilies::detect(&ts.all_font_names())))
    }

    #[test]
    fn real_shaping_gives_consistent_widths_and_stops() {
        let ts = real_text_system();
        let text = real_text(&ts);
        let mut m = GpuiMeasure::new(&ts, text.fonts());
        let size = text::PREVIEW_BODY.size;
        let whole = gv_inline::Measure::shape(&mut m, "Hello world, wrapped", BODY, size);
        let again = gv_inline::Measure::shape(&mut m, "Hello world, wrapped", BODY, size);
        assert!(whole.width() > 50.0, "{}", whole.width());
        assert_eq!(whole.width(), again.width(), "shaping is deterministic");
        assert_eq!(whole.stops(), again.stops());
        // Words shaped apart add up to the whole, give or take kerning.
        let a = gv_inline::Measure::shape(&mut m, "Hello world,", BODY, size);
        let b = gv_inline::Measure::shape(&mut m, " wrapped", BODY, size);
        assert!(
            (a.width() + b.width() - whole.width()).abs() < 1.0,
            "{} + {} vs {}",
            a.width(),
            b.width(),
            whole.width()
        );
        // The stop x of a prefix equals the prefix's own width.
        let prefix = whole
            .stops()
            .iter()
            .find(|s| s.offset as usize == "Hello world,".len())
            .map(|s| s.x);
        assert!(
            prefix.is_some_and(|x| (x - a.width()).abs() < 1.0),
            "{prefix:?}"
        );
        // Stops are grapheme boundaries, ascending, from 0 to the width.
        for text in [
            "Hello world",
            "日本語の文章。",
            "family 👩‍👩‍👧‍👦 and 👍🏽 1️⃣",
            "e\u{301}te\u{301}",
        ] {
            let run = gv_inline::Measure::shape(&mut m, text, BODY, size);
            let graphemes: Vec<usize> = text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            let stops = run.stops();
            assert_eq!(stops.first().map(|s| (s.offset, s.x)), Some((0, 0.0)));
            assert_eq!(stops.last().map(|s| s.offset as usize), Some(text.len()));
            assert_eq!(stops.last().map(|s| s.x), Some(run.width()));
            assert!(
                stops
                    .windows(2)
                    .all(|w| w[0].x <= w[1].x && w[0].offset < w[1].offset)
            );
            assert!(
                stops
                    .iter()
                    .all(|s| graphemes.contains(&(s.offset as usize))),
                "{text:?}: {stops:?}"
            );
            assert_eq!(run.glyphs().len, text.len());
        }
        assert_eq!(m.shaped(), 8);
    }

    #[test]
    fn real_metrics_come_from_the_primary_font() {
        let ts = real_text_system();
        let mut text = real_text(&ts);
        let size = text::PREVIEW_BODY.size;
        let mut m = GpuiMeasure::new(&ts, text.fonts());
        let metrics = gv_inline::Measure::metrics(&mut m, BODY, size);
        assert!(
            metrics.ascent > 0.5 * size && metrics.ascent < 1.3 * size,
            "{metrics:?}"
        );
        assert!(
            metrics.descent > 0.05 * size && metrics.descent < 0.5 * size,
            "{metrics:?}"
        );
        // From the font's tables, or (a font without an OS/2 x-height, as
        // on some Linux systems) the `x` glyph or half an em: never 0.
        assert!(
            metrics.x_height > 0.3 * size && metrics.x_height < 0.7 * size,
            "{metrics:?}"
        );
        // An emoji (a fallback face) and CJK do not grow a 24 px line.
        for content in ["plain words 👩‍👩‍👧‍👦🎉 more", "日本語の文章 with CJK"]
        {
            let pieces = [T(content, body())];
            let mut block = InlineBlock::new(Arc::new(build(&pieces, 0)));
            let layout = text.layout(&mut block, 600.0, &ts);
            assert_eq!(layout.lines().len(), 1);
            assert_eq!(layout.height(), text::PREVIEW_BODY.line, "{content:?}");
        }
    }

    #[test]
    fn real_letter_spacing_moves_every_cluster() {
        // H1 tracking, -0.02 em at 26 px, applied by the layout (GPUI has none).
        let ts = real_text_system();
        let mut text = real_text(&ts);
        let h1 = text::PREVIEW_H1.size;
        let words = "Inline formatting context";
        let plain = RunStyle::new(BOLD, h1);
        let tracked = plain.with_letter_spacing(text::H1_TRACKING * h1);
        let width = |text: &mut InlineText, style: RunStyle| {
            let mut block = InlineBlock::new(Arc::new(build(&[T(words, style)], 0)));
            text.layout(&mut block, f32::INFINITY, &ts).width()
        };
        let (a, b) = (width(&mut text, plain), width(&mut text, tracked));
        let expected = text::H1_TRACKING * h1 * words.graphemes(true).count() as f32;
        assert!(
            (b - a - expected).abs() < 0.01,
            "{a} → {b}, expected Δ {expected}"
        );
        assert_eq!(text.run_stats().misses, 1, "tracking shares the shaped run");
    }

    #[test]
    fn caret_and_hit_round_trip_with_real_shaping() {
        let ts = real_text_system();
        let mut text = real_text(&ts);
        let mut block = InlineBlock::new(Arc::new(build(&mixed(), 100)));
        let layout = text.layout(&mut block, 260.0, &ts);
        assert!(layout.lines().len() >= 3, "{} lines", layout.lines().len());
        let shaped = layout.shaped().clone();
        let mut checked = 0;
        for offset in shaped.caret_stops() {
            for affinity in [Affinity::Upstream, Affinity::Downstream] {
                let c = layout.caret(offset, affinity);
                let y = layout.lines()[c.line].top + 1.0;
                let h = layout.hit(c.x, y);
                assert_eq!(h.offset, offset, "offset {offset} {affinity:?} at {c:?}");
                assert_eq!(
                    layout.caret(h.offset, h.affinity),
                    c,
                    "same caret for {h:?}"
                );
                assert_eq!(
                    h.source,
                    layout.paragraph().source_offset(h.offset, h.affinity)
                );
                assert!(h.source >= 100);
                checked += 1;
            }
        }
        assert!(checked > 150, "{checked}");
    }

    /// Hit testing through GPUI's mouse dispatch, in a window whose text system
    /// is the platform's real one.
    #[test]
    fn mouse_hits_map_to_source_offsets_with_real_text() {
        let dispatcher = TestDispatcher::new(0);
        let mut cx = TestAppContext::build_with_text_system(
            dispatcher.clone(),
            Some("mouse_hits_map_to_source_offsets_with_real_text"),
            platform_text_system(),
        );
        {
            let names = cx.text_system().all_font_names();
            let text = InlineText::new(table(&FontFamilies::detect(&names)));
            let paragraphs = vec![
                build(&mixed(), 1000),
                build(&[T("next block", body())], 2000),
            ];
            let (view, vcx) = cx.add_window_view(|_, _| View::new(paragraphs, text, 300.0));
            draw(vcx);
            let block = view.read_with(vcx, |v, _| v.blocks[0].clone());
            let (bounds, layout) = {
                let b = block.borrow();
                (
                    b.bounds().expect("painted"),
                    b.layout().cloned().expect("laid out"),
                )
            };
            let mut expected = Vec::new();
            // A point on each line, a point on the math box, one past a line end.
            for line in layout.lines() {
                let (x, y) = (line.x + line.width / 3.0, line.top + line.height / 2.0);
                let p = bounds.origin + point(px(x), px(y));
                vcx.simulate_click(p, Modifiers::none());
                expected.push(layout.hit(x, y).source);
            }
            let math = layout
                .fragments()
                .iter()
                .find(|f| layout.paragraph().items()[f.item].inline_box().is_some())
                .expect("math box laid out")
                .clone();
            let line = layout
                .lines()
                .iter()
                .find(|l| {
                    l.frags.contains(
                        &layout
                            .fragments()
                            .iter()
                            .position(|f| *f == math)
                            .unwrap_or(0),
                    )
                })
                .expect("line of the box");
            for dx in [0.25, 0.75] {
                let (x, y) = (math.x + dx * math.width, line.top + 2.0);
                vcx.simulate_click(bounds.origin + point(px(x), px(y)), Modifiers::none());
                expected.push(layout.hit(x, y).source);
            }
            let clicks = view.read_with(vcx, |v, _| v.clicks.clone());
            assert_eq!(clicks, expected);
            // Before and after the box: its source start and end.
            let math_source = layout.paragraph().items()[math.item].source();
            assert_eq!(clicks[clicks.len() - 2], math_source.start);
            assert_eq!(clicks[clicks.len() - 1], math_source.end);
            assert!(clicks.iter().all(|s| (1000..2000).contains(s)));
            // The real text system shaped the runs: widths are not the no-op
            // system's 0.6 em per character.
            let first = layout.shaped().run(0).map(|r| r.width()).unwrap_or(0.0);
            let noop = 0.6 * text::PREVIEW_BODY.size * "Glyphvane lays out ".chars().count() as f32;
            assert!((first - noop).abs() > 0.5, "{first} vs {noop}");
        }
        cx.run_until_parked();
        cx.update(|cx| cx.quit());
        cx.run_until_parked();
        drop(cx);
        dispatcher.drain_tasks();
    }
}

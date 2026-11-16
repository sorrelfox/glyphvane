// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The paragraph element: lays a paragraph out with `gv-inline` at the
//! width GPUI gives it and paints the layout with GPUI primitives.

use std::cell::RefCell;
use std::ops::Range;
use std::panic;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, AvailableSpace, BorderStyle, Bounds, Corners, Element, ElementId, GlobalElementId, Hsla,
    ImageSource, ImgResourceLoader, InspectorElementId, IntoElement, LayoutId, LineLayout,
    PathBuilder, Pixels, Point, RenderImage, Style, TransformationMatrix, UnderlineStyle, Window,
    fill, point, px, quad, relative, size,
};
use gv_inline::{
    Bias, BoxKind, Decoration, DecorationKind, Fragment, InlineBox, ItemKind, Layout, Rect, Shaped,
    UnderlineStyle as LineStyle,
};

use super::{BoxContent, Glyphs, InlineBlock, InlineText, Painted, Palette, Token};
use crate::metrics::{HAIRLINE, SELECTED_BOX_TINT_OPACITY, radius, size::CARET_WIDTH};

/// What one paint of a paragraph drew (lines inside the content mask only).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaintStats {
    pub lines: u32,
    pub glyphs: u32,
    /// Run backgrounds (inline code, keycaps, highlights).
    pub backgrounds: u32,
    /// Underlines and strikethroughs.
    pub decorations: u32,
    pub selection_rects: u32,
    /// Rectangles of highlighted ranges (find matches).
    pub highlight_rects: u32,
    pub boxes: u32,
    pub caret: bool,
}

/// A paragraph of inline content.
///
/// Made every frame from the view's [`InlineBlock`] and [`InlineText`]. It
/// takes the full width of its parent and is as tall as its lines. The
/// selection and the caret are in **source** offsets; the parts outside the
/// paragraph's source span are ignored, so a view can pass one selection to
/// all its paragraphs.
#[derive(Debug)]
pub struct InlineParagraph {
    block: Rc<RefCell<InlineBlock>>,
    text: Rc<RefCell<InlineText>>,
    palette: Arc<Palette>,
    selection: Option<Range<usize>>,
    highlights: Vec<(Range<usize>, Token)>,
    caret: Option<(usize, Bias)>,
    cull: bool,
}

impl InlineParagraph {
    pub fn new(
        block: Rc<RefCell<InlineBlock>>,
        text: Rc<RefCell<InlineText>>,
        palette: Arc<Palette>,
    ) -> Self {
        Self {
            block,
            text,
            palette,
            selection: None,
            highlights: Vec::new(),
            caret: None,
            cull: true,
        }
    }

    /// Paints the selection `source` (document offsets) in
    /// `selection.text`, under the glyphs. Selected boxes get a translucent
    /// tint on top.
    pub fn selection(mut self, source: Range<usize>) -> Self {
        self.selection = (source.start < source.end).then_some(source);
        self
    }

    /// Paints the source ranges `ranges` (offsets as for the selection),
    /// each filled with its token, over the run backgrounds and under the
    /// selection and the glyphs (find matches). Empty ranges and the parts
    /// outside the paragraph are ignored.
    pub fn highlights(mut self, ranges: Vec<(Range<usize>, Token)>) -> Self {
        self.highlights = ranges;
        self
    }

    /// Paints a caret (2 px, `accent.default`) at a document offset, if the
    /// offset is in this paragraph. `bias` moves a position inside an atomic
    /// unit (a box, an escape) to its start or end.
    pub fn caret(mut self, source: usize, bias: Bias) -> Self {
        self.caret = Some((source, bias));
        self
    }

    /// Whether lines outside the content mask are skipped (the default).
    pub fn cull(mut self, cull: bool) -> Self {
        self.cull = cull;
        self
    }
}

impl IntoElement for InlineParagraph {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for InlineParagraph {
    type RequestLayoutState = ();
    type PrepaintState = Option<Arc<Layout<Glyphs>>>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let block = self.block.clone();
        let text = self.text.clone();
        let id = window.request_measured_layout(style, move |known, available, window, _cx| {
            let mut block = block.borrow_mut();
            let mut text = text.borrow_mut();
            let text_system = window.text_system();
            let width = match (known.width, available.width) {
                (Some(width), _) | (None, AvailableSpace::Definite(width)) => f32::from(width),
                (None, AvailableSpace::MinContent) => {
                    text.min_content_width(&mut block, text_system)
                }
                (None, AvailableSpace::MaxContent) => f32::INFINITY,
            };
            let layout = text.layout(&mut block, width, text_system);
            let width = if width.is_finite() {
                width
            } else {
                layout.width()
            };
            size(px(width), px(layout.height()))
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<Arc<Layout<Glyphs>>> {
        let mut block = self.block.borrow_mut();
        let layout = self.text.borrow_mut().layout(
            &mut block,
            f32::from(bounds.size.width),
            window.text_system(),
        );
        block.painted = Some(Painted {
            bounds,
            layout: layout.clone(),
            stats: PaintStats::default(),
        });
        Some(layout)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut Option<Arc<Layout<Glyphs>>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(layout) = layout.clone() else {
            return;
        };
        let stats = {
            let block = self.block.borrow();
            let mut painter = Painter {
                window,
                cx,
                layout: &layout,
                origin: bounds.origin,
                width: bounds.size.width,
                palette: &self.palette,
                block: &block,
                stats: PaintStats::default(),
            };
            painter.paint(
                self.selection.clone(),
                &self.highlights,
                self.caret,
                self.cull,
            );
            painter.stats
        };
        if let Some(painted) = self.block.borrow_mut().painted.as_mut() {
            painted.stats = stats;
        }
    }
}

struct Painter<'a> {
    window: &'a mut Window,
    cx: &'a mut App,
    layout: &'a Layout<Glyphs>,
    origin: Point<Pixels>,
    width: Pixels,
    palette: &'a Palette,
    block: &'a InlineBlock,
    stats: PaintStats,
}

impl Painter<'_> {
    fn at(&self, x: f32, y: f32) -> Point<Pixels> {
        point(self.origin.x + px(x), self.origin.y + px(y))
    }

    fn rect(&self, r: Rect) -> Bounds<Pixels> {
        Bounds::new(self.at(r.x, r.y), size(px(r.width), px(r.height)))
    }

    fn paint(
        &mut self,
        selection: Option<Range<usize>>,
        highlights: &[(Range<usize>, Token)],
        caret: Option<(usize, Bias)>,
        cull: bool,
    ) {
        let layout = self.layout;
        let paragraph = layout.paragraph();
        // Block-local y range to paint.
        let (y0, y1) = if cull {
            let mask = self.window.content_mask().bounds;
            (
                f32::from(mask.top() - self.origin.y),
                f32::from(mask.bottom() - self.origin.y),
            )
        } else {
            (f32::NEG_INFINITY, f32::INFINITY)
        };
        let lines = layout.lines_between(y0, y1);
        // The selection as a logical range; a partly covered box is whole.
        let selected = selection
            .map(|r| {
                let (a, _) = paragraph.logical_offset(r.start, Bias::Left);
                let (b, _) = paragraph.logical_offset(r.end, Bias::Right);
                a..b
            })
            .filter(|r| r.start < r.end);

        // 1. Run backgrounds, under the selection and the glyphs. The other
        // decorations wait for their line's layer.
        let mut line_decorations: Vec<Vec<Decoration>> = Vec::with_capacity(lines.len());
        for li in lines.clone() {
            let mut decorations = layout.decorations(li);
            let backgrounds = decorations
                .iter()
                .take_while(|d| matches!(d.kind, DecorationKind::Background { .. }))
                .count();
            for d in decorations.drain(..backgrounds) {
                self.paint_background(&d);
            }
            line_decorations.push(decorations);
        }

        // 2. Highlights, then the selection, under the glyphs.
        let span = paragraph.source_span();
        for (r, token) in highlights {
            let (start, end) = (r.start.max(span.start), r.end.min(span.end));
            if start >= end {
                continue;
            }
            let (a, _) = paragraph.logical_offset(start, Bias::Left);
            let (b, _) = paragraph.logical_offset(end, Bias::Right);
            let color = self.palette.color(*token);
            for rect in layout.selection_rects(a, b) {
                if rect.bottom() < y0 || rect.y > y1 {
                    continue;
                }
                self.window.paint_quad(fill(self.rect(rect), color));
                self.stats.highlight_rects += 1;
            }
        }
        if let Some(r) = &selected {
            let color = self.palette.color(Token::Selection);
            for rect in layout.selection_rects(r.start, r.end) {
                if rect.bottom() < y0 || rect.y > y1 {
                    continue;
                }
                self.window.paint_quad(fill(self.rect(rect), color));
                self.stats.selection_rects += 1;
            }
        }

        // 3. Glyphs and line decorations, one layer per line.
        for (li, decorations) in lines.clone().zip(&line_decorations) {
            self.paint_line(li, decorations);
        }

        // 4. Boxes, outside the line layers: inside one layer, image sprites
        // would draw after every quad, tint included.
        for li in lines {
            let line = &layout.lines()[li];
            for f in layout.line_fragments(li) {
                let item = &paragraph.items()[f.item];
                let ItemKind::Box(b) = item.kind() else {
                    continue;
                };
                let bounds = Bounds::new(
                    self.at(f.x, line.baseline - f.ascent),
                    size(px(f.width), px(f.ascent + f.descent)),
                );
                self.paint_box(b, bounds);
                self.stats.boxes += 1;
                let range = item.range();
                if selected
                    .as_ref()
                    .is_some_and(|s| s.start <= range.start && range.end <= s.end)
                {
                    let tint = self
                        .palette
                        .color(Token::Selection)
                        .opacity(SELECTED_BOX_TINT_OPACITY);
                    self.window.paint_quad(fill(bounds, tint));
                }
            }
        }

        // 5. The caret, on top.
        if let Some((source, bias)) = caret {
            let span = paragraph.source_span();
            if span.start <= source && source <= span.end {
                let c = layout.caret_for_source(source, bias);
                let bounds = Bounds::new(
                    self.at(c.x - CARET_WIDTH / 2.0, c.top),
                    size(px(CARET_WIDTH), px(c.height)),
                );
                self.window
                    .paint_quad(fill(bounds, self.palette.color(Token::AccentDefault)));
                self.stats.caret = true;
            }
        }
    }

    fn paint_background(&mut self, d: &Decoration) {
        let DecorationKind::Background {
            role,
            radius,
            round_left,
            round_right,
        } = d.kind
        else {
            return;
        };
        let r = px(radius);
        let (left, right) = (
            if round_left { r } else { px(0.) },
            if round_right { r } else { px(0.) },
        );
        let corners = Corners {
            top_left: left,
            bottom_left: left,
            top_right: right,
            bottom_right: right,
        };
        self.window
            .paint_quad(fill(self.rect(d.rect), self.palette.role(role)).corner_radii(corners));
        self.stats.backgrounds += 1;
    }

    fn paint_line(&mut self, li: usize, decorations: &[Decoration]) {
        let layout = self.layout;
        let line = &layout.lines()[li];
        let items = layout.paragraph().items();
        let shaped = layout.shaped();
        let line_bounds = Bounds::new(self.at(0.0, line.top), size(self.width, px(line.height)));
        let origin = self.origin;
        let palette = self.palette;
        let mut glyphs = 0;
        self.window.paint_layer(line_bounds, |window| {
            for f in layout.line_fragments(li) {
                let ItemKind::Text(style) = items[f.item].kind() else {
                    continue;
                };
                let Some(run) = shaped.run(f.item) else {
                    continue;
                };
                let left = point(origin.x, origin.y + px(line.baseline - f.shift));
                glyphs += paint_glyphs(
                    window,
                    run.glyphs(),
                    shaped,
                    layout.run_origin_x(f),
                    f,
                    left,
                    palette.role(style.color),
                );
            }
            for d in decorations {
                paint_decoration(window, d, origin, palette);
            }
        });
        self.stats.lines += 1;
        self.stats.glyphs += glyphs;
        self.stats.decorations += u32::try_from(decorations.len()).unwrap_or(u32::MAX);
    }

    fn paint_box(&mut self, b: &InlineBox, bounds: Bounds<Pixels>) {
        let block = self.block;
        match block.box_content(b.id) {
            Some(BoxContent::Image(source)) => {
                match resolve_image(source, self.window, self.cx) {
                    Some(image) => {
                        let frame = if image.frame_count() > 1 && !self.cx.reduce_motion() {
                            self.window.request_animation_frame();
                            super::animation_frame(&image, super::animation_clock_ms())
                        } else {
                            0
                        };
                        // A failure (for example a full atlas) leaves the
                        // box empty for this frame.
                        let _ = self.window.paint_image(
                            bounds,
                            bounds,
                            Corners::default(),
                            image,
                            frame,
                            false,
                        );
                    }
                    None => self.paint_placeholder(bounds),
                }
            }
            Some(BoxContent::Icon { path, color }) => {
                let side = bounds.size.width.min(bounds.size.height);
                let icon = Bounds::centered_at(bounds.center(), size(side, side));
                // An icon missing from the assets draws nothing.
                let _ = self.window.paint_svg(
                    icon,
                    path.clone(),
                    None,
                    TransformationMatrix::unit(),
                    self.palette.role(*color),
                    self.cx,
                );
            }
            Some(BoxContent::Blank) => {}
            Some(BoxContent::Diagram { diagram, scale }) => {
                super::diagram::paint_diagram(
                    self.window,
                    diagram,
                    bounds.origin,
                    *scale,
                    self.palette,
                );
            }
            Some(BoxContent::Math {
                layout,
                em,
                color,
                text_font,
            }) => {
                let current = self.palette.role(*color);
                super::math::paint_math(
                    self.window,
                    self.cx,
                    layout,
                    bounds.origin,
                    *em,
                    current,
                    text_font,
                );
            }
            None => match b.kind {
                BoxKind::Checkbox { checked } => self.paint_checkbox(bounds, checked),
                BoxKind::Math | BoxKind::Image | BoxKind::Other => self.paint_placeholder(bounds),
            },
        }
    }

    /// A box whose content is not available (yet): a sunken rectangle with
    /// a hairline.
    fn paint_placeholder(&mut self, bounds: Bounds<Pixels>) {
        self.window.paint_quad(quad(
            bounds,
            px(radius::CHIP),
            self.palette.color(Token::SurfaceSunken),
            px(HAIRLINE),
            self.palette.color(Token::BorderStrong),
            BorderStyle::Solid,
        ));
    }

    /// A task-list checkbox: `accent.default` with an `accent.on` tick when
    /// checked, a raised box with a strong border otherwise.
    fn paint_checkbox(&mut self, bounds: Bounds<Pixels>, checked: bool) {
        let r = px(radius::SMALL_CONTROL);
        if !checked {
            self.window.paint_quad(quad(
                bounds,
                r,
                self.palette.color(Token::SurfaceRaised),
                px(HAIRLINE),
                self.palette.color(Token::BorderStrong),
                BorderStyle::Solid,
            ));
            return;
        }
        self.window
            .paint_quad(fill(bounds, self.palette.color(Token::AccentDefault)).corner_radii(r));
        // Lucide's `check` (M20 6 9 17l-5-5) on the box's 24 grid.
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let grid = |x: f32, y: f32| {
            point(
                bounds.origin.x + px(x / 24.0 * w),
                bounds.origin.y + px(y / 24.0 * h),
            )
        };
        let mut path = PathBuilder::stroke(px(w / 8.0));
        path.move_to(grid(19.0, 7.0));
        path.line_to(grid(9.5, 16.5));
        path.line_to(grid(5.0, 12.0));
        if let Ok(path) = path.build() {
            self.window
                .paint_path(path, self.palette.color(Token::AccentOn));
        }
    }
}

/// Paints the glyphs of one text fragment straight from the run's cached
/// `LineLayout`: the glyphs whose cluster starts in the fragment's range,
/// moved by letter spacing. `left` is the block's left edge on the
/// fragment's baseline. Returns the number of glyphs.
fn paint_glyphs(
    window: &mut Window,
    line: &LineLayout,
    shaped: &Shaped<Glyphs>,
    run_x: f32,
    f: &Fragment,
    left: Point<Pixels>,
    color: Hsla,
) -> u32 {
    let mut count = 0;
    for run in &line.runs {
        let lo = run.glyphs.partition_point(|g| g.index < f.range.start);
        let hi = run.glyphs.partition_point(|g| g.index < f.range.end);
        for g in &run.glyphs[lo..hi] {
            let x = run_x + f32::from(g.position.x) + shaped.glyph_shift(f.item, g.index);
            let origin = point(left.x + px(x), left.y + g.position.y);
            // A glyph that cannot be rasterised is skipped.
            let _ = if g.is_emoji {
                window.paint_emoji(origin, run.font_id, g.id, line.font_size)
            } else {
                window.paint_glyph(origin, run.font_id, g.id, line.font_size, color)
            };
            count += 1;
        }
    }
    count
}

/// Paints an underline or a strikethrough. GPUI's underline primitive is
/// solid or wavy only: dashed and dotted lines are one quad per dash or dot.
fn paint_decoration(window: &mut Window, d: &Decoration, origin: Point<Pixels>, palette: &Palette) {
    let r = d.rect;
    let bounds = Bounds::new(
        point(origin.x + px(r.x), origin.y + px(r.y)),
        size(px(r.width), px(r.height)),
    );
    match d.kind {
        DecorationKind::Background { role, .. } => {
            window.paint_quad(fill(bounds, palette.role(role)));
        }
        DecorationKind::Strikethrough { color } => {
            window.paint_quad(fill(bounds, palette.role(color)));
        }
        DecorationKind::Underline { style, color } => {
            let color = palette.role(color);
            match style {
                LineStyle::Solid | LineStyle::Wavy => window.paint_underline(
                    bounds.origin,
                    bounds.size.width,
                    &UnderlineStyle {
                        thickness: bounds.size.height,
                        color: Some(color),
                        wavy: style == LineStyle::Wavy,
                    },
                ),
                // A quad with a dashed border looks almost dotted at 1 px
                // (dashes of about 2 px), so both styles are drawn here:
                // dashes 3 on and 2 off, dots as squares 1 on and 1 off.
                LineStyle::Dashed => paint_broken(window, bounds, 3.0, 2.0, color),
                LineStyle::Dotted => paint_broken(window, bounds, 1.0, 1.0, color),
            }
        }
    }
}

/// Paints a broken line across `bounds`: pieces `on` thicknesses long with
/// gaps `off` thicknesses long, from the left. The last piece is cut at the
/// right end but kept at least square.
fn paint_broken(window: &mut Window, bounds: Bounds<Pixels>, on: f32, off: f32, color: Hsla) {
    let thickness = f32::from(bounds.size.height).max(HAIRLINE);
    let (on, step) = (on * thickness, (on + off) * thickness);
    let width = f32::from(bounds.size.width);
    let mut x = 0.0;
    while x + thickness <= width + 0.01 {
        let len = on.min(width - x).max(thickness);
        window.paint_quad(fill(
            Bounds::new(
                point(bounds.origin.x + px(x), bounds.origin.y),
                size(px(len), px(thickness)),
            ),
            color,
        ));
        x += step;
    }
}

/// The decoded image of a source, if it is loaded. Resources load through
/// GPUI's asset system, which redraws the view when they arrive.
fn resolve_image(
    source: &ImageSource,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    match source {
        ImageSource::Render(image) => Some(image.clone()),
        ImageSource::Image(image) => image.clone().use_render_image(window, cx),
        ImageSource::Resource(resource) => window
            .use_asset::<ImgResourceLoader>(resource, cx)
            .and_then(Result::ok),
        ImageSource::Custom(load) => load(window, cx).and_then(Result::ok),
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view's element: one element for the whole view.
//!
//! - **Prepaint** asks the view for a [`Frame`]: it sets the soft-wrap
//!   width to the text column's, brings the caret into view when asked to,
//!   clamps the scroll, highlights and shapes the lines of the visible
//!   display rows, and records what to draw: the code fill and the current
//!   line (across all the rows of its line) under everything, selections,
//!   the rows, link underlines, the composition underline and the carets
//!   inside the text column, the line numbers in the gutter (on a line's
//!   first row only) and the scrollbar thumb.
//! - **Paint** draws the frame, registers the input handler for the focused
//!   view and the pointer handlers (presses, drags, the wheel and the
//!   scrollbar thumb).

use std::ops::Range;
use std::panic;

use gpui::{
    App, Bounds, ContentMask, Context, CursorStyle, DispatchPhase, Element, ElementId,
    ElementInputHandler, Entity, FocusHandle, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, ScrollEvent, ShapedLine, SharedString, Style,
    TextAlign, TextRun, TransformationMatrix, UnderlineStyle, Window, fill, point, px, relative,
    size,
};
use gv_editor::SelectionSpec;
use gv_text::{Bias, TextSnapshot};
use unicode_segmentation::UnicodeSegmentation as _;

use super::fold::{self, VisualLine};
use super::highlight::Underline;
use super::input::Columns;
use super::live_widgets::{WidgetClick, WidgetPaint};
use super::rows::RowEnv;
use super::source_a11y;
use super::wrap::RowMap;
use super::{EditorLayout, EditorView, Top};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::inline::{Palette, Token};
use crate::metrics::size::{
    CARET_WIDTH, SCROLLBAR_MIN_THUMB, SCROLLBAR_THUMB, editor as editor_size, split,
};
use crate::metrics::spacing::{inline, scrollbar};
use crate::metrics::{HAIRLINE, icon, radius};

/// Pointer travel (px) after which an Alt-press selects a column instead of
/// adding a caret.
const DRAG_THRESHOLD: f32 = 3.0;

/// The height of a misspelling's tick on the scrollbar (not designed).
const SPELL_TICK: f32 = 2.0;

/// What the left button is doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Press {
    #[default]
    None,
    /// Selecting: `tail` is what the press selected (a caret, a word or a
    /// line), which a drag extends by `unit`.
    Select { tail: Range<usize>, unit: Unit },
    /// Alt-press: a caret on release, a column selection when dragged.
    Column {
        tail: usize,
        origin: Point<Pixels>,
        moved: bool,
    },
    /// Dragging the scrollbar thumb, held `grab` px below its top.
    Thumb { grab: f32 },
}

/// What a selecting drag extends by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unit {
    Char,
    Word,
    Line,
}

/// The scrollbar of the last frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scrollbar {
    pub thumb: Bounds<Pixels>,
    /// Where presses go to the scrollbar.
    pub track: Bounds<Pixels>,
}

/// An underline of a link target or of the composition.
#[derive(Debug, Clone)]
pub(crate) enum Line {
    /// A wavy or solid line.
    Plain {
        origin: Point<Pixels>,
        width: Pixels,
        color: Hsla,
        wavy: bool,
    },
    /// Dashes or dots: pieces `on` thicknesses long, `off` apart.
    Broken {
        bounds: Bounds<Pixels>,
        on: f32,
        off: f32,
        color: Hsla,
    },
}

/// What the last frame drew, for tests.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct Probe {
    /// The lines laid out.
    pub lines: Range<usize>,
    /// The display rows laid out.
    pub rows: Range<usize>,
    /// Each display row drawn: its line and its continuation indent in px
    /// (`None` on a line's first row).
    pub row_lines: Vec<(usize, usize, Option<f32>)>,
    /// Lines with the current-line fill.
    pub current: Vec<usize>,
    /// The current-line fills.
    pub current_fills: Vec<Bounds<Pixels>>,
    /// The display row of each line number.
    pub number_rows: Vec<usize>,
    /// Lines with the code fill.
    pub code: Vec<usize>,
    /// Each line number and its colour.
    pub numbers: Vec<(usize, Token)>,
    pub carets: Vec<Bounds<Pixels>>,
    pub selections: Vec<Bounds<Pixels>>,
    /// Find highlights, each with whether it is the current match's.
    pub matches: Vec<(Bounds<Pixels>, bool)>,
    /// Composition underlines: start, width.
    pub marked: Vec<(Point<Pixels>, Pixels)>,
    /// Spelling squiggles: start, width.
    pub squiggles: Vec<(Point<Pixels>, Pixels)>,
    /// Spelling ticks on the scrollbar.
    pub spell_ticks: usize,
    /// Fold chevrons: line, folded, bounds.
    pub chevrons: Vec<(usize, bool, Bounds<Pixels>)>,
    /// Fold placeholder chips and the text they hide.
    pub chips: Vec<(Bounds<Pixels>, Range<usize>)>,
    /// Their fills.
    pub chip_tokens: Vec<Token>,
    /// Live Preview widgets: bounds and kind (`chip`, `text`, `task`,
    /// `image`, `math`).
    pub widgets: Vec<(Bounds<Pixels>, &'static str)>,
    /// The display text of each line drawn.
    pub row_texts: Vec<(usize, String)>,
    /// Live Preview block widgets: bounds and kind.
    pub blocks: Vec<(Bounds<Pixels>, &'static str)>,
    pub columns: Option<Columns>,
}

/// Everything one paint draws.
pub(crate) struct Frame {
    focus: FocusHandle,
    /// The row height.
    line: f32,
    /// Code fill and current line, under everything.
    under: Vec<PaintQuad>,
    /// Find matches, under the selections.
    matches: Vec<PaintQuad>,
    /// Fold placeholder chips, under the matches.
    chips: Vec<PaintQuad>,
    /// Inside the text column.
    selections: Vec<PaintQuad>,
    /// The current find match, over the selections.
    current_match: Vec<PaintQuad>,
    rows: Vec<(ShapedLine, Point<Pixels>)>,
    lines: Vec<Line>,
    carets: Vec<PaintQuad>,
    /// The text column, where rows are clipped.
    text_clip: Option<Bounds<Pixels>>,
    numbers: Vec<(ShapedLine, Point<Pixels>)>,
    /// Fold chevrons: bounds, SVG path and colour.
    icons: Vec<(Bounds<Pixels>, SharedString, Hsla)>,
    thumb: Option<PaintQuad>,
    /// Misspellings on the scrollbar, under the thumb.
    ticks: Vec<PaintQuad>,
    /// Where the pointer is an I-beam.
    text_area: Option<Bounds<Pixels>>,
    /// Where the pointer is a hand: chevrons and chips.
    pointers: Vec<Bounds<Pixels>>,
    /// Live Preview widgets, over the rows.
    widgets: Vec<Drawn>,
    /// Live Preview block widgets: their views, over the rows.
    blocks: Vec<super::live_blocks::DrawnBlock>,
}

/// A Live Preview widget to draw.
enum Drawn {
    Image {
        bounds: Bounds<Pixels>,
        image: std::sync::Arc<gpui::RenderImage>,
    },
    Math {
        origin: Point<Pixels>,
        layout: std::sync::Arc<gv_math::MathLayout>,
        em: f32,
        color: Hsla,
    },
    Quad(PaintQuad),
}

impl Frame {
    fn new(focus: FocusHandle, line: f32) -> Self {
        Self {
            focus,
            line,
            under: Vec::new(),
            matches: Vec::new(),
            chips: Vec::new(),
            selections: Vec::new(),
            current_match: Vec::new(),
            rows: Vec::new(),
            lines: Vec::new(),
            carets: Vec::new(),
            text_clip: None,
            numbers: Vec::new(),
            icons: Vec::new(),
            thumb: None,
            ticks: Vec::new(),
            text_area: None,
            pointers: Vec::new(),
            widgets: Vec::new(),
            blocks: Vec::new(),
        }
    }
}

fn bounds_of(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(w.max(0.0)), px(h.max(0.0))))
}

/// The baseline of a row `row_h` tall whose top is `top`, as
/// `ShapedLine::paint` centres it.
fn baseline(line: &ShapedLine, top: f32, row_h: f32) -> f32 {
    let (ascent, descent) = (f32::from(line.ascent), f32::from(line.descent));
    top + (row_h - ascent - descent) / 2.0 + ascent
}

impl EditorView {
    /// Lays out one frame in `bounds`.
    pub(crate) fn frame(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Frame {
        #[cfg(test)]
        let _frame = crate::reader::code_highlight::tests::FrameGuard::new();
        self.bounds = Some(bounds);
        let metrics = self.metrics.clone();
        let line_px = metrics.line;
        let mut frame = Frame::new(self.focus.clone(), line_px);
        let theme = Appearance::current_id(cx);
        let palette = Palette::of(theme);
        let focused = self.focus.is_focused(window);
        let (_, space) = self.advances(window);
        let Some(columns) = self.columns(Some(bounds), window) else {
            return frame;
        };
        self.sync_wrap(columns.text_w, cx);
        if let Some(doc) = self.doc.as_mut() {
            doc.editor
                .set_row_height(gv_editor::display_map::Height::from_px(line_px));
        }
        if self.is_live_preview_active() {
            let lines = self.live_window(f32::from(bounds.size.height));
            // Blocks first: images on their own line are blocks, not
            // inline widgets.
            let span = self.live_span(lines.clone());
            self.live_first_pass();
            self.refresh_live_blocks(span, columns.text_w, cx);
            let sizes = super::live_widgets::Sizes {
                line: line_px,
                font: self.fonts.size(),
                advance: space,
                column: columns.text_w,
                scale: window.scale_factor(),
            };
            self.refresh_live_preview(lines, &sizes, cx);
        }
        let Some((lines, rows_total, wrapping)) = self.row_map().map(|map| {
            (
                self.doc
                    .as_ref()
                    .map_or(0, |d| d.editor.text().line_count()),
                map.row_count(),
                map.is_wrapping(),
            )
        }) else {
            return frame;
        };
        let viewport = f32::from(bounds.size.height);
        let pad_top = self.layout.pad_top();

        // Scrolling: a line asked for at the top, the caret into view, then
        // the limits.
        if let Some(top) = self.pending_top.take()
            && let Some(doc) = &self.doc
        {
            let text = doc.editor.text();
            let line = match top {
                Top::Offset(offset) => text.line_of_offset(offset.min(text.len())).unwrap_or(0),
                Top::Line(line) => line.min(text.line_count().saturating_sub(1)),
            };
            let row = RowMap::of(&doc.editor).first_row(line);
            // The row's top at the top edge (the column's top padding
            // scrolls away).
            self.scroll_y = if row == 0 {
                0.0
            } else {
                f64::from(pad_top) + self.geo().row_y(row)
            };
        }
        if std::mem::take(&mut self.autoscroll) {
            self.scroll_into_view(&columns, viewport, space, window, cx);
        }
        let geo = self.geo();
        let content = 2.0 * f64::from(pad_top) + geo.total(rows_total);
        let max_y = (content - f64::from(viewport)).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_y);
        let max_x = if wrapping {
            0.0
        } else {
            (self.widest + CARET_WIDTH + 4.0 * space - columns.text_w).max(0.0)
        };
        self.scroll_x = self.scroll_x.clamp(0.0, max_x);

        let scroll_y = self.scroll_y;
        let pad = f64::from(pad_top);
        let first_row = geo.row_at(scroll_y - pad, rows_total);
        let last_row = geo
            .rows_until(scroll_y - pad + f64::from(viewport), rows_total)
            .min(rows_total);
        let origin_x = self.text_origin_x(&columns);
        let drawn_blocks = self.blocks_to_draw(
            &geo,
            (scroll_y - pad, scroll_y - pad + f64::from(viewport)),
            (columns.text_x, columns.text_w),
            f64::from(columns.top) - scroll_y,
            cx,
        );
        let whitespace = self.show_whitespace;
        let layout = self.layout;
        let fonts = self.fonts.clone();
        let Self {
            doc,
            links,
            find,
            spell,
            live,
            ..
        } = self;
        live.hits.clear();
        let Some(doc) = doc.as_mut() else {
            return frame;
        };
        let wrap = doc.editor.wrap().cloned();
        let text = doc.editor.text().clone();
        let tabs = doc.editor.display().clone();
        let map = RowMap::new(&text, &tabs, wrap.as_ref());
        let pane = layout == EditorLayout::Pane;

        // The visual lines on screen, with their first rows.
        let mut visuals: Vec<(usize, VisualLine)> = Vec::new();
        let mut next = map.line_of_row(first_row);
        while next < lines {
            let row = map.first_row(next);
            if row >= last_row {
                break;
            }
            let Some(visual) = VisualLine::new(&text, tabs.replace_snapshot(), next) else {
                break;
            };
            next = visual.last + 1;
            visuals.push((row, visual));
        }
        #[cfg(test)]
        let (first, last) = (
            visuals.first().map_or(0, |(_, v)| v.first),
            visuals.last().map_or(0, |(_, v)| v.last + 1),
        );
        // Highlight the lines shown, not the ones folds hide.
        let mut styled: Vec<usize> = visuals.iter().flat_map(|(_, v)| v.styled_lines()).collect();
        styled.dedup();
        for run in styled.chunk_by(|a, b| b == &(a + 1)) {
            if let (Some(&a), Some(&b)) = (run.first(), run.last()) {
                doc.highlights.ensure(&doc.syntax, links, a..b + 1);
            }
        }
        // Fences to highlight: on the background executor, never here.
        for (key, code) in doc.highlights.code.take_requests() {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .spawn(async move {
                        crate::reader::code_highlight::highlight(key, &code);
                    })
                    .await;
                this.update(cx, |view, cx| {
                    if let Some(doc) = view.doc.as_mut() {
                        doc.highlights.landed(&key);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }

        let visible = visuals.first().map_or(0, |(_, v)| v.range.start)
            ..visuals.last().map_or(text.len(), |(_, v)| v.range.end);
        let selections: Vec<(Range<usize>, usize)> = {
            let all = doc.editor.selections().as_slice();
            let from = all.partition_point(|s| s.end() < visible.start);
            all[from..]
                .iter()
                .take_while(|s| s.start() <= visible.end)
                .map(|s| (s.range(), s.head()))
                .collect()
        };
        let found = find.marks.touching(text.revision(), visible.clone());
        let misspelled = spell.visible(&doc.editor, visible.clone());
        let marked = doc.editor.marked_ranges();
        let replace = tabs.replace_snapshot();
        let env = RowEnv {
            text: &text,
            tabs: &tabs,
            fonts: &fonts,
            palette: &palette,
            theme,
            whitespace,
            tab_size: tabs.tab_size().get(),
            space,
        };
        let color = |t: Token| palette.color(t);
        let number_font = fonts.regular().clone();
        let mut widest = 0.0f32;
        #[cfg(test)]
        let mut probe = Probe {
            lines: first..last,
            rows: first_row..last_row,
            columns: Some(columns),
            ..Probe::default()
        };
        let row_top = |row: usize| columns.top + (geo.row_y(row) - scroll_y) as f32;
        // Assistive technology: the rows as text runs, and where the
        // primary selection's tail and head are in them.
        let a11y_on = window.is_a11y_active() || cfg!(test);
        let wanted = {
            let primary = doc.editor.selections().primary();
            [primary.tail(), primary.head()]
        };
        let mut a11y_runs: Vec<source_a11y::Run> = Vec::new();
        let mut a11y_found: [Option<source_a11y::Pos>; 2] = [None, None];

        for (line_row, visual) in &visuals {
            let (line_row, line) = (*line_row, visual.first);
            // A block shows instead of the line.
            if geo.hidden(line_row) {
                continue;
            }
            let range = visual.range.clone();
            let has_terminator = text.line_terminator(visual.last).ok().flatten().is_some();
            let breaks = map.breaks(visual);
            let styles = visual.styles(&doc.highlights);
            let Some(row) = doc.rows.row(visual, styles, breaks, env, window) else {
                continue;
            };
            widest = widest.max(row.content_width());
            if a11y_on {
                let tops = |k: usize| row_top(line_row + k);
                let env = source_a11y::LineEnv {
                    text: &text,
                    replace,
                    line,
                    range: visual.range.clone(),
                    terminator: a11y_terminator(&text, visual),
                    origin_x,
                    tops: &tops,
                    row_h: line_px,
                };
                let (runs, found) = source_a11y::line_runs(row, &env, &wanted);
                for (slot, f) in a11y_found.iter_mut().zip(found) {
                    if slot.is_none() {
                        *slot = f.map(|(r, i)| (r + a11y_runs.len(), i));
                    }
                }
                a11y_runs.extend(runs);
            }
            #[cfg(test)]
            probe.row_texts.push((line, row.text.display.clone()));
            let local = |offset: usize| row.column_of(&text, replace, offset, Bias::Left);
            let local_end = |offset: usize| row.column_of(&text, replace, offset, Bias::Right);
            let on_line = |offset: usize| visual.holds(offset);
            // Columns of what is drawn on the line, once for all its rows:
            // ranges with whether they go past its end (they hold its
            // terminator).
            let spans_of = |ranges: &mut dyn Iterator<Item = &Range<usize>>| {
                ranges
                    .filter(|r| r.end >= range.start && r.start <= range.end)
                    .map(|r| {
                        let a = local(r.start.max(range.start));
                        let b = local_end(r.end.min(range.end));
                        (a, b, r.end > range.end && has_terminator)
                    })
                    .collect::<Vec<_>>()
            };
            let found_spans = spans_of(&mut found.iter().map(|(r, _)| r));
            let spell_spans = spans_of(&mut misspelled.iter());
            let found_current: Vec<bool> = found
                .iter()
                .filter(|(r, _)| r.end >= range.start && r.start <= range.end)
                .map(|(_, current)| *current)
                .collect();
            let selection_spans = spans_of(
                &mut selections
                    .iter()
                    .filter(|(r, _)| !r.is_empty())
                    .map(|(r, _)| r),
            );
            let marked_spans: Vec<(usize, usize)> = marked
                .iter()
                .filter(|m| m.start <= range.end && m.end >= range.start)
                .map(|m| (local(m.start), local(m.end)))
                .collect();
            let caret_columns: Vec<usize> = selections
                .iter()
                .filter(|(_, h)| on_line(*h))
                .map(|(_, h)| local(*h))
                .collect();
            let line_top = row_top(line_row);
            let line_h = row.segments.len() as f32 * line_px;

            if row.code {
                #[cfg(test)]
                probe.code.push(line);
                frame.under.push(fill(
                    bounds_of(columns.text_x, line_top, columns.text_w, line_h),
                    color(Token::SurfaceSunken),
                ));
            }
            let current = selections
                .iter()
                .any(|(r, head)| r.is_empty() && on_line(*head));
            if current {
                let rect = bounds_of(
                    columns.gutter_left(),
                    line_top,
                    columns.fill_right - columns.gutter_left(),
                    line_h,
                );
                #[cfg(test)]
                {
                    probe.current.push(line);
                    probe.current_fills.push(rect);
                }
                frame.under.push(fill(rect, color(Token::StateHover)));
            }
            let has_caret = selections.iter().any(|(_, h)| on_line(*h));
            let last_segment = row.segments.len() - 1;

            for (k, segment) in row.segments.iter().enumerate() {
                let display_row = line_row + k;
                if display_row < first_row || display_row >= last_row {
                    continue;
                }
                let top = row_top(display_row);
                let is_last = k == last_segment;
                let cols = segment.columns.clone();
                #[cfg(test)]
                probe
                    .row_lines
                    .push((display_row, line, (k > 0).then_some(segment.indent)));

                // Find matches, as selections are drawn.
                for (&(a, b, beyond), current) in found_spans.iter().zip(&found_current) {
                    if b < cols.start || a > cols.end || (!is_last && a == cols.end) {
                        continue;
                    }
                    let (a, b) = (a.max(cols.start), b.min(cols.end));
                    let past_end = is_last && beyond;
                    if a == b && !past_end {
                        continue;
                    }
                    let x0 = row.x_in(k, a);
                    let x1 = row.x_in(k, b) + if past_end { space } else { 0.0 };
                    let rect = bounds_of(origin_x + x0, top, x1 - x0, line_px);
                    #[cfg(test)]
                    probe.matches.push((rect, *current));
                    if *current {
                        frame
                            .current_match
                            .push(fill(rect, color(super::find::CURRENT_MATCH)));
                    } else {
                        frame.matches.push(fill(rect, color(super::find::MATCH)));
                    }
                }
                // Selections, past the row's end when they go on, and past
                // the line's end when they hold its terminator.
                let mut selected: Vec<Range<usize>> = Vec::new();
                for &(a, b, beyond) in &selection_spans {
                    if b < cols.start || a > cols.end || (!is_last && a == cols.end) {
                        continue;
                    }
                    let (a, b) = (a.max(cols.start), b.min(cols.end));
                    let past_end = is_last && beyond;
                    if a == b && !past_end {
                        continue;
                    }
                    let x0 = row.x_in(k, a);
                    let x1 = row.x_in(k, b) + if past_end { space } else { 0.0 };
                    let rect = bounds_of(origin_x + x0, top, x1 - x0, line_px);
                    #[cfg(test)]
                    probe.selections.push(rect);
                    frame.selections.push(fill(rect, color(Token::Selection)));
                    if a < b {
                        selected.push(a..b);
                    }
                }
                let shaped = if selected.is_empty() {
                    segment.shaped.clone()
                } else {
                    row.with_selection(k, &selected, &fonts, &palette, window)
                };
                let base = baseline(&shaped, top, line_px);
                let seg_x = origin_x + segment.indent;

                // Link underlines.
                for d in &row.decorations {
                    let (a, b) = (
                        d.range.start.max(segment.display.start),
                        d.range.end.min(segment.display.end),
                    );
                    if a >= b {
                        continue;
                    }
                    let x0 = f32::from(shaped.x_for_index(a - segment.display.start));
                    let x1 = f32::from(shaped.x_for_index(b - segment.display.start));
                    let y = base + inline::UNDERLINE_OFFSET;
                    let c = color(d.color);
                    frame.lines.push(match d.kind {
                        Underline::Wavy => Line::Plain {
                            origin: point(px(seg_x + x0), px(y)),
                            width: px(x1 - x0),
                            color: c,
                            wavy: true,
                        },
                        Underline::Dashed => Line::Broken {
                            bounds: bounds_of(seg_x + x0, y, x1 - x0, HAIRLINE),
                            on: 3.0,
                            off: 2.0,
                            color: c,
                        },
                        Underline::Dotted => Line::Broken {
                            bounds: bounds_of(seg_x + x0, y, x1 - x0, HAIRLINE),
                            on: 1.0,
                            off: 1.0,
                            color: c,
                        },
                    });
                }
                // Misspellings: a wavy line, as selections are drawn.
                for &(a, b, _) in &spell_spans {
                    if b <= cols.start || a >= cols.end {
                        continue;
                    }
                    let (a, b) = (a.max(cols.start), b.min(cols.end));
                    let (x0, x1) = (row.x_in(k, a), row.x_in(k, b));
                    let origin = point(px(origin_x + x0), px(base + inline::UNDERLINE_OFFSET));
                    let width = px(x1 - x0);
                    #[cfg(test)]
                    probe.squiggles.push((origin, width));
                    frame.lines.push(Line::Plain {
                        origin,
                        width,
                        color: color(Token::StatusError),
                        wavy: true,
                    });
                }
                // The composition: a thin underline.
                for &(a, b) in &marked_spans {
                    let (a, b, on_row) = if a == b {
                        (a, b, row.segment_of(a) == k)
                    } else {
                        let (a, b) = (a.max(cols.start), b.min(cols.end));
                        (a, b, a < b)
                    };
                    if !on_row {
                        continue;
                    }
                    let (x0, x1) = (row.x_in(k, a), row.x_in(k, b));
                    let origin = point(px(origin_x + x0), px(base + inline::UNDERLINE_OFFSET));
                    let width = px((x1 - x0).max(CARET_WIDTH));
                    #[cfg(test)]
                    probe.marked.push((origin, width));
                    frame.lines.push(Line::Plain {
                        origin,
                        width,
                        color: color(Token::TextPrimary),
                        wavy: false,
                    });
                }
                // Carets.
                for &column in &caret_columns {
                    if !focused || row.segment_of(column) != k {
                        continue;
                    }
                    let x = origin_x + row.x_in(k, column);
                    let y = top + (line_px - metrics.caret_height) / 2.0;
                    let rect = bounds_of(x, y, CARET_WIDTH, metrics.caret_height);
                    #[cfg(test)]
                    probe.carets.push(rect);
                    frame.carets.push(fill(rect, color(Token::AccentDefault)));
                }
                // Fold placeholders: a chip under the `⋯`.
                for mark in &visual.marks {
                    let Some((x0, x1)) = fold::chip_span(row, k, mark, metrics.chip_inset) else {
                        continue;
                    };
                    let h = metrics.chip_height;
                    let rect = bounds_of(origin_x + x0, top + (line_px - h) / 2.0, x1 - x0, h);
                    #[cfg(test)]
                    probe.chips.push((rect, mark.buffer.clone()));
                    // On the code fill or the current line `surface.sunken`
                    // would not show.
                    let chip = if row.code || current {
                        Token::StatePress
                    } else {
                        Token::SurfaceSunken
                    };
                    #[cfg(test)]
                    probe.chip_tokens.push(chip);
                    frame
                        .chips
                        .push(fill(rect, color(chip)).corner_radii(px(radius::CHIP)));
                    frame.pointers.push(rect);
                }
                // Live Preview widgets on this row.
                for span in &row.text.spans {
                    let Some(gv_editor::display_map::Replaced::Widget(id)) = span.replaced else {
                        continue;
                    };
                    let (a, b) = (
                        span.columns.start.max(cols.start),
                        span.columns.end.min(cols.end),
                    );
                    let Some((paint, click)) = live.widgets.paint(id).filter(|_| a < b) else {
                        continue;
                    };
                    let (x0, x1) = (origin_x + row.x_in(k, a), origin_x + row.x_in(k, b));
                    let clickable = !matches!(click, WidgetClick::Reveal);
                    let kind = match paint {
                        WidgetPaint::Chip => {
                            let h = metrics.chip_height;
                            let rect = bounds_of(x0, top + (line_px - h) / 2.0, x1 - x0, h);
                            let token = if clickable {
                                Token::AccentTint
                            } else if row.code || current {
                                Token::StatePress
                            } else {
                                Token::SurfaceSunken
                            };
                            frame
                                .chips
                                .push(fill(rect, color(token)).corner_radii(px(radius::CHIP)));
                            "chip"
                        }
                        WidgetPaint::Text => "text",
                        WidgetPaint::Task { checked } => {
                            let side = (line_px * 0.6).min(x1 - x0).max(4.0);
                            let rect = bounds_of(
                                (x0 + x1 - side) / 2.0,
                                top + (line_px - side) / 2.0,
                                side,
                                side,
                            );
                            let (fill_token, border) = if *checked {
                                (Token::AccentDefault, Token::AccentDefault)
                            } else {
                                (Token::SurfacePage, Token::BorderStrong)
                            };
                            frame.widgets.push(Drawn::Quad(
                                fill(rect, color(fill_token))
                                    .corner_radii(px(radius::CHIP))
                                    .border_widths(px(1.0))
                                    .border_color(color(border)),
                            ));
                            if *checked {
                                let inset = side * 0.15;
                                let mark = bounds_of(
                                    f32::from(rect.origin.x) + inset,
                                    f32::from(rect.origin.y) + inset,
                                    side - 2.0 * inset,
                                    side - 2.0 * inset,
                                );
                                frame.icons.push((
                                    mark,
                                    Icon::MenuCheck.name().path(),
                                    color(Token::AccentOn),
                                ));
                            }
                            "task"
                        }
                        WidgetPaint::Image(image) => {
                            let size = image.size(0);
                            let (iw, ih) = (size.width.0 as f32, size.height.0 as f32);
                            let (w, h) = (x1 - x0, line_px);
                            let scale = if iw > 0.0 && ih > 0.0 {
                                (w / iw).min(h / ih)
                            } else {
                                0.0
                            };
                            let (dw, dh) = (iw * scale, ih * scale);
                            frame.widgets.push(Drawn::Image {
                                bounds: bounds_of(x0, top + (h - dh) / 2.0, dw, dh),
                                image: image.clone(),
                            });
                            "image"
                        }
                        WidgetPaint::Math {
                            layout,
                            em,
                            baseline,
                        } => {
                            frame.widgets.push(Drawn::Math {
                                origin: point(px(x0), px(top + baseline - layout.height * em)),
                                layout: layout.clone(),
                                em: *em,
                                color: color(Token::TextPrimary),
                            });
                            "math"
                        }
                    };
                    let rect = bounds_of(x0, top, x1 - x0, line_px);
                    if clickable {
                        frame.pointers.push(rect);
                    }
                    live.hits.push((rect, id));
                    #[cfg(test)]
                    probe.widgets.push((rect, kind));
                    #[cfg(not(test))]
                    let _ = kind;
                }
                frame.rows.push((shaped, point(px(seg_x), px(top))));

                // The line number, on the line's first row only.
                if k > 0 {
                    continue;
                }
                let number_token = if has_caret {
                    Token::TextPrimary
                } else {
                    Token::TextTertiary
                };
                #[cfg(test)]
                {
                    probe.numbers.push((line, number_token));
                    probe.number_rows.push(display_row);
                }
                let number_color = color(number_token);
                let label = (line + 1).to_string();
                let run = TextRun {
                    len: label.len(),
                    font: number_font.clone(),
                    color: number_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let number = window.text_system().shape_line(
                    SharedString::from(label),
                    px(metrics.number_size),
                    &[run],
                    None,
                );
                let x = columns.numbers_right - f32::from(number.width);
                frame.numbers.push((number, point(px(x), px(top))));

                // The fold chevron, in the Split view.
                if !pane {
                    continue;
                }
                let folded = !visual.marks.is_empty();
                if !folded && !doc.editor.is_foldable(line) {
                    continue;
                }
                let icon = if folded {
                    Icon::Folded
                } else {
                    Icon::FoldMarker
                };
                let size = icon::TINY;
                let rect = bounds_of(
                    columns.numbers_right + split::CHEVRON_GAP,
                    top + (line_px - size) / 2.0,
                    size,
                    size,
                );
                #[cfg(test)]
                probe.chevrons.push((line, folded, rect));
                frame
                    .icons
                    .push((rect, icon.name().path(), color(Token::TextTertiary)));
                frame.pointers.push(bounds_of(
                    columns.numbers_right,
                    top,
                    fold::chevron_slot(),
                    line_px,
                ));
            }
        }
        // The caret's line when it is off screen, so that the selection
        // has a run to point into.
        let head = wanted[1];
        if a11y_on && a11y_found[1].is_none() && (head < visible.start || head > visible.end) {
            let line = text.line_of_offset(head.min(text.len())).unwrap_or(0);
            if let Some(visual) = VisualLine::new(&text, replace, line) {
                let styled: Vec<usize> = visual.styled_lines().collect();
                if let (Some(&a), Some(&b)) = (styled.iter().min(), styled.iter().max()) {
                    doc.highlights.ensure(&doc.syntax, links, a..b + 1);
                }
                let line_row = map.first_row(visual.first);
                let breaks = map.breaks(&visual);
                let styles = visual.styles(&doc.highlights);
                if let Some(row) = doc.rows.row(&visual, styles, breaks, env, window) {
                    let tops = |k: usize| row_top(line_row + k);
                    let env = source_a11y::LineEnv {
                        text: &text,
                        replace,
                        line: visual.first,
                        range: visual.range.clone(),
                        terminator: a11y_terminator(&text, &visual),
                        origin_x,
                        tops: &tops,
                        row_h: line_px,
                    };
                    let (runs, found) = source_a11y::line_runs(row, &env, &wanted);
                    let before = head < visible.start;
                    let shift = if before { 0 } else { a11y_runs.len() };
                    let added = runs.len();
                    if before {
                        for (r, _) in a11y_found.iter_mut().flatten() {
                            *r += added;
                        }
                        a11y_runs.splice(0..0, runs);
                    } else {
                        a11y_runs.extend(runs);
                    }
                    for (slot, f) in a11y_found.iter_mut().zip(found) {
                        if let Some((r, i)) = f {
                            *slot = Some((r + shift, i));
                        }
                    }
                }
            }
        }
        doc.rows.end_frame();
        live.blocks_drawn = drawn_blocks
            .iter()
            .map(|b| (b.id, b.bounds, b.reader.clone()))
            .collect();
        #[cfg(test)]
        {
            probe.blocks = drawn_blocks
                .iter()
                .map(|b| (b.bounds, b.kind.name()))
                .collect();
        }
        frame.blocks = drawn_blocks;
        self.widest = self.widest.max(widest);
        if a11y_on {
            let mut source = self.a11y.borrow_mut();
            source.runs = a11y_runs;
            let [tail, head] = a11y_found;
            let tail = tail.or_else(|| source.locate(wanted[0]));
            let head = head.or_else(|| source.locate(wanted[1]));
            source.selection = tail.zip(head);
            source.scale = window.scale_factor();
            source.read_only = self.read_only;
        }
        #[cfg(test)]
        {
            self.probe = probe;
        }

        frame.text_clip = Some(bounds_of(
            columns.text_x,
            f32::from(bounds.top()),
            columns.text_w + CARET_WIDTH,
            viewport,
        ));
        let zone = 2.0 * scrollbar::INSET_X + SCROLLBAR_THUMB;
        frame.text_area = Some(bounds_of(
            columns.gutter_left(),
            f32::from(bounds.top()),
            f32::from(bounds.right()) - zone - columns.gutter_left(),
            viewport,
        ));

        // The scrollbar: a thumb, no track.
        self.scrollbar = None;
        if content > f64::from(viewport) {
            let track_top = f32::from(bounds.top()) + scrollbar::INSET_Y;
            let track_h = viewport - 2.0 * scrollbar::INSET_Y;
            let thumb_h = (track_h * viewport / content as f32)
                .max(SCROLLBAR_MIN_THUMB)
                .min(track_h);
            let ratio = if max_y > 0.0 {
                (self.scroll_y / max_y) as f32
            } else {
                0.0
            };
            let right = f32::from(bounds.right());
            let thumb = bounds_of(
                right - scrollbar::INSET_X - SCROLLBAR_THUMB,
                track_top + (track_h - thumb_h) * ratio,
                SCROLLBAR_THUMB,
                thumb_h,
            );
            frame.thumb = Some(
                fill(thumb, palette.color(Token::BorderStrong)).corner_radii(px(radius::SCROLLBAR)),
            );
            // A tick per misspelled line, one per pixel row.
            if let Some(doc) = &self.doc {
                let mut last = f32::NEG_INFINITY;
                for row in self.spell.rows(&doc.editor) {
                    let y = (track_top + track_h * (geo.row_y(row) / content) as f32).floor();
                    if y <= last {
                        continue;
                    }
                    last = y + SPELL_TICK - 1.0;
                    frame.ticks.push(fill(
                        bounds_of(
                            right - scrollbar::INSET_X - SCROLLBAR_THUMB,
                            y,
                            SCROLLBAR_THUMB,
                            SPELL_TICK,
                        ),
                        palette.color(Token::StatusError),
                    ));
                }
                #[cfg(test)]
                {
                    self.probe.spell_ticks = frame.ticks.len();
                }
            }
            self.scrollbar = Some(Scrollbar {
                thumb,
                track: bounds_of(right - zone, track_top, zone, track_h),
            });
        }
        frame
    }

    /// Scrolls so that the primary caret is in view, with a row above and
    /// below it and a few characters beside it (sideways only without soft
    /// wrap).
    fn scroll_into_view(
        &mut self,
        columns: &Columns,
        viewport: f32,
        space: f32,
        window: &Window,
        cx: &App,
    ) {
        let Some((line, head, rows, wrapping)) = self.doc.as_ref().and_then(|doc| {
            let text = doc.editor.text();
            let head = doc.editor.selections().primary().head();
            let line = text.line_of_offset(head).ok()?;
            let map = RowMap::of(&doc.editor);
            Some((line, head, map.row_count(), map.is_wrapping()))
        }) else {
            return;
        };
        let Some((first, _, row)) = self.line_row(line, window, cx) else {
            return;
        };
        let Some(editor) = self.editor() else {
            return;
        };
        let column = row.column_of(
            editor.text(),
            editor.display().replace_snapshot(),
            head,
            Bias::Left,
        );
        let (segment, x) = row.x(column);
        let display_row = first + segment;
        let pad_top = self.layout.pad_top();
        let pad = f64::from(pad_top);
        let row_px = self.metrics.line;
        let row_h = f64::from(row_px);
        let geo = self.geo();
        let top = pad + geo.row_y(display_row);
        let wanted_top = if display_row == 0 { 0.0 } else { top - row_h };
        let wanted_bottom = if display_row + 1 >= rows {
            2.0 * pad + geo.total(rows)
        } else {
            top + 2.0 * row_h
        };
        if wanted_top < self.scroll_y {
            self.scroll_y = wanted_top;
        } else if wanted_bottom > self.scroll_y + f64::from(viewport) {
            self.scroll_y = wanted_bottom - f64::from(viewport);
        }
        if wrapping {
            self.scroll_x = 0.0;
            return;
        }
        let margin = 4.0 * space;
        if x - margin < self.scroll_x {
            self.scroll_x = (x - margin).max(0.0);
        } else if x + CARET_WIDTH + margin > self.scroll_x + columns.text_w {
            self.scroll_x = x + CARET_WIDTH + margin - columns.text_w;
        }
        self.widest = self.widest.max(x);
    }

    // ----- the pointer -----------------------------------------------------------

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.focus.focus(window, cx);
        if let Some(bar) = self.scrollbar
            && bar.track.contains(&e.position)
        {
            let grab = if bar.thumb.contains(&e.position) {
                f32::from(e.position.y - bar.thumb.top())
            } else {
                f32::from(bar.thumb.size.height) / 2.0
            };
            self.press = Press::Thumb { grab };
            self.drag_thumb(e.position);
            cx.notify();
            return;
        }
        if !e.modifiers.modified()
            && let Some(hit) = self.fold_hit(e.position, window, cx)
        {
            self.press_fold(hit, window, cx);
            return;
        }
        // A checkbox or a tag in Live Preview.
        if !e.modifiers.modified()
            && e.click_count <= 1
            && self.click_live_widget(e.position, window, cx)
        {
            cx.notify();
            return;
        }
        // Mod+Click on a link follows it (a plain click never does).
        if e.modifiers.secondary(cx)
            && !e.modifiers.shift
            && !e.modifiers.alt
            && e.click_count <= 1
            && self.click_link(e.position, crate::reader::Disposition::InPlace, window, cx)
        {
            return;
        }
        let Some(offset) = self.offset_at(e.position, window, cx) else {
            return;
        };
        let in_gutter = self
            .columns(None, window)
            .is_some_and(|c| f32::from(e.position.x) < c.text_x - editor_size::GUTTER_GAP / 2.0);
        let line_range = self.visual_line_at(offset);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let editor = &mut doc.editor;
        if e.modifiers.alt && !e.modifiers.shift {
            editor.unmark_text();
            self.press = Press::Column {
                tail: offset,
                origin: e.position,
                moved: false,
            };
            cx.notify();
            return;
        }
        let clicks = if in_gutter { 3 } else { e.click_count };
        let text = editor.text().clone();
        self.press = match clicks {
            0 | 1 if e.modifiers.shift => {
                let tail = editor.selections().primary().tail();
                editor.set_selections(&[SelectionSpec::new(tail, offset)], 0);
                Press::Select {
                    tail: tail..tail,
                    unit: Unit::Char,
                }
            }
            0 | 1 => {
                editor.set_cursor(offset);
                Press::Select {
                    tail: offset..offset,
                    unit: Unit::Char,
                }
            }
            2 => {
                let word = word_at(&text, offset);
                editor.select_ranges([word.clone()]);
                Press::Select {
                    tail: word,
                    unit: Unit::Word,
                }
            }
            _ => {
                let line = line_range.unwrap_or_else(|| line_at(&text, offset));
                editor.select_ranges([line.clone()]);
                Press::Select {
                    tail: line,
                    unit: Unit::Line,
                }
            }
        };
        self.changed(window, cx);
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !e.dragging() {
            return;
        }
        match self.press.clone() {
            Press::None => {}
            Press::Thumb { .. } => {
                self.drag_thumb(e.position);
                cx.notify();
            }
            Press::Select { tail, unit } => {
                let Some(offset) = self.offset_at(e.position, window, cx) else {
                    return;
                };
                let line_range = self.visual_line_at(offset);
                let Some(doc) = self.doc.as_mut() else {
                    return;
                };
                let text = doc.editor.text().clone();
                let (from, to) = match unit {
                    Unit::Char => (tail.start, offset),
                    Unit::Word => {
                        let word = word_at(&text, offset);
                        if offset < tail.start {
                            (tail.end, word.start)
                        } else {
                            (tail.start, word.end.max(tail.end))
                        }
                    }
                    Unit::Line => {
                        let line = line_range.unwrap_or_else(|| line_at(&text, offset));
                        if offset < tail.start {
                            (tail.end, line.start)
                        } else {
                            (tail.start, line.end.max(tail.end))
                        }
                    }
                };
                doc.editor
                    .set_selections(&[SelectionSpec::new(from, to)], 0);
                self.changed(window, cx);
            }
            Press::Column {
                tail,
                origin,
                moved,
            } => {
                let far = (e.position.x - origin.x).abs() > px(DRAG_THRESHOLD)
                    || (e.position.y - origin.y).abs() > px(DRAG_THRESHOLD);
                if !(moved || far) {
                    return;
                }
                self.press = Press::Column {
                    tail,
                    origin,
                    moved: true,
                };
                let Some(offset) = self.offset_at(e.position, window, cx) else {
                    return;
                };
                if let Some(doc) = self.doc.as_mut() {
                    doc.editor.select_columns(tail, offset);
                }
                self.changed(window, cx);
            }
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<'_, Self>) {
        match std::mem::take(&mut self.press) {
            Press::Column {
                tail, moved: false, ..
            } => {
                if let Some(doc) = self.doc.as_mut() {
                    doc.editor.add_cursor(tail);
                }
                self.changed(window, cx);
            }
            Press::None => {}
            _ => cx.notify(),
        }
    }

    fn scroll_wheel(&mut self, e: &ScrollEvent, cx: &mut Context<'_, Self>) {
        let delta = e.delta.pixel_delta(px(self.metrics.line));
        let (mut dx, mut dy) = (f32::from(delta.x), f32::from(delta.y));
        if e.modifiers.shift && dx == 0.0 {
            (dx, dy) = (dy, 0.0);
        }
        self.scroll_y -= f64::from(dy);
        // Wrapped rows fit the column: no sideways scroll.
        if self.doc.as_ref().is_some_and(|d| d.editor.wrap().is_none()) {
            self.scroll_x -= dx;
        }
        cx.notify();
    }

    /// Moves the thumb so that the point it is held at is under `position`.
    fn drag_thumb(&mut self, position: Point<Pixels>) {
        let (Some(bar), Some(bounds), Some(rows)) = (
            self.scrollbar,
            self.bounds,
            self.row_map().map(RowMap::row_count),
        ) else {
            return;
        };
        let Press::Thumb { grab } = self.press else {
            return;
        };
        let room = f32::from(bar.track.size.height - bar.thumb.size.height);
        if room <= 0.0 {
            return;
        }
        let top = f32::from(position.y - bar.track.top()) - grab;
        let ratio = f64::from((top / room).clamp(0.0, 1.0));
        let max_y = (2.0 * f64::from(self.layout.pad_top()) + self.geo().total(rows)
            - f64::from(f32::from(bounds.size.height)))
        .max(0.0);
        self.scroll_y = ratio * max_y;
    }
}

/// The word, run of punctuation or run of spaces at `offset` (UAX #29
/// word boundaries within the line); at the end of a line, the segment
/// before it.
pub(crate) fn word_at(text: &TextSnapshot, offset: usize) -> Range<usize> {
    let Ok(line) = text.line_of_offset(offset) else {
        return offset..offset;
    };
    let Ok(range) = text.line_range(line) else {
        return offset..offset;
    };
    let content = text.text_for_range(range.clone()).unwrap_or_default();
    let column = offset.saturating_sub(range.start);
    let mut last = None;
    for (at, segment) in content.split_word_bound_indices() {
        let end = at + segment.len();
        if at <= column && column < end {
            return range.start + at..range.start + end;
        }
        last = Some(at..end);
    }
    match last {
        Some(r) if column >= content.len() => range.start + r.start..range.start + r.end,
        _ => offset..offset,
    }
}

/// The line at `offset` with its terminator.
pub(crate) fn line_at(text: &TextSnapshot, offset: usize) -> Range<usize> {
    text.line_of_offset(offset)
        .ok()
        .and_then(|line| text.line_range_with_terminator(line).ok())
        .unwrap_or(offset..offset)
}

/// Paints a broken line across `bounds` (see [`Line::Broken`]).
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

/// The buffer range of the terminator of visual line `visual`, if any.
fn a11y_terminator(text: &TextSnapshot, visual: &VisualLine) -> Option<Range<usize>> {
    let ending = text.line_terminator(visual.last).ok().flatten()?;
    Some(visual.range.end..visual.range.end + ending.byte_len())
}

/// The element of an [`EditorView`].
pub(crate) struct EditorElement {
    view: Entity<EditorView>,
}

impl EditorElement {
    pub fn new(view: Entity<EditorView>) -> Self {
        Self { view }
    }
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

pub(crate) struct Prepaint {
    hitbox: Hitbox,
    text_hitbox: Option<Hitbox>,
    /// Over the fold chevrons and chips.
    pointer_hitboxes: Vec<Hitbox>,
    frame: Frame,
    /// The views of the Live Preview blocks on screen, prepainted.
    blocks: Vec<gpui::AnyElement>,
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        Some("source-text".into())
    }

    fn source_location(&self) -> Option<&'static panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let frame = self
            .view
            .update(cx, |view, cx| view.frame(bounds, window, cx));
        let text_hitbox = frame
            .text_area
            .map(|area| window.insert_hitbox(area, HitboxBehavior::Normal));
        let pointer_hitboxes = frame
            .pointers
            .iter()
            .filter(|b| b.intersects(&bounds))
            .map(|b| window.insert_hitbox(b.intersect(&bounds), HitboxBehavior::Normal))
            .collect();
        // Live Preview blocks: their views laid out at their places, and
        // the heights they measure for the next frame.
        let clip = frame.text_clip.unwrap_or(bounds);
        let mut blocks = Vec::with_capacity(frame.blocks.len());
        let mut heights = Vec::with_capacity(frame.blocks.len());
        for block in &frame.blocks {
            let mut element = gpui::AnyView::from(block.reader.clone()).into_any_element();
            element.layout_as_root(
                size(
                    gpui::AvailableSpace::Definite(block.bounds.size.width),
                    gpui::AvailableSpace::Definite(block.bounds.size.height),
                ),
                window,
                cx,
            );
            window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                element.prepaint_at(block.bounds.origin, window, cx);
            });
            blocks.push(element);
            if let Some(height) = block.reader.read(cx).content_height() {
                heights.push((block.id, height));
            }
        }
        if !heights.is_empty() {
            self.view
                .update(cx, |view, cx| view.block_heights(heights, cx));
        }
        // The text's runs are in the view's shared description already;
        // the blocks among them go there too. The focused element around
        // this one hangs them on its node.
        if window.is_a11y_active() || cfg!(test) {
            let view = self.view.read(cx);
            let blocks = if frame.blocks.is_empty() {
                Vec::new()
            } else {
                view.blocks_a11y(cx)
            };
            view.a11y.borrow_mut().blocks = blocks;
        }
        Prepaint {
            hitbox,
            text_hitbox,
            pointer_hitboxes,
            frame,
            blocks,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let frame = &mut pre.frame;
        let row_h = frame.line;
        window.handle_input(
            &frame.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in frame.under.drain(..) {
                window.paint_quad(quad);
            }
            let clip = frame.text_clip.unwrap_or(bounds);
            window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                for quad in frame.chips.drain(..) {
                    window.paint_quad(quad);
                }
                for quad in frame.matches.drain(..) {
                    window.paint_quad(quad);
                }
                for quad in frame.selections.drain(..) {
                    window.paint_quad(quad);
                }
                for quad in frame.current_match.drain(..) {
                    window.paint_quad(quad);
                }
                for (line, origin) in &frame.rows {
                    // A line that cannot be painted (a full atlas) is skipped.
                    let _ = line.paint(*origin, px(row_h), TextAlign::Left, None, window, cx);
                }
                for line in frame.lines.drain(..) {
                    match line {
                        Line::Plain {
                            origin,
                            width,
                            color,
                            wavy,
                        } => window.paint_underline(
                            origin,
                            width,
                            &UnderlineStyle {
                                thickness: px(HAIRLINE),
                                color: Some(color),
                                wavy,
                            },
                        ),
                        Line::Broken {
                            bounds,
                            on,
                            off,
                            color,
                        } => paint_broken(window, bounds, on, off, color),
                    }
                }
                let text_font = gpui::font(crate::metrics::font::SYSTEM_FALLBACK);
                for widget in frame.widgets.drain(..) {
                    match widget {
                        Drawn::Image { bounds, image } => {
                            let _ = window.paint_image(
                                bounds,
                                bounds,
                                gpui::Corners::default(),
                                image,
                                0,
                                false,
                            );
                        }
                        Drawn::Math {
                            origin,
                            layout,
                            em,
                            color,
                        } => crate::inline::paint_math(
                            window, cx, &layout, origin, em, color, &text_font,
                        ),
                        Drawn::Quad(quad) => window.paint_quad(quad),
                    }
                }
                for element in &mut pre.blocks {
                    element.paint(window, cx);
                }
                for quad in frame.carets.drain(..) {
                    window.paint_quad(quad);
                }
            });
            for (number, origin) in &frame.numbers {
                let _ = number.paint(*origin, px(row_h), TextAlign::Left, None, window, cx);
            }
            for (bounds, path, color) in frame.icons.drain(..) {
                // An icon missing from the assets draws nothing.
                let _ =
                    window.paint_svg(bounds, path, None, TransformationMatrix::unit(), color, cx);
            }
            for tick in frame.ticks.drain(..) {
                window.paint_quad(tick);
            }
            if let Some(thumb) = frame.thumb.take() {
                window.paint_quad(thumb);
            }
        });
        if let Some(text_hitbox) = &pre.text_hitbox {
            window.set_cursor_style(CursorStyle::IBeam, text_hitbox);
        }
        for hitbox in &pre.pointer_hitboxes {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
        self.register_handlers(&pre.hitbox, window);
        crate::latency::painted(window, cx);
    }
}

impl EditorElement {
    fn register_handlers(&self, hitbox: &Hitbox, window: &mut Window) {
        {
            let view = self.view.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || e.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                // A Live Preview block: the caret goes into it, unless the
                // press is on one of its view's controls.
                if !e.modifiers.modified() {
                    let block = view.update(cx, |view, cx| view.press_block(e.position, cx));
                    match block {
                        Some(true) => {
                            let focus = view.read(cx).focus.clone();
                            focus.focus(window, cx);
                            cx.stop_propagation();
                            return;
                        }
                        Some(false) => return,
                        None => {}
                    }
                }
                view.update(cx, |view, cx| view.mouse_down(e, window, cx));
                cx.stop_propagation();
            });
        }
        // Right-click: the editor's context menu.
        {
            let view = self.view.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || e.button != MouseButton::Right
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let focus = view.read(cx).focus.clone();
                focus.focus(window, cx);
                // On a link: the link menu, about that link.
                let link = view.update(cx, |view, cx| view.menu_link_at(e.position, window, cx));
                if let Some(link) = link {
                    crate::reader::link_info::set_menu_link(Some(link), cx);
                    crate::menu::show_context_menu(
                        crate::menu::ContextMenu::Link,
                        e.position,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                    return;
                }
                crate::menu::show_context_menu(
                    crate::menu::ContextMenu::Editor,
                    e.position,
                    window,
                    cx,
                );
                // On a misspelled word: its suggestions.
                view.update(cx, |view, cx| {
                    view.attach_spell_menu(Some(e.position), window, cx);
                });
                cx.stop_propagation();
            });
        }
        {
            let view = self.view.clone();
            let hover_hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let pressed = view.read(cx).press != Press::None;
                if pressed {
                    view.update(cx, |view, cx| view.mouse_move(e, window, cx));
                } else if !e.dragging() {
                    // The link under the pointer, for its tooltip.
                    let at = hover_hitbox.is_hovered(window).then_some(e.position);
                    view.update(cx, |view, cx| view.hover_moved(at, window, cx));
                }
            });
        }
        // A middle click on a link: a background tab.
        {
            let view = self.view.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || e.button != MouseButton::Middle
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let followed = view.update(cx, |view, cx| {
                    view.click_link(
                        e.position,
                        crate::reader::Disposition::Background,
                        window,
                        cx,
                    )
                });
                if followed {
                    cx.stop_propagation();
                }
            });
        }
        {
            let view = self.view.clone();
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || e.button != MouseButton::Left {
                    return;
                }
                // A press a block's view took: the editor keeps the focus.
                if view.update(cx, |v, _| std::mem::take(&mut v.live.refocus)) {
                    let focus = view.read(cx).focus.clone();
                    focus.focus(window, cx);
                }
                let pressed = view.read(cx).press != Press::None;
                if pressed {
                    view.update(cx, |view, cx| view.mouse_up(e, window, cx));
                }
            });
        }
        {
            let view = self.view.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                    return;
                }
                view.update(cx, |view, cx| {
                    // A wide table or code block of Live Preview scrolls
                    // sideways itself.
                    if !view.wheel_block(e, cx) {
                        view.scroll_wheel(e, cx);
                    }
                });
                cx.stop_propagation();
            });
        }
    }
}

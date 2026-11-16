// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view's element: one element for the whole viewport.
//!
//! - **Prepaint** turns a scroll-sync request into the engine's target,
//!   runs the engine's frame pass for the text column, then hosts an
//!   [`InlineParagraph`] per visible text item (laid out as a root at the
//!   item's width and prepainted at its place) and records what to draw
//!   around them: the caret band (Split view), code block frames and
//!   heads, quote bars, list markers, rules, the ambiguity badges of links
//!   and the scrollbar.
//! - **Paint** draws the frames, the paragraphs (code clipped to its box),
//!   then the head buttons, badges and the scrollbar, and registers the
//!   pointer handlers: wheel scrolling (sideways inside code blocks),
//!   selection by dragging, link clicks, the code buttons and the
//!   scrollbar thumb.

use std::cell::RefCell;
use std::panic;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::assets::IconName;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ClipboardItem, ContentMask, Corners, CursorStyle,
    DispatchPhase, Edges, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollEvent, ShapedLine, SharedString, Style, TextAlign, TextRun,
    TransformationMatrix, WeakEntity, Window, fill, point, px, quad, relative, size,
};
use gv_inline::ItemKind as PieceKind;
use gv_render_tree::{FrameKind, ItemKind, ListMarker, StatusIcon, TextRole, Variant, style_id};

use super::bench::{load_line, percentiles};
use super::engine::{Engine, Items as _};
use super::follow::{Cause, begin as note_navigation};
use super::items::{
    DocItems, NO_WRAP, chunk_ends, code_inset, is_code, status_icon_path, text_top, text_width,
};
use super::live::{Spot, SyncRequest, block_items_at};
use super::state::{
    ButtonKind, CodeButton, CodeGeom, Geometry, Hover, LinkHit, PlacedGeom, Press, Selection, State,
};
use super::{Disposition, LinkRequest, Reader, ReaderEvent, find, startup};
use crate::icons::Icon;
use crate::inline::{InlineParagraph, Palette, Token};
use crate::metrics::size::{CHECKBOX, CODE_BUTTON, SCROLLBAR_MIN_THUMB, SCROLLBAR_THUMB, split};
use crate::metrics::spacing::{code_head, document as doc_spacing, inline, scrollbar};
use crate::metrics::{HAIRLINE, icon, radius, text};

/// Frames slower than this are logged while timing (one refresh at 120 Hz).
const SLOW_FRAME: std::time::Duration = std::time::Duration::from_micros(8_333);

/// Pointer travel (px) after which a press on a link selects instead of
/// following it.
const DRAG_THRESHOLD: f32 = 3.0;

/// Something drawn around the paragraphs.
enum Op {
    Rect {
        bounds: Bounds<Pixels>,
        color: Hsla,
        radius: Pixels,
    },
    Frame {
        bounds: Bounds<Pixels>,
        borders: Edges<Pixels>,
        corners: Corners<Pixels>,
        fill: Hsla,
        border: Hsla,
    },
    Text {
        line: Box<ShapedLine>,
        origin: Point<Pixels>,
        line_height: Pixels,
    },
    Icon {
        bounds: Bounds<Pixels>,
        path: SharedString,
        color: Hsla,
    },
}

impl Op {
    fn paint(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Rect {
                bounds,
                color,
                radius,
            } => window.paint_quad(fill(*bounds, *color).corner_radii(*radius)),
            Self::Frame {
                bounds,
                borders,
                corners,
                fill: background,
                border,
            } => window.paint_quad(quad(
                *bounds,
                *corners,
                *background,
                *borders,
                *border,
                gpui::BorderStyle::Solid,
            )),
            Self::Text {
                line,
                origin,
                line_height,
            } => {
                // A line that cannot be painted (a full atlas) is skipped.
                let _ = line.paint(*origin, *line_height, TextAlign::Left, None, window, cx);
            }
            Self::Icon {
                bounds,
                path,
                color,
            } => {
                // An icon missing from the assets draws nothing.
                let _ = window.paint_svg(
                    *bounds,
                    path.clone(),
                    None,
                    TransformationMatrix::unit(),
                    *color,
                    cx,
                );
            }
        }
    }
}

/// Where a list marker goes.
#[derive(Clone, Copy)]
enum MarkerAt {
    /// On the first line of the item's text: its baseline, top and height.
    Text {
        baseline: f32,
        top: f32,
        height: f32,
    },
    /// In a body line box with this top.
    LineTop(f32),
}

/// A hosted paragraph, clipped to `mask` if it has one.
struct Child {
    element: AnyElement,
    mask: Option<Bounds<Pixels>>,
}

pub(crate) struct Prepaint {
    hitbox: Hitbox,
    under: Vec<Op>,
    children: Vec<Child>,
    over: Vec<Op>,
}

/// Highlights code blocks on the background executor; as each lands, the
/// items drawn plain meanwhile are laid out again.
fn start_highlights(
    reader: WeakEntity<Reader>,
    requests: Vec<(gv_highlight::HighlightKey, Arc<gv_render_tree::CodeBlock>)>,
    cx: &mut App,
) {
    for (key, code) in requests {
        let reader = reader.clone();
        cx.spawn(async move |cx| {
            cx.background_executor()
                .spawn(async move {
                    super::code_highlight::highlight(key, &code.text);
                })
                .await;
            reader
                .update(cx, |reader, cx| {
                    let mut st = reader.state.borrow_mut();
                    let items = st.cache.code.landed(&key);
                    if !items.is_empty() {
                        st.stale.extend(items);
                        drop(st);
                        cx.notify();
                    }
                })
                .ok();
        })
        .detach();
    }
}

pub(crate) struct ReaderElement {
    state: Rc<RefCell<State>>,
    reader: WeakEntity<Reader>,
    focus: gpui::FocusHandle,
    palette: Arc<Palette>,
}

impl ReaderElement {
    pub fn new(
        state: Rc<RefCell<State>>,
        reader: WeakEntity<Reader>,
        focus: gpui::FocusHandle,
        palette: Arc<Palette>,
    ) -> Self {
        Self {
            state,
            reader,
            focus,
            palette,
        }
    }
}

impl IntoElement for ReaderElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

fn bounds_of(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(w.max(0.0)), px(h.max(0.0))))
}

/// Shapes a one-line label.
fn shape(window: &Window, text: &str, font: gpui::Font, size: f32, color: Hsla) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(SharedString::from(text.to_owned()), px(size), &[run], None)
}

/// The origin that puts `line`'s baseline at `baseline` when painted with
/// `line_height`.
fn origin_for_baseline(
    line: &ShapedLine,
    x: f32,
    baseline: f32,
    line_height: f32,
) -> Point<Pixels> {
    let (ascent, descent) = (f32::from(line.ascent), f32::from(line.descent));
    point(
        px(x),
        px(baseline - (line_height - ascent - descent) / 2.0 - ascent),
    )
}

/// Where a hosted paragraph goes: its origin, the width it is laid out at,
/// and the rectangle it is clipped to, if any.
struct Place {
    origin: Point<Pixels>,
    width: f32,
    mask: Option<Bounds<Pixels>>,
}

/// What every hosted paragraph shares.
struct Host<'a> {
    text: &'a Rc<RefCell<crate::inline::InlineText>>,
    palette: &'a Arc<Palette>,
}

impl Host<'_> {
    /// Lays `block` out as a root and prepaints it at its place.
    fn child(
        &self,
        block: &Rc<RefCell<crate::inline::InlineBlock>>,
        selection: Option<std::ops::Range<usize>>,
        highlights: Vec<(std::ops::Range<usize>, Token)>,
        place: Place,
        window: &mut Window,
        cx: &mut App,
    ) -> Child {
        let mut paragraph =
            InlineParagraph::new(block.clone(), self.text.clone(), self.palette.clone());
        if let Some(r) = selection {
            paragraph = paragraph.selection(r);
        }
        if !highlights.is_empty() {
            paragraph = paragraph.highlights(highlights);
        }
        let mut element = paragraph.into_any_element();
        element.layout_as_root(
            size(
                AvailableSpace::Definite(px(place.width)),
                AvailableSpace::MinContent,
            ),
            window,
            cx,
        );
        match place.mask {
            Some(m) => {
                window.with_content_mask(Some(ContentMask { bounds: m }), |window| {
                    element.prepaint_at(place.origin, window, cx);
                });
            }
            None => {
                element.prepaint_at(place.origin, window, cx);
            }
        }
        Child {
            element,
            mask: place.mask,
        }
    }
}

impl Element for ReaderElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

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
        #[cfg(test)]
        let _frame = super::code_highlight::tests::FrameGuard::new();
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let mut out = Prepaint {
            hitbox,
            under: Vec::new(),
            children: Vec::new(),
            over: Vec::new(),
        };
        let mut guard = self.state.borrow_mut();
        let st = &mut *guard;
        let ts = window.text_system().clone();
        if !st.stale.is_empty() {
            let stale = std::mem::take(&mut st.stale);
            st.changed(&stale, &ts);
        }
        st.geometry = Geometry {
            bounds,
            ..Geometry::default()
        };
        // The text column: Preview's centred column, or the Split pane's.
        let (x0, column) = st.layout.column(bounds);
        find::reveal_current(st, column, &ts);
        let State {
            doc,
            engine,
            cache,
            text,
            selection,
            hscroll,
            geometry,
            hover,
            find: marks,
            doc_version,
            sync,
            band,
            ..
        } = st;
        let Some(doc) = doc.as_ref() else {
            return out;
        };
        let tree = &doc.tree;
        let m = tree.options().metrics;
        let styles = &tree.options().styles;
        let palette = &self.palette;
        let color = |t: Token| palette.color(t);
        let host_text = text.clone();
        let host = Host {
            text: &host_text,
            palette,
        };

        geometry.column = (x0, column);
        // Find matches are painted while they belong to this document.
        let marks = (marks.version == *doc_version && !marks.matches.is_empty()).then_some(&*marks);

        {
            let mut items = DocItems {
                tree,
                cache,
                text,
                ts: &ts,
            };
            if let Some(request) = sync.take() {
                resolve_sync(engine, &mut items, request, column);
            }
            engine.frame(&mut items, column, f32::from(bounds.size.height));
        }
        let placed = engine.placed().to_vec();
        let band_items = band.and_then(|offset| block_items_at(tree, offset));
        let selected = selection
            .map(|s: Selection| s.range())
            .filter(|r| !r.is_empty());
        let font = |role: TextRole| {
            text.borrow()
                .fonts()
                .font(style_id(role, Variant::PLAIN))
                .clone()
        };
        let (body_font, label_font, badge_font) = (
            font(TextRole::Body),
            font(TextRole::CodeLabel),
            font(TextRole::Badge),
        );
        let body = styles.role(TextRole::Body);
        let label_spec = styles.role(TextRole::CodeLabel);

        for p in &placed {
            let item = &tree.items()[p.ix];
            let Some(node) = tree.node_of(p.ix) else {
                continue;
            };
            let top = f32::from(bounds.top()) + p.y;
            let bottom = top + p.height;
            let content_top = top + item.gap_before;
            let indent = item.content_x(&m);
            let left = x0 + indent;
            let content_w = (column - indent).max(1.0);
            let local_selection = selected.as_ref().and_then(|r| {
                let a = r.start.saturating_sub(node.source.start);
                let b = r.end.saturating_sub(node.source.start);
                (b > a).then_some(a..b)
            });
            let highlights = marks
                .map(|m| m.highlights(p.ix, node.source.start))
                .unwrap_or_default();

            // The caret band: the block's content and 2 px more above and
            // below, 6 px beyond it on each side.
            if let Some(band) = band_items.as_ref().filter(|r| r.contains(&p.ix)) {
                let (first, last) = (p.ix == band.start, p.ix + 1 == band.end);
                let (r, zero) = (px(radius::CHIP), px(0.0));
                let y0 = if first {
                    content_top - split::CARET_BAND_PAD_Y
                } else {
                    top
                };
                let y1 = if last {
                    bottom + split::CARET_BAND_PAD_Y
                } else {
                    bottom
                };
                out.under.push(Op::Frame {
                    bounds: bounds_of(
                        left - split::CARET_BAND_PAD_X,
                        y0,
                        content_w + 2.0 * split::CARET_BAND_PAD_X,
                        y1 - y0,
                    ),
                    borders: Edges::default(),
                    corners: Corners {
                        top_left: if first { r } else { zero },
                        top_right: if first { r } else { zero },
                        bottom_right: if last { r } else { zero },
                        bottom_left: if last { r } else { zero },
                    },
                    fill: color(Token::StateHover),
                    border: color(Token::StateHover),
                });
            }

            // Quote bars, and the markers of list items that start here.
            let mut markers = Vec::new();
            for (i, frame) in item.context.iter().enumerate() {
                let fx = x0 + item.frame_x(i, &m);
                match frame.kind {
                    FrameKind::Quote { .. } => {
                        let y0 = if item.opens_frame(i) {
                            content_top
                        } else {
                            top
                        };
                        out.under.push(Op::Rect {
                            bounds: bounds_of(fx, y0, m.quote_bar, bottom - y0),
                            color: color(Token::BorderStrong),
                            radius: px(0.0),
                        });
                    }
                    FrameKind::Item { marker } if item.opens_frame(i) => {
                        markers.push((fx + m.list_indent, marker));
                    }
                    _ => {}
                }
            }

            let block = cache.block(tree, p.ix);
            let mut geom = PlacedGeom {
                ix: p.ix,
                top,
                bottom,
                text: block.clone(),
                node_start: node.source.start,
                code: None,
            };
            // Where list markers sit: on the first text line's baseline, or
            // in a body line box at the top of other content.
            let mut marker_at = MarkerAt::LineTop(content_top);

            match &item.kind {
                ItemKind::ThematicBreak => out.under.push(Op::Rect {
                    bounds: bounds_of(left, content_top, content_w, m.rule_height),
                    color: color(Token::BorderDefault),
                    radius: px(0.0),
                }),
                // A table row: the table's frame (1 px `border.default`,
                // radius 6), the header on `surface.sunken`, a line above
                // each body row. The table is as wide as its columns, at
                // most the column; wider tables scroll sideways as a whole.
                ItemKind::TableRow { row, .. } => {
                    let (first, last) = chunk_ends(&item.kind);
                    let bw = m.table_border;
                    let table_w = cache
                        .table(item.id.block)
                        .map(|t| t.width(m.table_padding_x) + 2.0 * bw);
                    let frame_w = table_w.map_or(content_w, |w| w.min(content_w));
                    let (r, zero) = (px(radius::BLOCK), px(0.0));
                    let fill = if *row == 0 {
                        color(Token::SurfaceSunken)
                    } else {
                        color(Token::BorderDefault).opacity(0.0)
                    };
                    out.under.push(Op::Frame {
                        bounds: bounds_of(left, content_top, frame_w, bottom - content_top),
                        borders: Edges {
                            top: if first { px(bw) } else { zero },
                            right: px(bw),
                            bottom: if last { px(bw) } else { zero },
                            left: px(bw),
                        },
                        corners: Corners {
                            top_left: if first { r } else { zero },
                            top_right: if first { r } else { zero },
                            bottom_right: if last { r } else { zero },
                            bottom_left: if last { r } else { zero },
                        },
                        fill,
                        border: color(Token::BorderDefault),
                    });
                    if !first {
                        out.under.push(Op::Rect {
                            bounds: bounds_of(left + bw, content_top, frame_w - 2.0 * bw, bw),
                            color: color(Token::BorderDefault),
                            radius: px(0.0),
                        });
                    }
                    if let Some(block) = &block {
                        let layout_width = text
                            .borrow_mut()
                            .layout(&mut block.borrow_mut(), NO_WRAP, &ts)
                            .width();
                        let inner = (frame_w - 2.0 * bw).max(1.0);
                        let wide = table_w.map_or(layout_width, |w| w - 2.0 * bw);
                        let overflow = (wide - inner).max(0.0);
                        let scroll = hscroll
                            .get(&item.id.block)
                            .copied()
                            .unwrap_or(0.0)
                            .clamp(0.0, overflow);
                        let clip_top = content_top + bw;
                        let clip_bottom = if last { bottom - bw } else { bottom };
                        let clip = bounds_of(left + bw, clip_top, inner, clip_bottom - clip_top);
                        let origin = point(px(left + bw - scroll), px(top + text_top(item, &m)));
                        out.children.push(host.child(
                            block,
                            local_selection.clone(),
                            highlights.clone(),
                            Place {
                                origin,
                                width: NO_WRAP,
                                mask: Some(clip),
                            },
                            window,
                            cx,
                        ));
                        geom.code = Some(CodeGeom {
                            block: item.id.block,
                            clip,
                            overflow,
                        });
                    }
                }
                kind if is_code(kind) => {
                    let (first, last) = chunk_ends(kind);
                    let r = px(radius::BLOCK);
                    let zero = px(0.0);
                    out.under.push(Op::Frame {
                        bounds: bounds_of(left, content_top, content_w, bottom - content_top),
                        borders: Edges {
                            top: if first { px(m.code_border) } else { zero },
                            right: px(m.code_border),
                            bottom: if last { px(m.code_border) } else { zero },
                            left: px(m.code_border),
                        },
                        corners: Corners {
                            top_left: if first { r } else { zero },
                            top_right: if first { r } else { zero },
                            bottom_right: if last { r } else { zero },
                            bottom_left: if last { r } else { zero },
                        },
                        fill: color(Token::SurfaceSunken),
                        border: color(Token::BorderDefault),
                    });
                    let mut clip_top = content_top;
                    if first {
                        let head_top = content_top + m.code_border;
                        let head_h = m.code_head - m.code_border;
                        clip_top = head_top + m.code_head;
                        marker_at = MarkerAt::LineTop(head_top + (head_h - body.line_height) / 2.0);
                        // The separator under the head.
                        out.under.push(Op::Rect {
                            bounds: bounds_of(
                                left + m.code_border,
                                head_top + head_h,
                                content_w - 2.0 * m.code_border,
                                m.code_border,
                            ),
                            color: color(Token::BorderDefault),
                            radius: px(0.0),
                        });
                        // The language, or HTML for raw HTML shown as code.
                        let label: Option<String> = match kind {
                            ItemKind::Html { .. } => Some("html".to_owned()),
                            _ => node
                                .lines()
                                .and_then(|c| c.language.as_deref().map(str::to_owned)),
                        };
                        if let Some(label) = label.filter(|l| !l.is_empty()) {
                            let line = shape(
                                window,
                                &label,
                                label_font.clone(),
                                label_spec.size,
                                color(Token::TextSecondary),
                            );
                            out.over.push(Op::Text {
                                line: Box::new(line),
                                origin: point(
                                    px(left + code_inset(&m)),
                                    px(head_top + (head_h - label_spec.line_height) / 2.0),
                                ),
                                line_height: px(label_spec.line_height),
                            });
                        }
                        // Wrap and copy, from the right.
                        let y = head_top + (head_h - CODE_BUTTON) / 2.0;
                        let mut x = left + content_w - m.code_border - code_head::PAD_RIGHT;
                        for kind in [ButtonKind::Copy, ButtonKind::Wrap] {
                            x -= CODE_BUTTON;
                            let button = CodeButton {
                                bounds: bounds_of(x, y, CODE_BUTTON, CODE_BUTTON),
                                block: item.id.block,
                                node: item.node,
                                kind,
                            };
                            let active = kind == ButtonKind::Wrap && tree.code_wrap(item.id.block);
                            let hovered = *hover == Hover::Button(button);
                            if active || hovered {
                                out.over.push(Op::Rect {
                                    bounds: button.bounds,
                                    color: color(if active {
                                        Token::AccentTint
                                    } else {
                                        Token::StateHover
                                    }),
                                    radius: px(radius::SMALL_CONTROL),
                                });
                            }
                            let icon_name = match kind {
                                ButtonKind::Copy => Icon::CopyCode,
                                ButtonKind::Wrap => Icon::WrapCode,
                            };
                            out.over.push(Op::Icon {
                                bounds: Bounds::centered_at(
                                    button.bounds.center(),
                                    size(px(icon::INLINE), px(icon::INLINE)),
                                ),
                                path: icon_name.name().path(),
                                color: color(Token::TextSecondary),
                            });
                            geometry.buttons.push(button);
                            x -= code_head::GAP;
                        }
                    }
                    if let Some(block) = &block {
                        let is_wrapped = tree.code_wrap(item.id.block);
                        let tw = text_width(item, column, &m, is_wrapped);
                        let layout_width = text
                            .borrow_mut()
                            .layout(&mut block.borrow_mut(), tw, &ts)
                            .width();
                        let inner = content_w - 2.0 * code_inset(&m);
                        let overflow = if is_wrapped || tw < NO_WRAP {
                            0.0
                        } else {
                            (layout_width - inner).max(0.0)
                        };
                        let scroll = hscroll
                            .get(&item.id.block)
                            .copied()
                            .unwrap_or(0.0)
                            .clamp(0.0, overflow);
                        let clip_bottom = if last { bottom - m.code_border } else { bottom };
                        let clip = bounds_of(
                            left + m.code_border,
                            clip_top,
                            content_w - 2.0 * m.code_border,
                            clip_bottom - clip_top,
                        );
                        let origin = point(
                            px(left + code_inset(&m) - scroll),
                            px(top + text_top(item, &m)),
                        );
                        out.children.push(host.child(
                            block,
                            local_selection.clone(),
                            highlights.clone(),
                            Place {
                                origin,
                                width: tw,
                                mask: Some(clip),
                            },
                            window,
                            cx,
                        ));
                        geom.code = Some(CodeGeom {
                            block: item.id.block,
                            clip,
                            overflow,
                        });
                    }
                }
                _ => {
                    if let Some(block) = &block {
                        let tw = text_width(item, column, &m, false);
                        let first = text
                            .borrow_mut()
                            .layout(&mut block.borrow_mut(), tw, &ts)
                            .lines()
                            .first()
                            .map(|l| (l.baseline, l.top, l.height));
                        if let Some((baseline, top, height)) = first {
                            marker_at = MarkerAt::Text {
                                baseline: content_top + baseline,
                                top: content_top + top,
                                height,
                            };
                        }
                        out.children.push(host.child(
                            block,
                            local_selection.clone(),
                            highlights.clone(),
                            Place {
                                origin: point(px(left), px(content_top)),
                                width: tw,
                                mask: None,
                            },
                            window,
                            cx,
                        ));
                    }
                }
            }

            // List markers, right-aligned before the content.
            for (right, marker) in markers {
                if let ListMarker::Task { checked, .. } = marker {
                    let side = CHECKBOX;
                    let x = right - inline::CHECKBOX_GAP - side;
                    let y = match marker_at {
                        MarkerAt::Text { top, height, .. } => top + (height - side) / 2.0,
                        MarkerAt::LineTop(y) => y + (body.line_height - side) / 2.0,
                    };
                    let boxed = bounds_of(x, y, side, side);
                    out.over.push(Op::Frame {
                        bounds: boxed,
                        borders: if checked {
                            Edges::default()
                        } else {
                            Edges::all(px(HAIRLINE))
                        },
                        corners: Corners::all(px(radius::SMALL_CONTROL)),
                        fill: color(if checked {
                            Token::AccentDefault
                        } else {
                            Token::SurfaceRaised
                        }),
                        border: color(Token::BorderStrong),
                    });
                    if checked {
                        out.over.push(Op::Icon {
                            bounds: boxed,
                            path: IconName::Check.path(),
                            color: color(Token::AccentOn),
                        });
                    }
                    continue;
                }
                let line = shape(
                    window,
                    &marker.text(),
                    body_font.clone(),
                    body.size,
                    color(Token::TextPrimary),
                );
                let x = right - doc_spacing::LIST_MARKER_GAP - f32::from(line.width());
                let origin = match marker_at {
                    MarkerAt::Text { baseline, .. } => {
                        origin_for_baseline(&line, x, baseline, body.line_height)
                    }
                    MarkerAt::LineTop(y) => point(px(x), px(y)),
                };
                out.over.push(Op::Text {
                    line: Box::new(line),
                    origin,
                    line_height: px(body.line_height),
                });
            }
            geometry.items.push(geom);
        }

        // Ambiguity badges: the warning icon and the candidate count in the
        // blank box after an ambiguous link.
        for g in &geometry.items {
            let Some(block) = &g.text else {
                continue;
            };
            let badges = {
                let content = tree.item_inline(g.ix).cloned();
                content.map_or_else(Vec::new, |c| {
                    c.links
                        .iter()
                        .enumerate()
                        .filter_map(|(i, l)| match l.status {
                            gv_render_tree::LinkStatus::Ambiguous(n) => {
                                Some((u32::try_from(i).ok()?, n))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                })
            };
            if badges.is_empty() {
                continue;
            }
            let b = block.borrow();
            let (Some(layout), Some(origin)) = (b.layout(), b.bounds().map(|r| r.origin)) else {
                continue;
            };
            let spans = tree.item_inline(g.ix).cloned();
            let Some(spans) = spans else {
                continue;
            };
            for (link, count) in badges {
                let Some(piece) = spans.spans.iter().position(|s| {
                    s.role == gv_render_tree::InlineRole::LinkSuffix && s.link == Some(link)
                }) else {
                    continue;
                };
                let Some(frag) = layout.fragments().iter().find(|f| f.item == piece) else {
                    continue;
                };
                if !matches!(layout.paragraph().items()[piece].kind(), PieceKind::Box(_)) {
                    continue;
                }
                let Some(line) = layout.lines().iter().find(|l| {
                    let k = layout.fragments().iter().position(|f| f.item == piece);
                    k.is_some_and(|k| l.frags.contains(&k))
                }) else {
                    continue;
                };
                let icon_size = StatusIcon::TriangleAlert.size() * styles.zoom();
                let x = f32::from(origin.x) + frag.x;
                let y = f32::from(origin.y) + line.baseline - frag.ascent;
                out.over.push(Op::Icon {
                    bounds: bounds_of(x, y, icon_size, icon_size),
                    path: status_icon_path(StatusIcon::TriangleAlert),
                    color: color(Token::StatusWarning),
                });
                let badge = styles.role(TextRole::Badge);
                let digits = shape(
                    window,
                    &count.to_string(),
                    badge_font.clone(),
                    badge.size,
                    color(Token::StatusWarning),
                );
                let origin = origin_for_baseline(
                    &digits,
                    x + icon_size + inline::STATUS_COUNT_GAP,
                    f32::from(origin.y) + line.baseline,
                    badge.line_height,
                );
                out.over.push(Op::Text {
                    line: Box::new(digits),
                    origin,
                    line_height: px(badge.line_height),
                });
            }
        }
        cache.end_frame();
        let requests = cache.code.take_requests();
        if !requests.is_empty() {
            start_highlights(self.reader.clone(), requests, cx);
        }

        // The scrollbar: a thumb, no track.
        let track_top = f32::from(bounds.top()) + scrollbar::INSET_Y;
        let track_h = f32::from(bounds.size.height) - 2.0 * scrollbar::INSET_Y;
        if let Some(thumb) = engine.thumb(track_h, SCROLLBAR_MIN_THUMB) {
            let right = f32::from(bounds.right());
            let x = right - scrollbar::INSET_X - SCROLLBAR_THUMB;
            let rect = bounds_of(x, track_top + thumb.top, SCROLLBAR_THUMB, thumb.height);
            out.over.push(Op::Rect {
                bounds: rect,
                color: color(Token::BorderStrong),
                radius: px(radius::SCROLLBAR),
            });
            let zone = 2.0 * scrollbar::INSET_X + SCROLLBAR_THUMB;
            geometry.thumb = Some(rect);
            geometry.track = Some(bounds_of(right - zone, track_top, zone, track_h));
        }
        out
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
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for op in &pre.under {
                op.paint(window, cx);
            }
            for child in &mut pre.children {
                match child.mask {
                    Some(mask) => {
                        window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                            child.element.paint(window, cx);
                        });
                    }
                    None => child.element.paint(window, cx),
                }
            }
            for op in &pre.over {
                op.paint(window, cx);
            }
        });

        // The pointer: what it is over decides the cursor.
        {
            let mut st = self.state.borrow_mut();
            let hover = if pre.hitbox.is_hovered(window) {
                st.hover_at(window.mouse_position())
            } else {
                Hover::None
            };
            let dragging_text = matches!(st.press, Press::Select { moved: true, .. });
            let cursor = match &hover {
                _ if dragging_text => Some(CursorStyle::IBeam),
                Hover::Link(_) | Hover::Button(_) => Some(CursorStyle::PointingHand),
                Hover::Text => Some(CursorStyle::IBeam),
                Hover::Scrollbar | Hover::None => None,
            };
            if let Some(cursor) = cursor {
                window.set_cursor_style(cursor, &pre.hitbox);
            }
            st.hover = hover;
        }
        self.register_handlers(&pre.hitbox, window);
        self.finish_frame(window, cx);
    }
}

impl ReaderElement {
    fn register_handlers(&self, hitbox: &Hitbox, window: &mut Window) {
        let notify = {
            let reader = self.reader.clone();
            move |cx: &mut App| {
                let _ = reader.update(cx, |_, cx| cx.notify());
            }
        };

        // Wheel: vertical scrolling, sideways inside code that overflows.
        {
            let state = self.state.clone();
            let hitbox = hitbox.clone();
            let notify = notify.clone();
            window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let d = e.delta.pixel_delta(px(text::PREVIEW_BODY.line));
                let (mut dx, dy) = (f32::from(d.x), f32::from(d.y));
                if e.modifiers.shift && dx == 0.0 {
                    dx = dy;
                }
                {
                    let mut st = state.borrow_mut();
                    match st.code_at(e.position) {
                        Some(code) if dx.abs() > dy.abs() || e.modifiers.shift => {
                            st.scroll_code(code, -dx);
                        }
                        _ => st.engine.scroll_by(-dy),
                    }
                }
                cx.stop_propagation();
                notify(cx);
            });
        }

        // Right-click on a link: the link menu.
        {
            let state = self.state.clone();
            let hitbox = hitbox.clone();
            let focus = self.focus.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || e.button != MouseButton::Right
                    || !hitbox.is_hovered(window)
                    || state.borrow().link_at(e.position).is_none()
                {
                    return;
                }
                focus.focus(window, cx);
                crate::menu::show_context_menu(
                    crate::menu::ContextMenu::Link,
                    e.position,
                    window,
                    cx,
                );
                cx.stop_propagation();
            });
        }

        // Presses.
        {
            let state = self.state.clone();
            let hitbox = hitbox.clone();
            let focus = self.focus.clone();
            let notify = notify.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                let mut st = state.borrow_mut();
                match e.button {
                    MouseButton::Left => {
                        focus.focus(window, cx);
                        press_left(&mut st, e);
                    }
                    MouseButton::Middle => {
                        if let Some(link) = st.link_at(e.position) {
                            st.press = Press::Middle(link);
                        }
                    }
                    _ => return,
                }
                drop(st);
                cx.stop_propagation();
                notify(cx);
            });
        }

        // Moves: drags, and the pointer's target for the cursor.
        {
            let state = self.state.clone();
            let hitbox = hitbox.clone();
            let notify = notify.clone();
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let mut st = state.borrow_mut();
                let changed = match st.press.clone() {
                    Press::Thumb { grab } if e.dragging() => {
                        drag_thumb(&mut st, e.position, grab);
                        true
                    }
                    Press::Select {
                        origin,
                        moved,
                        link,
                    } if e.dragging() => {
                        let far = (e.position.x - origin.x).abs() > px(DRAG_THRESHOLD)
                            || (e.position.y - origin.y).abs() > px(DRAG_THRESHOLD);
                        let moved = moved || far;
                        st.press = Press::Select {
                            origin,
                            moved,
                            link: if moved { None } else { link },
                        };
                        if moved {
                            extend_selection(&mut st, e.position);
                        }
                        moved
                    }
                    _ => {
                        let hover = if hitbox.is_hovered(window) {
                            st.hover_at(e.position)
                        } else {
                            Hover::None
                        };
                        hover != st.hover
                    }
                };
                drop(st);
                if changed {
                    notify(cx);
                }
            });
        }

        // Releases: clicks on links and buttons, the end of drags.
        {
            let state = self.state.clone();
            let reader = self.reader.clone();
            let notify = notify.clone();
            window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let mut st = state.borrow_mut();
                let press = std::mem::take(&mut st.press);
                let mut follow = None;
                match (press, e.button) {
                    (Press::None, _) => return,
                    (Press::Thumb { .. }, MouseButton::Left) => st.engine.end_drag(),
                    (Press::Button(button), MouseButton::Left) => {
                        if st.button_at(e.position) == Some(button) {
                            match button.kind {
                                ButtonKind::Copy => {
                                    if let Some(code) = st.code_text(button.node) {
                                        cx.write_to_clipboard(ClipboardItem::new_string(code));
                                    }
                                }
                                ButtonKind::Wrap => {
                                    let ts = window.text_system().clone();
                                    st.toggle_wrap(button.block, &ts);
                                }
                            }
                        }
                    }
                    (Press::Select { moved, link, .. }, MouseButton::Left) => {
                        if let Some(link) = link.filter(|_| !moved)
                            && st.link_at(e.position).is_some_and(|l| same_link(&l, &link))
                        {
                            st.selection = None;
                            let disposition = if e.modifiers.secondary(cx) {
                                Disposition::NewTab
                            } else {
                                Disposition::InPlace
                            };
                            follow = Some(request(&link, disposition));
                        }
                    }
                    (Press::Middle(link), MouseButton::Middle) => {
                        if st.link_at(e.position).is_some_and(|l| same_link(&l, &link)) {
                            follow = Some(request(&link, Disposition::Background));
                        }
                    }
                    (other, _) => {
                        st.press = other;
                        return;
                    }
                }
                drop(st);
                if let Some(req) = follow {
                    note_navigation(Cause::Link {
                        destination: req.destination.clone(),
                        background: req.disposition == Disposition::Background,
                    });
                    let _ = reader.update(cx, |_, cx| cx.emit(ReaderEvent::FollowLink(req)));
                }
                notify(cx);
            });
        }
    }

    /// Frame bookkeeping: the first frame with content, frame timing and
    /// the debug benchmark.
    fn finish_frame(&self, window: &mut Window, cx: &mut App) {
        let mut st = self.state.borrow_mut();
        let has_content = !st.geometry.items.is_empty();
        if has_content
            && st.timing.first_content.is_none()
            && let Some(opened) = st.timing.opened
        {
            let elapsed = opened.elapsed();
            st.timing.first_content = Some(elapsed);
            if let Some(doc) = &st.doc {
                tracing::info!(
                    path = %doc.path.display(),
                    items = doc.tree.items().len(),
                    "first frame with content after {:.1} ms; {}",
                    elapsed.as_secs_f64() * 1e3,
                    load_line(&doc.timings)
                );
                if startup::enabled() {
                    let name = doc
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let what = format!("{name}, {} items", doc.tree.items().len());
                    startup::first_content_painted(&what, window, cx);
                }
            }
        }
        if has_content
            && let Some(nav) = st.timing.navigation.take()
            && let Some(doc) = &st.doc
        {
            tracing::info!("{}", nav.line(&doc.path, Some(nav.at.elapsed())));
        }
        if let Some(start) = st.timing.frame_start.take()
            && st.timing.record
        {
            let took = start.elapsed();
            if took > SLOW_FRAME {
                let first = st.geometry.items.first().map(|g| g.ix);
                let last = st.geometry.items.last().map(|g| g.ix);
                tracing::debug!(
                    scroll_top = st.engine.scroll_top(),
                    ?first,
                    ?last,
                    "slow reader frame: {:.2} ms",
                    took.as_secs_f64() * 1e3
                );
            }
            st.timing.frames.push(took);
        }
        if st.timing.first_content.is_none() {
            return;
        }
        let Some(mut bench) = st.bench.take() else {
            return;
        };
        let over = bench.tick();
        if bench.timing() && !st.timing.record {
            st.timing.record = true;
            st.timing.frames.clear();
        }
        if over {
            let first = st.timing.first_content.unwrap_or_default();
            let lines = st
                .doc
                .as_ref()
                .map(|d| {
                    (
                        d.path.display().to_string(),
                        d.tree.items().len(),
                        load_line(&d.timings),
                    )
                })
                .unwrap_or_default();
            eprintln!(
                "reader bench: {} ({} items; {})\n  open request to first frame with content: {:.1} ms\n  scroll {} px/frame: frame CPU {}",
                lines.0,
                lines.1,
                lines.2,
                first.as_secs_f64() * 1e3,
                bench.step,
                percentiles(&st.timing.frames),
            );
            st.timing.record = false;
            cx.quit();
            return;
        }
        st.engine.scroll_by(bench.step);
        st.bench = Some(bench);
        st.timing.frame_start = Some(Instant::now());
        window.request_animation_frame();
    }
}

/// Turns scroll sync's request into the engine's target: the place in
/// the item, measured at the column's width, and the caret's block to
/// show whole.
fn resolve_sync(engine: &mut Engine, items: &mut DocItems<'_>, request: SyncRequest, column: f32) {
    let tree = items.tree;
    let n = tree.items().len();
    if n == 0 {
        return;
    }
    let ix = request.ix.min(n - 1);
    let gap = tree.items()[ix].gap_before;
    let offset = match request.spot {
        Spot::Lead(f) => f.clamp(-1.0, 0.0) * engine.lead(),
        Spot::Gap(g) => g.clamp(0.0, 1.0) * gap,
        Spot::Content(c) => {
            let height = items.measure(ix, column);
            gap + c.clamp(0.0, 1.0) * (height - gap).max(0.0)
        }
    };
    let reveal = request.reveal.and_then(|o| block_items_at(tree, o));
    engine.sync_to(items, ix, offset, reveal, split::CARET_BAND_PAD_Y);
}

fn same_link(a: &LinkHit, b: &LinkHit) -> bool {
    a.item == b.item && a.link == b.link
}

fn request(link: &LinkHit, disposition: Disposition) -> LinkRequest {
    LinkRequest {
        destination: link.destination.clone(),
        kind: link.kind,
        disposition,
    }
}

/// A left press: the scrollbar, a code button, or the start of a
/// selection (or a link click).
fn press_left(st: &mut State, e: &MouseDownEvent) {
    let pos = e.position;
    if let (Some(track), Some(thumb)) = (st.geometry.track, st.geometry.thumb)
        && track.contains(&pos)
    {
        let grab = if thumb.contains(&point(thumb.center().x, pos.y)) {
            f32::from(pos.y - thumb.top())
        } else {
            // A press beside the thumb jumps there, centred on the pointer.
            f32::from(thumb.size.height) / 2.0
        };
        st.engine.begin_drag();
        st.press = Press::Thumb { grab };
        drag_thumb(st, pos, grab);
        return;
    }
    if let Some(button) = st.button_at(pos) {
        st.press = Press::Button(button);
        return;
    }
    let Some(offset) = st.offset_at(pos) else {
        return;
    };
    let link = if e.modifiers.shift {
        None
    } else {
        st.link_at(pos)
    };
    st.selection = match (st.selection, e.modifiers.shift) {
        (Some(s), true) => Some(Selection {
            anchor: s.anchor,
            head: offset,
        }),
        _ => Some(Selection {
            anchor: offset,
            head: offset,
        }),
    };
    st.press = Press::Select {
        origin: pos,
        moved: e.modifiers.shift,
        link,
    };
}

fn drag_thumb(st: &mut State, pos: Point<Pixels>, grab: f32) {
    let (Some(track), Some(thumb)) = (st.geometry.track, st.geometry.thumb) else {
        return;
    };
    let top = f32::from(pos.y - track.top()) - grab;
    st.engine.drag_thumb_to(
        top,
        f32::from(track.size.height),
        f32::from(thumb.size.height),
    );
}

/// Moves the selection's head to the pointer; beyond the top or bottom
/// edge the view scrolls toward it.
fn extend_selection(st: &mut State, pos: Point<Pixels>) {
    let bounds = st.geometry.bounds;
    let above = f32::from(bounds.top() - pos.y);
    let below = f32::from(pos.y - bounds.bottom());
    if above > 0.0 {
        st.engine.scroll_by(-above);
    } else if below > 0.0 {
        st.engine.scroll_by(below);
    }
    if let Some(offset) = st.offset_at(pos)
        && let Some(s) = &mut st.selection
    {
        s.head = offset;
    }
}

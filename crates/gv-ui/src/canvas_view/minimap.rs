// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The minimap (Should, 08 § Canvas view): an overview in the bottom right
//! corner with every group and card as a shape and the part the view
//! shows as a rectangle. A click centres the view there (animated unless
//! the system asks for reduced motion), a drag moves the view along. The
//! map holds still from the press to the release (only its view rectangle
//! moves), so moving the view does not move the map under the pointer.
//!
//! It is shown only when part of the canvas is out of view and the view
//! is big enough for it ([`MinimapPolicy`]); the plan names no command to
//! toggle it.
//!
//! Not designed (built plainly, flagged): the box, its shapes and the
//! viewport rectangle use existing tokens (`surface.raised`,
//! `border.default`, `accent.default`).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, ContentMask, Context, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, fill, point, px, size,
};
use gv_canvas::{Minimap, MinimapPolicy, NodeKind, Size, Transform};

use super::paint::Colors;
use super::{CanvasView, Doc};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};

/// The map's size, in px.
pub(crate) const MAP_SIZE: Size = Size::new(200.0, 140.0);
/// Margin inside the map, in px.
const MAP_PAD: f64 = 8.0;
/// Distance from the view's right edge, in px.
const MAP_RIGHT: f32 = 8.0;
/// Distance from the view's bottom edge (above the zoom readout), in px.
const MAP_BOTTOM: f32 = 40.0;

/// The minimap of the last render and where it was painted.
#[derive(Debug, Clone, Default)]
pub(crate) struct MinimapState {
    /// The layout drawn in the last render, `None` when hidden.
    pub map: Option<Minimap>,
    /// The map's window bounds, from the last paint.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// A press on the map is being dragged.
    pub dragging: bool,
    /// The layout at the press, used until the release.
    pub frozen: Option<Minimap>,
}

impl CanvasView {
    /// Whether the minimap is drawn (tests).
    #[cfg(test)]
    pub(crate) fn minimap_shown(&self) -> bool {
        self.minimap.map.is_some()
    }

    /// The minimap's layout of the last render (tests).
    #[cfg(test)]
    pub(crate) fn minimap_layout(&self) -> Option<Minimap> {
        self.minimap.map
    }

    /// Lays out the minimap for `doc` at the current transform; `None`
    /// when it is not worth showing.
    pub(crate) fn layout_minimap(&mut self, doc: &Doc) -> Option<Minimap> {
        let viewport = self.viewport();
        let t = self.transform;
        if self.minimap.dragging
            && let Some(frozen) = self.minimap.frozen
        {
            let mut map = frozen;
            map.viewport = frozen.rect(&t.visible_world(viewport));
            self.minimap.map = Some(map);
            return self.minimap.map;
        }
        let content = doc.layout.bounds();
        let visible = t.visible_world(viewport);
        self.minimap.map = MinimapPolicy::default()
            .shows(content, &visible, viewport)
            .then(|| content.map(|c| Minimap::new(c, &t, viewport, MAP_SIZE, MAP_PAD)))
            .flatten();
        self.minimap.map
    }

    /// The transform that centres the view on the world point under
    /// window point `at` of the map.
    fn minimap_target(&self, at: gpui::Point<Pixels>) -> Option<Transform> {
        let map = self.minimap.frozen.or(self.minimap.map)?;
        let origin = self.minimap.bounds.get().origin;
        let p = gv_canvas::Point::new(
            f64::from(f32::from(at.x - origin.x)),
            f64::from(f32::from(at.y - origin.y)),
        );
        Some(map.center_on(p, self.transform().zoom, self.viewport()))
    }

    fn minimap_down(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.drag = None;
        self.minimap.dragging = true;
        self.minimap.frozen = self.minimap.map;
        if let Some(to) = self.minimap_target(e.position) {
            self.move_to(to, cx);
        }
    }

    fn minimap_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.minimap.dragging {
            return;
        }
        if e.pressed_button != Some(MouseButton::Left) {
            self.end_minimap_drag();
            return;
        }
        cx.stop_propagation();
        if let Some(to) = self.minimap_target(e.position) {
            self.set_transform(to, cx);
        }
    }

    /// Ends a drag on the map (a release anywhere, or a press elsewhere).
    pub(crate) fn end_minimap_drag(&mut self) {
        self.minimap.dragging = false;
        self.minimap.frozen = None;
    }

    /// The minimap element: the box, painted shapes, and the pointer
    /// handlers.
    pub(crate) fn minimap_element(
        &self,
        doc: &Rc<Doc>,
        map: Minimap,
        colors: &Colors,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let doc = doc.clone();
        let colors = *colors;
        let frame = self.minimap.bounds.clone();
        let (w, h) = (MAP_SIZE.width as f32, MAP_SIZE.height as f32);
        div()
            .id("canvas-minimap")
            .debug_selector(|| "canvas-minimap".into())
            .a11y_group(Role::Image, "Minimap")
            .aria_description("The whole canvas; the rectangle is the part in view")
            .absolute()
            .right(px(MAP_RIGHT))
            .bottom(px(MAP_BOTTOM))
            .w(px(w))
            .h(px(h))
            .rounded(px(crate::metrics::radius::POPOVER))
            .overflow_hidden()
            .bg(colors.map_background)
            .border_1()
            .border_color(colors.map_border)
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::minimap_down))
            .on_mouse_move(cx.listener(Self::minimap_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.end_minimap_drag();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.end_minimap_drag()),
            )
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .child(
                canvas(
                    move |bounds, _, _| frame.set(bounds),
                    move |bounds, (), window, _| paint(window, &doc, &map, bounds, &colors),
                )
                .size_full(),
            )
            .accessible()
            .into_any_element()
    }
}

/// Paints the map's shapes into `bounds`.
fn paint(window: &mut Window, doc: &Doc, map: &Minimap, bounds: Bounds<Pixels>, colors: &Colors) {
    let o = bounds.origin;
    // Map rectangle to window bounds, at least a pixel big.
    let on_map = |s: gv_canvas::Rect| {
        Bounds::new(
            point(o.x + px(s.x as f32), o.y + px(s.y as f32)),
            size(
                px((s.width as f32).max(1.0)),
                px((s.height as f32).max(1.0)),
            ),
        )
    };
    let to_window = |r: &gv_canvas::Rect| on_map(map.rect(r));
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        for &item in doc.layout.paint_order() {
            let gv_canvas::ItemId::Node(n) = item else {
                continue;
            };
            let node = &doc.canvas.nodes()[n];
            let b = to_window(&node.rect);
            if matches!(node.kind, NodeKind::Group { .. }) {
                let (border, background) = colors.frame(node.color, true);
                window.paint_quad(
                    fill(b, background)
                        .border_widths(px(1.0))
                        .border_color(border),
                );
            } else {
                window.paint_quad(fill(b, colors.block(node.color)));
            }
        }
        let v = on_map(map.viewport);
        window.paint_quad(
            fill(v, colors.accent.opacity(0.08))
                .border_widths(px(1.5))
                .border_color(colors.accent),
        );
    });
}

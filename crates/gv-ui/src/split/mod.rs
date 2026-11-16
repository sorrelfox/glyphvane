// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Split view (SPEC § Split): the tab's Source view on the left and a
//! Reading view of the same text on the right, in panes of equal width
//! with a 1 px `border.default` line between them.
//!
//! # The divider
//!
//! Dragging the line (or a few pixels either side of it) moves it; neither
//! pane gets narrower than [`split::MIN_PANE`]. A double-click puts it back
//! in the middle. The split is a share of the view's width, so it holds
//! when the window is resized; it is the tab's, and lasts while the tab is
//! open (the session does not store it yet).
//!
//! # The preview follows the editor
//!
//! The editor is the tab's own [`EditorView`], drawn from the pane's left
//! edge ([`EditorLayout::Pane`]) and wrapping at the pane's width. The
//! preview is a [`Reader`] made for the pane ([`Reader::for_split`]:
//! padding `20 28 0`, the split type scale). After every frame the view
//! hands the editor's latest complete syntax tree to the preview, with the
//! reparses since the last hand-off; the preview updates its model and
//! render tree off the UI thread and only re-lays out the blocks that
//! changed. One update runs at a time and later edits are composed into
//! the next, so typing never waits for the preview, which lags by the
//! update's time (a frame or two).
//!
//! # Scroll sync
//!
//! The pane the user last scrolled, clicked or typed in leads; the other
//! follows. After each frame the leader's top, as a fractional source line
//! ([`sync`]), is mapped to the follower when it moved or when the preview
//! got a new revision of the text. The follower never drives back, so the
//! panes cannot oscillate even though the two mappings round, and a pane
//! whose scrollbar is being dragged leads, so nothing moves it under the
//! pointer. A follower lags its leader by one frame.
//!
//! When the caret moves or the text changes, the editor leads and the
//! preview also scrolls by as little as shows the caret's block whole; that
//! block gets a `state.hover` band. Both wait until the preview shows the
//! editor's current revision, so the band does not flicker while typing.
//!
//! # Folding
//!
//! The editor's gutter shows the fold chevrons (`crate::editor`'s folding).
//! The preview shows the whole text; folds are the editor's.
//!
//! # Not built
//!
//! - The divider cannot be moved from the keyboard.

mod sync;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, CursorStyle, DispatchPhase, Entity, Focusable as _,
    HitboxBehavior, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Render, ScrollEvent, Styled as _, Window,
    bounds, canvas, div, point, px, relative, size,
};
use gv_text::Revision;
use gv_vault::PathIndex;

use crate::appearance::Appearance;
use crate::editor::{EditorLayout, EditorView};
use crate::metrics::HAIRLINE;
use crate::metrics::size::split;
use crate::reader::{LoadStatus, Reader, ScrollTarget, SyncRequest};
use sync::Preview;

/// The keymap's `split` context.
pub(crate) const KEY_CONTEXT: &str = "Split";

/// One of the two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Editor,
    Preview,
}

/// What the follower was last moved to: the leader, its top and the
/// preview's document version then.
type Synced = (Side, f64, u64);

/// The Split view of one tab.
pub struct SplitView {
    editor: Entity<EditorView>,
    reader: Entity<Reader>,
    /// The document's path, for the preview's links.
    path: PathBuf,
    /// The pane the user moved last. Pointer and key handlers set it.
    leader: Rc<Cell<Side>>,
    synced: Option<Synced>,
    /// The caret and the text revision as last seen.
    caret: Option<(usize, Revision)>,
    /// The caret moved: the preview is to show its block.
    reveal: bool,
    /// The editor pane's share of the view's width, as last set.
    ratio: f32,
    /// While the divider is dragged: the pointer's distance right of the
    /// line when it was grabbed.
    drag: Option<f32>,
    /// The view's left edge and width, and the line's left edge, as last
    /// laid out.
    frame: Rc<Cell<Frame>>,
}

/// Where the view and its divider were last laid out (window pixels).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Frame {
    left: f32,
    width: f32,
    line: f32,
}

/// `ratio` kept so neither pane of a view `width` wide (divider included)
/// is narrower than [`split::MIN_PANE`]; a view too narrow for two such
/// panes is split in the middle.
fn clamp_ratio(ratio: f32, width: f32) -> f32 {
    if width <= 0.0 {
        return ratio.clamp(0.0, 1.0);
    }
    if width < 2.0 * split::MIN_PANE + HAIRLINE {
        return 0.5;
    }
    let low = split::MIN_PANE / width;
    let high = 1.0 - (split::MIN_PANE + HAIRLINE) / width;
    ratio.clamp(low, high)
}

impl std::fmt::Debug for SplitView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SplitView")
            .field("path", &self.path)
            .field("leader", &self.leader.get())
            .field("synced", &self.synced)
            .finish_non_exhaustive()
    }
}

impl SplitView {
    /// The Split view of `editor`'s document at `path`, with link statuses
    /// from `index`. The editor is put at the pane's edge.
    pub fn new(
        editor: Entity<EditorView>,
        path: PathBuf,
        index: Option<PathIndex>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        editor.update(cx, |e, cx| e.set_layout(EditorLayout::Pane, cx));
        let reader = cx.new(|cx| {
            let mut reader = Reader::for_split(cx);
            reader.set_index(index, cx);
            reader
        });
        Self {
            editor,
            reader,
            path,
            leader: Rc::new(Cell::new(Side::Editor)),
            synced: None,
            caret: None,
            reveal: false,
            ratio: 0.5,
            drag: None,
            frame: Rc::default(),
        }
    }

    /// The document moved to `path` (Save As): the preview's links resolve
    /// from there.
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = path;
    }

    /// The editor pane's share of the view's width, as last set (before
    /// clamping to the view's current width).
    pub fn ratio(&self) -> f32 {
        self.ratio
    }

    /// The editor pane's share of the view's width as drawn: the set ratio
    /// kept clear of the minimum pane widths.
    pub fn shown_ratio(&self) -> f32 {
        clamp_ratio(self.ratio, self.frame.get().width)
    }

    /// Puts the divider at `ratio` of the view's width, kept clear of the
    /// minimum pane widths.
    pub fn set_ratio(&mut self, ratio: f32, cx: &mut Context<'_, Self>) {
        let ratio = clamp_ratio(ratio, self.frame.get().width);
        if ratio != self.ratio {
            self.ratio = ratio;
            cx.notify();
        }
    }

    /// Whether the divider is being dragged.
    pub fn is_dragging_divider(&self) -> bool {
        self.drag.is_some()
    }

    /// The pointer went down on the divider at window x `x`.
    fn grab_divider(&mut self, x: f32, clicks: usize, cx: &mut Context<'_, Self>) {
        if clicks >= 2 {
            self.drag = None;
            self.set_ratio(0.5, cx);
            return;
        }
        self.drag = Some(x - self.frame.get().line);
        cx.notify();
    }

    /// The pointer moved to window x `x` with the divider grabbed.
    fn drag_divider(&mut self, x: f32, cx: &mut Context<'_, Self>) {
        let Some(grab) = self.drag else {
            return;
        };
        let frame = self.frame.get();
        if frame.width <= 0.0 {
            return;
        }
        self.set_ratio((x - grab - frame.left) / frame.width, cx);
    }

    pub fn editor(&self) -> &Entity<EditorView> {
        &self.editor
    }

    /// The preview pane's Reading view.
    pub fn reader(&self) -> &Entity<Reader> {
        &self.reader
    }

    /// The pane that leads scroll sync.
    pub fn leader(&self) -> Side {
        self.leader.get()
    }

    /// Makes `side` lead, as if the user had scrolled it.
    pub fn set_leader(&mut self, side: Side) {
        self.leader.set(side);
    }

    /// The tab comes to Split mode: the editor leads, and the caret where
    /// it is now does not count as moved.
    pub fn restart(&mut self) {
        self.leader.set(Side::Editor);
        self.synced = None;
        self.caret = None;
        self.reveal = false;
    }

    /// The window's path index changed.
    pub fn set_index(&self, index: Option<PathIndex>, cx: &mut Context<'_, Self>) {
        self.reader.update(cx, |r, cx| r.set_index(index, cx));
    }

    /// The source offset at the top of the leading pane, for the mode the
    /// tab switches to.
    pub fn top_offset(&self, cx: &gpui::App) -> Option<usize> {
        let from_preview = || match self.reader.read(cx).position() {
            Some(ScrollTarget::Offset(offset)) => Some(offset),
            _ => None,
        };
        match self.leader.get() {
            Side::Preview => from_preview().or_else(|| self.editor.read(cx).top_offset()),
            Side::Editor => self.editor.read(cx).top_offset(),
        }
    }

    /// After a frame: hands the editor's text to the preview, follows the
    /// caret and syncs the follower.
    fn after_frame(&mut self, cx: &mut Context<'_, Self>) {
        self.hand_off(cx);
        self.follow_caret(cx);
        self.sync(cx);
    }

    /// Gives the preview the editor's latest tree, unless an update is
    /// still being built (the next one then takes both edits).
    fn hand_off(&mut self, cx: &mut Context<'_, Self>) {
        if self.reader.read(cx).is_building() {
            return;
        }
        let Some(handoff) = self.editor.update(cx, |e, _| e.take_handoff()) else {
            return;
        };
        let target = if self.reader.read(cx).status() == &LoadStatus::Ready {
            None
        } else {
            self.editor.read(cx).top_offset().map(ScrollTarget::Offset)
        };
        let path = self.path.clone();
        self.reader
            .update(cx, |r, cx| r.follow(path, handoff, target, cx));
    }

    /// Notices a moved caret or new text: the editor leads, the preview is
    /// to show the caret's block, and the band follows once the preview
    /// shows the editor's revision.
    fn follow_caret(&mut self, cx: &mut Context<'_, Self>) {
        let Some(caret) = self.editor.read(cx).caret() else {
            return;
        };
        match self.caret.replace(caret) {
            Some(seen) if seen != caret => {
                self.leader.set(Side::Editor);
                self.reveal = true;
            }
            _ => {}
        }
        if self.preview_is_current(cx) {
            self.reader
                .update(cx, |r, cx| r.set_band(Some(caret.0), cx));
        }
    }

    /// Whether the preview shows the editor's current text.
    fn preview_is_current(&self, cx: &gpui::App) -> bool {
        let shown = self.reader.read(cx).shown_stamp().map(|s| s.revision);
        shown.is_some() && shown == self.caret.map(|(_, revision)| revision)
    }

    /// Moves the follower to the leader's top when the leader moved or the
    /// preview's text changed since the last sync.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let (doc, version, top) = {
            let reader = self.reader.read(cx);
            (reader.live_doc(), reader.doc_version(), reader.top_spot())
        };
        let Some((tree, text)) = doc else {
            return;
        };
        let lines = Preview {
            tree: &tree,
            text: &text,
        };
        match self.leader.get() {
            Side::Editor => {
                let Some(position) = self.editor.read(cx).top_position() else {
                    return;
                };
                let key = (Side::Editor, position, version);
                let reveal = self.reveal && self.preview_is_current(cx);
                if self.synced == Some(key) && !reveal {
                    return;
                }
                let Some((ix, spot)) = sync::to_preview(&lines, position) else {
                    return;
                };
                self.synced = Some(key);
                let reveal = reveal
                    .then(|| self.caret.map(|(offset, _)| offset))
                    .flatten();
                if reveal.is_some() {
                    self.reveal = false;
                }
                let request = SyncRequest { ix, spot, reveal };
                self.reader.update(cx, |r, cx| r.sync_to(request, cx));
            }
            Side::Preview => {
                let Some((ix, spot)) = top else {
                    return;
                };
                let position = sync::to_editor(&lines, ix, spot);
                let key = (Side::Preview, position, version);
                if self.synced == Some(key) {
                    return;
                }
                self.synced = Some(key);
                self.editor
                    .update(cx, |e, cx| e.scroll_to_position(position, cx));
            }
        }
    }
}

/// A pane: `content`, with handlers that make `side` lead when the wheel
/// turns or a button goes down over it (seen before the pane's own
/// handlers, which stop them).
fn pane(
    content: AnyElement,
    side: Side,
    leader: Rc<Cell<Side>>,
    frame: Rc<Cell<Frame>>,
) -> gpui::Div {
    div()
        .min_w_0()
        .h_full()
        .relative()
        .overflow_hidden()
        .child(content)
        .child(
            canvas(
                move |bounds, _, _| {
                    // The editor pane's right edge is the divider's.
                    if side == Side::Editor {
                        let mut f = frame.get();
                        f.line = f32::from(bounds.right());
                        frame.set(f);
                    }
                },
                move |bounds, (), window, _| {
                    let wheel = leader.clone();
                    window.on_mouse_event(move |e: &ScrollEvent, phase, _, _| {
                        if phase == DispatchPhase::Capture && bounds.contains(&e.position) {
                            wheel.set(side);
                        }
                    });
                    let press = leader.clone();
                    window.on_mouse_event(move |e: &MouseDownEvent, phase, _, _| {
                        if phase == DispatchPhase::Capture && bounds.contains(&e.position) {
                            press.set(side);
                        }
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

impl Render for SplitView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let editor_focus = self.editor.read(cx).focus_handle(cx);
        let reader_focus = self.reader.read(cx).focus_handle(cx);
        let keys = self.leader.clone();
        let this = cx.weak_entity();
        let ratio = self.shown_ratio();
        let dragging = self.drag.is_some();
        let measure = self.frame.clone();
        let handle_frame = self.frame.clone();
        let handle_view = cx.weak_entity();
        div()
            .id("split")
            .key_context(KEY_CONTEXT)
            .size_full()
            .flex()
            .capture_key_down(move |_, window, cx| {
                if editor_focus.contains_focused(window, cx) {
                    keys.set(Side::Editor);
                } else if reader_focus.contains_focused(window, cx) {
                    keys.set(Side::Preview);
                }
            })
            // The view's frame, before the panes are laid out.
            .child(
                canvas(
                    move |bounds, _, _| {
                        let mut f = measure.get();
                        f.left = f32::from(bounds.left());
                        f.width = f32::from(bounds.size.width);
                        measure.set(f);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                pane(
                    self.editor.clone().into_any_element(),
                    Side::Editor,
                    self.leader.clone(),
                    self.frame.clone(),
                )
                .flex_none()
                .w(relative(ratio)),
            )
            .child(
                div()
                    .w(px(HAIRLINE))
                    .h_full()
                    .flex_none()
                    .bg(theme.border.default),
            )
            .child(
                pane(
                    self.reader.clone().into_any_element(),
                    Side::Preview,
                    self.leader.clone(),
                    self.frame.clone(),
                )
                .flex_1(),
            )
            // Over the panes: the divider's grab area.
            .child(divider_handle(handle_view, handle_frame, dragging))
            // After the panes are laid out: sync them.
            .child(
                canvas(
                    move |_, _, cx| {
                        cx.defer(move |cx| {
                            this.update(cx, |split, cx| split.after_frame(cx)).ok();
                        });
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_0(),
            )
    }
}

/// The divider's grab area, over both panes: [`split::DIVIDER_GRAB`] either
/// side of the line. A press there grabs the divider (a double press puts
/// it back in the middle) before the panes see it; while it is held the
/// pointer moves it wherever it goes.
fn divider_handle(
    view: gpui::WeakEntity<SplitView>,
    frame: Rc<Cell<Frame>>,
    dragging: bool,
) -> impl IntoElement {
    canvas(
        move |area, window, _| {
            let f = frame.get();
            let grab = bounds(
                point(px(f.line - split::DIVIDER_GRAB), area.top()),
                size(px(2.0 * split::DIVIDER_GRAB + HAIRLINE), area.size.height),
            );
            window.insert_hitbox(grab, HitboxBehavior::BlockMouse)
        },
        move |_, hitbox, window, _| {
            if dragging {
                window.set_window_cursor_style(CursorStyle::ResizeLeftRight);
            } else {
                window.set_cursor_style(CursorStyle::ResizeLeftRight, &hitbox);
            }
            let press = view.clone();
            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || e.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let x = f32::from(e.position.x);
                press
                    .update(cx, |s, cx| s.grab_divider(x, e.click_count, cx))
                    .ok();
                cx.stop_propagation();
            });
            if dragging {
                let moved = view.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let x = f32::from(e.position.x);
                    moved.update(cx, |s, cx| s.drag_divider(x, cx)).ok();
                    cx.stop_propagation();
                });
                let released = view.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                    if phase != DispatchPhase::Capture || e.button != MouseButton::Left {
                        return;
                    }
                    released
                        .update(cx, |s, cx| {
                            s.drag = None;
                            cx.notify();
                        })
                        .ok();
                    cx.stop_propagation();
                });
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

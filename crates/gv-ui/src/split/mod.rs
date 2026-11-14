// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Split view (SPEC § Split): the tab's Source view on the left and a
//! Reading view of the same text on the right, in panes of equal width
//! with a 1 px `border.default` line between them.
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
//! # Not built
//!
//! - Folding: the editor's gutter keeps the fold chevrons' place, but none
//!   are drawn.
//! - The divider cannot be dragged; the panes stay equal.

mod sync;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AnyElement, AppContext as _, Context, DispatchPhase, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, MouseDownEvent, ParentElement as _, Render, ScrollEvent,
    Styled as _, Window, canvas, div, px,
};
use gv_text::Revision;
use gv_vault::PathIndex;

use crate::appearance::Appearance;
use crate::editor::{EditorLayout, EditorView};
use crate::metrics::HAIRLINE;
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
        }
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
fn pane(content: AnyElement, side: Side, leader: Rc<Cell<Side>>) -> impl IntoElement {
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .relative()
        .overflow_hidden()
        .child(content)
        .child(
            canvas(
                |_, _, _| {},
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
            .child(pane(
                self.editor.clone().into_any_element(),
                Side::Editor,
                self.leader.clone(),
            ))
            .child(
                div()
                    .w(px(HAIRLINE))
                    .h_full()
                    .flex_none()
                    .bg(theme.border.default),
            )
            .child(pane(
                self.reader.clone().into_any_element(),
                Side::Preview,
                self.leader.clone(),
            ))
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

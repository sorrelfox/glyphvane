// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The document area: the active tab's Reading view in Preview mode, its
//! Source view in Edit mode, and both side by side in Split mode
//! ([`crate::split`]).
//!
//! # Switching modes
//!
//! A tab's Source view is made the first time the tab goes to Edit or
//! Split and kept while the tab shows the same file (an untitled tab has
//! one from the start); its Split view likewise. The panels are never
//! touched (FR-VIEW-16). The source line at the top stays at the top:
//!
//! - **Preview → Edit or Split:** the editor scrolls so that the source
//!   line of the block at the top of the Reading view is at its top; in
//!   Split the preview pane follows the editor.
//! - **Edit ↔ Split:** the editor moves between the centred column and the
//!   pane's edge, keeping its top line.
//! - **Edit or Split → Preview:** the Reading view scrolls to the line at
//!   the top of the editor (of the leading pane, from Split). When the text
//!   changed since the Reading view last got it (saved or not), the Reading
//!   view is rebuilt from the editor's text
//!   ([`Reader::open_text`](crate::reader::Reader::open_text)) instead of
//!   the file.
//!
//! # Viewer mode
//!
//! With `general.viewer_mode` on, Edit and Split are unavailable (the mode
//! switch shows them disabled, Mod+E and the View menu do nothing) and
//! every Source view is read-only, except in a tab opened for an edit
//! someone waits for (`--wait`, which overrides viewer mode for that tab
//! only) and in untitled documents. A tab in Edit or Split when viewer
//! mode comes on, or restored in one, goes to Preview when it is shown.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, ExternalPaths, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, canvas, div, px,
};

use super::drag_drop::{DraggedFile, DraggedHeading};
use super::{TextPosition, ViewMode, Workspace};
use crate::editor::{EditorLayout, EditorView, Top};
use crate::metrics::size::document;
use crate::metrics::spacing::document as spacing;
use crate::metrics::text;
use crate::reader::{Reader, ReaderEvent, ScrollTarget, TaskToggle, startup};
use crate::split::{Side, SplitView};
use crate::theme::Theme;

impl Workspace {
    pub(super) fn render_document(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> AnyElement {
        let area = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(theme.surface.page)
            // Drops: links into the Source view, or files to open
            // ([`super::drag_drop`]).
            .on_drop(cx.listener(|this, file: &DraggedFile, window, cx| {
                this.drop_dragged_file(file, window, cx);
            }))
            .on_drop(cx.listener(|this, heading: &DraggedHeading, window, cx| {
                this.drop_heading(heading, window, cx);
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_external(paths, window, cx);
            }));
        let tab = self.active_tab();
        // A compare view takes the document's place while it is open (not
        // designed; see `crate::compare`).
        if let Some(compare) = tab.editor.as_ref().and_then(|e| e.read(cx).compare_view()) {
            return area.child(compare).into_any_element();
        }
        // A canvas file shows in its Canvas view, whatever the mode.
        if let Some(canvas) = self.shown_canvas() {
            return area.child(canvas.clone()).into_any_element();
        }
        // So does a base file, in its Bases view.
        if let Some(base) = self.shown_base() {
            return area.child(base.clone()).into_any_element();
        }
        // And any other file, in the built-in viewer in Preview (its text
        // in the Source view in Edit).
        if let Some(viewer) = self.shown_viewer() {
            return area.child(viewer.clone()).into_any_element();
        }
        match (self.mode(), &tab.reader, &tab.editor, &tab.split) {
            (ViewMode::Preview, Some(reader), _, _) => {
                return area.child(reader.clone()).into_any_element();
            }
            (ViewMode::Edit, _, Some(editor), _) => {
                return area
                    .child(editor.clone())
                    .children(startup::enabled().then(|| source_probe(&tab.label.name)))
                    .into_any_element();
            }
            (ViewMode::Split, _, _, Some(split)) => {
                return area.child(split.clone()).into_any_element();
            }
            _ => {}
        }
        let note = "Nothing to show";
        // The text column is at most 680 wide plus its side padding, centred,
        // in every mode, so text does not move sideways when the mode changes.
        let column_width = document::COLUMN_MAX + 2.0 * document::PAD_X;

        area.flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(column_width))
                    .px(px(document::PAD_X))
                    .pt(px(document::PAD_TOP))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .mb(px(spacing::HEADING_GAP))
                            .text_size(px(text::PREVIEW_H1.size))
                            .line_height(px(text::PREVIEW_H1.line))
                            .font_weight(FontWeight(text::weight::BOLD))
                            .child(tab.label.name.clone()),
                    )
                    .child(
                        div()
                            .mb(px(spacing::PARAGRAPH_GAP))
                            .text_size(px(text::PREVIEW_BODY.size))
                            .line_height(px(text::PREVIEW_BODY.line))
                            .text_color(theme.text.secondary)
                            .child(note),
                    ),
            )
            .children(startup::enabled().then(empty_probe))
            .into_any_element()
    }

    /// The Canvas view on screen: the active tab's, when it shows a canvas.
    pub(super) fn shown_canvas(&self) -> Option<&Entity<crate::canvas_view::CanvasView>> {
        let tab = self.active_tab();
        tab.canvas.as_ref().filter(|_| tab.is_canvas())
    }

    /// The Bases view on screen: the active tab's, when it shows a base.
    pub(super) fn shown_base(&self) -> Option<&Entity<crate::bases_view::BasesView>> {
        let tab = self.active_tab();
        tab.bases.as_ref().filter(|_| tab.is_base())
    }

    /// The built-in viewer on screen: the active tab's, in Preview mode,
    /// when it shows a file that is not a note, a canvas or a base (in
    /// Edit mode its Source view shows the file as plain text).
    pub(super) fn shown_viewer(&self) -> Option<&Entity<crate::file_viewer::FileViewer>> {
        let tab = self.active_tab();
        tab.viewer
            .as_ref()
            .filter(|_| tab.is_viewer() && tab.mode == ViewMode::Preview)
    }

    /// The Source view on screen: the active tab's, in Edit and Split
    /// mode.
    pub(super) fn shown_editor(&self) -> Option<&Entity<EditorView>> {
        match self.mode() {
            ViewMode::Edit | ViewMode::Split => self.active_tab().editor.as_ref(),
            ViewMode::Preview => None,
        }
    }

    /// The Split view's preview on screen: the active tab's, in Split mode.
    pub(super) fn shown_split_reader(&self, cx: &App) -> Option<Entity<Reader>> {
        match self.mode() {
            ViewMode::Split => self.active_tab().split_reader(cx),
            ViewMode::Preview | ViewMode::Edit => None,
        }
    }

    /// The Split view of tab `index`, made with its Source view on first
    /// use. The editor goes to the pane's edge.
    pub(super) fn ensure_split(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<SplitView>> {
        let editor = self.ensure_editor(index, cx)?;
        let tab = self.tabs.get(index)?;
        if let Some(split) = &tab.split {
            editor.update(cx, |e, cx| e.set_layout(EditorLayout::Pane, cx));
            return Some(split.clone());
        }
        let path = tab
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from(tab.label.name.as_ref()));
        let ratio = tab.split_ratio;
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let profile = self.tab_profile(index, cx).compiled;
        let split = cx.new(|cx| {
            let mut split = SplitView::new(editor, path, snapshot, cx);
            if let Some(ratio) = ratio {
                split.set_ratio(ratio, cx);
            }
            split
        });
        // A link followed in the preview pane goes where it goes from
        // Preview.
        let reader = split.read(cx).reader().clone();
        let delay = super::link_preview::hover_delay(cx);
        reader.update(cx, |r, cx| {
            r.set_profile(profile, cx);
            r.set_hover_previews(Some(delay));
        });
        cx.observe(&reader, |ws, reader, cx| {
            ws.check_reader_preview(&reader, cx)
        })
        .detach();
        if let Some(vault) = &self.vault {
            let content = vault.read(cx).content();
            reader.update(cx, |r, cx| r.set_content(content, cx));
        }
        cx.subscribe(&reader, |ws, reader, event: &ReaderEvent, cx| {
            ws.follow_split_link(&reader, event.clone(), cx);
        })
        .detach();
        self.tabs[index].split = Some(split.clone());
        Some(split)
    }

    /// Follows a link clicked in a Split view's preview pane `reader`, as
    /// if in its tab's Reading view.
    fn follow_split_link(
        &mut self,
        reader: &Entity<Reader>,
        event: ReaderEvent,
        cx: &mut Context<'_, Self>,
    ) {
        let request = match event {
            ReaderEvent::FollowLink(request) => request,
            ReaderEvent::ToggleTask(toggle) => {
                self.toggle_split_task(reader, toggle, cx);
                return;
            }
            ReaderEvent::ExpandTable(request) => {
                let (window, reader) = (self.window_handle, reader.clone());
                cx.spawn(async move |this, cx| {
                    // Fails only when the window has closed.
                    let _ = cx.update_window(window, |_, window, cx| {
                        this.update(cx, |ws, cx| ws.expand_table(reader, &request, window, cx))
                    });
                })
                .detach();
                return;
            }
        };
        let Some(preview) = self
            .tabs
            .iter()
            .find(|t| t.split_reader(cx).as_ref() == Some(reader))
            .and_then(|t| t.reader.clone())
        else {
            return;
        };
        let window = self.window_handle;
        cx.spawn(async move |this, cx| {
            // Fails only when the window has closed.
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |ws, cx| ws.follow_link(preview, request, window, cx))
            });
        })
        .detach();
    }

    /// A task checkbox clicked in the Reading view `reader` of a tab: the
    /// toggle is written back through the tab's Source view (one undoable
    /// edit, made and loaded on first use), then the Reading view shows
    /// the edited text where it was. Nothing changes in a tab that may not
    /// be edited (viewer mode).
    pub(super) fn toggle_task(
        &mut self,
        reader: &Entity<Reader>,
        toggle: TaskToggle,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.reader.as_ref() == Some(reader))
        else {
            return;
        };
        if !self.can_edit(index, cx) {
            return;
        }
        let Some(editor) = self.ensure_editor(index, cx) else {
            return;
        };
        if editor.read(cx).editor().is_some() {
            Self::write_task(&editor, reader, toggle, cx);
            return;
        }
        // The Source view is loading its file: write back once it is in.
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let reader = reader.clone();
        cx.observe(&editor, move |_, editor, cx| {
            if !done.get() && editor.read(cx).editor().is_some() {
                done.set(true);
                Self::write_task(&editor, &reader, toggle, cx);
            }
        })
        .detach();
    }

    /// Writes `toggle` to `editor` and shows the result in `reader`.
    fn write_task(
        editor: &Entity<EditorView>,
        reader: &Entity<Reader>,
        toggle: TaskToggle,
        cx: &mut Context<'_, Self>,
    ) {
        let changed = editor.update(cx, |e, cx| e.apply_task_toggle(toggle, cx));
        if !changed {
            return;
        }
        if let Some(text) = editor.update(cx, |e, _| e.take_preview_text()) {
            reader.update(cx, |r, cx| r.show_edited(text, cx));
        }
    }

    /// A task checkbox clicked in a Split view's preview pane `reader`: the
    /// toggle goes to the Split view's editor, which the pane follows.
    fn toggle_split_task(
        &mut self,
        reader: &Entity<Reader>,
        toggle: TaskToggle,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.split_reader(cx).as_ref() == Some(reader))
        else {
            return;
        };
        if !self.can_edit(index, cx) {
            return;
        }
        if let Some(editor) = self.tabs[index].editor.clone() {
            editor.update(cx, |e, cx| e.apply_task_toggle(toggle, cx));
        }
    }

    /// A jump inside tab `index`'s document (a heading, a fragment, Back):
    /// in Split mode the preview pane goes there and the editor follows.
    pub(super) fn split_scroll_to(
        &mut self,
        index: usize,
        target: ScrollTarget,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (ViewMode::Split, Some(split)) = (tab.mode, tab.split.clone()) else {
            return;
        };
        split.update(cx, |s, cx| {
            s.set_leader(Side::Preview);
            s.reader().update(cx, |r, cx| r.scroll_to(target, cx));
        });
    }

    /// Shows the active tab in Split mode, or in Edit mode when it is in
    /// Split (`view.toggle_split`).
    pub(super) fn toggle_split(&mut self, cx: &mut Context<'_, Self>) {
        let mode = match self.mode() {
            ViewMode::Split => ViewMode::Edit,
            ViewMode::Preview | ViewMode::Edit => ViewMode::Split,
        };
        self.set_mode(mode, cx);
    }

    /// The Source view of tab `index`, made and loading on first use.
    pub(super) fn ensure_editor(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<EditorView>> {
        let tab = self.tabs.get(index)?;
        if let Some(editor) = &tab.editor {
            return Some(editor.clone());
        }
        let profile = self.tab_profile(index, cx);
        let tab = &self.tabs[index];
        if let (None, Some(n)) = (&tab.path, tab.untitled) {
            let editor = cx.new(|cx| {
                let mut editor = EditorView::new(profile.parse_options(), cx);
                editor.set_compiled_profile(&profile.compiled, cx);
                editor
            });
            self.adopt_editor(&editor, cx);
            editor.update(cx, |e, cx| e.open_untitled(n, cx));
            self.tabs[index].editor = Some(editor.clone());
            return Some(editor);
        }
        let path = tab.path.clone()?;
        // Where the tab is: a restored anchor, or the line it was opened at.
        let top = match (tab.scroll.clone(), tab.position) {
            (Some(ScrollTarget::Offset(offset)), _) => Some(Top::Offset(offset)),
            (Some(ScrollTarget::Line(line)), _) | (None, Some(TextPosition { line, .. })) => {
                Some(Top::Line(line.saturating_sub(1) as usize))
            }
            _ => None,
        };
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let read_only = !self.can_edit(index, cx);
        // A file that is not a note is edited as plain text.
        let plain = super::viewer_tabs::is_viewer_file(&path);
        let folds = match self.tabs[index].restored_folds.take() {
            Some((p, folds)) if p == path => folds,
            _ => Vec::new(),
        };
        let editor = cx.new(|cx| {
            let mut editor = EditorView::new(profile.parse_options(), cx);
            editor.set_compiled_profile(&profile.compiled, cx);
            editor.set_read_only(read_only, cx);
            editor.set_plain(plain, cx);
            editor
        });
        // Watchers first: the view tells them about its saves.
        self.adopt_editor(&editor, cx);
        editor.update(cx, |editor, cx| {
            editor.open(path, snapshot, cx);
            editor.restore_folds(folds);
            match top {
                Some(Top::Offset(offset)) => editor.scroll_to_offset(offset, cx),
                Some(Top::Line(line)) => editor.scroll_to_line(line, cx),
                None => {}
            }
        });
        self.tabs[index].editor = Some(editor.clone());
        Some(editor)
    }

    /// The active tab went from mode `from` to its current mode: carries
    /// the top of the view over, and the edited text to Preview.
    pub(super) fn mode_changed(&mut self, from: ViewMode, cx: &mut Context<'_, Self>) {
        let to = self.mode();
        if from == to {
            return;
        }
        let index = self.active;
        // The top line in the mode left: the Reading view's, or the
        // leading pane's in Split.
        let preview_top = || match self.tabs[index].reader.as_ref()?.read(cx).position() {
            Some(ScrollTarget::Offset(offset)) => Some(offset),
            _ => None,
        };
        let split_top = || self.tabs[index].split.as_ref()?.read(cx).top_offset(cx);
        match (from, to) {
            (_, ViewMode::Edit) => {
                let top = match from {
                    ViewMode::Split => split_top(),
                    ViewMode::Preview | ViewMode::Edit => preview_top(),
                };
                let Some(editor) = self.ensure_editor(index, cx) else {
                    return;
                };
                editor.update(cx, |e, cx| {
                    e.set_layout(EditorLayout::Column, cx);
                    if let Some(offset) = top {
                        e.scroll_to_offset(offset, cx);
                    }
                });
            }
            (_, ViewMode::Split) => {
                let top = match from {
                    ViewMode::Preview => preview_top(),
                    ViewMode::Edit | ViewMode::Split => None,
                };
                let Some(split) = self.ensure_split(index, cx) else {
                    return;
                };
                let editor = split.read(cx).editor().clone();
                if let Some(offset) = top {
                    editor.update(cx, |e, cx| e.scroll_to_offset(offset, cx));
                }
                split.update(cx, |s, _| s.restart());
            }
            (ViewMode::Edit | ViewMode::Split, ViewMode::Preview) => {
                let split = self.tabs[index].split.clone();
                let tab = &self.tabs[index];
                // The built-in viewer shows the edited text, saved or not.
                if tab.is_viewer() {
                    let (Some(editor), Some(viewer)) = (tab.editor.clone(), tab.viewer.clone())
                    else {
                        return;
                    };
                    if let Some(text) = editor.update(cx, |e, _| e.take_preview_text()) {
                        viewer.update(cx, |v, cx| v.show_text(&text, cx));
                    }
                    return;
                }
                let (Some(editor), Some(reader), Some(path)) =
                    (tab.editor.clone(), tab.reader.clone(), tab.path.clone())
                else {
                    return;
                };
                let (text, top) = editor.update(cx, |e, _| (e.take_preview_text(), e.top_offset()));
                let top = match (from, split) {
                    (ViewMode::Split, Some(split)) => split.read(cx).top_offset(cx).or(top),
                    _ => top,
                };
                let target = top.map(ScrollTarget::Offset);
                reader.update(cx, |r, cx| match text {
                    Some(text) => r.open_text(path, text, target, cx),
                    None => {
                        if let Some(target) = target {
                            r.scroll_to(target, cx);
                        }
                    }
                });
            }
            _ => {}
        }
    }
}

/// For the start-up trace: an element drawn after a Source view, so that
/// its first frame counts as the first frame with content (the Reading
/// view marks its own).
fn source_probe(name: &str) -> impl IntoElement {
    let what = format!("{name}, Source view");
    canvas(
        |_, _, _| {},
        move |_, (), window, cx| startup::first_content_painted(&what, window, cx),
    )
    .absolute()
    .size_0()
}

/// For the start-up trace: an element drawn with the placeholder shown when
/// there is no document view.
fn empty_probe() -> impl IntoElement {
    canvas(|_, _, _| {}, |_, (), _, cx| startup::empty_painted(cx))
        .absolute()
        .size_0()
}

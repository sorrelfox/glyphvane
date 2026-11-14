// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The document area: the active tab's Reading view in Preview mode and
//! its Source view in Edit mode. Split holds a placeholder until it exists.
//!
//! # Switching modes
//!
//! A tab's Source view is made the first time the tab goes to Edit and kept
//! while the tab shows the same file (an untitled tab has one from the
//! start). The panels are never touched.
//!
//! - **Preview → Edit:** the editor scrolls so that the source line of the
//!   block at the top of the Reading view is at its top.
//! - **Edit → Preview:** the Reading view scrolls to the line at the top of
//!   the editor. When the text changed since the Reading view last got it
//!   (saved or not), the Reading view is rebuilt from the editor's text
//!   ([`Reader::open_text`](crate::reader::Reader::open_text)) instead of
//!   the file.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _,
    Styled as _, div, px,
};

use super::{TextPosition, ViewMode, Workspace};
use crate::editor::{EditorView, Top};
use crate::metrics::size::document;
use crate::metrics::spacing::document as spacing;
use crate::metrics::text;
use crate::reader::ScrollTarget;
use crate::theme::Theme;

impl Workspace {
    pub(super) fn render_document(&self, theme: &Theme, _cx: &mut Context<'_, Self>) -> AnyElement {
        let area = div()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(theme.surface.page);
        let tab = self.active_tab();
        match (self.mode(), &tab.reader, &tab.editor) {
            (ViewMode::Preview, Some(reader), _) => {
                return area.child(reader.clone()).into_any_element();
            }
            (ViewMode::Edit, _, Some(editor)) => {
                return area.child(editor.clone()).into_any_element();
            }
            _ => {}
        }
        let note = match self.mode() {
            ViewMode::Preview | ViewMode::Edit => "Nothing to show",
            ViewMode::Split => "Split view not implemented yet",
        };
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
            .into_any_element()
    }

    /// The Source view on screen: the active tab's, in Edit mode.
    pub(super) fn shown_editor(&self) -> Option<&Entity<EditorView>> {
        match self.mode() {
            ViewMode::Edit => self.active_tab().editor.as_ref(),
            ViewMode::Preview | ViewMode::Split => None,
        }
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
        if let (None, Some(n)) = (&tab.path, tab.untitled) {
            let editor = cx.new(|cx| EditorView::new(EditorView::profile_options(), cx));
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
        let editor = cx.new(|cx| EditorView::new(EditorView::profile_options(), cx));
        // Watchers first: the view tells them about its saves.
        self.adopt_editor(&editor, cx);
        editor.update(cx, |editor, cx| {
            editor.open(path, snapshot, cx);
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
        match (from, to) {
            (_, ViewMode::Edit) => {
                let Some(editor) = self.ensure_editor(index, cx) else {
                    return;
                };
                let top = self.tabs[index]
                    .reader
                    .as_ref()
                    .and_then(|r| r.read(cx).position());
                if let Some(ScrollTarget::Offset(offset)) = top {
                    editor.update(cx, |e, cx| e.scroll_to_offset(offset, cx));
                }
            }
            (ViewMode::Edit, ViewMode::Preview) => {
                let tab = &self.tabs[index];
                let (Some(editor), Some(reader), Some(path)) =
                    (tab.editor.clone(), tab.reader.clone(), tab.path.clone())
                else {
                    return;
                };
                let (text, top) = editor.update(cx, |e, _| (e.take_preview_text(), e.top_offset()));
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

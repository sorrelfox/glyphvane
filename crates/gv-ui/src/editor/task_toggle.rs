// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Writing a task checkbox clicked in the Reading view back to the text:
//! one byte patch, its own undo step (FR-VIEW-07).

use gpui::Context;
use gv_editor::EditKind;

use super::EditorView;
use crate::reader::TaskToggle;

impl EditorView {
    /// Replaces the task status character `toggle.from` at
    /// `toggle.offset` with `toggle.to`, as one undoable edit. Nothing
    /// changes when the view is read-only, not loaded, or the text there
    /// is not a task box holding `toggle.from` (it changed meanwhile).
    /// Returns whether the text changed.
    pub fn apply_task_toggle(&mut self, toggle: TaskToggle, cx: &mut Context<'_, Self>) -> bool {
        if self.read_only {
            return false;
        }
        let Some(doc) = self.doc.as_mut() else {
            return false;
        };
        if doc.editor.is_composing() {
            return false;
        }
        let text = doc.editor.text();
        let start = toggle.offset;
        let end = start + toggle.from.len_utf8();
        let (Some(before), Some(after)) = (start.checked_sub(1), end.checked_add(1)) else {
            return false;
        };
        if after > text.len() {
            return false;
        }
        let Ok(boxed) = text.text_for_range(before..after) else {
            return false;
        };
        let expected = format!("[{}]", toggle.from);
        if boxed != expected {
            return false;
        }
        let edited = doc
            .editor
            .edit([(start..end, toggle.to.to_string())], EditKind::Command);
        if !matches!(edited, Ok(Some(_))) {
            return false;
        }
        self.input_changed(cx);
        true
    }
}

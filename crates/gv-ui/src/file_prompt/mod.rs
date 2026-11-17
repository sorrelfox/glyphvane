// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The file prompt: the popover that asks for a file or folder name
//! (Rename…, New folder) and confirms file operations (the rename and
//! move preview, Move to trash).
//!
//! Not designed. It is drawn like the command palette and the Go
//! popovers (SPEC § Popovers): 540 wide, 84 below the top of the window
//! and centred, `surface.raised` with `shadow.popover`. From the top: the
//! title in weight 600 and a message; the name field (40 high) with an
//! error line under it when the name cannot be used; the list rows (28
//! high, at most 12 before they scroll): file headings, removed lines
//! with an `status.error_tint` fill and added lines with a
//! `status.success_tint` fill, notes and warnings; a footer with key
//! hints.
//!
//! # Behaviour
//!
//! - **Opening.** A view asks for a prompt with a [`PromptRequest`]. The
//!   prompt takes the keyboard and gives it back when it closes. With a
//!   field, the part of the name given by [`PromptField::select`] is
//!   selected (the name without its extension).
//! - **Enter** confirms: the callback gets the field's text (empty
//!   without a field) and the prompt closes. While the request's
//!   validator finds a problem with the text, Enter does nothing and the
//!   problem shows under the field.
//! - **Esc** or a click outside cancels.
//!
//! The prompt is a layer of the window root (GPUI Base's `Root` plugin),
//! like the Go popovers.

#[cfg(test)]
mod tests;
mod view;

use std::ops::Range;
use std::rc::Rc;

use gpui::base::Root;
use gpui::{App, Entity, KeyBinding, SharedString, Window};

pub use view::FilePrompt;

gpui::actions!(
    file_prompt,
    [
        /// Do what the prompt asks.
        Confirm,
        /// Close without doing anything.
        Cancel,
    ]
);

/// The prompt's key context.
pub const KEY_CONTEXT: &str = "FilePrompt";

/// The prompt's own keys, bound on its field as well, where the text
/// field's own Enter and Esc would otherwise take them.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let field = "FilePrompt > Input";
    let mut keys = Vec::new();
    for context in [KEY_CONTEXT, field] {
        keys.extend([
            KeyBinding::new("enter", Confirm, Some(context)),
            KeyBinding::new("escape", Cancel, Some(context)),
        ]);
    }
    keys
}

/// Binds the prompt's keys and adds it to the window root. Called by
/// [`crate::init`], before any window opens.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
    Root::register_plugin::<FilePrompt>(cx, FilePrompt::new);
}

/// The file prompt of `window`, if its root has one.
pub fn prompt_of(window: &Window, cx: &App) -> Option<Entity<FilePrompt>> {
    window
        .root::<Root>()
        .flatten()?
        .read(cx)
        .plugin::<FilePrompt>()
}

/// Opens the prompt of `window` for `request` (replacing one that is
/// open). Returns whether the window has a prompt.
pub fn open(request: PromptRequest, window: &mut Window, cx: &mut App) -> bool {
    let Some(prompt) = prompt_of(window, cx) else {
        return false;
    };
    prompt.update(cx, |prompt, cx| prompt.open(request, window, cx));
    true
}

/// The name field of a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptField {
    /// The text it starts with.
    pub initial: String,
    /// The byte range selected at first.
    pub select: Range<usize>,
}

impl PromptField {
    /// A field holding the file name `name`, with the part before its
    /// extension selected (all of a folder's name).
    pub fn for_name(name: &str, is_dir: bool) -> Self {
        let end = if is_dir {
            name.len()
        } else {
            gv_vault::path::stem_of(name).len()
        };
        Self {
            initial: name.to_owned(),
            select: 0..end,
        }
    }
}

/// How a list row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStyle {
    /// A file: weight 600.
    Heading,
    /// A line as it is now: `status.error_tint` fill, `−` before it.
    Removed,
    /// The line as it will be: `status.success_tint` fill, `+` before it.
    Added,
    /// Plain text in `text.secondary`.
    Note,
    /// A warning: `status.warning` text.
    Warning,
}

/// One list row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRow {
    pub text: SharedString,
    pub style: RowStyle,
}

impl PromptRow {
    pub fn new(text: impl Into<SharedString>, style: RowStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

/// Checks the field's text: the problem to show, or `None` when it can
/// be used.
pub type Validate = Rc<dyn Fn(&str) -> Option<SharedString>>;

/// Called with the field's text when the prompt is confirmed.
pub type OnConfirm = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// A prompt to open.
#[derive(Clone)]
pub struct PromptRequest {
    /// The heading, also the prompt's accessible name.
    pub title: SharedString,
    /// What the prompt is about, under the title.
    pub message: Option<SharedString>,
    /// The name field, if the prompt asks for a name.
    pub field: Option<PromptField>,
    /// The list (a preview of the changes, what will break).
    pub rows: Vec<PromptRow>,
    /// What the footer calls Enter ("rename", "move to trash").
    pub action: SharedString,
    pub validate: Option<Validate>,
    pub on_confirm: OnConfirm,
}

impl std::fmt::Debug for PromptRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptRequest")
            .field("title", &self.title)
            .field("message", &self.message)
            .field("field", &self.field)
            .field("rows", &self.rows.len())
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Save As: asking where a document goes.
//!
//! The question goes to a [`SavePathDialog`], by default the platform's own
//! save dialog through GPUI (`App::prompt_for_new_path`): the XDG desktop
//! portal's FileChooser on Linux (Wayland and X11), `NSSavePanel` on macOS
//! and `IFileSaveDialog` on Windows. It answers asynchronously, so the UI
//! thread never waits for it, and it asks before replacing an existing
//! file itself. Tests install their own dialog with [`set_dialog`].
//!
//! The rules around it:
//!
//! - The dialog opens in the document's folder with its file name, or, for
//!   an untitled document, in the window's vault folder with a name made
//!   from the document's title ([`suggested_name`]).
//! - A name without an extension gets `.md` ([`with_markdown_extension`]).
//!   The dialog did not ask about that name, so an existing file with it is
//!   not replaced: the save fails with an error infobar instead.
//! - Cancelling does nothing.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{App, Global};

/// The extension Save As adds to a name without one.
pub const MARKDOWN_EXTENSION: &str = "md";

/// The longest file name made from a title, in characters (before `.md`).
const MAX_TITLE_CHARS: usize = 80;

/// How much of the text is read for its title.
const TITLE_SCAN_BYTES: usize = 4096;

/// What the save dialog starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavePrompt {
    /// The folder it opens in.
    pub directory: PathBuf,
    /// The file name it suggests.
    pub suggested_name: String,
}

/// The dialog's answer: the chosen path, `None` when cancelled, or why it
/// could not be shown.
pub type SaveAnswer = Result<Option<PathBuf>, String>;

/// Asks the user where to save a document.
pub trait SavePathDialog {
    /// Shows the dialog. The future resolves once the user answered; the
    /// UI thread does not wait for it.
    fn ask(&self, prompt: &SavePrompt, cx: &mut App) -> LocalBoxFuture<'static, SaveAnswer>;
}

/// The platform's save dialog, through GPUI.
struct PlatformDialog;

impl SavePathDialog for PlatformDialog {
    fn ask(&self, prompt: &SavePrompt, cx: &mut App) -> LocalBoxFuture<'static, SaveAnswer> {
        let answer = cx.prompt_for_new_path(&prompt.directory, Some(&prompt.suggested_name));
        async move {
            match answer.await {
                Ok(Ok(path)) => Ok(path),
                Ok(Err(err)) => Err(err.to_string()),
                // The platform dropped the question: as if cancelled.
                Err(_) => Ok(None),
            }
        }
        .boxed_local()
    }
}

/// The dialog in use, when not the platform's.
struct InstalledDialog(Rc<dyn SavePathDialog>);

impl Global for InstalledDialog {}

/// Uses `dialog` instead of the platform's save dialog (tests).
pub fn set_dialog(cx: &mut App, dialog: impl SavePathDialog + 'static) {
    cx.set_global(InstalledDialog(Rc::new(dialog)));
}

/// The save dialog: the installed one, or the platform's.
pub(crate) fn dialog(cx: &App) -> Rc<dyn SavePathDialog> {
    match cx.try_global::<InstalledDialog>() {
        Some(installed) => installed.0.clone(),
        None => Rc::new(PlatformDialog),
    }
}

/// `path` with `.md` added when its name has no extension. Returns whether
/// the name changed.
pub fn with_markdown_extension(path: PathBuf) -> (PathBuf, bool) {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return (path, false);
    };
    let has_extension = Path::new(&name).extension().is_some_and(|e| !e.is_empty());
    if has_extension {
        return (path, false);
    }
    let stem = name.trim_end_matches('.');
    if stem.is_empty() {
        return (path, false);
    }
    let named = path.with_file_name(format!("{stem}.{MARKDOWN_EXTENSION}"));
    (named, true)
}

/// A file name for an untitled document whose text starts with `text`:
/// its title (the first line with text, without heading marks, after any
/// front matter) made safe for every platform's file names, with `.md`.
/// `None` when the text has no title.
pub fn suggested_name(text: &str) -> Option<String> {
    let mut end = text.len().min(TITLE_SCAN_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let lines: Vec<&str> = text[..end].lines().collect();
    // Front matter is skipped; a first `---` without a closing line is text.
    let start = match lines.first() {
        Some(first) if first.trim_end() == "---" => lines[1..]
            .iter()
            .position(|l| matches!(l.trim_end(), "---" | "..."))
            .map_or(0, |close| close + 2),
        _ => 0,
    };
    lines[start..].iter().find_map(|line| file_name_of(line))
}

/// `line` as a file name: heading marks dropped, characters no platform
/// allows in names left out, spaces collapsed; `None` when nothing is
/// left.
fn file_name_of(line: &str) -> Option<String> {
    let title = line.trim().trim_start_matches('#').trim();
    if title
        .chars()
        .all(|c| matches!(c, '-' | '=' | '*' | '_' | ' '))
    {
        // Empty, a thematic break or a setext underline.
        return None;
    }
    let mut name = String::new();
    let mut space = false;
    for ch in title.chars() {
        let unsafe_char =
            ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|');
        if unsafe_char || ch.is_whitespace() {
            space = !name.is_empty();
            continue;
        }
        if space {
            name.push(' ');
            space = false;
        }
        name.push(ch);
        if name.chars().count() >= MAX_TITLE_CHARS {
            break;
        }
    }
    // Windows drops trailing dots and spaces; a leading dot hides the file.
    let name = name
        .trim_end_matches(['.', ' '])
        .trim_start_matches(['.', ' ']);
    (!name.is_empty()).then(|| format!("{name}.{MARKDOWN_EXTENSION}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{suggested_name, with_markdown_extension};

    #[test]
    fn a_name_without_an_extension_gets_md() {
        let cases = [
            ("/n/notes", "/n/notes.md", true),
            ("/n/notes.", "/n/notes.md", true),
            ("/n/notes.md", "/n/notes.md", false),
            ("/n/notes.txt", "/n/notes.txt", false),
            ("/n/.hidden", "/n/.hidden.md", true),
            ("/n/v1.2 draft.markdown", "/n/v1.2 draft.markdown", false),
        ];
        for (given, expected, changed) in cases {
            assert_eq!(
                with_markdown_extension(PathBuf::from(given)),
                (PathBuf::from(expected), changed),
                "{given}"
            );
        }
    }

    #[test]
    fn the_title_names_an_untitled_document() {
        let cases = [
            ("# Shopping list\n\n- milk\n", Some("Shopping list.md")),
            (
                "\n\n  Plain first line  \nmore",
                Some("Plain first line.md"),
            ),
            ("---\ntitle: x\n---\n## Notes: a/b?\n", Some("Notes a b.md")),
            ("---\nnot front matter\n", Some("not front matter.md")),
            ("#\n...\n", None),
            ("", None),
            ("   \n#  \n", None),
            ("日本語のメモ\n", Some("日本語のメモ.md")),
        ];
        for (text, expected) in cases {
            assert_eq!(suggested_name(text).as_deref(), expected, "{text:?}");
        }
        let long = "x".repeat(300);
        assert_eq!(suggested_name(&long).map(|n| n.chars().count()), Some(83));
    }
}

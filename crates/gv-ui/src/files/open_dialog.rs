// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Open file and Open folder: asking which files or folder to open.
//!
//! The question goes to an [`OpenPathDialog`], by default the platform's
//! own open dialog through GPUI (`App::prompt_for_paths`): the XDG desktop
//! portal's FileChooser on Linux, `NSOpenPanel` on macOS and
//! `IFileOpenDialog` on Windows. It answers asynchronously, so the UI
//! thread never waits for it. Tests install their own dialog with
//! [`set_dialog`]; in unit tests that install none, the dialog answers as
//! if cancelled, so no test can show a real one.

use std::path::PathBuf;
use std::rc::Rc;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{App, Global};

/// What the open dialog asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenPrompt {
    /// One or more files.
    Files,
    /// One folder.
    Folder,
}

/// The dialog's answer: the chosen paths, `None` when cancelled, or why it
/// could not be shown.
pub type OpenAnswer = Result<Option<Vec<PathBuf>>, String>;

/// Asks the user which files or folder to open.
pub trait OpenPathDialog {
    /// Shows the dialog. The future resolves once the user answered; the
    /// UI thread does not wait for it.
    fn ask(&self, prompt: OpenPrompt, cx: &mut App) -> LocalBoxFuture<'static, OpenAnswer>;
}

/// The platform's open dialog, through GPUI.
#[cfg_attr(test, allow(dead_code))]
struct PlatformDialog;

impl OpenPathDialog for PlatformDialog {
    fn ask(&self, prompt: OpenPrompt, cx: &mut App) -> LocalBoxFuture<'static, OpenAnswer> {
        let folder = prompt == OpenPrompt::Folder;
        let answer = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: !folder,
            prompt: None,
        });
        async move {
            match answer.await {
                Ok(Ok(paths)) => Ok(paths),
                Ok(Err(err)) => Err(err.to_string()),
                // The platform dropped the question: as if cancelled.
                Err(_) => Ok(None),
            }
        }
        .boxed_local()
    }
}

/// The dialog of unit tests that installed none: always cancelled.
#[cfg(test)]
struct CancelledDialog;

#[cfg(test)]
impl OpenPathDialog for CancelledDialog {
    fn ask(&self, _: OpenPrompt, _: &mut App) -> LocalBoxFuture<'static, OpenAnswer> {
        async { Ok(None) }.boxed_local()
    }
}

/// The dialog in use, when not the platform's.
struct InstalledDialog(Rc<dyn OpenPathDialog>);

impl Global for InstalledDialog {}

/// Uses `dialog` instead of the platform's open dialog (tests).
pub fn set_dialog(cx: &mut App, dialog: impl OpenPathDialog + 'static) {
    cx.set_global(InstalledDialog(Rc::new(dialog)));
}

/// The open dialog: the installed one, or the platform's.
pub(crate) fn dialog(cx: &App) -> Rc<dyn OpenPathDialog> {
    match cx.try_global::<InstalledDialog>() {
        Some(installed) => installed.0.clone(),
        #[cfg(not(test))]
        None => Rc::new(PlatformDialog),
        #[cfg(test)]
        None => Rc::new(CancelledDialog),
    }
}

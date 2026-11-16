// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! *Export as HTML…*, *Open in browser* and *Copy as rich text*
//! (FR-EXPORT-01, -04, -06; see [`crate::export`]).
//!
//! - **Export as HTML…** asks where to save (the platform's save dialog,
//!   in the document's folder with its name and `.html`), then exports
//!   the tab's text off the UI thread: the editor's text when the tab has
//!   one (saved or not), else the file's. The file is written whole.
//! - **Open in browser** exports to a new private temporary folder and
//!   hands the file to the system's default app for HTML.
//! - **Copy as rich text** copies the selection of the view with the
//!   focus (the Reading view's, or the editor's) as HTML with a plain-text
//!   alternative.
//!
//! Failures show in the tab's infobar (not designed).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext as _, ClipboardItem, Context, Focusable as _, Window};
use gv_profile::CompiledProfile;
use gv_vault::PathIndex;

use super::{ViewMode, Workspace};
use crate::export;
use crate::files::notice::Notice;
use crate::files::save_as::{self, SavePrompt};
use crate::platform;

/// What an export works from.
struct Source {
    text: String,
    path: Option<PathBuf>,
    profile: Arc<CompiledProfile>,
    index: Option<PathIndex>,
    root: Option<PathBuf>,
}

impl Workspace {
    /// The document of tab `index`: its editor's text, else its file's.
    fn export_source(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) -> Result<Source, String> {
        let tab = self.tabs.get(index).ok_or("no document")?;
        let path = tab
            .path
            .clone()
            .map(|p| std::path::absolute(&p).unwrap_or(p));
        let edited = tab
            .editor
            .as_ref()
            .and_then(|e| e.read(cx).editor().map(|e| e.text().to_string()));
        let text = match (edited, &path) {
            (Some(text), _) => text,
            (None, Some(path)) => {
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                gv_text::decode(&bytes).map_err(|e| e.to_string())?.text
            }
            (None, None) => return Err("the tab has no document".to_owned()),
        };
        let profile = self.tab_profile(index, cx).compiled;
        let vault = self.vault.as_ref().map(|v| v.read(cx));
        let index = vault.map(|v| v.snapshot());
        let root = vault.map(|v| v.root().to_path_buf());
        Ok(Source {
            text,
            path,
            profile,
            index,
            root,
        })
    }

    /// `file.export_html`.
    pub(super) fn export_html_command(&mut self, cx: &mut Context<'_, Self>) {
        let tab = self.command_tab(cx);
        let source = match self.export_source(tab, cx) {
            Ok(source) => source,
            Err(reason) => return self.ask(Notice::export_failed(&reason), cx),
        };
        let directory = source
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| source.root.clone())
            .or_else(gv_vault::root::home_dir)
            .unwrap_or_default();
        let prompt = SavePrompt {
            directory,
            suggested_name: export::suggested_name(source.path.as_deref()),
        };
        let answer = save_as::dialog(cx).ask(&prompt, cx);
        cx.spawn(async move |this, cx| {
            let target = match answer.await {
                Ok(Some(target)) => export::with_html_extension(target),
                Ok(None) => return,
                Err(reason) => {
                    this.update(cx, |ws, cx| ws.ask(Notice::save_dialog_failed(&reason), cx))
                        .ok();
                    return;
                }
            };
            let written = cx
                .background_spawn(async move {
                    let export = export::export_text(
                        &source.text,
                        source.path.as_deref(),
                        source.root.as_deref(),
                        &source.profile,
                        source.index.as_ref(),
                    );
                    export::write_export(&target, &export)
                        .map(|()| export.notes.len())
                        .map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |ws, cx| match written {
                Ok(0) => {}
                Ok(skipped) => ws.ask(Notice::export_images_skipped(skipped), cx),
                Err(reason) => ws.ask(Notice::export_failed(&reason), cx),
            })
            .ok();
        })
        .detach();
    }

    /// `file.open_in_browser`.
    pub(super) fn open_in_browser_command(&mut self, cx: &mut Context<'_, Self>) {
        let tab = self.command_tab(cx);
        let source = match self.export_source(tab, cx) {
            Ok(source) => source,
            Err(reason) => return self.ask(Notice::export_failed(&reason), cx),
        };
        let name = export::suggested_name(source.path.as_deref());
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_spawn(async move {
                    let export = export::export_text(
                        &source.text,
                        source.path.as_deref(),
                        source.root.as_deref(),
                        &source.profile,
                        source.index.as_ref(),
                    );
                    export::write_temporary(&export, &name).map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |ws, cx| match written {
                Ok(path) => platform::launcher(cx).open_with_default_app(&path, cx),
                Err(reason) => ws.ask(Notice::export_failed(&reason), cx),
            })
            .ok();
        })
        .detach();
    }

    /// `editor.copy_as_rich_text`: the selection of the focused view (or,
    /// without one, of the view the mode shows).
    pub(super) fn copy_as_rich_text_command(
        &mut self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) {
        let index = self.active;
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let split = tab.split.as_ref().map(|s| {
            let s = s.read(cx);
            (s.editor().clone(), s.reader().clone())
        });
        let editors: Vec<_> = tab
            .editor
            .iter()
            .cloned()
            .chain(split.iter().map(|s| s.0.clone()))
            .collect();
        let readers: Vec<_> = tab
            .reader
            .iter()
            .cloned()
            .chain(split.iter().map(|s| s.1.clone()))
            .collect();
        let focused_editor = editors
            .iter()
            .find(|e| e.focus_handle(cx).contains_focused(window, cx))
            .cloned();
        let focused_reader = readers
            .iter()
            .find(|r| r.focus_handle(cx).contains_focused(window, cx))
            .cloned();
        let use_editor = match (&focused_editor, &focused_reader) {
            (Some(_), _) => true,
            (None, Some(_)) => false,
            (None, None) => self.mode() != ViewMode::Preview,
        };
        let path = tab
            .path
            .clone()
            .map(|p| std::path::absolute(&p).unwrap_or(p));
        let selected = if use_editor {
            focused_editor
                .or_else(|| editors.first().cloned())
                .and_then(|e| {
                    let view = e.read(cx);
                    let editor = view.editor()?;
                    let selections = editor.selections();
                    let range = selections.ranges().nth(selections.primary_index())?;
                    Some((editor.text().to_string(), range))
                })
        } else {
            focused_reader
                .or_else(|| readers.first().cloned())
                .and_then(|r| r.read(cx).selection_source())
                .map(|(text, range)| (text.to_string(), range))
        };
        let Some((text, range)) = selected.filter(|(_, r)| !r.is_empty()) else {
            return;
        };
        let profile = self.tab_profile(index, cx).compiled;
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let copy = export::rich_copy(&text, range, path.as_deref(), &profile, snapshot.as_ref());
        let written = match export::rich_clipboard(cx) {
            Some(clipboard) => clipboard.write(&copy.html, &copy.text),
            None => Err("no rich-text clipboard".to_owned()),
        };
        if let Err(reason) = written {
            tracing::info!("copied as plain text: {reason}");
            cx.write_to_clipboard(ClipboardItem::new_string(copy.text));
            self.ask(Notice::rich_copy_plain(&reason), cx);
        }
    }
}

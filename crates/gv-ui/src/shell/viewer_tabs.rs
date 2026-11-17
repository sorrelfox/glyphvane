// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Tabs that show a file that is not a note, a canvas or a base: the
//! built-in viewer ([`crate::file_viewer`]) takes the place of the Reading
//! view.
//!
//! - Text and code files and images open as tabs from the Files panel, the
//!   quick switcher, File ▸ Open, drops and links
//!   ([`super::canvas_tabs::opens_as_tab`], by name). Any other file that
//!   is not a known binary kind is sniffed first, off the UI thread: when
//!   its first bytes look like text it opens in the text viewer too;
//!   otherwise the window asks before handing it to the default app
//!   ([`Workspace::open_other_file`]).
//! - The viewer has no Source view: Edit and Split are unavailable in its
//!   tab ([`Workspace::can_edit`]).
//! - The file is watched like a note shown in Preview and read again when
//!   it changes on disk.
//! - The viewer's *Open with default app* goes through the same question.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Entity, Window};

use super::Workspace;
use crate::file_viewer::{FileViewer, FileViewerEvent};
use crate::files::notice::Notice;
use crate::platform::{self, FileClass};

/// Whether a tab showing `path` shows it in the built-in viewer: anything
/// but a note, a canvas or a base.
pub(crate) fn is_viewer_file(path: &Path) -> bool {
    !platform::is_markdown(path)
        && !super::canvas_tabs::is_canvas(path)
        && !super::bases_tabs::is_base(path)
}

/// Extensions of files that are never text: no need to look inside before
/// asking about the default app.
const BINARY_EXTENSIONS: &[&str] = &[
    "pdf", "zip", "gz", "tgz", "bz2", "xz", "zst", "tar", "7z", "rar", "docx", "xlsx", "pptx",
    "doc", "xls", "ppt", "odt", "ods", "odp", "epub", "mp3", "m4a", "wav", "flac", "ogg", "oga",
    "opus", "aac", "mp4", "m4v", "mov", "mkv", "webm", "avi", "ogv", "avif", "heic", "heif", "jxl",
    "psd", "ttf", "otf", "woff", "woff2", "sqlite", "db", "iso", "dmg", "exe", "dll", "so",
    "dylib", "o", "a", "class", "jar", "wasm", "bin",
];

/// Whether the name of `path` says it is not text.
fn known_binary(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| BINARY_EXTENSIONS.iter().any(|b| b.eq_ignore_ascii_case(e)))
}

impl Workspace {
    /// The viewer of tab `index`, which shows `path`: made on first use, or
    /// loading `path` when the tab showed another file.
    pub(super) fn ensure_viewer(
        &mut self,
        index: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<FileViewer>> {
        let tab = self.tabs.get(index)?;
        if let Some(viewer) = tab.viewer.clone() {
            if viewer.read(cx).path() != path.as_path() {
                viewer.update(cx, |v, cx| v.load(path.clone(), cx));
                self.track_file(&path, None, cx);
            }
            return Some(viewer);
        }
        let root = self.link_vault_root(cx);
        let viewer = FileViewer::open(path.clone(), root, cx);
        cx.subscribe_in(
            &viewer,
            window,
            |this, _, event: &FileViewerEvent, _, cx| match event {
                FileViewerEvent::OpenExternally(path) => this.ask_default_app(path.clone(), cx),
            },
        )
        .detach();
        self.tabs[index].viewer = Some(viewer.clone());
        self.track_file(&path, None, cx);
        Some(viewer)
    }

    /// The vault (and so where SVG images may load files from) is known.
    pub(super) fn sync_viewer_roots(&self, cx: &mut Context<'_, Self>) {
        let root = self.link_vault_root(cx);
        for viewer in self.tabs.iter().filter_map(|t| t.viewer.clone()) {
            let root = root.clone();
            viewer.update(cx, |v, _| v.set_root(root));
        }
    }

    /// The file of tab `index` changed on disk: it is read again.
    pub(super) fn reload_viewer(&self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if let (Some(viewer), Some(path)) = (tab.viewer.clone(), tab.path.clone()) {
            viewer.update(cx, |v, cx| v.load(path, cx));
        }
    }

    /// A file Glyphvane does not show by its name: one whose first bytes
    /// look like text opens in the text viewer; anything else is asked
    /// about ([`Self::ask_default_app`]). The file is read off the UI
    /// thread.
    pub(super) fn open_unknown_file(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        if known_binary(&path) || platform::classify(&path) != FileClass::Other {
            self.ask_default_app(path, cx);
            return;
        }
        let handle = self.window_handle;
        let sniff = cx.background_spawn({
            let path = path.clone();
            async move { crate::file_viewer::sniff_text(&path) }
        });
        cx.spawn(async move |this, cx| {
            let text = sniff.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| {
                    if text {
                        ws.open_path(path, window, cx);
                    } else {
                        ws.ask_default_app(path, cx);
                    }
                })
            });
        })
        .detach();
    }

    /// The question before a file goes to another app: a program is only
    /// offered to be revealed; anything else opens with the default app
    /// once confirmed.
    pub(super) fn ask_default_app(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        let notice = match platform::classify(&path) {
            FileClass::Executable => Notice::executable(&path),
            FileClass::Html => Notice::confirm_open_file(&path, true),
            FileClass::Markdown | FileClass::Other => Notice::confirm_open_file(&path, false),
        };
        self.ask(notice, cx);
    }
}

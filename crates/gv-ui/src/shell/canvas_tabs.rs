// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Tabs that show a `.canvas` file: the read-only Canvas view
//! ([`crate::canvas_view`]) takes the place of the Reading view.
//!
//! - A tab whose file is a canvas gets a Canvas view instead of a Reading
//!   view when it is first shown; it has no Source view, so Edit and Split
//!   are unavailable in it ([`Workspace::can_edit`]).
//! - The Files panel, the quick switcher, File ▸ Open and links all open
//!   canvas files as tabs; a rename or move shows the tab's file at its new
//!   place like any other tab.
//! - A file card opens its note (or canvas) like a followed link; any other
//!   file goes through the window's *open with the default app* question.
//!   A link card follows its URL as a link of the canvas file.
//! - The file is watched like a note shown in Preview and read again when
//!   it changes on disk.

use std::path::{Path, PathBuf};

use gpui::{Context, Entity, Window};

use super::Workspace;
use crate::canvas_view::{CanvasEvent, CanvasView};
use crate::reader::ScrollTarget;
use crate::reader::link_info::LinkAt;

/// Whether `path` is a canvas file, by its extension.
pub(crate) fn is_canvas(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("canvas"))
}

/// Whether the window opens `path` as a tab: a note, a canvas, a base, or
/// a file the built-in viewers show by its name (text, code, images).
pub(crate) fn opens_as_tab(path: &Path) -> bool {
    crate::platform::is_markdown(path)
        || is_canvas(path)
        || super::bases_tabs::is_base(path)
        || crate::file_viewer::kind(path).is_some()
}

impl Workspace {
    /// The Canvas view of tab `index`, which shows `path`: made on first
    /// use, or loading `path` when the tab showed another canvas.
    pub(super) fn ensure_canvas(
        &mut self,
        index: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<CanvasView>> {
        let tab = self.tabs.get(index)?;
        if let Some(canvas) = tab.canvas.clone() {
            if canvas.read(cx).path() != Some(path.as_path()) {
                canvas.update(cx, |c, cx| c.load(path.clone(), cx));
                self.track_file(&path, None, cx);
            }
            return Some(canvas);
        }
        let root = self.link_vault_root(cx);
        let canvas = CanvasView::open(path.clone(), root, cx);
        cx.subscribe_in(
            &canvas,
            window,
            |this, canvas, event: &CanvasEvent, window, cx| {
                this.canvas_event(canvas, event.clone(), window, cx);
            },
        )
        .detach();
        self.tabs[index].canvas = Some(canvas.clone());
        self.track_file(&path, None, cx);
        if let Some(vault) = &self.vault {
            let (index, content) = {
                let v = vault.read(cx);
                (v.snapshot(), v.content())
            };
            canvas.update(cx, |c, cx| {
                c.set_index(Some(index), cx);
                c.set_content(content, cx);
            });
        }
        // Queries and bases in cards read the vault: the vault is needed.
        if index == self.active {
            self.ensure_vault(cx);
        }
        Some(canvas)
    }

    /// A Canvas view asked for a file or a link.
    fn canvas_event(
        &mut self,
        canvas: &Entity<CanvasView>,
        event: CanvasEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.canvas.as_ref() == Some(canvas))
        else {
            return;
        };
        match event {
            CanvasEvent::OpenFile {
                path,
                fragment,
                disposition,
            } => {
                if opens_as_tab(&path) {
                    let scroll = fragment.map(ScrollTarget::Fragment);
                    self.open_link_target(index, path, scroll, disposition, window, cx);
                } else {
                    self.activate_tab(index, window, cx);
                    self.open_other_file(path, cx);
                }
            }
            CanvasEvent::FollowLink(request) => {
                let Some(file) = self.tabs[index].path.clone() else {
                    return;
                };
                let link = LinkAt {
                    file,
                    kind: request.kind,
                    image: false,
                    destination: request.destination,
                    range: None,
                    construct: None,
                    profile: gv_vault::ProfileId::Obsidian,
                };
                self.follow_link_at(index, link, request.disposition, false, window, cx);
            }
        }
    }

    /// The vault (and so the root of the file cards' paths) is known.
    pub(super) fn sync_canvas_roots(&self, cx: &mut Context<'_, Self>) {
        let root = self.link_vault_root(cx);
        for canvas in self.tabs.iter().filter_map(|t| t.canvas.clone()) {
            let root = root.clone();
            canvas.update(cx, |c, cx| c.set_vault_root(root, cx));
        }
    }

    /// The cards' live blocks (queries, bases, embeds) use the vault's
    /// path index and content index: every Canvas view gets each new
    /// snapshot.
    pub(super) fn push_canvas_content(&self, cx: &mut Context<'_, Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let canvases: Vec<_> = self.tabs.iter().filter_map(|t| t.canvas.clone()).collect();
        if canvases.is_empty() {
            return;
        }
        let (index, content) = {
            let v = vault.read(cx);
            (v.snapshot(), v.content())
        };
        for canvas in canvases {
            let (index, content) = (index.clone(), content.clone());
            canvas.update(cx, |c, cx| {
                c.set_index(Some(index), cx);
                c.set_content(content, cx);
            });
        }
    }

    /// The canvas file of tab `index` changed on disk: it is read again.
    pub(super) fn reload_canvas(&self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if let (Some(canvas), Some(path)) = (tab.canvas.clone(), tab.path.clone()) {
            canvas.update(cx, |c, cx| c.load(path, cx));
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Tabs that show a `.base` file: the read-only Bases view
//! ([`crate::bases_view`]) takes the place of the Reading view.
//!
//! - A tab whose file is a base gets a Bases view instead of a Reading
//!   view when it is first shown; it has no Source view, so Edit and Split
//!   are unavailable in it ([`Workspace::can_edit`]).
//! - The Files panel, the quick switcher, File ▸ Open and links all open
//!   base files as tabs; a rename or move shows the tab's file at its new
//!   place like any other tab.
//! - The files a base lists come from the window vault's content index:
//!   the view gets the current snapshot when it is made, and each new one
//!   the vault announces (also when the vault is made after the tab).
//! - A file value opens its note (or canvas, or base) like a followed
//!   link; any other file goes through the window's *open with the
//!   default app* question. A link value or a URL is followed as a link
//!   of the base file.
//! - The file is watched like a note shown in Preview and read again when
//!   it changes on disk.

use std::path::{Path, PathBuf};

use gpui::{Context, Entity, Window};

use super::Workspace;
use super::canvas_tabs::opens_as_tab;
use crate::bases_view::{BasesEvent, BasesView};
use crate::reader::ScrollTarget;
use crate::reader::link_info::LinkAt;

/// Whether `path` is a base file, by its extension.
pub(crate) fn is_base(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("base"))
}

impl Workspace {
    /// The Bases view of tab `index`, which shows `path`: made on first
    /// use, or loading `path` when the tab showed another base.
    pub(super) fn ensure_base(
        &mut self,
        index: usize,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<BasesView>> {
        let tab = self.tabs.get(index)?;
        if let Some(base) = tab.bases.clone() {
            if base.read(cx).path() != Some(path.as_path()) {
                base.update(cx, |b, cx| b.load(path.clone(), cx));
                self.track_file(&path, None, cx);
            }
            return Some(base);
        }
        let root = self.link_vault_root(cx);
        let base = BasesView::open(path.clone(), root, cx);
        cx.subscribe_in(
            &base,
            window,
            |this, base, event: &BasesEvent, window, cx| {
                this.bases_event(base, event.clone(), window, cx);
            },
        )
        .detach();
        self.tabs[index].bases = Some(base.clone());
        self.track_file(&path, None, cx);
        if let Some(content) = self.vault.as_ref().and_then(|v| v.read(cx).content()) {
            base.update(cx, |b, cx| b.set_content(content, cx));
        }
        // A base lists the vault's files: the vault is needed.
        if index == self.active {
            self.ensure_vault(cx);
        }
        Some(base)
    }

    /// A Bases view asked for a file or a link.
    fn bases_event(
        &mut self,
        base: &Entity<BasesView>,
        event: BasesEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.bases.as_ref() == Some(base))
        else {
            return;
        };
        match event {
            BasesEvent::OpenFile {
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
            BasesEvent::FollowLink(request) => {
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

    /// The vault (and so the root of the bases' paths) is known.
    pub(super) fn sync_base_roots(&self, cx: &mut Context<'_, Self>) {
        let root = self.link_vault_root(cx);
        for base in self.tabs.iter().filter_map(|t| t.bases.clone()) {
            let root = root.clone();
            base.update(cx, |b, cx| b.set_vault_root(root, cx));
        }
    }

    /// The vault's content index published a snapshot: each base lists
    /// its files again.
    pub(super) fn push_bases_content(&self, cx: &mut Context<'_, Self>) {
        let bases: Vec<_> = self.tabs.iter().filter_map(|t| t.bases.clone()).collect();
        if bases.is_empty() {
            return;
        }
        let Some(content) = self.vault.as_ref().and_then(|v| v.read(cx).content()) else {
            return;
        };
        for base in bases {
            let content = content.clone();
            base.update(cx, |b, cx| b.set_content(content, cx));
        }
    }

    /// The base file of tab `index` changed on disk: it is read again.
    pub(super) fn reload_base(&self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if let (Some(base), Some(path)) = (tab.bases.clone(), tab.path.clone()) {
            base.update(cx, |b, cx| b.load(path, cx));
        }
    }
}

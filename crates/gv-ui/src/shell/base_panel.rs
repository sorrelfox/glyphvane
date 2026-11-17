// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The sidebar base (08 § Bases view) in the right panel: a base whose
//! `this` is the file of the active tab ([`crate::bases_view::BasesPanel`]).
//!
//! - `workspace.show_base` (View ▸ Base) shows the right panel with the
//!   base section, made on first use; it stays below the other sections.
//! - The window feeds it: each content-index snapshot (the bases offered
//!   and the files listed), the vault folder, and the active tab's file
//!   whenever the tab changes.
//! - The chosen base file is read again when the index sees it change on
//!   disk (its size or modification time).
//! - Its events are handled as a base tab's are: a file opens from the
//!   active tab like a followed link; a link value is followed as a link
//!   written in the base file.
//!
//! Not designed (built plainly, flagged): the section's place and frame.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use gpui::{Context, Entity, Window};

use super::Workspace;
use super::canvas_tabs::opens_as_tab;
use crate::bases_view::{BasesEvent, BasesPanel};
use crate::reader::ScrollTarget;
use crate::reader::link_info::LinkAt;

/// The chosen base file as last read: vault path, size, mtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BaseSeen {
    path: Arc<str>,
    size: u64,
    mtime: Option<SystemTime>,
}

impl Workspace {
    /// The sidebar base, made on first use and given what the window
    /// knows.
    pub(super) fn ensure_base_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<BasesPanel> {
        if let Some(panel) = &self.panels.base {
            return panel.clone();
        }
        // The panel lists the vault's bases: the vault is needed.
        self.ensure_vault(cx);
        let panel = BasesPanel::new(self.link_vault_root(cx), cx);
        self.panels._base_events = vec![
            cx.subscribe_in(&panel, window, |this, _, event: &BasesEvent, window, cx| {
                this.base_panel_event(event.clone(), window, cx);
            }),
            // A base chosen: its file as it is now, to notice changes.
            cx.observe(&panel, |this, _, cx| this.note_base_file(cx)),
        ];
        self.panels.base = Some(panel.clone());
        self.sync_base_panel(cx);
        self.push_base_panel_content(cx);
        panel
    }

    /// Tells the sidebar base the vault folder and the active tab's file.
    pub(super) fn sync_base_panel(&mut self, cx: &mut Context<'_, Self>) {
        let Some(panel) = self.panels.base.clone() else {
            return;
        };
        let root = self.link_vault_root(cx);
        let active = self.active_tab().path.clone();
        panel.update(cx, |p, cx| {
            p.set_vault_root(root, cx);
            p.set_active_path(active.as_deref(), cx);
        });
    }

    /// The chosen base's file as the index has it now, unless it is the
    /// one already noted.
    fn note_base_file(&mut self, cx: &mut Context<'_, Self>) {
        let Some(panel) = self.panels.base.clone() else {
            return;
        };
        let Some(chosen) = panel.read(cx).chosen().map(Arc::<str>::from) else {
            return;
        };
        if self
            .panels
            .base_seen
            .as_ref()
            .is_some_and(|seen| seen.path == chosen)
        {
            return;
        }
        let content = self.vault.as_ref().and_then(|v| v.read(cx).content());
        self.panels.base_seen = content.and_then(|c| base_seen(&c, &chosen));
    }

    /// The content index published a snapshot: the sidebar base offers the
    /// vault's bases and lists its files again, and reads its base file
    /// again when it changed.
    pub(super) fn push_base_panel_content(&mut self, cx: &mut Context<'_, Self>) {
        let Some(panel) = self.panels.base.clone() else {
            return;
        };
        let Some(content) = self.vault.as_ref().and_then(|v| v.read(cx).content()) else {
            return;
        };
        let seen = panel
            .read(cx)
            .chosen()
            .and_then(|chosen| base_seen(&content, chosen));
        panel.update(cx, |p, cx| p.set_content(content, cx));
        let changed = match (&self.panels.base_seen, &seen) {
            (Some(old), Some(new)) => old.path == new.path && old != new,
            _ => false,
        };
        self.panels.base_seen = seen;
        if changed {
            panel.update(cx, BasesPanel::reload);
        }
    }

    /// The sidebar base asked for a file or a link: it opens from the
    /// active tab.
    fn base_panel_event(
        &mut self,
        event: BasesEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let index = self.active;
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
                    self.open_other_file(path, cx);
                }
            }
            BasesEvent::FollowLink(request) => {
                let Some(file) = self.base_panel_file(cx) else {
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

    /// The chosen base's file on disk.
    fn base_panel_file(&self, cx: &Context<'_, Self>) -> Option<PathBuf> {
        let chosen = self.panels.base.as_ref()?.read(cx).chosen()?.to_owned();
        let mut path = self.link_vault_root(cx)?;
        path.extend(chosen.split('/').filter(|s| !s.is_empty()));
        Some(path)
    }

    /// The sidebar base, once shown.
    #[cfg(test)]
    pub(crate) fn base_panel(&self) -> Option<Entity<BasesPanel>> {
        self.panels.base.clone()
    }
}

/// The file `path` of the vault as `content` has it.
fn base_seen(content: &gv_vault::content::ContentSnapshot, path: &str) -> Option<BaseSeen> {
    let entry = content.path_index()?.entry_by_path(path)?;
    Some(BaseSeen {
        path: Arc::from(path),
        size: entry.meta.size,
        mtime: entry.meta.mtime,
    })
}

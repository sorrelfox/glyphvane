// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of the Sync conflicts panel ([`crate::conflicts`]):
//! it is made when first shown (View ▸ Sync conflicts), follows the
//! vault's content index, and does what its entries ask.
//!
//! - **Compare** opens the original (its tab, or a new one) and compares
//!   it with the copy in the compare and merge view, as the sync-conflict
//!   infobar does ([`Workspace::compare_copy`]).
//! - **Open copy** opens the copy as a tab.
//! - **Keep original** and **Use copy** move files to the window's trash;
//!   open tabs follow through the file watcher (a reload, or the deleted
//!   or conflict infobar).
//! - Esc gives the keyboard back to the document.

use gpui::{AppContext as _, Context, Entity, Window};

use super::Workspace;
use crate::conflicts::{ConflictsScope, SyncConflictsEvent, SyncConflictsPanel};

impl Workspace {
    /// The Sync conflicts panel, made on first use (with the window's
    /// vault).
    pub(super) fn ensure_conflicts_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<SyncConflictsPanel> {
        if let Some(conflicts) = &self.panels.conflicts {
            return conflicts.clone();
        }
        let trash = super::vault_ops::trash_can(cx);
        let conflicts = cx.new(|cx| {
            let mut panel = SyncConflictsPanel::new(cx);
            panel.set_trash(trash);
            panel
        });
        self.panels._conflicts_events = Some(cx.subscribe_in(
            &conflicts,
            window,
            |this, _, event: &SyncConflictsEvent, window, cx| {
                this.conflicts_event(event.clone(), window, cx);
            },
        ));
        self.panels.conflicts = Some(conflicts.clone());
        self.ensure_vault(cx);
        self.sync_conflicts(cx);
        conflicts
    }

    /// The panel lists the copies of the vault's latest content snapshot.
    pub(super) fn sync_conflicts(&mut self, cx: &mut Context<'_, Self>) {
        let Some(conflicts) = self.panels.conflicts.clone() else {
            return;
        };
        let scope = self.vault.as_ref().map(|v| {
            let v = v.read(cx);
            ConflictsScope {
                root: v.root().to_path_buf(),
                content: v.content(),
            }
        });
        conflicts.update(cx, |c, cx| c.set_scope(scope, cx));
    }

    fn conflicts_event(
        &mut self,
        event: SyncConflictsEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            SyncConflictsEvent::Compare { original, copy } => {
                self.open_path(original, window, cx);
                self.compare_copy(copy, cx);
                cx.notify();
            }
            SyncConflictsEvent::Open(copy) => self.open_path(copy, window, cx),
            SyncConflictsEvent::Resolved { original, copy, .. } => {
                tracing::info!(
                    "sync conflict settled: {} and {}",
                    original.display(),
                    copy.display()
                );
            }
            SyncConflictsEvent::Failed(why) => {
                tracing::warn!("sync conflict not settled: {why}");
            }
            SyncConflictsEvent::Dismiss => self.focus_document(window, cx),
        }
    }

    /// The Sync conflicts panel, once made (tests).
    #[cfg(test)]
    pub(crate) fn conflicts_panel(&self) -> Option<Entity<SyncConflictsPanel>> {
        self.panels.conflicts.clone()
    }
}

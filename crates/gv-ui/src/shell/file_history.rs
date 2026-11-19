// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of the File history panel ([`crate::history`]): it is
//! made when first shown (`file.history`: File ▸ File history, More
//! actions, the palette), shows the versions of the active tab's file and
//! follows every change of the active tab, and does what its entries ask.
//!
//! - **Compare with current** opens the file (its tab, or a new one) and
//!   hands the version to its Source view
//!   ([`crate::editor::EditorView::compare_with_version`]), once that view
//!   has loaded.
//! - **Restore** does the same with
//!   [`crate::editor::EditorView::restore_version`]: the whole text
//!   replaced as one undoable edit; the tab goes to Edit so the change
//!   shows. Nothing is written until the document is saved.
//! - Esc gives the keyboard back to the document.
//!
//! Renames and moves carry a file's history along
//! ([`Workspace::carry_history`], called by the rename and move paths), and
//! so do renames by other programs that the vault watcher reports, when
//! the file is recognisably the same note
//! ([`Workspace::carry_history_from_outside`]).

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, Div, Entity, EntityId, InteractiveElement as _, SharedString,
    Stateful, Window,
};
use gv_vault::VaultPath;

use super::panels::SidebarPanel;
use super::{ViewMode, Workspace};
use crate::editor::{EditorStatus, EditorView};
use crate::history::{HistoryEvent, HistoryPanel};
use crate::keymap::actions::file as commands;

/// What a Source view does with a version once it has loaded.
#[derive(Debug, Clone)]
pub(super) enum VersionRequest {
    Compare { label: SharedString, text: Arc<str> },
    Restore { text: Arc<str> },
}

impl VersionRequest {
    fn run(self, editor: &Entity<EditorView>, cx: &mut gpui::App) {
        editor.update(cx, |e, cx| match self {
            Self::Compare { label, text } => e.compare_with_version(label, text, cx),
            Self::Restore { text } => e.restore_version(&text, cx),
        });
    }
}

impl Workspace {
    /// `file.history`: shows the File history panel in the sidebar with
    /// the keyboard in it.
    pub(super) fn show_file_history(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.show_sidebar_panel(SidebarPanel::FileHistory, true, window, cx);
    }

    /// The File history panel, made on first use (on the active tab's
    /// file).
    pub(super) fn ensure_history_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<HistoryPanel> {
        if let Some(history) = &self.panels.history {
            return history.clone();
        }
        let path = self.active_tab().path.clone();
        let history = cx.new(|cx| HistoryPanel::new(path, cx));
        self.panels._history_events = Some(cx.subscribe_in(
            &history,
            window,
            |this, _, event: &HistoryEvent, window, cx| {
                this.history_event(event.clone(), window, cx);
            },
        ));
        self.panels.history = Some(history.clone());
        history
    }

    /// `file.history`'s handler.
    pub(super) fn file_history_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, _: &commands::History, window, cx| {
            this.show_file_history(window, cx);
        }))
    }

    /// The panel shows the versions of the active tab's file.
    pub(super) fn sync_file_history(&mut self, cx: &mut Context<'_, Self>) {
        let Some(history) = self.panels.history.clone() else {
            return;
        };
        let path = self.active_tab().path.clone();
        history.update(cx, |h, cx| h.set_path(path, cx));
    }

    fn history_event(
        &mut self,
        event: HistoryEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            HistoryEvent::Compare {
                path, label, text, ..
            } => {
                self.open_path(path, window, cx);
                self.version_for_active(VersionRequest::Compare { label, text }, cx);
                cx.notify();
            }
            HistoryEvent::Restore { path, text, .. } => {
                self.open_path(path, window, cx);
                if self.mode() == ViewMode::Preview && self.mode_available(ViewMode::Edit, cx) {
                    self.set_mode(ViewMode::Edit, cx);
                }
                self.version_for_active(VersionRequest::Restore { text }, cx);
                self.focus_document(window, cx);
                cx.notify();
            }
            HistoryEvent::Dismiss => self.focus_document(window, cx),
        }
    }

    /// Hands `request` to the active tab's Source view (made for a tab
    /// that shows only its Reading view), now or once it has loaded.
    fn version_for_active(&mut self, request: VersionRequest, cx: &mut Context<'_, Self>) {
        let Some(editor) = self.ensure_editor(self.active, cx) else {
            return;
        };
        if *editor.read(cx).status() == EditorStatus::Ready {
            request.run(&editor, cx);
        } else {
            let id = editor.entity_id();
            self.panels.versions_after_load.retain(|(e, _)| *e != id);
            self.panels.versions_after_load.push((id, request));
        }
    }

    /// A Source view has loaded: a version waiting for it is compared or
    /// restored now.
    pub(super) fn version_after_load(
        &mut self,
        editor: &Entity<EditorView>,
        cx: &mut Context<'_, Self>,
    ) {
        let id: EntityId = editor.entity_id();
        if let Some(at) = self
            .panels
            .versions_after_load
            .iter()
            .position(|(e, _)| *e == id)
        {
            let (_, request) = self.panels.versions_after_load.remove(at);
            request.run(editor, cx);
        }
    }

    /// Files were renamed or moved inside the vault at `root`: their
    /// version history and their bookmarks follow them. The history is
    /// moved off the UI thread; the panel reads its list again after.
    pub(super) fn carry_history(
        &mut self,
        root: &std::path::Path,
        moved: &[(VaultPath, VaultPath)],
        cx: &mut Context<'_, Self>,
    ) {
        self.bookmarks_moved(moved, cx);
        self.move_histories(root, moved, false, cx);
    }

    /// Files were renamed or moved inside the vault at `root` by another
    /// program (the vault watcher's renames): a file's version history
    /// follows it when the file still holds the text of its newest
    /// version ([`HistoryStore::moved_from_outside`]), since a rename the
    /// watcher matched by file identity may be another file that reused a
    /// deleted one's identity. A path with a history of its own keeps it,
    /// and swaps or cycles among the renames move nothing.
    ///
    /// [`HistoryStore::moved_from_outside`]: gv_vault::files::history::HistoryStore::moved_from_outside
    pub(super) fn carry_history_from_outside(
        &mut self,
        root: &std::path::Path,
        moved: &[(VaultPath, VaultPath)],
        cx: &mut Context<'_, Self>,
    ) {
        self.move_histories(root, moved, true, cx);
    }

    /// Moves the histories of `moved` off the UI thread (checked against
    /// the files' text when `outside`); the panel reads its list again
    /// after.
    fn move_histories(
        &mut self,
        root: &std::path::Path,
        moved: &[(VaultPath, VaultPath)],
        outside: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(store) = crate::history::store(cx) else {
            return;
        };
        let pairs: Vec<(PathBuf, PathBuf)> = moved
            .iter()
            .map(|(from, to)| (from.to_fs_path(root), to.to_fs_path(root)))
            .collect();
        if pairs.is_empty() {
            return;
        }
        let task = cx.background_spawn(async move {
            let pairs = with_folder_contents(pairs);
            // Outside renames go as one batch, so that swaps and cycles
            // are seen and left alone.
            let results: Vec<std::io::Result<()>> = if outside {
                store
                    .moved_all_from_outside(&pairs)
                    .into_iter()
                    .map(|r| r.map(|_| ()))
                    .collect()
            } else {
                pairs
                    .iter()
                    .map(|(from, to)| store.moved(from, to))
                    .collect()
            };
            for ((from, to), result) in pairs.iter().zip(results) {
                if let Err(err) = result {
                    tracing::warn!(
                        "version history: cannot follow {} to {}: {err}",
                        from.display(),
                        to.display()
                    );
                }
            }
        });
        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |ws, cx| {
                if let Some(history) = ws.panels.history.clone() {
                    history.update(cx, |h, cx| h.reload(cx));
                }
            });
        })
        .detach();
    }

    /// The File history panel, once made (tests).
    #[cfg(test)]
    pub(crate) fn history_panel(&self) -> Option<Entity<HistoryPanel>> {
        self.panels.history.clone()
    }

    /// Whether the File history panel has the keyboard (tests).
    #[cfg(test)]
    pub(crate) fn history_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.panels.history.as_ref().is_some_and(|h| {
            gpui::Focusable::focus_handle(h.read(cx), cx).contains_focused(window, cx)
        })
    }
}

/// `pairs` plus, for each moved folder (already at its new place), every
/// file inside it at its old and new path: a folder's history is its
/// files' histories.
fn with_folder_contents(pairs: Vec<(PathBuf, PathBuf)>) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    for (from, to) in pairs {
        if to.is_dir() {
            let mut stack = vec![to.clone()];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.filter_map(Result::ok) {
                    let path = entry.path();
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push(path);
                    } else if let Ok(rest) = path.strip_prefix(&to) {
                        out.push((from.join(rest), path.clone()));
                    }
                }
            }
        } else {
            out.push((from, to));
        }
    }
    out
}

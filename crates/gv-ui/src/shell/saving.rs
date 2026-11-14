// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of data safety ([`crate::files`]): Save, untitled
//! tabs, closing with unsaved changes, the watchers' events, crash
//! recovery and the Source views' events.

use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui::{App, AppContext as _, Context, Entity, EntityId, Window};
use gv_vault::WatchEvent;
use gv_vault::files::{DiskState, ExternalChange};

use super::{OpenTarget, RecentWindows, Tab, ViewMode, Workspace};
use crate::editor::{DocEvent, EditorView};
use crate::files::notice::{Notice, Topic};
use crate::files::watch::WatchHandle;
use crate::files::{self};

/// Whether `a` and `b` name the same file: equal once made absolute.
pub(super) fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || std::path::absolute(a)
            .ok()
            .zip(std::path::absolute(b).ok())
            .is_some_and(|(a, b)| a == b)
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Workspace {
    // ----- commands -------------------------------------------------------------

    /// `file.save`: saves the active tab's document, in any mode.
    pub fn save_active(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(editor) = self.active_tab().editor.clone() {
            editor.update(cx, |e, cx| e.save(cx));
        }
    }

    /// `tabs.new`: an untitled document in a new tab, in Edit mode. It lives
    /// in the crash journal until it can be saved (Save As is not built).
    pub fn new_tab(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let n = files::next_untitled(cx);
        let index = self.place_tab(Tab::untitled(n));
        self.ensure_editor(index, cx);
        if index != self.active {
            self.leave_tab(self.active, cx);
        }
        self.active = index;
        self.tabs_changed(window, cx);
        self.focus_document(window, cx);
    }

    /// Reopens documents with unsaved changes from an earlier session in
    /// Edit mode (their Source views recover the text from the journal),
    /// and shows the first one.
    pub fn recover(
        &mut self,
        paths: Vec<PathBuf>,
        untitled: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let mut first: Option<EntityId> = None;
        for path in paths {
            let index = match self.tabs.iter().position(|t| t.shows(&path)) {
                Some(index) => index,
                None => self.place_tab(Tab::new(Some(OpenTarget::new(path)))),
            };
            self.tabs[index].mode = ViewMode::Edit;
            if let Some(editor) = self.ensure_editor(index, cx) {
                first.get_or_insert(editor.entity_id());
            }
        }
        for n in untitled {
            let index = self.place_tab(Tab::untitled(n));
            if let Some(editor) = self.ensure_editor(index, cx) {
                first.get_or_insert(editor.entity_id());
            }
        }
        let shown = first.and_then(|id| {
            self.tabs
                .iter()
                .position(|t| t.editor.as_ref().is_some_and(|e| e.entity_id() == id))
        });
        if let Some(index) = shown {
            if index != self.active {
                self.leave_tab(self.active, cx);
            }
            self.active = index;
        }
        self.tabs_changed(window, cx);
        self.focus_document(window, cx);
    }

    // ----- Source views -----------------------------------------------------------

    /// Listens to a new Source view and hands it the watchers.
    pub(super) fn adopt_editor(&mut self, editor: &Entity<EditorView>, cx: &mut Context<'_, Self>) {
        let watch = self.ensure_watch(cx);
        editor.update(cx, |e, _| e.set_watch(watch));
        let subscription = cx.subscribe(editor, |this, editor, event: &DocEvent, cx| {
            this.editor_event(&editor, *event, cx);
        });
        self.editor_events.insert(editor.entity_id(), subscription);
    }

    /// A Source view leaves its tab (the tab closed, or shows another
    /// file): it saves what autosave may save and keeps the rest in its
    /// journal.
    pub(super) fn drop_editor(&mut self, editor: &Entity<EditorView>, cx: &mut Context<'_, Self>) {
        editor.update(cx, |e, cx| {
            e.prepare_close(cx);
        });
        self.editor_events.remove(&editor.entity_id());
    }

    fn editor_event(
        &mut self,
        editor: &Entity<EditorView>,
        event: DocEvent,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            DocEvent::Loaded => {
                if let Some((path, baseline)) = editor.read(cx).disk_baseline() {
                    self.track_file(&path, Some(baseline), cx);
                }
            }
            DocEvent::Reloaded => self.refresh_reader(editor, cx),
            DocEvent::CloseRequested => {
                let id = editor.entity_id();
                let window = self.window_handle;
                cx.spawn(async move |this, cx| {
                    let _ = cx.update_window(window, |_, window, cx| {
                        this.update(cx, |ws, cx| {
                            let index = ws.tabs.iter().position(|t| {
                                t.editor.as_ref().is_some_and(|e| e.entity_id() == id)
                            });
                            if let Some(index) = index {
                                ws.close_tab(index, window, cx);
                            }
                        })
                    });
                })
                .detach();
            }
            DocEvent::ModifiedChanged | DocEvent::NoticeChanged | DocEvent::Saved => {}
        }
        cx.notify();
    }

    /// The Reading view of `editor`'s tab follows text reloaded from disk,
    /// when it is on screen.
    fn refresh_reader(&mut self, editor: &Entity<EditorView>, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.editor.as_ref() == Some(editor)) else {
            return;
        };
        if tab.mode != ViewMode::Preview {
            return;
        }
        let (Some(reader), Some(path)) = (tab.reader.clone(), tab.path.clone()) else {
            return;
        };
        let Some(text) = editor.update(cx, |e, _| e.take_preview_text()) else {
            return;
        };
        reader.update(cx, |r, cx| {
            let at = r.position();
            r.open_text(path, text, at, cx);
        });
    }

    // ----- autosave triggers and closing ----------------------------------------

    /// The window lost focus: every document autosaves and syncs its
    /// journal.
    pub(super) fn focus_lost(&mut self, cx: &mut Context<'_, Self>) {
        for editor in self.editors() {
            editor.update(cx, |e, cx| e.focus_lost(cx));
        }
    }

    fn editors(&self) -> Vec<Entity<EditorView>> {
        self.tabs.iter().filter_map(|t| t.editor.clone()).collect()
    }

    /// The window is asked to close. Documents that autosave may save are
    /// saved first and the window closes once they are done (at most
    /// [`files::CLOSE_WAIT`]); unsaved text that may not be saved stays in
    /// its journal for the next start. Not designed: there is no "save
    /// changes?" dialog.
    pub(super) fn should_close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        if self.closing {
            return false;
        }
        let editors = self.editors();
        let saves = editors.iter().any(|e| {
            let e = e.read(cx);
            e.saves_on_close() || e.is_saving()
        });
        let waits: Vec<oneshot::Receiver<()>> = editors
            .iter()
            .filter_map(|e| e.update(cx, |e, cx| e.prepare_close(cx)))
            .collect();
        if !saves {
            return true;
        }
        self.closing = true;
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            files::wait_for(waits, cx).await;
            // Forced on purpose: the saves the close waited for are done.
            let _ = cx.update_window(handle, |_, window, _| window.remove_window());
        })
        .detach();
        false
    }

    /// The window's own close button: closes the window the way a close
    /// request from the platform does, through [`Self::should_close`], so
    /// documents are saved or journaled and the close hooks run first.
    pub(super) fn request_close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.should_close(window, cx) {
            window.remove_window();
        }
    }

    // ----- watching ------------------------------------------------------------------

    /// The window's watchers, made on first use when files are watched.
    pub(super) fn ensure_watch(&mut self, cx: &mut Context<'_, Self>) -> Option<WatchHandle> {
        if let Some(watch) = &self.watch {
            return Some(watch.clone());
        }
        if !files::watches_files(cx) {
            return None;
        }
        let (watch, mut events) = WatchHandle::new(cx.background_executor().clone());
        self._watch_events = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update(cx, |ws, cx| ws.watch_event(event, cx)).is_err() {
                    break;
                }
            }
        }));
        for editor in self.editors() {
            let watch = watch.clone();
            editor.update(cx, |e, _| e.set_watch(Some(watch)));
        }
        self.watch = Some(watch.clone());
        Some(watch)
    }

    /// Watches the file at `path`, as last loaded or saved in `baseline`
    /// (read off the UI thread when not given).
    pub(super) fn track_file(
        &mut self,
        path: &Path,
        baseline: Option<DiskState>,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(watch) = self.ensure_watch(cx) {
            watch.track(&absolute(path), baseline);
        }
    }

    /// Stops watching `path` once no tab shows it.
    pub(super) fn untrack_if_unused(&mut self, path: &Path) {
        if self.tabs.iter().any(|t| t.shows(path)) {
            return;
        }
        if let Some(watch) = &self.watch {
            watch.untrack(&absolute(path));
        }
    }

    /// A watcher's event.
    pub(crate) fn watch_event(&mut self, event: WatchEvent, cx: &mut Context<'_, Self>) {
        match event {
            WatchEvent::DocumentChanged { path, change } => self.file_changed(&path, change, cx),
            WatchEvent::SyncConflict {
                copy,
                original,
                service,
            } => {
                for tab in self.tabs.iter_mut().filter(|t| t.shows(&original)) {
                    // Not designed: an information infobar naming the copy.
                    tab.notice = Some(Notice::sync_conflict(&copy, service));
                }
            }
            WatchEvent::IndexUpdated { index, .. } | WatchEvent::IndexBuilt { index, .. } => {
                if let Some(vault) = self.vault.clone() {
                    vault.update(cx, |v, cx| v.publish(index, cx));
                }
            }
            WatchEvent::Notice(notice) => tracing::info!("vault watcher: {notice:?}"),
            WatchEvent::Ready { mode } => tracing::debug!(?mode, "vault watcher ready"),
            WatchEvent::ModeChanged { mode } => tracing::info!(?mode, "vault watcher"),
            WatchEvent::DocumentMoved { .. } => {}
        }
        cx.notify();
    }

    /// The file at `path` changed on disk: its Source views decide (reload
    /// or conflict); a tab with only a Reading view reloads it.
    pub(crate) fn file_changed(
        &mut self,
        path: &Path,
        change: ExternalChange,
        cx: &mut Context<'_, Self>,
    ) {
        for index in 0..self.tabs.len() {
            if !self.tabs[index].shows(path) {
                continue;
            }
            if let Some(editor) = self.tabs[index].editor.clone() {
                editor.update(cx, |e, cx| e.disk_changed(change.clone(), cx));
                continue;
            }
            let tab = &mut self.tabs[index];
            let was_deleted = tab
                .notice
                .as_ref()
                .is_some_and(|n| n.topic == Topic::Deleted);
            match &change {
                ExternalChange::Modified { .. } => {
                    if was_deleted {
                        tab.notice = None;
                    }
                    if let (Some(reader), Some(file)) = (tab.reader.clone(), tab.path.clone()) {
                        reader.update(cx, |r, cx| {
                            let at = r.position();
                            r.open(file, at, cx);
                        });
                    }
                }
                ExternalChange::Deleted => {
                    // Not designed: an information infobar with Close.
                    tab.notice = Some(Notice::deleted(false));
                }
                ExternalChange::Touched(_) => {
                    if was_deleted {
                        tab.notice = None;
                    }
                }
                ExternalChange::Unchanged => {}
            }
        }
    }
}

#[cfg(test)]
impl Workspace {
    /// Takes every tab's Source view away, as closing it would.
    pub(crate) fn drop_editors_for_tests(&mut self, cx: &mut Context<'_, Self>) {
        for index in 0..self.tabs.len() {
            if let Some(editor) = self.tabs[index].editor.take() {
                self.drop_editor(&editor, cx);
            }
        }
    }
}

/// Every window's documents prepare for quitting: they save what autosave
/// may save and sync their journals. Returns what to wait for.
pub(crate) fn prepare_quit(cx: &mut App) -> Vec<oneshot::Receiver<()>> {
    let workspaces: Vec<_> = cx
        .default_global::<RecentWindows>()
        .0
        .iter()
        .map(|w| w.workspace.clone())
        .collect();
    let mut waits = Vec::new();
    for workspace in workspaces {
        let Some(workspace) = workspace.upgrade() else {
            continue;
        };
        let editors = workspace.read(cx).editors();
        for editor in editors {
            if let Some(wait) = editor.update(cx, |e, cx| e.prepare_close(cx)) {
                waits.push(wait);
            }
        }
    }
    waits
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of data safety ([`crate::files`]): Save, Save As,
//! untitled tabs, closing with unsaved changes, the watchers' events, crash
//! recovery and the Source views' events.
//!
//! # Closing with unsaved changes
//!
//! Closing a tab or a window, or quitting, first asks about every document
//! that would not be saved on its own ([`EditorView::needs_close_answer`]:
//! untitled, autosave off or held back by a question, a file that may not
//! be written, a save that failed). Not designed: the question is a
//! warning infobar in the window, like the other questions, naming the
//! documents, with *Save*, *Don't save* and *Cancel*.
//!
//! - *Save* saves them in turn (an untitled one asks where, through the
//!   save dialog) and closes once all are saved. A cancelled dialog or a
//!   failed save stops the whole close; the document shows why.
//! - *Don't save* drops their unsaved text and journals when they close.
//! - *Cancel* (or dismissing the question) closes nothing.
//!
//! Documents autosave may write are saved without asking, as before; a
//! window or quit waits for those saves and stops, asking, when one fails.
//! Quitting asks window by window, bringing each to the front.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use futures::channel::oneshot;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EntityId, Task, WeakEntity, Window};
use gv_vault::WatchEvent;
use gv_vault::files::{DiskState, ExternalChange};

use super::{FileLabel, OpenTarget, RecentWindows, Tab, ViewMode, Workspace, session};
use crate::editor::{DocEvent, EditorStatus, EditorView, SaveAsTarget};
use crate::files::notice::{Notice, Topic};
use crate::files::save_as::{self, SaveAnswer, SavePrompt};
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

/// What a window keeps for data safety between events.
#[derive(Default)]
pub(super) struct SafetyState {
    /// The close waiting for the user's answer, or for the saves the user
    /// asked for.
    close: Option<PendingClose>,
    /// Source views whose unsaved text the user chose not to save; their
    /// journals go when they close.
    discarded: HashSet<EntityId>,
    /// Sync-conflict copies to compare once a Source view has loaded.
    compare_after_load: Vec<(EntityId, PathBuf)>,
}

/// What is being closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CloseKind {
    /// The tab of this Source view.
    Tab(EntityId),
    Window,
    /// The application.
    Quit,
}

struct PendingClose {
    kind: CloseKind,
    /// The documents the question lists.
    editors: Vec<Entity<EditorView>>,
    /// The saves *Save* started; the close goes on once they succeed.
    saving: Option<Task<()>>,
}

/// Saves `editors` in turn for a close: an untitled one asks where first.
/// False as soon as a dialog is cancelled or a save leaves its document
/// modified.
async fn save_for_close(
    this: &WeakEntity<Workspace>,
    editors: &[Entity<EditorView>],
    cx: &mut AsyncApp,
) -> bool {
    for editor in editors {
        let untitled = cx.update(|cx| editor.read(cx).path().is_none());
        if untitled {
            let asked = this.update(cx, |ws, cx| {
                let prompt = ws.save_prompt(editor, cx);
                save_as::dialog(cx).ask(&prompt, cx)
            });
            let Ok(answer) = asked else {
                return false;
            };
            let answer = answer.await;
            let chosen = matches!(answer, Ok(Some(_)));
            let answered = this.update(cx, |ws, cx| ws.save_as_answered(editor, answer, cx));
            if !chosen || answered.is_err() {
                return false;
            }
        } else {
            cx.update(|cx| editor.update(cx, |e, cx| e.save(cx)));
        }
        let done = cx.update(|cx| editor.update(cx, |e, _| e.when_saved()));
        // Dropped with the view: nothing to wait for.
        let _ = done.await;
        if cx.update(|cx| editor.read(cx).is_modified()) {
            return false;
        }
    }
    true
}

impl Workspace {
    // ----- commands -------------------------------------------------------------

    /// `file.save`: saves the active tab's document, in any mode.
    pub fn save_active(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(editor) = self.active_tab().editor.clone() {
            editor.update(cx, |e, cx| e.save(cx));
        }
    }

    /// `file.save_as`: asks where to save the active tab's document, then
    /// saves it there and re-points the tab. A tab showing only its Reading
    /// view gets a Source view for it (loading while the dialog is open).
    pub fn save_as_active(&mut self, cx: &mut Context<'_, Self>) {
        if self.active_tab().is_placeholder() {
            return;
        }
        if let Some(editor) = self.ensure_editor(self.active, cx) {
            self.ask_save_as(editor, cx);
        }
    }

    /// Shows the save dialog for `editor`'s document. The dialog answers
    /// asynchronously; cancelling it does nothing.
    pub(super) fn ask_save_as(&mut self, editor: Entity<EditorView>, cx: &mut Context<'_, Self>) {
        let prompt = self.save_prompt(&editor, cx);
        let answer = save_as::dialog(cx).ask(&prompt, cx);
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |ws, cx| ws.save_as_answered(&editor, answer, cx))
                .ok();
        })
        .detach();
    }

    /// Where the save dialog opens and the name it suggests: the file's own
    /// folder and name; for an untitled document the vault folder (else the
    /// folder of another tab's file, else the home folder) and a name made
    /// from its title.
    fn save_prompt(&self, editor: &Entity<EditorView>, cx: &App) -> SavePrompt {
        let view = editor.read(cx);
        if let Some(path) = view.path() {
            let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let suggested_name = path.file_name().map_or_else(
                || format!("Untitled.{}", save_as::MARKDOWN_EXTENSION),
                |n| n.to_string_lossy().into_owned(),
            );
            return SavePrompt {
                directory,
                suggested_name,
            };
        }
        let directory = self
            .vault
            .as_ref()
            .map(|v| v.read(cx).root().to_path_buf())
            .or_else(|| {
                self.tabs
                    .iter()
                    .find_map(|t| Some(absolute(t.path.as_deref()?).parent()?.to_path_buf()))
            })
            .or_else(gv_vault::root::home_dir)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let head = view.editor().map(|e| {
            let text = e.text();
            let end = text.clip_offset(text.len().min(4096), gv_text::Bias::Left);
            text.text_for_range(0..end).unwrap_or_default()
        });
        let untitled = match view.untitled() {
            Some(n) if n > 1 => format!("Untitled {n}"),
            _ => "Untitled".to_owned(),
        };
        let suggested_name = head
            .as_deref()
            .and_then(save_as::suggested_name)
            .unwrap_or_else(|| format!("{untitled}.{}", save_as::MARKDOWN_EXTENSION));
        SavePrompt {
            directory,
            suggested_name,
        }
    }

    /// The save dialog answered for `editor`'s document.
    fn save_as_answered(
        &mut self,
        editor: &Entity<EditorView>,
        answer: SaveAnswer,
        cx: &mut Context<'_, Self>,
    ) {
        let path = match answer {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(reason) => {
                tracing::warn!("the save dialog could not be opened: {reason}");
                editor.update(cx, |e, cx| {
                    e.show_save_as_error(Notice::save_dialog_failed(&reason), cx);
                });
                return;
            }
        };
        // The dialog asked about replacing the name it was given, not one
        // with `.md` added.
        let (path, renamed) = save_as::with_markdown_extension(path);
        let path = absolute(&path);
        let elsewhere = self
            .tabs
            .iter()
            .any(|t| t.shows(&path) && t.editor.as_ref() != Some(editor));
        if elsewhere {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            editor.update(cx, |e, cx| {
                e.show_save_as_error(Notice::save_as_open_elsewhere(&name), cx);
            });
            return;
        }
        let target = SaveAsTarget {
            path,
            overwrite: !renamed,
        };
        editor.update(cx, |e, cx| e.save_as(target, cx));
    }

    /// Save As moved `editor`'s document: its tab follows the new path (the
    /// name, the window title, the watcher, the Reading and Split views,
    /// recent files and the session).
    fn retarget_tab(
        &mut self,
        editor: &Entity<EditorView>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.editor.as_ref() == Some(editor))
        else {
            return;
        };
        let Some((path, baseline)) = editor.read(cx).disk_baseline() else {
            return;
        };
        let tab = &mut self.tabs[index];
        let old = tab.path.replace(path.clone());
        tab.untitled = None;
        tab.label = FileLabel::new(Some(&path));
        tab.position = None;
        tab.notice = None;
        let reader = tab.reader.clone();
        let split = tab.split.clone();
        if let Some(old) = old {
            self.untrack_if_unused(&old);
        }
        self.track_file(&path, Some(baseline), cx);
        session::note_recent_file(cx, &path);
        let index_snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        editor.update(cx, |e, cx| e.set_index(index_snapshot, cx));
        if let Some(reader) = reader {
            let target = path.clone();
            reader.update(cx, |r, cx| {
                let at = r.position();
                r.open(target, at, cx);
            });
        }
        if let Some(split) = split {
            let target = path.clone();
            split.update(cx, |s, _| s.set_path(target));
        }
        self.tabs_changed(window, cx);
    }

    /// `tabs.new`: an untitled document in a new tab, in Edit mode. It lives
    /// in the crash journal until it is saved (Save As).
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
        let events = cx.subscribe(editor, |this, editor, event: &DocEvent, cx| {
            this.editor_event(&editor, *event, cx);
        });
        // Mod+hover on a link previews it.
        let hover = cx.observe(editor, |this, editor, cx| {
            this.check_editor_preview(&editor, cx);
        });
        let subscription = gpui::Subscription::join(events, hover);
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
                editor.update(cx, |e, cx| e.show_file_state(cx));
                let id = editor.entity_id();
                if let Some(at) = self
                    .safety
                    .compare_after_load
                    .iter()
                    .position(|(e, _)| *e == id)
                {
                    let (_, copy) = self.safety.compare_after_load.remove(at);
                    editor.update(cx, |e, cx| e.compare_with_copy(copy, cx));
                }
            }
            DocEvent::EditableChanged => {
                let index = self
                    .tabs
                    .iter()
                    .position(|t| t.editor.as_ref() == Some(editor));
                if let Some(index) = index {
                    let read_only = !self.can_edit(index, cx);
                    editor.update(cx, |e, cx| e.set_read_only(read_only, cx));
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
            DocEvent::SaveAsRequested => self.ask_save_as(editor.clone(), cx),
            DocEvent::SavedAs => {
                let editor = editor.clone();
                let window = self.window_handle;
                cx.spawn(async move |this, cx| {
                    let _ = cx.update_window(window, |_, window, cx| {
                        this.update(cx, |ws, cx| ws.retarget_tab(&editor, window, cx))
                    });
                })
                .detach();
            }
            DocEvent::FollowLink(disposition) => {
                let editor = editor.clone();
                let window = self.window_handle;
                cx.spawn(async move |this, cx| {
                    let _ = cx.update_window(window, |_, window, cx| {
                        this.update(cx, |ws, cx| {
                            ws.follow_clicked_link(&editor, disposition, window, cx);
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

    /// The window is asked to close. Documents that would not be saved on
    /// their own are asked about first (the window stays open). Documents
    /// that autosave may save are saved and the window closes once they
    /// are done (at most [`files::CLOSE_WAIT`]); if one fails, the close
    /// stops and asks about it.
    pub(super) fn should_close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        if self.closing || self.close_saving() {
            return false;
        }
        let ask = self.unsaved_to_ask(cx);
        if !ask.is_empty() {
            self.ask_close(CloseKind::Window, ask, cx);
            return false;
        }
        let editors: Vec<_> = self
            .editors()
            .into_iter()
            .filter(|e| !self.safety.discarded.contains(&e.entity_id()))
            .collect();
        let saves = editors.iter().any(|e| {
            let e = e.read(cx);
            e.saves_on_close() || e.is_saving()
        });
        let waits: Vec<oneshot::Receiver<()>> = editors
            .iter()
            .filter_map(|e| e.update(cx, |e, cx| e.prepare_close(cx)))
            .collect();
        if !saves {
            self.discard_dropped(cx);
            return true;
        }
        self.closing = true;
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            files::wait_for(waits, cx).await;
            let _ = cx.update_window(handle, |_, window, cx| {
                let close = this
                    .update(cx, |ws, cx| ws.saved_for_close(cx))
                    .unwrap_or(true);
                if close {
                    // Forced on purpose: the saves the close waited for
                    // are done.
                    window.remove_window();
                }
            });
        })
        .detach();
        false
    }

    /// The saves a window close waited for are done. Returns whether the
    /// window may close: not when a save failed (the close asks about it
    /// instead).
    fn saved_for_close(&mut self, cx: &mut Context<'_, Self>) -> bool {
        self.closing = false;
        let ask = self.unsaved_to_ask(cx);
        if !ask.is_empty() {
            self.ask_close(CloseKind::Window, ask, cx);
            return false;
        }
        self.discard_dropped(cx);
        true
    }

    /// The window's own close button: closes the window the way a close
    /// request from the platform does, through [`Self::should_close`], so
    /// documents are saved or journaled and the close hooks run first.
    pub(super) fn request_close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.should_close(window, cx) {
            window.remove_window();
        }
    }

    /// The Source views closing must ask about: modified, not saved on
    /// their own, and not already answered with *Don't save*.
    fn unsaved_to_ask(&self, cx: &App) -> Vec<Entity<EditorView>> {
        self.editors()
            .into_iter()
            .filter(|e| {
                !self.safety.discarded.contains(&e.entity_id()) && e.read(cx).needs_close_answer()
            })
            .collect()
    }

    /// Whether *Save* is saving documents for a close.
    fn close_saving(&self) -> bool {
        self.safety
            .close
            .as_ref()
            .is_some_and(|c| c.saving.is_some())
    }

    /// Drops the unsaved text of the documents answered with *Don't save*
    /// (they are closing). Returns what to wait for.
    fn discard_dropped(&mut self, cx: &mut Context<'_, Self>) -> Vec<oneshot::Receiver<()>> {
        let dropped = std::mem::take(&mut self.safety.discarded);
        self.editors()
            .into_iter()
            .filter(|e| dropped.contains(&e.entity_id()))
            .filter_map(|e| e.update(cx, |e, cx| e.discard_changes(cx)))
            .collect()
    }

    /// Asks whether to save `editors` before closing.
    fn ask_close(
        &mut self,
        kind: CloseKind,
        editors: Vec<Entity<EditorView>>,
        cx: &mut Context<'_, Self>,
    ) {
        self.clear_close_question();
        let names: Vec<String> = editors.iter().map(|e| self.doc_name(e, cx)).collect();
        self.safety.close = Some(PendingClose {
            kind,
            editors,
            saving: None,
        });
        self.ask(Notice::close_unsaved(&names), cx);
    }

    fn clear_close_question(&mut self) {
        for tab in &mut self.tabs {
            if tab
                .notice
                .as_ref()
                .is_some_and(|n| n.topic == Topic::CloseUnsaved)
            {
                tab.notice = None;
            }
        }
    }

    /// The name of `editor`'s document in messages: its tab's.
    fn doc_name(&self, editor: &Entity<EditorView>, cx: &App) -> String {
        if let Some(tab) = self.tabs.iter().find(|t| t.editor.as_ref() == Some(editor)) {
            return tab.label.name.to_string();
        }
        editor
            .read(cx)
            .path()
            .and_then(Path::file_name)
            .map_or_else(
                || "Untitled".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            )
    }

    /// Before tab `index` closes: returns true when it must wait for an
    /// answer (the question is shown in it). A document answered with
    /// *Don't save* drops its unsaved text now.
    pub(super) fn hold_tab_close(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(editor) = self.tabs.get(index).and_then(|t| t.editor.clone()) else {
            return false;
        };
        if self.safety.discarded.remove(&editor.entity_id()) {
            editor.update(cx, |e, cx| e.discard_changes(cx));
            return false;
        }
        if !editor.read(cx).needs_close_answer() {
            return false;
        }
        if index != self.active {
            self.leave_tab(self.active, cx);
            self.active = index;
            self.tabs_changed(window, cx);
        }
        self.ask_close(CloseKind::Tab(editor.entity_id()), vec![editor], cx);
        true
    }

    /// The close question was cancelled or dismissed: nothing closes, and
    /// earlier *Don't save* answers of the same close no longer count.
    pub(super) fn close_cancelled(&mut self, cx: &mut Context<'_, Self>) {
        let quitting = self
            .safety
            .close
            .take()
            .is_some_and(|c| c.kind == CloseKind::Quit);
        self.safety.discarded.clear();
        if quitting {
            // Every window forgets the answers of this quit.
            cx.defer(cancel_quit);
        }
    }

    /// *Save* or *Don't save* in the close question.
    pub(super) fn close_answered(
        &mut self,
        save: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.clear_close_question();
        let Some(pending) = self.safety.close.take() else {
            return;
        };
        let kind = pending.kind;
        if !save {
            self.safety
                .discarded
                .extend(pending.editors.iter().map(Entity::entity_id));
            self.continue_close(kind, window, cx);
            return;
        }
        let editors = pending.editors;
        let handle = self.window_handle;
        let saving = cx.spawn(async move |this, cx| {
            let saved = save_for_close(&this, &editors, cx).await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| {
                    ws.safety.close = None;
                    if saved {
                        ws.continue_close(kind, window, cx);
                    } else if kind == CloseKind::Quit {
                        ws.safety.discarded.clear();
                        cx.defer(cancel_quit);
                    } else {
                        ws.safety.discarded.clear();
                    }
                    cx.notify();
                })
            });
        });
        self.safety.close = Some(PendingClose {
            kind,
            editors: Vec::new(),
            saving: Some(saving),
        });
        cx.notify();
    }

    /// Goes on with a close the user answered.
    fn continue_close(&mut self, kind: CloseKind, window: &mut Window, cx: &mut Context<'_, Self>) {
        match kind {
            CloseKind::Tab(id) => {
                let index = self
                    .tabs
                    .iter()
                    .position(|t| t.editor.as_ref().is_some_and(|e| e.entity_id() == id));
                if let Some(index) = index {
                    self.close_tab(index, window, cx);
                }
            }
            CloseKind::Window => self.request_close(window, cx),
            CloseKind::Quit => cx.defer(files::quit),
        }
    }

    /// The sync-conflict infobar's Compare: the active tab's document
    /// against the copy, once its Source view has loaded (one is made for
    /// a tab that shows only its Reading view).
    pub(super) fn compare_copy(&mut self, copy: PathBuf, cx: &mut Context<'_, Self>) {
        let Some(editor) = self.ensure_editor(self.active, cx) else {
            return;
        };
        if *editor.read(cx).status() == EditorStatus::Ready {
            editor.update(cx, |e, cx| e.compare_with_copy(copy, cx));
        } else {
            self.safety
                .compare_after_load
                .push((editor.entity_id(), copy));
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
                    if tab.canvas.is_some() {
                        self.reload_canvas(index, cx);
                    } else if tab.bases.is_some() {
                        self.reload_base(index, cx);
                    } else if let (Some(reader), Some(file)) =
                        (tab.reader.clone(), tab.path.clone())
                    {
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

/// The windows' workspaces, most recently focused first.
fn workspaces(cx: &mut App) -> Vec<(gpui::AnyWindowHandle, Entity<Workspace>)> {
    cx.default_global::<RecentWindows>()
        .0
        .iter()
        .filter_map(|w| Some((w.window, w.workspace.upgrade()?)))
        .collect()
}

/// Before quitting: the first window with documents that would not be
/// saved on their own comes to the front and asks about them. Returns
/// whether quitting waits (a question, or saves the user asked for).
pub(crate) fn ask_before_quit(cx: &mut App) -> bool {
    for (handle, workspace) in workspaces(cx) {
        let (ask, saving) = {
            let ws = workspace.read(cx);
            (ws.unsaved_to_ask(cx), ws.close_saving())
        };
        if saving {
            return true;
        }
        if ask.is_empty() {
            continue;
        }
        let asked = cx.update_window(handle, |_, window, cx| {
            window.activate_window();
            workspace.update(cx, |ws, cx| ws.ask_close(CloseKind::Quit, ask, cx));
        });
        if asked.is_ok() {
            return true;
        }
    }
    false
}

/// A quit was cancelled: every window forgets its *Don't save* answers
/// and its quit question.
fn cancel_quit(cx: &mut App) {
    for (_, workspace) in workspaces(cx) {
        workspace.update(cx, |ws, cx| {
            if ws
                .safety
                .close
                .as_ref()
                .is_some_and(|c| c.kind == CloseKind::Quit && c.saving.is_none())
            {
                ws.safety.close = None;
                ws.clear_close_question();
            }
            ws.safety.discarded.clear();
            cx.notify();
        });
    }
}

/// Quitting goes ahead: the documents answered with *Don't save* drop
/// their unsaved text. Returns what to wait for.
pub(crate) fn discard_for_quit(cx: &mut App) -> Vec<oneshot::Receiver<()>> {
    let mut waits = Vec::new();
    for (_, workspace) in workspaces(cx) {
        waits.extend(workspace.update(cx, |ws, cx| ws.discard_dropped(cx)));
    }
    waits
}

/// Every window's documents prepare for quitting: they save what autosave
/// may save and sync their journals. Documents answered with *Don't
/// save* are left alone. Returns what to wait for.
pub(crate) fn prepare_quit(cx: &mut App) -> Vec<oneshot::Receiver<()>> {
    let mut waits = Vec::new();
    for (_, workspace) in workspaces(cx) {
        let editors: Vec<_> = {
            let ws = workspace.read(cx);
            ws.editors()
                .into_iter()
                .filter(|e| !ws.safety.discarded.contains(&e.entity_id()))
                .collect()
        };
        for editor in editors {
            if let Some(wait) = editor.update(cx, |e, cx| e.prepare_close(cx)) {
                waits.push(wait);
            }
        }
    }
    waits
}

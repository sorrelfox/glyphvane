// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The File menu's commands that open, create and hand off files, and the
//! questions they ask (FR-VAULT-01, FR-VAULT-02, FR-VAULT-13, FR-VAULT-14,
//! FR-FILE-07, FR-NAV-08, FR-NAV-09, FR-PLAT-04).
//!
//! - **Open file…** asks the open dialog ([`crate::files::open_dialog`])
//!   for files. Markdown files open as tabs of this window; other files go
//!   through [`Workspace::open_other_file`].
//! - **Open folder…** asks for a folder and makes it the vault of this
//!   window when the window shows nothing yet, else of a new window. The
//!   folder joins the recent folders and the Files panel shows it.
//! - **New note** (Mod+N) creates `Untitled.md` (then `Untitled 1.md`, …,
//!   checked without regard to case so a vault stays portable) in the root
//!   of the window's vault, the plan's default `newFileLocation`, and opens
//!   it in Edit mode. A window without a vault gets an untitled document
//!   instead, which lives in the journal until Save As (as New tab does).
//! - **New window** opens an empty window that belongs to the session.
//! - **Revert** takes the file on disk again as one undoable edit. With
//!   unsaved edits it asks first (an infobar).
//! - **Open recent…** shows the Recent panel with the keyboard in it.
//! - **Reveal in file manager** and **Open with default app** act on the
//!   Files panel's selected row when that panel has the keyboard, else on
//!   the tab the command is about; they go through the
//!   [`crate::platform::Launcher`]. Programs are never opened, only
//!   revealed.
//! - **Questions** (none designed; the infobar component): a link with a
//!   scheme that needs confirmation shows its full URL with Open and
//!   Cancel; a file Glyphvane does not show offers Open (with the default
//!   app, or the browser for HTML), Reveal in file manager and Cancel; a
//!   program offers only Reveal in file manager; a folder too large to
//!   index asks "Index ~/ (…)? · Index · Only this folder · Cancel".
//!
//! The dialogs answer asynchronously and the file system is touched only
//! off the UI thread.

use std::path::{Path, PathBuf};

use gpui::{
    AppContext as _, Context, Div, Entity, Focusable as _, InteractiveElement as _, Stateful,
    Window,
};

use super::panels::SidebarPanel;
use super::recent_panel::{RecentItem, RecentPanel, RecentPanelEvent};
use super::vault::WindowVault;
use super::{OpenTarget, Tab, Workspace, session};
use crate::files::notice::{Notice, NoticeAction, Topic};
use crate::files::open_dialog::{self, OpenAnswer, OpenPrompt};
use crate::keymap::actions::{file as file_commands, workspace as workspace_commands};
use crate::platform::{self, FileClass};

/// The name of a new note, before ` 1`, ` 2`, … are added.
const NEW_NOTE_NAME: &str = "Untitled";

/// The topics of the window's own questions: they show before the
/// document's notice, because the user just asked for them.
pub(super) fn is_question(topic: Topic) -> bool {
    matches!(
        topic,
        Topic::Confirm | Topic::NotOpened | Topic::IndexAsk | Topic::CloseUnsaved | Topic::FileOp
    )
}

impl Workspace {
    // ----- Open file, Open folder -----------------------------------------------

    /// `file.open`: asks for files and opens them.
    pub(super) fn ask_open_files(&mut self, cx: &mut Context<'_, Self>) {
        let answer = open_dialog::dialog(cx).ask(OpenPrompt::Files, cx);
        let window = self.window_handle;
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |ws, cx| ws.files_chosen(answer, window, cx))
            });
        })
        .detach();
    }

    fn files_chosen(
        &mut self,
        answer: OpenAnswer,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match answer {
            Ok(Some(paths)) => {
                for path in paths {
                    self.open_any_file(path, window, cx);
                }
            }
            Ok(None) => {}
            Err(reason) => {
                tracing::warn!("the open dialog could not be shown: {reason}");
                self.ask(Notice::open_dialog_failed(&reason), cx);
            }
        }
    }

    /// Opens the file at `path`: a Markdown file as a tab (or the tab that
    /// shows it), anything else by [`Self::open_other_file`].
    pub(crate) fn open_any_file(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let path = std::path::absolute(&path).unwrap_or(path);
        if platform::is_markdown(&path) {
            self.open_path(path, window, cx);
        } else {
            self.open_other_file(path, cx);
        }
    }

    /// A file Glyphvane does not show: a program is only offered to be
    /// revealed; anything else opens with the default app once confirmed.
    pub(super) fn open_other_file(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        let notice = match platform::classify(&path) {
            FileClass::Executable => Notice::executable(&path),
            FileClass::Html => Notice::confirm_open_file(&path, true),
            FileClass::Markdown | FileClass::Other => Notice::confirm_open_file(&path, false),
        };
        self.ask(notice, cx);
    }

    /// `file.open_folder`: asks for a folder and opens it as a vault.
    pub(super) fn ask_open_folder(&mut self, cx: &mut Context<'_, Self>) {
        let answer = open_dialog::dialog(cx).ask(OpenPrompt::Folder, cx);
        let window = self.window_handle;
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |ws, cx| match answer {
                    Ok(Some(paths)) => {
                        if let Some(folder) = paths.into_iter().next() {
                            ws.open_folder(folder, window, cx);
                        }
                    }
                    Ok(None) => {}
                    Err(reason) => {
                        tracing::warn!("the open dialog could not be shown: {reason}");
                        ws.ask(Notice::open_dialog_failed(&reason), cx);
                    }
                })
            });
        })
        .detach();
    }

    /// Whether the window shows nothing yet: its one empty tab, and no
    /// vault.
    fn is_blank(&self) -> bool {
        self.vault.is_none() && self.tabs.len() == 1 && self.tabs[0].is_placeholder()
    }

    /// Opens `folder` as a vault: in this window when it shows nothing
    /// yet, else in a new window.
    pub(crate) fn open_folder(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let folder = std::path::absolute(&folder).unwrap_or(folder);
        if self.takes_folder(&folder, cx) {
            self.show_folder(folder, window, cx);
            return;
        }
        cx.defer(move |cx| {
            if let Err(err) = crate::open_folder(cx, &folder, crate::Placement::NewWindow) {
                tracing::warn!("cannot open a window: {err:#}");
            }
        });
    }

    /// Whether `folder` opens in this window rather than a new one: the
    /// window shows nothing yet, or `folder` is already its vault.
    pub(crate) fn takes_folder(&self, folder: &Path, cx: &gpui::App) -> bool {
        self.is_blank() || self.vault_root(cx).as_deref() == Some(folder)
    }

    /// Shows `folder` in this window: its vault already, or made its vault
    /// (see [`Self::adopt_folder`]). Either way the Files panel shows and
    /// the folder becomes the most recent folder.
    pub(crate) fn show_folder(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.vault_root(cx).as_deref() == Some(folder.as_path()) {
            session::note_recent_folder(cx, &folder);
            self.show_sidebar_panel(SidebarPanel::Files, true, window, cx);
        } else {
            self.adopt_folder(folder, window, cx);
        }
    }

    /// Makes `folder` this window's vault and shows it in the Files panel.
    fn adopt_folder(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<'_, Self>) {
        let vault = cx.new(|cx| WindowVault::for_folder(&folder, cx));
        self.set_vault(vault, cx);
        session::note_recent_folder(cx, &folder);
        self.show_sidebar_panel(SidebarPanel::Files, true, window, cx);
    }

    /// The window's vault folder, if it has a vault.
    pub fn vault_root(&self, cx: &gpui::App) -> Option<PathBuf> {
        self.vault.as_ref().map(|v| v.read(cx).root().to_path_buf())
    }

    /// Replaces the window's vault with `vault`.
    pub(super) fn set_vault(&mut self, vault: Entity<WindowVault>, cx: &mut Context<'_, Self>) {
        self.vault = Some(vault.clone());
        self.vault_watch_started = false;
        self.index_asked = false;
        self._vault_changed = Some(cx.observe(&vault, |this, _, cx| this.vault_changed(cx)));
        self.follow_content(&vault, cx);
        self.vault_changed(cx);
        if let Some(files) = &self.files {
            files.update(cx, |files, cx| files.set_vault(vault, cx));
        }
    }

    // ----- New note, New window -----------------------------------------------------

    /// `file.new_note`: a new note in the vault's root, opened in Edit
    /// mode; without a vault, an untitled document.
    pub(super) fn new_note(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(folder) = self.vault_root(cx) else {
            self.new_tab(window, cx);
            return;
        };
        let handle = self.window_handle;
        let task = cx.background_spawn({
            let folder = folder.clone();
            async move { create_note(&folder, NEW_NOTE_NAME) }
        });
        cx.spawn(async move |this, cx| {
            let created = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| match created {
                    Ok(path) => {
                        ws.open_tab_for_edit(OpenTarget::new(path), None, window, cx);
                    }
                    Err(err) => {
                        tracing::warn!("cannot create a note in {}: {err}", folder.display());
                        ws.ask(Notice::new_note_failed(&folder, &err.to_string()), cx);
                    }
                })
            });
        })
        .detach();
    }

    /// `workspace.new_window`: an empty window, part of the session.
    pub(super) fn new_window(&mut self, cx: &mut Context<'_, Self>) {
        cx.defer(|cx| {
            if let Err(err) = crate::open_file_window(cx, None) {
                tracing::warn!("cannot open a window: {err:#}");
            }
        });
    }

    // ----- Revert -------------------------------------------------------------------------

    /// `file.revert`: the active tab's document goes back to the file on
    /// disk. With unsaved edits it asks first.
    pub(super) fn revert_active(&mut self, cx: &mut Context<'_, Self>) {
        let tab = self.active_tab();
        if tab.path.is_none() {
            return;
        }
        match tab.editor.clone() {
            Some(editor) if editor.read(cx).is_modified() => {
                self.ask(Notice::confirm_revert(), cx);
            }
            Some(editor) => editor.update(cx, |e, cx| e.revert(cx)),
            None => self.reload_reader(self.active, cx),
        }
    }

    /// The confirmed revert.
    fn revert_confirmed(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(editor) = self.active_tab().editor.clone() {
            editor.update(cx, |e, cx| e.revert(cx));
        } else {
            self.reload_reader(self.active, cx);
        }
    }

    /// A tab showing only its Reading view reads its file again.
    fn reload_reader(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let tab = &self.tabs[index];
        if let (Some(reader), Some(path)) = (tab.reader.clone(), tab.path.clone()) {
            reader.update(cx, |r, cx| {
                let at = r.position();
                r.open(path, at, cx);
            });
        }
    }

    // ----- Recent ---------------------------------------------------------------------------

    /// The Recent panel, made the first time it shows.
    pub(super) fn ensure_recent_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<RecentPanel> {
        if let Some(recent) = &self.recent {
            return recent.clone();
        }
        let recent = cx.new(RecentPanel::new);
        self._recent_events = Some(cx.subscribe_in(
            &recent,
            window,
            |this, _, event: &RecentPanelEvent, window, cx| match event {
                RecentPanelEvent::Open(item) => this.open_recent(item.clone(), window, cx),
            },
        ));
        self.recent = Some(recent.clone());
        recent
    }

    /// Opens a recent folder as a vault, or a recent file.
    pub(crate) fn open_recent(
        &mut self,
        item: RecentItem,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match item {
            RecentItem::Folder(folder) => self.open_folder(folder, window, cx),
            RecentItem::File(path) => self.open_any_file(path, window, cx),
        }
    }

    // ----- Reveal, Open with default app ---------------------------------------------

    /// The file a file command acts on: the Files panel's selected row
    /// when the panel has the keyboard, else the file of the tab the
    /// command is about.
    fn command_path(&self, window: &Window, cx: &gpui::App) -> Option<PathBuf> {
        if let Some(files) = &self.files
            && files.read(cx).focus_handle(cx).contains_focused(window, cx)
            && let Some(path) = files.read(cx).selected_path()
        {
            return Some(path);
        }
        let path = self.tabs.get(self.command_tab(cx))?.path.clone()?;
        Some(std::path::absolute(&path).unwrap_or(path))
    }

    /// `file.reveal_in_file_manager`.
    fn reveal_command(&mut self, window: &Window, cx: &mut Context<'_, Self>) {
        if let Some(path) = self.command_path(window, cx) {
            platform::launcher(cx).reveal(&path, cx);
        }
    }

    /// `file.open_with_default_app`: a program is never opened; it is
    /// offered to be revealed instead.
    fn open_with_default_app_command(&mut self, window: &Window, cx: &mut Context<'_, Self>) {
        let Some(path) = self.command_path(window, cx) else {
            return;
        };
        if platform::classify(&path) == FileClass::Executable {
            self.ask(Notice::executable(&path), cx);
            return;
        }
        platform::launcher(cx).open_with_default_app(&path, cx);
    }

    // ----- questions -------------------------------------------------------------------------

    /// Shows a question or a refusal in the active tab's infobar.
    pub(super) fn ask(&mut self, notice: Notice, cx: &mut Context<'_, Self>) {
        self.tabs[self.active].notice = Some(notice);
        cx.notify();
    }

    /// Answers a question of the window. Returns false for actions that
    /// are not the window's.
    pub(super) fn answer(&mut self, action: &NoticeAction, cx: &mut Context<'_, Self>) -> bool {
        let close = |ws: &mut Self| {
            let tab = &mut ws.tabs[ws.active];
            if tab.notice.as_ref().is_some_and(|n| is_question(n.topic)) {
                tab.notice = None;
            }
        };
        match action {
            NoticeAction::Revert => {
                close(self);
                self.revert_confirmed(cx);
            }
            NoticeAction::OpenUrl(url) => {
                close(self);
                platform::launcher(cx).open_url(url, cx);
            }
            NoticeAction::OpenExternally(path) => {
                close(self);
                // Checked again: the file may have changed since the question.
                if platform::classify(path) == FileClass::Executable {
                    self.ask(Notice::executable(path), cx);
                } else {
                    platform::launcher(cx).open_with_default_app(path, cx);
                }
            }
            NoticeAction::Reveal(path) => {
                close(self);
                platform::launcher(cx).reveal(path, cx);
            }
            NoticeAction::IndexAnyway => {
                self.clear_index_questions();
                if let Some(vault) = self.vault.clone() {
                    vault.update(cx, |v, cx| v.index_anyway(cx));
                }
            }
            NoticeAction::IndexFolder(folder) => {
                self.clear_index_questions();
                let folder = folder.clone();
                let vault = cx.new(|cx| WindowVault::for_folder(&folder, cx));
                self.set_vault(vault, cx);
            }
            NoticeAction::Cancel => {
                self.question_dismissed(cx);
            }
            NoticeAction::UndoFileOp => {
                close(self);
                self.undo_file_op(cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// The active tab's question was cancelled or dismissed. Cancelling
    /// the indexing question keeps the folder unindexed in this window.
    pub(super) fn question_dismissed(&mut self, cx: &mut Context<'_, Self>) {
        let notice = self.tabs[self.active].notice.take();
        if notice.as_ref().is_some_and(super::export::is_progress) {
            self.cancel_pdf_job(cx);
            return;
        }
        if notice
            .as_ref()
            .is_some_and(|n| n.topic == Topic::CloseUnsaved)
        {
            self.close_cancelled(cx);
            return;
        }
        if notice.is_some_and(|n| n.topic == Topic::IndexAsk) {
            self.clear_index_questions();
            if let Some(vault) = self.vault.clone() {
                vault.update(cx, |v, cx| v.decline(cx));
            }
        }
    }

    fn clear_index_questions(&mut self) {
        for tab in &mut self.tabs {
            if tab
                .notice
                .as_ref()
                .is_some_and(|n| n.topic == Topic::IndexAsk)
            {
                tab.notice = None;
            }
        }
    }

    /// The vault waits for the user before indexing: asks once, in the
    /// active tab. *Only this folder* is offered for the active file's
    /// folder when that is a smaller folder inside the vault.
    pub(super) fn ask_about_index(&mut self, cx: &mut Context<'_, Self>) {
        if self.index_asked {
            return;
        }
        let Some(vault) = &self.vault else {
            return;
        };
        let vault = vault.read(cx);
        let Some(question) = vault.question() else {
            return;
        };
        let root = vault.root().to_path_buf();
        let smaller = self
            .active_tab()
            .path
            .as_deref()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|folder| folder != &root && folder.starts_with(&root));
        let notice = Notice::index_ask(&root, &question.size, smaller.as_deref());
        self.index_asked = true;
        self.ask(notice, cx);
    }

    // ----- handlers -----------------------------------------------------------------------------

    /// The handlers of the commands above, on the window's root.
    pub(super) fn file_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, _: &file_commands::Open, _, cx| {
            this.ask_open_files(cx);
        }))
        .on_action(cx.listener(|this, _: &file_commands::OpenFolder, _, cx| {
            this.ask_open_folder(cx);
        }))
        .on_action(
            cx.listener(|this, _: &file_commands::OpenRecent, window, cx| {
                this.show_sidebar_panel(SidebarPanel::Recent, true, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &file_commands::NewNote, window, cx| {
            this.new_note(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &workspace_commands::NewWindow, _, cx| {
                this.new_window(cx);
            }),
        )
        .on_action(cx.listener(|this, _: &file_commands::Revert, _, cx| {
            this.revert_active(cx);
        }))
        .on_action(
            cx.listener(|this, _: &file_commands::RevealInFileManager, window, cx| {
                this.reveal_command(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &file_commands::OpenWithDefaultApp, window, cx| {
                this.open_with_default_app_command(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &file_commands::ExportHtml, _, cx| {
            this.export_html_command(cx);
        }))
        .on_action(cx.listener(|this, _: &file_commands::ExportPdf, _, cx| {
            this.export_pdf_command(cx);
        }))
        .on_action(cx.listener(|this, _: &file_commands::Print, window, cx| {
            this.print_command(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &file_commands::OpenInBrowser, _, cx| {
                this.open_in_browser_command(cx);
            }),
        )
        .on_action(cx.listener(
            |this, _: &crate::keymap::actions::editor::CopyAsRichText, window, cx| {
                this.copy_as_rich_text_command(window, cx);
            },
        ))
    }
}

impl Tab {
    /// Whether the tab's notice is one of the window's questions.
    pub(super) fn has_question(&self) -> bool {
        self.notice.as_ref().is_some_and(|n| is_question(n.topic))
    }
}

/// Creates an empty note named `name.md` in `folder`, or `name 1.md`,
/// `name 2.md`, … when a file of that name exists in any letter case.
/// Never replaces a file. Returns its path.
pub(super) fn create_note(folder: &Path, name: &str) -> std::io::Result<PathBuf> {
    let taken: Vec<String> = std::fs::read_dir(folder)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().to_lowercase())
        .collect();
    for n in 0..10_000u32 {
        let file_name = match n {
            0 => format!("{name}.md"),
            n => format!("{name} {n}.md"),
        };
        if taken.contains(&file_name.to_lowercase()) {
            continue;
        }
        let path = folder.join(&file_name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::other("too many notes with that name"))
}

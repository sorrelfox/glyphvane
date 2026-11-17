// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of the quick switcher ([`crate::switcher`]): what it
//! lists (the vault's content index, the open tabs, the recent files) and
//! what happens to the row the user picks.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Div, InteractiveElement as _, Stateful, Window};

use super::vault::{ContentUpdated, VaultStatus, WindowVault};
use super::{OpenTarget, Workspace, session};
use crate::files::notice::Notice;
use crate::keymap::actions::workspace as workspace_commands;
use crate::reader::ScrollTarget;
use crate::switcher::model::{self, Gather, VaultSource};
use crate::switcher::{self as quick, Choice, ChoiceFn, Sources, SourcesFn, SwitcherRequest};

impl Workspace {
    /// The switcher's command on the window's root.
    pub(super) fn switcher_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        root.on_action(
            cx.listener(|this, _: &workspace_commands::QuickSwitcher, window, cx| {
                this.toggle_switcher(window, cx);
            }),
        )
    }

    /// `workspace.quick_switcher`: opens the switcher, or closes it. The
    /// window's vault is made now if it has none yet, so its index starts
    /// building.
    pub(crate) fn toggle_switcher(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.ensure_vault(cx);
        let workspace = cx.weak_entity();
        let sources: SourcesFn = Rc::new({
            let workspace = workspace.clone();
            move |cx: &App| {
                workspace
                    .upgrade()
                    .map(|ws| ws.read(cx).switcher_sources(cx))
                    .unwrap_or_default()
            }
        });
        let on_choice: ChoiceFn = Rc::new(move |choice, window, cx| {
            // Fails only when the window has closed.
            let _ = workspace.update(cx, |ws, cx| ws.switcher_chose(choice, window, cx));
        });
        let request = SwitcherRequest { sources, on_choice };
        // The switcher reads this workspace as it opens.
        window.defer(cx, move |window, cx| {
            quick::toggle(request, window, cx);
        });
    }

    /// Follows the content index of `vault` while the switcher is open.
    pub(super) fn follow_content(
        &mut self,
        vault: &gpui::Entity<WindowVault>,
        cx: &mut Context<'_, Self>,
    ) {
        let handle = self.window_handle;
        self._content_updates =
            Some(cx.subscribe(vault, move |this, _, _: &ContentUpdated, cx| {
                // The switcher reads this workspace as it refreshes.
                cx.defer(move |cx| quick::refresh(handle, cx));
                // So does the Search panel.
                this.sync_search(cx);
            }));
    }

    /// What the switcher lists now.
    pub(crate) fn switcher_sources(&self, cx: &App) -> Sources {
        let vault = self.vault.as_ref().map(|v| v.read(cx));
        let ready = vault.filter(|v| *v.status() == VaultStatus::Ready);
        let content = ready.and_then(WindowVault::content);
        let source = match (ready, &content) {
            (Some(v), Some(snapshot)) => Some(VaultSource::from_content(v.root(), snapshot)),
            (Some(v), None) => Some(VaultSource::from_paths(&v.snapshot())),
            (None, _) => None,
        };
        let open = self
            .tabs
            .iter()
            .filter_map(|t| {
                let path = t.path.clone()?;
                let unsaved = t.editor.as_ref().is_some_and(|e| e.read(cx).is_modified());
                Some((path, unsaved))
            })
            .collect();
        let gather = Gather {
            vault: source,
            open,
            recent: session::recent_files(cx),
            home: gv_vault::root::home_dir(),
        };
        Sources {
            entries: model::collect(&gather),
            content: ready.map(|v| v.root().to_path_buf()).zip(content),
            can_create: ready.is_some(),
        }
    }

    /// The user picked a row (the switcher has closed).
    fn switcher_chose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<'_, Self>) {
        match choice {
            Choice::Open {
                file,
                line,
                new_tab,
            } => self.open_from_switcher(file, line, new_tab, window, cx),
            Choice::Create { path, new_tab } => self.create_from_switcher(&path, new_tab, cx),
        }
    }

    /// Enter: the file in the active tab, or the tab that shows it already;
    /// Mod+Enter: in a new tab. A heading or block opens at its line.
    fn open_from_switcher(
        &mut self,
        file: PathBuf,
        line: Option<u32>,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !super::canvas_tabs::opens_as_tab(&file) {
            self.open_other_file(file, cx);
            return;
        }
        let scroll = line.map(ScrollTarget::Line);
        if new_tab {
            let scroll = scroll.or(Some(ScrollTarget::Offset(0)));
            self.insert_tab(OpenTarget::new(file), scroll, true, window, cx);
            return;
        }
        let shown = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(file.as_path()));
        match shown {
            Some(index) => {
                self.activate_tab(index, window, cx);
                if scroll.is_some() {
                    self.navigate(index, file, scroll, window, cx);
                }
            }
            None => self.navigate(self.active, file, scroll, window, cx),
        }
    }

    /// Creates the note at `path` (vault-relative) and opens it in Edit
    /// mode; a note of that name (in any letter case) opens instead.
    fn create_from_switcher(&mut self, path: &str, new_tab: bool, cx: &mut Context<'_, Self>) {
        let Some(root) = self.vault.as_ref().map(|v| v.read(cx).root().to_path_buf()) else {
            return;
        };
        let handle = self.window_handle;
        let rel = path.to_owned();
        let task = cx.background_spawn({
            let root = root.clone();
            async move { create_named_note(&root, &rel) }
        });
        let folder = root.clone();
        cx.spawn(async move |this, cx| {
            let created = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| match created {
                    Ok((path, true)) => {
                        if new_tab {
                            ws.open_tab_for_edit(OpenTarget::new(path), None, window, cx);
                        } else {
                            ws.navigate(ws.active, path, None, window, cx);
                            ws.edit_active_tab(None, window, cx);
                        }
                    }
                    Ok((path, false)) => ws.open_from_switcher(path, None, new_tab, window, cx),
                    Err(err) => {
                        tracing::warn!("cannot create a note in {}: {err}", folder.display());
                        ws.ask(Notice::new_note_failed(&folder, &err.to_string()), cx);
                    }
                })
            });
        })
        .detach();
    }
}

/// Creates the note at `rel` (vault-relative, `/`-separated, already a
/// valid name) under `root`, with its folders. When a file of that name
/// exists in any letter case, returns it instead. Returns the path and
/// whether it was created.
pub(super) fn create_named_note(root: &Path, rel: &str) -> std::io::Result<(PathBuf, bool)> {
    let mut folder = root.to_path_buf();
    let mut parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let name = parts
        .pop()
        .ok_or_else(|| std::io::Error::other("no name"))?;
    for part in parts {
        if part == ".." {
            return Err(std::io::Error::other("outside the vault"));
        }
        // An existing folder in another letter case is used as it is.
        let existing = std::fs::read_dir(&folder).ok().and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .find(|e| {
                    e.file_name().to_string_lossy().to_lowercase() == part.to_lowercase()
                        && e.file_type().is_ok_and(|t| t.is_dir())
                })
                .map(|e| e.file_name())
        });
        folder.push(existing.unwrap_or_else(|| part.into()));
    }
    std::fs::create_dir_all(&folder)?;
    let wanted = name.to_lowercase();
    if let Some(found) = std::fs::read_dir(&folder)?
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_string_lossy().to_lowercase() == wanted)
    {
        return Ok((found.path(), false));
    }
    let path = folder.join(name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(_) => Ok((path, true)),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok((path, false)),
        Err(err) => Err(err),
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Files panel's file operations (FR-VAULT-06, FR-FILE-11, FR-NAV-10):
//! new folder, rename, move, duplicate, move to trash, the sort orders
//! and the vault switcher.
//!
//! They act on the Files panel's selected row when the panel has the
//! keyboard (its context menu included), else on the file of the tab the
//! command is about. Their handlers exist only while the window's vault
//! is indexed and there is something to act on, so their menu items are
//! disabled otherwise.
//!
//! - **Rename…** asks for the new name in the file prompt
//!   ([`crate::file_prompt`]), with the name before the extension
//!   selected. Names are checked as they are typed: empty, `/`, characters
//!   that break links or that some platform does not allow, a name taken
//!   in any letter case. A case-only rename is allowed.
//! - **Move to…** lists the vault's folders in the Go popover.
//! - Both plan the link updates off the UI thread
//!   ([`gv_vault::refactor::plan_moves`]). When links change, or some
//!   cannot be updated, a **preview** lists every note with each link as
//!   it is and as it will be; Enter applies it. Without link changes the
//!   operation runs at once. Afterwards an infobar says what was done and
//!   offers **Undo**, which reverts the moves and every link update in one
//!   step. Tabs showing a moved file follow it.
//! - **Duplicate** copies the file or folder next to itself as
//!   `Name 1.md` and selects the copy.
//! - **Move to trash** asks first, listing the links that will break; the
//!   file goes to the OS trash and its tabs close.
//! - **New folder** creates `Untitled` (`Untitled 1`, …) in the selected
//!   folder (the selected file's folder, else the vault folder) and asks
//!   for its name.
//! - **Sort** lists the orders in the Go popover; the panel keeps the one
//!   picked while the window is open.
//! - **The vault switcher** (the panel title) lists the recent folders and
//!   Open folder… in the Go popover.
//!
//! Files with unsaved changes are never moved, edited or trashed: the
//! operation is refused with an infobar asking to save first.
//!
//! None of the prompts, the preview, the infobars or the lists is
//! designed (SPEC § Not designed yet); they are built from the existing
//! popovers and the infobar, and flagged here for the design review.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, Entity, Focusable as _, Global, InteractiveElement as _,
    SharedString, Stateful, Window,
};
use gv_vault::content::ContentSnapshot;
use gv_vault::refactor::{
    self, AppliedRefactor, Move, RefactorError, RefactorPlan, SystemTrash, Trash,
};
use gv_vault::{Change, EntryKind, FileMeta, PathIndex, VaultPath};

use super::Workspace;
use super::session;
use super::tree::SortOrder;
use super::vault::{VaultStatus, WindowVault, refactor_journal_dir};
use crate::file_prompt::{self, PromptField, PromptRequest, PromptRow, RowStyle};
use crate::files::notice::Notice;
use crate::goto::{self, Choice, GotoEvent, GotoItem, GotoKind, GotoRequest};
use crate::keymap::actions::file as commands;

/// The name of a new folder, before ` 1`, ` 2`, … are added.
const NEW_FOLDER_NAME: &str = "Untitled";

/// The most broken links the trash question lists.
const MAX_LISTED_LINKS: usize = 200;

/// The window's file operation state.
#[derive(Debug, Default)]
pub(super) struct VaultOps {
    /// The last rename or move, until it is undone or another replaces it.
    undo: Option<AppliedRefactor>,
}

/// Where deleted files go: the OS trash, or what tests put in its place.
pub(crate) struct TrashCan(pub Arc<dyn Trash>);

impl Global for TrashCan {}

fn trash_can(cx: &App) -> Arc<dyn Trash> {
    cx.try_global::<TrashCan>()
        .map_or_else(|| Arc::new(SystemTrash) as Arc<dyn Trash>, |t| t.0.clone())
}

/// What a file command acts on.
#[derive(Debug, Clone)]
struct OpTarget {
    root: PathBuf,
    path: VaultPath,
    is_dir: bool,
}

impl OpTarget {
    fn name(&self) -> String {
        self.path.file_name().unwrap_or_default().to_owned()
    }

    fn parent(&self) -> VaultPath {
        self.path.parent().unwrap_or_default()
    }

    fn fs_path(&self) -> PathBuf {
        self.path.to_fs_path(&self.root)
    }
}

/// What the link planning needs, taken from the vault on the UI thread.
struct PlanInput {
    root: PathBuf,
    paths: PathIndex,
    content: Option<ContentSnapshot>,
    params: gv_vault::ProfileParams,
}

impl Workspace {
    /// The window's vault, once its index is built.
    fn ready_vault(&self, cx: &App) -> Option<Entity<WindowVault>> {
        self.vault
            .clone()
            .filter(|v| *v.read(cx).status() == VaultStatus::Ready)
    }

    /// The Files panel's selected row when the panel has the keyboard,
    /// else the file of the tab the command is about, when it is in the
    /// vault.
    fn op_target(&self, window: &Window, cx: &App) -> Option<OpTarget> {
        let vault = self.ready_vault(cx)?;
        let root = vault.read(cx).root().to_path_buf();
        if let Some(files) = &self.files
            && files.read(cx).focus_handle(cx).contains_focused(window, cx)
        {
            let (path, is_dir) = files.read(cx).selected_entry()?;
            return Some(OpTarget { root, path, is_dir });
        }
        let path = self.tabs.get(self.command_tab(cx))?.path.clone()?;
        let path = std::path::absolute(&path).unwrap_or(path);
        let path = VaultPath::from_relative_fs_path(path.strip_prefix(&root).ok()?).ok()?;
        (!path.is_root()).then_some(OpTarget {
            root,
            path,
            is_dir: false,
        })
    }

    /// The folder New folder creates in: the selected folder, the selected
    /// file's folder, else the vault folder.
    fn op_folder(&self, window: &Window, cx: &App) -> Option<(PathBuf, VaultPath)> {
        let vault = self.ready_vault(cx)?;
        let root = vault.read(cx).root().to_path_buf();
        let folder = match self
            .files
            .as_ref()
            .and_then(|f| f.read(cx).selected_entry())
        {
            Some((path, true)) => path,
            Some((path, false)) => path.parent().unwrap_or_default(),
            None => self
                .op_target(window, cx)
                .map(|t| t.parent())
                .unwrap_or_default(),
        };
        Some((root, folder))
    }

    fn plan_input(&self, cx: &App) -> Option<PlanInput> {
        let vault = self.ready_vault(cx)?;
        let vault = vault.read(cx);
        Some(PlanInput {
            root: vault.root().to_path_buf(),
            paths: vault.snapshot(),
            content: vault.content(),
            params: vault.params(),
        })
    }

    /// The name of an open document with unsaved changes at or below one
    /// of `paths`, if there is one.
    fn unsaved_below(&self, root: &Path, paths: &[VaultPath], cx: &App) -> Option<String> {
        self.tabs.iter().find_map(|tab| {
            let editor = tab.editor.as_ref()?;
            if !editor.read(cx).is_modified() {
                return None;
            }
            let path = tab.path.as_deref()?;
            let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            paths
                .iter()
                .any(|p| path.starts_with(p.to_fs_path(root)))
                .then(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))?
        })
    }

    /// Refuses an operation on files with unsaved changes. Returns whether
    /// it did.
    fn refuse_unsaved(
        &mut self,
        title: &str,
        root: &Path,
        paths: &[VaultPath],
        cx: &mut Context<'_, Self>,
    ) -> bool {
        match self.unsaved_below(root, paths, cx) {
            Some(name) => {
                let reason = format!("“{name}” has unsaved changes: save it first");
                self.ask(Notice::file_op_failed(title, &reason), cx);
                true
            }
            None => false,
        }
    }

    /// Applies `changes` to the vault's index at once, sooner than the
    /// watcher, so the Files panel shows the result.
    fn publish_changes(&self, changes: &[Change], cx: &mut Context<'_, Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        match vault.read(cx).snapshot().apply(changes) {
            Ok(next) => vault.update(cx, |v, cx| v.publish(next, cx)),
            Err(err) => tracing::warn!("file operation not shown in the index: {err}"),
        }
    }

    /// Selects `path` in the Files panel.
    fn reveal_entry(&self, path: &VaultPath, cx: &mut Context<'_, Self>) {
        if let Some(files) = &self.files {
            files.update(cx, |f, cx| f.reveal_vault_path(path, cx));
        }
    }

    // ----- Rename, Move --------------------------------------------------------------------

    /// `file.rename`: asks for the new name.
    pub(super) fn rename_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(target) = self.op_target(window, cx) else {
            return;
        };
        self.ask_name(target, "Rename", window, cx);
    }

    /// Asks for a new name of `target` (Rename…, and a new folder's name).
    fn ask_name(
        &mut self,
        target: OpTarget,
        title: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let name = target.name();
        let dir = target.parent().to_fs_path(&target.root);
        let old = name.clone();
        let validate: file_prompt::Validate = Rc::new(move |text: &str| {
            let new = refactor::clean_name(text);
            if new == old {
                return None;
            }
            if let Err(err) = refactor::validate_name(&new) {
                return Some(err.to_string().into());
            }
            let key = gv_vault::keys::fold_key(&new);
            let case_only = key == gv_vault::keys::fold_key(&old);
            let taken = !case_only
                && std::fs::read_dir(&dir).is_ok_and(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .any(|e| gv_vault::keys::fold_key(&e.file_name().to_string_lossy()) == key)
                });
            taken.then(|| format!("“{new}” already exists in this folder").into())
        });
        let is_dir = target.is_dir;
        let workspace = cx.weak_entity();
        let on_confirm: file_prompt::OnConfirm = Rc::new(move |text, window, cx| {
            let new = refactor::clean_name(&text);
            let target = target.clone();
            let _ = workspace.update(cx, |ws, cx| {
                if new == target.name() {
                    return;
                }
                let Ok(to) = target.parent().join(&new) else {
                    return;
                };
                let m = Move {
                    from: target.path.clone(),
                    to,
                };
                ws.start_moves(vec![m], window, cx);
            });
        });
        let request = PromptRequest {
            title: format!("{title} “{name}”").into(),
            message: Some(if is_dir {
                "New folder name. Links to the notes in it are updated.".into()
            } else {
                "New file name. Links to it are updated.".into()
            }),
            field: Some(PromptField::for_name(&name, is_dir)),
            rows: Vec::new(),
            action: "rename".into(),
            validate: Some(validate),
            on_confirm,
        };
        window.defer(cx, move |window, cx| {
            file_prompt::open(request, window, cx);
        });
    }

    /// `file.move`: lists the folders to move to.
    pub(super) fn move_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(target) = self.op_target(window, cx) else {
            return;
        };
        let Some(vault) = self.ready_vault(cx) else {
            return;
        };
        let index = vault.read(cx).snapshot();
        let here = target.parent();
        let mut folders: Vec<VaultPath> = index
            .iter()
            .filter(|e| e.kind.is_dir() && !e.path.starts_with(&target.path) && e.path != here)
            .map(|e| e.path.clone())
            .collect();
        folders.sort_by(|a, b| super::tree::natural_cmp(a.as_str(), b.as_str()));
        let vault_name = vault.read(cx).name();
        let items = folders
            .iter()
            .map(|f| GotoItem {
                label: if f.is_root() {
                    "/".into()
                } else {
                    f.as_str().to_owned().into()
                },
                detail: f.is_root().then(|| vault_name.clone()),
                depth: 0,
            })
            .collect();
        let workspace = cx.weak_entity();
        let name = target.name();
        let request = GotoRequest {
            title: format!("Move “{name}” to").into(),
            kind: GotoKind::List {
                placeholder: "Folder".into(),
                action: "move".into(),
                empty: "No other folder".into(),
                items,
                initial: 0,
            },
            on_event: Rc::new(move |event, window, cx| {
                let GotoEvent::Confirm(Choice::Item(i)) = event else {
                    return;
                };
                let Some(folder) = folders.get(i) else {
                    return;
                };
                let Ok(to) = folder.join(&name) else {
                    return;
                };
                let m = Move {
                    from: target.path.clone(),
                    to,
                };
                let _ = workspace.update(cx, |ws, cx| ws.start_moves(vec![m], window, cx));
            }),
        };
        window.defer(cx, move |window, cx| {
            goto::open(request, window, cx);
        });
    }

    /// Plans `moves` off the UI thread, then previews or applies them.
    pub(super) fn start_moves(
        &mut self,
        moves: Vec<Move>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(input) = self.plan_input(cx) else {
            return;
        };
        let title = moves_title(&moves, true);
        let sources: Vec<VaultPath> = moves.iter().map(|m| m.from.clone()).collect();
        if self.refuse_unsaved(&title, &input.root, &sources, cx) {
            return;
        }
        let _ = window;
        let handle = self.window_handle;
        let task = cx.background_spawn(async move {
            refactor::plan_moves(
                &input.root,
                &input.paths,
                input.content.as_ref(),
                &input.params,
                &moves,
            )
        });
        cx.spawn(async move |this, cx| {
            let planned = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| ws.planned(planned, window, cx))
            });
        })
        .detach();
    }

    fn planned(
        &mut self,
        planned: Result<RefactorPlan, RefactorError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let plan = match planned {
            Ok(plan) => plan,
            Err(err) => {
                let title = "Not renamed or moved.";
                self.ask(Notice::file_op_failed(title, &err.to_string()), cx);
                return;
            }
        };
        let title = moves_title(&plan.moves, true);
        let edited: Vec<VaultPath> = plan.edits.iter().map(|e| e.path.clone()).collect();
        if self.refuse_unsaved(&title, &plan.root, &edited, cx) {
            return;
        }
        if plan.edits.is_empty() && plan.skipped.is_empty() && plan.warnings.is_empty() {
            self.apply_plan(plan, cx);
            return;
        }
        let request = preview_request(&plan, cx.weak_entity());
        window.defer(cx, move |window, cx| {
            file_prompt::open(request, window, cx);
        });
    }

    /// Applies `plan` off the UI thread.
    fn apply_plan(&mut self, plan: RefactorPlan, cx: &mut Context<'_, Self>) {
        let journal = refactor_journal_dir(cx);
        let handle = self.window_handle;
        let task = cx.background_spawn(async move {
            let result = refactor::apply(&plan, journal.as_deref());
            (plan, result)
        });
        cx.spawn(async move |this, cx| {
            let (plan, result) = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| ws.applied(&plan, result, window, cx))
            });
        })
        .detach();
    }

    fn applied(
        &mut self,
        plan: &RefactorPlan,
        result: Result<AppliedRefactor, RefactorError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let done = moves_title(&plan.moves, false);
        let applied = match result {
            Ok(applied) => applied,
            Err(err) => {
                self.ask(
                    Notice::file_op_failed(&moves_title(&plan.moves, true), &err.to_string()),
                    cx,
                );
                return;
            }
        };
        self.follow_moves(&plan.root, &plan.moved, window, cx);
        let changes: Vec<Change> = plan
            .moves
            .iter()
            .map(|m| Change::Rename {
                from: m.from.clone(),
                to: m.to.clone(),
            })
            .collect();
        self.publish_changes(&changes, cx);
        if let Some(last) = plan.moves.last() {
            self.reveal_entry(&last.to, cx);
        }
        let links = plan.link_count();
        let detail = match (links, plan.edits.len()) {
            (0, _) => None,
            (1, _) => Some("Updated 1 link.".to_owned()),
            (n, 1) => Some(format!("Updated {n} links in 1 note.")),
            (n, m) => Some(format!("Updated {n} links in {m} notes.")),
        };
        self.vault_ops.undo = Some(applied);
        self.ask(Notice::file_op_done(done, detail, true), cx);
    }

    /// The tabs showing a moved file show it at its new place.
    fn follow_moves(
        &mut self,
        root: &Path,
        moved: &[(VaultPath, VaultPath)],
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        for index in 0..self.tabs.len() {
            let Some(path) = self.tabs[index].path.clone() else {
                continue;
            };
            let path = std::path::absolute(&path).unwrap_or(path);
            let Some((_, to)) = moved.iter().find(|(from, _)| from.to_fs_path(root) == path) else {
                continue;
            };
            let at = self.tabs[index]
                .reader
                .as_ref()
                .and_then(|r| r.read(cx).position());
            self.show_in_tab(index, to.to_fs_path(root), at, window, cx);
        }
    }

    /// The infobar's Undo: reverts the last rename or move.
    pub(super) fn undo_file_op(&mut self, cx: &mut Context<'_, Self>) {
        let Some(applied) = self.vault_ops.undo.take() else {
            return;
        };
        let mut paths: Vec<VaultPath> = applied.moves.iter().map(|m| m.to.clone()).collect();
        paths.extend(applied.edits.iter().map(|e| e.moved_to.clone()));
        if self.refuse_unsaved("Not undone.", &applied.root, &paths, cx) {
            self.vault_ops.undo = Some(applied);
            return;
        }
        let journal = refactor_journal_dir(cx);
        let handle = self.window_handle;
        let task = cx.background_spawn(async move {
            let result = refactor::undo(&applied, journal.as_deref());
            (applied, result)
        });
        cx.spawn(async move |this, cx| {
            let (applied, result) = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| ws.undone(&applied, result, window, cx))
            });
        })
        .detach();
    }

    fn undone(
        &mut self,
        applied: &AppliedRefactor,
        result: Result<(), RefactorError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Err(err) = result {
            self.ask(Notice::file_op_failed("Not undone.", &err.to_string()), cx);
            return;
        }
        let back: Vec<(VaultPath, VaultPath)> = applied
            .moves
            .iter()
            .rev()
            .map(|m| (m.to.clone(), m.from.clone()))
            .collect();
        // Entries inside a moved folder follow it back too.
        let mut moved = back.clone();
        for tab in &self.tabs {
            let Some(path) = tab.path.as_deref() else {
                continue;
            };
            let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            let Ok(rel) = path.strip_prefix(&applied.root) else {
                continue;
            };
            let Ok(now) = VaultPath::from_relative_fs_path(rel) else {
                continue;
            };
            for (from, to) in &back {
                if let Some(rest) = now.strip_prefix(from).filter(|r| !r.is_empty())
                    && let Ok(old) = VaultPath::new(&format!("{}/{rest}", to.as_str()))
                {
                    moved.push((now.clone(), old));
                }
            }
        }
        self.follow_moves(&applied.root, &moved, window, cx);
        let changes: Vec<Change> = back
            .iter()
            .map(|(from, to)| Change::Rename {
                from: from.clone(),
                to: to.clone(),
            })
            .collect();
        self.publish_changes(&changes, cx);
        if let Some((_, to)) = back.last() {
            self.reveal_entry(to, cx);
        }
        self.ask(Notice::file_op_done("Undone.".to_owned(), None, false), cx);
    }

    // ----- Duplicate, Trash, New folder ---------------------------------------------------

    /// `file.duplicate`: copies the file or folder next to itself.
    pub(super) fn duplicate_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(target) = self.op_target(window, cx) else {
            return;
        };
        let handle = self.window_handle;
        let task = cx.background_spawn({
            let target = target.clone();
            async move {
                let copy = refactor::duplicate(&target.root, &target.path)?;
                let changes = upserts(&target.root, &copy);
                Ok::<_, RefactorError>((copy, changes))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = cx.update_window(handle, |_, _, cx| {
                this.update(cx, |ws, cx| match result {
                    Ok((copy, changes)) => {
                        ws.publish_changes(&changes, cx);
                        ws.reveal_entry(&copy, cx);
                    }
                    Err(err) => {
                        let title = format!("“{}” was not duplicated.", target.name());
                        ws.ask(Notice::file_op_failed(&title, &err.to_string()), cx);
                    }
                })
            });
        })
        .detach();
    }

    /// `file.delete`: asks, listing the links that will break, then moves
    /// the file or folder to the trash.
    pub(super) fn delete_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(target) = self.op_target(window, cx) else {
            return;
        };
        let name = target.name();
        let title = format!("“{name}” was not moved to the trash.");
        if self.refuse_unsaved(&title, &target.root, std::slice::from_ref(&target.path), cx) {
            return;
        }
        let content = self.ready_vault(cx).and_then(|v| v.read(cx).content());
        let index = self.ready_vault(cx).map(|v| v.read(cx).snapshot());
        let (rows, links, notes) = broken_links(&target, content.as_ref(), index.as_ref());
        let message = match (links, notes) {
            (0, _) if content.is_none() => {
                "The link index is not ready: links to it were not counted.".to_owned()
            }
            (0, _) => "No links point to it.".to_owned(),
            (1, _) => "1 link will break.".to_owned(),
            (n, 1) => format!("{n} links in 1 note will break."),
            (n, m) => format!("{n} links in {m} notes will break."),
        };
        let workspace = cx.weak_entity();
        let request = PromptRequest {
            title: format!("Move “{name}” to the trash?").into(),
            message: Some(message.into()),
            field: None,
            rows,
            action: "move to trash".into(),
            validate: None,
            on_confirm: Rc::new(move |_, window, cx| {
                let target = target.clone();
                let _ = workspace.update(cx, |ws, cx| ws.trash(target, window, cx));
            }),
        };
        window.defer(cx, move |window, cx| {
            file_prompt::open(request, window, cx);
        });
    }

    /// Moves `target` to the trash off the UI thread.
    fn trash(&mut self, target: OpTarget, _window: &mut Window, cx: &mut Context<'_, Self>) {
        let name = target.name();
        let title = format!("“{name}” was not moved to the trash.");
        if self.refuse_unsaved(&title, &target.root, std::slice::from_ref(&target.path), cx) {
            return;
        }
        let trash = trash_can(cx);
        let handle = self.window_handle;
        let task = cx.background_spawn({
            let target = target.clone();
            async move {
                refactor::move_to_trash(&target.root, std::slice::from_ref(&target.path), &*trash)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| match result {
                    Ok(_) => ws.trashed(&target, window, cx),
                    Err(err) => ws.ask(Notice::file_op_failed(&title, &err.to_string()), cx),
                })
            });
        })
        .detach();
    }

    fn trashed(&mut self, target: &OpTarget, window: &mut Window, cx: &mut Context<'_, Self>) {
        let gone = target.fs_path();
        for index in (0..self.tabs.len()).rev() {
            let shows = self.tabs[index].path.as_deref().is_some_and(|p| {
                std::path::absolute(p)
                    .unwrap_or_else(|_| p.to_path_buf())
                    .starts_with(&gone)
            });
            if shows {
                self.close_tab(index, window, cx);
            }
        }
        self.publish_changes(
            &[Change::Remove {
                path: target.path.clone(),
            }],
            cx,
        );
        self.ask(
            Notice::file_op_done(
                format!("Moved “{}” to the trash.", target.name()),
                None,
                false,
            ),
            cx,
        );
    }

    /// `file.new_folder`: creates `Untitled` and asks for its name.
    pub(super) fn new_folder_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((root, parent)) = self.op_folder(window, cx) else {
            return;
        };
        let handle = self.window_handle;
        let task = cx.background_spawn({
            let (root, parent) = (root.clone(), parent.clone());
            async move { refactor::create_folder(&root, &parent, NEW_FOLDER_NAME) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| match result {
                    Ok(path) => {
                        ws.publish_changes(
                            &[Change::Upsert {
                                path: path.clone(),
                                kind: EntryKind::Dir,
                                meta: FileMeta::default(),
                            }],
                            cx,
                        );
                        ws.reveal_entry(&path, cx);
                        let target = OpTarget {
                            root,
                            path,
                            is_dir: true,
                        };
                        ws.ask_name(target, "Name the new folder", window, cx);
                    }
                    Err(err) => {
                        ws.ask(
                            Notice::file_op_failed("No new folder.", &err.to_string()),
                            cx,
                        );
                    }
                })
            });
        })
        .detach();
    }

    // ----- Sort, vault switcher --------------------------------------------------------------

    /// The Files panel's Sort button: the orders in the Go popover.
    pub(super) fn show_sort_orders(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(files) = self.files.clone() else {
            return;
        };
        let current = files.read(cx).sort();
        let items = SortOrder::ALL
            .iter()
            .map(|order| GotoItem {
                label: order.label().into(),
                detail: (*order == current).then(|| SharedString::from("current")),
                depth: 0,
            })
            .collect();
        let initial = SortOrder::ALL
            .iter()
            .position(|o| *o == current)
            .unwrap_or_default();
        let request = GotoRequest {
            title: "Sort files".into(),
            kind: GotoKind::List {
                placeholder: "Sort order".into(),
                action: "sort".into(),
                empty: "No such order".into(),
                items,
                initial,
            },
            on_event: Rc::new(move |event, _, cx| {
                if let GotoEvent::Confirm(Choice::Item(i)) = event
                    && let Some(order) = SortOrder::ALL.get(i)
                {
                    files.update(cx, |f, cx| f.set_sort(*order, cx));
                }
            }),
        };
        window.defer(cx, move |window, cx| {
            goto::open(request, window, cx);
        });
    }

    /// The Files panel's title: the recent folders and Open folder… in
    /// the Go popover. Another folder opens in a new window, unless this
    /// one shows nothing yet.
    pub(super) fn show_vault_switcher(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let current = self.vault_root(cx);
        let folders: Vec<PathBuf> = session::recent_folders(cx)
            .into_iter()
            .filter(|f| Some(f) != current.as_ref())
            .collect();
        let mut items: Vec<GotoItem> = Vec::new();
        if let Some(current) = &current {
            items.push(folder_item(current, Some("this window")));
        }
        items.extend(folders.iter().map(|f| folder_item(f, None)));
        items.push(GotoItem {
            label: "Open folder\u{2026}".into(),
            detail: None,
            depth: 0,
        });
        let has_current = current.is_some();
        let workspace = cx.weak_entity();
        let request = GotoRequest {
            title: "Switch vault".into(),
            kind: GotoKind::List {
                placeholder: "Folder".into(),
                action: "open".into(),
                empty: "No recent folders".into(),
                items,
                initial: usize::from(has_current && !folders.is_empty()),
            },
            on_event: Rc::new(move |event, window, cx| {
                let GotoEvent::Confirm(Choice::Item(i)) = event else {
                    return;
                };
                let _ = workspace.update(cx, |ws, cx| {
                    let i = if has_current {
                        if i == 0 {
                            return;
                        }
                        i - 1
                    } else {
                        i
                    };
                    match folders.get(i) {
                        Some(folder) => ws.open_folder(folder.clone(), window, cx),
                        None => ws.ask_open_folder(cx),
                    }
                });
            }),
        };
        window.defer(cx, move |window, cx| {
            goto::open(request, window, cx);
        });
    }

    // ----- handlers -----------------------------------------------------------------------------

    /// The file operations' handlers, while they can run.
    pub(super) fn vault_op_actions(
        &self,
        root: Stateful<Div>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let ready = self.ready_vault(cx).is_some();
        let target = ready && self.op_target(window, cx).is_some();
        root.when(ready, |root| {
            root.on_action(cx.listener(|this, _: &commands::NewFolder, window, cx| {
                this.new_folder_command(window, cx);
            }))
        })
        .when(target, |root| {
            root.on_action(cx.listener(|this, _: &commands::Rename, window, cx| {
                this.rename_command(window, cx);
            }))
            .on_action(cx.listener(|this, _: &commands::Move, window, cx| {
                this.move_command(window, cx);
            }))
            .on_action(cx.listener(|this, _: &commands::Duplicate, window, cx| {
                this.duplicate_command(window, cx);
            }))
            .on_action(cx.listener(|this, _: &commands::Delete, window, cx| {
                this.delete_command(window, cx);
            }))
        })
    }

    /// Whether Undo has something to undo (tests).
    #[cfg(test)]
    pub(super) fn can_undo_file_op(&self) -> bool {
        self.vault_ops.undo.is_some()
    }
}

/// "Rename “a.md” to “b.md”" style titles: what will be done (`asking`)
/// or what was done.
fn moves_title(moves: &[Move], asking: bool) -> String {
    let Some(m) = moves.first() else {
        return String::new();
    };
    let from = m.from.file_name().unwrap_or_default();
    let to = m.to.file_name().unwrap_or_default();
    let same_folder = m.from.parent() == m.to.parent();
    match (same_folder, asking) {
        (true, true) => format!("“{from}” was not renamed."),
        (true, false) => format!("Renamed “{from}” to “{to}”."),
        (false, true) => format!("“{from}” was not moved."),
        (false, false) => {
            let folder = m.to.parent().unwrap_or_default();
            let folder = if folder.is_root() {
                "the vault folder".to_owned()
            } else {
                format!("“{}”", folder.as_str())
            };
            format!("Moved “{from}” to {folder}.")
        }
    }
}

/// The preview of a rename or move with link changes (not designed).
fn preview_request(plan: &RefactorPlan, workspace: gpui::WeakEntity<Workspace>) -> PromptRequest {
    let m = &plan.moves[0];
    let from = m.from.file_name().unwrap_or_default();
    let rename = m.from.parent() == m.to.parent();
    let title = if rename {
        format!(
            "Rename “{from}” to “{}”?",
            m.to.file_name().unwrap_or_default()
        )
    } else {
        let folder = m.to.parent().unwrap_or_default();
        let folder = if folder.is_root() {
            "/"
        } else {
            folder.as_str()
        };
        format!("Move “{from}” to “{folder}”?")
    };
    let links = plan.link_count();
    let message = match (links, plan.edits.len()) {
        (0, _) => "No links change.".to_owned(),
        (1, _) => "1 link will be updated.".to_owned(),
        (n, 1) => format!("{n} links in 1 note will be updated."),
        (n, m) => format!("{n} links in {m} notes will be updated."),
    };
    let mut rows = Vec::new();
    for warning in &plan.warnings {
        rows.push(PromptRow::new(warning.clone(), RowStyle::Warning));
    }
    for skipped in &plan.skipped {
        let text = if skipped.link.is_empty() {
            format!("{} {}", skipped.source.as_str(), skipped.reason)
        } else {
            format!(
                "{}:{} {} not updated: {}",
                skipped.source.as_str(),
                skipped.line + 1,
                skipped.link,
                skipped.reason
            )
        };
        rows.push(PromptRow::new(text, RowStyle::Warning));
    }
    for edit in &plan.edits {
        let n = edit.changes.len();
        let heading = if edit.moved_to == edit.path {
            format!("{} ({n})", edit.path.as_str())
        } else {
            format!("{} → {} ({n})", edit.path.as_str(), edit.moved_to.as_str())
        };
        rows.push(PromptRow::new(heading, RowStyle::Heading));
        for change in &edit.changes {
            rows.push(PromptRow::new(
                format!("{}: {}", change.line + 1, change.old),
                RowStyle::Removed,
            ));
            rows.push(PromptRow::new(
                format!("{}: {}", change.line + 1, change.new),
                RowStyle::Added,
            ));
        }
    }
    let plan = plan.clone();
    PromptRequest {
        title: title.into(),
        message: Some(message.into()),
        field: None,
        rows,
        action: if rename { "rename" } else { "move" }.into(),
        validate: None,
        on_confirm: Rc::new(move |_, _, cx| {
            let plan = plan.clone();
            let _ = workspace.update(cx, |ws, cx| ws.apply_plan(plan, cx));
        }),
    }
}

/// The links to `target` (or to the files in it) from other notes, as
/// rows, with the link and note counts.
fn broken_links(
    target: &OpTarget,
    content: Option<&ContentSnapshot>,
    index: Option<&PathIndex>,
) -> (Vec<PromptRow>, usize, usize) {
    let (Some(content), Some(index)) = (content, index) else {
        return (Vec::new(), 0, 0);
    };
    let targets: Vec<VaultPath> = index
        .iter()
        .filter(|e| !e.kind.is_dir() && e.path.starts_with(&target.path))
        .map(|e| e.path.clone())
        .collect();
    let mut rows = Vec::new();
    let mut links = 0;
    let mut notes = std::collections::BTreeSet::new();
    for t in &targets {
        for backlink in content.backlinks(t) {
            if backlink.source.starts_with(&target.path) {
                continue;
            }
            links += 1;
            notes.insert(backlink.source.clone());
            if rows.len() < MAX_LISTED_LINKS {
                rows.push(PromptRow::new(
                    format!(
                        "{}:{} → {}",
                        backlink.source.as_str(),
                        backlink.link.line + 1,
                        t.as_str()
                    ),
                    RowStyle::Note,
                ));
            }
        }
    }
    (rows, links, notes.len())
}

/// The index entries of a new file or folder (a duplicate), walked from
/// disk.
fn upserts(root: &Path, path: &VaultPath) -> Vec<Change> {
    let mut out = Vec::new();
    let mut stack = vec![path.clone()];
    while let Some(path) = stack.pop() {
        let full = path.to_fs_path(root);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        let kind = if meta.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        if meta.is_dir()
            && let Ok(entries) = std::fs::read_dir(&full)
        {
            for entry in entries.filter_map(Result::ok) {
                if let Some(name) = entry.file_name().to_str()
                    && let Ok(child) = path.join(name)
                {
                    stack.push(child);
                }
            }
        }
        out.push(Change::Upsert {
            path,
            kind,
            meta: FileMeta {
                size: if meta.is_dir() { 0 } else { meta.len() },
                mtime: meta.modified().ok(),
                identity: None,
            },
        });
    }
    out
}

/// A row of the vault switcher.
fn folder_item(folder: &Path, detail: Option<&'static str>) -> GotoItem {
    let name = folder.file_name().map_or_else(
        || folder.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    GotoItem {
        label: name.into(),
        detail: Some(
            detail
                .map_or_else(|| folder.display().to_string(), str::to_owned)
                .into(),
        ),
        depth: 0,
    }
}

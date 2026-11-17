// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Drag and drop in the main window (07 § Paste and drop, FR-EDIT-09,
//! FR-VAULT-06).
//!
//! - **Files panel → folder.** A file or folder dragged onto a folder row
//!   (or onto the panel below the rows, for the vault folder) moves there
//!   like Move to…: the links to it are planned, previewed when they
//!   change, and the move can be undone ([`Workspace::start_moves`]).
//! - **Files panel → Source view.** A link at the drop point in the
//!   vault's link style; Alt (Option) at the drop embeds it instead.
//!   Images, media and PDFs are always embedded. Folders are not linked.
//! - **Outline → Source view.** A link to the heading
//!   (`[[Note#Heading]]`, `[[#Heading]]` in its own note).
//! - **Files from the OS → Source view.** Files inside the vault are
//!   linked (or embedded) where they are. Files outside it follow
//!   `files.external_files`: copied into the attachment folder
//!   ([`crate::attachments`]) and linked, linked where they are (a
//!   `file://` Markdown link), or a question offering both.
//! - **Anywhere else** (the Reading view, the tabs bar, the window): the
//!   files open, as tabs when Glyphvane shows their kind.
//!
//! Each insertion is one undo step of the Source view
//! ([`EditorView::insert_links`]).
//!
//! # Not designed (built plainly, flagged)
//!
//! The drag preview (a raised pill with the name), the folder-row and
//! drop-target highlights, and the external-files question (the Go
//! popover's list).

use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, Entity, ExternalPaths, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};
use gv_settings::ExternalFiles;
use gv_vault::VaultPath;
use gv_vault::refactor::Move;

use super::Workspace;
use super::canvas_tabs::opens_as_tab;
use crate::appearance::Appearance;
use crate::attachments;
use crate::editor::{EditorView, LinkTarget};
use crate::goto::{self, Choice, GotoEvent, GotoItem, GotoKind, GotoRequest};
use crate::metrics::{radius, size};
use crate::settings::AppSettings;

/// A file or folder dragged from the Files panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraggedFile {
    /// The file, absolute.
    pub path: PathBuf,
    /// Its vault path.
    pub vault_path: VaultPath,
    pub is_dir: bool,
}

/// A heading dragged from the Outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraggedHeading {
    /// The note it is in, absolute.
    pub file: PathBuf,
    pub text: SharedString,
}

/// What follows the pointer during a drag: the dragged thing's name.
pub(crate) struct DragPreview {
    label: SharedString,
}

impl DragPreview {
    pub(crate) fn view(label: impl Into<SharedString>, cx: &mut App) -> Entity<Self> {
        let label = label.into();
        cx.new(|_| Self { label })
    }
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        div()
            .px(px(size::LIST_ROW_INSET))
            .py(px(2.0))
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(theme.border.strong)
            .bg(theme.surface.raised)
            .text_color(theme.text.primary)
            .child(self.label.clone())
    }
}

/// The two answers to the external-files question, in list order.
const ASK_ITEMS: [&str; 2] = [
    "Copy into the attachment folder",
    "Link to them where they are",
];

impl Workspace {
    /// A Files panel entry was dropped on `folder`: it moves there, with
    /// its links, as Move to… does. A drop where it already is does
    /// nothing.
    pub(super) fn drag_move(
        &mut self,
        from: VaultPath,
        folder: VaultPath,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if from.is_root()
            || folder.starts_with(&from)
            || from.parent().unwrap_or_default() == folder
        {
            return;
        }
        let Some(name) = from.file_name() else {
            return;
        };
        let Ok(to) = folder.join(name) else {
            return;
        };
        self.start_moves(vec![Move { from, to }], window, cx);
    }

    /// The Source view on screen and the text offset under the pointer,
    /// when the pointer is over its text.
    fn editor_drop_target(
        &self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<(Entity<EditorView>, usize)> {
        let editor = self.shown_editor()?.clone();
        let position = window.mouse_position();
        let offset = editor.update(cx, |e, cx| e.drop_offset(position, window, cx))?;
        Some((editor, offset))
    }

    /// A Files panel entry was dropped on the document area.
    pub(super) fn drop_dragged_file(
        &mut self,
        file: &DraggedFile,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((editor, at)) = self.editor_drop_target(window, cx) else {
            if !file.is_dir {
                self.open_dropped(vec![file.path.clone()], window, cx);
            }
            return;
        };
        if file.is_dir {
            return;
        }
        let mut target = LinkTarget::file(file.path.clone());
        target.embed |= window.modifiers().alt;
        editor.update(cx, |e, cx| e.insert_links(Some(at), &[target], window, cx));
        self.focus_document(window, cx);
    }

    /// An Outline heading was dropped on the document area.
    pub(super) fn drop_heading(
        &mut self,
        heading: &DraggedHeading,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some((editor, at)) = self.editor_drop_target(window, cx) else {
            return;
        };
        let target = LinkTarget {
            path: heading.file.clone(),
            embed: false,
            heading: Some(heading.text.to_string()),
        };
        editor.update(cx, |e, cx| e.insert_links(Some(at), &[target], window, cx));
        self.focus_document(window, cx);
    }

    /// Files from the OS were dropped on the document area: linked into
    /// the Source view under the pointer, else opened.
    pub(super) fn drop_external(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let paths: Vec<PathBuf> = paths
            .paths()
            .iter()
            .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
            .collect();
        let Some((editor, at)) = self.editor_drop_target(window, cx) else {
            self.open_dropped(paths, window, cx);
            return;
        };
        self.focus_document(window, cx);
        let root = self.link_vault_root(cx);
        let (inside, outside): (Vec<PathBuf>, Vec<PathBuf>) = paths
            .into_iter()
            .filter(|p| p.is_file())
            .partition(|p| root.as_ref().is_some_and(|r| p.starts_with(r)));
        if !inside.is_empty() {
            let targets: Vec<LinkTarget> = inside.into_iter().map(LinkTarget::file).collect();
            editor.update(cx, |e, cx| e.insert_links(Some(at), &targets, window, cx));
        }
        if outside.is_empty() {
            return;
        }
        // Where the inside ones went in, the outside ones follow.
        let at = editor
            .read(cx)
            .editor()
            .map(|e| e.selections().primary().head())
            .unwrap_or(at);
        let choice = cx
            .try_global::<AppSettings>()
            .map_or(ExternalFiles::Ask, |s| s.settings().files.external_files);
        match choice {
            ExternalFiles::Ask => self.ask_external(editor, at, outside, window, cx),
            choice => self.insert_external(editor, at, outside, choice, window, cx),
        }
    }

    /// Asks whether files from outside the vault are copied in or linked.
    fn ask_external(
        &mut self,
        editor: Entity<EditorView>,
        at: usize,
        files: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let workspace = cx.weak_entity();
        let count = files.len();
        let title = if count == 1 {
            let name = files[0]
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("“{name}” is outside the vault")
        } else {
            format!("{count} files are outside the vault")
        };
        let request = GotoRequest {
            title: title.into(),
            kind: GotoKind::List {
                placeholder: "Action".into(),
                action: "choose".into(),
                empty: "Nothing to choose".into(),
                items: ASK_ITEMS
                    .iter()
                    .map(|label| GotoItem {
                        label: (*label).into(),
                        detail: None,
                        depth: 0,
                    })
                    .collect(),
                initial: 0,
            },
            on_event: Rc::new(move |event, window, cx| {
                let GotoEvent::Confirm(Choice::Item(i)) = event else {
                    return;
                };
                let choice = if i == 0 {
                    ExternalFiles::Copy
                } else {
                    ExternalFiles::Link
                };
                let (editor, files) = (editor.clone(), files.clone());
                let _ = workspace.update(cx, |ws, cx| {
                    ws.insert_external(editor, at, files, choice, window, cx);
                });
            }),
        };
        window.defer(cx, move |window, cx| {
            goto::open(request, window, cx);
        });
    }

    /// Copies `files` into the attachment folder and links them at `at`,
    /// or links them where they are.
    pub(super) fn insert_external(
        &mut self,
        editor: Entity<EditorView>,
        at: usize,
        files: Vec<PathBuf>,
        choice: ExternalFiles,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if choice != ExternalFiles::Copy {
            let text = files
                .iter()
                .map(|f| {
                    let name = f
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .replace('[', "\\[")
                        .replace(']', "\\]");
                    format!("[{name}]({})", attachments::file_url(f))
                })
                .collect::<Vec<_>>()
                .join("\n");
            editor.update(cx, |e, cx| {
                e.insert_text_at(Some(at..at), &text, window, cx)
            });
            return;
        }
        let note = editor.read(cx).path().map(std::path::Path::to_path_buf);
        let Some(root) = self
            .link_vault_root(cx)
            .or_else(|| note.as_deref()?.parent().map(std::path::Path::to_path_buf))
        else {
            return;
        };
        let default = cx.try_global::<AppSettings>().map_or_else(
            || "/".to_owned(),
            |s| s.settings().files.attachment_folder.clone(),
        );
        let handle = self.window_handle;
        let task = cx.background_spawn(async move {
            let setting = attachments::folder_setting(&root, &default);
            let dir = attachments::folder(&root, note.as_deref(), &setting);
            files
                .iter()
                .map(|f| attachments::copy_into(&dir, f).map_err(|e| (f.clone(), e)))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let copied = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                let mut targets = Vec::new();
                let mut failed = None;
                for result in copied {
                    match result {
                        Ok(path) => targets.push(LinkTarget::file(path)),
                        Err((file, err)) => {
                            tracing::warn!("cannot copy {}: {err}", file.display());
                            failed = Some((file, err));
                        }
                    }
                }
                editor.update(cx, |e, cx| e.insert_links(Some(at), &targets, window, cx));
                if let Some((file, err)) = failed {
                    let name = file
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let _ = this.update(cx, |ws, cx| {
                        let reason = format!("“{name}”: {err}");
                        ws.ask(
                            crate::files::notice::Notice::file_op_failed(
                                "Not copied into the vault.",
                                &reason,
                            ),
                            cx,
                        );
                    });
                }
            });
        })
        .detach();
    }

    /// Files dropped outside the Source view (the Reading view, the tabs
    /// bar, the window): each opens, as a tab when Glyphvane shows its
    /// kind.
    pub(super) fn open_dropped(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        for path in paths {
            if path.is_dir() {
                continue;
            }
            if opens_as_tab(&path) {
                self.open_path(path, window, cx);
            } else {
                self.open_other_file(path, cx);
            }
        }
    }
}

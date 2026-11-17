// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What following a link does when it has no single target, and the
//! `links.*` commands on the link under the pointer or the caret (09 §
//! Interaction model; § Unresolved, ambiguous and pending links).
//!
//! - **Unresolved:** following offers *Create note* in a popover (SPEC §
//!   Popovers, Create note): in the folder written in the link; for a bare
//!   name where the vault's `newFileLocation` says, otherwise next to the
//!   source. A name not valid on every platform is sanitised as the quick
//!   switcher does, and the link is rewritten to it first (one undoable
//!   edit); with nothing left after sanitising, nothing is offered.
//! - **Ambiguous:** following opens the best candidate and shows the
//!   infobar "*n* notes match *X*." with *Choose…* (a popover listing the
//!   candidates; choosing one opens it) and *Rewrite link* (the shortest
//!   unambiguous form of the note opened, one undoable edit, from the Link
//!   checker's fix).
//! - **Pending:** following waits up to [`PENDING_WAIT`] for the vault
//!   index, then follows again or says the index is still building. No
//!   create offer while pending.
//! - **The link commands** act on the link the link menu was opened on,
//!   or else on the link under the Source view's caret. Each is offered
//!   only where it can run: open in a new tab or window, copy the link as
//!   written, its resolved path or its URL, reveal its target in the file
//!   tree or the file manager, the resolution trace (a popover), *Fix link*
//!   (the Link checker's rewrites for this link; several candidates open a
//!   picker) and *Create note*. `links.open_in_split` (no pane splits
//!   yet), `links.preview` (hover previews) and `links.copy_link_to_heading`
//!   (no link generator yet) have no handler.
//!
//! The popovers (trace, create, candidates) are not designed beyond the
//! Create note row of SPEC § Popovers; they share the menu panel's look.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Div, FocusHandle, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, anchored,
    deferred, div, point, px,
};
use gv_vault::content::{Finding, Fix, FixAction, Span};
use gv_vault::{EntryKind, FileMeta, VaultPath};

use super::panels::SidebarPanel;
use super::{OpenTarget, Workspace};
use crate::appearance::Appearance;
use crate::files::notice::Notice;
use crate::keymap::actions::links as commands;
use crate::menu::{ContextMenu, ContextTarget, context_target, panel, row};
use crate::metrics::{HAIRLINE, font, radius, text};
use crate::reader::link_info::{LinkAt, menu_link};
use crate::reader::{Disposition, Navigation, NewNote, ScrollTarget};
use crate::settings::AppSettings;

/// How long following a pending link waits for the vault index (09 §
/// Unresolved, ambiguous and pending links).
pub(super) const PENDING_WAIT: Duration = Duration::from_secs(1);

/// How often the wait looks at the index.
const PENDING_POLL: Duration = Duration::from_millis(50);

/// Not designed: the popovers' widths and the Create note popover's
/// padding (12) and gaps.
mod spacing {
    pub const CREATE_WIDTH: f32 = 300.0;
    pub const CREATE_PAD: f32 = 12.0;
    pub const CREATE_GAP: f32 = 8.0;
    pub const TRACE_WIDTH: f32 = 460.0;
    pub const TRACE_PAD: f32 = 8.0;
    pub const BUTTON_HEIGHT: f32 = 24.0;
    pub const BUTTON_PAD_X: f32 = 10.0;
    pub const OFFSET_Y: f32 = 6.0;
}

/// A candidate of an ambiguous link in the picker.
#[derive(Debug, Clone)]
pub(super) struct Choice {
    pub label: SharedString,
    pub path: PathBuf,
    /// The rewrite that names this candidate, in the *Fix link* picker.
    pub fix: Option<(Finding, Fix)>,
}

/// What a link popover shows.
#[derive(Debug, Clone)]
pub(super) enum PopoverKind {
    /// The resolution trace (`links.show_trace`).
    Trace {
        title: SharedString,
        lines: Vec<SharedString>,
    },
    /// *Create note* for an unresolved link.
    Create {
        link: LinkAt,
        note: NewNote,
        disposition: Disposition,
        name: String,
    },
    /// The candidates of an ambiguous link: choosing one opens it, or, for
    /// *Fix link*, rewrites the link to it.
    Choose { rows: Vec<Choice>, rewrite: bool },
}

#[derive(Debug, Clone)]
pub(super) struct LinkPopover {
    at: Point<Pixels>,
    pub kind: PopoverKind,
    highlighted: usize,
    restore: Option<FocusHandle>,
}

/// An ambiguous link that was followed, for its infobar.
#[derive(Debug, Clone)]
pub(super) struct AmbiguousFollow {
    link: LinkAt,
    candidates: Vec<PathBuf>,
    /// The candidate opened.
    opened: PathBuf,
    /// The rewrite to the candidate opened, when the content index has it.
    rewrite: Option<(Finding, Fix)>,
}

/// The window's link popover and the last ambiguous link followed.
pub(super) struct LinkUx {
    focus: FocusHandle,
    pub popover: Option<LinkPopover>,
    ambiguous: Option<AmbiguousFollow>,
    /// The link the *Fix link* picker rewrites.
    fix_link: Option<LinkAt>,
}

impl LinkUx {
    pub fn new(cx: &mut App) -> Self {
        Self {
            focus: cx.focus_handle(),
            popover: None,
            ambiguous: None,
            fix_link: None,
        }
    }
}

/// What the commands know about a link.
#[derive(Debug, Clone)]
pub(super) struct LinkFacts {
    pub link: LinkAt,
    pub nav: Navigation,
    /// The local file it resolves to.
    pub file: Option<PathBuf>,
    /// The URL it hands to the system.
    pub url: Option<String>,
    /// The Link checker's rewrites for it.
    pub fixes: Vec<(Finding, Fix)>,
}

impl LinkFacts {
    fn note(&self) -> Option<&PathBuf> {
        match &self.nav {
            Navigation::Open { path, .. } | Navigation::Ambiguous { path, .. } => Some(path),
            _ => None,
        }
    }

    fn create(&self) -> Option<&NewNote> {
        match &self.nav {
            Navigation::Unresolved { create, .. } => create.as_ref(),
            _ => None,
        }
    }
}

impl Workspace {
    // ----- the link a command acts on ----------------------------------------------

    /// The link under the Source view's caret, when the Source view on
    /// screen has the keyboard.
    fn caret_link(&self, window: &Window, cx: &App) -> Option<LinkAt> {
        let editor = self.shown_editor()?;
        let e = editor.read(cx);
        if !e.focus_handle(cx).contains_focused(window, cx) {
            return None;
        }
        e.link_at_caret()
    }

    /// The link a running command acts on: the link menu's, or the one
    /// under the caret.
    pub(super) fn command_link(&self, window: &Window, cx: &App) -> Option<LinkAt> {
        if context_target(cx) == ContextTarget::Link {
            return menu_link(cx).cloned();
        }
        self.caret_link(window, cx)
    }

    /// The link the commands are offered for while drawing: the open link
    /// menu's, or the one under the caret.
    fn shown_link(&self, window: &Window, cx: &App) -> Option<LinkAt> {
        let menu = self.menus.popover.read(cx);
        if menu.is_open() && menu.kind() == Some(ContextMenu::Link) {
            return menu_link(cx).cloned();
        }
        self.caret_link(window, cx)
    }

    /// What following `link` does and what the commands can do with it.
    pub(super) fn link_facts(&self, link: &LinkAt, cx: &App) -> LinkFacts {
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let nav = crate::reader::navigation(
            &link.file,
            snapshot.as_ref(),
            link.profile,
            link.kind,
            &link.destination,
        );
        let file = match &nav {
            Navigation::Open { path, .. }
            | Navigation::Ambiguous { path, .. }
            | Navigation::OpenFile(path) => Some(path.clone()),
            Navigation::Here { .. } => Some(link.file.clone()),
            _ => None,
        };
        let url = match &nav {
            Navigation::External(url) | Navigation::Confirm { url, .. } => Some(url.clone()),
            _ => None,
        };
        LinkFacts {
            fixes: self.link_fixes(link, cx),
            link: link.clone(),
            nav,
            file,
            url,
        }
    }

    /// The Link checker's rewrites for `link`, from the content index.
    fn link_fixes(&self, link: &LinkAt, cx: &App) -> Vec<(Finding, Fix)> {
        let Some(vault) = &self.vault else {
            return Vec::new();
        };
        let vault = vault.read(cx);
        let (Some(content), Some(range)) = (vault.content(), link.range.clone()) else {
            return Vec::new();
        };
        let Some(source) = link
            .file
            .strip_prefix(vault.root())
            .ok()
            .and_then(|rel| VaultPath::from_relative_fs_path(rel).ok())
        else {
            return Vec::new();
        };
        let kind = crate::reader::resolver_kind(link.kind, link.image);
        content
            .check_link(
                &source,
                kind,
                &link.destination,
                Span::of(range),
                link.construct.as_deref(),
            )
            .into_iter()
            .flat_map(|finding| {
                finding
                    .fixes
                    .iter()
                    .filter(|f| matches!(f.action, FixAction::Rewrite { .. }))
                    .cloned()
                    .map(|fix| (finding.clone(), fix))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    // ----- following ---------------------------------------------------------------

    /// The tab a link of `file` acts in: the active tab when it shows the
    /// file, else the first tab that does.
    fn link_tab(&self, file: &Path) -> usize {
        if self.tabs[self.active].path.as_deref() == Some(file) {
            return self.active;
        }
        self.tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(file))
            .unwrap_or(self.active)
    }

    /// The link a Source view's Mod+click or middle click asked for.
    pub(super) fn follow_clicked_link(
        &mut self,
        editor: &gpui::Entity<crate::editor::EditorView>,
        disposition: Disposition,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(link) = editor.update(cx, |e, _| e.take_clicked_link()) else {
            return;
        };
        let index = self.link_tab(&link.file);
        self.follow_link_at(index, link, disposition, false, window, cx);
    }

    /// Follows `link` of tab `index`'s document. `retried`: a pending link
    /// already waited for the index.
    pub(super) fn follow_link_at(
        &mut self,
        index: usize,
        link: LinkAt,
        disposition: Disposition,
        retried: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let target = crate::reader::navigation(
            &link.file,
            snapshot.as_ref(),
            link.profile,
            link.kind,
            &link.destination,
        );
        tracing::debug!(?target, "follow link");
        let (path, fragment) = match target {
            Navigation::External(url) => {
                crate::platform::launcher(cx).open_url(&url, cx);
                return;
            }
            Navigation::Confirm { url, why } => {
                self.activate_tab(index, window, cx);
                self.ask(Notice::confirm_url(&url, &why), cx);
                return;
            }
            Navigation::OpenFile(path) if super::canvas_tabs::is_canvas(&path) => {
                self.open_link_target(index, path, None, disposition, window, cx);
                return;
            }
            Navigation::OpenFile(path) => {
                self.activate_tab(index, window, cx);
                self.open_other_file(path, cx);
                return;
            }
            Navigation::Refused(why) => {
                tracing::info!("link not followed: {why}");
                return;
            }
            Navigation::Pending => {
                self.activate_tab(index, window, cx);
                if retried {
                    self.ask(
                        Notice::link_not_followed(
                            "The vault is still being indexed.".to_owned(),
                            "Where this link goes is not known yet; follow it again in a moment."
                                .to_owned(),
                        ),
                        cx,
                    );
                } else {
                    self.wait_for_index(index, link, disposition, cx);
                }
                return;
            }
            Navigation::Unresolved { name, create } => {
                self.activate_tab(index, window, cx);
                self.offer_create(link, name, create, disposition, window, cx);
                return;
            }
            Navigation::Ambiguous {
                path,
                fragment,
                candidates,
            } => {
                let scroll = fragment.map(ScrollTarget::Fragment);
                let shown =
                    self.open_link_target(index, path.clone(), scroll, disposition, window, cx);
                let rewrite = self.rewrite_for(&link, &path, cx);
                let follow = AmbiguousFollow {
                    link,
                    candidates,
                    opened: path,
                    rewrite,
                };
                if let Some(shown) = shown {
                    self.tabs[shown].notice =
                        Some(ambiguous_notice(&follow, self.link_vault_root(cx)));
                }
                self.link_ux.ambiguous = Some(follow);
                cx.notify();
                return;
            }
            Navigation::Here { fragment } => (link.file.clone(), fragment),
            Navigation::Open { path, fragment } => (path, fragment),
        };
        let scroll = fragment.map(ScrollTarget::Fragment);
        self.open_link_target(index, path, scroll, disposition, window, cx);
    }

    /// Shows a followed link's target; returns the tab it shows in.
    pub(super) fn open_link_target(
        &mut self,
        index: usize,
        path: PathBuf,
        scroll: Option<ScrollTarget>,
        disposition: Disposition,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<usize> {
        match disposition {
            Disposition::InPlace => {
                self.navigate(index, path, scroll, window, cx);
                Some(index)
            }
            Disposition::NewTab => {
                let scroll = scroll.or(Some(ScrollTarget::Offset(0)));
                self.insert_tab(OpenTarget::new(path), scroll, true, window, cx);
                Some(self.active)
            }
            Disposition::Background => {
                let scroll = scroll.or(Some(ScrollTarget::Offset(0)));
                self.insert_tab(OpenTarget::new(path.clone()), scroll, false, window, cx);
                self.tabs
                    .iter()
                    .rposition(|t| t.path.as_deref() == Some(path.as_path()))
            }
        }
    }

    pub(super) fn link_vault_root(&self, cx: &App) -> Option<PathBuf> {
        self.vault.as_ref().map(|v| v.read(cx).root().to_path_buf())
    }

    /// Waits up to [`PENDING_WAIT`] for the vault index to be complete,
    /// then follows `link` again.
    fn wait_for_index(
        &mut self,
        index: usize,
        link: LinkAt,
        disposition: Disposition,
        cx: &mut Context<'_, Self>,
    ) {
        let handle = self.window_handle;
        let vault = self.vault.clone();
        cx.spawn(async move |this, cx| {
            let mut waited = Duration::ZERO;
            while waited < PENDING_WAIT {
                cx.background_executor().timer(PENDING_POLL).await;
                waited += PENDING_POLL;
                let complete = vault
                    .as_ref()
                    .is_some_and(|v| cx.read_entity(v, |v, _| v.snapshot().is_complete()));
                if complete {
                    break;
                }
            }
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| {
                    let index = index.min(ws.tabs.len() - 1);
                    ws.follow_link_at(index, link, disposition, true, window, cx);
                })
            });
        })
        .detach();
    }

    // ----- unresolved: create ------------------------------------------------------

    /// An unresolved link was followed: the Create note popover, or why
    /// nothing can be created.
    fn offer_create(
        &mut self,
        link: LinkAt,
        name: String,
        create: Option<NewNote>,
        disposition: Disposition,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if AppSettings::viewer_mode(cx) {
            self.ask(
                Notice::link_not_followed(
                    format!("No note “{name}”."),
                    "Viewer mode is on: notes are not created.".to_owned(),
                ),
                cx,
            );
            return;
        }
        let Some(note) = create else {
            self.ask(
                Notice::link_not_followed(
                    format!("No note “{name}”."),
                    "Nothing can be created for this link: it does not name a note in the vault."
                        .to_owned(),
                ),
                cx,
            );
            return;
        };
        self.open_link_popover(
            PopoverKind::Create {
                link,
                note,
                disposition,
                name,
            },
            window,
            cx,
        );
    }

    /// Creates the note the popover offers (rewriting the link first when
    /// its name was sanitised) and opens it.
    fn create_from_popover(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(PopoverKind::Create {
            link,
            note,
            disposition,
            ..
        }) = self.link_ux.popover.as_ref().map(|p| p.kind.clone())
        else {
            return;
        };
        self.close_link_popover(window, cx);
        // The link's document keeps its tab while the rewrite waits for
        // its Source view: the note opens in a new tab.
        let disposition = if let Some(rewrite) = &note.rewrite {
            self.rewrite_link_text(&link, rewrite.clone(), window, cx);
            Disposition::NewTab
        } else {
            disposition
        };
        self.create_note_for_link(&note, disposition, cx);
    }

    /// Creates `note` (or opens it when it exists in another letter case)
    /// and shows it for editing.
    pub(super) fn create_note_for_link(
        &mut self,
        note: &NewNote,
        disposition: Disposition,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(root) = note
            .file
            .to_str()
            .and_then(|f| f.strip_suffix(note.path.as_str()))
            .map(PathBuf::from)
        else {
            return;
        };
        let handle = self.window_handle;
        let rel = note.path.as_str().to_owned();
        let task = cx.background_spawn({
            let root = root.clone();
            async move {
                let (path, created) = super::switcher::create_named_note(&root, &rel)?;
                let meta = std::fs::metadata(&path)?;
                Ok::<_, std::io::Error>((path, created, meta))
            }
        });
        let vault_path = note.path.clone();
        cx.spawn(async move |this, cx| {
            let created = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| match created {
                    Ok((path, created, meta)) => {
                        if created && let Some(vault) = ws.vault.clone() {
                            // Sooner than the watcher, and without one.
                            let index = vault.read(cx).snapshot();
                            if let Ok(next) = index.apply(&[gv_vault::Change::Upsert {
                                path: vault_path,
                                kind: EntryKind::File,
                                meta: FileMeta {
                                    size: meta.len(),
                                    mtime: meta.modified().ok(),
                                    identity: None,
                                },
                            }]) {
                                vault.update(cx, |v, cx| v.publish(next, cx));
                            }
                        }
                        match disposition {
                            Disposition::InPlace => {
                                ws.navigate(ws.active, path, None, window, cx);
                                ws.edit_active_tab(None, window, cx);
                            }
                            Disposition::NewTab | Disposition::Background => {
                                ws.open_tab_for_edit(OpenTarget::new(path), None, window, cx);
                            }
                        }
                    }
                    Err(err) => {
                        let folder = root.clone();
                        tracing::warn!("cannot create a note in {}: {err}", folder.display());
                        ws.ask(Notice::new_note_failed(&folder, &err.to_string()), cx);
                    }
                })
            });
        })
        .detach();
    }

    /// Replaces the path part of `link`'s destination with `path` in its
    /// document's buffer (one undoable edit).
    fn rewrite_link_text(
        &mut self,
        link: &LinkAt,
        path: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(range) = link.range.clone() else {
            return;
        };
        let kind = crate::reader::resolver_kind(link.kind, link.image);
        let raw = link.destination.to_string();
        self.edit_file(
            link.file.clone(),
            range,
            move |construct| gv_vault::content::check::rewrite_link(construct, kind, &raw, &path),
            window,
            cx,
        );
    }

    /// Replaces `range` of `file`'s buffer with what `make` makes of its
    /// text (see [`Workspace::edit_source`]).
    fn edit_file(
        &mut self,
        file: PathBuf,
        range: Range<usize>,
        make: impl FnOnce(&str) -> Option<String> + 'static,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if AppSettings::viewer_mode(cx) {
            return;
        }
        self.edit_path(file, range, make, window, cx);
    }

    // ----- ambiguous ---------------------------------------------------------------

    /// The Link checker's rewrite of `link` to `target`.
    fn rewrite_for(&self, link: &LinkAt, target: &Path, cx: &App) -> Option<(Finding, Fix)> {
        let root = self.link_vault_root(cx)?;
        let target = target
            .strip_prefix(&root)
            .ok()
            .and_then(|rel| VaultPath::from_relative_fs_path(rel).ok())?;
        let label = format!("Use {}", target.as_str());
        self.link_fixes(link, cx)
            .into_iter()
            .find(|(_, fix)| fix.label == label)
    }

    /// The infobar's *Choose…*: the candidates of the ambiguous link.
    pub(super) fn choose_link_target(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(follow) = self.link_ux.ambiguous.clone() else {
            return;
        };
        let root = self.link_vault_root(cx);
        let rows = follow
            .candidates
            .iter()
            .map(|path| Choice {
                label: shown_path(path, root.as_deref()).into(),
                path: path.clone(),
                fix: None,
            })
            .collect();
        self.open_link_popover(
            PopoverKind::Choose {
                rows,
                rewrite: false,
            },
            window,
            cx,
        );
    }

    /// The infobar's *Rewrite link*: the link names the note it opened.
    pub(super) fn rewrite_ambiguous_link(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(follow) = self.link_ux.ambiguous.take() else {
            return;
        };
        let Some((finding, fix)) = follow.rewrite.clone() else {
            self.link_ux.ambiguous = Some(follow);
            return;
        };
        self.tabs[self.active].notice = None;
        self.apply_fix_to_link(&follow.link, &finding, &fix, window, cx);
    }

    /// Applies a rewrite of the checker to `link`'s construct.
    fn apply_fix_to_link(
        &mut self,
        link: &LinkAt,
        finding: &Finding,
        fix: &Fix,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let FixAction::Rewrite { path } = &fix.action else {
            return;
        };
        let Some(range) = link.range.clone() else {
            return;
        };
        let (kind, raw, path) = (finding.link_kind, finding.raw.clone(), path.clone());
        self.edit_file(
            link.file.clone(),
            range,
            move |construct| gv_vault::content::check::rewrite_link(construct, kind, &raw, &path),
            window,
            cx,
        );
    }

    // ----- the commands ------------------------------------------------------------

    fn link_command(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<LinkFacts> {
        let link = self.command_link(window, cx)?;
        Some(self.link_facts(&link, cx))
    }

    fn follow_command(
        &mut self,
        disposition: Disposition,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(link) = self.command_link(window, cx) {
            let index = self.link_tab(&link.file);
            self.follow_link_at(index, link, disposition, false, window, cx);
        }
    }

    fn open_in_new_window(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(path) = self
            .link_command(window, cx)
            .and_then(|f| f.note().cloned())
        else {
            return;
        };
        cx.defer(move |cx| {
            if let Err(err) = crate::open_file_window(cx, Some(OpenTarget::new(path))) {
                tracing::warn!("cannot open a window: {err:#}");
            }
        });
    }

    fn copy_link(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(text) = self.command_link(window, cx).and_then(|l| l.construct) {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    fn copy_resolved_path(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(path) = self.link_command(window, cx).and_then(|f| f.file) {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
    }

    fn copy_url(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(url) = self.link_command(window, cx).and_then(|f| f.url) {
            cx.write_to_clipboard(ClipboardItem::new_string(url));
        }
    }

    fn reveal_link_in_tree(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(path) = self.link_command(window, cx).and_then(|f| f.file) else {
            return;
        };
        self.show_sidebar_panel(SidebarPanel::Files, true, window, cx);
        if let Some(files) = &self.files {
            files.update(cx, |f, cx| f.reveal_path(&path, cx));
        }
    }

    fn reveal_link_in_file_manager(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(path) = self.link_command(window, cx).and_then(|f| f.file) {
            crate::platform::launcher(cx).reveal(&path, cx);
        }
    }

    /// `links.show_trace`: the resolver's steps for the link.
    fn show_trace(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(link) = self.command_link(window, cx) else {
            return;
        };
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let resolver =
            crate::reader::Resolver::for_click(&link.file, snapshot.as_ref(), link.profile);
        let res = resolver.resolve(
            crate::reader::resolver_kind(link.kind, link.image),
            &link.destination,
        );
        let written = link
            .construct
            .as_deref()
            .map_or_else(|| link.destination.to_string(), str::to_owned);
        let title = format!(
            "{written} from {} (profile: {})",
            resolver.source().as_str(),
            link.profile.as_str()
        );
        let mut lines: Vec<SharedString> = res.trace.iter().map(|s| s.to_string().into()).collect();
        lines.push(format!("{:<11} {}", "status", res.status).into());
        self.open_link_popover(
            PopoverKind::Trace {
                title: title.into(),
                lines,
            },
            window,
            cx,
        );
    }

    /// `links.fix` and `links.create_note`: on the link under the pointer
    /// or the caret, else on the Link checker's selected finding (also
    /// when the Link checker has the keyboard).
    pub(super) fn link_fix_command(
        &mut self,
        create: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let checker_focused = self
            .panels
            .link_checker
            .as_ref()
            .is_some_and(|c| c.focus_handle(cx).contains_focused(window, cx));
        let facts = (!checker_focused)
            .then(|| self.link_command(window, cx))
            .flatten();
        match facts {
            Some(facts) if create && facts.create().is_some() => {
                if AppSettings::viewer_mode(cx) {
                    return;
                }
                let note = facts.create().cloned().expect("checked");
                if let Some(rewrite) = &note.rewrite {
                    self.rewrite_link_text(&facts.link, rewrite.clone(), window, cx);
                }
                self.create_note_for_link(&note, Disposition::NewTab, cx);
            }
            Some(facts) if !create && !facts.fixes.is_empty() => {
                if let [(finding, fix)] = facts.fixes.as_slice() {
                    let (finding, fix) = (finding.clone(), fix.clone());
                    self.apply_fix_to_link(&facts.link, &finding, &fix, window, cx);
                    return;
                }
                let rows = facts
                    .fixes
                    .iter()
                    .map(|(finding, fix)| Choice {
                        label: fix.label.clone().into(),
                        path: PathBuf::new(),
                        fix: Some((finding.clone(), fix.clone())),
                    })
                    .collect();
                self.link_ux.ambiguous = None;
                self.link_ux.fix_link = Some(facts.link.clone());
                self.open_link_popover(
                    PopoverKind::Choose {
                        rows,
                        rewrite: true,
                    },
                    window,
                    cx,
                );
            }
            _ => self.fix_command(create, window, cx),
        }
    }

    /// The handlers of the `links.*` commands that act on a link, on the
    /// window's root: each only where it can run.
    pub(super) fn link_actions(
        &self,
        root: Stateful<Div>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let facts = self.shown_link(window, cx).map(|l| self.link_facts(&l, cx));
        let checker_fix = |create: bool| {
            self.panels
                .link_checker
                .as_ref()
                .is_some_and(|c| c.read(cx).fix_for_command(create).is_some())
        };
        let (fix, create) = (checker_fix(false), checker_fix(true));
        let f = facts.as_ref();
        let has = f.is_some();
        let note = f.is_some_and(|f| f.note().is_some());
        let construct = f.is_some_and(|f| f.link.construct.is_some());
        let file = f.is_some_and(|f| f.file.is_some());
        let url = f.is_some_and(|f| f.url.is_some());
        let in_vault = file
            && f.and_then(|f| f.file.as_ref())
                .zip(self.link_vault_root(cx))
                .is_some_and(|(p, r)| p.starts_with(r));
        let fixes = fix || f.is_some_and(|f| !f.fixes.is_empty());
        let creates = create || f.is_some_and(|f| f.create().is_some());
        root.when(has, |root| {
            root.on_action(cx.listener(|this, _: &commands::Follow, window, cx| {
                this.follow_command(Disposition::InPlace, window, cx);
            }))
            .on_action(cx.listener(|this, _: &commands::OpenInNewTab, window, cx| {
                this.follow_command(Disposition::NewTab, window, cx);
            }))
            .on_action(cx.listener(|this, _: &commands::ShowTrace, window, cx| {
                this.show_trace(window, cx);
            }))
        })
        .when(note, |root| {
            root.on_action(
                cx.listener(|this, _: &commands::OpenInNewWindow, window, cx| {
                    this.open_in_new_window(window, cx);
                }),
            )
        })
        .when(construct, |root| {
            root.on_action(cx.listener(|this, _: &commands::CopyLink, window, cx| {
                this.copy_link(window, cx);
            }))
        })
        .when(file, |root| {
            root.on_action(
                cx.listener(|this, _: &commands::CopyResolvedPath, window, cx| {
                    this.copy_resolved_path(window, cx);
                }),
            )
            .on_action(cx.listener(
                |this, _: &commands::RevealInFileManager, window, cx| {
                    this.reveal_link_in_file_manager(window, cx);
                },
            ))
        })
        .when(in_vault, |root| {
            root.on_action(cx.listener(|this, _: &commands::RevealInTree, window, cx| {
                this.reveal_link_in_tree(window, cx);
            }))
        })
        .when(url, |root| {
            root.on_action(cx.listener(|this, _: &commands::CopyUrl, window, cx| {
                this.copy_url(window, cx);
            }))
        })
        .when(fixes, |root| {
            root.on_action(cx.listener(|this, _: &commands::Fix, window, cx| {
                this.link_fix_command(false, window, cx);
            }))
        })
        .when(creates, |root| {
            root.on_action(cx.listener(|this, _: &commands::CreateNote, window, cx| {
                this.link_fix_command(true, window, cx);
            }))
        })
    }

    // ----- the popover -------------------------------------------------------------

    fn open_link_popover(
        &mut self,
        kind: PopoverKind,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let restore = window.focused(cx);
        self.link_ux.popover = Some(LinkPopover {
            at: window.mouse_position(),
            kind,
            highlighted: 0,
            restore,
        });
        self.link_ux.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn close_link_popover(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(popover) = self.link_ux.popover.take()
            && let Some(focus) = popover.restore
        {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    /// The link popover's kind, for tests.
    #[cfg(test)]
    pub(super) fn link_popover(&self) -> Option<&PopoverKind> {
        self.link_ux.popover.as_ref().map(|p| &p.kind)
    }

    fn move_highlight(&mut self, by: isize, cx: &mut Context<'_, Self>) {
        if let Some(p) = &mut self.link_ux.popover
            && let PopoverKind::Choose { rows, .. } = &p.kind
            && !rows.is_empty()
        {
            let n = rows.len() as isize;
            p.highlighted = ((p.highlighted as isize + by).rem_euclid(n)) as usize;
            cx.notify();
        }
    }

    /// Enter in the popover, or a click on row `row`.
    fn confirm_popover(
        &mut self,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(popover) = self.link_ux.popover.clone() else {
            return;
        };
        match popover.kind {
            PopoverKind::Trace { .. } => self.close_link_popover(window, cx),
            PopoverKind::Create { .. } => self.create_from_popover(window, cx),
            PopoverKind::Choose { rows, rewrite } => {
                let Some(choice) = rows.get(row.unwrap_or(popover.highlighted)).cloned() else {
                    return;
                };
                self.close_link_popover(window, cx);
                if rewrite {
                    if let (Some((finding, fix)), Some(link)) =
                        (choice.fix, self.link_ux.fix_link.take())
                    {
                        self.apply_fix_to_link(&link, &finding, &fix, window, cx);
                    }
                } else {
                    // Another candidate of the ambiguous link: in its place.
                    self.tabs[self.active].notice = None;
                    if let Some(follow) = &mut self.link_ux.ambiguous {
                        follow.opened = choice.path.clone();
                    }
                    let active = self.active;
                    self.navigate(active, choice.path, None, window, cx);
                }
            }
        }
    }

    /// The link popover, drawn over the window at the place it opened.
    pub(super) fn render_link_popover(&self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let popover = self.link_ux.popover.as_ref()?;
        let theme = Appearance::theme(cx);
        let body: Div = match &popover.kind {
            PopoverKind::Trace { title, lines } => panel(theme)
                .debug_selector(|| "link-trace".to_owned())
                .w(px(spacing::TRACE_WIDTH))
                .p(px(spacing::TRACE_PAD))
                .gap(px(4.0))
                .child(
                    div()
                        .font_weight(FontWeight(text::weight::STRONG))
                        .child(title.clone()),
                )
                .child(div().h(px(HAIRLINE)).bg(theme.border.default))
                .children(lines.iter().map(|l| {
                    div()
                        .font_family(font::EDITOR)
                        .text_size(px(text::MENU_SHORTCUT_SIZE))
                        .whitespace_nowrap()
                        .child(l.clone())
                })),
            PopoverKind::Create {
                note, name, link, ..
            } => {
                let shown = note.path.as_str().to_owned();
                let explanation = match &note.rewrite {
                    Some(_) => format!(
                        "“{name}” is not a valid file name everywhere. The link becomes “{}”.",
                        note.path.as_str().rsplit('/').next().unwrap_or("")
                    ),
                    None => format!("No note “{name}” exists yet."),
                };
                let primary = if note.rewrite.is_some() {
                    "Create and rewrite link"
                } else {
                    "Create note"
                };
                let _ = link;
                panel(theme)
                    .debug_selector(|| "link-create".to_owned())
                    .w(px(spacing::CREATE_WIDTH))
                    .p(px(spacing::CREATE_PAD))
                    .gap(px(spacing::CREATE_GAP))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(12.0))
                            .font_weight(FontWeight(text::weight::STRONG))
                            .child(
                                gpui::component::Icon::empty()
                                    .path(crate::icons::FILE_PLUS_DASHED)
                                    .size(px(14.0)),
                            )
                            .child(format!("Create “{shown}”?")),
                    )
                    .child(div().text_color(theme.text.secondary).child(explanation))
                    .child(
                        div()
                            .flex()
                            .gap(px(spacing::CREATE_GAP))
                            .child(button("link-create-confirm", primary, true, cx).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.create_from_popover(window, cx);
                                }),
                            ))
                            .child(button("link-create-cancel", "Cancel", false, cx).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.close_link_popover(window, cx);
                                }),
                            )),
                    )
            }
            PopoverKind::Choose { rows, rewrite } => panel(theme)
                .debug_selector(move || {
                    if *rewrite {
                        "link-fixes".to_owned()
                    } else {
                        "link-candidates".to_owned()
                    }
                })
                .children(rows.iter().enumerate().map(|(i, choice)| {
                    row(theme, i == popover.highlighted)
                        .id(("link-choice", i))
                        .debug_selector(move || format!("link-choice-{i}"))
                        .child(div().truncate().child(choice.label.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_popover(Some(i), window, cx);
                        }))
                        .into_any_element()
                })),
        };
        let at = popover.at + point(px(0.0), px(spacing::OFFSET_Y));
        Some(
            deferred(
                anchored().position(at).snap_to_window().child(
                    div()
                        .id("link-popover")
                        .key_context(crate::keymap::context::MENU)
                        .track_focus(&self.link_ux.focus)
                        .on_action(cx.listener(|this, _: &crate::menu::SelectNext, _, cx| {
                            this.move_highlight(1, cx);
                        }))
                        .on_action(cx.listener(|this, _: &crate::menu::SelectPrevious, _, cx| {
                            this.move_highlight(-1, cx);
                        }))
                        .on_action(cx.listener(|this, _: &crate::menu::Confirm, window, cx| {
                            this.confirm_popover(None, window, cx);
                        }))
                        .on_action(cx.listener(|this, _: &crate::menu::Cancel, window, cx| {
                            this.close_link_popover(window, cx);
                        }))
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.close_link_popover(window, cx);
                        }))
                        .child(body),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// A button of the Create note popover: primary (`accent.default` fill)
/// or secondary (the infobar's look). Not designed beyond "a primary and a
/// secondary button".
fn button(id: &'static str, label: &'static str, primary: bool, cx: &App) -> Stateful<Div> {
    let theme = Appearance::theme(cx);
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .h(px(spacing::BUTTON_HEIGHT))
        .px(px(spacing::BUTTON_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .font_weight(FontWeight(text::weight::ACTIVE))
        .map(|b| {
            if primary {
                b.bg(theme.accent.default).text_color(theme.accent.on)
            } else {
                b.border_1()
                    .border_color(theme.border.strong)
                    .bg(theme.surface.raised)
                    .text_color(theme.text.primary)
                    .hover(move |s| s.bg(hover))
                    .active(move |s| s.bg(press))
            }
        })
        .child(label)
}

/// `path` below the vault root, for a row.
fn shown_path(path: &Path, root: Option<&Path>) -> String {
    root.and_then(|r| path.strip_prefix(r).ok())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The infobar after following the ambiguous link `follow`.
fn ambiguous_notice(follow: &AmbiguousFollow, root: Option<PathBuf>) -> Notice {
    let name = crate::reader::link_info::link_name(follow.link.kind, &follow.link.destination);
    let opened = shown_path(&follow.opened, root.as_deref());
    let why = follow.rewrite.is_none().then(|| {
        "The vault's content index is not ready, or the link cannot be rewritten.".to_owned()
    });
    Notice::ambiguous_link(follow.candidates.len(), &name, &opened, why)
}

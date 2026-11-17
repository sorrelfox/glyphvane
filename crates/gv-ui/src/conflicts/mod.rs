// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Sync conflicts panel of the sidebar (FR-VAULT-11; plan 10 §
//! Cloud-sync awareness, "Sync conflicts" panel, Should).
//!
//! It lists the conflict copies sync clients left in the vault (Dropbox,
//! Syncthing, OneDrive and iCloud naming patterns), from the vault's
//! content index ([`ContentSnapshot::sync_conflicts`]): one entry per
//! copy, its name, then "Dropbox copy of Note.md" and the folder, then
//! its actions:
//!
//! - **Compare**: the window opens the original and compares it with the
//!   copy (the compare and merge view, [`crate::compare`]: the original's
//!   buffer on the left, the copy only read on the right; *Apply* changes
//!   the buffer as one undoable edit). Enter on the selected entry does
//!   the same.
//! - **Open copy**: the window opens the copy as a tab.
//! - **Keep original**: the copy goes to the trash.
//! - **Use copy**: the original goes to the trash and the copy takes its
//!   name (the open tab reloads it, or shows the conflict infobar when it
//!   has unsaved edits).
//!
//! Both resolutions only move files to the trash, so either can be undone
//! from there; they run off the UI thread
//! ([`gv_vault::files::resolve_sync_conflict`]). An entry being resolved
//! is dimmed and gone once done; the next index snapshot confirms it.
//!
//! Keys ([`key_bindings`]): Up and Down select, Enter compares, Esc gives
//! the keyboard back to the document.
//!
//! Not designed (flagged): the entry's layout, its text buttons, the
//! empty and indexing states, and the error line.

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use gv_vault::VaultPath;
use gv_vault::content::{ContentSnapshot, SyncConflictCopy};
use gv_vault::files::{SyncChoice, resolve_sync_conflict};
use gv_vault::index::conflict::SyncService;
use gv_vault::refactor::{SystemTrash, Trash};

use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::spacing::{list, list_row, panel_head};
use crate::metrics::{DISABLED_OPACITY, icon, radius, size, text};
use crate::theme::Theme;

/// The key context of the panel.
pub(crate) const KEY_CONTEXT: &str = "SyncConflicts";

gpui::actions!(
    sync_conflicts,
    [
        /// Select the entry below.
        SelectNext,
        /// Select the entry above.
        SelectPrevious,
        /// Compare the selected copy with its original.
        CompareSelected,
        /// Give the keyboard back to the document.
        Dismiss,
    ]
);

/// The panel's keys.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("up", SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", CompareSelected, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
    ]
}

/// What the panel lists: the vault's root, and its content index once
/// built.
#[derive(Debug, Clone)]
pub struct ConflictsScope {
    pub root: PathBuf,
    pub content: Option<ContentSnapshot>,
}

/// What the panel asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncConflictsEvent {
    /// Open `original` and compare it with `copy` (only read).
    Compare { original: PathBuf, copy: PathBuf },
    /// Open the copy as a tab.
    Open(PathBuf),
    /// A conflict was settled (the files are already in place).
    Resolved {
        original: PathBuf,
        copy: PathBuf,
        choice: SyncChoice,
    },
    /// Settling failed; the sentence says why (also shown in the panel).
    Failed(String),
    /// Esc: the keyboard goes back to the document.
    Dismiss,
}

/// The name of a sync service, as the panel says it.
pub fn service_name(service: SyncService) -> &'static str {
    match service {
        SyncService::Dropbox => "Dropbox",
        SyncService::Syncthing => "Syncthing",
        SyncService::OneDrive => "OneDrive",
        SyncService::ICloud => "iCloud",
    }
}

/// The line under a copy's name: "Dropbox copy of Note.md · Folder".
pub fn entry_detail(conflict: &SyncConflictCopy) -> String {
    let name = conflict.original.file_name().unwrap_or("");
    let mut text = format!("{} copy of {name}", service_name(conflict.service));
    if let Some(folder) = conflict.original.parent().filter(|p| !p.is_root()) {
        text.push_str(" \u{b7} ");
        text.push_str(folder.as_str());
    }
    text
}

/// The Sync conflicts panel of a window's sidebar.
pub struct SyncConflictsPanel {
    focus: FocusHandle,
    scope: Option<ConflictsScope>,
    entries: Vec<SyncConflictCopy>,
    selected: Option<usize>,
    /// Copies being settled.
    busy: HashSet<VaultPath>,
    /// Copies settled since the last snapshot.
    done: HashSet<VaultPath>,
    error: Option<SharedString>,
    trash: Arc<dyn Trash>,
}

impl EventEmitter<SyncConflictsEvent> for SyncConflictsPanel {}

impl std::fmt::Debug for SyncConflictsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncConflictsPanel")
            .field("entries", &self.entries.len())
            .field("selected", &self.selected)
            .field("busy", &self.busy.len())
            .finish_non_exhaustive()
    }
}

impl Focusable for SyncConflictsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SyncConflictsPanel {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            scope: None,
            entries: Vec::new(),
            selected: None,
            busy: HashSet::new(),
            done: HashSet::new(),
            error: None,
            trash: Arc::new(SystemTrash),
        }
    }

    /// Where settled files go (the system trash by default; the window's
    /// own, and a fake one in tests).
    pub fn set_trash(&mut self, trash: Arc<dyn Trash>) {
        self.trash = trash;
    }

    /// The vault and its latest content index. A new snapshot lists the
    /// copies again.
    pub fn set_scope(&mut self, scope: Option<ConflictsScope>, cx: &mut Context<'_, Self>) {
        let same_root = self.scope.as_ref().map(|s| &s.root) == scope.as_ref().map(|s| &s.root);
        self.entries = scope
            .as_ref()
            .and_then(|s| s.content.as_ref())
            .map(ContentSnapshot::sync_conflicts)
            .unwrap_or_default();
        self.done
            .retain(|p| self.entries.iter().any(|e| &e.copy == p));
        self.entries.retain(|e| !self.done.contains(&e.copy));
        if !same_root {
            self.selected = None;
            self.error = None;
            self.busy.clear();
            self.done.clear();
        }
        self.scope = scope;
        cx.notify();
    }

    /// The copies listed.
    pub fn entries(&self) -> &[SyncConflictCopy] {
        &self.entries
    }

    /// The selected entry, kept inside the list.
    pub fn selected(&self) -> Option<usize> {
        self.selected.filter(|&s| s < self.entries.len())
    }

    /// The last error, until the next action.
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    fn root(&self) -> Option<PathBuf> {
        self.scope.as_ref().map(|s| s.root.clone())
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<'_, Self>) {
        let count = self.entries.len();
        if count == 0 {
            return;
        }
        self.selected = Some(self.selected().map_or(count - 1, |s| s.saturating_sub(1)));
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<'_, Self>) {
        let count = self.entries.len();
        if count == 0 {
            return;
        }
        self.selected = Some(self.selected().map_or(0, |s| (s + 1).min(count - 1)));
        cx.notify();
    }

    fn compare_selected(
        &mut self,
        _: &CompareSelected,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(ix) = self.selected() {
            self.compare(ix, cx);
        }
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<'_, Self>) {
        cx.emit(SyncConflictsEvent::Dismiss);
    }

    /// Compare entry `ix` with its original.
    pub fn compare(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        let (Some(root), Some(e)) = (self.root(), self.entries.get(ix)) else {
            return;
        };
        self.selected = Some(ix);
        cx.emit(SyncConflictsEvent::Compare {
            original: e.original.to_fs_path(&root),
            copy: e.copy.to_fs_path(&root),
        });
        cx.notify();
    }

    /// Open entry `ix`'s copy.
    pub fn open_copy(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        let (Some(root), Some(e)) = (self.root(), self.entries.get(ix)) else {
            return;
        };
        self.selected = Some(ix);
        cx.emit(SyncConflictsEvent::Open(e.copy.to_fs_path(&root)));
        cx.notify();
    }

    /// Settles entry `ix` as `choice`, off the UI thread.
    pub fn resolve(&mut self, ix: usize, choice: SyncChoice, cx: &mut Context<'_, Self>) {
        let (Some(root), Some(e)) = (self.root(), self.entries.get(ix).cloned()) else {
            return;
        };
        if !self.busy.insert(e.copy.clone()) {
            return;
        }
        self.error = None;
        let trash = self.trash.clone();
        let (copy, original) = (e.copy.clone(), e.original.clone());
        let at = root.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    resolve_sync_conflict(&at, &copy, &original, choice, &*trash)
                })
                .await;
            this.update(cx, |panel, cx| {
                panel.busy.remove(&e.copy);
                match result {
                    Ok(()) => {
                        panel.done.insert(e.copy.clone());
                        panel.entries.retain(|x| x.copy != e.copy);
                        cx.emit(SyncConflictsEvent::Resolved {
                            original: e.original.to_fs_path(&root),
                            copy: e.copy.to_fs_path(&root),
                            choice,
                        });
                    }
                    Err(err) => {
                        let why = sentence(&err.to_string());
                        panel.error = Some(why.clone().into());
                        cx.emit(SyncConflictsEvent::Failed(why));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn render_head(&self, theme: &Theme) -> Div {
        let count = self.entries.len();
        div()
            .h(px(size::PANEL_HEADER))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(panel_head::GAP))
            .pl(px(panel_head::PAD_LEFT))
            .pr(px(panel_head::PAD_RIGHT))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child("Sync conflicts"),
            )
            .when(count > 0, |head| {
                head.child(
                    div()
                        .debug_selector(|| "conflicts-count".into())
                        .flex_none()
                        .text_color(theme.text.tertiary)
                        .text_size(px(text::UI_SMALL.size))
                        .child(count.to_string()),
                )
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn action(
        &self,
        id: ElementId,
        selector: String,
        label: &'static str,
        theme: &Theme,
        enabled: bool,
        on_click: impl Fn(&mut Self, &mut Context<'_, Self>) + 'static,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (hover, press) = (theme.state.hover, theme.state.press);
        let button = div()
            .id(id)
            .debug_selector(move || selector.clone())
            .flex_none()
            .px(px(list_row::GAP))
            .rounded(px(radius::CONTROL))
            .text_color(theme.accent.default)
            .text_size(px(text::UI_SMALL.size))
            .line_height(px(text::UI_SMALL.line))
            .child(label);
        if enabled {
            button
                .hover(move |s| s.bg(hover))
                .active(move |s| s.bg(press))
                .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
                .into_any_element()
        } else {
            button.opacity(DISABLED_OPACITY).into_any_element()
        }
    }

    fn render_entry(
        &self,
        ix: usize,
        entry: &SyncConflictCopy,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let selected = self.selected() == Some(ix);
        let busy = self.busy.contains(&entry.copy);
        let name: SharedString = entry.copy.file_name().unwrap_or("").to_owned().into();
        let detail: SharedString = entry_detail(entry).into();
        let action =
            |id: &str, label: &'static str, f: fn(&mut Self, usize, &mut Context<'_, Self>)| {
                let selector = format!("conflict-{id}-{ix}");
                (ElementId::Name(selector.clone().into()), selector, label, f)
            };
        let actions = [
            action("compare", "Compare", |p, ix, cx| p.compare(ix, cx)),
            action("open", "Open copy", |p, ix, cx| p.open_copy(ix, cx)),
            action("keep", "Keep original", |p, ix, cx| {
                p.resolve(ix, SyncChoice::KeepOriginal, cx);
            }),
            action("use", "Use copy", |p, ix, cx| {
                p.resolve(ix, SyncChoice::UseCopy, cx);
            }),
        ];
        let buttons: Vec<AnyElement> = actions
            .into_iter()
            .map(|(id, selector, label, f)| {
                self.action(
                    id,
                    selector,
                    label,
                    theme,
                    !busy,
                    move |this, cx| f(this, ix, cx),
                    cx,
                )
            })
            .collect();
        let hover = theme.state.hover;
        div()
            .id(ElementId::Name(format!("conflict-{ix}").into()))
            .debug_selector(move || format!("conflict-{ix}"))
            .mx(px(size::LIST_ROW_INSET))
            .pl(px(list_row::PAD_LEFT))
            .pr(px(list_row::PAD_RIGHT))
            .py(px(list::PAD_Y * 2.0))
            .rounded(px(radius::CONTROL))
            .flex()
            .flex_col()
            .map(|e| {
                if selected {
                    e.bg(theme.accent.tint)
                } else {
                    e.hover(move |s| s.bg(hover))
                }
            })
            .when(busy, |e| e.opacity(DISABLED_OPACITY))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(ix);
                cx.notify();
            }))
            .child(
                div()
                    .h(px(size::LIST_ROW))
                    .flex()
                    .items_center()
                    .gap(px(list_row::GAP))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.status.warning)
                            .child(Icon::SyncConflict.sized(icon::ROW)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(name)),
            )
            .child(
                div()
                    .text_color(theme.text.secondary)
                    .text_size(px(text::UI_SMALL.size))
                    .line_height(px(text::UI_SMALL.line))
                    .truncate()
                    .child(detail),
            )
            .child(
                div()
                    .mt(px(list::PAD_Y))
                    .flex()
                    .flex_wrap()
                    .gap(px(list::PAD_Y))
                    .children(buttons),
            )
            .into_any_element()
    }
}

/// A quiet one-line state where the entries would be.
fn message(note: &'static str, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "conflicts-message".into())
        .h(px(size::LIST_ROW))
        .flex_none()
        .mt(px(list::PAD_Y))
        .mx(px(size::LIST_ROW_INSET))
        .pl(px(list_row::PAD_LEFT))
        .flex()
        .items_center()
        .text_color(theme.text.secondary)
        .child(note)
}

/// "cannot move …" → "Cannot move ….", for the error line.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    let mut text: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !text.ends_with('.') {
        text.push('.');
    }
    text
}

impl Render for SyncConflictsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let head = self.render_head(theme);
        let indexed = self.scope.as_ref().is_some_and(|s| s.content.is_some());
        let entries: Vec<AnyElement> = self
            .entries
            .iter()
            .enumerate()
            .map(|(ix, e)| self.render_entry(ix, e, theme, cx))
            .collect();
        let error = self.error.clone();
        div()
            .id("sync-conflicts-panel")
            .debug_selector(|| "sidebar-panel:sync-conflicts".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::compare_selected))
            .on_action(cx.listener(Self::dismiss))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(head)
            .when_some(error, |panel, why| {
                panel.child(
                    div()
                        .debug_selector(|| "conflicts-error".into())
                        .mx(px(size::LIST_ROW_INSET))
                        .pl(px(list_row::PAD_LEFT))
                        .text_color(theme.status.error)
                        .text_size(px(text::UI_SMALL.size))
                        .child(why),
                )
            })
            .map(|panel| {
                if self.scope.is_none() {
                    panel.child(message("No folder is open", theme))
                } else if !indexed {
                    panel.child(message("Indexing\u{2026}", theme))
                } else if entries.is_empty() {
                    panel.child(message("No sync conflicts", theme))
                } else {
                    panel.child(
                        div()
                            .id("conflict-entries")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .py(px(list::PAD_Y))
                            .flex()
                            .flex_col()
                            .gap(px(list::PAD_Y))
                            .children(entries),
                    )
                }
            })
    }
}

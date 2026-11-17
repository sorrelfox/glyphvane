// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Files panel: the vault's folders and files as a tree, with Git
//! status badges when the vault is in a work tree ([`super::git`]).
//!
//! The panel shows whatever snapshot of the path index its [`WindowVault`]
//! last published and redraws when a newer one arrives, keeping open
//! folders and the selection. The active tab's file is selected and
//! revealed. Rows are drawn lazily, so the size of the vault does not
//! matter. A right-click on a row selects it and opens the file tree's
//! context menu.
//!
//! The head's buttons: New note, New folder, Sort (a list of orders in
//! the Go popover; not designed), Collapse all. The title opens the vault
//! switcher. **Filter** (not designed): Mod+F in the tree shows a field
//! above the rows; typing keeps the entries whose name contains the text
//! and the folders above them, Enter goes to the rows, Esc clears it.

use std::cell::Cell;
use std::f32::consts::FRAC_PI_2;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Point, Render, ScrollStrategy, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle,
    Window, canvas, div, px, radians, uniform_list,
};
use gv_vault::VaultPath;

use super::controls::icon_button;
use super::drag_drop::{DragPreview, DraggedFile};
use super::tree::{RowKind, SortOrder, TreeModel};
use super::vault::{VaultStatus, WindowVault};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::actions::file::{NewFolder, NewNote};
use crate::metrics::spacing::{list, list_row, panel_head};
use crate::metrics::{icon, radius, size, text};
use crate::theme::Theme;

gpui::actions!(
    file_tree,
    [
        /// Select the row above.
        SelectPrevious,
        /// Select the row below.
        SelectNext,
        /// Close the selected folder, or select its parent folder.
        CollapseOrParent,
        /// Open the selected folder, or select its first entry.
        ExpandOrChild,
        /// Open the selected file, or open or close the selected folder.
        OpenSelected,
        /// Show the filter field and put the keyboard in it.
        Filter,
        /// Clear the filter and hide its field.
        ClearFilter,
        /// Leave the filter field for the rows.
        FocusRows,
    ]
);

/// The key context of the filter field.
pub(crate) const FILTER_CONTEXT: &str = "FileTreeFilter";

/// The key context of the tree.
pub(crate) const KEY_CONTEXT: &str = "FileTree";

/// The tree's keys. They apply while the Files panel has the focus.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let context = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("left", CollapseOrParent, context),
        KeyBinding::new("right", ExpandOrChild, context),
        KeyBinding::new("enter", OpenSelected, context),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-f"
            } else {
                "ctrl-f"
            },
            Filter,
            context,
        ),
        KeyBinding::new("escape", ClearFilter, Some("FileTreeFilter > Input")),
        KeyBinding::new("enter", FocusRows, Some("FileTreeFilter > Input")),
        KeyBinding::new("down", FocusRows, Some("FileTreeFilter > Input")),
    ]
}

/// What the panel asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilesPanelEvent {
    /// Open this file as a tab.
    Open(PathBuf),
    /// The title was clicked: show the vault switcher.
    SwitchVault,
    /// The Sort button was clicked: show the sort orders.
    Sort,
    /// An entry was dragged onto a folder (the vault folder for the
    /// panel below the rows): move it there.
    MoveInto { from: VaultPath, folder: VaultPath },
}

/// The Files panel of a window's sidebar.
pub(crate) struct FilesPanel {
    focus: FocusHandle,
    vault: Option<Entity<WindowVault>>,
    /// `None` until the vault's first complete snapshot.
    tree: Option<TreeModel>,
    /// The active tab's file, selected and revealed.
    active: Option<PathBuf>,
    scroll: UniformListScrollHandle,
    /// The selected row as last drawn, in window coordinates, for the
    /// context menu key.
    selected_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The sort order, kept when the tree is made again.
    sort: SortOrder,
    /// The filter field, while it shows.
    filter: Option<Entity<InputState>>,
    _filter_events: Option<Subscription>,
    _vault_changed: Option<Subscription>,
}

impl std::fmt::Debug for FilesPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesPanel")
            .field("vault", &self.vault)
            .field("tree", &self.tree)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<FilesPanelEvent> for FilesPanel {}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FilesPanel {
    pub fn new(
        vault: Option<Entity<WindowVault>>,
        active: Option<PathBuf>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            vault: None,
            tree: None,
            active,
            scroll: UniformListScrollHandle::new(),
            selected_bounds: Rc::default(),
            sort: SortOrder::default(),
            filter: None,
            _filter_events: None,
            _vault_changed: None,
        };
        if let Some(vault) = vault {
            this.set_vault(vault, cx);
        }
        this
    }

    pub fn set_vault(&mut self, vault: Entity<WindowVault>, cx: &mut Context<'_, Self>) {
        self._vault_changed = Some(cx.observe(&vault, |this, _, cx| this.sync(cx)));
        self.vault = Some(vault);
        self.tree = None;
        self.sync(cx);
    }

    /// The active tab changed: select and reveal its file.
    pub fn set_active(&mut self, path: Option<PathBuf>, cx: &mut Context<'_, Self>) {
        if self.active != path {
            self.active = path;
            self.reveal_active();
            cx.notify();
        }
    }

    /// Adopts the vault's current snapshot if it is complete and newer.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let snapshot = vault.read(cx).snapshot();
        if snapshot.is_complete() {
            match &mut self.tree {
                Some(tree) if tree.index().generation() == snapshot.generation() => {}
                Some(tree) => tree.set_index(snapshot),
                None => {
                    let mut tree = TreeModel::new(snapshot);
                    tree.set_sort(self.sort);
                    if let Some(filter) = &self.filter {
                        tree.set_filter(&filter.read(cx).value());
                    }
                    self.tree = Some(tree);
                    self.reveal_active();
                }
            }
        }
        cx.notify();
    }

    /// Selects the active tab's file again and scrolls to it
    /// (`workspace.reveal_in_tree`).
    pub fn reveal(&mut self, cx: &mut Context<'_, Self>) {
        self.reveal_active();
        cx.notify();
    }

    /// Selects the vault's file `path` and scrolls to it (`links.reveal_in_tree`).
    pub fn reveal_path(&mut self, path: &Path, cx: &mut Context<'_, Self>) {
        if let Some(tree) = &mut self.tree
            && let Some(id) = tree.id_of_fs_path(path)
        {
            tree.reveal(id);
            self.scroll_to_selection();
        }
        cx.notify();
    }

    /// Selects the vault entry `path` and scrolls to it (after a file
    /// operation). Does nothing until the snapshot has it.
    pub fn reveal_vault_path(&mut self, path: &VaultPath, cx: &mut Context<'_, Self>) {
        self.sync(cx);
        if let Some(tree) = &mut self.tree
            && let Some(id) = tree.index().id_of(path.as_str())
        {
            tree.reveal(id);
            self.scroll_to_selection();
        }
        cx.notify();
    }

    /// The order of the entries.
    pub fn sort(&self) -> SortOrder {
        self.sort
    }

    pub fn set_sort(&mut self, sort: SortOrder, cx: &mut Context<'_, Self>) {
        self.sort = sort;
        if let Some(tree) = &mut self.tree {
            tree.set_sort(sort);
            self.scroll_to_selection();
        }
        cx.notify();
    }

    /// Mod+F: shows the filter field and puts the keyboard in it.
    fn show_filter(&mut self, _: &Filter, window: &mut Window, cx: &mut Context<'_, Self>) {
        let input = match &self.filter {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| {
                    let mut state = InputState::new(window, cx);
                    state.set_placeholder("Filter files", window, cx);
                    state
                });
                self._filter_events = Some(cx.subscribe_in(
                    &input,
                    window,
                    |this, input, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            let text = input.read(cx).value();
                            if let Some(tree) = &mut this.tree {
                                tree.set_filter(&text);
                            }
                            cx.notify();
                        }
                    },
                ));
                self.filter = Some(input.clone());
                input
            }
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Esc in the filter field: everything shows again.
    fn clear_filter(&mut self, _: &ClearFilter, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.filter = None;
        self._filter_events = None;
        if let Some(tree) = &mut self.tree {
            tree.set_filter("");
        }
        self.scroll_to_selection();
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Enter or Down in the filter field: the rows take the keyboard,
    /// with the first one selected.
    fn focus_rows(&mut self, _: &FocusRows, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(tree) = &mut self.tree
            && tree.selected_row().is_none()
        {
            tree.select_next();
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Filters the rows by `text`, as typing in the filter field does
    /// (tests).
    #[cfg(test)]
    pub fn set_filter_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.show_filter(&Filter, window, cx);
        if let Some(input) = &self.filter {
            input.update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
        }
        if let Some(tree) = &mut self.tree {
            tree.set_filter(text);
        }
        cx.notify();
    }

    /// The selected row, where the context menu key opens the menu.
    pub fn selected_row_bounds(&self) -> Option<Bounds<Pixels>> {
        self.selected_bounds.get()
    }

    /// A right-click on row `row`: selects it without opening it and opens
    /// the file tree's context menu at `position`.
    fn context_row(
        &mut self,
        row: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.focus.focus(window, cx);
        let Some(tree) = &mut self.tree else {
            return;
        };
        let Some(id) = tree.rows().get(row).map(|r| r.id) else {
            return;
        };
        tree.select(id);
        cx.notify();
        crate::menu::show_context_menu(crate::menu::ContextMenu::FileTree, position, window, cx);
    }

    fn reveal_active(&mut self) {
        let (Some(tree), Some(path)) = (&mut self.tree, &self.active) else {
            return;
        };
        if let Some(id) = tree.id_of_fs_path(path) {
            tree.reveal(id);
            self.scroll_to_selection();
        }
    }

    fn scroll_to_selection(&self) {
        if let Some(row) = self.tree.as_ref().and_then(TreeModel::selected_row) {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    fn moved(&mut self, moved: bool, cx: &mut Context<'_, Self>) {
        if moved {
            self.scroll_to_selection();
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<'_, Self>) {
        let moved = self.tree.as_mut().is_some_and(TreeModel::select_previous);
        self.moved(moved, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<'_, Self>) {
        let moved = self.tree.as_mut().is_some_and(TreeModel::select_next);
        self.moved(moved, cx);
    }

    fn collapse_or_parent(
        &mut self,
        _: &CollapseOrParent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let moved = self
            .tree
            .as_mut()
            .is_some_and(TreeModel::collapse_or_parent);
        self.moved(moved, cx);
    }

    fn expand_or_child(&mut self, _: &ExpandOrChild, _: &mut Window, cx: &mut Context<'_, Self>) {
        let moved = self.tree.as_mut().is_some_and(TreeModel::expand_or_child);
        self.moved(moved, cx);
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(id) = self.tree.as_ref().and_then(TreeModel::selected) {
            self.activate(id, cx);
        }
    }

    /// A folder opens or closes; a file opens as a tab.
    fn activate(&mut self, id: gv_vault::FileId, cx: &mut Context<'_, Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        if tree.is_dir(id) {
            tree.toggle(id);
            cx.notify();
        } else if let Some(path) = tree.fs_path(id) {
            cx.emit(FilesPanelEvent::Open(path));
        }
    }

    fn click_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.focus.focus(window, cx);
        let Some(tree) = &mut self.tree else {
            return;
        };
        let Some(id) = tree.rows().get(row).map(|r| r.id) else {
            return;
        };
        tree.select(id);
        self.activate(id, cx);
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(tree) = &mut self.tree {
            tree.collapse_all();
            self.scroll_to_selection();
            cx.notify();
        }
    }

    /// The vault name with the switcher's chevron, then the actions.
    fn render_head(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let title = self
            .vault
            .as_ref()
            .map_or_else(|| SharedString::from("Files"), |v| v.read(cx).name());
        let action = |id: &'static str, icon: Icon| {
            let size = (size::SMALL_ICON_BUTTON, size::SMALL_ICON_BUTTON);
            icon_button(id, icon, icon::ROW, size, theme).debug_selector(move || id.into())
        };
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
                    .id("vault-switcher")
                    .debug_selector(|| "vault-switcher".into())
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FilesPanelEvent::SwitchVault)))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(panel_head::TITLE_GAP))
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child(div().min_w_0().truncate().child(title))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text.secondary)
                            .child(Icon::ChevronDown.sized(icon::CHEVRON)),
                    ),
            )
            .child(action("new-note", Icon::NewNote).on_click(|_, window, cx| {
                window.dispatch_action(Box::new(NewNote), cx);
            }))
            .child(
                action("new-folder", Icon::NewFolder).on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(NewFolder), cx);
                }),
            )
            .child(
                action("sort-files", Icon::Sort)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FilesPanelEvent::Sort))),
            )
            .child(
                action("collapse-all", Icon::CollapseAll)
                    .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
            )
    }

    fn render_body(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> AnyElement {
        let Some(vault) = &self.vault else {
            return message("No folder open", theme).into_any_element();
        };
        match (vault.read(cx).status(), &self.tree) {
            (VaultStatus::NotIndexed(note) | VaultStatus::Failed(note), _) => {
                message(note.clone(), theme).into_any_element()
            }
            (_, None) | (VaultStatus::Building, _) => {
                message("Indexing\u{2026}", theme).into_any_element()
            }
            (VaultStatus::Ready, Some(tree)) if tree.rows().is_empty() => {
                let note = if tree.filter().is_some() {
                    "No matching files"
                } else {
                    "No files"
                };
                message(note, theme).into_any_element()
            }
            (VaultStatus::Ready, Some(tree)) => div()
                .flex_1()
                .min_h_0()
                .py(px(list::PAD_Y))
                // Below the rows is the vault folder.
                .on_drop(cx.listener(|_, file: &DraggedFile, _, cx| {
                    cx.emit(FilesPanelEvent::MoveInto {
                        from: file.vault_path.clone(),
                        folder: VaultPath::root(),
                    });
                }))
                .child(
                    uniform_list(
                        "file-tree",
                        tree.rows().len(),
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            this.render_rows(range, cx)
                        }),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .into_any_element(),
        }
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<Div> {
        let theme = Appearance::theme(cx);
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let selected = tree.selected_row();
        if selected.is_none_or(|row| !range.contains(&row)) {
            self.selected_bounds.set(None);
        }
        let git = self.vault.as_ref().and_then(|v| v.read(cx).git().cloned());
        let rows =
            &tree.rows()[range.start.min(tree.rows().len())..range.end.min(tree.rows().len())];
        rows.iter()
            .zip(range)
            .map(|(row, ix)| {
                let chevron = div()
                    .flex_none()
                    .size(px(icon::CHEVRON))
                    .text_color(theme.text.secondary)
                    .when(row.kind == RowKind::Folder, |slot| {
                        let chevron = Icon::ChevronRight.sized(icon::CHEVRON);
                        slot.child(if row.expanded {
                            chevron.rotate(radians(FRAC_PI_2))
                        } else {
                            chevron
                        })
                    });
                let entry = tree.index().get(row.id);
                let dragged = entry.map(|e| DraggedFile {
                    path: e.path.to_fs_path(tree.index().root()),
                    vault_path: e.path.clone(),
                    is_dir: e.kind.is_dir(),
                });
                let folder = entry.filter(|e| e.kind.is_dir()).map(|e| e.path.clone());
                let status = git
                    .as_deref()
                    .zip(entry)
                    .and_then(|(git, e)| super::git::row_status(git, &e.path, e.kind.is_dir()));
                let ignored = status == Some(gv_vault::git::FileStatus::Ignored);
                let drop_tint = theme.accent.tint;
                let label = row.name.clone();
                let row_frame =
                    list_row_frame(("file-row", ix), row.depth, selected == Some(ix), theme)
                        .debug_selector(move || format!("file-row-{ix}"))
                        .when_some(dragged, |frame, dragged| {
                            frame.on_drag(dragged, move |_, _, _, cx| {
                                DragPreview::view(label.clone(), cx)
                            })
                        })
                        .when_some(folder, |frame, folder| {
                            // A folder takes the entries dropped on it
                            // (not designed: the row's tint while over it).
                            frame
                                .drag_over::<DraggedFile>(move |style, _, _, _| style.bg(drop_tint))
                                .on_drop(cx.listener(move |_, file: &DraggedFile, _, cx| {
                                    cx.stop_propagation();
                                    cx.emit(FilesPanelEvent::MoveInto {
                                        from: file.vault_path.clone(),
                                        folder: folder.clone(),
                                    });
                                }))
                        })
                        .child(chevron)
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.text.secondary)
                                .child(row_icon(row.kind).sized(icon::ROW)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(ignored, |name| name.text_color(theme.text.secondary))
                                .child(row.name.clone()),
                        )
                        .children(status.and_then(|s| super::git::badge(ix, s, theme)))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.click_row(ix, window, cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.context_row(ix, e.position, window, cx);
                            }),
                        )
                        .when(selected == Some(ix), |row| {
                            let bounds = self.selected_bounds.clone();
                            row.child(
                                canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                            )
                        });
                // The list sizes its items to their content: a full-width
                // slot holds the row and its side margins.
                div()
                    .w_full()
                    .h(px(size::LIST_ROW))
                    .px(px(size::LIST_ROW_INSET))
                    .child(row_frame)
            })
            .collect()
    }

    /// The selected row's vault path, and whether it is a folder.
    pub fn selected_entry(&self) -> Option<(VaultPath, bool)> {
        let tree = self.tree.as_ref()?;
        let row = tree.rows().get(tree.selected_row()?)?;
        let entry = tree.index().get(row.id)?;
        Some((entry.path.clone(), entry.kind.is_dir()))
    }

    /// The filter field, while it shows.
    fn render_filter(&self, theme: &Theme) -> Option<Div> {
        let input = self.filter.as_ref()?;
        Some(
            div()
                .key_context(FILTER_CONTEXT)
                .debug_selector(|| "file-filter".into())
                .flex_none()
                .h(px(size::FIELD))
                .mx(px(size::LIST_ROW_INSET))
                .mt(px(list::PAD_Y))
                .px(px(list_row::PAD_LEFT))
                .flex()
                .items_center()
                .rounded(px(radius::CONTROL))
                .bg(theme.surface.sunken)
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(input)
                            .aria_label("Filter files")
                            .appearance(false)
                            .px_0()
                            .py_0()
                            .text_color(theme.text.primary),
                    ),
                ),
        )
    }

    /// The selected row's file or folder.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let tree = self.tree.as_ref()?;
        let row = tree.rows().get(tree.selected_row()?)?;
        tree.fs_path(row.id)
    }

    /// The rows as `(depth, name)`, for tests.
    #[cfg(test)]
    pub fn visible_rows(&self) -> Vec<(usize, String)> {
        self.tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.rows()
                .iter()
                .map(|r| (r.depth, r.name.to_string()))
                .collect()
        })
    }

    /// The selected row's name, for tests.
    #[cfg(test)]
    pub fn selected_name(&self) -> Option<String> {
        let tree = self.tree.as_ref()?;
        Some(tree.rows()[tree.selected_row()?].name.to_string())
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        div()
            .id("files-panel")
            .debug_selector(|| "files-panel".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::collapse_or_parent))
            .on_action(cx.listener(Self::expand_or_child))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::show_filter))
            .on_action(cx.listener(Self::clear_filter))
            .on_action(cx.listener(Self::focus_rows))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_head(theme, cx))
            .children(self.render_filter(theme))
            .child(self.render_body(theme, cx))
    }
}

/// A list row's frame, filling a slot inset 6 from the panel's sides:
/// indented 14 per level; `accent.tint` and weight 500 when selected, the
/// hover overlay otherwise.
pub(crate) fn list_row_frame(
    id: impl Into<gpui::ElementId>,
    depth: usize,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let hover = theme.state.hover;
    let indent = size::LIST_INDENT * depth as f32;
    div()
        .id(id.into())
        .size_full()
        .pl(px(list_row::PAD_LEFT + indent))
        .pr(px(list_row::PAD_RIGHT))
        .rounded(px(radius::CONTROL))
        .flex()
        .items_center()
        .gap(px(list_row::GAP))
        .map(|row| {
            if selected {
                row.bg(theme.accent.tint)
                    .font_weight(FontWeight(text::weight::ACTIVE))
            } else {
                row.hover(move |style| style.bg(hover))
            }
        })
}

/// A quiet one-line state where the rows would be: indexing, empty, not
/// indexed.
pub(crate) fn message(note: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "files-message".into())
        .h(px(size::LIST_ROW))
        .flex_none()
        .mt(px(list::PAD_Y))
        .mx(px(size::LIST_ROW_INSET))
        .pl(px(list_row::PAD_LEFT))
        .flex()
        .items_center()
        .text_color(theme.text.secondary)
        .child(note.into())
}

fn row_icon(kind: RowKind) -> Icon {
    match kind {
        RowKind::Folder => Icon::Folder,
        RowKind::Note => Icon::Note,
        RowKind::Canvas => Icon::Canvas,
        RowKind::Base => Icon::Base,
        RowKind::Image => Icon::ImageFile,
        RowKind::Other => Icon::OtherFile,
    }
}

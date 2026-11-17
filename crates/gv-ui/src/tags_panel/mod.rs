// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Tags panel (FR-VAULT-08; SPEC § Sidebar): the vault's tags as a
//! tree, nested tags (`#a/b/c`) under their parents, each with the number
//! of notes that have it or a tag nested under it.
//!
//! - **Rows** as designed: a chevron for tags with nested ones (open by
//!   default), `#` in `text.secondary` before a top-level name, the count
//!   at the right in 11 px `text.secondary`.
//! - **Head:** *Sort tags* (the orders in the Go popover, as the Files
//!   panel's sort; not designed) and *Collapse all*.
//! - **Click** or Enter on a tag runs the search `tag:#name` in the
//!   Search panel (10 § Tags and properties), as a click on a tag in the
//!   Reading view does. A click on a chevron opens or closes the tag.
//! - **Keys:** the file tree's (arrows, Enter). **Filter** (not
//!   designed, as in the Files panel): Mod+F shows a field above the rows;
//!   typing keeps the tags whose name contains the text and the tags above
//!   them, Enter goes to the rows, Esc clears it.
//! - **Live:** the window hands every new content index snapshot
//!   ([`TagsPanel::set_source`]); the tree is built again off the UI
//!   thread, keeping the order, the closed tags, the filter and the
//!   selection.

pub mod model;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::f32::consts::FRAC_PI_2;
use std::ops::Range;
use std::sync::Arc;

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    UniformListScrollHandle, Window, div, px, radians, uniform_list,
};
use gv_vault::content::ContentSnapshot;

use self::model::{TagRow, TagSort, TagTree};
use crate::a11y::{A11yExt as _, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::spacing::{list, list_row, panel_head};
use crate::metrics::{icon, radius, size, text};
use crate::shell::tree_keys::{
    ClearFilter, CollapseOrParent, ExpandOrChild, FILTER_CONTEXT, Filter, FocusRows, KEY_CONTEXT,
    OpenSelected, SelectNext, SelectPrevious,
};
use crate::shell::{icon_button, list_row_frame, panel_message};
use crate::theme::Theme;

/// What the panel shows tags from.
#[derive(Debug, Clone)]
pub enum TagsSource {
    /// The window has no vault.
    NoVault,
    /// The vault is not indexed (it waits for the user, or failed).
    NotIndexed(SharedString),
    /// The path or content index is being built.
    Indexing,
    /// The content index's latest snapshot.
    Ready(ContentSnapshot),
}

/// What the panel asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagsPanelEvent {
    /// Run this search in the Search panel (`tag:#name`).
    Search(String),
    /// The Sort button was clicked: show the orders.
    Sort,
}

/// Where the tree is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    NoVault,
    NotIndexed(SharedString),
    Indexing,
    Ready,
}

/// The Tags panel of a window's sidebar.
pub struct TagsPanel {
    focus: FocusHandle,
    phase: Phase,
    /// The snapshot generation the tree was built from.
    generation: Option<u64>,
    tree: Option<TagTree>,
    rows: Vec<TagRow>,
    sort: TagSort,
    /// Tags whose nested tags are hidden, by key.
    collapsed: HashSet<Arc<str>>,
    /// The selected tag, by key.
    selected: Option<Arc<str>>,
    scroll: UniformListScrollHandle,
    /// The filter field, while it shows.
    filter: Option<Entity<InputState>>,
    filter_text: String,
    _filter_events: Option<Subscription>,
    /// Increases with each build; older builds are dropped.
    job: u64,
    _build: Option<Task<()>>,
}

impl std::fmt::Debug for TagsPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TagsPanel")
            .field("phase", &self.phase)
            .field("generation", &self.generation)
            .field("rows", &self.rows.len())
            .field("sort", &self.sort)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<TagsPanelEvent> for TagsPanel {}

impl Focusable for TagsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TagsPanel {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            phase: Phase::NoVault,
            generation: None,
            tree: None,
            rows: Vec::new(),
            sort: TagSort::default(),
            collapsed: HashSet::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            filter: None,
            filter_text: String::new(),
            _filter_events: None,
            job: 0,
            _build: None,
        }
    }

    /// Follows the window's vault and its content index. A snapshot of a
    /// generation already shown changes nothing.
    pub fn set_source(&mut self, source: TagsSource, cx: &mut Context<'_, Self>) {
        let snapshot = match source {
            TagsSource::NoVault => return self.set_phase(Phase::NoVault, cx),
            TagsSource::NotIndexed(why) => return self.set_phase(Phase::NotIndexed(why), cx),
            TagsSource::Indexing => {
                // The tags of the last snapshot stay while a new one comes.
                if self.tree.is_none() {
                    self.set_phase(Phase::Indexing, cx);
                }
                return;
            }
            TagsSource::Ready(snapshot) => snapshot,
        };
        if self.generation == Some(snapshot.generation()) && self.tree.is_some() {
            return;
        }
        self.generation = Some(snapshot.generation());
        self.job += 1;
        let job = self.job;
        if self.tree.is_none() {
            self.phase = Phase::Indexing;
        }
        let build = cx.background_spawn(async move { model::build(&snapshot) });
        self._build = Some(cx.spawn(async move |this, cx| {
            let tree = build.await;
            this.update(cx, |this, cx| {
                if this.job == job {
                    this.tree = Some(tree);
                    this.phase = Phase::Ready;
                    this.rebuild_rows();
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn set_phase(&mut self, phase: Phase, cx: &mut Context<'_, Self>) {
        self.job += 1;
        self._build = None;
        self.generation = None;
        self.tree = None;
        self.rows.clear();
        if self.phase != phase {
            self.phase = phase;
        }
        cx.notify();
    }

    /// The order of the rows.
    pub fn sort(&self) -> TagSort {
        self.sort
    }

    pub fn set_sort(&mut self, sort: TagSort, cx: &mut Context<'_, Self>) {
        self.sort = sort;
        self.rebuild_rows();
        self.scroll_to_selection();
        cx.notify();
    }

    /// Hides the nested tags of every tag.
    pub fn collapse_all(&mut self, cx: &mut Context<'_, Self>) {
        let Some(tree) = &self.tree else {
            return;
        };
        fn walk(nodes: &[model::TagNode], out: &mut HashSet<Arc<str>>) {
            for node in nodes {
                if !node.children.is_empty() {
                    out.insert(node.key.clone());
                    walk(&node.children, out);
                }
            }
        }
        walk(&tree.roots, &mut self.collapsed);
        // A nested tag that was selected hands it to its top-level tag.
        if let Some(key) = &self.selected
            && let Some((top, _)) = key.split_once('/')
        {
            self.selected = Some(Arc::from(top));
        }
        self.rebuild_rows();
        self.scroll_to_selection();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        self.rows = match &self.tree {
            Some(tree) => model::rows(tree, self.sort, &self.filter_text, &self.collapsed),
            None => Vec::new(),
        };
    }

    fn selected_row(&self) -> Option<usize> {
        let key = self.selected.as_ref()?;
        self.rows.iter().position(|r| &r.key == key)
    }

    fn scroll_to_selection(&self) {
        if let Some(row) = self.selected_row() {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    fn select_row(&mut self, row: usize, cx: &mut Context<'_, Self>) {
        if let Some(r) = self.rows.get(row) {
            self.selected = Some(r.key.clone());
            self.scroll_to_selection();
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<'_, Self>) {
        let row = match self.selected_row() {
            Some(row) => row.saturating_sub(1),
            None => self.rows.len().saturating_sub(1),
        };
        self.select_row(row, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<'_, Self>) {
        let row = match self.selected_row() {
            Some(row) => (row + 1).min(self.rows.len().saturating_sub(1)),
            None => 0,
        };
        self.select_row(row, cx);
    }

    /// Left: closes the selected tag, or selects the tag above it.
    fn collapse_or_parent(
        &mut self,
        _: &CollapseOrParent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(row) = self.selected_row().and_then(|r| self.rows.get(r)).cloned() else {
            return;
        };
        if row.has_children && row.expanded && self.filter_text.is_empty() {
            self.toggle(&row.key, cx);
        } else if let Some((parent, _)) = row.key.rsplit_once('/') {
            self.selected = Some(Arc::from(parent));
            self.scroll_to_selection();
            cx.notify();
        }
    }

    /// Right: opens the selected tag, or selects its first nested tag.
    fn expand_or_child(&mut self, _: &ExpandOrChild, _: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(ix) = self.selected_row() else {
            return;
        };
        let row = self.rows[ix].clone();
        if !row.has_children {
            return;
        }
        if row.expanded {
            self.select_row(ix + 1, cx);
        } else {
            self.toggle(&row.key, cx);
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(row) = self.selected_row().and_then(|r| self.rows.get(r)) {
            cx.emit(TagsPanelEvent::Search(model::search_for(&row.tag)));
        }
    }

    /// Opens or closes the tag `key`.
    fn toggle(&mut self, key: &Arc<str>, cx: &mut Context<'_, Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.clone());
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn click_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.focus.focus(window, cx);
        let Some(r) = self.rows.get(row).cloned() else {
            return;
        };
        self.selected = Some(r.key.clone());
        cx.emit(TagsPanelEvent::Search(model::search_for(&r.tag)));
        cx.notify();
    }

    /// Mod+F: shows the filter field and puts the keyboard in it.
    fn show_filter(&mut self, _: &Filter, window: &mut Window, cx: &mut Context<'_, Self>) {
        let input = match &self.filter {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| {
                    let mut state = InputState::new(window, cx);
                    state.set_placeholder("Filter tags", window, cx);
                    state
                });
                self._filter_events = Some(cx.subscribe_in(
                    &input,
                    window,
                    |this, input, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            let text = input.read(cx).value().to_string();
                            this.apply_filter(text, cx);
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

    fn apply_filter(&mut self, text: String, cx: &mut Context<'_, Self>) {
        self.filter_text = text;
        self.rebuild_rows();
        self.scroll_to_selection();
        cx.notify();
    }

    /// Esc in the filter field: every tag shows again.
    fn clear_filter(&mut self, _: &ClearFilter, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.filter = None;
        self._filter_events = None;
        self.apply_filter(String::new(), cx);
        self.focus.focus(window, cx);
    }

    /// Enter or Down in the filter field: the rows take the keyboard,
    /// with the first one selected when none is.
    fn focus_rows(&mut self, _: &FocusRows, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.selected_row().is_none() {
            self.select_row(0, cx);
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
        self.apply_filter(text.to_owned(), cx);
    }

    /// The rows as `(depth, tag, notes)`, for tests.
    #[cfg(test)]
    pub fn visible_rows(&self) -> Vec<(usize, String, usize)> {
        self.rows
            .iter()
            .map(|r| (r.depth, r.tag.to_string(), r.notes))
            .collect()
    }

    /// The selected tag, for tests.
    #[cfg(test)]
    pub fn selected_tag(&self) -> Option<String> {
        let row = self.selected_row()?;
        Some(self.rows[row].tag.to_string())
    }

    /// What the panel says instead of rows, for tests.
    #[cfg(test)]
    pub fn message(&self) -> Option<&'static str> {
        self.message_text().map(|m| m.0)
    }

    /// The one-line state shown instead of rows.
    fn message_text(&self) -> Option<(&'static str, Option<SharedString>)> {
        match &self.phase {
            Phase::NoVault => Some(("No folder open", None)),
            Phase::NotIndexed(why) => Some(("", Some(why.clone()))),
            Phase::Indexing => Some(("Indexing\u{2026}", None)),
            Phase::Ready if self.rows.is_empty() && !self.filter_text.trim().is_empty() => {
                Some(("No matching tags", None))
            }
            Phase::Ready if self.rows.is_empty() => Some(("No tags", None)),
            Phase::Ready => None,
        }
    }

    fn render_head(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let action = |id: &'static str, label: &'static str, icon: Icon| {
            let size = (size::SMALL_ICON_BUTTON, size::SMALL_ICON_BUTTON);
            icon_button(id, label, icon, icon::ROW, size, theme).debug_selector(move || id.into())
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
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child("Tags"),
            )
            .child(
                action("sort-tags", "Sort", Icon::Sort)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TagsPanelEvent::Sort))),
            )
            .child(
                action("collapse-tags", "Collapse all", Icon::CollapseAll)
                    .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
            )
    }

    /// The filter field, while it shows.
    fn render_filter(&self, theme: &Theme) -> Option<Div> {
        let input = self.filter.as_ref()?;
        Some(
            div()
                .key_context(FILTER_CONTEXT)
                .debug_selector(|| "tag-filter".into())
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
                            .aria_label("Filter tags")
                            .appearance(false)
                            .px_0()
                            .py_0()
                            .text_color(theme.text.primary),
                    ),
                ),
        )
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<Div> {
        let theme = Appearance::theme(cx);
        let selected = self.selected_row();
        let end = range.end.min(self.rows.len());
        let start = range.start.min(end);
        self.rows[start..end]
            .iter()
            .zip(start..end)
            .map(|(row, ix)| {
                let key = row.key.clone();
                let chevron = div()
                    .id(("tag-chevron", ix))
                    .flex_none()
                    .size(px(icon::CHEVRON))
                    .text_color(theme.text.secondary)
                    .when(row.has_children, |slot| {
                        let chevron = Icon::ChevronRight.sized(icon::CHEVRON);
                        slot.debug_selector(move || format!("tag-chevron-{ix}"))
                            .child(if row.expanded {
                                chevron.rotate(radians(FRAC_PI_2))
                            } else {
                                chevron
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.selected = Some(key.clone());
                                this.toggle(&key, cx);
                            }))
                    });
                // `#` and the name read as one word.
                let name = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .when(row.depth == 0, |name| {
                        name.child(
                            div()
                                .flex_none()
                                .text_color(theme.text.secondary)
                                .child("#"),
                        )
                    })
                    .child(div().flex_1().min_w_0().truncate().child(row.name.clone()));
                let is_selected = selected == Some(ix);
                let notes = match row.notes {
                    1 => "1 note".to_owned(),
                    n => format!("{n} notes"),
                };
                let frame = list_row_frame(("tag-row", ix), row.depth, is_selected, theme)
                    .a11y_tree_item(
                        format!("#{}, {notes}", row.tag),
                        row.depth + 1,
                        row.has_children.then_some(row.expanded),
                        is_selected,
                    )
                    .when(is_selected, |r| r.aria_active_descendant())
                    .debug_selector(move || format!("tag-row-{ix}"))
                    .child(chevron)
                    .child(name)
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(text::UI_SMALL.size))
                            .text_color(theme.text.secondary)
                            .child(row.notes.to_string()),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.click_row(ix, window, cx)),
                    );
                div()
                    .w_full()
                    .h(px(size::LIST_ROW))
                    .px(px(size::LIST_ROW_INSET))
                    .child(frame)
            })
            .collect()
    }
}

impl Render for TagsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let body = match self.message_text() {
            Some((note, why)) => panel_message(why.unwrap_or_else(|| note.into()), theme)
                .debug_selector(|| "tags-message".into())
                .into_any_element(),
            None => div()
                .flex_1()
                .min_h_0()
                .py(px(list::PAD_Y))
                .child(
                    uniform_list(
                        "tag-tree",
                        self.rows.len(),
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            this.render_rows(range, cx)
                        }),
                    )
                    .size_full()
                    .track_scroll(&self.scroll),
                )
                .into_any_element(),
        };
        div()
            .id("tags-panel")
            .a11y_group(Role::Tree, "Tags")
            .debug_selector(|| "sidebar-panel:tags".into())
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
            .child(body)
    }
}

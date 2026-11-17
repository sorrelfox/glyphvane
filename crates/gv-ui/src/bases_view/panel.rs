// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The sidebar base (08 § Bases view): a base shown in a side panel, where
//! `this` is the file active in the main area, so a base can list, say,
//! the notes that link to whatever note is open (`file.hasLink(this.file)`).
//!
//! - A picker at the top chooses the base among the vault's `.base` files
//!   (from the content index, [`BasesPanel::set_content`]); the base's own
//!   view tabs choose the view.
//! - The window tells the panel which file is active
//!   ([`BasesPanel::set_active_path`]); the base is evaluated again for it.
//! - The panel passes on the base's [`BasesEvent`]s (open a file, follow
//!   a link) for the window to handle as it does for a base tab.
//!
//! Not designed (built plainly, flagged): the picker, its list, the
//! "for" line and the empty states.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gv_vault::EntryKind;
use gv_vault::content::ContentSnapshot;

use super::{BaseData, BasesEvent, BasesView, Colors};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::appearance::Appearance;
use crate::metrics::{radius, text};

/// The sidebar base panel.
pub struct BasesPanel {
    focus: FocusHandle,
    vault_root: Option<PathBuf>,
    /// The vault's `.base` files (vault paths), sorted.
    bases: Vec<Arc<str>>,
    /// The base shown.
    chosen: Option<Arc<str>>,
    /// The vault path of the file active in the main area: `this`.
    active: Option<Arc<str>>,
    view: Option<Entity<BasesView>>,
    content: Option<ContentSnapshot>,
    /// Files given directly (tests, embedders) instead of the index.
    data: Option<BaseData>,
    picker_open: bool,
    _events: Option<Subscription>,
}

impl std::fmt::Debug for BasesPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasesPanel")
            .field("chosen", &self.chosen)
            .field("active", &self.active)
            .field("bases", &self.bases.len())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<BasesEvent> for BasesPanel {}

impl Focusable for BasesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// The vault path of `path` below `root` (`/` separators), or `None` for
/// a file outside the vault.
fn vault_path_in(path: &Path, root: Option<&Path>) -> Option<Arc<str>> {
    let rel = path.strip_prefix(root?).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    (!parts.is_empty()).then(|| Arc::from(parts.join("/")))
}

/// A vault path's file name without `.base`.
fn base_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".base").unwrap_or(name)
}

impl BasesPanel {
    /// An empty panel for the vault at `vault_root`.
    pub fn new(vault_root: Option<PathBuf>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            focus: cx.focus_handle(),
            vault_root,
            bases: Vec::new(),
            chosen: None,
            active: None,
            view: None,
            content: None,
            data: None,
            picker_open: false,
            _events: None,
        })
    }

    /// The vault's bases the picker offers (vault paths).
    pub fn bases(&self) -> &[Arc<str>] {
        &self.bases
    }

    /// The base shown (a vault path).
    pub fn chosen(&self) -> Option<&str> {
        self.chosen.as_deref()
    }

    /// The file `this` refers to (a vault path).
    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// The base's view, once a base is chosen.
    pub fn view(&self) -> Option<&Entity<BasesView>> {
        self.view.as_ref()
    }

    /// Whether the picker's list is open.
    pub fn picker_open(&self) -> bool {
        self.picker_open
    }

    /// Sets the vault; a chosen base is opened again from it.
    pub fn set_vault_root(&mut self, root: Option<PathBuf>, cx: &mut Context<'_, Self>) {
        if root != self.vault_root {
            self.vault_root = root;
            if let Some(chosen) = self.chosen.clone() {
                self.open_view(&chosen, cx);
            }
            cx.notify();
        }
    }

    /// A new snapshot of the vault's content index: the bases offered, and
    /// the files the chosen base lists.
    pub fn set_content(&mut self, snapshot: ContentSnapshot, cx: &mut Context<'_, Self>) {
        let mut bases: Vec<Arc<str>> = snapshot
            .path_index()
            .map(|index| {
                index
                    .iter()
                    .filter(|e| e.kind == EntryKind::File && e.path.as_str().ends_with(".base"))
                    .map(|e| Arc::from(e.path.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        bases.sort_by_key(|b| b.to_lowercase());
        self.bases = bases;
        if let Some(view) = &self.view {
            let s = snapshot.clone();
            view.update(cx, |v, cx| v.set_content(s, cx));
        }
        self.content = Some(snapshot);
        cx.notify();
    }

    /// Sets the bases offered directly (tests, a window without an index).
    pub fn set_bases(&mut self, mut bases: Vec<Arc<str>>, cx: &mut Context<'_, Self>) {
        bases.sort_by_key(|b| b.to_lowercase());
        self.bases = bases;
        cx.notify();
    }

    /// Gives the files the base lists directly (tests, embedders).
    pub fn set_data(&mut self, data: BaseData, cx: &mut Context<'_, Self>) {
        if let Some(view) = &self.view {
            let d = data.clone();
            view.update(cx, |v, cx| v.set_data(d, cx));
        }
        self.data = Some(data);
    }

    /// The file active in the main area (a path on disk; `None` when no
    /// file is, or for a file outside the vault): the base's `this`.
    pub fn set_active_path(&mut self, path: Option<&Path>, cx: &mut Context<'_, Self>) {
        let active = path.and_then(|p| vault_path_in(p, self.vault_root.as_deref()));
        self.set_active(active, cx);
    }

    /// The active file as a vault path.
    pub fn set_active(&mut self, active: Option<Arc<str>>, cx: &mut Context<'_, Self>) {
        if active == self.active {
            return;
        }
        self.active = active.clone();
        if let Some(view) = &self.view {
            view.update(cx, |v, cx| v.set_this(active, cx));
        }
        cx.notify();
    }

    /// Shows the base at vault path `base`.
    pub fn choose_base(&mut self, base: Arc<str>, cx: &mut Context<'_, Self>) {
        self.picker_open = false;
        if self.chosen.as_ref() != Some(&base) || self.view.is_none() {
            self.open_view(&base, cx);
            self.chosen = Some(base);
        }
        cx.notify();
    }

    /// Shows view `index` of the chosen base.
    pub fn choose_view(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if let Some(view) = &self.view {
            view.update(cx, |v, cx| v.select_view(index, cx));
        }
    }

    /// Reads the chosen base again (its file changed on disk).
    pub fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some(chosen) = self.chosen.clone() else {
            return;
        };
        let path = self.file_path(&chosen);
        if let Some(view) = &self.view {
            view.update(cx, |v, cx| v.load(path, cx));
        }
    }

    fn file_path(&self, vault_path: &str) -> PathBuf {
        let mut p = self.vault_root.clone().unwrap_or_default();
        for part in vault_path.split('/').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }

    fn open_view(&mut self, base: &str, cx: &mut Context<'_, Self>) {
        let path = self.file_path(base);
        let view = BasesView::sidebar(path, self.vault_root.clone(), self.active.clone(), cx);
        if let Some(snapshot) = self.content.clone() {
            view.update(cx, |v, cx| v.set_content(snapshot, cx));
        } else if let Some(data) = self.data.clone() {
            view.update(cx, |v, cx| v.set_data(data, cx));
        }
        self._events = Some(cx.subscribe(&view, |_, _, e: &BasesEvent, cx| cx.emit(e.clone())));
        self.view = Some(view);
    }

    fn toggle_picker(&mut self, cx: &mut Context<'_, Self>) {
        self.picker_open = !self.picker_open;
        cx.notify();
    }

    fn render_header(&self, colors: &Colors, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let shown: SharedString = self
            .chosen
            .as_deref()
            .map_or_else(|| "Choose a base…".to_owned(), |b| base_name(b).to_owned())
            .into();
        let label = match &self.chosen {
            Some(b) => format!("Base: {}", base_name(b)),
            None => "Choose a base".to_owned(),
        };
        let open = self.picker_open;
        let for_line = match &self.active {
            Some(a) => format!("For {}", a.rsplit('/').next().unwrap_or(a)),
            None => "No active file".to_owned(),
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(2.0))
            .px(px(8.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.header)
            .child(
                div()
                    .id("bases-panel-picker")
                    .debug_selector(|| "bases-panel-picker".into())
                    .a11y_button(label)
                    .aria_expanded(open)
                    .keyboard_control(theme)
                    .h(px(24.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .rounded(px(radius::CONTROL))
                    .border_1()
                    .border_color(colors.border_strong)
                    .bg(colors.page)
                    .cursor_pointer()
                    .text_size(px(text::UI.size))
                    .text_color(if self.chosen.is_some() {
                        colors.text
                    } else {
                        colors.secondary
                    })
                    .child(div().flex_1().min_w_0().truncate().child(shown))
                    .child(
                        div()
                            .flex_none()
                            .text_color(colors.tertiary)
                            .child(crate::icons::Icon::ChevronDown.sized(12.0)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_picker(cx)))
                    .accessible(),
            )
            .child(
                div()
                    .id("bases-panel-for")
                    .debug_selector(|| "bases-panel-for".into())
                    .role(Role::Status)
                    .aria_label(for_line.clone())
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(colors.tertiary)
                    .truncate()
                    .child(for_line)
                    .accessible(),
            )
            .into_any_element()
    }

    fn render_picker(&self, colors: &Colors, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let count = self.bases.len();
        let mut list = div()
            .id("bases-panel-list")
            .debug_selector(|| "bases-panel-list".into())
            .a11y_group(Role::ListBox, "Bases")
            .flex()
            .flex_col()
            .flex_none()
            .max_h(px(240.0))
            .overflow_y_scroll()
            .py(px(4.0))
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.page)
            .text_size(px(text::UI.size));
        if self.bases.is_empty() {
            list = list.child(
                div()
                    .px(px(12.0))
                    .py(px(4.0))
                    .text_color(colors.secondary)
                    .child("This vault has no bases."),
            );
        }
        for (i, base) in self.bases.iter().enumerate() {
            let selected = self.chosen.as_ref() == Some(base);
            let hover = colors.hover;
            let chosen = base.clone();
            let folder = base
                .rsplit_once('/')
                .map(|(f, _)| f.to_owned())
                .unwrap_or_default();
            list = list.child(
                div()
                    .id(("bases-panel-option", i))
                    .debug_selector(move || format!("bases-panel-option:{i}"))
                    .a11y_option(base.to_string(), selected, i, count)
                    .keyboard_control(theme)
                    .px(px(12.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .when(selected, |d| d.bg(colors.accent_tint))
                    .child(
                        div()
                            .flex_none()
                            .text_color(colors.text)
                            .child(base_name(base).to_owned()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(text::UI_SMALL.size))
                            .text_color(colors.tertiary)
                            .child(folder),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_base(chosen.clone(), cx);
                    }))
                    .accessible(),
            );
        }
        list.accessible().into_any_element()
    }
}

fn note(text: &'static str, colors: &Colors) -> AnyElement {
    div()
        .debug_selector(|| "bases-panel-empty".into())
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p(px(16.0))
        .text_color(colors.secondary)
        .child(text)
        .into_any_element()
}

impl Render for BasesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let colors = Colors::of(Appearance::theme(cx));
        let mut root = div()
            .id("bases-panel")
            .a11y_group(Role::Group, "Base")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(colors.page)
            .font_family(Appearance::interface_font(cx))
            .text_size(px(text::UI.size))
            .text_color(colors.text)
            .child(self.render_header(&colors, cx));
        if self.picker_open {
            root = root.child(self.render_picker(&colors, cx));
        }
        root = match &self.view {
            Some(view) => root.child(div().flex_1().min_h_0().child(view.clone())),
            None if self.bases.is_empty() && self.content.is_some() => {
                root.child(note("This vault has no bases.", &colors))
            }
            None => root.child(note("Choose a base to show it here.", &colors)),
        };
        root.accessible()
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Recent panel (FR-VAULT-14; plan § Panels): recent folders, then
//! recent files, most recent first, from the session (`session.json`).
//! *Open recent…* (`file.open_recent`) and View ▸ Recent show it.
//!
//! Not designed: the rows (folders and files as in the Files panel, the
//! parent folder after the name in `text.secondary`), the *Clear* button in
//! the head, and the empty state. Clicking a row or Enter opens it: a
//! folder as the window's vault, a file as a tab. The panel shares the
//! file tree's keys (arrows, Enter).
//!
//! Nothing here touches the file system: an entry that is gone shows until
//! it fails to open.

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Div, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::controls::icon_button;
use super::files_panel::{
    KEY_CONTEXT, OpenSelected, SelectNext, SelectPrevious, list_row_frame, message,
};
use super::session::{self, SessionStore};
use crate::a11y::{A11yExt as _, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::spacing::{list, panel_head};
use crate::metrics::{icon, size, text};
use crate::theme::Theme;

/// One entry of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecentItem {
    Folder(PathBuf),
    File(PathBuf),
}

/// What the panel asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecentPanelEvent {
    Open(RecentItem),
}

/// The recent folders, then the recent files.
pub(crate) fn recent_items(cx: &App) -> Vec<RecentItem> {
    session::recent_folders(cx)
        .into_iter()
        .map(RecentItem::Folder)
        .chain(session::recent_files(cx).into_iter().map(RecentItem::File))
        .collect()
}

/// The Recent panel of a window's sidebar.
pub(crate) struct RecentPanel {
    focus: FocusHandle,
    selected: Option<usize>,
    _session: gpui::Subscription,
}

impl EventEmitter<RecentPanelEvent> for RecentPanel {}

impl Focusable for RecentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl RecentPanel {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            selected: None,
            // Every change to the recent lists goes through the session's
            // global.
            _session: cx.observe_global::<SessionStore>(|_, cx| cx.notify()),
        }
    }

    /// The selected row, kept inside the list.
    fn selection(&self, count: usize) -> Option<usize> {
        self.selected.filter(|&s| s < count)
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<'_, Self>) {
        let count = recent_items(cx).len();
        if count == 0 {
            return;
        }
        self.selected = Some(
            self.selection(count)
                .map_or(count - 1, |s| s.saturating_sub(1)),
        );
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<'_, Self>) {
        let count = recent_items(cx).len();
        if count == 0 {
            return;
        }
        self.selected = Some(self.selection(count).map_or(0, |s| (s + 1).min(count - 1)));
        cx.notify();
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<'_, Self>) {
        let items = recent_items(cx);
        if let Some(item) = self.selection(items.len()).and_then(|s| items.get(s)) {
            cx.emit(RecentPanelEvent::Open(item.clone()));
        }
    }

    fn click_row(&mut self, row: usize, cx: &mut Context<'_, Self>) {
        let items = recent_items(cx);
        if let Some(item) = items.get(row) {
            self.selected = Some(row);
            cx.emit(RecentPanelEvent::Open(item.clone()));
            cx.notify();
        }
    }

    fn render_head(&self, theme: &Theme, empty: bool, cx: &mut Context<'_, Self>) -> Div {
        let clear = icon_button(
            "recent-clear",
            "Clear recent",
            Icon::Dismiss,
            icon::ROW,
            (size::SMALL_ICON_BUTTON, size::SMALL_ICON_BUTTON),
            theme,
        )
        .debug_selector(|| "recent-clear".into())
        .when(!empty, |button| {
            button.on_click(cx.listener(|this, _, _, cx| {
                this.selected = None;
                session::clear_recent(cx);
            }))
        });
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
                    .child("Recent"),
            )
            .when(!empty, |head| head.child(clear))
    }

    fn render_row(
        &self,
        ix: usize,
        item: &RecentItem,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let (row_icon, path) = match item {
            RecentItem::Folder(path) => (Icon::Folder, path),
            RecentItem::File(path) => (Icon::Note, path),
        };
        let name: SharedString = path
            .file_name()
            .map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
            .into();
        let parent: SharedString = path
            .parent()
            .map(crate::files::notice::shown_folder)
            .unwrap_or_default()
            .into();
        let frame = list_row_frame(("recent-row", ix), 0, selected, theme)
            .role(Role::ListItem)
            .aria_label(format!("{name}, {parent}"))
            .aria_selected(selected)
            .when(selected, |row| row.aria_active_descendant())
            .debug_selector(move || format!("recent-row-{ix}"))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(row_icon.sized(icon::ROW)),
            )
            .child(div().flex_none().max_w_full().truncate().child(name))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text.secondary)
                    .child(parent),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.click_row(ix, cx)));
        div()
            .w_full()
            .h(px(size::LIST_ROW))
            .flex_none()
            .px(px(size::LIST_ROW_INSET))
            .child(frame)
    }
}

impl Render for RecentPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let items = recent_items(cx);
        let selected = self.selection(items.len());
        let head = self.render_head(theme, items.is_empty(), cx);
        let rows: Vec<Div> = items
            .iter()
            .enumerate()
            .map(|(ix, item)| self.render_row(ix, item, selected == Some(ix), theme, cx))
            .collect();
        div()
            .id("recent-panel")
            .a11y_group(Role::List, "Recent")
            .debug_selector(|| "sidebar-panel:recent".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::open_selected))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(head)
            .map(|panel| {
                if rows.is_empty() {
                    panel.child(message("No recent files", theme))
                } else {
                    panel.child(
                        div()
                            .id("recent-rows")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .py(px(list::PAD_Y))
                            .flex()
                            .flex_col()
                            .children(rows),
                    )
                }
            })
    }
}

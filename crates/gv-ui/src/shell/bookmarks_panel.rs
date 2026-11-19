// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Bookmarks panel (plan 10 § Panels, Should; the switcher's fourth
//! button, View ▸ Bookmarks): Glyphvane's own bookmarks of the vault,
//! then Obsidian's (`.obsidian/bookmarks.json`, read only) under a
//! "From Obsidian" label when both have some. Groups show as rows with
//! their items indented below them.
//!
//! Not designed: everything. Rows follow the Recent panel: an icon per
//! kind (note, heading, block, folder, search, web page; graph views and
//! unknown kinds are listed in `text.secondary` and do not open), the
//! label (the bookmark's title, else the note's name with its heading or
//! block, the folder, the query or the URL). A note or folder that is not
//! in the vault any more shows "missing". Click or Enter opens; the panel
//! shares the file tree's keys. The lists are read off the UI thread when
//! the panel is made, when the vault changes and after every write, and
//! again when Obsidian's file changes (its time and size are checked every
//! [`WATCH_EVERY`]: the vault watcher leaves `.obsidian` out).

use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use gv_vault::VaultPath;

use super::bookmarks::{self, Bookmark, BookmarksRevision, Kind};
use super::files_panel::{
    KEY_CONTEXT, OpenSelected, SelectNext, SelectPrevious, list_row_frame, message,
};
use crate::a11y::{A11yExt as _, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::spacing::{list, panel_head};
use crate::metrics::{icon, size, text};
use crate::theme::Theme;

/// What a bookmark opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BookmarkTarget {
    /// A note or other file, at a heading or block (`#…`, `#^…`).
    File {
        path: PathBuf,
        subpath: Option<String>,
    },
    Folder(VaultPath),
    Search(String),
    Url(String),
}

/// What the panel asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BookmarksEvent {
    Open(BookmarkTarget),
}

/// One row of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub depth: usize,
    pub label: SharedString,
    pub icon: RowIcon,
    /// `None`: a group, a label, or a kind that does not open.
    pub target: Option<BookmarkTarget>,
    /// A note or folder that is not in the vault.
    pub missing: bool,
}

/// A row's icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowIcon {
    Note,
    Heading,
    Block,
    Folder,
    Group,
    Search,
    Url,
    Other,
    /// The "From Obsidian" label: no icon.
    Label,
}

impl RowIcon {
    fn icon(self) -> Option<Icon> {
        Some(match self {
            Self::Note => Icon::Note,
            Self::Heading => Icon::SwitcherHeading,
            Self::Block => Icon::SwitcherBlock,
            Self::Folder | Self::Group => Icon::Folder,
            Self::Search => Icon::Search,
            Self::Url => Icon::WebPage,
            Self::Other => Icon::OtherFile,
            Self::Label => return None,
        })
    }
}

/// The lists as read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum State {
    /// No vault.
    #[default]
    NoVault,
    Loading,
    Ready(Vec<Row>),
    Failed(String),
}

/// What the panel shows bookmarks of.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Scope {
    root: Option<PathBuf>,
    own: Option<PathBuf>,
}

/// How often Obsidian's `bookmarks.json` is checked for changes.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// The time and size of the file at `path`, `None` when it is not there.
fn stamp(path: &Path) -> Option<(Option<std::time::SystemTime>, u64)> {
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.modified().ok(), m.len()))
}

/// The Bookmarks panel of a window's sidebar.
pub(crate) struct BookmarksPanel {
    focus: FocusHandle,
    scope: Scope,
    /// The active tab's note, vault-relative.
    active: Option<String>,
    state: State,
    selected: Option<usize>,
    revision: u64,
    _load: Option<Task<()>>,
    /// Checks Obsidian's file for changes ([`WATCH_EVERY`]).
    _watch: Option<Task<()>>,
    _revision: gpui::Subscription,
}

impl EventEmitter<BookmarksEvent> for BookmarksPanel {}

impl Focusable for BookmarksPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BookmarksPanel {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        let revision = cx.default_global::<BookmarksRevision>().0;
        Self {
            focus: cx.focus_handle(),
            scope: Scope::default(),
            active: None,
            state: State::NoVault,
            selected: None,
            revision,
            _load: None,
            _watch: None,
            _revision: cx.observe_global::<BookmarksRevision>(|this, cx| {
                let revision = cx.global::<BookmarksRevision>().0;
                if revision != this.revision {
                    this.revision = revision;
                    this.reload(cx);
                }
            }),
        }
    }

    /// Shows the bookmarks of the vault at `root` (`own`: its own list's
    /// file); `active` is the active tab's note.
    pub fn set_scope(
        &mut self,
        root: Option<PathBuf>,
        own: Option<PathBuf>,
        active: Option<String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.active = active;
        let scope = Scope { root, own };
        if scope != self.scope || self.state == State::NoVault {
            self.scope = scope;
            self.reload(cx);
            self.watch_obsidian(cx);
        }
        cx.notify();
    }

    /// Reads the lists again whenever Obsidian's file of the vault shown
    /// changes (written by Obsidian, a sync client or by hand).
    fn watch_obsidian(&mut self, cx: &mut Context<'_, Self>) {
        let Some(root) = self.scope.root.clone() else {
            self._watch = None;
            return;
        };
        let file = bookmarks::obsidian_file(&root);
        self._watch = Some(cx.spawn(async move |this, cx| {
            let first = file.clone();
            let mut seen = cx.background_spawn(async move { stamp(&first) }).await;
            loop {
                cx.background_executor().timer(WATCH_EVERY).await;
                let next = file.clone();
                let now = cx.background_spawn(async move { stamp(&next) }).await;
                let changed = now != seen;
                seen = now;
                let alive = this
                    .update(cx, |this, cx| {
                        if changed {
                            this.reload(cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        }));
    }

    /// Reads the lists again, off the UI thread.
    pub fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some(root) = self.scope.root.clone() else {
            self.state = State::NoVault;
            self._load = None;
            cx.notify();
            return;
        };
        if !matches!(self.state, State::Ready(_)) {
            self.state = State::Loading;
        }
        let own = self.scope.own.clone();
        let task = cx.background_spawn(async move { load(&root, own.as_deref()) });
        self._load = Some(cx.spawn(async move |this, cx| {
            let state = task.await;
            let _ = this.update(cx, |this, cx| {
                this.state = state;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The rows (tests).
    pub fn rows(&self) -> &[Row] {
        match &self.state {
            State::Ready(rows) => rows,
            _ => &[],
        }
    }

    /// The selected row, kept inside the list.
    fn selection(&self) -> Option<usize> {
        self.selected.filter(|&s| s < self.rows().len())
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<'_, Self>) {
        let count = self.rows().len();
        if count == 0 {
            return;
        }
        self.selected = Some(self.selection().map_or(count - 1, |s| s.saturating_sub(1)));
        cx.notify();
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<'_, Self>) {
        let count = self.rows().len();
        if count == 0 {
            return;
        }
        self.selected = Some(self.selection().map_or(0, |s| (s + 1).min(count - 1)));
        cx.notify();
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(row) = self.selection() {
            self.open_row(row, cx);
        }
    }

    /// Opens row `row` (a click, Enter).
    pub fn open_row(&mut self, row: usize, cx: &mut Context<'_, Self>) {
        let Some(target) = self.rows().get(row).and_then(|r| r.target.clone()) else {
            return;
        };
        self.selected = Some(row);
        cx.emit(BookmarksEvent::Open(target));
        cx.notify();
    }

    fn render_head() -> Div {
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
                    .child("Bookmarks"),
            )
    }

    fn render_row(
        &self,
        ix: usize,
        row: &Row,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let active = matches!(
            (&row.target, &self.active, &self.scope.root),
            (Some(BookmarkTarget::File { path, subpath: None }), Some(active), Some(root))
                if path.strip_prefix(root).is_ok_and(|rel| {
                    VaultPath::from_relative_fs_path(rel).is_ok_and(|v| v.as_str() == active)
                })
        );
        let quiet = row.target.is_none() || row.missing;
        let frame = list_row_frame(("bookmark-row", ix), row.depth, selected, theme)
            .a11y_tree_item(row.label.clone(), row.depth + 1, None, selected)
            .when(row.missing, |r| r.aria_description("Missing"))
            .when(selected, |r| r.aria_active_descendant())
            .debug_selector(move || format!("bookmark-row-{ix}"))
            .when_some(row.icon.icon(), |frame, row_icon| {
                frame.child(
                    div()
                        .flex_none()
                        .text_color(theme.text.secondary)
                        .child(row_icon.sized(icon::ROW)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(quiet, |label| label.text_color(theme.text.secondary))
                    .when(row.icon == RowIcon::Label, |label| {
                        label
                            .text_size(px(text::UI_SECTION.size))
                            .font_weight(FontWeight(text::weight::STRONG))
                    })
                    .when(active && !selected, |label| {
                        label.font_weight(FontWeight(text::weight::ACTIVE))
                    })
                    .child(row.label.clone()),
            )
            .when(row.missing, |frame| {
                frame.child(
                    div()
                        .flex_none()
                        .text_color(theme.text.secondary)
                        .child("missing"),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| this.open_row(ix, cx)));
        div()
            .w_full()
            .h(px(size::LIST_ROW))
            .flex_none()
            .px(px(size::LIST_ROW_INSET))
            .child(frame)
    }
}

impl Render for BookmarksPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let head = Self::render_head();
        let selected = self.selection();
        let rows: Vec<Div> = self
            .rows()
            .iter()
            .enumerate()
            .map(|(ix, row)| self.render_row(ix, row, selected == Some(ix), theme, cx))
            .collect();
        let note: Option<SharedString> = match &self.state {
            State::NoVault => Some("Open a folder to keep bookmarks".into()),
            State::Loading => Some("Loading…".into()),
            State::Failed(why) => Some(format!("Bookmarks could not be read: {why}").into()),
            State::Ready(rows) if rows.is_empty() => Some("No bookmarks".into()),
            State::Ready(_) => None,
        };
        div()
            .id("bookmarks-panel")
            .a11y_group(Role::Tree, "Bookmarks")
            .debug_selector(|| "sidebar-panel:bookmarks".into())
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
            .map(|panel| match note {
                Some(note) => panel.child(message(note, theme)),
                None => panel.child(
                    div()
                        .id("bookmark-rows")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .py(px(list::PAD_Y))
                        .flex()
                        .flex_col()
                        .children(rows),
                ),
            })
    }
}

/// Reads both lists of the vault at `root` into rows.
fn load(root: &Path, own: Option<&Path>) -> State {
    let own = match own.map(bookmarks::read_file) {
        Some(Ok(items)) => items.unwrap_or_default(),
        Some(Err(why)) => return State::Failed(why),
        None => Vec::new(),
    };
    // Obsidian's list is someone else's file: a broken one is left out.
    let obsidian = match bookmarks::read_file(&bookmarks::obsidian_file(root)) {
        Ok(items) => items.unwrap_or_default(),
        Err(why) => {
            tracing::warn!("bookmarks: Obsidian's list cannot be read: {why}");
            Vec::new()
        }
    };
    State::Ready(rows(root, &own, &obsidian))
}

/// The rows of both lists.
pub(crate) fn rows(root: &Path, own: &[Bookmark], obsidian: &[Bookmark]) -> Vec<Row> {
    let mut out = Vec::new();
    push_rows(root, own, 0, &mut out);
    if !own.is_empty() && !obsidian.is_empty() {
        out.push(Row {
            depth: 0,
            label: "From Obsidian".into(),
            icon: RowIcon::Label,
            target: None,
            missing: false,
        });
    }
    push_rows(root, obsidian, 0, &mut out);
    out
}

fn push_rows(root: &Path, items: &[Bookmark], depth: usize, out: &mut Vec<Row>) {
    for item in items {
        let fs_path = |p: &str| {
            VaultPath::new(p)
                .ok()
                .map(|v| (v.to_fs_path(root), v))
                .filter(|_| !p.is_empty())
        };
        let (icon, target, missing) = match &item.kind {
            Kind::File => {
                let subpath = item.subpath.clone().filter(|s| !s.is_empty());
                let icon = match subpath.as_deref() {
                    Some(s) if s.starts_with("#^") => RowIcon::Block,
                    Some(_) => RowIcon::Heading,
                    None => RowIcon::Note,
                };
                match item.path.as_deref().and_then(fs_path) {
                    Some((path, _)) => {
                        let missing = !path.is_file();
                        (icon, Some(BookmarkTarget::File { path, subpath }), missing)
                    }
                    None => (icon, None, true),
                }
            }
            Kind::Folder => match item.path.as_deref().and_then(fs_path) {
                Some((path, vault_path)) => (
                    RowIcon::Folder,
                    Some(BookmarkTarget::Folder(vault_path)),
                    !path.is_dir(),
                ),
                None => (RowIcon::Folder, None, true),
            },
            Kind::Search => (
                RowIcon::Search,
                item.query.clone().map(BookmarkTarget::Search),
                false,
            ),
            Kind::Url => (
                RowIcon::Url,
                item.url
                    .clone()
                    .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                    .map(BookmarkTarget::Url),
                false,
            ),
            Kind::Group => (RowIcon::Group, None, false),
            Kind::Graph | Kind::Other(_) => (RowIcon::Other, None, false),
        };
        out.push(Row {
            depth,
            label: item.label().into(),
            icon,
            target: target.filter(|_| !missing),
            missing,
        });
        push_rows(root, &item.items, depth + 1, out);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The main window: title row, side panels, document area and status bar.

mod controls;
mod document;
mod files_panel;
#[cfg(test)]
mod files_panel_tests;
mod panels;
mod status_bar;
#[cfg(test)]
mod tests;
mod title_row;
mod tree;
mod vault;

#[cfg(test)]
pub(crate) use vault::VaultStatus;

pub(crate) use files_panel::key_bindings as file_tree_key_bindings;

use std::fmt;
use std::path::{Path, PathBuf};

use gpui::component::window_corner_radii;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Result, SharedString,
    Styled as _, Subscription, WeakEntity, Window, div, px,
};

use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::metrics::text;
use crate::reader::{Disposition, LinkRequest, Navigation, Reader, ReaderEvent, ScrollTarget};
use crate::{ToggleEditMode, TogglePanel, ToggleSidebar};
use files_panel::{FilesPanel, FilesPanelEvent};
use vault::WindowVault;

/// Set to `1` to open main windows with the sidebar shown, for screenshots
/// and manual checks. Windows opened for a file otherwise start with both
/// side panels hidden.
pub const DEBUG_SHOW_SIDEBAR_ENV: &str = "GLYPHVANE_DEBUG_SHOW_SIDEBAR";

/// What the document area shows. Each tab has its own mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Preview,
    Edit,
    Split,
}

/// A place in a document: a 1-based line and, optionally, a 1-based column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPosition {
    pub line: u32,
    pub column: Option<u32>,
}

/// A file to open, and where to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTarget {
    pub path: PathBuf,
    pub position: Option<TextPosition>,
}

impl OpenTarget {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            position: None,
        }
    }

    pub fn at(mut self, position: TextPosition) -> Self {
        self.position = Some(position);
        self
    }
}

/// Most entries a tab's back stack keeps.
const HISTORY_LIMIT: usize = 100;

/// A place a tab was at before it followed a link.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub path: PathBuf,
    /// Where the Reading view was scrolled.
    pub scroll: Option<ScrollTarget>,
}

/// One open document in a window.
#[derive(Debug, Clone)]
pub struct Tab {
    label: FileLabel,
    /// `None` for the empty tab of a window opened without a file.
    path: Option<PathBuf>,
    /// Where the Reading view was asked to scroll, from `PATH:LINE[:COL]`.
    position: Option<TextPosition>,
    mode: ViewMode,
    /// The Reading view, made when the tab is first shown.
    reader: Option<Entity<Reader>>,
    /// Where following links came from, most recent last (no interface
    /// yet; see [`Workspace::go_back`]).
    back: Vec<HistoryEntry>,
}

impl Tab {
    fn new(target: Option<OpenTarget>) -> Self {
        let (path, position) = match target {
            Some(target) => (Some(target.path), target.position),
            None => (None, None),
        };
        Self {
            label: FileLabel::new(path.as_deref()),
            path,
            position,
            mode: ViewMode::default(),
            reader: None,
            back: Vec::new(),
        }
    }

    pub fn reader(&self) -> Option<&Entity<Reader>> {
        self.reader.as_ref()
    }

    pub fn history(&self) -> &[HistoryEntry] {
        &self.back
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn position(&self) -> Option<TextPosition> {
        self.position
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }
}

/// How the opened file is named in the tab, the breadcrumb and the window
/// title. The file itself is not read yet.
#[derive(Debug, Clone)]
pub struct FileLabel {
    name: SharedString,
    folder: Option<SharedString>,
}

impl FileLabel {
    pub fn new(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self {
                name: "Untitled".into(),
                folder: None,
            };
        };
        let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
        let name = file_name(path).unwrap_or_else(|| path.display().to_string());
        let folder = std::path::absolute(path)
            .ok()
            .and_then(|abs| abs.parent().and_then(file_name));
        Self {
            name: name.into(),
            folder: folder.map(Into::into),
        }
    }

    pub fn window_title(&self) -> String {
        format!("{} — Glyphvane", self.name)
    }
}

/// A main window's content.
pub struct Workspace {
    focus: FocusHandle,
    chrome: WindowChrome,
    /// Never empty.
    tabs: Vec<Tab>,
    active: usize,
    /// Side panels start hidden: a window opened for a file shows only the
    /// document until the user calls a panel.
    sidebar_open: bool,
    panel_open: bool,
    /// The window's vault, created when something first needs it: the
    /// sidebar being shown. Until then nothing is indexed.
    vault: Option<Entity<WindowVault>>,
    /// The Files panel, created with the sidebar.
    files: Option<Entity<FilesPanel>>,
    _files_events: Option<Subscription>,
    _vault_changed: Option<Subscription>,
    _reader_events: Vec<Subscription>,
    /// The pointer went down on a drag region of a client-decorated window
    /// and has not moved yet.
    drag_pending: bool,
    _appearance: Subscription,
    _activation: Subscription,
}

impl fmt::Debug for Workspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workspace")
            .field("chrome", &self.chrome)
            .field("tabs", &self.tabs)
            .field("active", &self.active)
            .field("sidebar_open", &self.sidebar_open)
            .field("panel_open", &self.panel_open)
            .field("vault", &self.vault)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// A workspace with one tab: `target`, or an empty tab.
    pub fn new(
        target: Option<OpenTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self::with_chrome(target, WindowChrome::current(), window, cx);
        if std::env::var_os(DEBUG_SHOW_SIDEBAR_ENV).is_some_and(|v| v == "1") {
            this.set_sidebar_open(true, window, cx);
        }
        this
    }

    /// A workspace drawn with another platform's chrome. Only the window's
    /// own platform behaves natively; the others are for layout tests.
    pub fn with_chrome(
        target: Option<OpenTarget>,
        chrome: WindowChrome,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Appearance::set_os_appearance(cx, window.appearance());
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Appearance::set_os_appearance(cx, window.appearance());
        });
        let this = WorkspaceWindow {
            window: window.window_handle(),
            workspace: cx.weak_entity(),
        };
        RecentWindows::touch(this.clone(), cx);
        let activation = cx.observe_window_activation(window, move |_, window, cx| {
            if window.is_window_active() {
                RecentWindows::touch(this.clone(), cx);
            }
        });
        let mut this = Self {
            focus,
            chrome,
            tabs: vec![Tab::new(target)],
            active: 0,
            sidebar_open: false,
            panel_open: false,
            vault: None,
            files: None,
            _files_events: None,
            _vault_changed: None,
            _reader_events: Vec::new(),
            drag_pending: false,
            _appearance: appearance,
            _activation: activation,
        };
        // The document takes the keyboard from the start (copy, scrolling).
        if let Some(reader) = this.ensure_reader(0, window, cx) {
            reader.read(cx).focus_handle(cx).focus(window, cx);
        }
        this
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// Opens `target` as a new tab after the active one and activates it.
    /// The window's panels stay as they are. A window holding only its empty
    /// tab gets the file in that tab instead.
    pub fn open_tab(
        &mut self,
        target: OpenTarget,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.insert_tab(target, None, true, window, cx);
    }

    /// Adds a tab for `target` after the active one, scrolled to `scroll`
    /// once loaded; makes it active unless it opens in the background.
    fn insert_tab(
        &mut self,
        target: OpenTarget,
        scroll: Option<ScrollTarget>,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let focused = self.reader_focused(window, cx);
        let tab = Tab::new(Some(target));
        let index = if self.tabs.len() == 1 && self.tabs[0].path.is_none() {
            self.tabs[0] = tab;
            self.active = 0;
            0
        } else {
            self.tabs.insert(self.active + 1, tab);
            self.active + 1
        };
        if let Some(scroll) = scroll {
            let reader = self.make_reader(index, Some(scroll), window, cx);
            self.tabs[index].reader = reader;
        }
        if activate || index == self.active {
            self.active = index;
            self.tabs_changed(window, cx);
            self.focus_reader_if(focused, window, cx);
        } else {
            cx.notify();
        }
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        if index < self.tabs.len() && index != self.active {
            let focused = self.reader_focused(window, cx);
            self.active = index;
            self.tabs_changed(window, cx);
            self.focus_reader_if(focused, window, cx);
        }
    }

    /// Whether the active tab's Reading view has the keyboard.
    fn reader_focused(&self, window: &Window, cx: &App) -> bool {
        self.active_tab()
            .reader
            .as_ref()
            .is_some_and(|r| r.read(cx).focus_handle(cx).contains_focused(window, cx))
    }

    fn focus_reader_if(&self, focused: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        if focused {
            self.focus_document(window, cx);
        }
    }

    /// The Reading view on screen: the active tab's, in Preview mode.
    fn shown_reader(&self) -> Option<&Entity<Reader>> {
        match self.mode() {
            ViewMode::Preview => self.active_tab().reader.as_ref(),
            ViewMode::Edit | ViewMode::Split => None,
        }
    }

    /// Gives the keyboard to the document on screen, or to the window when
    /// it shows none.
    fn focus_document(&self, window: &mut Window, cx: &mut App) {
        match self.shown_reader() {
            Some(reader) => reader.read(cx).focus_handle(cx).focus(window, cx),
            None => self.focus.focus(window, cx),
        }
    }

    /// Keeps the keyboard on something that is drawn: a Reading view that
    /// leaves the screen (a mode switch) hands it to the window, and the
    /// window hands it to the Reading view that comes back.
    fn settle_focus(&self, window: &mut Window, cx: &mut App) {
        let shown = self.shown_reader().map(Entity::entity_id);
        let hidden_focused = self.tabs.iter().filter_map(|t| t.reader.as_ref()).any(|r| {
            Some(r.entity_id()) != shown && r.read(cx).focus_handle(cx).contains_focused(window, cx)
        });
        if hidden_focused || (shown.is_some() && self.focus.is_focused(window)) {
            self.focus_document(window, cx);
        }
    }

    fn tabs_changed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.ensure_reader(self.active, window, cx);
        window.set_window_title(&self.active_tab().label.window_title());
        if let Some(files) = self.files.clone() {
            self.ensure_vault(cx);
            let active = self.active_tab().path.clone();
            files.update(cx, |files, cx| files.set_active(active, cx));
        }
        cx.notify();
    }

    /// Opens a file from a panel: activates its tab, or opens a new one.
    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<'_, Self>) {
        let open = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(&path));
        match open {
            Some(index) => self.activate_tab(index, window, cx),
            None => self.open_tab(OpenTarget::new(path), window, cx),
        }
    }

    /// Creates the window's vault from the active tab's file, once.
    fn ensure_vault(&mut self, cx: &mut Context<'_, Self>) {
        if self.vault.is_some() {
            return;
        }
        let Some(path) = self.active_tab().path.clone() else {
            return;
        };
        let vault = cx.new(|cx| WindowVault::for_file(&path, cx));
        self.vault = Some(vault.clone());
        self._vault_changed = Some(cx.observe(&vault, |this, _, cx| this.vault_changed(cx)));
        self.vault_changed(cx);
        if let Some(files) = &self.files {
            files.update(cx, |files, cx| files.set_vault(vault, cx));
        }
    }

    /// The vault published a new index snapshot: link statuses follow it.
    fn vault_changed(&mut self, cx: &mut Context<'_, Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let index = vault.read(cx).snapshot();
        for reader in self.tabs.iter().filter_map(|t| t.reader.clone()) {
            let index = index.clone();
            reader.update(cx, |r, cx| r.set_index(Some(index), cx));
        }
    }

    /// The Reading view of tab `index`, made and loading on first use.
    fn ensure_reader(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<Reader>> {
        let tab = self.tabs.get(index)?;
        if let Some(reader) = &tab.reader {
            return Some(reader.clone());
        }
        let scroll = tab.position.map(|p| ScrollTarget::Line(p.line));
        let reader = self.make_reader(index, scroll, window, cx)?;
        self.tabs[index].reader = Some(reader.clone());
        Some(reader)
    }

    fn make_reader(
        &mut self,
        index: usize,
        scroll: Option<ScrollTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<Reader>> {
        let path = self.tabs.get(index)?.path.clone()?;
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let reader = cx.new(|cx| {
            let mut reader = Reader::new(cx);
            reader.set_index(snapshot, cx);
            reader.open(path, scroll, cx);
            reader
        });
        self._reader_events.push(cx.subscribe_in(
            &reader,
            window,
            |this, reader, event: &ReaderEvent, window, cx| match event {
                ReaderEvent::FollowLink(request) => {
                    this.follow_link(reader.clone(), request.clone(), window, cx);
                }
            },
        ));
        Some(reader)
    }

    /// Follows a link clicked in a tab's Reading view.
    fn follow_link(
        &mut self,
        reader: Entity<Reader>,
        request: LinkRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.reader.as_ref() == Some(&reader))
        else {
            return;
        };
        let Some(file) = self.tabs[index].path.clone() else {
            return;
        };
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let target =
            crate::reader::navigation(&file, snapshot.as_ref(), request.kind, &request.destination);
        tracing::debug!(?target, "follow link");
        let (path, fragment) = match target {
            Navigation::External(url) => {
                cx.open_url(&url);
                return;
            }
            Navigation::Refused(why) => {
                tracing::info!("link not followed: {why}");
                return;
            }
            Navigation::Here { fragment } => (file.clone(), fragment),
            Navigation::Open { path, fragment } => (path, fragment),
        };
        let scroll = fragment.map(ScrollTarget::Fragment);
        match request.disposition {
            Disposition::InPlace => self.navigate(index, path, scroll, window, cx),
            Disposition::NewTab => {
                let scroll = scroll.or(Some(ScrollTarget::Offset(0)));
                self.insert_tab(OpenTarget::new(path), scroll, true, window, cx);
            }
            Disposition::Background => {
                let scroll = scroll.or(Some(ScrollTarget::Offset(0)));
                self.insert_tab(OpenTarget::new(path), scroll, false, window, cx);
            }
        }
    }

    /// Shows `path` in tab `index` (the same tab), remembering where the tab
    /// was. A jump inside the same document is remembered too.
    fn navigate(
        &mut self,
        index: usize,
        path: PathBuf,
        scroll: Option<ScrollTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if let Some(previous) = tab.path.clone() {
            let position = tab.reader.as_ref().and_then(|r| r.read(cx).position());
            if tab.back.len() >= HISTORY_LIMIT {
                tab.back.remove(0);
            }
            tab.back.push(HistoryEntry {
                path: previous,
                scroll: position,
            });
        }
        self.show_in_tab(index, path, scroll, window, cx);
    }

    fn show_in_tab(
        &mut self,
        index: usize,
        path: PathBuf,
        scroll: Option<ScrollTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let same = tab.path.as_deref() == Some(path.as_path());
        tab.label = FileLabel::new(Some(&path));
        tab.path = Some(path.clone());
        tab.position = None;
        match tab.reader.clone() {
            Some(reader) if same => reader.update(cx, |r, cx| {
                r.scroll_to(scroll.unwrap_or(ScrollTarget::Offset(0)), cx);
            }),
            Some(reader) => reader.update(cx, |r, cx| r.open(path, scroll, cx)),
            None => {
                let reader = self.make_reader(index, scroll, window, cx);
                self.tabs[index].reader = reader;
            }
        }
        if index == self.active {
            self.tabs_changed(window, cx);
        } else {
            cx.notify();
        }
    }

    /// The state of the window's vault, once it exists.
    #[cfg(test)]
    pub(crate) fn vault_status(&self, cx: &App) -> Option<VaultStatus> {
        self.vault.as_ref().map(|v| v.read(cx).status().clone())
    }

    /// Goes back to where the active tab was before it last followed a
    /// link. Returns false when there is nothing to go back to.
    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        let index = self.active;
        let Some(entry) = self.tabs[index].back.pop() else {
            return false;
        };
        self.show_in_tab(index, entry.path, entry.scroll, window, cx);
        true
    }

    /// Creates the Files panel, and with it the vault, the first time the
    /// sidebar is shown.
    fn ensure_files_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.files.is_some() {
            return;
        }
        self.ensure_vault(cx);
        let (vault, active) = (self.vault.clone(), self.active_tab().path.clone());
        let files = cx.new(|cx| FilesPanel::new(vault, active, cx));
        self._files_events = Some(cx.subscribe_in(
            &files,
            window,
            |this, _, event: &FilesPanelEvent, window, cx| match event {
                FilesPanelEvent::Open(path) => this.open_path(path.clone(), window, cx),
            },
        ));
        self.files = Some(files);
    }

    fn mode(&self) -> ViewMode {
        self.active_tab().mode
    }

    fn set_mode(&mut self, mode: ViewMode, cx: &mut Context<'_, Self>) {
        self.tabs[self.active].mode = mode;
        cx.notify();
    }

    fn toggle_edit_mode(&mut self, cx: &mut Context<'_, Self>) {
        let mode = match self.mode() {
            ViewMode::Edit => ViewMode::Preview,
            ViewMode::Preview | ViewMode::Split => ViewMode::Edit,
        };
        self.set_mode(mode, cx);
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.set_sidebar_open(!self.sidebar_open, window, cx);
    }

    fn set_sidebar_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.sidebar_open = open;
        if open {
            self.ensure_files_panel(window, cx);
        }
        cx.notify();
    }

    fn toggle_panel(&mut self, cx: &mut Context<'_, Self>) {
        self.panel_open = !self.panel_open;
        cx.notify();
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.settle_focus(window, cx);
        let theme = Appearance::theme(cx);
        let radii = window_corner_radii(window, cx);

        // The window root (GPUI Component's `Root`) draws the frame of a
        // client-decorated window: shadow, border, rounded corners and resize
        // edges. Surfaces that touch a corner round it themselves.
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleEditMode, _, cx| this.toggle_edit_mode(cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePanel, _, cx| this.toggle_panel(cx)))
            .size_full()
            .flex()
            .flex_col()
            .rounded_tl(radii.top_left)
            .rounded_tr(radii.top_right)
            .rounded_bl(radii.bottom_left)
            .rounded_br(radii.bottom_right)
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.sidebar_open, |row| {
                        row.child(self.render_sidebar(radii, window, cx))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(self.render_title_row(radii, window, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .child(self.render_document(theme, cx))
                                    .when(self.panel_open, |row| {
                                        row.child(panels::right_panel(theme))
                                    }),
                            ),
                    ),
            )
            .child(status_bar::status_bar(self.active_tab(), theme, radii))
    }
}

/// A main window and its workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceWindow {
    pub window: AnyWindowHandle,
    pub workspace: WeakEntity<Workspace>,
}

impl WorkspaceWindow {
    /// Opens `target` as a tab in this window (see [`Workspace::open_tab`]).
    pub fn open_tab(&self, target: OpenTarget, cx: &mut App) -> Result<()> {
        let workspace = self.workspace.clone();
        cx.update_window(self.window, |_, window, cx| {
            workspace.update(cx, |ws, cx| ws.open_tab(target, window, cx))
        })?
    }

    /// Brings the window to the front.
    pub fn activate(&self, cx: &mut App) {
        // Fails only when the window has closed.
        let _ = cx.update_window(self.window, |_, window, _| window.activate_window());
    }
}

/// The main windows, most recently focused first.
#[derive(Debug, Default)]
struct RecentWindows(Vec<WorkspaceWindow>);

impl Global for RecentWindows {}

impl RecentWindows {
    fn touch(entry: WorkspaceWindow, cx: &mut App) {
        let order = &mut cx.default_global::<Self>().0;
        order.retain(|w| w.window != entry.window);
        order.insert(0, entry);
    }
}

/// The main window that was focused last and is still open.
pub fn most_recent_window(cx: &mut App) -> Option<WorkspaceWindow> {
    let open = cx.windows();
    let order = &mut cx.default_global::<RecentWindows>().0;
    order.retain(|w| open.contains(&w.window) && w.workspace.upgrade().is_some());
    order.first().cloned()
}

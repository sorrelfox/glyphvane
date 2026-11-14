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

pub(crate) use files_panel::key_bindings as file_tree_key_bindings;

use std::fmt;
use std::path::{Path, PathBuf};

use gpui::component::window_corner_radii;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, FocusHandle, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Result, SharedString,
    Styled as _, Subscription, WeakEntity, Window, div, px,
};

use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::metrics::text;
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

/// One open document in a window.
#[derive(Debug, Clone)]
pub struct Tab {
    label: FileLabel,
    /// `None` for the empty tab of a window opened without a file.
    path: Option<PathBuf>,
    /// Where the Reading view is to be scrolled, from `PATH:LINE[:COL]`. The
    /// Reading view does not exist yet, so it is only shown in the status bar.
    position: Option<TextPosition>,
    mode: ViewMode,
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
        }
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
        Self {
            focus,
            chrome,
            tabs: vec![Tab::new(target)],
            active: 0,
            sidebar_open: false,
            panel_open: false,
            vault: None,
            files: None,
            _files_events: None,
            drag_pending: false,
            _appearance: appearance,
            _activation: activation,
        }
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
        let tab = Tab::new(Some(target));
        if self.tabs.len() == 1 && self.tabs[0].path.is_none() {
            self.tabs[0] = tab;
            self.active = 0;
        } else {
            self.active += 1;
            self.tabs.insert(self.active, tab);
        }
        self.tabs_changed(window, cx);
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        if index < self.tabs.len() && index != self.active {
            self.active = index;
            self.tabs_changed(window, cx);
        }
    }

    fn tabs_changed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
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
        if let Some(files) = &self.files {
            files.update(cx, |files, cx| files.set_vault(vault, cx));
        }
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
                                    .child(self.render_document(theme))
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

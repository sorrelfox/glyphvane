// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The main window: title row, side panels, document area and status bar.

mod controls;
mod document;
mod panels;
mod status_bar;
#[cfg(test)]
mod tests;
mod title_row;

use std::fmt;
use std::path::Path;

use gpui::component::window_corner_radii;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Window, div, px,
};

use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::metrics::text;
use crate::{ToggleEditMode, TogglePanel, ToggleSidebar};

/// What the document area shows. Each tab has its own mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Preview,
    Edit,
    Split,
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
    file: FileLabel,
    mode: ViewMode,
    /// Side panels start hidden: a window opened for a file shows only the
    /// document until the user calls a panel.
    sidebar_open: bool,
    panel_open: bool,
    /// The pointer went down on a drag region of a client-decorated window
    /// and has not moved yet.
    drag_pending: bool,
    _appearance: Subscription,
}

impl fmt::Debug for Workspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workspace")
            .field("chrome", &self.chrome)
            .field("file", &self.file)
            .field("mode", &self.mode)
            .field("sidebar_open", &self.sidebar_open)
            .field("panel_open", &self.panel_open)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    pub fn new(file: FileLabel, window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        Self::with_chrome(file, WindowChrome::current(), window, cx)
    }

    /// A workspace drawn with another platform's chrome. Only the window's
    /// own platform behaves natively; the others are for layout tests.
    pub fn with_chrome(
        file: FileLabel,
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
        Self {
            focus,
            chrome,
            file,
            mode: ViewMode::default(),
            sidebar_open: false,
            panel_open: false,
            drag_pending: false,
            _appearance: appearance,
        }
    }

    fn set_mode(&mut self, mode: ViewMode, cx: &mut Context<'_, Self>) {
        self.mode = mode;
        cx.notify();
    }

    fn toggle_edit_mode(&mut self, cx: &mut Context<'_, Self>) {
        let mode = match self.mode {
            ViewMode::Edit => ViewMode::Preview,
            ViewMode::Preview | ViewMode::Split => ViewMode::Edit,
        };
        self.set_mode(mode, cx);
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<'_, Self>) {
        self.sidebar_open = !self.sidebar_open;
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
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
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
            .child(status_bar::status_bar(&self.file, theme, radii))
    }
}

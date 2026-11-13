// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GPUI views: workspace, panes, editor and reader elements, panels, theming.

pub mod appearance;
pub mod chrome;
pub mod icons;
pub mod metrics;
pub mod shell;
pub mod theme;

use std::path::PathBuf;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, KeyBinding, Result, WindowBounds, WindowOptions,
    px, size,
};

pub use appearance::Appearance;
pub use chrome::WindowChrome;
pub use icons::Assets;
pub use theme::{ThemeChoice, ThemeId};

use crate::metrics::size as sz;
use crate::shell::Workspace;

/// The application id the desktop uses to group windows and find the icon.
pub const APP_ID: &str = "io.github.sorrelfox.Glyphvane";

gpui::actions!(
    glyphvane,
    [
        /// Switch to the next theme: System, Light, Semi-light, Dark.
        CycleTheme,
        /// Switch the active tab between Preview and Edit.
        ToggleEditMode,
        /// Show or hide the sidebar.
        ToggleSidebar,
        /// Show or hide the right panel.
        TogglePanel,
        /// Quit the application.
        Quit,
    ]
);

/// Default key bindings. Mod is Cmd on macOS and Ctrl elsewhere. Themes cycle
/// on the chord Mod+K Mod+T: Ctrl+Alt+T opens a terminal on most Linux desktops.
#[cfg(target_os = "macos")]
mod keys {
    pub const CYCLE_THEME: &str = "cmd-k cmd-t";
    pub const TOGGLE_EDIT_MODE: &str = "cmd-e";
    pub const QUIT: &str = "cmd-q";
}
#[cfg(not(target_os = "macos"))]
mod keys {
    pub const CYCLE_THEME: &str = "ctrl-k ctrl-t";
    pub const TOGGLE_EDIT_MODE: &str = "ctrl-e";
    pub const QUIT: &str = "ctrl-q";
}

/// Initialises GPUI Component, the theme and the key bindings. Call once,
/// inside `Application::run`, before opening a window.
pub fn init(cx: &mut App, theme: ThemeChoice) {
    gpui::component::init(cx);
    Appearance::init(cx, theme);

    cx.bind_keys([
        KeyBinding::new(keys::CYCLE_THEME, CycleTheme, None),
        KeyBinding::new(keys::TOGGLE_EDIT_MODE, ToggleEditMode, None),
        KeyBinding::new(keys::QUIT, Quit, None),
    ]);
    cx.on_action(|_: &CycleTheme, cx| {
        let next = Appearance::choice(cx).next();
        Appearance::set_choice(cx, next);
    });
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

/// Opens a main window for `path`, as when the file is passed on the command
/// line: Preview mode, both side panels hidden.
pub fn open_file_window(cx: &mut App, path: Option<PathBuf>) -> Result<AnyWindowHandle> {
    let file_name = shell::FileLabel::new(path.as_deref());
    let options = main_window_options(WindowChrome::current(), file_name.window_title().into(), cx);
    let (window, _) = gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::new(file_name, window, cx))
    })?;
    cx.activate(true);
    Ok(window)
}

/// A main window with the platform's chrome: no system title bar, so the
/// title row is the only row of chrome above the document.
fn main_window_options(chrome: WindowChrome, title: gpui::SharedString, cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(
        None,
        size(px(sz::MAIN_WINDOW_WIDTH), px(sz::MAIN_WINDOW_HEIGHT)),
        cx,
    );
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(chrome.titlebar(title)),
        window_decorations: Some(chrome.decorations()),
        app_owns_titlebar_drag: chrome.app_owns_titlebar_drag(),
        app_id: Some(APP_ID.to_owned()),
        ..WindowOptions::default()
    }
}

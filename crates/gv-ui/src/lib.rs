// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GPUI views: workspace, panes, editor and reader elements, panels, theming.

pub mod appearance;
pub mod chrome;
pub mod icons;
pub mod inline;
pub mod metrics;
pub mod reader;
pub mod settings;
pub mod shell;
pub mod theme;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, KeyBinding, Result, WindowBounds, WindowOptions,
    px, size,
};

pub use appearance::Appearance;
pub use chrome::WindowChrome;
pub use icons::Assets;
pub use settings::AppSettings;
pub use shell::{OpenTarget, TextPosition, WorkspaceWindow, most_recent_window};
pub use theme::{PreferredLight, ThemeChoice, ThemeId};

use crate::metrics::size as sz;
use crate::shell::Workspace;

/// The application id the desktop uses to group windows and find the icon.
pub const APP_ID: &str = "io.github.sorrelfox.Glyphvane";

gpui::actions!(
    glyphvane,
    [
        /// Switch to the next theme: System, Light, Semi-light, Dark. The
        /// choice is saved to the settings file.
        CycleTheme,
        /// Switch the active tab between Preview and Edit.
        ToggleEditMode,
        /// Show or hide the sidebar.
        ToggleSidebar,
        /// Show or hide the right panel.
        TogglePanel,
        /// Go back to where the active tab was.
        GoBack,
        /// Go forward again after going back.
        GoForward,
        /// Close the active tab.
        CloseTab,
        /// Reopen the tab closed last, with its history.
        ReopenClosedTab,
        /// Quit the application.
        Quit,
    ]
);

/// Default key bindings. Mod is Cmd on macOS and Ctrl elsewhere. Themes cycle
/// on the chord Mod+K Mod+T: Ctrl+Alt+T opens a terminal on most Linux desktops.
/// History is Alt+Left and Alt+Right outside macOS, where Alt+Left moves by
/// word; there it is Cmd+[ and Cmd+] or Cmd+Alt+Left and Cmd+Alt+Right.
// TODO: bind through the keymap model (`gv_settings::keymap`) once it is
// wired into the window; these are its defaults.
#[cfg(target_os = "macos")]
mod keys {
    pub const CYCLE_THEME: &str = "cmd-k cmd-t";
    pub const TOGGLE_EDIT_MODE: &str = "cmd-e";
    pub const QUIT: &str = "cmd-q";
    pub const GO_BACK: &[&str] = &["cmd-[", "cmd-alt-left"];
    pub const GO_FORWARD: &[&str] = &["cmd-]", "cmd-alt-right"];
    pub const CLOSE_TAB: &str = "cmd-w";
    pub const REOPEN_CLOSED_TAB: &str = "cmd-shift-t";
}
#[cfg(not(target_os = "macos"))]
mod keys {
    pub const CYCLE_THEME: &str = "ctrl-k ctrl-t";
    pub const TOGGLE_EDIT_MODE: &str = "ctrl-e";
    pub const QUIT: &str = "ctrl-q";
    pub const GO_BACK: &[&str] = &["alt-left"];
    pub const GO_FORWARD: &[&str] = &["alt-right"];
    pub const CLOSE_TAB: &str = "ctrl-w";
    pub const REOPEN_CLOSED_TAB: &str = "ctrl-shift-t";
}

/// Initialises GPUI Component, the settings, the theme and the key bindings.
/// Call once, inside `Application::run`, before opening a window.
pub fn init(cx: &mut App, settings: AppSettings) {
    gpui::component::init(cx);
    let (choice, preferred_light) = settings.theme_preference();
    settings.install(cx);
    Appearance::init(cx, choice, preferred_light);

    cx.bind_keys([
        KeyBinding::new(keys::CYCLE_THEME, CycleTheme, None),
        KeyBinding::new(keys::TOGGLE_EDIT_MODE, ToggleEditMode, None),
        KeyBinding::new(keys::QUIT, Quit, None),
        KeyBinding::new(keys::CLOSE_TAB, CloseTab, None),
        KeyBinding::new(keys::REOPEN_CLOSED_TAB, ReopenClosedTab, None),
    ]);
    cx.bind_keys(
        keys::GO_BACK
            .iter()
            .map(|k| KeyBinding::new(k, GoBack, None)),
    );
    cx.bind_keys(
        keys::GO_FORWARD
            .iter()
            .map(|k| KeyBinding::new(k, GoForward, None)),
    );
    cx.bind_keys(shell::file_tree_key_bindings());
    cx.bind_keys(reader::key_bindings());
    cx.on_action(|_: &CycleTheme, cx| AppSettings::cycle_theme(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

/// Opens a main window for `target`, as for a file passed on the command
/// line: Preview mode, both side panels hidden (FR-VIEW-16). `None` opens a
/// window with an empty tab.
pub fn open_file_window(cx: &mut App, target: Option<OpenTarget>) -> Result<AnyWindowHandle> {
    open_workspace_window(cx, target).map(|w| w.window)
}

fn open_workspace_window(cx: &mut App, target: Option<OpenTarget>) -> Result<WorkspaceWindow> {
    let title = shell::FileLabel::new(target.as_ref().map(|t| t.path.as_path())).window_title();
    let options = main_window_options(WindowChrome::current(), title.into(), cx);
    let (window, workspace) = gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::new(target, window, cx))
    })?;
    cx.activate(true);
    Ok(WorkspaceWindow {
        window,
        workspace: workspace.downgrade(),
    })
}

/// Where [`open`] puts files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// As tabs in the most recently focused main window, leaving its panels
    /// as they are; in a new window when none is open.
    #[default]
    RecentWindow,
    /// In a new window, opened as for a file passed at launch.
    NewWindow,
}

/// Opens files from the command line, another instance or the platform.
///
/// The files open as tabs of one window, the last one active. A new window
/// (because of `placement` or because none is open) starts in Preview with
/// its side panels hidden (FR-VIEW-16); an existing one keeps its panels.
/// With no files, an existing window is brought forward, or an empty one is
/// opened. Returns the window the files went to.
pub fn open(
    cx: &mut App,
    targets: Vec<OpenTarget>,
    placement: Placement,
) -> Result<AnyWindowHandle> {
    let existing = match placement {
        Placement::RecentWindow => most_recent_window(cx),
        Placement::NewWindow => None,
    };
    let mut targets = targets.into_iter();
    let window = match existing {
        Some(window) => window,
        None => open_workspace_window(cx, targets.next())?,
    };
    for target in targets {
        window.open_tab(target, cx)?;
    }
    window.activate(cx);
    Ok(window.window)
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

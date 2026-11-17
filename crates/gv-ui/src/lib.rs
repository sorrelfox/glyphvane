// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! GPUI views: workspace, panes, editor and reader elements, panels, theming.

pub mod about;
pub mod appearance;
pub mod canvas_view;
pub mod chrome;
pub mod compare;
pub mod editor;
pub mod export;
pub mod file_prompt;
pub mod files;
pub mod find;
pub mod goto;
pub mod icons;
pub mod inline;
pub mod keymap;
pub mod latency;
pub mod menu;
pub mod metrics;
pub mod palette;
pub mod platform;
pub mod print;
pub mod profile;
pub mod reader;
pub mod settings;
pub mod settings_window;
pub mod shell;
pub mod spell;
pub mod split;
pub mod switcher;
pub mod theme;
pub mod zoom;

use std::path::Path;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Result, WindowBounds, WindowOptions, px, size,
};

pub use appearance::Appearance;
pub use chrome::WindowChrome;
pub use icons::Assets;
pub use settings::AppSettings;
pub use shell::{
    CloseHook, FileClosed, OpenTarget, TextPosition, WorkspaceWindow, most_recent_window,
};
pub use theme::{PreferredLight, ThemeChoice, ThemeId};

use crate::metrics::size as sz;
use crate::shell::Workspace;

/// The application id the desktop uses to group windows and find the icon.
pub const APP_ID: &str = "io.github.sorrelfox.Glyphvane";

// The commands the window handles, under the names the views use. Every
// command's action is in [`keymap::actions`].
pub use keymap::actions::app::Quit;
pub use keymap::actions::navigation::{Back as GoBack, Forward as GoForward};
pub use keymap::actions::tabs::{Close as CloseTab, ReopenClosed as ReopenClosedTab};
pub use keymap::actions::view::{CycleTheme, ToggleMode as ToggleEditMode};
pub use keymap::actions::workspace::{TogglePanel, ToggleSidebar};

/// Initialises GPUI Component, the settings, the theme and the key bindings.
/// Call once, inside `Application::run`, before opening a window. The
/// bindings are the keymap's built-in ones until [`keymap::install`] adds
/// the user's `keymap.toml`.
pub fn init(cx: &mut App, settings: AppSettings) {
    gpui::component::init(cx);
    let (choice, preferred_light) = settings.theme_preference();
    settings.install(cx);
    Appearance::init(cx, choice, preferred_light);

    // The widgets' own keys, then the commands' keys from the keymap.
    cx.bind_keys(shell::file_tree_key_bindings());
    cx.bind_keys(reader::key_bindings());
    cx.bind_keys(canvas_view::key_bindings());
    cx.bind_keys(editor::key_bindings());
    cx.bind_keys(find::key_bindings());
    menu::init(cx);
    palette::init(cx);
    goto::init(cx);
    file_prompt::init(cx);
    switcher::init(cx);
    settings_window::init(cx);
    keymap::init(cx);
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

/// Opens `folder` as a vault, as File ▸ Open folder… does, for the command
/// line, another instance or the platform.
///
/// The folder goes to the most recently focused main window when that
/// window shows nothing yet or already has the folder as its vault;
/// otherwise, or with [`Placement::NewWindow`], to a new window. The window
/// shows the Files panel, is brought forward, and the folder becomes the
/// most recent folder. The home folder, a filesystem root and very large
/// folders still ask before they are indexed. Returns the window, so files
/// inside the folder can open there as tabs.
pub fn open_folder(cx: &mut App, folder: &Path, placement: Placement) -> Result<WorkspaceWindow> {
    let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    if !folder.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not a folder", folder.display()),
        )
        .into());
    }
    let existing = match placement {
        Placement::RecentWindow => most_recent_window(cx).filter(|window| {
            window
                .workspace
                .upgrade()
                .is_some_and(|ws| ws.read(cx).takes_folder(&folder, cx))
        }),
        Placement::NewWindow => None,
    };
    let window = match existing {
        Some(window) => window,
        None => open_workspace_window(cx, None)?,
    };
    let workspace = window.workspace.clone();
    cx.update_window(window.window, |_, w, cx| {
        workspace.update(cx, |ws, cx| ws.show_folder(folder, w, cx))
    })??;
    window.activate(cx);
    Ok(window)
}

/// Opens files for an edit that someone waits for (`--wait`): as [`open`]
/// does, but each file opens in Edit mode (Source), and its [`CloseHook`] is
/// told when its tab lets go of it.
pub fn open_for_edit(
    cx: &mut App,
    targets: Vec<(OpenTarget, CloseHook)>,
    placement: Placement,
) -> Result<AnyWindowHandle> {
    let existing = match placement {
        Placement::RecentWindow => most_recent_window(cx),
        Placement::NewWindow => None,
    };
    let mut targets = targets.into_iter();
    let window = match existing {
        Some(window) => window,
        None => {
            let (target, hook) = targets.next().unzip();
            let window = open_workspace_window(cx, target)?;
            if hook.is_some() {
                window.edit_active_tab(hook, cx)?;
            }
            window
        }
    };
    for (target, hook) in targets {
        window.open_tab_for_edit(target, Some(hook), cx)?;
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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the application menu holds, per platform. Its contents are not
//! designed: they follow the platforms' usual menus, trimmed to the commands
//! the application has.
//!
//! Items name commands of the keymap, so a menu runs exactly what the keys
//! run and shows their keys. Items whose command nothing handles yet (New
//! tab, Open file, About, zoom) are shown disabled.

use gv_settings::keymap::Platform;

use crate::shell::ViewMode;

/// A menu: File, Edit, View, Go, Help, and on macOS the application menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

impl Menu {
    fn new(title: &'static str, entries: impl IntoIterator<Item = Entry>) -> Self {
        Self {
            title,
            entries: entries.into_iter().collect(),
        }
    }

    /// The command items, in order.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Command(item) => Some(item),
            Entry::Separator | Entry::System(_) => None,
        })
    }

    /// Whether some item shows a check mark.
    pub fn has_checks(&self) -> bool {
        self.items().any(|i| i.mode.is_some())
    }
}

/// One entry of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// Runs a command.
    Command(Item),
    Separator,
    /// An entry of the system's own (macOS).
    System(SystemEntry),
}

/// A menu item that runs a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The command id, as in `keymap.toml`.
    pub command: &'static str,
    pub title: &'static str,
    /// The item is checked while the active tab shows this mode.
    pub mode: Option<ViewMode>,
    /// The item is enabled only while this holds.
    pub requires: Option<Requirement>,
    /// What the system knows the item as (macOS routes these through the
    /// text system).
    pub role: Option<Role>,
}

/// A condition for an item to be enabled, besides its command having a
/// handler where the focus is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    CanGoBack,
    CanGoForward,
}

/// A standard edit item the system recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Copy,
    SelectAll,
}

/// Entries of the macOS application menu that the system provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEntry {
    Services,
    Hide,
    HideOthers,
    ShowAll,
}

impl SystemEntry {
    pub fn title(self) -> &'static str {
        match self {
            Self::Services => "Services",
            Self::Hide => "Hide Glyphvane",
            Self::HideOthers => "Hide others",
            Self::ShowAll => "Show all",
        }
    }
}

/// What the window tells the menu when it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuState {
    pub mode: ViewMode,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

impl MenuState {
    pub fn allows(self, requirement: Requirement) -> bool {
        match requirement {
            Requirement::CanGoBack => self.can_go_back,
            Requirement::CanGoForward => self.can_go_forward,
        }
    }
}

const fn item(command: &'static str, title: &'static str) -> Entry {
    Entry::Command(Item {
        command,
        title,
        mode: None,
        requires: None,
        role: None,
    })
}

const fn mode_item(command: &'static str, title: &'static str, mode: ViewMode) -> Entry {
    Entry::Command(Item {
        command,
        title,
        mode: Some(mode),
        requires: None,
        role: None,
    })
}

const fn requiring(command: &'static str, title: &'static str, requires: Requirement) -> Entry {
    Entry::Command(Item {
        command,
        title,
        mode: None,
        requires: Some(requires),
        role: None,
    })
}

const fn with_role(command: &'static str, title: &'static str, role: Role) -> Entry {
    Entry::Command(Item {
        command,
        title,
        mode: None,
        requires: None,
        role: Some(role),
    })
}

const SEPARATOR: Entry = Entry::Separator;

/// The application's menus on `platform`, in menu bar order. On macOS the
/// application menu comes first and holds About, Settings and Quit; on
/// Windows and Linux, Quit ends the File menu and About is under Help.
pub fn app_menus(platform: Platform) -> Vec<Menu> {
    let mac = platform == Platform::MacOs;
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu::new(
            "Glyphvane",
            [
                item("app.about", "About Glyphvane"),
                SEPARATOR,
                item("app.settings", "Settings…"),
                SEPARATOR,
                Entry::System(SystemEntry::Services),
                SEPARATOR,
                Entry::System(SystemEntry::Hide),
                Entry::System(SystemEntry::HideOthers),
                Entry::System(SystemEntry::ShowAll),
                SEPARATOR,
                item("app.quit", "Quit Glyphvane"),
            ],
        ));
    }
    let mut file = vec![
        item("tabs.new", "New tab"),
        item("file.open", "Open file…"),
        SEPARATOR,
        item("tabs.close", "Close tab"),
        item("tabs.reopen_closed", "Reopen closed tab"),
    ];
    if !mac {
        file.extend([SEPARATOR, item("app.quit", "Quit")]);
    }
    menus.push(Menu::new("File", file));
    menus.push(Menu::new(
        "Edit",
        [
            with_role("editor.copy", "Copy", Role::Copy),
            with_role("editor.select_all", "Select all", Role::SelectAll),
            SEPARATOR,
            item("search.find", "Find"),
        ],
    ));
    menus.push(Menu::new(
        "View",
        [
            mode_item("view.preview", "Preview", ViewMode::Preview),
            mode_item("view.edit", "Edit", ViewMode::Edit),
            mode_item("view.split", "Split", ViewMode::Split),
            SEPARATOR,
            item("workspace.toggle_sidebar", "Toggle sidebar"),
            item("workspace.toggle_panel", "Toggle right panel"),
            SEPARATOR,
            item("view.cycle_theme", "Cycle theme"),
            SEPARATOR,
            item("view.zoom_in", "Zoom in"),
            item("view.zoom_out", "Zoom out"),
            item("view.zoom_reset", "Reset zoom"),
        ],
    ));
    menus.push(Menu::new(
        "Go",
        [
            requiring("navigation.back", "Back", Requirement::CanGoBack),
            requiring("navigation.forward", "Forward", Requirement::CanGoForward),
        ],
    ));
    // On macOS About is in the application menu, which leaves Help empty
    // until there is help to show.
    if !mac {
        menus.push(Menu::new("Help", [item("app.about", "About Glyphvane")]));
    }
    menus
}

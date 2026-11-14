// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The application menu: File, Edit, View, Go and Help ([`model`]).
//!
//! - **macOS:** the system menu bar, set with [`gpui::App::set_menus`]. The
//!   system shows each item's shortcut from the key bindings (a chord such
//!   as ⌘K ⌘T cannot be shown there) and disables items whose command has
//!   no handler where the focus is.
//! - **Windows and Linux:** the menu button at the start of the sidebar head
//!   (or of the title row) opens the menu as a popover drawn like the link
//!   menu (SPEC § Popovers): one row per menu, each opening its items in a
//!   submenu beside it. Submenus keep the popover as short as the menu bar
//!   it replaces and keep the macOS structure. F10 (`app.open_menu`) opens
//!   it from the keyboard; arrow keys move and open, Enter runs, Esc closes;
//!   the shortcut labels come from the keymap.
//!
//! The menu's contents and the submenu layout are not designed.

pub mod model;
mod popover;
#[cfg(test)]
mod tests;

use gpui::{App, KeyBinding, OsAction, SystemMenuType};
use gv_settings::keymap::Platform;

use crate::keymap::action_for;
use model::{Entry, Role, SystemEntry};
pub use model::{Item, Menu, MenuState};
pub(crate) use popover::open_in_last_window;
pub use popover::{DEBUG_APP_MENU_ENV, MenuButton, MenuPopover};

gpui::actions!(
    app_menu,
    [
        /// Highlight the previous item.
        SelectPrevious,
        /// Highlight the next item.
        SelectNext,
        /// Highlight the first item.
        SelectFirst,
        /// Highlight the last item.
        SelectLast,
        /// Open the highlighted menu, or the next menu from a submenu.
        OpenSubmenu,
        /// Go back from a submenu to the menus.
        CloseSubmenu,
        /// Run the highlighted item, or open the highlighted menu.
        Confirm,
        /// Close the menu.
        Cancel,
        /// Hide the application (macOS).
        Hide,
        /// Hide the other applications (macOS).
        HideOthers,
        /// Show the hidden applications again (macOS).
        ShowAll,
    ]
);

/// The menu popover's own keys, in its key context.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let context = Some(crate::keymap::context::MENU);
    let mut keys = vec![
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("home", SelectFirst, context),
        KeyBinding::new("end", SelectLast, context),
        KeyBinding::new("right", OpenSubmenu, context),
        KeyBinding::new("left", CloseSubmenu, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("escape", Cancel, context),
    ];
    // The system's conventions for the application menu; they are not
    // commands of the keymap.
    if cfg!(target_os = "macos") {
        keys.extend([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
        ]);
    }
    keys
}

/// Binds the menu's keys and the macOS application menu's actions. Called
/// by [`crate::init`] before the keymap is applied.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
}

/// The menus for the system menu bar of `platform`. Built on every
/// platform so it is tested everywhere; only macOS shows it.
pub fn native_menus(platform: Platform) -> Vec<gpui::Menu> {
    model::app_menus(platform)
        .into_iter()
        .map(|menu| gpui::Menu {
            name: menu.title.into(),
            items: menu.entries.iter().filter_map(native_item).collect(),
            disabled: false,
        })
        .collect()
}

fn native_item(entry: &Entry) -> Option<gpui::MenuItem> {
    Some(match *entry {
        Entry::Separator => gpui::MenuItem::separator(),
        Entry::Command(item) => gpui::MenuItem::Action {
            name: item.title.into(),
            action: action_for(item.command)?,
            os_action: item.role.map(|role| match role {
                Role::Copy => OsAction::Copy,
                Role::SelectAll => OsAction::SelectAll,
            }),
            checked: false,
            disabled: false,
        },
        Entry::System(system) => match system {
            SystemEntry::Services => {
                gpui::MenuItem::os_submenu(system.title(), SystemMenuType::Services)
            }
            SystemEntry::Hide => gpui::MenuItem::action(system.title(), Hide),
            SystemEntry::HideOthers => gpui::MenuItem::action(system.title(), HideOthers),
            SystemEntry::ShowAll => gpui::MenuItem::action(system.title(), ShowAll),
        },
    })
}

/// Sets the system menu bar again, for its shortcuts to follow the keymap.
/// Only macOS has one.
pub(crate) fn refresh_native(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.set_menus(native_menus(Platform::MacOs));
    }
}

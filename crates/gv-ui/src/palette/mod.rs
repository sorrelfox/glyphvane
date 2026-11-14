// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The command palette (`workspace.command_palette`, Mod+P and
//! Mod+Shift+P): every command of the keymap that can run where the
//! keyboard is, searched by title or id.
//!
//! The palette is not designed. It is drawn like the quick switcher (SPEC
//! § Popovers): 540 wide, 84 below the top of the window and centred, an
//! input row 40 high with the search icon, rows 28 high, a footer with key
//! hints. A row shows the title with the matched letters in weight 700,
//! then the category and the shortcut at the right.
//!
//! # Behaviour
//!
//! - **What is listed.** The commands whose action has a handler on the
//!   focus path of what had the keyboard when the palette opened, and the
//!   application-wide commands ([`crate::keymap::APP_COMMANDS`]). The rest
//!   are left out rather than listed dimmed: most of the registry is editor
//!   commands, which would bury the few that apply in Preview. The palette
//!   does not list itself.
//! - **Order.** With no query, the commands run recently (in this session,
//!   newest first), then the rest alphabetically. With a query, the best
//!   fuzzy matches first ([`model::rows`]).
//! - **Keys.** Up and Down move the highlight, Enter runs the highlighted
//!   command, Esc closes; Mod+P again closes too. A click on a row runs it,
//!   a click outside closes.
//! - **Running.** The palette closes, gives the keyboard back, and dispatches
//!   the command's action there, as if its keys had been pressed.
//!
//! The palette is a layer of the window root (GPUI Base's `Root` plugin),
//! so it covers any window opened after [`init`].

pub mod model;
#[cfg(test)]
mod tests;
mod view;

use gpui::base::Root;
use gpui::{App, Entity, Global, KeyBinding, Window};

pub use view::{CommandPalette, DEBUG_PALETTE_ENV};

gpui::actions!(
    command_palette,
    [
        /// Highlight the previous command.
        SelectPrevious,
        /// Highlight the next command.
        SelectNext,
        /// Run the highlighted command.
        Confirm,
        /// Close the palette.
        Cancel,
    ]
);

/// The palette's key context: the model's `palette` context.
pub const KEY_CONTEXT: &str = "CommandPalette";

/// The palette's own keys. They are bound on the field as well, where the
/// text field's own Up, Down and Esc would otherwise take them.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let field = "CommandPalette > Input";
    let mut keys = Vec::new();
    for context in [KEY_CONTEXT, field] {
        keys.extend([
            KeyBinding::new("up", SelectPrevious, Some(context)),
            KeyBinding::new("down", SelectNext, Some(context)),
            KeyBinding::new("enter", Confirm, Some(context)),
            KeyBinding::new("escape", Cancel, Some(context)),
        ]);
    }
    keys
}

/// The commands run from the palette, newest first, for the whole
/// application. Not saved.
#[derive(Debug, Default)]
struct Recent(Vec<String>);

impl Global for Recent {}

fn recent(cx: &App) -> Vec<String> {
    cx.try_global::<Recent>()
        .map(|r| r.0.clone())
        .unwrap_or_default()
}

fn remember(id: &str, cx: &mut App) {
    model::remember(&mut cx.default_global::<Recent>().0, id);
}

/// Binds the palette's keys and adds it to the window root. Called by
/// [`crate::init`], before any window opens.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
    Root::register_plugin::<CommandPalette>(cx, CommandPalette::new);
}

/// The palette of `window`, if its root has one.
pub fn palette_of(window: &Window, cx: &App) -> Option<Entity<CommandPalette>> {
    window
        .root::<Root>()
        .flatten()?
        .read(cx)
        .plugin::<CommandPalette>()
}

/// Opens the palette over the active window, or closes it
/// (`workspace.command_palette`).
pub(crate) fn toggle(cx: &mut App) {
    let window = cx
        .active_window()
        .or_else(|| crate::shell::most_recent_window(cx).map(|w| w.window));
    let Some(window) = window else {
        return;
    };
    // The window is busy with the key that asked for the palette.
    cx.defer(move |cx| {
        // Fails only when the window has closed.
        let _ = window.update(cx, |_, window, cx| {
            if let Some(palette) = palette_of(window, cx) {
                palette.update(cx, |palette, cx| palette.toggle(window, cx));
            }
        });
    });
}

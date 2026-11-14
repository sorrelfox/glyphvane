// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keymap model ([`gv_settings::keymap`]) in GPUI: commands become
//! actions, the model's contexts become key contexts, and the effective
//! bindings (the built-in defaults with the user's `keymap.toml` on top)
//! become GPUI key bindings.
//!
//! # Design
//!
//! - **Actions.** Every command has an action type of its own
//!   ([`actions`]), GPUI's usual pattern rather than one action carrying a
//!   command id: views handle the commands they implement with typed
//!   `on_action` listeners, and GPUI's action availability (menus) and
//!   binding lookups work per type. A command nothing handles yet is simply
//!   unavailable: its keys fall through to other bindings and to text input,
//!   and its menu items are disabled. No command takes arguments yet, so
//!   `args` from the keymap file are ignored with a warning.
//! - **Contexts.** [`context`] maps each model context to the GPUI key
//!   context its view sets, and translates predicates node for node.
//! - **Precedence.** The model ranks a user binding over a built-in one,
//!   then the more specific context, then the later binding. GPUI ranks the
//!   binding that matches on the deeper element of the focus chain first,
//!   then the binding added later. The bindings are added in the model's
//!   order, lowest rank first, so the two agree whenever both bindings match
//!   on the same element. Where they match on different elements (a text
//!   field inside the Reading view), GPUI's rule applies, which keeps the
//!   field's own keys working. Bindings the model finds unreachable are left
//!   out, so a key whose winning command has no handler cannot fall through
//!   to the binding it replaced.
//! - **Widget keys.** Keys that move within a focused widget (arrow keys,
//!   Page Up and Down, Space, Home and End in the Reading view; arrow keys
//!   and Enter in the file tree and in menus; Esc in popovers) belong to the
//!   widget, which binds them itself, as a text field binds its editing
//!   keys. They are not commands.
//! - **Reload.** [`install`] reads `keymap.toml`, logs its diagnostics and
//!   watches it. Every change replaces the command bindings, and the native
//!   menus whose shortcuts come from them, without a restart.

pub mod actions;
pub mod context;
#[cfg(test)]
mod tests;
mod watch;

use std::io;
use std::path::Path;
use std::rc::Rc;

use gpui::{App, Global, KeyBinding, KeyBindingMetaIndex};
use gv_settings::keymap::{CommandId, CommandRegistry, ContextExpr, Keymap, Platform, UserKeymap};
use gv_settings::{Locations, SettingsError, Severity};

use crate::settings::AppSettings;
use crate::shell::{ViewMode, most_recent_window};
pub use actions::action_for;

/// Marks the key bindings made from the keymap model, so that a reload
/// replaces exactly those.
const COMMAND_BINDING: KeyBindingMetaIndex = KeyBindingMetaIndex(0x676b);

/// Commands handled for the whole application rather than by a view. Their
/// actions are available whatever has the focus.
pub const APP_COMMANDS: &[&str] = &[
    "app.quit",
    "app.open_menu",
    "view.cycle_theme",
    "view.preview",
    "view.edit",
    "view.split",
];

/// One binding of the model, as it is handed to GPUI.
#[derive(Debug, Clone, PartialEq)]
pub struct BindingSpec {
    /// The keystrokes in GPUI's syntax, such as `ctrl-k ctrl-t`.
    pub keys: String,
    pub command: CommandId,
    /// Where the binding applies; `None` means everywhere.
    pub context: Option<ContextExpr>,
    /// Whether the binding gave arguments, which are not passed yet.
    pub has_args: bool,
}

/// The model's reachable bindings, lowest precedence first (see the
/// module documentation).
pub fn binding_specs(keymap: &Keymap) -> Vec<BindingSpec> {
    let mut ranked: Vec<_> = keymap
        .bindings()
        .iter()
        .enumerate()
        .filter(|(_, b)| b.is_reachable())
        .collect();
    ranked.sort_by_key(|(order, b)| (b.origin.layer, b.specificity(), *order));
    ranked
        .into_iter()
        .map(|(_, b)| BindingSpec {
            keys: b.keys.to_string(),
            command: b.command.clone(),
            context: b.context.clone(),
            has_args: b.args.is_some(),
        })
        .collect()
}

/// GPUI key bindings for `specs`, in the same order. A spec whose command
/// has no action, or whose keys GPUI cannot read, is left out and logged.
pub fn key_bindings(specs: &[BindingSpec], cx: &App) -> Vec<KeyBinding> {
    specs
        .iter()
        .filter_map(|spec| {
            let Some(action) = action_for(spec.command.as_str()) else {
                tracing::warn!(
                    "keymap: `{}` has no action, so {} is not bound",
                    spec.command,
                    spec.keys
                );
                return None;
            };
            if spec.has_args {
                tracing::warn!(
                    "keymap: `{}` takes no arguments, so those given with {} are ignored",
                    spec.command,
                    spec.keys
                );
            }
            let predicate = spec
                .context
                .as_ref()
                .map(|c| Rc::new(context::predicate(c)));
            match KeyBinding::load(&spec.keys, action, predicate, false, None, cx) {
                Ok(binding) => Some(binding.with_meta(COMMAND_BINDING)),
                Err(err) => {
                    tracing::warn!("keymap: {} is not bound: {err}", spec.command);
                    None
                }
            }
        })
        .collect()
}

/// The commands and the bindings in effect. One per application.
#[derive(Debug)]
pub struct Keybindings {
    registry: CommandRegistry,
    keymap: Keymap,
}

impl Global for Keybindings {}

impl Keybindings {
    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// The label of a command's primary keys on this platform (`Ctrl+W`,
    /// `⌘W`), as menus show it.
    pub fn shortcut(&self, command: &str) -> Option<String> {
        self.keymap.key_labels(command).into_iter().next()
    }
}

/// The label of a command's primary keys (see [`Keybindings::shortcut`]).
pub fn shortcut(cx: &App, command: &str) -> Option<String> {
    cx.try_global::<Keybindings>()?.shortcut(command)
}

/// Installs the built-in bindings and the handlers of the app-level
/// commands. Called by [`crate::init`], after the widgets' own keys.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &actions::app::Quit, cx| cx.quit());
    cx.on_action(|_: &actions::app::OpenMenu, cx| crate::menu::open_in_last_window(cx));
    cx.on_action(|_: &actions::view::CycleTheme, cx| AppSettings::cycle_theme(cx));
    cx.on_action(|_: &actions::view::Preview, cx| show_mode(ViewMode::Preview, cx));
    cx.on_action(|_: &actions::view::Edit, cx| show_mode(ViewMode::Edit, cx));
    cx.on_action(|_: &actions::view::Split, cx| show_mode(ViewMode::Split, cx));
    apply(cx, &UserKeymap::empty(Platform::current()));
}

/// Shows the active tab of the window focused last in `mode`.
fn show_mode(mode: ViewMode, cx: &mut App) {
    if let Some(window) = most_recent_window(cx) {
        // Fails only when the window has closed.
        let _ = window
            .workspace
            .update(cx, |workspace, cx| workspace.show_mode(mode, cx));
    }
}

/// Reads the user's `keymap.toml`, applies it, and applies every later
/// change to it. Without locations (no home directory) the built-in
/// bindings stay. Call once at startup, after [`crate::init`].
pub fn install(cx: &mut App, locations: Option<&Locations>) {
    let Some(locations) = locations else {
        return;
    };
    let path = locations.keymap_file();
    load_file(cx, &path);
    watch::watch(path, read_only(cx), cx);
}

/// Safe mode writes nothing, so it does not upgrade an old keymap file.
fn read_only(cx: &App) -> bool {
    cx.try_global::<AppSettings>()
        .is_some_and(AppSettings::safe_mode)
}

/// Reads `keymap.toml` at `path` and applies it. A missing file has no
/// bindings; a file that cannot be read leaves the built-in bindings.
pub fn load_file(cx: &mut App, path: &Path) {
    let user = read(path, Platform::current(), read_only(cx));
    apply_read(cx, user);
}

fn apply_read(cx: &mut App, user: Result<UserKeymap, SettingsError>) {
    match user {
        Ok(user) => apply(cx, &user),
        Err(err) => {
            tracing::warn!("keymap: cannot read the keymap, so the built-in keys are used: {err}");
            apply(cx, &UserKeymap::empty(Platform::current()));
        }
    }
}

/// Reads a keymap file for `platform`. `read_only` parses it without the
/// upgrade [`UserKeymap::load`] may write.
fn read(path: &Path, platform: Platform, read_only: bool) -> Result<UserKeymap, SettingsError> {
    if !read_only {
        return UserKeymap::load(path, platform);
    }
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Ok(UserKeymap::parse(&text, Some(path), platform)),
            Err(_) => {
                tracing::warn!(
                    "keymap: {} is not valid UTF-8, so no binding in it is used",
                    path.display()
                );
                Ok(UserKeymap::empty(platform))
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(UserKeymap::empty(platform)),
        Err(source) => Err(SettingsError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Applies `user` on top of the built-in bindings: logs the diagnostics,
/// replaces the command bindings and refreshes the native menus.
pub fn apply(cx: &mut App, user: &UserKeymap) {
    let registry = CommandRegistry::builtin();
    let keymap = Keymap::build(&registry, user);
    for diagnostic in keymap.diagnostics() {
        match diagnostic.severity() {
            Severity::Info => tracing::info!("keymap: {diagnostic}"),
            Severity::Warning | Severity::Error => tracing::warn!("keymap: {diagnostic}"),
        }
    }
    let bindings = key_bindings(&binding_specs(&keymap), cx);
    replace_command_bindings(cx, bindings);
    cx.set_global(Keybindings { registry, keymap });
    crate::menu::refresh_native(cx);
}

/// Swaps the command bindings for `bindings`, keeping every other binding
/// (GPUI Component's, the widgets' own) in its place before them.
fn replace_command_bindings(cx: &mut App, bindings: Vec<KeyBinding>) {
    let others: Vec<KeyBinding> = cx
        .key_bindings()
        .borrow()
        .bindings()
        .filter(|b| b.meta() != Some(COMMAND_BINDING))
        .cloned()
        .collect();
    cx.clear_key_bindings();
    cx.bind_keys(others);
    cx.bind_keys(bindings);
}

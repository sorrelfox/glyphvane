// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Key bindings: commands, keystrokes, contexts, `keymap.toml`, conflict
//! checks, key resolution and cheatsheet data. Nothing here depends on the
//! UI toolkit; the UI converts [`Keystroke`]s field by field and asks
//! [`Keymap::resolve`] what a key press does.
//!
//! # Commands
//!
//! A [`Command`] has a stable [`CommandId`] (`view.toggle_mode`), a title, a
//! [`Category`], an optional context and default keys per [`Platform`].
//! [`CommandRegistry::builtin`] holds the app's commands.
//!
//! # Keys
//!
//! Keys use GPUI's syntax: modifiers joined to the key with `-`
//! (`ctrl-shift-p`), the keystrokes of a chord separated by a space
//! (`ctrl-k ctrl-t`). Modifiers are `ctrl`, `alt`, `shift`, `cmd` (also
//! `super` or `win`), `fn`, and `secondary`, which is Cmd on macOS and Ctrl
//! elsewhere. Keys are single characters (`a`, `,`, `[`, `-` as in
//! `ctrl--`) or names: `enter`, `escape`, `tab`, `backspace`, `delete`,
//! `insert`, `space`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`,
//! `pagedown`, `menu`, `f1`–`f35`. Chord timing is up to the UI.
//!
//! # Contexts
//!
//! A binding applies where its context predicate holds, such as
//! `editor && !autocomplete` ([`context`] has the grammar and
//! [`CONTEXTS`] the names).
//!
//! # `keymap.toml`
//!
//! ```toml
//! version = 1
//!
//! # Bind keys to a command. Without `context`, the binding applies where
//! # the command does; `context = ""` means everywhere.
//! [[bind]]
//! keys = "secondary-shift-k"
//! command = "editor.insert_link"
//!
//! [[bind]]
//! keys = "ctrl-k ctrl-t"
//! command = "view.cycle_theme"
//! context = "!editor && !autocomplete"
//!
//! # Arguments (any TOML value) are passed to the command as given.
//! [[bind]]
//! keys = "secondary-shift-1"
//! command = "editor.heading_1"
//! args = { toggle = true }
//!
//! # Remove built-in bindings: by keys, by command, or both; `context`
//! # narrows it to the built-in bindings with exactly that context.
//! [[unbind]]
//! keys = "f6"
//!
//! [[unbind]]
//! command = "workspace.command_palette"
//! keys = "secondary-shift-p"
//!
//! # Entries for one platform only.
//! [[macos.bind]]
//! keys = "cmd-shift-l"
//! command = "editor.toggle_task"
//! ```
//!
//! Rules:
//!
//! - User bindings win over built-in ones on the same keys. Then the more
//!   specific context wins ([`ContextExpr::specificity`]), then the later
//!   entry. Top-level entries come before the current platform's section.
//! - `unbind` entries apply to the built-in bindings only, before any
//!   `bind`; to drop one of your own bindings, delete it.
//! - Entries in the other platforms' sections are checked but not used.
//! - Problems never stop loading. Invalid entries are left out and reported
//!   with line and column ([`KeymapDiagnostic`]). Unknown commands, unknown
//!   context names and keys that type a character in the editor are
//!   warnings. Same-key conflicts within a layer, user bindings replacing
//!   built-in ones, and chords that are the start of longer chords are
//!   reported with both sources.
//! - The file carries `version = N` and is upgraded by the settings
//!   migration machinery ([`keymap_migrations`]), keeping a backup.

pub mod context;

mod bindings;
mod command;
mod defaults;
mod diagnostic;
mod file;
mod keystroke;

pub use bindings::{Binding, CheatsheetEntry, Keymap, Resolution};
pub use command::{
    Category, Command, CommandError, CommandId, CommandRegistry, DefaultKeys, InvalidCommandId,
    Platforms,
};
pub use context::{CONTEXTS, ContextExpr, ContextInfo, ContextParseError, ContextSet};
pub use defaults::builtin_commands;
pub use diagnostic::{BindingOrigin, KeymapDiagnostic, KeymapIssue};
pub use file::{
    EntryAction, EntryContext, KEYMAP_VERSION, UserEntry, UserKeymap, keymap_migrations,
};
pub use keystroke::{KeyParseError, KeySequence, Keystroke, Modifiers, NAMED_KEYS, Platform};

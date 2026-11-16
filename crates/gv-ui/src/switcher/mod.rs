// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The quick switcher (`workspace.quick_switcher`, Mod+O; FR-SEARCH-03).
//!
//! Drawn as SPEC § Popovers designs it: 540 wide, radius 10, 84 below the
//! top of the window and centred; an input row 40 high with the search
//! icon, the query in 14 px and the mode hints (`#` headings, `^` blocks)
//! at the right; rows 28 high with the title (matched letters in weight
//! 700) and the folder at the right in 11 px; a footer with key hints.
//! Alias rows read "Alias → Note" with the `corner-up-right` icon and
//! "alias" at the right; a note that does not exist yet has the `file-plus`
//! icon, reads "Name · not created yet" and says how many notes link to
//! it.
//!
//! Not designed, and built from the parts above: the unsaved dot after a
//! title (the tab's 7 px dot), the row offering to create the query when
//! nothing matches, heading and block rows (the note's title at the
//! right), and the empty states.
//!
//! # Sources
//!
//! - **With a vault:** every file of the content index with its title,
//!   one row per alias, and the notes that links point to but that do not
//!   exist yet ([`gv_vault::content::ContentSnapshot::switcher_entries`],
//!   `unresolved_targets`). Unsaved notes are listed too. While the content
//!   index is still building, the path index's files (names only).
//! - **Open tabs and recent files** outside the vault, and all of them
//!   when the window has no vault, so the switcher works without one.
//! - **`#` and `^` modes** list the headings and block ids of every note.
//! - Opening the switcher makes the window's vault (as showing the
//!   sidebar does), so the index starts building. While the switcher is
//!   open, every new snapshot of the content index refreshes the list,
//!   keeping the highlighted row.
//!
//! # Matching and ranking ([`model::rank`])
//!
//! nucleo fuzzy matching, word by word in any order, ignoring case and
//! (unless the query has some) accents, against the title or file name and
//! the vault-relative path, so `proj/read` finds `Projects/Readme.md`.
//! Title matches rank above path matches, a title equal to the query
//! first; open tabs and recent files are boosted; notes not created yet
//! come last. With no query: the open tabs and recent files, then every
//! file by path.
//!
//! # Keys
//!
//! - Up and Down move the highlight (wrapping), Esc or Mod+O closes, as
//!   does a click outside.
//! - **Enter** opens the highlighted file in the active tab (a tab that
//!   already shows it is activated instead); a heading or block opens at
//!   its line. On a note that does not exist yet, or when nothing matches,
//!   Enter creates it.
//! - **Mod+Enter** opens it in a new tab.
//! - **Shift+Enter** creates a note named after the query: in the vault's
//!   root (the default `newFileLocation`), or in the folders the query
//!   names (`Projects/Plan`); a name that exists already opens instead.
//!   The name loses the characters no file system or wikilink allows
//!   ([`model::new_note_path`]). Without a vault nothing is created.

pub mod model;
mod view;

use std::path::PathBuf;
use std::rc::Rc;

use gpui::base::Root;
use gpui::{AnyWindowHandle, App, Entity, KeyBinding, Window};
use gv_vault::content::ContentSnapshot;

pub use view::QuickSwitcher;

gpui::actions!(
    quick_switcher,
    [
        /// Highlight the previous row.
        SelectPrevious,
        /// Highlight the next row.
        SelectNext,
        /// Open the highlighted row.
        Confirm,
        /// Open the highlighted row in a new tab.
        ConfirmInNewTab,
        /// Create a note named after the query.
        CreateNote,
        /// Close the switcher.
        Cancel,
    ]
);

/// The switcher's key context: the model's `switcher` context.
pub const KEY_CONTEXT: &str = "QuickSwitcher";

/// Mod+Enter in GPUI's syntax.
const MOD_ENTER: &str = if cfg!(target_os = "macos") {
    "cmd-enter"
} else {
    "ctrl-enter"
};

/// The switcher's own keys, bound on its field as well, where the text
/// field's own keys would otherwise take them.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let field = "QuickSwitcher > Input";
    let mut keys = Vec::new();
    for context in [KEY_CONTEXT, field] {
        keys.extend([
            KeyBinding::new("up", SelectPrevious, Some(context)),
            KeyBinding::new("down", SelectNext, Some(context)),
            KeyBinding::new("enter", Confirm, Some(context)),
            KeyBinding::new(MOD_ENTER, ConfirmInNewTab, Some(context)),
            KeyBinding::new("shift-enter", CreateNote, Some(context)),
            KeyBinding::new("escape", Cancel, Some(context)),
        ]);
    }
    keys
}

/// Binds the switcher's keys and adds it to the window root. Called by
/// [`crate::init`], before any window opens.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
    Root::register_plugin::<QuickSwitcher>(cx, QuickSwitcher::new);
}

/// The switcher of `window`, if its root has one.
pub fn switcher_of(window: &Window, cx: &App) -> Option<Entity<QuickSwitcher>> {
    window
        .root::<Root>()
        .flatten()?
        .read(cx)
        .plugin::<QuickSwitcher>()
}

/// What the switcher lists, gathered by its window.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// Files, aliases, notes to create.
    pub entries: Vec<model::Entry>,
    /// The content index and the vault root, for the `#` and `^` modes.
    pub content: Option<(PathBuf, ContentSnapshot)>,
    /// Notes can be created: the window has a vault.
    pub can_create: bool,
}

/// What the user picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Open a file, at a 1-based line.
    Open {
        file: PathBuf,
        line: Option<u32>,
        new_tab: bool,
    },
    /// Create the note at this vault-relative path (or open it when it
    /// exists), then open it.
    Create { path: String, new_tab: bool },
}

/// Gathers the sources again (when the switcher opens and when the index
/// changes).
pub type SourcesFn = Rc<dyn Fn(&App) -> Sources>;
/// Hears the choice; the switcher has closed.
pub type ChoiceFn = Rc<dyn Fn(Choice, &mut Window, &mut App)>;

/// A switcher to open.
#[derive(Clone)]
pub struct SwitcherRequest {
    pub sources: SourcesFn,
    pub on_choice: ChoiceFn,
}

impl std::fmt::Debug for SwitcherRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwitcherRequest").finish_non_exhaustive()
    }
}

/// Opens the switcher of `window` for `request`, or closes it when it is
/// open (Mod+O twice). Returns whether the window has a switcher.
pub fn toggle(request: SwitcherRequest, window: &mut Window, cx: &mut App) -> bool {
    let Some(switcher) = switcher_of(window, cx) else {
        return false;
    };
    switcher.update(cx, |s, cx| {
        if s.is_open() {
            s.close(window, cx);
        } else {
            s.open(request, window, cx);
        }
    });
    true
}

/// The window's sources changed (a new content index snapshot): an open
/// switcher gathers them again.
pub fn refresh(window: AnyWindowHandle, cx: &mut App) {
    // Fails only when the window has closed.
    let _ = window.update(cx, |_, window, cx| {
        if let Some(switcher) = switcher_of(window, cx) {
            switcher.update(cx, |s, cx| s.refresh(cx));
        }
    });
}

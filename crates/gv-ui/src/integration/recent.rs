// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Recent documents for the operating system (FR-PLAT-05, plan 14 § Per-OS
//! integration), kept in step with the session's recent files and
//! folders:
//!
//! | | What | How |
//! |---|---|---|
//! | macOS | the Dock menu's and *Open Recent*'s recent items | `NSDocumentController noteNewRecentDocumentURL` for each newly opened file or folder (GPUI `add_recent_document`); the Dock menu also gets *New window* |
//! | Windows | the jump list's *Recent Folders* | `ICustomDestinationList` through GPUI `update_jump_list`, the ten most recent folders; each entry starts `glyphvane "<folder>"`, which the single instance opens |
//! | Linux | `recently-used.xbel` | **not built**: GPUI has no API, and the shared GTK file needs a careful writer |
//!
//! Windows' own *Recent* category of files (`SHAddToRecentDocs`) is not
//! reachable through GPUI either. GPUI's jump list names its category
//! "Recent Folders" itself, so recent files are not added there.

use std::path::PathBuf;

use gpui::{App, Global, MenuItem};

use crate::keymap::actions;
use crate::shell::session::{self, SessionStore};

/// Folders in the Windows jump list.
pub const JUMP_LIST_FOLDERS: usize = 10;

/// What was last handed to the system.
#[derive(Debug, Default)]
struct Synced {
    files: Vec<PathBuf>,
    folders: Vec<PathBuf>,
}

impl Global for Synced {}

/// Starts following the session's recent files and folders. Call once,
/// after the session store is installed.
pub fn install(cx: &mut App) {
    if cx.has_global::<Synced>() {
        return;
    }
    cx.set_global(Synced::default());
    if cfg!(target_os = "macos") {
        cx.set_dock_menu(vec![MenuItem::action(
            "New window",
            actions::workspace::NewWindow,
        )]);
    }
    sync(cx);
    cx.observe_global::<SessionStore>(sync).detach();
}

/// Hands the changes since the last call to the system.
fn sync(cx: &mut App) {
    let files = session::recent_files(cx);
    let folders = session::recent_folders(cx);
    let synced = cx.global::<Synced>();
    if synced.files == files && synced.folders == folders {
        return;
    }
    let mut added = newly_added(&synced.files, &files);
    added.extend(newly_added(&synced.folders, &folders));
    let folders_changed = synced.folders != folders;
    // Oldest first, so the most recent ends up on top.
    for path in added.iter().rev() {
        cx.add_recent_document(path);
    }
    if folders_changed && cfg!(target_os = "windows") {
        let entries = jump_list_entries(&folders)
            .into_iter()
            .map(|folder| vec![folder].into())
            .collect();
        // The answer lists entries the user removed from the jump list;
        // the next update leaves them out on its own.
        cx.update_jump_list(Vec::new(), entries).detach();
    }
    let synced = cx.global_mut::<Synced>();
    synced.files = files;
    synced.folders = folders;
}

/// The entries of `now` (most recent first) that were opened since
/// `before`: those above the entry that was on top before, most recent
/// first. Without it (a first call, a cleared list), all of `now`.
pub fn newly_added(before: &[PathBuf], now: &[PathBuf]) -> Vec<PathBuf> {
    let top = before
        .first()
        .and_then(|first| now.iter().position(|p| p == first))
        .unwrap_or(now.len());
    now[..top].to_vec()
}

/// The folders for the Windows jump list: the most recent first, those
/// still present.
pub fn jump_list_entries(folders: &[PathBuf]) -> Vec<PathBuf> {
    folders
        .iter()
        .filter(|f| f.is_dir())
        .take(JUMP_LIST_FOLDERS)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn new_and_moved_entries_are_found() {
        assert_eq!(newly_added(&[], &paths(&["a", "b"])), paths(&["a", "b"]));
        assert_eq!(
            newly_added(&paths(&["a", "b"]), &paths(&["c", "a", "b"])),
            paths(&["c"])
        );
        // `b` moved to the top.
        assert_eq!(
            newly_added(&paths(&["a", "b", "c"]), &paths(&["b", "a", "c"])),
            paths(&["b"])
        );
        // The oldest entry dropped off the end.
        assert_eq!(
            newly_added(&paths(&["a", "b"]), &paths(&["c", "a"])),
            paths(&["c"])
        );
        assert!(newly_added(&paths(&["a", "b"]), &paths(&["a", "b"])).is_empty());
        // A cleared list adds nothing.
        assert!(newly_added(&paths(&["a", "b"]), &[]).is_empty());
    }

    #[test]
    fn the_jump_list_keeps_existing_folders() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut folders: Vec<PathBuf> = (0..12)
            .map(|i| {
                let p = dir.path().join(format!("v{i}"));
                std::fs::create_dir(&p).expect("dir");
                p
            })
            .collect();
        folders.insert(1, dir.path().join("gone"));
        let entries = jump_list_entries(&folders);
        assert_eq!(entries.len(), JUMP_LIST_FOLDERS);
        assert_eq!(entries[0], dir.path().join("v0"));
        assert_eq!(entries[1], dir.path().join("v1"));
    }

    #[gpui::test]
    fn follows_the_session(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::init(
                cx,
                crate::AppSettings::new(None, Some(crate::ThemeChoice::Light)),
            );
            session::install(cx, SessionStore::new(session::Session::default(), None));
            install(cx);
        });
        cx.update(|cx| {
            session::note_recent_file(cx, std::path::Path::new("/notes/a.md"));
        });
        cx.run_until_parked();
        cx.update(|cx| {
            // Stored absolute: on Windows `/notes/a.md` gains the current drive.
            let expected = std::path::absolute("/notes/a.md").expect("absolute");
            assert_eq!(cx.global::<Synced>().files, [expected]);
        });
    }
}

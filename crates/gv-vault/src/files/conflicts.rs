// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Sync-conflict copies of one file, offered for comparison next to it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::index::conflict::{SyncService, detect_sync_conflict};

/// A conflict copy a sync client left next to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncConflictCopy {
    /// The copy.
    pub path: PathBuf,
    /// Whose naming pattern it matches.
    pub service: SyncService,
}

/// The sync-conflict copies of `file` in its directory (Dropbox, Syncthing,
/// OneDrive, iCloud patterns), sorted by path. Compare each with the file
/// through [`merge3`](super::merge3) or a two-way diff.
pub fn sync_conflict_copies(file: &Path) -> io::Result<Vec<SyncConflictCopy>> {
    let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
        return Ok(Vec::new());
    };
    let dir = match file.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let mut copies = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(candidate) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if candidate == name {
            continue;
        }
        let conflict = detect_sync_conflict(&candidate, |original| dir.join(original).is_file());
        if let Some(conflict) = conflict
            && conflict.original == name
        {
            copies.push(SyncConflictCopy {
                path: entry.path(),
                service: conflict.service,
            });
        }
    }
    copies.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(copies)
}

/// How a sync conflict is settled. Whatever is dropped goes to the trash,
/// so either choice can be undone from there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncChoice {
    /// Keep the original; the copy goes to the trash.
    KeepOriginal,
    /// The copy replaces the original: the original goes to the trash and
    /// the copy takes its name.
    UseCopy,
}

/// Why a sync conflict was not settled.
#[derive(Debug, thiserror::Error)]
pub enum SyncResolveError {
    /// The copy and the original are not in the same folder (the panel
    /// only offers siblings), or one of them is the vault root.
    #[error("“{0}” is not a conflict copy of “{1}”")]
    NotSiblings(crate::VaultPath, crate::VaultPath),
    /// The file is gone.
    #[error("“{0}” does not exist")]
    Missing(crate::VaultPath),
    /// Moving a file to the trash failed; nothing else changed.
    #[error("cannot move “{0}” to the trash: {1}")]
    Trash(crate::VaultPath, String),
    /// The copy could not take the original's name after the original went
    /// to the trash (restore it from there).
    #[error("“{path}” is in the trash, but the copy could not take its name: {source}")]
    Rename {
        /// The original.
        path: crate::VaultPath,
        /// The error.
        source: io::Error,
    },
}

/// Settles the conflict between `copy` and `original`, two files of the
/// vault at `root` in the same folder, as `choice` says. Dropped files go
/// to `trash`.
pub fn resolve_sync_conflict(
    root: &Path,
    copy: &crate::VaultPath,
    original: &crate::VaultPath,
    choice: SyncChoice,
    trash: &dyn crate::refactor::Trash,
) -> Result<(), SyncResolveError> {
    if copy.is_root()
        || original.is_root()
        || copy == original
        || copy.parent() != original.parent()
    {
        return Err(SyncResolveError::NotSiblings(
            copy.clone(),
            original.clone(),
        ));
    }
    let copy_fs = copy.to_fs_path(root);
    let original_fs = original.to_fs_path(root);
    for (path, fs_path) in [(copy, &copy_fs), (original, &original_fs)] {
        if !fs::symlink_metadata(fs_path).is_ok_and(|m| m.is_file()) {
            return Err(SyncResolveError::Missing(path.clone()));
        }
    }
    match choice {
        SyncChoice::KeepOriginal => trash
            .delete(&copy_fs)
            .map_err(|e| SyncResolveError::Trash(copy.clone(), e)),
        SyncChoice::UseCopy => {
            trash
                .delete(&original_fs)
                .map_err(|e| SyncResolveError::Trash(original.clone(), e))?;
            fs::rename(&copy_fs, &original_fs).map_err(|source| SyncResolveError::Rename {
                path: original.clone(),
                source,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VaultPath;
    use std::sync::Mutex;

    /// A trash that moves files into a folder of its own.
    struct FolderTrash(PathBuf, Mutex<Vec<PathBuf>>);

    impl crate::refactor::Trash for FolderTrash {
        fn delete(&self, path: &Path) -> Result<(), String> {
            let name = path.file_name().ok_or("no name")?;
            fs::rename(path, self.0.join(name)).map_err(|e| e.to_string())?;
            self.1
                .lock()
                .map_err(|e| e.to_string())?
                .push(path.to_path_buf());
            Ok(())
        }
    }

    fn vp(s: &str) -> VaultPath {
        VaultPath::new(s).expect("vault path")
    }

    #[test]
    fn keeping_the_original_trashes_the_copy() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = tempfile::tempdir().expect("trash dir");
        let trash = FolderTrash(bin.path().to_path_buf(), Mutex::default());
        fs::create_dir(dir.path().join("a")).expect("mkdir");
        fs::write(dir.path().join("a/Note.md"), "mine").expect("write");
        let copy = "a/Note (conflicted copy 2026-01-02).md";
        fs::write(dir.path().join(copy), "theirs").expect("write");
        resolve_sync_conflict(
            dir.path(),
            &vp(copy),
            &vp("a/Note.md"),
            SyncChoice::KeepOriginal,
            &trash,
        )
        .expect("resolved");
        assert!(!dir.path().join(copy).exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("a/Note.md"))
                .ok()
                .as_deref(),
            Some("mine")
        );
        assert!(
            bin.path()
                .join("Note (conflicted copy 2026-01-02).md")
                .exists()
        );
    }

    #[test]
    fn using_the_copy_trashes_the_original_and_renames_the_copy() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = tempfile::tempdir().expect("trash dir");
        let trash = FolderTrash(bin.path().to_path_buf(), Mutex::default());
        fs::write(dir.path().join("Note.md"), "mine").expect("write");
        let copy = "Note.sync-conflict-20260102-101010-ABCDEFG.md";
        fs::write(dir.path().join(copy), "theirs").expect("write");
        resolve_sync_conflict(
            dir.path(),
            &vp(copy),
            &vp("Note.md"),
            SyncChoice::UseCopy,
            &trash,
        )
        .expect("resolved");
        assert!(!dir.path().join(copy).exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("Note.md"))
                .ok()
                .as_deref(),
            Some("theirs")
        );
        assert_eq!(
            fs::read_to_string(bin.path().join("Note.md"))
                .ok()
                .as_deref(),
            Some("mine")
        );
    }

    #[test]
    fn refuses_files_in_other_folders_and_missing_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = tempfile::tempdir().expect("trash dir");
        let trash = FolderTrash(bin.path().to_path_buf(), Mutex::default());
        fs::write(dir.path().join("Note.md"), "mine").expect("write");
        let r = resolve_sync_conflict(
            dir.path(),
            &vp("b/Note 2.md"),
            &vp("Note.md"),
            SyncChoice::KeepOriginal,
            &trash,
        );
        assert!(matches!(r, Err(SyncResolveError::NotSiblings(..))));
        let r = resolve_sync_conflict(
            dir.path(),
            &vp("Note 2.md"),
            &vp("Note.md"),
            SyncChoice::UseCopy,
            &trash,
        );
        assert!(matches!(r, Err(SyncResolveError::Missing(p)) if p.as_str() == "Note 2.md"));
        assert!(dir.path().join("Note.md").exists());
    }

    #[test]
    fn finds_copies_of_this_file_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        let note = dir.path().join("Note.md");
        for name in [
            "Note.md",
            "Note (conflicted copy 2026-01-02).md",
            "Note.sync-conflict-20260102-101010-ABCDEFG.md",
            "Other (conflicted copy 2026-01-02).md",
            "Note 2.md",
            "Unrelated.md",
        ] {
            fs::write(dir.path().join(name), "x").expect("write");
        }
        let copies = sync_conflict_copies(&note).expect("list");
        let found: Vec<(String, SyncService)> = copies
            .iter()
            .map(|c| {
                (
                    c.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    c.service,
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    "Note (conflicted copy 2026-01-02).md".to_owned(),
                    SyncService::Dropbox
                ),
                ("Note 2.md".to_owned(), SyncService::ICloud),
                (
                    "Note.sync-conflict-20260102-101010-ABCDEFG.md".to_owned(),
                    SyncService::Syncthing
                ),
            ]
        );
    }
}

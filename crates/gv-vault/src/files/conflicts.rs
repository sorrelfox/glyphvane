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

#[cfg(test)]
mod tests {
    use super::*;

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

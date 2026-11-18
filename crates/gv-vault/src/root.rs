// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Vault root detection: which folder a file belongs to, and which folders
//! are never indexed without asking.
//!
//! The markers of other site generators (MkDocs, Docusaurus, Quarto, ...)
//! come with their profiles.

use std::fmt;
use std::path::{Path, PathBuf};

/// What made a folder the root of a vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootMarker {
    /// `.obsidian/`: an Obsidian vault.
    Obsidian,
    /// `.glyphvane/`, or `.glyphvane.toml` with `root = true`.
    Glyphvane,
    /// `.git` (a folder, or a file in a worktree): the repository root.
    Git,
}

/// Why a folder was chosen as the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootReason {
    /// The nearest ancestor holding this marker.
    Marker(RootMarker),
    /// No ancestor has a marker: the file's own folder.
    FileFolder,
}

impl RootReason {
    /// The marker, if a marker decided the root.
    pub fn marker(self) -> Option<RootMarker> {
        match self {
            Self::Marker(marker) => Some(marker),
            Self::FileFolder => None,
        }
    }
}

/// The root a file belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootDetection {
    /// Absolute folder.
    pub root: PathBuf,
    pub reason: RootReason,
}

/// The vault root of `file`: the nearest ancestor folder holding a root
/// marker, or the file's own folder.
///
/// Relative paths are made absolute against the current directory first.
pub fn detect_root(file: &Path) -> RootDetection {
    let file = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let folder = file.parent().unwrap_or(&file).to_path_buf();
    for dir in folder.ancestors() {
        if let Some(marker) = marker_in(dir) {
            return RootDetection {
                root: dir.to_path_buf(),
                reason: RootReason::Marker(marker),
            };
        }
    }
    RootDetection {
        root: folder,
        reason: RootReason::FileFolder,
    }
}

/// The root marker directly inside `dir`, if any.
pub fn marker_in(dir: &Path) -> Option<RootMarker> {
    if dir.join(".obsidian").is_dir() {
        Some(RootMarker::Obsidian)
    } else if dir.join(".glyphvane").is_dir() || glyphvane_toml_is_root(dir) {
        Some(RootMarker::Glyphvane)
    } else if dir.join(".git").exists() {
        Some(RootMarker::Git)
    } else {
        None
    }
}

/// `dir/.glyphvane.toml` exists, parses, and says `root = true`.
fn glyphvane_toml_is_root(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(".glyphvane.toml")) else {
        return false;
    };
    text.parse::<toml::Table>()
        .ok()
        .and_then(|table| table.get("root").and_then(toml::Value::as_bool))
        .unwrap_or(false)
}

/// Why a root is not indexed without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusedRoot {
    /// `/`, a drive root, or another path without a parent.
    FilesystemRoot,
    /// The user's home folder.
    Home,
}

impl RefusedRoot {
    /// A short lower-case description: "filesystem root", "home folder".
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemRoot => "filesystem root",
            Self::Home => "home folder",
        }
    }
}

impl fmt::Display for RefusedRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether `root` is never indexed automatically: a filesystem root, or
/// `home` (pass absolute paths). The comparison ignores letter case where
/// the platform's file names usually do (Windows, macOS) and is exact on
/// other systems.
pub fn is_refused_root(root: &Path, home: Option<&Path>) -> Option<RefusedRoot> {
    if root.parent().is_none() {
        return Some(RefusedRoot::FilesystemRoot);
    }
    if home.is_some_and(|home| same_path(root, home, PATHS_IGNORE_CASE)) {
        return Some(RefusedRoot::Home);
    }
    None
}

/// Whether file names on this platform usually ignore letter case: NTFS on
/// Windows and the default APFS volume on macOS.
const PATHS_IGNORE_CASE: bool = cfg!(any(windows, target_os = "macos"));

/// Whether `a` and `b` name the same path, component by component (so a
/// trailing separator does not matter), ignoring letter case when
/// `ignore_case` is set.
fn same_path(a: &Path, b: &Path, ignore_case: bool) -> bool {
    if !ignore_case {
        return a == b;
    }
    let mut a = a.components();
    let mut b = b.components();
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) => {
                let (x, y) = (
                    x.as_os_str().to_string_lossy(),
                    y.as_os_str().to_string_lossy(),
                );
                if x != y && x.to_lowercase() != y.to_lowercase() {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

/// The current user's home folder, made absolute.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().and_then(|h| std::path::absolute(h).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_detection_picks_the_nearest_marker() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).expect("git");
        std::fs::create_dir_all(root.join("vault/.obsidian")).expect("obsidian");
        std::fs::create_dir_all(root.join("vault/notes")).expect("notes");
        std::fs::create_dir_all(root.join("docs")).expect("docs");
        std::fs::create_dir_all(root.join("site")).expect("site");
        std::fs::write(root.join("site/.glyphvane.toml"), "root = true\n").expect("toml");

        let found = detect_root(&root.join("vault/notes/a.md"));
        assert_eq!(found.root, root.join("vault"));
        assert_eq!(found.reason, RootReason::Marker(RootMarker::Obsidian));
        let found = detect_root(&root.join("docs/a.md"));
        assert_eq!(
            (found.root, found.reason),
            (root.to_path_buf(), RootReason::Marker(RootMarker::Git))
        );
        let found = detect_root(&root.join("site/a.md"));
        assert_eq!(found.root, root.join("site"));
        assert_eq!(found.reason.marker(), Some(RootMarker::Glyphvane));
    }

    #[test]
    fn glyphvane_folder_and_git_worktree_file_are_markers() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/.glyphvane")).expect("glyphvane");
        std::fs::create_dir_all(root.join("wt/sub")).expect("worktree");
        std::fs::write(root.join("wt/.git"), "gitdir: /elsewhere\n").expect("git file");

        assert_eq!(marker_in(&root.join("a")), Some(RootMarker::Glyphvane));
        let found = detect_root(&root.join("wt/sub/a.md"));
        assert_eq!(found.root, root.join("wt"));
        assert_eq!(found.reason, RootReason::Marker(RootMarker::Git));
    }

    #[test]
    fn glyphvane_toml_needs_root_true() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        for (name, text) in [
            ("false", "root = false\n"),
            ("missing", "theme = \"dark\"\n"),
            ("string", "root = \"true\"\n"),
            ("broken", "root = = true\n"),
        ] {
            std::fs::create_dir_all(root.join(name)).expect("dir");
            std::fs::write(root.join(name).join(".glyphvane.toml"), text).expect("toml");
            assert_eq!(marker_in(&root.join(name)), None, "{name}");
        }
    }

    #[test]
    fn without_a_marker_the_root_is_the_file_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        let folder = dir.path().join("plain");
        std::fs::create_dir_all(&folder).expect("dir");
        let found = detect_root(&folder.join("a.md"));
        // An ancestor of the temp dir could itself carry a marker.
        if found.reason == RootReason::FileFolder {
            assert_eq!(found.root, folder);
        } else {
            assert!(folder.starts_with(&found.root));
        }
    }

    #[test]
    fn the_home_folder_and_filesystem_root_are_refused() {
        let home = tempfile::tempdir().expect("temp dir");
        let home = home.path();
        assert_eq!(
            is_refused_root(Path::new("/"), Some(home)),
            Some(RefusedRoot::FilesystemRoot)
        );
        assert_eq!(is_refused_root(home, Some(home)), Some(RefusedRoot::Home));
        assert_eq!(is_refused_root(&home.join("notes"), Some(home)), None);
        assert_eq!(is_refused_root(&home.join("notes"), None), None);
        assert_eq!(RefusedRoot::Home.to_string(), "home folder");
        assert_eq!(RefusedRoot::FilesystemRoot.to_string(), "filesystem root");
    }

    #[test]
    fn path_comparison_ignores_case_only_when_asked() {
        let home = Path::new("/Users/Ann");
        assert!(same_path(Path::new("/users/ann"), home, true));
        assert!(same_path(Path::new("/USERS/ANN/"), home, true));
        assert!(!same_path(Path::new("/users/ann"), home, false));
        assert!(same_path(Path::new("/Users/Ann/"), home, false));
        assert!(!same_path(Path::new("/users/anna"), home, true));
        assert!(!same_path(Path::new("/users/ann/notes"), home, true));
        assert!(same_path(
            Path::new("/Users/Änne"),
            Path::new("/users/änne"),
            true
        ));
    }

    #[test]
    fn the_current_home_is_refused() {
        if let Some(home) = home_dir() {
            assert!(is_refused_root(&home, Some(&home)).is_some());
            assert!(is_refused_root(&home.join("notes"), Some(&home)).is_none());
        }
    }
}

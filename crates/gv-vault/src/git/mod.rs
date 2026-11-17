// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Git awareness (FR-VAULT-12): the branch of the work tree a vault is in
//! and the status of its files, for the status bar and the Files panel's
//! badges. Read only: nothing here writes to the repository.
//!
//! - [`read_status`] reads one [`GitStatus`] snapshot with gix (pure
//!   Rust): the branch (or the detached commit), and every file that is
//!   modified, added, untracked, ignored or in conflict, staged or not.
//!   Untracked and ignored folders are reported once, as folders. Only
//!   the vault's folder is walked when the vault is a subfolder of the
//!   work tree; paths are vault paths.
//! - [`GitWatcher`] keeps a snapshot current on a background thread: it
//!   reads at start, again after [`GitWatcher::refresh`] (the window calls
//!   it for each vault watcher event) and after changes in the
//!   repository's own folder (`HEAD`, the index, refs: a commit, a
//!   checkout, a stage), each burst debounced, and reports
//!   [`GitEvent::Updated`] when the snapshot changed.
//!
//! A vault outside any work tree has no Git status: [`read_status`]
//! returns `None` and [`GitWatcher::start`] does not start.

mod watcher;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gix::bstr::{BString, ByteSlice};
use gix::status::index_worktree;

pub use watcher::{GitEvent, GitWatchOptions, GitWatcher};

use crate::path::VaultPath;

/// The status of one file (or of a whole untracked or ignored folder).
/// Ordered by importance: when a folder sums up its files, the first wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileStatus {
    /// In conflict (a merge or rebase stopped on it).
    Conflicted,
    /// Changed since the last commit, staged or not.
    Modified,
    /// New and staged.
    Added,
    /// Renamed (staged).
    Renamed,
    /// Removed from the work tree or the index.
    Deleted,
    /// Not tracked and not ignored.
    Untracked,
    /// Ignored by `.gitignore` or the repository's excludes.
    Ignored,
}

impl FileStatus {
    /// The badge letter: `M`, `A`, `R`, `D`, `U` (untracked), `C`
    /// (conflict); `None` for ignored files, which are dimmed instead.
    #[must_use]
    pub fn badge(self) -> Option<char> {
        match self {
            Self::Conflicted => Some('C'),
            Self::Modified => Some('M'),
            Self::Added => Some('A'),
            Self::Renamed => Some('R'),
            Self::Deleted => Some('D'),
            Self::Untracked => Some('U'),
            Self::Ignored => None,
        }
    }

    /// For people: "Modified", "Untracked", …
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Conflicted => "Conflict",
            Self::Modified => "Modified",
            Self::Added => "Added",
            Self::Renamed => "Renamed",
            Self::Deleted => "Deleted",
            Self::Untracked => "Untracked",
            Self::Ignored => "Ignored",
        }
    }
}

/// Where `HEAD` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Branch {
    /// On a branch (its short name, `main`, `feature/x`).
    Named(String),
    /// On a branch with no commit yet.
    Unborn(String),
    /// On a commit: its abbreviated id.
    Detached(String),
}

impl Branch {
    /// What the status bar shows: the branch name, or the commit id.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Named(name) | Self::Unborn(name) | Self::Detached(name) => name,
        }
    }
}

/// One reading of a vault's Git status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatus {
    /// The work tree's top folder.
    pub work_tree: PathBuf,
    /// The repository folder (`.git`, or a linked work tree's own).
    pub git_dir: PathBuf,
    pub branch: Branch,
    /// Files with a status, by vault path.
    files: BTreeMap<VaultPath, FileStatus>,
    /// Untracked or ignored folders reported as a whole, by vault path.
    folders: BTreeMap<VaultPath, FileStatus>,
}

impl GitStatus {
    /// The status of the file `path`: its own, or that of the untracked or
    /// ignored folder it is in. `None` for an unchanged tracked file.
    #[must_use]
    pub fn file(&self, path: &VaultPath) -> Option<FileStatus> {
        if let Some(status) = self.files.get(path) {
            return Some(*status);
        }
        self.folders
            .iter()
            .filter(|(dir, _)| path.starts_with(dir))
            .map(|(_, s)| *s)
            .min()
    }

    /// The most important status of anything in the folder `dir` (the
    /// root for the whole vault), for a folder badge. An untracked or
    /// ignored folder has its own status.
    #[must_use]
    pub fn folder(&self, dir: &VaultPath) -> Option<FileStatus> {
        let own = self
            .folders
            .iter()
            .filter(|(d, _)| dir.starts_with(d))
            .map(|(_, s)| *s)
            .min();
        let inside = self
            .files
            .iter()
            .chain(&self.folders)
            .filter(|(p, s)| p.starts_with(dir) && *p != dir && **s != FileStatus::Ignored)
            .map(|(_, s)| *s)
            .min();
        match (own, inside) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Every file and folder with a status, files first, each in path
    /// order.
    pub fn entries(&self) -> impl Iterator<Item = (&VaultPath, FileStatus)> {
        self.files.iter().chain(&self.folders).map(|(p, s)| (p, *s))
    }

    /// The number of files (and whole folders) that are changed, added,
    /// untracked or in conflict: not counting ignored ones.
    #[must_use]
    pub fn changed_count(&self) -> usize {
        self.entries()
            .filter(|(_, s)| *s != FileStatus::Ignored)
            .count()
    }
}

/// Reading the status failed (a damaged repository, an unreadable index).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot read the Git status: {0}")]
pub struct GitError(pub String);

fn err(e: impl std::fmt::Display) -> GitError {
    GitError(e.to_string())
}

/// The work tree `vault_root` is in, and the vault's folder inside it
/// (`""` when the vault is the whole work tree). `None` outside any work
/// tree or for a bare repository. Cheap: no status is read.
#[must_use]
pub fn find_work_tree(vault_root: &Path) -> Option<(PathBuf, String)> {
    let repo = gix::discover(vault_root).ok()?;
    let work_tree = repo.workdir()?.to_path_buf();
    let prefix = vault_prefix(&work_tree, vault_root)?;
    Some((work_tree, prefix))
}

/// The vault's folder relative to the work tree, with `/`, ending in `/`
/// unless empty.
fn vault_prefix(work_tree: &Path, vault_root: &Path) -> Option<String> {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let rel = canon(vault_root)
        .strip_prefix(canon(work_tree))
        .ok()?
        .to_path_buf();
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(if parts.is_empty() {
        String::new()
    } else {
        format!("{}/", parts.join("/"))
    })
}

/// Reads the Git status of the vault at `vault_root`. `Ok(None)` when it
/// is not in a Git work tree. Stops early (with an error) when `cancel` is
/// set.
pub fn read_status(
    vault_root: &Path,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<GitStatus>, GitError> {
    let Ok(repo) = gix::discover(vault_root) else {
        return Ok(None);
    };
    let Some(work_tree) = repo.workdir().map(Path::to_path_buf) else {
        return Ok(None);
    };
    let Some(prefix) = vault_prefix(&work_tree, vault_root) else {
        return Ok(None);
    };
    let branch = branch_of(&repo)?;
    let mut files: BTreeMap<VaultPath, FileStatus> = BTreeMap::new();
    let mut folders: BTreeMap<VaultPath, FileStatus> = BTreeMap::new();
    let patterns: Vec<BString> = if prefix.is_empty() {
        Vec::new()
    } else {
        vec![BString::from(prefix.as_str())]
    };
    let iter = repo
        .status(gix::progress::Discard)
        .map_err(err)?
        .untracked_files(gix::status::UntrackedFiles::Collapsed)
        .index_worktree_submodules(None)
        .index_worktree_rewrites(None)
        .should_interrupt_owned(cancel.clone())
        .dirwalk_options(|o| o.emit_ignored(Some(gix::dir::walk::EmissionMode::CollapseDirectory)))
        .into_iter(patterns)
        .map_err(err)?;
    for item in iter {
        let item = item.map_err(err)?;
        let Some((path, status, is_dir)) = classify(&item) else {
            continue;
        };
        let Some(rest) = path.to_str().ok().and_then(|p| p.strip_prefix(&prefix)) else {
            continue;
        };
        let Ok(vp) = VaultPath::new(rest.trim_end_matches('/')) else {
            continue;
        };
        if vp.is_root() {
            continue;
        }
        let map = if is_dir { &mut folders } else { &mut files };
        map.entry(vp)
            .and_modify(|s| *s = (*s).min(status))
            .or_insert(status);
    }
    Ok(Some(GitStatus {
        work_tree,
        git_dir: repo.git_dir().to_path_buf(),
        branch,
        files,
        folders,
    }))
}

/// Where `HEAD` is.
fn branch_of(repo: &gix::Repository) -> Result<Branch, GitError> {
    let head = repo.head().map_err(err)?;
    Ok(match head.kind {
        gix::head::Kind::Symbolic(r) => Branch::Named(r.name.shorten().to_string()),
        gix::head::Kind::Unborn(name) => Branch::Unborn(name.shorten().to_string()),
        gix::head::Kind::Detached { target, .. } => {
            let hex = target.to_hex().to_string();
            Branch::Detached(hex.chars().take(7).collect())
        }
    })
}

/// The repository path of a status item, its status and whether it is a
/// whole folder. `None` for items that only refresh the index's cache.
fn classify(item: &gix::status::Item) -> Option<(&gix::bstr::BStr, FileStatus, bool)> {
    use gix::status::Item;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
    match item {
        Item::TreeIndex(change) => {
            use gix::diff::index::ChangeRef;
            let status = match change {
                ChangeRef::Addition { .. } => FileStatus::Added,
                ChangeRef::Deletion { .. } => FileStatus::Deleted,
                ChangeRef::Modification { .. } => FileStatus::Modified,
                ChangeRef::Rewrite { .. } => FileStatus::Renamed,
            };
            Some((change.location(), status, false))
        }
        Item::IndexWorktree(item) => match item {
            index_worktree::Item::Modification {
                rela_path, status, ..
            } => {
                let status = match status {
                    EntryStatus::Conflict { .. } => FileStatus::Conflicted,
                    EntryStatus::Change(Change::Removed) => FileStatus::Deleted,
                    EntryStatus::Change(_) => FileStatus::Modified,
                    EntryStatus::IntentToAdd => FileStatus::Added,
                    EntryStatus::NeedsUpdate(_) => return None,
                };
                Some((rela_path.as_ref(), status, false))
            }
            index_worktree::Item::DirectoryContents { entry, .. } => {
                let status = match entry.status {
                    gix::dir::entry::Status::Untracked => FileStatus::Untracked,
                    gix::dir::entry::Status::Ignored(_) => FileStatus::Ignored,
                    gix::dir::entry::Status::Tracked | gix::dir::entry::Status::Pruned => {
                        return None;
                    }
                };
                let is_dir = entry
                    .disk_kind
                    .is_some_and(|k| k == gix::dir::entry::Kind::Directory);
                Some((entry.rela_path.as_ref(), status, is_dir))
            }
            index_worktree::Item::Rewrite { dirwalk_entry, .. } => {
                Some((dirwalk_entry.rela_path.as_ref(), FileStatus::Renamed, false))
            }
        },
    }
}

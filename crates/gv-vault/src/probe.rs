// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Direct existence checks while the path index is still building.
//!
//! Until the walk finishes, the resolver can still decide links whose
//! target exists at an exact path, by asking a [`PathProbe`] (a `stat`).
//! Anything that needs NFC or case-fold matching, or a basename lookup,
//! stays `Pending`.

use std::fs;
use std::path::PathBuf;

use crate::path::VaultPath;
use crate::platform::PlatformRules;

/// The answer of a [`PathProbe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeResult {
    /// No entry with exactly this name.
    Missing,
    /// A regular file with exactly this name.
    File,
    /// A directory with exactly this name.
    Dir,
    /// Cannot tell (no probe, a symlink, an error).
    Unknown,
}

/// Exact-name existence checks, relative to the vault root.
pub trait PathProbe {
    /// Whether an entry with **exactly** this path exists. On
    /// case-insensitive file systems an implementation must not report a
    /// case variant as a hit.
    fn probe(&self, path: &VaultPath) -> ProbeResult;
}

/// The probe that knows nothing: every miss in a building index is
/// `Pending`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProbe;

impl PathProbe for NoProbe {
    fn probe(&self, _: &VaultPath) -> ProbeResult {
        ProbeResult::Unknown
    }
}

/// A probe backed by the file system (`symlink_metadata`).
#[derive(Debug, Clone)]
pub struct FsProbe {
    root: PathBuf,
    rules: PlatformRules,
}

impl FsProbe {
    /// Probes below `root` under the naming `rules` of its file system.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, rules: PlatformRules) -> Self {
        Self {
            root: root.into(),
            rules,
        }
    }
}

impl PathProbe for FsProbe {
    fn probe(&self, path: &VaultPath) -> ProbeResult {
        // Never touch names the platform would interpret specially
        // (reserved devices, `:` alternate data streams).
        if path
            .segments()
            .any(|s| !self.rules.name_issues(s).is_empty())
        {
            return ProbeResult::Missing;
        }
        let fs_path = path.to_fs_path(&self.root);
        let Ok(meta) = fs::symlink_metadata(&fs_path) else {
            return ProbeResult::Missing;
        };
        if meta.file_type().is_symlink() {
            return ProbeResult::Unknown;
        }
        if !self.rules.is_exact() {
            // A case-insensitive file system also finds case variants:
            // confirm the exact name in the parent's listing.
            let (Some(name), Some(parent)) = (path.file_name(), fs_path.parent()) else {
                return ProbeResult::Unknown;
            };
            let Ok(dir) = fs::read_dir(parent) else {
                return ProbeResult::Unknown;
            };
            if !dir.flatten().any(|e| e.file_name().to_str() == Some(name)) {
                return ProbeResult::Missing;
            }
        }
        if meta.is_dir() {
            ProbeResult::Dir
        } else {
            ProbeResult::File
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What a file looked like on disk, and whether it changed since.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

use super::hash::ContentHash;

/// The identity of a file independent of its name: device and inode on Unix.
/// A save by rename gives the file a new identity; an in-place write keeps
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileIdentity {
    /// The device.
    pub device: u64,
    /// The inode.
    pub inode: u64,
}

impl FileIdentity {
    /// The identity in `meta`. `None` where the platform does not expose one
    /// through the standard library (Windows).
    pub fn of(meta: &fs::Metadata) -> Option<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                device: meta.dev(),
                inode: meta.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            None
        }
    }
}

/// A file's state on disk: the baseline that saves and change checks
/// compare against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskState {
    /// Length in bytes.
    pub len: u64,
    /// Last modification time, where the file system has one.
    pub modified: Option<SystemTime>,
    /// Device and inode (Unix).
    pub identity: Option<FileIdentity>,
    /// Whether the file is read-only for this process.
    pub read_only: bool,
    /// The hash of the bytes.
    pub hash: ContentHash,
}

impl DiskState {
    /// The state of a file with metadata `meta` and contents `bytes`.
    /// `read_only` is taken from the metadata; [`DiskState::read`] checks
    /// real write access instead.
    pub fn from_parts(meta: &fs::Metadata, bytes: &[u8]) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            identity: FileIdentity::of(meta),
            read_only: meta.permissions().readonly(),
            hash: ContentHash::of(bytes),
        }
    }

    /// Reads the file at `path` (following symlinks) and returns its bytes
    /// and state.
    ///
    /// Fails with [`io::ErrorKind::NotFound`] when the file vanishes at any
    /// step, including between the read and the write-access check, so a
    /// file moved away is never reported as read-only.
    pub fn read(path: &Path) -> io::Result<(Vec<u8>, Self)> {
        Self::read_with(path, write_access)
    }

    /// [`DiskState::read`] with the write-access check supplied, so tests can
    /// change the file between the steps.
    fn read_with(
        path: &Path,
        access: impl FnOnce(&Path, &fs::Metadata) -> io::Result<bool>,
    ) -> io::Result<(Vec<u8>, Self)> {
        let mut file = fs::File::open(path)?;
        let meta = file.metadata()?;
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        file.read_to_end(&mut bytes)?;
        let mut state = Self::from_parts(&meta, &bytes);
        // The handle's metadata describes the bytes just read even if the
        // file was replaced meanwhile; only the length can be trusted from
        // the read itself.
        state.len = bytes.len() as u64;
        state.read_only = !access(path, &meta)?;
        Ok((bytes, state))
    }

    /// Whether `meta` matches this state's length, modification time,
    /// identity and write access. A match is taken as "unchanged" by
    /// [`CheckMode::Quick`].
    fn stat_matches(&self, meta: &fs::Metadata, writable: bool) -> bool {
        self.len == meta.len()
            && self.modified == meta.modified().ok()
            && self.identity == FileIdentity::of(meta)
            && self.read_only == !writable
    }
}

/// Whether this process may write the file: `access(W_OK)` on Unix (so
/// group, ACL and read-only mounts count), the read-only attribute
/// elsewhere.
pub(crate) fn is_writable(path: &Path, meta: &fs::Metadata) -> bool {
    write_access(path, meta).unwrap_or(false)
}

/// Like [`is_writable`], but a file that no longer exists is an
/// [`io::ErrorKind::NotFound`] error rather than "not writable". Any other
/// failure of the check counts as not writable.
fn write_access(path: &Path, meta: &fs::Metadata) -> io::Result<bool> {
    #[cfg(unix)]
    {
        let _ = meta;
        match rustix::fs::access(path, rustix::fs::Access::WRITE_OK) {
            Ok(()) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Err(io::ErrorKind::NotFound.into()),
            Err(_) => Ok(false),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(!meta.permissions().readonly())
    }
}

/// How thoroughly [`check_external_change`] looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckMode {
    /// Compare length, modification time, identity and write access; read
    /// and hash the file only when they differ. Cheap, for frequent checks.
    /// Misses a same-length rewrite within the file system's timestamp
    /// granularity.
    Quick,
    /// Always read and hash the file. For focus changes, saves and network
    /// file systems (whose timestamps are coarse or skewed).
    Thorough,
}

/// What happened to a file since its baseline was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalChange {
    /// Nothing changed.
    Unchanged,
    /// Same bytes, but the metadata moved: a `touch`, a sync client
    /// rewriting identical contents, or a permission change (compare
    /// [`DiskState::read_only`] with the baseline). The new state should
    /// become the baseline.
    Touched(DiskState),
    /// The bytes changed. They are returned so a reload or merge works on
    /// exactly what was hashed.
    Modified {
        /// The new state.
        state: DiskState,
        /// The new contents.
        bytes: Vec<u8>,
    },
    /// The file no longer exists at that path (deleted, or moved away).
    Deleted,
}

impl ExternalChange {
    /// Whether the file's bytes differ from the baseline (modified or
    /// deleted).
    pub fn is_content_change(&self) -> bool {
        matches!(self, Self::Modified { .. } | Self::Deleted)
    }
}

/// Compares the file at `path` with `baseline`.
pub fn check_external_change(
    path: &Path,
    baseline: &DiskState,
    mode: CheckMode,
) -> io::Result<ExternalChange> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ExternalChange::Deleted),
        Err(e) => return Err(e),
    };
    if mode == CheckMode::Quick {
        let writable = match write_access(path, &meta) {
            Ok(writable) => writable,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ExternalChange::Deleted),
            Err(e) => return Err(e),
        };
        if baseline.stat_matches(&meta, writable) {
            return Ok(ExternalChange::Unchanged);
        }
    }
    let (bytes, state) = match DiskState::read(path) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ExternalChange::Deleted),
        Err(e) => return Err(e),
    };
    Ok(if state.hash != baseline.hash {
        ExternalChange::Modified { state, bytes }
    } else if state == *baseline {
        ExternalChange::Unchanged
    } else {
        ExternalChange::Touched(state)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_modification_touch_and_deletion() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("note.md");
        fs::write(&path, "one\n").expect("write");
        let (_, baseline) = DiskState::read(&path).expect("read");
        assert_eq!(
            check_external_change(&path, &baseline, CheckMode::Thorough).expect("check"),
            ExternalChange::Unchanged
        );
        assert_eq!(
            check_external_change(&path, &baseline, CheckMode::Quick).expect("check"),
            ExternalChange::Unchanged
        );

        // Same bytes, new modification time.
        let later = baseline
            .modified
            .map(|t| t + std::time::Duration::from_secs(5));
        if let Some(later) = later {
            let file = fs::File::options().write(true).open(&path).expect("open");
            file.set_modified(later).expect("set mtime");
            drop(file);
            match check_external_change(&path, &baseline, CheckMode::Quick).expect("check") {
                ExternalChange::Touched(state) => assert_eq!(state.hash, baseline.hash),
                other => panic!("expected Touched, got {other:?}"),
            }
        }

        fs::write(&path, "two\n").expect("write");
        match check_external_change(&path, &baseline, CheckMode::Thorough).expect("check") {
            ExternalChange::Modified { bytes, state } => {
                assert_eq!(bytes, b"two\n");
                assert_eq!(state.hash, ContentHash::of(b"two\n"));
            }
            other => panic!("expected Modified, got {other:?}"),
        }

        fs::remove_file(&path).expect("remove");
        assert_eq!(
            check_external_change(&path, &baseline, CheckMode::Quick).expect("check"),
            ExternalChange::Deleted
        );
    }

    // Only Unix asks the file system for write access by path.
    #[cfg(unix)]
    #[test]
    fn file_vanishing_before_access_check_is_not_found() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("note.md");
        let moved = dir.path().join("moved.md");
        fs::write(&path, "one\n").expect("write");
        let err = DiskState::read_with(&path, |p, meta| {
            fs::rename(p, &moved).expect("move away");
            write_access(p, meta)
        })
        .expect_err("a vanished file must not read as read-only");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);

        // The file still at its path keeps its real write access.
        let (bytes, state) = DiskState::read(&moved).expect("read");
        assert_eq!(bytes, b"one\n");
        assert!(!state.read_only);
    }
}

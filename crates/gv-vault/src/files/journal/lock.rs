// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The exclusive lock that keeps two processes (or two handles in one
//! process) from writing or recovering the same journal.
//!
//! The lock is an OS advisory lock ([`fs::File::try_lock`]: `flock` on Unix,
//! `LockFileEx` on Windows) on `.<file-id>.lock`, a separate empty file. The
//! journal itself cannot carry it: it does not exist before the first edit,
//! it is replaced atomically on every compaction (a rename, which would move
//! the lock to the old file) and it is deleted on save.
//!
//! The lock belongs to an open file handle, not to the process: a second
//! handle in the same process is refused as well. The OS drops it when the
//! handle closes, which includes every way a process can end (normal exit,
//! panic, `kill -9`, a crash), so a stale lock from a dead process cannot
//! exist and there is nothing to time out or break. A lock file left
//! behind by a killed process is an empty file and is reused as is.
//!
//! The lock file is removed on release where that is safe:
//!
//! - **Unix**: the holder unlinks it *before* unlocking. A process that
//!   opened the old file in the meantime and then wins its lock sees that
//!   the path no longer names the file it locked, and starts over.
//! - **Windows**: the file is opened without delete sharing, so it can only
//!   be deleted when nobody has it open; the holder tries after closing it
//!   and leaves it when another process is about to take it.
//! - Elsewhere it stays.
//!
//! A file system without locking support (`Unsupported`) gets no lock: the
//! journal then works as before, unprotected, rather than not at all.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How often [`JournalLock::try_acquire`] starts over when the lock file was
/// replaced under it. Each round needs a holder to release in between, so
/// this is never reached in practice.
const ATTEMPTS: usize = 32;

/// An exclusive lock on one journal, held until dropped.
#[derive(Debug)]
pub(super) struct JournalLock {
    path: PathBuf,
    /// `None` when the file system does not support locking.
    file: Option<fs::File>,
}

impl JournalLock {
    /// Takes the lock at `path`, creating the file if needed. `Ok(None)`:
    /// another handle (in this or another live process) holds it.
    pub(super) fn try_acquire(path: PathBuf) -> io::Result<Option<Self>> {
        for _ in 0..ATTEMPTS {
            let file = open(&path)?;
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => return Ok(None),
                Err(fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => {
                    return Ok(Some(Self { path, file: None }));
                }
                Err(fs::TryLockError::Error(e)) => return Err(e),
            }
            if still_named(&file, &path)? {
                return Ok(Some(Self {
                    path,
                    file: Some(file),
                }));
            }
        }
        Err(io::Error::other(format!(
            "{}: the lock file keeps being replaced",
            path.display()
        )))
    }
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        // Unix: unlink while still holding the lock (see the module docs).
        #[cfg(unix)]
        let _ = fs::remove_file(&self.path);
        drop(file);
        // Windows: fails while another process has the file open; it then
        // removes it itself when it releases the lock.
        #[cfg(windows)]
        let _ = fs::remove_file(&self.path);
    }
}

fn open(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_WRITE: no FILE_SHARE_DELETE.
        options.share_mode(0x1 | 0x2);
    }
    options.open(path)
}

/// Whether `path` still names the locked `file`: on Unix a holder may have
/// unlinked it (and another process created a new one) between our open and
/// our lock.
#[cfg(unix)]
fn still_named(file: &fs::File, path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let held = file.metadata()?;
    match fs::symlink_metadata(path) {
        Ok(named) => Ok(named.dev() == held.dev() && named.ino() == held.ino()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Elsewhere a lock file is never deleted while it is open.
#[cfg(not(unix))]
fn still_named(_file: &fs::File, _path: &Path) -> io::Result<bool> {
    Ok(true)
}

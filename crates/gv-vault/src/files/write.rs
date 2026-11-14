// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Writing bytes over a file without risking it.
//!
//! | Situation | What [`write_file`] does |
//! |---|---|
//! | Regular file | Atomic replace through [`gv_settings::atomic_write_with`]: a `.<name>.gv-tmp-<random>` file in the same directory, metadata copied onto it, `fsync`, rename over the original, `fsync` of the directory |
//! | Symlink | The link is resolved and its **target** is written; the link stays |
//! | Hardlink (link count > 1, Unix) | In place, because a rename would split the link: the original bytes go to a [`Backup`] first (the crash journal), then the file is overwritten, truncated and synced, then the backup is cleared |
//! | New file | Created exclusively, written, synced, then the directory is synced |
//! | Read-only file | [`WriteError::ReadOnly`] before anything is written (a rename would otherwise replace a file the user may not modify) |
//! | Write or rename error | The temporary file is removed and the original is untouched |
//! | Network or user-space file system | Atomic replace as well, then the file is read back and compared |
//!
//! Metadata copied onto the temporary file: permissions (all platforms);
//! owner and group where the process may set them, and extended attributes
//! (Linux, macOS: Finder tags, `com.apple.*`, POSIX ACLs, SELinux labels
//! when allowed); creation time (macOS, Windows); hidden, system, archive
//! and not-indexed attributes (Windows). What could not be copied is listed
//! in [`WriteReport::not_preserved`].

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::disk::{DiskState, is_writable};
use super::fskind::{FsKind, filesystem_kind};

/// How [`write_file`] writes an existing file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteStrategy {
    /// Atomic replace, or in place for hardlinked files.
    #[default]
    Auto,
    /// Always in place, after a backup. For file systems that refuse the
    /// rename (some FUSE and network mounts), chosen by the user after an
    /// atomic save failed. Not atomic: a crash mid-write leaves the backup
    /// in the journal to restore from.
    InPlace,
}

/// How a file was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMethod {
    /// The file did not exist and was created.
    Created,
    /// A temporary file replaced the original by rename.
    AtomicReplace,
    /// The original was overwritten in place.
    InPlace(InPlaceReason),
}

/// Why a file was written in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InPlaceReason {
    /// The file has more than one hard link.
    Hardlinked {
        /// The link count.
        links: u64,
    },
    /// [`WriteStrategy::InPlace`] was asked for.
    Requested,
}

/// Metadata of the original that the new file does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataLoss {
    /// The owner could not be set (the process is not the owner and not
    /// privileged); the file now belongs to this process's user.
    Owner,
    /// The group could not be set.
    Group,
    /// An extended attribute could not be copied.
    ExtendedAttribute(String),
    /// The original's extended attributes could not be listed.
    ExtendedAttributes,
}

/// Where the original bytes go before an in-place write.
///
/// [`DocumentJournal`](super::journal::DocumentJournal) implements it: the
/// copy lives in the crash journal, so a crash mid-write can be undone.
pub trait Backup {
    /// Stores `original`, the current contents of `target`. Must be durable
    /// (synced) when it returns; the in-place write starts right after.
    fn store(&mut self, target: &Path, original: &[u8]) -> io::Result<()>;

    /// Drops the copy after the new contents were synced.
    fn clear(&mut self, target: &Path) -> io::Result<()>;
}

/// The result of a successful [`write_file`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteReport {
    /// The file actually written (a symlink's target).
    pub target: PathBuf,
    /// How it was written.
    pub method: WriteMethod,
    /// The file system it is on.
    pub fs: FsKind,
    /// The file's state after the write: the new baseline for change checks.
    pub state: DiskState,
    /// Whether the file was read back and compared.
    pub verified: bool,
    /// Metadata that could not be carried over.
    pub not_preserved: Vec<MetadataLoss>,
}

/// The step of a write that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStep {
    /// Resolving a symlink.
    Resolve,
    /// Reading the original's metadata.
    Stat,
    /// Reading the original or storing the backup before an in-place write.
    Backup,
    /// Creating a new file.
    Create,
    /// Writing a temporary file and renaming it over the original.
    Replace,
    /// Reading the result back.
    Verify,
}

impl std::fmt::Display for WriteStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Resolve => "resolving the link",
            Self::Stat => "reading the file's metadata",
            Self::Backup => "backing up the original",
            Self::Create => "creating the file",
            Self::Replace => "writing and replacing the file",
            Self::Verify => "checking the written file",
        })
    }
}

/// Why a write failed. In every case except [`WriteError::InPlace`] with
/// `restored: false`, the original file is unchanged.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// The file may not be written by this process.
    #[error("{}: the file is read-only", path.display())]
    ReadOnly {
        /// The file.
        path: PathBuf,
    },
    /// The path is a symlink to a missing file.
    #[error("{}: the link points to a file that does not exist", path.display())]
    DanglingSymlink {
        /// The link.
        path: PathBuf,
    },
    /// The path is a directory or a special file.
    #[error("{}: not a regular file", path.display())]
    NotAFile {
        /// The path.
        path: PathBuf,
    },
    /// An I/O error.
    #[error("{}: {step}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// What was being done.
        step: WriteStep,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// The file on disk does not hold what was written (read back on a
    /// network or user-space file system).
    #[error("{}: the saved file does not match what was written", path.display())]
    Verification {
        /// The file.
        path: PathBuf,
    },
    /// The in-place write failed. With `restored`, the original bytes were
    /// written back; otherwise the backup stays in the journal.
    #[error("{}: writing in place failed ({}): {source}", path.display(), if *restored { "the original was restored" } else { "the original is in the recovery journal" })]
    InPlace {
        /// The file.
        path: PathBuf,
        /// Whether the original contents were put back.
        restored: bool,
        /// The error.
        #[source]
        source: io::Error,
    },
}

/// A failure injected into [`write_file`] by data-safety tests.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Fail after the temporary file is written, before the rename (as a
    /// full disk or an I/O error would).
    BeforeRename,
    /// Fail halfway through an in-place write.
    MidWrite,
}

/// Writes `bytes` to the file at `path` (see the module docs for the
/// strategy per situation). `backup` receives the original before an
/// in-place write.
pub fn write_file(
    path: &Path,
    bytes: &[u8],
    strategy: WriteStrategy,
    backup: &mut dyn Backup,
    fault: Option<Fault>,
) -> Result<WriteReport, WriteError> {
    let target = resolve_target(path)?;
    let io_err = |step| {
        let target = target.clone();
        move |source| WriteError::Io {
            path: target,
            step,
            source,
        }
    };
    let mut not_preserved = Vec::new();
    let method = match fs::metadata(&target) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            create_new(&target, bytes, fault).map_err(io_err(WriteStep::Create))?;
            WriteMethod::Created
        }
        Err(e) => return Err(io_err(WriteStep::Stat)(e)),
        Ok(meta) => {
            if !meta.is_file() {
                return Err(WriteError::NotAFile { path: target });
            }
            if !is_writable(&target, &meta) {
                return Err(WriteError::ReadOnly { path: target });
            }
            let links = link_count(&meta);
            let method = match strategy {
                WriteStrategy::InPlace => WriteMethod::InPlace(InPlaceReason::Requested),
                WriteStrategy::Auto if links > 1 => {
                    WriteMethod::InPlace(InPlaceReason::Hardlinked { links })
                }
                WriteStrategy::Auto => WriteMethod::AtomicReplace,
            };
            match method {
                WriteMethod::InPlace(_) => write_in_place(&target, bytes, backup, fault)?,
                _ => replace_atomically(&target, bytes, fault, &mut not_preserved)
                    .map_err(io_err(WriteStep::Replace))?,
            }
            method
        }
    };
    let fs_kind = filesystem_kind(&target);
    let (state, verified) = verify(&target, bytes, fs_kind)?;
    Ok(WriteReport {
        target,
        method,
        fs: fs_kind,
        state,
        verified,
        not_preserved,
    })
}

/// The file a write goes to: the path itself, or a symlink's final target.
pub(crate) fn resolve_target(path: &Path) -> Result<PathBuf, WriteError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                WriteError::DanglingSymlink {
                    path: path.to_path_buf(),
                }
            } else {
                WriteError::Io {
                    path: path.to_path_buf(),
                    step: WriteStep::Resolve,
                    source,
                }
            }
        }),
        _ => Ok(path.to_path_buf()),
    }
}

fn link_count(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink()
    }
    #[cfg(not(unix))]
    {
        // The link count needs an unstable API on Windows; hardlinked files
        // there are replaced like any other.
        let _ = meta;
        1
    }
}

fn create_new(target: &Path, bytes: &[u8], fault: Option<Fault>) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let written = (|| {
        if fault == Some(Fault::BeforeRename) {
            return Err(io::Error::other("injected fault while creating the file"));
        }
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(e) = written {
        drop(file);
        let _ = fs::remove_file(target);
        return Err(e);
    }
    if let Some(dir) = target.parent() {
        sync_dir(dir);
    }
    Ok(())
}

fn replace_atomically(
    target: &Path,
    bytes: &[u8],
    fault: Option<Fault>,
    not_preserved: &mut Vec<MetadataLoss>,
) -> io::Result<()> {
    gv_settings::atomic_write_with(target, bytes, &mut |temp, original| {
        if let Some(original) = original {
            copy_extra_metadata(temp, original, target, not_preserved);
        }
        if fault == Some(Fault::BeforeRename) {
            return Err(io::Error::other("injected fault before the rename"));
        }
        Ok(())
    })
}

fn write_in_place(
    target: &Path,
    bytes: &[u8],
    backup: &mut dyn Backup,
    fault: Option<Fault>,
) -> Result<(), WriteError> {
    let backup_err = |source| WriteError::Io {
        path: target.to_path_buf(),
        step: WriteStep::Backup,
        source,
    };
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(target)
        .map_err(backup_err)?;
    let mut original = Vec::new();
    file.read_to_end(&mut original).map_err(backup_err)?;
    backup.store(target, &original).map_err(backup_err)?;
    match overwrite(&mut file, bytes, fault) {
        Ok(()) => {
            // A copy left behind is only ever offered for recovery, never
            // applied by itself.
            let _ = backup.clear(target);
            Ok(())
        }
        Err(source) => {
            let restored = overwrite(&mut file, &original, None).is_ok();
            if restored {
                let _ = backup.clear(target);
            }
            Err(WriteError::InPlace {
                path: target.to_path_buf(),
                restored,
                source,
            })
        }
    }
}

fn overwrite(file: &mut fs::File, bytes: &[u8], fault: Option<Fault>) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    if fault == Some(Fault::MidWrite) {
        file.write_all(&bytes[..bytes.len() / 2])?;
        file.sync_all()?;
        return Err(io::Error::other("injected fault during the in-place write"));
    }
    file.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.sync_all()
}

/// Re-reads the file's state after a write. On network and user-space file
/// systems the contents are read back and compared.
fn verify(target: &Path, bytes: &[u8], fs_kind: FsKind) -> Result<(DiskState, bool), WriteError> {
    let io_err = |source| WriteError::Io {
        path: target.to_path_buf(),
        step: WriteStep::Verify,
        source,
    };
    if fs_kind.needs_verification() {
        let (disk, state) = DiskState::read(target).map_err(io_err)?;
        if disk != bytes {
            return Err(WriteError::Verification {
                path: target.to_path_buf(),
            });
        }
        return Ok((state, true));
    }
    let meta = fs::metadata(target).map_err(io_err)?;
    if meta.len() != bytes.len() as u64 {
        return Err(WriteError::Verification {
            path: target.to_path_buf(),
        });
    }
    let mut state = DiskState::from_parts(&meta, bytes);
    state.read_only = !is_writable(target, &meta);
    Ok((state, false))
}

/// `fsync` of a directory, so a new or renamed entry survives a power cut.
/// Best effort, and a no-op where directories cannot be opened (Windows).
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(handle) = fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Owner, group and extended attributes, best effort.
fn copy_extra_metadata(
    temp: &fs::File,
    original: &fs::Metadata,
    source: &Path,
    not_preserved: &mut Vec<MetadataLoss>,
) {
    #[cfg(unix)]
    copy_owner(temp, original, not_preserved);
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    copy_xattrs(source, temp, not_preserved);
    #[cfg(not(unix))]
    let _ = (temp, original, source, not_preserved);
}

#[cfg(unix)]
fn copy_owner(temp: &fs::File, original: &fs::Metadata, not_preserved: &mut Vec<MetadataLoss>) {
    use std::os::unix::fs::{MetadataExt, fchown};
    let Ok(current) = temp.metadata() else {
        return;
    };
    let (uid, gid) = (original.uid(), original.gid());
    if current.uid() == uid && current.gid() == gid {
        return;
    }
    if fchown(temp, Some(uid), Some(gid)).is_err() {
        if current.uid() != uid {
            not_preserved.push(MetadataLoss::Owner);
        }
        if current.gid() != gid && fchown(temp, None, Some(gid)).is_err() {
            not_preserved.push(MetadataLoss::Group);
        }
    }
    // A change of owner clears the set-user-ID and set-group-ID bits.
    let _ = temp.set_permissions(original.permissions());
}

/// Copies every extended attribute of `source` that the temporary file does
/// not already have with the same value. A file system without extended
/// attributes is skipped silently.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn copy_xattrs(source: &Path, temp: &fs::File, not_preserved: &mut Vec<MetadataLoss>) {
    use rustix::fs::{
        XattrFlags, fgetxattr, fgetxattr_query, fsetxattr, getxattr, getxattr_query, listxattr,
        listxattr_query,
    };
    use rustix::io::Errno;

    let names = match read_sized(|| listxattr_query(source), |buf| listxattr(source, buf)) {
        Ok(names) => names,
        Err(e) if e == Errno::NOTSUP || e == Errno::OPNOTSUPP => return,
        Err(_) => {
            not_preserved.push(MetadataLoss::ExtendedAttributes);
            return;
        }
    };
    for name in names.split(|&b| b == 0).filter(|n| !n.is_empty()) {
        let label = String::from_utf8_lossy(name).into_owned();
        let Ok(value) = read_sized(
            || getxattr_query(source, name),
            |buf| getxattr(source, name, buf),
        ) else {
            not_preserved.push(MetadataLoss::ExtendedAttribute(label));
            continue;
        };
        let existing = read_sized(
            || fgetxattr_query(temp, name),
            |buf| fgetxattr(temp, name, buf),
        );
        if existing.as_deref() == Ok(&value[..]) {
            continue;
        }
        if fsetxattr(temp, name, &value, XattrFlags::empty()).is_err() {
            not_preserved.push(MetadataLoss::ExtendedAttribute(label));
        }
    }
}

/// Reads a value whose size must be queried first, retrying if it grows in
/// between.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn read_sized(
    query: impl Fn() -> rustix::io::Result<usize>,
    read: impl Fn(&mut [u8]) -> rustix::io::Result<usize>,
) -> rustix::io::Result<Vec<u8>> {
    for _ in 0..3 {
        let size = query()?;
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0; size];
        match read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                return Ok(buf);
            }
            Err(rustix::io::Errno::RANGE) => {}
            Err(e) => return Err(e),
        }
    }
    Err(rustix::io::Errno::RANGE)
}

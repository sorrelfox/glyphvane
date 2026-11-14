// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What kind of file system a path is on.
//!
//! Saves use the same atomic replace everywhere (OneDrive, Dropbox, iCloud
//! and SMB all support it). On network and user-space file systems the
//! result is read back after the write, because their caches and coarse or
//! skewed timestamps can hide a failed write, and later change checks hash
//! instead of trusting timestamps.

use std::path::Path;

/// The family of a file system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsKind {
    /// A local disk file system.
    Local,
    /// A network file system (NFS, SMB/CIFS, AFP, WebDAV, 9P, …). The name
    /// is the file system's.
    Network(&'static str),
    /// A user-space (FUSE) file system: cloud-drive clients, sshfs, GVfs.
    UserSpace,
    /// Not known on this platform.
    Unknown,
}

impl FsKind {
    /// Whether saves read the file back and change checks always hash.
    pub fn needs_verification(self) -> bool {
        !matches!(self, Self::Local)
    }
}

/// The kind of file system holding `path` (an existing file or directory).
pub fn filesystem_kind(path: &Path) -> FsKind {
    imp::kind(path)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::FsKind;
    use std::path::Path;

    /// `statfs` magic numbers (linux/magic.h and the file systems' sources).
    const NETWORK: &[(u64, &str)] = &[
        (0x6969, "nfs"),
        (0x517B, "smb"),
        (0xFF53_4D42, "cifs"),
        (0xFE53_4D42, "smb2"),
        (0x5346_414F, "afs"),
        (0x7375_7245, "coda"),
        (0x0102_1997, "9p"),
        (0x00C3_6400, "ceph"),
        (0x564C, "ncp"),
        (0x0BD0_0BD0, "lustre"),
        (0x4732_4653, "gfs2"),
    ];
    const FUSE: u64 = 0x6573_5546;

    pub(super) fn kind(path: &Path) -> FsKind {
        let Ok(stat) = rustix::fs::statfs(path) else {
            return FsKind::Unknown;
        };
        // `f_type` is signed on some targets; the magic numbers are 32-bit.
        #[allow(clippy::unnecessary_cast)]
        let magic = (stat.f_type as u64) & 0xFFFF_FFFF;
        if magic == FUSE {
            return FsKind::UserSpace;
        }
        NETWORK
            .iter()
            .find(|(m, _)| *m == magic)
            .map_or(FsKind::Local, |(_, name)| FsKind::Network(name))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::FsKind;
    use std::path::Path;

    const NETWORK: &[&str] = &["smbfs", "nfs", "afpfs", "webdav", "cifs", "ftp"];

    pub(super) fn kind(path: &Path) -> FsKind {
        let Ok(stat) = rustix::fs::statfs(path) else {
            return FsKind::Unknown;
        };
        let name: Vec<u8> = stat
            .f_fstypename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        let name = String::from_utf8_lossy(&name);
        if let Some(found) = NETWORK.iter().find(|n| **n == name) {
            return FsKind::Network(found);
        }
        if name.starts_with("macfuse") || name.starts_with("osxfuse") || name == "fuse" {
            return FsKind::UserSpace;
        }
        FsKind::Local
    }
}

#[cfg(windows)]
mod imp {
    use super::FsKind;
    use std::path::{Component, Path, Prefix};

    /// UNC paths are network shares. A mapped drive letter cannot be told
    /// apart without `GetDriveTypeW`, so it is `Unknown` (and verified like
    /// a share).
    pub(super) fn kind(path: &Path) -> FsKind {
        match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => FsKind::Network("smb"),
                _ => FsKind::Unknown,
            },
            _ => FsKind::Unknown,
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
mod imp {
    use super::FsKind;
    use std::path::Path;

    pub(super) fn kind(_path: &Path) -> FsKind {
        FsKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_has_a_kind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let kind = filesystem_kind(dir.path());
        #[cfg(target_os = "linux")]
        assert_ne!(kind, FsKind::Unknown);
        let _ = kind.needs_verification();
    }
}

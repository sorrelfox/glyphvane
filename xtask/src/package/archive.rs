// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Reproducible file trees, `.tar.gz` and `.zip` writers.
//!
//! The output depends only on the tree's contents and `SOURCE_DATE_EPOCH`:
//! entries are sorted by path, every timestamp is the epoch, owners are
//! 0:0 without names, and permissions are 0644 or 0755.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use flate2::write::{DeflateEncoder, GzEncoder};
use flate2::{Compression, Crc, GzBuilder};

/// Where an entry's bytes come from.
#[derive(Debug, Clone)]
pub(crate) enum Source {
    File(PathBuf),
    Bytes(Vec<u8>),
    /// A symbolic link to this (relative) target.
    Symlink(String),
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) source: Source,
    /// Unix permission bits (0o644 or 0o755); ignored for symlinks.
    pub(crate) mode: u32,
}

/// A file tree to package: `/`-separated relative paths to entries.
/// Directories are implied by the paths.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    entries: BTreeMap<String, Entry>,
}

impl Tree {
    pub(crate) fn file(&mut self, path: &str, source: &Path, mode: u32) {
        self.insert(path, Source::File(source.to_path_buf()), mode);
    }

    pub(crate) fn bytes(&mut self, path: &str, bytes: Vec<u8>, mode: u32) {
        self.insert(path, Source::Bytes(bytes), mode);
    }

    pub(crate) fn symlink(&mut self, path: &str, target: &str) {
        self.insert(path, Source::Symlink(target.to_owned()), 0o777);
    }

    fn insert(&mut self, path: &str, source: Source, mode: u32) {
        debug_assert!(!path.starts_with('/') && !path.ends_with('/'));
        self.entries.insert(path.to_owned(), Entry { source, mode });
    }

    /// Every directory implied by the entries, parents first.
    fn dirs(&self) -> BTreeSet<String> {
        let mut dirs = BTreeSet::new();
        for path in self.entries.keys() {
            let mut end = 0;
            while let Some(i) = path[end..].find('/') {
                end += i;
                dirs.insert(path[..end].to_owned());
                end += 1;
            }
        }
        dirs
    }

    /// Directories and entries in one sorted sequence (`None` = directory).
    fn sorted(&self) -> Vec<(String, Option<&Entry>)> {
        let mut all: Vec<(String, Option<&Entry>)> = self
            .dirs()
            .into_iter()
            .map(|d| (d, None))
            .chain(self.entries.iter().map(|(p, e)| (p.clone(), Some(e))))
            .collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// Writes the tree below `dir` (which must exist) and sets every file
    /// time it can to `epoch`.
    pub(crate) fn materialize(&self, dir: &Path, epoch: u64) -> Result<()> {
        for d in self.dirs() {
            fs::create_dir_all(dir.join(&d))?;
        }
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch);
        for (path, entry) in &self.entries {
            let dest = dir.join(path);
            match &entry.source {
                Source::Symlink(target) => symlink(target, &dest)?,
                Source::File(src) => {
                    fs::copy(src, &dest).with_context(|| format!("copying {}", src.display()))?;
                }
                Source::Bytes(bytes) => fs::write(&dest, bytes)?,
            }
            if !matches!(entry.source, Source::Symlink(_)) {
                set_mode(&dest, entry.mode)?;
                let file = fs::File::options().write(true).open(&dest)?;
                file.set_modified(time)?;
            }
        }
        // Directory times last: creating entries changes them. Not portable
        // (Windows cannot open directories as files), and only cosmetic there.
        if cfg!(unix) {
            for d in self.dirs().iter().rev() {
                if let Ok(f) = fs::File::open(dir.join(d)) {
                    let _ = f.set_modified(time);
                }
            }
        }
        Ok(())
    }

    fn read(entry: &Entry) -> Result<Vec<u8>> {
        Ok(match &entry.source {
            Source::File(p) => fs::read(p).with_context(|| format!("reading {}", p.display()))?,
            Source::Bytes(b) => b.clone(),
            Source::Symlink(t) => t.clone().into_bytes(),
        })
    }

    /// A reproducible gzip-compressed ustar archive.
    pub(crate) fn write_tar_gz(&self, out: &Path, epoch: u64) -> Result<()> {
        let file = fs::File::create(out).with_context(|| format!("creating {}", out.display()))?;
        // gzip header: mtime 0, no file name (like `gzip -n`).
        let mut gz: GzEncoder<fs::File> = GzBuilder::new()
            .mtime(0)
            .operating_system(3)
            .write(file, Compression::best());
        for (path, entry) in self.sorted() {
            match entry {
                None => {
                    let header = tar_header(&format!("{path}/"), 0o755, 0, epoch, b'5', "")?;
                    gz.write_all(&header)?;
                }
                Some(Entry {
                    source: Source::Symlink(target),
                    ..
                }) => {
                    let header = tar_header(&path, 0o777, 0, epoch, b'2', target)?;
                    gz.write_all(&header)?;
                }
                Some(e) => {
                    let data = Self::read(e)?;
                    let header = tar_header(&path, e.mode, data.len() as u64, epoch, b'0', "")?;
                    gz.write_all(&header)?;
                    gz.write_all(&data)?;
                    let pad = (512 - data.len() % 512) % 512;
                    gz.write_all(&vec![0; pad])?;
                }
            }
        }
        gz.write_all(&[0; 1024])?;
        gz.finish()?.sync_all()?;
        Ok(())
    }

    /// A reproducible zip archive (deflate, Unix permissions, no extra fields).
    pub(crate) fn write_zip(&self, out: &Path, epoch: u64) -> Result<()> {
        let (dos_time, dos_date) = dos_datetime(epoch);
        let mut body = Vec::new();
        let mut central = Vec::new();
        let mut count: u16 = 0;
        for (path, entry) in self.sorted() {
            let (name, data, mode, method, external) = match entry {
                None => (format!("{path}/"), Vec::new(), 0o40755u32, 0u16, 0x10u32),
                Some(Entry {
                    source: Source::Symlink(_),
                    ..
                }) => {
                    bail!("zip archives here do not hold symlinks ({path})")
                }
                Some(e) => (path.clone(), Self::read(e)?, 0o100_000 | e.mode, 8, 0),
            };
            let mut crc = Crc::new();
            crc.update(&data);
            let compressed = if method == 8 {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::best());
                enc.write_all(&data)?;
                enc.finish()?
            } else {
                Vec::new()
            };
            let size = u32::try_from(data.len()).context("file too large for zip")?;
            let csize = u32::try_from(compressed.len()).context("file too large for zip")?;
            let offset = u32::try_from(body.len()).context("archive too large for zip")?;
            let name_len = u16::try_from(name.len()).context("path too long")?;
            // Local file header.
            put32(&mut body, 0x0403_4b50);
            put16(&mut body, 20); // version needed: 2.0
            put16(&mut body, 0x0800); // flags: UTF-8 names
            put16(&mut body, method);
            put16(&mut body, dos_time);
            put16(&mut body, dos_date);
            put32(&mut body, crc.sum());
            put32(&mut body, csize);
            put32(&mut body, size);
            put16(&mut body, name_len);
            put16(&mut body, 0);
            body.extend_from_slice(name.as_bytes());
            body.extend_from_slice(&compressed);
            // Central directory record.
            put32(&mut central, 0x0201_4b50);
            put16(&mut central, (3 << 8) | 20); // made by: Unix, 2.0
            put16(&mut central, 20);
            put16(&mut central, 0x0800);
            put16(&mut central, method);
            put16(&mut central, dos_time);
            put16(&mut central, dos_date);
            put32(&mut central, crc.sum());
            put32(&mut central, csize);
            put32(&mut central, size);
            put16(&mut central, name_len);
            put16(&mut central, 0); // extra
            put16(&mut central, 0); // comment
            put16(&mut central, 0); // disk
            put16(&mut central, 0); // internal attributes
            put32(&mut central, (mode << 16) | external);
            put32(&mut central, offset);
            central.extend_from_slice(name.as_bytes());
            count = count.checked_add(1).context("too many zip entries")?;
        }
        let cd_offset = u32::try_from(body.len()).context("archive too large for zip")?;
        let cd_size = u32::try_from(central.len()).context("archive too large for zip")?;
        body.extend_from_slice(&central);
        put32(&mut body, 0x0605_4b50);
        put16(&mut body, 0);
        put16(&mut body, 0);
        put16(&mut body, count);
        put16(&mut body, count);
        put32(&mut body, cd_size);
        put32(&mut body, cd_offset);
        put16(&mut body, 0);
        fs::write(out, body).with_context(|| format!("writing {}", out.display()))?;
        Ok(())
    }
}

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

/// One 512-byte ustar header. Long paths are split into prefix and name.
fn tar_header(
    path: &str,
    mode: u32,
    size: u64,
    mtime: u64,
    kind: u8,
    link: &str,
) -> Result<[u8; 512]> {
    let mut h = [0u8; 512];
    let (prefix, name) = split_ustar_path(path)?;
    ensure!(link.len() <= 100, "symlink target too long: {link}");
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], u64::from(mode));
    octal(&mut h[108..116], 0); // uid
    octal(&mut h[116..124], 0); // gid
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], mtime);
    h[148..156].fill(b' ');
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    octal(&mut h[329..337], 0); // devmajor
    octal(&mut h[337..345], 0); // devminor
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let field = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(field.as_bytes());
    Ok(h)
}

/// Zero-padded octal digits followed by a NUL, filling `field`.
fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let s = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(&s.as_bytes()[s.len() - digits..]);
    field[digits] = 0;
}

fn split_ustar_path(path: &str) -> Result<(&str, &str)> {
    if path.len() <= 100 {
        return Ok(("", path));
    }
    // Split at a `/` so that the name is at most 100 bytes and the prefix at
    // most 155 (a trailing `/` on directories stays with the name).
    let body = path.strip_suffix('/').unwrap_or(path);
    for (i, _) in body.match_indices('/') {
        let (prefix, name) = (&path[..i], &path[i + 1..]);
        if prefix.len() <= 155 && name.len() <= 100 {
            return Ok((prefix, name));
        }
    }
    bail!("path too long for a ustar archive: {path}")
}

/// MS-DOS time and date fields (UTC) for a Unix time, clamped to 1980..2107.
fn dos_datetime(epoch: u64) -> (u16, u16) {
    let days = i64::try_from(epoch / 86_400).unwrap_or(i64::MAX / 2);
    let secs = epoch % 86_400;
    let (y, m, d) = civil_from_days(days);
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    let y = u16::try_from((y - 1980).min(127)).unwrap_or(127);
    let time = u16::try_from(((secs / 3600) << 11) | ((secs % 3600 / 60) << 5) | ((secs % 60) / 2))
        .unwrap_or(0);
    let date = (y << 9) | (u16::from(m) << 5) | u16::from(d);
    (time, date)
}

/// Gregorian date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u8::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u8::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("creating symlink {}", link.display()))
}

#[cfg(not(unix))]
fn symlink(_target: &str, link: &Path) -> Result<()> {
    bail!(
        "symlinks are only created on Unix hosts ({})",
        link.display()
    )
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dos_dates() {
        // 2026-11-13 19:45:30 UTC
        let (time, date) = dos_datetime(1_794_599_130);
        assert_eq!(date >> 9, 2026 - 1980);
        assert_eq!((date >> 5) & 0xf, 11);
        assert_eq!(date & 0x1f, 13);
        assert_eq!(time >> 11, 19);
        assert_eq!((time >> 5) & 0x3f, 45);
        assert_eq!((time & 0x1f) * 2, 30);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(dos_datetime(0), (0, (1 << 5) | 1));
    }

    #[test]
    fn ustar_headers() {
        let h = tar_header("a/b.txt", 0o644, 5, 1, b'0', "").expect("header");
        assert_eq!(&h[..7], b"a/b.txt");
        assert_eq!(&h[100..108], b"0000644\0");
        assert_eq!(&h[124..136], b"00000000005\0");
        assert_eq!(&h[257..265], b"ustar\x0000");
        let sum: u32 = h
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u32::from(b)
                }
            })
            .sum();
        assert_eq!(&h[148..156], format!("{sum:06o}\0 ").as_bytes());

        let long = format!("{}/{}", "d".repeat(60), "f".repeat(60));
        let h = tar_header(&long, 0o644, 0, 0, b'0', "").expect("split header");
        assert_eq!(&h[..60], "f".repeat(60).as_bytes());
        assert_eq!(&h[345..405], "d".repeat(60).as_bytes());
        assert!(tar_header(&"x".repeat(101), 0o644, 0, 0, b'0', "").is_err());
    }

    #[test]
    fn archives_are_deterministic_and_sorted() {
        let mut tree = Tree::default();
        tree.bytes("top/z.txt", b"zed".to_vec(), 0o644);
        tree.bytes("top/bin/tool", b"#!/bin/sh\n".to_vec(), 0o755);
        tree.bytes("top/a.txt", b"a".to_vec(), 0o644);
        let sorted: Vec<String> = tree.sorted().into_iter().map(|(p, _)| p).collect();
        assert_eq!(
            sorted,
            ["top", "top/a.txt", "top/bin", "top/bin/tool", "top/z.txt"]
        );

        let dir = std::env::temp_dir().join(format!("xtask-archive-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let (a, b) = (dir.join("a.zip"), dir.join("b.zip"));
        tree.write_zip(&a, 1_700_000_000).expect("zip a");
        tree.write_zip(&b, 1_700_000_000).expect("zip b");
        let za = fs::read(&a).expect("read a");
        assert_eq!(za, fs::read(&b).expect("read b"));
        assert_eq!(&za[..4], b"PK\x03\x04");
        assert_eq!(&za[za.len() - 22..za.len() - 18], b"PK\x05\x06");

        let (a, b) = (dir.join("a.tar.gz"), dir.join("b.tar.gz"));
        tree.write_tar_gz(&a, 1_700_000_000).expect("tar a");
        tree.write_tar_gz(&b, 1_700_000_000).expect("tar b");
        assert_eq!(fs::read(&a).expect("a"), fs::read(&b).expect("b"));
        let _ = fs::remove_dir_all(&dir);
    }
}

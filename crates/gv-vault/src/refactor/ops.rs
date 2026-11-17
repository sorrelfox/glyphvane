// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The simple file operations: names, new folders, duplicates and the
//! trash.

use std::fs;
use std::io;
use std::path::Path;

use super::RefactorError;
use crate::keys::{fold_key, nfc_key};
use crate::path::{VaultPath, extension_of, stem_of};
use crate::platform::{NameIssue, PlatformRules};

/// Characters that break wikilinks to a file named with them (Obsidian).
const LINK_BREAKING: [char; 5] = ['#', '^', '[', ']', '|'];

/// The name typed by the user, ready to check: trimmed and in NFC.
#[must_use]
pub fn clean_name(input: &str) -> String {
    nfc_key(input.trim()).into_owned()
}

/// Refuses a file or folder name that is empty, would break links to it,
/// or is not valid on every platform (so the vault stays portable).
pub fn validate_name(name: &str) -> Result<(), RefactorError> {
    let invalid = |m: String| Err(RefactorError::InvalidName(m));
    if name.trim().is_empty() {
        return invalid("A name is needed".into());
    }
    if name == "." || name == ".." {
        return invalid(format!("“{name}” is not a valid name"));
    }
    if name.contains(['/', '\\']) {
        return invalid("Names cannot contain / or \\".into());
    }
    if let Some(c) = name.chars().find(|c| LINK_BREAKING.contains(c)) {
        return invalid(format!("“{c}” in a name breaks links to it"));
    }
    for rules in PlatformRules::ALL {
        let platform = match rules.name {
            "windows" => "Windows",
            "linux" => "Linux",
            _ => "macOS",
        };
        if let Some(issue) = rules.name_issues(name).into_iter().next() {
            return invalid(match issue {
                NameIssue::ReservedName(n) => format!("“{n}” is a reserved name on {platform}"),
                NameIssue::ForbiddenChar(c) if c.is_control() => {
                    "Names cannot contain control characters".into()
                }
                NameIssue::ForbiddenChar(c) => {
                    format!("“{c}” is not allowed in names on {platform}")
                }
                NameIssue::TrailingDotOrSpace => {
                    format!("Names cannot end with a dot or a space on {platform}")
                }
                NameIssue::TooLong { max, .. } => format!("The name is too long (at most {max})"),
            });
        }
    }
    Ok(())
}

/// A name for a new entry in the folder `dir` (on disk): `stem.ext`, else
/// `stem 1.ext`, `stem 2.ext`, …, the first not taken in any letter case.
/// With `skip_plain`, `stem.ext` itself is not offered (duplicates).
pub fn unique_name(
    dir: &Path,
    stem: &str,
    ext: Option<&str>,
    skip_plain: bool,
) -> io::Result<String> {
    let taken: Vec<String> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| fold_key(&e.file_name().to_string_lossy()))
        .collect();
    let start = u32::from(skip_plain);
    for n in start..100_000u32 {
        let base = match n {
            0 => stem.to_owned(),
            n => format!("{stem} {n}"),
        };
        let name = match ext {
            Some(ext) => format!("{base}.{ext}"),
            None => base,
        };
        if !taken.contains(&fold_key(&name)) {
            return Ok(name);
        }
    }
    Err(io::Error::other("too many entries with that name"))
}

/// The name of a duplicate of `name` in the folder `dir`: `Note 1.md`,
/// `Note 2.md`, … (`Folder 1` for a folder).
pub fn duplicate_name(dir: &Path, name: &str, is_dir: bool) -> io::Result<String> {
    if is_dir {
        unique_name(dir, name, None, true)
    } else {
        unique_name(dir, stem_of(name), extension_of(name), true)
    }
}

/// Creates a folder named `name` (or `name 1`, …) in the vault folder
/// `parent`. Returns its path.
pub fn create_folder(
    root: &Path,
    parent: &VaultPath,
    name: &str,
) -> Result<VaultPath, RefactorError> {
    validate_name(name)?;
    let dir = parent.to_fs_path(root);
    for _ in 0..8 {
        let unique =
            unique_name(&dir, name, None, false).map_err(|e| RefactorError::io(&dir, e))?;
        let path = parent
            .join(&unique)
            .map_err(|e| RefactorError::InvalidName(e.to_string()))?;
        match fs::create_dir(path.to_fs_path(root)) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(RefactorError::io(path.to_fs_path(root), e)),
        }
    }
    Err(RefactorError::Exists(parent.clone()))
}

/// Copies the file or folder `path` next to itself as `Name 1.md`
/// (`Folder 1`). Never replaces anything; a failed folder copy removes
/// what it made. Returns the copy's path.
pub fn duplicate(root: &Path, path: &VaultPath) -> Result<VaultPath, RefactorError> {
    if path.is_root() {
        return Err(RefactorError::Root);
    }
    let source = path.to_fs_path(root);
    let meta = fs::symlink_metadata(&source).map_err(|_| RefactorError::Missing(path.clone()))?;
    let parent = path.parent().unwrap_or_default();
    let dir = parent.to_fs_path(root);
    let name = path.file_name().unwrap_or_default();
    let copy_name =
        duplicate_name(&dir, name, meta.is_dir()).map_err(|e| RefactorError::io(&dir, e))?;
    let copy = parent
        .join(&copy_name)
        .map_err(|e| RefactorError::InvalidName(e.to_string()))?;
    let target = copy.to_fs_path(root);
    let result = if meta.is_dir() {
        copy_dir(&source, &target)
    } else {
        copy_file(&source, &target)
    };
    if let Err(e) = result {
        if meta.is_dir() {
            let _ = fs::remove_dir_all(&target);
        }
        return Err(RefactorError::io(&target, e));
    }
    Ok(copy)
}

/// Copies one file to a path that must not exist.
fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            return std::os::unix::fs::symlink(fs::read_link(from)?, to);
        }
        #[cfg(not(unix))]
        {
            return Err(io::Error::other("links are not copied"));
        }
    }
    let mut input = fs::File::open(from)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    let _ = fs::set_permissions(to, meta.permissions());
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Where deleted files go.
pub trait Trash: Send + Sync {
    /// Moves the file or folder at `path` to the trash.
    fn delete(&self, path: &Path) -> Result<(), String>;
}

/// The OS trash: the freedesktop trash on Linux and the BSDs, the Finder's
/// on macOS, the recycle bin on Windows.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemTrash;

impl Trash for SystemTrash {
    fn delete(&self, path: &Path) -> Result<(), String> {
        trash::delete(path).map_err(|e| e.to_string())
    }
}

/// Moves the entries `paths` of the vault at `root` to `trash`, stopping
/// at the first failure. Returns the paths moved.
pub fn move_to_trash(
    root: &Path,
    paths: &[VaultPath],
    trash: &dyn Trash,
) -> Result<Vec<VaultPath>, RefactorError> {
    let mut done = Vec::new();
    for path in paths {
        if path.is_root() {
            return Err(RefactorError::Root);
        }
        let full = path.to_fs_path(root);
        if fs::symlink_metadata(&full).is_err() {
            return Err(RefactorError::Missing(path.clone()));
        }
        trash.delete(&full).map_err(RefactorError::Trash)?;
        done.push(path.clone());
    }
    Ok(done)
}

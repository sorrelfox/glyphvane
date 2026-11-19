// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Where new notes go: Obsidian's `newFileLocation` and
//! `newFileFolderPath` in `.obsidian/app.json`.
//!
//! - A note made from nothing but a name (New note, the quick switcher)
//!   goes to [`new_note_folder`]: the root when the setting is absent.
//! - A note made for an unresolved link with a bare name (no folder
//!   written) goes where [`place_bare_link_note`] says: the source's
//!   folder when the setting is absent or `current`.

use std::path::{Component, Path};

use crate::path::VaultPath;

/// Where Obsidian puts new notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewFileLocation {
    /// The vault root.
    Root,
    /// The current note's folder.
    Current,
    /// `newFileFolderPath`, as written.
    Folder(String),
}

impl NewFileLocation {
    /// The vault's setting, when its `.obsidian/app.json` sets one.
    #[must_use]
    pub fn read(root: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(root.join(".obsidian").join("app.json")).ok()?;
        match json_string(&text, "newFileLocation")?.as_str() {
            "root" => Some(Self::Root),
            "current" => Some(Self::Current),
            "folder" => Some(Self::Folder(
                json_string(&text, "newFileFolderPath").unwrap_or_default(),
            )),
            _ => None,
        }
    }
}

/// `folder` vault-relative without surrounding `/`, or empty (the root)
/// when it would leave the vault or has a part [`is_safe_folder_part`]
/// refuses.
fn safe_folder(folder: &str) -> String {
    let folder = folder.trim_matches('/');
    if !folder.split('/').all(is_safe_folder_part) {
        return String::new();
    }
    folder.to_owned()
}

/// `true` when `part` is one plain name on every platform, so that joining
/// it to a folder stays in that folder: not empty, `.` or `..`, and
/// without `\`, `:` (a drive or stream on Windows), NUL or control
/// characters, nor a Windows device name (`CON`, `NUL.md`).
#[must_use]
pub fn is_safe_folder_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part
            .chars()
            .any(|c| matches!(c, '\\' | ':') || c.is_control())
        && !crate::platform::is_reserved_device_name(part)
}

/// `true` when `path` is `root` or below it, compared component by
/// component, with no `..`, `.`, root or drive component after `root`:
/// the last check before a file or folder is made from a joined path.
#[must_use]
pub fn is_within(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .is_ok_and(|rest| rest.components().all(|c| matches!(c, Component::Normal(_))))
}

/// The vault folder a new note made from nothing but a name goes to (New
/// note, the quick switcher), by the vault's `newFileLocation`: its root
/// (also when unset), the folder of `current` (a file in the vault), or
/// `newFileFolderPath`. Vault-relative and `/`-separated, empty for the
/// root; a folder that would leave the vault is the root.
#[must_use]
pub fn new_note_folder(root: &Path, current: Option<&Path>) -> String {
    match NewFileLocation::read(root) {
        Some(NewFileLocation::Folder(f)) => safe_folder(&f),
        // A real folder below the root: its own names, as they are.
        Some(NewFileLocation::Current) => current
            .and_then(|file| file.parent()?.strip_prefix(root).ok())
            .map(|rel| {
                rel.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default(),
        Some(NewFileLocation::Root) | None => String::new(),
    }
}

/// The note an unresolved link with a bare name creates, given `path`,
/// where it goes in the source's folder: unchanged when `location` is
/// unset or `current`, else the same name in the root or in
/// `newFileFolderPath` (the root when that would leave the vault).
#[must_use]
pub fn place_bare_link_note(
    location: Option<&NewFileLocation>,
    path: VaultPath,
) -> Option<VaultPath> {
    let folder = match location {
        Some(NewFileLocation::Root) => String::new(),
        Some(NewFileLocation::Folder(f)) => safe_folder(f),
        Some(NewFileLocation::Current) | None => return Some(path),
    };
    let name = path.file_name()?;
    if folder.is_empty() {
        VaultPath::new(name).ok()
    } else {
        VaultPath::new(&format!("{folder}/{name}")).ok()
    }
}

/// The string value of `"key"` in a flat JSON object (enough for the two
/// settings above: `\"`, `\\` and `\/` are unescaped, other escapes kept).
fn json_string(text: &str, key: &str) -> Option<String> {
    let quoted = format!("\"{key}\"");
    let at = text.find(&quoted)? + quoted.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                c @ ('"' | '\\' | '/') => out.push(c),
                c => {
                    out.push('\\');
                    out.push(c);
                }
            },
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VaultPath {
        VaultPath::new(s).expect("valid path")
    }

    #[test]
    fn json_strings_are_read_from_flat_objects() {
        let text = r#"{ "a": 1, "newFileLocation" : "current", "p": "x\"y" }"#;
        assert_eq!(
            json_string(text, "newFileLocation").as_deref(),
            Some("current")
        );
        assert_eq!(json_string(text, "p").as_deref(), Some("x\"y"));
        assert_eq!(json_string(text, "a"), None);
        assert_eq!(json_string(text, "missing"), None);
    }

    #[test]
    fn bare_link_notes_follow_the_setting() {
        let place = |loc: Option<NewFileLocation>, p: &str| {
            place_bare_link_note(loc.as_ref(), vp(p)).map(|p| p.as_str().to_owned())
        };
        assert_eq!(place(None, "Sub/New.md").as_deref(), Some("Sub/New.md"));
        assert_eq!(
            place(Some(NewFileLocation::Current), "Sub/New.md").as_deref(),
            Some("Sub/New.md")
        );
        assert_eq!(
            place(Some(NewFileLocation::Root), "Sub/New.md").as_deref(),
            Some("New.md")
        );
        assert_eq!(
            place(
                Some(NewFileLocation::Folder("/In/box/".into())),
                "Sub/New.md"
            )
            .as_deref(),
            Some("In/box/New.md")
        );
        assert_eq!(
            place(Some(NewFileLocation::Folder("../out".into())), "Sub/New.md").as_deref(),
            Some("New.md")
        );
    }

    #[test]
    fn folders_that_would_leave_the_vault_on_windows_are_the_root() {
        let place = |folder: &str| {
            place_bare_link_note(
                Some(&NewFileLocation::Folder(folder.into())),
                vp("Sub/New.md"),
            )
            .map(|p| p.as_str().to_owned())
        };
        for folder in [
            "C:\\Users\\x",
            "C:/Users/x",
            "..\\..\\x",
            "In\\box",
            "\\\\server\\share",
            "a/CON/b",
            "a/b:stream",
            "a\0b",
        ] {
            assert_eq!(place(folder).as_deref(), Some("New.md"), "{folder:?}");
        }
        assert_eq!(place("In/box").as_deref(), Some("In/box/New.md"));
    }

    #[test]
    fn new_note_folders_from_app_json_stay_in_the_vault() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".obsidian")).expect("mkdir");
        let set = |folder: &str| {
            std::fs::write(
                root.join(".obsidian").join("app.json"),
                format!(r#"{{"newFileLocation":"folder","newFileFolderPath":"{folder}"}}"#),
            )
            .expect("write app.json");
            new_note_folder(root, None)
        };
        assert_eq!(set(r"C:\\Users\\x"), "");
        assert_eq!(set(r"..\\..\\x"), "");
        assert_eq!(set("In/box"), "In/box");
    }

    #[test]
    fn joined_paths_must_stay_under_the_root() {
        let root = Path::new("vault");
        assert!(is_within(root, Path::new("vault")));
        assert!(is_within(root, &root.join("a").join("b.md")));
        assert!(!is_within(root, &root.join("..").join("x")));
        assert!(!is_within(root, Path::new("elsewhere/x")));
        assert!(!is_within(root, Path::new("/abs/x")));
    }
}

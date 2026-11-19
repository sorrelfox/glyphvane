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

use std::path::Path;

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
/// when it would leave the vault or has empty, `.` or `..` parts.
fn safe_folder(folder: &str) -> String {
    let folder = folder.trim_matches('/');
    if folder
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return String::new();
    }
    folder.to_owned()
}

/// The vault folder a new note made from nothing but a name goes to (New
/// note, the quick switcher), by the vault's `newFileLocation`: its root
/// (also when unset), the folder of `current` (a file in the vault), or
/// `newFileFolderPath`. Vault-relative and `/`-separated, empty for the
/// root; a folder that would leave the vault is the root.
#[must_use]
pub fn new_note_folder(root: &Path, current: Option<&Path>) -> String {
    let folder = match NewFileLocation::read(root) {
        Some(NewFileLocation::Folder(f)) => f,
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
    };
    safe_folder(&folder)
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
}

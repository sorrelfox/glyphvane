// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Where new attachments go, and what they are called (11 § Attachments).
//!
//! - **Folder.** `attachmentFolderPath` from the vault's
//!   `.obsidian/app.json`, else the `files.attachment_folder` setting:
//!   `/` is the vault root, `./` the note's folder, `./sub` a subfolder of
//!   it (made on demand), anything else a vault-relative folder. Without a
//!   note (an untitled document) `./` falls back to the vault root.
//! - **Names.** A pasted image is `Pasted image YYYYMMDDHHMMSS.png` (the
//!   Obsidian convention); a dropped or copied file keeps its name. Either
//!   way the name goes through the filename rules (NFC, no character a
//!   platform forbids or that breaks a wikilink, no trailing dot or space)
//!   and gets ` 1`, ` 2`, … when it is taken in any letter case.
//! - **Writing.** A new file is created, never replaced (`create_new`), so
//!   a name taken meanwhile is retried with the next suffix.
//! - **Links.** Images, audio, video and PDFs are embedded; other files
//!   linked ([`is_embeddable`]).

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use gv_vault::path::{extension_of, stem_of};

/// The file name stem of a pasted image.
pub(crate) const PASTED_IMAGE: &str = "Pasted image";

/// What a name becomes when nothing of it is left after cleaning.
const FALLBACK_NAME: &str = "Attachment";

/// The attachment folder setting of the vault at `root`: Obsidian's
/// `attachmentFolderPath` when its `app.json` has one, else `default`
/// (the `files.attachment_folder` setting).
pub(crate) fn folder_setting(root: &Path, default: &str) -> String {
    std::fs::read_to_string(root.join(".obsidian").join("app.json"))
        .ok()
        .and_then(|app| crate::autocomplete::json_string(&app, "attachmentFolderPath"))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// The folder new attachments of the note at `note` (absolute; `None`
/// for an untitled document) go to, in the vault at `root`, for the
/// attachment folder `setting`. Never outside `root`.
pub(crate) fn folder(root: &Path, note: Option<&Path>, setting: &str) -> PathBuf {
    let setting = setting.trim().replace('\\', "/");
    let note_dir = note
        .and_then(Path::parent)
        .filter(|d| d.starts_with(root))
        .unwrap_or(root);
    let (base, rest) = if setting == "/" || setting.is_empty() {
        (root, "")
    } else if setting == "." || setting == "./" {
        (note_dir, "")
    } else if let Some(rest) = setting.strip_prefix("./") {
        (note_dir, rest)
    } else {
        (root, setting.trim_start_matches('/'))
    };
    let mut dir = base.to_path_buf();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            // Never above the vault.
            ".." => {
                if dir != root {
                    dir.pop();
                }
            }
            part => dir.push(part),
        }
    }
    if dir.starts_with(root) {
        dir
    } else {
        root.to_path_buf()
    }
}

/// `name` made fit to create: NFC, characters some platform forbids or
/// that break wikilinks (`# ^ [ ] |`) replaced by `-`, no trailing dots
/// or spaces, at most 255 bytes.
pub(crate) fn clean_name(name: &str) -> String {
    let name = gv_vault::keys::nfc_key(name);
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_control()
                || matches!(
                    c,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '#' | '^' | '[' | ']'
                )
            {
                '-'
            } else {
                c
            }
        })
        .collect();
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    let out = out.trim_start().to_owned();
    let mut out = if out.is_empty() || out.chars().all(|c| c == '.') {
        FALLBACK_NAME.to_owned()
    } else {
        out
    };
    // Reserved Windows names get a suffix.
    let stem = stem_of(&out).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        out = match extension_of(&out) {
            Some(ext) => format!("{} file.{ext}", stem_of(&out)),
            None => format!("{out} file"),
        };
    }
    truncate_name(out, 255)
}

/// `name` cut to at most `max` bytes on a character boundary, keeping
/// the extension.
fn truncate_name(name: String, max: usize) -> String {
    if name.len() <= max {
        return name;
    }
    let ext = extension_of(&name)
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let mut stem = stem_of(&name).to_owned();
    while stem.len() + ext.len() > max {
        stem.pop();
    }
    format!("{stem}{ext}")
}

/// The name of an image pasted at `now` with extension `ext`.
pub(crate) fn pasted_image_name(now: DateTime<Local>, ext: &str) -> String {
    format!("{PASTED_IMAGE} {}.{ext}", now.format("%Y%m%d%H%M%S"))
}

/// Creates the file `name` (cleaned, with ` 1`, ` 2`, … when taken) in
/// `dir`, which is made when missing, and writes `bytes` to it. Returns
/// its path.
pub(crate) fn write_new(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let name = clean_name(name);
    let (stem, ext) = (
        stem_of(&name).to_owned(),
        extension_of(&name).map(str::to_owned),
    );
    // A name taken between the check and the creation is tried again.
    for _ in 0..16 {
        let unique = gv_vault::refactor::unique_name(dir, &stem, ext.as_deref(), false)?;
        let path = dir.join(&unique);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&path);
                    return Err(err);
                }
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("no free name for the attachment"))
}

/// Copies the file `source` into `dir` under its own name (cleaned, with
/// ` 1`, ` 2`, … when taken). Returns the copy's path.
pub(crate) fn copy_into(dir: &Path, source: &Path) -> io::Result<PathBuf> {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| FALLBACK_NAME.to_owned());
    if !fs::metadata(source)?.is_file() {
        return Err(io::Error::other(format!("“{name}” is not a file")));
    }
    let bytes = fs::read(source)?;
    write_new(dir, &name, &bytes)
}

/// Whether a link to `path` is written as an embed: images, audio,
/// video and PDFs.
pub(crate) fn is_embeddable(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "svg"
            | "bmp"
            | "avif"
            | "mp3"
            | "wav"
            | "m4a"
            | "ogg"
            | "flac"
            | "3gp"
            | "mp4"
            | "webm"
            | "ogv"
            | "mov"
            | "mkv"
            | "pdf"
    )
}

/// A `file://` URL of the absolute path `path`, percent-encoded.
pub(crate) fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let encoded: Vec<String> = text
        .split('/')
        .map(crate::autocomplete::encode_segment)
        .collect();
    let joined = encoded.join("/");
    if joined.starts_with('/') {
        format!("file://{joined}")
    } else {
        format!("file:///{joined}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_follows_the_setting() {
        let root = Path::new("/v");
        let note = Path::new("/v/a/b/Note.md");
        assert_eq!(folder(root, Some(note), "/"), Path::new("/v"));
        assert_eq!(folder(root, Some(note), "./"), Path::new("/v/a/b"));
        assert_eq!(folder(root, Some(note), "./img"), Path::new("/v/a/b/img"));
        assert_eq!(
            folder(root, Some(note), "Files/Att"),
            Path::new("/v/Files/Att")
        );
        assert_eq!(folder(root, Some(note), "/Files"), Path::new("/v/Files"));
        assert_eq!(folder(root, None, "./"), Path::new("/v"));
        // Never outside the vault.
        assert_eq!(folder(root, Some(note), "../../../.."), Path::new("/v"));
        assert_eq!(folder(root, Some(note), "./../../../x"), Path::new("/v/x"));
    }

    #[test]
    fn obsidian_settings_win_over_the_default() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(folder_setting(dir.path(), "./att"), "./att");
        fs::create_dir(dir.path().join(".obsidian")).expect("mkdir");
        fs::write(
            dir.path().join(".obsidian/app.json"),
            r#"{"attachmentFolderPath": "Assets", "x": 1}"#,
        )
        .expect("write");
        assert_eq!(folder_setting(dir.path(), "./att"), "Assets");
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("a#b[c]|d^e.png"), "a-b-c--d-e.png");
        assert_eq!(clean_name("x: y?.pdf"), "x- y-.pdf");
        assert_eq!(clean_name("name. "), "name");
        assert_eq!(clean_name("..."), FALLBACK_NAME);
        assert_eq!(clean_name("CON.txt"), "CON file.txt");
        assert_eq!(clean_name("Cafe\u{301}.png"), "Caf\u{e9}.png");
        let long = format!("{}.png", "a".repeat(300));
        assert_eq!(clean_name(&long).len(), 255);
        assert!(clean_name(&long).ends_with(".png"));
    }

    #[test]
    fn new_files_get_free_names() {
        let dir = tempfile::tempdir().expect("dir");
        let att = dir.path().join("att");
        let a = write_new(&att, "pic.png", b"1").expect("write");
        let b = write_new(&att, "PIC.png", b"2").expect("write");
        assert_eq!(a.file_name().unwrap(), "pic.png");
        assert_eq!(b.file_name().unwrap(), "PIC 1.png");
        assert_eq!(fs::read(&a).unwrap(), b"1");
        let src = dir.path().join("doc.pdf");
        fs::write(&src, b"pdf").expect("write");
        let copied = copy_into(&att, &src).expect("copy");
        assert_eq!(copied, att.join("doc.pdf"));
        assert_eq!(fs::read(copied).unwrap(), b"pdf");
        assert!(copy_into(&att, dir.path()).is_err());
    }

    #[test]
    fn pasted_images_are_named_by_time() {
        use chrono::TimeZone as _;
        let now = Local
            .with_ymd_and_hms(2026, 11, 17, 9, 5, 3)
            .single()
            .expect("time");
        assert_eq!(
            pasted_image_name(now, "png"),
            "Pasted image 20261117090503.png"
        );
    }

    #[test]
    fn embeds_and_urls() {
        assert!(is_embeddable(Path::new("a.PNG")));
        assert!(is_embeddable(Path::new("a.pdf")));
        assert!(!is_embeddable(Path::new("a.zip")));
        assert_eq!(
            file_url(Path::new("/home/x/My file.zip")),
            "file:///home/x/My%20file.zip"
        );
    }
}

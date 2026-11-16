// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Local images of an export: read, checked and inlined as `data:` URIs,
//! or listed for an `assets/` folder beside the HTML file.
//!
//! Only files inside the vault root (or, without a root, the document's
//! folder) are read, never through a network path, and only when their
//! first bytes are a known image format. Each image is capped, and so is
//! their total; what is not embedded keeps its relative reference and is
//! reported. Remote images are never fetched here.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use crate::ExportOptions;
use crate::escape::base64;

/// How local images are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageMode {
    /// Inlined as `data:` URIs: one self-contained file.
    #[default]
    Embed,
    /// Copied to `assets/` beside the HTML file ([`Asset`]s).
    Assets,
}

/// Caps on embedded images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest image embedded, in bytes (the URL policy's cap on
    /// `data:` images).
    pub image_bytes: u64,
    /// The most image bytes one export embeds.
    pub total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            image_bytes: 10 * 1024 * 1024,
            total_bytes: 64 * 1024 * 1024,
        }
    }
}

/// A file to copy beside the exported HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// The image file.
    pub source: PathBuf,
    /// Where it goes, relative to the HTML file (`assets/N-name.ext`).
    pub target: String,
}

/// An image that was not embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageNote {
    /// The reference as written.
    pub src: String,
    pub reason: String,
}

#[derive(Default)]
struct State {
    total: u64,
    assets: Vec<Asset>,
    notes: Vec<ImageNote>,
    /// Embedded or copied already, by resolved path: the same image twice
    /// is read once.
    done: Vec<(PathBuf, String)>,
}

/// The images of one export. Shared with the sanitizer's attribute filter,
/// which runs on ammonia's side.
pub(crate) struct Store {
    base: Option<PathBuf>,
    root: Option<PathBuf>,
    mode: ImageMode,
    limits: Limits,
    state: Mutex<State>,
}

impl Store {
    pub(crate) fn new(opts: &ExportOptions, mode: ImageMode) -> Self {
        let base = opts.base_dir().map(Path::to_path_buf);
        let root = opts.root.clone().or_else(|| base.clone());
        Self {
            base: base.and_then(|b| std::fs::canonicalize(&b).ok().or(Some(b))),
            root: root.and_then(|r| std::fs::canonicalize(&r).ok()),
            mode,
            limits: opts.limits,
            state: Mutex::new(State::default()),
        }
    }

    /// The assets to copy and the images not embedded.
    pub(crate) fn finish(&self) -> (Vec<Asset>, Vec<ImageNote>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            std::mem::take(&mut state.assets),
            std::mem::take(&mut state.notes),
        )
    }

    fn note(&self, state: &mut State, src: &str, reason: impl Into<String>) {
        state.notes.push(ImageNote {
            src: src.to_owned(),
            reason: reason.into(),
        });
    }

    /// What a relative image reference `src` becomes: a `data:` URI, an
    /// `assets/` path, the reference itself (not embedded, reported), or
    /// `None` when it must not be referenced at all (outside the root).
    pub(crate) fn image(&self, src: &str) -> Option<String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(base) = &self.base else {
            self.note(&mut state, src, "the document has no folder");
            return Some(src.to_owned());
        };
        let Some(rel) = decode_path(src) else {
            self.note(&mut state, src, "not a plain relative path");
            return None;
        };
        let path = base.join(&rel);
        let Ok(path) = std::fs::canonicalize(&path) else {
            self.note(&mut state, src, "file not found");
            return Some(src.to_owned());
        };
        if let Some(root) = &self.root
            && !path.starts_with(root)
        {
            self.note(&mut state, src, "outside the vault");
            return None;
        }
        if let Some((_, done)) = state.done.iter().find(|(p, _)| *p == path) {
            return Some(done.clone());
        }
        let size = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or(u64::MAX);
        if size > self.limits.image_bytes {
            self.note(&mut state, src, "larger than the image cap");
            return Some(src.to_owned());
        }
        if self.mode == ImageMode::Embed && state.total + size > self.limits.total_bytes {
            self.note(&mut state, src, "the export's image cap is reached");
            return Some(src.to_owned());
        }
        let Ok(bytes) = std::fs::read(&path) else {
            self.note(&mut state, src, "cannot be read");
            return Some(src.to_owned());
        };
        let Some(mime) = sniff(&bytes) else {
            self.note(&mut state, src, "not a known image format");
            return Some(src.to_owned());
        };
        let out = match self.mode {
            ImageMode::Embed => {
                state.total += bytes.len() as u64;
                format!("data:{mime};base64,{}", base64(&bytes))
            }
            ImageMode::Assets => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let safe: String = name
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect();
                let target = format!("assets/{}-{safe}", state.assets.len() + 1);
                state.assets.push(Asset {
                    source: path.clone(),
                    target: target.clone(),
                });
                target
            }
        };
        state.done.push((path, out.clone()));
        Some(out)
    }
}

/// A relative reference as a relative path: percent-decoded, without a
/// query or fragment; `None` for anything else (absolute paths, drive
/// letters, backslashes, encoded separators).
fn decode_path(src: &str) -> Option<PathBuf> {
    let src = src.split(['?', '#']).next().unwrap_or("");
    if src.is_empty() || src.starts_with('/') || src.contains('\\') {
        return None;
    }
    let mut bytes = Vec::with_capacity(src.len());
    let raw = src.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            let hex = std::str::from_utf8(raw.get(i + 1..i + 3)?).ok()?;
            let b = u8::from_str_radix(hex, 16).ok()?;
            if matches!(b, b'/' | b'\\' | 0) {
                return None;
            }
            bytes.push(b);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    let path = PathBuf::from(decoded);
    if path
        .components()
        .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return None;
    }
    Some(path)
}

/// The image type of `bytes`, from their signature.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let starts = |sig: &[u8]| bytes.starts_with(sig);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if starts(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
        && matches!(&bytes[8..12], b"avif" | b"avis")
    {
        Some("image/avif")
    } else if is_svg(bytes) {
        // In an `<img>`, an SVG runs no script and loads nothing.
        Some("image/svg+xml")
    } else {
        None
    }
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let Ok(head) =
        std::str::from_utf8(head).or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]))
    else {
        return false;
    };
    let head = head.trim_start_matches('\u{feff}').trim_start();
    (head.starts_with("<?xml") || head.starts_with("<svg") || head.starts_with("<!--"))
        && head.contains("<svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_only() {
        assert_eq!(
            decode_path("img/a%20b.png?x#y"),
            Some(PathBuf::from("img/a b.png"))
        );
        assert_eq!(decode_path("../up.png"), Some(PathBuf::from("../up.png")));
        for bad in [
            "/abs.png",
            "a\\b.png",
            "a%2Fb.png",
            "a%5cb.png",
            "%00",
            "",
            "%zz",
        ] {
            assert_eq!(decode_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn formats_are_sniffed() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(
            sniff(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            Some("image/svg+xml")
        );
        assert_eq!(sniff(b"<html><script>"), None);
        assert_eq!(sniff(b"plain text"), None);
    }
}

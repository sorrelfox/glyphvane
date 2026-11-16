// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Colour emoji on Linux.
//!
//! GPUI's Linux rasterizer draws colour emoji only from the face whose
//! PostScript name is `NotoColorEmoji`, and only from CBDT or sbix bitmaps or
//! COLR version 0 layers. Some distributions install that font only as COLRv1,
//! which draws nothing. When no installed face is drawable, the app adds the
//! CBDT build of Noto Color Emoji that the packages install next to the
//! executable. It has the same PostScript name, so it replaces the installed
//! face for the whole process. Otherwise the installed font is left alone.
//!
//! The system fonts are scanned on a background thread, so start-up does not
//! wait: emoji drawn before the font is added are drawn again once it is.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use gpui::App;
use skrifa::Tag;
use skrifa::raw::{FontRef, TableProvider as _};

/// The PostScript name of the one face GPUI draws colour emoji from.
const EMOJI_POSTSCRIPT_NAME: &str = "NotoColorEmoji";
/// The bundled font's file name.
const BUNDLED_FILE: &str = "NotoColorEmoji.ttf";
/// Where the packages install the bundled font, relative to the folder that
/// holds the executable: next to it in the tarball, `../share` in the deb,
/// rpm and AppImage.
const BUNDLED_DIRS: &[&str] = &["share/glyphvane/fonts", "../share/glyphvane/fonts"];

/// Adds the bundled colour emoji font if no installed one can be drawn.
pub fn install(cx: &mut App) {
    let task = cx
        .background_executor()
        .spawn(async { bundled_font_if_needed() });
    cx.spawn(async move |cx| {
        let Some((path, bytes)) = task.await else {
            return;
        };
        cx.update(
            |cx| match cx.text_system().add_fonts(vec![Cow::Owned(bytes)]) {
                Ok(()) => {
                    tracing::info!(
                        "emoji: using the bundled colour emoji font {}",
                        path.display()
                    );
                    cx.refresh_windows();
                }
                Err(err) => {
                    tracing::warn!("emoji: cannot add {}: {err:#}", path.display());
                }
            },
        );
    })
    .detach();
}

/// The bundled font's path and contents, if the installed fonts have no
/// drawable colour emoji face and the bundled font is found.
fn bundled_font_if_needed() -> Option<(PathBuf, Vec<u8>)> {
    let started = std::time::Instant::now();
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let usable = installed_emoji_usable(&db);
    tracing::debug!(
        "emoji: scanned {} installed faces in {:?}; drawable colour emoji: {usable}",
        db.len(),
        started.elapsed()
    );
    if usable {
        return None;
    }
    let Some(path) = bundled_font_path() else {
        tracing::warn!(
            "emoji: the installed colour emoji font cannot be drawn and {BUNDLED_FILE} is not \
             installed with the application; emoji will not show"
        );
        return None;
    };
    match std::fs::read(&path) {
        Ok(bytes) if is_drawable_emoji_font(&bytes, 0) => Some((path, bytes)),
        Ok(_) => {
            tracing::warn!("emoji: {} is not a CBDT colour emoji font", path.display());
            None
        }
        Err(err) => {
            tracing::warn!("emoji: cannot read {}: {err}", path.display());
            None
        }
    }
}

/// Whether the installed `NotoColorEmoji` faces can all be drawn. `false`
/// when there is none: GPUI draws other emoji fonts without colour.
fn installed_emoji_usable(db: &fontdb::Database) -> bool {
    let mut faces = db
        .faces()
        .filter(|face| face.post_script_name == EMOJI_POSTSCRIPT_NAME)
        .peekable();
    faces.peek().is_some()
        && faces.all(|face| {
            db.with_face_data(face.id, is_drawable_emoji_font)
                .unwrap_or(false)
        })
}

/// Whether GPUI's rasterizer (swash) can draw colour glyphs from this face:
/// CBDT or sbix bitmaps, or COLR version 0 layers. COLRv1 paint graphs and
/// SVG glyphs are not supported.
fn is_drawable_emoji_font(data: &[u8], index: u32) -> bool {
    let Ok(font) = FontRef::from_index(data, index) else {
        return false;
    };
    let has = |tag: &[u8; 4]| font.data_for_tag(Tag::new(tag)).is_some();
    (has(b"CBDT") && has(b"CBLC"))
        || has(b"sbix")
        || font
            .colr()
            .is_ok_and(|colr| colr.num_base_glyph_records() > 0 && has(b"CPAL"))
}

/// The bundled font next to the executable, or, in debug builds, in the
/// source tree.
fn bundled_font_path() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let installed = exe_dir.iter().flat_map(|dir| {
        BUNDLED_DIRS
            .iter()
            .map(move |rel| dir.join(rel).join(BUNDLED_FILE))
    });
    let source_tree = cfg!(debug_assertions).then(source_tree_font);
    installed.chain(source_tree).find(|path| path.is_file())
}

/// The font as vendored in the source tree.
fn source_tree_font() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/noto-emoji")
        .join(BUNDLED_FILE)
}

#[cfg(test)]
mod tests {
    use skrifa::MetadataProvider as _;

    use super::*;

    #[test]
    fn the_bundled_font_is_drawable() {
        let bytes = std::fs::read(source_tree_font()).expect("the vendored emoji font");
        assert!(is_drawable_emoji_font(&bytes, 0));
        let font = FontRef::new(&bytes).expect("a font");
        let names: Vec<String> = font
            .localized_strings(skrifa::string::StringId::POSTSCRIPT_NAME)
            .map(|name| name.to_string())
            .collect();
        assert!(
            names.iter().any(|name| name == EMOJI_POSTSCRIPT_NAME),
            "{names:?}"
        );
    }

    #[test]
    fn other_data_is_not_drawable() {
        assert!(!is_drawable_emoji_font(b"not a font", 0));
        assert!(!is_drawable_emoji_font(&[], 0));
    }

    /// The COLRv1-only build that some distributions install (Fedora's
    /// `google-noto-color-emoji-fonts`). Skipped where it is not installed.
    #[test]
    fn colrv1_only_noto_is_not_drawable() {
        let path = Path::new("/usr/share/fonts/google-noto-color-emoji-fonts/Noto-COLRv1.ttf");
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        assert!(!is_drawable_emoji_font(&bytes, 0));
    }

    #[test]
    fn an_empty_database_has_no_usable_emoji() {
        assert!(!installed_emoji_usable(&fontdb::Database::new()));
    }

    #[test]
    fn the_bundled_font_alone_is_usable() {
        let mut db = fontdb::Database::new();
        db.load_font_file(source_tree_font())
            .expect("the vendored emoji font");
        assert!(installed_emoji_usable(&db));
    }
}

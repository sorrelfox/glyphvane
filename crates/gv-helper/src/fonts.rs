// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The fonts PDF export lays text out with.
//!
//! The packages ship Inter and JetBrains Mono (variable, OFL-1.1) in
//! `share/glyphvane/fonts/`: next to the helper in the tarball and the
//! Windows zip, `../share/` in the AppImage, `../../share/` from
//! `/usr/libexec/glyphvane/` in the deb and rpm (`/usr/lib/glyphvane/` in
//! the Arch package), and
//! `Contents/Resources/fonts/` in the macOS bundle. A family the bundled
//! faces have is taken from them only, never from the installed fonts, so
//! the same note gives the same PDF on every machine. Families they lack
//! (CJK, symbols, emoji) come from the installed fonts, scanned only when a
//! document asks for one.
//!
//! [`FONTS_DIR_ENV`] names another folder to look in first; development
//! builds also look in the repository's `third_party/` folders.

use std::path::{Path, PathBuf};

/// The bundled font files.
pub const BUNDLED_FILES: &[&str] = &[
    "InterVariable.ttf",
    "InterVariable-Italic.ttf",
    "JetBrainsMono-Variable.ttf",
    "JetBrainsMono-Variable-Italic.ttf",
];

/// The families of the bundled files.
pub const BUNDLED_FAMILIES: &[&str] = &["Inter Variable", "JetBrains Mono"];

/// An environment variable naming a folder of bundled fonts, looked in
/// before the installed locations.
pub const FONTS_DIR_ENV: &str = "GLYPHVANE_FONTS_DIR";

/// Where the packages put the fonts, relative to the folder of the helper.
const INSTALLED_DIRS: &[&str] = &[
    "share/glyphvane/fonts",
    "../share/glyphvane/fonts",
    "../../share/glyphvane/fonts",
    "../Resources/fonts",
];

/// The folders searched for the bundled files, in order.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os(FONTS_DIR_ENV).filter(|d| !d.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        dirs.extend(INSTALLED_DIRS.iter().map(|rel| exe_dir.join(rel)));
    }
    if cfg!(debug_assertions) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party");
        dirs.push(repo.join("inter"));
        dirs.push(repo.join("jetbrains-mono"));
    }
    dirs
}

/// The bundled files found in `dirs`: each file from the first folder
/// that has it.
pub fn find_bundled(dirs: &[PathBuf]) -> Vec<PathBuf> {
    BUNDLED_FILES
        .iter()
        .filter_map(|name| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn development_builds_find_every_bundled_font() {
        let found = find_bundled(&search_dirs());
        assert_eq!(found.len(), BUNDLED_FILES.len(), "{found:?}");
    }

    #[test]
    fn the_first_folder_wins() {
        let a = tempfile::tempdir().expect("dir");
        let b = tempfile::tempdir().expect("dir");
        std::fs::write(a.path().join(BUNDLED_FILES[0]), b"a").expect("write");
        std::fs::write(b.path().join(BUNDLED_FILES[0]), b"b").expect("write");
        std::fs::write(b.path().join(BUNDLED_FILES[1]), b"b").expect("write");
        let found = find_bundled(&[a.path().to_path_buf(), b.path().to_path_buf()]);
        assert_eq!(
            found,
            vec![
                a.path().join(BUNDLED_FILES[0]),
                b.path().join(BUNDLED_FILES[1])
            ]
        );
    }
}

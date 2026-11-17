// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The fonts PDF export lays text out with (Inter and JetBrains Mono,
//! variable, OFL-1.1, from `third_party/`), installed with the helper in
//! `share/glyphvane/fonts/` (`Contents/Resources/fonts/` on macOS), where
//! `glyphvane-helper` looks for them. Only packaged with the helper, the
//! only program that reads them.

use super::Ctx;
use super::archive::Tree;

/// Each font file and licence: its path in the repository and its name in
/// the fonts folder. The deb, rpm and MSI list the same files in
/// `crates/gv-app/Cargo.toml` and `packaging/windows/glyphvane.wxs`.
pub(super) const PDF_FONTS: &[(&str, &str)] = &[
    ("third_party/inter/InterVariable.ttf", "InterVariable.ttf"),
    (
        "third_party/inter/InterVariable-Italic.ttf",
        "InterVariable-Italic.ttf",
    ),
    ("third_party/inter/LICENSE", "Inter-OFL.txt"),
    (
        "third_party/jetbrains-mono/JetBrainsMono-Variable.ttf",
        "JetBrainsMono-Variable.ttf",
    ),
    (
        "third_party/jetbrains-mono/JetBrainsMono-Variable-Italic.ttf",
        "JetBrainsMono-Variable-Italic.ttf",
    ),
    (
        "third_party/jetbrains-mono/LICENSE",
        "JetBrainsMono-OFL.txt",
    ),
];

/// Adds the PDF fonts in `dir` (no trailing slash) when the helper is
/// packaged.
pub(super) fn add_pdf_fonts(ctx: &Ctx, tree: &mut Tree, dir: &str) {
    if ctx.helper_exe.is_none() {
        return;
    }
    for (source, name) in PDF_FONTS {
        tree.file(&format!("{dir}/{name}"), &ctx.root.join(source), 0o644);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::PDF_FONTS;

    #[test]
    fn every_font_file_is_in_the_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for (source, _) in PDF_FONTS {
            assert!(root.join(source).is_file(), "{source}");
        }
    }
}

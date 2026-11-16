// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! HTML export and rich copy with the app's theme, diagram engine and link
//! resolver (FR-EXPORT-01, -04, -06), for the window's commands and for
//! `glyphvane export`.
//!
//! - Exports are styled with the **light** theme's tokens, whatever theme
//!   the window shows: a print-friendly page.
//! - Diagrams are drawn by the Reading view's engine in the light theme's
//!   colours, with their text as outlines.
//! - Links get their status from the vault's resolver: unresolved ones
//!   become plain text.
//! - Rich text goes to the clipboard through a [`RichClipboard`] the
//!   application installs ([`set_rich_clipboard`]): GPUI's clipboard has
//!   no HTML flavour. Without one, or when it fails (a Wayland compositor
//!   without the data-control protocol and no X server), the copy is plain
//!   text and the window says so.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Global};
use gv_diagram::{Outcome, Palette as DiagramPalette, Trust};
use gv_export::{DiagramRenderer, Export, ExportOptions, ExportTheme, RichCopy};
use gv_model::DocModel;
use gv_profile::{CompiledProfile, ProfileId};
use gv_render_tree::{AllPending, NoImageSizes, RenderOptions, RenderTree, Sources};
use gv_syntax::ParseOptions;
use gv_vault::PathIndex;

use crate::inline::Token;
use crate::reader::Resolver;
use crate::theme::ThemeId;

/// The theme every export is styled with.
const EXPORT_THEME: ThemeId = ThemeId::Light;

/// The export theme: the light theme's colours by token name.
pub fn theme() -> ExportTheme {
    let theme = EXPORT_THEME.theme();
    ExportTheme::from_fn(|name| {
        Token::ALL.iter().find(|t| t.name() == name).map(|t| {
            let c = t.color(theme);
            [c.r, c.g, c.b, c.a]
        })
    })
}

/// Diagrams through the Reading view's engine, in the export theme.
struct Diagrams;

impl DiagramRenderer for Diagrams {
    fn svg(&self, source: &str) -> Option<String> {
        let engine = crate::reader::diagram::diagram_engine()?;
        match engine.render(source, Trust::Interactive) {
            Outcome::Rendered(diagram) => {
                let theme = EXPORT_THEME.theme();
                let palette = DiagramPalette::from_fn(|role| {
                    let c = crate::inline::diagram::token_of(role).color(theme);
                    [c.r, c.g, c.b, c.a]
                });
                Some(diagram.to_svg(&palette, true))
            }
            _ => None,
        }
    }
}

/// The render tree of `text` (the document at `path`, if it has one) as
/// an export sees it: parsed in `profile`, its links resolved through
/// `index` when the file is in that vault.
pub fn document_tree(
    text: &str,
    path: Option<&Path>,
    profile: &CompiledProfile,
    index: Option<&PathIndex>,
) -> RenderTree {
    let syntax = gv_syntax::parse_with(text, ParseOptions::from_profile(profile));
    let model = DocModel::build(&syntax, text);
    let options = RenderOptions::for_profile(profile);
    match path {
        Some(path) => {
            let links = Resolver::statuses(path, index, profile.id);
            let sources = Sources {
                links: &links,
                images: &NoImageSizes,
            };
            RenderTree::build(&syntax, &model, text, options, sources)
        }
        None => {
            let sources = Sources {
                links: &AllPending,
                images: &NoImageSizes,
            };
            RenderTree::build(&syntax, &model, text, options, sources)
        }
    }
}

/// The export options for the document at `path` in `profile`: images
/// inlined (within the vault `root`, if given), `style` dropped in
/// GitHub profiles as GitHub does, diagrams drawn.
pub fn options(path: Option<&Path>, root: Option<&Path>, profile: ProfileId) -> ExportOptions {
    let mut opts = ExportOptions::new(path.map(Path::to_path_buf), theme());
    opts.root = root.map(Path::to_path_buf);
    opts.strip_style = matches!(profile, ProfileId::GfmFile | ProfileId::GfmComment);
    opts.diagrams = Some(Arc::new(Diagrams));
    opts
}

/// Exports `text` (the document at `path`) as a standalone HTML file.
pub fn export_text(
    text: &str,
    path: Option<&Path>,
    root: Option<&Path>,
    profile: &CompiledProfile,
    index: Option<&PathIndex>,
) -> Export {
    let tree = document_tree(text, path, profile, index);
    gv_export::export_html(&tree, &options(path, root, profile.id))
}

/// The source range `range` of `text` as rich text.
pub fn rich_copy(
    text: &str,
    range: Range<usize>,
    path: Option<&Path>,
    profile: &CompiledProfile,
    index: Option<&PathIndex>,
) -> RichCopy {
    let tree = document_tree(text, path, profile, index);
    let root = index.map(|i| i.root().to_path_buf());
    gv_export::rich_copy(&tree, range, &options(path, root.as_deref(), profile.id))
}

/// `path` with `.html` when its name has no extension.
pub fn with_html_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|e| !e.is_empty()) {
        path
    } else {
        path.with_extension("html")
    }
}

/// The file name an export of the document at `path` suggests.
pub fn suggested_name(path: Option<&Path>) -> String {
    let stem = path.and_then(Path::file_stem).map_or_else(
        || "Untitled".to_owned(),
        |s| s.to_string_lossy().into_owned(),
    );
    format!("{stem}.html")
}

/// Writes `export` to `path` (and its assets beside it) so that readers
/// see the old file or the new one, never a partial write: the bytes go
/// to a temporary file in the same folder, which is then renamed over
/// `path`.
pub fn write_export(path: &Path, export: &Export) -> io::Result<()> {
    write_atomically(path, export.html.as_bytes())?;
    let folder = path.parent().unwrap_or(Path::new("."));
    for asset in &export.assets {
        let target = folder.join(&asset.target);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&asset.source, &target)?;
    }
    Ok(())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "it is a folder",
        ));
    }
    let (temp, mut file) = create_beside(path, ".tmp")?;
    let written = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if written.is_err() {
        // Best effort: the temporary file is ours and useless now.
        let _ = fs::remove_file(&temp);
    }
    written
}

/// A new, empty file next to `target`, named after it.
fn create_beside(target: &Path, suffix: &str) -> io::Result<(PathBuf, File)> {
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path names no file"))?;
    for attempt in 0..100u32 {
        let mut temp_name = OsString::from(".");
        temp_name.push(name);
        temp_name.push(format!(".{}-{attempt}{suffix}", std::process::id()));
        let temp = dir.join(temp_name);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("no temporary file name is free"))
}

/// Writes an export for *Open in browser*: in a new private folder in the
/// system's temporary folder (so no other user can swap the file), named
/// after the document.
pub fn write_temporary(export: &Export, name: &str) -> io::Result<PathBuf> {
    let base = std::env::temp_dir();
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    let mut seed = {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.finish()
    };
    for _ in 0..100 {
        let dir = base.join(format!("glyphvane-export-{seed:016x}"));
        match builder.create(&dir) {
            Ok(()) => {
                let path = dir.join(name);
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                file.write_all(export.html.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            }
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::other("no temporary folder name is free"))
}

/// Puts rich text on the system clipboard.
pub trait RichClipboard {
    /// Writes `html` with `text` as its plain-text alternative, or says why
    /// it could not.
    fn write(&self, html: &str, text: &str) -> Result<(), String>;
}

/// The clipboard [`set_rich_clipboard`] installed.
struct InstalledClipboard(Rc<dyn RichClipboard>);

impl Global for InstalledClipboard {}

/// Uses `clipboard` for *Copy as rich text*; `None` copies plain text only.
pub fn set_rich_clipboard(cx: &mut App, clipboard: Option<Rc<dyn RichClipboard>>) {
    match clipboard {
        Some(clipboard) => cx.set_global(InstalledClipboard(clipboard)),
        None => {
            if cx.has_global::<InstalledClipboard>() {
                cx.remove_global::<InstalledClipboard>();
            }
        }
    }
}

/// The installed rich-text clipboard.
pub(crate) fn rich_clipboard(cx: &App) -> Option<Rc<dyn RichClipboard>> {
    cx.try_global::<InstalledClipboard>().map(|c| c.0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_theme_is_the_light_one() {
        let theme = theme();
        let light = ThemeId::Light.theme().text.primary;
        assert_eq!(
            theme.color("text.primary"),
            Some([light.r, light.g, light.b, light.a])
        );
        for name in gv_export::THEME_TOKENS {
            assert!(theme.color(name).is_some(), "{name}");
        }
    }

    #[test]
    fn names_and_extensions() {
        assert_eq!(suggested_name(Some(Path::new("/a/Note.md"))), "Note.html");
        assert_eq!(suggested_name(None), "Untitled.html");
        assert_eq!(
            with_html_extension(PathBuf::from("/a/x")),
            PathBuf::from("/a/x.html")
        );
        assert_eq!(
            with_html_extension(PathBuf::from("/a/x.htm")),
            PathBuf::from("/a/x.htm")
        );
    }

    #[test]
    fn diagrams_are_drawn_in_the_export() {
        let profile = gv_profile::compile_builtin(ProfileId::Extended);
        let text = "```mermaid\ngraph TD; A-->B\n```\n\n```mermaid\ngraph TD; C-->D\n```\n";
        let export = export_text(text, None, None, &profile, None);
        assert_eq!(
            export
                .html
                .matches("<div class=\"gv-diagram\"><svg")
                .count(),
            2,
            "{}",
            export.html
        );
        assert!(!export.html.contains("gv-diagram-source"));
    }

    #[test]
    fn exports_are_written_whole() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("out.html");
        let export = Export {
            html: "<p>x</p>".into(),
            assets: Vec::new(),
            notes: Vec::new(),
        };
        write_export(&path, &export).expect("write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "<p>x</p>");
        assert_eq!(fs::read_dir(dir.path()).expect("list").count(), 1);
        assert!(write_export(dir.path(), &export).is_err());
        let temp = write_temporary(&export, "Note.html").expect("temporary");
        assert_eq!(fs::read_to_string(&temp).expect("read"), "<p>x</p>");
        fs::remove_dir_all(temp.parent().expect("folder")).expect("clean up");
    }
}

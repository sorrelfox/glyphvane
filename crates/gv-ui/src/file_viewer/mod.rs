// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The built-in viewers for linked files that are not notes (FR-VIEW-11;
//! plan 09 § Special targets): text and code files, images and PDFs.
//!
//! A [`FileViewer`] takes the place of the Reading view in a tab whose
//! file is not a note, a canvas or a base. What it shows is decided when
//! the file is loaded, off the UI thread:
//!
//! - **Images** ([`ViewerKind::Image`], by extension): decoded by the
//!   media engine the Reading view uses (`gv-media`), at most
//!   [`IMAGE_MAX_SIDE`] device pixels a side. Fit (the default, never
//!   enlarged), 100 % (one image pixel per device pixel), zoom steps and
//!   Ctrl-wheel (Cmd-wheel on macOS) zoom; dragging and the wheel pan.
//!   Keys ([`key_bindings`]): `=`/`+` and `-` zoom, `0` fits, `1` shows
//!   100 %; `view.zoom_in`, `view.zoom_out` and `view.zoom_reset` (fit)
//!   work too.
//! - **Text and code** ([`ViewerKind::Text`] by extension or name, or any
//!   file whose first bytes look like text): read up to
//!   [`TEXT_MAX_BYTES`] and shown read-only in a Reading view as one code
//!   block, highlighted by the language its extension names (the shared
//!   highlighter, with its own size caps). Selection, copy and Find work
//!   as in a note. The tab's Edit mode edits the file in the Source view
//!   as plain text; [`FileViewer::show_text`] then shows the edited text.
//! - **PDF** ([`ViewerKind::Pdf`]): read up to [`PDF_MAX_BYTES`] and handed
//!   to the PDF engine the application installs (the helper process),
//!   which says how many pages it has and how large they are; the pages
//!   then show in a [`PdfViewer`] ([`pdf`]). A `#page=N` link fragment
//!   shows that page ([`FileViewer::go_to_page`]).
//! - Anything else, a text file over the cap, an image that does not
//!   decode, or a PDF the engine cannot read (encrypted, damaged, too
//!   large, too slow, no engine) says why, with *Open with default app*
//!   (the window asks first, as for any file it does not show).
//!
//! Not designed (flagged): the image and PDF toolbars, the zoom readout
//! and the states.

pub mod pdf;
#[cfg(test)]
pub(crate) mod pdf_tests;
#[cfg(test)]
mod tests;

pub use pdf::{PdfPlace, PdfViewer, PdfZoom};

use std::cell::Cell;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, EventEmitter, FocusHandle, Focusable,
    FontWeight, ImageSource, InteractiveElement as _, IntoElement, KeyBinding, Modifiers,
    MouseButton, ObjectFit, ParentElement as _, Pixels, Point, Render, RenderImage, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, Window,
    canvas, div, img, point, px,
};
use gv_media::{CancelFlag, DecodeRequest, Probe};

use crate::a11y::{A11yExt as _, AccessibleExt as _, Live, Role, Toggled};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::actions::view::{ZoomIn, ZoomOut, ZoomReset};
use crate::metrics::size::infobar as size;
use crate::metrics::spacing::infobar as spacing;
use crate::metrics::{icon, radius, size as sizes, text};
use crate::reader::{LoadStatus, Reader};
use crate::theme::Theme;

/// The largest text file shown.
pub const TEXT_MAX_BYTES: u64 = 4 << 20;

/// The bytes read to tell text from binary.
const SNIFF_BYTES: usize = 8 << 10;

/// The longest side an image is decoded at, in pixels; larger images are
/// scaled down to it (100 % then shows that).
pub const IMAGE_MAX_SIDE: u32 = 8192;

/// The largest PDF shown, in bytes (under the helper's frame cap).
pub const PDF_MAX_BYTES: u64 = 200 << 20;

/// The zoom steps of the image viewer (image pixels to device pixels).
const ZOOM_STEPS: [f32; 12] = [
    0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 8.0,
];

/// Pixels of one arrow-key pan.
const PAN_STEP: f32 = 48.0;

/// The key context of the image viewer.
pub(crate) const KEY_CONTEXT: &str = "ImageViewer";

gpui::actions!(
    image_viewer,
    [
        /// Show the whole image.
        Fit,
        /// One image pixel per screen pixel.
        ActualSize,
        /// Enlarge the image.
        ZoomInImage,
        /// Shrink the image.
        ZoomOutImage,
        /// Move the view left.
        PanLeft,
        /// Move the view right.
        PanRight,
        /// Move the view up.
        PanUp,
        /// Move the view down.
        PanDown,
    ]
);

/// The image viewer's keys.
pub fn key_bindings() -> Vec<KeyBinding> {
    let c = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("0", Fit, c),
        KeyBinding::new("1", ActualSize, c),
        KeyBinding::new("=", ZoomInImage, c),
        KeyBinding::new("+", ZoomInImage, c),
        KeyBinding::new("shift-=", ZoomInImage, c),
        KeyBinding::new("-", ZoomOutImage, c),
        KeyBinding::new("left", PanLeft, c),
        KeyBinding::new("right", PanRight, c),
        KeyBinding::new("up", PanUp, c),
        KeyBinding::new("down", PanDown, c),
    ]
}

/// What a file is shown as, as far as its name tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerKind {
    Text,
    Image,
    Pdf,
}

/// Image extensions the media engine decodes here.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg", "tif", "tiff",
];

/// Text and code extensions, shown in the text viewer.
const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "text",
    "log",
    "csv",
    "tsv",
    "json",
    "jsonc",
    "json5",
    "jsonl",
    "ndjson",
    "geojson",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "properties",
    "env",
    "xml",
    "xsd",
    "xsl",
    "plist",
    "rs",
    "py",
    "pyi",
    "js",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "tsx",
    "jsx",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hh",
    "hxx",
    "cs",
    "java",
    "kt",
    "kts",
    "go",
    "rb",
    "php",
    "pl",
    "pm",
    "lua",
    "r",
    "swift",
    "m",
    "mm",
    "scala",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "psm1",
    "bat",
    "cmd",
    "sql",
    "css",
    "scss",
    "sass",
    "less",
    "vue",
    "svelte",
    "astro",
    "tex",
    "bib",
    "sty",
    "cls",
    "diff",
    "patch",
    "mk",
    "cmake",
    "gradle",
    "nix",
    "zig",
    "dart",
    "ex",
    "exs",
    "erl",
    "hrl",
    "hs",
    "ml",
    "mli",
    "fs",
    "fsx",
    "clj",
    "cljs",
    "edn",
    "el",
    "lisp",
    "scm",
    "rkt",
    "vim",
    "graphql",
    "gql",
    "proto",
    "tf",
    "hcl",
    "rst",
    "adoc",
    "asciidoc",
    "org",
    "typ",
    "srt",
    "vtt",
    "lock",
    "gitignore",
    "gitattributes",
    "editorconfig",
    "dockerignore",
    "ipynb",
    "jl",
    "nim",
    "v",
    "sv",
    "vhd",
    "vhdl",
    "asm",
    "s",
    "f90",
    "pas",
    "groovy",
    "awk",
    "sed",
    "tcl",
    "ron",
    "kdl",
    "dot",
    "gv",
    "mermaid",
    "mmd",
    "puml",
    "http",
    "rest",
];

/// File names (without an extension) shown in the text viewer.
const TEXT_NAMES: &[&str] = &[
    "makefile",
    "gnumakefile",
    "dockerfile",
    "containerfile",
    "jenkinsfile",
    "vagrantfile",
    "gemfile",
    "rakefile",
    "procfile",
    "justfile",
    "license",
    "licence",
    "copying",
    "readme",
    "changelog",
    "authors",
    "contributors",
    "notice",
    "todo",
    "codeowners",
    ".gitignore",
    ".gitattributes",
    ".editorconfig",
    ".env",
];

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// What `path` is shown as, by its name alone (no file access).
pub fn kind(path: &Path) -> Option<ViewerKind> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if TEXT_NAMES.contains(&name.as_str()) {
        return Some(ViewerKind::Text);
    }
    let ext = extension(path)?;
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        Some(ViewerKind::Image)
    } else if ext == "pdf" {
        Some(ViewerKind::Pdf)
    } else if TEXT_EXTENSIONS.contains(&ext.as_str()) {
        Some(ViewerKind::Text)
    } else {
        None
    }
}

/// Whether `bytes` (the start of a file) look like text: no NUL byte and
/// valid UTF-8, allowing a character cut at the end.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return false;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && bytes.len() - e.valid_up_to() < 4,
    }
}

/// Whether the file at `path` is shown in the text viewer: by its name,
/// or by its first bytes (blocking: reads the file).
pub fn sniff_text(path: &Path) -> bool {
    match kind(path) {
        Some(ViewerKind::Text) => return true,
        Some(ViewerKind::Image | ViewerKind::Pdf) => return false,
        None => {}
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    if file
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    looks_like_text(&head)
}

/// The fence info string for `path`: the language its extension or name
/// names, when the highlighter has a grammar for it.
pub fn language_info(path: &Path) -> Option<String> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let word = match name.as_str() {
        "makefile" | "gnumakefile" => "makefile".to_owned(),
        "dockerfile" | "containerfile" => "dockerfile".to_owned(),
        "jenkinsfile" => "groovy".to_owned(),
        "vagrantfile" | "gemfile" | "rakefile" => "ruby".to_owned(),
        "justfile" => "just".to_owned(),
        _ => match extension(path)?.as_str() {
            "h" => "c".to_owned(),
            "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp".to_owned(),
            "jsonc" | "json5" | "geojson" | "ipynb" => "json".to_owned(),
            "mk" => "makefile".to_owned(),
            "gitignore" | "dockerignore" => "ignore".to_owned(),
            "mmd" => "mermaid".to_owned(),
            "txt" | "text" | "log" | "lock" => return None,
            ext => ext.to_owned(),
        },
    };
    gv_highlight::Language::from_info(&word).map(|_| word)
}

/// `text` as one fenced code block of `info`: the fence is longer than any
/// run of backticks in the text, so nothing inside closes it.
pub fn fenced(text: &str, info: Option<&str>) -> String {
    let mut longest = 0;
    let mut run = 0;
    for b in text.bytes() {
        if b == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    let mut out = String::with_capacity(text.len() + 2 * fence.len() + 16);
    out.push_str(&fence);
    out.push_str(info.unwrap_or(""));
    out.push('\n');
    out.push_str(text);
    if !text.is_empty() && !text.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
    out
}

/// What the viewer asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileViewerEvent {
    /// Hand the file to the system's app for it (the window asks first).
    OpenExternally(PathBuf),
}

/// A loaded image.
#[derive(Clone)]
pub struct ShownImage {
    pub image: Arc<RenderImage>,
    pub probe: Probe,
}

impl std::fmt::Debug for ShownImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShownImage")
            .field("probe", &self.probe)
            .finish_non_exhaustive()
    }
}

/// What the viewer shows.
#[derive(Debug, Clone)]
pub enum ViewerStatus {
    Loading,
    /// The text viewer (its Reading view).
    Text {
        /// The language label, if highlighted.
        language: Option<SharedString>,
        bytes: u64,
    },
    Image(ShownImage),
    /// The PDF viewer ([`FileViewer::pdf`]), with the PDF's page count.
    Pdf {
        pages: usize,
    },
    /// The file is not shown; the sentence says why.
    NotShown(SharedString),
}

/// How large the image is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageZoom {
    /// The whole image in the view, never enlarged.
    Fit,
    /// Device pixels per image pixel.
    Scale(f32),
}

/// What a background load produced.
enum Loaded {
    Text {
        markdown: Arc<str>,
        language: Option<String>,
        bytes: u64,
    },
    Image(ShownImage),
    Pdf {
        bytes: Arc<[u8]>,
        key: u64,
        sizes: Vec<(f32, f32)>,
        engine: Arc<dyn crate::export::PdfEngine>,
    },
    NotShown(String),
}

/// A built-in viewer of one file.
pub struct FileViewer {
    path: PathBuf,
    /// The folder SVG images may load files from.
    root: Option<PathBuf>,
    focus: FocusHandle,
    status: ViewerStatus,
    reader: Option<gpui::Entity<Reader>>,
    zoom: ImageZoom,
    scroll: ScrollHandle,
    /// The image area, as last laid out.
    area: Rc<Cell<Bounds<Pixels>>>,
    /// Where a drag started: the pointer and the scroll offset.
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
    cancel: CancelFlag,
    /// Stops the PDF engine reading a PDF.
    pdf_cancel: Arc<AtomicBool>,
    /// The PDF viewer of a PDF.
    pdf: Option<gpui::Entity<PdfViewer>>,
    /// A page (from 0) to show once the PDF is read.
    pdf_page: Option<usize>,
    /// Where to show the PDF once it is read (a restored session's).
    pdf_restore: Option<PdfPlace>,
    _load: Option<Task<()>>,
}

impl EventEmitter<FileViewerEvent> for FileViewer {}

impl std::fmt::Debug for FileViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileViewer")
            .field("path", &self.path)
            .field("status", &self.status)
            .field("zoom", &self.zoom)
            .finish_non_exhaustive()
    }
}

impl Focusable for FileViewer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match (&self.status, &self.reader, &self.pdf) {
            (ViewerStatus::Text { .. }, Some(reader), _) => reader.read(cx).focus_handle(cx),
            (ViewerStatus::Pdf { .. }, _, Some(pdf)) => pdf.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        }
    }
}

impl FileViewer {
    /// A viewer of `path`, loading it now. `root` is the vault (SVG images
    /// may load files below it; without one, below the file's folder).
    pub fn open(path: PathBuf, root: Option<PathBuf>, cx: &mut App) -> gpui::Entity<Self> {
        cx.new(|cx| {
            let mut viewer = Self {
                path: path.clone(),
                root,
                focus: cx.focus_handle(),
                status: ViewerStatus::Loading,
                reader: None,
                zoom: ImageZoom::Fit,
                scroll: ScrollHandle::new(),
                area: Rc::default(),
                drag: None,
                cancel: CancelFlag::new(),
                pdf_cancel: Arc::new(AtomicBool::new(false)),
                pdf: None,
                pdf_page: None,
                pdf_restore: None,
                _load: None,
            };
            viewer.load(path, cx);
            viewer
        })
    }

    /// The file shown.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn status(&self) -> &ViewerStatus {
        &self.status
    }

    /// The Reading view of a text file.
    pub fn reader(&self) -> Option<&gpui::Entity<Reader>> {
        self.reader.as_ref()
    }

    pub fn zoom(&self) -> ImageZoom {
        self.zoom
    }

    /// The PDF viewer of a PDF, once it is read.
    pub fn pdf(&self) -> Option<&gpui::Entity<PdfViewer>> {
        self.pdf.as_ref()
    }

    /// Shows page `page` (from 0) of a PDF, now or once it is read.
    pub fn go_to_page(&mut self, page: usize, cx: &mut Context<'_, Self>) {
        match (&self.status, &self.pdf) {
            (ViewerStatus::Pdf { .. }, Some(pdf)) => pdf.update(cx, |v, cx| v.go_to(page, cx)),
            _ => self.pdf_page = Some(page),
        }
    }

    /// Shows the PDF at `place` once it is read (a restored tab's place;
    /// a page past the end is the last page).
    pub fn restore_pdf(&mut self, place: PdfPlace) {
        self.pdf_restore = Some(place);
    }

    /// Where the PDF is read: the PDF viewer's place, else the place it
    /// will be shown at once read. `None` for other files.
    pub fn pdf_place(&self, cx: &App) -> Option<PdfPlace> {
        match (&self.status, &self.pdf) {
            (ViewerStatus::Pdf { .. }, Some(pdf)) => Some(pdf.read(cx).place()),
            _ => self.pdf_restore,
        }
    }

    /// The vault's folder is known.
    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
    }

    /// Loads `path` (again, or another file), off the UI thread.
    pub fn load(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        if path != self.path {
            self.zoom = ImageZoom::Fit;
            self.scroll.set_offset(Point::default());
            self.pdf = None;
            self.pdf_restore = None;
        }
        self.path = path.clone();
        self.cancel.cancel();
        self.cancel = CancelFlag::new();
        let cancel = self.cancel.clone();
        let root = self
            .root
            .clone()
            .filter(|r| path.starts_with(r))
            .or_else(|| path.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let animate = !cx.reduce_motion();
        if matches!(self.status, ViewerStatus::NotShown(_)) {
            self.status = ViewerStatus::Loading;
        }
        self.pdf_cancel.store(true, Ordering::Release);
        self.pdf_cancel = Arc::new(AtomicBool::new(false));
        let pdf = (kind(&path) == Some(ViewerKind::Pdf))
            .then(|| (crate::export::pdf_engine(cx), Arc::clone(&self.pdf_cancel)));
        self._load = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    match pdf {
                        Some((engine, cancel)) => load_pdf(&path, engine, &cancel),
                        None => load(&path, &root, animate, &cancel),
                    }
                })
                .await;
            this.update(cx, |viewer, cx| viewer.finish(loaded, cx)).ok();
        }));
        cx.notify();
    }

    fn finish(&mut self, loaded: Loaded, cx: &mut Context<'_, Self>) {
        self._load = None;
        match loaded {
            Loaded::Text {
                markdown,
                language,
                bytes,
            } => {
                let reader = match &self.reader {
                    Some(reader) => reader.clone(),
                    None => {
                        let reader = cx.new(|cx| {
                            let mut reader = Reader::new(cx);
                            let profile =
                                gv_profile::compile_builtin(gv_profile::ProfileId::CommonMark);
                            reader.set_profile(Arc::new(profile), cx);
                            reader
                        });
                        cx.observe(&reader, |_, _, cx| cx.notify()).detach();
                        self.reader = Some(reader.clone());
                        reader
                    }
                };
                let path = self.path.clone();
                reader.update(cx, |r, cx| {
                    let at = r.position();
                    r.open_text(path, markdown, at, cx);
                });
                let language = language
                    .and_then(|l| gv_highlight::Language::from_info(&l))
                    .map(|l| SharedString::from(l.display_name()));
                self.status = ViewerStatus::Text { language, bytes };
            }
            Loaded::Image(image) => self.status = ViewerStatus::Image(image),
            Loaded::Pdf {
                bytes,
                key,
                sizes,
                engine,
            } => {
                // Read again (changed on disk): the same zoom and place;
                // a restored tab: its place.
                let place = match (&self.pdf, self.pdf_restore.take()) {
                    (_, Some(place)) => Some(place),
                    (Some(old), None) => Some(old.read(cx).place()),
                    (None, None) => None,
                };
                let zoom = place.map_or(PdfZoom::FitWidth, |p| p.zoom);
                let page = match self.pdf_page.take() {
                    Some(page) => Some((page, None)),
                    None => place.map(|p| (p.page, p.into)),
                };
                let pages = sizes.len();
                let path = self.path.clone();
                let viewer =
                    cx.new(|cx| PdfViewer::new(path, bytes, key, sizes, engine, zoom, page, cx));
                cx.observe(&viewer, |_, _, cx| cx.notify()).detach();
                self.pdf = Some(viewer);
                self.status = ViewerStatus::Pdf { pages };
            }
            Loaded::NotShown(why) => {
                self.pdf = None;
                self.status = ViewerStatus::NotShown(why.into());
            }
        }
        cx.notify();
    }

    /// Shows `text` in place of the file's text (the Source view's edits,
    /// saved or not), when the viewer shows text.
    pub fn show_text(&mut self, text: &str, cx: &mut Context<'_, Self>) {
        let ViewerStatus::Text { bytes, .. } = &mut self.status else {
            return;
        };
        let Some(reader) = self.reader.clone() else {
            return;
        };
        *bytes = text.len() as u64;
        let language = language_info(&self.path);
        let markdown: Arc<str> = fenced(text, language.as_deref()).into();
        let path = self.path.clone();
        reader.update(cx, |r, cx| {
            let at = r.position();
            r.open_text(path, markdown, at, cx);
        });
        cx.notify();
    }

    /// Whether the text viewer's document is laid out.
    pub fn text_ready(&self, cx: &App) -> bool {
        self.reader
            .as_ref()
            .is_some_and(|r| *r.read(cx).status() == LoadStatus::Ready)
    }

    // ----- image zoom ----------------------------------------------------------------

    /// The image's size in pixels.
    fn image_size(&self) -> Option<(f32, f32)> {
        match &self.status {
            ViewerStatus::Image(shown) => {
                let size = shown.image.size(0);
                Some((size.width.0 as f32, size.height.0 as f32))
            }
            _ => None,
        }
    }

    /// Device pixels per image pixel now, for a window of `scale` device
    /// pixels per point.
    pub fn effective_scale(&self, scale: f32) -> f32 {
        match self.zoom {
            ImageZoom::Scale(z) => z,
            ImageZoom::Fit => {
                let (Some((w, h)), area) = (self.image_size(), self.area.get()) else {
                    return 1.0;
                };
                let (aw, ah) = (f32::from(area.size.width), f32::from(area.size.height));
                if aw <= 0.0 || ah <= 0.0 || w <= 0.0 || h <= 0.0 {
                    return 1.0;
                }
                (aw * scale / w).min(ah * scale / h).min(1.0)
            }
        }
    }

    pub fn set_zoom(&mut self, zoom: ImageZoom, cx: &mut Context<'_, Self>) {
        self.zoom = zoom;
        if zoom == ImageZoom::Fit {
            self.scroll.set_offset(Point::default());
        }
        cx.notify();
    }

    /// The next zoom step up (`up`) or down from `current`.
    fn step(current: f32, up: bool) -> f32 {
        if up {
            ZOOM_STEPS
                .iter()
                .copied()
                .find(|s| *s > current + 0.001)
                .unwrap_or(ZOOM_STEPS[ZOOM_STEPS.len() - 1])
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|s| *s < current - 0.001)
                .unwrap_or(ZOOM_STEPS[0])
        }
    }

    fn zoom_by_step(&mut self, up: bool, window: &Window, cx: &mut Context<'_, Self>) {
        if self.image_size().is_none() {
            return;
        }
        let current = self.effective_scale(window.scale_factor());
        self.set_zoom(ImageZoom::Scale(Self::step(current, up)), cx);
    }

    fn pan(&mut self, dx: f32, dy: f32, cx: &mut Context<'_, Self>) {
        let offset = self.scroll.offset();
        self.set_offset(point(offset.x + px(dx), offset.y + px(dy)));
        cx.notify();
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let max = self.scroll.max_offset();
        let clamp = |v: Pixels, max: Pixels| v.min(px(0.0)).max(-max);
        self.scroll
            .set_offset(point(clamp(offset.x, max.x), clamp(offset.y, max.y)));
    }

    // ----- drawing --------------------------------------------------------------------

    /// A toolbar button showing `label`, named `name` for assistive
    /// technology; `pressed` is `Some` for a toggle (Fit, 100 %).
    #[allow(clippy::too_many_arguments)]
    fn button(
        &self,
        id: &'static str,
        label: SharedString,
        name: &'static str,
        pressed: Option<bool>,
        theme: &Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<'_, Self>) + 'static,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (hover, press) = (theme.state.hover, theme.state.press);
        div()
            .id(id)
            .a11y_button(name)
            .when_some(pressed, |b, on| {
                b.aria_toggled(if on { Toggled::True } else { Toggled::False })
            })
            .keyboard_control(theme)
            .debug_selector(move || id.to_owned())
            .flex_none()
            .h(px(size::BUTTON_HEIGHT))
            .px(px(spacing::BUTTON_PAD_X))
            .flex()
            .items_center()
            .rounded(px(radius::CONTROL))
            .border_1()
            .border_color(theme.border.strong)
            .bg(if pressed == Some(true) {
                theme.accent.tint
            } else {
                theme.surface.raised
            })
            .text_color(theme.text.primary)
            .font_weight(FontWeight(text::weight::ACTIVE))
            .hover(move |style| style.bg(hover))
            .active(move |style| style.bg(press))
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .accessible()
            .into_any_element()
    }

    fn render_image(
        &self,
        shown: &ShownImage,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let scale = window.scale_factor();
        let z = self.effective_scale(scale);
        let size = shown.image.size(0);
        let (w, h) = (size.width.0 as f32, size.height.0 as f32);
        let (dw, dh) = (w * z / scale, h * z / scale);
        let area = self.area.get();
        let (aw, ah) = (f32::from(area.size.width), f32::from(area.size.height));
        let percent = format!("{:.0} %", z * 100.0);
        let format = shown.probe.format.name();
        let facts = format!(
            "{format} · {} × {} px",
            shown.probe.width, shown.probe.height
        );
        let facts_for_reader = format!(
            "{format}, {} by {} pixels",
            shown.probe.width, shown.probe.height
        );
        let fit = self.zoom == ImageZoom::Fit;
        let actual = self.zoom == ImageZoom::Scale(1.0);
        let name = self.file_name();
        let bar = div()
            .id("image-viewer-toolbar")
            .a11y_group(Role::Toolbar, "Image")
            .flex_none()
            .min_h(px(sizes::INFOBAR))
            .flex()
            .items_center()
            .gap(px(spacing::GAP))
            .pt(px(spacing::PAD_TOP))
            .pr(px(spacing::PAD_RIGHT))
            .pb(px(spacing::PAD_BOTTOM))
            .pl(px(spacing::PAD_LEFT))
            .border_b_1()
            .border_color(theme.border.default)
            .bg(theme.surface.panel)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(Icon::ImageFile.sized(icon::ROW)),
            )
            .child(
                div()
                    .debug_selector(|| "image-viewer-facts".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text.secondary)
                    .child(facts),
            )
            .child(self.button(
                "image-viewer-fit",
                "Fit".into(),
                "Fit",
                Some(fit),
                theme,
                |this, _, cx| this.set_zoom(ImageZoom::Fit, cx),
                cx,
            ))
            .child(self.button(
                "image-viewer-actual",
                "100 %".into(),
                "Actual size",
                Some(actual),
                theme,
                |this, _, cx| this.set_zoom(ImageZoom::Scale(1.0), cx),
                cx,
            ))
            .child(self.button(
                "image-viewer-zoom-out",
                "−".into(),
                "Zoom out",
                None,
                theme,
                |this, window, cx| this.zoom_by_step(false, window, cx),
                cx,
            ))
            .child(
                div()
                    .id("image-viewer-zoom")
                    .role(Role::Status)
                    .aria_label("Zoom")
                    .aria_value(percent.clone())
                    .debug_selector(|| "image-viewer-zoom".into())
                    .flex_none()
                    .min_w(px(sizes::INFOBAR * 2.0))
                    .flex()
                    .justify_center()
                    .text_color(theme.text.primary)
                    .child(percent)
                    .accessible()
                    .live(Live::Polite),
            )
            .child(self.button(
                "image-viewer-zoom-in",
                "+".into(),
                "Zoom in",
                None,
                theme,
                |this, window, cx| this.zoom_by_step(true, window, cx),
                cx,
            ));
        let cell = self.area.clone();
        let picture = img(ImageSource::Render(shown.image.clone()))
            .flex_none()
            .w(px(dw))
            .h(px(dh))
            .object_fit(ObjectFit::Fill);
        let body = div()
            .id("image-viewer-body")
            .role(Role::Image)
            .aria_label(name)
            .aria_description(facts_for_reader)
            .debug_selector(|| "image-viewer-body".into())
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_scroll()
            .track_scroll(&self.scroll)
            .bg(theme.surface.page)
            .on_scroll(cx.listener(|this, e: &gpui::ScrollEvent, window, cx| {
                if zoom_modifier(&e.modifiers) {
                    let dy = f32::from(e.delta.pixel_delta(px(16.0)).y);
                    if dy != 0.0 {
                        this.zoom_by_step(dy > 0.0, window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.focus.focus(window, cx);
                    this.drag = Some((e.position, this.scroll.offset()));
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| {
                if let (Some((start, offset)), true) = (this.drag, e.dragging()) {
                    this.set_offset(offset + (e.position - start));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, _| this.drag = None),
            )
            .child(
                canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .w(px(dw.max(aw)))
                    .h(px(dh.max(ah)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(picture),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(bar.accessible())
            .child(body.accessible())
            .into_any_element()
    }

    /// The file's name, as the viewer is named.
    fn file_name(&self) -> SharedString {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into()
    }

    fn render_message(
        &self,
        message: SharedString,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let name = self.file_name();
        let path = self.path.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(spacing::GAP))
            .bg(theme.surface.page)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(
                div()
                    .text_color(theme.text.secondary)
                    .child(Icon::OtherFile.sized(icon::TOOLBAR)),
            )
            .child(
                div()
                    .font_weight(FontWeight(text::weight::STRONG))
                    .text_color(theme.text.primary)
                    .child(name),
            )
            .child(
                div()
                    .id("file-viewer-message")
                    .role(Role::Status)
                    .aria_label(message.clone())
                    .debug_selector(|| "file-viewer-message".into())
                    .text_color(theme.text.secondary)
                    .child(message)
                    .accessible()
                    .live(Live::Polite),
            )
            .child(self.button(
                "file-viewer-open-externally",
                "Open with default app".into(),
                "Open with default app",
                None,
                theme,
                move |_, _, cx| cx.emit(FileViewerEvent::OpenExternally(path.clone())),
                cx,
            ))
            .into_any_element()
    }
}

/// Ctrl (Cmd or Ctrl on macOS) turns the wheel into zoom.
fn zoom_modifier(m: &Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        m.platform || m.control
    } else {
        m.control
    }
}

/// "1.2 MB".
fn byte_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// The engine's names for PDFs: a new one for every read.
static NEXT_PDF_KEY: AtomicU64 = AtomicU64::new(1);

/// Reads the PDF at `path` and asks `engine` for its pages (blocking: the
/// background executor only).
fn load_pdf(
    path: &Path,
    engine: Option<Arc<dyn crate::export::PdfEngine>>,
    cancel: &AtomicBool,
) -> Loaded {
    let meta = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Loaded::NotShown("This is not a file.".into()),
        Err(err) => return Loaded::NotShown(format!("The file cannot be read: {err}.")),
    };
    if meta.len() > PDF_MAX_BYTES {
        return Loaded::NotShown(format!(
            "This PDF is too large to show here ({}; at most {}).",
            byte_size(meta.len()),
            byte_size(PDF_MAX_BYTES)
        ));
    }
    let Some(engine) = engine else {
        return Loaded::NotShown("The PDF engine is not available.".into());
    };
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(err) => return Loaded::NotShown(format!("The file cannot be read: {err}.")),
    };
    let key = NEXT_PDF_KEY.fetch_add(1, Ordering::Relaxed);
    match engine.open_document(key, &data, cancel) {
        Ok(sizes) if sizes.is_empty() => Loaded::NotShown("The PDF has no pages.".into()),
        Ok(sizes) => Loaded::Pdf {
            bytes: data.into(),
            key,
            sizes,
            engine,
        },
        Err(crate::export::PdfError::Cancelled) => Loaded::NotShown("Cancelled.".into()),
        Err(crate::export::PdfError::Failed(why)) => Loaded::NotShown(sentence(&why)),
    }
}

/// `why` as a sentence: a capital first letter and a full stop.
fn sentence(why: &str) -> String {
    let why = why.trim();
    let mut chars = why.chars();
    let mut out: String = match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => return "The PDF cannot be shown.".to_owned(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Reads and decodes `path` (blocking: the background executor only).
fn load(path: &Path, root: &Path, animate: bool, cancel: &CancelFlag) -> Loaded {
    let meta = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => meta,
        Ok(_) => return Loaded::NotShown("This is not a file.".into()),
        Err(err) => return Loaded::NotShown(format!("The file cannot be read: {err}.")),
    };
    if kind(path) == Some(ViewerKind::Image) {
        return load_image(path, root, animate, cancel);
    }
    let mut head = Vec::new();
    let read =
        std::fs::File::open(path).and_then(|f| f.take(SNIFF_BYTES as u64).read_to_end(&mut head));
    if let Err(err) = read {
        return Loaded::NotShown(format!("The file cannot be read: {err}."));
    }
    if kind(path) != Some(ViewerKind::Text) && !looks_like_text(&head) {
        return Loaded::NotShown("Glyphvane does not show this kind of file.".into());
    }
    let bytes = meta.len();
    if bytes > TEXT_MAX_BYTES {
        return Loaded::NotShown(format!(
            "This file is too large to show here ({}; at most {}).",
            byte_size(bytes),
            byte_size(TEXT_MAX_BYTES)
        ));
    }
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(err) => return Loaded::NotShown(format!("The file cannot be read: {err}.")),
    };
    if data.contains(&0) {
        return Loaded::NotShown("Glyphvane does not show this kind of file.".into());
    }
    let text = String::from_utf8_lossy(&data);
    let language = language_info(path);
    Loaded::Text {
        markdown: fenced(&text, language.as_deref()).into(),
        language,
        bytes,
    }
}

fn load_image(path: &Path, root: &Path, animate: bool, cancel: &CancelFlag) -> Loaded {
    let engine = crate::reader::media::media_engine(root);
    let probe = match engine.probe_path(path) {
        Ok(probe) => probe,
        Err(err) => return Loaded::NotShown(format!("The image cannot be shown: {err}.")),
    };
    let request = DecodeRequest {
        fit: Some((IMAGE_MAX_SIDE, IMAGE_MAX_SIDE)),
        animate,
    };
    let decoded = match engine.load_path(path, request, cancel) {
        Ok(decoded) => decoded,
        Err(err) => return Loaded::NotShown(format!("The image cannot be shown: {err}.")),
    };
    match crate::reader::media::render_image(&decoded) {
        Some(image) => Loaded::Image(ShownImage { image, probe }),
        None => Loaded::NotShown("The image has no pixels.".into()),
    }
}

impl Render for FileViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let status = self.status.clone();
        // The keyboard the viewer had while loading goes to the text's
        // Reading view once it shows.
        if let (ViewerStatus::Text { .. }, Some(reader)) = (&status, &self.reader)
            && self.focus.is_focused(window)
        {
            reader.read(cx).focus_handle(cx).focus(window, cx);
        }
        let content = match &status {
            ViewerStatus::Loading => div().size_full().bg(theme.surface.page).into_any_element(),
            ViewerStatus::Text { .. } => match &self.reader {
                Some(reader) => div().size_full().child(reader.clone()).into_any_element(),
                None => div().size_full().into_any_element(),
            },
            ViewerStatus::Image(shown) => self.render_image(shown, theme, window, cx),
            ViewerStatus::Pdf { .. } => match &self.pdf {
                Some(pdf) => div().size_full().child(pdf.clone()).into_any_element(),
                None => div().size_full().into_any_element(),
            },
            ViewerStatus::NotShown(why) => self.render_message(why.clone(), theme, cx),
        };
        // The keyboard the viewer had while loading goes to the pages.
        if let (ViewerStatus::Pdf { .. }, Some(pdf)) = (&status, &self.pdf)
            && self.focus.is_focused(window)
        {
            pdf.read(cx).focus_handle(cx).focus(window, cx);
        }
        let image = matches!(status, ViewerStatus::Image(_));
        // The text viewer is a Reading view (a document of its own); the
        // others are a named group the keyboard reaches with F6.
        let group = !matches!(status, ViewerStatus::Text { .. });
        let label: SharedString = if image {
            format!("Image viewer: {}", self.file_name()).into()
        } else if matches!(status, ViewerStatus::Pdf { .. }) {
            format!("PDF viewer: {}", self.file_name()).into()
        } else {
            self.file_name()
        };
        div()
            .id("file-viewer")
            .when(group, |d| d.a11y_group(Role::Group, label))
            .debug_selector(|| "file-viewer".into())
            .track_focus(&self.focus)
            .size_full()
            .when(image, |d| {
                d.key_context(KEY_CONTEXT)
                    .on_action(
                        cx.listener(|this, _: &Fit, _, cx| this.set_zoom(ImageZoom::Fit, cx)),
                    )
                    .on_action(cx.listener(|this, _: &ZoomReset, _, cx| {
                        this.set_zoom(ImageZoom::Fit, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ActualSize, _, cx| {
                        this.set_zoom(ImageZoom::Scale(1.0), cx);
                    }))
                    .on_action(cx.listener(|this, _: &ZoomInImage, window, cx| {
                        this.zoom_by_step(true, window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ZoomIn, window, cx| {
                        this.zoom_by_step(true, window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ZoomOutImage, window, cx| {
                        this.zoom_by_step(false, window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &ZoomOut, window, cx| {
                        this.zoom_by_step(false, window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &PanLeft, _, cx| this.pan(PAN_STEP, 0.0, cx)))
                    .on_action(
                        cx.listener(|this, _: &PanRight, _, cx| this.pan(-PAN_STEP, 0.0, cx)),
                    )
                    .on_action(cx.listener(|this, _: &PanUp, _, cx| this.pan(0.0, PAN_STEP, cx)))
                    .on_action(cx.listener(|this, _: &PanDown, _, cx| this.pan(0.0, -PAN_STEP, cx)))
            })
            .child(content)
            .accessible()
    }
}

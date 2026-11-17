// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What cards show at full detail.
//!
//! - **Text cards**: their Markdown through the Reading view's pipeline
//!   (parse, model, render tree in the Obsidian profile) and drawn as
//!   [`InlineParagraph`](crate::inline::InlineParagraph)s: paragraphs,
//!   headings, list items (with their markers) and table rows keep their
//!   styles; code, math, diagram and front matter blocks show as plain
//!   monospaced text; images and link status icons inside the text are not
//!   drawn (their boxes stay blank).
//! - **File cards**: read off the UI thread when first shown. A note shows
//!   its first blocks (the section of `subpath` when given: `#Heading`,
//!   `#Parent#Child` or `#^block`), an image is decoded through the media
//!   engine the Reading view uses, a PDF shows its first page drawn by the
//!   PDF engine of the export commands (the helper process; cached by
//!   file, size and time; a file card without an engine that draws pages
//!   or when drawing fails), a `.canvas` file shows its shapes, any other
//!   file a file card. Missing files say so.
//! - **Group backgrounds**: a group's `background` (a vault path) is
//!   decoded through the same media engine when the group is first drawn
//!   and painted under the group's content as `backgroundStyle` says
//!   (`cover`, the default, `ratio`, `repeat`). A remote address is never
//!   fetched: the group is drawn without it.
//! - **Link cards**: the URL only. Nothing is fetched (remote content
//!   policy).
//!
//! Content is built at a zoom *bucket* (steps of 2^(1/8), about 9 %), so a
//! zoom gesture builds each card at most once per step.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use gpui::{RenderImage, SharedString};
use gv_canvas::Subpath;
use gv_model::DocModel;
use gv_profile::{CompiledProfile, ProfileId};
use gv_render_tree::{FrameKind, ItemKind, RenderOptions, RenderTree, Sources, StyleTable};
use gv_syntax::{ParseOptions, SyntaxTree};

use super::Doc;
use crate::export::PdfEngine;
use crate::inline::{FontFamilies, FontTable, InlineBlock, InlineText, font_of};

/// The most of a note read for its preview.
const READ_CAP: u64 = 512 * 1024;
/// The most of a note's (or section's) text shown: its first blocks.
const PREVIEW_BYTES: usize = 4 * 1024;
/// The most of a text card's text built (the card clips the rest).
const TEXT_CAP: usize = 16 * 1024;
/// Longest side of a decoded image preview, in device pixels.
const IMAGE_FIT: u32 = 1024;
/// The largest PDF whose first page is drawn.
const PDF_CAP: u64 = 64 * 1024 * 1024;
/// First pages kept drawn, most recent last.
const PDF_CACHE: usize = 16;
/// Longest side of a decoded group background, in device pixels.
const BACKGROUND_FIT: u32 = 2048;

/// Steps of the zoom buckets per doubling.
const BUCKETS_PER_OCTAVE: f64 = 8.0;

/// The bucket of `zoom`.
pub(crate) fn bucket(zoom: f64) -> i32 {
    (zoom.max(1e-6).log2() * BUCKETS_PER_OCTAVE).round() as i32
}

/// The scale content of `bucket` is built at.
pub(crate) fn bucket_scale(bucket: i32) -> f32 {
    2f64.powf(f64::from(bucket) / BUCKETS_PER_OCTAVE) as f32
}

/// A parsed Markdown text, ready to be built at any scale.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub text: Arc<str>,
    syntax: SyntaxTree,
    model: DocModel,
}

impl Parsed {
    pub fn new(text: Arc<str>, profile: &CompiledProfile) -> Self {
        let syntax = gv_syntax::parse_with(&*text, ParseOptions::from_profile(profile));
        let model = DocModel::build(&syntax, &*text);
        Self {
            text,
            syntax,
            model,
        }
    }
}

/// A group's decoded background image and its size as a source (world
/// units for `repeat`).
#[derive(Debug, Clone)]
pub(crate) struct BackgroundImage {
    pub image: Arc<RenderImage>,
    pub width: f32,
    pub height: f32,
}

/// A group's background.
#[derive(Debug, Clone)]
pub(crate) enum Background {
    Pending,
    Ready(BackgroundImage),
    /// Not shown (missing, unreadable, remote): the group is drawn plain.
    None,
}

/// What a file card shows, as loaded off the UI thread.
#[derive(Debug)]
pub(crate) enum Loaded {
    Markdown(Parsed),
    Image(Arc<RenderImage>),
    Canvas(Box<Doc>),
    /// A file Glyphvane does not preview: its extension, upper case.
    Other(SharedString),
    /// Why there is nothing to show.
    Missing(SharedString),
}

/// What a file card shows.
#[derive(Debug, Clone)]
pub(crate) enum Preview {
    Pending,
    Markdown(Rc<Parsed>),
    Image(Arc<RenderImage>),
    Canvas(Rc<Doc>),
    Other(SharedString),
    Missing(SharedString),
}

impl From<Loaded> for Preview {
    fn from(l: Loaded) -> Self {
        match l {
            Loaded::Markdown(p) => Self::Markdown(Rc::new(p)),
            Loaded::Image(i) => Self::Image(i),
            Loaded::Canvas(d) => Self::Canvas(Rc::new(*d)),
            Loaded::Other(e) => Self::Other(e),
            Loaded::Missing(why) => Self::Missing(why),
        }
    }
}

/// One block of built Markdown, top to bottom.
#[derive(Debug, Clone)]
pub(crate) enum Block {
    /// Inline content. `indent` and `gap` (above) are in px at the build's
    /// scale; `marker` is a list item's marker, drawn in the indent.
    Text {
        block: Rc<RefCell<InlineBlock>>,
        indent: f32,
        gap: f32,
        marker: Option<SharedString>,
    },
    /// Code-like text in the mono family.
    Code {
        text: SharedString,
        indent: f32,
        gap: f32,
    },
    /// A thematic break.
    Rule { gap: f32 },
}

/// Markdown built at one zoom bucket.
#[derive(Debug, Clone)]
pub(crate) struct Built {
    pub bucket: i32,
    pub blocks: Rc<Vec<Block>>,
    /// Body font size at the bucket's scale (code blocks use it).
    pub body_size: f32,
}

/// The cards' content: parsed texts, file previews and built Markdown.
#[derive(Debug)]
pub(crate) struct Cards {
    pub text: Rc<RefCell<InlineText>>,
    pub mono: SharedString,
    profile: Arc<CompiledProfile>,
    options: RenderOptions,
    /// Text cards' parsed Markdown, by node.
    pub(super) parsed: HashMap<usize, Rc<Parsed>>,
    /// File cards' previews, by node (absent: not asked for yet).
    previews: HashMap<usize, Preview>,
    /// Built Markdown, by node, at its last bucket.
    pub(super) built: HashMap<usize, Built>,
    /// Groups' background images, by node (absent: not asked for yet).
    backgrounds: HashMap<usize, Background>,
    /// Builds made (for tests and the culling check).
    pub builds: u64,
}

impl Cards {
    pub fn new(families: &FontFamilies) -> Self {
        let profile = Arc::new(gv_profile::compile_builtin(ProfileId::Obsidian));
        let options = RenderOptions::for_profile(&profile);
        let fonts = font_table(&options.styles, families);
        Self {
            text: Rc::new(RefCell::new(InlineText::new(fonts))),
            mono: families.mono.clone(),
            profile,
            options,
            parsed: HashMap::new(),
            previews: HashMap::new(),
            built: HashMap::new(),
            backgrounds: HashMap::new(),
            builds: 0,
        }
    }

    /// Forgets everything (a new canvas was loaded).
    pub fn clear(&mut self) {
        self.parsed.clear();
        self.previews.clear();
        self.built.clear();
        self.backgrounds.clear();
    }

    /// Group `node`'s background; `None` when it was never asked for.
    pub fn background(&self, node: usize) -> Option<&Background> {
        self.backgrounds.get(&node)
    }

    /// Marks group `node`'s background as being loaded.
    pub fn start_background(&mut self, node: usize) {
        self.backgrounds.insert(node, Background::Pending);
    }

    /// A background landed.
    pub fn set_background(&mut self, node: usize, background: Background) {
        self.backgrounds.insert(node, background);
    }

    /// The backgrounds ready to paint, by node.
    pub fn ready_backgrounds(&self) -> HashMap<usize, BackgroundImage> {
        self.backgrounds
            .iter()
            .filter_map(|(n, b)| match b {
                Background::Ready(image) => Some((*n, image.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn profile(&self) -> Arc<CompiledProfile> {
        Arc::clone(&self.profile)
    }

    /// The Markdown of text card `node`, parsed on first use.
    pub fn text_card(&mut self, node: usize, text: &str) -> Rc<Parsed> {
        let profile = &self.profile;
        Rc::clone(self.parsed.entry(node).or_insert_with(|| {
            let text = cap_at_line(text, TEXT_CAP);
            Rc::new(Parsed::new(text.into(), profile))
        }))
    }

    /// File card `node`'s preview; `None` when it was never asked for.
    pub fn preview(&self, node: usize) -> Option<&Preview> {
        self.previews.get(&node)
    }

    /// Marks file card `node`'s preview as being loaded.
    pub fn start_preview(&mut self, node: usize) {
        self.previews.insert(node, Preview::Pending);
    }

    /// A preview landed.
    pub fn set_preview(&mut self, node: usize, loaded: Loaded) {
        self.built.remove(&node);
        self.previews.insert(node, loaded.into());
    }

    /// `parsed` (the Markdown of `node`) built at `bucket`.
    pub fn built(&mut self, node: usize, parsed: &Parsed, bucket: i32) -> Built {
        if let Some(b) = self.built.get(&node)
            && b.bucket == bucket
        {
            return b.clone();
        }
        let scale = bucket_scale(bucket);
        let options = self.options.clone().zoomed(scale);
        let body_size = options.styles.role(gv_render_tree::TextRole::Body).size;
        let blocks = build_blocks(parsed, options);
        self.builds += 1;
        let b = Built {
            bucket,
            blocks: Rc::new(blocks),
            body_size,
        };
        self.built.insert(node, b.clone());
        b
    }
}

/// The font of every style: the text family for prose, the mono family for
/// code (as the Reading view's table).
fn font_table(styles: &StyleTable, families: &FontFamilies) -> FontTable {
    let mut table = FontTable::new();
    for (id, spec) in styles.ids() {
        let family = match spec.family {
            gv_render_tree::Family::Text => families.text.clone(),
            gv_render_tree::Family::Mono => families.mono.clone(),
        };
        table.set(id, font_of(family, f32::from(spec.weight), spec.italic));
    }
    table
}

/// `text` cut to at most `cap` bytes, at the end of a line when there is
/// one.
fn cap_at_line(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &text[..end];
    match cut.rfind('\n') {
        Some(nl) if nl > 0 => &cut[..nl + 1],
        _ => cut,
    }
}

/// The blocks of `parsed` built with `options`.
fn build_blocks(parsed: &Parsed, options: RenderOptions) -> Vec<Block> {
    let tree = RenderTree::build(
        &parsed.syntax,
        &parsed.model,
        &*parsed.text,
        options,
        Sources::default(),
    );
    let metrics = tree.options().metrics;
    let mut out = Vec::new();
    for (ix, item) in tree.items().iter().enumerate() {
        let Some(node) = tree.node_of(ix) else {
            continue;
        };
        let indent = item.content_x(&metrics);
        let gap = item.gap_before;
        match item.kind {
            ItemKind::Paragraph
            | ItemKind::Heading { .. }
            | ItemKind::Details
            | ItemKind::TableRow { .. } => {
                let Some(content) = node.inline() else {
                    continue;
                };
                let marker = (item.opens > 0)
                    .then(|| item.context.last())
                    .flatten()
                    .and_then(|f| match &f.kind {
                        FrameKind::Item { marker } => Some(SharedString::from(marker.text())),
                        _ => None,
                    });
                out.push(Block::Text {
                    block: Rc::new(RefCell::new(InlineBlock::new(content.paragraph.clone()))),
                    indent,
                    gap,
                    marker,
                });
            }
            ItemKind::Code { .. }
            | ItemKind::Html { .. }
            | ItemKind::Properties { .. }
            | ItemKind::Math { .. }
            | ItemKind::Diagram { .. }
            | ItemKind::Live { .. } => {
                let src = parsed.text.get(item.source.clone()).unwrap_or_default();
                let text: String = src
                    .lines()
                    .filter(|l| {
                        let t = l.trim_start();
                        !(t.starts_with("```") || t.starts_with("~~~"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                out.push(Block::Code {
                    text: text.into(),
                    indent,
                    gap,
                });
            }
            ItemKind::ThematicBreak => out.push(Block::Rule { gap }),
            ItemKind::EmptyItem | ItemKind::EmptyQuote | ItemKind::Picture => {}
        }
    }
    out
}

/// Image extensions the media engine decodes.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "ico", "tif", "tiff",
];

/// Note extensions previewed as Markdown.
const NOTE_EXTENSIONS: &[&str] = &["md", "markdown"];

/// The absolute path of a file node's `file` (a vault path). Without a
/// vault, paths are taken from the canvas's folder.
pub(crate) fn file_path(root: &Path, file: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in file.split('/').filter(|s| !s.is_empty() && *s != ".") {
        p.push(part);
    }
    p
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Loads what file card `path` shows (blocking: the background executor
/// only). `root` bounds what an SVG may load; `animate` is false with
/// reduced motion; `pdf` draws a PDF's first page.
pub(crate) fn load_preview(
    path: &Path,
    root: &Path,
    subpath: Option<&Subpath>,
    profile: &CompiledProfile,
    animate: bool,
    pdf: Option<&dyn PdfEngine>,
) -> Loaded {
    use std::io::Read as _;
    let ext = extension(path);
    if !path.is_file() {
        return Loaded::Missing("File not found".into());
    }
    if NOTE_EXTENSIONS.contains(&ext.as_str()) {
        let mut bytes = Vec::new();
        let read = std::fs::File::open(path)
            .and_then(|f| f.take(READ_CAP).read_to_end(&mut bytes).map(|_| ()));
        if let Err(e) = read {
            return Loaded::Missing(format!("Cannot read the file: {e}").into());
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let text = match subpath {
            Some(sub) => match section(&text, sub, profile) {
                Some(s) => s,
                None => {
                    return Loaded::Missing("The heading or block is not in the note".into());
                }
            },
            None => text,
        };
        let shown = cap_at_line(&text, PREVIEW_BYTES);
        return Loaded::Markdown(Parsed::new(shown.into(), profile));
    }
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        let request = gv_media::DecodeRequest {
            fit: Some((IMAGE_FIT, IMAGE_FIT)),
            animate,
        };
        let engine = crate::reader::media::media_engine(root);
        return match engine.load_path(path, request, &gv_media::CancelFlag::new()) {
            Ok(decoded) => match crate::reader::media::render_image(&decoded) {
                Some(image) => Loaded::Image(image),
                None => Loaded::Missing("The image has no frames".into()),
            },
            Err(e) => Loaded::Missing(format!("Cannot show the image: {e}").into()),
        };
    }
    if ext == "pdf" {
        if let Some(engine) = pdf
            && let Some(image) = pdf_first_page(path, engine)
        {
            return Loaded::Image(image);
        }
        return Loaded::Other("PDF".into());
    }
    if ext == "canvas" {
        return match std::fs::read_to_string(path) {
            Ok(text) => match gv_canvas::Canvas::parse(&text) {
                Ok(canvas) => Loaded::Canvas(Box::new(Doc::new(canvas))),
                Err(e) => Loaded::Missing(format!("Cannot read the canvas: {e}").into()),
            },
            Err(e) => Loaded::Missing(format!("Cannot read the file: {e}").into()),
        };
    }
    Loaded::Other(ext.to_uppercase().into())
}

/// First pages drawn, by file, size and modification time.
type PdfPages = Vec<((PathBuf, u64, Option<SystemTime>), Arc<RenderImage>)>;

fn pdf_pages() -> &'static Mutex<PdfPages> {
    static PAGES: Mutex<PdfPages> = Mutex::new(Vec::new());
    &PAGES
}

/// The first page of the PDF at `path` drawn by `engine` (blocking), from
/// the cache when the file did not change; `None` when it cannot be drawn.
fn pdf_first_page(path: &Path, engine: &dyn PdfEngine) -> Option<Arc<RenderImage>> {
    use std::io::Read as _;
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > PDF_CAP {
        return None;
    }
    let key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    {
        let mut pages = pdf_pages().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = pages.iter().position(|(k, _)| *k == key) {
            let hit = pages.remove(i);
            let image = Arc::clone(&hit.1);
            pages.push(hit);
            return Some(image);
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(PDF_CAP).read_to_end(&mut bytes))
        .ok()?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let raster = match engine.page(&bytes, 0, IMAGE_FIT, IMAGE_FIT, &cancel) {
        Ok(raster) => raster,
        Err(e) => {
            tracing::debug!("canvas: no first page for a PDF card: {e}");
            return None;
        }
    };
    let (image, _, _) = crate::export::dialog::raster_image(raster)?;
    let mut pages = pdf_pages().lock().unwrap_or_else(PoisonError::into_inner);
    pages.retain(|(k, _)| k.0 != key.0);
    if pages.len() >= PDF_CACHE {
        pages.remove(0);
    }
    pages.push((key, Arc::clone(&image)));
    Some(image)
}

/// Loads group background `background` (a vault path from `root`;
/// blocking: the background executor only). Remote addresses are not
/// fetched.
pub(crate) fn load_background(root: &Path, background: &str) -> Background {
    if background.contains("://") || background.starts_with("//") {
        return Background::None;
    }
    let path = file_path(root, background);
    if !path.is_file() {
        return Background::None;
    }
    let request = gv_media::DecodeRequest {
        fit: Some((BACKGROUND_FIT, BACKGROUND_FIT)),
        animate: false,
    };
    let engine = crate::reader::media::media_engine(root);
    match engine.load_path(&path, request, &gv_media::CancelFlag::new()) {
        Ok(decoded) => match crate::reader::media::render_image(&decoded) {
            #[allow(clippy::cast_precision_loss)]
            Some(image) => Background::Ready(BackgroundImage {
                image,
                width: decoded.source_width.max(1) as f32,
                height: decoded.source_height.max(1) as f32,
            }),
            None => Background::None,
        },
        Err(_) => Background::None,
    }
}

/// The part of `text` a subpath names: a heading's section (a chain
/// narrows section by section) or a block id's block.
fn section(text: &str, sub: &Subpath, profile: &CompiledProfile) -> Option<String> {
    let syntax = gv_syntax::parse_with(text, ParseOptions::from_profile(profile));
    let model = DocModel::build(&syntax, text);
    match sub {
        Subpath::Heading(parts) => {
            let mut range = 0..text.len();
            for part in parts {
                let want = part.trim();
                let (i, _) = model.headings().iter().enumerate().find(|(_, h)| {
                    h.range.start >= range.start
                        && h.range.start < range.end
                        && h.text.trim().eq_ignore_ascii_case(want)
                })?;
                range = model.section(i)?;
            }
            text.get(range).map(str::to_owned)
        }
        Subpath::Block(id) => {
            let a = model
                .block_anchors()
                .iter()
                .find(|a| a.id.eq_ignore_ascii_case(id))?;
            let mut start = a.range.start;
            if a.own_line {
                // The id names the block before it.
                start = text[..start].trim_end().rfind("\n\n").map_or(0, |i| i + 2);
            }
            text.get(start..a.range.end).map(str::to_owned)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obsidian() -> CompiledProfile {
        gv_profile::compile_builtin(ProfileId::Obsidian)
    }

    #[test]
    fn buckets_step_by_an_eighth_octave() {
        assert_eq!(bucket(1.0), 0);
        assert_eq!(bucket(2.0), 8);
        assert_eq!(bucket(0.5), -8);
        assert_eq!(bucket(1.04), 0);
        assert_eq!(bucket(1.06), 1);
        assert!((bucket_scale(8) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn subpaths_narrow_to_a_section_or_a_block() {
        let text = "# A\n\nintro\n\n## B\n\nin b\n\n## C\n\nin c ^blk\n\n# D\n\nlast\n";
        let p = obsidian();
        let b = section(text, &Subpath::parse("#A#B").expect("sub"), &p).expect("found");
        assert!(
            b.starts_with("## B") && b.contains("in b") && !b.contains("in c"),
            "{b:?}"
        );
        let d = section(text, &Subpath::parse("#d").expect("sub"), &p).expect("found");
        assert!(d.starts_with("# D"), "{d:?}");
        let blk = section(text, &Subpath::parse("#^blk").expect("sub"), &p).expect("found");
        assert!(blk.contains("in c") && !blk.contains("in b"), "{blk:?}");
        assert!(section(text, &Subpath::parse("#Nope").expect("sub"), &p).is_none());
    }

    #[test]
    fn previews_load_by_kind() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path();
        std::fs::write(root.join("n.md"), "# Title\n\nBody text.\n").expect("write");
        std::fs::write(root.join("x.pdf"), b"%PDF-1.4").expect("write");
        std::fs::write(
            root.join("c.canvas"),
            r#"{"nodes":[{"id":"a","type":"text","text":"x","x":0,"y":0,"width":10,"height":10}]}"#,
        )
        .expect("write");
        let p = obsidian();
        assert!(matches!(
            load_preview(&root.join("n.md"), root, None, &p, false, None),
            Loaded::Markdown(m) if m.text.contains("Body text.")
        ));
        assert!(matches!(
            load_preview(&root.join("x.pdf"), root, None, &p, false, None),
            Loaded::Other(e) if e.as_ref() == "PDF"
        ));
        assert!(matches!(
            load_preview(&root.join("c.canvas"), root, None, &p, false, None),
            Loaded::Canvas(d) if d.canvas.nodes().len() == 1
        ));
        assert!(matches!(
            load_preview(&root.join("gone.md"), root, None, &p, false, None),
            Loaded::Missing(_)
        ));
    }

    #[test]
    fn markdown_builds_blocks_with_markers() {
        let p = obsidian();
        let parsed = Parsed::new(
            "# Head\n\nSome *text*.\n\n- one\n- two\n\n```\ncode\n```\n\n---\n".into(),
            &p,
        );
        let blocks = build_blocks(&parsed, RenderOptions::for_profile(&p));
        let texts = blocks
            .iter()
            .filter(|b| matches!(b, Block::Text { .. }))
            .count();
        assert_eq!(texts, 4, "{blocks:?}");
        let markers: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { marker, .. } => marker.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(markers.len(), 2);
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::Code { text, .. } if text.as_ref() == "code"))
        );
        assert!(blocks.iter().any(|b| matches!(b, Block::Rule { .. })));
    }

    #[test]
    fn long_text_is_cut_at_a_line() {
        let text = "aaaa\nbbbb\ncccc\n";
        assert_eq!(cap_at_line(text, 12), "aaaa\nbbbb\n");
        assert_eq!(cap_at_line(text, 100), text);
    }
}

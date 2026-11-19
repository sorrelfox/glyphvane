// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What cards show at full detail.
//!
//! - **Text cards**: their Markdown through the Reading view's pipeline
//!   (parse, model, render tree in the Obsidian profile) and drawn as
//!   [`InlineParagraph`](crate::inline::InlineParagraph)s: paragraphs,
//!   headings, list items (with their markers; task items with a
//!   read-only checkbox) and table rows keep their styles, formulas
//!   (inline and display) are typeset; fenced code is highlighted as in
//!   the Reading view (same highlighter, same theme tokens) and Mermaid
//!   diagrams are drawn by its diagram engine, both off the UI thread
//!   ([`Job`]): until a result lands, and when a diagram cannot be drawn,
//!   the block shows as plain monospaced text; front matter is not shown
//!   (as in Obsidian's embeds); images and link status icons inside the
//!   text are not drawn (their boxes stay blank).
//! - **HTML** goes the Reading view's way, with only what it supports: a
//!   `<details>` element shows its summary with a disclosure triangle
//!   (read-only) and hides its content unless it has `open`, as a closed
//!   foldable callout hides its body; any other HTML block shows its
//!   source as code-like text under an *html* label, never run or loaded
//!   (the same lines the Reading view draws, through
//!   [`code_paragraph`](crate::reader::code_paragraph)); `<audio>`,
//!   `<video>` and `<iframe>` blocks are live blocks (below); a
//!   `<picture>` is an image, not drawn. Nothing here needs a job.
//! - **Live blocks** (` ```base ` and ` ```query ` fences, base, canvas
//!   and media embeds, HTML media): a placeholder card, built with the
//!   block, that names the kind and what it shows (`Base: Tasks.base`,
//!   the query's first line) and says to open the note. The Reading view
//!   fills these in from the vault (a hosted bases table, a vault-wide
//!   search, a file read), too much for every card of a canvas; nothing
//!   is read or run for them. Not designed: built plainly from the card's
//!   tokens, flagged.
//! - **File cards**: read off the UI thread when first shown. A note shows
//!   its first blocks (the section of `subpath` when given: `#Heading`,
//!   `#Parent#Child` or `#^block`), an image is decoded through the media
//!   engine the Reading view uses, a PDF shows its first page drawn by the
//!   PDF engine of the export commands (the helper process, asked for one
//!   page at a time by the view's queue; cached by file, size and time; a
//!   file card without an engine that draws pages or when drawing fails),
//!   a `.canvas` file shows its shapes, any other file a file card.
//!   Missing files say so, and so do paths that leave the vault
//!   ([`file_path`]), which are never read.
//! - **Group backgrounds**: a group's `background` (a vault path) is
//!   decoded through the same media engine when the group is first drawn
//!   and painted under the group's content as `backgroundStyle` says
//!   (`cover`, the default, `ratio`, `repeat`). A remote address is never
//!   fetched: the group is drawn without it.
//! - **Link cards**: the URL only. Nothing is fetched (remote content
//!   policy).
//!
//! Content is built at a zoom *bucket* (steps of 2^(1/8), about 9 %), so a
//! zoom gesture builds each card at most once per step. Highlighting and
//! diagrams do not depend on the bucket: a card asks for each once, and is
//! built again when it lands.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use gpui::{RenderImage, SharedString};
use gv_canvas::Subpath;
use gv_diagram::{DiagramKey, Outcome, Trust};
use gv_highlight::HighlightKey;
use gv_inline::{Align, BoxKind, InlineBox, Paragraph, ParagraphBuilder};
use gv_model::DocModel;
use gv_profile::{CompiledProfile, ProfileId};
use gv_render_tree::{
    CodeBlock, FrameKind, Item, ItemId, ItemKind, ListMarker, LiveBlock, LiveKind, NodeKind,
    RenderOptions, RenderTree, Sources, StyleTable, TextRole, Variant,
};
use gv_syntax::{ParseOptions, SyntaxTree};

use super::Doc;
use crate::export::PdfEngine;
use crate::icons::Icon;
use crate::inline::{BoxContent, FontFamilies, FontTable, InlineBlock, InlineText, font_of};
use crate::reader::code_highlight::{self, LineSpans};
use crate::reader::diagram::{CancelSlot, Request};

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

/// Diagrams kept per canvas (the drawn ones are forgotten beyond).
const DIAGRAMS_CAP: usize = 512;
/// A code block's padding, in body font sizes.
pub(crate) const CODE_PAD_EM: f32 = 0.4;

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

/// A list item's marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Marker {
    /// A bullet or a number, as text.
    Text(SharedString),
    /// A task item's checkbox (read-only).
    Task { checked: bool },
    /// A `<details>` summary's disclosure triangle (read-only): open or
    /// closed.
    Disclosure { open: bool },
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
        marker: Option<Marker>,
    },
    /// Code-like text in the mono family: a code block's lines
    /// (highlighted once its result is in), a diagram's source while it is
    /// drawn or when it cannot be, an HTML block's source. `label` is shown
    /// above it (*html* on an HTML block's first chunk).
    Code {
        block: Rc<RefCell<InlineBlock>>,
        indent: f32,
        gap: f32,
        label: Option<SharedString>,
    },
    /// A drawn Mermaid diagram: one box at the build's scale, fitted to the
    /// card's width by the view ([`InlineBlock::fit_boxes`]).
    Diagram {
        block: Rc<RefCell<InlineBlock>>,
        indent: f32,
        gap: f32,
    },
    /// A live block's placeholder card: the kind's icon, a title naming
    /// the kind and what it shows, and a note saying where to see it.
    Live {
        icon: Icon,
        title: SharedString,
        note: SharedString,
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
    /// Body line height at the bucket's scale (markers are centred in it).
    pub body_line: f32,
}

/// Work the view runs off the UI thread for the cards' blocks
/// ([`Cards::take_jobs`]); its result goes back through
/// [`Cards::highlighted`] or [`Cards::diagram_landed`].
#[derive(Debug)]
pub(crate) enum Job {
    /// Highlights a code block's text into the shared cache
    /// ([`code_highlight::highlight`]).
    Highlight(HighlightKey, Arc<str>),
    /// Renders a Mermaid diagram ([`crate::reader::diagram::run`]).
    Diagram(Request),
}

/// What a built card waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Wait {
    Code(HighlightKey),
    Diagram(DiagramKey),
}

/// A diagram asked for.
#[derive(Debug)]
enum DiagramState {
    Running(Arc<CancelSlot>),
    /// `None`: the engine cannot start.
    Done(Option<Outcome>),
}

/// The highlighting and diagram jobs of the cards' blocks.
#[derive(Debug, Default)]
pub(crate) struct Work {
    highlights: code_highlight::Jobs,
    diagrams: HashMap<DiagramKey, DiagramState>,
    requests: Vec<Job>,
}

impl Work {
    /// The highlighting of `code` if it is in the shared cache; else
    /// `None` (it stays plain), and when it can be highlighted, its job is
    /// asked for once and its key pushed to `pending`. Never highlights.
    fn spans(&mut self, code: &CodeBlock, pending: &mut Vec<Wait>) -> Option<LineSpans> {
        let info = code.info.as_deref().or(code.language.as_deref());
        let key = info
            .filter(|_| code.fenced)
            .and_then(|info| code_highlight::key_for(&code.text, info))?;
        if let Some(h) = code_highlight::cached(&key) {
            let lines: Arc<[std::ops::Range<usize>]> =
                code.lines.iter().map(|l| l.text.clone()).collect();
            return Some(LineSpans::new(h, lines));
        }
        pending.push(Wait::Code(key));
        if self.highlights.start(key) {
            self.requests
                .push(Job::Highlight(key, Arc::from(code.text.as_str())));
        }
        None
    }

    /// The diagram of `code` when it was drawn; else `None` (its source is
    /// shown), and while it is drawn, its job is asked for once and its
    /// key pushed to `pending`. Never renders.
    fn diagram(
        &mut self,
        code: &CodeBlock,
        pending: &mut Vec<Wait>,
    ) -> Option<Arc<gv_diagram::Diagram>> {
        let key = DiagramKey::new(&code.text, Trust::Interactive);
        match self.diagrams.get(&key) {
            Some(DiagramState::Done(Some(Outcome::Rendered(d)))) => return Some(Arc::clone(d)),
            Some(DiagramState::Done(_)) => return None,
            Some(DiagramState::Running(_)) => {}
            None => {
                if self.diagrams.len() >= DIAGRAMS_CAP {
                    self.diagrams
                        .retain(|_, s| matches!(s, DiagramState::Running(_)));
                }
                let slot = Arc::new(CancelSlot::default());
                self.diagrams
                    .insert(key, DiagramState::Running(Arc::clone(&slot)));
                self.requests.push(Job::Diagram(Request {
                    key,
                    source: Arc::from(code.text.as_str()),
                    trust: Trust::Interactive,
                    slot,
                }));
            }
        }
        pending.push(Wait::Diagram(key));
        None
    }

    /// Cancels the diagrams being drawn and forgets every diagram; the
    /// highlighting jobs not started yet are dropped.
    fn clear(&mut self) {
        for state in self.diagrams.values() {
            if let DiagramState::Running(slot) = state {
                slot.cancel();
            }
        }
        self.diagrams.clear();
        for job in self.requests.drain(..) {
            if let Job::Highlight(key, _) = job {
                self.highlights.finish(&key);
            }
        }
    }
}

/// The cards' content: parsed texts, file previews and built Markdown.
#[derive(Debug)]
pub(crate) struct Cards {
    pub text: Rc<RefCell<InlineText>>,
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
    /// Highlighting and diagram jobs.
    work: Work,
    /// The nodes whose built Markdown waits for a job's result.
    waiting: HashMap<Wait, Vec<usize>>,
    /// Builds made (for tests and the culling check).
    pub builds: u64,
}

impl Drop for Cards {
    fn drop(&mut self) {
        self.work.clear();
    }
}

impl Cards {
    pub fn new(families: &FontFamilies) -> Self {
        let profile = Arc::new(gv_profile::compile_builtin(ProfileId::Obsidian));
        let options = RenderOptions::for_profile(&profile);
        let fonts = font_table(&options.styles, families);
        Self {
            text: Rc::new(RefCell::new(InlineText::new(fonts))),
            profile,
            options,
            parsed: HashMap::new(),
            previews: HashMap::new(),
            built: HashMap::new(),
            backgrounds: HashMap::new(),
            work: Work::default(),
            waiting: HashMap::new(),
            builds: 0,
        }
    }

    /// Forgets everything (a new canvas was loaded).
    pub fn clear(&mut self) {
        self.parsed.clear();
        self.previews.clear();
        self.built.clear();
        self.backgrounds.clear();
        self.work.clear();
        self.waiting.clear();
    }

    /// The jobs to start.
    pub fn take_jobs(&mut self) -> Vec<Job> {
        std::mem::take(&mut self.work.requests)
    }

    /// Drops the built Markdown of the nodes waiting for `wait`; whether
    /// there were any (the view draws again).
    fn landed(&mut self, wait: Wait) -> bool {
        let nodes = self.waiting.remove(&wait).unwrap_or_default();
        for node in &nodes {
            self.built.remove(node);
        }
        !nodes.is_empty()
    }

    /// The highlighting job of `key` ended (its result is in the shared
    /// cache): whether a card is to be built again.
    pub fn highlighted(&mut self, key: &HighlightKey) -> bool {
        self.work.highlights.finish(key);
        self.landed(Wait::Code(*key))
    }

    /// A diagram job ended with `outcome` (`None`: the engine cannot
    /// start): whether a card is to be built again. A job of a canvas
    /// shown before is ignored.
    pub fn diagram_landed(&mut self, request: &Request, outcome: Option<Outcome>) -> bool {
        let ours = matches!(
            self.work.diagrams.get(&request.key),
            Some(DiagramState::Running(slot)) if Arc::ptr_eq(slot, &request.slot)
        );
        if !ours {
            return false;
        }
        if matches!(outcome, Some(Outcome::Cancelled)) {
            // Asked for again when its card is built again.
            self.work.diagrams.remove(&request.key);
        } else {
            self.work
                .diagrams
                .insert(request.key, DiagramState::Done(outcome));
        }
        self.landed(Wait::Diagram(request.key))
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
        let body = options.styles.role(gv_render_tree::TextRole::Body);
        let (body_size, body_line) = (body.size, body.line_height);
        let mut pending = Vec::new();
        let blocks = build_blocks(parsed, options, &mut self.work, &mut pending);
        for wait in pending {
            let nodes = self.waiting.entry(wait).or_default();
            if !nodes.contains(&node) {
                nodes.push(node);
            }
        }
        self.builds += 1;
        let b = Built {
            bucket,
            blocks: Rc::new(blocks),
            body_size,
            body_line,
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

/// `text`'s lines as a paragraph of plain code.
fn plain_code(text: &str, styles: &StyleTable) -> Paragraph {
    let mut b = ParagraphBuilder::new(styles.paragraph_style(TextRole::Code));
    let style = styles.run_style(TextRole::Code, Variant::PLAIN);
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            b.hard_break(0..0);
        }
        if !line.is_empty() {
            b.text(line, style, 0..0);
        }
    }
    b.build()
}

/// Diagram `diagram` drawn at `scale` px per diagram px: one box, centred.
fn diagram_block(
    diagram: Arc<gv_diagram::Diagram>,
    scale: f32,
    styles: &StyleTable,
) -> InlineBlock {
    let style = styles
        .paragraph_style(TextRole::Body)
        .with_align(Align::Center);
    let mut b = ParagraphBuilder::new(style);
    let (w, h) = (diagram.width * scale, diagram.height * scale);
    b.inline_box(InlineBox::new(BoxKind::Other, w, h, 0.0), 0..0);
    let mut block = InlineBlock::new(Arc::new(b.build()));
    block.set_box_content(0, BoxContent::Diagram { diagram, scale });
    block
}

/// The blocks of `parsed` built with `options`. Code is highlighted and
/// diagrams drawn when `work` has their results; what waits for one is
/// pushed to `pending`.
fn build_blocks(
    parsed: &Parsed,
    options: RenderOptions,
    work: &mut Work,
    pending: &mut Vec<Wait>,
) -> Vec<Block> {
    let tree = RenderTree::build(
        &parsed.syntax,
        &parsed.model,
        &*parsed.text,
        options,
        Sources::default(),
    );
    let metrics = tree.options().metrics;
    let styles = &tree.options().styles;
    let code_block = |p: Paragraph| Rc::new(RefCell::new(InlineBlock::new(Arc::new(p))));
    let mut out = Vec::new();
    for (ix, item) in tree.items().iter().enumerate() {
        let Some(node) = tree.node_of(ix) else {
            continue;
        };
        if folded(&tree, item) {
            continue;
        }
        let indent = item.content_x(&metrics);
        let gap = item.gap_before;
        match (&item.kind, &node.kind) {
            (
                ItemKind::Paragraph
                | ItemKind::Heading { .. }
                | ItemKind::Details
                | ItemKind::TableRow { .. }
                | ItemKind::Math { .. },
                _,
            ) => {
                // Formulas typeset as the Reading view does.
                let block = match crate::reader::standalone_block(&tree, ix) {
                    Some(block) => block,
                    None => match node.inline() {
                        Some(content) => InlineBlock::new(content.paragraph.clone()),
                        None => continue,
                    },
                };
                let disclosure = match node.kind {
                    NodeKind::Details { .. } => Some(Marker::Disclosure {
                        open: open_by_default(&tree, item.id.block),
                    }),
                    _ => None,
                };
                let marker = (item.opens > 0)
                    .then(|| item.context.last())
                    .flatten()
                    .and_then(|f| match &f.kind {
                        FrameKind::Item {
                            marker: ListMarker::Task { checked, .. },
                        } => Some(Marker::Task { checked: *checked }),
                        FrameKind::Item { marker } => {
                            Some(Marker::Text(SharedString::from(marker.text())))
                        }
                        _ => None,
                    })
                    .or(disclosure);
                out.push(Block::Text {
                    block: Rc::new(RefCell::new(block)),
                    indent,
                    gap,
                    marker,
                });
            }
            // Front matter is not shown, as in Obsidian's embeds.
            (ItemKind::Properties { .. }, _) => {}
            (ItemKind::Code { lines, .. }, NodeKind::Code(code)) => {
                let spans = work.spans(code, pending);
                let lines = lines.start as usize..lines.end as usize;
                out.push(Block::Code {
                    block: code_block(crate::reader::code_paragraph(
                        code,
                        lines,
                        styles,
                        spans.as_ref(),
                    )),
                    indent,
                    gap,
                    label: None,
                });
            }
            // Raw HTML: its source, as the Reading view shows it.
            (ItemKind::Html { chunk, lines, .. }, NodeKind::Html { code, .. }) => {
                let lines = lines.start as usize..lines.end as usize;
                out.push(Block::Code {
                    block: code_block(crate::reader::code_paragraph(code, lines, styles, None)),
                    indent,
                    gap,
                    label: (*chunk == 0).then(|| SharedString::from("html")),
                });
            }
            (ItemKind::Live { .. }, NodeKind::Live(live)) => {
                let (icon, title, note) = live_placeholder(live);
                out.push(Block::Live {
                    icon,
                    title,
                    note,
                    indent,
                    gap,
                });
            }
            (ItemKind::Diagram { .. }, NodeKind::Diagram(code)) => {
                let block = match work.diagram(code, pending) {
                    Some(d) => Block::Diagram {
                        block: Rc::new(RefCell::new(diagram_block(d, styles.zoom(), styles))),
                        indent,
                        gap,
                    },
                    None => Block::Code {
                        block: code_block(crate::reader::code_paragraph(
                            code,
                            0..code.lines.len(),
                            styles,
                            None,
                        )),
                        indent,
                        gap,
                        label: None,
                    },
                };
                out.push(block);
            }
            (
                ItemKind::Code { .. }
                | ItemKind::Html { .. }
                | ItemKind::Diagram { .. }
                | ItemKind::Live { .. },
                _,
            ) => {
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
                    block: code_block(plain_code(&text, styles)),
                    indent,
                    gap,
                    label: None,
                });
            }
            (ItemKind::ThematicBreak, _) => out.push(Block::Rule { gap }),
            (ItemKind::EmptyItem | ItemKind::EmptyQuote | ItemKind::Picture, _) => {}
        }
    }
    out
}

/// Whether the `<details>` or foldable callout whose first row is block
/// `block` of `tree` is open as written (the Reading view's state before
/// the reader folds anything).
fn open_by_default(tree: &RenderTree, block: gv_syntax::BlockId) -> bool {
    tree.index_of(ItemId { block, chunk: 0 })
        .and_then(|ix| tree.node_of(ix))
        .is_some_and(|n| match n.kind {
            NodeKind::Details { open, .. } => open,
            NodeKind::CalloutTitle { fold, .. } => fold != gv_syntax::CalloutFold::Closed,
            _ => false,
        })
}

/// Whether `item` is inside a closed `<details>` or a closed callout.
fn folded(tree: &RenderTree, item: &Item) -> bool {
    item.context
        .iter()
        .any(|f| matches!(f.kind, FrameKind::Details) && !open_by_default(tree, f.id))
}

/// A live block's placeholder: the kind's icon (the Reading view's), its
/// title and its note.
fn live_placeholder(live: &LiveBlock) -> (Icon, SharedString, SharedString) {
    let (icon, kind) = match live.kind {
        LiveKind::Canvas => (Icon::Canvas, "Canvas"),
        LiveKind::Base => (Icon::Base, "Base"),
        LiveKind::Query => (Icon::Search, "Query"),
        LiveKind::Audio => (Icon::AudioFile, "Audio"),
        LiveKind::Video => (Icon::VideoFile, "Video"),
        LiveKind::Pdf => (Icon::PdfFile, "PDF"),
        LiveKind::Frame => (Icon::WebPage, "Embedded page"),
    };
    let name = match (&live.target, live.kind) {
        (Some(t), k) if k.is_media() => live
            .html
            .as_ref()
            .and_then(|h| h.title.as_deref())
            .unwrap_or_else(|| t.rsplit('/').next().unwrap_or(t))
            .to_owned(),
        (Some(t), _) => {
            let name = t.rsplit('/').next().unwrap_or(t);
            match &live.anchor {
                Some(view) => format!("{name} \u{203a} {view}"),
                None => name.to_owned(),
            }
        }
        (None, LiveKind::Query) => live
            .fence_text()
            .and_then(|t| t.lines().map(str::trim).find(|l| !l.is_empty()))
            .unwrap_or("")
            .to_owned(),
        (None, _) => String::new(),
    };
    let title = if name.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}: {name}")
    };
    let note = match live.kind {
        LiveKind::Query => "Open the note to see the results",
        _ => "Open the note to see it",
    };
    (icon, title.into(), note.into())
}

/// Image extensions the media engine decodes.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "ico", "tif", "tiff",
];

/// Note extensions previewed as Markdown.
const NOTE_EXTENSIONS: &[&str] = &["md", "markdown"];

/// The absolute path of a file node's `file` (a vault path; without a
/// vault, paths are taken from the canvas's folder), or `None` when it
/// would leave `root`:
///
/// - every `/`-separated part must be a plain name: no `..`, no `\`, no
///   `:` (a Windows drive, `\\server\share` or stream), nothing a
///   platform reads as a root or prefix;
/// - its real path (links resolved; of its nearest existing folder when
///   it is not there) must be inside the real `root`.
///
/// Blocking (it reads the file system): the background executor, or once
/// per user action.
pub(crate) fn file_path(root: &Path, file: &str) -> Option<PathBuf> {
    let p = lexical_path(root, file)?;
    let real_root = std::fs::canonicalize(root).ok()?;
    // The file, or its nearest folder that is there.
    let real = p
        .ancestors()
        .take_while(|a| a.starts_with(root))
        .find_map(|a| std::fs::canonicalize(a).ok())?;
    real.starts_with(&real_root).then_some(p)
}

/// `file`'s parts joined to `root`, when every part is a plain name.
fn lexical_path(root: &Path, file: &str) -> Option<PathBuf> {
    use std::path::Component;
    let mut p = root.to_path_buf();
    for part in file.split('/').filter(|s| !s.is_empty() && *s != ".") {
        if part == ".." || part.contains(['\\', ':', '\0']) {
            return None;
        }
        let mut parts = Path::new(part).components();
        if !matches!(
            (parts.next(), parts.next()),
            (Some(Component::Normal(_)), None)
        ) {
            return None;
        }
        p.push(part);
    }
    Some(p)
}

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Whether file card `file` is a PDF (its first page is drawn through the
/// view's one-at-a-time queue).
pub(crate) fn is_pdf(file: &str) -> bool {
    extension(Path::new(file)) == "pdf"
}

/// A PDF engine and the flag that stops its work.
pub(crate) type PdfPass<'a> = (&'a dyn PdfEngine, &'a AtomicBool);

/// Loads what file card `file` (a vault path from `root`) shows, nothing
/// when it would leave `root` (blocking: the background executor only).
/// See [`load_preview`].
pub(crate) fn load_file(
    root: &Path,
    file: &str,
    subpath: Option<&Subpath>,
    profile: &CompiledProfile,
    animate: bool,
    pdf: Option<PdfPass<'_>>,
) -> Loaded {
    match file_path(root, file) {
        Some(path) => load_preview(&path, root, subpath, profile, animate, pdf),
        None => Loaded::Missing("The file is outside the vault".into()),
    }
}

/// Loads what file card `path` shows (blocking: the background executor
/// only). `root` bounds what an SVG may load; `animate` is false with
/// reduced motion; `pdf` draws a PDF's first page unless its flag is set.
pub(crate) fn load_preview(
    path: &Path,
    root: &Path,
    subpath: Option<&Subpath>,
    profile: &CompiledProfile,
    animate: bool,
    pdf: Option<PdfPass<'_>>,
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
        if let Some((engine, cancel)) = pdf
            && let Some(image) = pdf_first_page(path, engine, cancel)
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
fn pdf_first_page(
    path: &Path,
    engine: &dyn PdfEngine,
    cancel: &AtomicBool,
) -> Option<Arc<RenderImage>> {
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
    if cancel.load(Ordering::Acquire) {
        return None;
    }
    let raster = match engine.page(&bytes, 0, IMAGE_FIT, IMAGE_FIT, cancel) {
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
    let Some(path) = file_path(root, background) else {
        return Background::None;
    };
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

    /// `parsed`'s blocks, with no results of jobs.
    fn blocks_of(parsed: &Parsed, profile: &CompiledProfile) -> Vec<Block> {
        let mut work = Work::default();
        build_blocks(
            parsed,
            RenderOptions::for_profile(profile),
            &mut work,
            &mut Vec::new(),
        )
    }

    /// The text of a code block.
    fn code_text(b: &Block) -> Option<String> {
        match b {
            Block::Code { block, .. } => Some(block.borrow().paragraph().text().to_string()),
            _ => None,
        }
    }

    #[test]
    fn cards_typeset_math_show_task_boxes_and_hide_front_matter() {
        let profile = obsidian();
        let text = "---\ntitle: Hidden\n---\n# Head\n\nInline $x$.\n\n$$\nh(t) = x^2\n$$\n\n- [ ] open\n- [x] done\n- plain\n";
        let parsed = Parsed::new(text.into(), &profile);
        let blocks = blocks_of(&parsed, &profile);
        // No raw blocks: the front matter is not shown, the formula is.
        assert!(
            blocks.iter().all(|b| !matches!(b, Block::Code { .. })),
            "{blocks:?}"
        );
        let math = |b: &Block| match b {
            Block::Text { block, .. } => {
                let block = block.borrow();
                block.paragraph().items().iter().any(|i| {
                    i.inline_box().is_some_and(|ib| {
                        matches!(
                            block.box_content(ib.id),
                            Some(crate::inline::BoxContent::Math { .. })
                        )
                    })
                })
            }
            _ => false,
        };
        assert_eq!(blocks.iter().filter(|b| math(b)).count(), 2, "{blocks:?}");
        let texts: Vec<String> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { block, .. } => Some(block.borrow().paragraph().text().to_string()),
                _ => None,
            })
            .collect();
        assert!(texts.iter().all(|t| !t.contains("Hidden")), "{texts:?}");
        let markers: Vec<Option<Marker>> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { marker, .. } => Some(marker.clone()),
                _ => None,
            })
            .filter(Option::is_some)
            .collect();
        assert_eq!(
            markers,
            [
                Some(Marker::Task { checked: false }),
                Some(Marker::Task { checked: true }),
                Some(Marker::Text("\u{2022}".into())),
            ]
        );
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
        let blocks = blocks_of(&parsed, &p);
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
                .any(|b| code_text(b).as_deref() == Some("code"))
        );
        assert!(blocks.iter().any(|b| matches!(b, Block::Rule { .. })));
    }

    #[test]
    fn file_paths_stay_in_the_root() {
        let dir = tempfile::tempdir().expect("dir");
        let root = dir.path().join("vault");
        std::fs::create_dir_all(root.join("notes")).expect("folder");
        std::fs::write(root.join("notes/a.md"), "a").expect("write");
        std::fs::write(dir.path().join("secret.md"), "s").expect("write");
        let inside = root.join("notes").join("a.md");
        assert_eq!(file_path(&root, "notes/a.md"), Some(inside.clone()));
        assert_eq!(file_path(&root, "./notes//a.md"), Some(inside));
        // Not there (yet): plain names are still inside.
        assert_eq!(
            file_path(&root, "notes/new.md"),
            Some(root.join("notes").join("new.md"))
        );
        for bad in [
            "../secret.md",
            "notes/../../secret.md",
            "..",
            "C:/Windows/x.md",
            "C:\\x.md",
            "\\\\server\\share\\x.md",
            "notes\\..\\..\\secret.md",
            "notes/a.md:stream",
        ] {
            assert_eq!(file_path(&root, bad), None, "{bad}");
        }
        assert!(matches!(
            load_file(&root, "../secret.md", None, &obsidian(), false, None),
            Loaded::Missing(why) if why.contains("outside")
        ));
        assert!(matches!(
            load_background(&root, "../secret.md"),
            Background::None
        ));
        // A link inside the vault to a file outside it.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), root.join("out")).expect("link");
            assert_eq!(file_path(&root, "out/secret.md"), None);
            assert_eq!(file_path(&root, "out/missing.md"), None);
        }
    }

    /// The colours of `block`'s text runs.
    fn run_colours(block: &Rc<RefCell<InlineBlock>>) -> Vec<gv_inline::ColorRole> {
        block
            .borrow()
            .paragraph()
            .items()
            .iter()
            .filter_map(|i| i.style().map(|s| s.color))
            .collect()
    }

    fn cards() -> Cards {
        Cards::new(&FontFamilies {
            text: "Sans".into(),
            mono: "Mono".into(),
        })
    }

    #[test]
    fn code_is_highlighted_once_its_job_lands_never_while_built() {
        let p = obsidian();
        let mut cards = cards();
        let parsed = Parsed::new(
            "Text.\n\n```rust\nfn card_code_highlight() -> u32 { 1 }\n```\n".into(),
            &p,
        );
        let jobs = code_highlight::tests::jobs_run();
        let built = cards.built(0, &parsed, 0);
        assert_eq!(
            code_highlight::tests::jobs_run(),
            jobs,
            "built without highlighting"
        );
        let plain = p_styles(&cards)
            .run_style(TextRole::Code, Variant::PLAIN)
            .color;
        let code = |built: &Built| {
            built
                .blocks
                .iter()
                .find_map(|b| match b {
                    Block::Code { block, .. } => Some(block.clone()),
                    _ => None,
                })
                .expect("a code block")
        };
        let first = code(&built);
        assert_eq!(
            first.borrow().paragraph().text(),
            "fn card_code_highlight() -> u32 { 1 }"
        );
        assert!(run_colours(&first).iter().all(|c| *c == plain));
        // One job, asked for once: the same bucket is not built again, and
        // another bucket asks for nothing more.
        cards.built(0, &parsed, 0);
        cards.built(0, &parsed, 1);
        assert_eq!(cards.builds, 2);
        let mut jobs = cards.take_jobs();
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        let Some(Job::Highlight(key, text)) = jobs.pop() else {
            panic!("a highlighting job");
        };
        code_highlight::highlight(key, &text);
        assert!(cards.highlighted(&key), "the card is built again");
        let again = cards.built(0, &parsed, 1);
        assert_eq!(cards.builds, 3);
        assert!(cards.take_jobs().is_empty());
        let colours = run_colours(&code(&again));
        let keyword = code_highlight::token_of(gv_highlight::TokenClass::Keyword).role();
        assert!(colours.contains(&keyword), "{colours:?}");
        // Another card with the same code: highlighted at once.
        let other = cards.built(1, &parsed, 0);
        assert_eq!(run_colours(&code(&other)), colours);
        assert!(cards.take_jobs().is_empty());
    }

    fn p_styles(cards: &Cards) -> StyleTable {
        cards.options.styles.clone()
    }

    #[test]
    fn diagrams_are_drawn_once_their_job_lands_with_the_source_meanwhile() {
        let p = obsidian();
        let mut cards = cards();
        let source = "flowchart LR\n  CardA --> CardB";
        let parsed = Parsed::new(format!("```mermaid\n{source}\n```\n").into(), &p);
        let built = cards.built(0, &parsed, 0);
        assert!(
            built
                .blocks
                .iter()
                .any(|b| code_text(b).as_deref() == Some(source)),
            "the source while it is drawn: {:?}",
            built.blocks
        );
        let mut jobs = cards.take_jobs();
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        let Some(Job::Diagram(request)) = jobs.pop() else {
            panic!("a diagram job");
        };
        let outcome = crate::reader::diagram::run(&request);
        assert!(matches!(outcome, Some(Outcome::Rendered(_))), "{outcome:?}");
        assert!(cards.diagram_landed(&request, outcome));
        let built = cards.built(0, &parsed, 0);
        let diagram = built
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Diagram { block, .. } => Some(block.clone()),
                _ => None,
            })
            .expect("the diagram is drawn");
        let block = diagram.borrow();
        assert!(matches!(
            block.box_content(0),
            Some(BoxContent::Diagram { scale, .. }) if (*scale - 1.0).abs() < 1e-6
        ));
        assert!(cards.take_jobs().is_empty());
        drop(block);
        // A wider bucket draws it bigger, with no new job.
        let big = cards.built(0, &parsed, 8);
        assert!(big.blocks.iter().any(|b| matches!(
            b,
            Block::Diagram { block, .. }
                if matches!(block.borrow().box_content(0),
                    Some(BoxContent::Diagram { scale, .. }) if (*scale - 2.0).abs() < 1e-3)
        )));
        assert!(cards.take_jobs().is_empty());
    }

    #[test]
    fn a_diagram_that_cannot_be_drawn_keeps_its_source() {
        let p = obsidian();
        let mut cards = cards();
        let parsed = Parsed::new("```mermaid\nnot a diagram at all\n```\n".into(), &p);
        cards.built(0, &parsed, 0);
        let Some(Job::Diagram(request)) = cards.take_jobs().pop() else {
            panic!("a diagram job");
        };
        let outcome = crate::reader::diagram::run(&request);
        assert!(
            !matches!(outcome, Some(Outcome::Rendered(_))),
            "{outcome:?}"
        );
        assert!(cards.diagram_landed(&request, outcome));
        let built = cards.built(0, &parsed, 0);
        assert!(
            built
                .blocks
                .iter()
                .any(|b| code_text(b).as_deref() == Some("not a diagram at all"))
        );
        assert!(
            !built
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Diagram { .. }))
        );
        assert!(cards.take_jobs().is_empty(), "not asked for again");
    }

    #[test]
    fn jobs_of_a_canvas_shown_before_are_ignored() {
        let p = obsidian();
        let mut cards = cards();
        let parsed = Parsed::new(
            "```mermaid\nflowchart TD\n  Old --> Gone\n```\n\n```rust\nlet card_gone = 1;\n```\n"
                .into(),
            &p,
        );
        cards.built(0, &parsed, 0);
        let jobs = cards.take_jobs();
        assert_eq!(jobs.len(), 2, "{jobs:?}");
        cards.clear();
        for job in jobs {
            match job {
                Job::Diagram(request) => {
                    assert!(request.slot.is_cancelled(), "cancelled by the new canvas");
                    assert!(!cards.diagram_landed(&request, Some(Outcome::Cancelled)));
                }
                Job::Highlight(key, text) => {
                    code_highlight::highlight(key, &text);
                    assert!(!cards.highlighted(&key), "no card waits for it");
                }
            }
        }
        // The new canvas asks again.
        cards.built(0, &parsed, 0);
        assert_eq!(
            cards.take_jobs().len(),
            1,
            "the diagram; the code is cached"
        );
    }

    /// The plain text of the text blocks.
    fn texts(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { block, .. } => Some(block.borrow().paragraph().text().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn html_blocks_show_as_the_reading_view_shows_them() {
        let p = obsidian();
        let text = "<div class=\"note\">\n<b>Raw</b> <script>alert(1)</script>\n</div>\n\n\
            <details>\n<summary>Shut</summary>\n\nHidden body.\n\n</details>\n\n\
            <details open>\n<summary>Open</summary>\n\nShown body.\n\n</details>\n\n\
            > [!note]- Folded\n> Callout body.\n\nAfter.\n";
        let parsed = Parsed::new(text.into(), &p);
        let mut work = Work::default();
        let mut pending = Vec::new();
        let blocks = build_blocks(
            &parsed,
            RenderOptions::for_profile(&p),
            &mut work,
            &mut pending,
        );
        // Raw HTML: its source as code-like text, labelled, never run.
        let html: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code { block, label, .. } => {
                    Some((block.borrow().paragraph().text().to_string(), label.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            html,
            [(
                "<div class=\"note\">\n<b>Raw</b> <script>alert(1)</script>\n</div>".to_owned(),
                Some(SharedString::from("html"))
            )],
            "{blocks:?}"
        );
        // `<details>`: the summary with its triangle; the body only when
        // open, as written; a closed callout's body is folded too.
        let texts = texts(&blocks);
        assert!(texts.iter().any(|t| t == "Shut"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Open"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Shown body."), "{texts:?}");
        assert!(texts.iter().any(|t| t == "After."), "{texts:?}");
        assert!(
            texts
                .iter()
                .all(|t| !t.contains("Hidden body") && !t.contains("Callout body")),
            "{texts:?}"
        );
        let triangles: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text {
                    marker: Some(m @ Marker::Disclosure { .. }),
                    ..
                } => Some(m.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            triangles,
            [
                Marker::Disclosure { open: false },
                Marker::Disclosure { open: true }
            ]
        );
        assert!(pending.is_empty() && work.requests.is_empty(), "no jobs");
    }

    /// The placeholders of the live blocks: (icon, title, note).
    fn lives(blocks: &[Block]) -> Vec<(Icon, String, String)> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Live {
                    icon, title, note, ..
                } => Some((*icon, title.to_string(), note.to_string())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn live_blocks_show_a_placeholder_and_run_nothing() {
        let p = obsidian();
        let text = "```base\nviews:\n  - type: table\n    name: All\n```\n\n\
            ```query\n\n  tag:#card-query\n```\n\n\
            ![[Boards/Tasks.base#Open]]\n\n\
            ![[Plan.canvas]]\n\n\
            <video src=\"clip.mp4\" title=\"Demo clip\">\n</video>\n";
        let parsed = Parsed::new(text.into(), &p);
        let mut work = Work::default();
        let mut pending = Vec::new();
        let blocks = build_blocks(
            &parsed,
            RenderOptions::for_profile(&p),
            &mut work,
            &mut pending,
        );
        let see = "Open the note to see it".to_owned();
        assert_eq!(
            lives(&blocks),
            [
                (Icon::Base, "Base".to_owned(), see.clone()),
                (
                    Icon::Search,
                    "Query: tag:#card-query".to_owned(),
                    "Open the note to see the results".to_owned()
                ),
                (
                    Icon::Base,
                    "Base: Tasks.base \u{203a} Open".to_owned(),
                    see.clone()
                ),
                (Icon::Canvas, "Canvas: Plan.canvas".to_owned(), see.clone()),
                (Icon::VideoFile, "Video: Demo clip".to_owned(), see),
            ],
            "{blocks:?}"
        );
        // Nothing shows the fences' text, and nothing is asked for.
        assert!(
            blocks.iter().all(|b| !matches!(b, Block::Code { .. })),
            "{blocks:?}"
        );
        assert!(pending.is_empty() && work.requests.is_empty(), "no jobs");
    }

    #[test]
    fn long_text_is_cut_at_a_line() {
        let text = "aaaa\nbbbb\ncccc\n";
        assert_eq!(cap_at_line(text, 12), "aaaa\nbbbb\n");
        assert_eq!(cap_at_line(text, 100), text);
    }
}

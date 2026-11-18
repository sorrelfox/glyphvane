// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live Preview block widgets (FR-EDIT-17, phase 3): blocks rendered in
//! place of their source lines while no selection touches them.
//!
//! # What becomes a block
//!
//! Top-level blocks only (not inside a list or a quote): fenced and
//! indented code (Mermaid and other diagrams included), tables, display
//! math `$$…$$`, callouts (the whole callout), thematic breaks, front
//! matter, and paragraphs that are one line holding only an image or an
//! embed (`![alt](src)`, `![[note]]`, `![[note#heading]]`, `![[img.png|200]]`).
//!
//! # Rendering
//!
//! Each block is drawn by a Reading view of its own source
//! ([`Reader::for_block`]): the same renderers as the Reading view (code
//! highlighting with its language label, tables, math, diagrams laid out
//! off the UI thread, callouts with their fold toggle, embeds, front matter
//! as properties, images at their size), with the note's context around
//! the source (reference and footnote definitions, footnote numbers, math
//! macros: [`super::live_context`]). The editor hosts the view at the
//! block's place in the text column and sizes the block to the view's
//! content height once it is laid out ([`EditorView::block_heights`]);
//! until then the block is as tall as it was last measured, else as
//! estimated from the blocks of its kind measured so far (a least-squares
//! line over their source line counts), else as its source rows. A block
//! above the top of the view that changes height moves the scroll by as
//! much, so what is on screen stays put. A short note has all its blocks
//! from its first frame ([`EditorView::live_first_pass`]).
//!
//! Assistive technology gets each block as the Reading view describes it
//! (tables by rows and cells, code, math, images by their alt text,
//! callouts), among the editor's text runs ([`EditorView::blocks_a11y`]).
//!
//! # Reveal
//!
//! A block shows its source while a selection touches its lines (ends
//! included). Blocks live in the display map's block layer as
//! [`BlockPlacement::Replace`] blocks revealed on selection
//! ([`Reveal::OnSelection`]): Up and Down enter a block at its nearest
//! line, and a caret that lands in its rows any other way removes it. An
//! image on its own line is the exception: while its line is touched, the
//! source line shows and the image stays rendered in a block below it.
//!
//! A press on a block puts the caret at the source under the pointer (or
//! the block's start), which reveals it; presses on the view's own
//! controls (a callout's fold toggle, a diagram's zoom, a code block's
//! buttons) stay the view's. Links in a block are the source's links:
//! Mod+click follows one, hovering shows its tooltip or preview, a
//! right-click its menu. A sideways wheel turn (or Shift) over a table or
//! code block too wide for the column scrolls it; other turns scroll the
//! editor.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::sync::Arc;

use gpui::{AppContext as _, Bounds, Context, Entity, Pixels, Point};
use gv_editor::display_map::{
    BlockId, BlockItem, BlockPlacement, BlockProperties, BlockSnapshot, DisplayItem, Height,
    Reveal, WhenDeleted, Y,
};
use gv_syntax::inline::InlineKind;
use gv_syntax::{Container, ContainerKind, Leaf, LeafKind, SyntaxTree, Visitor, walk, walk_range};
use gv_text::{Anchor, Revision, TextSnapshot, TextSource as _};

use super::EditorView;
use super::live_context::{BlockContext, NoteContext, SYNC_CONTEXT_BYTES};
use crate::reader::Reader;

/// Block readers kept at most (the least recently drawn go first).
const READER_CAP: usize = 64;

/// Block heights applied in one frame at most.
const RESIZE_SLICE: usize = 256;

/// A height change smaller than this (px) is not applied.
const HEIGHT_EPSILON: f32 = 0.5;

/// What a block widget shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BlockWidget {
    /// Fenced or indented code.
    Code,
    /// Fenced code in a diagram language (Mermaid and the like).
    Diagram,
    /// A GFM table.
    Table,
    /// Display math.
    Math,
    /// An embed of a note, a heading, a canvas or a base.
    Embed,
    /// A callout, title and body.
    Callout,
    /// A thematic break.
    Rule,
    /// Front matter, as properties.
    Properties,
    /// An image on its own line.
    Image,
}

impl BlockWidget {
    /// A name for tests.
    #[cfg(test)]
    pub fn name(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Diagram => "diagram",
            Self::Table => "table",
            Self::Math => "math",
            Self::Embed => "embed",
            Self::Callout => "callout",
            Self::Rule => "rule",
            Self::Properties => "properties",
            Self::Image => "image",
        }
    }
}

/// A block found in the syntax tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockSpec {
    pub kind: BlockWidget,
    /// From the start of its first line to the end of its last line's
    /// content (no line break).
    pub range: Range<usize>,
    /// Its kind, source text and context, hashed: a block whose key
    /// changed is drawn anew.
    pub key: u64,
    /// The note's context it is rendered with ([`super::live_context`]).
    pub context: Option<Arc<BlockContext>>,
}

/// Fence languages drawn as diagrams by the Reading view.
const DIAGRAM_LANGUAGES: [&str; 3] = ["mermaid", "mmd", "diagram"];

/// Image file extensions an embed shows as an image.
const IMAGE_EXTENSIONS: [&str; 10] = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "tif", "tiff",
];

fn is_image_path(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| {
        IMAGE_EXTENSIONS
            .iter()
            .any(|e| e.eq_ignore_ascii_case(ext.trim()))
    })
}

/// Collects the top-level blocks a walk visits.
struct Finder<'a> {
    tree: &'a SyntaxTree,
    text: &'a TextSnapshot,
    context: Option<&'a NoteContext>,
    depth: usize,
    out: Vec<BlockSpec>,
    buf: Vec<u8>,
}

impl Finder<'_> {
    /// The range of lines `first..=last`, without the blank lines at its
    /// end and the last line break.
    fn lines(&self, first: usize, last: usize) -> Option<Range<usize>> {
        let start = self.text.line_start(first).ok()?;
        let mut last = last;
        let mut bytes = Vec::new();
        while last > first {
            let line = self.text.line_range(last).ok()?;
            bytes.clear();
            self.text.copy_range(line, &mut bytes);
            if !bytes.iter().all(u8::is_ascii_whitespace) {
                break;
            }
            last -= 1;
        }
        let end = self.text.line_range(last).ok()?.end;
        (end > start).then_some(start..end)
    }

    fn push(&mut self, kind: BlockWidget, range: Range<usize>) {
        let mut hasher = DefaultHasher::new();
        kind.hash(&mut hasher);
        self.buf.clear();
        self.text.copy_range(range.clone(), &mut self.buf);
        self.buf.hash(&mut hasher);
        let context = self.context.and_then(|note| {
            note.for_block(kind, &range, &String::from_utf8_lossy(&self.buf))
                .map(Arc::new)
        });
        context.hash(&mut hasher);
        self.out.push(BlockSpec {
            kind,
            range,
            key: hasher.finish(),
            context,
        });
    }

    /// A paragraph that is one image or embed: its kind.
    fn lone_embed(&self, leaf: &Leaf<'_>) -> Option<BlockWidget> {
        if leaf.start_line != leaf.end_line {
            return None;
        }
        let line = leaf.lines.first()?;
        let mut head = Vec::new();
        self.text.copy_range(
            line.content.start..(line.content.start + 2).min(line.content.end),
            &mut head,
        );
        if head != b"![" {
            return None;
        }
        let inline = self.tree.inlines(leaf, self.text);
        let mut nodes = inline
            .top_level()
            .filter(|n| !(n.kind() == InlineKind::Text && n.text().trim().is_empty()));
        let node = nodes.next()?;
        if nodes.next().is_some() {
            return None;
        }
        match node.kind() {
            InlineKind::Image => Some(BlockWidget::Image),
            InlineKind::Embed => {
                let wiki = node.wikilink()?;
                Some(if is_image_path(wiki.path()) {
                    BlockWidget::Image
                } else {
                    BlockWidget::Embed
                })
            }
            _ => None,
        }
    }
}

impl Visitor for Finder<'_> {
    fn enter(&mut self, _: &Container) {
        self.depth += 1;
    }

    fn exit(&mut self, container: &Container) {
        self.depth = self.depth.saturating_sub(1);
        if self.depth == 0 && matches!(container.kind, ContainerKind::Callout { .. }) {
            let last = self
                .text
                .line_of_offset(
                    container
                        .range
                        .end
                        .saturating_sub(1)
                        .max(container.range.start),
                )
                .unwrap_or(container.line);
            if let Some(range) = self.lines(container.line, last) {
                self.push(BlockWidget::Callout, range);
            }
        }
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        if self.depth > 0 {
            return;
        }
        let kind = match leaf.kind {
            LeafKind::FencedCode => {
                let mut info = Vec::new();
                if let Some(range) = leaf.info.clone() {
                    self.text.copy_range(range, &mut info);
                }
                let info = String::from_utf8_lossy(&info);
                let language = info.split_whitespace().next().unwrap_or_default();
                if DIAGRAM_LANGUAGES
                    .iter()
                    .any(|l| l.eq_ignore_ascii_case(language))
                {
                    BlockWidget::Diagram
                } else {
                    BlockWidget::Code
                }
            }
            LeafKind::IndentedCode => BlockWidget::Code,
            LeafKind::Table => BlockWidget::Table,
            LeafKind::MathBlock => BlockWidget::Math,
            LeafKind::ThematicBreak => BlockWidget::Rule,
            LeafKind::FrontMatter { .. } => BlockWidget::Properties,
            LeafKind::Paragraph => match self.lone_embed(leaf) {
                Some(kind) => kind,
                None => return,
            },
            _ => return,
        };
        // The lines of the whole block: a code block's or front matter's
        // lines leave out their fences and delimiters.
        let (first, last) = match leaf.kind {
            LeafKind::Paragraph => (leaf.start_line, leaf.end_line),
            _ => (
                self.text.line_of_offset(leaf.range.start).unwrap_or(0),
                self.text
                    .line_of_offset(leaf.range.end.saturating_sub(1).max(leaf.range.start))
                    .unwrap_or(0),
            ),
        };
        if let Some(range) = self.lines(first, last) {
            self.push(kind, range);
        }
    }
}

/// The top-level blocks of `range` (the whole text with `None`), in
/// document order, with the note's `context` when it is known.
pub(crate) fn find_blocks(
    tree: &SyntaxTree,
    text: &TextSnapshot,
    range: Option<Range<usize>>,
    context: Option<&NoteContext>,
) -> Vec<BlockSpec> {
    let mut finder = Finder {
        tree,
        text,
        context,
        depth: 0,
        out: Vec::new(),
        buf: Vec::new(),
    };
    match range {
        Some(range) => walk_range(tree, range, &mut finder),
        None => walk(tree, &mut finder),
    }
    let mut out = finder.out;
    out.sort_by_key(|b| (b.range.start, b.range.end));
    out.dedup_by(|b, a| b.range.start < a.range.end);
    out
}

/// Whether a selection in `selections` (sorted by start) touches `range`,
/// ends included.
fn touched(selections: &[Range<usize>], range: &Range<usize>) -> bool {
    let index = selections.partition_point(|s| s.end < range.start);
    selections.get(index).is_some_and(|s| s.start <= range.end)
}

// ----- geometry --------------------------------------------------------------------

/// Where display rows are in the view: `line` px each, or through the
/// block layer while blocks are shown.
#[derive(Clone)]
pub(crate) struct Geo {
    blocks: Option<BlockSnapshot>,
    line: f64,
}

impl Geo {
    /// Geometry without blocks.
    pub fn rows(line: f32) -> Self {
        Self {
            blocks: None,
            line: f64::from(line),
        }
    }

    /// The top of display row `row` below the top padding.
    pub fn row_y(&self, row: usize) -> f64 {
        match &self.blocks {
            Some(blocks) => blocks.row_top(row as u32).to_px(),
            None => row as f64 * self.line,
        }
    }

    /// The height of `rows` display rows and the blocks among them.
    pub fn total(&self, rows: usize) -> f64 {
        match &self.blocks {
            Some(blocks) => blocks.total_height().to_px(),
            None => rows as f64 * self.line,
        }
    }

    /// The display row at `y` below the top padding (a block's first row
    /// for a block); `rows` past the end.
    pub fn row_at(&self, y: f64, rows: usize) -> usize {
        let y = y.max(0.0);
        match &self.blocks {
            Some(blocks) => match blocks.item_at(Y::from_px(y)) {
                Some(DisplayItem::Row { row, .. }) => row as usize,
                Some(DisplayItem::Block(item)) => item.rows.start as usize,
                None => rows,
            },
            None => (y / self.line).floor() as usize,
        }
    }

    /// The end of the display rows to lay out for a view whose bottom is
    /// at `y` below the top padding: past the row there, and one more.
    pub fn rows_until(&self, y: f64, rows: usize) -> usize {
        match &self.blocks {
            Some(_) => self.row_at(y, rows).saturating_add(2),
            None => (y / self.line).ceil().max(0.0) as usize + 1,
        }
    }

    /// The display row at `y` as a fraction: into a block, in proportion
    /// to its rows.
    pub fn fractional_row(&self, y: f64) -> f64 {
        let y = y.max(0.0);
        match &self.blocks {
            Some(blocks) => match blocks.item_at(Y::from_px(y)) {
                Some(DisplayItem::Row { row, top }) => {
                    f64::from(row) + (y - top.to_px()) / self.line
                }
                Some(DisplayItem::Block(item)) => {
                    let height = f64::from(item.height.to_px()).max(1.0);
                    let rows = f64::from(item.rows.end - item.rows.start);
                    f64::from(item.rows.start) + (y - item.top.to_px()) / height * rows
                }
                None => f64::from(blocks.row_count()),
            },
            None => y / self.line,
        }
    }

    /// The y of fractional row `row` ([`Self::fractional_row`]'s inverse).
    pub fn y_of_fractional_row(&self, row: f64) -> f64 {
        let row = row.max(0.0);
        match &self.blocks {
            Some(blocks) => {
                let whole = row.floor() as u32;
                match blocks.replacement_at(whole) {
                    Some(item) => {
                        let rows = f64::from(item.rows.end - item.rows.start).max(1.0);
                        let into = (row - f64::from(item.rows.start)) / rows;
                        item.top.to_px() + into * f64::from(item.height.to_px())
                    }
                    None => blocks.row_top(whole).to_px() + (row - f64::from(whole)) * self.line,
                }
            }
            None => row * self.line,
        }
    }

    /// Whether display row `row` is hidden by a block.
    pub fn hidden(&self, row: usize) -> bool {
        self.blocks
            .as_ref()
            .is_some_and(|b| b.replacement_at(row as u32).is_some())
    }

    /// The blocks with a part in `y0..y1` (below the top padding).
    pub fn blocks_in(&self, y0: f64, y1: f64) -> Vec<BlockItem> {
        let Some(blocks) = &self.blocks else {
            return Vec::new();
        };
        blocks
            .items(Y::from_px(y0.max(0.0))..Y::from_px(y1.max(0.0)))
            .filter_map(|item| match item {
                DisplayItem::Block(block) if block.height.0 > 0 => Some(block),
                _ => None,
            })
            .collect()
    }
}

// ----- state -------------------------------------------------------------------------

/// Measured heights of one kind of block against their source lines: a
/// least-squares line, to estimate the height of a block not drawn yet
/// from its line count (the content height then changes less when it
/// comes on screen).
#[derive(Debug, Default, Clone, Copy)]
struct Fit {
    n: f64,
    x: f64,
    y: f64,
    xx: f64,
    xy: f64,
}

impl Fit {
    fn add(&mut self, lines: usize, height: f32) {
        let (x, y) = (lines as f64, f64::from(height));
        self.n += 1.0;
        self.x += x;
        self.y += y;
        self.xx += x * x;
        self.xy += x * y;
    }

    /// The estimated height of a block of `lines` lines; `None` before a
    /// measurement.
    fn estimate(&self, lines: usize) -> Option<f32> {
        if self.n < 1.0 {
            return None;
        }
        let (mx, my) = (self.x / self.n, self.y / self.n);
        let var = self.xx / self.n - mx * mx;
        let lines = lines as f64;
        let h = if var < 1e-6 {
            // All measured blocks had the same line count: in proportion.
            my * lines / mx.max(1.0)
        } else {
            let b = (self.xy / self.n - mx * my) / var;
            my + b * (lines - mx)
        };
        Some(h.max(1.0) as f32)
    }
}

/// A block in the editor's block layer.
#[derive(Debug, Clone)]
struct Shown {
    kind: BlockWidget,
    key: u64,
    context: Option<Arc<BlockContext>>,
    /// Its source lines.
    lines: usize,
    /// Below its line (a touched image) rather than instead of it.
    below: bool,
}

/// A block view: the reader and the frame it was last drawn in.
struct Hosted {
    reader: Entity<Reader>,
    drawn: u64,
}

/// What the last block refresh worked from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlocksKey {
    revision: Revision,
    generation: u64,
    selections: u64,
    span: Range<usize>,
    full: bool,
}

/// The block widgets of a view.
#[derive(Default)]
pub(crate) struct LiveBlocks {
    shown: HashMap<BlockId, Shown>,
    readers: HashMap<u64, Hosted>,
    /// Measured content heights: by key, with the width they were laid
    /// out at.
    measured: HashMap<u64, (f32, f32)>,
    /// Heights measured in the last frame, applied in the next.
    landed: Vec<(BlockId, f32)>,
    /// The whole document's blocks, for the text revision.
    pub(super) full: Option<(Revision, Arc<Vec<BlockSpec>>)>,
    /// The note's context, for the text revision it was worked out for
    /// (an older one is used until the background pass gives the new one).
    pub(super) context: Option<(Revision, Arc<NoteContext>)>,
    /// The path index the block views were given: (generation, complete).
    index_key: Option<(u64, bool)>,
    /// Measured heights by kind, to estimate the blocks not measured yet.
    fits: HashMap<BlockWidget, Fit>,
    last: Option<BlocksKey>,
    frame: u64,
    /// The text column's width blocks are laid out at.
    width: f32,
    /// Ranges of images on their own line: their inline widgets are not
    /// used.
    pub(super) lone_images: Vec<Range<usize>>,
}

impl std::fmt::Debug for LiveBlocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveBlocks")
            .field("shown", &self.shown.len())
            .field("readers", &self.readers.len())
            .finish_non_exhaustive()
    }
}

/// A block to draw this frame.
pub(crate) struct DrawnBlock {
    pub id: BlockId,
    #[cfg_attr(not(test), allow(dead_code))]
    pub kind: BlockWidget,
    pub bounds: Bounds<Pixels>,
    pub reader: Entity<Reader>,
}

impl LiveBlocks {
    /// Works the blocks out again at the next frame.
    pub fn invalidate(&mut self) {
        self.full = None;
        self.last = None;
    }

    /// How much text block `id`'s view has before the block's source.
    pub fn prefix_len(&self, id: BlockId) -> usize {
        self.shown
            .get(&id)
            .and_then(|s| s.context.as_ref())
            .map_or(0, |c| c.prefix.len())
    }

    /// Drops the block views (they read their source anew).
    pub fn forget_views(&mut self) {
        self.readers.clear();
    }

    /// Forgets everything but the measured heights (another document, Live
    /// Preview off). The caller removes the blocks from the editor.
    pub fn reset(&mut self) -> Vec<BlockId> {
        self.full = None;
        self.context = None;
        self.last = None;
        self.landed.clear();
        self.lone_images.clear();
        self.shown.drain().map(|(id, _)| id).collect()
    }
}

impl EditorView {
    /// The geometry of display rows for the current frame.
    pub(crate) fn geo(&self) -> Geo {
        let line = self.metrics.line;
        match self.doc.as_ref() {
            Some(doc) if doc.editor.blocks().block_count() > 0 => Geo {
                blocks: Some(doc.editor.blocks().clone()),
                line: f64::from(line),
            },
            _ => Geo::rows(line),
        }
    }

    /// Removes every block widget (Live Preview off, another document).
    pub(super) fn clear_live_blocks(&mut self) {
        let ids = self.live.blocks.reset();
        if let Some(doc) = self.doc.as_mut()
            && !ids.is_empty()
        {
            doc.editor.remove_blocks(ids);
        }
    }

    /// Brings the block widgets of `window` (byte range of the decorated
    /// lines; the whole document once its blocks are known) up to date:
    /// see the module docs. Blocks are laid out `width` px wide with rows
    /// `line` px tall until measured.
    pub(super) fn refresh_live_blocks(
        &mut self,
        window: Range<usize>,
        width: f32,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.is_live_preview_active() {
            return;
        }
        self.live.blocks.frame += 1;
        let line = self.metrics.line;
        if (self.live.blocks.width - width).abs() > HEIGHT_EPSILON {
            self.live.blocks.width = width;
            // Laid out anew at the new width: their heights land again.
            self.live.blocks.last = None;
            self.live.blocks.fits.clear();
        }
        self.apply_block_heights(cx);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let text = doc.editor.snapshot();
        let revision = text.revision();
        let (tree, parsed) = doc.syntax.shown();
        if parsed.revision() != revision || tree.is_interim() {
            return;
        }
        let tree = tree.clone();
        let selections: Vec<Range<usize>> =
            doc.editor.selections().iter().map(|s| s.range()).collect();
        let mut hasher = DefaultHasher::new();
        selections.hash(&mut hasher);
        let blocks = &mut self.live.blocks;
        let full = blocks
            .full
            .as_ref()
            .filter(|(r, _)| *r == revision)
            .map(|(_, specs)| Arc::clone(specs));
        let key = BlocksKey {
            revision,
            generation: doc.syntax.generation(),
            selections: hasher.finish(),
            span: if full.is_some() {
                0..text.len()
            } else {
                window.clone()
            },
            full: full.is_some(),
        };
        // Blocks the editor removed (revealed, their text deleted).
        blocks.shown.retain(|id, _| doc.editor.block(*id).is_some());
        if blocks.last.as_ref() == Some(&key) {
            return;
        }
        let (specs, span) = match &full {
            Some(specs) => (specs.as_ref().clone(), 0..text.len()),
            None => {
                // The note's context: worked out here for a short note,
                // else the last one until the background pass lands.
                let stale = blocks.context.as_ref().is_none_or(|(r, _)| *r != revision);
                if stale && text.len() <= SYNC_CONTEXT_BYTES {
                    blocks.context =
                        Some((revision, Arc::new(NoteContext::compute(&tree, &text, true))));
                }
                let context = blocks.context.as_ref().map(|(_, c)| Arc::clone(c));
                let specs = find_blocks(&tree, &text, Some(window.clone()), context.as_deref());
                let start = specs
                    .first()
                    .map_or(window.start, |s| s.range.start.min(window.start));
                let end = specs
                    .last()
                    .map_or(window.end, |s| s.range.end.max(window.end));
                (specs, start..end)
            }
        };
        blocks.lone_images = specs
            .iter()
            .filter(|s| s.kind == BlockWidget::Image)
            .map(|s| s.range.clone())
            .collect();

        // What should be there: (start, end, key, below).
        let mut desired: Vec<(BlockSpec, bool)> = Vec::new();
        for spec in specs {
            if touched(&selections, &spec.range) {
                if spec.kind == BlockWidget::Image {
                    desired.push((spec, true));
                }
            } else {
                desired.push((spec, false));
            }
        }
        let place = |id: BlockId| -> Option<(usize, usize)> {
            match &doc.editor.block(id)?.placement {
                BlockPlacement::Replace(r) => Some((r.start.offset, r.end.offset)),
                BlockPlacement::Below(a) | BlockPlacement::Above(a) => Some((a.offset, a.offset)),
            }
        };
        let wanted: HashSet<(usize, usize, u64, bool)> = desired
            .iter()
            .map(|(s, below)| {
                if *below {
                    (s.range.end, s.range.end, s.key, true)
                } else {
                    (s.range.start, s.range.end, s.key, false)
                }
            })
            .collect();
        let mut have: HashSet<(usize, usize, u64, bool)> = HashSet::new();
        let mut remove = Vec::new();
        for (id, shown) in &blocks.shown {
            let Some((start, end)) = place(*id) else {
                remove.push(*id);
                continue;
            };
            let inside = span.start <= start && end <= span.end;
            let item = (start, end, shown.key, shown.below);
            if wanted.contains(&item) && have.insert(item) {
                continue;
            }
            if inside || !wanted.contains(&item) && full.is_some() {
                remove.push(*id);
            }
        }
        let mut inserts = Vec::new();
        let mut infos = Vec::new();
        for (spec, below) in desired {
            let item = if below {
                (spec.range.end, spec.range.end, spec.key, true)
            } else {
                (spec.range.start, spec.range.end, spec.key, false)
            };
            if have.contains(&item) {
                continue;
            }
            let lines = if below {
                1
            } else {
                let first = text.line_of_offset(spec.range.start).unwrap_or(0);
                let last = text.line_of_offset(spec.range.end).unwrap_or(first);
                last - first + 1
            };
            let height = match blocks.measured.get(&spec.key) {
                Some(&(w, h)) if (w - width).abs() <= HEIGHT_EPSILON => h,
                _ => blocks
                    .fits
                    .get(&spec.kind)
                    .and_then(|f| f.estimate(lines))
                    .unwrap_or(lines as f32 * line),
            };
            let placement = if below {
                BlockPlacement::Below(Anchor::before(spec.range.end))
            } else {
                BlockPlacement::Replace(
                    Anchor::after(spec.range.start)..Anchor::before(spec.range.end),
                )
            };
            let reveal = if below {
                Reveal::Never
            } else {
                Reveal::OnSelection
            };
            inserts.push(
                BlockProperties::new(placement, Height::from_px(height))
                    .reveal(reveal)
                    .when_deleted(WhenDeleted::Remove),
            );
            infos.push(Shown {
                kind: spec.kind,
                key: spec.key,
                context: spec.context.clone(),
                lines,
                below,
            });
        }
        if !remove.is_empty() {
            for id in &remove {
                blocks.shown.remove(id);
            }
            doc.editor.remove_blocks(remove);
        }
        if !inserts.is_empty() {
            let ids = doc.editor.insert_blocks(inserts);
            for (id, info) in ids.into_iter().zip(infos) {
                blocks.shown.insert(id, info);
            }
            // An insert may have revealed one at once (a caret on its
            // rows): those are gone again.
            blocks.shown.retain(|id, _| doc.editor.block(*id).is_some());
        }
        blocks.last = Some(key);
        cx.notify();
    }

    /// The blocks to draw in `y0..y1` (below the top padding), with their
    /// views (made on first use), at `x` in a column `width` wide; `top`
    /// is the window y of row y 0.
    pub(super) fn blocks_to_draw(
        &mut self,
        geo: &Geo,
        (y0, y1): (f64, f64),
        (x, width): (f32, f32),
        top: f64,
        cx: &mut Context<'_, Self>,
    ) -> Vec<DrawnBlock> {
        let items = geo.blocks_in(y0, y1);
        if items.is_empty() {
            return Vec::new();
        }
        let path = self
            .path
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("Untitled.md"));
        let index = self.autocomplete.index.clone();
        let profile = self.live.profile.clone();
        // A new path index: the views resolve their links and embeds again.
        let index_key = index.as_ref().map(|i| (i.generation(), i.is_complete()));
        if index_key != self.live.blocks.index_key {
            self.live.blocks.index_key = index_key;
            for hosted in self.live.blocks.readers.values() {
                let index = index.clone();
                hosted
                    .reader
                    .update(cx, |reader, cx| reader.set_index(index, cx));
            }
        }
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let text = doc.editor.text().clone();
        let frame = self.live.blocks.frame;
        let mut out = Vec::new();
        for item in items {
            let Some(shown) = self.live.blocks.shown.get(&item.id).cloned() else {
                continue;
            };
            let reader = match self.live.blocks.readers.get_mut(&shown.key) {
                Some(hosted) => {
                    hosted.drawn = frame;
                    hosted.reader.clone()
                }
                None => {
                    let Some(block) = doc.editor.block(item.id) else {
                        continue;
                    };
                    let range = match &block.placement {
                        BlockPlacement::Replace(r) => r.start.offset..r.end.offset,
                        BlockPlacement::Below(a) | BlockPlacement::Above(a) => {
                            let line = text.line_of_offset(a.offset).unwrap_or(0);
                            text.line_range(line).unwrap_or(a.offset..a.offset)
                        }
                    };
                    let source = text.text_for_range(range).unwrap_or_default();
                    let (path, index, profile) = (path.clone(), index.clone(), profile.clone());
                    let context = shown.context.clone();
                    let reader = cx.new(|cx| {
                        let mut reader = Reader::for_block(cx);
                        if let Some(profile) = profile {
                            reader.set_profile(profile, cx);
                        }
                        if index.is_some() {
                            reader.set_index(index, cx);
                        }
                        match context {
                            // With the note's context around it.
                            Some(context) => {
                                let (text, crop) = context.text(&source);
                                reader.open_block(path, text.into(), crop, cx);
                            }
                            None => reader.open_text(path, Arc::from(&*source), None, cx),
                        }
                        reader
                    });
                    self.live.blocks.readers.insert(
                        shown.key,
                        Hosted {
                            reader: reader.clone(),
                            drawn: frame,
                        },
                    );
                    reader
                }
            };
            let bounds = Bounds::new(
                gpui::point(gpui::px(x), gpui::px((top + item.top.to_px()) as f32)),
                gpui::size(gpui::px(width), gpui::px(item.height.to_px())),
            );
            out.push(DrawnBlock {
                id: item.id,
                kind: shown.kind,
                bounds,
                reader,
            });
        }
        // Views not drawn for a while go, the least recent first.
        let readers = &mut self.live.blocks.readers;
        if readers.len() > READER_CAP {
            let mut by_age: Vec<(u64, u64)> = readers.iter().map(|(k, h)| (h.drawn, *k)).collect();
            by_age.sort_unstable();
            for (_, key) in by_age.into_iter().take(readers.len() - READER_CAP) {
                readers.remove(&key);
            }
        }
        out
    }

    /// The content heights the block views measured this frame: applied
    /// in the next one.
    pub(crate) fn block_heights(
        &mut self,
        heights: Vec<(BlockId, f32)>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let width = self.live.blocks.width;
        let blocks = &mut self.live.blocks;
        let mut changed = false;
        let mut better = Vec::new();
        let mut keys = HashSet::new();
        for (id, height) in heights {
            let height = height.max(1.0);
            let Some(shown) = blocks.shown.get(&id) else {
                continue;
            };
            let old = blocks.measured.insert(shown.key, (width, height));
            if old.is_none_or(|(w, h)| {
                (w - width).abs() > HEIGHT_EPSILON || (h - height).abs() > HEIGHT_EPSILON
            }) {
                // A new measurement: the estimates of its kind get better.
                // The unmeasured ones change at the 1st and the 4th only
                // (each change resizes every such block).
                keys.insert(shown.key);
                let fit = blocks.fits.entry(shown.kind).or_default();
                fit.add(shown.lines, height);
                if matches!(fit.n as u64, 1 | 4) {
                    better.push(shown.kind);
                }
            }
            let now = doc.editor.block(id).map(|b| b.height.to_px());
            if now.is_some_and(|now| (now - height).abs() > HEIGHT_EPSILON) {
                blocks.landed.push((id, height));
                changed = true;
            }
        }
        if !better.is_empty() || !keys.is_empty() {
            // Blocks not drawn yet: the height measured for the same
            // source, else the better estimate.
            for (id, shown) in &blocks.shown {
                if blocks.landed.iter().any(|(l, _)| l == id) {
                    continue;
                }
                let estimate = if keys.contains(&shown.key) {
                    blocks.measured.get(&shown.key).map(|&(_, h)| h)
                } else if better.contains(&shown.kind) && !blocks.measured.contains_key(&shown.key)
                {
                    blocks
                        .fits
                        .get(&shown.kind)
                        .and_then(|f| f.estimate(shown.lines))
                } else {
                    None
                };
                let Some(estimate) = estimate else {
                    continue;
                };
                let now = doc.editor.block(*id).map(|b| b.height.to_px());
                if now.is_some_and(|now| (now - estimate).abs() > HEIGHT_EPSILON) {
                    blocks.landed.push((*id, estimate));
                    changed = true;
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Resizes the blocks whose heights landed; a block above the top of
    /// the view moves the scroll by its change, so the view stays put.
    fn apply_block_heights(&mut self, cx: &mut Context<'_, Self>) {
        // At most a slice per frame (re-estimates can touch thousands of
        // blocks); measured heights come first.
        let take = self.live.blocks.landed.len().min(RESIZE_SLICE);
        let landed: Vec<(BlockId, f32)> = self.live.blocks.landed.drain(..take).collect();
        if landed.is_empty() {
            return;
        }
        if !self.live.blocks.landed.is_empty() {
            cx.notify();
        }
        let pad = f64::from(self.layout.pad_top());
        let top = self.scroll_y - pad;
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let mut shift = 0.0;
        for (id, height) in &landed {
            if let Some(item) = doc.editor.block_bounds(*id) {
                let old = f64::from(item.height.to_px());
                if item.top.to_px() + old <= top + 0.01 {
                    shift += f64::from(*height) - old;
                }
            }
        }
        let resized = doc
            .editor
            .resize_blocks(landed.into_iter().map(|(id, h)| (id, Height::from_px(h))));
        if resized > 0 {
            self.scroll_y = (self.scroll_y + shift).max(0.0);
            cx.notify();
        }
    }

    /// The block under window point `position`, with its view, as last
    /// drawn.
    pub(super) fn block_view_at(
        &self,
        position: Point<Pixels>,
    ) -> Option<(BlockId, Entity<Reader>)> {
        self.live
            .blocks_drawn
            .iter()
            .find(|(_, bounds, _)| bounds.contains(&position))
            .map(|(id, _, reader)| (*id, reader.clone()))
    }

    /// A wheel turn over a block: a table, code block or diagram there
    /// that overflows scrolls sideways when the turn is sideways (or Shift
    /// is held), as in the Reading view. Returns whether it did; other
    /// turns scroll the editor.
    pub(super) fn wheel_block(
        &mut self,
        e: &gpui::ScrollEvent,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some((_, reader)) = self.block_view_at(e.position) else {
            return false;
        };
        let d = e.delta.pixel_delta(gpui::px(self.metrics.line));
        reader.update(cx, |reader, cx| {
            reader.wheel_sideways(
                e.position,
                (f32::from(d.x), f32::from(d.y)),
                e.modifiers.shift,
                cx,
            )
        })
    }

    /// The blocks drawn in the last frame described for assistive
    /// technology, as the Reading view describes them (tables by rows and
    /// cells, code, math, diagrams, images by their alt text, callouts):
    /// each block's top (window y) and its nodes. Their keys are made
    /// unique per block; their links get no action (a screen reader reads
    /// them, the caret follows them from the source).
    pub(crate) fn blocks_a11y(
        &self,
        cx: &gpui::App,
    ) -> Vec<(f32, Vec<crate::a11y::document::DocNode>)> {
        fn rekey(nodes: &mut [crate::a11y::document::DocNode], salt: u64) {
            for node in nodes {
                node.key.0 ^= salt;
                node.link = None;
                rekey(&mut node.children, salt);
            }
        }
        self.live
            .blocks_drawn
            .iter()
            .map(|(id, bounds, reader)| {
                let mut nodes = reader.read(cx).a11y_nodes();
                let mut hasher = DefaultHasher::new();
                id.hash(&mut hasher);
                rekey(&mut nodes, hasher.finish());
                (f32::from(bounds.top()), nodes)
            })
            .collect()
    }

    /// Where block `id`'s view's source starts in the text: the block's
    /// start, or its line's for a block below its line.
    pub(super) fn block_source_start(&self, id: BlockId) -> Option<usize> {
        let doc = self.doc.as_ref()?;
        match &doc.editor.block(id)?.placement {
            BlockPlacement::Replace(r) => Some(r.start.offset),
            BlockPlacement::Below(a) | BlockPlacement::Above(a) => {
                let text = doc.editor.text();
                let line = text.line_of_offset(a.offset).ok()?;
                text.line_start(line).ok()
            }
        }
    }

    /// A press on a block: the caret goes to the source under the pointer
    /// (or the block's start), which shows the source. Returns `false` for
    /// a press the block's view takes (its controls).
    pub(super) fn press_block(
        &mut self,
        position: Point<Pixels>,
        cx: &mut Context<'_, Self>,
    ) -> Option<bool> {
        let (id, reader) = self.block_view_at(position)?;
        if reader.read(cx).takes_press_at(position) {
            self.live.refocus = true;
            return Some(false);
        }
        let doc = self.doc.as_mut()?;
        let block = doc.editor.block(id)?;
        let (start, end) = match &block.placement {
            BlockPlacement::Replace(r) => (r.start.offset, r.end.offset),
            BlockPlacement::Below(a) | BlockPlacement::Above(a) => (a.offset, a.offset),
        };
        let below = matches!(block.placement, BlockPlacement::Below(_));
        let prefix = self.live.blocks.prefix_len(id);
        let offset = if below {
            end
        } else {
            reader
                .read(cx)
                .source_offset_at(position)
                .map_or(start, |o| (start + o.saturating_sub(prefix)).min(end))
        };
        doc.editor.set_cursor(offset);
        self.autoscroll = true;
        cx.notify();
        Some(true)
    }

    /// The kinds of the blocks in the editor's block layer, in document
    /// order, and whether each is below its line (for tests).
    #[cfg(test)]
    pub(crate) fn live_blocks(&self) -> Vec<(&'static str, Range<usize>, bool)> {
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<_> = self
            .live
            .blocks
            .shown
            .iter()
            .filter_map(|(id, shown)| {
                let block = doc.editor.block(*id)?;
                let range = match &block.placement {
                    BlockPlacement::Replace(r) => r.start.offset..r.end.offset,
                    BlockPlacement::Below(a) | BlockPlacement::Above(a) => a.offset..a.offset,
                };
                Some((shown.kind.name(), range, shown.below))
            })
            .collect();
        out.sort_by_key(|(_, r, _)| (r.start, r.end));
        out
    }

    /// The ids of the blocks in the block layer, in document order (for
    /// tests).
    #[cfg(test)]
    pub(crate) fn live_block_ids(&self) -> Vec<BlockId> {
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<(usize, BlockId)> = self
            .live
            .blocks
            .shown
            .keys()
            .filter_map(|id| {
                let start = match &doc.editor.block(*id)?.placement {
                    BlockPlacement::Replace(r) => r.start.offset,
                    BlockPlacement::Below(a) | BlockPlacement::Above(a) => a.offset,
                };
                Some((start, *id))
            })
            .collect();
        out.sort_unstable();
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// The heights of the blocks in the block layer, in document order
    /// (for tests).
    #[cfg(test)]
    pub(crate) fn live_block_heights(&self) -> Vec<f32> {
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<(usize, f32)> = self
            .live
            .blocks
            .shown
            .keys()
            .filter_map(|id| {
                let block = doc.editor.block(*id)?;
                let start = match &block.placement {
                    BlockPlacement::Replace(r) => r.start.offset,
                    BlockPlacement::Below(a) | BlockPlacement::Above(a) => a.offset,
                };
                Some((start, block.height.to_px()))
            })
            .collect();
        out.sort_by_key(|(s, _)| *s);
        out.into_iter().map(|(_, h)| h).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_syntax::ParseOptions;

    fn specs(text: &str, options: ParseOptions) -> Vec<(&'static str, String)> {
        let snapshot = TextSnapshot::new(text);
        let tree = SyntaxTree::parse_with(&snapshot, options);
        find_blocks(&tree, &snapshot, None, None)
            .into_iter()
            .map(|s| (s.kind.name(), text[s.range].to_owned()))
            .collect()
    }

    fn obsidian() -> ParseOptions {
        ParseOptions::from_profile(&gv_profile::compile_builtin(
            gv_profile::ProfileId::Obsidian,
        ))
    }

    #[test]
    fn top_level_blocks_become_widgets() {
        let text = "---\ntitle: x\n---\n\n# H\n\n```rust\nfn a() {}\n```\n\n```mermaid\ngraph TD\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n$$\nx^2\n$$\n\n---\n\n![[note]]\n\n![[pic.png|200]]\n\n> [!note] Title\n> body\n\ntext ![[note]] here\n\n- ```\n  nested\n  ```\n";
        let found = specs(text, obsidian());
        let kinds: Vec<&str> = found.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            [
                "properties",
                "code",
                "diagram",
                "table",
                "math",
                "rule",
                "embed",
                "image",
                "callout"
            ]
        );
        assert_eq!(found[0].1, "---\ntitle: x\n---");
        assert_eq!(found[1].1, "```rust\nfn a() {}\n```");
        assert_eq!(found[8].1, "> [!note] Title\n> body");
    }

    #[test]
    fn touching_includes_the_ends() {
        let sel = [5..5, 20..25];
        assert!(touched(&sel, &(0..5)));
        assert!(touched(&sel, &(5..9)));
        assert!(!touched(&sel, &(6..19)));
        assert!(touched(&sel, &(25..30)));
    }
}

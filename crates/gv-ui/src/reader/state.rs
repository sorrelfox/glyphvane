// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the Reading view keeps between frames, shared by the view and its
//! element: the loaded document, the virtual list engine, the item layouts,
//! the selection (in source offsets) and where the last frame put things,
//! for hit testing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Bounds, Pixels, Point, WindowTextSystem};
use gv_render_tree::{FrameKind, InlineRole, ItemId};
use gv_syntax::BlockId;

use super::bench::Bench;
use super::engine::Engine;
use super::find::{FindMarks, SearchHit};
use super::items::{DocItems, ItemCache};
use super::live::SyncRequest;
use super::load::Loaded;
use crate::inline::{InlineBlock, InlineText};
use crate::metrics::size::{document, split};

/// Px laid out beyond each edge of the viewport, so that scrolling finds
/// measured items (the virtual list spike's value).
pub(crate) const OVERDRAW: f32 = 200.0;

/// Where the view puts its text column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReaderLayout {
    /// Preview: at most 680 wide (the readable line width), centred, 40 px
    /// side padding, 22 above.
    #[default]
    Column,
    /// The Split view's preview pane: padding `20 28 0`, the column fills
    /// the pane.
    Pane,
    /// A Live Preview block widget in the editor: no padding, the column
    /// fills the view, nothing grows beside it, no scrollbar.
    Block,
}

impl ReaderLayout {
    /// The px above the first item.
    pub(crate) fn lead(self) -> f32 {
        match self {
            Self::Column => document::PAD_TOP,
            Self::Pane => split::PREVIEW_PAD_TOP,
            Self::Block => 0.0,
        }
    }

    /// The window x of the text column and its width, in `bounds`.
    /// `column_max` is the widest Preview's column gets.
    pub(crate) fn column(self, bounds: Bounds<Pixels>, column_max: f32) -> (f32, f32) {
        let width = f32::from(bounds.size.width);
        let left = f32::from(bounds.left());
        match self {
            Self::Column => {
                let column = (width - 2.0 * document::PAD_X)
                    .min(column_max)
                    .max(1.0)
                    .floor();
                (left + ((width - column) / 2.0).max(0.0).floor(), column)
            }
            Self::Pane => {
                let column = (width - 2.0 * split::PREVIEW_PAD_X).floor().max(1.0);
                (left + split::PREVIEW_PAD_X, column)
            }
            Self::Block => (left, width.floor().max(1.0)),
        }
    }

    /// Where tables and diagrams too wide for the text column (`x0`,
    /// `column`, from [`Self::column`]) may grow in `bounds`: the window x
    /// of the area's left and right edges (the view without its side
    /// padding), and the widest such a block gets, `wide_max` capped by the
    /// area and never narrower than the column. The Split view's pane is
    /// all column: nothing grows there.
    pub(crate) fn wide(
        self,
        bounds: Bounds<Pixels>,
        (x0, column): (f32, f32),
        wide_max: f32,
    ) -> ((f32, f32), f32) {
        match self {
            Self::Column => {
                let left = f32::from(bounds.left()) + document::PAD_X;
                let area = (f32::from(bounds.size.width) - 2.0 * document::PAD_X).floor();
                let area = (left.min(x0), (left + area).max(x0 + column));
                let limit = wide_max.min(area.1 - area.0).max(column).floor();
                (area, limit)
            }
            Self::Pane | Self::Block => ((x0, x0 + column), column),
        }
    }
}

/// A selection in document source offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

/// A link under the pointer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LinkHit {
    pub item: usize,
    pub link: u32,
    pub destination: Arc<str>,
    pub kind: gv_model::LinkKind,
    pub image: bool,
    /// The whole construct in the document's text (`None` inside an
    /// embed, whose text is another note's).
    pub range: Option<std::ops::Range<usize>>,
    /// Inside a note embed: the embedded note's key (its vault path),
    /// which the link is followed from.
    pub from: Option<Arc<str>>,
}

/// A button in a code block's head, or a table's Expand button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonKind {
    Copy,
    Wrap,
    /// Shows the table in the expanded view.
    Expand,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CodeButton {
    pub bounds: Bounds<Pixels>,
    pub block: BlockId,
    /// Index of the code block's (or the table's) node.
    pub node: usize,
    pub kind: ButtonKind,
}

/// A control the last frame drew in the document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Control {
    pub bounds: Bounds<Pixels>,
    pub kind: ControlKind,
}

/// What a control does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlKind {
    /// The checkbox of task item `item` (the list item's block), whose
    /// first block starts at source offset `at`.
    Task { item: BlockId, at: usize },
    /// The summary row of the `<details>` of block `block`: folds it.
    Details { block: BlockId },
    /// *Render anyway* on the card of diagram block `block`.
    RenderAnyway { block: BlockId },
    /// *Copy source* on the card of the diagram of node `node`.
    CopySource { node: usize },
    /// The zoom buttons of the diagram of block `block`: in (`1`), out
    /// (`-1`) or back to its fitted size (`0`).
    DiagramZoom { block: BlockId, step: i8 },
    /// *Open externally* on an image card: the file is
    /// [`Geometry::external`]`[slot]`.
    OpenExternally { slot: usize },
    /// A live block's header or canvas (opens its file), or a query
    /// block's result row: [`Geometry::live`]`[slot]`.
    Live { slot: usize },
}

/// What a click on a live block opens: the block's node, and the result
/// row of a query block (`None`: the block's own file) with the context
/// line of that note (`None`: the note's title).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveTarget {
    pub node: usize,
    pub row: Option<usize>,
    pub line: Option<usize>,
}

/// A footnote reference or backreference under the pointer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FootnoteHit {
    /// A reference's number: jumps to the footnote.
    Ref { label: Arc<str> },
    /// A footnote's `↩` back to its reference `k` (0-based).
    Back { label: Arc<str>, k: u32 },
    /// A reference's number inside a note embed (item `item`, footnote
    /// `number` of the embed): jumps to the footnote in the list at the
    /// embed's end.
    EmbedRef { item: usize, number: u32 },
    /// The `↩` of an entry (item `item`) of the list of footnotes at a
    /// note embed's end, back to its reference `k` (0-based) in the embed.
    EmbedBack { item: usize, k: u32 },
}

/// What is under the pointer.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Hover {
    #[default]
    None,
    Text,
    Link(LinkHit),
    /// A local image (not inside a link), whose click opens it in the
    /// image viewer, or a diagram's link.
    Target(LinkHit),
    Button(CodeButton),
    Control(Control),
    Footnote(FootnoteHit),
    Scrollbar,
}

/// What a pressed mouse button is doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Press {
    #[default]
    None,
    /// Selecting text; a press on a link that does not move follows it.
    Select {
        origin: Point<Pixels>,
        moved: bool,
        link: Option<LinkHit>,
    },
    /// The middle button on a link.
    Middle(LinkHit),
    /// Dragging the scrollbar thumb, grabbed `grab` px below its top.
    Thumb {
        grab: f32,
    },
    /// Dragging the sideways thumb of block `block`, grabbed `grab` px
    /// right of its left end.
    HThumb {
        block: BlockId,
        grab: f32,
    },
    Button(CodeButton),
    /// A task checkbox or a summary row.
    Control(Control),
    /// A footnote reference or backreference.
    Footnote(FootnoteHit),
}

/// The code part of a placed item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CodeGeom {
    pub block: BlockId,
    /// Where its text shows.
    pub clip: Bounds<Pixels>,
    /// How far its text can scroll sideways.
    pub overflow: f32,
    /// How much of the text's width shows (the clip without the code's
    /// padding).
    pub view: f32,
    /// How far it is scrolled sideways.
    pub scroll: f32,
    /// Whether its left and right sides are faded (text out of sight
    /// there).
    pub fades: (bool, bool),
}

/// The sideways scrollbar of a table or code block that scrolls
/// sideways, as the last frame drew it: inside the block's bottom edge,
/// or the bottom of its visible part.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HBar {
    pub block: BlockId,
    /// The area that grabs the thumb: its row, across the block.
    pub zone: Bounds<Pixels>,
    /// Window x of the track's left end and its width.
    pub track: (f32, f32),
    pub thumb: Bounds<Pixels>,
    /// How far the block scrolls sideways.
    pub overflow: f32,
}

/// An item as the last frame placed it, in window coordinates.
#[derive(Debug, Clone)]
pub(crate) struct PlacedGeom {
    pub ix: usize,
    pub top: f32,
    pub bottom: f32,
    /// The window x and width of what it covers sideways: the text
    /// column, and the content box of a table or diagram that grew beside
    /// it.
    pub span: (f32, f32),
    pub text: Option<Rc<RefCell<InlineBlock>>>,
    pub node_start: usize,
    pub code: Option<CodeGeom>,
}

/// Where the last frame put things.
#[derive(Debug, Clone, Default)]
pub(crate) struct Geometry {
    pub bounds: Bounds<Pixels>,
    /// Window x of the text column and its width.
    pub column: (f32, f32),
    /// Window x of the left and right edges of the area tables and diagrams
    /// too wide for the column may grow into.
    pub area: (f32, f32),
    pub items: Vec<PlacedGeom>,
    pub buttons: Vec<CodeButton>,
    /// Task checkboxes, summary rows and card buttons.
    pub controls: Vec<Control>,
    /// The files the image cards' *Open externally* buttons open.
    pub external: Vec<std::path::PathBuf>,
    /// The area that grabs the scrollbar, and the thumb.
    pub track: Option<Bounds<Pixels>>,
    pub thumb: Option<Bounds<Pixels>>,
    /// The sideways scrollbars of the blocks that scroll sideways.
    pub hbars: Vec<HBar>,
    /// What the live blocks' controls open.
    pub live: Vec<LiveTarget>,
    /// Where hosted views (embedded bases) are: pointer input there is
    /// theirs.
    pub hosts: Vec<Bounds<Pixels>>,
    /// The front matter keys drawn, with the window y of their baselines.
    pub property_keys: Vec<(String, f32)>,
    /// The matched text marked in query blocks' context lines drawn.
    pub query_marks: Vec<String>,
}

/// Frame timings, for the log and the debug benchmark.
#[derive(Debug, Default)]
pub(crate) struct Timing {
    /// When the document was asked for.
    pub opened: Option<Instant>,
    /// From the request to the first frame with content.
    pub first_content: Option<Duration>,
    /// When the current frame's render started.
    pub frame_start: Option<Instant>,
    /// Frame CPU times (render to the end of paint), when recording.
    pub frames: Vec<Duration>,
    pub record: bool,
    /// A followed link, Back or Forward, logged when its target is painted.
    pub navigation: Option<super::follow::Navigation>,
}

pub(crate) struct State {
    pub doc: Option<Loaded>,
    /// Bumped per load, to drop stale background results.
    pub revision: u64,
    pub engine: Engine,
    pub cache: ItemCache,
    pub text: Rc<RefCell<InlineText>>,
    pub selection: Option<Selection>,
    /// Sideways scroll of code blocks without soft wrap and of tables.
    pub hscroll: HashMap<BlockId, f32>,
    pub hover: Hover,
    pub press: Press,
    pub geometry: Geometry,
    pub timing: Timing,
    pub bench: Option<Bench>,
    /// Items whose content changed since the last frame (link statuses).
    pub stale: Vec<ItemId>,
    /// Bumped per installed document: find results belong to one.
    pub doc_version: u64,
    /// Find in page: the matches to paint.
    pub find: FindMarks,
    /// The Search panel result the note was opened at.
    pub search_hit: Option<SearchHit>,
    pub layout: ReaderLayout,
    /// The source offset whose block gets the caret band (the Split view's
    /// preview: the editor's caret).
    pub band: Option<usize>,
    /// Scroll sync's request for the next frame.
    pub sync: Option<SyncRequest>,
    /// The widest the Preview text column gets (the readable line width;
    /// infinite when it is off).
    pub column_max: f32,
    /// The widest tables and diagrams too wide for the column get (the
    /// wide block setting; infinite when they fill the space beside it).
    pub wide_max: f32,
    /// The search a click on a tag asked for (`tag:#name`), handed to the
    /// vault search with the `search.vault` command.
    pub tag_search: Option<Arc<str>>,
    /// Tables not at their natural width get an Expand button while the
    /// pointer is over them (not in the expanded view itself).
    pub expandable: bool,
    /// The table or rendered diagram under the pointer (the block of
    /// its rows or its fence).
    pub hover_table: Option<BlockId>,
    /// Whether the document holds Git conflict markers, for the document
    /// version checked ([`super::Reader::conflict_markers`]).
    pub markers: std::cell::Cell<Option<(u64, bool)>>,
    /// Folds of a restored session waiting for their file to be shown
    /// ([`super::fold_state`]).
    pub pending_folds: Option<(std::path::PathBuf, Vec<super::fold_state::ReadingFold>)>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("doc", &self.doc.as_ref().map(|d| &d.path))
            .field("revision", &self.revision)
            .field("selection", &self.selection)
            .field("cached", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl State {
    pub fn new(text: InlineText) -> Self {
        Self {
            doc: None,
            revision: 0,
            engine: Engine::new(document::PAD_TOP, OVERDRAW),
            cache: ItemCache::default(),
            text: Rc::new(RefCell::new(text)),
            selection: None,
            hscroll: HashMap::new(),
            hover: Hover::None,
            press: Press::None,
            geometry: Geometry::default(),
            timing: Timing::default(),
            bench: None,
            stale: Vec::new(),
            doc_version: 0,
            find: FindMarks::default(),
            search_hit: None,
            layout: ReaderLayout::Column,
            band: None,
            sync: None,
            tag_search: None,
            column_max: document::COLUMN_MAX,
            wide_max: f32::INFINITY,
            expandable: true,
            hover_table: None,
            markers: std::cell::Cell::new(None),
            pending_folds: None,
        }
    }

    /// Puts the text column in `layout`.
    pub fn set_layout(&mut self, layout: ReaderLayout) {
        self.layout = layout;
        self.engine = Engine::new(layout.lead(), OVERDRAW);
    }

    /// Shows `doc`, scrolled to the top.
    pub fn install(&mut self, doc: Loaded) {
        // Live blocks keep their content across builds of the same file.
        if self.doc.as_ref().is_some_and(|d| d.path == doc.path) {
            self.cache.live.retain_tree(&doc.tree);
        } else {
            self.cache.live.clear();
            self.cache.reset_diagram_zoom();
        }
        if self
            .doc
            .as_ref()
            .is_none_or(|d| !Arc::ptr_eq(&d.images, &doc.images))
        {
            self.cache.images.forget();
        }
        self.cache.targets = Some(Arc::clone(&doc.images));
        self.doc = Some(doc);
        self.engine = Engine::new(self.layout.lead(), OVERDRAW);
        self.sync = None;
        self.cache.clear();
        self.cache.code.forget_blocks();
        self.cache.diagrams.forget_blocks();
        self.selection = None;
        self.hscroll.clear();
        self.hover = Hover::None;
        self.hover_table = None;
        self.press = Press::None;
        self.geometry = Geometry::default();
        self.stale.clear();
        self.doc_version += 1;
        // A Search panel result waiting for its note.
        if let Some(hit) = &mut self.search_hit
            && hit.version.is_none()
            && self.doc.as_ref().is_some_and(|d| d.path == hit.path)
        {
            hit.version = Some(self.doc_version);
        }
        self.apply_pending_folds();
    }

    /// The selection as a non-empty range.
    pub fn selected(&self) -> Option<Range<usize>> {
        self.selection.map(|s| s.range()).filter(|r| !r.is_empty())
    }

    /// The placed item at window y `y`, clamped to the first and last.
    fn placed_at(&self, y: f32) -> Option<&PlacedGeom> {
        let items = &self.geometry.items;
        let first = items.first()?;
        if y < first.top {
            return Some(first);
        }
        items
            .iter()
            .find(|g| g.top <= y && y < g.bottom)
            .or(items.last())
    }

    /// The document offset nearest to a window position, from the last
    /// frame's layouts.
    pub fn offset_at(&self, position: Point<Pixels>) -> Option<usize> {
        let g = self.placed_at(f32::from(position.y))?;
        if let Some(block) = &g.text
            && let Some(hit) = block.borrow().hit(position)
        {
            return Some(g.node_start + hit.source);
        }
        let item = self.doc.as_ref()?.tree.items().get(g.ix)?;
        let middle = (g.top + g.bottom) / 2.0;
        Some(if f32::from(position.y) < middle {
            item.source.start
        } else {
            item.source.end
        })
    }

    /// The link under a window position.
    pub fn link_at(&self, position: Point<Pixels>) -> Option<LinkHit> {
        let y = f32::from(position.y);
        let g = self
            .geometry
            .items
            .iter()
            .find(|g| g.top <= y && y < g.bottom)?;
        let block = g.text.as_ref()?;
        let piece = block.borrow().item_at(position)?;
        let doc = self.doc.as_ref()?;
        let content = doc.tree.item_inline(g.ix)?;
        let link = content.spans.get(piece)?.link?;
        self.link_hit(g.ix, link)
    }

    /// Link `link` of item `item`, as a click on it finds it (what
    /// assistive technology follows: [`crate::a11y::document`]).
    pub fn link_hit(&self, item: usize, link: u32) -> Option<LinkHit> {
        let doc = self.doc.as_ref()?;
        let content = doc.tree.item_inline(item)?;
        let info = content.links.get(link as usize)?;
        let start = doc.tree.node_of(item).map(|n| n.source.start);
        let range = start
            .filter(|_| info.from.is_none())
            .map(|s| s + info.source.start..s + info.source.end)
            .filter(|r| {
                // A link of an embedded note is not in this text.
                doc.text
                    .get(r.clone())
                    .is_some_and(|t| t.starts_with(['[', '!', '<']) || t.contains("://"))
            });
        Some(LinkHit {
            item,
            link,
            destination: info.destination.clone(),
            kind: info.kind,
            image: false,
            range,
            from: info.from.clone(),
        })
    }

    /// The local image file under a window position that a click opens
    /// in the image viewer (an image inside a link follows the link): its
    /// destination, as a link of the image's kind (`image` set; `link` is
    /// the image's index).
    pub fn image_at(&self, position: Point<Pixels>) -> Option<LinkHit> {
        let y = f32::from(position.y);
        let g = self
            .geometry
            .items
            .iter()
            .find(|g| g.top <= y && y < g.bottom)?;
        let block = g.text.as_ref()?;
        let piece = block.borrow().item_at(position)?;
        let doc = self.doc.as_ref()?;
        let content = doc.tree.item_inline(g.ix)?;
        let span = content.spans.get(piece)?;
        let (InlineRole::Image(i), None) = (span.role, span.link) else {
            return None;
        };
        let info = content.images.get(i as usize)?;
        let key = gv_render_tree::image_key(&info.destination, info.from.as_deref());
        let target = self.cache.targets.as_ref()?.get(&key)?;
        if !matches!(target, super::media::ImageTarget::Local { .. }) {
            return None;
        }
        let start = doc.tree.node_of(g.ix).map(|n| n.source.start);
        let range = start
            .map(|s| s + info.source.start..s + info.source.end)
            .filter(|r| doc.text.get(r.clone()).is_some_and(|t| t.starts_with('!')));
        Some(LinkHit {
            item: g.ix,
            link: i,
            destination: info.destination.clone(),
            kind: info.kind,
            image: true,
            range,
            from: None,
        })
    }

    /// The link of a diagram under a window position (`link` is its
    /// index in the diagram; the diagram's item has no other links).
    pub fn diagram_link_at(&self, position: Point<Pixels>) -> Option<LinkHit> {
        let doc = self.doc.as_ref()?;
        let y = f32::from(position.y);
        let g = self
            .geometry
            .items
            .iter()
            .find(|g| g.top <= y && y < g.bottom)?;
        if !matches!(
            doc.tree.items().get(g.ix)?.kind,
            gv_render_tree::ItemKind::Diagram { .. }
        ) || !g.code?.clip.contains(&position)
        {
            return None;
        }
        let block = g.text.as_ref()?.borrow();
        let (k, l) = diagram_link_in(&block, position)?;
        let (destination, kind) = match &l.target {
            gv_diagram::LinkTarget::Href(h) => (h.as_str(), gv_model::LinkKind::Inline),
            gv_diagram::LinkTarget::Note(n) => (n.as_str(), gv_model::LinkKind::Wikilink),
        };
        Some(LinkHit {
            item: g.ix,
            link: k,
            destination: Arc::from(destination),
            kind,
            image: false,
            range: None,
            from: None,
        })
    }

    /// The link under a window position, else the image a click opens or
    /// the diagram link.
    pub fn target_at(&self, position: Point<Pixels>) -> Option<LinkHit> {
        self.link_at(position)
            .or_else(|| self.image_at(position))
            .or_else(|| self.diagram_link_at(position))
    }

    pub fn button_at(&self, position: Point<Pixels>) -> Option<CodeButton> {
        self.geometry
            .buttons
            .iter()
            .find(|b| b.bounds.contains(&position))
            .copied()
    }

    /// The code block under a window position.
    pub fn code_at(&self, position: Point<Pixels>) -> Option<CodeGeom> {
        self.geometry
            .items
            .iter()
            .filter_map(|g| g.code)
            .find(|c| c.clip.contains(&position))
    }

    /// The control under a window position.
    pub fn control_at(&self, position: Point<Pixels>) -> Option<Control> {
        self.geometry
            .controls
            .iter()
            .find(|c| c.bounds.contains(&position))
            .copied()
    }

    /// The footnote reference or backreference under a window position.
    pub fn footnote_at(&self, position: Point<Pixels>) -> Option<FootnoteHit> {
        let y = f32::from(position.y);
        let g = self
            .geometry
            .items
            .iter()
            .find(|g| g.top <= y && y < g.bottom)?;
        let block = g.text.as_ref()?;
        let piece = block.borrow().item_at(position)?;
        let doc = self.doc.as_ref()?;
        let content = doc.tree.item_inline(g.ix)?;
        match content.spans.get(piece)?.role {
            InlineRole::FootnoteRef(i) => {
                let f = content.footnotes.get(i as usize)?;
                // A note embed's footnote: listed at the embed's end, not
                // in the document's footnote section.
                Some(if f.embedded {
                    FootnoteHit::EmbedRef {
                        item: g.ix,
                        number: f.number,
                    }
                } else {
                    FootnoteHit::Ref {
                        label: f.label.clone(),
                    }
                })
            }
            InlineRole::FootnoteBackref(k) => {
                let item = doc.tree.items().get(g.ix)?;
                let label = item.context.iter().find_map(|f| match &f.kind {
                    FrameKind::Footnote { label, .. } => Some(label.clone()),
                    _ => None,
                });
                Some(match label {
                    Some(label) => FootnoteHit::Back { label, k },
                    None => FootnoteHit::EmbedBack { item: g.ix, k },
                })
            }
            _ => None,
        }
    }

    /// Follows a footnote reference to its footnote, or a backreference
    /// to its reference. Returns whether the view moved.
    pub fn follow_footnote(&mut self, hit: &FootnoteHit) -> bool {
        let Some(doc) = self.doc.as_ref() else {
            return false;
        };
        let target = match hit {
            FootnoteHit::Ref { label } => doc.tree.footnote_item(label),
            FootnoteHit::Back { label, k } => doc
                .tree
                .footnote_refs(label)
                .get(*k as usize)
                .map(|(ix, _)| *ix),
            FootnoteHit::EmbedRef { item, number } => doc.tree.embed_footnote_item(*item, *number),
            FootnoteHit::EmbedBack { item, k } => doc
                .tree
                .embed_footnote_refs(*item)
                .get(*k as usize)
                .copied(),
        };
        let Some(ix) = target else {
            return false;
        };
        self.engine.jump_near_top(ix, 0.0);
        true
    }

    /// Opens or closes the `<details>` of summary block `block`.
    pub fn toggle_details(&mut self, block: BlockId, ts: &WindowTextSystem) {
        let State {
            doc,
            engine,
            cache,
            text,
            ..
        } = self;
        let Some(doc) = doc else {
            return;
        };
        let open = !cache.details_open(&doc.tree, block);
        cache.folds.insert(block, open);
        let changed: Vec<usize> = doc
            .tree
            .items()
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                it.id.block == block
                    || it
                        .context
                        .iter()
                        .any(|f| f.id == block && matches!(f.kind, FrameKind::Details))
            })
            .map(|(ix, _)| ix)
            .collect();
        let items = DocItems {
            tree: &doc.tree,
            cache,
            text,
            ts,
        };
        engine.invalidate(&items, changed);
    }

    /// The edit a click on a task checkbox asks for: the status character
    /// of list item `item` (whose first block starts at `at`) toggled
    /// between ` ` and `x` (any other status becomes ` `).
    pub fn task_toggle(&self, item: BlockId, at: usize) -> Option<super::TaskToggle> {
        let doc = self.doc.as_ref()?;
        struct Find {
            item: BlockId,
            marker: Option<Range<usize>>,
        }
        impl gv_syntax::Visitor for Find {
            fn enter(&mut self, c: &gv_syntax::Container) {
                if c.id == self.item && c.is_task() {
                    self.marker.clone_from(&c.marker);
                }
            }
        }
        let mut find = Find { item, marker: None };
        gv_syntax::walk_range(&doc.syntax, at..at + 1, &mut find);
        let marker = find.marker?;
        let offset = marker.start + 1;
        let from = doc.text.get(offset..)?.chars().next()?;
        let to = if from == ' ' { 'x' } else { ' ' };
        Some(super::TaskToggle { offset, from, to })
    }

    /// What is under the pointer at `position`.
    pub fn hover_at(&self, position: Point<Pixels>) -> Hover {
        if self.in_host(position) {
            return Hover::None;
        }
        if self.geometry.track.is_some_and(|t| t.contains(&position))
            || self.hbar_at(position).is_some()
        {
            return Hover::Scrollbar;
        }
        if let Some(b) = self.button_at(position) {
            return Hover::Button(b);
        }
        if let Some(c) = self.control_at(position) {
            return Hover::Control(c);
        }
        if let Some(f) = self.footnote_at(position) {
            return Hover::Footnote(f);
        }
        if let Some(link) = self.link_at(position) {
            return Hover::Link(link);
        }
        if let Some(target) = self
            .image_at(position)
            .or_else(|| self.diagram_link_at(position))
        {
            return Hover::Target(target);
        }
        let y = f32::from(position.y);
        let x = f32::from(position.x);
        let over_text = self.geometry.items.iter().any(|g| {
            let (x0, w) = g.span;
            g.text.is_some() && g.top <= y && y < g.bottom && x >= x0 && x < x0 + w
        });
        if over_text { Hover::Text } else { Hover::None }
    }

    /// Whether `position` is over a hosted view (an embedded base).
    pub fn in_host(&self, position: Point<Pixels>) -> bool {
        self.geometry.hosts.iter().any(|b| b.contains(&position))
    }

    /// The whole text of code block node `node`.
    pub fn code_text(&self, node: usize) -> Option<String> {
        let doc = self.doc.as_ref()?;
        Some(doc.tree.nodes().get(node)?.lines()?.text.clone())
    }

    /// *Render anyway*: the diagram of block `block` renders with the
    /// trusted caps.
    pub fn render_anyway(&mut self, block: BlockId, ts: &WindowTextSystem) {
        if self.cache.diagrams.trust(block) {
            self.changed(&[ItemId { block, chunk: 0 }], ts);
        }
    }

    /// Turns soft wrap of a code block on or off.
    pub fn toggle_wrap(&mut self, block: BlockId, ts: &WindowTextSystem) {
        self.hscroll.remove(&block);
        let State {
            doc,
            engine,
            cache,
            text,
            ..
        } = self;
        if let Some(doc) = doc {
            let wrap = !doc.tree.code_wrap(block);
            let range = Arc::make_mut(&mut doc.tree).set_code_wrap(block, wrap);
            let items = DocItems {
                tree: &doc.tree,
                cache,
                text,
                ts,
            };
            engine.invalidate(&items, range);
        }
    }

    /// The table whose drawn rows hold a window position, or the
    /// rendered diagram that does.
    pub fn table_at(&self, position: Point<Pixels>) -> Option<BlockId> {
        let doc = self.doc.as_ref()?;
        self.geometry.items.iter().find_map(|g| {
            let code = g.code?;
            let item = doc.tree.items().get(g.ix)?;
            let kind_ok = match item.kind {
                gv_render_tree::ItemKind::TableRow { .. } => true,
                gv_render_tree::ItemKind::Diagram { .. } => {
                    !self.cache.diagram_frames.contains_key(&item.id)
                }
                _ => false,
            };
            (kind_ok && code.clip.contains(&position)).then_some(code.block)
        })
    }

    /// Zooms the diagram of block `block` in (`step` 1) or out (-1) one
    /// step, or back to its fitted size (0).
    pub fn zoom_diagram(&mut self, block: BlockId, step: i8, ts: &WindowTextSystem) {
        let zoom = self.cache.diagram_zoom(block);
        let next = super::items::diagram_zoom_step(zoom, step);
        if (next - zoom).abs() < f32::EPSILON {
            return;
        }
        self.cache.set_diagram_zoom(block, next);
        self.changed(&[ItemId { block, chunk: 0 }], ts);
    }

    /// The sideways scrollbar whose zone holds a window position.
    pub fn hbar_at(&self, position: Point<Pixels>) -> Option<HBar> {
        self.geometry
            .hbars
            .iter()
            .find(|b| b.zone.contains(&position))
            .copied()
    }

    /// Drags the sideways thumb of block `block`, grabbed `grab` px right
    /// of its left end, to window x `x`: the block scrolls as far as the
    /// thumb's place along the track says.
    pub fn drag_hthumb(&mut self, block: BlockId, x: f32, grab: f32) {
        let Some(bar) = self.geometry.hbars.iter().find(|b| b.block == block) else {
            return;
        };
        let (track_x, track_w) = bar.track;
        let room = track_w - f32::from(bar.thumb.size.width);
        let at = if room > 0.0 {
            ((x - grab - track_x) / room).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.hscroll.insert(block, at * bar.overflow);
    }

    /// Scrolls the code block `code` sideways by `dx` px.
    pub fn scroll_code(&mut self, code: CodeGeom, dx: f32) {
        let x = self.hscroll.entry(code.block).or_default();
        *x = (*x + dx).clamp(0.0, code.overflow);
    }

    /// Items whose content changed (a refresh of link statuses): their
    /// layouts are dropped and their heights become estimates.
    pub fn changed(&mut self, ids: &[ItemId], ts: &WindowTextSystem) {
        self.cache.forget(ids.iter().copied());
        let State {
            doc,
            engine,
            cache,
            text,
            ..
        } = self;
        if let Some(doc) = doc {
            let ixs: Vec<usize> = ids.iter().filter_map(|id| doc.tree.index_of(*id)).collect();
            let items = DocItems {
                tree: &doc.tree,
                cache,
                text,
                ts,
            };
            engine.invalidate(&items, ixs);
        }
    }
}

/// The link of the diagram drawn in `block` (its one box) under a window
/// position, with its index: the last one drawn there.
pub(crate) fn diagram_link_in(
    block: &InlineBlock,
    position: Point<Pixels>,
) -> Option<(u32, gv_diagram::DiagramLink)> {
    let origin = block.bounds()?.origin;
    let layout = block.layout()?;
    let line = layout.lines().first()?;
    let f = layout.line_fragments(0).iter().find(|f| {
        layout
            .paragraph()
            .items()
            .get(f.item)
            .is_some_and(|i| i.inline_box().is_some())
    })?;
    let b = layout.paragraph().items().get(f.item)?.inline_box()?;
    let Some(crate::inline::BoxContent::Diagram { diagram, .. }) = block.box_content(b.id) else {
        return None;
    };
    if diagram.width <= 0.0 || f.width <= 0.0 {
        return None;
    }
    let scale = f.width / diagram.width;
    let x = (f32::from(position.x - origin.x) - f.x) / scale;
    let y = (f32::from(position.y - origin.y) - (line.baseline - f.ascent)) / scale;
    diagram
        .links
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| {
            x >= l.rect.x
                && x < l.rect.x + l.rect.width
                && y >= l.rect.y
                && y < l.rect.y + l.rect.height
        })
        .map(|(k, l)| (u32::try_from(k).unwrap_or(u32::MAX), l.clone()))
}

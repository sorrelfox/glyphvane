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
use super::find::FindMarks;
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
}

impl ReaderLayout {
    /// The px above the first item.
    pub(crate) fn lead(self) -> f32 {
        match self {
            Self::Column => document::PAD_TOP,
            Self::Pane => split::PREVIEW_PAD_TOP,
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
            Self::Pane => ((x0, x0 + column), column),
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
}

/// A button in a code block's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonKind {
    Copy,
    Wrap,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CodeButton {
    pub bounds: Bounds<Pixels>,
    pub block: BlockId,
    /// Index of the code block's node.
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
    /// *Open externally* on an image card: the file is
    /// [`Geometry::external`]`[slot]`.
    OpenExternally { slot: usize },
}

/// A footnote reference or backreference under the pointer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FootnoteHit {
    /// A reference's number: jumps to the footnote.
    Ref { label: Arc<str> },
    /// A footnote's `↩` back to its reference `k` (0-based).
    Back { label: Arc<str>, k: u32 },
}

/// What is under the pointer.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Hover {
    #[default]
    None,
    Text,
    Link(LinkHit),
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
    /// Sideways scroll of code blocks without soft wrap.
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
            layout: ReaderLayout::Column,
            band: None,
            sync: None,
            tag_search: None,
            column_max: document::COLUMN_MAX,
            wide_max: f32::INFINITY,
        }
    }

    /// Puts the text column in `layout`.
    pub fn set_layout(&mut self, layout: ReaderLayout) {
        self.layout = layout;
        self.engine = Engine::new(layout.lead(), OVERDRAW);
    }

    /// Shows `doc`, scrolled to the top.
    pub fn install(&mut self, doc: Loaded) {
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
        self.press = Press::None;
        self.geometry = Geometry::default();
        self.stale.clear();
        self.doc_version += 1;
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
        let info = content.links.get(link as usize)?;
        let start = doc.tree.node_of(g.ix).map(|n| n.source.start);
        let range = start
            .map(|s| s + info.source.start..s + info.source.end)
            .filter(|r| {
                // A link of an embedded note is not in this text.
                doc.text
                    .get(r.clone())
                    .is_some_and(|t| t.starts_with(['[', '!', '<']) || t.contains("://"))
            });
        Some(LinkHit {
            item: g.ix,
            link,
            destination: info.destination.clone(),
            kind: info.kind,
            image: false,
            range,
        })
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
            InlineRole::FootnoteRef(i) => Some(FootnoteHit::Ref {
                label: content.footnotes.get(i as usize)?.label.clone(),
            }),
            InlineRole::FootnoteBackref(k) => {
                let item = doc.tree.items().get(g.ix)?;
                let label = item.context.iter().find_map(|f| match &f.kind {
                    FrameKind::Footnote { label, .. } => Some(label.clone()),
                    _ => None,
                })?;
                Some(FootnoteHit::Back { label, k })
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
        if self.geometry.track.is_some_and(|t| t.contains(&position)) {
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
        let y = f32::from(position.y);
        let x = f32::from(position.x);
        let over_text = self.geometry.items.iter().any(|g| {
            let (x0, w) = g.span;
            g.text.is_some() && g.top <= y && y < g.bottom && x >= x0 && x < x0 + w
        });
        if over_text { Hover::Text } else { Hover::None }
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

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
use gv_render_tree::ItemId;
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
    /// Preview: at most 680 wide, centred, 40 px side padding, 22 above.
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
    pub(crate) fn column(self, bounds: Bounds<Pixels>) -> (f32, f32) {
        let width = f32::from(bounds.size.width);
        let left = f32::from(bounds.left());
        match self {
            Self::Column => {
                let column = (width - 2.0 * document::PAD_X)
                    .clamp(1.0, document::COLUMN_MAX)
                    .floor();
                (left + ((width - column) / 2.0).max(0.0).floor(), column)
            }
            Self::Pane => {
                let column = (width - 2.0 * split::PREVIEW_PAD_X).floor().max(1.0);
                (left + split::PREVIEW_PAD_X, column)
            }
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

/// What is under the pointer.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Hover {
    #[default]
    None,
    Text,
    Link(LinkHit),
    Button(CodeButton),
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
    pub items: Vec<PlacedGeom>,
    pub buttons: Vec<CodeButton>,
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
        }
    }

    /// Puts the text column in `layout`.
    pub fn set_layout(&mut self, layout: ReaderLayout) {
        self.layout = layout;
        self.engine = Engine::new(layout.lead(), OVERDRAW);
    }

    /// Shows `doc`, scrolled to the top.
    pub fn install(&mut self, doc: Loaded) {
        self.doc = Some(doc);
        self.engine = Engine::new(self.layout.lead(), OVERDRAW);
        self.sync = None;
        self.cache.clear();
        self.cache.code.forget_blocks();
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
        Some(LinkHit {
            item: g.ix,
            link,
            destination: info.destination.clone(),
            kind: info.kind,
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

    /// What is under the pointer at `position`.
    pub fn hover_at(&self, position: Point<Pixels>) -> Hover {
        if self.geometry.track.is_some_and(|t| t.contains(&position)) {
            return Hover::Scrollbar;
        }
        if let Some(b) = self.button_at(position) {
            return Hover::Button(b);
        }
        if let Some(link) = self.link_at(position) {
            return Hover::Link(link);
        }
        let y = f32::from(position.y);
        let (x0, w) = self.geometry.column;
        let x = f32::from(position.x);
        let over_text = self
            .geometry
            .items
            .iter()
            .any(|g| g.text.is_some() && g.top <= y && y < g.bottom && x >= x0 && x < x0 + w);
        if over_text { Hover::Text } else { Hover::None }
    }

    /// The whole text of code block node `node`.
    pub fn code_text(&self, node: usize) -> Option<String> {
        let doc = self.doc.as_ref()?;
        Some(doc.tree.nodes().get(node)?.lines()?.text.clone())
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

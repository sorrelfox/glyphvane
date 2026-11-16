// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Parser state at a line start (the *checkpoint*) and its hash-consed
//! storage.
//!
//! The block parser carries nothing from one line to the next except a
//! stack of [`Entry`] values. Checkpoints are those stacks in canonical form
//! (fields no later line can read are cleared), interned as a persistent
//! linked list `(parent id, top entry) -> id`. Equal stacks get equal ids, a
//! checkpoint costs one `u32` per line, and re-interning after a change only
//! touches the changed top of the stack.
//!
//! An open paragraph that may still start with link reference definitions
//! carries the text of the *pending* definition candidate (the lines since
//! the last definition that was decided). That text is interned too, so
//! checkpoint equality is exact, never a hash comparison.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use crate::tree::event::FrontMatterKind;

/// Interned parser state.
pub(crate) type StateId = u32;

/// The empty stack (only the document is open).
pub(crate) const ROOT: StateId = 0;

/// [`Kind::Paragraph`] `cells` after a delimiter row that did not match.
pub(crate) const TABLE_VISITED: u16 = u16::MAX;

/// Reference-definition status of an open paragraph.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum RefMode {
    /// No (more) definitions possible: the paragraph has text.
    Text,
    /// Everything so far was complete definitions, all already decided; no
    /// paragraph text yet.
    Defs,
    /// A definition candidate is pending; the id names its text in the
    /// interner.
    Pending(u32),
    /// A long definition candidate is pending (more than the per-line
    /// check limit). Its text is not part of the state: it is rebuilt from
    /// the document after a restart, and lines in this mode are never
    /// convergence points.
    Long,
}

/// Open block kinds with the data later lines depend on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Kind {
    Document,
    BlockQuote,
    /// `ch`: bullet character or ordered delimiter; decides whether a new
    /// item continues the list.
    List {
        ordered: bool,
        ch: u8,
    },
    /// `indent`: content column relative to the parent (marker offset plus
    /// padding).
    Item {
        indent: u32,
    },
    FencedCode {
        ch: u8,
        len: u32,
        offset: u32,
    },
    IndentedCode,
    /// A `$$` math block; `offset`: the opener's indentation, removed from
    /// its lines like a fenced code block's.
    MathBlock {
        offset: u32,
    },
    Html {
        ty: u8,
    },
    /// `cells`: with GFM tables, the number of cells of the paragraph's
    /// last line read as a table row (0 if it is not one), the count a
    /// delimiter row on the next line must match; [`TABLE_VISITED`] once a
    /// delimiter row failed to match (cmark-gfm then never tries again in
    /// this paragraph). Always 0 without tables.
    Paragraph {
        refs: RefMode,
        cells: u16,
    },
    /// A GFM table (a text block whose delimiter row matched). It continues
    /// on every line that is a table row, unless another block starts.
    Table,
    /// ATX or setext heading. Like cmark, it stays open until the next line
    /// closes it.
    Heading,
    /// Like cmark, a thematic break stays open until the next line closes
    /// it.
    ThematicBreak,
    /// Front matter, opened on line 0 only when a closing delimiter line
    /// follows (decided by a lookahead, see [`crate::block::front_matter`]).
    FrontMatter {
        kind: FrontMatterKind,
    },
}

impl Kind {
    #[inline]
    pub(crate) fn is_list_or_item(self) -> bool {
        matches!(self, Kind::List { .. } | Kind::Item { .. })
    }

    /// Leaf blocks that take the rest of a line as content.
    #[inline]
    pub(crate) fn accepts_lines(self) -> bool {
        matches!(
            self,
            Kind::Paragraph { .. }
                | Kind::Heading
                | Kind::FencedCode { .. }
                | Kind::IndentedCode
                | Kind::MathBlock { .. }
                | Kind::Html { .. }
                | Kind::FrontMatter { .. }
        )
    }

    /// cmark's `can_contain_type`.
    #[inline]
    pub(crate) fn can_contain(self, child: Kind) -> bool {
        match self {
            Kind::Document | Kind::BlockQuote | Kind::Item { .. } => {
                !matches!(child, Kind::Item { .. })
            }
            Kind::List { .. } => matches!(child, Kind::Item { .. }),
            _ => false,
        }
    }
}

/// Summary of the last *closed* child of a list or list item: exactly the
/// cmark flags that list-tightness decisions read later (`last_line_blank`
/// and, through `ends_with_blank_line`, the same flag of its last
/// descendants).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub(crate) struct Closed {
    /// cmark `last_line_blank` of the child; can still flip to true on a
    /// blank line.
    pub lb: bool,
    /// `ends_with_blank_line` of the child's own last child (lists and items
    /// only).
    pub ebl_tail: bool,
    /// The child is a list or an item.
    pub li: bool,
}

impl Closed {
    /// cmark `ends_with_blank_line` of the child.
    #[inline]
    pub(crate) fn ebl(self) -> bool {
        self.lb || (self.li && self.ebl_tail)
    }
}

/// One open block on the parser stack.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Entry {
    pub kind: Kind,
    /// cmark `last_line_blank`. At a line start at most the top entry has
    /// it set.
    pub lb: bool,
    /// The block has (had) at least one child (only read for items). For a
    /// text block in an item that may hold only definitions: the item's
    /// value before the text block opened.
    pub has_child: bool,
    /// Last closed child (lists and items only; `None` while the last child
    /// is open or there is none). For a text block in an item that may hold
    /// only definitions: the item's value before the text block opened.
    pub last: Option<Closed>,
}

impl Entry {
    pub(crate) fn new(kind: Kind) -> Self {
        Entry {
            kind,
            lb: false,
            has_child: false,
            last: None,
        }
    }

    /// The canonical form stored in checkpoints: fields that no later line
    /// can read are cleared, so states that behave identically compare
    /// equal.
    ///
    /// `lb` is only read for children of lists and items (when they close
    /// into their parent's `last`); `last` and `has_child` only for lists
    /// and items themselves.
    pub(crate) fn canonical(self, parent: Option<Kind>) -> Entry {
        let lb_matters = parent.is_some_and(Kind::is_list_or_item);
        let li = self.kind.is_list_or_item();
        // A text block in an item that may hold only definitions stashes the
        // item's previous child summary in `has_child` and `last`.
        let stash = matches!(self.kind, Kind::Paragraph { .. })
            && matches!(parent, Some(Kind::Item { .. }));
        Entry {
            kind: self.kind,
            lb: self.lb && lb_matters,
            has_child: self.has_child && (matches!(self.kind, Kind::Item { .. }) || stash),
            last: if li || stash { self.last } else { None },
        }
    }

    /// Whether a blank line continues this block without consuming anything
    /// once the line's whitespace is used up (lists, and items that have a
    /// child). Runs of such entries are skipped in one step.
    #[inline]
    pub(crate) fn blank_transparent(self) -> bool {
        match self.kind {
            Kind::List { .. } => true,
            Kind::Item { .. } => self.has_child,
            _ => false,
        }
    }
}

/// FxHash (rustc's hasher): fast and good enough for small fixed-size keys.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, w: u64) {
        self.hash = (self.hash.rotate_left(5) ^ w).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rest) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.add(u64::from_le_bytes(*chunk));
        }
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(buf) ^ ((rest.len() as u64) << 56));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub(crate) type FxBuild = BuildHasherDefault<FxHasher>;
pub(crate) type FxHashMap<K, V> = HashMap<K, V, FxBuild>;

/// One interned state: the parent state, the top entry and the depth (the
/// number of entries, the document included).
#[derive(Clone, Copy, Debug)]
struct Node {
    parent: StateId,
    entry: Entry,
    depth: u32,
}

#[derive(Clone, Debug)]
struct InternerData {
    map: FxHashMap<(StateId, Entry), StateId>,
    nodes: Vec<Node>,
    texts: FxHashMap<Arc<[u8]>, u32>,
    text_list: Vec<Arc<[u8]>>,
}

/// Hash-consed parser states, shared between the snapshots of a document.
///
/// Cloning is O(1). The first insertion into an interner that is shared
/// with another snapshot copies it (copy on write), so a reparse that only
/// meets known states (typing inside a paragraph) never copies.
#[derive(Clone, Debug)]
pub(crate) struct Interner(Arc<InternerData>);

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub(crate) fn new() -> Self {
        Interner(Arc::new(InternerData {
            map: FxHashMap::default(),
            nodes: vec![Node {
                parent: ROOT,
                entry: Entry::new(Kind::Document),
                depth: 1,
            }],
            texts: FxHashMap::default(),
            text_list: Vec::new(),
        }))
    }

    /// The id of the stack `parent` + `e` (interning it if new).
    #[inline]
    pub(crate) fn intern(&mut self, parent: StateId, e: Entry) -> StateId {
        if let Some(&id) = self.0.map.get(&(parent, e)) {
            return id;
        }
        let data = Arc::make_mut(&mut self.0);
        let id = StateId::try_from(data.nodes.len()).unwrap_or(StateId::MAX);
        let depth = data.nodes[parent as usize].depth + 1;
        data.nodes.push(Node {
            parent,
            entry: e,
            depth,
        });
        data.map.insert((parent, e), id);
        id
    }

    /// The id of a pending reference-definition text (interning it if new).
    pub(crate) fn intern_text(&mut self, text: &[u8]) -> u32 {
        if let Some(&id) = self.0.texts.get(text) {
            return id;
        }
        let data = Arc::make_mut(&mut self.0);
        let id = u32::try_from(data.text_list.len()).unwrap_or(u32::MAX);
        let text: Arc<[u8]> = Arc::from(text);
        data.text_list.push(Arc::clone(&text));
        data.texts.insert(text, id);
        id
    }

    /// A pending reference-definition text by id.
    pub(crate) fn text(&self, id: u32) -> &[u8] {
        &self.0.text_list[id as usize]
    }

    /// Number of entries in the stack (the document entry included).
    #[inline]
    pub(crate) fn depth(&self, id: StateId) -> usize {
        self.0.nodes[id as usize].depth as usize
    }

    /// The top entry of a state.
    #[inline]
    pub(crate) fn top(&self, id: StateId) -> Entry {
        self.0.nodes[id as usize].entry
    }

    /// The stack bottom-up, with the id of each prefix.
    pub(crate) fn resolve(&self, id: StateId) -> (Vec<Entry>, Vec<StateId>) {
        let depth = self.depth(id);
        let mut entries = Vec::with_capacity(depth);
        let mut ids = Vec::with_capacity(depth);
        let mut cur = id;
        loop {
            let n = self.0.nodes[cur as usize];
            entries.push(n.entry);
            ids.push(cur);
            if cur == ROOT {
                break;
            }
            cur = n.parent;
        }
        entries.reverse();
        ids.reverse();
        (entries, ids)
    }

    /// Number of distinct states interned so far.
    pub(crate) fn len(&self) -> usize {
        self.0.nodes.len()
    }
}

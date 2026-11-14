// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Find in page in the Reading view.
//!
//! - **What is searched**: the render tree's plain-text projection, item by
//!   item ([`gv_render_tree::Finder`] has the matching rules; matches do
//!   not span items). The text of every item is kept in a
//!   [`FindIndex`], built once per document on the background executor
//!   when find opens, so a keystroke only runs the matcher (about 5 ms for
//!   a 10 MB document).
//! - **Incremental**: every change of the query or of an option starts a
//!   new search. Documents of up to [`SYNC_ITEMS`] items are searched on the
//!   UI thread; larger ones on the background executor, in chunks of
//!   [`CHUNK`] items that check a generation counter, so a search that a
//!   newer one replaces stops early. Results of an older generation or an
//!   older document are dropped.
//! - **Capped**: at most [`MAX_MATCHES`] matches are kept; the count then
//!   reads "10,000+" and next and previous wrap within those.
//! - **Current match**: after a search, the first match at or after the
//!   previous current match (typing more keeps the place), or else after
//!   the top of the viewport. It is scrolled into view, centred, when it is
//!   not visible; next and previous wrap around.
//! - **Highlights**: every match on screen has an `accent.tint` background
//!   (the search panel's colour for matched text, SPEC § Panels); the
//!   current one has `accent.line`. `selection.text` would be the obvious
//!   second colour, but in every theme it is within a few percent of
//!   lightness of `accent.tint` (#B6E1E6 against #CCEEEF in Light, #0D4B51
//!   against #104042 in Dark), so the current match would not stand out.
//!   Only items that are laid out paint their matches.
//! - **Document changes**: a newly loaded document (a reload, another file
//!   in the tab) is searched again with the same query; a refresh of link
//!   statuses changes neither item texts nor item indices, so the matches
//!   stay valid.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use gpui::{AppContext as _, Context, Entity, Subscription, Task, Window, WindowTextSystem};
use gv_inline::Bias;
use gv_render_tree::{FindIndex, FindMatch, Finder, RenderTree};

use super::Reader;
use super::items::{code_inset, is_code, text_top, text_width};
use super::state::State;
use crate::find::{CloseFind, Find, FindBar, FindBarEvent, FindNext, FindPrevious, FindStatus};
use crate::inline::Token;

/// With `GLYPHVANE_DEBUG_FIND=QUERY` the first document that shows opens
/// find with `QUERY` in the field (for screenshots of the popover).
pub const DEBUG_FIND_ENV: &str = "GLYPHVANE_DEBUG_FIND";

/// One per process: the first reader that shows a document takes it.
static DEBUG_FIND_TAKEN: AtomicBool = AtomicBool::new(false);

/// At most this many matches are kept.
pub(crate) const MAX_MATCHES: usize = 10_000;

/// Documents with at most this many items are searched on the UI thread
/// (their index builds in well under a millisecond).
pub(crate) const SYNC_ITEMS: usize = 2_000;

/// Items searched between two checks for a newer search.
const CHUNK: usize = 4_096;

/// The token of a match highlight.
pub(crate) const MATCH: Token = Token::AccentTint;
/// The token of the current match's highlight.
pub(crate) const CURRENT_MATCH: Token = Token::AccentLine;

/// The matches the element paints.
#[derive(Debug, Clone, Default)]
pub(crate) struct FindMarks {
    /// The document they are for ([`State::doc_version`]).
    pub version: u64,
    pub matches: Arc<Vec<FindMatch>>,
    /// More matches than [`MAX_MATCHES`].
    pub more: bool,
    pub current: Option<usize>,
    /// Scroll the current match into view in the next frame.
    pub reveal: bool,
}

impl FindMarks {
    /// The matches in item `ix`, with their indices.
    pub fn in_item(&self, ix: usize) -> impl Iterator<Item = (usize, &FindMatch)> {
        let start = self.matches.partition_point(|m| m.item < ix);
        self.matches[start..]
            .iter()
            .take_while(move |m| m.item == ix)
            .enumerate()
            .map(move |(k, m)| (start + k, m))
    }

    /// The highlights of item `ix` in node-relative offsets (the node
    /// starts at `node_start`).
    pub fn highlights(&self, ix: usize, node_start: usize) -> Vec<(Range<usize>, Token)> {
        self.in_item(ix)
            .map(|(k, m)| {
                let r = m.source.start.saturating_sub(node_start)
                    ..m.source.end.saturating_sub(node_start);
                let token = if Some(k) == self.current {
                    CURRENT_MATCH
                } else {
                    MATCH
                };
                (r, token)
            })
            .collect()
    }

    fn status(&self) -> FindStatus {
        FindStatus::Matches {
            current: self.current,
            total: self.matches.len(),
            more: self.more,
        }
    }
}

/// What one search found.
struct Found {
    /// The index, when the search built it.
    index: Option<Arc<FindIndex>>,
    /// The matches and whether there are more; `None` when a newer search
    /// replaced this one.
    matches: Option<(Vec<FindMatch>, bool)>,
}

/// Searches `index` with `finder`, checking `generation` between chunks.
fn search(
    index: &FindIndex,
    finder: &Finder,
    generation: &AtomicU64,
    mine: u64,
) -> Option<(Vec<FindMatch>, bool)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < index.len() {
        if generation.load(Ordering::Relaxed) != mine {
            return None;
        }
        let end = (start + CHUNK).min(index.len());
        if index.find_in(finder, start..end, MAX_MATCHES, &mut out) {
            return Some((out, true));
        }
        start = end;
    }
    Some((out, false))
}

/// Builds the index if needed, then searches it.
fn run(
    tree: &RenderTree,
    index: Option<Arc<FindIndex>>,
    finder: Option<&Finder>,
    generation: &AtomicU64,
    mine: u64,
) -> Found {
    let (index, built) = match index {
        Some(i) => (i, false),
        None => (Arc::new(tree.find_index()), true),
    };
    let matches = match finder {
        Some(f) => search(&index, f, generation, mine),
        None => Some((Vec::new(), false)),
    };
    Found {
        index: built.then_some(index),
        matches,
    }
}

/// Find in page of one Reading view.
#[derive(Default)]
pub(crate) struct FindState {
    pub bar: Option<Entity<FindBar>>,
    pub open: bool,
    /// The find index of a document version.
    index: Option<(u64, Arc<FindIndex>)>,
    /// Bumped per search; a search that sees a newer value stops.
    generation: Arc<AtomicU64>,
    /// Where the next search's current match starts from (the selection
    /// that filled the field).
    from: Option<usize>,
    task: Option<Task<()>>,
    _events: Option<Subscription>,
}

impl std::fmt::Debug for FindState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FindState")
            .field("open", &self.open)
            .field("indexed", &self.index.as_ref().map(|(r, _)| *r))
            .finish_non_exhaustive()
    }
}

impl Reader {
    /// The find popover, if it was ever opened.
    pub fn find_bar(&self) -> Option<&Entity<FindBar>> {
        self.find.bar.as_ref()
    }

    /// Whether the find popover is open.
    pub fn is_find_open(&self) -> bool {
        self.find.open
    }

    fn ensure_bar(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Entity<FindBar> {
        if let Some(bar) = &self.find.bar {
            return bar.clone();
        }
        let bar = cx.new(|cx| FindBar::new(window, cx));
        self.find._events = Some(cx.subscribe_in(
            &bar,
            window,
            |this, _, event: &FindBarEvent, window, cx| match event {
                FindBarEvent::Changed => this.search(cx),
                FindBarEvent::Next => this.step(true, cx),
                FindBarEvent::Previous => this.step(false, cx),
                FindBarEvent::Close => this.close_find(window, cx),
            },
        ));
        self.find.bar = Some(bar.clone());
        bar
    }

    /// The debug query of [`DEBUG_FIND_ENV`], once per process, when the
    /// document shows.
    pub(super) fn debug_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        static QUERY: OnceLock<Option<String>> = OnceLock::new();
        if DEBUG_FIND_TAKEN.load(Ordering::Relaxed) {
            return;
        }
        let Some(query) = QUERY.get_or_init(|| std::env::var(DEBUG_FIND_ENV).ok()) else {
            return;
        };
        if DEBUG_FIND_TAKEN.swap(true, Ordering::Relaxed) {
            return;
        }
        let bar = self.ensure_bar(window, cx);
        self.find.open = true;
        bar.update(cx, |bar, cx| {
            bar.set_query(query, window, cx);
            bar.focus_field(window, cx);
        });
        self.search(cx);
    }

    /// Mod+F: opens the popover (or keeps it open), fills it with the
    /// selection when that is one line of text, and focuses the field with
    /// its text selected.
    pub(super) fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<'_, Self>) {
        let bar = self.ensure_bar(window, cx);
        let prefill = self
            .selected_text()
            .filter(|t| !t.is_empty() && !t.contains('\n'));
        if prefill.is_some() {
            self.find.from = self.state.borrow().selected().map(|r| r.start);
        }
        let was_open = std::mem::replace(&mut self.find.open, true);
        bar.update(cx, |bar, cx| {
            if let Some(text) = &prefill {
                bar.set_query(text, window, cx);
            }
            bar.focus_field(window, cx);
        });
        if prefill.is_some() || !was_open {
            self.search(cx);
        }
        cx.notify();
    }

    /// Esc or the close button: closes the popover, drops the highlights
    /// and gives the keyboard back to the document. The query and options
    /// stay for the next time.
    pub(super) fn close_find(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.find.open {
            cx.propagate();
            return;
        }
        self.find.open = false;
        self.find.from = None;
        self.find.generation.fetch_add(1, Ordering::Relaxed);
        if let Some(task) = self.find.task.take() {
            task.detach();
        }
        self.state.borrow_mut().find = FindMarks::default();
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn on_close_find(
        &mut self,
        _: &CloseFind,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.close_find(window, cx);
    }

    /// F3 and Shift+F3 (Cmd+G, Cmd+Shift+G): the next or previous match;
    /// with the popover closed, they open it first.
    pub(super) fn on_find_next(
        &mut self,
        _: &FindNext,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.next_or_open(true, window, cx);
    }

    pub(super) fn on_find_previous(
        &mut self,
        _: &FindPrevious,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.next_or_open(false, window, cx);
    }

    fn next_or_open(&mut self, forward: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.find.open {
            self.step(forward, cx);
        } else {
            self.open_find(&Find, window, cx);
        }
    }

    /// Moves the current match forward or back, wrapping around.
    fn step(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let status = {
            let mut st = self.state.borrow_mut();
            let marks = &mut st.find;
            let n = marks.matches.len();
            if n == 0 {
                return;
            }
            let next = match (marks.current, forward) {
                (None, true) => 0,
                (None, false) => n - 1,
                (Some(k), true) => (k + 1) % n,
                (Some(k), false) => (k + n - 1) % n,
            };
            marks.current = Some(next);
            marks.reveal = true;
            marks.status()
        };
        self.set_bar_status(status, cx);
        cx.notify();
    }

    fn set_bar_status(&self, status: FindStatus, cx: &mut Context<'_, Self>) {
        if let Some(bar) = &self.find.bar {
            bar.update(cx, |bar, cx| bar.set_status(status, cx));
        }
    }

    /// The loaded document changed: search it again.
    pub(super) fn find_document_changed(&mut self, cx: &mut Context<'_, Self>) {
        if self.find.open {
            self.search(cx);
        }
    }

    /// Starts a search for the popover's query in the loaded document.
    pub(super) fn search(&mut self, cx: &mut Context<'_, Self>) {
        let Some(bar) = self.find.bar.clone() else {
            return;
        };
        if !self.find.open {
            return;
        }
        let (query, options) = bar.read_with(cx, |bar, cx| (bar.query(cx), bar.options()));
        let mine = self.find.generation.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(task) = self.find.task.take() {
            // It stops at its next chunk; if it was building the index,
            // the index is still kept.
            task.detach();
        }
        let finder = match Finder::new(&query, options) {
            Ok(f) if f.is_empty() => None,
            Ok(f) => Some(f),
            Err(err) => {
                tracing::debug!("find: {err}");
                self.show_results(mine, None, FindStatus::Invalid, cx);
                return;
            }
        };
        let (tree, revision) = {
            let st = self.state.borrow();
            let Some(doc) = &st.doc else {
                return;
            };
            (doc.tree.clone(), st.doc_version)
        };
        let index = self
            .find
            .index
            .as_ref()
            .filter(|(r, _)| *r == revision)
            .map(|(_, i)| i.clone());
        if finder.is_none() {
            self.show_results(mine, None, FindStatus::Empty, cx);
            if index.is_some() {
                return;
            }
        }
        let generation = self.find.generation.clone();
        if tree.items().len() <= SYNC_ITEMS {
            let found = run(&tree, index, finder.as_ref(), &generation, mine);
            self.finish_search(mine, revision, finder.is_some(), found, cx);
            return;
        }
        let searching = finder.is_some();
        self.find.task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(
                    async move { run(&tree, index, finder.as_ref(), &generation, mine) },
                )
                .await;
            this.update(cx, |reader, cx| {
                reader.finish_search(mine, revision, searching, found, cx);
            })
            .ok();
        }));
    }

    fn finish_search(
        &mut self,
        mine: u64,
        revision: u64,
        searching: bool,
        found: Found,
        cx: &mut Context<'_, Self>,
    ) {
        let current_revision = self.state.borrow().doc_version;
        if revision != current_revision {
            return;
        }
        if let Some(index) = found.index {
            self.find.index = Some((revision, index));
        }
        if !searching {
            return;
        }
        let Some((matches, more)) = found.matches else {
            return;
        };
        let marks = {
            let st = self.state.borrow();
            // Keep the place: from the selection that filled the field,
            // the current match, or else the top of the viewport.
            let from = self
                .find
                .from
                .take()
                .or_else(|| {
                    st.find
                        .current
                        .filter(|_| st.find.version == revision)
                        .and_then(|k| st.find.matches.get(k))
                        .map(|m| m.source.start)
                })
                .or_else(|| self.position_offset(&st))
                .unwrap_or(0);
            let k = matches.partition_point(|m| m.source.start < from);
            let current = if matches.is_empty() {
                None
            } else if k < matches.len() {
                Some(k)
            } else {
                Some(0)
            };
            FindMarks {
                version: revision,
                matches: Arc::new(matches),
                more,
                current,
                reveal: current.is_some(),
            }
        };
        let status = marks.status();
        self.show_results(mine, Some(marks), status, cx);
    }

    /// The source offset of the top of the viewport.
    fn position_offset(&self, st: &State) -> Option<usize> {
        let doc = st.doc.as_ref()?;
        let anchor = st.engine.anchor()?;
        let ix = doc.tree.index_of(anchor.id)?;
        Some(doc.tree.items()[ix].source.start)
    }

    fn show_results(
        &mut self,
        mine: u64,
        marks: Option<FindMarks>,
        status: FindStatus,
        cx: &mut Context<'_, Self>,
    ) {
        if mine != self.find.generation.load(Ordering::Relaxed) || !self.find.open {
            return;
        }
        self.state.borrow_mut().find = marks.unwrap_or_default();
        self.set_bar_status(status, cx);
        cx.notify();
    }
}

/// Scrolls the current match into view if it is asked for and not visible:
/// centred vertically, and sideways in code that does not wrap. Runs at
/// the start of a frame, before the engine places the items, with the text
/// column `column` px wide.
pub(super) fn reveal_current(st: &mut State, column: f32, ts: &WindowTextSystem) {
    if !std::mem::take(&mut st.find.reveal) {
        return;
    }
    let State {
        doc,
        engine,
        cache,
        text,
        hscroll,
        find,
        doc_version,
        ..
    } = st;
    let Some(doc) = doc.as_ref() else {
        return;
    };
    if find.version != *doc_version {
        return;
    }
    let Some(m) = find.current.and_then(|k| find.matches.get(k)) else {
        return;
    };
    let tree = &doc.tree;
    let (Some(item), Some(node)) = (tree.items().get(m.item), tree.node_of(m.item)) else {
        return;
    };
    let metrics = tree.options().metrics;
    let wrapped = tree.code_wrap(item.id.block);
    let local = m.source.start.saturating_sub(node.source.start);
    let local_end = m.source.end.saturating_sub(node.source.start);
    let line_height = tree
        .options()
        .styles
        .role(gv_render_tree::TextRole::Body)
        .line_height;
    let (offset, height) = match cache.block(tree, m.item) {
        Some(block) => {
            let w = text_width(item, column, &metrics, wrapped);
            let layout = text.borrow_mut().layout(&mut block.borrow_mut(), w, ts);
            let start = layout.caret_for_source(local, Bias::Left);
            if is_code(&item.kind) && !wrapped {
                let end = layout.caret_for_source(local_end, Bias::Right);
                let inner = (column - item.content_x(&metrics)) - 2.0 * code_inset(&metrics);
                let x = hscroll.get(&item.id.block).copied().unwrap_or(0.0);
                if start.x < x || end.x > x + inner {
                    hscroll.insert(item.id.block, (start.x - inner / 3.0).max(0.0));
                }
            }
            (
                text_top(item, &metrics) + start.top + start.height / 2.0,
                start.height,
            )
        }
        None => (item.gap_before + line_height / 2.0, line_height),
    };
    let viewport = engine.viewport();
    let visible = engine.placed().iter().any(|p| {
        p.ix == m.item && {
            let y = p.y + offset;
            y - height / 2.0 >= 0.0 && y + height / 2.0 <= viewport
        }
    });
    if !visible {
        engine.jump_centred(m.item, offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(md: &str) -> RenderTree {
        let syntax = gv_syntax::parse(md);
        let model = gv_model::DocModel::build(&syntax, md);
        RenderTree::build(
            &syntax,
            &model,
            md,
            gv_render_tree::RenderOptions::commonmark(),
            gv_render_tree::Sources::default(),
        )
    }

    #[test]
    fn a_newer_search_stops_an_older_one() {
        let t = tree("a b a\n\nb a\n");
        let index = t.find_index();
        let finder = Finder::new("a", gv_render_tree::FindOptions::default()).expect("compiles");
        let generation = AtomicU64::new(7);
        let (found, more) = search(&index, &finder, &generation, 7).expect("current");
        assert_eq!((found.len(), more), (3, false));
        assert!(search(&index, &finder, &generation, 6).is_none());
        // A stopped search that built the index still hands it over.
        let job = run(&t, None, Some(&finder), &generation, 6);
        assert!(job.index.is_some() && job.matches.is_none());
    }

    #[test]
    fn marks_give_each_item_its_highlights() {
        let t = tree("x a\n\nb\n\na a\n");
        let finder = Finder::new("a", gv_render_tree::FindOptions::default()).expect("compiles");
        let (matches, _) = t.find_index().find(&finder, MAX_MATCHES);
        let marks = FindMarks {
            version: 1,
            matches: Arc::new(matches),
            more: false,
            current: Some(1),
            reveal: false,
        };
        let start = |ix: usize| t.node_of(ix).expect("node").source.start;
        assert_eq!(marks.highlights(0, start(0)), [(2..3, MATCH)]);
        assert!(marks.highlights(1, start(1)).is_empty());
        assert_eq!(
            marks.highlights(2, start(2)),
            [(0..1, CURRENT_MATCH), (2..3, MATCH)]
        );
    }
}

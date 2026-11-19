// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live blocks in the Reading view ([`gv_render_tree::LiveBlock`]): canvas
//! embeds, bases (embeds and ` ```base ` fences), search embeds
//! (` ```query ` fences) and media cards (audio, video, PDF, iframes;
//! [`super::media_card`]).
//!
//! # Filling in
//!
//! - While drawing, a live item looks its content up by the block's
//!   [`LiveKey`] ([`LiveBlocks::shown`]); nothing is read or run on the UI
//!   thread. A block not asked for yet is asked for and drawn *pending* at
//!   its reserved height (the render tree's estimate).
//! - The view fetches what was asked ([`Reader::fetch_live`]): an embed's
//!   target is resolved like a note embed's (from the embedding note
//!   inside a note embed), read and parsed on the background executor; a
//!   query runs on the low-priority pool over the vault's content
//!   snapshot ([`gv_search::search_all`]). When it lands, the items that
//!   showed it are laid out again; the virtual list keeps its scroll
//!   anchor while their height changes. A known height is also what the
//!   item is estimated at from then on ([`LiveBlocks::height`]).
//! - Content is kept by key across rebuilds (zoom, profile, edits
//!   elsewhere) and dropped for keys the new document does not have; a
//!   new document file starts afresh.
//! - A new content snapshot ([`Reader::set_content`], or the vault's index
//!   when the window has one) refreshes every block: bases list the files
//!   again, queries run again, canvas and base files are read again; what
//!   is shown stays until the new content lands.
//!
//! # Drawing (not designed: built plainly, flagged)
//!
//! A bordered box (like a note embed's) with a header row: the kind's
//! icon and the file name, the query, or *Base*; a query's result count
//! on the right. Below it:
//!
//! - **Canvas**: the canvas as shapes, fitted
//!   ([`crate::canvas_view::paint_preview`]); a click anywhere opens the
//!   canvas.
//! - **Base**: a [`BasesView`] hosted in the box ([`BasesView::embedded`],
//!   `this` the embedding note): it scrolls and its clicks open files; the
//!   Reading view leaves pointer input over it to it.
//! - **Query**: a note per row (a click opens it) with up to
//!   [`QUERY_SNIPPETS`] context lines under it, at most [`QUERY_FILES`]
//!   notes and a line saying how many more there are.
//! - **Media**: a line saying what the file or page is, and the action
//!   (*Open externally*, *Open in PDF viewer*, *Open in browser*) on the
//!   header's right; a click anywhere on the card follows it.
//! - Pending blocks say *Loading…*; blocks that cannot be shown say why.
//!
//! Selection and copy see the embed's line or the fence (the render
//! tree's text); a selection over a block tints it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, Subscription};
use gv_render_tree::{ItemId, LiveBlock, LiveKey, LiveKind, NodeKind, RenderTree};
use gv_search::{CancelToken, FileResult, Query, SearchOptions, VaultSource};
use gv_vault::content::{ContentSnapshot, DiskSource};
use gv_vault::{Status, TargetKind, VaultPath};

use super::links::Resolver;
use super::{Disposition, LinkRequest, Reader, ReaderEvent};
use crate::bases_view::{BasesEvent, BasesView};
use crate::canvas_view::Doc as CanvasDoc;

/// Notes a query block lists at most.
pub(crate) const QUERY_FILES: usize = 20;
/// Context lines a query block shows per note at most.
pub(crate) const QUERY_SNIPPETS: usize = 2;
/// Notes a query block's search stops at (counted, not listed).
const QUERY_SCAN_LIMIT: usize = 1_000;
/// Padding inside a live block's box, px at zoom 1 (not designed).
pub(crate) const PAD: f32 = 8.0;
/// The largest canvas or base file a block reads.
const MAX_FILE_BYTES: u64 = 8 << 20;

/// What a live block shows now.
#[derive(Clone)]
pub(crate) enum Shown {
    /// Waiting for its content.
    Pending,
    Canvas(Arc<CanvasDoc>),
    Base(Entity<BasesView>),
    Query(Arc<QueryShown>),
    /// A media card.
    Media(Arc<super::media_card::MediaCard>),
    /// An audio file of the vault: its card's header over a hosted player.
    Audio(
        Arc<super::media_card::MediaCard>,
        Entity<crate::audio::AudioPlayerView>,
    ),
    /// A PDF of the vault: its card's header over the page drawn.
    PdfPage(Arc<super::media_card::PdfPageShown>),
    /// It cannot be shown; why.
    Notice(Arc<str>),
}

impl Shown {
    /// The media card shown, also over an audio player.
    pub fn card(&self) -> Option<&Arc<super::media_card::MediaCard>> {
        match self {
            Self::Media(m) | Self::Audio(m, _) => Some(m),
            Self::PdfPage(p) => Some(&p.card),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Shown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::Canvas(_) => f.write_str("Canvas(..)"),
            Self::Base(_) => f.write_str("Base(..)"),
            Self::Query(q) => write!(f, "Query({} files)", q.files.len()),
            Self::Media(m) => write!(f, "Media({:?})", m.title),
            Self::Audio(m, _) => write!(f, "Audio({:?})", m.title),
            Self::PdfPage(p) => write!(f, "PdfPage({p:?})"),
            Self::Notice(n) => write!(f, "Notice({n:?})"),
        }
    }
}

/// A query's results as a block shows them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QueryShown {
    /// The notes listed (at most [`QUERY_FILES`]), ranked.
    pub files: Vec<FileResult>,
    /// Notes that matched (all of them, up to the scan limit).
    pub matched: usize,
    /// The scan stopped at its limit.
    pub limit_reached: bool,
    pub rows: Vec<QueryRow>,
}

/// A row of a query block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryRow {
    /// Note `files[i]`.
    Note(usize),
    /// Context line `j` of note `i`.
    Line(usize, usize),
    /// So many more notes, not listed.
    More(usize),
    /// Nothing matched.
    Empty,
}

impl QueryShown {
    pub fn new(mut files: Vec<FileResult>, limit_reached: bool) -> Self {
        let matched = files.len();
        files.truncate(QUERY_FILES);
        let mut rows = Vec::new();
        for (i, f) in files.iter().enumerate() {
            rows.push(QueryRow::Note(i));
            for j in 0..f.snippets.len().min(QUERY_SNIPPETS) {
                rows.push(QueryRow::Line(i, j));
            }
        }
        if matched > files.len() {
            rows.push(QueryRow::More(matched - files.len()));
        }
        if rows.is_empty() {
            rows.push(QueryRow::Empty);
        }
        Self {
            files,
            matched,
            limit_reached,
            rows,
        }
    }

    /// The header's count: "3 notes", "1,000+ notes".
    pub fn count(&self) -> String {
        let n = crate::search_panel::grouped_count(self.matched);
        let plus = if self.limit_reached { "+" } else { "" };
        let what = if self.matched == 1 { "note" } else { "notes" };
        format!("{n}{plus} {what}")
    }
}

/// The height of a live block's box showing `shown`, in px: `reserve` (the
/// render tree's estimate) while it is pending; the reserved height for a
/// canvas or a base; a query's rows; one line for a notice. `line` is the
/// body line height and `zoom` the view's scale.
pub(crate) fn box_height(shown: &Shown, reserve: f32, line: f32, zoom: f32) -> f32 {
    let head = line + 2.0 * PAD * zoom;
    match shown {
        Shown::Pending => reserve,
        Shown::Canvas(_) | Shown::Base(_) => reserve.max(head + line),
        Shown::Query(q) => {
            #[allow(clippy::cast_precision_loss)]
            let rows = q.rows.len() as f32;
            head + rows * line + PAD * zoom
        }
        Shown::Notice(_) | Shown::Media(_) => head + line + PAD * zoom,
        // The player draws at fixed interface sizes.
        Shown::Audio(..) => head + super::media_card::PLAYER_HEIGHT + PAD * zoom,
        Shown::PdfPage(p) => head + p.height * zoom + PAD * zoom,
    }
}

/// What the view knows of one block.
struct Entry {
    block: Arc<LiveBlock>,
    shown: Shown,
    /// The items drawn with it (laid out again when it lands).
    items: HashSet<ItemId>,
    /// A fetch runs.
    asked: bool,
    /// Asked for again on its next draw (new content).
    stale: bool,
    /// Its height as last measured, px.
    height: Option<f32>,
    /// A base embed's file text, to keep its view when the file did not
    /// change.
    text: Option<Arc<str>>,
    _events: Option<Subscription>,
}

/// The live blocks of the shown document and their content.
#[derive(Default)]
pub(crate) struct LiveBlocks {
    entries: HashMap<LiveKey, Entry>,
    requests: Vec<Arc<LiveBlock>>,
}

impl std::fmt::Debug for LiveBlocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveBlocks")
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl LiveBlocks {
    /// What `block` shows for item `item`; asks for it when it is not
    /// known yet or is stale.
    pub fn shown(&mut self, block: &Arc<LiveBlock>, item: ItemId) -> Shown {
        let e = self.entries.entry(block.key).or_insert_with(|| Entry {
            block: Arc::clone(block),
            shown: Shown::Pending,
            items: HashSet::new(),
            asked: false,
            stale: true,
            height: None,
            text: None,
            _events: None,
        });
        e.items.insert(item);
        if e.stale && !e.asked {
            e.stale = false;
            e.asked = true;
            self.requests.push(Arc::clone(&e.block));
        }
        e.shown.clone()
    }

    /// What `key` shows, without asking for it.
    pub fn get(&self, key: LiveKey) -> Option<&Shown> {
        self.entries.get(&key).map(|e| &e.shown)
    }

    /// The blocks asked for since the last call.
    pub fn take_requests(&mut self) -> Vec<Arc<LiveBlock>> {
        std::mem::take(&mut self.requests)
    }

    /// The content of `key` landed: the items to lay out again.
    pub fn landed(&mut self, key: LiveKey, shown: Shown) -> Vec<ItemId> {
        let Some(e) = self.entries.get_mut(&key) else {
            return Vec::new();
        };
        e.asked = false;
        e.shown = shown;
        e.items.iter().copied().collect()
    }

    /// A fetch of `key` ended without new content (the shown one stays).
    pub fn unchanged(&mut self, key: LiveKey) {
        if let Some(e) = self.entries.get_mut(&key) {
            e.asked = false;
        }
    }

    /// Keeps the base view's event subscription and file text with `key`.
    pub(super) fn keep(
        &mut self,
        key: LiveKey,
        events: Option<Subscription>,
        text: Option<Arc<str>>,
    ) {
        if let Some(e) = self.entries.get_mut(&key) {
            e._events = events;
            e.text = text;
        }
    }

    fn text(&self, key: LiveKey) -> Option<Arc<str>> {
        self.entries.get(&key).and_then(|e| e.text.clone())
    }

    /// Every block is asked for again on its next draw (new content); what
    /// is shown stays meanwhile. Returns the items to draw again.
    pub fn refresh(&mut self, which: impl Fn(&LiveBlock) -> bool) -> Vec<ItemId> {
        let mut items = Vec::new();
        for e in self.entries.values_mut() {
            if which(&e.block) {
                e.stale = true;
                items.extend(e.items.iter().copied());
            }
        }
        items
    }

    /// The base views shown.
    pub fn bases(&self) -> impl Iterator<Item = &Entity<BasesView>> {
        self.entries.values().filter_map(|e| match &e.shown {
            Shown::Base(v) => Some(v),
            _ => None,
        })
    }

    /// Notes block `key`'s measured height.
    pub fn set_height(&mut self, key: LiveKey, height: f32) {
        if let Some(e) = self.entries.get_mut(&key) {
            e.height = Some(height);
        }
    }

    /// Block `key`'s height as last measured: its estimate from then on.
    pub fn height(&self, key: LiveKey) -> Option<f32> {
        self.entries.get(&key).and_then(|e| e.height)
    }

    /// Keeps the blocks of `tree` (a new build of the document): the
    /// others go. Item ids the new tree does not have are ignored when
    /// content lands.
    pub fn retain_tree(&mut self, tree: &RenderTree) {
        let keys: HashSet<LiveKey> = tree
            .nodes()
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::Live(l) => Some(l.key),
                _ => None,
            })
            .collect();
        self.entries.retain(|k, _| keys.contains(k));
        self.requests.retain(|b| keys.contains(&b.key));
    }

    /// Forgets everything (another document).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.requests.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The vault path of `path` below `root`.
fn vault_path_of(path: &Path, root: &Path) -> Option<VaultPath> {
    VaultPath::from_relative_fs_path(path.strip_prefix(root).ok()?).ok()
}

/// Why an embed's target cannot be shown.
pub(crate) fn target_notice(target: &str, status: Status) -> Arc<str> {
    match status {
        Status::OutsideRoot => format!("\u{201c}{target}\u{201d} is outside the vault.").into(),
        Status::Blocked => format!("\u{201c}{target}\u{201d} is blocked.").into(),
        _ => format!("\u{201c}{target}\u{201d} was not found.").into(),
    }
}

/// The search a query block's text stands for: its non-empty lines,
/// joined; why when it is not a valid query.
pub(crate) fn query_of(text: &str) -> Result<Query, String> {
    let words: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Query::new(&words.join(" "), false).map_err(|e| format!("Invalid query: {e}"))
}

/// Runs `query` over the vault's `content` (files read below `root`) as a
/// query block does (blocking: the low-priority pool).
pub(crate) fn search(
    query: &Query,
    content: ContentSnapshot,
    root: &Path,
    cancel: &CancelToken,
) -> QueryShown {
    let source = VaultSource::new(
        content.clone(),
        Arc::new(DiskSource::new(root, content.options())),
    );
    let options = SearchOptions {
        max_files: QUERY_SCAN_LIMIT,
        max_snippets_per_file: QUERY_SNIPPETS,
        ..SearchOptions::default()
    };
    let (files, summary) = gv_search::search_all(query, &source, &options, cancel);
    QueryShown::new(files, summary.limit_reached)
}

/// Reads a canvas or base file (blocking: the background executor).
pub(crate) fn read_file(path: &Path) -> Result<Arc<str>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Cannot read the file: {e}"))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err("The file is too large to show here.".to_owned());
    }
    std::fs::read_to_string(path)
        .map(Arc::from)
        .map_err(|e| format!("Cannot read the file: {e}"))
}

impl Reader {
    /// The vault content snapshot live blocks use (bases list its files,
    /// queries search it). The window gives each new one; a new
    /// generation refreshes every live block.
    pub fn set_content(&mut self, snapshot: Option<ContentSnapshot>, cx: &mut Context<'_, Self>) {
        self.content_given = true;
        self.apply_content(snapshot, cx);
    }

    fn apply_content(&mut self, snapshot: Option<ContentSnapshot>, cx: &mut Context<'_, Self>) {
        let generation = |s: &Option<ContentSnapshot>| {
            s.as_ref()
                .map(|s| (s.generation(), s.root().map(Path::to_path_buf)))
        };
        if generation(&snapshot) == generation(&self.content) {
            return;
        }
        self.content = snapshot;
        let bases: Vec<Entity<BasesView>> = {
            let st = self.state.borrow();
            st.cache.live.bases().cloned().collect()
        };
        if let Some(content) = &self.content {
            for base in bases {
                let content = content.clone();
                base.update(cx, |b, cx| b.set_content(content, cx));
            }
        }
        let mut st = self.state.borrow_mut();
        // Bases list the new files themselves; the files read again and
        // the queries run again.
        let items = st
            .cache
            .live
            .refresh(|b| !(b.kind == LiveKind::Base && b.target.is_none()));
        st.stale.extend(items);
        drop(st);
        cx.notify();
    }

    /// Without a snapshot from the window: the content index of the vault
    /// that holds the document, when the window has one.
    pub(super) fn poll_content(&mut self, cx: &mut Context<'_, Self>) {
        if self.content_given {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        if let Some((_, snapshot)) = crate::files::overlay::content_of(&path) {
            self.apply_content(Some(snapshot), cx);
        }
    }

    /// The links of embeds may resolve differently with a new index.
    pub(super) fn refresh_live_targets(&mut self) {
        let mut st = self.state.borrow_mut();
        let items = st.cache.live.refresh(|b| b.target.is_some());
        st.stale.extend(items);
    }

    /// Starts checking the vault's content index for new snapshots, every
    /// [`CONTENT_POLL`], while the window gives none.
    fn watch_content(&mut self, cx: &mut Context<'_, Self>) {
        if self.content_given || self._content_poll.is_some() {
            return;
        }
        self.poll_content(cx);
        self._content_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CONTENT_POLL).await;
                if this.update(cx, |r, cx| r.poll_content(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Fetches the content of `blocks` (asked for while drawing).
    pub(super) fn fetch_live(&mut self, blocks: Vec<Arc<LiveBlock>>, cx: &mut Context<'_, Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if blocks
            .iter()
            .any(|b| b.kind != LiveKind::Canvas && !b.kind.is_media())
        {
            self.watch_content(cx);
        }
        let resolver = Resolver::for_click(&path, self.index.as_ref(), self.profile.id);
        let root: PathBuf = self
            .content
            .as_ref()
            .and_then(|c| c.root().map(Path::to_path_buf))
            .unwrap_or_else(|| resolver.root().to_path_buf());
        let doc_key: Arc<str> = resolver.source().as_str().into();
        for block in blocks {
            if block.kind.is_media() {
                self.fetch_media(&block, &resolver, cx);
                continue;
            }
            let this = Some(block.host.clone().unwrap_or_else(|| Arc::clone(&doc_key)));
            match (&block.target, block.fence_text()) {
                (None, Some(text)) if block.kind == LiveKind::Base => {
                    let text: Arc<str> = text.into();
                    self.show_base(&block, text, this, root.clone(), cx);
                }
                (None, Some(text)) => self.run_query(&block, text, root.clone(), cx),
                (Some(target), _) => {
                    let res = resolver.resolve_embed(target, block.host.as_deref());
                    let building = self.index.as_ref().is_some_and(|i| !i.is_complete());
                    if res.status == Status::Pending && building {
                        // Asked again when the index is complete.
                        self.land(block.key, Shown::Pending, cx);
                        continue;
                    }
                    let found = res
                        .target_path
                        .as_ref()
                        .filter(|_| res.status.is_resolved())
                        .filter(|_| res.target_kind == Some(TargetKind::File));
                    let Some(found) = found else {
                        let notice = target_notice(target, res.status);
                        self.land(block.key, Shown::Notice(notice), cx);
                        continue;
                    };
                    let file = resolver.fs_path(found);
                    self.read_target(block, file, this, root.clone(), cx);
                }
                (None, None) => {}
            }
        }
    }

    /// Shows `shown` for block `key`.
    pub(super) fn land(&mut self, key: LiveKey, shown: Shown, cx: &mut Context<'_, Self>) {
        let mut st = self.state.borrow_mut();
        let items = st.cache.live.landed(key, shown);
        st.stale.extend(items);
        drop(st);
        cx.notify();
    }

    /// Reads (and, for a canvas, parses) an embed's file off the UI
    /// thread, then shows it.
    fn read_target(
        &mut self,
        block: Arc<LiveBlock>,
        file: PathBuf,
        this: Option<Arc<str>>,
        root: PathBuf,
        cx: &mut Context<'_, Self>,
    ) {
        let revision = self.state.borrow().revision;
        let kind = block.kind;
        cx.spawn(async move |reader, cx| {
            let read = cx
                .background_spawn(async move {
                    let text = read_file(&file)?;
                    let canvas = match kind {
                        LiveKind::Canvas => Some(crate::canvas_view::preview_doc(&text)?),
                        _ => None,
                    };
                    Ok::<_, String>((text, canvas))
                })
                .await;
            reader
                .update(cx, |reader, cx| {
                    if reader.state.borrow().revision != revision {
                        reader.state.borrow_mut().cache.live.unchanged(block.key);
                        return;
                    }
                    match read {
                        Ok((_, Some(canvas))) => {
                            reader.land(block.key, Shown::Canvas(canvas), cx);
                        }
                        Ok((text, None)) => {
                            reader.show_base(&block, text, this, root, cx);
                        }
                        Err(why) => reader.land(block.key, Shown::Notice(why.into()), cx),
                    }
                })
                .ok();
        })
        .detach();
    }

    /// Shows a base (a fence's text, or a `.base` file's) in a hosted
    /// Bases view; the one shown stays when the text did not change.
    fn show_base(
        &mut self,
        block: &Arc<LiveBlock>,
        text: Arc<str>,
        this: Option<Arc<str>>,
        root: PathBuf,
        cx: &mut Context<'_, Self>,
    ) {
        let key = block.key;
        let same = {
            let st = self.state.borrow();
            matches!(st.cache.live.get(key), Some(Shown::Base(_)))
                && st.cache.live.text(key).as_deref() == Some(&*text)
        };
        if same {
            self.state.borrow_mut().cache.live.unchanged(key);
            return;
        }
        let view = block.anchor.as_deref().map(str::to_owned);
        let base = BasesView::embedded(Arc::clone(&text), this, view, Some(root.clone()), cx);
        if let Some(content) = self.content.clone() {
            base.update(cx, |b, cx| b.set_content(content, cx));
        }
        let events = cx.subscribe(&base, move |reader, _, event: &BasesEvent, cx| {
            let request = match event {
                BasesEvent::OpenFile {
                    path, disposition, ..
                } => vault_path_of(path, &root).map(|p| LinkRequest {
                    destination: p.as_str().into(),
                    kind: gv_model::LinkKind::Wikilink,
                    disposition: *disposition,
                    range: None,
                    from: None,
                    at: None,
                }),
                BasesEvent::FollowLink(request) => Some(request.clone()),
            };
            if let Some(request) = request {
                reader.follow_live(request, cx);
            }
        });
        self.land(key, Shown::Base(base), cx);
        self.state
            .borrow_mut()
            .cache
            .live
            .keep(key, Some(events), Some(text));
    }

    /// Runs a query block's search over the content snapshot on the
    /// low-priority pool.
    fn run_query(
        &mut self,
        block: &Arc<LiveBlock>,
        text: &str,
        root: PathBuf,
        cx: &mut Context<'_, Self>,
    ) {
        let key = block.key;
        let query = match query_of(text) {
            Ok(q) => q,
            Err(why) => {
                self.land(key, Shown::Notice(why.into()), cx);
                return;
            }
        };
        let Some(content) = self.content.clone() else {
            let waiting = if self.content_given {
                "Search results show once the vault is indexed."
            } else {
                "Search results show in a vault, once it is indexed."
            };
            // Asked for again with the first snapshot.
            self.land(key, Shown::Notice(waiting.into()), cx);
            return;
        };
        let revision = self.state.borrow().revision;
        cx.spawn(async move |reader, cx| {
            let shown = cx
                .background_executor()
                .spawn_with_priority(gpui::Priority::Low, async move {
                    search(&query, content, &root, &CancelToken::new())
                })
                .await;
            reader
                .update(cx, |reader, cx| {
                    if reader.state.borrow().revision != revision {
                        reader.state.borrow_mut().cache.live.unchanged(key);
                        return;
                    }
                    reader.land(key, Shown::Query(Arc::new(shown)), cx);
                })
                .ok();
        })
        .detach();
    }

    /// Follows a link a live block asked for.
    pub(super) fn follow_live(&mut self, request: LinkRequest, cx: &mut Context<'_, Self>) {
        cx.emit(ReaderEvent::FollowLink(request));
    }
}

#[cfg(test)]
impl Reader {
    /// The generation of the content snapshot live blocks use, and
    /// whether the window gave it (tests).
    pub(crate) fn content_for_tests(&self) -> (Option<u64>, bool) {
        (
            self.content.as_ref().map(ContentSnapshot::generation),
            self.content_given,
        )
    }
}

/// How often the view checks the vault's content index for a new
/// snapshot while the window gives none (a window without a vault).
pub(crate) const CONTENT_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// The link a click on a live block (or on its result `row`: a note, and
/// one of its context lines) follows. A context line opens the note there.
/// A block inside a note embed follows its link from the embedded note.
pub(crate) fn open_request(
    block: &LiveBlock,
    shown: Option<&Shown>,
    row: Option<(usize, Option<usize>)>,
    disposition: Disposition,
) -> Option<LinkRequest> {
    if block.kind.is_media() {
        return match (row, shown) {
            (None, Some(shown)) => shown.card()?.request(disposition).map(|r| LinkRequest {
                from: block.host.clone(),
                ..r
            }),
            _ => None,
        };
    }
    let (destination, at): (Arc<str>, _) = match (row, shown) {
        (Some((i, line)), Some(Shown::Query(q))) => {
            let file = q.files.get(i)?;
            let at = line
                .and_then(|j| file.snippets.get(j))
                .map(|s| s.range.start);
            (file.path.as_str().into(), at)
        }
        (None, _) => (block.target.clone()?, None),
        _ => return None,
    };
    Some(LinkRequest {
        destination,
        kind: gv_model::LinkKind::Wikilink,
        disposition,
        range: None,
        from: block.host.clone(),
        at,
    })
}

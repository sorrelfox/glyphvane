// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live blocks in cards: what a text card (or a note file card's preview)
//! shows for its ` ```query ` and ` ```base ` fences, base embeds and
//! media embeds.
//!
//! # Filling in
//!
//! - A card looks a block's content up by its node and the block's key
//!   while it is drawn ([`LiveCards::shown`]); nothing is read or run on
//!   the UI thread. A block not asked for yet is asked for, and its
//!   placeholder says *Loading…* until the content lands.
//! - Only blocks drawn with readable text are asked for
//!   ([`MIN_TEXT_PX`]): zoomed out, a card keeps the placeholder and no
//!   search or base runs for it.
//! - The view fetches what was asked ([`CanvasView::fetch_live`]) on the
//!   low-priority pool, the way the Reading view does
//!   ([`crate::reader::embeds`]): an embed's target is resolved from the
//!   card's document (the canvas file for a text card, the note for a
//!   file card), a query runs over the vault's content snapshot
//!   ([`crate::reader::embeds::search`]), a base is compiled and evaluated
//!   by the Bases engine over the snapshot's files (built once per
//!   snapshot and shared by every base of the canvas), `this` being the
//!   card's document. A media file of the vault has its size read.
//! - A new content snapshot ([`CanvasView::set_content`]) asks every
//!   query and base again, and a new path index every embed; what is
//!   shown stays until the new content lands. Another canvas or vault
//!   starts afresh.
//!
//! # Drawing (not designed: built plainly, flagged)
//!
//! The placeholder's box with its header (the kind's icon and title, the
//! count on the right), then:
//!
//! - **Query**: a note per row with its first context line, at most
//!   [`CARD_QUERY_FILES`] notes and a line saying how many more there are.
//! - **Base**: the shown view as a small table: at most
//!   [`CARD_BASE_COLUMNS`] columns and [`CARD_BASE_ROWS`] rows, cells as
//!   text, and a line saying how many more rows there are.
//! - **Media**: the Reading view's media card (what the file or page is,
//!   and the action); nothing is played, drawn or loaded.
//!
//! A click on a note or a row opens it, a click on a media card follows
//! it, as in the Reading view; any other click selects the card.
//!
//! Embedded canvases keep their placeholder (a canvas inside a canvas
//! card is not drawn: no recursion).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui::{
    Bounds, Context, FontWeight, ParentElement as _, Pixels, SharedString, Styled as _, canvas,
    div, px,
};
use gv_bases::{CellValue, CompiledBase, EvalContext, Incomplete, IndexData, Value, ViewResult};
use gv_canvas::NodeKind;
use gv_render_tree::{LiveBlock, LiveKey, LiveKind};
use gv_search::CancelToken;
use gv_vault::content::ContentSnapshot;
use gv_vault::{PathIndex, Status, TargetKind, VaultPath};

use super::paint::Colors;
use super::{CanvasEvent, CanvasView, LABEL_EM};
use crate::bases_view::{BaseData, EVAL_BUDGET};
use crate::icons::Icon;
use crate::reader::embeds::{QueryShown, query_of, read_file, search, target_notice};
use crate::reader::media_card::{MediaCard, MediaSource, local_card, media_source, size_of};
use crate::reader::{Disposition, LinkRequest, Resolver};

/// Notes a query block lists in a card at most.
pub(crate) const CARD_QUERY_FILES: usize = 5;
/// Rows of a base a card lists at most.
pub(crate) const CARD_BASE_ROWS: usize = 6;
/// Columns of a base a card shows at most.
pub(crate) const CARD_BASE_COLUMNS: usize = 4;
/// The smallest body text, in screen px, at which a card shows a live
/// block's content; below it the placeholder stays and nothing is run.
pub(crate) const MIN_TEXT_PX: f32 = 6.0;

/// What a click on a live block's content opens.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Open {
    /// A file of the vault, as a file card opens its file.
    File(PathBuf),
    /// A link (a web address), followed as a link of the canvas file.
    Link(Arc<str>, gv_model::LinkKind),
}

/// One row of a base as a card lists it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BaseRow {
    /// Its vault path.
    pub path: Arc<str>,
    /// The text of its first [`CARD_BASE_COLUMNS`] cells.
    pub cells: Vec<String>,
}

/// A base's result as a card lists it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BaseShown {
    /// The headers of the columns shown.
    pub columns: Vec<String>,
    /// The first [`CARD_BASE_ROWS`] rows.
    pub rows: Vec<BaseRow>,
    /// The header's count ("12 results").
    pub count: String,
    /// Rows of the result not listed.
    pub more: usize,
    /// A layout Glyphvane does not draw, or an incomplete evaluation.
    pub notice: Option<String>,
}

impl BaseShown {
    pub fn new(result: &ViewResult) -> Self {
        let named = !result.columns.is_empty();
        let columns: Vec<String> = if named {
            result
                .columns
                .iter()
                .take(CARD_BASE_COLUMNS)
                .map(|c| c.label.clone())
                .collect()
        } else {
            vec!["Name".to_owned()]
        };
        let rows: Vec<BaseRow> = result
            .rows()
            .take(CARD_BASE_ROWS)
            .map(|r| BaseRow {
                path: Arc::clone(&r.path),
                cells: if named {
                    r.cells.iter().take(CARD_BASE_COLUMNS).map(cell).collect()
                } else {
                    vec![r.path.rsplit('/').next().unwrap_or(&r.path).to_owned()]
                },
            })
            .collect();
        let notice = match result.incomplete {
            Some(Incomplete::Cancelled) | None => result.notice.clone(),
            Some(_) => Some("The results are incomplete.".to_owned()),
        };
        Self {
            columns,
            more: result.shown.saturating_sub(rows.len()),
            rows,
            count: crate::bases_view::count_text(result),
            notice,
        }
    }
}

/// A cell's text in a card: the Bases view's, a check box as a tick.
fn cell(value: &CellValue) -> String {
    match value {
        Ok(Value::Bool(true)) => "\u{2713}".to_owned(),
        Ok(Value::Bool(false)) => String::new(),
        _ => crate::bases_view::cell_text(value),
    }
}

/// What a live block in a card shows now.
#[derive(Debug, Clone)]
pub(crate) enum Shown {
    /// Waiting for its content.
    Pending,
    Query(Arc<QueryShown>),
    Base(Arc<BaseShown>),
    /// A media card and what a click on it opens.
    Media(Arc<MediaCard>, Option<Open>),
    /// It cannot be shown; why.
    Notice(Arc<str>),
}

/// What the view knows of one block of one card.
#[derive(Debug)]
struct Entry {
    block: Arc<LiveBlock>,
    shown: Shown,
    /// The fetch running, if any.
    running: Option<u64>,
    /// Asked for again on its next draw.
    stale: bool,
    cancel: Arc<AtomicBool>,
}

/// A block asked for.
#[derive(Debug)]
pub(crate) struct Request {
    pub node: usize,
    pub block: Arc<LiveBlock>,
    pub ticket: u64,
    pub cancel: Arc<AtomicBool>,
}

/// The live blocks of the cards and the vault they are filled from.
#[derive(Default)]
pub(crate) struct LiveCards {
    entries: HashMap<(usize, LiveKey), Entry>,
    requests: Vec<Request>,
    tickets: u64,
    content: Option<ContentSnapshot>,
    index: Option<PathIndex>,
    /// The bases' view of `content`, built by the first base that needs it.
    data: Arc<OnceLock<BaseData>>,
}

impl std::fmt::Debug for LiveCards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveCards")
            .field("entries", &self.entries.len())
            .field("requests", &self.requests.len())
            .field(
                "content",
                &self.content.as_ref().map(ContentSnapshot::generation),
            )
            .finish_non_exhaustive()
    }
}

impl LiveCards {
    /// What `block` of card `node` shows; asks for it when it is not
    /// known yet or is stale and no fetch runs.
    pub fn shown(&mut self, node: usize, block: &Arc<LiveBlock>) -> Shown {
        let e = self
            .entries
            .entry((node, block.key))
            .or_insert_with(|| Entry {
                block: Arc::clone(block),
                shown: Shown::Pending,
                running: None,
                stale: true,
                cancel: Arc::new(AtomicBool::new(false)),
            });
        if e.stale && e.running.is_none() {
            self.tickets += 1;
            e.stale = false;
            e.running = Some(self.tickets);
            e.cancel = Arc::new(AtomicBool::new(false));
            self.requests.push(Request {
                node,
                block: Arc::clone(&e.block),
                ticket: self.tickets,
                cancel: Arc::clone(&e.cancel),
            });
        }
        e.shown.clone()
    }

    /// What block `key` of card `node` shows, without asking for it
    /// (tests).
    #[cfg(test)]
    pub fn get(&self, node: usize, key: LiveKey) -> Option<&Shown> {
        self.entries.get(&(node, key)).map(|e| &e.shown)
    }

    /// The blocks asked for since the last call.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// Fetch `ticket` of block `key` of card `node` landed with `shown`:
    /// whether it is still wanted (the view draws again).
    pub fn landed(&mut self, node: usize, key: LiveKey, ticket: u64, shown: Shown) -> bool {
        match self.entries.get_mut(&(node, key)) {
            Some(e) if e.running == Some(ticket) => {
                e.running = None;
                e.shown = shown;
                true
            }
            _ => false,
        }
    }

    /// Every block `which` keeps is asked for again on its next draw;
    /// what it shows stays meanwhile. Whether there were any.
    fn refresh(&mut self, which: impl Fn(&LiveBlock) -> bool) -> bool {
        let mut any = false;
        for e in self.entries.values_mut() {
            if which(&e.block) {
                e.stale = true;
                any = true;
            }
        }
        any
    }

    /// Forgets the cards' blocks (another canvas or vault) and stops their
    /// fetches; the vault's snapshot stays.
    pub fn clear(&mut self) {
        for e in self.entries.values() {
            e.cancel.store(true, Ordering::Relaxed);
        }
        self.entries.clear();
        self.requests.clear();
    }

    /// Uses `content` for queries and bases: when it is a new snapshot,
    /// they are asked for again. Whether anything is to be drawn again.
    pub fn set_content(&mut self, content: Option<ContentSnapshot>) -> bool {
        let generation = |s: &Option<ContentSnapshot>| {
            s.as_ref()
                .map(|s| (s.generation(), s.root().map(Path::to_path_buf)))
        };
        if generation(&content) == generation(&self.content) {
            return false;
        }
        self.content = content;
        self.data = Arc::new(OnceLock::new());
        self.refresh(|b| matches!(b.kind, LiveKind::Base | LiveKind::Query))
    }

    /// Resolves embeds through `index`: they are asked for again when it
    /// is a new one. Whether anything is to be drawn again.
    pub fn set_index(&mut self, index: Option<PathIndex>) -> bool {
        let same = match (&index, &self.index) {
            (Some(a), Some(b)) => {
                a.generation() == b.generation()
                    && a.root() == b.root()
                    && a.is_complete() == b.is_complete()
            }
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        self.index = index;
        self.refresh(|b| b.target.is_some())
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// What the blocks of card `node` show, by kind (tests).
    #[cfg(test)]
    pub fn of_node(&self, node: usize) -> Vec<(LiveKind, Shown)> {
        let mut out: Vec<_> = self
            .entries
            .iter()
            .filter(|((n, _), _)| *n == node)
            .map(|(_, e)| (e.block.kind, e.shown.clone()))
            .collect();
        out.sort_by_key(|(k, _)| k.name());
        out
    }
}

/// The document a card's live blocks belong to.
#[derive(Debug, Clone)]
enum Host {
    /// A text card: the canvas file.
    Canvas(Option<PathBuf>),
    /// A file card: the note at this vault path.
    Note(String),
}

/// What a fetch needs, shared with the low-priority pool.
#[derive(Clone)]
struct Env {
    root: PathBuf,
    index: Option<PathIndex>,
    content: Option<ContentSnapshot>,
    data: Arc<OnceLock<BaseData>>,
    utc_offset: i32,
}

/// The content of `block` in a card of `host` (blocking: the low-priority
/// pool).
fn fetch(block: &LiveBlock, host: &Host, env: &Env, cancel: &Arc<AtomicBool>) -> Shown {
    let doc = match host {
        Host::Canvas(Some(path)) => Some(path.clone()),
        Host::Canvas(None) => None,
        Host::Note(file) => super::cards::file_path(&env.root, file),
    };
    let Some(doc) = doc else {
        return Shown::Notice("The card's file is outside the vault.".into());
    };
    let resolver = Resolver::for_click(&doc, env.index.as_ref(), gv_vault::ProfileId::Obsidian);
    let building = env.index.as_ref().is_some_and(|i| !i.is_complete());
    if block.kind.is_media() {
        return match media_source(block, &resolver, building) {
            MediaSource::Web(card) => {
                let open = card.open.clone().map(|(d, k)| Open::Link(d, k));
                Shown::Media(Arc::new(card), open)
            }
            MediaSource::Notice(why) => Shown::Notice(why),
            MediaSource::Pending => Shown::Pending,
            MediaSource::File { path, name, open } => match size_of(&path) {
                Ok(size) => {
                    let card =
                        local_card(block.kind, &name, Some(size), block.anchor.as_deref(), open);
                    Shown::Media(Arc::new(card), Some(Open::File(path)))
                }
                Err(why) => Shown::Notice(why.into()),
            },
        };
    }
    let this: Arc<str> = resolver.source().as_str().into();
    let root = env
        .content
        .as_ref()
        .and_then(|c| c.root().map(Path::to_path_buf))
        .unwrap_or_else(|| resolver.root().to_path_buf());
    match (&block.target, block.fence_text(), block.kind) {
        (None, Some(text), LiveKind::Base) => base(text, None, this, env, cancel),
        (None, Some(text), LiveKind::Query) => {
            let query = match query_of(text) {
                Ok(q) => q,
                Err(why) => return Shown::Notice(why.into()),
            };
            let Some(content) = env.content.clone() else {
                return Shown::Notice("Search results show once the vault is indexed.".into());
            };
            // Stops with the block's other work (another canvas, the
            // card's block asked for again).
            let cancel = CancelToken::from_flag(Arc::clone(cancel));
            Shown::Query(Arc::new(search(&query, content, &root, &cancel)))
        }
        (Some(target), _, LiveKind::Base) => {
            let res = resolver.resolve_embed(target, block.host.as_deref());
            if res.status == Status::Pending && building {
                return Shown::Pending;
            }
            let found = res
                .target_path
                .as_ref()
                .filter(|_| res.status.is_resolved())
                .filter(|_| res.target_kind == Some(TargetKind::File));
            let Some(found) = found else {
                return Shown::Notice(target_notice(target, res.status));
            };
            match read_file(&resolver.fs_path(found)) {
                Ok(text) => base(&text, block.anchor.as_deref(), this, env, cancel),
                Err(why) => Shown::Notice(why.into()),
            }
        }
        _ => Shown::Notice("Open the canvas's file to see it.".into()),
    }
}

/// The result of the base `text` (view `view`, else its first) with
/// `this` as the card's document.
fn base(
    text: &str,
    view: Option<&str>,
    this: Arc<str>,
    env: &Env,
    cancel: &Arc<AtomicBool>,
) -> Shown {
    let compiled = match CompiledBase::parse(text) {
        Ok(b) => b,
        Err(e) => return Shown::Notice(e.to_string().into()),
    };
    let Some(content) = env.content.clone() else {
        return Shown::Notice("Base results show once the vault is indexed.".into());
    };
    let view = view
        .and_then(|v| compiled.base().view_index(v))
        .unwrap_or(0);
    let offset = env.utc_offset;
    let data = env
        .data
        .get_or_init(|| Arc::new(IndexData::new(content, offset)) as BaseData);
    let mut ctx = EvalContext::now(offset);
    ctx.this = Some(this);
    ctx.cancel = Some(Arc::clone(cancel));
    ctx.deadline = Some(Instant::now() + EVAL_BUDGET);
    let result = compiled.evaluate(view, &**data, &ctx);
    Shown::Base(Arc::new(BaseShown::new(&result)))
}

/// Where a live block's clickable part was drawn, from the last frame.
#[derive(Debug, Clone)]
pub(crate) struct Hit {
    pub bounds: Bounds<Pixels>,
    pub node: usize,
    pub open: Open,
}

/// The clickable parts drawn.
pub(crate) type Hits = Rc<RefCell<Vec<Hit>>>;

/// `element` recording its bounds as a part of card `node` that opens
/// `open` when it is painted.
fn clickable(element: gpui::Div, hits: &Hits, node: usize, open: Option<Open>) -> gpui::Div {
    let Some(open) = open else {
        return element;
    };
    let hits = Rc::clone(hits);
    element.relative().child(
        canvas(
            move |bounds, _, _| hits.borrow_mut().push(Hit { bounds, node, open }),
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full(),
    )
}

/// The look of a live block's box.
pub(super) struct Look<'a> {
    pub icon: Icon,
    pub title: &'a SharedString,
    pub note: &'a SharedString,
    /// Body text size, screen px.
    pub size: f32,
    pub pad: f32,
    pub colors: &'a Colors,
}

/// A live block's box in card `node`: its placeholder (`shown` is `None`)
/// or what it shows. Clickable parts are recorded in `hits`; `root` is the
/// vault the rows' paths are in.
pub(super) fn live_box(
    look: &Look<'_>,
    shown: Option<&Shown>,
    node: usize,
    root: &Path,
    hits: &Hits,
) -> gpui::Div {
    let Look {
        icon,
        title,
        note,
        size,
        pad,
        colors,
    } = *look;
    let small = size * LABEL_EM;
    let line = |text: String, color| {
        div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_color(color)
            .child(text)
    };
    let header = |count: Option<String>| {
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(size * 0.4))
            .text_color(colors.text)
            .child(
                div()
                    .flex_none()
                    .text_color(colors.label)
                    .child(icon.sized(size)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(title.clone()),
            );
        if let Some(count) = count {
            row = row.child(
                div()
                    .flex_none()
                    .text_size(px(small))
                    .text_color(colors.muted)
                    .child(count),
            );
        }
        row
    };
    let open_path = |path: &str| {
        VaultPath::new(path)
            .ok()
            .map(|p| Open::File(p.to_fs_path(root)))
    };
    let mut boxed = div()
        .flex_none()
        .p(px(pad))
        .flex()
        .flex_col()
        .gap(px(size * 0.3))
        .rounded(px(size * 0.3))
        .border_1()
        .border_color(colors.card_border)
        .text_size(px(size));
    match shown {
        None => {
            boxed = boxed.child(header(None)).child(
                div()
                    .text_size(px(small))
                    .text_color(colors.muted)
                    .child(note.clone()),
            );
        }
        Some(Shown::Pending) => {
            boxed = boxed
                .child(header(None))
                .child(line("Loading\u{2026}".to_owned(), colors.muted).text_size(px(small)));
        }
        Some(Shown::Notice(why)) => {
            boxed = boxed
                .child(header(None))
                .child(line(why.to_string(), colors.muted).text_size(px(small)));
        }
        Some(Shown::Query(q)) => {
            boxed = boxed.child(header(Some(q.count())));
            for f in q.files.iter().take(CARD_QUERY_FILES) {
                let mut row = div()
                    .flex()
                    .flex_col()
                    .child(line(f.title.to_string(), colors.accent));
                if let Some(snip) = f.snippets.first() {
                    let mut text = String::new();
                    if snip.clipped_start {
                        text.push('\u{2026}');
                    }
                    text.push_str(snip.text.trim());
                    row = row.child(line(text, colors.muted).text_size(px(small)).pl(px(size)));
                }
                boxed = boxed.child(clickable(row, hits, node, open_path(f.path.as_str())));
            }
            let listed = q.files.len().min(CARD_QUERY_FILES);
            if q.matched > listed {
                let n = q.matched - listed;
                let what = if n == 1 { "note" } else { "notes" };
                boxed = boxed.child(line(
                    format!("and {} more {what}", crate::search_panel::grouped_count(n)),
                    colors.muted,
                ));
            } else if q.files.is_empty() {
                boxed = boxed.child(line("No results".to_owned(), colors.muted));
            }
        }
        Some(Shown::Base(b)) => {
            boxed = boxed.child(header(Some(b.count.clone())));
            if let Some(notice) = &b.notice {
                boxed = boxed.child(line(notice.clone(), colors.muted).text_size(px(small)));
            }
            let row_of = |cells: Vec<gpui::Div>| {
                div()
                    .flex()
                    .gap(px(size * 0.5))
                    .children(cells.into_iter().map(|c| c.flex_1().flex_basis(px(0.0))))
            };
            boxed = boxed.child(
                row_of(
                    b.columns
                        .iter()
                        .map(|c| line(c.clone(), colors.label).font_weight(FontWeight::MEDIUM))
                        .collect(),
                )
                .text_size(px(small))
                .pb(px(size * 0.2))
                .border_b_1()
                .border_color(colors.card_border),
            );
            for r in &b.rows {
                let cells = r
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let color = if i == 0 { colors.accent } else { colors.text };
                        line(c.clone(), color)
                    })
                    .collect();
                boxed = boxed.child(clickable(row_of(cells), hits, node, open_path(&r.path)));
            }
            if b.more > 0 {
                let what = if b.more == 1 { "row" } else { "rows" };
                boxed = boxed.child(line(
                    format!(
                        "and {} more {what}",
                        crate::search_panel::grouped_count(b.more)
                    ),
                    colors.muted,
                ));
            } else if b.rows.is_empty() {
                boxed = boxed.child(line("No results".to_owned(), colors.muted));
            }
        }
        Some(Shown::Media(card, open)) => {
            let head = div()
                .flex()
                .items_center()
                .gap(px(size * 0.4))
                .text_color(colors.text)
                .child(
                    div()
                        .flex_none()
                        .text_color(colors.label)
                        .child(icon.sized(size)),
                )
                .child(line(card.title.clone(), colors.text).flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_size(px(small))
                        .text_color(colors.accent)
                        .child(card.action),
                );
            boxed = clickable(
                boxed
                    .child(head)
                    .child(line(card.detail.clone(), colors.muted).text_size(px(small))),
                hits,
                node,
                open.clone(),
            );
        }
    }
    boxed
}

impl CanvasView {
    /// The vault content snapshot live blocks use (bases list its files,
    /// queries search it). The window gives each new one; a new one
    /// refreshes the cards' queries and bases.
    pub fn set_content(&mut self, content: Option<ContentSnapshot>, cx: &mut Context<'_, Self>) {
        if self.cards.live.set_content(content) {
            cx.notify();
        }
    }

    /// The vault's path index, which live embeds resolve through; a new
    /// one refreshes them.
    pub fn set_index(&mut self, index: Option<PathIndex>, cx: &mut Context<'_, Self>) {
        if self.cards.live.set_index(index) {
            cx.notify();
        }
    }

    /// Fetches the live blocks the cards asked for while they were drawn,
    /// on the low-priority pool.
    pub(super) fn fetch_live(&mut self, cx: &mut Context<'_, Self>) {
        let requests = self.cards.live.take_requests();
        if requests.is_empty() {
            return;
        }
        let Some(doc) = self.doc.clone() else {
            return;
        };
        // Without a snapshot from the window: the content index of the
        // vault that holds the canvas, when one is open.
        if self.cards.live.content.is_none()
            && let Some((_, content)) = self
                .path
                .as_deref()
                .and_then(crate::files::overlay::content_of)
        {
            self.cards.live.set_content(Some(content));
        }
        let env = Env {
            root: self.root(),
            index: self.cards.live.index.clone(),
            content: self.cards.live.content.clone(),
            data: Arc::clone(&self.cards.live.data),
            utc_offset: chrono::Local::now().offset().local_minus_utc() / 60,
        };
        for request in requests {
            let host = match doc.canvas.nodes().get(request.node).map(|n| &n.kind) {
                Some(NodeKind::File { file, .. }) => Host::Note(file.clone()),
                _ => Host::Canvas(self.path.clone()),
            };
            let env = env.clone();
            let Request {
                node,
                block,
                ticket,
                cancel,
            } = request;
            let key = block.key;
            cx.spawn(async move |this, cx| {
                let shown = cx
                    .background_executor()
                    .spawn_with_priority(gpui::Priority::Low, async move {
                        if cancel.load(Ordering::Relaxed) {
                            return None;
                        }
                        Some(fetch(&block, &host, &env, &cancel))
                    })
                    .await;
                let Some(shown) = shown else {
                    return;
                };
                this.update(cx, |view, cx| {
                    if view.cards.live.landed(node, key, ticket, shown) {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// Opens what a live block's part opens.
    pub(super) fn open_live(
        &mut self,
        open: &Open,
        disposition: Disposition,
        cx: &mut Context<'_, Self>,
    ) {
        match open {
            Open::File(path) => cx.emit(CanvasEvent::OpenFile {
                path: path.clone(),
                fragment: None,
                disposition,
            }),
            Open::Link(destination, kind) => cx.emit(CanvasEvent::FollowLink(LinkRequest {
                destination: Arc::clone(destination),
                kind: *kind,
                disposition,
                range: None,
                from: None,
                at: None,
            })),
        }
    }

    /// The live part of card `node` at window position `at`, from the
    /// last frame (the topmost).
    pub(super) fn live_hit(&self, node: usize, at: gpui::Point<Pixels>) -> Option<Open> {
        self.live_hits
            .borrow()
            .iter()
            .rev()
            .find(|h| h.node == node && h.bounds.contains(&at))
            .map(|h| h.open.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_bases::{Column, Group, PropertyId, Row, ViewKind};

    fn result(rows: usize, columns: usize) -> ViewResult {
        ViewResult {
            view: 0,
            name: None,
            kind: ViewKind::Table,
            notice: None,
            columns: (0..columns)
                .map(|i| Column {
                    property: PropertyId::Note(format!("p{i}")),
                    label: format!("P{i}"),
                    width: None,
                })
                .collect(),
            grouped: false,
            groups: vec![Group {
                key: Value::Null,
                rows: (0..rows)
                    .map(|r| Row {
                        path: format!("n/{r}.md").into(),
                        cells: (0..columns)
                            .map(|c| {
                                if c == 1 {
                                    Ok(Value::Bool(r % 2 == 0))
                                } else {
                                    Ok(Value::String(format!("r{r}c{c}").into()))
                                }
                            })
                            .collect(),
                        cover: None,
                    })
                    .collect(),
                summaries: Vec::new(),
            }],
            summaries: Vec::new(),
            matched: rows,
            shown: rows,
            incomplete: None,
            diagnostics: Vec::new(),
            cards: None,
        }
    }

    #[test]
    fn bases_are_capped_in_rows_and_columns() {
        let b = BaseShown::new(&result(10, 6));
        assert_eq!(b.columns, ["P0", "P1", "P2", "P3"]);
        assert_eq!(b.rows.len(), CARD_BASE_ROWS);
        assert_eq!(b.more, 10 - CARD_BASE_ROWS);
        assert_eq!(b.count, "10 results");
        assert_eq!(b.rows[0].path.as_ref(), "n/0.md");
        assert_eq!(b.rows[0].cells, ["r0c0", "\u{2713}", "r0c2", "r0c3"]);
        assert_eq!(b.rows[1].cells[1], "");
        let few = BaseShown::new(&result(2, 0));
        assert_eq!(few.columns, ["Name"]);
        assert_eq!(few.rows[1].cells, ["1.md"]);
        assert_eq!(few.more, 0);
    }

    #[test]
    fn blocks_are_asked_for_once_per_card_and_late_fetches_are_dropped() {
        let block = super::super::cards::live_blocks_of("```query\nfoo\n```\n")
            .pop()
            .expect("the query block");
        let mut live = LiveCards::default();
        assert!(matches!(live.shown(0, &block), Shown::Pending));
        assert!(matches!(live.shown(0, &block), Shown::Pending));
        let asked = live.take_requests();
        assert_eq!(asked.len(), 1, "once");
        // Another card's block is its own.
        live.shown(1, &block);
        assert_eq!(live.take_requests().len(), 1);
        // An old fetch is ignored; the one asked for lands.
        assert!(!live.landed(0, block.key, asked[0].ticket + 99, Shown::Pending));
        assert!(live.landed(0, block.key, asked[0].ticket, Shown::Notice("x".into())));
        assert!(matches!(live.get(0, block.key), Some(Shown::Notice(_))));
        // Nothing new: nothing asked; an index does not refresh fences.
        live.shown(0, &block);
        assert!(live.take_requests().is_empty());
        assert!(!live.set_index(None));
        // Clearing forgets the blocks and stops the fetches that run, a
        // query's search too (its token shares the block's flag).
        let search = CancelToken::from_flag(Arc::clone(&asked[0].cancel));
        live.clear();
        assert_eq!(live.len(), 0);
        assert!(asked[0].cancel.load(Ordering::Relaxed));
        assert!(search.is_cancelled());
        assert!(!live.landed(1, block.key, asked[0].ticket + 1, Shown::Pending));
    }
}

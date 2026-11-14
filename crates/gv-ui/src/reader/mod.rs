// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view: a document rendered for reading (Preview mode).
//!
//! # Pipeline
//!
//! 1. [`Reader::open`] reads, decodes and parses the file, builds its model
//!    and its render tree on the background executor ([`load`]). Link
//!    statuses come from the window's path index when the window has a
//!    vault, else only URLs are classified and local links are pending.
//! 2. The view keeps the render tree's virtual items in an own virtual list
//!    ([`engine`]): a height index over estimates that become measured
//!    heights as items are laid out, and a scroll anchor `(item id, px)`
//!    that keeps the content still while heights change and the window is
//!    resized.
//! 3. One element ([`element`]) covers the viewport. It lays out only the
//!    visible items (paragraphs through `gv-inline`, code chunks as
//!    paragraphs of their lines), hosts an
//!    [`InlineParagraph`](crate::inline::InlineParagraph) per text item, and
//!    draws code block frames, quote bars, list markers, rules and the
//!    scrollbar itself.
//!
//! # Interaction
//!
//! - Selection in source offsets by dragging (Shift extends); Mod+C copies
//!   the plain text of the selection ([`RenderTree::plain_text`]); Mod+A
//!   selects everything. No caret is shown.
//! - A click on a link emits [`ReaderEvent::FollowLink`]; Mod-click asks for
//!   a new tab and a middle click for a background tab. The window decides
//!   what to do ([`navigation`]).
//! - Wheel scrolling (sideways in code blocks that do not wrap), the
//!   scrollbar thumb, and the keys of [`key_bindings`].
//! - A code block's head copies the block and toggles its soft wrap.

mod bench;
mod element;
mod engine;
mod heights;
mod items;
mod links;
mod load;
mod state;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use gpui::{
    App, AppContext as _, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};
use gv_render_tree::{CopyOptions, RenderOptions, StyleTable};
use gv_vault::{Fragment, PathIndex, ProfileParams};

use crate::appearance::Appearance;
use crate::inline::{FontFamilies, InlineText, Palette};
use crate::metrics::size::document;
use crate::metrics::spacing::document as spacing;
use crate::metrics::text;
use crate::theme::Theme;
use bench::Bench;
use element::ReaderElement;
use links::{PROFILE, Resolver};
use load::Loaded;
use state::{Selection, State};

pub use bench::DEBUG_READER_BENCH_ENV;
pub(crate) use links::{Navigation, navigation};

gpui::actions!(
    reader,
    [
        /// Copy the selection as plain text.
        Copy,
        /// Select the whole document.
        SelectAll,
        /// Scroll up by a line.
        LineUp,
        /// Scroll down by a line.
        LineDown,
        /// Scroll up by a page.
        PageUp,
        /// Scroll down by a page.
        PageDown,
        /// Scroll to the top.
        ScrollToTop,
        /// Scroll to the end.
        ScrollToBottom,
    ]
);

/// The key context of the Reading view.
const KEY_CONTEXT: &str = "Reader";

/// The Reading view's keys. They apply while it has the focus.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let context = Some(KEY_CONTEXT);
    let mut keys = vec![
        KeyBinding::new("up", LineUp, context),
        KeyBinding::new("down", LineDown, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("pagedown", PageDown, context),
        KeyBinding::new("shift-space", PageUp, context),
        KeyBinding::new("space", PageDown, context),
        KeyBinding::new("home", ScrollToTop, context),
        KeyBinding::new("end", ScrollToBottom, context),
    ];
    if cfg!(target_os = "macos") {
        keys.extend([
            KeyBinding::new("cmd-c", Copy, context),
            KeyBinding::new("cmd-a", SelectAll, context),
            KeyBinding::new("cmd-up", ScrollToTop, context),
            KeyBinding::new("cmd-down", ScrollToBottom, context),
        ]);
    } else {
        keys.extend([
            KeyBinding::new("ctrl-c", Copy, context),
            KeyBinding::new("ctrl-a", SelectAll, context),
            KeyBinding::new("ctrl-home", ScrollToTop, context),
            KeyBinding::new("ctrl-end", ScrollToBottom, context),
        ]);
    }
    keys
}

/// Where a followed link opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// In the same tab (a click).
    InPlace,
    /// In a new tab that becomes active (Mod-click).
    NewTab,
    /// In a new tab behind the active one (middle click).
    Background,
}

/// A link the user followed.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRequest {
    /// The destination as the resolver takes it.
    pub destination: Arc<str>,
    pub kind: gv_model::LinkKind,
    pub disposition: Disposition,
}

/// What the Reading view asks of its window.
#[derive(Debug, Clone, PartialEq)]
pub enum ReaderEvent {
    FollowLink(LinkRequest),
}

/// Where to scroll a document.
#[derive(Debug, Clone, PartialEq)]
pub enum ScrollTarget {
    /// A link's fragment: a heading, a block id, lines.
    Fragment(Fragment),
    /// A 1-based line (`PATH:LINE`).
    Line(u32),
    /// The block holding this source offset, at the top (history).
    Offset(usize),
}

/// Where the view is with its document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    /// Nothing asked for yet.
    Empty,
    Loading,
    Ready,
    /// The file could not be shown; why.
    Failed(SharedString),
}

/// One benchmark per process: the first reader that is created takes it.
static BENCH_TAKEN: AtomicBool = AtomicBool::new(false);

/// The Reading view of one tab.
pub struct Reader {
    focus: FocusHandle,
    state: Rc<RefCell<State>>,
    path: Option<PathBuf>,
    status: LoadStatus,
    /// The window's path index, once the window has a vault.
    index: Option<PathIndex>,
    /// The index snapshot (generation, complete) the shown statuses were
    /// resolved with; `None` without an index.
    statuses_from: Option<(u64, bool)>,
    /// Where to scroll once the document is loaded.
    target: Option<ScrollTarget>,
    _load: Option<Task<()>>,
    _refresh: Option<Task<()>>,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("path", &self.path)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

fn index_key(index: Option<&PathIndex>) -> Option<(u64, bool)> {
    index.map(|i| (i.generation(), i.is_complete()))
}

impl Reader {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        let families = FontFamilies::from_app(cx);
        let fonts = items::font_table(&StyleTable::preview(), &families);
        let mut state = State::new(InlineText::new(fonts));
        if let Some(bench) = Bench::from_env()
            && !BENCH_TAKEN.swap(true, Ordering::Relaxed)
        {
            state.bench = Some(bench);
        }
        Self {
            focus: cx.focus_handle(),
            state: Rc::new(RefCell::new(state)),
            path: None,
            status: LoadStatus::Empty,
            index: None,
            statuses_from: None,
            target: None,
            _load: None,
            _refresh: None,
        }
    }

    /// The file shown (or being loaded).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn status(&self) -> &LoadStatus {
        &self.status
    }

    /// Loads `path` and shows it, scrolled to `target` when given.
    pub fn open(
        &mut self,
        path: PathBuf,
        target: Option<ScrollTarget>,
        cx: &mut Context<'_, Self>,
    ) {
        let revision = {
            let mut st = self.state.borrow_mut();
            st.revision += 1;
            st.timing.opened = Some(Instant::now());
            st.timing.first_content = None;
            st.revision
        };
        self.path = Some(path.clone());
        self.status = LoadStatus::Loading;
        self.target = target;
        self._refresh = None;
        let resolver = Resolver::statuses(&path, self.index.as_ref());
        self.statuses_from = index_key(self.index.as_ref());
        let options = RenderOptions::commonmark();
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { load::load(&path, &resolver, options) })
                .await;
            this.update(cx, |reader, cx| reader.finish_load(revision, result, cx))
                .ok();
        }));
        cx.notify();
    }

    fn finish_load(
        &mut self,
        revision: u64,
        result: Result<Loaded, String>,
        cx: &mut Context<'_, Self>,
    ) {
        if self.state.borrow().revision != revision {
            return;
        }
        self._load = None;
        match result {
            Ok(doc) => {
                if let Some(why) = &doc.lossy {
                    tracing::warn!(path = %doc.path.display(), "{why}");
                }
                tracing::debug!(
                    path = %doc.path.display(),
                    items = doc.tree.items().len(),
                    "loaded in {:.1} ms",
                    doc.timings.total().as_secs_f64() * 1e3
                );
                let target = self.target.take();
                let mut st = self.state.borrow_mut();
                st.install(doc);
                if let Some(target) = target {
                    jump(&mut st, &target);
                }
                drop(st);
                self.status = LoadStatus::Ready;
                // The index may have changed while the document loaded.
                if index_key(self.index.as_ref()) != self.statuses_from {
                    self.refresh_statuses(cx);
                }
            }
            Err(err) => {
                let name = self
                    .path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                tracing::warn!("cannot open {name}: {err}");
                self.state.borrow_mut().doc = None;
                self.status = LoadStatus::Failed(err.into());
            }
        }
        cx.notify();
    }

    /// The window's path index changed (it was created, finished building
    /// or was updated): link statuses are resolved again.
    pub fn set_index(&mut self, index: Option<PathIndex>, cx: &mut Context<'_, Self>) {
        let changed = index_key(index.as_ref()) != index_key(self.index.as_ref());
        self.index = index;
        if changed && self.status == LoadStatus::Ready {
            self.refresh_statuses(cx);
        }
    }

    fn refresh_statuses(&mut self, cx: &mut Context<'_, Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (doc, revision) = {
            let st = self.state.borrow();
            let Some(doc) = st.doc.clone() else {
                return;
            };
            (doc, st.revision)
        };
        let resolver = Resolver::statuses(&path, self.index.as_ref());
        self.statuses_from = index_key(self.index.as_ref());
        self._refresh = Some(cx.spawn(async move |this, cx| {
            let (doc, changed) = cx
                .background_spawn(async move {
                    let mut doc = doc;
                    let changed = load::refresh(&mut doc, &resolver);
                    (doc, changed)
                })
                .await;
            this.update(cx, |reader, cx| {
                let mut st = reader.state.borrow_mut();
                if st.revision != revision || changed.is_empty() {
                    return;
                }
                if let Some(current) = &mut st.doc {
                    current.tree = doc.tree;
                }
                st.stale.extend(changed);
                drop(st);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Scrolls the shown document to `target`.
    pub fn scroll_to(&mut self, target: ScrollTarget, cx: &mut Context<'_, Self>) {
        if self.status != LoadStatus::Ready {
            self.target = Some(target);
            return;
        }
        jump(&mut self.state.borrow_mut(), &target);
        cx.notify();
    }

    /// Where the view is scrolled, for history: the start of the block at
    /// the top.
    pub fn position(&self) -> Option<ScrollTarget> {
        let st = self.state.borrow();
        let doc = st.doc.as_ref()?;
        let anchor = st.engine.anchor()?;
        let ix = doc.tree.index_of(anchor.id)?;
        Some(ScrollTarget::Offset(doc.tree.items()[ix].source.start))
    }

    /// The selection as plain text.
    pub fn selected_text(&self) -> Option<String> {
        let st = self.state.borrow();
        let range = st.selected()?;
        Some(
            st.doc
                .as_ref()?
                .tree
                .plain_text(range, CopyOptions::default()),
        )
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<'_, Self>) {
        let mut st = self.state.borrow_mut();
        let Some(len) = st.doc.as_ref().map(|d| d.tree.len()) else {
            return;
        };
        st.selection = Some(Selection {
            anchor: 0,
            head: len,
        });
        drop(st);
        cx.notify();
    }

    fn scroll(&mut self, dy: impl Fn(f32) -> f32, cx: &mut Context<'_, Self>) {
        let mut st = self.state.borrow_mut();
        let viewport = st.engine.viewport();
        st.engine.scroll_by(dy(viewport));
        drop(st);
        cx.notify();
    }
}

/// Scrolls `st` to `target` in its loaded document.
fn jump(st: &mut State, target: &ScrollTarget) {
    let Some(doc) = &st.doc else {
        return;
    };
    let last = doc.tree.items().len().checked_sub(1);
    let ix = match target {
        ScrollTarget::Fragment(fragment) => {
            let params = ProfileParams::for_profile(PROFILE);
            let ix = links::fragment_item(fragment, &params, &doc.model, &doc.tree, &doc.text);
            if ix.is_none() {
                tracing::info!(
                    path = %doc.path.display(),
                    "fragment {:?} not found; showing the top",
                    fragment.raw
                );
            }
            ix
        }
        ScrollTarget::Line(line) => doc
            .tree
            .item_at(links::line_offset(&doc.text, line.saturating_sub(1)))
            .or(last),
        ScrollTarget::Offset(offset) => doc.tree.item_at(*offset).or(last),
    };
    let Some(ix) = ix else {
        return;
    };
    let gap = doc.tree.items()[ix].gap_before;
    match target {
        ScrollTarget::Offset(_) => st.engine.jump_to(ix, gap),
        _ => st.engine.jump_near_top(ix, gap),
    }
}

impl Focusable for Reader {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<ReaderEvent> for Reader {}

/// A message in the text column (a file that cannot be shown).
fn notice(title: SharedString, message: SharedString, theme: &Theme) -> impl IntoElement {
    let column_width = document::COLUMN_MAX + 2.0 * document::PAD_X;
    div().size_full().flex().justify_center().child(
        div()
            .w_full()
            .max_w(px(column_width))
            .px(px(document::PAD_X))
            .pt(px(document::PAD_TOP))
            .flex()
            .flex_col()
            .child(
                div()
                    .mb(px(spacing::HEADING_GAP))
                    .text_size(px(text::PREVIEW_H1.size))
                    .line_height(px(text::PREVIEW_H1.line))
                    .font_weight(FontWeight(text::weight::BOLD))
                    .text_color(theme.text.primary)
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(text::PREVIEW_BODY.size))
                    .line_height(px(text::PREVIEW_BODY.line))
                    .text_color(theme.text.secondary)
                    .child(message),
            ),
    )
}

impl Render for Reader {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        self.state.borrow_mut().timing.frame_start = Some(Instant::now());
        let line = text::PREVIEW_BODY.line;
        let root = div()
            .id("reader")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(move |this, _: &LineUp, _, cx| this.scroll(|_| -line, cx)))
            .on_action(cx.listener(move |this, _: &LineDown, _, cx| this.scroll(|_| line, cx)))
            .on_action(cx.listener(move |this, _: &PageUp, _, cx| {
                this.scroll(|v| -(v - line).max(line), cx);
            }))
            .on_action(cx.listener(move |this, _: &PageDown, _, cx| {
                this.scroll(|v| (v - line).max(line), cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollToTop, _, cx| {
                this.state.borrow_mut().engine.scroll_to_y(0.0);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| {
                this.state.borrow_mut().engine.scroll_to_end();
                cx.notify();
            }))
            .size_full()
            .bg(theme.surface.page);
        match &self.status {
            LoadStatus::Ready => root.child(ReaderElement::new(
                self.state.clone(),
                cx.weak_entity(),
                self.focus.clone(),
                Palette::of(Appearance::current_id(cx)),
            )),
            LoadStatus::Failed(err) => {
                let name: SharedString = self
                    .path
                    .as_deref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into();
                root.child(notice(
                    name,
                    format!("Cannot open this file: {err}").into(),
                    theme,
                ))
            }
            LoadStatus::Loading | LoadStatus::Empty => root,
        }
    }
}

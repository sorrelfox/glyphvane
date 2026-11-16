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
//!   the plain text of the selection ([`RenderTree::plain_text`]), Copy as
//!   Markdown (a command without a default key) its source bytes
//!   ([`RenderTree::markdown_for`]); Mod+A selects everything. No caret is
//!   shown.
//! - A click on a link emits [`ReaderEvent::FollowLink`]; Mod-click asks for
//!   a new tab and a middle click for a background tab. The window decides
//!   what to do ([`navigation`]).
//! - Wheel scrolling (sideways in code blocks that do not wrap), the
//!   scrollbar thumb, and the keys of [`key_bindings`].
//! - A code block's head copies the block and toggles its soft wrap.
//! - Find in page ([`find`]): Mod+F opens the find popover at the top right
//!   of the document area; matches are highlighted and the current one is
//!   scrolled into view.
//!
//! # Following an editor
//!
//! The Split view's preview pane is a Reading view made with
//! [`Reader::for_split`] (padding `20 28 0`, the split type scale) that
//! follows the editor's text ([`live`]): the model and the render tree are
//! updated incrementally from the editor's reparses, and scroll sync reads
//! and sets the top of the view as a place in an item.

mod bench;
pub(crate) mod code_highlight;
pub(crate) mod diagram;
mod edited;
mod element;
mod engine;
mod find;
pub(crate) mod follow;
mod heights;
mod items;
mod links;
mod live;
mod load;
pub(crate) mod media;
pub(crate) mod remote;
pub mod startup;
mod state;
#[cfg(test)]
mod tests;
mod typography;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use gpui::{
    App, AppContext as _, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, px,
};
use gv_profile::{CompiledProfile, ProfileId};
use gv_render_tree::{CopyOptions, RenderOptions, StyleTable};
use gv_vault::{Fragment, PathIndex, ProfileParams};

use crate::appearance::Appearance;
use crate::inline::{InlineText, Palette};
use crate::metrics::size::document;
use crate::metrics::spacing::document as spacing;
use crate::metrics::spacing::find as find_spacing;
use crate::metrics::text;
use crate::settings::AppSettings;
use crate::theme::Theme;
use bench::Bench;
use element::ReaderElement;
use load::Loaded;
use state::{Selection, State};
use typography::Typography;

pub(crate) use live::{Spot, SyncRequest};
pub use state::ReaderLayout;

pub use bench::DEBUG_READER_BENCH_ENV;
pub use find::DEBUG_FIND_ENV;
pub(crate) use links::{Navigation, Resolver, navigation};

/// Copy the selection as plain text (`editor.copy`) or as Markdown
/// (`editor.copy_as_markdown`), and select the whole document
/// (`editor.select_all`): commands of the keymap.
pub use crate::keymap::actions::editor::{Copy, CopyAsMarkdown, SelectAll};

gpui::actions!(
    reader,
    [
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
pub(crate) const KEY_CONTEXT: &str = "Reader";

/// The Reading view's own scrolling keys. They apply while it has the
/// focus. Copy, select all and find are commands, bound by the keymap
/// ([`crate::keymap`]).
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
            KeyBinding::new("cmd-up", ScrollToTop, context),
            KeyBinding::new("cmd-down", ScrollToBottom, context),
        ]);
    } else {
        keys.extend([
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
    /// A task checkbox was clicked: the document's owner writes the
    /// toggle back (one undoable edit) unless the document is read-only.
    ToggleTask(TaskToggle),
}

/// A task item's status character to replace: `from` at byte `offset` of
/// the shown text becomes `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskToggle {
    pub offset: usize,
    pub from: char,
    pub to: char,
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
    /// Find in page.
    find: find::FindState,
    /// What documents are built with: the type scale and the metrics.
    options: RenderOptions,
    /// The tab's profile: what documents are parsed, rendered and resolved
    /// with.
    profile: Arc<CompiledProfile>,
    /// Following an editor's text (the Split view's preview).
    live: live::Live,
    /// The Appearance settings documents are shown with (zoom, fonts,
    /// readable width).
    typography: Typography,
    _load: Option<Task<()>>,
    _refresh: Option<Task<()>>,
    /// Remote images: the fetches, failures and sites waiting.
    remote: remote::RemoteImages,
    _settings: Subscription,
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
        let typography = Typography::from_app(cx);
        let families = typography.families(&cx.text_system().all_font_names());
        let fonts = items::font_table(&StyleTable::preview(), &families);
        let mut state = State::new(InlineText::new(fonts));
        state.column_max = typography.column_max();
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
            find: find::FindState::default(),
            options: RenderOptions::commonmark(),
            profile: Arc::new(gv_profile::compile_builtin(ProfileId::CommonMark)),
            live: live::Live::default(),
            typography,
            _load: None,
            _refresh: None,
            remote: remote::RemoteImages::default(),
            _settings: cx.observe_global::<AppSettings>(|this, cx| {
                this.sync_typography(cx);
                this.sync_remote(cx);
            }),
        }
    }

    /// The options documents are built with: the type scale and metrics
    /// at the zoom and preview font size in effect.
    fn build_options(&self) -> RenderOptions {
        self.options.clone().zoomed(self.typography.scale())
    }

    /// The zoom in effect, as a factor of the designed sizes (zoom times
    /// the preview font size over 15).
    pub fn scale(&self) -> f32 {
        self.typography.scale()
    }

    /// Applies the Appearance settings in effect: a new zoom or font size
    /// builds the document again where the view is, new font families
    /// replace the fonts, a new readable width moves the column.
    fn sync_typography(&mut self, cx: &mut Context<'_, Self>) {
        let new = Typography::from_app(cx);
        if new == self.typography {
            return;
        }
        let old = std::mem::replace(&mut self.typography, new);
        self.state.borrow_mut().column_max = self.typography.column_max();
        let fonts_differ = old.fonts_differ(&self.typography);
        if fonts_differ {
            let families = self.typography.families(&cx.text_system().all_font_names());
            let fonts = items::font_table(&StyleTable::preview(), &families);
            self.state.borrow().text.borrow_mut().set_fonts(fonts);
        }
        if fonts_differ || old.scale() != self.typography.scale() {
            self.rebuild(cx);
        }
        cx.notify();
    }

    /// Whether the shown document was built at another scale than the one
    /// in effect (the zoom changed while it was being built).
    fn typography_stale(&self) -> bool {
        self.state
            .borrow()
            .doc
            .as_ref()
            .is_some_and(|doc| doc.tree.options().styles.zoom() != self.typography.scale())
    }

    /// Builds the shown document again with the options in effect, where
    /// the view is. While a build runs, its result is checked when it
    /// arrives instead ([`Self::typography_stale`]).
    fn rebuild(&mut self, cx: &mut Context<'_, Self>) {
        if self._load.is_some() || self.is_building() || self.status != LoadStatus::Ready {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        if let Some(stamp) = self.live.shown_stamp() {
            let handoff = {
                let st = self.state.borrow();
                st.doc.as_ref().and_then(|doc| {
                    Some(crate::editor::SyntaxHandoff {
                        tree: doc.syntax.clone(),
                        text: doc.lines.clone()?,
                        stamp,
                        since: None,
                    })
                })
            };
            if let Some(handoff) = handoff {
                self.follow(path, handoff, None, cx);
            }
            return;
        }
        let text = self.state.borrow().doc.as_ref().map(|d| d.text.clone());
        if let Some(text) = text {
            let at = self.position();
            self.open_text(path, text, at, cx);
        }
    }

    /// Sets the tab's profile. A document shown from a file is built again
    /// from its text, where the view is; one that follows an editor is
    /// built again from the editor's next hand-off (the editor restarts it
    /// when its profile changes).
    pub fn set_profile(&mut self, profile: Arc<CompiledProfile>, cx: &mut Context<'_, Self>) {
        if *self.profile == *profile {
            return;
        }
        self.profile = profile;
        self.options = self.options.clone().with_profile(&self.profile);
        if self.is_building() || self.live.shown_stamp().is_some() {
            self.live.restart();
            return;
        }
        let text = self.state.borrow().doc.as_ref().map(|d| d.text.clone());
        if let (Some(path), Some(text)) = (self.path.clone(), text)
            && self.status == LoadStatus::Ready
        {
            let at = self.position();
            self.open_text(path, text, at, cx);
        }
    }

    /// The profile documents are read with.
    pub fn profile(&self) -> ProfileId {
        self.profile.id
    }

    /// Where the view puts its text column.
    pub fn layout(&self) -> ReaderLayout {
        self.state.borrow().layout
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
            st.timing.navigation = taken_navigation(&path, target.as_ref());
            st.revision
        };
        self.path = Some(path.clone());
        self.status = LoadStatus::Loading;
        self.target = target;
        self._refresh = None;
        self.live.forget();
        let resolver = Resolver::statuses(&path, self.index.as_ref(), self.profile.id);
        self.statuses_from = index_key(self.index.as_ref());
        let options = self.build_options();
        let profile = Arc::clone(&self.profile);
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { load::load(&path, &resolver, options, &profile) })
                .await;
            this.update(cx, |reader, cx| reader.finish_load(revision, result, cx))
                .ok();
        }));
        cx.notify();
    }

    /// Shows `text` as the document at `path`, scrolled to `target`: the
    /// editor's text when the tab comes back to Preview before it is saved.
    /// It is built like a loaded file, off the UI thread.
    pub fn open_text(
        &mut self,
        path: PathBuf,
        text: Arc<str>,
        target: Option<ScrollTarget>,
        cx: &mut Context<'_, Self>,
    ) {
        let revision = {
            let mut st = self.state.borrow_mut();
            st.revision += 1;
            st.timing.opened = Some(Instant::now());
            st.timing.first_content = None;
            st.timing.navigation = None;
            st.revision
        };
        self.path = Some(path.clone());
        self.status = LoadStatus::Loading;
        self.target = target;
        self._refresh = None;
        self.live.forget();
        let resolver = Resolver::statuses(&path, self.index.as_ref(), self.profile.id);
        self.statuses_from = index_key(self.index.as_ref());
        let options = self.build_options();
        let profile = Arc::clone(&self.profile);
        self._load = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Ok(load::load_text(&path, text, &resolver, options, &profile))
                })
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
                self.find_document_changed(cx);
                self.sync_remote(cx);
                if self.typography_stale() {
                    self.rebuild(cx);
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
        let resolver = Resolver::statuses(&path, self.index.as_ref(), doc.profile);
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
                    let mut tree = doc.tree;
                    // Code blocks whose wrap was toggled meanwhile.
                    for (block, wrap) in current.tree.code_wrap_settings() {
                        Arc::make_mut(&mut tree).set_code_wrap(block, wrap);
                    }
                    current.tree = tree;
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
        if let Some(path) = self.path.clone()
            && let Some(nav) = taken_navigation(&path, Some(&target))
        {
            self.state.borrow_mut().timing.navigation = Some(nav);
        }
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

    /// The loaded document's model (headings, outline), for the window's
    /// outline and status bar. Cloning it is O(1).
    pub fn model(&self) -> Option<gv_model::DocModel> {
        let st = self.state.borrow();
        st.doc.as_ref().map(|doc| doc.model.clone())
    }

    /// The plain text of the whole loaded document, without list markers
    /// or blank lines (for the status bar's word count).
    pub fn plain_text(&self) -> Option<String> {
        let st = self.state.borrow();
        let tree = &st.doc.as_ref()?.tree;
        let opts = CopyOptions {
            list_markers: false,
            blank_lines: false,
        };
        Some(tree.plain_text(0..tree.len(), opts))
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

    /// The selection as Markdown: its source bytes, with partly selected
    /// inline constructs whole and the first line's container prefix.
    pub fn selected_markdown(&self) -> Option<String> {
        let st = self.state.borrow();
        let range = st.selected()?;
        let doc = st.doc.as_ref()?;
        Some(doc.tree.markdown_for(&doc.syntax, &doc.text, range))
    }

    /// The shown document's text and the selected source range, for
    /// *Copy as rich text*; `None` without a selection.
    pub fn selection_source(&self) -> Option<(Arc<str>, std::ops::Range<usize>)> {
        let st = self.state.borrow();
        let range = st.selected()?;
        Some((st.doc.as_ref()?.text.clone(), range))
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn copy_as_markdown(&mut self, _: &CopyAsMarkdown, _: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(text) = self.selected_markdown().filter(|t| !t.is_empty()) {
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

#[cfg(test)]
impl Reader {
    /// The items the last frame painted: the source offset each starts at
    /// and its content's top in px below the top of the view, in order.
    pub(crate) fn painted_items(&self) -> Vec<(usize, f32)> {
        let st = self.state.borrow();
        let Some(doc) = &st.doc else {
            return Vec::new();
        };
        let top = f32::from(st.geometry.bounds.top());
        st.geometry
            .items
            .iter()
            .map(|g| {
                let item = &doc.tree.items()[g.ix];
                (item.source.start, g.top + item.gap_before - top)
            })
            .collect()
    }

    /// The height of the view, in px.
    pub(crate) fn viewport_height(&self) -> f32 {
        self.state.borrow().engine.viewport()
    }

    /// The bounds of the last frame and its text column (window x, width).
    pub(crate) fn frame_geometry(&self) -> (gpui::Bounds<gpui::Pixels>, (f32, f32)) {
        let st = self.state.borrow();
        (st.geometry.bounds, st.geometry.column)
    }

    /// The type of the document shown.
    pub(crate) fn styles(&self) -> Option<StyleTable> {
        let st = self.state.borrow();
        Some(st.doc.as_ref()?.tree.options().styles.clone())
    }
}

/// The navigation that is showing `path` at `target` now, if an input
/// started one; logged at the first frame that paints the target. One that
/// opens in a background tab is logged now, untimed.
fn taken_navigation(path: &Path, target: Option<&ScrollTarget>) -> Option<follow::Navigation> {
    let nav = follow::Navigation::new(follow::take()?, target.cloned());
    if nav.in_background() {
        tracing::info!("{}", nav.line(path, None));
        return None;
    }
    Some(nav)
}

/// Scrolls `st` to `target` in its loaded document.
fn jump(st: &mut State, target: &ScrollTarget) {
    let Some(doc) = &st.doc else {
        return;
    };
    let last = doc.tree.items().len().checked_sub(1);
    let ix = match target {
        ScrollTarget::Fragment(fragment) => {
            let params = ProfileParams::for_profile(doc.profile);
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        if self.status == LoadStatus::Ready {
            self.debug_find(window, cx);
        }
        self.state.borrow_mut().timing.frame_start = Some(Instant::now());
        let line = text::PREVIEW_BODY.line * self.typography.scale();
        let root = div()
            .id("reader")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::copy_as_markdown))
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
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::on_find_next))
            .on_action(cx.listener(Self::on_find_previous))
            .on_action(cx.listener(Self::on_close_find))
            .relative()
            .size_full()
            .bg(theme.surface.page);
        // The find popover, at the top right of the document area.
        let popover = self
            .find
            .bar
            .clone()
            // Kept while another document loads in the tab, so the field
            // keeps the keyboard.
            .filter(|_| {
                self.find.open && matches!(self.status, LoadStatus::Ready | LoadStatus::Loading)
            })
            .map(|bar| {
                div()
                    .absolute()
                    .top(px(find_spacing::INSET_TOP))
                    .right(px(find_spacing::INSET_RIGHT))
                    .child(bar)
            });
        match &self.status {
            LoadStatus::Ready => root
                .child(ReaderElement::new(
                    self.state.clone(),
                    cx.weak_entity(),
                    self.focus.clone(),
                    Palette::of(Appearance::current_id(cx)),
                    Appearance::current_id(cx) == crate::theme::ThemeId::Dark,
                ))
                .children(popover),
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
            LoadStatus::Loading | LoadStatus::Empty => root.children(popover),
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Search panel of the sidebar (FR-SEARCH-01, FR-SEARCH-02; SPEC §
//! Sidebar, "Search panel").
//!
//! - **Field:** the query, with the match case (`Aa`) and regular
//!   expression (`.*`, the whole query is one `/regex/`) toggles. Every
//!   keystroke cancels the running search and starts a new one
//!   ([`gv_search::search`], on the low-priority background pool); the
//!   results stream in, ranked, and the previous ones stay until the first
//!   batch of the new search arrives. An invalid regular expression is
//!   reported where the summary goes.
//! - **Summary line:** "7 results in 4 files", with the progress while
//!   the scan runs ("· 1,204 of 10,000 notes") and a note when the file
//!   limit stopped it.
//! - **Results**, grouped by note: a row with a chevron, the note's title
//!   and its match count, then one line per match with the matched text
//!   marked, cut with an ellipsis where the line goes on; inline markup is
//!   removed and formulas are typeset ([`typeset::segments`]). A click on a
//!   note's row folds it; a click on a line opens the note scrolled there
//!   (Mod+click or a middle click: in a new tab). The heading path of a
//!   line shows as its tooltip.
//! - **Head:** *Fold all* (unfold all when all are folded) and *Copy
//!   results as Markdown links*.
//! - **Keys** ([`key_bindings`]): Up and Down move through the rows from the
//!   field or the list, Enter opens the selected row (the first match when
//!   none is), Mod+Enter in a new tab, Left and Right fold and unfold a
//!   note in the list, Esc gives the keyboard back to the document.
//!
//! The window tells the panel what to search ([`SearchScope`]): the
//! vault's root and its content index once built. A new index snapshot
//! (an edit, a file changed on disk) runs the query again after
//! [`RERUN_DELAY`].
//!
//! Not designed (built plainly, flagged): the head's buttons, the empty,
//! indexing and error states, the selected row, the heading path tooltip,
//! the space between context lines, and the matched text's 2 px radius
//! (text highlights cannot round their corners; the marks are square).

pub(crate) mod excerpt;
mod model;
#[cfg(test)]
mod tests;
pub(crate) mod typeset;

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, BoxShadow, ClipboardItem, Context, Div, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle, InteractiveElement as _,
    IntoElement, KeyBinding, ListAlignment, ListState, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task,
    Window, div, list, point, px,
};
use gv_search::{CancelToken, Query, ResultBatch, SearchOptions, VaultSource};
use gv_vault::content::{ContentSnapshot, DiskSource};

use crate::a11y::Role;
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::find as toggle_size;
use crate::metrics::spacing::{find as field_spacing, list_row, panel_head, search as spacing};
use crate::metrics::{FOCUS_HALO, HAIRLINE, font, icon, radius, size, text};
use crate::reader::Disposition;
use crate::theme::Theme;
/// `n` with thousands separators ("1,204"), as the panel counts.
pub(crate) use model::grouped as grouped_count;
pub use model::{Progress, Row};
use model::{Results, grouped};
use typeset::math_run;
pub(crate) use typeset::{Line, Segment, segments};

/// A context line as shown: text, marks, whether text comes before it.
struct Shown {
    text: String,
    marks: Vec<std::ops::Range<usize>>,
    math: Vec<excerpt::Formula>,
    clipped_start: bool,
}

/// The average character width of the interface font, in em, to count the
/// characters a context row holds.
const AVERAGE_CHAR_EM: f32 = 0.52;
/// Characters a row holds before the list has a width (the design's
/// 232 px panel).
const DEFAULT_ROW_CHARS: usize = 30;
/// Characters always kept before a match.
const MIN_LEAD_CHARS: usize = 8;

/// How long a new index snapshot waits before the query runs again.
pub const RERUN_DELAY: Duration = Duration::from_millis(400);

/// The key context of the panel (the keymap model's `search`).
pub(crate) const KEY_CONTEXT: &str = "VaultSearch";

gpui::actions!(
    vault_search,
    [
        /// Select the row below.
        SelectNext,
        /// Select the row above.
        SelectPrevious,
        /// Open the selected row (the first match when none is selected).
        OpenSelected,
        /// Open the selected row in a new tab.
        OpenSelectedInNewTab,
        /// Fold the selected note, or select the note of a context line.
        Collapse,
        /// Unfold the selected note.
        Expand,
        /// Give the keyboard back to the document.
        Dismiss,
    ]
);

const MOD_ENTER: &str = if cfg!(target_os = "macos") {
    "cmd-enter"
} else {
    "ctrl-enter"
};

/// The panel's keys, in its list and in its field (where they come before
/// the field's own; Left and Right stay the field's).
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let field = "VaultSearch > Input";
    let mut keys = Vec::new();
    for context in [KEY_CONTEXT, field] {
        keys.extend([
            KeyBinding::new("up", SelectPrevious, Some(context)),
            KeyBinding::new("down", SelectNext, Some(context)),
            KeyBinding::new("enter", OpenSelected, Some(context)),
            KeyBinding::new(MOD_ENTER, OpenSelectedInNewTab, Some(context)),
            KeyBinding::new("escape", Dismiss, Some(context)),
        ]);
    }
    keys.extend([
        KeyBinding::new("left", Collapse, Some(KEY_CONTEXT)),
        KeyBinding::new("right", Expand, Some(KEY_CONTEXT)),
    ]);
    keys
}

/// What the panel searches: the vault's root, and its content index once
/// it is built.
#[derive(Debug, Clone)]
pub struct SearchScope {
    pub root: PathBuf,
    pub content: Option<ContentSnapshot>,
}

/// What the panel asks of its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchPanelEvent {
    /// Open the note at `path`, scrolled to the block holding `offset`.
    Open {
        path: PathBuf,
        offset: Option<usize>,
        disposition: Disposition,
    },
    /// Give the keyboard back to the document.
    Dismiss,
}

/// The field's toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchToggle {
    MatchCase,
    Regex,
}

impl SearchToggle {
    fn label(self) -> &'static str {
        match self {
            Self::MatchCase => "Aa",
            Self::Regex => ".*",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::MatchCase => "search-match-case",
            Self::Regex => "search-regex",
        }
    }
}

/// The Search panel of a window's sidebar.
pub struct SearchPanel {
    focus: FocusHandle,
    input: Entity<InputState>,
    match_case: bool,
    regex: bool,
    scope: Option<SearchScope>,
    results: Results,
    /// Why the query cannot run (an invalid regular expression).
    error: Option<SharedString>,
    options: SearchOptions,
    cancel: CancelToken,
    /// Counts searches; batches of an older one are dropped.
    generation: u64,
    /// The results shown are those of the previous search, replaced when
    /// the new one sends its first batch or ends.
    stale: bool,
    list: ListState,
    /// The list's width at the last frame, read before the list draws its
    /// rows (they cannot ask the list while it draws).
    list_width: std::cell::Cell<f32>,
    _search: Option<Task<()>>,
    _rerun: Option<Task<()>>,
    _input_events: Subscription,
}

impl std::fmt::Debug for SearchPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchPanel")
            .field("match_case", &self.match_case)
            .field("regex", &self.regex)
            .field("files", &self.results.files().len())
            .field("progress", &self.results.progress)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<SearchPanelEvent> for SearchPanel {}

impl Focusable for SearchPanel {
    /// The field's: showing the panel puts the keyboard there.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Drop for SearchPanel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl SearchPanel {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let events =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.run(cx),
                    InputEvent::Focus | InputEvent::Blur => cx.notify(),
                    // Enter is the panel's (`OpenSelected`).
                    InputEvent::PressEnter { .. } => {}
                },
            );
        Self {
            focus: cx.focus_handle(),
            input,
            match_case: false,
            regex: false,
            scope: None,
            results: Results::default(),
            error: None,
            options: SearchOptions::default(),
            cancel: CancelToken::new(),
            generation: 0,
            stale: false,
            list: ListState::new(0, ListAlignment::Top, px(200.0)),
            list_width: std::cell::Cell::new(0.0),
            _search: None,
            _rerun: None,
            _input_events: events,
        }
    }

    /// Sets what the panel searches. A newer content index runs the query
    /// again (at once when there was none, else after [`RERUN_DELAY`]).
    pub fn set_scope(&mut self, scope: Option<SearchScope>, cx: &mut Context<'_, Self>) {
        let generation = |s: &Option<SearchScope>| {
            s.as_ref()
                .map(|s| (s.root.clone(), s.content.as_ref().map(|c| c.generation())))
        };
        let (old, new) = (generation(&self.scope), generation(&scope));
        if old == new {
            return;
        }
        let had_content = matches!(&old, Some((_, Some(_))));
        let same_root = old.as_ref().map(|o| &o.0) == new.as_ref().map(|n| &n.0);
        self.scope = scope;
        if had_content && same_root {
            self._rerun = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RERUN_DELAY).await;
                let _ = this.update(cx, |p, cx| p.run(cx));
            }));
        } else {
            self.run(cx);
        }
        cx.notify();
    }

    /// The query as typed.
    pub fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// Replaces the query and runs it.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        self.run(cx);
    }

    /// Gives the keyboard to the field, with its text selected.
    pub fn focus_field(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// Whether the field or the list has the keyboard.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
            || self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Turns a toggle on or off and runs the query again.
    pub fn toggle(&mut self, toggle: SearchToggle, cx: &mut Context<'_, Self>) {
        match toggle {
            SearchToggle::MatchCase => self.match_case = !self.match_case,
            SearchToggle::Regex => self.regex = !self.regex,
        }
        self.run(cx);
        cx.notify();
    }

    pub fn progress(&self) -> Progress {
        self.results.progress
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// The summary line as shown.
    pub fn summary(&self) -> String {
        self.results.summary()
    }

    /// The results as Markdown links, one per note.
    pub fn markdown_links(&self) -> String {
        self.results.as_markdown_links()
    }

    /// The query as the engine takes it: with the regular expression
    /// toggle, the whole text is one `/regex/` (its slashes escaped).
    fn effective_query(&self, text: &str) -> String {
        if !self.regex || text.is_empty() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len() + 2);
        out.push('/');
        let mut escaped = false;
        for c in text.chars() {
            if c == '/' && !escaped {
                out.push('\\');
            }
            escaped = c == '\\' && !escaped;
            out.push(c);
        }
        out.push('/');
        out
    }

    /// Cancels the running search and starts the current query.
    fn run(&mut self, cx: &mut Context<'_, Self>) {
        self.cancel.cancel();
        self._search = None;
        self._rerun = None;
        self.generation += 1;
        self.error = None;
        let text = self.query(cx);
        let finish_idle = |this: &mut Self, cx: &mut Context<'_, Self>| {
            this.stale = false;
            this.results.clear();
            this.results.clear_selection();
            this.sync_list();
            cx.notify();
        };
        if text.trim().is_empty() {
            finish_idle(self, cx);
            return;
        }
        let query = match Query::new(&self.effective_query(&text), self.match_case) {
            Ok(query) => query,
            Err(err) => {
                self.error = Some(err.to_string().into());
                finish_idle(self, cx);
                return;
            }
        };
        let Some((root, content)) = self
            .scope
            .as_ref()
            .and_then(|s| Some((s.root.clone(), s.content.clone()?)))
        else {
            // No vault, or its index is being built: the summary says so;
            // the index's first snapshot runs the query.
            finish_idle(self, cx);
            return;
        };
        if query.is_empty() {
            finish_idle(self, cx);
            return;
        }
        let cancel = CancelToken::new();
        self.cancel = cancel.clone();
        self.stale = true;
        let generation = self.generation;
        let options = self.options.clone();
        let total = content.note_count();
        let source = VaultSource::new(
            content.clone(),
            Arc::new(DiskSource::new(root, content.options())),
        );
        let (batches, mut received) = futures::channel::mpsc::unbounded::<ResultBatch>();
        let scan = cx
            .background_executor()
            .spawn_with_priority(gpui::Priority::Low, async move {
                gv_search::search(&query, &source, &options, &cancel, None, |batch| {
                    // Fails only when the panel is gone.
                    let _ = batches.unbounded_send(batch);
                })
            });
        self.results.progress = Progress::Running { scanned: 0, total };
        self._search = Some(cx.spawn(async move |this, cx| {
            while let Some(batch) = received.next().await {
                let alive = this.update(cx, |p, cx| p.add_batch(generation, batch, cx));
                if alive.is_err() {
                    return;
                }
            }
            let summary = scan.await;
            let _ = this.update(cx, |p, cx| {
                if p.generation != generation {
                    return;
                }
                p.take_stale();
                p.results.progress = Progress::Done(summary);
                p.sync_list();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The previous search's results go when the new one has something to
    /// show.
    fn take_stale(&mut self) {
        if std::mem::take(&mut self.stale) {
            self.results.clear();
        }
    }

    fn add_batch(&mut self, generation: u64, batch: ResultBatch, cx: &mut Context<'_, Self>) {
        if generation != self.generation {
            return;
        }
        self.take_stale();
        let content = self.scope.as_ref().and_then(|s| s.content.clone());
        self.results.add(batch.files, content.as_ref());
        self.results.progress = Progress::Running {
            scanned: batch.scanned,
            total: batch.total,
        };
        self.sync_list();
        cx.notify();
    }

    /// The list follows the rows.
    fn sync_list(&mut self) {
        let count = self.list.item_count();
        let rows = self.results.rows().len();
        if count != rows || rows > 0 {
            self.list.splice(0..count, rows);
        }
    }

    // ----- keys and clicks ------------------------------------------------------

    fn step(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        if let Some(row) = self.results.step(forward) {
            self.list.scroll_to_reveal_item(row);
        }
        cx.notify();
    }

    fn open_target(&mut self, disposition: Disposition, cx: &mut Context<'_, Self>) {
        let Some(row) = self.results.target() else {
            return;
        };
        self.open_row(row, disposition, cx);
    }

    fn open_row(&mut self, row: Row, disposition: Disposition, cx: &mut Context<'_, Self>) {
        let Some(root) = self.scope.as_ref().map(|s| s.root.clone()) else {
            return;
        };
        let (file, offset) = match row {
            Row::File(f) => (f, None),
            Row::Match(f, s) => (
                f,
                self.results
                    .files()
                    .get(f)
                    .and_then(|r| r.snippets.get(s))
                    .map(|s| s.range.start),
            ),
        };
        let Some(result) = self.results.files().get(file) else {
            return;
        };
        cx.emit(SearchPanelEvent::Open {
            path: result.path.to_fs_path(&root),
            offset,
            disposition,
        });
    }

    fn click_row(
        &mut self,
        ix: usize,
        disposition: Disposition,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.focus.focus(window, cx);
        self.results.select(ix);
        match self.results.rows().get(ix).copied() {
            Some(Row::File(f)) if disposition == Disposition::InPlace => {
                self.results.toggle(f);
                self.sync_list();
            }
            Some(row) => self.open_row(row, disposition, cx),
            None => {}
        }
        cx.notify();
    }

    fn copy_links(&mut self, cx: &mut Context<'_, Self>) {
        let links = self.markdown_links();
        if !links.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(links));
        }
    }

    fn fold_all(&mut self, cx: &mut Context<'_, Self>) {
        self.results.toggle_all();
        self.sync_list();
        cx.notify();
    }

    // ----- drawing --------------------------------------------------------------

    fn render_head(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let has_results = !self.results.files().is_empty();
        let all_folded = has_results && self.results.all_folded();
        let button = |id: &'static str, icon: Icon| {
            let (hover, press) = (theme.state.hover, theme.state.press);
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex_none()
                .size(px(size::SMALL_ICON_BUTTON))
                .rounded(px(radius::CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text.secondary)
                .hover(move |s| s.bg(hover))
                .active(move |s| s.bg(press))
                .child(icon.sized(icon::ROW))
        };
        div()
            .h(px(size::PANEL_HEADER))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(panel_head::GAP))
            .pl(px(panel_head::PAD_LEFT))
            .pr(px(panel_head::PAD_RIGHT))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child("Search"),
            )
            .when(has_results, |head| {
                head.child(
                    button("search-fold-all", Icon::CollapseAll)
                        .when(all_folded, |b| b.bg(theme.accent.tint))
                        .on_click(cx.listener(|this, _, _, cx| this.fold_all(cx))),
                )
                .child(
                    button("search-copy-links", Icon::CopyCode)
                        .on_click(cx.listener(|this, _, _, cx| this.copy_links(cx))),
                )
            })
    }

    fn toggle_button(&self, t: SearchToggle, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let on = match t {
            SearchToggle::MatchCase => self.match_case,
            SearchToggle::Regex => self.regex,
        };
        let hover = theme.state.hover;
        let id = t.id();
        div().child(
            div()
                .id(ElementId::from(SharedString::from(id)))
                .debug_selector(move || id.into())
                .flex_none()
                .w(px(toggle_size::TOGGLE_WIDTH))
                .h(px(toggle_size::TOGGLE_HEIGHT))
                .rounded(px(radius::SMALL_CONTROL))
                .flex()
                .items_center()
                .justify_center()
                .font_family(font::EDITOR)
                .text_size(px(text::field_toggle_size()))
                .map(|d| {
                    if on {
                        d.bg(theme.accent.tint).text_color(theme.text.primary)
                    } else {
                        d.text_color(theme.text.secondary)
                            .hover(move |s| s.bg(hover))
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| this.toggle(t, cx)))
                .child(t.label()),
        )
    }

    fn render_field(&self, window: &Window, theme: &Theme, cx: &mut Context<'_, Self>) -> Div {
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        div()
            .flex_none()
            .mx(px(spacing::FIELD_MARGIN_X))
            .h(px(size::FIELD))
            .flex()
            .items_center()
            .gap(px(field_spacing::FIELD_GAP))
            .pl(px(field_spacing::FIELD_PAD_LEFT))
            .pr(px(field_spacing::FIELD_PAD_RIGHT))
            .rounded(px(radius::CONTROL))
            .bg(theme.surface.page)
            .border(px(HAIRLINE))
            .border_color(if focused {
                theme.accent.default
            } else {
                theme.border.strong
            })
            .when(focused, |d| {
                d.shadow(vec![BoxShadow {
                    color: theme.accent.tint.into(),
                    offset: point(px(0.0), px(0.0)),
                    blur_radius: px(0.0),
                    spread_radius: px(FOCUS_HALO),
                    inset: false,
                }])
            })
            .child(
                div().flex_1().min_w_0().h_full().child(
                    Input::new(&self.input)
                        .aria_label("Search in vault")
                        .appearance(false)
                        .h_full()
                        .px_0()
                        .py_0()
                        .text_size(px(text::UI.size))
                        .text_color(theme.text.primary),
                ),
            )
            .child(self.toggle_button(SearchToggle::MatchCase, theme, cx))
            .child(self.toggle_button(SearchToggle::Regex, theme, cx))
    }

    /// The line under the field: the summary, the progress, or why
    /// nothing runs.
    fn status_line(&self, cx: &App) -> (SharedString, bool) {
        if let Some(error) = &self.error {
            return (error.clone(), true);
        }
        let empty_query = self.query(cx).trim().is_empty();
        let line = match &self.scope {
            None => "Open a folder to search its notes".to_owned(),
            Some(scope) if scope.content.is_none() && !empty_query => {
                "Indexing the vault…".to_owned()
            }
            _ if empty_query => String::new(),
            _ => self.results.summary(),
        };
        (line.into(), false)
    }

    fn render_row(&self, ix: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let Some(row) = self.results.rows().get(ix).copied() else {
            return div().into_any_element();
        };
        let selected = self.results.selected_row() == Some(ix);
        let hover = theme.state.hover;
        let id: SharedString = match row {
            Row::File(f) => format!("search-file-{f}").into(),
            Row::Match(f, s) => format!("search-line-{f}-{s}").into(),
        };
        let selector = id.clone();
        let body = match row {
            Row::File(f) => self.render_file(f, theme),
            Row::Match(f, s) => self.render_line(f, s, theme),
        };
        let heading = match row {
            Row::Match(f, s) => self.results.heading(f, s).cloned(),
            Row::File(_) => None,
        };
        let name = match row {
            Row::File(f) => self.results.files().get(f).map(|file| {
                let n = file.match_count.max(1);
                let matches = if n == 1 {
                    "1 match".to_owned()
                } else {
                    format!("{n} matches")
                };
                format!("{}, {matches}", file.title)
            }),
            Row::Match(f, s) => self
                .results
                .files()
                .get(f)
                .and_then(|file| file.snippets.get(s))
                .map(|snippet| {
                    let options = excerpt::inline_options(self.profile());
                    excerpt::strip(&snippet.text, &[], options)
                        .text
                        .trim()
                        .to_string()
                }),
        };
        let frame = body
            .id(ElementId::Name(id))
            .role(match row {
                Row::File(_) => Role::TreeItem,
                Row::Match(..) => Role::ListItem,
            })
            .aria_label(name.unwrap_or_default())
            .aria_level(match row {
                Row::File(_) => 1,
                Row::Match(..) => 2,
            })
            .aria_selected(selected)
            .when(matches!(row, Row::File(_)), |r| {
                r.aria_expanded(!self.results.is_folded(match row {
                    Row::File(f) | Row::Match(f, _) => f,
                }))
            })
            .when(selected, |r| r.aria_active_descendant())
            .debug_selector(move || selector.to_string())
            .rounded(px(radius::CONTROL))
            .when(selected, |r| r.bg(theme.accent.tint))
            .when(!selected, |r| r.hover(move |s| s.bg(hover)))
            .when_some(heading, |r, heading| {
                let tip: SharedString = heading.to_string().into();
                r.tooltip(move |_, cx| cx.new(|_| Tooltip(tip.clone())).into())
            })
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, window, cx| {
                let disposition = Disposition::for_click(&e.modifiers(), cx);
                this.click_row(ix, disposition, window, cx);
            }))
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    this.click_row(ix, Disposition::Background, window, cx);
                }),
            );
        let last_line = matches!(row, Row::Match(..))
            && !matches!(self.results.rows().get(ix + 1), Some(Row::Match(..)));
        div()
            .w_full()
            .px(px(size::LIST_ROW_INSET))
            .when(matches!(row, Row::Match(..)), |slot| {
                slot.pb(px(spacing::LINE_GAP))
            })
            .when(last_line, |slot| slot.pb(px(2.0 * spacing::LINE_GAP)))
            .child(frame)
            .into_any_element()
    }

    /// A note: chevron, icon, title (weight 500), match count.
    fn render_file(&self, f: usize, theme: &Theme) -> Div {
        let Some(file) = self.results.files().get(f) else {
            return div();
        };
        let chevron = if self.results.is_folded(f) || file.snippets.is_empty() {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        };
        let count = file.match_count.max(1);
        div()
            .h(px(size::LIST_ROW))
            .flex()
            .items_center()
            .gap(px(list_row::GAP))
            .pl(px(list_row::PAD_LEFT))
            .pr(px(list_row::PAD_RIGHT))
            .text_color(theme.text.primary)
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(chevron.sized(icon::CHEVRON)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(Icon::Note.sized(icon::ROW)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight(text::weight::ACTIVE))
                    .child(SharedString::from(file.title.to_string())),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(grouped(count)),
            )
    }

    /// What a context line shows of `snippet`: its text without inline
    /// markup, cut at the start so the first match is about centred in the
    /// row (the list's width at about half an em per character).
    fn excerpt(&self, snippet: &gv_search::Snippet, profile: gv_vault::ProfileId) -> Shown {
        let mut m = excerpt::strip(
            &snippet.text,
            &snippet.highlights,
            excerpt::inline_options(profile),
        );
        let row = self.list_width.get()
            - (spacing::LINE_INDENT - size::LIST_ROW_INSET)
            - list_row::PAD_RIGHT;
        let chars = if row > 0.0 {
            (row / (text::UI_SECTION.size * AVERAGE_CHAR_EM)) as usize
        } else {
            DEFAULT_ROW_CHARS
        };
        let first = m
            .marks
            .first()
            .map_or(0, |r| m.text[r.clone()].chars().count());
        let lead = (chars.saturating_sub(first) / 2).max(MIN_LEAD_CHARS);
        let cut = excerpt::keep_lead(&mut m, lead);
        Shown {
            clipped_start: snippet.clipped_start || cut,
            text: m.text,
            marks: m.marks,
            math: m.math,
        }
    }

    /// The profile the vault's notes are read with.
    fn profile(&self) -> gv_vault::ProfileId {
        self.scope
            .as_ref()
            .and_then(|s| s.content.as_ref())
            .map_or(gv_vault::ProfileId::Extended, |c| c.options().profile)
    }

    /// Context line `s` of note `f` as its row shows it, with an ellipsis
    /// where the line goes on, and its marks.
    #[cfg(test)]
    pub(crate) fn line(&self, f: usize, s: usize) -> Option<(String, Vec<Range<usize>>)> {
        self.line_parts(f, s).map(|l| (l.text, l.marks))
    }

    /// Context line `s` of note `f` as its row shows it: with an ellipsis
    /// where the line goes on, its marks and its formulas.
    pub(crate) fn line_parts(&self, f: usize, s: usize) -> Option<Line> {
        let snippet = self.results.files().get(f)?.snippets.get(s)?;
        let shown = self.excerpt(snippet, self.profile());
        const ELLIPSIS: &str = "…";
        let mut line = String::with_capacity(shown.text.len() + 2 * ELLIPSIS.len());
        let shift = if shown.clipped_start {
            line.push_str(ELLIPSIS);
            ELLIPSIS.len()
        } else {
            0
        };
        // Tabs and stray line breaks would break the one-line row (spaces
        // of the same length keep the ranges in place).
        for c in shown.text.chars() {
            if c.is_control() {
                line.extend(std::iter::repeat_n(' ', c.len_utf8()));
            } else {
                line.push(c);
            }
        }
        if snippet.clipped_end {
            line.push_str(ELLIPSIS);
        }
        let marks = shown
            .marks
            .iter()
            .map(|r| r.start + shift..r.end + shift)
            .collect();
        let math = shown
            .math
            .into_iter()
            .map(|f| excerpt::Formula {
                out: f.out.start + shift..f.out.end + shift,
                ..f
            })
            .collect();
        Some(Line {
            text: line,
            marks,
            math,
        })
    }

    /// A context line, its matches marked, cut with an ellipsis: inline
    /// markup removed ([`excerpt`]), formulas typeset ([`segments`]), the
    /// start cut so the first match sits near the middle of the row.
    fn render_line(&self, f: usize, s: usize, theme: &Theme) -> Div {
        let Some(line) = self.line_parts(f, s) else {
            return div();
        };
        let mark = HighlightStyle {
            color: Some(theme.text.primary.into()),
            background_color: Some(theme.accent.tint.into()),
            ..HighlightStyle::default()
        };
        let styled = |text: String, marks: Vec<Range<usize>>| {
            let highlights: Vec<(Range<usize>, HighlightStyle)> =
                marks.into_iter().map(|r| (r, mark)).collect();
            StyledText::new(text).with_highlights(highlights)
        };
        let mut runs = segments(&line);
        let content = match runs.as_slice() {
            [] | [Segment::Text { .. }] => {
                let (text, marks) = match runs.pop() {
                    Some(Segment::Text { text, marks }) => (text, marks),
                    _ => (String::new(), Vec::new()),
                };
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(styled(text, marks))
            }
            _ => {
                // Text runs keep their width; the last one ends with an
                // ellipsis when the row is full, and the row clips the rest.
                let last = runs.len() - 1;
                let color: gpui::Hsla = theme.text.secondary.into();
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .children(runs.into_iter().enumerate().map(|(ix, run)| {
                        match run {
                            Segment::Text { text, marks } => div()
                                .when(ix < last, |d| d.flex_none().whitespace_nowrap())
                                .when(ix == last, |d| d.flex_1().min_w_0().truncate())
                                .child(styled(text, marks))
                                .into_any_element(),
                            Segment::Math { layout, .. } => {
                                math_run(layout, color).into_any_element()
                            }
                        }
                    }))
            }
        };
        div()
            .h(px(text::UI_SECTION.line))
            .flex()
            .items_center()
            .pl(px(spacing::LINE_INDENT - size::LIST_ROW_INSET))
            .pr(px(list_row::PAD_RIGHT))
            .text_size(px(text::UI_SECTION.size))
            .line_height(px(text::UI_SECTION.line))
            .text_color(theme.text.secondary)
            .child(content)
    }
}

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.list_width
            .set(f32::from(self.list.viewport_bounds().size.width));
        let theme = Appearance::theme(cx);
        let head = self.render_head(theme, cx);
        let field = self.render_field(window, theme, cx);
        let (status, is_error) = self.status_line(cx);
        let rows = !self.results.rows().is_empty();
        div()
            .id("search-panel")
            .role(gpui::Role::Group)
            .aria_label("Search panel")
            .debug_selector(|| "sidebar-panel:search".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                this.open_target(Disposition::InPlace, cx);
            }))
            .on_action(cx.listener(|this, _: &OpenSelectedInNewTab, _, cx| {
                this.open_target(Disposition::NewTab, cx);
            }))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| {
                this.results.collapse();
                this.sync_list();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Expand, _, cx| {
                this.results.expand();
                this.sync_list();
                cx.notify();
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(SearchPanelEvent::Dismiss)))
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(head)
            .child(field)
            .child(
                div()
                    .debug_selector(|| "search-summary".into())
                    .h(px(spacing::SUMMARY_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(size::LIST_ROW_INSET + list_row::PAD_LEFT))
                    .text_size(px(text::UI_SMALL.size))
                    .line_height(px(text::UI_SMALL.line))
                    .text_color(if is_error {
                        theme.status.error
                    } else {
                        theme.text.secondary
                    })
                    .child(div().min_w_0().truncate().child(status)),
            )
            .when(rows, |panel| {
                panel.child(
                    div().flex_1().min_h_0().child(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix: usize, _, cx| this.render_row(ix, cx)),
                        )
                        .size_full(),
                    ),
                )
            })
    }
}

/// The heading path of a context line, as a tooltip (SPEC § Popovers:
/// inverted colours, 22 high).
struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        div()
            .h(px(size::TOOLTIP))
            .px(px(crate::metrics::spacing::tooltip::PAD_X))
            .flex()
            .items_center()
            .rounded(px(radius::CONTROL))
            .bg(theme.text.primary)
            .text_color(theme.surface.page)
            .text_size(px(text::tooltip_size()))
            .child(self.0.clone())
    }
}

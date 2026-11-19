// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Bases view: a `.base` file, read-only (FR-BASES-01..04).
//!
//! The model, the expression language and the evaluation are
//! `gv-bases`'s; this module runs them and draws the result.
//!
//! # Evaluation
//!
//! - The file is read and compiled on the background executor
//!   ([`BasesView::load`]); a base that is not valid YAML shows why.
//! - The files a base lists come from the window's content index: the
//!   window hands each new snapshot to [`BasesView::set_content`], which
//!   builds the base's view of it ([`gv_bases::IndexData`], one pass over
//!   the vault) on the low-priority pool, then evaluates again. Tests and
//!   embedders can give the files directly ([`BasesView::set_data`]).
//! - Each evaluation runs on the low-priority pool with a time budget
//!   ([`EVAL_BUDGET`]) and a cancel flag. A newer one (new data, another
//!   view, the file changed) cancels the one running; a stale result is
//!   dropped. Until the first result the view says what it waits for;
//!   later ones replace the shown result when they land.
//! - `this` is the base file's vault path (FR-BASES-04); an embedded base
//!   ([`BasesView::embedded`]) gets the embedding note's, a sidebar base
//!   ([`BasesView::sidebar`], [`BasesPanel`]) the active file's.
//! - Dates are read and shown in the system's time zone.
//!
//! # Drawing
//!
//! - A tab per view of the base, the number of results (`shown of
//!   matched` when a limit hides some), and a problems chip that opens the
//!   problems panel: model warnings, syntax errors and evaluation
//!   problems with the number of files each affected.
//! - **Table** ([`table`]): a header that stays put, rows in a virtual
//!   list, columns `columnSize` wide (scrolls sideways when wider than
//!   the view), group headers with their summaries, and the summaries of
//!   the whole view in a footer.
//! - **Cards** ([`cards`]): a grid of cards `cardSize` wide, the cover
//!   image (`image`, `imageFit`, `imageAspectRatio`) above the property
//!   lines; group headers between groups. Covers from the vault go through
//!   the reader's media engine; remote covers follow the remote-content
//!   setting and are fetched only where it allows them without asking.
//! - **List**: one line per file, the first property first.
//! - Layouts Glyphvane does not draw (map, kanban, others) show a notice
//!   above the rows as a table.
//! - Cells ([`cells`]): links, files and URLs are clickable (Mod for a
//!   new tab, middle click for a background tab), lists are chips, dates
//!   use the base's display format, booleans a check box, `html()` its
//!   text only (no markup is ever rendered), `image()` a thumbnail. A cell
//!   that could not be evaluated says why in red, in full in a tooltip.
//!
//! # Keys
//!
//! `]` and `[` switch to the next and the previous view. The view tabs and
//! the problems chip are Tab stops (Enter or Space presses them); so is
//! the view itself, except in an embed. With the view focused, the arrow
//! keys, Home, End, Page Up and Page Down move over the rows of a table
//! or a list and Left and Right over a table's columns ([`cursor`]);
//! Enter opens the row's file (Mod+Enter in a new tab). The cards of the
//! cards layout are Tab stops of their own.
//!
//! # Accessibility
//!
//! The view is a group named after the base. The views are a tab list;
//! the results count is a polite live status; the problems chip a button
//! that says whether its panel is open, the panel a list. The table is a
//! table of rows and cells with column headers, row and column indices
//! and counts ([`table`]); the cards and the list layouts are lists whose
//! items are named after the file's first property, a card describing
//! its other properties. The keyboard's row is selected and its cell (a
//! list's item) is the view's active descendant ([`cursor`]). The rows
//! are a virtual list, so only the rows in view (and their cells) are
//! described, however long the base.
//!
//! # Not designed (built plainly, flagged)
//!
//! The whole surface: tabs, counts, notices, the problems chip and panel,
//! the table, cards, list, chips, error cells, empty and loading states.

mod cards;
mod cells;
mod cursor;
mod panel;
mod table;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyBinding, ListAlignment, ListState,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, UniformListScrollHandle, Window, div, px,
};
use gv_bases::{CompiledBase, EvalContext, Incomplete, IndexData, VaultData, ViewKind, ViewResult};
use gv_vault::Fragment;
use gv_vault::content::ContentSnapshot;

use crate::a11y::{A11yExt as _, AccessibleExt as _, Live, Role};
use crate::appearance::Appearance;
use crate::metrics::radius;
use crate::metrics::text;
use crate::reader::{Disposition, LinkRequest};
use crate::theme::Theme;
use cards::Covers;
pub(crate) use cells::cell_text;
pub use panel::BasesPanel;
use table::Line;

/// The key context of the Bases view.
pub(crate) const KEY_CONTEXT: &str = "Bases";

gpui::actions!(
    bases,
    [
        /// Show the base's next view.
        NextView,
        /// Show the base's previous view.
        PreviousView,
        /// Move the keyboard to the next row.
        NextRow,
        /// Move the keyboard to the previous row.
        PreviousRow,
        /// Move the keyboard to the first row.
        FirstRow,
        /// Move the keyboard to the last row.
        LastRow,
        /// Move the keyboard a page of rows down.
        PageDownRows,
        /// Move the keyboard a page of rows up.
        PageUpRows,
        /// Move the keyboard to the next column of the table.
        NextColumn,
        /// Move the keyboard to the previous column of the table.
        PreviousColumn,
        /// Open the keyboard row's file.
        OpenRow,
        /// Open the keyboard row's file in a new tab.
        OpenRowInNewTab,
    ]
);

/// The Bases view's keys. They apply while it has the focus.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let c = Some(KEY_CONTEXT);
    vec![
        KeyBinding::new("]", NextView, c),
        KeyBinding::new("[", PreviousView, c),
        KeyBinding::new("down", NextRow, c),
        KeyBinding::new("up", PreviousRow, c),
        KeyBinding::new("home", FirstRow, c),
        KeyBinding::new("end", LastRow, c),
        KeyBinding::new("pagedown", PageDownRows, c),
        KeyBinding::new("pageup", PageUpRows, c),
        KeyBinding::new("right", NextColumn, c),
        KeyBinding::new("left", PreviousColumn, c),
        KeyBinding::new("enter", OpenRow, c),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-enter"
            } else {
                "ctrl-enter"
            },
            OpenRowInNewTab,
            c,
        ),
    ]
}

/// How long one evaluation may run; the result is then shown as
/// incomplete.
pub const EVAL_BUDGET: Duration = Duration::from_secs(5);

/// The files a base evaluates over, shared with the background pool.
pub type BaseData = Arc<dyn VaultData + Send + Sync>;

/// What the Bases view asks of its window.
#[derive(Debug, Clone, PartialEq)]
pub enum BasesEvent {
    /// Open a file of the vault (a file value, a result's file): a note,
    /// or any other file as the window opens files of its kind.
    OpenFile {
        path: PathBuf,
        fragment: Option<Fragment>,
        disposition: Disposition,
    },
    /// A link value or a URL was clicked: follow it as a link written in
    /// the base file (the window's resolver and URL policy apply).
    FollowLink(LinkRequest),
}

/// Where the view is with its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BasesStatus {
    Empty,
    Loading,
    /// The base is compiled; results follow once there are files.
    Ready,
    /// The file could not be read; why.
    Unreadable(SharedString),
    /// The file is not a base (invalid YAML, not a mapping); why.
    Invalid(SharedString),
}

/// Where the base's text comes from.
#[derive(Debug, Clone)]
enum Source {
    File(PathBuf),
    Embedded,
}

/// The theme colours the view draws with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Colors {
    pub page: Hsla,
    pub header: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub secondary: Hsla,
    pub tertiary: Hsla,
    pub accent: Hsla,
    pub accent_tint: Hsla,
    pub hover: Hsla,
    pub chip: Hsla,
    pub error: Hsla,
    pub warning: Hsla,
    pub warning_tint: Hsla,
    pub info_tint: Hsla,
    pub tooltip_bg: Hsla,
    pub tooltip_fg: Hsla,
}

impl Colors {
    pub fn of(theme: &Theme) -> Self {
        Self {
            page: theme.surface.page.into(),
            header: theme.surface.panel.into(),
            border: theme.border.default.into(),
            border_strong: theme.border.strong.into(),
            text: theme.text.primary.into(),
            secondary: theme.text.secondary.into(),
            tertiary: theme.text.tertiary.into(),
            accent: theme.accent.default.into(),
            accent_tint: theme.accent.tint.into(),
            hover: theme.state.hover.into(),
            chip: theme.surface.sunken.into(),
            error: theme.status.error.into(),
            warning: theme.status.warning.into(),
            warning_tint: theme.status.warning_tint.into(),
            info_tint: theme.status.info_tint.into(),
            tooltip_bg: theme.text.primary.into(),
            tooltip_fg: theme.surface.page.into(),
        }
    }
}

/// The local time zone's offset from UTC, in minutes east, now.
fn local_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

/// `path` as a vault path below `root` (`/` separators); the file name
/// without a root or outside it.
fn vault_path(path: &Path, root: Option<&Path>) -> Arc<str> {
    let rel = root.and_then(|r| path.strip_prefix(r).ok());
    let parts: Vec<String> = match rel {
        Some(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect(),
        None => path
            .file_name()
            .map(|n| vec![n.to_string_lossy().into_owned()])
            .unwrap_or_default(),
    };
    Arc::from(parts.join("/"))
}

/// The Bases view of one tab (or of one embed).
pub struct BasesView {
    focus: FocusHandle,
    source: Option<Source>,
    vault_root: Option<PathBuf>,
    /// The file `this` refers to.
    this: Option<Arc<str>>,
    /// `this` was given ([`set_this`](Self::set_this)) and does not follow
    /// the base file.
    this_given: bool,
    status: BasesStatus,
    compiled: Option<Arc<CompiledBase>>,
    /// The view shown (an index into the base's views).
    view: usize,
    /// The view to show once the base is read (`![[x.base#View]]`).
    wanted_view: Option<String>,
    data: Option<BaseData>,
    result: Option<Arc<ViewResult>>,
    evaluating: bool,
    eval_generation: u64,
    data_generation: u64,
    cancel: Option<Arc<AtomicBool>>,
    utc_offset: i32,
    /// The monospace family (problem locations).
    mono: SharedString,
    problems_open: bool,
    /// The table's (and the list's) lines for the shown result.
    lines: Rc<Vec<Line>>,
    /// Each line's position among the rows (the list's item numbers).
    row_numbers: Rc<Vec<Option<usize>>>,
    /// The keyboard's row (a line of `lines`) and table column
    /// ([`cursor`]), and the row's file, which it follows when the base
    /// is evaluated again.
    cursor: Option<(usize, usize)>,
    cursor_path: Option<Arc<str>>,
    table_scroll: UniformListScrollHandle,
    list_scroll: UniformListScrollHandle,
    cards_list: ListState,
    /// The cards' lines for the shown result at the last width.
    card_lines: Rc<Vec<cards::CardLine>>,
    card_columns: usize,
    /// The body's width at the last paint.
    width: Rc<Cell<f32>>,
    covers: Covers,
    /// Evaluations finished (tests).
    evaluations: usize,
    /// Rows drawn in the last frame (tests: virtualization).
    rows_drawn: Rc<Cell<usize>>,
    _load: Option<Task<()>>,
    _eval: Option<Task<()>>,
    _index: Option<Task<()>>,
    _load_once: Subscription,
}

impl std::fmt::Debug for BasesView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasesView")
            .field("source", &self.source)
            .field("status", &self.status)
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<BasesEvent> for BasesView {}

impl Focusable for BasesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for BasesView {
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
    }
}

impl BasesView {
    /// A Bases view showing the `.base` file at `path`. Its `this` and the
    /// covers' paths are relative to `vault_root` (the file's folder
    /// without a vault). Results come once the window gives the files
    /// ([`set_content`](Self::set_content)).
    pub fn open(path: PathBuf, vault_root: Option<PathBuf>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let mut view = Self::new(cx);
            view.vault_root = vault_root;
            view.load(path, cx);
            view
        })
    }

    /// A base embedded in a note (a `base` block, or `![[x.base#View]]`
    /// with the file's text): `this` is the embedding note's vault path,
    /// `view` the view named after `#`.
    pub fn embedded(
        text: Arc<str>,
        this: Option<Arc<str>>,
        view: Option<String>,
        vault_root: Option<PathBuf>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let mut v = Self::new(cx);
            v.vault_root = vault_root;
            v.this = this;
            v.wanted_view = view;
            v.source = Some(Source::Embedded);
            v.finish_load(Ok(text), cx);
            v
        })
    }

    /// A base shown in a sidebar panel: `this` is `active` (the vault path
    /// of the file in the main area), changed with
    /// [`set_this`](Self::set_this) as the active file changes.
    pub fn sidebar(
        path: PathBuf,
        vault_root: Option<PathBuf>,
        active: Option<Arc<str>>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let mut view = Self::new(cx);
            view.vault_root = vault_root;
            view.this = active;
            view.this_given = true;
            view.load(path, cx);
            view
        })
    }

    /// Sets the file `this` refers to (a sidebar base's active file) and
    /// evaluates again when it changed. From then on `this` no longer
    /// follows the base file.
    pub fn set_this(&mut self, this: Option<Arc<str>>, cx: &mut Context<'_, Self>) {
        self.this_given = true;
        if this != self.this {
            self.this = this;
            self.evaluate(cx);
            cx.notify();
        }
    }

    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            source: None,
            vault_root: None,
            this: None,
            this_given: false,
            status: BasesStatus::Empty,
            compiled: None,
            view: 0,
            wanted_view: None,
            data: None,
            result: None,
            evaluating: false,
            eval_generation: 0,
            data_generation: 0,
            cancel: None,
            utc_offset: local_offset_minutes(),
            mono: crate::inline::FontFamilies::from_app(cx).mono,
            problems_open: false,
            lines: Rc::default(),
            row_numbers: Rc::default(),
            cursor: None,
            cursor_path: None,
            table_scroll: UniformListScrollHandle::new(),
            list_scroll: UniformListScrollHandle::new(),
            cards_list: ListState::new(0, ListAlignment::Top, px(400.0)),
            card_lines: Rc::default(),
            card_columns: 0,
            width: Rc::new(Cell::new(0.0)),
            covers: Covers::default(),
            evaluations: 0,
            rows_drawn: Rc::new(Cell::new(0)),
            _load: None,
            _eval: None,
            _index: None,
            _load_once: cx.observe_global::<crate::reader::remote::LoadOnce>(|this, cx| {
                this.approvals_changed(cx);
            }),
        }
    }

    /// Reads and shows `path` (again, after a change on disk: the view
    /// shown stays when the base still has it).
    pub fn load(&mut self, path: PathBuf, cx: &mut Context<'_, Self>) {
        if !self.this_given && !matches!(&self.source, Some(Source::File(p)) if *p == path) {
            self.this = Some(vault_path(&path, self.vault_root.as_deref()));
        }
        self.source = Some(Source::File(path.clone()));
        self.status = BasesStatus::Loading;
        self.covers.clear();
        self._load = Some(cx.spawn(async move |this, cx| {
            let text = cx
                .background_spawn(async move {
                    std::fs::read_to_string(&path)
                        .map(Arc::<str>::from)
                        .map_err(|e| format!("Cannot read the file: {e}"))
                })
                .await;
            this.update(cx, |view, cx| view.finish_load(text, cx)).ok();
        }));
        cx.notify();
    }

    fn finish_load(&mut self, text: Result<Arc<str>, String>, cx: &mut Context<'_, Self>) {
        self._load = None;
        let text = match text {
            Ok(text) => text,
            Err(why) => {
                tracing::warn!("cannot show base: {why}");
                self.compiled = None;
                self.result = None;
                self.status = BasesStatus::Unreadable(why.into());
                cx.notify();
                return;
            }
        };
        match CompiledBase::parse(&text) {
            Ok(base) => {
                let shown_name = self
                    .compiled
                    .as_ref()
                    .and_then(|b| b.base().views.get(self.view))
                    .and_then(|v| v.name.clone());
                let views = &base.base().views;
                self.view = self
                    .wanted_view
                    .take()
                    .or(shown_name)
                    .and_then(|name| base.base().view_index(&name))
                    .unwrap_or(if self.view < views.len() {
                        self.view
                    } else {
                        0
                    });
                self.compiled = Some(Arc::new(base));
                self.status = BasesStatus::Ready;
                self.evaluate(cx);
            }
            Err(e) => {
                self.compiled = None;
                self.result = None;
                self.status = BasesStatus::Invalid(e.to_string().into());
            }
        }
        cx.notify();
    }

    /// Lists the files of `snapshot` (the window's content index): the
    /// base's view of it is built on the low-priority pool, then the base
    /// is evaluated again. A newer snapshot given meanwhile wins.
    pub fn set_content(&mut self, snapshot: ContentSnapshot, cx: &mut Context<'_, Self>) {
        self.data_generation += 1;
        let generation = self.data_generation;
        let offset = self.utc_offset;
        self._index = Some(cx.spawn(async move |this, cx| {
            let data = cx
                .background_executor()
                .spawn_with_priority(gpui::Priority::Low, async move {
                    Arc::new(IndexData::new(snapshot, offset)) as BaseData
                })
                .await;
            this.update(cx, |view, cx| {
                if view.data_generation == generation {
                    view.data = Some(data);
                    view.evaluate(cx);
                }
            })
            .ok();
        }));
    }

    /// Lists the files of `data` and evaluates again.
    pub fn set_data(&mut self, data: BaseData, cx: &mut Context<'_, Self>) {
        self.data_generation += 1;
        self._index = None;
        self.data = Some(data);
        self.evaluate(cx);
    }

    /// Sets the time zone dates are read and shown in (minutes east of
    /// UTC; the system's by default).
    pub fn set_utc_offset(&mut self, minutes: i32, cx: &mut Context<'_, Self>) {
        if minutes != self.utc_offset {
            self.utc_offset = minutes;
            self.evaluate(cx);
        }
    }

    /// Evaluates the shown view again, cancelling a run in progress.
    fn evaluate(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        let (Some(base), Some(data)) = (self.compiled.clone(), self.data.clone()) else {
            self.evaluating = false;
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.eval_generation += 1;
        let generation = self.eval_generation;
        let mut ctx = EvalContext::now(self.utc_offset);
        ctx.this = self.this.clone();
        ctx.cancel = Some(cancel);
        let view = self.view;
        self.evaluating = true;
        self._eval = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn_with_priority(gpui::Priority::Low, async move {
                    // The budget starts when the run does, not while it
                    // waits for the pool.
                    ctx.deadline = Some(Instant::now() + EVAL_BUDGET);
                    base.evaluate(view, &*data, &ctx)
                })
                .await;
            this.update(cx, |v, cx| v.finish_eval(generation, result, cx))
                .ok();
        }));
        cx.notify();
    }

    fn finish_eval(&mut self, generation: u64, result: ViewResult, cx: &mut Context<'_, Self>) {
        if generation != self.eval_generation || result.incomplete == Some(Incomplete::Cancelled) {
            return;
        }
        self.evaluating = false;
        self._eval = None;
        self.evaluations += 1;
        self.lines = Rc::new(table::lines(&result));
        self.row_numbers = Rc::new(table::row_numbers(&self.lines));
        self.cursor = self.cursor_path.as_ref().and_then(|path| {
            let line = self.lines.iter().position(
                |l| matches!(*l, Line::Row(g, r) if result.groups[g].rows[r].path == *path),
            )?;
            let column = self
                .cursor
                .map_or(0, |(_, c)| c.min(result.columns.len().saturating_sub(1)));
            Some((line, column))
        });
        if self.cursor.is_none() {
            self.cursor_path = None;
        }
        self.result = Some(Arc::new(result));
        self.card_columns = 0;
        cx.notify();
    }

    /// The keyboard's row and table column, once a key has moved it.
    #[cfg(test)]
    pub(crate) fn cursor(&self) -> Option<(usize, usize)> {
        self.cursor
    }

    /// The file of line `line`, when it is a row.
    fn line_path(&self, line: usize) -> Option<Arc<str>> {
        let result = self.result.as_ref()?;
        match self.lines.get(line)? {
            Line::Row(g, r) => Some(result.groups[*g].rows[*r].path.clone()),
            _ => None,
        }
    }

    /// Whether the shown layout has a keyboard cursor (a table or a list;
    /// cards are Tab stops of their own).
    fn has_cursor_layout(&self) -> bool {
        self.result
            .as_ref()
            .is_some_and(|r| r.view == self.view && r.matched > 0 && r.kind != ViewKind::Cards)
    }

    /// Moves the keyboard's row; `false` when there is nothing to move.
    fn move_row(&mut self, step: cursor::Step, cx: &mut Context<'_, Self>) -> bool {
        if !self.has_cursor_layout() {
            return false;
        }
        let Some(line) = cursor::step_row(&self.lines, self.cursor.map(|(l, _)| l), step) else {
            return false;
        };
        let column = self.cursor.map_or(0, |(_, c)| c);
        self.cursor = Some((line, column));
        self.cursor_path = self.line_path(line);
        let scroll = if self
            .result
            .as_ref()
            .is_some_and(|r| r.kind == ViewKind::List)
        {
            &self.list_scroll
        } else {
            &self.table_scroll
        };
        scroll.scroll_to_item(line, gpui::ScrollStrategy::Nearest);
        cx.notify();
        true
    }

    /// Moves the keyboard's column in a table; `false` outside a table or
    /// before the first row move.
    fn move_column(&mut self, forward: bool, cx: &mut Context<'_, Self>) -> bool {
        let Some(result) = self.result.as_ref() else {
            return false;
        };
        if !self.has_cursor_layout() || result.kind == ViewKind::List {
            return false;
        }
        let Some((line, column)) = self.cursor else {
            return false;
        };
        self.cursor = Some((
            line,
            cursor::step_column(column, result.columns.len(), forward),
        ));
        cx.notify();
        true
    }

    /// Opens the keyboard row's file; `false` without one.
    fn open_row(&mut self, disposition: Disposition, cx: &mut Context<'_, Self>) -> bool {
        if !self.has_cursor_layout() {
            return false;
        }
        let Some(path) = self.cursor.and_then(|(line, _)| self.line_path(line)) else {
            return false;
        };
        self.open_file(&path, disposition, cx);
        true
    }

    /// The `.base` file shown (none for an embedded base).
    pub fn path(&self) -> Option<&Path> {
        match &self.source {
            Some(Source::File(p)) => Some(p),
            _ => None,
        }
    }

    /// Sets the vault the base's paths (`this`, covers, opened files) are
    /// relative to; evaluates again when `this` changes.
    pub fn set_vault_root(&mut self, root: Option<PathBuf>, cx: &mut Context<'_, Self>) {
        if root == self.vault_root {
            return;
        }
        self.vault_root = root;
        self.covers.clear();
        if let Some(Source::File(path)) = &self.source
            && !self.this_given
        {
            self.this = Some(vault_path(path, self.vault_root.as_deref()));
            self.evaluate(cx);
        }
        cx.notify();
    }

    pub fn status(&self) -> &BasesStatus {
        &self.status
    }

    /// The shown result, once evaluated.
    pub fn result(&self) -> Option<&ViewResult> {
        self.result.as_deref()
    }

    /// The compiled base, once read.
    pub fn base(&self) -> Option<&CompiledBase> {
        self.compiled.as_deref()
    }

    /// The view shown (an index into the base's views).
    pub fn view_index(&self) -> usize {
        self.view
    }

    /// The file `this` refers to.
    pub fn this(&self) -> Option<&str> {
        self.this.as_deref()
    }

    /// Whether an evaluation is running.
    pub fn is_evaluating(&self) -> bool {
        self.evaluating
    }

    /// Shows view `index` of the base.
    pub fn select_view(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let count = self.compiled.as_ref().map_or(0, |b| b.base().views.len());
        if index < count && index != self.view {
            self.view = index;
            self.cursor = None;
            self.cursor_path = None;
            self.table_scroll = UniformListScrollHandle::new();
            self.list_scroll = UniformListScrollHandle::new();
            self.cards_list = ListState::new(0, ListAlignment::Top, px(400.0));
            self.card_columns = 0;
            self.evaluate(cx);
        }
    }

    /// The shown view's name, once the base is read (`None` for a view
    /// without one).
    pub fn view_name(&self) -> Option<String> {
        self.compiled
            .as_ref()
            .and_then(|b| b.base().views.get(self.view))
            .and_then(|v| v.name.clone())
    }

    /// Shows the view named `name`: now, or once the base is read (a
    /// restored sidebar base).
    pub fn want_view(&mut self, name: String, cx: &mut Context<'_, Self>) {
        if self.compiled.is_some() {
            self.select_view_named(&name, cx);
        } else {
            self.wanted_view = Some(name);
        }
    }

    /// Shows the view named `name`; `false` when the base has none.
    pub fn select_view_named(&mut self, name: &str, cx: &mut Context<'_, Self>) -> bool {
        match self
            .compiled
            .as_ref()
            .and_then(|b| b.base().view_index(name))
        {
            Some(i) => {
                self.select_view(i, cx);
                true
            }
            None => false,
        }
    }

    fn step_view(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let count = self.compiled.as_ref().map_or(0, |b| b.base().views.len());
        if count > 1 {
            let next = if forward {
                (self.view + 1) % count
            } else {
                (self.view + count - 1) % count
            };
            self.select_view(next, cx);
        }
    }

    /// The shown view's name (its layout without one).
    pub(crate) fn view_label(&self) -> String {
        self.compiled
            .as_ref()
            .and_then(|b| b.base().views.get(self.view))
            .map_or_else(
                || "Results".to_owned(),
                |v| v.name.clone().unwrap_or_else(|| kind_label(&v.kind)),
            )
    }

    fn root(&self) -> PathBuf {
        self.vault_root
            .clone()
            .or_else(|| match &self.source {
                Some(Source::File(p)) => p.parent().map(Path::to_path_buf),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The file of a vault path, as a path on disk.
    fn file_path(&self, vault_path: &str) -> PathBuf {
        let mut p = self.root();
        for part in vault_path.split('/').filter(|s| !s.is_empty()) {
            p.push(part);
        }
        p
    }

    /// Asks the window to open the file at `vault_path`.
    pub(crate) fn open_file(
        &mut self,
        vault_path: &str,
        disposition: Disposition,
        cx: &mut Context<'_, Self>,
    ) {
        let path = self.file_path(vault_path);
        cx.emit(BasesEvent::OpenFile {
            path,
            fragment: None,
            disposition,
        });
    }

    /// Asks the window to follow a link written in the base.
    pub(crate) fn follow(
        &mut self,
        destination: Arc<str>,
        kind: gv_model::LinkKind,
        disposition: Disposition,
        cx: &mut Context<'_, Self>,
    ) {
        cx.emit(BasesEvent::FollowLink(LinkRequest {
            destination,
            kind,
            disposition,
            range: None,
            from: None,
            at: None,
        }));
    }

    /// The problems of the base and of the shown result, as (where, what)
    /// lines.
    pub fn problems(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(base) = &self.compiled {
            for d in &base.base().diagnostics {
                out.push((d.at.clone(), d.message.clone()));
            }
            for (at, src, e) in base.syntax_errors() {
                out.push((at, format!("Syntax error in `{src}`: {e}")));
            }
        }
        if let Some(r) = &self.result {
            for d in &r.diagnostics {
                // Syntax errors are listed once, above.
                if d.message.starts_with("Syntax error") {
                    continue;
                }
                let files = if d.count == 1 {
                    "1 file".to_owned()
                } else {
                    format!("{} files", d.count)
                };
                out.push((d.source.clone(), format!("{} ({files})", d.message)));
            }
        }
        out
    }

    // ----- drawing ------------------------------------------------------------------

    fn render_tabs(&mut self, colors: &Colors, cx: &mut Context<'_, Self>) -> AnyElement {
        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .h(px(TOOLBAR))
            .px(px(8.0))
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.header);
        let theme = Appearance::theme(cx);
        let mut tabs = div()
            .id("bases-view-tabs")
            .a11y_group(Role::TabList, "Views")
            .flex()
            .items_center()
            .gap(px(2.0))
            .min_w_0();
        if let Some(base) = &self.compiled {
            let count = base.base().views.len();
            for (i, view) in base.base().views.iter().enumerate() {
                let label: SharedString = view
                    .name
                    .clone()
                    .unwrap_or_else(|| kind_label(&view.kind))
                    .into();
                let active = i == self.view;
                let hover = colors.hover;
                tabs = tabs.child(
                    div()
                        .id(("bases-view-tab", i))
                        .debug_selector(move || format!("bases-view-tab:{i}"))
                        .a11y_tab(label.clone(), active, i, count)
                        .keyboard_control(theme)
                        .h(px(22.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .rounded(px(radius::CONTROL))
                        .text_size(px(text::UI.size))
                        .when(active, |d| {
                            d.bg(colors.page)
                                .text_color(colors.text)
                                .font_weight(FontWeight::MEDIUM)
                        })
                        .when(!active, |d| {
                            d.text_color(colors.secondary).hover(move |s| s.bg(hover))
                        })
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| this.select_view(i, cx)))
                        .accessible(),
                );
            }
        }
        row = row.child(tabs.accessible()).child(div().flex_1());
        if self.evaluating {
            row = row.child(
                div()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(colors.tertiary)
                    .mr(px(8.0))
                    .child("Updating…"),
            );
        }
        if let Some(r) = &self.result {
            let count = count_text(r);
            row = row.child(
                div()
                    .id("bases-count")
                    .debug_selector(|| "bases-count".into())
                    .role(Role::Status)
                    .aria_label(count.clone())
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(colors.secondary)
                    .child(count)
                    .accessible()
                    .live(Live::Polite),
            );
        }
        let problems = self.problems().len();
        if problems > 0 {
            let open = self.problems_open;
            let label = if problems == 1 {
                "1 problem".to_owned()
            } else {
                format!("{problems} problems")
            };
            row = row.child(
                div()
                    .id("bases-problems-chip")
                    .debug_selector(|| "bases-problems-chip".into())
                    .a11y_button(label.clone())
                    .aria_expanded(open)
                    .keyboard_control(theme)
                    .ml(px(8.0))
                    .h(px(20.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .rounded(px(radius::PILL))
                    .bg(if open {
                        colors.warning_tint
                    } else {
                        colors.chip
                    })
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(colors.warning)
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.problems_open = !this.problems_open;
                        cx.notify();
                    }))
                    .accessible(),
            );
        }
        row.into_any_element()
    }

    fn render_problems(&self, colors: &Colors) -> AnyElement {
        let problems = self.problems();
        let count = problems.len();
        let mut panel = div()
            .id("bases-problems")
            .debug_selector(|| "bases-problems".into())
            .a11y_group(Role::List, "Problems")
            .flex_none()
            .max_h(px(180.0))
            .overflow_y_scroll()
            .px(px(12.0))
            .py(px(6.0))
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.warning_tint)
            .text_size(px(text::UI_SMALL.size))
            .flex()
            .flex_col()
            .gap(px(2.0));
        for (i, (at, what)) in problems.into_iter().enumerate() {
            panel = panel.child(
                div()
                    .id(("bases-problem", i))
                    .a11y_list_item(format!("{at}: {what}"), i, count)
                    .flex()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_none()
                            .font_family(self.mono.clone())
                            .text_color(colors.secondary)
                            .child(at),
                    )
                    .child(div().text_color(colors.text).child(what))
                    .accessible(),
            );
        }
        panel.accessible().into_any_element()
    }

    fn render_body(&mut self, colors: &Colors, cx: &mut Context<'_, Self>) -> AnyElement {
        let note = |text: SharedString| centered_note(text, colors);
        match &self.status {
            BasesStatus::Empty | BasesStatus::Loading => return note("Loading the base…".into()),
            BasesStatus::Unreadable(why) => {
                return note(format!("This base cannot be shown. {why}").into());
            }
            BasesStatus::Invalid(why) => {
                return note(format!("This base cannot be read: {why}").into());
            }
            BasesStatus::Ready => {}
        }
        let Some(result) = self.result.clone().filter(|r| r.view == self.view) else {
            return if self.data.is_none() {
                note("Waiting for the vault index…".into())
            } else {
                note("Evaluating…".into())
            };
        };
        let mut body = div().flex().flex_col().flex_1().min_h_0().w_full();
        if let Some(notice) = &result.notice {
            body = body.child(notice_bar(notice.clone(), colors.info_tint, colors));
        }
        match result.incomplete {
            Some(Incomplete::TimedOut) => {
                body = body.child(notice_bar(
                    "The base took too long to evaluate; the results are incomplete.".into(),
                    colors.warning_tint,
                    colors,
                ));
            }
            Some(Incomplete::RowCap) => {
                body = body.child(notice_bar(
                    format!(
                        "More files match than can be listed; the first {} are shown.",
                        result.matched
                    ),
                    colors.warning_tint,
                    colors,
                ));
            }
            _ => {}
        }
        if result.matched == 0 {
            return body
                .child(note("No files match this view's filters.".into()))
                .into_any_element();
        }
        let content = match result.kind {
            ViewKind::Cards => self.render_cards(&result, colors, cx),
            ViewKind::List => self.render_list(&result, colors, cx),
            _ => self.render_table(&result, colors, cx),
        };
        body.child(content).into_any_element()
    }
}

/// The toolbar's height.
const TOOLBAR: f32 = 32.0;

/// A view's tab label without a name.
fn kind_label(kind: &ViewKind) -> String {
    let s = kind.as_str();
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// "12 results", or "10 of 12 results" when a limit hides some.
pub(crate) fn count_text(r: &ViewResult) -> String {
    let noun = |n: usize| if n == 1 { "result" } else { "results" };
    if r.shown < r.matched {
        format!("{} of {} {}", r.shown, r.matched, noun(r.matched))
    } else {
        format!("{} {}", r.matched, noun(r.matched))
    }
}

fn notice_bar(text: String, bg: Hsla, colors: &Colors) -> AnyElement {
    div()
        .debug_selector(|| "bases-notice".into())
        .flex_none()
        .px(px(12.0))
        .py(px(6.0))
        .bg(bg)
        .border_b_1()
        .border_color(colors.border)
        .text_size(px(text::UI_SMALL.size))
        .text_color(colors.text)
        .child(text)
        .into_any_element()
}

fn centered_note(text: SharedString, colors: &Colors) -> AnyElement {
    div()
        .debug_selector(|| "bases-empty".into())
        .flex_1()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .text_color(colors.secondary)
        .child(text)
        .into_any_element()
}

impl Render for BasesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let colors = Colors::of(theme);
        self.rows_drawn.set(0);
        let embedded = matches!(self.source, Some(Source::Embedded));
        let name = match self.path().and_then(Path::file_stem) {
            Some(stem) => format!("Base {}", stem.to_string_lossy()),
            None => "Base".to_owned(),
        };
        let mut root = div()
            .id("bases-view")
            .a11y_group(Role::Group, name)
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus.clone().tab_index(0).tab_stop(!embedded))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(colors.page)
            .font_family(Appearance::interface_font(cx))
            .text_size(px(text::UI.size))
            .text_color(colors.text)
            .on_action(cx.listener(|this, _: &NextView, _, cx| this.step_view(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousView, _, cx| this.step_view(false, cx)))
            // The cursor's keys act while the view itself has the keyboard;
            // on a tab, the chip or a card they do what those do.
            .on_action(cx.listener(|this, _: &NextRow, window, cx| {
                this.row_key(cursor::Step::Next, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousRow, window, cx| {
                this.row_key(cursor::Step::Previous, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FirstRow, window, cx| {
                this.row_key(cursor::Step::First, window, cx);
            }))
            .on_action(cx.listener(|this, _: &LastRow, window, cx| {
                this.row_key(cursor::Step::Last, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PageDownRows, window, cx| {
                this.row_key(cursor::Step::PageDown, window, cx);
            }))
            .on_action(cx.listener(|this, _: &PageUpRows, window, cx| {
                this.row_key(cursor::Step::PageUp, window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextColumn, window, cx| {
                if !this.focus.is_focused(window) || !this.move_column(true, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &PreviousColumn, window, cx| {
                if !this.focus.is_focused(window) || !this.move_column(false, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &OpenRow, window, cx| {
                if !this.focus.is_focused(window) || !this.open_row(Disposition::InPlace, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &OpenRowInNewTab, window, cx| {
                if !this.focus.is_focused(window) || !this.open_row(Disposition::NewTab, cx) {
                    cx.propagate();
                }
            }));
        if self.compiled.is_some() {
            root = root.child(self.render_tabs(&colors, cx));
            if self.problems_open {
                root = root.child(self.render_problems(&colors));
            }
        }
        root.child(self.render_body(&colors, cx)).accessible()
    }
}

impl BasesView {
    /// A row key: moves the cursor while the view has the keyboard, else
    /// lets the key go on.
    fn row_key(&mut self, step: cursor::Step, window: &Window, cx: &mut Context<'_, Self>) {
        if !self.focus.is_focused(window) || !self.move_row(step, cx) {
            cx.propagate();
        }
    }
}

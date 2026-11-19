// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The main window: title row, side panels, document area and status bar.

mod base_panel;
#[cfg(test)]
mod base_panel_tests;
mod bases_tabs;
#[cfg(test)]
mod bases_tabs_tests;
mod bookmarks;
mod bookmarks_panel;
#[cfg(test)]
mod bookmarks_tests;
mod canvas_tabs;
#[cfg(test)]
mod canvas_tabs_tests;
#[cfg(test)]
mod command_handlers_tests;
#[cfg(test)]
mod conflict_markers_tests;
mod controls;
mod daily_notes;
#[cfg(test)]
mod daily_notes_tests;
mod document;
mod drag_drop;
#[cfg(test)]
mod drag_drop_tests;
mod expanded_table;
mod export;
#[cfg(test)]
mod export_tests;
mod file_history;
#[cfg(test)]
mod file_history_tests;
mod file_ops;
#[cfg(test)]
mod file_ops_tests;
mod files_panel;
#[cfg(test)]
mod files_panel_tests;
mod git;
#[cfg(test)]
mod git_tests;
mod history;
mod infobar;
mod link_actions;
#[cfg(test)]
mod link_actions_tests;
mod link_checker;
#[cfg(test)]
mod link_checker_tests;
mod link_list;
mod link_preview;
#[cfg(test)]
mod link_preview_tests;
mod links_data;
mod links_panel;
#[cfg(test)]
mod links_panel_tests;
#[cfg(test)]
mod live_content_tests;
mod menus;
#[cfg(test)]
mod menus_tests;
#[cfg(test)]
mod navigation_tests;
mod outline_panel;
mod panels;
mod panes;
#[cfg(test)]
mod panes_tests;
mod profile_chip;
#[cfg(test)]
mod profile_chip_tests;
mod recent_panel;
mod saving;
mod search;
#[cfg(test)]
mod search_tests;
pub mod session;
mod status_bar;
mod switcher;
#[cfg(test)]
mod switcher_tests;
mod sync_conflicts;
#[cfg(test)]
mod sync_conflicts_tests;
mod tags;
#[cfg(test)]
mod tags_tests;
#[cfg(test)]
mod tests;
mod title_row;
mod tree;
mod trust_prompt;
#[cfg(test)]
mod trust_prompt_tests;
mod vault;
mod vault_ops;
#[cfg(test)]
mod vault_ops_tests;
mod viewer_tabs;
#[cfg(test)]
mod viewer_tabs_tests;

#[cfg(test)]
pub(crate) use vault::VaultStatus;

#[cfg(test)]
pub(crate) use controls::ClientDecorationsForTests;
pub(crate) use controls::icon_button;
pub(crate) use files_panel::key_bindings as file_tree_key_bindings;
pub(crate) use files_panel::{list_row_frame, message as panel_message};

/// The file tree's keys and key contexts, shared by the sidebar's lists.
pub(crate) mod tree_keys {
    pub(crate) use super::files_panel::{
        ClearFilter, CollapseOrParent, ExpandOrChild, FILTER_CONTEXT, Filter, FocusRows,
        KEY_CONTEXT, OpenSelected, SelectNext, SelectPrevious,
    };
}
pub(crate) use infobar::wrapping_row;
use saving::same_file;
pub(crate) use saving::{ask_before_quit, discard_for_quit, prepare_quit};
pub(crate) use status_bar::count_words;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::channel::oneshot;

use gpui::component::window_corner_radii;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, EntityId, FocusHandle, Focusable as _,
    Global, InteractiveElement as _, IntoElement, MouseButton, NavigationDirection,
    ParentElement as _, Render, Result, SharedString, Styled as _, Subscription, Task, WeakEntity,
    Window, WindowBounds, canvas, div, px,
};
use gv_model::DocModel;
use serde::{Deserialize, Serialize};

use crate::a11y::{A11yExt as _, AccessibleExt as _};
use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::editor::{EditorFacts, EditorView};
use crate::files::notice::Notice;
use crate::files::watch::WatchHandle;
use crate::keymap::actions::view::{ToggleLivePreview, ToggleSplit, ToggleViewerMode};
use crate::keymap::actions::{file as file_commands, tabs as tab_commands};
use crate::metrics::text;
use crate::reader::follow::{self, Cause};
use crate::reader::{LinkRequest, LoadStatus, Reader, ReaderEvent, ScrollTarget};
use crate::split::SplitView;
use crate::{
    AppSettings, CloseTab, GoBack, GoForward, ReopenClosedTab, ToggleEditMode, TogglePanel,
    ToggleSidebar,
};
use files_panel::{FilesPanel, FilesPanelEvent};
use history::{ClosedTab, ClosedTabs};
use outline_panel::{OutlinePanel, OutlinePanelEvent};
use vault::WindowVault;

pub use history::{History, HistoryEntry};

/// Set to `1` to open main windows with the sidebar shown, for screenshots
/// and manual checks. Windows opened for a file otherwise start with both
/// side panels hidden.
pub const DEBUG_SHOW_SIDEBAR_ENV: &str = "GLYPHVANE_DEBUG_SHOW_SIDEBAR";

/// Set to `1` to open main windows with the right panel (Outline) shown,
/// for screenshots and manual checks.
pub const DEBUG_SHOW_RIGHT_PANEL_ENV: &str = "GLYPHVANE_DEBUG_SHOW_RIGHT_PANEL";

/// Set to `1` to open main windows in Edit mode, for screenshots and
/// manual checks.
pub const DEBUG_EDIT_ENV: &str = "GLYPHVANE_DEBUG_EDIT";

/// Set to `1` to open main windows in Split mode, for screenshots and
/// manual checks.
pub const DEBUG_SPLIT_ENV: &str = "GLYPHVANE_DEBUG_SPLIT";

/// What the document area shows. Each tab has its own mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    #[default]
    Preview,
    Edit,
    Split,
}

/// A place in a document: a 1-based line and, optionally, a 1-based column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextPosition {
    pub line: u32,
    pub column: Option<u32>,
}

/// A file to open, and where to show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTarget {
    pub path: PathBuf,
    pub position: Option<TextPosition>,
}

impl OpenTarget {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            position: None,
        }
    }

    pub fn at(mut self, position: TextPosition) -> Self {
        self.position = Some(position);
        self
    }
}

/// What a tab's [`CloseHook`] is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileClosed {
    pub path: PathBuf,
    /// The document had changes that are not in the file: autosave is off,
    /// held back, or could not write. They stay in the recovery journal.
    pub unsaved: bool,
}

/// Called once when a tab lets go of its file: the tab closes, its window
/// closes, the tab goes on to show another file, or the application quits.
/// When closing saves the document (autosave), the hook is called once that
/// save has finished, so the file on disk is final by then.
#[derive(Clone)]
pub struct CloseHook(Rc<RefCell<Option<CloseFn>>>);

type CloseFn = Box<dyn FnOnce(FileClosed, &mut App)>;

impl CloseHook {
    pub fn new(hook: impl FnOnce(FileClosed, &mut App) + 'static) -> Self {
        Self(Rc::new(RefCell::new(Some(Box::new(hook)))))
    }

    /// Calls the hook, the first time only.
    fn fire(&self, closed: FileClosed, cx: &mut App) {
        let hook = self.0.borrow_mut().take();
        if let Some(hook) = hook {
            hook(closed, cx);
        }
    }

    fn is(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for CloseHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pending = self.0.borrow().is_some();
        f.debug_struct("CloseHook")
            .field("pending", &pending)
            .finish()
    }
}

/// How long a closed document's save may take before its hook is called
/// anyway (as unsaved).
const CLOSE_HOOK_WAIT: Duration = crate::files::CLOSE_WAIT;

/// How often a closed document's save is looked at.
const CLOSE_HOOK_POLL: Duration = Duration::from_millis(10);

/// Documents whose tab let go of them while their save was still running:
/// their hooks are called when it ends, or by [`release_close_hooks`].
#[derive(Default)]
struct ClosingFiles(Vec<(CloseHook, PathBuf, Entity<EditorView>)>);

impl Global for ClosingFiles {}

/// Tells `hook` that its tab let go of `path`. `editor` is the tab's Source
/// view, already asked to save what autosave may save; when that save is
/// still running, the hook waits for it.
fn let_go(hook: CloseHook, path: PathBuf, editor: Option<Entity<EditorView>>, cx: &mut App) {
    let Some(editor) = editor else {
        hook.fire(
            FileClosed {
                path,
                unsaved: false,
            },
            cx,
        );
        return;
    };
    if !editor.read(cx).is_saving() {
        let unsaved = editor.read(cx).is_modified();
        hook.fire(FileClosed { path, unsaved }, cx);
        return;
    }
    cx.default_global::<ClosingFiles>()
        .0
        .push((hook.clone(), path.clone(), editor.clone()));
    cx.spawn(async move |cx| {
        let deadline = Instant::now() + CLOSE_HOOK_WAIT;
        while cx.update(|cx| editor.read(cx).is_saving()) && Instant::now() < deadline {
            cx.background_executor().timer(CLOSE_HOOK_POLL).await;
        }
        cx.update(|cx| {
            cx.default_global::<ClosingFiles>()
                .0
                .retain(|(h, ..)| !h.is(&hook));
            let unsaved = editor.read(cx).is_modified();
            hook.fire(FileClosed { path, unsaved }, cx);
        });
    })
    .detach();
}

/// Calls every close hook still waiting, for the application quitting: the
/// tabs that are still open let go of their files now. Their documents
/// were saved by [`crate::files::quit`]; a save still running is waited
/// for here, blocking, for at most [`CLOSE_HOOK_WAIT`].
pub fn release_close_hooks(cx: &mut App) {
    let workspaces: Vec<_> = cx
        .default_global::<RecentWindows>()
        .0
        .iter()
        .filter_map(|w| w.workspace.upgrade())
        .collect();
    let mut hooks: Vec<_> = workspaces
        .into_iter()
        .flat_map(|ws| ws.update(cx, |ws, _| ws.take_close_hooks()))
        .collect();
    hooks.extend(
        std::mem::take(&mut cx.default_global::<ClosingFiles>().0)
            .into_iter()
            .map(|(hook, path, editor)| (hook, path, Some(editor))),
    );
    for (hook, path, editor) in hooks {
        let unsaved = editor.is_some_and(|editor| {
            let wait = editor.update(cx, |e, cx| e.prepare_close(cx));
            if editor.read(cx).is_saving() {
                // The save's reply is handled on this thread, which is
                // blocked: the worker's queue says when it is written.
                !wait.is_some_and(|wait| block_until_done(wait, CLOSE_HOOK_WAIT))
            } else {
                editor.read(cx).is_modified()
            }
        });
        hook.fire(FileClosed { path, unsaved }, cx);
    }
}

/// Blocks until `wait` fires or is dropped, for at most `limit`. Returns
/// whether it did.
fn block_until_done(mut wait: oneshot::Receiver<()>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        match wait.try_recv() {
            Ok(Some(())) | Err(oneshot::Canceled) => return true,
            Ok(None) if Instant::now() >= deadline => return false,
            Ok(None) => std::thread::sleep(CLOSE_HOOK_POLL),
        }
    }
}

/// One open document in a window.
#[derive(Debug, Clone)]
pub struct Tab {
    label: FileLabel,
    /// `None` for the empty tab of a window opened without a file.
    path: Option<PathBuf>,
    /// Where the Reading view was asked to scroll, from `PATH:LINE[:COL]`.
    position: Option<TextPosition>,
    /// Where the Reading view goes when it is made: the anchor of a
    /// restored tab. Kept until the tab shows another place.
    scroll: Option<ScrollTarget>,
    mode: ViewMode,
    /// The Reading view, made when the tab is first shown.
    reader: Option<Entity<Reader>>,
    /// The Source view, made when the tab first goes to Edit. It lives as
    /// long as the tab shows the same file; when it goes, unsaved text is
    /// saved or kept in the crash journal ([`crate::files`]).
    editor: Option<Entity<EditorView>>,
    /// The Split view, made with the Source view when the tab first goes
    /// to Split; it goes with the Source view.
    split: Option<Entity<SplitView>>,
    /// The Canvas view, in place of the Reading view, when the file is a
    /// canvas ([`canvas_tabs`]).
    canvas: Option<Entity<crate::canvas_view::CanvasView>>,
    /// The Bases view, in place of the Reading view, when the file is a
    /// base ([`bases_tabs`]).
    bases: Option<Entity<crate::bases_view::BasesView>>,
    /// The built-in viewer, in place of the Reading view, when the file is
    /// not a note, a canvas or a base ([`viewer_tabs`]).
    viewer: Option<Entity<crate::file_viewer::FileViewer>>,
    /// The number of an untitled document (`tabs.new`), which has no path.
    untitled: Option<u64>,
    /// The tab's own infobar, for what happens to its file while only the
    /// Reading view is open (deleted on disk, a sync-conflict copy).
    notice: Option<Notice>,
    /// Where Back and Forward go.
    history: History,
    /// Told when the tab lets go of its file (`--wait`).
    on_close: Option<CloseHook>,
    /// The tab was opened for an edit (`--wait`): viewer mode does not
    /// apply to it.
    edit_requested: bool,
    /// Viewer mode switched for this tab only (`view.toggle_viewer_mode`):
    /// on or off whatever the setting says. `None` follows the setting.
    viewer_toggle: Option<bool>,
    /// The document's profile, detected for `path` when first needed.
    profile: Option<crate::profile::TabProfile>,
    /// The Split view's divider for when the Split view is made (a
    /// restored tab's).
    split_ratio: Option<f32>,
    /// A restored tab's Source-view folds, for its file, until the Source
    /// view is made.
    restored_folds: Option<(PathBuf, Vec<std::ops::Range<usize>>)>,
    /// A restored tab's Reading-view folds, for its file, until its
    /// Reading view has shown that file.
    restored_reading_folds: Option<(PathBuf, Vec<crate::reader::fold_state::ReadingFold>)>,
    /// A restored tab's Split-preview folds, for its file, until its
    /// Split view's preview has shown that file.
    restored_split_folds: Option<(PathBuf, Vec<crate::reader::fold_state::ReadingFold>)>,
    /// The editing style of Edit mode: Live Preview (`true`) or Source.
    live_preview: bool,
    /// Where the PDF viewer was in this tab's file: restored (given to the
    /// viewer once made), else as last seen.
    pdf_place: Option<(PathBuf, crate::file_viewer::PdfPlace)>,
    /// The conflict-marker warning was dismissed; it comes back once the
    /// markers have gone and come back ([`infobar`]).
    markers_dismissed: std::cell::Cell<bool>,
}

impl Tab {
    fn new(target: Option<OpenTarget>) -> Self {
        let (path, position) = match target {
            Some(target) => (Some(target.path), target.position),
            None => (None, None),
        };
        Self {
            label: FileLabel::new(path.as_deref()),
            path,
            position,
            scroll: None,
            mode: ViewMode::default(),
            reader: None,
            editor: None,
            split: None,
            canvas: None,
            bases: None,
            viewer: None,
            untitled: None,
            notice: None,
            history: History::default(),
            on_close: None,
            edit_requested: false,
            viewer_toggle: None,
            profile: None,
            split_ratio: None,
            restored_folds: None,
            restored_reading_folds: None,
            restored_split_folds: None,
            live_preview: false,
            pdf_place: None,
            markers_dismissed: std::cell::Cell::new(false),
        }
    }

    /// The Source view's folds of the tab's file, as the session keeps
    /// them: the Source view's, else those restored and not applied yet.
    fn folds(&self, cx: &App) -> Vec<std::ops::Range<usize>> {
        let path = self.path.as_deref();
        if let Some(editor) = &self.editor {
            let editor = editor.read(cx);
            if editor.path().is_some() && editor.path() == path {
                return editor.fold_ranges();
            }
        }
        match &self.restored_folds {
            Some((p, folds)) if Some(p.as_path()) == path => folds.clone(),
            _ => Vec::new(),
        }
    }

    /// Where the PDF viewer is in the tab's file, as the session keeps
    /// it: the viewer's once it shows the file, else the one restored.
    fn pdf_place(&self, cx: &App) -> Option<crate::file_viewer::PdfPlace> {
        let path = self.path.as_deref()?;
        if let Some(viewer) = &self.viewer {
            let viewer = viewer.read(cx);
            if viewer.path() == path
                && let Some(place) = viewer.pdf_place(cx)
            {
                return Some(place);
            }
        }
        match &self.pdf_place {
            Some((p, place)) if p == path => Some(*place),
            _ => None,
        }
    }

    /// The Reading view's folds of the tab's file, as the session keeps
    /// them: the Reading view's once it shows the file, else those
    /// restored and not applied yet.
    fn reading_folds(&self, cx: &App) -> Vec<crate::reader::fold_state::ReadingFold> {
        Self::folds_of(
            self.reader.as_ref(),
            self.restored_reading_folds.as_ref(),
            self.path.as_deref(),
            cx,
        )
    }

    /// The Split view preview's folds of the tab's file, as the session
    /// keeps them: the preview's once it shows the file, else those
    /// restored and not applied yet.
    fn split_reading_folds(&self, cx: &App) -> Vec<crate::reader::fold_state::ReadingFold> {
        Self::folds_of(
            self.split_reader(cx).as_ref(),
            self.restored_split_folds.as_ref(),
            self.path.as_deref(),
            cx,
        )
    }

    fn folds_of(
        reader: Option<&Entity<Reader>>,
        restored: Option<&(PathBuf, Vec<crate::reader::fold_state::ReadingFold>)>,
        path: Option<&Path>,
        cx: &App,
    ) -> Vec<crate::reader::fold_state::ReadingFold> {
        if let Some(reader) = reader {
            let reader = reader.read(cx);
            if *reader.status() == LoadStatus::Ready
                && reader.path().is_some()
                && reader.path() == path
            {
                return reader.reading_folds();
            }
        }
        match restored {
            Some((p, folds)) if Some(p.as_path()) == path => folds.clone(),
            _ => Vec::new(),
        }
    }

    /// Untitled document `n`, in Edit mode.
    fn untitled(n: u64) -> Self {
        let mut tab = Self::new(None);
        tab.untitled = Some(n);
        tab.mode = ViewMode::Edit;
        if n > 1 {
            tab.label.name = format!("Untitled {n}").into();
        }
        tab
    }

    /// The empty tab of a window opened without a file.
    fn is_placeholder(&self) -> bool {
        self.path.is_none() && self.untitled.is_none()
    }

    /// The number of an untitled document.
    pub fn untitled_number(&self) -> Option<u64> {
        self.untitled
    }

    /// The tab's own infobar notice.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Whether the tab shows the file at `path` (absolute).
    fn shows(&self, path: &Path) -> bool {
        self.path.as_deref().is_some_and(|p| same_file(p, path))
    }

    pub fn reader(&self) -> Option<&Entity<Reader>> {
        self.reader.as_ref()
    }

    pub fn editor(&self) -> Option<&Entity<EditorView>> {
        self.editor.as_ref()
    }

    pub fn split(&self) -> Option<&Entity<SplitView>> {
        self.split.as_ref()
    }

    /// The Canvas view of a tab that shows a canvas file.
    pub fn canvas(&self) -> Option<&Entity<crate::canvas_view::CanvasView>> {
        self.canvas.as_ref()
    }

    /// Whether the tab shows a canvas file (in the Canvas view).
    fn is_canvas(&self) -> bool {
        self.path.as_deref().is_some_and(canvas_tabs::is_canvas)
    }

    /// The Bases view of a tab that shows a base file.
    pub fn bases(&self) -> Option<&Entity<crate::bases_view::BasesView>> {
        self.bases.as_ref()
    }

    /// Whether the tab shows a base file (in the Bases view).
    fn is_base(&self) -> bool {
        self.path.as_deref().is_some_and(bases_tabs::is_base)
    }

    /// The built-in viewer of a tab that shows a file that is not a note.
    pub fn viewer(&self) -> Option<&Entity<crate::file_viewer::FileViewer>> {
        self.viewer.as_ref()
    }

    /// Whether the tab shows its file in the built-in viewer.
    fn is_viewer(&self) -> bool {
        self.path
            .as_deref()
            .is_some_and(viewer_tabs::is_viewer_file)
    }

    /// Whether the built-in viewer of the tab shows text, which the Source
    /// view may edit in plain-text mode: its viewer shows text or, while it
    /// loads, the file is not an image by its name.
    fn viewer_shows_text(&self, cx: &App) -> bool {
        use crate::file_viewer::{ViewerKind, ViewerStatus};
        match self.viewer.as_ref().map(|v| v.read(cx).status()) {
            Some(ViewerStatus::Text { .. }) => true,
            Some(ViewerStatus::Image(_) | ViewerStatus::Pdf { .. } | ViewerStatus::NotShown(_)) => {
                false
            }
            Some(ViewerStatus::Loading) | None => self.path.as_deref().is_some_and(|p| {
                !matches!(
                    crate::file_viewer::kind(p),
                    Some(ViewerKind::Image | ViewerKind::Pdf)
                )
            }),
        }
    }

    /// Whether the tab shows a file in a view of its own (a canvas, a base
    /// or the built-in viewer) instead of the Reading view.
    fn is_special_view(&self) -> bool {
        self.is_canvas() || self.is_base() || self.is_viewer()
    }

    /// The Split view's preview pane, once the tab has a Split view.
    fn split_reader(&self, cx: &App) -> Option<Entity<Reader>> {
        Some(self.split.as_ref()?.read(cx).reader().clone())
    }

    /// Where Back goes, oldest first.
    pub fn history(&self) -> &[HistoryEntry] {
        self.history.back()
    }

    /// The tab's back and forward stacks.
    pub fn navigation(&self) -> &History {
        &self.history
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn position(&self) -> Option<TextPosition> {
        self.position
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    /// The document's profile, once detected (the status bar detects the
    /// active tab's when it is drawn).
    pub fn profile(&self) -> Option<&crate::profile::TabProfile> {
        self.profile.as_ref()
    }
}

/// How the opened file is named in the tab, the breadcrumb and the window
/// title. The file itself is not read yet.
#[derive(Debug, Clone)]
pub struct FileLabel {
    name: SharedString,
    folder: Option<SharedString>,
}

impl FileLabel {
    pub fn new(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self {
                name: "Untitled".into(),
                folder: None,
            };
        };
        let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
        let name = file_name(path).unwrap_or_else(|| path.display().to_string());
        let folder = std::path::absolute(path)
            .ok()
            .and_then(|abs| abs.parent().and_then(file_name));
        Self {
            name: name.into(),
            folder: folder.map(Into::into),
        }
    }

    pub fn window_title(&self) -> String {
        format!("{} — Glyphvane", self.name)
    }
}

/// What the window knows about the document a Reading view shows, for the
/// outline and the status bar.
#[derive(Debug, Clone)]
pub(super) struct DocFacts {
    path: PathBuf,
    model: DocModel,
    /// Counted off the UI thread; `None` until then.
    words: Option<usize>,
    /// The heading whose section is at the top of the view.
    heading: Option<usize>,
}

impl DocFacts {
    fn heading_text(&self) -> Option<SharedString> {
        let heading = self.model.headings().get(self.heading?)?;
        Some(heading.text.to_string().into())
    }
}

/// A Reading view the window listens to.
struct ReaderWatch {
    /// The loaded document; `None` while one loads.
    doc: Option<DocFacts>,
    /// Whether its document held conflict markers when last looked at.
    markers: bool,
    _count: Option<Task<()>>,
    _events: Subscription,
    _changes: Subscription,
}

/// A main window's content.
pub struct Workspace {
    focus: FocusHandle,
    chrome: WindowChrome,
    window_handle: AnyWindowHandle,
    /// Never empty.
    tabs: Vec<Tab>,
    active: usize,
    /// Side panels start hidden: a window opened for a file shows only the
    /// document until the user calls a panel.
    sidebar_open: bool,
    panel_open: bool,
    /// The window's vault, created when something first needs it: the
    /// sidebar being shown. Until then nothing is indexed.
    vault: Option<Entity<WindowVault>>,
    /// The Files panel, created with the sidebar.
    files: Option<Entity<FilesPanel>>,
    /// The Files panel's sort order, kept with the session.
    files_sort: tree::SortOrder,
    _files_events: Option<Subscription>,
    _vault_changed: Option<Subscription>,
    _content_updates: Option<Subscription>,
    /// Redraws on the vault's Git status changes ([`git`]).
    _git_updates: Option<Subscription>,
    /// The workspace-trust prompt of the window's vault, while it asks
    /// ([`trust_prompt`]).
    trust: Option<(Entity<crate::trust::TrustPrompt>, Subscription)>,
    /// The Outline, created with the right panel.
    outline: Option<Entity<OutlinePanel>>,
    _outline_events: Option<Subscription>,
    /// Backlinks and *Links in this note*, created with the vault or the
    /// right panel.
    links: Option<Entity<links_panel::LinksPanel>>,
    _links_events: Option<Subscription>,
    /// The tabs' Reading views, by entity.
    readers: HashMap<EntityId, ReaderWatch>,
    /// The window is part of the saved session: it was not created for a
    /// file (FR-VIEW-16).
    in_session: bool,
    /// The window's bounds, kept for the session: a closing window no
    /// longer has them.
    bounds: WindowBounds,
    /// The active Reading view's position as last seen, to notice scrolling.
    anchor_seen: Rc<RefCell<Option<ScrollTarget>>>,
    /// Closed tabs, for reopening.
    closed: ClosedTabs,
    /// The vault watcher was asked to start.
    vault_watch_started: bool,
    /// The window's file and vault watchers, made with the first document
    /// (only when [`crate::files::DataSafety`] watches files).
    watch: Option<WatchHandle>,
    _watch_events: Option<Task<()>>,
    /// The Source views' events, by entity.
    editor_events: HashMap<EntityId, Subscription>,
    /// The window waits for saves before it closes.
    closing: bool,
    /// Closing with unsaved changes, compare requests (see `saving`).
    safety: saving::SafetyState,
    /// The pointer went down on a drag region of a client-decorated window
    /// and has not moved yet.
    drag_pending: bool,
    _appearance: Subscription,
    _activation: Subscription,
    _bounds: Subscription,
    /// Calls the close hooks of the tabs that are open when the window goes.
    _release: Subscription,
    /// Viewer mode and the profiles follow the settings.
    _settings: Subscription,
    /// The status bar's profile chip menu.
    profile_menu: profile_chip::ProfileMenu,
    /// The context menu and the More actions button.
    menus: menus::ShellMenus,
    /// Which side panels show.
    panels: panels::Panels,
    /// The Recent panel, created when first shown.
    recent: Option<Entity<recent_panel::RecentPanel>>,
    _recent_events: Option<Subscription>,
    /// The vault's indexing question was asked in this window.
    index_asked: bool,
    /// The profiles follow the overrides picked in any window.
    _profiles: Subscription,
    /// The link popover (create, candidates, trace) and the last ambiguous
    /// link followed.
    link_ux: link_actions::LinkUx,
    /// The expanded table over the content area, while it is open.
    expanded: Option<expanded_table::Expanded>,
    /// The hover previews of links, while open.
    previews: link_preview::HoverPreviews,
    /// The last rename or move, for Undo.
    vault_ops: vault_ops::VaultOps,
    /// The open tabs' focus handles, for the arrow keys of the tab list.
    tab_keys: crate::a11y::RovingTabs,
}

impl fmt::Debug for Workspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Workspace")
            .field("chrome", &self.chrome)
            .field("tabs", &self.tabs)
            .field("active", &self.active)
            .field("sidebar_open", &self.sidebar_open)
            .field("panel_open", &self.panel_open)
            .field("vault", &self.vault)
            .field("in_session", &self.in_session)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    /// A workspace with one tab: `target`, or an empty tab.
    pub fn new(
        target: Option<OpenTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self::with_chrome(target, WindowChrome::current(), window, cx);
        if std::env::var_os(DEBUG_SHOW_SIDEBAR_ENV).is_some_and(|v| v == "1") {
            this.set_sidebar_open(true, window, cx);
        }
        if std::env::var_os(DEBUG_SHOW_RIGHT_PANEL_ENV).is_some_and(|v| v == "1") {
            this.set_panel_open(true, window, cx);
        }
        if std::env::var_os(DEBUG_EDIT_ENV).is_some_and(|v| v == "1") {
            this.set_mode(ViewMode::Edit, cx);
        }
        if std::env::var_os(DEBUG_SPLIT_ENV).is_some_and(|v| v == "1") {
            this.set_mode(ViewMode::Split, cx);
        }
        this
    }

    /// A workspace drawn with another platform's chrome. Only the window's
    /// own platform behaves natively; the others are for layout tests.
    pub fn with_chrome(
        target: Option<OpenTarget>,
        chrome: WindowChrome,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        // A window created for a file stays out of the session (FR-VIEW-16).
        let for_file = target.is_some();
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Appearance::set_os_appearance(cx, window.appearance());
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            Appearance::set_os_appearance(cx, window.appearance());
        });
        let this = WorkspaceWindow {
            window: window.window_handle(),
            workspace: cx.weak_entity(),
        };
        RecentWindows::touch(this.clone(), cx);
        let activation = cx.observe_window_activation(window, move |ws, window, cx| {
            if window.is_window_active() {
                RecentWindows::touch(this.clone(), cx);
            } else {
                ws.focus_lost(cx);
            }
        });
        let closing = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            closing
                .update(cx, |ws, cx| ws.should_close(window, cx))
                .unwrap_or(true)
        });
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            this.bounds = window.window_bounds();
            this.session_changed(cx);
        });
        let in_session = !for_file && session::track(window.window_handle(), cx.weak_entity(), cx);
        // However the window goes (closed, removed, the app quitting), the
        // tabs let go of their files.
        let release = cx.on_release(|ws: &mut Self, cx| {
            for (hook, path, editor) in ws.take_close_hooks() {
                if let Some(editor) = &editor {
                    editor.update(cx, |e, cx| {
                        e.prepare_close(cx);
                    });
                }
                let_go(hook, path, editor, cx);
            }
        });
        let settings = cx.observe_global::<AppSettings>(|ws, cx| {
            ws.apply_viewer_mode(cx);
            ws.refresh_profiles(cx);
            ws.sync_hover_delay(cx);
        });
        let profiles = cx.observe_global::<crate::profile::ProfileOverrides>(|ws, cx| {
            ws.refresh_profiles(cx);
        });
        let mut this = Self {
            focus,
            chrome,
            window_handle: window.window_handle(),
            tabs: vec![Tab::new(target)],
            active: 0,
            sidebar_open: false,
            panel_open: false,
            vault: None,
            files: None,
            files_sort: tree::SortOrder::default(),
            _files_events: None,
            _vault_changed: None,
            _content_updates: None,
            _git_updates: None,
            trust: None,
            outline: None,
            _outline_events: None,
            links: None,
            _links_events: None,
            readers: HashMap::new(),
            in_session,
            bounds: window.window_bounds(),
            anchor_seen: Rc::default(),
            closed: ClosedTabs::default(),
            vault_watch_started: false,
            watch: None,
            _watch_events: None,
            editor_events: HashMap::new(),
            closing: false,
            safety: saving::SafetyState::default(),
            drag_pending: false,
            _appearance: appearance,
            _activation: activation,
            _bounds: bounds,
            _release: release,
            _settings: settings,
            profile_menu: profile_chip::ProfileMenu::default(),
            menus: menus::ShellMenus::new(window, cx),
            panels: panels::Panels::default(),
            recent: None,
            _recent_events: None,
            index_asked: false,
            _profiles: profiles,
            link_ux: link_actions::LinkUx::new(cx),
            expanded: None,
            previews: link_preview::HoverPreviews::default(),
            vault_ops: vault_ops::VaultOps::default(),
            tab_keys: crate::a11y::RovingTabs::new(crate::a11y::Axis::Horizontal),
        };
        // The document takes the keyboard from the start (copy, scrolling):
        // a note's Reading view, or the canvas, base or viewer of another
        // file.
        this.ensure_reader(0, window, cx);
        if this.tabs[0].path.is_some() {
            this.focus_document(window, cx);
        }
        this
    }

    /// A session window brought back as it was saved: its tabs with their
    /// modes, anchors and history, and its panels.
    fn restored(
        state: session::WindowState,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut this = Self::with_chrome(None, WindowChrome::current(), window, cx);
        let tabs: Vec<Tab> = state
            .tabs
            .into_iter()
            .map(session::TabState::into_tab)
            .collect();
        if !tabs.is_empty() {
            this.active = state.active_tab.min(tabs.len() - 1);
            this.tabs = tabs;
        }
        if let Some(folder) = state.vault {
            let vault = cx.new(|cx| WindowVault::for_folder(&folder, cx));
            this.set_vault(vault, cx);
        }
        if let Some(sort) = state
            .files_sort
            .as_deref()
            .and_then(tree::SortOrder::from_saved_id)
        {
            this.set_files_sort(sort, cx);
        }
        this.tabs_changed(window, cx);
        // The sidebar's panel comes back with the sidebar (a hidden
        // sidebar shows Files when called).
        let panel = state
            .sidebar_panel
            .as_deref()
            .and_then(panels::SidebarPanel::from_saved_id);
        if state.sidebar_open {
            match panel {
                Some(panel) => this.show_sidebar_panel(panel, false, window, cx),
                None => this.set_sidebar_open(true, window, cx),
            }
        }
        if state.panel_open {
            this.set_panel_open(true, window, cx);
        }
        if let Some(base) = state.base {
            this.restore_sidebar_base(base, window, cx);
        }
        this.focus_document(window, cx);
        this
    }

    /// Whether the window is part of the saved session.
    pub fn in_session(&self) -> bool {
        self.in_session
    }

    /// Schedules a session save, for a session window.
    fn session_changed(&self, cx: &mut App) {
        if self.in_session {
            session::changed(cx);
        }
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// Opens `target` as a new tab after the active one and activates it.
    /// The window's panels stay as they are. A window holding only its empty
    /// tab gets the file in that tab instead.
    pub fn open_tab(
        &mut self,
        target: OpenTarget,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.insert_tab(target, None, true, window, cx);
    }

    /// Opens `target` like [`Self::open_tab`], in Edit mode, with `on_close`
    /// told when the tab lets go of the file (`--wait`).
    pub fn open_tab_for_edit(
        &mut self,
        target: OpenTarget,
        on_close: Option<CloseHook>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.insert_tab(target, None, true, window, cx);
        self.edit_active_tab(on_close, window, cx);
    }

    /// Puts the active tab in Edit mode with the keyboard in its Source
    /// view, and gives it `on_close` when it has a file.
    pub fn edit_active_tab(
        &mut self,
        on_close: Option<CloseHook>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let tab = &mut self.tabs[self.active];
        if tab.path.is_some() && on_close.is_some() {
            tab.on_close = on_close;
        }
        // The caller waits for an edit: viewer mode gives way in this tab.
        tab.edit_requested = true;
        tab.viewer_toggle = None;
        if let Some(editor) = tab.editor.clone() {
            editor.update(cx, |e, cx| e.set_read_only(false, cx));
        }
        self.set_mode(ViewMode::Edit, cx);
        self.focus_document(window, cx);
    }

    /// Takes the close hooks of every tab, with the tab's file and Source
    /// view.
    fn take_close_hooks(&mut self) -> Vec<(CloseHook, PathBuf, Option<Entity<EditorView>>)> {
        self.tabs
            .iter_mut()
            .filter_map(|tab| {
                let path = tab.path.clone()?;
                let hook = tab.on_close.take()?;
                Some((hook, path, tab.editor.clone()))
            })
            .collect()
    }

    /// Adds a tab for `target` after the active one, scrolled to `scroll`
    /// once loaded; makes it active unless it opens in the background.
    fn insert_tab(
        &mut self,
        target: OpenTarget,
        scroll: Option<ScrollTarget>,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let focused = self.reader_focused(window, cx);
        let tab = Tab::new(Some(target));
        let index = self.place_tab(tab);
        if let Some(scroll) = scroll {
            let reader = self.make_reader(index, Some(scroll), window, cx);
            self.tabs[index].reader = reader;
        }
        if activate || index == self.active {
            if index != self.active {
                self.leave_tab(self.active, cx);
            }
            self.active = index;
            self.tabs_changed(window, cx);
            self.focus_reader_if(focused, window, cx);
        } else {
            self.session_changed(cx);
            cx.notify();
        }
    }

    /// Puts `tab` after the active one, or in place of the window's empty
    /// tab, and returns its index.
    fn place_tab(&mut self, tab: Tab) -> usize {
        if self.tabs.len() == 1 && self.tabs[0].is_placeholder() {
            self.tabs[0] = tab;
            self.active = 0;
            0
        } else {
            self.tabs.insert(self.active + 1, tab);
            self.active + 1
        }
    }

    /// The user leaves tab `index` for another: its document autosaves.
    fn leave_tab(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        if let Some(editor) = self.tabs.get(index).and_then(|t| t.editor.clone()) {
            editor.update(cx, |e, cx| e.tab_switched(cx));
        }
    }

    pub(crate) fn activate_tab(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if index < self.tabs.len() && index != self.active {
            let focused = self.reader_focused(window, cx);
            self.leave_tab(self.active, cx);
            self.active = index;
            self.tabs_changed(window, cx);
            self.focus_reader_if(focused, window, cx);
        }
    }

    /// Whether the active tab's Reading view or Source view has the
    /// keyboard.
    fn reader_focused(&self, window: &Window, cx: &App) -> bool {
        let tab = self.active_tab();
        tab.reader
            .as_ref()
            .is_some_and(|r| r.read(cx).focus_handle(cx).contains_focused(window, cx))
            || tab
                .editor
                .as_ref()
                .is_some_and(|e| e.read(cx).focus_handle(cx).contains_focused(window, cx))
            || tab
                .split_reader(cx)
                .is_some_and(|r| r.read(cx).focus_handle(cx).contains_focused(window, cx))
            || tab
                .canvas
                .as_ref()
                .is_some_and(|c| c.read(cx).focus_handle(cx).contains_focused(window, cx))
            || tab
                .bases
                .as_ref()
                .is_some_and(|b| b.read(cx).focus_handle(cx).contains_focused(window, cx))
            || tab
                .viewer
                .as_ref()
                .is_some_and(|v| v.read(cx).focus_handle(cx).contains_focused(window, cx))
    }

    fn focus_reader_if(&self, focused: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        if focused {
            self.focus_document(window, cx);
        }
    }

    /// The Reading view on screen: the active tab's, in Preview mode.
    fn shown_reader(&self) -> Option<&Entity<Reader>> {
        match self.mode() {
            ViewMode::Preview => self.active_tab().reader.as_ref(),
            ViewMode::Edit | ViewMode::Split => None,
        }
    }

    /// Gives the keyboard to the document on screen, or to the window when
    /// it shows none.
    fn focus_document(&self, window: &mut Window, cx: &mut App) {
        if let Some(canvas) = self.shown_canvas() {
            canvas.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        if let Some(base) = self.shown_base() {
            base.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        if let Some(viewer) = self.shown_viewer() {
            viewer.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        match (self.shown_reader(), self.shown_editor()) {
            (Some(reader), _) => reader.read(cx).focus_handle(cx).focus(window, cx),
            (None, Some(editor)) => editor.read(cx).focus_handle(cx).focus(window, cx),
            (None, None) => self.focus.focus(window, cx),
        }
    }

    /// Keeps the keyboard on something that is drawn: a Reading or Source
    /// view that leaves the screen (a mode switch) hands it to the window,
    /// and the window hands it to the view that comes back.
    fn settle_focus(&self, window: &mut Window, cx: &mut App) {
        let shown: Vec<EntityId> = self
            .shown_reader()
            .map(Entity::entity_id)
            .into_iter()
            .chain(self.shown_editor().map(Entity::entity_id))
            .chain(self.shown_split_reader(cx).map(|r| r.entity_id()))
            .chain(self.shown_canvas().map(Entity::entity_id))
            .chain(self.shown_base().map(Entity::entity_id))
            .chain(self.shown_viewer().map(Entity::entity_id))
            .collect();
        let readers = self
            .tabs
            .iter()
            .filter_map(|t| t.reader.clone())
            .chain(self.tabs.iter().filter_map(|t| t.split_reader(cx)));
        let editors = self.tabs.iter().filter_map(|t| t.editor.as_ref());
        let canvases = self.tabs.iter().filter_map(|t| t.canvas.as_ref());
        let bases = self.tabs.iter().filter_map(|t| t.bases.as_ref());
        let viewers = self.tabs.iter().filter_map(|t| t.viewer.as_ref());
        let hidden_focused = readers
            .map(|r| (r.entity_id(), r.read(cx).focus_handle(cx)))
            .chain(editors.map(|e| (e.entity_id(), e.read(cx).focus_handle(cx))))
            .chain(canvases.map(|c| (c.entity_id(), c.read(cx).focus_handle(cx))))
            .chain(bases.map(|b| (b.entity_id(), b.read(cx).focus_handle(cx))))
            .chain(viewers.map(|v| (v.entity_id(), v.read(cx).focus_handle(cx))))
            .any(|(id, focus)| !shown.contains(&id) && focus.contains_focused(window, cx));
        if hidden_focused || (!shown.is_empty() && self.focus.is_focused(window)) {
            self.focus_document(window, cx);
        }
    }

    fn tabs_changed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.ensure_reader(self.active, window, cx);
        self.leave_locked_mode(cx);
        match self.mode() {
            ViewMode::Edit => {
                self.ensure_editor(self.active, cx);
            }
            ViewMode::Split => {
                self.ensure_split(self.active, cx);
            }
            ViewMode::Preview => {}
        }
        window.set_window_title(&self.active_tab().label.window_title());
        if let Some(files) = self.files.clone() {
            self.ensure_vault(cx);
            let active = self.active_tab().path.clone();
            files.update(cx, |files, cx| files.set_active(active, cx));
        }
        if let Some(id) = self.active_tab().reader.as_ref().map(Entity::entity_id) {
            self.refresh_heading(id, cx);
        }
        self.sync_outline(cx);
        self.sync_links(cx);
        self.sync_file_history(cx);
        self.sync_bookmarks(cx);
        self.session_changed(cx);
        cx.notify();
    }

    /// Opens a file from a panel: activates its tab, or opens a new one.
    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<'_, Self>) {
        let open = self
            .tabs
            .iter()
            .position(|t| t.path.as_deref() == Some(&path));
        match open {
            Some(index) => self.activate_tab(index, window, cx),
            None => self.open_tab(OpenTarget::new(path), window, cx),
        }
    }

    /// Creates the window's vault from the active tab's file, once.
    fn ensure_vault(&mut self, cx: &mut Context<'_, Self>) {
        if self.vault.is_some() {
            return;
        }
        let Some(path) = self.active_tab().path.clone() else {
            return;
        };
        let vault = cx.new(|cx| WindowVault::for_file(&path, cx));
        self.set_vault(vault, cx);
    }

    /// The vault published a new index snapshot: link statuses follow it.
    /// Once its index is built, the vault gets a watcher.
    fn vault_changed(&mut self, cx: &mut Context<'_, Self>) {
        let Some(vault) = self.vault.clone() else {
            return;
        };
        self.ask_about_index(cx);
        let ready = vault.read(cx).status() == &vault::VaultStatus::Ready;
        if ready
            && !self.vault_watch_started
            && let Some(watch) = self.ensure_watch(cx)
        {
            self.vault_watch_started = true;
            watch.start_vault(vault.read(cx).shared_index());
        }
        let index = vault.read(cx).snapshot();
        for reader in self.tabs.iter().filter_map(|t| t.reader.clone()) {
            let index = index.clone();
            reader.update(cx, |r, cx| r.set_index(Some(index), cx));
        }
        for editor in self.tabs.iter().filter_map(|t| t.editor.clone()) {
            let index = index.clone();
            editor.update(cx, |e, cx| e.set_index(Some(index), cx));
        }
        for split in self.tabs.iter().filter_map(|t| t.split.clone()) {
            let index = index.clone();
            split.update(cx, |s, cx| s.set_index(Some(index), cx));
        }
        self.sync_canvas_roots(cx);
        self.sync_base_roots(cx);
        self.sync_viewer_roots(cx);
        self.sync_search(cx);
        self.sync_conflicts(cx);
        self.sync_bookmarks(cx);
        self.sync_tags(cx);
        self.sync_links(cx);
        self.push_reader_content(cx);
    }

    /// Live blocks (canvas, base and query embeds) use the vault's content
    /// index: every Reading view of the window, the Split views' preview
    /// panes included, gets each new snapshot. A window without a vault
    /// gives none; its views look for the vault that holds their file
    /// themselves.
    /// The Reading views follow the hover preview delay setting.
    fn sync_hover_delay(&self, cx: &mut Context<'_, Self>) {
        let delay = link_preview::hover_delay(cx);
        let readers: Vec<Entity<Reader>> = self
            .tabs
            .iter()
            .flat_map(|t| [t.reader.clone(), t.split_reader(cx)])
            .flatten()
            .collect();
        for reader in readers {
            reader.update(cx, |r, _| r.set_hover_previews(Some(delay)));
        }
    }

    pub(super) fn push_reader_content(&self, cx: &mut Context<'_, Self>) {
        let Some(vault) = &self.vault else {
            return;
        };
        let content = vault.read(cx).content();
        let readers: Vec<Entity<Reader>> = self
            .tabs
            .iter()
            .flat_map(|t| [t.reader.clone(), t.split_reader(cx)])
            .flatten()
            .collect();
        for reader in readers {
            let content = content.clone();
            reader.update(cx, |r, cx| r.set_content(content, cx));
        }
        self.push_canvas_content(cx);
    }

    /// The Reading view of tab `index`, made and loading on first use.
    fn ensure_reader(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<Reader>> {
        let tab = self.tabs.get(index)?;
        if let Some(reader) = &tab.reader {
            return Some(reader.clone());
        }
        let scroll = tab
            .scroll
            .clone()
            .or_else(|| tab.position.map(|p| ScrollTarget::Line(p.line)));
        let reader = self.make_reader(index, scroll, window, cx)?;
        self.tabs[index].reader = Some(reader.clone());
        Some(reader)
    }

    fn make_reader(
        &mut self,
        index: usize,
        scroll: Option<ScrollTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<Reader>> {
        let path = self.tabs.get(index)?.path.clone()?;
        // A canvas file gets a Canvas view instead.
        if canvas_tabs::is_canvas(&path) {
            self.tabs[index].bases = None;
            self.tabs[index].viewer = None;
            self.ensure_canvas(index, path, window, cx);
            return None;
        }
        // So does a base file.
        if bases_tabs::is_base(&path) {
            self.tabs[index].canvas = None;
            self.tabs[index].viewer = None;
            self.ensure_base(index, path, window, cx);
            return None;
        }
        // And any other file that is not a note, the built-in viewer.
        if viewer_tabs::is_viewer_file(&path) {
            self.tabs[index].canvas = None;
            self.tabs[index].bases = None;
            self.ensure_viewer(index, path, scroll.as_ref(), window, cx);
            return None;
        }
        self.tabs[index].canvas = None;
        self.tabs[index].bases = None;
        self.tabs[index].viewer = None;
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let content = self.vault.as_ref().map(|v| v.read(cx).content());
        let profile = self.tab_profile(index, cx).compiled;
        let delay = link_preview::hover_delay(cx);
        let folds = self.tabs[index]
            .restored_reading_folds
            .as_ref()
            .filter(|(p, _)| *p == path)
            .map(|(_, folds)| folds.clone());
        let reader = cx.new(|cx| {
            let mut reader = Reader::new(cx);
            if let Some(folds) = folds {
                reader.restore_reading_folds(&path, folds, cx);
            }
            reader.set_hover_previews(Some(delay));
            reader.set_index(snapshot, cx);
            // With a vault the window hands its content snapshots.
            if let Some(content) = content {
                reader.set_content(content, cx);
            }
            reader.set_profile(profile, cx);
            reader.open(path, scroll, cx);
            reader
        });
        let events = cx.subscribe_in(
            &reader,
            window,
            |this, reader, event: &ReaderEvent, window, cx| match event {
                ReaderEvent::FollowLink(request) => {
                    this.follow_link(reader.clone(), request.clone(), window, cx);
                }
                ReaderEvent::ToggleTask(toggle) => this.toggle_task(reader, *toggle, cx),
                // The session keeps the tab's Reading-view folds.
                ReaderEvent::FoldToggled => this.session_changed(cx),
                ReaderEvent::ExpandTable(request) => {
                    this.expand_table(reader.clone(), request, window, cx);
                }
            },
        );
        let changes = cx.observe(&reader, |this, reader, cx| this.reader_changed(&reader, cx));
        self.readers.insert(
            reader.entity_id(),
            ReaderWatch {
                doc: None,
                markers: false,
                _count: None,
                _events: events,
                _changes: changes,
            },
        );
        Some(reader)
    }

    /// A Reading view changed: a document started or finished loading, or
    /// the view moved.
    fn reader_changed(&mut self, reader: &Entity<Reader>, cx: &mut Context<'_, Self>) {
        self.check_reader_preview(reader, cx);
        let id = reader.entity_id();
        let (loaded, model, text) = {
            let r = reader.read(cx);
            match (r.status(), r.path()) {
                (LoadStatus::Ready, Some(path)) => {
                    let known = self
                        .readers
                        .get(&id)
                        .and_then(|w| w.doc.as_ref())
                        .is_some_and(|d| d.path == path);
                    if known {
                        // A document read again: the infobar follows its
                        // conflict markers.
                        let markers = r.conflict_markers();
                        if let Some(watch) = self.readers.get_mut(&id)
                            && watch.markers != markers
                        {
                            watch.markers = markers;
                            cx.notify();
                        }
                        self.refresh_heading(id, cx);
                        return;
                    }
                    (Some(path.to_path_buf()), r.model(), r.plain_text())
                }
                _ => (None, None, None),
            }
        };
        if let (Some(path), Some(_)) = (&loaded, &model) {
            // A file shown only in Preview is watched too.
            self.track_file(path, None, cx);
        }
        let Some(watch) = self.readers.get_mut(&id) else {
            return;
        };
        match (loaded, model) {
            (Some(path), Some(model)) => {
                session::note_recent_file(cx, &path);
                let source_path = path.clone();
                watch.doc = Some(DocFacts {
                    path,
                    model,
                    words: None,
                    heading: None,
                });
                watch._count = text.map(|text| {
                    cx.spawn(async move |this, cx| {
                        let (words, signals) = cx
                            .background_spawn(async move {
                                // Dialect signs are in the source, not in
                                // the rendered text.
                                let signals = std::fs::read_to_string(&source_path)
                                    .map(|source| gv_profile::hints::scan(&source))
                                    .unwrap_or_default();
                                (status_bar::count_words(&text), signals)
                            })
                            .await;
                        this.update(cx, |this, cx| {
                            this.words_counted(id, words, cx);
                            this.dialect_signals(id, &signals, cx);
                        })
                        .ok();
                    })
                });
            }
            _ if watch.doc.is_none() => return,
            _ => {
                watch.doc = None;
                watch._count = None;
            }
        }
        self.refresh_heading(id, cx);
        if self.is_active_reader(id) {
            self.sync_outline(cx);
        }
        cx.notify();
    }

    fn words_counted(&mut self, id: EntityId, words: usize, cx: &mut Context<'_, Self>) {
        if let Some(doc) = self.readers.get_mut(&id).and_then(|w| w.doc.as_mut()) {
            doc.words = Some(words);
            cx.notify();
        }
    }

    fn is_active_reader(&self, id: EntityId) -> bool {
        self.active_tab()
            .reader
            .as_ref()
            .is_some_and(|r| r.entity_id() == id)
    }

    /// What the window knows about the active tab's document.
    fn active_doc(&self) -> Option<&DocFacts> {
        let reader = self.active_tab().reader.as_ref()?;
        self.readers.get(&reader.entity_id())?.doc.as_ref()
    }

    /// Finds the section at the top of Reading view `id`, and tells the
    /// outline and the status bar when it changed.
    fn refresh_heading(&mut self, id: EntityId, cx: &mut Context<'_, Self>) {
        let Some(reader) = self
            .tabs
            .iter()
            .filter_map(|t| t.reader.as_ref())
            .find(|r| r.entity_id() == id)
        else {
            return;
        };
        let position = reader.read(cx).position();
        let Some(doc) = self.readers.get_mut(&id).and_then(|w| w.doc.as_mut()) else {
            return;
        };
        let heading = section_at(&doc.model, position.as_ref());
        if heading == doc.heading {
            return;
        }
        doc.heading = heading;
        if self.is_active_reader(id)
            && let Some(outline) = &self.outline
        {
            outline.update(cx, |o, cx| o.set_current(heading, cx));
        }
        cx.notify();
    }

    /// Shows the active document in the Outline.
    fn sync_outline(&mut self, cx: &mut Context<'_, Self>) {
        let Some(outline) = self.outline.clone() else {
            return;
        };
        let (model, heading, file) = match self.active_doc() {
            Some(doc) => (Some(doc.model.clone()), doc.heading, Some(doc.path.clone())),
            None => (None, None, None),
        };
        outline.update(cx, |o, cx| {
            o.set_file(file);
            o.set_document(model, heading, cx);
        });
    }

    /// An element drawn after the document that looks where the Reading
    /// view settled in this frame (a jump or a scroll lands while the view
    /// is laid out) and updates the current section when it moved.
    fn heading_probe(&self, cx: &mut Context<'_, Self>) -> Option<impl IntoElement> {
        let reader = self.shown_reader()?.clone();
        let id = reader.entity_id();
        let doc = self.readers.get(&id)?.doc.as_ref()?;
        let (model, known) = (doc.model.clone(), doc.heading);
        let this = cx.weak_entity();
        Some(
            canvas(
                move |_, _, cx| {
                    let position = reader.read(cx).position();
                    if section_at(&model, position.as_ref()) != known {
                        cx.defer(move |cx| {
                            this.update(cx, |ws, cx| ws.refresh_heading(id, cx)).ok();
                        });
                    }
                },
                |_, (), _, _| {},
            )
            .absolute()
            .size_0(),
        )
    }

    /// An element drawn after the document that notices when the active
    /// Reading view scrolled, so that the session is saved with the new
    /// anchor. Only session windows have one.
    fn anchor_probe(&self) -> Option<impl IntoElement> {
        if !self.in_session {
            return None;
        }
        let reader = self.active_tab().reader.clone()?;
        let seen = self.anchor_seen.clone();
        Some(
            canvas(
                move |_, _, cx| {
                    let position = reader.read(cx).position();
                    if *seen.borrow() != position {
                        seen.replace(position);
                        cx.defer(session::changed);
                    }
                },
                |_, (), _, _| {},
            )
            .absolute()
            .size_0(),
        )
    }

    /// Follows a link clicked in a tab's Reading view.
    fn follow_link(
        &mut self,
        reader: Entity<Reader>,
        request: LinkRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.reader.as_ref() == Some(&reader))
        else {
            return;
        };
        // A query block's context line: its note (a vault path), there.
        if let Some(at) = request.at
            && let Some(root) = self.link_vault_root(cx)
            && let Ok(note) = gv_vault::VaultPath::new(&request.destination)
        {
            let scroll = Some(ScrollTarget::Offset(at));
            let path = note.to_fs_path(&root);
            self.open_link_target(index, path, scroll, request.disposition, window, cx);
            return;
        }
        let r = reader.read(cx);
        // A link inside a note embed is followed from the embedded note.
        let file = match request.from.as_deref() {
            Some(from) => r.link_file(Some(from)),
            None => self.tabs[index].path.clone(),
        };
        let Some(file) = file else {
            return;
        };
        let construct = request.range.clone().and_then(|range| r.source_text(range));
        let link = crate::reader::link_info::LinkAt {
            file,
            kind: request.kind,
            image: false,
            destination: request.destination,
            range: request.range,
            construct,
            profile: r.profile(),
        };
        self.follow_link_at(index, link, request.disposition, false, window, cx);
    }

    /// Shows `path` in tab `index` (the same tab), remembering where the tab
    /// was. A jump inside the same document is remembered too.
    fn navigate(
        &mut self,
        index: usize,
        path: PathBuf,
        scroll: Option<ScrollTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(left) = self.place(index, cx) {
            self.tabs[index].history.push(left);
        }
        self.show_in_tab(index, path, scroll, window, cx);
    }

    /// Where tab `index` is now, as a history entry. The scroll position
    /// is known once its document has loaded.
    fn place(&self, index: usize, cx: &App) -> Option<HistoryEntry> {
        let tab = self.tabs.get(index)?;
        let path = tab.path.clone()?;
        let scroll = tab.reader.as_ref().and_then(|reader| {
            let reader = reader.read(cx);
            let shown = reader.status() == &LoadStatus::Ready && reader.path() == Some(&path);
            if shown { reader.position() } else { None }
        });
        // In Split, the top of the leading pane.
        let scroll = match (tab.mode, &tab.split) {
            (ViewMode::Split, Some(split)) => split
                .read(cx)
                .top_offset(cx)
                .map(ScrollTarget::Offset)
                .or(scroll),
            _ => scroll,
        };
        let scroll = scroll
            .or_else(|| tab.scroll.clone())
            .or_else(|| tab.position.map(|p| ScrollTarget::Line(p.line)));
        let pdf = tab.pdf_place(cx);
        Some(HistoryEntry {
            path,
            mode: tab.mode,
            scroll,
            pdf,
        })
    }

    fn show_in_tab(
        &mut self,
        index: usize,
        path: PathBuf,
        scroll: Option<ScrollTarget>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let same = tab.path.as_deref() == Some(path.as_path());
        let left = if same { None } else { tab.path.clone() };
        let editor = if same { None } else { tab.editor.take() };
        if !same {
            tab.notice = None;
            tab.split = None;
        }
        let on_close = if same { None } else { tab.on_close.take() };
        tab.label = FileLabel::new(Some(&path));
        tab.path = Some(path.clone());
        tab.position = None;
        tab.scroll = None;
        // The Source view goes with the file it edits (saved or journaled).
        if let Some(editor) = &editor {
            self.drop_editor(editor, cx);
        }
        if let (Some(hook), Some(left)) = (on_close, left.clone()) {
            let_go(hook, left, editor, cx);
        }
        if let Some(left) = left {
            self.untrack_if_unused(&left);
        }
        let tab = &mut self.tabs[index];
        if !crate::platform::is_markdown(&path) {
            // The Reading view of the file left goes; the Canvas (or
            // Bases, or the built-in) view shows the file (or loads it in place of another).
            if let Some(reader) = tab.reader.take() {
                self.readers.remove(&reader.entity_id());
            }
            // A PDF's `#page=N` reaches the built-in viewer.
            self.make_reader(index, scroll.clone(), window, cx);
        } else {
            match tab.reader.clone() {
                Some(reader) if same => {
                    let target = scroll.clone().unwrap_or(ScrollTarget::Offset(0));
                    self.split_scroll_to(index, target.clone(), cx);
                    reader.update(cx, |r, cx| r.scroll_to(target, cx));
                }
                Some(reader) => {
                    self.tabs[index].canvas = None;
                    self.tabs[index].bases = None;
                    self.tabs[index].viewer = None;
                    reader.update(cx, |r, cx| r.open(path, scroll.clone(), cx));
                }
                None => {
                    let reader = self.make_reader(index, scroll.clone(), window, cx);
                    self.tabs[index].reader = reader;
                }
            }
        }
        if index == self.active {
            self.tabs_changed(window, cx);
            if let (false, Some(target)) = (same, scroll) {
                self.split_scroll_to(index, target, cx);
            }
        } else {
            self.session_changed(cx);
            cx.notify();
        }
    }

    /// Whether the sidebar and the right panel are shown.
    #[cfg(test)]
    pub(crate) fn panels_open(&self) -> (bool, bool) {
        (self.sidebar_open, self.panel_open)
    }

    /// The state of the window's vault, once it exists.
    #[cfg(test)]
    pub(crate) fn vault_status(&self, cx: &App) -> Option<VaultStatus> {
        self.vault.as_ref().map(|v| v.read(cx).status().clone())
    }

    /// Goes back to where the active tab was before it last followed a
    /// link. Returns false when there is nothing to go back to.
    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        self.travel(History::go_back, Cause::Back, window, cx)
    }

    /// Goes forward again after [`Self::go_back`]. Returns false when there
    /// is nothing to go forward to.
    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        self.travel(History::go_forward, Cause::Forward, window, cx)
    }

    pub fn can_go_back(&self) -> bool {
        self.active_tab().history.can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.active_tab().history.can_go_forward()
    }

    /// Every Back and Forward (keys, menus, mouse buttons, toolbar) comes
    /// here, so each is noted once for the navigation log.
    fn travel(
        &mut self,
        step: fn(&mut History, HistoryEntry) -> Option<HistoryEntry>,
        cause: Cause,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let index = self.active;
        let Some(current) = self.place(index, cx) else {
            return false;
        };
        let Some(to) = step(&mut self.tabs[index].history, current) else {
            return false;
        };
        let from = self.tabs[index].mode;
        self.tabs[index].mode = to.mode;
        if self.tabs[index].path.as_deref() == Some(to.path.as_path()) {
            self.mode_changed(from, cx);
        }
        if to.mode != ViewMode::Edit {
            // The Reading view that shows the target takes the note.
            follow::begin(cause);
        }
        // A PDF goes back to where it was left, not to a page a link named.
        let pdf = to.pdf;
        let scroll = match pdf {
            Some(_) => Some(ScrollTarget::Offset(0)),
            None => to.scroll.or(Some(ScrollTarget::Offset(0))),
        };
        if let Some(place) = pdf {
            self.tabs[index].pdf_place = Some((to.path.clone(), place));
        }
        self.show_in_tab(index, to.path.clone(), scroll, window, cx);
        // The viewer already showed that PDF (a jump within it): it moves.
        if let Some(place) = pdf
            && let Some(viewer) = self.tabs[index].viewer.clone()
            && viewer.read(cx).path() == to.path.as_path()
        {
            viewer.update(cx, |v, cx| v.show_pdf_place(place, cx));
        }
        true
    }

    /// Closes tab `index`; it can be reopened with its history. Closing
    /// the last tab leaves an empty one.
    pub fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        if index >= self.tabs.len() || self.hold_tab_close(index, window, cx) {
            return;
        }
        if let Some(place) = self.place(index, cx) {
            self.closed.push(ClosedTab {
                index,
                place,
                history: self.tabs[index].history.clone(),
            });
        } else if self.tabs.len() == 1 && self.tabs[index].is_placeholder() {
            // Only the empty tab: nothing to close.
            return;
        }
        let was_active = index == self.active;
        let tab = self.tabs.remove(index);
        if let Some(reader) = &tab.reader {
            self.readers.remove(&reader.entity_id());
        }
        if let Some(editor) = &tab.editor {
            self.drop_editor(editor, cx);
        }
        if let Some(path) = &tab.path {
            self.untrack_if_unused(path);
        }
        if let (Some(hook), Some(path)) = (tab.on_close, tab.path) {
            let_go(hook, path, tab.editor, cx);
        }
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new(None));
        }
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.tabs_changed(window, cx);
        if was_active {
            self.focus_document(window, cx);
        }
    }

    /// Reopens the tab closed last where it was, with its history.
    /// Returns false when no closed tab is left.
    pub fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> bool {
        let Some(closed) = self.closed.pop() else {
            return false;
        };
        let place = closed.place;
        let mut tab = Tab::new(Some(OpenTarget::new(place.path)));
        tab.mode = place.mode;
        tab.history = closed.history;
        let index = if self.tabs.len() == 1 && self.tabs[0].is_placeholder() {
            self.tabs[0] = tab;
            0
        } else {
            let index = closed.index.min(self.tabs.len());
            self.tabs.insert(index, tab);
            index
        };
        let scroll = place.scroll.or(Some(ScrollTarget::Offset(0)));
        let reader = self.make_reader(index, scroll, window, cx);
        self.tabs[index].reader = reader;
        self.active = index;
        self.tabs_changed(window, cx);
        self.focus_document(window, cx);
        true
    }

    /// Scrolls the active document to heading `heading` (from the
    /// Outline). The jump is history, like a followed link.
    fn jump_to_heading(&mut self, heading: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        let index = self.active;
        let Some(doc) = self.active_doc() else {
            return;
        };
        let Some(start) = doc.model.headings().get(heading).map(|h| h.range.start) else {
            return;
        };
        let path = doc.path.clone();
        self.navigate(index, path, Some(ScrollTarget::Offset(start)), window, cx);
        self.focus_document(window, cx);
    }

    /// Creates the Files panel, and with it the vault, the first time the
    /// sidebar is shown.
    fn ensure_files_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.files.is_some() {
            return;
        }
        self.ensure_vault(cx);
        let (vault, active) = (self.vault.clone(), self.active_tab().path.clone());
        let sort = self.files_sort;
        let files = cx.new(|cx| {
            let mut panel = FilesPanel::new(vault, active, cx);
            panel.set_sort(sort, cx);
            panel
        });
        self._files_events = Some(cx.subscribe_in(
            &files,
            window,
            |this, _, event: &FilesPanelEvent, window, cx| match event {
                FilesPanelEvent::Open(path) => this.open_path(path.clone(), window, cx),
                FilesPanelEvent::SwitchVault => this.show_vault_switcher(window, cx),
                FilesPanelEvent::MoveInto { from, folder } => {
                    this.drag_move(from.clone(), folder.clone(), window, cx);
                }
                FilesPanelEvent::CopyInto { paths, folder } => {
                    this.copy_dropped(paths.clone(), folder.clone(), cx);
                }
                FilesPanelEvent::Sort => this.show_sort_orders(window, cx),
            },
        ));
        self.files = Some(files);
    }

    /// The Files panel's sort order, kept with the session.
    fn set_files_sort(&mut self, sort: tree::SortOrder, cx: &mut Context<'_, Self>) {
        self.files_sort = sort;
        if let Some(files) = &self.files {
            files.update(cx, |f, cx| f.set_sort(sort, cx));
        }
        self.session_changed(cx);
    }

    fn mode(&self) -> ViewMode {
        self.active_tab().mode
    }

    /// Shows the active tab in `mode`; in viewer mode only Preview, unless
    /// the tab may be edited ([`Self::can_edit`]).
    fn set_mode(&mut self, mode: ViewMode, cx: &mut Context<'_, Self>) {
        if !self.mode_available(mode, cx) {
            return;
        }
        let from = self.mode();
        self.tabs[self.active].mode = mode;
        self.mode_changed(from, cx);
        self.session_changed(cx);
        cx.notify();
    }

    /// Whether tab `index` may be edited: viewer mode is off for it (see
    /// [`Self::viewer_mode_of`]).
    pub(crate) fn can_edit(&self, index: usize, cx: &App) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        // The Canvas and Bases views are read-only and have no Source view.
        if tab.is_canvas() || tab.is_base() {
            return false;
        }
        // The built-in viewer's text is edited as plain text; images and
        // files it does not show are not.
        if tab.is_viewer() && !tab.viewer_shows_text(cx) {
            return false;
        }
        !self.viewer_mode_of(index, cx)
    }

    /// Whether viewer mode is on for tab `index`: as switched for the tab
    /// (`view.toggle_viewer_mode`), else as the setting says, except in a
    /// tab opened for an edit (`--wait`) and an untitled document (there is
    /// no file to protect, and it was made to be written).
    pub(crate) fn viewer_mode_of(&self, index: usize, cx: &App) -> bool {
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        tab.viewer_toggle.unwrap_or_else(|| {
            !tab.edit_requested && tab.untitled.is_none() && AppSettings::viewer_mode(cx)
        })
    }

    /// `view.toggle_viewer_mode`: switches viewer mode for the active tab
    /// only; the setting stays as it is for the other tabs. A tab that is
    /// never edited (a canvas, a base, an image) is left alone. The tab
    /// stays in its mode, or leaves Edit and Split for Preview.
    pub(crate) fn toggle_viewer_mode(&mut self, cx: &mut Context<'_, Self>) {
        let index = self.active;
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if tab.is_canvas() || tab.is_base() || (tab.is_viewer() && !tab.viewer_shows_text(cx)) {
            return;
        }
        let on = !self.viewer_mode_of(index, cx);
        self.tabs[index].viewer_toggle = Some(on);
        self.apply_viewer_mode(cx);
        self.session_changed(cx);
    }

    /// The status bar's facts from the Source view on screen.
    pub(crate) fn editor_facts(&self, cx: &App) -> Option<EditorFacts> {
        self.shown_editor()?.read(cx).status_facts()
    }

    /// Whether the active tab can be shown in `mode` (Edit and Split are
    /// unavailable in viewer mode; a file that is not a note has no Split
    /// view).
    pub fn mode_available(&self, mode: ViewMode, cx: &App) -> bool {
        match mode {
            ViewMode::Preview => true,
            ViewMode::Edit => self.can_edit(self.active, cx),
            ViewMode::Split => self.can_edit(self.active, cx) && !self.active_tab().is_viewer(),
        }
    }

    /// Viewer mode was switched on or off: each Source view is read-only
    /// or not, and the active tab leaves Edit or Split if it may not be
    /// edited. Edits made before stay and are saved as usual.
    fn apply_viewer_mode(&mut self, cx: &mut Context<'_, Self>) {
        for index in 0..self.tabs.len() {
            let read_only = !self.can_edit(index, cx);
            if let Some(editor) = self.tabs[index].editor.clone() {
                editor.update(cx, |e, cx| e.set_read_only(read_only, cx));
            }
        }
        self.leave_locked_mode(cx);
        self.sync_links(cx);
        cx.notify();
    }

    /// Puts the active tab in Preview if it shows Edit or Split but may
    /// not be edited (viewer mode; a restored tab, a setting changed).
    fn leave_locked_mode(&mut self, cx: &mut Context<'_, Self>) {
        let from = self.mode();
        if self.mode_available(from, cx) {
            return;
        }
        self.tabs[self.active].mode = ViewMode::Preview;
        self.mode_changed(from, cx);
        self.session_changed(cx);
        cx.notify();
    }

    /// `view.toggle_live_preview`: the active tab's editing style, Live
    /// Preview or Source (kept with the session; Source by default).
    pub(crate) fn toggle_live_preview(&mut self, cx: &mut Context<'_, Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        tab.live_preview = !tab.live_preview;
        let on = tab.live_preview;
        if let Some(editor) = tab.editor.clone() {
            editor.update(cx, |editor, cx| editor.set_live_preview(on, cx));
        }
        self.session_changed(cx);
        cx.notify();
    }

    fn toggle_edit_mode(&mut self, cx: &mut Context<'_, Self>) {
        let mode = match self.mode() {
            ViewMode::Edit => ViewMode::Preview,
            ViewMode::Preview | ViewMode::Split => ViewMode::Edit,
        };
        self.set_mode(mode, cx);
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.set_sidebar_open(!self.sidebar_open, window, cx);
    }

    fn set_sidebar_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.sidebar_open = open;
        if open {
            self.ensure_files_panel(window, cx);
        }
        self.session_changed(cx);
        cx.notify();
    }

    fn toggle_panel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.set_panel_open(!self.panel_open, window, cx);
    }

    fn set_panel_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.panel_open = open;
        if open {
            self.ensure_outline(window, cx);
        }
        self.sync_links(cx);
        self.session_changed(cx);
        cx.notify();
    }

    /// Creates the Outline the first time the right panel is shown.
    fn ensure_outline(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.outline.is_some() {
            return;
        }
        let outline = cx.new(|_| OutlinePanel::new());
        self._outline_events = Some(cx.subscribe_in(
            &outline,
            window,
            |this, _, event: &OutlinePanelEvent, window, cx| match *event {
                OutlinePanelEvent::Jump(heading) => this.jump_to_heading(heading, window, cx),
            },
        ));
        self.outline = Some(outline);
        self.sync_outline(cx);
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.settle_focus(window, cx);
        self.prune_previews();
        self.tab_keys.sync(self.tabs.len(), cx);
        self.panels.sync_switcher_keys(cx);
        let theme = Appearance::theme(cx);
        let radii = window_corner_radii(window, cx);
        let editor_facts = self.editor_facts(cx);
        let profile_chip = self.render_profile_chip(theme, cx);
        let profile_menu = self.render_profile_menu(window, theme, cx);
        let expanded = self.render_expanded(cx);

        // The window root (GPUI Component's `Root`) draws the frame of a
        // client-decorated window: shadow, border, rounded corners and resize
        // edges. Surfaces that touch a corner round it themselves.
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleEditMode, _, cx| this.toggle_edit_mode(cx)))
            .on_action(cx.listener(|this, _: &ToggleSplit, _, cx| this.toggle_split(cx)))
            .on_action(cx.listener(|this, _: &ToggleLivePreview, _, cx| {
                this.toggle_live_preview(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleViewerMode, _, cx| {
                this.toggle_viewer_mode(cx);
            }))
            // Files dropped anywhere the document area does not take them
            // open ([`drag_drop`]).
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                this.open_dropped(paths.paths().to_vec(), window, cx);
            }))
            .on_drop(cx.listener(|this, file: &drag_drop::DraggedFile, window, cx| {
                if !file.is_dir {
                    this.open_dropped(vec![file.path.clone()], window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, window, cx| {
                this.toggle_sidebar(window, cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePanel, window, cx| {
                this.toggle_panel(window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoBack, window, cx| {
                this.go_back(window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoForward, window, cx| {
                this.go_forward(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                this.close_tab(this.command_tab(cx), window, cx);
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                this.reopen_closed_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &tab_commands::New, window, cx| {
                this.new_tab(window, cx);
            }))
            .on_action(cx.listener(|this, _: &file_commands::Save, _, cx| {
                this.save_active(cx);
            }))
            .on_action(cx.listener(|this, _: &file_commands::SaveAs, _, cx| {
                this.save_as_active(cx);
            }))
            .map(|root| self.menu_actions(root, cx))
            .map(|root| self.file_actions(root, cx))
            .map(|root| self.panel_actions(root, cx))
            .map(|root| self.pane_actions(root, cx))
            .map(|root| self.switcher_actions(root, cx))
            .map(|root| self.search_actions(root, cx))
            .map(|root| self.link_actions(root, window, cx))
            .map(|root| self.preview_actions(root, window, cx))
            .map(|root| self.link_list_actions(root, cx))
            .map(|root| self.vault_op_actions(root, window, cx))
            .map(|root| self.file_history_actions(root, cx))
            .map(|root| self.bookmark_actions(root, cx))
            .map(|root| self.daily_note_actions(root, cx))
            // The mouse's back and forward buttons.
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _, window, cx| {
                    this.go_back(window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _, window, cx| {
                    this.go_forward(window, cx);
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .rounded_tl(radii.top_left)
            .rounded_tr(radii.top_right)
            .rounded_bl(radii.bottom_left)
            .rounded_br(radii.bottom_right)
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(self.sidebar_open, |row| {
                        row.child(self.render_sidebar(radii, window, cx).accessible())
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(self.render_title_row(radii, window, cx).accessible())
                            .child(
                                // The content area: the expanded table
                                // covers it.
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .flex()
                                    .relative()
                                    .child(
                                        // The infobar spans the document,
                                        // not the right panel.
                                        div()
                                            .id("main")
                                            .a11y_group(gpui::Role::Main, "Document area")
                                            .flex_1()
                                            .min_w_0()
                                            .h_full()
                                            .flex()
                                            .flex_col()
                                            .children(self.render_trust_prompt())
                                            .children(crate::updates::notices::render(theme, cx))
                                            .children(self.render_infobar(theme, cx))
                                            .child(self.render_document(theme, cx)),
                                    )
                                    .when(self.panel_open, |row| {
                                        row.child(panels::right_panel(
                                            theme,
                                            self.outline.clone(),
                                            self.links.clone(),
                                            &self.panels,
                                            cx,
                                        ))
                                    })
                                    .children(expanded),
                            ),
                    ),
            )
            .child(status_bar::status_bar(
                self.git_branch(cx),
                self.active_tab(),
                self.active_doc(),
                editor_facts.as_ref(),
                self.link_findings(cx),
                crate::zoom::percent(cx),
                profile_chip,
                theme,
                radii,
            )
            .accessible())
            .children(profile_menu)
            .child(self.menus.popover.clone())
            .children(self.render_link_popover(cx))
            .children(self.render_previews(cx))
            .children(self.heading_probe(cx))
            .children(self.anchor_probe())
    }
}

/// The heading whose section holds the top of the view at `position`.
fn section_at(model: &DocModel, position: Option<&ScrollTarget>) -> Option<usize> {
    match position? {
        ScrollTarget::Offset(offset) => model.heading_at(*offset),
        ScrollTarget::Fragment(_) | ScrollTarget::Line(_) => None,
    }
}

/// A main window and its workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceWindow {
    pub window: AnyWindowHandle,
    pub workspace: WeakEntity<Workspace>,
}

impl WorkspaceWindow {
    /// Opens `target` as a tab in this window (see [`Workspace::open_tab`]).
    pub fn open_tab(&self, target: OpenTarget, cx: &mut App) -> Result<()> {
        let workspace = self.workspace.clone();
        cx.update_window(self.window, |_, window, cx| {
            workspace.update(cx, |ws, cx| ws.open_tab(target, window, cx))
        })?
    }

    /// Opens `target` as a tab in Edit mode (see
    /// [`Workspace::open_tab_for_edit`]).
    pub fn open_tab_for_edit(
        &self,
        target: OpenTarget,
        on_close: Option<CloseHook>,
        cx: &mut App,
    ) -> Result<()> {
        let workspace = self.workspace.clone();
        cx.update_window(self.window, |_, window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.open_tab_for_edit(target, on_close, window, cx);
            })
        })?
    }

    /// Puts the active tab in Edit mode (see [`Workspace::edit_active_tab`]).
    pub fn edit_active_tab(&self, on_close: Option<CloseHook>, cx: &mut App) -> Result<()> {
        let workspace = self.workspace.clone();
        cx.update_window(self.window, |_, window, cx| {
            workspace.update(cx, |ws, cx| ws.edit_active_tab(on_close, window, cx))
        })?
    }

    /// Brings the window to the front.
    pub fn activate(&self, cx: &mut App) {
        // Fails only when the window has closed.
        let _ = cx.update_window(self.window, |_, window, _| window.activate_window());
    }
}

/// The main windows, most recently focused first.
#[derive(Debug, Default)]
struct RecentWindows(Vec<WorkspaceWindow>);

impl Global for RecentWindows {}

impl RecentWindows {
    fn touch(entry: WorkspaceWindow, cx: &mut App) {
        let order = &mut cx.default_global::<Self>().0;
        order.retain(|w| w.window != entry.window);
        order.insert(0, entry);
    }
}

/// The main window that was focused last and is still open.
pub fn most_recent_window(cx: &mut App) -> Option<WorkspaceWindow> {
    let open = cx.windows();
    let order = &mut cx.default_global::<RecentWindows>().0;
    order.retain(|w| open.contains(&w.window) && w.workspace.upgrade().is_some());
    order.first().cloned()
}

/// The open main windows, the one focused last first.
pub fn main_windows(cx: &mut App) -> Vec<WorkspaceWindow> {
    most_recent_window(cx);
    cx.default_global::<RecentWindows>().0.clone()
}

/// `window.bring_all_to_front` (macOS Window menu): every main window
/// comes forward, in the order they were last used, the latest on top.
fn bring_all_to_front(cx: &mut App) {
    cx.activate(true);
    for entry in main_windows(cx).into_iter().rev() {
        // Fails only when the window has closed.
        let _ = cx.update_window(entry.window, |_, window, _| window.activate_window());
    }
}

/// Files opened for an edit someone waits for (`--wait`): Edit mode, and
/// the close hook, called once the file is final.
#[cfg(test)]
mod close_hook_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{TestAppContext, VisualTestContext};
    use gv_vault::files::journal::JournalStore;

    use super::*;
    use crate::{AppSettings, Placement, ThemeChoice};

    const NOTE: &str = "# Title\n\nFirst paragraph.\n";

    /// What a hook saw when it was called: the report and the file's text
    /// at that moment.
    type Seen = Rc<RefCell<Vec<(FileClosed, String)>>>;

    struct Fixture {
        dir: tempfile::TempDir,
        /// Weak, so that closing the window releases the workspace.
        workspace: WeakEntity<Workspace>,
        vcx: VisualTestContext,
        seen: Seen,
    }

    fn hook(seen: &Seen) -> CloseHook {
        let seen = Rc::clone(seen);
        CloseHook::new(move |closed, _| {
            let text = std::fs::read_to_string(&closed.path).unwrap_or_default();
            seen.borrow_mut().push((closed, text));
        })
    }

    /// `names` written in a temporary folder with a journal, opened for
    /// editing in a new window, each with a hook that notes what it saw.
    fn fixture(cx: &mut TestAppContext, names: &[&str], autosave: bool) -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in names {
            std::fs::write(dir.path().join(name), NOTE).expect("write");
        }
        let seen = Seen::default();
        let targets: Vec<_> = names
            .iter()
            .map(|name| (OpenTarget::new(dir.path().join(name)), hook(&seen)))
            .collect();
        let journal = dir.path().join("journal");
        let (window, workspace) = cx.update(|cx| {
            crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
            let store = JournalStore::open(journal).expect("journal store");
            crate::files::install_store(cx, Some(store), false);
            crate::files::override_autosave(cx, Some(autosave));
            let window =
                crate::open_for_edit(cx, targets, Placement::NewWindow).expect("files open");
            let workspace = most_recent_window(cx).expect("a workspace").workspace;
            (window, workspace)
        });
        let mut vcx = VisualTestContext::from_window(window, cx);
        draw(&mut vcx);
        Fixture {
            dir,
            workspace,
            vcx,
            seen,
        }
    }

    fn draw(vcx: &mut VisualTestContext) {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }

    impl Fixture {
        fn workspace(&self) -> Entity<Workspace> {
            self.workspace.upgrade().expect("the window is open")
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn type_at_end(&mut self, text: &str) {
            self.vcx.simulate_keystrokes("secondary-end");
            self.vcx.simulate_input(text);
            draw(&mut self.vcx);
        }

        fn close_active_tab(&mut self) {
            self.workspace().update_in(&mut self.vcx, |ws, window, cx| {
                ws.close_tab(ws.active, window, cx);
            });
            draw(&mut self.vcx);
        }

        fn seen(&self) -> Vec<(FileClosed, String)> {
            self.seen.borrow().clone()
        }

        fn journals(&self) -> usize {
            std::fs::read_dir(self.dir.path().join("journal"))
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                        .count()
                })
                .unwrap_or(0)
        }
    }

    #[gpui::test]
    fn files_open_in_edit_mode_with_the_panels_hidden(cx: &mut TestAppContext) {
        let mut f = fixture(cx, &["a.md", "b.md"], true);
        let (a, b) = (f.path("a.md"), f.path("b.md"));
        f.workspace().read_with(&f.vcx, |ws, cx| {
            assert_eq!(ws.panels_open(), (false, false));
            assert!(
                !ws.in_session(),
                "a window for files stays out of the session"
            );
            let tabs: Vec<_> = ws.tabs().iter().map(|t| (t.path(), t.mode())).collect();
            assert_eq!(
                tabs,
                [
                    (Some(a.as_path()), ViewMode::Edit),
                    (Some(b.as_path()), ViewMode::Edit)
                ]
            );
            assert_eq!(ws.active, 1);
            let editor = ws.active_tab().editor().expect("a Source view");
            assert_eq!(
                editor.read(cx).status(),
                &crate::editor::EditorStatus::Ready
            );
        });
        // The editor has the keyboard: typing edits the file.
        f.type_at_end("B");
        let modified = f.workspace().read_with(&f.vcx, |ws, cx| {
            ws.active_tab()
                .editor()
                .is_some_and(|e| e.read(cx).is_modified())
        });
        assert!(modified);
        assert!(f.seen().is_empty(), "nothing is closed yet");
    }

    #[gpui::test]
    fn closing_the_tab_saves_then_calls_the_hook(cx: &mut TestAppContext) {
        let mut f = fixture(cx, &["msg.md"], true);
        f.type_at_end("Done.");
        f.close_active_tab();
        let edited = format!("{NOTE}Done.");
        assert_eq!(
            f.seen(),
            [(
                FileClosed {
                    path: f.path("msg.md"),
                    unsaved: false
                },
                edited.clone()
            )],
            "the hook sees the saved file"
        );
        assert_eq!(f.journals(), 0);

        // Once only: reopening the tab and closing it again is not reported.
        f.workspace().update_in(&mut f.vcx, |ws, window, cx| {
            ws.reopen_closed_tab(window, cx)
        });
        draw(&mut f.vcx);
        f.close_active_tab();
        assert_eq!(f.seen().len(), 1);
    }

    #[gpui::test]
    fn without_autosave_closing_asks_first(cx: &mut TestAppContext) {
        let mut f = fixture(cx, &["msg.md"], false);
        f.type_at_end("Done.");
        f.close_active_tab();
        assert!(f.seen().is_empty(), "the tab asks before closing");
        assert_eq!(f.journals(), 1, "the change is kept meanwhile");
        // Don't save: reported closed with the file untouched.
        f.workspace().update_in(&mut f.vcx, |ws, window, cx| {
            ws.close_answered(false, window, cx);
        });
        draw(&mut f.vcx);
        assert_eq!(
            f.seen(),
            [(
                FileClosed {
                    path: f.path("msg.md"),
                    unsaved: false
                },
                NOTE.to_owned()
            )],
            "reported closed with the file untouched"
        );
        assert_eq!(f.journals(), 0, "the change was dropped");
    }

    #[gpui::test]
    fn closing_the_window_calls_every_hook(cx: &mut TestAppContext) {
        let mut f = fixture(cx, &["a.md", "b.md"], true);
        f.type_at_end("B");
        assert!(!f.vcx.simulate_close(), "the window waits for its save");
        f.vcx.run_until_parked();
        assert!(f.workspace.upgrade().is_none(), "the window is gone");
        let mut seen = f.seen();
        seen.sort_by(|x, y| x.0.path.cmp(&y.0.path));
        assert_eq!(
            seen,
            [
                (
                    FileClosed {
                        path: f.path("a.md"),
                        unsaved: false
                    },
                    NOTE.to_owned()
                ),
                (
                    FileClosed {
                        path: f.path("b.md"),
                        unsaved: false
                    },
                    format!("{NOTE}B")
                ),
            ]
        );
    }

    #[gpui::test]
    fn a_window_removed_without_asking_still_saves_first(cx: &mut TestAppContext) {
        // A window removed without a close request (the client-drawn close
        // button asks first) still calls the hooks on release.
        let mut f = fixture(cx, &["msg.md"], true);
        f.type_at_end("Done.");
        f.vcx.update(|window, _| window.remove_window());
        f.vcx.run_until_parked();
        assert!(f.workspace.upgrade().is_none(), "the window is gone");
        assert_eq!(
            f.seen(),
            [(
                FileClosed {
                    path: f.path("msg.md"),
                    unsaved: false
                },
                format!("{NOTE}Done.")
            )]
        );
    }

    #[gpui::test]
    fn showing_another_file_lets_go_of_the_first(cx: &mut TestAppContext) {
        let mut f = fixture(cx, &["a.md", "b.md"], true);
        let (a, b) = (f.path("a.md"), f.path("b.md"));
        f.workspace().update_in(&mut f.vcx, |ws, window, cx| {
            ws.navigate(0, b, None, window, cx);
        });
        draw(&mut f.vcx);
        assert_eq!(
            f.seen(),
            [(
                FileClosed {
                    path: a,
                    unsaved: false
                },
                NOTE.to_owned()
            )]
        );
    }

    #[gpui::test]
    fn quitting_calls_the_hooks_of_open_tabs(cx: &mut TestAppContext) {
        let mut f = fixture(cx, &["a.md"], true);
        f.type_at_end("A");
        // As `app.quit` does: save first, then let the tabs go.
        f.vcx.update(|_, cx| {
            crate::shell::prepare_quit(cx);
        });
        draw(&mut f.vcx);
        f.vcx.update(|_, cx| release_close_hooks(cx));
        assert_eq!(
            f.seen(),
            [(
                FileClosed {
                    path: f.path("a.md"),
                    unsaved: false
                },
                format!("{NOTE}A")
            )]
        );
        // Nothing is called twice when the window then goes.
        f.vcx.simulate_close();
        f.vcx.run_until_parked();
        assert_eq!(f.seen().len(), 1);
    }
}

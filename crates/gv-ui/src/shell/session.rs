// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Session state: the windows, tabs and panels a plain launch brings back,
//! and the recent files and folders (FR-VAULT-14).
//!
//! # What is kept
//!
//! Per window: its bounds, its tabs in order, the active tab, whether each
//! side panel is shown, which panel the sidebar shows, and the folder it
//! was opened on (*Open folder*: its vault comes back with it, even without
//! tabs), and the sidebar base in the right panel (its base file, view and
//! whether its section is folded). Per tab: the file, the view mode, the
//! scroll anchor (the source offset of the block at the top, never
//! pixels), the back and forward history, the Split view's divider, the
//! Source view's folds (byte ranges; restored only where they are still
//! foldable regions) and the Reading view's folds (callouts and
//! `<details>`, by kind, first source line and repeat, so they survive
//! edits elsewhere; [`crate::reader::fold_state`]), viewer mode when
//! switched for the tab, and, in a PDF, the page at the top, how far down
//! it and the zoom (a page past the end comes back as the last page); a
//! PDF in the back and forward history keeps its place the same way.
//! Closed tabs are not kept.
//!
//! Every window that is not created for a file is a session window, so
//! several windows (`workspace.new_window`, *Open folder* in a window
//! that already shows something) are saved and restored together, each
//! with its own vault and tabs.
//!
//! # Which windows
//!
//! A window is either a *session window* (restored, or opened empty) or a
//! window created for a file: one passed at launch, or later with
//! `--new-window` (FR-VIEW-16). The rules:
//!
//! - Only a launch without files restores the session ([`restore`]). A launch
//!   with files opens just those files, in Preview with the panels hidden.
//! - A window created for a file is never written to the session: not its
//!   tabs, not its panels. So panels the user opens there last for that window
//!   only, and the next launch with a file starts with them hidden again, as
//!   does the next plain launch (Q-14).
//! - Only a process that restored the session writes the window list. A
//!   launch with files therefore leaves the stored windows exactly as they
//!   were, so opening one file from a terminal never costs the user their
//!   session. It still records recent files.
//! - A file routed into a session window opens as a tab there and is saved
//!   with it; that window's panels stay as they are (Q-13).
//! - Closing a session window while others stay open drops it from the
//!   session. Closing the last window quits, and that window is kept.
//!
//! # Saving
//!
//! Changes are saved [`SAVE_DELAY`] after the first unsaved one; changes in
//! between are folded into that save, so the file is written at most once
//! per [`SAVE_DELAY`]. The snapshot is taken on the UI thread (it is small)
//! and written by a [`SessionSink`] on a background thread. On quit the
//! session is written once more, synchronously, because the process ends
//! right after. Writes are numbered, so an older background write that
//! finishes late never replaces a newer one.
//!
//! The storage itself (file format, versions, damaged files) is the sink's
//! business; the application provides it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Global, Pixels, Result, WeakEntity,
    WindowBounds, WindowId, point, px, size,
};
use serde::{Deserialize, Serialize};

use super::history::HISTORY_LIMIT;
use super::panels::SidebarPanel;
use super::{FileLabel, History, HistoryEntry, ViewMode, Workspace, most_recent_window};
use crate::chrome::WindowChrome;
use crate::file_viewer::PdfPlace;
use crate::reader::ScrollTarget;
use crate::reader::fold_state::ReadingFold;

/// The session format this build reads and writes.
pub const SESSION_VERSION: u32 = 1;

/// Most entries each recent list keeps.
pub const RECENT_LIMIT: usize = 50;

/// How long after the first unsaved change the session is saved.
pub const SAVE_DELAY: Duration = Duration::from_secs(2);

/// The smallest window a stored size is grown to.
const MIN_WIDTH: f32 = 360.0;
const MIN_HEIGHT: f32 = 240.0;

/// The saved session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<WindowState>,
    /// The window that was used last, as an index into `windows`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_window: Option<usize>,
    /// Most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_files: Vec<PathBuf>,
    /// Most recent first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_folders: Vec<PathBuf>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            version: SESSION_VERSION,
            windows: Vec::new(),
            active_window: None,
            recent_files: Vec::new(),
            recent_folders: Vec::new(),
        }
    }
}

impl Session {
    /// Puts `path` first in the recent files.
    pub fn touch_recent_file(&mut self, path: &Path) {
        touch(&mut self.recent_files, path);
    }

    /// Puts `path` first in the recent folders.
    pub fn touch_recent_folder(&mut self, path: &Path) {
        touch(&mut self.recent_folders, path);
    }
}

/// Moves `path` to the front of `list`, which keeps [`RECENT_LIMIT`] entries.
fn touch(list: &mut Vec<PathBuf>, path: &Path) {
    if !storable(path) {
        return;
    }
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    list.retain(|p| *p != path);
    list.insert(0, path);
    list.truncate(RECENT_LIMIT);
}

/// Paths are stored as text; one that is not valid Unicode is left out.
fn storable(path: &Path) -> bool {
    path.to_str().is_some()
}

/// One window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<SavedBounds>,
    /// Empty for a window showing only its empty tab.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default)]
    pub sidebar_open: bool,
    #[serde(default)]
    pub panel_open: bool,
    /// The sidebar's panel ([`SidebarPanel::id`]); the Files panel when
    /// missing or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_panel: Option<String>,
    /// The folder the window was opened on: its vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<PathBuf>,
    /// The sidebar base in the right panel, once shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<SidebarBase>,
}

/// The right panel's sidebar base: which base and view it showed, and
/// whether its section was folded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarBase {
    /// The base file, as a vault path; none chosen yet when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The view shown, by name (the base's first view when missing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub collapsed: bool,
}

/// Where a window was, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedBounds {
    #[serde(default)]
    pub state: BoundsState,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// How a window was shown. A maximized or full-screen window keeps the
/// bounds it returns to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoundsState {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

impl SavedBounds {
    pub fn from_window(bounds: WindowBounds) -> Self {
        let (state, b) = match bounds {
            WindowBounds::Windowed(b) => (BoundsState::Windowed, b),
            WindowBounds::Maximized(b) => (BoundsState::Maximized, b),
            WindowBounds::Fullscreen(b) => (BoundsState::Fullscreen, b),
        };
        Self {
            state,
            x: f32::from(b.origin.x),
            y: f32::from(b.origin.y),
            width: f32::from(b.size.width),
            height: f32::from(b.size.height),
        }
    }

    fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

/// One tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    pub path: PathBuf,
    #[serde(default)]
    pub mode: ViewMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// Where Back goes, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub back: Vec<Place>,
    /// Where Forward goes, the next one last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward: Vec<Place>,
    /// The Split view's divider, as a share of its width, when moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_ratio: Option<f32>,
    /// The Source view's folds, as byte ranges `[start, end]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folds: Vec<[usize; 2]>,
    /// The Reading view's folds (callouts and `<details>` opened or
    /// closed), by block kind, first source line and repeat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reading_folds: Vec<ReadingFold>,
    /// The Split view's preview folds, kept apart from the Reading
    /// view's (each pane folds on its own).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub split_reading_folds: Vec<ReadingFold>,
    /// Edit mode's editing style: Live Preview (`true`) or Source.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub live_preview: bool,
    /// Viewer mode switched for this tab (`view.toggle_viewer_mode`); the
    /// setting applies when missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_mode: Option<bool>,
    /// Where the PDF viewer was, for a PDF read past its start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<PdfPlace>,
}

/// A place in a tab's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub path: PathBuf,
    #[serde(default)]
    pub mode: ViewMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// Where the PDF viewer was, for a PDF read past its start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pdf: Option<PdfPlace>,
}

/// Where a document was scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    /// The source offset of the block at the top.
    Offset(usize),
    /// A 1-based line, for a tab opened at `PATH:LINE` and not shown yet.
    Line(u32),
}

impl Anchor {
    /// A link fragment is not kept: it was a request, not a position.
    pub fn from_target(target: &ScrollTarget) -> Option<Self> {
        match target {
            ScrollTarget::Offset(offset) => Some(Self::Offset(*offset)),
            ScrollTarget::Line(line) => Some(Self::Line(*line)),
            ScrollTarget::Fragment(_) => None,
        }
    }

    pub fn target(self) -> ScrollTarget {
        match self {
            Self::Offset(offset) => ScrollTarget::Offset(offset),
            Self::Line(line) => ScrollTarget::Line(line),
        }
    }
}

impl Place {
    fn from_entry(entry: &HistoryEntry) -> Option<Self> {
        storable(&entry.path).then(|| Self {
            path: entry.path.clone(),
            mode: entry.mode,
            anchor: entry.scroll.as_ref().and_then(Anchor::from_target),
            pdf: entry.pdf.filter(|p| !p.is_start()),
        })
    }

    fn entry(&self) -> HistoryEntry {
        HistoryEntry {
            path: self.path.clone(),
            mode: self.mode,
            scroll: self.anchor.map(Anchor::target),
            pdf: self.pdf.map(PdfPlace::sanitized),
        }
    }
}

/// The places of a history stack that can be stored, at most
/// [`HISTORY_LIMIT`]. Both stacks hold the nearest place last, so the last
/// ones are kept.
fn places(entries: &[HistoryEntry]) -> Vec<Place> {
    let mut places: Vec<Place> = entries.iter().filter_map(Place::from_entry).collect();
    let extra = places.len().saturating_sub(HISTORY_LIMIT);
    places.drain(..extra);
    places
}

/// A tab's history rebuilt from its stored stacks.
fn history(back: &[Place], forward: &[Place]) -> History {
    let mut history = History::default();
    for place in back {
        history.push(place.entry());
    }
    // `History` is built through its own operations: going back moves the
    // current place onto the forward stack. Push the forward places so that
    // going back once per place stacks them in order; the first push is
    // only a stand-in for the place the last step leaves.
    let Some((first, rest)) = forward.split_first() else {
        return history;
    };
    history.push(first.entry());
    for place in rest.iter().rev() {
        history.push(place.entry());
    }
    let mut current = first.entry();
    for _ in 0..forward.len() {
        match history.go_back(current) {
            Some(to) => current = to,
            None => break,
        }
    }
    history
}

impl TabState {
    pub(super) fn into_tab(self) -> super::Tab {
        let mut tab = super::Tab::new(Some(super::OpenTarget::new(self.path.clone())));
        tab.mode = self.mode;
        tab.live_preview = self.live_preview;
        tab.viewer_toggle = self.viewer_mode;
        tab.pdf_place = self.pdf.map(|p| (self.path.clone(), p.sanitized()));
        tab.scroll = self.anchor.map(Anchor::target);
        tab.history = history(&self.back, &self.forward);
        tab.split_ratio = self
            .split_ratio
            .filter(|r| r.is_finite() && *r > 0.0 && *r < 1.0);
        let folds: Vec<std::ops::Range<usize>> = self
            .folds
            .iter()
            .filter(|[start, end]| start < end)
            .map(|[start, end]| *start..*end)
            .collect();
        if !folds.is_empty() {
            tab.restored_folds = Some((self.path.clone(), folds));
        }
        if !self.split_reading_folds.is_empty() {
            tab.restored_split_folds = Some((self.path.clone(), self.split_reading_folds));
        }
        if !self.reading_folds.is_empty() {
            tab.restored_reading_folds = Some((self.path, self.reading_folds));
        }
        tab
    }
}

/// The window state of `workspace`, as it would be saved now.
pub(super) fn snapshot(workspace: &Workspace, cx: &App) -> WindowState {
    let mut tabs = Vec::new();
    let mut active_tab = 0;
    for (index, tab) in workspace.tabs.iter().enumerate() {
        let Some(place) = workspace.place(index, cx) else {
            continue;
        };
        if !storable(&place.path) {
            continue;
        }
        if index == workspace.active {
            active_tab = tabs.len();
        }
        let split_ratio = tab
            .split
            .as_ref()
            .map(|s| s.read(cx).ratio())
            .or(tab.split_ratio)
            .filter(|r| (r - 0.5).abs() > f32::EPSILON);
        let folds = tab
            .folds(cx)
            .into_iter()
            .map(|r| [r.start, r.end])
            .collect();
        tabs.push(TabState {
            path: place.path,
            mode: place.mode,
            anchor: place.scroll.as_ref().and_then(Anchor::from_target),
            back: places(tab.history.back()),
            forward: places(tab.history.forward()),
            split_ratio,
            folds,
            reading_folds: tab.reading_folds(cx),
            split_reading_folds: tab.split_reading_folds(cx),
            live_preview: tab.live_preview,
            viewer_mode: tab.viewer_toggle,
            pdf: tab.pdf_place(cx).filter(|p| !p.is_start()),
        });
    }
    WindowState {
        bounds: Some(SavedBounds::from_window(workspace.bounds)),
        tabs,
        active_tab,
        sidebar_open: workspace.sidebar_open,
        panel_open: workspace.panel_open,
        sidebar_panel: (workspace.panels.sidebar != SidebarPanel::Files)
            .then(|| workspace.panels.sidebar.saved_id())
            .flatten()
            .map(str::to_owned),
        vault: workspace
            .vault
            .as_ref()
            .map(|v| v.read(cx))
            .filter(|v| v.opened_as_folder())
            .map(|v| v.root().to_path_buf())
            .filter(|p| storable(p)),
        base: workspace.sidebar_base_state(cx),
    }
}

/// `state` with the tabs whose files are gone left out, or `None` when it
/// had tabs and none is left (a window opened on a folder stays, without
/// tabs).
pub fn present(mut state: WindowState, exists: impl Fn(&Path) -> bool) -> Option<WindowState> {
    if state.tabs.is_empty() {
        return Some(state);
    }
    let active = state.active_tab.min(state.tabs.len() - 1);
    let mut kept = Vec::with_capacity(state.tabs.len());
    let mut new_active = None;
    for (index, tab) in state.tabs.into_iter().enumerate() {
        if exists(&tab.path) {
            if index <= active {
                new_active = Some(kept.len());
            }
            kept.push(tab);
        } else {
            tracing::info!("session: skipped {}: the file is gone", tab.path.display());
        }
    }
    // A window opened on a folder stays without its files.
    if kept.is_empty() {
        state.vault.as_ref()?;
    }
    // The active tab, or the nearest kept tab before it, or the first.
    state.active_tab = new_active.filter(|_| !kept.is_empty()).unwrap_or(0);
    state.tabs = kept;
    Some(state)
}

/// A rectangle in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    fn overlap(&self, other: &Rect) -> f32 {
        let w = (self.x + self.width).min(other.x + other.width) - self.x.max(other.x);
        let h = (self.y + self.height).min(other.y + other.height) - self.y.max(other.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }
}

/// Where a restored window goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fit {
    At(Rect),
    /// Centred on the primary display with this width and height.
    Centered(f32, f32),
}

/// Fits stored bounds to the displays there are now: on the display it
/// overlaps most, shrunk to that display and moved inside it. A window that
/// is on no display any more is centred. Without display information the
/// bounds are kept. Sizes never go below a usable minimum.
pub fn fit(saved: Rect, displays: &[Rect]) -> Fit {
    let width = if saved.width.is_finite() {
        saved.width.max(MIN_WIDTH)
    } else {
        MIN_WIDTH
    };
    let height = if saved.height.is_finite() {
        saved.height.max(MIN_HEIGHT)
    } else {
        MIN_HEIGHT
    };
    let finite = saved.x.is_finite() && saved.y.is_finite();
    let wanted = Rect {
        x: if finite { saved.x } else { 0.0 },
        y: if finite { saved.y } else { 0.0 },
        width,
        height,
    };
    if displays.is_empty() {
        return if finite {
            Fit::At(wanted)
        } else {
            Fit::Centered(width, height)
        };
    }
    let best = displays
        .iter()
        .map(|d| (d, wanted.overlap(d)))
        .filter(|(_, area)| *area > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let Some((display, _)) = best.filter(|_| finite) else {
        let primary = displays[0];
        return Fit::Centered(width.min(primary.width), height.min(primary.height));
    };
    let width = width.min(display.width);
    let height = height.min(display.height);
    Fit::At(Rect {
        x: wanted.x.clamp(display.x, display.x + display.width - width),
        y: wanted
            .y
            .clamp(display.y, display.y + display.height - height),
        width,
        height,
    })
}

fn window_bounds(saved: SavedBounds, cx: &App) -> WindowBounds {
    let displays: Vec<Rect> = cx
        .displays()
        .iter()
        .map(|d| {
            let b = d.bounds();
            Rect {
                x: f32::from(b.origin.x),
                y: f32::from(b.origin.y),
                width: f32::from(b.size.width),
                height: f32::from(b.size.height),
            }
        })
        .collect();
    let bounds: Bounds<Pixels> = match fit(saved.rect(), &displays) {
        Fit::At(r) => Bounds::new(point(px(r.x), px(r.y)), size(px(r.width), px(r.height))),
        Fit::Centered(w, h) => Bounds::centered(None, size(px(w), px(h)), cx),
    };
    match saved.state {
        BoundsState::Windowed => WindowBounds::Windowed(bounds),
        BoundsState::Maximized => WindowBounds::Maximized(bounds),
        BoundsState::Fullscreen => WindowBounds::Fullscreen(bounds),
    }
}

/// Where the session is written. Called on a background thread, and once on
/// the UI thread when the application quits.
pub trait SessionSink: Send + Sync {
    fn save(&self, session: &Session) -> std::io::Result<()>;
}

/// Writes numbered snapshots through a sink, never one older than the last.
struct Writer {
    sink: Arc<dyn SessionSink>,
    written: Mutex<u64>,
}

impl Writer {
    /// Writes snapshot `seq`. Returns false when a newer one was written.
    fn write(&self, seq: u64, session: &Session) -> std::io::Result<bool> {
        let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        if seq <= *written {
            return Ok(false);
        }
        self.sink.save(session)?;
        *written = seq;
        Ok(true)
    }
}

/// When to save: one save is scheduled after a change, and changes made
/// while a save is waiting or running are folded into the next one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveSchedule {
    scheduled: bool,
    saving: bool,
    again: bool,
}

impl SaveSchedule {
    /// Something changed. True when a save must be scheduled now.
    pub fn change(&mut self) -> bool {
        if self.saving {
            self.again = true;
            false
        } else if self.scheduled {
            false
        } else {
            self.scheduled = true;
            true
        }
    }

    /// The scheduled save takes its snapshot.
    pub fn start(&mut self) {
        self.scheduled = false;
        self.saving = true;
    }

    /// The save finished. True when another must be scheduled for changes
    /// made meanwhile.
    pub fn finish(&mut self) -> bool {
        self.saving = false;
        if std::mem::take(&mut self.again) {
            self.scheduled = true;
            true
        } else {
            false
        }
    }
}

/// A session window: its last known state, kept when it closes as the
/// last window.
struct Tracked {
    window: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
    last: Option<WindowState>,
}

/// The session of the running application: what was loaded, the session
/// windows, and when to save.
pub struct SessionStore {
    session: Session,
    writer: Option<Arc<Writer>>,
    /// Set by [`restore`]: this process writes the window list.
    owns_windows: bool,
    tracked: Vec<Tracked>,
    schedule: SaveSchedule,
    seq: u64,
    /// The application is quitting: nothing changes any more.
    quitting: bool,
}

impl fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionStore")
            .field("session", &self.session)
            .field("writable", &self.writer.is_some())
            .field("owns_windows", &self.owns_windows)
            .field("tracked", &self.tracked.len())
            .field("schedule", &self.schedule)
            .finish_non_exhaustive()
    }
}

impl Global for SessionStore {}

impl SessionStore {
    /// The loaded `session`, saved through `sink`. Without a sink nothing is
    /// written (no data folder, or a file this build must not replace).
    pub fn new(session: Session, sink: Option<Arc<dyn SessionSink>>) -> Self {
        Self {
            session,
            writer: sink.map(|sink| {
                Arc::new(Writer {
                    sink,
                    written: Mutex::new(0),
                })
            }),
            owns_windows: false,
            tracked: Vec::new(),
            schedule: SaveSchedule::default(),
            seq: 0,
            quitting: false,
        }
    }

    /// The session as it would be saved now, from the last snapshots.
    fn current(&self, active: Option<AnyWindowHandle>) -> Session {
        let mut session = Session {
            version: SESSION_VERSION,
            recent_files: self.session.recent_files.clone(),
            recent_folders: self.session.recent_folders.clone(),
            ..Session::default()
        };
        if self.owns_windows {
            for tracked in &self.tracked {
                if let Some(state) = &tracked.last {
                    if Some(tracked.window) == active {
                        session.active_window = Some(session.windows.len());
                    }
                    session.windows.push(state.clone());
                }
            }
        } else {
            session.windows = self.session.windows.clone();
            session.active_window = self.session.active_window;
        }
        session
    }
}

/// Installs `store` and saves the session when the application quits and
/// when session windows close. Call once, after [`crate::init`].
pub fn install(cx: &mut App, store: SessionStore) {
    cx.set_global(store);
    cx.on_app_quit(|cx| {
        save_now(cx);
        if cx.has_global::<SessionStore>() {
            cx.global_mut::<SessionStore>().quitting = true;
        }
        async {}
    })
    .detach();
    cx.on_window_closed(window_closed).detach();
}

/// The recent files, most recent first.
pub fn recent_files(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<SessionStore>()
        .map(|s| s.session.recent_files.clone())
        .unwrap_or_default()
}

/// The recent folders, most recent first.
pub fn recent_folders(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<SessionStore>()
        .map(|s| s.session.recent_folders.clone())
        .unwrap_or_default()
}

/// Records that the file at `path` was opened.
pub fn note_recent_file(cx: &mut App, path: &Path) {
    if !cx.has_global::<SessionStore>() {
        return;
    }
    let store = cx.global_mut::<SessionStore>();
    if store.session.recent_files.first().map(PathBuf::as_path) == Some(path) {
        return;
    }
    store.session.touch_recent_file(path);
    changed(cx);
}

/// Records that the folder at `path` was opened.
pub fn note_recent_folder(cx: &mut App, path: &Path) {
    if !cx.has_global::<SessionStore>() {
        return;
    }
    cx.global_mut::<SessionStore>()
        .session
        .touch_recent_folder(path);
    changed(cx);
}

/// Empties the recent files and folders (the Recent panel's *Clear*).
pub fn clear_recent(cx: &mut App) {
    if !cx.has_global::<SessionStore>() {
        return;
    }
    let store = cx.global_mut::<SessionStore>();
    if store.session.recent_files.is_empty() && store.session.recent_folders.is_empty() {
        return;
    }
    store.session.recent_files.clear();
    store.session.recent_folders.clear();
    changed(cx);
}

/// Adds a window that was not created for a file to the session, when this
/// process writes the window list. Returns whether it was added.
pub(super) fn track(
    window: AnyWindowHandle,
    workspace: WeakEntity<Workspace>,
    cx: &mut App,
) -> bool {
    if !cx.has_global::<SessionStore>() {
        return false;
    }
    let store = cx.global_mut::<SessionStore>();
    if !store.owns_windows || store.quitting {
        return false;
    }
    store.tracked.push(Tracked {
        window,
        workspace,
        last: None,
    });
    true
}

/// Something in the session changed: a save is scheduled.
pub fn changed(cx: &mut App) {
    if !cx.has_global::<SessionStore>() {
        return;
    }
    let store = cx.global_mut::<SessionStore>();
    if store.writer.is_none() || store.quitting {
        return;
    }
    if store.schedule.change() {
        schedule_save(cx);
    }
}

fn schedule_save(cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SAVE_DELAY).await;
        let job = cx.update(|cx| {
            let store = cx.global_mut::<SessionStore>();
            if store.quitting {
                return None;
            }
            store.schedule.start();
            prepare(cx)
        });
        if let Some((writer, seq, session)) = job {
            let result = cx
                .background_spawn(async move { writer.write(seq, &session) })
                .await;
            match result {
                Ok(true) => tracing::debug!("session: saved"),
                Ok(false) => {}
                Err(err) => tracing::warn!("session: cannot save the session: {err}"),
            }
        }
        cx.update(|cx| {
            let store = cx.global_mut::<SessionStore>();
            if store.schedule.finish() && !store.quitting {
                schedule_save(cx);
            }
        });
    })
    .detach();
}

/// Refreshes the snapshots of the open session windows and returns what to
/// write, numbered.
fn prepare(cx: &mut App) -> Option<(Arc<Writer>, u64, Session)> {
    let windows: Vec<(usize, WeakEntity<Workspace>)> = cx
        .global::<SessionStore>()
        .tracked
        .iter()
        .enumerate()
        .map(|(i, t)| (i, t.workspace.clone()))
        .collect();
    let states: Vec<(usize, WindowState)> = windows
        .into_iter()
        .filter_map(|(i, ws)| Some((i, snapshot(ws.upgrade()?.read(cx), cx))))
        .collect();
    let active = most_recent_window(cx).map(|w| w.window);
    let store = cx.global_mut::<SessionStore>();
    for (i, state) in states {
        store.tracked[i].last = Some(state);
    }
    let writer = store.writer.clone()?;
    store.seq += 1;
    Some((writer, store.seq, store.current(active)))
}

/// Writes the session now, on this thread. Used when the application quits.
pub fn save_now(cx: &mut App) {
    if !cx.has_global::<SessionStore>() || cx.global::<SessionStore>().quitting {
        return;
    }
    let Some((writer, seq, session)) = prepare(cx) else {
        return;
    };
    match writer.write(seq, &session) {
        Ok(_) => tracing::debug!("session: saved"),
        Err(err) => tracing::warn!("session: cannot save the session: {err}"),
    }
}

/// A window closed. A session window closed while others stay open leaves
/// the session; the last one stays in it, because closing it quits.
fn window_closed(cx: &mut App, id: WindowId) {
    let Some(store) = cx.try_global::<SessionStore>() else {
        return;
    };
    if store.quitting {
        return;
    }
    let Some(index) = store
        .tracked
        .iter()
        .position(|t| t.window.window_id() == id)
    else {
        return;
    };
    // The window is gone, but its workspace is still alive while the
    // closing finishes.
    let state = store.tracked[index]
        .workspace
        .upgrade()
        .map(|ws| snapshot(ws.read(cx), cx));
    let others = !cx.windows().is_empty();
    let store = cx.global_mut::<SessionStore>();
    if let Some(state) = state {
        store.tracked[index].last = Some(state);
    }
    if others {
        store.tracked.remove(index);
        changed(cx);
    }
}

/// Restores the session's windows, for a launch without files. Files that
/// are gone are skipped with a log line, and a window left without tabs is
/// skipped. Returns the window brought to the front, or `None` when nothing
/// was restored. From now on this process writes the window list.
pub fn restore(cx: &mut App) -> Result<Option<AnyWindowHandle>> {
    if !cx.has_global::<SessionStore>() {
        return Ok(None);
    }
    let store = cx.global_mut::<SessionStore>();
    store.owns_windows = true;
    let windows = store.session.windows.clone();
    let active = store.session.active_window;
    let mut opened = Vec::new();
    let mut tabs = 0;
    for (index, mut state) in windows.into_iter().enumerate() {
        if state.vault.as_deref().is_some_and(|v| !v.is_dir()) {
            tracing::info!("session: a window's folder is gone");
            state.vault = None;
        }
        let Some(state) = present(state, Path::is_file) else {
            tracing::info!("session: skipped a window: none of its files is left");
            continue;
        };
        let count = state.tabs.len();
        match open_restored(cx, state) {
            Ok(window) => {
                opened.push((index, window));
                tabs += count;
            }
            Err(err) if opened.is_empty() => return Err(err),
            Err(err) => tracing::warn!("session: cannot open a window: {err:#}"),
        }
    }
    let front = opened
        .iter()
        .find(|(index, _)| Some(*index) == active)
        .or(opened.last())
        .map(|(_, window)| *window);
    if let Some(front) = front {
        // Fails only when the window has closed.
        let _ = cx.update_window(front, |_, window, _| window.activate_window());
    }
    tracing::info!(
        "session: restored {} window(s) with {tabs} tab(s)",
        opened.len()
    );
    Ok(front)
}

fn open_restored(cx: &mut App, state: WindowState) -> Result<AnyWindowHandle> {
    let active = state.tabs.get(state.active_tab).map(|t| t.path.as_path());
    let title = FileLabel::new(active).window_title();
    let mut options = crate::main_window_options(WindowChrome::current(), title.into(), cx);
    if let Some(bounds) = state.bounds {
        options.window_bounds = Some(window_bounds(bounds, cx));
    }
    let (window, _) = gpui::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workspace::restored(state, window, cx))
    })?;
    cx.activate(true);
    Ok(window)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;

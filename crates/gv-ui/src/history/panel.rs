// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The File history panel (not designed): built from the Sync conflicts
//! panel's head, list rows and text buttons, and the editor's mono text.
//!
//! - Head: "File history", the file's name and the number of versions.
//! - The versions, newest first: when (today and yesterday by name, else
//!   the date; the local time zone) and the size. Click or Up/Down select.
//! - The selected version below, read-only, with *Compare with current*
//!   (Enter), *Restore* and *Copy*. Compare and Restore are events for the
//!   window, which hands them to the file's Source view.
//! - Esc gives the keyboard back ([`HistoryEvent::Dismiss`]).
//!
//! Lists and texts are read off the UI thread. The list is read again when
//! the file changes ([`HistoryPanel::set_path`]) and after each new
//! snapshot.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Div, ElementId, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, UniformListScrollHandle, Window, div, px, uniform_list,
};
use gv_vault::files::history::{HistoryStore, Version};

use super::VersionHistory;
use crate::a11y::{A11yExt as _, AccessibleExt as _, Live, Role};
use crate::appearance::Appearance;
use crate::metrics::spacing::{list, list_row, panel_head};
use crate::metrics::{DISABLED_OPACITY, font, radius, size, text};
use crate::theme::Theme;

/// The panel's key context.
pub const KEY_CONTEXT: &str = "HistoryPanel";

gpui::actions!(
    history_panel,
    [
        /// Select the newer version.
        SelectPrevious,
        /// Select the older version.
        SelectNext,
        /// Compare the selected version with the current text.
        CompareSelected,
        /// Give the keyboard back to the document.
        Dismiss,
    ]
);

/// The panel's keys.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("up", SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", CompareSelected, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
    ]
}

/// What the panel asks of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryEvent {
    /// Compare `text`, a version of `path` saved at `time`, with the
    /// file's current text (the window calls
    /// [`crate::editor::EditorView::compare_with_version`] on its Source
    /// view; `label` names the version's side).
    Compare {
        path: PathBuf,
        time: SystemTime,
        label: SharedString,
        text: Arc<str>,
    },
    /// Replace the file's text with `text` as one undoable edit
    /// ([`crate::editor::EditorView::restore_version`]); nothing is
    /// written until the document is saved.
    Restore {
        path: PathBuf,
        time: SystemTime,
        text: Arc<str>,
    },
    /// Esc: the window gives the keyboard back to the document.
    Dismiss,
}

/// The list's state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum List {
    /// No file (an untitled document, no tab).
    NoFile,
    /// Versions are not kept (no data folder, safe mode).
    NoStore,
    Loading,
    Ready(Vec<Version>),
    Failed(String),
}

/// The selected version's text.
#[derive(Debug, Clone)]
struct Preview {
    hash: gv_vault::files::ContentHash,
    text: Result<Arc<str>, String>,
    lines: Arc<[SharedString]>,
}

/// The versions of one file.
pub struct HistoryPanel {
    path: Option<PathBuf>,
    list: List,
    selected: Option<usize>,
    preview: Option<Preview>,
    copied: bool,
    generation: u64,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    _load: Option<Task<()>>,
    _preview: Option<Task<()>>,
    _history: gpui::Subscription,
}

impl EventEmitter<HistoryEvent> for HistoryPanel {}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl std::fmt::Debug for HistoryPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryPanel")
            .field("path", &self.path)
            .field("list", &self.list)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl HistoryPanel {
    /// A panel on the versions of `path` (`None`: no file yet).
    pub fn new(path: Option<PathBuf>, cx: &mut Context<'_, Self>) -> Self {
        let history = cx.observe_global::<VersionHistory>(|this, cx| {
            let generation = cx.global::<VersionHistory>().generation();
            if generation != this.generation {
                this.generation = generation;
                this.reload(cx);
            }
        });
        let generation = cx
            .try_global::<VersionHistory>()
            .map_or(0, VersionHistory::generation);
        let mut panel = Self {
            path: None,
            list: List::NoFile,
            selected: None,
            preview: None,
            copied: false,
            generation,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            _load: None,
            _preview: None,
            _history: history,
        };
        panel.set_path(path, cx);
        panel
    }

    /// Shows the versions of `path`, as when the active tab changes.
    pub fn set_path(&mut self, path: Option<PathBuf>, cx: &mut Context<'_, Self>) {
        if path == self.path && !matches!(self.list, List::NoFile) {
            return;
        }
        self.path = path;
        self.selected = None;
        self.preview = None;
        self.reload(cx);
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The versions listed, newest first (empty while loading).
    pub fn versions(&self) -> &[Version] {
        match &self.list {
            List::Ready(versions) => versions,
            _ => &[],
        }
    }

    /// Whether the list is being read.
    pub fn is_loading(&self) -> bool {
        self.list == List::Loading
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The selected version's text, once read.
    pub fn preview_text(&self) -> Option<Arc<str>> {
        self.preview.as_ref()?.text.clone().ok()
    }

    /// Reads the list again (keeping the selected version if still there).
    pub fn reload(&mut self, cx: &mut Context<'_, Self>) {
        let Some(path) = self.path.clone() else {
            self.list = List::NoFile;
            cx.notify();
            return;
        };
        let Some(store) = super::store(cx) else {
            self.list = List::NoStore;
            cx.notify();
            return;
        };
        if !matches!(self.list, List::Ready(_)) {
            self.list = List::Loading;
        }
        let load = cx.background_spawn(async move { store.versions(&path) });
        let for_path = self.path.clone();
        self._load = Some(cx.spawn(async move |this, cx| {
            let loaded = load.await;
            this.update(cx, |this, cx| {
                if this.path != for_path {
                    return;
                }
                let selected_hash = this
                    .selected
                    .and_then(|i| this.versions().get(i))
                    .map(|v| (v.time, v.hash));
                this.list = match loaded {
                    Ok(versions) => List::Ready(versions),
                    Err(err) => List::Failed(format!("The versions cannot be read: {err}")),
                };
                let still = selected_hash.and_then(|(time, hash)| {
                    this.versions()
                        .iter()
                        .position(|v| v.time == time && v.hash == hash)
                });
                match still {
                    Some(i) => this.selected = Some(i),
                    None => {
                        this.selected = None;
                        this.preview = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Selects version `index` (newest first) and reads its text.
    pub fn select(&mut self, index: usize, cx: &mut Context<'_, Self>) {
        let Some(version) = self.versions().get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        self.copied = false;
        self.scroll.scroll_to_item(index, gpui::ScrollStrategy::Top);
        if self
            .preview
            .as_ref()
            .is_some_and(|p| p.hash == version.hash)
        {
            cx.notify();
            return;
        }
        self.preview = None;
        let Some(store) = super::store(cx) else {
            return;
        };
        let read = cx.background_spawn(async move { read_text(&store, &version) });
        self._preview = Some(cx.spawn(async move |this, cx| {
            let preview = read.await;
            this.update(cx, |this, cx| {
                let current = this.selected.and_then(|i| this.versions().get(i));
                if current.is_some_and(|v| v.hash == preview.hash) {
                    this.preview = Some(preview);
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// *Compare with current*.
    pub fn compare(&mut self, cx: &mut Context<'_, Self>) {
        if let Some((path, version, text)) = self.chosen() {
            let label = format!("Version of {}", when(version.time, SystemTime::now()));
            cx.emit(HistoryEvent::Compare {
                path,
                time: version.time,
                label: label.into(),
                text,
            });
        }
    }

    /// *Restore*.
    pub fn restore(&mut self, cx: &mut Context<'_, Self>) {
        if let Some((path, version, text)) = self.chosen() {
            cx.emit(HistoryEvent::Restore {
                path,
                time: version.time,
                text,
            });
        }
    }

    /// *Copy*: the selected version's text to the clipboard.
    pub fn copy(&mut self, cx: &mut Context<'_, Self>) {
        if let Some((_, _, text)) = self.chosen() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
            self.copied = true;
            cx.notify();
        }
    }

    /// The file, the selected version and its text, once read.
    fn chosen(&self) -> Option<(PathBuf, Version, Arc<str>)> {
        let version = self.versions().get(self.selected?)?.clone();
        let text = self.preview_text()?;
        Some((self.path.clone()?, version, text))
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<'_, Self>) {
        let index = self.selected.map_or(0, |i| i.saturating_sub(1));
        self.select(index, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<'_, Self>) {
        let last = self.versions().len().saturating_sub(1);
        let index = self.selected.map_or(0, |i| (i + 1).min(last));
        self.select(index, cx);
    }

    fn compare_selected(
        &mut self,
        _: &CompareSelected,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.compare(cx);
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<'_, Self>) {
        cx.emit(HistoryEvent::Dismiss);
    }
}

fn read_text(store: &HistoryStore, version: &Version) -> Preview {
    let text: Result<Arc<str>, String> = match store.read(version) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into()),
        Err(err) => Err(sentence(&err.to_string())),
    };
    let lines: Arc<[SharedString]> = match &text {
        Ok(text) => text
            .split('\n')
            .map(|l| SharedString::from(l.strip_suffix('\r').unwrap_or(l).to_owned()))
            .collect(),
        Err(_) => Arc::new([]),
    };
    Preview {
        hash: version.hash,
        text,
        lines,
    }
}

/// When a version was saved, in the local time zone: "Today 14:03",
/// "Yesterday 09:12", else "12 Nov 2026 14:03".
pub fn when(time: SystemTime, now: SystemTime) -> String {
    use chrono::{DateTime, Local};
    let at: DateTime<Local> = time.into();
    let today: DateTime<Local> = now.into();
    let day = at.date_naive();
    if day == today.date_naive() {
        format!("Today {}", at.format("%H:%M"))
    } else if today.date_naive().pred_opt() == Some(day) {
        format!("Yesterday {}", at.format("%H:%M"))
    } else {
        at.format("%-d %b %Y %H:%M").to_string()
    }
}

/// A size for people: "512 B", "1.2 KB", "3.4 MB".
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    let mut text: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    if !text.ends_with('.') {
        text.push('.');
    }
    text
}

/// `element` as a polite status saying `text`.
fn announced(element: Div, id: &'static str, text: impl Into<SharedString>) -> AnyElement {
    element
        .id(id)
        .role(Role::Status)
        .aria_label(text)
        .accessible()
        .live(Live::Polite)
        .into_any_element()
}

fn message(note: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .debug_selector(|| "history-message".into())
        .min_h(px(size::LIST_ROW))
        .flex_none()
        .mt(px(list::PAD_Y))
        .mx(px(size::LIST_ROW_INSET))
        .pl(px(list_row::PAD_LEFT))
        .flex()
        .items_center()
        .text_color(theme.text.secondary)
        .child(note.into())
}

impl HistoryPanel {
    fn render_head(&self, theme: &Theme) -> Div {
        let name = self
            .path
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned());
        let count = self.versions().len();
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
                    .flex_none()
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child("File history"),
            )
            .when_some(name, |head, name| {
                head.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .pl(px(panel_head::TITLE_GAP))
                        .text_color(theme.text.secondary)
                        .child(name),
                )
            })
            .when(count > 0, |head| {
                head.child(
                    div()
                        .debug_selector(|| "history-count".into())
                        .flex_none()
                        .text_color(theme.text.tertiary)
                        .text_size(px(text::UI_SMALL.size))
                        .child(count.to_string()),
                )
            })
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let theme = Appearance::theme(cx);
        let now = SystemTime::now();
        let hover = theme.state.hover;
        range
            .filter_map(|ix| {
                let v = self.versions().get(ix)?;
                let selected = self.selected == Some(ix);
                Some(
                    div()
                        .w_full()
                        .h(px(size::LIST_ROW))
                        .px(px(size::LIST_ROW_INSET))
                        .child(
                            div()
                                .id(("history-row", ix))
                                .a11y_list_item(when(v.time, now), ix, self.versions().len())
                                .aria_selected(selected)
                                .when(selected, |r| r.aria_active_descendant())
                                .debug_selector(move || format!("history-row-{ix}"))
                                .size_full()
                                .flex()
                                .items_center()
                                .gap(px(list_row::GAP))
                                .pl(px(list_row::PAD_LEFT))
                                .pr(px(list_row::PAD_RIGHT))
                                .rounded(px(radius::CONTROL))
                                .map(|row| {
                                    if selected {
                                        row.bg(theme.accent.tint)
                                    } else {
                                        row.hover(move |s| s.bg(hover))
                                    }
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.focus.focus(window, cx);
                                    this.select(ix, cx);
                                }))
                                .child(div().flex_1().min_w_0().truncate().child(when(v.time, now)))
                                .child(
                                    div()
                                        .flex_none()
                                        .text_size(px(text::UI_SMALL.size))
                                        .text_color(theme.text.secondary)
                                        .child(human_size(v.size)),
                                )
                                .accessible(),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn action(
        &self,
        id: &'static str,
        label: &'static str,
        enabled: bool,
        on_click: impl Fn(&mut Self, &mut Context<'_, Self>) + 'static,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let (hover, press) = (theme.state.hover, theme.state.press);
        let button = div()
            .id(ElementId::Name(id.into()))
            .a11y_button(label)
            .debug_selector(move || id.into())
            .flex_none()
            .px(px(list_row::GAP))
            .rounded(px(radius::CONTROL))
            .text_color(theme.accent.default)
            .text_size(px(text::UI_SMALL.size))
            .line_height(px(text::UI_SMALL.line))
            .child(label);
        if enabled {
            button
                .keyboard_control(theme)
                .hover(move |s| s.bg(hover))
                .active(move |s| s.bg(press))
                .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
                .accessible()
                .into_any_element()
        } else {
            button
                .opacity(DISABLED_OPACITY)
                .accessible()
                .disabled(true)
                .into_any_element()
        }
    }

    fn render_preview(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> AnyElement {
        let ready = self.preview_text().is_some();
        let buttons = div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(list_row::GAP))
            .px(px(size::LIST_ROW_INSET))
            .py(px(list::PAD_Y))
            .child(self.action(
                "history-compare",
                "Compare with current",
                ready,
                Self::compare,
                theme,
                cx,
            ))
            .child(self.action(
                "history-restore",
                "Restore",
                ready,
                Self::restore,
                theme,
                cx,
            ))
            .child(self.action(
                "history-copy",
                if self.copied { "Copied" } else { "Copy" },
                ready,
                Self::copy,
                theme,
                cx,
            ));
        let body = match &self.preview {
            None => announced(message("Reading…", theme), "history-reading", "Reading…"),
            Some(Preview { text: Err(why), .. }) => div()
                .id("history-error")
                .role(Role::Alert)
                .aria_label(why.clone())
                .debug_selector(|| "history-error".into())
                .mx(px(size::LIST_ROW_INSET))
                .pl(px(list_row::PAD_LEFT))
                .text_color(theme.status.error)
                .text_size(px(text::UI_SMALL.size))
                .child(why.clone())
                .accessible()
                .live(Live::Assertive)
                .into_any_element(),
            Some(Preview { lines, .. }) => {
                let lines = lines.clone();
                let color = theme.text.primary;
                div()
                    .id("history-preview")
                    .role(Role::Document)
                    .aria_label("Version preview")
                    .aria_description(format!("{} lines", lines.len()))
                    .debug_selector(|| "history-preview".into())
                    .flex_1()
                    .min_h_0()
                    .px(px(size::LIST_ROW_INSET + list_row::PAD_LEFT))
                    .font_family(font::EDITOR)
                    .text_size(px(text::UI_SMALL.size))
                    .line_height(px(text::UI_SMALL.line))
                    .text_color(color)
                    .child(
                        uniform_list("history-preview-lines", lines.len(), move |range, _, _| {
                            range
                                .map(|i| {
                                    div()
                                        .h(px(text::UI_SMALL.line))
                                        .whitespace_nowrap()
                                        .child(lines[i].clone())
                                        .into_any_element()
                                })
                                .collect()
                        })
                        .size_full(),
                    )
                    .accessible()
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .border_t(px(crate::metrics::HAIRLINE))
            .border_color(theme.border.default)
            .child(
                buttons
                    .id("history-actions")
                    .a11y_group(Role::Toolbar, "Version")
                    .accessible(),
            )
            .child(body)
            .into_any_element()
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let head = self.render_head(theme);
        let count = self.versions().len();
        let note: Option<SharedString> = match &self.list {
            List::NoFile => Some("Save the file to keep its versions".into()),
            List::NoStore => Some("Versions are not kept".into()),
            List::Loading => Some("Reading…".into()),
            List::Failed(why) => Some(why.clone().into()),
            List::Ready(v) if v.is_empty() => {
                Some("No versions yet: one is kept each time the file is saved".into())
            }
            List::Ready(_) => None,
        };
        let preview = self
            .selected
            .is_some()
            .then(|| self.render_preview(theme, cx));
        div()
            .id("history-panel")
            .role(gpui::Role::Group)
            .aria_label("File history")
            .debug_selector(|| "history-panel".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::compare_selected))
            .on_action(cx.listener(Self::dismiss))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .map(|panel| match note {
                Some(note) => panel.child(announced(
                    message(note.clone(), theme),
                    "history-note",
                    note,
                )),
                None => panel.child(
                    div()
                        .id("history-versions-list")
                        .a11y_group(Role::List, "Versions")
                        .flex_1()
                        .min_h_0()
                        .py(px(list::PAD_Y))
                        .child(
                            uniform_list(
                                "history-versions",
                                count,
                                cx.processor(|this, range: Range<usize>, _, cx| {
                                    this.render_rows(range, cx)
                                }),
                            )
                            .size_full()
                            .track_scroll(&self.scroll),
                        )
                        .accessible(),
                ),
            })
            .children(preview)
            .accessible()
    }
}

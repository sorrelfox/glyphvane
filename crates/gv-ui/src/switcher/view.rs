// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The switcher's view: a layer of the window root that draws nothing until
//! it opens.

use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::base::RootPlugin;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, BoxShadow, Context, Div, Entity, FocusHandle, FontWeight,
    HighlightStyle, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels,
    Render, ScrollStrategy, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription,
    UniformListScrollHandle, Window, div, point, px, uniform_list,
};

use super::model::{self, Entry, EntryKind, Mode};
use super::{
    Cancel, Choice, Confirm, ConfirmInNewTab, CreateNote, KEY_CONTEXT, SelectNext, SelectPrevious,
    Sources, SwitcherRequest,
};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::actions::workspace::QuickSwitcher as ToggleSwitcher;
use crate::metrics::size::{SWITCHER_ROW, switcher as size, tab};
use crate::metrics::spacing::switcher as spacing;
use crate::metrics::{HAIRLINE, font, icon, radius, text};
use crate::theme::Theme;

/// The headings or block ids of the content index, built on the first
/// query that asks for them.
#[derive(Debug)]
struct Sections {
    generation: u64,
    blocks: bool,
    entries: Vec<Entry>,
}

/// The quick switcher of one window.
pub struct QuickSwitcher {
    input: Entity<InputState>,
    request: Option<SwitcherRequest>,
    /// What had the keyboard before the switcher took it.
    restore: Option<FocusHandle>,
    sources: Sources,
    sections: Option<Sections>,
    mode: Mode,
    /// The query without its mode character.
    query: String,
    /// Indices into the listed entries (the sources' or the sections'),
    /// best first.
    rows: Vec<usize>,
    /// The vault-relative path of the note that Enter creates when nothing
    /// matches.
    create: Option<String>,
    /// The highlighted row.
    selected: usize,
    scroll: UniformListScrollHandle,
    /// How long the last ranking took.
    last_rank: Duration,
    _input_events: Subscription,
}

impl std::fmt::Debug for QuickSwitcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickSwitcher")
            .field("open", &self.request.is_some())
            .field("entries", &self.sources.entries.len())
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl RootPlugin for QuickSwitcher {}

impl QuickSwitcher {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find or create a note"));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) && this.is_open() {
                this.refilter(cx);
            }
        });
        Self {
            input,
            request: None,
            restore: None,
            sources: Sources::default(),
            sections: None,
            mode: Mode::Files,
            query: String::new(),
            rows: Vec::new(),
            create: None,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            last_rank: Duration::ZERO,
            _input_events: events,
        }
    }

    pub fn is_open(&self) -> bool {
        self.request.is_some()
    }

    /// Opens the switcher with an empty query and takes the keyboard.
    pub fn open(
        &mut self,
        request: SwitcherRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.is_open() {
            return;
        }
        self.restore = window.focused(cx);
        self.sources = (request.sources)(cx);
        self.sections = None;
        self.request = Some(request);
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.refilter(cx);
    }

    /// Closes the switcher and gives the keyboard back.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.close_quietly(window, cx).is_some() {
            cx.notify();
        }
    }

    fn close_quietly(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<SwitcherRequest> {
        let request = self.request.take()?;
        match self.restore.take() {
            Some(focus) => focus.focus(window, cx),
            None => window.blur(cx),
        }
        self.sources = Sources::default();
        self.sections = None;
        self.rows.clear();
        self.create = None;
        cx.notify();
        Some(request)
    }

    /// Gathers the sources again (a new index snapshot), keeping the
    /// highlighted row where it is still listed.
    pub fn refresh(&mut self, cx: &mut Context<'_, Self>) {
        let Some(request) = &self.request else {
            return;
        };
        let keep = self.selected_entry().map(identity);
        self.sources = (request.sources)(cx);
        self.refilter(cx);
        if let Some(keep) = keep
            && let Some(at) = self
                .rows
                .iter()
                .position(|&i| identity(&self.listed_entries()[i]) == keep)
        {
            self.selected = at;
            self.scroll.scroll_to_item(at, ScrollStrategy::Nearest);
        }
    }

    /// Replaces the query, as typing it does.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx);
        });
        self.refilter(cx);
    }

    /// The labels of the listed rows, in order (the create row as
    /// `+ path`).
    pub fn listed(&self) -> Vec<String> {
        let entries = self.listed_entries();
        let mut out: Vec<String> = self.rows.iter().map(|&i| describe(&entries[i])).collect();
        out.extend(self.create.iter().map(|p| format!("+ {p}")));
        out
    }

    /// The highlighted row, as [`Self::listed`] writes it.
    pub fn selected(&self) -> Option<String> {
        self.listed().into_iter().nth(self.selected)
    }

    /// How many entries the switcher draws on (files, aliases, notes to
    /// create).
    pub fn entry_count(&self) -> usize {
        self.sources.entries.len()
    }

    /// How long ranking the entries for the last query took.
    pub fn last_rank_time(&self) -> Duration {
        self.last_rank
    }

    fn listed_entries(&self) -> &[Entry] {
        match (self.mode, &self.sections) {
            (Mode::Files, _) | (_, None) => &self.sources.entries,
            (Mode::Headings | Mode::Blocks, Some(s)) => &s.entries,
        }
    }

    fn selected_entry(&self) -> Option<&Entry> {
        let index = *self.rows.get(self.selected)?;
        self.listed_entries().get(index)
    }

    /// The headings or block ids, built when a query first asks for them
    /// and again when the index changes.
    fn ensure_sections(&mut self, blocks: bool) {
        let Some((root, snapshot)) = &self.sources.content else {
            self.sections = None;
            return;
        };
        let current = self
            .sections
            .as_ref()
            .is_some_and(|s| s.blocks == blocks && s.generation == snapshot.generation());
        if !current {
            self.sections = Some(Sections {
                generation: snapshot.generation(),
                blocks,
                entries: model::sections(snapshot, root, blocks),
            });
        }
    }

    fn refilter(&mut self, cx: &mut Context<'_, Self>) {
        let raw = self.input.read(cx).value().to_string();
        let (mode, query) = model::mode_of(&raw);
        self.mode = mode;
        self.query = query.to_owned();
        match mode {
            Mode::Files => {}
            Mode::Headings => self.ensure_sections(false),
            Mode::Blocks => self.ensure_sections(true),
        }
        let started = Instant::now();
        self.rows = match (mode, &self.sections) {
            (Mode::Files, _) => model::rank(&self.sources.entries, query),
            (_, Some(sections)) => model::rank(&sections.entries, query),
            (_, None) => Vec::new(),
        };
        self.last_rank = started.elapsed();
        self.create = (mode == Mode::Files && self.rows.is_empty() && self.sources.can_create)
            .then(|| model::new_note_path(query))
            .flatten();
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Moves the highlight to the next (or previous) row, wrapping around.
    fn step(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let len = self.row_count();
        if len == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % len
        } else {
            (self.selected + len - 1) % len
        };
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn row_count(&self) -> usize {
        self.rows.len() + usize::from(self.create.is_some())
    }

    /// What row `position` does.
    fn choice(&self, position: usize, new_tab: bool) -> Option<Choice> {
        if let Some(&index) = self.rows.get(position) {
            let entry = &self.listed_entries()[index];
            return match (&entry.kind, &entry.file) {
                (EntryKind::Unresolved { .. }, _) | (_, None) => self
                    .sources
                    .can_create
                    .then(|| model::new_note_path(&entry.label))
                    .flatten()
                    .map(|path| Choice::Create { path, new_tab }),
                (_, Some(file)) => Some(Choice::Open {
                    file: file.clone(),
                    line: entry.line,
                    new_tab,
                }),
            };
        }
        (position == self.rows.len())
            .then(|| self.create.clone())
            .flatten()
            .map(|path| Choice::Create { path, new_tab })
    }

    /// The choice of the row at `position`: the switcher closes, then the
    /// window hears it.
    fn confirm(
        &mut self,
        position: usize,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(choice) = self.choice(position, new_tab) else {
            return;
        };
        self.finish(choice, window, cx);
    }

    /// Shift+Enter: creates a note named after the query.
    fn create_from_query(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.mode != Mode::Files || !self.sources.can_create {
            return;
        }
        let Some(path) = model::new_note_path(&self.query) else {
            return;
        };
        self.finish(
            Choice::Create {
                path,
                new_tab: false,
            },
            window,
            cx,
        );
    }

    fn finish(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(request) = self.close_quietly(window, cx) {
            (request.on_choice)(choice, window, cx);
        }
    }
}

/// How an entry is told apart from the others across refreshes.
fn identity(entry: &Entry) -> (EntryKind, std::sync::Arc<str>, Option<std::path::PathBuf>) {
    (entry.kind, entry.label.clone(), entry.file.clone())
}

/// A row as [`QuickSwitcher::listed`] writes it.
fn describe(entry: &Entry) -> String {
    match (&entry.kind, &entry.target) {
        (EntryKind::Alias, Some(target)) => format!("{} → {target}", entry.label),
        (EntryKind::Unresolved { .. }, _) => format!("{} · not created yet", entry.label),
        (EntryKind::Heading { .. } | EntryKind::Block, Some(target)) => {
            format!("{} · {target}", entry.label)
        }
        _ => entry.label.to_string(),
    }
}

/// `shadow.popover`, the application's only shadow.
fn popover_shadow(theme: &Theme) -> Vec<BoxShadow> {
    theme
        .shadow
        .popover
        .iter()
        .map(|l| BoxShadow {
            color: l.color.into(),
            offset: point(px(l.x), px(l.y)),
            blur_radius: px(l.blur),
            spread_radius: px(l.spread),
            inset: false,
        })
        .collect()
}

/// `label` with its `matched` letters in weight 700, then `suffix` in
/// `text.secondary`.
fn label_text(label: &str, matched: &[Range<usize>], suffix: &str, theme: &Theme) -> StyledText {
    let bold = HighlightStyle {
        font_weight: Some(FontWeight(text::weight::BOLD)),
        ..HighlightStyle::default()
    };
    let mut spans: Vec<(Range<usize>, HighlightStyle)> =
        matched.iter().map(|r| (r.clone(), bold)).collect();
    if suffix.is_empty() {
        return StyledText::new(label.to_owned()).with_highlights(spans);
    }
    let text = format!("{label}{suffix}");
    spans.push((
        label.len()..text.len(),
        HighlightStyle {
            color: Some(theme.text.secondary.into()),
            ..HighlightStyle::default()
        },
    ));
    StyledText::new(text).with_highlights(spans)
}

/// A key hint: the key in mono `text.primary`, then what it does.
fn hint(key: &'static str, label: String, theme: &Theme) -> Div {
    div()
        .flex()
        .gap(px(spacing::HINT_KEY_GAP))
        .child(
            div()
                .font_family(font::EDITOR)
                .text_color(theme.text.primary)
                .child(key),
        )
        .child(label)
}

/// The modifier of Mod+Enter as the footer writes it.
const MOD_ENTER: &str = if cfg!(target_os = "macos") {
    "⌘ Enter"
} else {
    "Ctrl Enter"
};

fn entry_icon(entry: &Entry) -> Icon {
    match entry.kind {
        EntryKind::Alias => Icon::SwitcherAlias,
        EntryKind::Unresolved { .. } => Icon::NotCreated,
        EntryKind::Heading { .. } => Icon::SwitcherHeading,
        EntryKind::Block => Icon::SwitcherBlock,
        EntryKind::Note => Icon::Note,
        EntryKind::File => {
            let ext = entry
                .file
                .as_deref()
                .and_then(|f| f.extension())
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            match ext.as_str() {
                "canvas" => Icon::Canvas,
                "base" => Icon::Base,
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico" => {
                    Icon::ImageFile
                }
                _ => Icon::OtherFile,
            }
        }
    }
}

impl QuickSwitcher {
    fn row_frame(
        &self,
        position: usize,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> gpui::Stateful<Div> {
        let hover = theme.state.hover;
        div()
            .id(("quick-switcher-row", position))
            .w_full()
            .h(px(SWITCHER_ROW))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing::ROW_GAP))
            .px(px(spacing::ROW_PAD_X))
            .rounded(px(radius::SWITCHER_ROW))
            .map(|row| {
                if selected {
                    row.bg(theme.accent.tint)
                } else {
                    row.hover(move |s| s.bg(hover))
                }
            })
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    let new_tab = event.modifiers().secondary(cx);
                    this.confirm(position, new_tab, window, cx);
                }),
            )
    }

    fn render_row(&self, position: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let selected = position == self.selected;
        let Some(&index) = self.rows.get(position) else {
            // The create row.
            let Some(path) = self.create.clone() else {
                return div().into_any_element();
            };
            let label = format!("Create “{path}”");
            return self
                .row_frame(position, selected, theme, cx)
                .debug_selector(|| "quick-switcher-create".into())
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.text.secondary)
                        .child(Icon::NotCreated.sized(icon::ROW)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(text::UI_TITLE.size))
                        .child(label_text(&label, &[], " · not created yet", theme)),
                )
                .into_any_element();
        };
        let entry = &self.listed_entries()[index];
        let matched = model::highlight(&entry.label, &self.query);
        let suffix = match (&entry.kind, &entry.target) {
            (EntryKind::Alias, Some(target)) => format!(" → {target}"),
            (EntryKind::Unresolved { .. }, _) => " · not created yet".to_owned(),
            _ => String::new(),
        };
        let right: String = match (&entry.kind, &entry.target) {
            (EntryKind::Alias, _) => "alias".to_owned(),
            (EntryKind::Unresolved { links: 1 }, _) => "linked from 1 note".to_owned(),
            (EntryKind::Unresolved { links }, _) => format!("linked from {links} notes"),
            (EntryKind::Heading { .. } | EntryKind::Block, Some(target)) => target.to_string(),
            _ => entry.folder.to_string(),
        };
        let label = entry.label.to_string();
        let selector = format!("quick-switcher-row:{}", describe(entry));
        let unsaved = entry.unsaved.then(|| {
            let selector = format!("quick-switcher-unsaved:{label}");
            div()
                .debug_selector(move || selector.clone())
                .flex_none()
                .size(px(tab::UNSAVED_DOT))
                .rounded_full()
                .bg(theme.text.primary)
        });
        let indent = match entry.kind {
            EntryKind::Heading { level } => f32::from(level.saturating_sub(1)) * spacing::ROW_GAP,
            _ => 0.0,
        };
        self.row_frame(position, selected, theme, cx)
            .debug_selector(move || selector.clone())
            .child(
                div()
                    .flex_none()
                    .pl(px(indent))
                    .text_color(theme.text.secondary)
                    .child(entry_icon(entry).sized(icon::ROW)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(spacing::ROW_GAP))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(text::UI_TITLE.size))
                            .child(label_text(&label, &matched, &suffix, theme)),
                    )
                    .children(unsaved),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(size::WIDTH / 3.0))
                    .truncate()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(right),
            )
            .into_any_element()
    }

    /// How many rows the list shows before it scrolls: at most
    /// [`size::MAX_ROWS`], fewer when the window is short.
    fn visible_rows(&self, viewport_height: Pixels) -> usize {
        let footer =
            spacing::FOOTER_PAD_TOP + text::UI_SMALL.line + spacing::FOOTER_PAD_BOTTOM + HAIRLINE;
        let room = f32::from(viewport_height)
            - size::TOP
            - size::INPUT_ROW
            - 2.0 * spacing::LIST_PAD
            - footer
            - size::SIDE_MARGIN;
        let fit = (room / SWITCHER_ROW).floor().max(1.0) as usize;
        self.row_count().clamp(1, size::MAX_ROWS.min(fit))
    }

    fn empty_message(&self) -> &'static str {
        match (self.mode, &self.sources.content) {
            (Mode::Headings | Mode::Blocks, None) => "The vault is still being indexed",
            (Mode::Headings, Some(_)) => "No matching headings",
            (Mode::Blocks, Some(_)) => "No matching blocks",
            (Mode::Files, _) if self.query.trim().is_empty() => "No recent files",
            (Mode::Files, _) => "No matching files",
        }
    }
}

impl Render for QuickSwitcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if !self.is_open() {
            return div().into_any_element();
        }
        let theme = Appearance::theme(cx);
        let viewport = window.viewport_size();
        let width = px(size::WIDTH)
            .min(viewport.width - px(2.0 * size::SIDE_MARGIN))
            .max(px(0.0));

        let input_row = div()
            .h(px(size::INPUT_ROW))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing::INPUT_GAP))
            .px(px(spacing::INPUT_PAD_X))
            .border_b(px(HAIRLINE))
            .border_color(theme.border.default)
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(Icon::SwitcherSearch.sized(icon::TOOLBAR)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.input)
                        .aria_label("Quick switcher")
                        .appearance(false)
                        .h(px(text::SWITCHER_INPUT.line))
                        .px_0()
                        .py_0()
                        .text_size(px(text::SWITCHER_INPUT.size))
                        .text_color(theme.text.primary),
                ),
            )
            .child(
                div()
                    .debug_selector(|| "quick-switcher-modes".into())
                    .flex_none()
                    .flex()
                    .gap(px(spacing::FOOTER_GAP_X))
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(hint("#", "headings".to_owned(), theme))
                    .child(hint("^", "blocks".to_owned(), theme)),
            );

        let list = div().flex_none().p(px(spacing::LIST_PAD)).map(|list| {
            if self.row_count() == 0 {
                list.child(
                    div()
                        .debug_selector(|| "quick-switcher-empty".into())
                        .h(px(SWITCHER_ROW))
                        .flex()
                        .items_center()
                        .px(px(spacing::ROW_PAD_X))
                        .text_size(px(text::UI_TITLE.size))
                        .text_color(theme.text.secondary)
                        .child(self.empty_message()),
                )
            } else {
                let visible = self.visible_rows(viewport.height);
                list.child(
                    uniform_list(
                        "quick-switcher-rows",
                        self.row_count(),
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            range
                                .map(|position| this.render_row(position, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.scroll)
                    .w_full()
                    .h(px(visible as f32 * SWITCHER_ROW)),
                )
            }
        });

        let can_create =
            self.mode == Mode::Files && self.sources.can_create && !self.query.trim().is_empty();
        let footer = div()
            .debug_selector(|| "quick-switcher-footer".into())
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(spacing::FOOTER_GAP_X))
            .gap_y(px(spacing::FOOTER_GAP_Y))
            .pt(px(spacing::FOOTER_PAD_TOP))
            .pb(px(spacing::FOOTER_PAD_BOTTOM))
            .px(px(spacing::FOOTER_PAD_X))
            .border_t(px(HAIRLINE))
            .border_color(theme.border.default)
            .text_size(px(text::UI_SMALL.size))
            .line_height(px(text::UI_SMALL.line))
            .text_color(theme.text.secondary)
            .child(hint("Enter", "open".to_owned(), theme))
            .child(hint(MOD_ENTER, "new tab".to_owned(), theme))
            .when(can_create, |footer| {
                footer.child(hint(
                    "Shift Enter",
                    format!("create “{}”", self.query.trim()),
                    theme,
                ))
            })
            .child(div().flex_1())
            .child(hint("Esc", "close".to_owned(), theme));

        let panel = div()
            .id("quick-switcher")
            .debug_selector(|| "quick-switcher".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                this.confirm(this.selected, false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ConfirmInNewTab, window, cx| {
                this.confirm(this.selected, true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CreateNote, window, cx| {
                this.create_from_query(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close(window, cx)))
            // Mod+O again closes.
            .on_action(cx.listener(|this, _: &ToggleSwitcher, window, cx| {
                this.close(window, cx);
            }))
            .absolute()
            .top(px(size::TOP))
            .left((viewport.width - width) / 2.0)
            .w(width)
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(radius::SWITCHER))
            .bg(theme.surface.raised)
            .border(px(HAIRLINE))
            .border_color(theme.border.strong)
            .shadow(popover_shadow(theme))
            .text_color(theme.text.primary)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            // A click inside is not a click outside.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(input_row)
            .child(list)
            .child(footer);

        div()
            .id("quick-switcher-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .font_family(Appearance::interface_font(cx))
            // A click outside closes the switcher and does nothing else.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.close(window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.close(window, cx);
                }),
            )
            .child(panel)
            .into_any_element()
    }
}

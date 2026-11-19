// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Go popover's view: a layer of the window root that draws nothing
//! until a view opens it.

use std::ops::Range;

use gpui::base::RootPlugin;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, BoxShadow, Context, Div, Entity, FocusHandle, FontWeight,
    HighlightStyle, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels,
    Render, ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText,
    Subscription, UniformListScrollHandle, Window, div, point, px, uniform_list,
};
use gv_editor::commands::parse_line_target;

use super::{
    Cancel, Choice, Confirm, GotoEvent, GotoKind, GotoRequest, KEY_CONTEXT, Row, SelectNext,
    SelectPrevious, filter,
};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::{SWITCHER_ROW, switcher as size};
use crate::metrics::spacing::switcher as spacing;
use crate::metrics::{HAIRLINE, font, icon, radius, text};
use crate::theme::Theme;

/// The indentation of one heading level in a row (not designed).
const DEPTH_STEP: f32 = 12.0;

/// The Go popover of one window.
pub struct GotoPopover {
    input: Entity<InputState>,
    request: Option<GotoRequest>,
    /// The field's text, as last read.
    query: String,
    /// What had the keyboard before the popover took it.
    restore: Option<FocusHandle>,
    rows: Vec<Row>,
    /// The highlighted row.
    selected: usize,
    /// The last choice previewed, so that the same one is not sent twice.
    previewed: Option<Choice>,
    scroll: UniformListScrollHandle,
    _input_events: Subscription,
}

impl std::fmt::Debug for GotoPopover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GotoPopover")
            .field("request", &self.request)
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl RootPlugin for GotoPopover {}

impl GotoPopover {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Change) {
                this.refilter(window, cx);
            }
        });
        Self {
            input,
            request: None,
            query: String::new(),
            restore: None,
            rows: Vec::new(),
            selected: 0,
            previewed: None,
            scroll: UniformListScrollHandle::new(),
            _input_events: events,
        }
    }

    pub fn is_open(&self) -> bool {
        self.request.is_some()
    }

    /// The accessible name of the open popover.
    pub fn title(&self) -> Option<&str> {
        self.request.as_ref().map(|r| r.title.as_ref())
    }

    /// The labels of the rows listed, in order.
    pub fn listed(&self) -> Vec<String> {
        let Some(GotoKind::List { items, .. }) = self.request.as_ref().map(|r| &r.kind) else {
            return Vec::new();
        };
        self.rows
            .iter()
            .map(|row| items[row.index].label.to_string())
            .collect()
    }

    /// The highlighted row's label.
    pub fn selected(&self) -> Option<String> {
        let Some(GotoKind::List { items, .. }) = self.request.as_ref().map(|r| &r.kind) else {
            return None;
        };
        let row = self.rows.get(self.selected)?;
        Some(items[row.index].label.to_string())
    }

    /// Opens the popover for `request` and takes the keyboard. One that is
    /// open is cancelled first.
    pub fn open(&mut self, request: GotoRequest, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.request.is_some() {
            self.cancel(window, cx);
        }
        self.restore = window.focused(cx);
        let placeholder: SharedString = match &request.kind {
            GotoKind::Line { count, .. } => {
                format!("Line number (1 to {count}), or line:column").into()
            }
            GotoKind::Page { count, .. } => format!("Page number (1 to {count})").into(),
            GotoKind::List { placeholder, .. } => placeholder.clone(),
        };
        self.request = Some(request);
        self.previewed = None;
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        // The view that opens the popover is busy: it is not told now.
        self.refilter_rows(cx);
        if let Some(GotoKind::List { initial, .. }) = self.request.as_ref().map(|r| &r.kind) {
            self.selected = (*initial).min(self.rows.len().saturating_sub(1));
            self.scroll
                .scroll_to_item(self.selected, ScrollStrategy::Center);
        }
        // Opening does not move the text: the first preview comes with the
        // first key.
        self.previewed = self.choice();
        cx.notify();
    }

    /// Replaces the query, as typing it does.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx);
        });
        self.refilter(window, cx);
    }

    /// What Enter would go to now.
    fn choice(&self) -> Option<Choice> {
        match &self.request.as_ref()?.kind {
            GotoKind::Line { .. } => {
                let (line, column) = parse_line_target(&self.query)?;
                Some(Choice::Line(line, column))
            }
            GotoKind::Page { count, .. } => {
                let page = self.query.trim().parse::<usize>().ok()?;
                Some(Choice::Line(page.clamp(1, (*count).max(1)), None))
            }
            GotoKind::List { .. } => self
                .rows
                .get(self.selected)
                .map(|row| Choice::Item(row.index)),
        }
    }

    fn refilter(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.refilter_rows(cx);
        self.preview(window, cx);
    }

    /// Reads the query and lists the rows for it, without telling the view.
    fn refilter_rows(&mut self, cx: &mut Context<'_, Self>) {
        let query = self.input.read(cx).value().to_string();
        self.query = query.clone();
        if let Some(GotoKind::List { items, .. }) = self.request.as_ref().map(|r| &r.kind) {
            self.rows = filter(items, &query);
            self.selected = 0;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        } else {
            self.rows.clear();
        }
        cx.notify();
    }

    /// Tells the view about the current choice, if it changed.
    fn preview(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let choice = self.choice();
        if choice == self.previewed {
            return;
        }
        self.previewed = choice;
        if let (Some(choice), Some(request)) = (choice, self.request.as_ref()) {
            let callback = request.on_event.clone();
            callback(GotoEvent::Preview(choice), window, cx);
        }
    }

    /// Moves the highlight to the next (or previous) row, wrapping around.
    fn step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        let len = self.rows.len();
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
        self.preview(window, cx);
        cx.notify();
    }

    /// Closes the popover and gives the keyboard back.
    fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Option<GotoRequest> {
        let request = self.request.take()?;
        match self.restore.take() {
            Some(focus) => focus.focus(window, cx),
            None => window.blur(cx),
        }
        self.rows.clear();
        cx.notify();
        Some(request)
    }

    /// Esc: closes, and the view goes back.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(request) = self.close(window, cx) {
            (request.on_event)(GotoEvent::Cancel, window, cx);
        }
    }

    /// Enter: goes to the choice and closes; nothing without one.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(choice) = self.choice() else {
            return;
        };
        if let Some(request) = self.close(window, cx) {
            (request.on_event)(GotoEvent::Confirm(choice), window, cx);
        }
    }

    /// A click on row `position`.
    fn click(&mut self, position: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.selected = position;
        self.confirm(window, cx);
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

/// A key hint of the footer: the key in mono `text.primary`, then what it
/// does.
fn hint(key: &'static str, label: SharedString, theme: &Theme) -> Div {
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

impl GotoPopover {
    fn render_row(&self, position: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let (Some(row), Some(GotoKind::List { items, .. })) = (
            self.rows.get(position),
            self.request.as_ref().map(|r| &r.kind),
        ) else {
            return div().into_any_element();
        };
        let item = &items[row.index];
        let selected = position == self.selected;
        let hover = theme.state.hover;
        let bold = HighlightStyle {
            font_weight: Some(FontWeight(text::weight::BOLD)),
            ..HighlightStyle::default()
        };
        let label = StyledText::new(item.label.clone())
            .with_highlights(row.matched.iter().map(|r: &Range<usize>| (r.clone(), bold)));
        let selector = format!("goto-row:{position}");
        div()
            .id(("goto-row", position))
            .a11y_option(item.label.clone(), selected, position, self.rows.len())
            .debug_selector(move || selector.clone())
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
            .on_click(cx.listener(move |this, _, window, cx| this.click(position, window, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .pl(px(f32::from(item.depth) * DEPTH_STEP))
                    .text_size(px(text::UI_TITLE.size))
                    .child(label),
            )
            .children(item.detail.clone().map(|detail| {
                div()
                    .flex_none()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(detail)
            }))
            .into_any_element()
    }

    /// How many rows the list shows before it scrolls.
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
        self.rows.len().clamp(1, size::MAX_ROWS.min(fit))
    }

    fn render_body(
        &self,
        viewport_height: Pixels,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        let Some(request) = self.request.as_ref() else {
            return div();
        };
        let note = |text: SharedString, selector: &'static str| {
            div()
                .debug_selector(move || selector.into())
                .h(px(SWITCHER_ROW))
                .flex()
                .items_center()
                .px(px(spacing::ROW_PAD_X))
                .text_size(px(text::UI_TITLE.size))
                .text_color(theme.text.secondary)
                .child(text)
        };
        let body = div().flex_none().p(px(spacing::LIST_PAD));
        match &request.kind {
            GotoKind::Line { current, count } => {
                let text: SharedString = match parse_line_target(&self.query) {
                    Some((line, Some(column))) => format!(
                        "Go to line {}, column {column}",
                        line.clamp(1, (*count).max(1))
                    )
                    .into(),
                    Some((line, None)) => {
                        format!("Go to line {}", line.clamp(1, (*count).max(1))).into()
                    }
                    None if self.query.trim().is_empty() => {
                        format!("Current line {current} of {count}").into()
                    }
                    None => "Type a line number".into(),
                };
                body.child(note(text, "goto-line-note"))
            }
            GotoKind::Page { current, count } => {
                let text: SharedString = match self.query.trim().parse::<usize>() {
                    Ok(page) => format!("Go to page {}", page.clamp(1, (*count).max(1))).into(),
                    Err(_) if self.query.trim().is_empty() => {
                        format!("Current page {current} of {count}").into()
                    }
                    Err(_) => "Type a page number".into(),
                };
                body.child(note(text, "goto-line-note"))
            }
            GotoKind::List { empty, .. } if self.rows.is_empty() => {
                body.child(note(empty.clone(), "goto-empty"))
            }
            GotoKind::List { .. } => {
                let visible = self.visible_rows(viewport_height);
                body.child(
                    uniform_list(
                        "goto-rows",
                        self.rows.len(),
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
        }
    }
}

impl Render for GotoPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(request) = self.request.as_ref() else {
            return div().into_any_element();
        };
        let theme = Appearance::theme(cx);
        let viewport = window.viewport_size();
        let width = px(size::WIDTH)
            .min(viewport.width - px(2.0 * size::SIDE_MARGIN))
            .max(px(0.0));
        let title = request.title.clone();
        let (action, is_list) = match &request.kind {
            GotoKind::Line { .. } | GotoKind::Page { .. } => (SharedString::from("go"), false),
            GotoKind::List { action, .. } => (action.clone(), true),
        };
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
                        .aria_label(title)
                        .appearance(false)
                        .h(px(text::SWITCHER_INPUT.line))
                        .px_0()
                        .py_0()
                        .text_size(px(text::SWITCHER_INPUT.size))
                        .text_color(theme.text.primary),
                ),
            );
        let body = self.render_body(viewport.height, theme, cx);
        let footer = div()
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
            .when(is_list, |footer| {
                footer.child(hint("↑ ↓", "select".into(), theme))
            })
            .child(hint("Enter", action, theme))
            .child(div().flex_1())
            .child(hint("Esc", "back".into(), theme));
        let announcement = match self.request.as_ref().map(|r| &r.kind) {
            Some(GotoKind::List { items, empty, .. }) => crate::a11y::option_announcement(
                self.rows
                    .get(self.selected)
                    .and_then(|r| items.get(r.index))
                    .map(|i| i.label.as_ref()),
                self.selected,
                self.rows.len(),
                empty.as_ref(),
            ),
            _ => String::new(),
        };
        let title = self
            .request
            .as_ref()
            .map(|r| r.title.clone())
            .unwrap_or_default();
        let panel = div()
            .id("goto-popover")
            .a11y_group(Role::Dialog, title)
            .debug_selector(|| "goto-popover".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| this.step(true, window, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.step(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
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
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(input_row)
            .child(body)
            .child(crate::a11y::announcer("goto-status", announcement))
            .child(footer)
            .accessible()
            .modal();
        div()
            .id("goto-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .font_family(Appearance::interface_font(cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.cancel(window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.cancel(window, cx);
                }),
            )
            .child(panel)
            .into_any_element()
    }
}

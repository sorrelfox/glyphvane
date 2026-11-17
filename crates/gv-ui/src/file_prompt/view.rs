// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The file prompt's view: a layer of the window root that draws nothing
//! until a view opens it.

use std::ops::Range;

use gpui::base::RootPlugin;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, BoxShadow, Context, Div, Entity, FocusHandle, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Render,
    SharedString, Styled as _, Subscription, UniformListScrollHandle, Window, div, point, px,
    uniform_list,
};

use super::{Cancel, Confirm, KEY_CONTEXT, PromptRequest, RowStyle};
use crate::appearance::Appearance;
use crate::metrics::size::{SWITCHER_ROW, switcher as size};
use crate::metrics::spacing::switcher as spacing;
use crate::metrics::{HAIRLINE, font, radius, text};
use crate::theme::Theme;

/// The title block's padding (not designed: the input row's sides, and
/// the footer's top and bottom).
const TITLE_PAD_Y: f32 = 10.0;

/// The file prompt of one window.
pub struct FilePrompt {
    focus: FocusHandle,
    input: Entity<InputState>,
    request: Option<PromptRequest>,
    /// The field's text, as last read.
    text: String,
    /// What the validator says about `text`.
    problem: Option<SharedString>,
    /// What had the keyboard before the prompt took it.
    restore: Option<FocusHandle>,
    scroll: UniformListScrollHandle,
    _input_events: Subscription,
}

impl std::fmt::Debug for FilePrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePrompt")
            .field("request", &self.request)
            .field("text", &self.text)
            .field("problem", &self.problem)
            .finish_non_exhaustive()
    }
}

impl RootPlugin for FilePrompt {}

impl FilePrompt {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.read_text(cx);
            }
        });
        Self {
            focus: cx.focus_handle(),
            input,
            request: None,
            text: String::new(),
            problem: None,
            restore: None,
            scroll: UniformListScrollHandle::new(),
            _input_events: events,
        }
    }

    pub fn is_open(&self) -> bool {
        self.request.is_some()
    }

    /// The open prompt's title.
    pub fn title(&self) -> Option<&str> {
        self.request.as_ref().map(|r| r.title.as_ref())
    }

    /// The open prompt's message.
    pub fn message(&self) -> Option<&str> {
        self.request.as_ref()?.message.as_deref()
    }

    /// The list rows' texts, in order.
    pub fn rows(&self) -> Vec<(String, RowStyle)> {
        self.request.as_ref().map_or_else(Vec::new, |r| {
            r.rows
                .iter()
                .map(|row| (row.text.to_string(), row.style))
                .collect()
        })
    }

    /// The field's text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// What is wrong with the field's text, if anything.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// The field's selected byte range.
    pub fn selection(&self, cx: &gpui::App) -> Range<usize> {
        self.input.read(cx).selected_range()
    }

    /// Opens the prompt for `request` and takes the keyboard. One that is
    /// open is cancelled first.
    pub fn open(
        &mut self,
        request: PromptRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.request.is_some() {
            self.cancel(window, cx);
        }
        self.restore = window.focused(cx);
        let field = request.field.clone();
        self.request = Some(request);
        match field {
            Some(field) => {
                self.input.update(cx, |input, cx| {
                    input.set_value(field.initial.clone(), window, cx);
                    input.focus(window, cx);
                    input.set_selected_range(field.select.clone(), cx);
                });
            }
            None => {
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.focus.focus(window, cx);
            }
        }
        self.read_text(cx);
        cx.notify();
    }

    /// Replaces the field's text, as typing it does.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(text.to_owned(), window, cx);
        });
        self.read_text(cx);
    }

    fn read_text(&mut self, cx: &mut Context<'_, Self>) {
        self.text = self.input.read(cx).value().to_string();
        self.problem = self
            .request
            .as_ref()
            .filter(|r| r.field.is_some())
            .and_then(|r| r.validate.as_ref())
            .and_then(|validate| validate(&self.text));
        cx.notify();
    }

    /// Closes the prompt and gives the keyboard back.
    fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Option<PromptRequest> {
        let request = self.request.take()?;
        match self.restore.take() {
            Some(focus) => focus.focus(window, cx),
            None => window.blur(cx),
        }
        self.problem = None;
        cx.notify();
        Some(request)
    }

    /// Esc: closes without doing anything.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.close(window, cx);
    }

    /// Enter: closes and does what the prompt asks; nothing while the
    /// name cannot be used.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.problem.is_some() {
            return;
        }
        let text = self.text.clone();
        if let Some(request) = self.close(window, cx) {
            (request.on_confirm)(text, window, cx);
        }
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

impl FilePrompt {
    fn render_row(&self, position: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let Some(row) = self.request.as_ref().and_then(|r| r.rows.get(position)) else {
            return div().into_any_element();
        };
        let selector = format!("prompt-row:{position}");
        let (sign, fill, color, weight) = match row.style {
            RowStyle::Heading => (
                None,
                None,
                theme.text.primary,
                FontWeight(text::weight::STRONG),
            ),
            RowStyle::Removed => (
                Some("\u{2212}"),
                Some(theme.status.error_tint),
                theme.text.primary,
                FontWeight(text::weight::REGULAR),
            ),
            RowStyle::Added => (
                Some("+"),
                Some(theme.status.success_tint),
                theme.text.primary,
                FontWeight(text::weight::REGULAR),
            ),
            RowStyle::Note => (
                None,
                None,
                theme.text.secondary,
                FontWeight(text::weight::REGULAR),
            ),
            RowStyle::Warning => (
                None,
                None,
                theme.status.warning,
                FontWeight(text::weight::REGULAR),
            ),
        };
        let code = matches!(row.style, RowStyle::Removed | RowStyle::Added);
        div()
            .debug_selector(move || selector.clone())
            .w_full()
            .h(px(SWITCHER_ROW))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing::ROW_GAP))
            .px(px(spacing::ROW_PAD_X))
            .rounded(px(radius::SWITCHER_ROW))
            .when_some(fill, |row, fill| row.bg(fill))
            .text_color(color)
            .font_weight(weight)
            .children(sign.map(|sign| {
                div()
                    .flex_none()
                    .font_family(font::EDITOR)
                    .text_color(theme.text.secondary)
                    .child(sign)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(code, |d| d.font_family(font::EDITOR))
                    .child(row.text.clone()),
            )
            .into_any_element()
    }

    /// How many rows the list shows before it scrolls.
    fn visible_rows(&self, rows: usize, viewport_height: Pixels) -> usize {
        let footer =
            spacing::FOOTER_PAD_TOP + text::UI_SMALL.line + spacing::FOOTER_PAD_BOTTOM + HAIRLINE;
        let room = f32::from(viewport_height)
            - size::TOP
            - 2.0 * TITLE_PAD_Y
            - 2.0 * text::UI.line
            - size::INPUT_ROW
            - 2.0 * spacing::LIST_PAD
            - footer
            - size::SIDE_MARGIN;
        let fit = (room / SWITCHER_ROW).floor().max(1.0) as usize;
        rows.clamp(1, size::MAX_ROWS.min(fit))
    }
}

impl Render for FilePrompt {
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
        let header = div()
            .flex_none()
            .px(px(spacing::INPUT_PAD_X))
            .py(px(TITLE_PAD_Y))
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(|| "prompt-title".into())
                    .font_weight(FontWeight(text::weight::STRONG))
                    .text_size(px(text::UI_TITLE.size))
                    .child(title.clone()),
            )
            .children(request.message.clone().map(|message| {
                div()
                    .debug_selector(|| "prompt-message".into())
                    .text_color(theme.text.secondary)
                    .child(message)
            }));
        let field = request.field.is_some().then(|| {
            div()
                .flex_none()
                .border_t(px(HAIRLINE))
                .border_color(theme.border.default)
                .child(
                    div()
                        .h(px(size::INPUT_ROW))
                        .flex()
                        .items_center()
                        .px(px(spacing::INPUT_PAD_X))
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(&self.input)
                                    .aria_label(title.clone())
                                    .appearance(false)
                                    .h(px(text::SWITCHER_INPUT.line))
                                    .px_0()
                                    .py_0()
                                    .text_size(px(text::SWITCHER_INPUT.size))
                                    .text_color(theme.text.primary),
                            ),
                        ),
                )
                .children(self.problem.clone().map(|problem| {
                    div()
                        .debug_selector(|| "prompt-problem".into())
                        .px(px(spacing::INPUT_PAD_X))
                        .pb(px(spacing::LIST_PAD))
                        .text_color(theme.status.error)
                        .child(problem)
                }))
        });
        let rows = request.rows.len();
        let list = (rows > 0).then(|| {
            let visible = self.visible_rows(rows, viewport.height);
            div()
                .flex_none()
                .p(px(spacing::LIST_PAD))
                .border_t(px(HAIRLINE))
                .border_color(theme.border.default)
                .child(
                    uniform_list(
                        "prompt-rows",
                        rows,
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
        });
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
            .child(hint("Enter", request.action.clone(), theme))
            .child(div().flex_1())
            .child(hint("Esc", "cancel".into(), theme));
        let panel = div()
            .id("file-prompt")
            .debug_selector(|| "file-prompt".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
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
            .child(header)
            .children(field)
            .children(list)
            .child(footer);
        div()
            .id("file-prompt-overlay")
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

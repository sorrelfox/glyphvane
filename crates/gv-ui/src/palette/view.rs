// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The palette's view: a layer of the window root that draws nothing until
//! the palette opens.

use std::ops::Range;
use std::sync::OnceLock;

use gpui::base::RootPlugin;
use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, BoxShadow, Context, Div, Entity, FocusHandle, FontWeight,
    HighlightStyle, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels,
    Render, ScrollStrategy, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription,
    UniformListScrollHandle, Window, div, point, px, uniform_list,
};

use super::model::{self, Candidate, Highlight, Row};
use super::{Cancel, Confirm, KEY_CONTEXT, SelectNext, SelectPrevious};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::{APP_COMMANDS, Keybindings, action_for};
use crate::metrics::size::{SWITCHER_ROW, switcher as size};
use crate::metrics::spacing::switcher as spacing;
use crate::metrics::{HAIRLINE, font, icon, radius, text};
use crate::theme::Theme;

/// The palette's own command, which it does not list.
const PALETTE_COMMAND: &str = "workspace.command_palette";

/// Set to a query (or to nothing) to open the palette with that query once
/// the window is drawn, for screenshots and manual checks.
pub const DEBUG_PALETTE_ENV: &str = "GLYPHVANE_DEBUG_PALETTE";

fn debug_query() -> Option<&'static str> {
    static QUERY: OnceLock<Option<String>> = OnceLock::new();
    QUERY
        .get_or_init(|| std::env::var(DEBUG_PALETTE_ENV).ok())
        .as_deref()
}

/// Frames to wait before the debug palette opens, so that the document has
/// been drawn and has the keyboard.
const DEBUG_FRAMES: u8 = 3;

/// The command palette of one window.
pub struct CommandPalette {
    input: Entity<InputState>,
    open: bool,
    /// What had the keyboard before the palette took it. Commands run there.
    restore: Option<FocusHandle>,
    /// The commands available where the keyboard was, in registry order.
    candidates: Vec<Candidate>,
    rows: Vec<Row>,
    /// The highlighted row.
    selected: usize,
    scroll: UniformListScrollHandle,
    debug_frames: u8,
    _input_events: Subscription,
}

impl std::fmt::Debug for CommandPalette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandPalette")
            .field("open", &self.open)
            .field("candidates", &self.candidates.len())
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl RootPlugin for CommandPalette {}

impl CommandPalette {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search commands"));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.refilter(cx);
            }
        });
        Self {
            input,
            open: false,
            restore: None,
            candidates: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            debug_frames: 0,
            _input_events: events,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The ids of the listed commands, in order.
    pub fn listed(&self) -> Vec<&str> {
        self.rows
            .iter()
            .map(|r| self.candidates[r.index].id.as_str())
            .collect()
    }

    /// The highlighted command.
    pub fn selected(&self) -> Option<&str> {
        let row = self.rows.get(self.selected)?;
        Some(self.candidates[row.index].id.as_str())
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.open {
            self.close(window, cx);
        } else {
            self.open(window, cx);
        }
    }

    /// Opens the palette with an empty query and takes the keyboard. What
    /// it lists is decided now, where the keyboard was.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.open {
            return;
        }
        self.restore = window.focused(cx);
        self.candidates = self.available(window, cx);
        self.open = true;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.refilter(cx);
    }

    /// Closes the palette and gives the keyboard back.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        match self.restore.take() {
            Some(focus) => focus.focus(window, cx),
            None => window.blur(cx),
        }
        cx.notify();
    }

    /// Replaces the query, as typing it does.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        self.refilter(cx);
    }

    /// The commands that can run where the keyboard is: those with a
    /// handler on its focus path, and the application-wide ones.
    fn available(&self, window: &Window, cx: &gpui::App) -> Vec<Candidate> {
        let Some(keys) = cx.try_global::<Keybindings>() else {
            return Vec::new();
        };
        keys.registry()
            .iter()
            .filter_map(|command| {
                let id = command.id().as_str();
                if id == PALETTE_COMMAND {
                    return None;
                }
                let action = action_for(id)?;
                let available = APP_COMMANDS.contains(&id)
                    || match &self.restore {
                        Some(focus) => window.is_action_available_in(action.as_ref(), focus),
                        None => window.is_action_available(action.as_ref(), cx),
                    };
                available.then(|| Candidate {
                    id: id.to_owned(),
                    title: command.title().to_owned(),
                    category: command.category(),
                    shortcut: keys.shortcut(id),
                })
            })
            .collect()
    }

    fn refilter(&mut self, cx: &mut Context<'_, Self>) {
        let query = self.input.read(cx).value().to_string();
        self.rows = model::rows(&self.candidates, &query, &super::recent(cx));
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Moves the highlight to the next (or previous) row, wrapping around.
    fn step(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
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
        cx.notify();
    }

    /// Closes the palette and runs the command of row `position` where the
    /// keyboard was, as if its keys had been pressed there.
    fn run(&mut self, position: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(id) = self
            .rows
            .get(position)
            .map(|row| self.candidates[row.index].id.clone())
        else {
            return;
        };
        let Some(action) = action_for(&id) else {
            return;
        };
        self.close(window, cx);
        super::remember(&id, cx);
        window.dispatch_action(action, cx);
    }

    /// Opens the palette with [`DEBUG_PALETTE_ENV`]'s query once, a few
    /// frames after the window first draws.
    fn open_for_debug(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(query) = debug_query() else {
            return;
        };
        if self.debug_frames > DEBUG_FRAMES {
            return;
        }
        self.debug_frames += 1;
        if self.debug_frames <= DEBUG_FRAMES {
            window.request_animation_frame();
            return;
        }
        self.open(window, cx);
        self.set_query(query, window, cx);
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

/// A command's title with its matched letters in weight 700. When only the
/// id matched, the id follows the title in `text.secondary`, its matched
/// letters in weight 700 (not designed).
fn title_text(candidate: &Candidate, highlight: &Highlight, theme: &Theme) -> StyledText {
    let bold = HighlightStyle {
        font_weight: Some(FontWeight(text::weight::BOLD)),
        ..HighlightStyle::default()
    };
    match highlight {
        Highlight::None => StyledText::new(candidate.title.clone()),
        Highlight::Title(ranges) => StyledText::new(candidate.title.clone())
            .with_highlights(ranges.iter().map(|r| (r.clone(), bold))),
        Highlight::Id(ranges) => {
            let prefix = format!("{} · ", candidate.title);
            let start = candidate.title.len();
            let offset = prefix.len();
            let text = format!("{prefix}{}", candidate.id);
            let secondary = HighlightStyle {
                color: Some(theme.text.secondary.into()),
                ..HighlightStyle::default()
            };
            let matched = HighlightStyle {
                color: Some(theme.text.secondary.into()),
                ..bold
            };
            let mut spans: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
            let mut at = start;
            for r in ranges {
                let r = r.start + offset..r.end + offset;
                if at < r.start {
                    spans.push((at..r.start, secondary));
                }
                spans.push((r.clone(), matched));
                at = r.end;
            }
            if at < text.len() {
                spans.push((at..text.len(), secondary));
            }
            StyledText::new(text).with_highlights(spans)
        }
    }
}

/// A key hint of the footer: the key in mono `text.primary`, then what it
/// does.
fn hint(key: &'static str, label: &'static str, theme: &Theme) -> Div {
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

impl CommandPalette {
    fn render_row(&self, position: usize, cx: &mut Context<'_, Self>) -> AnyElement {
        let theme = Appearance::theme(cx);
        let Some(row) = self.rows.get(position) else {
            return div().into_any_element();
        };
        let candidate = &self.candidates[row.index];
        let selected = position == self.selected;
        let hover = theme.state.hover;
        let id = candidate.id.clone();
        let shortcut = candidate.shortcut.clone().map(|shortcut| {
            let selector = format!("command-palette-shortcut:{id}:{shortcut}");
            div()
                .debug_selector(move || selector.clone())
                .flex_none()
                .font_family(font::EDITOR)
                .text_size(px(text::menu_shortcut_size()))
                .text_color(theme.text.secondary)
                .child(shortcut)
        });
        let selector = format!("command-palette-row:{id}");
        let selected_selector = format!("command-palette-selected:{id}");
        div()
            .id(("command-palette-row", position))
            .a11y_option(candidate.title.clone(), selected, position, self.rows.len())
            .when_some(candidate.shortcut.clone(), |row, keys| {
                row.aria_keyshortcuts(keys)
            })
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
            .on_click(cx.listener(move |this, _, window, cx| this.run(position, window, cx)))
            .child(
                div()
                    .when(selected, |title| {
                        title.debug_selector(move || selected_selector.clone())
                    })
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(text::UI_TITLE.size))
                    .child(title_text(candidate, &row.highlight, theme)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(candidate.category.title()),
            )
            .children(shortcut)
            .accessible()
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
        self.rows.len().clamp(1, size::MAX_ROWS.min(fit))
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        self.open_for_debug(window, cx);
        if !self.open {
            return div().into_any_element();
        }
        let theme = Appearance::theme(cx);
        let viewport = window.viewport_size();
        let width = px(size::WIDTH)
            .min(viewport.width - px(2.0 * size::SIDE_MARGIN))
            .max(px(0.0));

        let count = match self.rows.len() {
            1 => "1 command".to_owned(),
            n => format!("{n} commands"),
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
                        .aria_label("Command palette")
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
                    .debug_selector(|| "command-palette-count".into())
                    .flex_none()
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(count),
            );

        let announcement = crate::a11y::option_announcement(
            self.rows
                .get(self.selected)
                .map(|r| self.candidates[r.index].title.as_str()),
            self.selected,
            self.rows.len(),
            "No matching commands",
        );
        let list = div()
            .id("command-palette-list")
            .a11y_group(Role::ListBox, "Commands")
            .flex_none()
            .p(px(spacing::LIST_PAD))
            .map(|list| {
                if self.rows.is_empty() {
                    list.child(
                        div()
                            .debug_selector(|| "command-palette-empty".into())
                            .h(px(SWITCHER_ROW))
                            .flex()
                            .items_center()
                            .px(px(spacing::ROW_PAD_X))
                            .text_size(px(text::UI_TITLE.size))
                            .text_color(theme.text.secondary)
                            .child("No matching commands"),
                    )
                } else {
                    let visible = self.visible_rows(viewport.height);
                    list.child(
                        uniform_list(
                            "command-palette-rows",
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
            .child(hint("↑ ↓", "select", theme))
            .child(hint("Enter", "run", theme))
            .child(div().flex_1())
            .child(hint("Esc", "close", theme));

        let panel = div()
            .id("command-palette")
            .a11y_group(Role::Dialog, "Command palette")
            .debug_selector(|| "command-palette".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                this.run(this.selected, window, cx);
            }))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close(window, cx)))
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
            // A click inside the palette is not a click outside it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(input_row)
            .child(list.accessible())
            .child(crate::a11y::announcer("command-palette-status", announcement))
            .child(footer)
            .accessible()
            .modal();

        div()
            .id("command-palette-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .font_family(Appearance::interface_font(cx))
            // A click outside closes the palette and does nothing else.
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

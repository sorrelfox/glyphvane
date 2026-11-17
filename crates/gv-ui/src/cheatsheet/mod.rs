// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The keyboard shortcuts cheatsheet (`app.keymap_cheatsheet`, Help ▸
//! Keyboard shortcuts; FR-SET-02, Should).
//!
//! Not designed. A window of its own (one per application, opening it
//! again brings it forward), built from the settings window's frame: a
//! head with the title, a filter field, *Copy as Markdown* (the printable
//! form: a Markdown table per group, for printing from any viewer) and
//! *Edit shortcuts…* (the settings window's Keymap section); below, every
//! command with keys on this platform, by group ([`Category`] order) in
//! columns. It follows keymap changes as they happen. Esc clears the
//! filter, then closes the window.

#[cfg(test)]
mod tests;

use gpui::component::input::{Input, InputEvent, InputState};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, ClipboardItem, Context, Decorations,
    Entity, FocusHandle, Focusable, FontWeight, Global, InteractiveElement as _, IntoElement,
    KeyBinding, MouseButton, ParentElement as _, Render, StatefulInteractiveElement as _,
    Styled as _, Subscription, TitlebarOptions, WeakEntity, Window, WindowBounds,
    WindowControlArea, WindowOptions, div, px, size,
};
use gv_settings::keymap::{Category, CheatsheetEntry};

use crate::appearance::Appearance;
use crate::chrome::WindowChrome;
use crate::keymap::{Keybindings, actions};
use crate::metrics::spacing::linux_controls;
use crate::metrics::{HAIRLINE, radius, size as sz, text};
use crate::settings_window::controls;
use crate::settings_window::keymap_editor::key_caps;
use crate::settings_window::model::SectionId;
use crate::theme::Theme;

gpui::actions!(
    cheatsheet,
    [
        /// Clear the filter, else close the window.
        Dismiss,
    ]
);

/// The cheatsheet's key context.
pub const KEY_CONTEXT: &str = "Cheatsheet";

const TITLE: &str = "Keyboard shortcuts — Glyphvane";

/// Sizes (not designed).
mod metrics {
    pub const WIDTH: f32 = 860.0;
    pub const HEIGHT: f32 = 640.0;
    pub const MIN_WIDTH: f32 = 420.0;
    pub const MIN_HEIGHT: f32 = 320.0;
    pub const HEAD: f32 = 44.0;
    pub const PAD_X: f32 = 20.0;
    pub const PAD_Y: f32 = 12.0;
    pub const GAP: f32 = 12.0;
    pub const FIELD_WIDTH: f32 = 220.0;
    /// A group's column: at least this wide, 24 between columns.
    pub const COLUMN: f32 = 250.0;
    pub const COLUMN_GAP: f32 = 24.0;
    pub const ROW: f32 = 24.0;
    pub const ROW_GAP: f32 = 8.0;
}

/// Installs the `app.keymap_cheatsheet` handler and the window's keys.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some("Cheatsheet > Input")),
    ]);
    cx.on_action(|_: &actions::app::KeymapCheatsheet, cx| {
        // Deferred: the command may come from a window being updated.
        cx.defer(|cx| {
            if let Err(err) = open(cx) {
                tracing::warn!("cheatsheet: cannot open the window: {err}");
            }
        });
    });
}

struct OpenWindow {
    window: AnyWindowHandle,
    view: WeakEntity<Cheatsheet>,
}

impl Global for OpenWindow {}

/// The cheatsheet window and its view, if it is open.
pub fn cheatsheet_window(cx: &App) -> Option<(AnyWindowHandle, Entity<Cheatsheet>)> {
    let open = cx.try_global::<OpenWindow>()?;
    let view = open.view.upgrade()?;
    cx.windows()
        .contains(&open.window)
        .then_some((open.window, view))
}

/// Opens the cheatsheet window, or brings it forward.
pub fn open(cx: &mut App) -> gpui::Result<AnyWindowHandle> {
    if let Some((window, _)) = cheatsheet_window(cx) {
        cx.update_window(window, |_, window, _| window.activate_window())?;
        return Ok(window);
    }
    let (window, view) = gpui::open_window(window_options(cx), cx, |window, cx| {
        cx.new(|cx| Cheatsheet::new(window, cx))
    })?;
    cx.set_global(OpenWindow {
        window,
        view: view.downgrade(),
    });
    cx.activate(true);
    Ok(window)
}

fn window_options(cx: &App) -> WindowOptions {
    let chrome = WindowChrome::current();
    let bounds = Bounds::centered(None, size(px(metrics::WIDTH), px(metrics::HEIGHT)), cx);
    let titlebar = match chrome {
        WindowChrome::Windows => TitlebarOptions {
            title: Some(TITLE.into()),
            appears_transparent: false,
            traffic_light_position: None,
        },
        WindowChrome::MacOs | WindowChrome::Linux => chrome.titlebar(TITLE.into()),
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(titlebar),
        window_decorations: Some(chrome.decorations()),
        app_owns_titlebar_drag: chrome.app_owns_titlebar_drag(),
        window_min_size: Some(size(px(metrics::MIN_WIDTH), px(metrics::MIN_HEIGHT))),
        app_id: Some(crate::APP_ID.to_owned()),
        ..WindowOptions::default()
    }
}

/// The commands with keys, grouped, keeping those that match `query`
/// (title, command id or key label, ignoring case).
pub fn groups(entries: Vec<CheatsheetEntry>, query: &str) -> Vec<(Category, Vec<CheatsheetEntry>)> {
    let needle = query.trim().to_lowercase();
    let mut out: Vec<(Category, Vec<CheatsheetEntry>)> = Vec::new();
    for entry in entries {
        let matches = needle.is_empty()
            || entry.title.to_lowercase().contains(&needle)
            || entry.command.as_str().contains(&needle)
            || entry
                .keys
                .iter()
                .any(|k| k.to_lowercase().contains(&needle));
        if !matches {
            continue;
        }
        match out.last_mut() {
            Some((category, list)) if *category == entry.category => list.push(entry),
            _ => out.push((entry.category, vec![entry])),
        }
    }
    out
}

/// The cheatsheet as Markdown: a heading and a table per group.
pub fn markdown(groups: &[(Category, Vec<CheatsheetEntry>)]) -> String {
    let escape = |s: &str| s.replace('|', "\\|");
    let mut out = String::from("# Keyboard shortcuts\n");
    for (category, entries) in groups {
        out.push_str(&format!(
            "\n## {}\n\n| Command | Keys |\n|---|---|\n",
            category.title()
        ));
        for e in entries {
            let keys: Vec<String> = e.keys.iter().map(|k| format!("`{}`", escape(k))).collect();
            out.push_str(&format!("| {} | {} |\n", escape(&e.title), keys.join(", ")));
        }
    }
    out
}

/// The cheatsheet's view.
pub struct Cheatsheet {
    search: Entity<InputState>,
    query: String,
    /// "Copied" after Copy as Markdown.
    copied: bool,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for Cheatsheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cheatsheet")
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl Focusable for Cheatsheet {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Cheatsheet {
    pub fn new(window: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let search_events = cx.subscribe_in(&search, window, |this, input, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = input.read(cx).value().to_string();
                this.copied = false;
                cx.notify();
            }
        });
        let keymap_changed = cx.observe_global::<Keybindings>(|_, cx| cx.notify());
        search.update(cx, |input, cx| input.focus(window, cx));
        Self {
            search,
            query: String::new(),
            copied: false,
            focus: cx.focus_handle(),
            _subscriptions: vec![search_events, keymap_changed],
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replaces the filter, as typing it does.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx)
        });
        self.query = query.to_owned();
        cx.notify();
    }

    /// The groups shown.
    pub fn groups(&self, cx: &App) -> Vec<(Category, Vec<CheatsheetEntry>)> {
        let Some(bindings) = cx.try_global::<Keybindings>() else {
            return Vec::new();
        };
        groups(
            bindings.keymap().cheatsheet(bindings.registry()),
            &self.query,
        )
    }

    /// Copies the groups shown as Markdown.
    pub fn copy_markdown(&mut self, cx: &mut Context<'_, Self>) {
        let text = markdown(&self.groups(cx));
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = true;
        cx.notify();
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.query.is_empty() {
            window.remove_window();
        } else {
            self.set_query("", window, cx);
        }
    }

    fn head(&self, theme: &Theme, window: &Window, cx: &mut Context<'_, Self>) -> AnyElement {
        let focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
        let chrome = WindowChrome::current();
        let moves_itself = chrome == WindowChrome::MacOs || client_decorated;
        let hover = theme.state.hover;
        let view = cx.entity().downgrade();
        let copy: controls::OnClick = Box::new(move |_, _, cx| {
            let _ = view.update(cx, |this, cx| this.copy_markdown(cx));
        });
        let edit: controls::OnClick = Box::new(|_, _, cx| {
            cx.defer(|cx| {
                if crate::settings_window::open(cx).is_ok()
                    && let Some((_, view)) = crate::settings_window::settings_window(cx)
                {
                    view.update(cx, |this, cx| this.select_section(SectionId::Keymap, cx));
                }
            });
        });
        div()
            .id("cheatsheet-head")
            .h(px(metrics::HEAD))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(metrics::GAP))
            .px(px(metrics::PAD_X))
            .border_b(px(HAIRLINE))
            .border_color(theme.border.default)
            .window_control_area(WindowControlArea::Drag)
            .when(moves_itself, |d| {
                d.on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            })
            .when(client_decorated && chrome == WindowChrome::Linux, |d| {
                d.child(
                    div()
                        .id("cheatsheet-close")
                        .debug_selector(|| "cheatsheet-close".into())
                        .size(px(linux_controls::DIAMETER))
                        .flex_none()
                        .rounded_full()
                        .bg(theme.state.press)
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(move |s| s.bg(hover))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(|_, window, _| window.remove_window())
                        .child(crate::icons::Icon::WindowClose.sized(linux_controls::GLYPH)),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(text::UI_TITLE.size))
                    .line_height(px(text::UI_TITLE.line))
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child("Keyboard shortcuts"),
            )
            .child(
                div()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        controls::field_frame(focused, false, Some(metrics::FIELD_WIDTH), theme)
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.search)
                                        .aria_label("Filter shortcuts")
                                        .appearance(false)
                                        .h(px(text::UI.line))
                                        .px_0()
                                        .py_0()
                                        .text_size(px(text::UI.size))
                                        .text_color(theme.text.primary),
                                ),
                            ),
                    ),
            )
            .child(
                controls::quiet_button(
                    "cheatsheet-copy",
                    if self.copied {
                        "Copied"
                    } else {
                        "Copy as Markdown"
                    },
                    copy,
                    theme,
                )
                .debug_selector(|| "cheatsheet-copy".into()),
            )
            .child(
                controls::quiet_button("cheatsheet-edit", "Edit shortcuts…", edit, theme)
                    .debug_selector(|| "cheatsheet-edit".into()),
            )
            .into_any_element()
    }

    fn group(category: Category, entries: Vec<CheatsheetEntry>, theme: &Theme) -> AnyElement {
        div()
            .flex_1()
            .min_w(px(metrics::COLUMN))
            .flex()
            .flex_col()
            .child(controls::heading(category.title(), true, theme))
            .children(entries.into_iter().map(|e| {
                let selector = format!("cheatsheet-row:{}", e.command);
                div()
                    .debug_selector(move || selector.clone())
                    .min_h(px(metrics::ROW))
                    .flex()
                    .items_center()
                    .gap(px(metrics::ROW_GAP))
                    .child(div().flex_1().min_w_0().child(e.title))
                    .children(e.keys.into_iter().map(|k| key_caps(k, theme)))
            }))
            .into_any_element()
    }
}

impl Render for Cheatsheet {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let groups = self.groups(cx);
        let empty = groups.is_empty();
        div()
            .id("cheatsheet")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::dismiss))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.surface.page)
            .text_color(theme.text.primary)
            .font_family(Appearance::interface_font(cx))
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(self.head(theme, window, cx))
            .child(
                div()
                    .id("cheatsheet-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(metrics::PAD_X))
                    .py(px(metrics::PAD_Y))
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_x(px(metrics::COLUMN_GAP))
                    .gap_y(px(metrics::PAD_Y))
                    .when(empty, |d| {
                        d.child(
                            div()
                                .h(px(sz::LIST_ROW))
                                .rounded(px(radius::CONTROL))
                                .text_color(theme.text.secondary)
                                .child("No shortcut matches."),
                        )
                    })
                    .children(
                        groups
                            .into_iter()
                            .map(|(category, entries)| Self::group(category, entries, theme)),
                    ),
            )
    }
}

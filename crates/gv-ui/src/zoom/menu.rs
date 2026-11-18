// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The status bar's zoom ("100%") and the menu it opens (not designed:
//! drawn like the profile chip's menu, opening upwards from the zoom,
//! right-aligned with it).
//!
//! - **Rows:** Zoom in, Zoom out and Reset zoom with their keys from the
//!   keymap, a separator, then the levels of [`super::LEVELS`] (and the
//!   zoom in effect when it is none of them) with the current one checked.
//!   Zoom in and Zoom out leave the menu open, so the check can be watched
//!   moving; a level or Reset closes it.
//! - **Keys:** like the other menus: the menu takes the keyboard when it
//!   opens, with the current level highlighted; Up, Down, Home and End
//!   move, Enter picks, Esc closes and gives the keyboard back. The zoom
//!   itself is a Tab stop that Enter and Space open.
//! - A click outside closes the menu without doing more.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, Context, Div, FocusHandle, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Point, Render, RenderOnce, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, anchored, canvas, deferred, div, px,
};

use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap;
use crate::menu::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use crate::menu::{panel, row, separator};
use crate::metrics::spacing::menu as spacing;
use crate::metrics::{font, icon, text};
use crate::theme::Theme;

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    In,
    Out,
    Reset,
    /// A zoom level, in percent.
    Level(u32),
}

impl Entry {
    /// The row's name and the command whose keys it shows.
    fn title_and_command(self) -> (SharedString, Option<&'static str>) {
        match self {
            Self::In => ("Zoom in".into(), Some("view.zoom_in")),
            Self::Out => ("Zoom out".into(), Some("view.zoom_out")),
            Self::Reset => (
                format!("Reset zoom ({}%)", super::DEFAULT).into(),
                Some("view.zoom_reset"),
            ),
            Self::Level(percent) => (format!("{percent}%").into(), None),
        }
    }

    /// The row's debug selector.
    fn selector(self) -> String {
        match self {
            Self::In => "zoom-menu-item:in".to_owned(),
            Self::Out => "zoom-menu-item:out".to_owned(),
            Self::Reset => "zoom-menu-item:reset".to_owned(),
            Self::Level(percent) => format!("zoom-menu-level:{percent}"),
        }
    }
}

/// The rows for the zoom `current`: the commands, then the levels with
/// `current` among them.
pub fn entries(current: u32) -> Vec<Entry> {
    let mut levels = super::LEVELS.to_vec();
    if !levels.contains(&current) {
        levels.push(current);
        levels.sort_unstable();
    }
    [Entry::In, Entry::Out, Entry::Reset]
        .into_iter()
        .chain(levels.into_iter().map(Entry::Level))
        .collect()
}

/// The status bar's zoom with its menu. `label` is the zoom as shown
/// ("110%").
#[derive(Debug, IntoElement)]
pub struct ZoomButton {
    label: SharedString,
}

impl ZoomButton {
    pub fn new(label: SharedString) -> Self {
        Self { label }
    }
}

impl RenderOnce for ZoomButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let menu = window.use_keyed_state("status-zoom-menu", cx, |_, cx| ZoomMenu::new(cx));
        let (open, anchor) = {
            let menu = menu.read(cx);
            (menu.open, menu.anchor.clone())
        };
        let theme = Appearance::theme(cx);
        let hover = theme.text.primary;
        let toggle = menu.clone();
        let button: Stateful<Div> = div()
            .id("status-zoom")
            .a11y_button(format!("Zoom {}", self.label))
            .aria_description("Change the zoom")
            .aria_expanded(open)
            .keyboard_control(theme)
            .debug_selector(|| "status-zoom".into())
            .flex_none()
            .hover(move |style| style.text_color(hover))
            .on_click(move |_, window, cx| {
                toggle.update(cx, |menu, cx| menu.toggle(window, cx));
            })
            // Where the zoom is, for the menu to open above it.
            .child(
                canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(self.label);
        button
            .when(open, |button| {
                button.child(
                    deferred(anchored().position(Point::default()).child(menu)).with_priority(1),
                )
            })
            .accessible()
    }
}

/// The zoom menu.
pub struct ZoomMenu {
    focus: FocusHandle,
    open: bool,
    /// What had the keyboard before the menu took it.
    restore: Option<FocusHandle>,
    /// The highlighted row.
    item: Option<usize>,
    /// The zoom, in window coordinates.
    anchor: Rc<Cell<Bounds<Pixels>>>,
}

impl std::fmt::Debug for ZoomMenu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoomMenu")
            .field("open", &self.open)
            .field("item", &self.item)
            .finish_non_exhaustive()
    }
}

impl ZoomMenu {
    fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            open: false,
            restore: None,
            item: None,
            anchor: Rc::default(),
        }
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.open {
            self.close(window, cx);
        } else {
            self.open(window, cx);
        }
    }

    /// Opens the menu with the current level highlighted, and takes the
    /// keyboard.
    fn open(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let current = super::percent(cx);
        self.item = entries(current)
            .iter()
            .position(|e| *e == Entry::Level(current));
        self.restore = window.focused(cx);
        self.open = true;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the menu and gives the keyboard back.
    fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.item = None;
        if let Some(focus) = self.restore.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Moves the highlight by one row, wrapping round.
    fn step(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let len = entries(super::percent(cx)).len();
        self.item = Some(match (self.item, forward) {
            (Some(i), true) => (i + 1) % len,
            (Some(i), false) => (i + len - 1) % len,
            (None, true) => 0,
            (None, false) => len - 1,
        });
        cx.notify();
    }

    fn edge(&mut self, last: bool, cx: &mut Context<'_, Self>) {
        let len = entries(super::percent(cx)).len();
        self.item = Some(if last { len - 1 } else { 0 });
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let rows = entries(super::percent(cx));
        if let Some(entry) = self.item.and_then(|i| rows.get(i).copied()) {
            self.run(entry, window, cx);
        }
    }

    /// Does what `entry` says. Zoom in and out keep the menu open, with the
    /// highlight on the same command.
    fn run(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<'_, Self>) {
        match entry {
            Entry::In => super::zoom_in(cx),
            Entry::Out => super::zoom_out(cx),
            Entry::Reset => super::reset(cx),
            Entry::Level(percent) => super::set(cx, percent),
        }
        if matches!(entry, Entry::Reset | Entry::Level(_)) {
            self.close(window, cx);
        }
        cx.notify();
    }

    fn row(
        &self,
        index: usize,
        entry: Entry,
        current: u32,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let highlighted = self.item == Some(index);
        let (title, command) = entry.title_and_command();
        let checked = match entry {
            Entry::Level(percent) => Some(percent == current),
            _ => None,
        };
        let shortcut = command.and_then(|c| keymap::shortcut(cx, c));
        let selector = entry.selector();
        let hover = theme.state.hover;
        row(theme, highlighted)
            .id(("zoom-menu-row", index))
            .a11y_menu_item(title.clone(), checked)
            .when_some(shortcut.clone(), |row, keys| row.aria_keyshortcuts(keys))
            // The menu keeps the keyboard; the highlighted row is the one
            // assistive technology reads.
            .when(highlighted, |row| row.aria_active_descendant())
            .debug_selector(move || selector.clone())
            .when(!highlighted, |row| row.hover(move |s| s.bg(hover)))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.item != Some(index) {
                    this.item = Some(index);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.run(entry, window, cx)))
            .child(
                div()
                    .flex_none()
                    .w(px(icon::ROW))
                    .when(checked == Some(true), |slot| {
                        slot.debug_selector(move || format!("zoom-menu-check:{current}"))
                            .child(Icon::MenuCheck.sized(icon::ROW))
                    }),
            )
            .child(div().flex_1().min_w_0().truncate().child(title))
            .children(shortcut.map(|shortcut| {
                div()
                    .flex_none()
                    .ml(px(spacing::SHORTCUT_GAP))
                    .font_family(font::EDITOR)
                    .text_size(px(text::menu_shortcut_size()))
                    .text_color(theme.text.secondary)
                    .child(shortcut)
            }))
            .accessible()
            .into_any_element()
    }
}

impl Render for ZoomMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let anchor = self.anchor.get();
        let viewport = window.viewport_size();
        let current = super::percent(cx);
        let rows = entries(current);
        let mut children: Vec<AnyElement> = Vec::with_capacity(rows.len() + 1);
        for (index, entry) in rows.into_iter().enumerate() {
            if index == 3 {
                children.push(separator(theme).into_any_element());
            }
            children.push(self.row(index, entry, current, theme, cx));
        }
        // Above the zoom, inside the window.
        let room = (anchor.top() - px(spacing::OFFSET_Y + spacing::EDGE)).max(px(0.0));
        let menu = panel(theme)
            .id("zoom-menu")
            .a11y_group(Role::Menu, "Zoom")
            .debug_selector(|| "zoom-menu".into())
            .max_h(room)
            .overflow_y_scroll()
            .children(children)
            .accessible();
        div()
            .id("zoom-menu-overlay")
            .key_context(keymap::context::MENU)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.edge(false, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.edge(true, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close(window, cx)))
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            // A click outside closes the menu. It must not focus the layer,
            // which would keep the keyboard from going back.
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
            .child(
                div()
                    .absolute()
                    .right(viewport.width - anchor.right())
                    .bottom(viewport.height - anchor.top() + px(spacing::OFFSET_Y))
                    .child(menu),
            )
    }
}

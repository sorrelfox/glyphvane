// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The application menu as a popover, for Windows and Linux.
//!
//! The popover lists the menus; the highlighted one shows its items in a
//! submenu at its right, its first row level with the menu's row. While the
//! menu is open it has the keyboard and covers the window with a clear
//! layer, so a click outside only closes it. A command runs where the
//! keyboard was before the menu opened, as if its keys had been pressed
//! there; the menu enables an item only when that place handles its command.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, BoxShadow, Context, Div, FocusHandle, Global,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Render,
    RenderOnce, Stateful, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window,
    anchored, canvas, deferred, div, point, px,
};
use gv_settings::keymap::Platform;

use super::model::{Entry, Item, Menu, MenuState, app_menus};
use super::{
    Cancel, CloseSubmenu, Confirm, OpenSubmenu, SelectFirst, SelectLast, SelectNext, SelectPrevious,
};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::keymap::{self, action_for};
use crate::metrics::size::{MENU_ROW, MENU_WIDTH};
use crate::metrics::spacing::menu as spacing;
use crate::metrics::{DISABLED_OPACITY, HAIRLINE, font, icon, radius, text};
use crate::shell::most_recent_window;
use crate::theme::Theme;

/// Set to a menu's title (such as `View`) to open the application menu with
/// that menu's items showing once the window is drawn, for screenshots and
/// manual checks.
pub const DEBUG_APP_MENU_ENV: &str = "GLYPHVANE_DEBUG_APP_MENU";

fn debug_menu() -> Option<&'static str> {
    static MENU: OnceLock<Option<String>> = OnceLock::new();
    MENU.get_or_init(|| std::env::var(DEBUG_APP_MENU_ENV).ok())
        .as_deref()
}

/// The menu button with its menu. The window draws the button; this adds
/// what it opens.
#[derive(IntoElement)]
pub struct MenuButton {
    button: Stateful<Div>,
    state: MenuState,
}

impl std::fmt::Debug for MenuButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuButton")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl MenuButton {
    pub fn new(button: Stateful<Div>, state: MenuState) -> Self {
        Self { button, state }
    }
}

/// The menus of the open windows, newest last, for the key that opens the
/// menu ([`open_in_last_window`]).
#[derive(Debug, Default)]
struct Popovers(Vec<(AnyWindowHandle, WeakEntity<MenuPopover>)>);

impl Global for Popovers {}

impl Popovers {
    fn add(window: AnyWindowHandle, popover: WeakEntity<MenuPopover>, cx: &mut App) {
        let popovers = &mut cx.default_global::<Self>().0;
        popovers.retain(|(_, p)| p.upgrade().is_some());
        popovers.push((window, popover));
    }
}

/// Opens the application menu of the main window focused last, or closes
/// it, as its button does (`app.open_menu`). Nothing happens where the menu
/// is the system's menu bar.
pub(crate) fn open_in_last_window(cx: &mut App) {
    let Some(window) = most_recent_window(cx).map(|w| w.window) else {
        return;
    };
    let popover = cx.try_global::<Popovers>().and_then(|popovers| {
        popovers
            .0
            .iter()
            .rev()
            .find(|(w, p)| *w == window && p.upgrade().is_some())
            .map(|(_, p)| p.clone())
    });
    let Some(popover) = popover else {
        return;
    };
    // The window is busy with the key that asked for the menu.
    cx.defer(move |cx| {
        // Fails only when the window or its menu has gone.
        let _ = window.update(cx, |_, window, cx| {
            popover.update(cx, |popover, cx| popover.toggle(window, cx))
        });
    });
}

impl RenderOnce for MenuButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let popover = window.use_keyed_state("app-menu-popover", cx, |window, cx| {
            Popovers::add(window.window_handle(), cx.weak_entity(), cx);
            MenuPopover::new(cx)
        });
        let state = self.state;
        popover.update(cx, |popover, _| popover.state = state);
        open_for_debug(&popover, window, cx);
        let (open, anchor) = {
            let popover = popover.read(cx);
            (popover.is_open(), popover.anchor.clone())
        };
        let toggle = popover.clone();
        self.button
            .on_click(move |_, window, cx| {
                toggle.update(cx, |popover, cx| popover.toggle(window, cx));
            })
            // Where the button is, for the menu to hang below it.
            .child(
                canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .when(open, |button| {
                button.child(
                    deferred(anchored().position(point(px(0.0), px(0.0))).child(popover))
                        .with_priority(1),
                )
            })
    }
}

/// Opens the menu named by [`DEBUG_APP_MENU_ENV`] once, as soon as the
/// button has been laid out.
fn open_for_debug(popover: &gpui::Entity<MenuPopover>, window: &mut Window, cx: &mut App) {
    let Some(title) = debug_menu() else {
        return;
    };
    let (done, anchor) = {
        let popover = popover.read(cx);
        (popover.debug_opened, popover.anchor.get())
    };
    if done {
        return;
    }
    if anchor.size.width <= px(0.0) {
        window.request_animation_frame();
        return;
    }
    popover.update(cx, |popover, cx| {
        popover.debug_opened = true;
        popover.open(window, cx);
        if let Some(index) = popover.menus.iter().position(|m| m.title == title) {
            popover.open_submenu(index, true, window, cx);
        }
    });
}

/// The application menu while it is open.
pub struct MenuPopover {
    focus: FocusHandle,
    open: bool,
    /// What had the keyboard before the menu took it. Commands run there.
    restore: Option<FocusHandle>,
    menus: Vec<Menu>,
    state: MenuState,
    /// The highlighted menu.
    top: usize,
    /// Whether the highlighted menu shows its items.
    submenu: bool,
    /// The highlighted entry of the submenu.
    item: Option<usize>,
    /// The menu button, in window coordinates.
    anchor: Rc<Cell<Bounds<Pixels>>>,
    debug_opened: bool,
}

impl std::fmt::Debug for MenuPopover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuPopover")
            .field("open", &self.open)
            .field("top", &self.top)
            .field("submenu", &self.submenu)
            .field("item", &self.item)
            .finish_non_exhaustive()
    }
}

impl MenuPopover {
    pub fn new(cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            open: false,
            restore: None,
            menus: app_menus(Platform::current()),
            state: MenuState::default(),
            top: 0,
            submenu: false,
            item: None,
            anchor: Rc::default(),
            debug_opened: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.open {
            self.close(window, cx);
        } else {
            self.open(window, cx);
        }
    }

    /// Opens the menu with its first menu highlighted, and takes the
    /// keyboard.
    pub fn open(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.open {
            return;
        }
        self.restore = window.focused(cx);
        self.open = true;
        self.top = 0;
        self.submenu = false;
        self.item = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the menu and gives the keyboard back.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.open {
            return;
        }
        self.open = false;
        self.submenu = false;
        self.item = None;
        if let Some(focus) = self.restore.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Shows the items of menu `index`; `select_first` also highlights its
    /// first enabled item, as the keyboard does.
    fn open_submenu(
        &mut self,
        index: usize,
        select_first: bool,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) {
        if index >= self.menus.len() || (self.submenu && self.top == index && !select_first) {
            return;
        }
        self.top = index;
        self.submenu = true;
        self.item = if select_first {
            self.enabled_entries(window).first().copied()
        } else {
            None
        };
        cx.notify();
    }

    /// Whether `item` can run now: its condition holds and its command has
    /// a handler where the keyboard was.
    fn enabled(&self, item: &Item, window: &Window) -> bool {
        if item.requires.is_some_and(|r| !self.state.allows(r)) {
            return false;
        }
        let Some(action) = action_for(item.command) else {
            return false;
        };
        keymap::APP_COMMANDS.contains(&item.command)
            || self
                .restore
                .as_ref()
                .is_some_and(|focus| window.is_action_available_in(action.as_ref(), focus))
    }

    /// The indices of the open submenu's enabled items.
    fn enabled_entries(&self, window: &Window) -> Vec<usize> {
        let Some(menu) = self.menus.get(self.top) else {
            return Vec::new();
        };
        menu.entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| match entry {
                Entry::Command(item) if self.enabled(item, window) => Some(i),
                _ => None,
            })
            .collect()
    }

    /// Moves the highlight to the next (or previous) menu, or item of the
    /// submenu, wrapping around. Disabled items are skipped.
    fn step(&mut self, forward: bool, window: &Window, cx: &mut Context<'_, Self>) {
        if self.submenu {
            let enabled = self.enabled_entries(window);
            let len = enabled.len();
            if len == 0 {
                return;
            }
            let at = self
                .item
                .and_then(|item| enabled.iter().position(|&e| e == item));
            let next = match (at, forward) {
                (Some(p), true) => (p + 1) % len,
                (Some(p), false) => (p + len - 1) % len,
                (None, true) => 0,
                (None, false) => len - 1,
            };
            self.item = Some(enabled[next]);
        } else {
            let len = self.menus.len();
            self.top = if forward {
                (self.top + 1) % len
            } else {
                (self.top + len - 1) % len
            };
        }
        cx.notify();
    }

    /// Moves the highlight to the first (or last) menu or item.
    fn edge(&mut self, last: bool, window: &Window, cx: &mut Context<'_, Self>) {
        if self.submenu {
            let enabled = self.enabled_entries(window);
            self.item = if last {
                enabled.last().copied()
            } else {
                enabled.first().copied()
            };
        } else {
            self.top = if last { self.menus.len() - 1 } else { 0 };
        }
        cx.notify();
    }

    /// Right: opens the highlighted menu; in a submenu, moves on to the
    /// next menu, as in a menu bar.
    fn open_next(&mut self, window: &Window, cx: &mut Context<'_, Self>) {
        let index = if self.submenu {
            (self.top + 1) % self.menus.len()
        } else {
            self.top
        };
        self.submenu = false;
        self.open_submenu(index, true, window, cx);
    }

    /// Left: goes back from the submenu to the menus.
    fn close_submenu(&mut self, cx: &mut Context<'_, Self>) {
        if self.submenu {
            self.submenu = false;
            self.item = None;
            cx.notify();
        }
    }

    /// Enter: runs the highlighted item, or opens the highlighted menu.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.submenu {
            self.open_submenu(self.top, true, window, cx);
            return;
        }
        let entry = self
            .item
            .and_then(|i| self.menus.get(self.top)?.entries.get(i).copied());
        if let Some(Entry::Command(item)) = entry {
            self.run(item, window, cx);
        }
    }

    /// Closes the menu and runs `item`'s command where the keyboard was.
    fn run(&mut self, item: Item, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.enabled(&item, window) {
            return;
        }
        let Some(action) = action_for(item.command) else {
            return;
        };
        self.close(window, cx);
        window.dispatch_action(action, cx);
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

/// A menu's panel: the popover look, 228 wide, padding 4. A submenu grows
/// past 228 when a label and its shortcut need more room (not designed).
pub(crate) fn panel(theme: &Theme) -> Div {
    div()
        .min_w(px(MENU_WIDTH))
        .p(px(spacing::PAD))
        .flex()
        .flex_col()
        .rounded(px(radius::POPOVER))
        .bg(theme.surface.raised)
        .border(px(HAIRLINE))
        .border_color(theme.border.strong)
        .shadow(popover_shadow(theme))
        .text_color(theme.text.primary)
        .text_size(px(text::UI.size))
        .line_height(px(text::UI.line))
        // A click inside a panel is not a click outside the menu.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
}

/// A row: 24 high, padding `0 8`, radius 5; `accent.tint` when highlighted.
pub(crate) fn row(theme: &Theme, highlighted: bool) -> Div {
    div()
        .h(px(MENU_ROW))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(spacing::ICON_GAP))
        .px(px(spacing::ROW_PAD_X))
        .rounded(px(radius::CONTROL))
        .when(highlighted, |row| row.bg(theme.accent.tint))
}

fn label(title: &'static str) -> Div {
    div().flex_1().min_w_0().truncate().child(title)
}

/// A separator: 1 px `border.default`, margin `4 8`.
pub(crate) fn separator(theme: &Theme) -> Div {
    div()
        .h(px(HAIRLINE))
        .flex_none()
        .mx(px(spacing::SEPARATOR_MARGIN_X))
        .my(px(spacing::SEPARATOR_MARGIN_Y))
        .bg(theme.border.default)
}

impl MenuPopover {
    fn top_row(
        &self,
        index: usize,
        title: &'static str,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        row(theme, self.top == index)
            .id(("app-menu-top", index))
            .debug_selector(move || format!("app-menu-top:{title}"))
            .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                if *hovered {
                    this.open_submenu(index, false, window, cx);
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_submenu(index, false, window, cx);
            }))
            .child(label(title))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text.secondary)
                    .child(Icon::Submenu.sized(icon::CHEVRON)),
            )
            .into_any_element()
    }

    fn item_row(
        &self,
        index: usize,
        item: Item,
        checks: bool,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let enabled = self.enabled(&item, window);
        let highlighted = enabled && self.item == Some(index);
        let checked = item.mode.is_some_and(|mode| mode == self.state.mode);
        let shortcut = keymap::shortcut(cx, item.command);
        let command = item.command;
        row(theme, highlighted)
            .id(("app-menu-item", index))
            .debug_selector(move || format!("app-menu-item:{command}"))
            .map(|row| {
                if enabled {
                    row.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.item != Some(index) {
                            this.item = Some(index);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.run(item, window, cx)))
                } else {
                    row.opacity(DISABLED_OPACITY)
                }
            })
            .when(checks, |row| {
                row.child(div().flex_none().w(px(icon::ROW)).when(checked, |slot| {
                    slot.debug_selector(move || format!("app-menu-check:{command}"))
                        .child(Icon::MenuCheck.sized(icon::ROW))
                }))
            })
            .child(label(item.title))
            .children(shortcut.map(|shortcut| {
                let selector = format!("app-menu-shortcut:{shortcut}");
                div()
                    .debug_selector(move || selector.clone())
                    .flex_none()
                    .ml(px(spacing::SHORTCUT_GAP))
                    .font_family(font::EDITOR)
                    .text_size(px(text::MENU_SHORTCUT_SIZE))
                    .text_color(theme.text.secondary)
                    .child(shortcut)
            }))
            .into_any_element()
    }

    fn submenu_panel(
        &self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let menu = self.menus.get(self.top)?;
        let checks = menu.has_checks();
        let rows: Vec<AnyElement> = menu
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match *entry {
                Entry::Separator => Some(separator(theme).into_any_element()),
                Entry::Command(item) => Some(self.item_row(index, item, checks, theme, window, cx)),
                Entry::System(_) => None,
            })
            .collect();
        Some(
            panel(theme)
                .debug_selector(|| "app-menu-submenu".into())
                .absolute()
                .left(px(MENU_WIDTH + spacing::SUBMENU_GAP))
                // The first item level with its menu's row.
                .top(px(self.top as f32 * MENU_ROW))
                .children(rows)
                .into_any_element(),
        )
    }
}

impl Render for MenuPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let anchor = self.anchor.get();
        let viewport = window.viewport_size();
        let titles: Vec<&'static str> = self.menus.iter().map(|m| m.title).collect();
        let menus = panel(theme)
            .w(px(MENU_WIDTH))
            .debug_selector(|| "app-menu-popover".into())
            .children(
                titles
                    .into_iter()
                    .enumerate()
                    .map(|(index, title)| self.top_row(index, title, theme, cx)),
            );
        let submenu = if self.submenu {
            self.submenu_panel(theme, window, cx)
        } else {
            None
        };
        div()
            .id("app-menu-overlay")
            .key_context(keymap::context::MENU)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| this.step(true, window, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.step(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, window, cx| this.edge(false, window, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, window, cx| this.edge(true, window, cx)))
            .on_action(cx.listener(|this, _: &OpenSubmenu, window, cx| this.open_next(window, cx)))
            .on_action(cx.listener(|this, _: &CloseSubmenu, _, cx| this.close_submenu(cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close(window, cx)))
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            // A click outside closes the menu. It must not focus the layer
            // (GPUI focuses what is clicked), which would keep the keyboard
            // from going back.
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
                    .left(anchor.left())
                    .top(anchor.bottom() + px(spacing::OFFSET_Y))
                    .child(menus)
                    .children(submenu),
            )
    }
}

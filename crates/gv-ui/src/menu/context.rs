// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Context menus: one panel drawn like the link menu (SPEC § Popovers:
//! 228 wide, padding 4, rows 24 with the shortcut at the right, separators
//! with margin `4 8`, the highlighted row in `accent.tint`).
//!
//! - **Where:** at the pointer for a right-click, below a button for the
//!   More actions menu, at the caret or the selected row for the context
//!   menu key (Menu or Shift+F10, `workspace.context_menu`). The menu stays
//!   inside the window: it moves left or up as far as it needs to, and when
//!   it is taller than the window it scrolls, following the highlighted
//!   item (not designed).
//! - **Items** run their command where the keyboard was when the menu
//!   opened, like the application menu; items whose command nothing handles
//!   there are dimmed and skipped by the arrow keys. While the command runs,
//!   [`context_target`] names what the menu was opened on (a tab), for
//!   commands that otherwise act on the active tab.
//! - **Keys:** Up, Down, Home and End move, Enter runs, Esc closes; a click
//!   outside closes without doing more.
//!
//! Each main window has one context menu ([`ContextMenuPopover`]), drawn by
//! the window. Views open it with [`show_context_menu`].

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, Entity, FocusHandle, Global,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, anchored,
    deferred, div, point, px,
};

use super::model::{ContextMenu, Entry, Menu, MenuState, context_menu};
use super::popover::{
    ItemLook, child_index, fit_horizontally, fit_vertically, item_enabled, item_row, panel,
    panel_height, separator,
};
use super::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use crate::appearance::Appearance;
use crate::keymap::{self, action_for};
use crate::metrics::size::MENU_WIDTH;
use crate::metrics::spacing::menu as spacing;

/// Where a context menu opens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// Its top left corner at this point (the pointer, the caret), or its
    /// top right corner when there is no room at the right.
    At(Point<Pixels>),
    /// Below this button, right-aligned with it.
    Below(Bounds<Pixels>),
}

/// What a context menu was opened on, for the commands it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextTarget {
    /// The active document, the caret or the selection.
    #[default]
    Active,
    /// Tab `n` of the window.
    Tab(usize),
}

/// What the item running now was opened on.
#[derive(Debug, Default)]
struct RunningTarget(ContextTarget);

impl Global for RunningTarget {}

/// What the context menu item whose command is running was opened on;
/// [`ContextTarget::Active`] when the command came from anywhere else.
pub fn context_target(cx: &App) -> ContextTarget {
    cx.try_global::<RunningTarget>()
        .map_or(ContextTarget::Active, |t| t.0)
}

/// The context menus of the open windows.
#[derive(Debug, Default)]
struct Hosts(Vec<(AnyWindowHandle, WeakEntity<ContextMenuPopover>)>);

impl Global for Hosts {}

/// The context menu of `window`, if the window has one.
pub fn context_menu_of(window: &Window, cx: &App) -> Option<Entity<ContextMenuPopover>> {
    let handle = window.window_handle();
    cx.try_global::<Hosts>()?
        .0
        .iter()
        .rev()
        .filter(|(w, _)| *w == handle)
        .find_map(|(_, p)| p.upgrade())
}

/// Opens context menu `kind` at `position` (window coordinates) in
/// `window`, for a right-click in a view. Its commands run where the
/// keyboard is now, so a view focuses itself first.
pub fn show_context_menu(
    kind: ContextMenu,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(popover) = context_menu_of(window, cx) {
        popover.update(cx, |popover, cx| {
            popover.open(
                kind,
                Placement::At(position),
                MenuState::default(),
                ContextTarget::Active,
                false,
                window,
                cx,
            );
        });
    }
}

/// An open menu.
#[derive(Debug)]
struct Open {
    kind: ContextMenu,
    menu: Menu,
    placement: Placement,
    state: MenuState,
    target: ContextTarget,
    /// What had the keyboard before the menu took it. Commands run there.
    restore: Option<FocusHandle>,
    /// The highlighted entry.
    item: Option<usize>,
}

/// A window's context menu.
pub struct ContextMenuPopover {
    focus: FocusHandle,
    open: Option<Open>,
    scroll: ScrollHandle,
}

impl std::fmt::Debug for ContextMenuPopover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextMenuPopover")
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl ContextMenuPopover {
    /// The context menu of `window`, closed.
    pub fn new(window: &Window, cx: &mut Context<'_, Self>) -> Self {
        let this = cx.weak_entity();
        let hosts = &mut cx.default_global::<Hosts>().0;
        hosts.retain(|(_, p)| p.upgrade().is_some());
        hosts.push((window.window_handle(), this));
        Self {
            focus: cx.focus_handle(),
            open: None,
            scroll: ScrollHandle::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The open menu.
    pub fn kind(&self) -> Option<ContextMenu> {
        self.open.as_ref().map(|o| o.kind)
    }

    /// The highlighted item's command.
    pub fn highlighted(&self) -> Option<&'static str> {
        let open = self.open.as_ref()?;
        match open.menu.entries.get(open.item?)? {
            Entry::Command(item) => Some(item.command),
            Entry::Separator | Entry::System(_) => None,
        }
    }

    /// Opens menu `kind` and takes the keyboard. `state` decides the items'
    /// conditions; `keyboard` highlights the first enabled item, as when
    /// the menu key opened it.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        kind: ContextMenu,
        placement: Placement,
        state: MenuState,
        target: ContextTarget,
        keyboard: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // A second right-click while open: the keyboard still goes back
        // where it was before the first.
        let restore = match self.open.take() {
            Some(open) => open.restore,
            None => window.focused(cx),
        };
        self.open = Some(Open {
            kind,
            menu: context_menu(kind),
            placement,
            state,
            target,
            restore,
            item: None,
        });
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        if keyboard {
            let first = self.enabled_entries(window).first().copied();
            if let Some(open) = &mut self.open {
                open.item = first;
            }
            self.reveal();
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Closes the menu and gives the keyboard back.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(open) = self.open.take() else {
            return;
        };
        if let Some(focus) = open.restore {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn enabled_entries(&self, window: &Window) -> Vec<usize> {
        let Some(open) = &self.open else {
            return Vec::new();
        };
        open.menu
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| match entry {
                Entry::Command(item)
                    if item_enabled(item, open.state, open.restore.as_ref(), window) =>
                {
                    Some(i)
                }
                _ => None,
            })
            .collect()
    }

    /// Moves the highlight to the next (or previous) enabled item,
    /// wrapping around.
    fn step(&mut self, forward: bool, window: &Window, cx: &mut Context<'_, Self>) {
        let enabled = self.enabled_entries(window);
        let len = enabled.len();
        if len == 0 {
            return;
        }
        let Some(open) = &mut self.open else {
            return;
        };
        let at = open
            .item
            .and_then(|item| enabled.iter().position(|&e| e == item));
        let next = match (at, forward) {
            (Some(p), true) => (p + 1) % len,
            (Some(p), false) => (p + len - 1) % len,
            (None, true) => 0,
            (None, false) => len - 1,
        };
        open.item = Some(enabled[next]);
        self.reveal();
        cx.notify();
    }

    /// Scrolls a menu taller than the window to its highlighted item.
    fn reveal(&self) {
        if let Some(open) = &self.open
            && let Some(item) = open.item
        {
            self.scroll
                .scroll_to_item(child_index(&open.menu.entries, item));
        }
    }

    /// Moves the highlight to the first (or last) enabled item.
    fn edge(&mut self, last: bool, window: &Window, cx: &mut Context<'_, Self>) {
        let enabled = self.enabled_entries(window);
        let Some(open) = &mut self.open else {
            return;
        };
        open.item = if last {
            enabled.last().copied()
        } else {
            enabled.first().copied()
        };
        self.reveal();
        cx.notify();
    }

    /// Runs entry `index`'s command where the keyboard was, if it can run,
    /// and closes the menu.
    fn run(&mut self, index: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(open) = &self.open else {
            return;
        };
        let Some(Entry::Command(item)) = open.menu.entries.get(index).copied() else {
            return;
        };
        if !item_enabled(&item, open.state, open.restore.as_ref(), window) {
            return;
        }
        let Some(action) = action_for(item.command) else {
            return;
        };
        let (target, restore) = (open.target, open.restore.clone());
        self.close(window, cx);
        // After this update: the command's handler may open a menu again.
        window.defer(cx, move |window, cx| {
            cx.set_global(RunningTarget(target));
            match restore {
                Some(focus) => focus.dispatch_action(action.as_ref(), window, cx),
                None => window.dispatch_action(action, cx),
            }
            cx.set_global(RunningTarget(ContextTarget::Active));
        });
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(index) = self.open.as_ref().and_then(|o| o.item) {
            self.run(index, window, cx);
        }
    }

    /// The panel's top left corner and its height cap, in the window.
    fn frame(open: &Open, window: &Window) -> (Point<Pixels>, Option<f32>) {
        let viewport = window.viewport_size();
        let (width, height) = (MENU_WIDTH, panel_height(&open.menu.entries));
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let (left, top) = match open.placement {
            Placement::At(at) => {
                let x = f32::from(at.x);
                (fit_horizontally(x, x - width, width, vw), f32::from(at.y))
            }
            Placement::Below(button) => (
                fit_horizontally(
                    f32::from(button.right()) - width,
                    f32::from(button.left()),
                    width,
                    vw,
                ),
                f32::from(button.bottom()) + spacing::OFFSET_Y,
            ),
        };
        let (top, cap) = fit_vertically(top, height, vh);
        (point(px(left), px(top)), cap)
    }
}

impl Render for ContextMenuPopover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        let theme = Appearance::theme(cx);
        let (origin, cap) = Self::frame(open, window);
        let viewport = window.viewport_size();
        let checks = open.menu.has_checks();
        let rows: Vec<AnyElement> = open
            .menu
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match *entry {
                Entry::Separator => Some(separator(theme).into_any_element()),
                Entry::Command(item) => {
                    let enabled = item_enabled(&item, open.state, open.restore.as_ref(), window);
                    let checked = item.mode.is_some_and(|mode| mode == open.state.mode);
                    Some(
                        item_row(
                            "context-menu",
                            index,
                            item,
                            ItemLook {
                                enabled,
                                highlighted: open.item == Some(index),
                                check: checks.then_some(checked),
                            },
                            theme,
                            cx,
                        )
                        .when(enabled, |row| {
                            row.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                if let Some(open) = &mut this.open
                                    && *hovered
                                    && open.item != Some(index)
                                {
                                    open.item = Some(index);
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.run(index, window, cx);
                                },
                            ))
                        })
                        .into_any_element(),
                    )
                }
                Entry::System(_) => None,
            })
            .collect();
        let menu = panel(theme)
            .id("context-menu")
            .debug_selector(|| "context-menu".into())
            .w(px(MENU_WIDTH))
            .absolute()
            .left(origin.x)
            .top(origin.y)
            .when_some(cap, |panel, cap| panel.max_h(px(cap)))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);
        let overlay = div()
            .id("context-menu-overlay")
            .key_context(keymap::context::MENU)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| this.step(true, window, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                this.step(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, window, cx| this.edge(false, window, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, window, cx| this.edge(true, window, cx)))
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close(window, cx)))
            .w(viewport.width)
            .h(viewport.height)
            .occlude()
            // A click outside closes the menu without focusing the layer.
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
            .child(menu);
        deferred(anchored().position(Point::default()).child(overlay))
            .with_priority(1)
            .into_any_element()
    }
}

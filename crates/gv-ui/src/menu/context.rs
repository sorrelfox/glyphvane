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
//! - **Spelling:** the editor's menu opened on a misspelled word starts
//!   with the word's suggestions ([`SpellMenu`]): the fast ones at once,
//!   the full list when it arrives ([`ContextMenuPopover::set_suggestions`]).
//!   Choosing one replaces the word.
//!
//! Each main window has one context menu ([`ContextMenuPopover`]), drawn by
//! the window. Views open it with [`show_context_menu`].

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, Context, ElementId, Entity, FocusHandle, Global,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window,
    anchored, deferred, div, point, px,
};

use super::model::{ContextMenu, Entry, Menu, MenuState, context_menu};
use super::popover::{
    ItemLook, child_index, fit_horizontally, fit_vertically, item_enabled, item_row, panel,
    panel_height, row, separator,
};
use super::{Cancel, Confirm, SelectFirst, SelectLast, SelectNext, SelectPrevious};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
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
    /// The link the link menu was opened on
    /// ([`crate::reader::link_info::menu_link`]), in the active tab.
    Link,
}

/// Replaces the misspelled word with the suggestion given.
pub type ApplySuggestion = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The spelling part of the editor's menu: the misspelled word it was
/// opened on and its suggestions.
#[derive(Clone)]
pub struct SpellMenu {
    pub word: SharedString,
    /// Best first; at most [`MAX_SUGGESTIONS`] are shown.
    pub suggestions: Vec<SharedString>,
    /// A vault is open: Add to vault dictionary applies.
    pub in_vault: bool,
    /// The text cannot be changed: the suggestions are disabled.
    pub read_only: bool,
    pub apply: ApplySuggestion,
}

impl std::fmt::Debug for SpellMenu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpellMenu")
            .field("word", &self.word)
            .field("suggestions", &self.suggestions)
            .field("in_vault", &self.in_vault)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

/// Suggestions shown in the menu.
pub const MAX_SUGGESTIONS: usize = 6;

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
            let target = if kind == ContextMenu::Link {
                ContextTarget::Link
            } else {
                ContextTarget::Active
            };
            popover.open(
                kind,
                Placement::At(position),
                MenuState::default(),
                target,
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
    /// The spelling part (the editor's menu on a misspelled word).
    spell: Option<SpellMenu>,
}

impl Open {
    fn suggestion_enabled(&self, n: usize) -> bool {
        self.spell
            .as_ref()
            .is_some_and(|s| !s.read_only && n < s.suggestions.len())
    }

    /// The menu's entries: the suggestions and a separator before the
    /// menu's own.
    fn rebuild(&mut self) {
        let mut menu = context_menu(self.kind);
        if let Some(spell) = &self.spell {
            let n = spell.suggestions.len().min(MAX_SUGGESTIONS);
            if n > 0 {
                let mut entries: Vec<Entry> = (0..n).map(Entry::Suggestion).collect();
                entries.push(Entry::Separator);
                entries.append(&mut menu.entries);
                menu.entries = entries;
            }
        }
        self.menu = menu;
    }
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
            Entry::Separator | Entry::System(_) | Entry::Suggestion(_) => None,
        }
    }

    /// The spelling suggestions shown.
    pub fn suggestions(&self) -> Vec<SharedString> {
        self.open
            .as_ref()
            .and_then(|o| o.spell.as_ref())
            .map(|s| {
                s.suggestions
                    .iter()
                    .take(MAX_SUGGESTIONS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Makes the open editor menu about the misspelled word `spell`: its
    /// suggestions first, and the spelling items enabled. A menu opened
    /// from the keyboard highlights the first suggestion.
    pub fn set_spell(&mut self, spell: SpellMenu, window: &Window, cx: &mut Context<'_, Self>) {
        let Some(open) = &mut self.open else {
            return;
        };
        if open.kind != ContextMenu::Editor {
            return;
        }
        open.state.misspelled = true;
        open.state.in_vault = spell.in_vault;
        let keyboard = open.item.is_some();
        open.spell = Some(spell);
        open.rebuild();
        open.item = None;
        if keyboard {
            let first = self.enabled_entries(window).first().copied();
            if let Some(open) = &mut self.open {
                open.item = first;
            }
            self.reveal();
        }
        cx.notify();
    }

    /// The full list of suggestions for `word`, once it arrives. Ignored
    /// when the menu is no longer about `word`.
    pub fn set_suggestions(
        &mut self,
        word: &str,
        suggestions: Vec<SharedString>,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(open) = &mut self.open else {
            return;
        };
        let Some(spell) = open.spell.as_mut().filter(|s| s.word == word) else {
            return;
        };
        let before = open.menu.entries.len();
        let on_suggestion = open
            .item
            .is_some_and(|i| matches!(open.menu.entries.get(i), Some(Entry::Suggestion(_))));
        spell.suggestions = suggestions;
        open.rebuild();
        // Keep the highlight on the same command, or on the first
        // suggestion.
        let after = open.menu.entries.len();
        open.item = open.item.map(|i| {
            if on_suggestion {
                0
            } else {
                (i + after)
                    .saturating_sub(before)
                    .min(after.saturating_sub(1))
            }
        });
        cx.notify();
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
            spell: None,
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
                Entry::Suggestion(n) if open.suggestion_enabled(*n) => Some(i),
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
        if let Some(Entry::Suggestion(n)) = open.menu.entries.get(index).copied() {
            self.apply_suggestion(n, window, cx);
            return;
        }
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

    /// Replaces the word with suggestion `n` and closes the menu.
    fn apply_suggestion(&mut self, n: usize, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(open) = &self.open else {
            return;
        };
        if !open.suggestion_enabled(n) {
            return;
        }
        let Some(spell) = &open.spell else {
            return;
        };
        let (apply, text) = (Rc::clone(&spell.apply), spell.suggestions[n].clone());
        self.close(window, cx);
        window.defer(cx, move |window, cx| apply(&text, window, cx));
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
                    let checked = open.state.checks(&item);
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
                        .accessible()
                        .disabled(!enabled)
                        .into_any_element(),
                    )
                }
                Entry::Suggestion(n) => {
                    let title = open.spell.as_ref()?.suggestions.get(n)?.clone();
                    let enabled = open.suggestion_enabled(n);
                    Some(
                        suggestion_row(
                            index,
                            title,
                            enabled,
                            enabled && open.item == Some(index),
                            theme,
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
                        .accessible()
                        .disabled(!enabled)
                        .into_any_element(),
                    )
                }
                Entry::System(_) => None,
            })
            .collect();
        let menu = panel(theme)
            .id("context-menu")
            .a11y_group(Role::Menu, "Menu")
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
            .a11y_group(Role::Group, "Menu")
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

/// A suggestion's row: the replacement in bold, dimmed when the text
/// cannot be changed.
fn suggestion_row(
    index: usize,
    title: SharedString,
    enabled: bool,
    highlighted: bool,
    theme: &crate::theme::Theme,
) -> gpui::Stateful<gpui::Div> {
    let selector = format!("context-menu-suggestion:{title}");
    row(theme, highlighted)
        .id(ElementId::NamedInteger(
            "context-menu-suggestion".into(),
            index as u64,
        ))
        .a11y_menu_item(format!("Replace with {title}"), None)
        .when(highlighted, |row| row.aria_active_descendant())
        .debug_selector(move || selector.clone())
        .when(!enabled, |row| {
            row.opacity(crate::metrics::DISABLED_OPACITY)
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
}

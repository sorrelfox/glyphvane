// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's context menus: the tab menu, the More actions menu, the
//! context menu key, and the commands only these menus offer so far.
//!
//! - **Tab menu** (right-click on a tab; not designed): Close, Close
//!   others, Close to the right, Copy path, Reveal in file tree, Reveal in
//!   file manager. Its commands act on the tab that was clicked.
//! - **More actions** (the title row's `ellipsis` button; its menu is not
//!   designed): commands about the active document.
//! - **Context menu key** (Menu or Shift+F10, `workspace.context_menu`):
//!   the editor's menu at the caret in the Source view, the file tree's at
//!   the selected row. The Reading view has no link under the keyboard yet,
//!   so the key does nothing there.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, Div, Entity, EntityInputHandler as _,
    Focusable as _, InteractiveElement as _, Pixels, Stateful, Window, point,
};

use super::panels::SidebarPanel;
use super::{ViewMode, Workspace};
use crate::keymap::actions::{file as file_commands, tabs as tab_commands, workspace as commands};
use crate::menu::{
    ContextMenu, ContextMenuPopover, ContextTarget, MenuState, Placement, context_target,
};

/// The window's context menu and what it hangs from.
#[derive(Debug)]
pub(super) struct ShellMenus {
    pub popover: Entity<ContextMenuPopover>,
    /// The More actions button, in window coordinates.
    pub more_anchor: Rc<Cell<Bounds<Pixels>>>,
}

impl ShellMenus {
    pub fn new(window: &Window, cx: &mut App) -> Self {
        Self {
            popover: cx.new(|cx| ContextMenuPopover::new(window, cx)),
            more_anchor: Rc::default(),
        }
    }
}

impl Workspace {
    /// What the menus' conditions see for tab `index`.
    pub(super) fn menu_state(&self, index: usize, cx: &App) -> MenuState {
        MenuState {
            mode: self.mode(),
            can_go_back: self.can_go_back(),
            can_go_forward: self.can_go_forward(),
            edit_locked: !self.can_edit(self.active, cx),
            split_locked: !self.mode_available(ViewMode::Split, cx),
            has_file: self.tabs.get(index).is_some_and(|t| t.path.is_some()),
            misspelled: false,
            in_vault: false,
            live_preview: self.tabs.get(self.active).is_some_and(|t| t.live_preview),
            viewer_mode: self.viewer_mode_of(self.active, cx),
        }
    }

    /// The tab a command acts on: the one whose menu ran it, or the active
    /// tab.
    pub(super) fn command_tab(&self, cx: &App) -> usize {
        match context_target(cx) {
            ContextTarget::Tab(index) if index < self.tabs.len() => index,
            ContextTarget::Tab(_) | ContextTarget::Active | ContextTarget::Link => self.active,
        }
    }

    /// The context menu, open or not.
    pub fn context_menu(&self) -> &Entity<ContextMenuPopover> {
        &self.menus.popover
    }

    fn open_menu(
        &self,
        kind: ContextMenu,
        placement: Placement,
        target: ContextTarget,
        keyboard: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let index = match target {
            ContextTarget::Tab(index) => index,
            ContextTarget::Active | ContextTarget::Link => self.active,
        };
        let state = self.menu_state(index, cx);
        self.menus.popover.update(cx, |popover, cx| {
            popover.open(kind, placement, state, target, keyboard, window, cx);
        });
    }

    /// A right-click on tab `index`.
    pub(super) fn open_tab_menu(
        &self,
        index: usize,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.open_menu(
            ContextMenu::Tab,
            Placement::At(position),
            ContextTarget::Tab(index),
            false,
            window,
            cx,
        );
    }

    /// The More actions button: opens its menu below it, or closes it.
    pub(super) fn toggle_more_actions(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.menus.popover.read(cx).kind() == Some(ContextMenu::MoreActions) {
            self.menus
                .popover
                .update(cx, |popover, cx| popover.close(window, cx));
            return;
        }
        let anchor = self.menus.more_anchor.get();
        self.open_menu(
            ContextMenu::MoreActions,
            Placement::Below(anchor),
            ContextTarget::Active,
            false,
            window,
            cx,
        );
    }

    /// `workspace.context_menu`: the menu of what has the keyboard.
    fn context_menu_from_keys(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(files) = &self.files
            && files.read(cx).focus_handle(cx).contains_focused(window, cx)
        {
            let at = files
                .read(cx)
                .selected_row_bounds()
                .map_or_else(|| window.mouse_position(), |b| b.bottom_left());
            self.open_menu(
                ContextMenu::FileTree,
                Placement::At(at),
                ContextTarget::Active,
                true,
                window,
                cx,
            );
            return;
        }
        if let Some(editor) = self.shown_editor().cloned()
            && editor
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        {
            let caret = editor.update(cx, |e, cx| {
                let range = e.selected_text_range(false, window, cx)?.range;
                let start = range.start..range.start;
                e.bounds_for_range(start, Bounds::default(), window, cx)
            });
            let at = caret.map_or_else(|| window.mouse_position(), |b| point(b.left(), b.bottom()));
            // With the caret on a link: the link menu.
            if let Some(link) = editor.read(cx).link_at_caret() {
                crate::reader::link_info::set_menu_link(Some(link), cx);
                self.open_menu(
                    ContextMenu::Link,
                    Placement::At(at),
                    ContextTarget::Link,
                    true,
                    window,
                    cx,
                );
                return;
            }
            self.open_menu(
                ContextMenu::Editor,
                Placement::At(at),
                ContextTarget::Active,
                true,
                window,
                cx,
            );
            // With the caret in a misspelled word: its suggestions.
            editor.update(cx, |e, cx| e.attach_spell_menu(None, window, cx));
        }
    }

    /// Closes every tab but `keep`.
    pub(super) fn close_other_tabs(
        &mut self,
        keep: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if keep >= self.tabs.len() {
            return;
        }
        // From the end, so the indices left to close do not move.
        for index in (0..self.tabs.len()).rev().filter(|&i| i != keep) {
            self.close_tab(index, window, cx);
        }
    }

    /// Closes the tabs after `index`.
    pub(super) fn close_tabs_to_right(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        for i in (index + 1..self.tabs.len()).rev() {
            self.close_tab(i, window, cx);
        }
    }

    /// Copies tab `index`'s file path.
    fn copy_path(&self, index: usize, cx: &mut Context<'_, Self>) {
        if let Some(path) = self.tabs.get(index).and_then(|t| t.path.as_deref()) {
            cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        }
    }

    /// Shows tab `index` and its file selected in the Files panel, which
    /// takes the keyboard.
    pub(super) fn reveal_in_tree(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.tabs.get(index).is_none_or(|t| t.path.is_none()) {
            return;
        }
        self.activate_tab(index, window, cx);
        self.show_sidebar_panel(SidebarPanel::Files, true, window, cx);
        if let Some(files) = &self.files {
            files.update(cx, |f, cx| f.reveal(cx));
        }
    }

    /// The handlers of the commands above, on the window's root.
    pub(super) fn menu_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        root.on_action(
            cx.listener(|this, _: &tab_commands::CloseOthers, window, cx| {
                let keep = this.command_tab(cx);
                this.close_other_tabs(keep, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &tab_commands::CloseToRight, window, cx| {
                let index = this.command_tab(cx);
                this.close_tabs_to_right(index, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &file_commands::CopyPath, _, cx| {
            let index = this.command_tab(cx);
            this.copy_path(index, cx);
        }))
        .on_action(cx.listener(|this, _: &commands::RevealInTree, window, cx| {
            let index = this.command_tab(cx);
            this.reveal_in_tree(index, window, cx);
        }))
        .on_action(cx.listener(|this, _: &commands::ContextMenu, window, cx| {
            this.context_menu_from_keys(window, cx);
        }))
    }
}

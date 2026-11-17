// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Pane cycling and Go to heading in the Reading view.
//!
//! - **F6 / Shift+F6** (`workspace.focus_next_pane`,
//!   `workspace.focus_previous_pane`, NFR-A11Y-01) give the keyboard to the
//!   next or previous pane, left to right and around: the Files or Tags
//!   panel when the sidebar shows it, then the document (in Split mode the Source
//!   pane, then the preview). Panes that take no keyboard (the right panel,
//!   the sidebar's placeholder panels) are passed over. With nothing
//!   focused, F6 starts at the first pane and Shift+F6 at the last.
//! - **Go to heading** (Mod+Shift+O) in the Reading view opens the Go
//!   popover ([`crate::goto`]) with the document's headings; Enter jumps
//!   there as a click in the Outline does (and Back returns). The Source
//!   view handles the command itself while it has the keyboard.

use std::rc::Rc;

use gpui::{
    App, Context, Div, FocusHandle, Focusable as _, InteractiveElement as _, Stateful, Window,
};

use super::Workspace;
use super::panels::SidebarPanel;
use crate::goto::{self, Choice, GotoEvent, GotoItem, GotoKind, GotoRequest};
use crate::keymap::actions::navigation::GoToHeading;
use crate::keymap::actions::workspace::{FocusNextPane, FocusPreviousPane};

impl Workspace {
    /// Registers pane cycling and Go to heading for the Reading view.
    pub(super) fn pane_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let root = root
            .on_action(cx.listener(|this, _: &FocusNextPane, window, cx| {
                this.cycle_pane(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousPane, window, cx| {
                this.cycle_pane(false, window, cx);
            }));
        if self.active_doc().is_none() {
            return root;
        }
        root.on_action(cx.listener(|this, _: &GoToHeading, window, cx| {
            this.pick_heading(window, cx);
        }))
    }

    /// The panes that take the keyboard, left to right.
    pub(crate) fn panes(&self, cx: &App) -> Vec<FocusHandle> {
        let mut panes = Vec::new();
        if self.sidebar_open
            && self.panels.sidebar == SidebarPanel::Files
            && let Some(files) = &self.files
        {
            panes.push(files.read(cx).focus_handle(cx));
        }
        if self.sidebar_open
            && self.panels.sidebar == SidebarPanel::Tags
            && let Some(tags) = &self.panels.tags
        {
            panes.push(tags.read(cx).focus_handle(cx));
        }
        if let Some(reader) = self.shown_reader() {
            panes.push(reader.read(cx).focus_handle(cx));
        }
        if let Some(editor) = self.shown_editor() {
            panes.push(editor.read(cx).focus_handle(cx));
        }
        if let Some(reader) = self.shown_split_reader(cx) {
            panes.push(reader.read(cx).focus_handle(cx));
        }
        panes
    }

    /// F6 (`forward`) or Shift+F6.
    fn cycle_pane(&mut self, forward: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        let panes = self.panes(cx);
        if panes.is_empty() {
            return;
        }
        let n = panes.len();
        let current = panes
            .iter()
            .position(|pane| pane.contains_focused(window, cx));
        let next = match (current, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, true) => 0,
            (None, false) => n - 1,
        };
        panes[next].focus(window, cx);
        cx.notify();
    }

    /// Go to heading in the Reading view.
    fn pick_heading(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.active_doc() else {
            return;
        };
        let headings = doc.model.headings();
        let top = headings.iter().map(|h| h.level).min().unwrap_or(1);
        let items: Vec<GotoItem> = headings
            .iter()
            .map(|h| GotoItem {
                label: h.text.to_string().into(),
                detail: Some(format!("H{}", h.level).into()),
                depth: h.level.saturating_sub(top),
            })
            .collect();
        let initial = doc.heading.unwrap_or(0);
        let view = cx.entity().downgrade();
        let on_event = Rc::new(move |event: GotoEvent, window: &mut Window, cx: &mut App| {
            if let GotoEvent::Confirm(Choice::Item(i)) = event {
                view.update(cx, |this, cx| this.jump_to_heading(i, window, cx))
                    .ok();
            }
        });
        goto::open(
            GotoRequest {
                title: "Go to heading".into(),
                kind: GotoKind::List {
                    placeholder: "Go to heading".into(),
                    action: "go".into(),
                    empty: "No headings".into(),
                    items,
                    initial,
                },
                on_event,
            },
            window,
            cx,
        );
    }
}

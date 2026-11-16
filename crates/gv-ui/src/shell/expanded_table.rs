// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The expanded table over the window's content area
//! ([`ExpandedTable`]): opened by a table's Expand button in a tab's
//! Reading view or Split preview, closed by Esc, its close button, a link
//! followed from it, or the tab no longer showing that view. Closing gives
//! the keyboard back to the view, which has not moved.

use std::path::{Path, PathBuf};

use gpui::{AnyElement, AppContext as _, Context, Entity, IntoElement as _, Subscription, Window};

use super::{ViewMode, Workspace};
use crate::reader::{ExpandRequest, ExpandedEvent, ExpandedTable, Reader, ReaderEvent};

/// The open expanded table.
pub(super) struct Expanded {
    view: Entity<ExpandedTable>,
    /// The Reading view it was opened from, and its document.
    source: Entity<Reader>,
    path: PathBuf,
    _events: Subscription,
}

impl Workspace {
    /// Shows the table of `request`, from the Reading view `source`, over
    /// the content area, and gives it the keyboard.
    pub(super) fn expand_table(
        &mut self,
        source: Entity<Reader>,
        request: &ExpandRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let (path, profile) = {
            let r = source.read(cx);
            let Some(path) = r.path().map(Path::to_path_buf) else {
                return;
            };
            (path, r.compiled_profile())
        };
        let index = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let view = {
            let path = path.clone();
            cx.new(|cx| ExpandedTable::new(request, path, profile, index, cx))
        };
        let events = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ExpandedEvent, window, cx| match event {
                ExpandedEvent::Close => {
                    this.close_expanded(window, cx);
                }
                ExpandedEvent::FollowLink(request) => {
                    // Followed from the document, as a click there would.
                    if let Some(source) = this.close_expanded(window, cx) {
                        let request = request.clone();
                        source.update(cx, |_, cx| cx.emit(ReaderEvent::FollowLink(request)));
                    }
                }
            },
        );
        let focus = view.read(cx).table_focus(cx);
        focus.focus(window, cx);
        self.expanded = Some(Expanded {
            view,
            source,
            path,
            _events: events,
        });
        cx.notify();
    }

    /// Closes the expanded table and gives the keyboard back to the view
    /// it was opened from (returned).
    pub(super) fn close_expanded(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<Entity<Reader>> {
        let expanded = self.expanded.take()?;
        let focus = gpui::Focusable::focus_handle(expanded.source.read(cx), cx);
        focus.focus(window, cx);
        cx.notify();
        Some(expanded.source)
    }

    /// The expanded table, while the active tab still shows the view and
    /// the document it was opened from; dropped otherwise.
    pub(super) fn render_expanded(&mut self, cx: &mut Context<'_, Self>) -> Option<AnyElement> {
        let expanded = self.expanded.as_ref()?;
        let tab = self.active_tab();
        let shown = match tab.mode() {
            ViewMode::Preview => tab.reader.as_ref() == Some(&expanded.source),
            ViewMode::Split => tab.split_reader(cx).as_ref() == Some(&expanded.source),
            ViewMode::Edit => false,
        } && expanded.source.read(cx).path() == Some(expanded.path.as_path());
        if !shown {
            self.expanded = None;
            return None;
        }
        Some(expanded.view.clone().into_any_element())
    }

    /// The expanded table's view, while it is open.
    pub fn expanded_table(&self) -> Option<&Entity<ExpandedTable>> {
        self.expanded.as_ref().map(|e| &e.view)
    }
}

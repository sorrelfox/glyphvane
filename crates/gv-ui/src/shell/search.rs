// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of the Search panel ([`crate::search_panel`]): it is
//! made when first shown, searches the window's vault, and opens what it
//! asks for.
//!
//! `search.vault` (Mod+Shift+F) shows the panel with the keyboard in its
//! field, filled with the selection of the document on screen when that is
//! one line, or with the tag a click in the Reading view asked for. A
//! tag of the Tags panel shows it with that tag's search too.

use gpui::{AppContext as _, Context, Div, Entity, InteractiveElement as _, Stateful, Window};

use super::panels::SidebarPanel;
use super::{OpenTarget, Workspace};
use crate::keymap::actions::search as commands;
use crate::reader::{Disposition, ScrollTarget};
use crate::search_panel::{SearchPanel, SearchPanelEvent, SearchScope};

impl Workspace {
    /// The Search panel, made on first use.
    pub(super) fn ensure_search_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<SearchPanel> {
        if let Some(search) = &self.panels.search {
            return search.clone();
        }
        let search = cx.new(|cx| SearchPanel::new(window, cx));
        self.panels._search_events = Some(cx.subscribe_in(
            &search,
            window,
            |this, _, event: &SearchPanelEvent, window, cx| {
                this.search_event(event.clone(), window, cx);
            },
        ));
        self.panels.search = Some(search.clone());
        self.ensure_vault(cx);
        self.sync_search(cx);
        search
    }

    /// The panel searches the window's vault, with its latest content
    /// index.
    pub(super) fn sync_search(&mut self, cx: &mut Context<'_, Self>) {
        let Some(search) = self.panels.search.clone() else {
            return;
        };
        let scope = self.vault.as_ref().map(|v| {
            let v = v.read(cx);
            SearchScope {
                root: v.root().to_path_buf(),
                content: v.content(),
            }
        });
        search.update(cx, |s, cx| s.set_scope(scope, cx));
    }

    fn search_event(
        &mut self,
        event: SearchPanelEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            SearchPanelEvent::Open {
                path,
                offset,
                disposition,
            } => {
                let scroll = offset.map(ScrollTarget::Offset);
                match disposition {
                    Disposition::InPlace => {
                        let shown = self
                            .tabs
                            .iter()
                            .position(|t| t.path.as_deref() == Some(path.as_path()));
                        let index = shown.unwrap_or(self.active);
                        self.activate_tab(index, window, cx);
                        self.navigate(index, path, scroll, window, cx);
                    }
                    Disposition::NewTab | Disposition::Background => {
                        let scroll = scroll.or(Some(ScrollTarget::Offset(0)));
                        let activate = disposition == Disposition::NewTab;
                        self.insert_tab(OpenTarget::new(path), scroll, activate, window, cx);
                    }
                    Disposition::NewWindow => {
                        self.open_link_target(self.active, path, scroll, disposition, window, cx);
                    }
                }
            }
            SearchPanelEvent::Dismiss => self.focus_document(window, cx),
        }
    }

    /// The one-line selection of the document on screen, or the tag a
    /// click asked for.
    fn search_seed(&self, cx: &mut Context<'_, Self>) -> Option<String> {
        let tab = self.active_tab();
        let readers = [tab.reader.clone(), tab.split_reader(cx)];
        // A tag click asks once; take it from every view so none lingers.
        let mut tag = None;
        for reader in readers.iter().flatten() {
            tag = reader.read(cx).take_tag_search().or(tag);
        }
        if let Some(editor) = self.shown_editor() {
            tag = editor.read(cx).take_tag_search().or(tag);
        }
        if let Some(tag) = tag {
            return Some(tag.to_string());
        }
        let selected = match (self.shown_reader(), self.shown_editor()) {
            (Some(reader), _) => reader.read(cx).selected_text(),
            (None, Some(editor)) => editor.read(cx).editor().and_then(|e| {
                if e.selections().len() != 1 {
                    return None;
                }
                let range = e.selections().primary().range();
                e.text().text_for_range(range).ok()
            }),
            (None, None) => None,
        };
        selected
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && !s.contains(['\n', '\r']))
    }

    /// `search.vault`: the Search panel with the keyboard in its field.
    pub(super) fn search_vault(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let seed = self.search_seed(cx);
        self.search_with(seed, window, cx);
    }

    /// The Search panel with the keyboard in its field, running `seed`
    /// when given (a tag's `tag:#name`).
    pub(super) fn search_with(
        &mut self,
        seed: Option<String>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.show_sidebar_panel(SidebarPanel::Search, true, window, cx);
        let search = self.ensure_search_panel(window, cx);
        search.update(cx, |s, cx| {
            if let Some(seed) = &seed {
                s.set_query(seed, window, cx);
            }
            s.focus_field(window, cx);
        });
    }

    /// The Search panel's query, once the panel exists.
    #[cfg(test)]
    pub(crate) fn search_query(&self, cx: &gpui::App) -> Option<String> {
        Some(self.panels.search.as_ref()?.read(cx).query(cx))
    }

    pub(super) fn search_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, _: &commands::Vault, window, cx| {
            this.search_vault(window, cx);
        }))
    }
}

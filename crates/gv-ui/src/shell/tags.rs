// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The window's side of the Tags panel ([`crate::tags_panel`]): it is made
//! when first shown, follows the vault's content index, and runs the
//! search a tag asks for in the Search panel.

use std::rc::Rc;

use gpui::{AppContext as _, Context, Entity, SharedString, Window};

use super::Workspace;
use super::vault::VaultStatus;
use crate::goto::{self, Choice, GotoEvent, GotoItem, GotoKind, GotoRequest};
use crate::tags_panel::model::TagSort;
use crate::tags_panel::{TagsPanel, TagsPanelEvent, TagsSource};

impl Workspace {
    /// The Tags panel, made on first use (with the window's vault).
    pub(super) fn ensure_tags_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<TagsPanel> {
        if let Some(tags) = &self.panels.tags {
            return tags.clone();
        }
        let tags = cx.new(TagsPanel::new);
        self.panels._tags_events = Some(cx.subscribe_in(
            &tags,
            window,
            |this, _, event: &TagsPanelEvent, window, cx| match event {
                TagsPanelEvent::Search(query) => {
                    this.search_with(Some(query.clone()), window, cx);
                }
                TagsPanelEvent::Sort => this.show_tag_orders(window, cx),
            },
        ));
        self.panels.tags = Some(tags.clone());
        self.ensure_vault(cx);
        self.sync_tags(cx);
        tags
    }

    /// The panel shows the tags of the vault's latest content snapshot.
    pub(super) fn sync_tags(&mut self, cx: &mut Context<'_, Self>) {
        let Some(tags) = self.panels.tags.clone() else {
            return;
        };
        let source = match self.vault.as_ref().map(|v| v.read(cx)) {
            None => TagsSource::NoVault,
            Some(v) => match v.status() {
                VaultStatus::Building => TagsSource::Indexing,
                VaultStatus::NotIndexed(why) | VaultStatus::Failed(why) => {
                    TagsSource::NotIndexed(why.clone())
                }
                VaultStatus::Ready => v.content().map_or(TagsSource::Indexing, TagsSource::Ready),
            },
        };
        tags.update(cx, |t, cx| t.set_source(source, cx));
    }

    /// The Tags panel's Sort button: the orders in the Go popover.
    fn show_tag_orders(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(tags) = self.panels.tags.clone() else {
            return;
        };
        let current = tags.read(cx).sort();
        let items = TagSort::ALL
            .iter()
            .map(|order| GotoItem {
                label: order.label().into(),
                detail: (*order == current).then(|| SharedString::from("current")),
                depth: 0,
            })
            .collect();
        let initial = TagSort::ALL
            .iter()
            .position(|o| *o == current)
            .unwrap_or_default();
        let request = GotoRequest {
            title: "Sort tags".into(),
            kind: GotoKind::List {
                placeholder: "Sort order".into(),
                action: "sort".into(),
                empty: "No such order".into(),
                items,
                initial,
            },
            on_event: Rc::new(move |event, _, cx| {
                if let GotoEvent::Confirm(Choice::Item(i)) = event
                    && let Some(order) = TagSort::ALL.get(i)
                {
                    tags.update(cx, |t, cx| t.set_sort(*order, cx));
                }
            }),
        };
        window.defer(cx, move |window, cx| {
            goto::open(request, window, cx);
        });
    }

    /// The Tags panel, once made (tests).
    #[cfg(test)]
    pub(crate) fn tags_panel(&self) -> Option<Entity<TagsPanel>> {
        self.panels.tags.clone()
    }
}

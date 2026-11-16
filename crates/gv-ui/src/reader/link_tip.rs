// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The tooltip of the link under the pointer in the Reading view: after
//! [`TOOLTIP_DELAY`] on a link whose status needs explaining (unresolved,
//! ambiguous with its candidates, case-only or loose matches, pending,
//! outside the root, blocked). A table's Expand button gets one too.

use gpui::{AnyElement, Context, Task, Window};
use gv_render_tree::LinkStatus;

use super::Reader;
use super::link_info::{TOOLTIP_DELAY, link_name, status_tooltip, tooltip};
use super::links::{Resolver, resolver_kind};
use super::state::{ButtonKind, Hover};

/// What the Expand button's tooltip says.
pub(super) const EXPAND_TIP: &str = "Expand table";

/// What the pointer rests on that has a tooltip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TipKey {
    /// A link: the item and link index.
    Link(usize, u32),
    /// The Expand button of the table of a block.
    Expand(gv_syntax::BlockId),
}

/// Which link the pointer rests on, and whether its tooltip is due.
#[derive(Default)]
pub(super) struct LinkTip {
    key: Option<TipKey>,
    shown: bool,
    _timer: Option<Task<()>>,
}

impl Reader {
    /// The document's text at `range`, when it is loaded.
    pub(crate) fn source_text(&self, range: std::ops::Range<usize>) -> Option<std::sync::Arc<str>> {
        let st = self.state.borrow();
        st.doc.as_ref()?.text.get(range).map(std::sync::Arc::from)
    }

    /// The status and tooltip text of the link under the pointer, if its
    /// status needs explaining.
    pub(crate) fn hovered_link_tooltip(&self) -> Option<(LinkStatus, String)> {
        let st = self.state.borrow();
        let Hover::Link(link) = &st.hover else {
            return None;
        };
        let doc = st.doc.as_ref()?;
        let status = doc
            .tree
            .item_inline(link.item)?
            .links
            .get(link.link as usize)?
            .status;
        if matches!(status, LinkStatus::Exact | LinkStatus::External) {
            return None;
        }
        let path = self.path.as_deref()?;
        let resolution = Resolver::for_click(path, self.index.as_ref(), self.profile())
            .resolve(resolver_kind(link.kind, false), &link.destination);
        let name = link_name(link.kind, &link.destination);
        let text = status_tooltip(status, &name, Some(&resolution), "click")?;
        Some((status, text))
    }

    /// The tooltip to draw: started on a new link, shown once the delay
    /// has passed.
    pub(super) fn link_tooltip(
        &mut self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        let key = match &self.state.borrow().hover {
            Hover::Link(link) => Some(TipKey::Link(link.item, link.link)),
            Hover::Button(b) if b.kind == ButtonKind::Expand => Some(TipKey::Expand(b.block)),
            _ => None,
        };
        if key != self.link_tip.key {
            self.link_tip = LinkTip {
                key,
                shown: false,
                _timer: key.map(|_| {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(TOOLTIP_DELAY).await;
                        let _ = this.update(cx, |r, cx| {
                            r.link_tip.shown = true;
                            cx.notify();
                        });
                    })
                }),
            };
            return None;
        }
        if !self.link_tip.shown {
            return None;
        }
        let text = match key? {
            TipKey::Link(..) => self.hovered_link_tooltip()?.1,
            TipKey::Expand(_) => EXPAND_TIP.to_owned(),
        };
        Some(tooltip(text, window.mouse_position(), cx))
    }
}

#[cfg(test)]
impl Reader {
    /// Every link of the document with its status, in order.
    pub(crate) fn link_statuses_for_tests(&self) -> Vec<(String, LinkStatus)> {
        let st = self.state.borrow();
        let Some(doc) = st.doc.as_ref() else {
            return Vec::new();
        };
        (0..doc.tree.items().len())
            .filter_map(|ix| doc.tree.item_inline(ix))
            .flat_map(|c| {
                c.links
                    .iter()
                    .map(|l| (l.destination.to_string(), l.status))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The middle of the first link's text in the item that holds
    /// `needle`, once painted.
    pub(crate) fn link_point_for_tests(&self, needle: &str) -> Option<gpui::Point<gpui::Pixels>> {
        let st = self.state.borrow();
        let doc = st.doc.as_ref()?;
        let offset = doc.text.find(needle)?;
        for g in &st.geometry.items {
            let node = doc.tree.node_of(g.ix)?;
            if !(node.source.start <= offset && offset < node.source.end) {
                continue;
            }
            let content = node.inline()?;
            let piece = content
                .spans
                .iter()
                .position(|s| s.link.is_some() && s.role == gv_render_tree::InlineRole::Text)?;
            let block = g.text.as_ref()?.borrow();
            let layout = block.layout()?;
            let origin = block.bounds()?.origin;
            let r = layout.item_rects(piece)[0];
            return Some(
                origin
                    + gpui::point(
                        gpui::px(r.x + r.width / 2.0),
                        gpui::px(r.y + r.height / 2.0),
                    ),
            );
        }
        None
    }
}

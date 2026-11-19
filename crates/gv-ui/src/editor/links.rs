// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Links in the Source view (09 § Interaction model):
//!
//! - **Mod+Click** on a link follows it (in place); a middle click opens
//!   it in a background tab. A plain click never navigates. The view
//!   records the link and asks the window ([`DocEvent::FollowLink`]).
//! - **Right-click** on a link opens the link menu instead of the
//!   editor's, about that link; the context menu key does the same with
//!   the caret on a link.
//! - The `links.*` commands act on the link under the caret
//!   ([`EditorView::link_at_caret`]) when not run from the link menu.
//! - A link of an embedded note in a Live Preview block is not in the
//!   text: Mod+click and the link menu take it from the block's view,
//!   resolved from the embedded note.
//! - **Tooltip:** resting the pointer on a link whose status needs
//!   explaining shows what it means, after [`TOOLTIP_DELAY`].
//! - **Mod+hover:** the window opens the link's hover preview when the
//!   pointer rests on it with Mod held ([`EditorView::take_hover_preview`]);
//!   the tooltip stays away while Mod is held.

use std::sync::Arc;

use gpui::{AnyElement, Context, Pixels, Point, Task, Window};
use gv_render_tree::LinkStatus;

use super::{DocEvent, EditorView};
use crate::reader::Disposition;
use crate::reader::link_info::{
    LinkAt, SourceLink, TOOLTIP_DELAY, link_name, source_link_at, status_tooltip, tooltip,
};

/// The link under the pointer and whether its tooltip is due.
#[derive(Default)]
pub(super) struct LinkHover {
    link: Option<SourceLink>,
    /// Where the pointer came to rest on it.
    at: Option<Point<Pixels>>,
    /// Mod was held at the pointer's last move.
    modded: bool,
    /// The window took the preview of this rest.
    taken: bool,
    shown: bool,
    _timer: Option<Task<()>>,
}

/// How a click follows a link in the Source view, for tooltips.
fn follow_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd+click"
    } else {
        "Ctrl+click"
    }
}

impl EditorView {
    /// The link at byte `offset` of the text, once the parse is current.
    pub(crate) fn link_at_offset(&self, offset: usize) -> Option<SourceLink> {
        // Plain text has no links.
        let doc = self.doc.as_ref().filter(|_| self.plain.is_none())?;
        let (tree, text) = doc.syntax.shown();
        if text.len() != doc.editor.text().len() || !doc.syntax.is_current(doc.editor.text()) {
            return None;
        }
        source_link_at(tree, text, offset)
    }

    /// The link the primary caret is on (an empty selection).
    pub(crate) fn link_at_caret(&self) -> Option<LinkAt> {
        let doc = self.doc.as_ref()?;
        let primary = doc.editor.selections().primary();
        if primary.head() != primary.tail() {
            return None;
        }
        let link = self.link_at_offset(primary.head())?;
        self.link_of(link)
    }

    /// The link under window point `position`.
    pub(crate) fn link_at_point(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<SourceLink> {
        // A Live Preview block: the link its view shows there, in the
        // block's source.
        if let Some((id, reader)) = self.block_view_at(position) {
            let start = reader.read(cx).link_start_at(position)?;
            let offset = self.block_source_start(id)?
                + start.checked_sub(self.live.blocks.prefix_len(id))?;
            return self.link_at_offset(offset);
        }
        let offset = self.offset_at(position, window, cx)?;
        self.link_at_offset(offset)
    }

    /// A link of an embedded note under window point `position`, in a
    /// Live Preview block: not in this document's text, so it is followed
    /// from the embedded note.
    fn embedded_link_at(&self, position: Point<Pixels>, cx: &gpui::App) -> Option<LinkAt> {
        let (_, reader) = self.block_view_at(position)?;
        reader.read(cx).embedded_link_at(position)
    }

    /// `link` of this document, for the window.
    pub(crate) fn link_of(&self, link: SourceLink) -> Option<LinkAt> {
        let file = self.path.clone()?;
        let construct = self
            .doc
            .as_ref()?
            .editor
            .text()
            .text_for_range(link.range.clone())
            .ok()
            .map(Arc::<str>::from);
        Some(LinkAt {
            file,
            kind: link.kind,
            image: link.image,
            destination: link.destination.into(),
            range: Some(link.range),
            construct,
            profile: self.links.profile(),
        })
    }

    /// Mod+click or a middle click on a link: the window follows it.
    /// Returns whether there was a link.
    pub(super) fn click_link(
        &mut self,
        position: Point<Pixels>,
        disposition: Disposition,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(link) = self.embedded_link_at(position, cx).or_else(|| {
            self.link_at_point(position, window, cx)
                .and_then(|l| self.link_of(l))
        }) else {
            return false;
        };
        self.clicked_link = Some(link);
        cx.emit(DocEvent::FollowLink(disposition));
        true
    }

    /// The link a click asked the window to follow.
    pub fn take_clicked_link(&mut self) -> Option<LinkAt> {
        self.clicked_link.take()
    }

    /// A right-click: on a link, records it for the link menu.
    pub(super) fn menu_link_at(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<LinkAt> {
        self.embedded_link_at(position, cx).or_else(|| {
            self.link_at_point(position, window, cx)
                .and_then(|l| self.link_of(l))
        })
    }

    /// The pointer moved (no button down): the link under it, for its
    /// tooltip.
    pub(super) fn hover_moved(
        &mut self,
        position: Option<Point<Pixels>>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) {
        let link = position.and_then(|p| self.link_at_point(p, window, cx));
        let modded = window.modifiers().secondary(cx);
        if link == self.link_hover.link {
            if modded != self.link_hover.modded {
                self.link_hover.modded = modded;
                // Mod pressed again: another preview.
                self.link_hover.taken &= !modded;
                cx.notify();
            }
            return;
        }
        self.link_hover = LinkHover {
            _timer: link.as_ref().map(|_| {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(TOOLTIP_DELAY).await;
                    let _ = this.update(cx, |e, cx| {
                        e.link_hover.shown = true;
                        cx.notify();
                    });
                })
            }),
            at: link.as_ref().and(position),
            modded,
            taken: false,
            link,
            shown: false,
        };
        cx.notify();
    }

    /// The link the pointer rests on (for the tooltip delay) with Mod
    /// held, and where it came to rest: Mod+hover previews it, once per
    /// rest.
    pub(crate) fn take_hover_preview(&mut self) -> Option<(LinkAt, Point<Pixels>)> {
        let hover = &self.link_hover;
        if !hover.shown || !hover.modded || hover.taken {
            return None;
        }
        let link = self.link_of(hover.link.clone()?)?;
        let at = hover.at?;
        self.link_hover.taken = true;
        Some((link, at))
    }

    /// The link under the pointer, as the window's commands take it.
    pub(crate) fn hovered_link(&self) -> Option<LinkAt> {
        self.link_of(self.link_hover.link.clone()?)
    }

    /// The status of the link under the pointer and its tooltip text, if
    /// the status needs explaining.
    pub(crate) fn hovered_link_tooltip(&mut self) -> Option<(LinkStatus, String)> {
        let link = self.link_hover.link.clone()?;
        let status = self
            .links
            .status_of(link.kind, link.image, &link.destination);
        if matches!(status, LinkStatus::Exact | LinkStatus::External) {
            return None;
        }
        let resolution = self
            .links
            .resolution(link.kind, link.image, &link.destination);
        let name = link_name(link.kind, &link.destination);
        let text = status_tooltip(status, &name, resolution.as_ref(), follow_hint())?;
        Some((status, text))
    }

    /// The tooltip to draw, once the delay has passed.
    pub(super) fn link_tooltip(
        &mut self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        // Mod+hover shows the hover preview instead.
        if !self.link_hover.shown || window.modifiers().secondary(cx) {
            return None;
        }
        let (_, text) = self.hovered_link_tooltip()?;
        Some(tooltip(text, window.mouse_position(), cx))
    }
}

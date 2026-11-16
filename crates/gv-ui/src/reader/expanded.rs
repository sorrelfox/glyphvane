// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The expanded table: a table whose cells wrap or that scrolls sideways,
//! shown on its own over the window's content area (below the tab bar,
//! above the status bar) after a click on its Expand button.
//!
//! - It is a second Reading view ([`Reader::fill_view`]) of the table's
//!   Markdown alone, as the document at the original path: the table is
//!   fitted to the whole view with the usual rules whatever the readable
//!   line width, scrolls both ways with visible scrollbars, and selection,
//!   copy, find and link statuses work as in the document. Relative links
//!   and images resolve against the original document.
//! - A link clicked in it is followed from the document (the view closes).
//! - Esc or the header's close button closes it; the document has not
//!   moved.
//! - Not designed: a panel head with the title "Table", the heading the
//!   table is under (or the file name) and a close button.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    AppContext as _, Context, EventEmitter, FontWeight, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use gv_profile::CompiledProfile;
use gv_vault::PathIndex;

use super::{ExpandRequest, LinkRequest, Reader, ReaderEvent};
use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::{PANEL_HEADER, SMALL_ICON_BUTTON};
use crate::metrics::spacing::panel_head;
use crate::metrics::{HAIRLINE, icon, radius, text};

gpui::actions!(
    expanded_table,
    [
        /// Close the expanded table.
        Close,
    ]
);

/// The key context of the expanded table.
pub(crate) const KEY_CONTEXT: &str = "ExpandedTable";

/// Esc closes the expanded table (after the find popover, when it is
/// open: its own Esc comes first).
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("escape", Close, Some(KEY_CONTEXT))]
}

/// What the expanded table asks of its window.
#[derive(Debug, Clone, PartialEq)]
pub enum ExpandedEvent {
    /// Esc or the close button.
    Close,
    /// A link clicked in the table, to follow from the document.
    FollowLink(LinkRequest),
}

/// A table shown on its own over the window's content area.
pub struct ExpandedTable {
    reader: gpui::Entity<Reader>,
    title: SharedString,
    _events: Subscription,
}

impl std::fmt::Debug for ExpandedTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpandedTable")
            .field("title", &self.title)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ExpandedEvent> for ExpandedTable {}

impl ExpandedTable {
    /// The table of `request`, read with `profile` as the document at
    /// `path`, its links' statuses from `index`.
    pub(crate) fn new(
        request: &ExpandRequest,
        path: PathBuf,
        profile: Arc<CompiledProfile>,
        index: Option<PathIndex>,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let markdown: Arc<str> = format!("{}\n", request.markdown.trim_end()).into();
        let reader = cx.new(|cx| {
            let mut reader = Reader::new(cx);
            reader.fill_view(cx);
            reader.set_index(index, cx);
            reader.set_profile(profile, cx);
            reader.open_text(path, markdown, None, cx);
            reader
        });
        let events = cx.subscribe(&reader, |_, _, event: &ReaderEvent, cx| {
            if let ReaderEvent::FollowLink(request) = event {
                cx.emit(ExpandedEvent::FollowLink(request.clone()));
            }
        });
        Self {
            reader,
            title: request.title.clone(),
            _events: events,
        }
    }

    /// The view's Reading view.
    pub fn reader(&self) -> &gpui::Entity<Reader> {
        &self.reader
    }

    /// What takes the keyboard in it: its Reading view.
    pub(crate) fn table_focus(&self, cx: &gpui::App) -> gpui::FocusHandle {
        gpui::Focusable::focus_handle(self.reader.read(cx), cx)
    }
}

impl Render for ExpandedTable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let (hover, press) = (theme.state.hover, theme.state.press);
        let close = div()
            .id("expanded-table-close")
            .debug_selector(|| "expanded-table-close".into())
            .flex_none()
            .size(px(SMALL_ICON_BUTTON))
            .rounded(px(radius::CONTROL))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.text.secondary)
            .hover(move |s| s.bg(hover))
            .active(move |s| s.bg(press))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ExpandedEvent::Close)))
            .child(Icon::Dismiss.sized(icon::ROW));
        let head = div()
            .h(px(PANEL_HEADER))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(panel_head::GAP))
            .pl(px(panel_head::PAD_LEFT))
            .pr(px(panel_head::PAD_RIGHT))
            .border_b(px(HAIRLINE))
            .border_color(theme.border.default)
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight(text::weight::STRONG))
                    .child("Table"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .pl(px(panel_head::TITLE_GAP))
                    .text_color(theme.text.secondary)
                    .child(self.title.clone()),
            )
            .child(close);
        div()
            .id("expanded-table")
            .debug_selector(|| "expanded-table".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|_, _: &Close, _, cx| cx.emit(ExpandedEvent::Close)))
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .bg(theme.surface.page)
            // Nothing under it takes the pointer.
            .occlude()
            .child(head)
            .child(div().flex_1().min_h_0().child(self.reader.clone()))
    }
}

/// The Markdown of the table at `source` in `text` on its own: its lines
/// without the prefixes of the quotes and list items around it (as many
/// leading blanks and `>` as precede the table on its first line).
pub(crate) fn table_markdown(text: &str, source: Range<usize>) -> String {
    let start = source.start.min(text.len());
    let end = source.end.clamp(start, text.len());
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let width = text[line_start..start].chars().count();
    let mut out = String::with_capacity(end - start);
    for (k, line) in text[start..end].split_inclusive('\n').enumerate() {
        out.push_str(if k == 0 {
            line
        } else {
            strip_prefix(line, width)
        });
    }
    out
}

/// `line` without up to `width` leading blanks and `>` markers.
fn strip_prefix(line: &str, width: usize) -> &str {
    let cut = line
        .char_indices()
        .take(width)
        .take_while(|(_, c)| matches!(c, ' ' | '\t' | '>'))
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    &line[cut..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, needle: &str) -> Range<usize> {
        let start = text.find(needle).expect("the table");
        let end = text[start..].find("\n\n").map_or(text.len(), |e| start + e);
        start..end
    }

    #[test]
    fn a_top_level_table_is_taken_as_written() {
        let text = "# T\n\n| a | b |\n|---|:-:|\n| x | y |\n\nafter\n";
        assert_eq!(
            table_markdown(text, at(text, "| a")),
            "| a | b |\n|---|:-:|\n| x | y |"
        );
    }

    #[test]
    fn quote_and_list_prefixes_are_dropped() {
        let text = "> | a | b |\n> |---|---|\n> | x | y |\n";
        assert_eq!(
            table_markdown(text, at(text, "| a")),
            "| a | b |\n|---|---|\n| x | y |\n"
        );
        let text = "- item\n\n  | a | b |\n  |---|---|\n  | `>` | y |\n";
        assert_eq!(
            table_markdown(text, at(text, "| a")),
            "| a | b |\n|---|---|\n| `>` | y |\n"
        );
        // A lazy line without the prefix keeps its text.
        let text = "> | a |\n> |---|\n| x |\n";
        assert_eq!(
            table_markdown(text, at(text, "| a")),
            "| a |\n|---|\n| x |\n"
        );
    }
}

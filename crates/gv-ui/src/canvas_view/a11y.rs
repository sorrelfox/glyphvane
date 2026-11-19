// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What assistive technology is told about a canvas.
//!
//! The cards are drawn only where the view shows them, so the canvas
//! also keeps an invisible list of every card (groups are not cards) in
//! reading order: each item is named after its card (kind and title),
//! its description says which group holds it, which cards it links to
//! and which link to it (with the edges' labels), and its problems. The
//! selected card is the list's selected item and the active descendant
//! of the focused canvas, so a screen reader announces it as the focus
//! moves (Tab, the arrows). A click action on an item selects and opens
//! its card.
//!
//! The cards are list items rather than articles or groups: AccessKit's
//! list items carry their place in the list ("3 of 40"), screen readers
//! move through a list item by item, and the reading order the keyboard
//! follows is the list's order.
//!
//! The list is built only while assistive technology is listening. A
//! canvas with more than [`MAX_LISTED`] cards lists a window of them in
//! reading order around the selected card (or, with none, around the
//! first card in view), so a large canvas costs a bounded amount per
//! frame; each item still says its place among all the cards. The edges
//! are indexed once per frame, not once per card.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
};
use std::ops::Range;

use gv_canvas::{ItemId, NodeKind};

use super::{CanvasView, Doc};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::reader::Disposition;

/// The most cards the list names; a larger canvas lists a window of
/// them ([`listed_range`]).
pub(crate) const MAX_LISTED: usize = 500;
/// The longest title taken from a text card.
const MAX_TITLE: usize = 80;

/// A short title of node `node`: a text card's first line, a file's
/// name, a link's URL, a group's label.
pub(crate) fn title(doc: &Doc, node: usize) -> String {
    let n = &doc.canvas.nodes()[node];
    match &n.kind {
        NodeKind::Text { text } => first_line(text),
        NodeKind::File {
            file, subpath_raw, ..
        } => {
            let name = file.rsplit('/').next().unwrap_or(file);
            format!("{name}{}", subpath_raw.as_deref().unwrap_or(""))
        }
        NodeKind::Link { url } => url.clone(),
        NodeKind::Group { label, .. } => label.clone().unwrap_or_default(),
        NodeKind::Unknown { type_name } => type_name.clone(),
    }
}

/// The accessible name of card `node`: its kind and title.
pub(crate) fn card_name(doc: &Doc, node: usize) -> String {
    let n = &doc.canvas.nodes()[node];
    let t = title(doc, node);
    match &n.kind {
        NodeKind::Text { .. } if t.is_empty() => "Empty text card".to_owned(),
        NodeKind::Text { .. } => format!("Text card: {t}"),
        NodeKind::File { .. } => format!("File card: {t}"),
        NodeKind::Link { .. } => format!("Link card: {t}"),
        NodeKind::Group { .. } if t.is_empty() => "Unnamed group".to_owned(),
        NodeKind::Group { .. } => format!("Group: {t}"),
        NodeKind::Unknown { type_name } => format!("Card of unknown type {type_name}"),
    }
}

/// How a node is called in another card's description.
fn reference(doc: &Doc, node: usize) -> String {
    let n = &doc.canvas.nodes()[node];
    let t = title(doc, node);
    match &n.kind {
        NodeKind::Group { .. } if t.is_empty() => "an unnamed group".to_owned(),
        NodeKind::Group { .. } => format!("group {t}"),
        NodeKind::Text { .. } if t.is_empty() => "an empty text card".to_owned(),
        _ => t,
    }
}

/// The edges of each node, by index into the canvas's edges, in the
/// file's order: the ones going out and the ones coming in (loops left
/// out).
pub(crate) struct Links {
    out: Vec<Vec<usize>>,
    incoming: Vec<Vec<usize>>,
}

impl Links {
    pub(crate) fn of(doc: &Doc) -> Self {
        let n = doc.canvas.nodes().len();
        let mut out = vec![Vec::new(); n];
        let mut incoming = vec![Vec::new(); n];
        for (i, e) in doc.canvas.edges().iter().enumerate() {
            if e.from == e.to {
                continue;
            }
            if let Some(o) = out.get_mut(e.from) {
                o.push(i);
            }
            if let Some(o) = incoming.get_mut(e.to) {
                o.push(i);
            }
        }
        Self { out, incoming }
    }
}

/// The accessible description of card `node`: its group, its edges and
/// its problems.
#[cfg(test)]
pub(crate) fn card_description(doc: &Doc, node: usize) -> String {
    describe(doc, &Links::of(doc), node)
}

/// [`card_description`] with the edges already indexed.
fn describe(doc: &Doc, links: &Links, node: usize) -> String {
    let mut parts = Vec::new();
    if let Some(g) = doc.layout.parent_group(node) {
        parts.push(format!("In {}", reference(doc, g)));
    }
    let labelled = |label: &Option<String>| {
        label
            .as_deref()
            .filter(|l| !l.trim().is_empty())
            .map_or_else(String::new, |l| {
                format!(" ({})", l.split_whitespace().collect::<Vec<_>>().join(" "))
            })
    };
    let edges = doc.canvas.edges();
    let out: Vec<String> = links.out[node]
        .iter()
        .map(|&i| {
            let e = &edges[i];
            format!("{}{}", reference(doc, e.to), labelled(&e.label))
        })
        .collect();
    if !out.is_empty() {
        parts.push(format!("Links to {}", out.join(", ")));
    }
    let incoming: Vec<String> = links.incoming[node]
        .iter()
        .map(|&i| {
            let e = &edges[i];
            format!("{}{}", reference(doc, e.from), labelled(&e.label))
        })
        .collect();
    if !incoming.is_empty() {
        parts.push(format!("Linked from {}", incoming.join(", ")));
    }
    let issues = &doc.node_issues[node];
    if !issues.is_empty() {
        parts.push(format!("Problems: {}", issues.join("; ")));
    }
    parts.join(". ")
}

/// The places in reading order of the `count` cards that the list names:
/// all of them up to `max`, else `max` of them centred on `anchor` (the
/// selected card's place, or the first one in view), kept inside the
/// list.
pub(crate) fn listed_range(count: usize, anchor: Option<usize>, max: usize) -> Range<usize> {
    if count <= max {
        return 0..count;
    }
    let anchor = anchor.unwrap_or(0).min(count - 1);
    let start = anchor.saturating_sub(max / 2).min(count - max);
    start..start + max
}

/// A text card's first line without Markdown markers, shortened.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("---") && !l.starts_with("```"))
        .unwrap_or("");
    let line = line
        .trim_start_matches(['#', '>', ' '])
        .trim_start_matches("- [ ] ")
        .trim_start_matches("- [x] ")
        .trim_start_matches(['-', '*', '+', ' ']);
    let cleaned: String = line
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`' | '[' | ']'))
        .collect();
    let words = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.chars().count() > MAX_TITLE {
        let cut: String = words.chars().take(MAX_TITLE - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        words
    }
}

impl CanvasView {
    /// The invisible list of cards (see the module documentation).
    pub(crate) fn card_list(&self, doc: &Doc, cx: &mut Context<'_, Self>) -> AnyElement {
        let order = doc.layout.reading_order();
        let count = order.len();
        let edges = doc.canvas.edges().len();
        let label = match (count, edges) {
            (1, 0) => "1 card".to_owned(),
            (n, 0) => format!("{n} cards"),
            (1, 1) => "1 card, 1 connection".to_owned(),
            (n, 1) => format!("{n} cards, 1 connection"),
            (n, e) => format!("{n} cards, {e} connections"),
        };
        let mut list = div()
            .id("canvas-cards")
            .a11y_group(Role::List, label)
            .absolute()
            .top_0()
            .left_0()
            .size_0()
            .overflow_hidden();
        let range = listed_range(count, self.list_anchor(doc), MAX_LISTED);
        let links = Links::of(doc);
        for (pos, &node) in order.iter().enumerate().take(range.end).skip(range.start) {
            let selected = self.selected == Some(node);
            let description = describe(doc, &links, node);
            list = list.child(
                div()
                    .id(("canvas-card", node))
                    .a11y_list_item(card_name(doc, node), pos, count)
                    .aria_selected(selected)
                    .when(!description.is_empty(), |d| d.aria_description(description))
                    .when(selected, |d| d.aria_active_descendant())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(Some(node), cx);
                        this.open_node(node, Disposition::InPlace, cx);
                    }))
                    .accessible(),
            );
        }
        list.accessible().into_any_element()
    }

    /// The place in reading order the listed window centres on: the
    /// selected card's, else the first card in view's. Only worked out
    /// when the canvas has more cards than the list names.
    fn list_anchor(&self, doc: &Doc) -> Option<usize> {
        let order = doc.layout.reading_order();
        if order.len() <= MAX_LISTED {
            return None;
        }
        if let Some(s) = self.selected
            && let Some(pos) = order.iter().position(|&n| n == s)
        {
            return Some(pos);
        }
        let mut place = vec![usize::MAX; doc.canvas.nodes().len()];
        for (pos, &n) in order.iter().enumerate() {
            place[n] = pos;
        }
        self.visible_items()
            .into_iter()
            .filter_map(|item| match item {
                ItemId::Node(n) => place.get(n).copied().filter(|&p| p != usize::MAX),
                ItemId::Edge(_) => None,
            })
            .min()
    }
}

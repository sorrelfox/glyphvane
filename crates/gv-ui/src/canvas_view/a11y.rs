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

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
};
use gv_canvas::NodeKind;

use super::{CanvasView, Doc};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::reader::Disposition;

/// The most cards the list names; a larger canvas lists the first ones in
/// reading order.
pub(crate) const MAX_LISTED: usize = 2_000;
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

/// The accessible description of card `node`: its group, its edges and
/// its problems.
pub(crate) fn card_description(doc: &Doc, node: usize) -> String {
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
    let out: Vec<String> = doc
        .canvas
        .edges()
        .iter()
        .filter(|e| e.from == node && e.to != node)
        .map(|e| format!("{}{}", reference(doc, e.to), labelled(&e.label)))
        .collect();
    if !out.is_empty() {
        parts.push(format!("Links to {}", out.join(", ")));
    }
    let incoming: Vec<String> = doc
        .canvas
        .edges()
        .iter()
        .filter(|e| e.to == node && e.from != node)
        .map(|e| format!("{}{}", reference(doc, e.from), labelled(&e.label)))
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
        for (pos, &node) in order.iter().enumerate().take(MAX_LISTED) {
            let selected = self.selected == Some(node);
            let description = card_description(doc, node);
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
}

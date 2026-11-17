// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Reading view's document structure for assistive technology:
//! headings with their levels, paragraphs, list items, links (with their
//! destination and, when it is not plain, their status, so it never rests
//! on colour alone), images by their alt text, tables by rows and cells,
//! code, math, diagrams and live blocks.
//!
//! The view is virtual, so it describes the items around the ones on
//! screen ([`window_of`]); the nodes keep the same ids while their items
//! stay, so moving through the document does not announce it again.

use std::ops::Range;

use gpui::{A11ySubtreeBuilder, Role, accesskit};
use gv_inline::Bias;
use gv_render_tree::{FrameKind, ItemKind, LinkStatus, LiveKind, RenderTree};

/// Items described before and after the ones on screen.
pub(crate) const MARGIN: usize = 40;

/// One node of the description.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DocNode {
    /// Stable within the document: the item's id and the node's place in
    /// it.
    pub key: (u64, u32, u32),
    pub role: Role,
    /// The name: a heading's or link's text, an image's alt text.
    pub label: Option<String>,
    /// The text of a text run.
    pub value: Option<String>,
    pub description: Option<String>,
    pub level: Option<usize>,
    pub url: Option<String>,
    /// A table row's index, or a cell's column.
    pub index: Option<usize>,
    /// A table's row count.
    pub count: Option<usize>,
    pub children: Vec<DocNode>,
}

impl DocNode {
    fn new(key: (u64, u32, u32), role: Role) -> Self {
        Self {
            key,
            role,
            label: None,
            value: None,
            description: None,
            level: None,
            url: None,
            index: None,
            count: None,
            children: Vec::new(),
        }
    }

    fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// The items to describe when `placed` are on screen.
pub(crate) fn window_of(placed: Range<usize>, len: usize) -> Range<usize> {
    placed.start.saturating_sub(MARGIN)..placed.end.saturating_add(MARGIN).min(len)
}

/// Describes items `items` of `tree`.
pub(crate) fn describe(tree: &RenderTree, items: Range<usize>) -> Vec<DocNode> {
    let mut out: Vec<DocNode> = Vec::new();
    let end = items.end.min(tree.items().len());
    for ix in items.start.min(end)..end {
        let item = &tree.items()[ix];
        let key = (item.id.block.0, item.id.chunk, 0);
        let lists = item
            .context
            .iter()
            .filter(|f| matches!(f.kind, FrameKind::List { .. }))
            .count();
        let in_item = item
            .context
            .iter()
            .any(|f| matches!(f.kind, FrameKind::Item { .. }));
        let node = match &item.kind {
            ItemKind::Heading { level } => {
                let mut n = text_node(tree, ix, key, Role::Heading);
                n.level = Some(usize::from(*level));
                n
            }
            ItemKind::Paragraph | ItemKind::Details | ItemKind::EmptyItem => {
                let role = if in_item {
                    Role::ListItem
                } else if matches!(item.kind, ItemKind::Details) {
                    Role::Details
                } else {
                    Role::Paragraph
                };
                let mut n = text_node(tree, ix, key, role);
                if in_item {
                    n.level = Some(lists.max(1));
                }
                n
            }
            ItemKind::TableRow { row, rows } => {
                let row_node = table_row(tree, ix, key, *row);
                // Rows of one table go into one table node.
                let same_table = out.last().is_some_and(|t| {
                    t.role == Role::Table && t.key.0 == key.0 && t.key.2 == u32::MAX
                });
                if !same_table {
                    let mut table = DocNode::new((key.0, key.1, u32::MAX), Role::Table);
                    table.count = Some(*rows as usize);
                    out.push(table);
                }
                if let Some(table) = out.last_mut() {
                    table.children.push(row_node);
                }
                continue;
            }
            ItemKind::Code { chunk, chunks, .. } => {
                let mut n = text_node(tree, ix, key, Role::Code);
                if *chunks > 1 {
                    n.description = Some(format!("Code, part {} of {chunks}", chunk + 1));
                } else {
                    n.description = Some("Code".into());
                }
                n
            }
            ItemKind::Html { .. } | ItemKind::Properties { .. } => {
                text_node(tree, ix, key, Role::Code)
            }
            ItemKind::Math { .. } => {
                let source = item_text(tree, ix);
                let mut n = DocNode::new(key, Role::Math).labelled(source.trim());
                n.description = Some("Formula".into());
                n
            }
            ItemKind::Diagram { .. } => {
                let mut n = DocNode::new(key, Role::Figure).labelled("Diagram");
                n.description = Some(item_text(tree, ix).trim().to_owned());
                n
            }
            ItemKind::ThematicBreak => DocNode::new(key, Role::Splitter),
            ItemKind::EmptyQuote => continue,
            ItemKind::Picture => DocNode::new(key, Role::Image).labelled("Picture"),
            ItemKind::Live { kind } => DocNode::new(key, Role::Figure).labelled(live_name(*kind)),
        };
        out.push(node);
    }
    out
}

fn live_name(kind: LiveKind) -> &'static str {
    match kind {
        LiveKind::Canvas => "Embedded canvas",
        LiveKind::Base => "Embedded base",
        LiveKind::Query => "Search results",
        LiveKind::Audio => "Audio",
        LiveKind::Video => "Video",
        LiveKind::Pdf => "PDF",
        LiveKind::Frame => "Embedded page",
    }
}

fn item_text(tree: &RenderTree, ix: usize) -> String {
    tree.item_text(ix).text
}

/// A node with the item's text as text runs, links and images in
/// reading order. A heading is also named by its text.
fn text_node(tree: &RenderTree, ix: usize, key: (u64, u32, u32), role: Role) -> DocNode {
    let text = tree.item_text(ix);
    let mut node = DocNode::new(key, role);
    if matches!(role, Role::Heading | Role::ListItem | Role::Details) {
        node.label = Some(text.text.trim().to_owned());
    }
    let item = &tree.items()[ix];
    let block = tree.node_of(item.node);
    let content = block.and_then(|b| b.item_inline(&item.kind));
    // (start, end, node) of each link and image, in text offsets.
    let mut marks: Vec<(usize, usize, DocNode)> = Vec::new();
    if let (Some(block), Some(content)) = (block, content) {
        let span = |local: &Range<usize>| {
            let start = text.text_offset(block.to_document(local.start), Bias::Left);
            let end = text.text_offset(block.to_document(local.end), Bias::Right);
            (start.min(text.text.len()), end.min(text.text.len()))
        };
        for (k, link) in content.links.iter().enumerate() {
            let (s, e) = span(&link.source);
            let label = text.text.get(s..e).unwrap_or_default().trim().to_owned();
            let mut n = DocNode::new((key.0, key.1, 1 + k as u32), Role::Link).labelled(
                if label.is_empty() {
                    link.destination.to_string()
                } else {
                    label
                },
            );
            n.url = Some(link.destination.to_string());
            n.description = status_words(link.status).map(str::to_owned);
            marks.push((s, e, n));
        }
        for (k, image) in content.images.iter().enumerate() {
            let (s, e) = span(&image.source);
            let alt = image.alt.trim();
            let n = DocNode::new((key.0, key.1, 10_000 + k as u32), Role::Image).labelled(
                if alt.is_empty() {
                    "Image".to_owned()
                } else {
                    alt.to_owned()
                },
            );
            // An image inside a link is the link's content.
            if image.link.is_some() {
                if let Some(link) = image
                    .link
                    .and_then(|l| marks.get_mut(l as usize))
                    .filter(|m| m.2.role == Role::Link)
                {
                    link.2.children.push(n);
                }
                continue;
            }
            marks.push((s, e, n));
        }
    }
    marks.sort_by_key(|m| m.0);
    let mut at = 0;
    let mut run = 0u32;
    let push_run = |node: &mut DocNode, piece: &str, run: &mut u32| {
        let piece = piece.trim();
        if piece.is_empty() {
            return;
        }
        let mut n = DocNode::new((key.0, key.1, 20_000 + *run), Role::Label);
        n.value = Some(piece.to_owned());
        *run += 1;
        node.children.push(n);
    };
    for (s, e, mark) in marks {
        if s < at {
            continue;
        }
        if let Some(before) = text.text.get(at..s) {
            push_run(&mut node, before, &mut run);
        }
        node.children.push(mark);
        at = e;
    }
    if let Some(rest) = text.text.get(at..) {
        push_run(&mut node, rest, &mut run);
    }
    node
}

/// A table row: one cell per column (column headers in row 0), split at
/// the tabs the row's text has between cells.
fn table_row(tree: &RenderTree, ix: usize, key: (u64, u32, u32), row: u32) -> DocNode {
    let text = item_text(tree, ix);
    let mut node = DocNode::new(key, Role::Row);
    node.index = Some(row as usize);
    let cell_role = if row == 0 {
        Role::ColumnHeader
    } else {
        Role::Cell
    };
    for (col, cell) in text.split('\t').enumerate() {
        let mut c =
            DocNode::new((key.0, key.1, 30_000 + col as u32), cell_role).labelled(cell.trim());
        c.index = Some(col);
        node.children.push(c);
    }
    node
}

/// What a link's status adds to its name; `None` for a plain link.
fn status_words(status: LinkStatus) -> Option<&'static str> {
    match status {
        LinkStatus::Exact | LinkStatus::External | LinkStatus::Pending => None,
        LinkStatus::CaseFold | LinkStatus::Loose => Some("Matched loosely"),
        LinkStatus::Ambiguous(_) => Some("Several notes match"),
        LinkStatus::Unresolved => Some("Broken link: no such note"),
        LinkStatus::OutsideRoot => Some("Link outside the vault"),
        LinkStatus::Blocked => Some("Blocked link"),
    }
}

/// Adds `nodes` as synthetic children of the element being described.
pub(crate) fn push(nodes: &[DocNode], builder: &mut A11ySubtreeBuilder<'_>) {
    let existing: Vec<accesskit::NodeId> = builder.parent_node().children().to_vec();
    let mut top = Vec::with_capacity(nodes.len());
    for node in nodes {
        top.push(push_node(node, builder));
    }
    // `push_child` hangs every node on the element; only the top level
    // belongs there.
    let mut children = existing;
    children.extend(top);
    builder.parent_node().set_children(children);
}

fn push_node(node: &DocNode, builder: &mut A11ySubtreeBuilder<'_>) -> accesskit::NodeId {
    let children: Vec<accesskit::NodeId> = node
        .children
        .iter()
        .map(|c| push_node(c, builder))
        .collect();
    let id = builder.synthetic_node_id(node.key);
    let mut n = accesskit::Node::new(node.role);
    if let Some(label) = &node.label {
        n.set_label(label.clone());
    }
    if let Some(value) = &node.value {
        n.set_value(value.clone());
    }
    if let Some(description) = &node.description {
        n.set_description(description.clone());
    }
    if let Some(level) = node.level {
        n.set_level(level);
    }
    if let Some(url) = &node.url {
        n.set_url(url.clone());
        n.add_action(accesskit::Action::Click);
    }
    match node.role {
        Role::Row => {
            if let Some(i) = node.index {
                n.set_row_index(i);
            }
        }
        Role::Cell | Role::ColumnHeader => {
            if let Some(i) = node.index {
                n.set_column_index(i);
            }
        }
        Role::Table => {
            if let Some(rows) = node.count {
                n.set_row_count(rows);
            }
        }
        _ => {}
    }
    n.set_children(children);
    builder.push_child(id, n);
    id
}

#[cfg(test)]
mod tests {
    use gv_model::DocModel;
    use gv_profile::ProfileId;
    use gv_render_tree::{AllPending, NoImageSizes, RenderOptions, Sources};
    use gv_syntax::ParseOptions;

    use super::*;

    fn tree(text: &str) -> RenderTree {
        let id = ProfileId::GfmFile;
        let syntax = gv_syntax::parse_with(text, ParseOptions::builtin(id));
        let model = DocModel::build(&syntax, text);
        let sources = Sources {
            links: &AllPending,
            images: &NoImageSizes,
        };
        RenderTree::build(&syntax, &model, text, RenderOptions::builtin(id), sources)
    }

    fn all(tree: &RenderTree) -> Vec<DocNode> {
        describe(tree, 0..tree.items().len())
    }

    #[test]
    fn headings_carry_their_level_and_text() {
        let t = tree("# Title\n\ntext\n\n### Deep\n");
        let nodes = all(&t);
        let headings: Vec<_> = nodes.iter().filter(|n| n.role == Role::Heading).collect();
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].label.as_deref(), Some("Title"));
        assert_eq!(headings[0].level, Some(1));
        assert_eq!(headings[1].level, Some(3));
        assert_eq!(nodes[1].role, Role::Paragraph);
        assert_eq!(nodes[1].children[0].value.as_deref(), Some("text"));
    }

    #[test]
    fn links_and_images_are_named_in_reading_order() {
        let t = tree("See [the docs](https://example.org/a) and ![a cat](cat.png) now.\n");
        let p = &all(&t)[0];
        let roles: Vec<Role> = p.children.iter().map(|c| c.role).collect();
        assert_eq!(
            roles,
            [
                Role::Label,
                Role::Link,
                Role::Label,
                Role::Image,
                Role::Label
            ]
        );
        let link = &p.children[1];
        assert_eq!(link.label.as_deref(), Some("the docs"));
        assert_eq!(link.url.as_deref(), Some("https://example.org/a"));
        assert_eq!(p.children[3].label.as_deref(), Some("a cat"));
        assert_eq!(p.children[0].value.as_deref(), Some("See"));
    }

    #[test]
    fn list_items_have_their_nesting_level() {
        let t = tree("- one\n  - two\n- three\n");
        let nodes = all(&t);
        let levels: Vec<_> = nodes
            .iter()
            .map(|n| (n.role, n.label.clone(), n.level))
            .collect();
        assert_eq!(
            levels,
            [
                (Role::ListItem, Some("one".into()), Some(1)),
                (Role::ListItem, Some("two".into()), Some(2)),
                (Role::ListItem, Some("three".into()), Some(1)),
            ]
        );
    }

    #[test]
    fn a_table_is_one_node_with_rows_and_cells() {
        let t = tree("| A | B |\n|---|---|\n| 1 | 2 |\n");
        let nodes = all(&t);
        assert_eq!(nodes.len(), 1);
        let table = &nodes[0];
        assert_eq!(table.role, Role::Table);
        assert_eq!(table.count, Some(2));
        assert_eq!(table.children.len(), 2);
        let header: Vec<_> = table.children[0]
            .children
            .iter()
            .map(|c| (c.role, c.label.clone()))
            .collect();
        assert_eq!(
            header,
            [
                (Role::ColumnHeader, Some("A".into())),
                (Role::ColumnHeader, Some("B".into()))
            ]
        );
        assert_eq!(table.children[1].index, Some(1));
        assert_eq!(table.children[1].children[1].label.as_deref(), Some("2"));
    }

    #[test]
    fn code_and_breaks_have_roles() {
        let t = tree("```\nlet x = 1;\n```\n\n---\n");
        let roles: Vec<Role> = all(&t).iter().map(|n| n.role).collect();
        assert_eq!(roles, [Role::Code, Role::Splitter]);
    }

    #[test]
    fn keys_are_unique_and_stable() {
        let text = "# A\n\n[x](y) ![i](j.png)\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
        let first = all(&tree(text));
        let second = all(&tree(text));
        assert_eq!(first, second);
        let mut keys = Vec::new();
        fn walk(n: &DocNode, keys: &mut Vec<(u64, u32, u32)>) {
            keys.push(n.key);
            for c in &n.children {
                walk(c, keys);
            }
        }
        for n in &first {
            walk(n, &mut keys);
        }
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "{keys:?}");
    }

    #[test]
    fn the_window_reaches_around_the_screen() {
        assert_eq!(window_of(100..110, 1000), 60..150);
        assert_eq!(window_of(0..5, 20), 0..20);
    }
}

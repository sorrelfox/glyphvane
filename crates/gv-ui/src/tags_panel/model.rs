// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Tags panel's tree, built from a content index snapshot without the
//! interface: nested tags (`#a/b/c`) with the number of notes that have
//! each one or a tag nested under it, sorted and filtered into rows.
//!
//! - **Casing:** tags are told apart ignoring letter case; a tag shows as
//!   first written in path order, as [`ContentSnapshot::tags`] does. A
//!   parent that is never written alone takes the casing of its first
//!   nested tag.
//! - **Counts:** a note counts once per tag, front matter `tags` and
//!   inline tags alike, and once for each tag above them.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::SharedString;
use gv_vault::content::ContentSnapshot;

/// One tag of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagNode {
    /// The last segment as shown (`lists` of `editing/lists`).
    pub name: SharedString,
    /// The whole tag as shown, without `#` (`editing/lists`).
    pub tag: Arc<str>,
    /// The whole tag in lower case: what tells tags apart.
    pub key: Arc<str>,
    /// Notes with this tag or one nested under it.
    pub notes: usize,
    pub children: Vec<TagNode>,
}

/// The tags of a vault, as top-level nodes in no particular order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagTree {
    pub roots: Vec<TagNode>,
    /// Distinct tags (nested ones and their parents each count).
    pub len: usize,
}

/// The tag key: lower case, as Obsidian matches tags.
pub fn fold(tag: &str) -> String {
    tag.to_lowercase()
}

/// The tree of every tag in `snapshot`.
pub fn build(snapshot: &ContentSnapshot) -> TagTree {
    // Key -> (shown, notes); keys of every level, parents included.
    let mut shown: HashMap<String, String> = HashMap::new();
    let mut notes: HashMap<String, usize> = HashMap::new();
    for record in snapshot.notes() {
        let mut seen: HashSet<String> = HashSet::new();
        for tag in &record.data.tags {
            let written = tag.name.trim_matches('/');
            if written.is_empty() {
                continue;
            }
            let segments: Vec<&str> = written.split('/').filter(|s| !s.is_empty()).collect();
            for depth in 1..=segments.len() {
                let path = segments[..depth].join("/");
                let key = fold(&path);
                // A tag written as it is beats a parent seen through a
                // nested one only when it comes first.
                shown.entry(key.clone()).or_insert(path);
                if seen.insert(key.clone()) {
                    *notes.entry(key).or_default() += 1;
                }
            }
        }
    }
    let len = shown.len();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut top: Vec<&str> = Vec::new();
    for key in shown.keys() {
        match key.rsplit_once('/') {
            Some((parent, _)) => children.entry(parent).or_default().push(key),
            None => top.push(key),
        }
    }
    let node = |key: &str| make_node(key, &shown, &notes, &children);
    TagTree {
        roots: top.into_iter().map(node).collect(),
        len,
    }
}

fn make_node(
    key: &str,
    shown: &HashMap<String, String>,
    notes: &HashMap<String, usize>,
    children: &HashMap<&str, Vec<&str>>,
) -> TagNode {
    let tag = shown.get(key).map_or(key, String::as_str);
    let name = tag.rsplit('/').next().unwrap_or(tag);
    TagNode {
        name: SharedString::from(name.to_owned()),
        tag: Arc::from(tag),
        key: Arc::from(key),
        notes: notes.get(key).copied().unwrap_or_default(),
        children: children
            .get(key)
            .map(|keys| {
                keys.iter()
                    .map(|k| make_node(k, shown, notes, children))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The order of the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TagSort {
    /// Tag name, A to Z.
    #[default]
    NameAscending,
    /// Tag name, Z to A.
    NameDescending,
    /// Most notes first.
    CountDescending,
    /// Fewest notes first.
    CountAscending,
}

impl TagSort {
    pub const ALL: [Self; 4] = [
        Self::NameAscending,
        Self::NameDescending,
        Self::CountDescending,
        Self::CountAscending,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NameAscending => "Tag name (A to Z)",
            Self::NameDescending => "Tag name (Z to A)",
            Self::CountDescending => "Frequency (high to low)",
            Self::CountAscending => "Frequency (low to high)",
        }
    }

    fn compare(self, a: &TagNode, b: &TagNode) -> Ordering {
        let by_name = || a.key.cmp(&b.key).then_with(|| a.tag.cmp(&b.tag));
        match self {
            Self::NameAscending => by_name(),
            Self::NameDescending => by_name().reverse(),
            Self::CountDescending => b.notes.cmp(&a.notes).then_with(by_name),
            Self::CountAscending => a.notes.cmp(&b.notes).then_with(by_name),
        }
    }
}

/// One row of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRow {
    pub depth: usize,
    pub name: SharedString,
    pub tag: Arc<str>,
    pub key: Arc<str>,
    pub notes: usize,
    pub has_children: bool,
    pub expanded: bool,
}

/// The rows of `tree` in `sort` order. A tag shows when its whole name
/// contains `filter` (ignoring case) or one nested under it does; with a
/// filter every shown tag is open. Without one, tags in `collapsed` hide
/// their children.
pub fn rows(
    tree: &TagTree,
    sort: TagSort,
    filter: &str,
    collapsed: &HashSet<Arc<str>>,
) -> Vec<TagRow> {
    let filter = fold(filter.trim().trim_start_matches('#'));
    let mut out = Vec::new();
    push_rows(&tree.roots, 0, sort, &filter, collapsed, &mut out);
    out
}

fn matches(node: &TagNode, filter: &str) -> bool {
    filter.is_empty()
        || node.key.contains(filter)
        || node.children.iter().any(|c| matches(c, filter))
}

fn push_rows(
    nodes: &[TagNode],
    depth: usize,
    sort: TagSort,
    filter: &str,
    collapsed: &HashSet<Arc<str>>,
    out: &mut Vec<TagRow>,
) {
    let mut shown: Vec<&TagNode> = nodes.iter().filter(|n| matches(n, filter)).collect();
    shown.sort_by(|a, b| sort.compare(a, b));
    for node in shown {
        let expanded = !filter.is_empty() || !collapsed.contains(&node.key);
        out.push(TagRow {
            depth,
            name: node.name.clone(),
            tag: node.tag.clone(),
            key: node.key.clone(),
            notes: node.notes,
            has_children: !node.children.is_empty(),
            expanded,
        });
        if expanded {
            push_rows(&node.children, depth + 1, sort, filter, collapsed, out);
        }
    }
}

/// The search a tag runs (`tag:#name`).
pub fn search_for(tag: &str) -> String {
    format!("tag:#{tag}")
}

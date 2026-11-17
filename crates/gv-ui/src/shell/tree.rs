// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The file tree's model, without GPUI: the order of a folder's entries,
//! and the visible rows with their selection.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::SharedString;
use gv_vault::{EntryKind, FileId, PathIndex, VaultPath};

/// Compares names the way people sort them: case-insensitively, with runs
/// of digits by their value (`a2` before `a10`). Names equal by that measure
/// fall back to their exact text, so the order is total.
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let (n, m) = (digits(&mut x), digits(&mut y));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let order = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(c), Some(d)) => {
                x.next();
                y.next();
                let order = c.to_lowercase().cmp(d.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

fn digits(chars: &mut Peekable<impl Iterator<Item = char>>) -> String {
    let mut run = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        run.push(c);
    }
    run
}

/// What a row shows, which decides its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    Folder,
    Note,
    Canvas,
    Base,
    Image,
    Other,
}

impl RowKind {
    fn of(kind: &EntryKind, path: &VaultPath) -> Self {
        if kind.is_dir() {
            return Self::Folder;
        }
        let ext = path.extension().unwrap_or_default().to_ascii_lowercase();
        match ext.as_str() {
            "md" | "markdown" | "mdown" | "mkd" | "mdx" | "qmd" => Self::Note,
            "canvas" => Self::Canvas,
            "base" => Self::Base,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico" => Self::Image,
            _ => Self::Other,
        }
    }
}

/// The order of a folder's entries. Folders always come first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SortOrder {
    /// By name, A to Z (natural order).
    #[default]
    NameAscending,
    /// By name, Z to A.
    NameDescending,
    /// Files modified last first; folders by name.
    ModifiedNewest,
    /// Files modified first first; folders by name.
    ModifiedOldest,
}

impl SortOrder {
    /// Every order, as the sort menu lists them.
    pub const ALL: [Self; 4] = [
        Self::NameAscending,
        Self::NameDescending,
        Self::ModifiedNewest,
        Self::ModifiedOldest,
    ];

    /// What the sort menu calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::NameAscending => "File name (A to Z)",
            Self::NameDescending => "File name (Z to A)",
            Self::ModifiedNewest => "Modified time (new to old)",
            Self::ModifiedOldest => "Modified time (old to new)",
        }
    }
}

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub id: FileId,
    /// 0 for the root's entries.
    pub depth: usize,
    pub name: SharedString,
    pub kind: RowKind,
    /// An open folder.
    pub expanded: bool,
}

/// The folders and files of a vault as visible rows: folders first, then by
/// name; open folders show their entries below them. One row is selected.
#[derive(Debug)]
pub(crate) struct TreeModel {
    index: PathIndex,
    expanded: HashSet<FileId>,
    selected: Option<FileId>,
    rows: Vec<TreeRow>,
    row_of: HashMap<FileId, usize>,
    sorted: HashMap<FileId, Arc<[FileId]>>,
    sort: SortOrder,
    /// Rows shown while filtering: the entries whose name contains the
    /// filter (ignoring case) and the folders above them, which show open.
    filter: Option<Filter>,
}

#[derive(Debug)]
struct Filter {
    /// The text, case-folded.
    key: String,
    /// The matches and their folders.
    shown: HashSet<FileId>,
    /// The folders above a match.
    open: HashSet<FileId>,
}

impl TreeModel {
    /// Every folder closed, nothing selected.
    pub fn new(index: PathIndex) -> Self {
        let mut this = Self {
            index,
            expanded: HashSet::new(),
            selected: None,
            rows: Vec::new(),
            row_of: HashMap::new(),
            sorted: HashMap::new(),
            sort: SortOrder::default(),
            filter: None,
        };
        this.rebuild();
        this
    }

    pub fn set_sort(&mut self, sort: SortOrder) {
        if self.sort != sort {
            self.sort = sort;
            self.sorted.clear();
            self.rebuild();
        }
    }

    /// The filter text, if the tree is filtered.
    pub fn filter(&self) -> Option<&str> {
        self.filter.as_ref().map(|f| f.key.as_str())
    }

    /// Shows only the entries whose name contains `text` (ignoring case)
    /// and the folders above them; an empty text shows everything again.
    pub fn set_filter(&mut self, text: &str) {
        let key = gv_vault::keys::fold_key(text.trim());
        if key.is_empty() {
            if self.filter.take().is_some() {
                self.rebuild();
            }
            return;
        }
        let mut shown = HashSet::new();
        let mut open = HashSet::new();
        for entry in self.index.iter() {
            if entry.id == FileId::ROOT {
                continue;
            }
            let name = entry.path.file_name().unwrap_or_default();
            if !gv_vault::keys::fold_key(name).contains(&key) {
                continue;
            }
            shown.insert(entry.id);
            let mut parent = entry.path.parent();
            while let Some(dir) = parent.filter(|p| !p.is_root()) {
                if let Some(id) = self.index.id_of(dir.as_str()) {
                    if !open.insert(id) {
                        break;
                    }
                    shown.insert(id);
                }
                parent = dir.parent();
            }
        }
        self.filter = Some(Filter { key, shown, open });
        self.rebuild();
    }

    pub fn index(&self) -> &PathIndex {
        &self.index
    }

    /// Takes a newer snapshot of the same vault. Open folders and the
    /// selection stay where they still exist: [`FileId`]s survive renames.
    pub fn set_index(&mut self, index: PathIndex) {
        self.index = index;
        self.sorted.clear();
        if let Some(filter) = self.filter.take() {
            // The matches of the new snapshot.
            self.set_filter(&filter.key);
        }
        let index = &self.index;
        self.expanded
            .retain(|id| index.get(*id).is_some_and(|e| e.kind.is_dir()));
        if self.selected.is_some_and(|id| index.get(id).is_none()) {
            self.selected = None;
        }
        self.rebuild();
    }

    pub fn rows(&self) -> &[TreeRow] {
        &self.rows
    }

    pub fn selected(&self) -> Option<FileId> {
        self.selected
    }

    pub fn selected_row(&self) -> Option<usize> {
        self.selected.and_then(|id| self.row_of.get(&id).copied())
    }

    /// The absolute path of an entry.
    pub fn fs_path(&self, id: FileId) -> Option<PathBuf> {
        let entry = self.index.get(id)?;
        Some(entry.path.to_fs_path(self.index.root()))
    }

    pub fn is_dir(&self, id: FileId) -> bool {
        self.index.get(id).is_some_and(|e| e.kind.is_dir())
    }

    /// The entry at an absolute path inside the root.
    pub fn id_of_fs_path(&self, path: &Path) -> Option<FileId> {
        let path = std::path::absolute(path).ok()?;
        let rel = path.strip_prefix(self.index.root()).ok()?;
        let vault_path = VaultPath::from_relative_fs_path(rel).ok()?;
        self.index.id_of(vault_path.as_str())
    }

    /// Selects an entry and opens every folder above it.
    pub fn reveal(&mut self, id: FileId) {
        let mut parent = self.parent(id);
        while let Some(dir) = parent.filter(|dir| *dir != FileId::ROOT) {
            self.expanded.insert(dir);
            parent = self.parent(dir);
        }
        self.selected = Some(id);
        self.rebuild();
    }

    pub fn select(&mut self, id: FileId) {
        if self.row_of.contains_key(&id) {
            self.selected = Some(id);
        }
    }

    /// Opens or closes a folder.
    pub fn toggle(&mut self, id: FileId) {
        if !self.is_dir(id) {
            return;
        }
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.rebuild();
    }

    /// Closes every folder. A selection inside one moves to its top folder.
    pub fn collapse_all(&mut self) {
        self.expanded.clear();
        self.rebuild();
    }

    /// Moves the selection one row up (or to the last row). Returns `true`
    /// when it moved.
    pub fn select_previous(&mut self) -> bool {
        let target = match self.selected_row() {
            Some(0) => return false,
            Some(row) => row - 1,
            None => match self.rows.len() {
                0 => return false,
                n => n - 1,
            },
        };
        self.selected = Some(self.rows[target].id);
        true
    }

    /// Moves the selection one row down (or to the first row).
    pub fn select_next(&mut self) -> bool {
        let target = match self.selected_row() {
            Some(row) if row + 1 < self.rows.len() => row + 1,
            Some(_) => return false,
            None if self.rows.is_empty() => return false,
            None => 0,
        };
        self.selected = Some(self.rows[target].id);
        true
    }

    /// Left: closes an open folder, otherwise selects the parent folder.
    pub fn collapse_or_parent(&mut self) -> bool {
        let Some(row) = self.selected_row() else {
            return false;
        };
        let TreeRow { id, expanded, .. } = self.rows[row];
        if expanded {
            self.toggle(id);
            return true;
        }
        match self.parent(id) {
            Some(parent) if parent != FileId::ROOT => {
                self.selected = Some(parent);
                true
            }
            _ => false,
        }
    }

    /// Right: opens a closed folder, otherwise selects an open folder's
    /// first entry.
    pub fn expand_or_child(&mut self) -> bool {
        let Some(row) = self.selected_row() else {
            return false;
        };
        let TreeRow {
            id, kind, expanded, ..
        } = self.rows[row];
        if kind != RowKind::Folder {
            return false;
        }
        if !expanded {
            self.toggle(id);
            return true;
        }
        match self.rows.get(row + 1) {
            Some(next) if next.depth > self.rows[row].depth => {
                self.selected = Some(next.id);
                true
            }
            _ => false,
        }
    }

    fn parent(&self, id: FileId) -> Option<FileId> {
        let path = self.index.get(id)?.path.parent()?;
        self.index.id_of(path.as_str())
    }

    /// The entries of a folder in display order, cached per snapshot.
    fn sorted_children(&mut self, dir: FileId) -> Arc<[FileId]> {
        if let Some(children) = self.sorted.get(&dir) {
            return children.clone();
        }
        let index = &self.index;
        let mut children: Vec<(bool, &str, Option<std::time::SystemTime>, FileId)> = index
            .children(dir)
            .iter()
            .filter_map(|id| {
                let entry = index.get(*id)?;
                Some((
                    entry.kind.is_dir(),
                    entry.path.file_name().unwrap_or_default(),
                    entry.meta.mtime,
                    *id,
                ))
            })
            .collect();
        let sort = self.sort;
        children.sort_by(|a, b| {
            let by_name = natural_cmp(a.1, b.1);
            b.0.cmp(&a.0).then_with(|| match sort {
                SortOrder::NameAscending => by_name,
                SortOrder::NameDescending => by_name.reverse(),
                // Folders have no meaningful time: by name.
                _ if a.0 => by_name,
                SortOrder::ModifiedNewest => b.2.cmp(&a.2).then(by_name),
                SortOrder::ModifiedOldest => a.2.cmp(&b.2).then(by_name),
            })
        });
        let children: Arc<[FileId]> = children.into_iter().map(|(_, _, _, id)| id).collect();
        self.sorted.insert(dir, children.clone());
        children
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let shown = |id: &FileId, filter: &Option<Filter>| {
            filter.as_ref().is_none_or(|f| f.shown.contains(id))
        };
        let mut stack: Vec<(FileId, usize)> = self
            .sorted_children(FileId::ROOT)
            .iter()
            .rev()
            .filter(|id| shown(id, &self.filter))
            .map(|id| (*id, 0))
            .collect();
        while let Some((id, depth)) = stack.pop() {
            let Some(entry) = self.index.get(id) else {
                continue;
            };
            let kind = RowKind::of(&entry.kind, &entry.path);
            let expanded = kind == RowKind::Folder
                && match &self.filter {
                    Some(f) => f.open.contains(&id),
                    None => self.expanded.contains(&id),
                };
            rows.push(TreeRow {
                id,
                depth,
                name: entry.path.file_name().unwrap_or_default().to_owned().into(),
                kind,
                expanded,
            });
            if expanded {
                let children = self.sorted_children(id);
                stack.extend(
                    children
                        .iter()
                        .rev()
                        .filter(|child| shown(child, &self.filter))
                        .map(|child| (*child, depth + 1)),
                );
            }
        }
        self.row_of = rows.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
        self.rows = rows;
        // A selection hidden in a closed folder moves up to the folder.
        let mut selected = self.selected;
        while let Some(id) = selected.filter(|id| !self.row_of.contains_key(id)) {
            selected = self.parent(id).filter(|p| *p != FileId::ROOT);
        }
        self.selected = selected;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(model: &TreeModel) -> Vec<String> {
        model
            .rows()
            .iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    fn model(paths: &[&str]) -> TreeModel {
        TreeModel::new(PathIndex::from_paths(paths).expect("index"))
    }

    fn id(model: &TreeModel, path: &str) -> FileId {
        model.index().id_of(path).expect("indexed")
    }

    #[test]
    fn natural_order_is_case_insensitive_and_numeric() {
        let mut names = vec![
            "b.md", "a10.md", "A2.md", "a2.md", "a01.md", "Zeta", "alpha",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "a01.md", "A2.md", "a2.md", "a10.md", "alpha", "b.md", "Zeta"
            ]
        );
    }

    #[test]
    fn folders_come_first_and_start_closed() {
        let m = model(&[
            "b.md",
            "a10.md",
            "a2.md",
            "zdir/x.md",
            "Adir/y.md",
            "C.canvas",
        ]);
        assert_eq!(
            names(&m),
            ["Adir", "zdir", "a2.md", "a10.md", "b.md", "C.canvas"]
        );
        let kinds: Vec<_> = m.rows().iter().map(|r| r.kind).collect();
        assert_eq!(kinds[0], RowKind::Folder);
        assert_eq!(kinds[2], RowKind::Note);
        assert_eq!(kinds[5], RowKind::Canvas);
    }

    #[test]
    fn reveal_opens_the_ancestors_and_selects() {
        let mut m = model(&["a/b/c.md", "a/d.md", "e.md"]);
        let c = id(&m, "a/b/c.md");
        m.reveal(c);
        assert_eq!(names(&m), ["a", "  b", "    c.md", "  d.md", "e.md"]);
        assert_eq!(m.selected_row(), Some(2));
    }

    #[test]
    fn collapsing_moves_a_hidden_selection_up() {
        let mut m = model(&["a/b/c.md", "e.md"]);
        m.reveal(id(&m, "a/b/c.md"));
        m.toggle(id(&m, "a/b"));
        assert_eq!(m.selected(), Some(id(&m, "a/b")));
        m.reveal(id(&m, "a/b/c.md"));
        m.collapse_all();
        assert_eq!(names(&m), ["a", "e.md"]);
        assert_eq!(m.selected(), Some(id(&m, "a")));
    }

    #[test]
    fn arrow_keys_walk_the_tree() {
        let mut m = model(&["a/b.md", "a/c.md", "d.md"]);
        assert!(m.select_next());
        assert_eq!(m.selected(), Some(id(&m, "a")));
        assert!(m.expand_or_child());
        assert_eq!(names(&m), ["a", "  b.md", "  c.md", "d.md"]);
        assert!(m.expand_or_child());
        assert_eq!(m.selected(), Some(id(&m, "a/b.md")));
        assert!(m.select_next());
        assert!(m.select_next());
        assert_eq!(m.selected(), Some(id(&m, "d.md")));
        assert!(!m.select_next());
        assert!(m.select_previous());
        assert_eq!(m.selected(), Some(id(&m, "a/c.md")));
        assert!(m.collapse_or_parent());
        assert_eq!(m.selected(), Some(id(&m, "a")));
        assert!(m.collapse_or_parent());
        assert_eq!(names(&m), ["a", "d.md"]);
        assert!(!m.collapse_or_parent());
        assert!(!m.select_previous());
    }

    #[test]
    fn the_sort_order_applies_to_files_not_folders_first() {
        let mut m = model(&["b.md", "a.md", "c.md", "Z/x.md", "Y/x.md"]);
        m.set_sort(SortOrder::NameDescending);
        assert_eq!(names(&m), ["Z", "Y", "c.md", "b.md", "a.md"]);
        // Modified time: the index has none here, so by name.
        m.set_sort(SortOrder::ModifiedNewest);
        assert_eq!(names(&m), ["Y", "Z", "a.md", "b.md", "c.md"]);
    }

    #[test]
    fn modified_time_orders_files() {
        use std::time::{Duration, SystemTime};
        let base = model(&["a.md", "b.md", "c.md"]);
        let at = |s: u64| gv_vault::FileMeta {
            mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(s)),
            ..gv_vault::FileMeta::default()
        };
        let next = base
            .index()
            .apply(&[
                gv_vault::Change::Upsert {
                    path: VaultPath::new("a.md").expect("path"),
                    kind: EntryKind::File,
                    meta: at(20),
                },
                gv_vault::Change::Upsert {
                    path: VaultPath::new("b.md").expect("path"),
                    kind: EntryKind::File,
                    meta: at(30),
                },
                gv_vault::Change::Upsert {
                    path: VaultPath::new("c.md").expect("path"),
                    kind: EntryKind::File,
                    meta: at(10),
                },
            ])
            .expect("apply");
        let mut m = TreeModel::new(next);
        m.set_sort(SortOrder::ModifiedNewest);
        assert_eq!(names(&m), ["b.md", "a.md", "c.md"]);
        m.set_sort(SortOrder::ModifiedOldest);
        assert_eq!(names(&m), ["c.md", "a.md", "b.md"]);
    }

    #[test]
    fn the_filter_shows_matches_with_their_folders_open() {
        let mut m = model(&["Notes/Meeting.md", "Notes/other.md", "meet.md", "x/y.md"]);
        m.set_filter("MEET");
        assert_eq!(names(&m), ["Notes", "  Meeting.md", "meet.md"]);
        assert_eq!(m.filter(), Some("meet"));
        m.set_filter("");
        assert_eq!(names(&m), ["Notes", "x", "meet.md"]);
        assert_eq!(m.filter(), None);
    }

    #[test]
    fn a_new_snapshot_keeps_open_folders_and_selection() {
        let mut m = model(&["a/b.md", "c.md"]);
        m.reveal(id(&m, "a/b.md"));
        let next = m
            .index()
            .apply(&[gv_vault::Change::Upsert {
                path: VaultPath::new("a/a.md").expect("path"),
                kind: EntryKind::File,
                meta: gv_vault::FileMeta::default(),
            }])
            .expect("apply");
        m.set_index(next);
        assert_eq!(names(&m), ["a", "  a.md", "  b.md", "c.md"]);
        assert_eq!(m.selected_row(), Some(2));
    }
}

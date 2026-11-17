// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Bookmarks (plan 10 § Panels, Should): the window's side of the
//! Bookmarks panel ([`super::bookmarks_panel`]), the `bookmarks.toggle`
//! command, and the bookmark lists themselves.
//!
//! Two lists are shown, in Obsidian's format (`{"items": [...]}`; each item
//! has a `type` of `file`, `folder`, `search`, `url`, `graph` or `group`,
//! with `path`, `subpath` (`#Heading`, `#^block`), `query`, `url`, `title`
//! and, for a group, `items`):
//!
//! - **Glyphvane's own**, in the app data dir keyed by the vault
//!   (`<data>/vaults/<vault id>/bookmarks.json`). `bookmarks.toggle` adds
//!   and removes the active note here, and renames and moves rewrite it.
//! - **Obsidian's**, `<vault>/.obsidian/bookmarks.json` when present: read
//!   only, never written (NFR-SAFE-04), so a note bookmarked only there
//!   cannot be removed here (an infobar says so), and our renames do not
//!   rewrite it (Obsidian rewrites it for its own renames).
//!
//! Items and keys this code does not know are kept as they are when the
//! own list is written again.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, BorrowAppContext as _, Context, Div, Entity, Global,
    InteractiveElement as _, Stateful, Window,
};
use gv_vault::{Fragment, VaultPath};
use serde_json::{Map, Value};

use super::Workspace;
use super::bookmarks_panel::{BookmarkTarget, BookmarksEvent, BookmarksPanel};
use super::panels::SidebarPanel;
use crate::files::notice::Notice;
use crate::keymap::actions::bookmarks as commands;
use crate::reader::ScrollTarget;

/// The own list's file name in the vault's data folder.
pub(super) const OWN_FILE: &str = "bookmarks.json";

/// What an item is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Folder,
    Search,
    Url,
    Graph,
    Group,
    /// A type this version does not know: kept, shown, not opened.
    Other(String),
}

impl Kind {
    fn parse(name: &str) -> Self {
        match name {
            "file" => Self::File,
            "folder" => Self::Folder,
            "search" => Self::Search,
            "url" => Self::Url,
            "graph" => Self::Graph,
            "group" => Self::Group,
            other => Self::Other(other.to_owned()),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
            Self::Search => "search",
            Self::Url => "url",
            Self::Graph => "graph",
            Self::Group => "group",
            Self::Other(name) => name,
        }
    }
}

/// One bookmark, or a group of them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Bookmark {
    pub kind: Kind,
    pub title: Option<String>,
    /// Vault-relative, `/`-separated (files and folders).
    pub path: Option<String>,
    /// `#Heading`, `#^block` or empty (files).
    pub subpath: Option<String>,
    pub query: Option<String>,
    pub url: Option<String>,
    /// A group's items.
    pub items: Vec<Bookmark>,
    /// Every key as read, written back with the fields above.
    raw: Map<String, Value>,
}

impl Bookmark {
    /// A bookmark of the note at `path` (vault-relative), made at `ctime`
    /// (ms since the epoch).
    pub(crate) fn file(path: &str, ctime: i64) -> Self {
        let mut raw = Map::new();
        raw.insert("ctime".to_owned(), Value::from(ctime));
        Self {
            kind: Kind::File,
            title: None,
            path: Some(path.to_owned()),
            subpath: None,
            query: None,
            url: None,
            items: Vec::new(),
            raw,
        }
    }

    fn from_value(value: &Value) -> Option<Self> {
        let raw = value.as_object()?.clone();
        let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_owned);
        let kind = Kind::parse(raw.get("type")?.as_str()?);
        let items = raw
            .get("items")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Self::from_value).collect())
            .unwrap_or_default();
        Some(Self {
            kind,
            title: text("title"),
            path: text("path"),
            subpath: text("subpath"),
            query: text("query"),
            url: text("url"),
            items,
            raw,
        })
    }

    fn to_value(&self) -> Value {
        let mut raw = self.raw.clone();
        raw.insert("type".to_owned(), Value::from(self.kind.name()));
        let mut put = |key: &str, value: &Option<String>| match value {
            Some(v) => {
                raw.insert(key.to_owned(), Value::from(v.as_str()));
            }
            None => {
                raw.remove(key);
            }
        };
        put("title", &self.title);
        put("path", &self.path);
        put("subpath", &self.subpath);
        put("query", &self.query);
        put("url", &self.url);
        if self.kind == Kind::Group || !self.items.is_empty() {
            raw.insert(
                "items".to_owned(),
                Value::Array(self.items.iter().map(Self::to_value).collect()),
            );
        }
        Value::Object(raw)
    }

    /// A file bookmark of the whole note at `path`.
    fn is_note(&self, path: &str) -> bool {
        self.kind == Kind::File
            && self.path.as_deref() == Some(path)
            && self.subpath.as_deref().is_none_or(str::is_empty)
    }

    /// The text a row shows: the title, else what it points at.
    pub(crate) fn label(&self) -> String {
        if let Some(title) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return title.to_owned();
        }
        match self.kind {
            Kind::File => {
                let path = self.path.as_deref().unwrap_or_default();
                let name = path.rsplit('/').next().unwrap_or(path);
                let name = name.strip_suffix(".md").unwrap_or(name);
                match self.subpath.as_deref().filter(|s| !s.is_empty()) {
                    Some(sub) => format!("{name} › {}", sub.trim_start_matches('#')),
                    None => name.to_owned(),
                }
            }
            Kind::Folder => self.path.clone().unwrap_or_default(),
            Kind::Search => self.query.clone().unwrap_or_default(),
            Kind::Url => self.url.clone().unwrap_or_default(),
            Kind::Graph => "Graph".to_owned(),
            Kind::Group => "Group".to_owned(),
            Kind::Other(ref name) => name.clone(),
        }
    }
}

/// Reads a bookmarks file's items. Items that are not objects with a
/// `type` are left out.
pub(crate) fn parse(text: &str) -> Result<Vec<Bookmark>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "no list of items".to_owned())?;
    Ok(items.iter().filter_map(Bookmark::from_value).collect())
}

/// A bookmarks file of `items`.
pub(crate) fn serialize(items: &[Bookmark]) -> String {
    let mut root = Map::new();
    root.insert(
        "items".to_owned(),
        Value::Array(items.iter().map(Bookmark::to_value).collect()),
    );
    let mut text = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default();
    text.push('\n');
    text
}

/// Whether the whole note at `path` is bookmarked, in any group.
pub(crate) fn contains_note(items: &[Bookmark], path: &str) -> bool {
    items
        .iter()
        .any(|b| b.is_note(path) || contains_note(&b.items, path))
}

/// Removes every bookmark of the whole note at `path`. Returns whether one
/// was there.
pub(crate) fn remove_note(items: &mut Vec<Bookmark>, path: &str) -> bool {
    let before = items.len();
    items.retain(|b| !b.is_note(path));
    let mut removed = items.len() != before;
    for item in items.iter_mut() {
        removed |= remove_note(&mut item.items, path);
    }
    removed
}

/// Rewrites the files and folders at or below `from` to be at or below
/// `to` (a rename or move). Returns whether anything changed.
pub(crate) fn rename(items: &mut [Bookmark], from: &str, to: &str) -> bool {
    let mut changed = false;
    for item in items.iter_mut() {
        if matches!(item.kind, Kind::File | Kind::Folder)
            && let Some(path) = item.path.as_deref()
        {
            let moved = if path == from {
                Some(to.to_owned())
            } else {
                path.strip_prefix(from)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|rest| format!("{to}/{rest}"))
            };
            if let Some(moved) = moved {
                item.path = Some(moved);
                changed = true;
            }
        }
        changed |= rename(&mut item.items, from, to);
    }
    changed
}

/// Read-modify-write of the own lists, one at a time in the process.
static OWN_LOCK: Mutex<()> = Mutex::new(());

/// Reads a bookmarks file: `Ok(None)` when it does not exist.
pub(super) fn read_file(path: &Path) -> Result<Option<Vec<Bookmark>>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// Changes the own list at `file` with `change`, which returns whether it
/// changed anything; writes it back atomically when it did.
pub(super) fn update_own(
    file: &Path,
    change: impl FnOnce(&mut Vec<Bookmark>) -> bool,
) -> Result<bool, String> {
    let _guard = OWN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut items = read_file(file)?.unwrap_or_default();
    if !change(&mut items) {
        return Ok(false);
    }
    gv_settings::atomic_write(file, serialize(&items).as_bytes()).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Obsidian's bookmarks of the vault at `root`.
pub(super) fn obsidian_file(root: &Path) -> PathBuf {
    root.join(".obsidian").join(OWN_FILE)
}

/// Glyphvane's own bookmarks of the vault at `root`, in the app data dir;
/// `None` without one.
pub(super) fn own_file(root: &Path, cx: &App) -> Option<PathBuf> {
    let locations = cx
        .try_global::<crate::settings::AppSettings>()?
        .locations()?
        .clone();
    let id = crate::settings::vault_id(root);
    Some(locations.vault_data_dir(&id).ok()?.join(OWN_FILE))
}

/// Grows each time an own list is written, so every window's panel reads
/// the lists again.
#[derive(Debug, Default)]
pub(crate) struct BookmarksRevision(pub u64);

impl Global for BookmarksRevision {}

/// Tells every panel that a list changed.
pub(super) fn changed(cx: &mut App) {
    cx.update_default_global::<BookmarksRevision, _>(|r, _| r.0 += 1);
}

/// `path` relative to `root`, `/`-separated.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let rel = path.strip_prefix(root).ok()?;
    VaultPath::from_relative_fs_path(rel)
        .ok()
        .map(|p| p.as_str().to_owned())
}

impl Workspace {
    /// The Bookmarks panel, made on first use, on the window's vault.
    pub(super) fn ensure_bookmarks_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Entity<BookmarksPanel> {
        if let Some(panel) = &self.panels.bookmarks {
            return panel.clone();
        }
        self.ensure_vault(cx);
        let panel = cx.new(BookmarksPanel::new);
        self.panels._bookmarks_events = Some(cx.subscribe_in(
            &panel,
            window,
            |this, _, event: &BookmarksEvent, window, cx| {
                this.bookmark_event(event.clone(), window, cx);
            },
        ));
        self.panels.bookmarks = Some(panel.clone());
        self.sync_bookmarks(cx);
        panel
    }

    /// The panel follows the window's vault and the active tab.
    pub(super) fn sync_bookmarks(&mut self, cx: &mut Context<'_, Self>) {
        let Some(panel) = self.panels.bookmarks.clone() else {
            return;
        };
        let root = self.vault_root(cx);
        let own = root.as_deref().and_then(|r| own_file(r, cx));
        let active = root
            .as_deref()
            .zip(self.active_tab().path.as_deref())
            .and_then(|(root, path)| relative(root, path));
        panel.update(cx, |p, cx| p.set_scope(root, own, active, cx));
    }

    /// `bookmarks.toggle`, while the window has a vault and the active tab
    /// a file.
    pub(super) fn bookmark_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let can = self.vault.is_some() && self.active_tab().path.is_some();
        root.when(can, |root| {
            root.on_action(cx.listener(|this, _: &commands::Toggle, _, cx| {
                this.toggle_bookmark(cx);
            }))
        })
    }

    /// Adds the active note to the own bookmarks, or removes it. A note
    /// bookmarked only in Obsidian is not removed (its file is never
    /// written): an infobar says so.
    pub(super) fn toggle_bookmark(&mut self, cx: &mut Context<'_, Self>) {
        let Some(root) = self.vault_root(cx) else {
            return;
        };
        let Some(path) = self
            .active_tab()
            .path
            .as_deref()
            .and_then(|p| relative(&root, p))
        else {
            return;
        };
        let Some(own) = own_file(&root, cx) else {
            self.ask(
                Notice::file_op_failed(
                    "Not bookmarked.",
                    "There is no data folder to keep bookmarks in",
                ),
                cx,
            );
            return;
        };
        let obsidian = obsidian_file(&root);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let task = cx.background_spawn(async move {
            let mut outcome = Toggled::Added;
            let result = update_own(&own, |items| {
                if remove_note(items, &path) {
                    outcome = Toggled::Removed;
                    return true;
                }
                let in_obsidian = read_file(&obsidian)
                    .ok()
                    .flatten()
                    .is_some_and(|items| contains_note(&items, &path));
                if in_obsidian {
                    outcome = Toggled::InObsidian;
                    return false;
                }
                items.push(Bookmark::file(&path, now));
                true
            });
            result.map(|_| outcome)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(Toggled::Added | Toggled::Removed) => changed(cx),
                    Ok(Toggled::InObsidian) => ws.ask(
                        Notice::file_op_failed(
                            "Bookmark not removed.",
                            "This note is bookmarked in Obsidian, whose bookmarks are only \
                             read here; remove it in Obsidian",
                        ),
                        cx,
                    ),
                    Err(err) => ws.ask(Notice::file_op_failed("Not bookmarked.", &err), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Files were renamed or moved: the own bookmarks follow them.
    pub(super) fn bookmarks_moved(
        &mut self,
        moved: &[(VaultPath, VaultPath)],
        cx: &mut Context<'_, Self>,
    ) {
        let Some(own) = self.vault_root(cx).and_then(|r| own_file(&r, cx)) else {
            return;
        };
        if moved.is_empty() {
            return;
        }
        let moved: Vec<(String, String)> = moved
            .iter()
            .map(|(f, t)| (f.as_str().to_owned(), t.as_str().to_owned()))
            .collect();
        let task = cx.background_spawn(async move {
            update_own(&own, |items| {
                let mut any = false;
                for (from, to) in &moved {
                    any |= rename(items, from, to);
                }
                any
            })
        });
        cx.spawn(async move |_, cx| match task.await {
            Ok(true) => cx.update(changed),
            Ok(false) => {}
            Err(err) => tracing::warn!("bookmarks: cannot follow a rename: {err}"),
        })
        .detach();
    }

    fn bookmark_event(
        &mut self,
        event: BookmarksEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            BookmarksEvent::Open(target) => self.open_bookmark(target, window, cx),
        }
    }

    /// Opens what a bookmark points at: a note (at its heading or block),
    /// a folder in the Files panel, a search in the Search panel, a web
    /// page after confirmation.
    fn open_bookmark(
        &mut self,
        target: BookmarkTarget,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match target {
            BookmarkTarget::File { path, subpath } => {
                if !super::canvas_tabs::opens_as_tab(&path) {
                    self.open_other_file(path, cx);
                    return;
                }
                let scroll = subpath
                    .as_deref()
                    .map(|s| s.trim_start_matches('#'))
                    .filter(|s| !s.is_empty())
                    .map(|s| ScrollTarget::Fragment(Fragment::from_wikilink(s)));
                let shown = self
                    .tabs
                    .iter()
                    .position(|t| t.path.as_deref() == Some(path.as_path()));
                match (shown, scroll) {
                    (Some(index), scroll) => {
                        self.activate_tab(index, window, cx);
                        if scroll.is_some() {
                            self.navigate(index, path, scroll, window, cx);
                        }
                    }
                    (None, scroll) => {
                        let scroll = scroll.or(Some(ScrollTarget::Offset(0)));
                        self.insert_tab(super::OpenTarget::new(path), scroll, true, window, cx);
                    }
                }
            }
            BookmarkTarget::Folder(folder) => {
                self.show_sidebar_panel(SidebarPanel::Files, true, window, cx);
                if let Some(files) = &self.files {
                    files.update(cx, |f, cx| f.reveal_vault_path(&folder, cx));
                }
            }
            BookmarkTarget::Search(query) => {
                self.show_sidebar_panel(SidebarPanel::Search, true, window, cx);
                let search = self.ensure_search_panel(window, cx);
                search.update(cx, |s, cx| s.set_query(&query, window, cx));
            }
            BookmarkTarget::Url(url) => {
                self.ask(
                    Notice::confirm_url(&url, "This bookmark opens a web page."),
                    cx,
                );
            }
        }
    }

    /// The Bookmarks panel, once made (tests).
    #[cfg(test)]
    pub(crate) fn bookmarks_panel(&self) -> Option<Entity<BookmarksPanel>> {
        self.panels.bookmarks.clone()
    }
}

/// What `bookmarks.toggle` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggled {
    Added,
    Removed,
    InObsidian,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file in Obsidian's format (obsidian-typings, catalyst 1.13.2,
    /// `BookmarkItem`; read 2026-11-17).
    const OBSIDIAN: &str = r##"{
  "items": [
    {"type": "file", "ctime": 1700000000000, "path": "Notes/A.md"},
    {"type": "file", "ctime": 1700000000001, "path": "Notes/A.md", "subpath": "#Intro"},
    {"type": "file", "ctime": 1700000000002, "path": "B.md", "subpath": "#^abc123", "title": "Block"},
    {"type": "folder", "ctime": 1700000000003, "path": "Notes"},
    {"type": "search", "ctime": 1700000000004, "query": "tag:#todo"},
    {"type": "url", "ctime": 1700000000005, "url": "https://example.org", "title": "Site"},
    {"type": "graph", "ctime": 1700000000006, "title": "Graph", "options": {"search": ""}},
    {"type": "group", "ctime": 1700000000007, "title": "Work", "items": [
      {"type": "file", "ctime": 1700000000008, "path": "Notes/C.md"}
    ]},
    {"type": "mystery", "ctime": 1, "x": [1, 2]},
    "not an item"
  ]
}"##;

    #[test]
    fn reads_every_kind_of_obsidian_item() {
        let items = parse(OBSIDIAN).expect("parses");
        assert_eq!(items.len(), 9);
        assert_eq!(items[0].label(), "A");
        assert_eq!(items[1].label(), "A › Intro");
        assert_eq!(items[2].label(), "Block");
        assert_eq!(items[3].kind, Kind::Folder);
        assert_eq!(items[4].query.as_deref(), Some("tag:#todo"));
        assert_eq!(items[5].url.as_deref(), Some("https://example.org"));
        assert_eq!(items[6].kind, Kind::Graph);
        assert_eq!(items[7].items.len(), 1);
        assert_eq!(items[8].kind, Kind::Other("mystery".to_owned()));
        assert!(contains_note(&items, "Notes/A.md"));
        assert!(contains_note(&items, "Notes/C.md"));
        assert!(!contains_note(&items, "B.md"), "only a block of B");
    }

    #[test]
    fn writing_keeps_unknown_keys_and_items() {
        let items = parse(OBSIDIAN).expect("parses");
        let again = parse(&serialize(&items)).expect("parses again");
        assert_eq!(items, again);
        let text = serialize(&items);
        assert!(text.contains("\"options\""), "{text}");
        assert!(text.contains("\"mystery\""), "{text}");
        assert!(text.contains("1700000000008"), "{text}");
    }

    #[test]
    fn notes_are_added_removed_and_follow_renames() {
        let mut items = vec![Bookmark::file("a.md", 5)];
        items.push(
            Bookmark::from_value(&serde_json::json!({
                "type": "group", "title": "G", "items": [
                    {"type": "file", "path": "Folder/b.md"},
                    {"type": "folder", "path": "Folder"},
                    {"type": "file", "path": "Folder2/c.md"}
                ]
            }))
            .expect("group"),
        );
        assert!(rename(&mut items, "Folder", "Moved"));
        let group = &items[1].items;
        assert_eq!(group[0].path.as_deref(), Some("Moved/b.md"));
        assert_eq!(group[1].path.as_deref(), Some("Moved"));
        assert_eq!(
            group[2].path.as_deref(),
            Some("Folder2/c.md"),
            "not a prefix match"
        );
        assert!(rename(&mut items, "a.md", "z.md"));
        assert!(contains_note(&items, "z.md"));
        assert!(remove_note(&mut items, "Moved/b.md"));
        assert!(!contains_note(&items, "Moved/b.md"));
        assert!(!remove_note(&mut items, "nothing.md"));
    }

    #[test]
    fn the_own_list_is_written_atomically_and_read_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("vaults/x/bookmarks.json");
        assert_eq!(read_file(&file), Ok(None));
        let changed = update_own(&file, |items| {
            items.push(Bookmark::file("n.md", 1));
            true
        });
        assert_eq!(changed, Ok(true));
        let items = read_file(&file).expect("reads").expect("exists");
        assert!(contains_note(&items, "n.md"));
        assert!(parse("{").is_err());
        assert!(parse("{}").is_err());
    }
}

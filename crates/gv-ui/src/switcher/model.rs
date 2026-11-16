// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the switcher lists and how it ranks it: the entries gathered from
//! the vault's content index, the open tabs and the recent files, matched
//! against the query with `nucleo-matcher`.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gv_vault::VaultPath;
use gv_vault::content::{ContentSnapshot, RecordKind, SwitcherEntry};
use gv_vault::keys::fold_key;
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use unicode_segmentation::UnicodeSegmentation as _;

/// What a row stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A Markdown note.
    Note,
    /// Any other file of the vault (an image, a canvas, ...).
    File,
    /// An alias of a note: "Alias → Note".
    Alias,
    /// A note that links point to but that does not exist yet.
    Unresolved {
        /// How many links point to it.
        links: usize,
    },
    /// A heading of a note (`#` mode).
    Heading {
        /// 1 to 6.
        level: u8,
    },
    /// A block id of a note (`^` mode).
    Block,
}

/// One thing the switcher can list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    /// What the row shows first and the query is matched against: a note's
    /// title, a file's name, an alias, a heading.
    pub label: Arc<str>,
    /// The path the query is also matched against (vault-relative inside
    /// the vault, else the full path with `~` for the home folder). Empty
    /// for entries that are not files.
    pub path_text: Arc<str>,
    /// The folder shown at the right of the row.
    pub folder: Arc<str>,
    /// An alias's note, a heading's or block's note.
    pub target: Option<Arc<str>>,
    /// The file, absolute. `None` for a note that does not exist.
    pub file: Option<PathBuf>,
    /// A heading's or block's 1-based line.
    pub line: Option<u32>,
    /// The position of the file among the recent files (open tabs first);
    /// `u32::MAX` when it is not recent.
    pub recent: u32,
    /// The file has unsaved changes (an open tab's, or an unsaved note).
    pub unsaved: bool,
}

impl Entry {
    fn is_file(&self) -> bool {
        matches!(self.kind, EntryKind::Note | EntryKind::File)
    }
}

/// The window's vault, as far as it is known.
#[derive(Debug, Clone, Default)]
pub struct VaultSource {
    /// Its root folder.
    pub root: PathBuf,
    /// The content index's switcher entries (files, titles, aliases).
    pub entries: Vec<SwitcherEntry>,
    /// Notes with unsaved changes in the content index (overlays).
    pub unsaved: HashSet<VaultPath>,
    /// Notes that links point to but that do not exist, with the number of
    /// links.
    pub unresolved: Vec<(Arc<str>, usize)>,
}

impl VaultSource {
    /// The switcher's part of a content snapshot.
    pub fn from_content(root: &Path, snapshot: &ContentSnapshot) -> Self {
        let entries = snapshot.switcher_entries();
        let unsaved = entries
            .iter()
            .filter(|e| e.alias.is_none())
            .filter(|e| {
                snapshot
                    .note(&e.path)
                    .is_some_and(|r| r.kind == RecordKind::Overlay)
            })
            .map(|e| e.path.clone())
            .collect();
        Self {
            root: root.to_path_buf(),
            entries,
            unsaved,
            unresolved: snapshot.unresolved_targets(),
        }
    }

    /// The files of a path index, before the content index is built: no
    /// titles or aliases yet.
    pub fn from_paths(index: &gv_vault::PathIndex) -> Self {
        let entries = index
            .iter()
            .filter(|e| e.kind == gv_vault::EntryKind::File)
            .map(|e| SwitcherEntry {
                path: e.path.clone(),
                title: None,
                alias: None,
                is_note: is_note_path(e.path.as_str()),
            })
            .collect();
        Self {
            root: index.root().to_path_buf(),
            entries,
            unsaved: HashSet::new(),
            unresolved: Vec::new(),
        }
    }
}

/// Everything the switcher draws on when it opens or its sources change.
#[derive(Debug, Clone, Default)]
pub struct Gather {
    /// The vault, once its index is built.
    pub vault: Option<VaultSource>,
    /// The files of the open tabs, in tab order, with whether each has
    /// unsaved changes.
    pub open: Vec<(PathBuf, bool)>,
    /// The recent files, most recent first.
    pub recent: Vec<PathBuf>,
    /// The user's home folder, shown as `~`.
    pub home: Option<PathBuf>,
}

/// Whether a file name is a Markdown note's.
fn is_note_path(name: &str) -> bool {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    ["md", "markdown", "mdown", "mkd", "mdx", "qmd"]
        .iter()
        .any(|m| m.eq_ignore_ascii_case(ext))
}

fn stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// The entries for `gather`: the vault's files and aliases, then the open
/// and recent files outside it (all of them when there is no vault), then
/// the notes that do not exist yet. Recent files rank by their position:
/// the open tabs first, in tab order, then the recent files.
pub fn collect(gather: &Gather) -> Vec<Entry> {
    // Recent positions, by absolute path.
    let mut recent: HashMap<&Path, u32> = HashMap::new();
    let mut order: Vec<&Path> = Vec::new();
    for path in gather
        .recent
        .iter()
        .map(PathBuf::as_path)
        .chain(gather.open.iter().map(|(p, _)| p.as_path()))
    {
        if !recent.contains_key(path) {
            recent.insert(path, u32::try_from(order.len()).unwrap_or(u32::MAX));
            order.push(path);
        }
    }
    let unsaved_tabs: HashSet<&Path> = gather
        .open
        .iter()
        .filter(|(_, unsaved)| *unsaved)
        .map(|(p, _)| p.as_path())
        .collect();

    let mut out = Vec::new();
    let mut listed: HashSet<PathBuf> = HashSet::new();
    if let Some(vault) = &gather.vault {
        out.reserve(vault.entries.len() + vault.unresolved.len());
        for e in &vault.entries {
            let file = e.path.to_fs_path(&vault.root);
            let name = e.path.file_name().unwrap_or_default();
            let note = e.is_note || is_note_path(name);
            let folder: Arc<str> = e
                .path
                .parent()
                .filter(|p| !p.is_root())
                .map_or_else(|| Arc::from(""), |p| Arc::from(p.as_str()));
            let rank = recent.get(file.as_path()).copied().unwrap_or(u32::MAX);
            let unsaved = vault.unsaved.contains(&e.path) || unsaved_tabs.contains(file.as_path());
            let title: Arc<str> = match &e.title {
                Some(t) => t.clone(),
                None if note => Arc::from(stem(name)),
                None => Arc::from(name),
            };
            let entry = match &e.alias {
                Some(alias) => Entry {
                    kind: EntryKind::Alias,
                    label: alias.clone(),
                    path_text: Arc::from(""),
                    folder,
                    target: Some(title),
                    file: Some(file),
                    line: None,
                    recent: rank,
                    unsaved,
                },
                None => {
                    listed.insert(file.clone());
                    Entry {
                        kind: if note {
                            EntryKind::Note
                        } else {
                            EntryKind::File
                        },
                        label: title,
                        path_text: Arc::from(e.path.as_str()),
                        folder,
                        target: None,
                        file: Some(file),
                        line: None,
                        recent: rank,
                        unsaved,
                    }
                }
            };
            out.push(entry);
        }
    }
    // Open and recent files outside the vault (or all of them without
    // one): by order, so the open tabs come first.
    let inside = |p: &Path| {
        gather
            .vault
            .as_ref()
            .is_some_and(|v| p.starts_with(&v.root))
    };
    for (rank, path) in order.iter().enumerate() {
        if listed.contains(*path) || inside(path) {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy()) else {
            continue;
        };
        let note = is_note_path(&name);
        let folder = path
            .parent()
            .and_then(Path::file_name)
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        out.push(Entry {
            kind: if note {
                EntryKind::Note
            } else {
                EntryKind::File
            },
            label: Arc::from(if note { stem(&name) } else { &*name }),
            path_text: Arc::from(display_path(path, gather.home.as_deref())),
            folder: Arc::from(folder),
            target: None,
            file: Some(path.to_path_buf()),
            line: None,
            recent: u32::try_from(rank).unwrap_or(u32::MAX),
            unsaved: unsaved_tabs.contains(*path),
        });
        listed.insert(path.to_path_buf());
    }
    if let Some(vault) = &gather.vault {
        for (name, links) in &vault.unresolved {
            out.push(Entry {
                kind: EntryKind::Unresolved { links: *links },
                label: name.clone(),
                path_text: Arc::from(""),
                folder: Arc::from(""),
                target: None,
                file: None,
                line: None,
                recent: u32::MAX,
                unsaved: false,
            });
        }
    }
    out
}

/// `path` with the home folder written `~`.
fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The headings (`#` mode) or block ids (`^` mode) of every note of the
/// snapshot, in path order.
pub fn sections(snapshot: &ContentSnapshot, root: &Path, blocks: bool) -> Vec<Entry> {
    let mut out = Vec::new();
    for record in snapshot.notes() {
        let file = record.path.to_fs_path(root);
        let note: Arc<str> = Arc::from(record.title());
        let folder: Arc<str> = record
            .path
            .parent()
            .filter(|p| !p.is_root())
            .map_or_else(|| Arc::from(""), |p| Arc::from(p.as_str()));
        if blocks {
            for b in &record.data.block_ids {
                out.push(Entry {
                    kind: EntryKind::Block,
                    label: Arc::from(format!("^{}", b.id)),
                    path_text: Arc::from(""),
                    folder: folder.clone(),
                    target: Some(note.clone()),
                    file: Some(file.clone()),
                    line: Some(b.line + 1),
                    recent: u32::MAX,
                    unsaved: false,
                });
            }
        } else {
            for h in &record.data.headings {
                out.push(Entry {
                    kind: EntryKind::Heading { level: h.level },
                    label: h.text.clone(),
                    path_text: Arc::from(""),
                    folder: folder.clone(),
                    target: Some(note.clone()),
                    file: Some(file.clone()),
                    line: Some(h.line + 1),
                    recent: u32::MAX,
                    unsaved: false,
                });
            }
        }
    }
    out
}

/// What the query asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Files, titles, aliases and notes to create.
    Files,
    /// `#`: headings.
    Headings,
    /// `^`: block ids.
    Blocks,
}

/// The mode of `query` and the query without its mode character.
pub fn mode_of(query: &str) -> (Mode, &str) {
    let trimmed = query.trim_start();
    if let Some(rest) = trimmed.strip_prefix('#') {
        (Mode::Headings, rest)
    } else if let Some(rest) = trimmed.strip_prefix('^') {
        (Mode::Blocks, rest)
    } else {
        (Mode::Files, query)
    }
}

/// What a label match is worth more than the same match in the path:
/// file names and titles rank first.
const LABEL_BONUS: u32 = 16;
/// The label equals the query (ignoring case): such a row comes first.
const EXACT_BONUS: u32 = 1_000;
/// The most a recent file gains: about two matched letters, so recency
/// breaks near-ties without burying a clearly better match.
const RECENT_BONUS: u32 = 32;
/// Recent positions that earn a bonus.
const RECENT_SPAN: u32 = 32;

fn recent_bonus(rank: u32) -> u32 {
    RECENT_SPAN.saturating_sub(rank) * RECENT_BONUS / RECENT_SPAN
}

/// The fuzzy pattern of a query: words in any order, case ignored,
/// accents ignored unless the query has some.
fn pattern(query: &str) -> Pattern {
    Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    )
}

/// The rows (indices into `entries`) for a query without its mode
/// character, best first.
///
/// An empty query lists the files only: recent ones first (open tabs, then
/// the recent files), then the rest by path. Otherwise every word of the
/// query must match, in any order, the label or (for files) the path; a
/// label match is worth [`LABEL_BONUS`] more, a label equal to the query
/// comes first, and recent files gain up to [`RECENT_BONUS`]. Notes that
/// do not exist yet come after everything that does. Ties go to the
/// shorter label, then to path order.
pub fn rank(entries: &[Entry], query: &str) -> Vec<usize> {
    let query = query.trim();
    if query.is_empty() {
        let mut rows: Vec<usize> = (0..entries.len())
            .filter(|&i| entries[i].is_file() || entries[i].line.is_some())
            .collect();
        rows.sort_by(|&a, &b| {
            let (a, b) = (&entries[a], &entries[b]);
            a.recent
                .cmp(&b.recent)
                .then_with(|| a.path_text.cmp(&b.path_text))
        });
        return rows;
    }
    let pattern = pattern(query);
    let folded = fold_key(query);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(bool, u32, usize)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let label = pattern
            .score(Utf32Str::new(&entry.label, &mut buf), &mut matcher)
            .map(|s| s + LABEL_BONUS);
        let path = if entry.is_file() && !entry.path_text.is_empty() {
            pattern.score(Utf32Str::new(&entry.path_text, &mut buf), &mut matcher)
        } else {
            None
        };
        let Some(mut score) = label.max(path) else {
            continue;
        };
        if label.is_some() && fold_key(&entry.label) == folded {
            score += EXACT_BONUS;
        }
        score += recent_bonus(entry.recent);
        let missing = matches!(entry.kind, EntryKind::Unresolved { .. });
        scored.push((missing, score, index));
    }
    scored.sort_unstable_by(|a, b| {
        a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then_with(|| {
            let (x, y) = (&entries[a.2], &entries[b.2]);
            x.label
                .len()
                .cmp(&y.label.len())
                .then_with(|| x.path_text.cmp(&y.path_text))
                .then(a.2.cmp(&b.2))
        })
    });
    scored.into_iter().map(|(_, _, i)| i).collect()
}

/// The byte ranges of `text` that `query` (without its mode character)
/// matches, merged where they touch; empty when it does not match.
pub fn highlight(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let pattern = pattern(query);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    if pattern
        .indices(Utf32Str::new(text, &mut buf), &mut matcher, &mut indices)
        .is_none()
    {
        return Vec::new();
    }
    indices.sort_unstable();
    indices.dedup();
    let spans: Vec<Range<usize>> = if text.is_ascii() {
        indices
            .iter()
            .map(|&i| i as usize..i as usize + 1)
            .collect()
    } else {
        text.grapheme_indices(true)
            .enumerate()
            .filter(|(i, _)| indices.binary_search(&(*i as u32)).is_ok())
            .map(|(_, (start, g))| start..start + g.len())
            .collect()
    };
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if last.end == span.start => last.end = span.end,
            _ => merged.push(span),
        }
    }
    merged
}

/// Characters that a new note's name may not have: forbidden on Windows
/// (and `/`, `\` that would make folders), and those that break wikilinks
/// (`#`, `^`, `[`, `]`, `|`).
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '\\', '|', '?', '*', '#', '^', '[', ']'];

/// The vault-relative path (with `/`) of the note a query creates, or
/// `None` when nothing is left of it. The query's folders are kept
/// (`Projects/Plan` creates `Plan.md` in `Projects`); each part loses the
/// characters no file system or wikilink allows, its trailing dots and
/// spaces, and is normalised to NFC; a Windows device name gets `_` after
/// it; `.md` is added unless the name has a Markdown extension.
pub fn new_note_path(query: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in query.split('/') {
        let cleaned: String = part
            .chars()
            .filter(|c| !c.is_control() && !FORBIDDEN.contains(c))
            .collect();
        let mut cleaned = gv_vault::keys::nfc_key(&cleaned)
            .trim()
            .trim_end_matches(['.', ' '])
            .trim_start_matches('.')
            .to_owned();
        if cleaned.is_empty() {
            continue;
        }
        if gv_vault::platform::is_reserved_device_name(&cleaned) {
            cleaned.push('_');
        }
        // 255 bytes per component, the extension included.
        while cleaned.len() > 250 {
            cleaned.pop();
        }
        parts.push(cleaned);
    }
    let name = parts.pop()?;
    let name = if is_note_path(&name) {
        name
    } else {
        format!("{name}.md")
    };
    parts.push(name);
    Some(parts.join("/"))
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;

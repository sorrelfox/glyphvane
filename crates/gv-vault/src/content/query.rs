// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Queries on a [`ContentSnapshot`].

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::FileSource;
use super::record::{IndexedLink, NoteRecord, RecordKind, Span, file_stem};
use super::state::{Cancelled, LinkTarget, State, parallel_map};
use crate::index::conflict::{SyncService, detect_sync_conflict};
use crate::index::{EntryKind, PathIndex};
use crate::keys::fold_key;
use crate::path::VaultPath;
use crate::resolve::Status;

/// An immutable snapshot of the content index. Cloning is O(1); it is
/// `Send + Sync`, so queries run on any thread.
#[derive(Clone)]
pub struct ContentSnapshot(pub(crate) Arc<State>);

impl std::fmt::Debug for ContentSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentSnapshot")
            .field("generation", &self.0.generation)
            .field("notes", &self.note_count())
            .finish_non_exhaustive()
    }
}

/// One link of a note with where it points.
#[derive(Debug, Clone)]
pub struct OutgoingLink<'a> {
    /// The link.
    pub link: &'a IndexedLink,
    /// Where it resolves.
    pub target: LinkTarget,
}

/// One link to a note from another.
#[derive(Debug, Clone)]
pub struct Backlink {
    /// The note the link is in.
    pub source: VaultPath,
    /// The link.
    pub link: IndexedLink,
    /// How it resolved (exact, case-fold, ambiguous, ...).
    pub status: Status,
    /// The headings around the link in the source, outermost first.
    pub heading_path: Vec<Arc<str>>,
}

/// A link whose target does not exist.
#[derive(Debug, Clone)]
pub struct UnresolvedLink {
    /// The note the link is in.
    pub source: VaultPath,
    /// The link.
    pub link: IndexedLink,
    /// [`Status::Unresolved`] or [`Status::Ambiguous`].
    pub status: Status,
}

/// One tag with its use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCount {
    /// The tag as first written (in path order), without `#`.
    pub name: Arc<str>,
    /// Occurrences (front matter and inline).
    pub count: usize,
    /// Notes that have it.
    pub notes: usize,
}

/// A quick-switcher entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitcherEntry {
    /// The file.
    pub path: VaultPath,
    /// The note's title (notes only).
    pub title: Option<Arc<str>>,
    /// For alias entries: the alias ("Alias → Note").
    pub alias: Option<Arc<str>>,
    /// A note (indexed or placeholder) rather than another file.
    pub is_note: bool,
}

/// A sync client's conflict copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConflictCopy {
    /// The copy.
    pub copy: VaultPath,
    /// The file it is a copy of.
    pub original: VaultPath,
    /// Whose pattern it matches.
    pub service: SyncService,
}

/// A text occurrence of a note's title or alias that is not a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// The note it is in.
    pub source: VaultPath,
    /// Where.
    pub span: Span,
    /// 0-based line.
    pub line: u32,
    /// The line, trimmed.
    pub context: Arc<str>,
    /// The title or alias it matches.
    pub term: Arc<str>,
}

impl ContentSnapshot {
    /// Increases with every update.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0.generation
    }

    /// The vault root.
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.0.paths.as_ref().map(PathIndex::root)
    }

    /// The path index the links were resolved against.
    #[must_use]
    pub fn path_index(&self) -> Option<&PathIndex> {
        self.0.paths.as_ref()
    }

    /// The record of `path` (an unsaved buffer's when there is one).
    #[must_use]
    pub fn note(&self, path: &VaultPath) -> Option<&NoteRecord> {
        self.0.effective(path).map(|r| &**r)
    }

    /// Every note (overlays replacing disk records), sorted by path.
    #[must_use]
    pub fn notes(&self) -> Vec<&NoteRecord> {
        let mut out: Vec<&NoteRecord> = self
            .0
            .disk
            .iter()
            .filter(|(p, _)| self.0.overlay.get(*p).is_none())
            .map(|(_, r)| &**r)
            .chain(self.0.overlay.iter().map(|(_, r)| &**r))
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    /// The number of notes.
    #[must_use]
    pub fn note_count(&self) -> usize {
        let extra = self
            .0
            .overlay
            .iter()
            .filter(|(p, _)| self.0.disk.get(*p).is_none())
            .count();
        self.0.disk.len() + extra
    }

    /// The links of `path` in order, with their targets.
    #[must_use]
    pub fn outgoing(&self, path: &VaultPath) -> Vec<OutgoingLink<'_>> {
        let (Some(record), Some(targets)) = (self.0.effective(path), self.0.resolved.get(path))
        else {
            return Vec::new();
        };
        record
            .data
            .links
            .iter()
            .enumerate()
            .filter_map(|(i, link)| {
                Some(OutgoingLink {
                    link,
                    target: targets.get(i)?,
                })
            })
            .collect()
    }

    /// The notes linking to `target`, without sync-conflict copies.
    #[must_use]
    pub fn backlink_sources(&self, target: &VaultPath) -> Vec<VaultPath> {
        let mut out: Vec<VaultPath> = self
            .0
            .incoming
            .get(target)
            .filter(|s| !self.is_conflict_copy(s))
            .cloned()
            .collect();
        out.sort();
        out
    }

    /// Every link to `target` from other notes (linked mentions), grouped
    /// by source in path order, then in text order. Sync-conflict copies
    /// are left out.
    #[must_use]
    pub fn backlinks(&self, target: &VaultPath) -> Vec<Backlink> {
        let mut out = Vec::new();
        for source in self.backlink_sources(target) {
            let Some(record) = self.0.effective(&source) else {
                continue;
            };
            for link in self.outgoing(&source) {
                if link.target.target.as_ref() == Some(target) {
                    out.push(Backlink {
                        source: source.clone(),
                        link: link.link.clone(),
                        status: link.target.status,
                        heading_path: record.heading_path(link.link.span.start as usize),
                    });
                }
            }
        }
        out
    }

    /// Links that resolve to nothing ([`Status::Unresolved`]) or to one of
    /// several candidates ([`Status::Ambiguous`]), by source and text
    /// order.
    #[must_use]
    pub fn unresolved(&self) -> Vec<UnresolvedLink> {
        let mut out = Vec::new();
        for record in self.notes() {
            if self.is_conflict_copy(&record.path) {
                continue;
            }
            for link in self.outgoing(&record.path) {
                if matches!(
                    link.target.status,
                    Status::Unresolved | Status::Ambiguous(_)
                ) {
                    out.push(UnresolvedLink {
                        source: record.path.clone(),
                        link: link.link.clone(),
                        status: link.target.status,
                    });
                }
            }
        }
        out
    }

    /// Names of notes that do not exist yet, with how many links point to
    /// each (the switcher's "create" suggestions), sorted by name.
    #[must_use]
    pub fn unresolved_targets(&self) -> Vec<(Arc<str>, usize)> {
        let mut counts: BTreeMap<String, (Arc<str>, usize)> = BTreeMap::new();
        for u in self.unresolved() {
            if u.status != Status::Unresolved {
                continue;
            }
            let raw = &*u.link.raw;
            let name = if u.link.kind.is_wiki() {
                raw.split('|')
                    .next()
                    .unwrap_or(raw)
                    .split('#')
                    .next()
                    .unwrap_or(raw)
            } else {
                raw.split(['#', '?']).next().unwrap_or(raw)
            }
            .trim();
            if name.is_empty() {
                continue;
            }
            let entry = counts
                .entry(fold_key(name))
                .or_insert_with(|| (Arc::from(name), 0));
            entry.1 += 1;
        }
        counts.into_values().collect()
    }

    /// Every tag with its counts, sorted case-insensitively. Nested tags
    /// (`a/b`) are listed as written; the tag pane builds the tree.
    #[must_use]
    pub fn tags(&self) -> Vec<TagCount> {
        let mut tags: BTreeMap<String, TagCount> = BTreeMap::new();
        for record in self.notes() {
            let mut seen = HashSet::new();
            for tag in &record.data.tags {
                let key = fold_key(&tag.name);
                let entry = tags.entry(key.clone()).or_insert_with(|| TagCount {
                    name: tag.name.clone(),
                    count: 0,
                    notes: 0,
                });
                entry.count += 1;
                if seen.insert(key) {
                    entry.notes += 1;
                }
            }
        }
        tags.into_values().collect()
    }

    /// Notes with `tag` (without `#`) or a tag nested under it, ignoring
    /// case, sorted.
    #[must_use]
    pub fn notes_with_tag(&self, tag: &str) -> Vec<VaultPath> {
        let key = fold_key(tag.trim_start_matches('#'));
        let nested = format!("{key}/");
        self.notes()
            .into_iter()
            .filter(|r| {
                r.data.tags.iter().any(|t| {
                    let k = fold_key(&t.name);
                    k == key || k.starts_with(&nested)
                })
            })
            .map(|r| r.path.clone())
            .collect()
    }

    /// The quick switcher's sources: every file of the path index, with
    /// titles for notes, plus one entry per alias.
    #[must_use]
    pub fn switcher_entries(&self) -> Vec<SwitcherEntry> {
        let mut out = Vec::new();
        let mut listed = HashSet::new();
        if let Some(paths) = &self.0.paths {
            for entry in paths.iter() {
                if entry.kind != EntryKind::File {
                    continue;
                }
                listed.insert(entry.path.clone());
                self.push_switcher(&entry.path, &mut out);
            }
        }
        // Unsaved notes not on disk yet.
        for (path, _) in self.0.overlay.iter() {
            if !listed.contains(path) {
                self.push_switcher(path, &mut out);
            }
        }
        out
    }

    fn push_switcher(&self, path: &VaultPath, out: &mut Vec<SwitcherEntry>) {
        let record = self.0.effective(path);
        out.push(SwitcherEntry {
            path: path.clone(),
            title: record.map(|r| Arc::from(r.title())),
            alias: None,
            is_note: record.is_some(),
        });
        if let Some(record) = record {
            for alias in &record.data.aliases {
                out.push(SwitcherEntry {
                    path: path.clone(),
                    title: Some(Arc::from(record.title())),
                    alias: Some(alias.clone()),
                    is_note: true,
                });
            }
        }
    }

    /// Online-only cloud files, indexed by path only.
    #[must_use]
    pub fn placeholders(&self) -> Vec<VaultPath> {
        let mut out: Vec<VaultPath> = self
            .0
            .disk
            .iter()
            .filter(|(_, r)| r.kind == RecordKind::Placeholder)
            .map(|(p, _)| p.clone())
            .collect();
        out.sort();
        out
    }

    /// Sync-conflict copies of notes in the vault (Dropbox, Syncthing,
    /// OneDrive, iCloud patterns), sorted by copy.
    #[must_use]
    pub fn sync_conflicts(&self) -> Vec<SyncConflictCopy> {
        let mut out: Vec<SyncConflictCopy> = self
            .0
            .disk
            .iter()
            .filter_map(|(p, _)| self.conflict_of(p))
            .collect();
        out.sort_by(|a, b| a.copy.cmp(&b.copy));
        out
    }

    fn conflict_of(&self, path: &VaultPath) -> Option<SyncConflictCopy> {
        let paths = self.0.paths.as_ref()?;
        let name = path.file_name()?;
        let parent = path.parent().unwrap_or_else(VaultPath::root);
        let sibling = |n: &str| parent.join(n).ok();
        let hit = detect_sync_conflict(name, |original| {
            sibling(original).is_some_and(|p| paths.entry_by_path(p.as_str()).is_some())
        })?;
        Some(SyncConflictCopy {
            copy: path.clone(),
            original: sibling(&hit.original)?,
            service: hit.service,
        })
    }

    fn is_conflict_copy(&self, path: &VaultPath) -> bool {
        self.conflict_of(path).is_some()
    }

    /// The text of `path` as the index sees it: an unsaved buffer's text,
    /// or the file read through `source` (for context lines). `None` when
    /// it is not a note of the index or cannot be read.
    #[must_use]
    pub fn note_text(&self, path: &VaultPath, source: &dyn FileSource) -> Option<String> {
        let record = self.0.effective(path)?;
        if !matches!(record.kind, RecordKind::Indexed | RecordKind::Overlay) {
            return None;
        }
        match self.0.overlay_text.get(path) {
            Some(text) => Some(text.to_string()),
            None => source
                .read(path)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        }
    }

    /// Occurrences of `target`'s title, file name and aliases in other
    /// notes that are not links (unlinked mentions): whole words, ignoring
    /// ASCII case, outside code, math, comments, front matter and links.
    /// Reads every note (overlays from memory) on the snapshot's worker
    /// threads; `cancel` stops it.
    pub fn unlinked_mentions(
        &self,
        target: &VaultPath,
        source: &dyn FileSource,
        cancel: &AtomicBool,
    ) -> Result<Vec<Mention>, Cancelled> {
        let Some(record) = self.0.effective(target) else {
            return Ok(Vec::new());
        };
        let mut terms: Vec<Arc<str>> = Vec::new();
        for t in std::iter::once(record.title())
            .chain(std::iter::once(file_stem(target)))
            .chain(record.data.aliases.iter().map(|a| &**a))
        {
            let t = t.trim();
            if t.chars().count() >= 2 && !terms.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                terms.push(t.into());
            }
        }
        let sources: Vec<&NoteRecord> = self
            .notes()
            .into_iter()
            .filter(|r| {
                &r.path != target
                    && matches!(r.kind, RecordKind::Indexed | RecordKind::Overlay)
                    && !self.is_conflict_copy(&r.path)
            })
            .collect();
        let overlay_text = &self.0.overlay_text;
        let found = parallel_map(&sources, self.0.options.threads, cancel, |r| {
            let text = match overlay_text.get(&r.path) {
                Some(t) => t.to_string(),
                None => match source.read(&r.path) {
                    Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Err(_) => return Vec::new(),
                },
            };
            mentions_in(r, &text, &terms)
        })?;
        Ok(found.into_iter().flatten().collect())
    }

    /// An estimate of the heap the index holds (records, link maps and
    /// hash tables; shared path strings counted once per record), for
    /// memory budgets. Allocator overhead is not included.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        use std::mem::size_of;
        let arc_str = |s: &str| s.len() + 16;
        let strs = |v: &[Arc<str>]| v.iter().map(|s| arc_str(s) + 16).sum::<usize>();
        let record = |r: &NoteRecord| {
            let d = &r.data;
            size_of::<NoteRecord>()
                + 16
                + arc_str(r.path.as_str())
                + d.title.as_deref().map_or(0, arc_str)
                + d.headings
                    .iter()
                    .map(|h| {
                        size_of::<super::IndexedHeading>()
                            + arc_str(&h.text)
                            + if Arc::ptr_eq(&h.text, &h.source) {
                                0
                            } else {
                                arc_str(&h.source)
                            }
                    })
                    .sum::<usize>()
                + d.links.len() * size_of::<IndexedLink>()
                + d.tags.len() * size_of::<super::IndexedTag>()
                + strs(&d.aliases)
                + strs(&d.cssclasses)
                + d.properties.len() * size_of::<super::Property>()
                + d.block_ids
                    .iter()
                    .map(|b| size_of::<super::IndexedBlockId>() + arc_str(&b.id))
                    .sum::<usize>()
                + d.tasks
                    .iter()
                    .map(|t| size_of::<super::IndexedTask>() + arc_str(&t.text))
                    .sum::<usize>()
                + strs(&d.footnotes)
                + d.skip.len() * size_of::<Span>()
        };
        let table = |n: usize, entry: usize| n * (entry + 1) * 8 / 7;
        let mut total = 0;
        for (_, r) in self.0.disk.iter().chain(self.0.overlay.iter()) {
            total += record(r);
        }
        total += table(self.0.disk.len() + self.0.overlay.len(), 32);
        for (_, r) in self.0.resolved.iter() {
            total += size_of::<super::state::Resolved>()
                + 16
                + r.targets.len() * size_of::<super::state::Packed>()
                + r.paths.len() * 16
                + r.keys.len() * 16;
        }
        // Shared strings, counted once.
        let strings = self.0.strings.lock().map_or(0, |s| s.bytes());
        total += strings;
        total += table(self.0.resolved.len(), 32);
        let (incoming, by_key) = (self.0.incoming.stats(), self.0.by_key.stats());
        for (keys, values) in [incoming, by_key] {
            total += table(keys, 32) + keys * 64 + table(values, 16);
        }
        total
    }

    /// A text dump of everything the index holds, for comparing two
    /// indexes in tests.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_dump(&self) -> String {
        let mut out = String::new();
        for r in self.notes() {
            let _ = writeln!(
                out,
                "{} {:?} size={} mtime={:?}\n  {:?}",
                r.path, r.kind, r.stamp.size, r.stamp.mtime_ns, r.data
            );
            if let Some(resolved) = self.0.resolved.get(&r.path) {
                let targets: Vec<LinkTarget> = self
                    .outgoing(&r.path)
                    .into_iter()
                    .map(|o| o.target)
                    .collect();
                let _ = writeln!(out, "  -> {targets:?}\n  keys {:?}", resolved.keys);
            }
            let _ = writeln!(out, "  <- {:?}", self.backlink_sources(&r.path));
        }
        out
    }
}

fn mentions_in(record: &NoteRecord, text: &str, terms: &[Arc<str>]) -> Vec<Mention> {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let excluded = |at: usize, end: usize| {
        record
            .data
            .links
            .iter()
            .map(|l| l.span)
            .chain(record.data.skip.iter().copied())
            .any(|s| (s.start as usize) < end && at < s.end as usize)
    };
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut out = Vec::new();
    for term in terms {
        let needle = term.to_ascii_lowercase();
        let mut from = 0;
        while let Some(at) = lower[from..].find(&needle).map(|i| i + from) {
            let end = at + needle.len();
            from = end;
            let before = text[..at].chars().next_back();
            let after = text[end..].chars().next();
            if is_word(before) || is_word(after) || excluded(at, end) {
                continue;
            }
            let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[end..].find('\n').map_or(text.len(), |i| i + end);
            let line = bytes[..at].iter().filter(|&&b| b == b'\n').count();
            out.push(Mention {
                source: record.path.clone(),
                span: Span::of(at..end),
                line: u32::try_from(line).unwrap_or(u32::MAX),
                context: text[line_start..line_end].trim().into(),
                term: term.clone(),
            });
        }
    }
    out.sort_by_key(|m| m.span.start);
    out
}

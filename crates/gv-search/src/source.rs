// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What a search scans: notes of the content index and their text.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use gv_vault::VaultPath;
use gv_vault::content::{ContentSnapshot, FileSource, FileStamp, NoteRecord, RecordKind, extract};
use gv_vault::profile::ProfileId;

/// The notes a search scans.
///
/// Metadata (tags, properties, headings, tasks) comes from the index
/// records; the text is loaded only when the query needs it, so
/// `file:`, `path:` and `[property]` queries read nothing.
pub trait SearchSource: Sync {
    /// Every note to scan.
    fn records(&self) -> Vec<&NoteRecord>;

    /// The text of `record` (an unsaved buffer's when there is one), or
    /// `None` for path-only records (placeholders, too large, unreadable).
    fn text<'a>(&'a self, record: &'a NoteRecord) -> Option<Cow<'a, str>>;
}

/// A content-index snapshot, reading note text through a [`FileSource`]
/// (unsaved buffers from memory). Owns both, so it can move to a worker
/// thread.
#[derive(Clone)]
pub struct VaultSource {
    snapshot: ContentSnapshot,
    files: Arc<dyn FileSource>,
}

impl std::fmt::Debug for VaultSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultSource")
            .field("snapshot", &self.snapshot)
            .finish_non_exhaustive()
    }
}

impl VaultSource {
    /// The notes of `snapshot`, read through `files`
    /// (normally a [`gv_vault::content::DiskSource`]).
    #[must_use]
    pub fn new(snapshot: ContentSnapshot, files: Arc<dyn FileSource>) -> Self {
        Self { snapshot, files }
    }

    /// The snapshot searched.
    #[must_use]
    pub fn snapshot(&self) -> &ContentSnapshot {
        &self.snapshot
    }
}

impl SearchSource for VaultSource {
    fn records(&self) -> Vec<&NoteRecord> {
        self.snapshot.notes()
    }

    fn text<'a>(&'a self, record: &'a NoteRecord) -> Option<Cow<'a, str>> {
        self.snapshot
            .note_text(&record.path, &*self.files)
            .map(Cow::Owned)
    }
}

/// Notes held in memory: tests, benchmarks, and searches over a fixed
/// set of texts.
#[derive(Debug, Default, Clone)]
pub struct MemorySource {
    notes: Vec<NoteRecord>,
    texts: HashMap<VaultPath, Arc<str>>,
}

impl MemorySource {
    /// An empty source.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a record with its text.
    pub fn push(&mut self, record: NoteRecord, text: impl Into<Arc<str>>) {
        self.texts.insert(record.path.clone(), text.into());
        self.notes.push(record);
    }

    /// Adds `text` at `path`, indexed with `profile`'s parser. Invalid
    /// paths are skipped.
    pub fn add(&mut self, path: &str, text: &str, profile: ProfileId) {
        let Ok(path) = VaultPath::new(path) else {
            return;
        };
        let record = NoteRecord {
            path,
            stamp: FileStamp {
                size: text.len() as u64,
                ..FileStamp::default()
            },
            kind: RecordKind::Indexed,
            data: extract(text, profile),
        };
        self.push(record, text);
    }

    /// Sets the modification time of the note at `path` (ranking).
    pub fn set_mtime(&mut self, path: &str, mtime_ns: i64) {
        for r in &mut self.notes {
            if r.path.as_str() == path {
                r.stamp.mtime_ns = Some(mtime_ns);
            }
        }
    }

    /// The number of notes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }
}

impl SearchSource for MemorySource {
    fn records(&self) -> Vec<&NoteRecord> {
        self.notes.iter().collect()
    }

    fn text<'a>(&'a self, record: &'a NoteRecord) -> Option<Cow<'a, str>> {
        if !matches!(record.kind, RecordKind::Indexed | RecordKind::Overlay) {
            return None;
        }
        self.texts.get(&record.path).map(|t| Cow::Borrowed(&**t))
    }
}

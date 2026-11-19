// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The search panel's results: the matching notes, ranked, their rows,
//! which notes are folded, and the selected row. No GPUI here.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gv_search::{FileResult, SearchSummary, rank};
use gv_vault::VaultPath;
use gv_vault::content::ContentSnapshot;

use super::macros::NoteMacros;

/// One row of the result list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Row {
    /// A matching note: chevron, icon, title, match count.
    File(usize),
    /// A context line of note `.0`: snippet `.1`.
    Match(usize, usize),
}

/// What a selection points at; it survives re-ranking and new batches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RowKey {
    pub path: VaultPath,
    pub snippet: Option<usize>,
}

/// How the current search is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Progress {
    /// No search (an empty query, an invalid one, or no vault).
    #[default]
    Idle,
    /// Notes scanned so far, of `total`.
    Running {
        scanned: usize,
        total: usize,
    },
    Done(SearchSummary),
}

/// The results of one search.
#[derive(Debug, Default)]
pub(crate) struct Results {
    files: Vec<FileResult>,
    /// The heading path of each snippet, outermost first (" › " joined),
    /// by note.
    headings: HashMap<VaultPath, Vec<Option<Arc<str>>>>,
    /// The math macros of the notes with a formula in a context line
    /// (worked out with the search, off the UI thread).
    macros: HashMap<VaultPath, Arc<NoteMacros>>,
    folded: HashSet<VaultPath>,
    selected: Option<RowKey>,
    rows: Vec<Row>,
    pub progress: Progress,
}

impl Results {
    /// Forgets the results, keeping which notes are folded.
    pub fn clear(&mut self) {
        self.files.clear();
        self.headings.clear();
        self.macros.clear();
        self.rows.clear();
        self.progress = Progress::Idle;
    }

    /// Adds notes found by the running search and ranks the whole list
    /// again. `content` gives the heading paths.
    pub fn add(&mut self, files: Vec<FileResult>, content: Option<&ContentSnapshot>) {
        for file in &files {
            let record = content.and_then(|c| c.note(&file.path));
            let headings = file
                .snippets
                .iter()
                .map(|s| {
                    let path = record?.heading_path(s.range.start);
                    (!path.is_empty()).then(|| {
                        let joined: Vec<&str> = path.iter().map(AsRef::as_ref).collect();
                        Arc::from(joined.join(" › "))
                    })
                })
                .collect();
            self.headings.insert(file.path.clone(), headings);
        }
        self.files.extend(files);
        rank(&mut self.files);
        self.rebuild();
    }

    /// Adds the math macros of notes found (see [`super::macros`]).
    pub fn add_macros(&mut self, macros: Vec<(VaultPath, Arc<NoteMacros>)>) {
        self.macros.extend(macros);
    }

    /// The math macros of note `file`, when it defines any.
    pub fn macros(&self, file: usize) -> Option<&Arc<NoteMacros>> {
        self.macros.get(&self.files.get(file)?.path)
    }

    pub fn files(&self) -> &[FileResult] {
        &self.files
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The heading path of snippet `snippet` of note `file`.
    pub fn heading(&self, file: usize, snippet: usize) -> Option<&Arc<str>> {
        let path = &self.files.get(file)?.path;
        self.headings.get(path)?.get(snippet)?.as_ref()
    }

    /// Content matches over every note (counted past the highlight limit).
    pub fn match_count(&self) -> usize {
        self.files.iter().map(|f| f.match_count.max(1)).sum()
    }

    pub fn is_folded(&self, file: usize) -> bool {
        self.files
            .get(file)
            .is_some_and(|f| self.folded.contains(&f.path))
    }

    /// Whether every note with context lines is folded.
    pub fn all_folded(&self) -> bool {
        self.files
            .iter()
            .filter(|f| !f.snippets.is_empty())
            .all(|f| self.folded.contains(&f.path))
    }

    /// Folds or unfolds note `file`.
    pub fn toggle(&mut self, file: usize) {
        let Some(path) = self.files.get(file).map(|f| f.path.clone()) else {
            return;
        };
        if !self.folded.remove(&path) {
            self.folded.insert(path.clone());
            // A selected line of a folded note gives way to its note.
            if self.selected.as_ref().is_some_and(|s| s.path == path) {
                self.selected = Some(RowKey {
                    path,
                    snippet: None,
                });
            }
        }
        self.rebuild();
    }

    /// Folds every note, or unfolds every note when all are folded.
    pub fn toggle_all(&mut self) {
        if self.all_folded() {
            self.folded.clear();
        } else {
            self.folded = self.files.iter().map(|f| f.path.clone()).collect();
            if let Some(selected) = &mut self.selected {
                selected.snippet = None;
            }
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        for (i, file) in self.files.iter().enumerate() {
            self.rows.push(Row::File(i));
            if !self.folded.contains(&file.path) {
                self.rows
                    .extend((0..file.snippets.len()).map(|s| Row::Match(i, s)));
            }
        }
    }

    pub fn key(&self, row: Row) -> Option<RowKey> {
        match row {
            Row::File(f) => Some(RowKey {
                path: self.files.get(f)?.path.clone(),
                snippet: None,
            }),
            Row::Match(f, s) => Some(RowKey {
                path: self.files.get(f)?.path.clone(),
                snippet: Some(s),
            }),
        }
    }

    /// The row of `key`, if it is shown.
    fn row_of(&self, key: &RowKey) -> Option<usize> {
        let file = self.files.iter().position(|f| f.path == key.path)?;
        self.rows.iter().position(|r| match (*r, key.snippet) {
            (Row::File(f), None) => f == file,
            (Row::Match(f, s), Some(k)) => f == file && s == k,
            _ => false,
        })
    }

    /// The index of the selected row, if it is shown.
    pub fn selected_row(&self) -> Option<usize> {
        self.row_of(self.selected.as_ref()?)
    }

    pub fn select(&mut self, row: usize) {
        self.selected = self.rows.get(row).and_then(|r| self.key(*r));
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// Moves the selection one row down (`forward`) or up; the first or
    /// last row when nothing is selected. Returns the new row.
    pub fn step(&mut self, forward: bool) -> Option<usize> {
        if self.rows.is_empty() {
            return None;
        }
        let last = self.rows.len() - 1;
        let row = match (self.selected_row(), forward) {
            (None, true) => 0,
            (None, false) => last,
            (Some(r), true) => (r + 1).min(last),
            (Some(r), false) => r.saturating_sub(1),
        };
        self.select(row);
        Some(row)
    }

    /// What Enter opens: the selected row, or the first context line
    /// (the first note when it has none).
    pub fn target(&self) -> Option<Row> {
        match self.selected_row() {
            Some(row) => self.rows.get(row).copied(),
            None => self
                .rows
                .iter()
                .find(|r| matches!(r, Row::Match(..)))
                .or(self.rows.first())
                .copied(),
        }
    }

    /// Left on a row: folds its note, or from a context line selects
    /// the note's row.
    pub fn collapse(&mut self) {
        let Some(row) = self.selected_row().and_then(|r| self.rows.get(r).copied()) else {
            return;
        };
        match row {
            Row::File(f) if !self.is_folded(f) => self.toggle(f),
            Row::Match(f, _) => {
                self.selected = self.key(Row::File(f));
            }
            Row::File(_) => {}
        }
    }

    /// Right on a folded note's row unfolds it.
    pub fn expand(&mut self) {
        if let Some(Row::File(f)) = self.selected_row().and_then(|r| self.rows.get(r).copied())
            && self.is_folded(f)
        {
            self.toggle(f);
        }
    }

    /// The summary line under the field.
    pub fn summary(&self) -> String {
        let files = self.files.len();
        let matches = self.match_count();
        let found = match (matches, files) {
            (0, _) => "No results".to_owned(),
            (m, f) => format!(
                "{} {} in {} {}",
                grouped(m),
                if m == 1 { "result" } else { "results" },
                grouped(f),
                if f == 1 { "file" } else { "files" },
            ),
        };
        match self.progress {
            Progress::Idle => String::new(),
            Progress::Running { scanned, total } => {
                let found = if files == 0 {
                    "Searching".to_owned()
                } else {
                    found
                };
                format!("{found} · {} of {} notes", grouped(scanned), grouped(total))
            }
            Progress::Done(summary) if summary.limit_reached => {
                format!("{found} (search stopped at {} files)", grouped(files))
            }
            Progress::Done(_) => found,
        }
    }

    /// The results as a Markdown list of wikilinks, one per note.
    pub fn as_markdown_links(&self) -> String {
        let mut out = String::new();
        for file in &self.files {
            let path = file.path.as_str();
            let target = path
                .strip_suffix(".md")
                .or_else(|| path.strip_suffix(".markdown"))
                .unwrap_or(path);
            out.push_str("- [[");
            out.push_str(target);
            out.push_str("]]\n");
        }
        out
    }
}

/// `n` with thousands separators (the interface is English).
pub(crate) fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

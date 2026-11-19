// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Running a query: the parallel scan, streamed result batches,
//! cancellation, limits, context snippets and ranking.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use gv_vault::VaultPath;
use gv_vault::content::NoteRecord;
use rayon::prelude::*;

use crate::matcher::{self, Doc, Hits, Needle, Node, QueryError};
use crate::query::{self, Expr};
use crate::source::SearchSource;

/// A compiled query, ready to run on any number of threads.
#[derive(Debug, Clone)]
pub struct Query {
    text: String,
    expr: Option<Expr>,
    node: Option<Arc<Node>>,
    positive: Arc<Vec<Arc<Needle>>>,
}

impl Query {
    /// Parses and compiles `text`. Terms ignore case unless `match_case`
    /// (the panel's toggle) or a `match-case:` operator says otherwise.
    ///
    /// # Errors
    ///
    /// [`QueryError::Regex`] when a `/regex/` does not compile.
    pub fn new(text: &str, match_case: bool) -> Result<Self, QueryError> {
        let expr = query::parse(text);
        let mut positive = Vec::new();
        let node = match &expr {
            Some(e) => Some(Arc::new(matcher::compile(
                e,
                match_case,
                &mut positive,
                false,
            )?)),
            None => None,
        };
        Ok(Self {
            text: text.to_owned(),
            expr,
            node,
            positive: Arc::new(positive),
        })
    }

    /// The query as typed.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The parsed query; `None` when it is empty (it matches nothing).
    #[must_use]
    pub fn expr(&self) -> Option<&Expr> {
        self.expr.as_ref()
    }

    /// Whether the query is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.node.is_none()
    }

    /// Matches one note; `text` loads its text when the query needs it.
    /// `None` when it does not match.
    #[must_use]
    pub fn match_note<'a>(
        &self,
        record: &'a NoteRecord,
        text: &'a (dyn Fn() -> Option<Cow<'a, str>> + 'a),
        options: &SearchOptions,
    ) -> Option<FileResult> {
        let node = self.node.as_deref()?;
        let doc = Doc::new(record, text);
        if !doc.eval(node, None) {
            return None;
        }
        let mut hits = Hits::new(options.max_matches_per_file);
        doc.eval(node, Some(&mut hits));
        Some(self.result(&doc, hits, options))
    }

    fn result(&self, doc: &Doc<'_>, mut hits: Hits, options: &SearchOptions) -> FileResult {
        let record = doc.record;
        let title = record.title();
        let path = doc.path();
        let name_start = path.rfind('/').map_or(0, |i| i + 1);
        hits.path.sort_by_key(|r| (r.start, r.end));
        hits.path = merge(std::mem::take(&mut hits.path));
        let name_match = hits.path.iter().any(|r| r.start >= name_start);
        let title_match = self.positive.iter().any(|n| n.is_match(title));
        let content = merge({
            let mut c = std::mem::take(&mut hits.content);
            c.sort_by_key(|r| (r.start, r.end));
            c
        });
        let snippets = if content.is_empty() {
            Vec::new()
        } else {
            snippets(doc, &content, options)
        };
        let text_len = if doc.text_loaded() {
            doc.text().len()
        } else {
            usize::try_from(record.stamp.size).unwrap_or(usize::MAX)
        };
        FileResult {
            path: record.path.clone(),
            title: title.into(),
            mtime_ns: record.stamp.mtime_ns,
            text_len,
            match_count: hits.count.max(content.len()),
            path_hits: hits.path,
            name_match,
            title_match,
            snippets,
        }
    }
}

/// Limits and batching of a search.
#[derive(Debug, Clone)]
pub struct SearchOptions {
    /// Stop after this many matching notes.
    pub max_files: usize,
    /// Highlight ranges kept per note (matches past it are only counted).
    pub max_matches_per_file: usize,
    /// Context snippets per note.
    pub max_snippets_per_file: usize,
    /// Bytes of context kept on each side of the first match of a long
    /// line.
    pub context_bytes: usize,
    /// A batch is sent when it holds this many notes...
    pub batch_size: usize,
    /// ...or when this long has passed since the last one. The first
    /// result is sent at once.
    pub batch_interval: Duration,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            max_files: 2_000,
            max_matches_per_file: 200,
            max_snippets_per_file: 20,
            context_bytes: 80,
            batch_size: 64,
            batch_interval: Duration::from_millis(50),
        }
    }
}

/// Stops a running search. Cloning shares the flag: the panel keeps one
/// clone and cancels it when the next keystroke starts a new query.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A token cancelled through `flag` (one that other work shares).
    #[must_use]
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// Cancels the search.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// Whether it was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// The flag, for APIs taking an `AtomicBool`
    /// (`ContentSnapshot::unlinked_mentions`).
    #[must_use]
    pub fn as_flag(&self) -> &AtomicBool {
        &self.0
    }
}

/// One line of context around matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    /// 0-based line in the note.
    pub line: u32,
    /// The bytes of the note shown in `text`.
    pub range: Range<usize>,
    /// The context text (part of one line).
    pub text: String,
    /// Match ranges within `text`.
    pub highlights: Vec<Range<usize>>,
    /// The line goes on before `text` (show an ellipsis).
    pub clipped_start: bool,
    /// The line goes on after `text`.
    pub clipped_end: bool,
}

/// One matching note.
#[derive(Debug, Clone, PartialEq)]
pub struct FileResult {
    /// The note.
    pub path: VaultPath,
    /// Its display title.
    pub title: Arc<str>,
    /// Its modification time, if known.
    pub mtime_ns: Option<i64>,
    /// Its text length in bytes.
    pub text_len: usize,
    /// Content matches, counted past the highlight limit.
    pub match_count: usize,
    /// Match ranges in the vault-relative path string.
    pub path_hits: Vec<Range<usize>>,
    /// The file name matched.
    pub name_match: bool,
    /// The title matched a search term.
    pub title_match: bool,
    /// Context lines in text order.
    pub snippets: Vec<Snippet>,
}

/// Results streamed while a search runs.
#[derive(Debug, Clone, Default)]
pub struct ResultBatch {
    /// Notes matched since the previous batch, in no particular order.
    pub files: Vec<FileResult>,
    /// Notes scanned so far.
    pub scanned: usize,
    /// Notes to scan.
    pub total: usize,
}

/// How a search ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchSummary {
    /// Notes that matched (at most [`SearchOptions::max_files`]).
    pub files_matched: usize,
    /// Content matches over those notes.
    pub matches: usize,
    /// Notes scanned.
    pub scanned: usize,
    /// Notes to scan.
    pub total: usize,
    /// The token was cancelled; no batch was sent after that.
    pub cancelled: bool,
    /// [`SearchOptions::max_files`] was reached and the scan stopped.
    pub limit_reached: bool,
    /// Wall-clock time.
    pub elapsed: Duration,
    /// Time to the first batch, when there was one.
    pub first_batch: Option<Duration>,
}

/// Runs `query` over `source` in parallel (on `pool`, or rayon's global
/// pool), calling `on_batch` on this thread as results arrive. Returns
/// when the scan has finished, was cancelled, or hit the file limit.
///
/// Meant to run on a background thread: each new keystroke cancels the
/// previous search's token and starts a new one.
pub fn search<S: SearchSource + ?Sized>(
    query: &Query,
    source: &S,
    options: &SearchOptions,
    cancel: &CancelToken,
    pool: Option<&rayon::ThreadPool>,
    mut on_batch: impl FnMut(ResultBatch),
) -> SearchSummary {
    let started = Instant::now();
    let records = source.records();
    let total = records.len();
    let mut summary = SearchSummary {
        total,
        ..SearchSummary::default()
    };
    let Some(node) = query.node.as_deref() else {
        summary.elapsed = started.elapsed();
        return summary;
    };
    if cancel.is_cancelled() {
        summary.cancelled = true;
        summary.elapsed = started.elapsed();
        return summary;
    }
    let scanned = AtomicUsize::new(0);
    let matched = AtomicUsize::new(0);
    let limit_hit = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<FileResult>();

    std::thread::scope(|scope| {
        let records = &records;
        let scanned = &scanned;
        let matched = &matched;
        let limit_hit = &limit_hit;
        scope.spawn(move || {
            let work = move || {
                let _ = records.par_iter().try_for_each_with(tx, |tx, record| {
                    if cancel.is_cancelled() || limit_hit.load(Ordering::Relaxed) {
                        return Err(());
                    }
                    let load = || source.text(record);
                    let doc = Doc::new(record, &load);
                    let hit = doc.eval(node, None);
                    if hit {
                        if matched.fetch_add(1, Ordering::Relaxed) >= options.max_files {
                            limit_hit.store(true, Ordering::Relaxed);
                            scanned.fetch_add(1, Ordering::Relaxed);
                            return Err(());
                        }
                        let mut hits = Hits::new(options.max_matches_per_file);
                        doc.eval(node, Some(&mut hits));
                        let result = query.result(&doc, hits, options);
                        scanned.fetch_add(1, Ordering::Relaxed);
                        tx.send(result).map_err(|_| ())
                    } else {
                        scanned.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                });
            };
            match pool {
                Some(p) => p.install(work),
                None => work(),
            }
        });

        let mut pending: Vec<FileResult> = Vec::new();
        let mut last_flush = Instant::now();
        let mut flush = |pending: &mut Vec<FileResult>, summary: &mut SearchSummary| {
            if pending.is_empty() || cancel.is_cancelled() {
                return;
            }
            summary.first_batch.get_or_insert_with(|| started.elapsed());
            on_batch(ResultBatch {
                files: std::mem::take(pending),
                scanned: scanned.load(Ordering::Relaxed),
                total,
            });
        };
        loop {
            match rx.recv_timeout(options.batch_interval) {
                Ok(result) => {
                    summary.files_matched += 1;
                    summary.matches += result.match_count;
                    pending.push(result);
                    if summary.first_batch.is_none()
                        || pending.len() >= options.batch_size
                        || last_flush.elapsed() >= options.batch_interval
                    {
                        flush(&mut pending, &mut summary);
                        last_flush = Instant::now();
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    flush(&mut pending, &mut summary);
                    last_flush = Instant::now();
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        flush(&mut pending, &mut summary);
    });

    summary.scanned = scanned.load(Ordering::Relaxed);
    summary.cancelled = cancel.is_cancelled();
    summary.limit_reached = limit_hit.load(Ordering::Relaxed);
    summary.elapsed = started.elapsed();
    summary
}

/// Runs `query` to the end and returns the ranked results (`query` code
/// blocks, tests).
pub fn search_all<S: SearchSource + ?Sized>(
    query: &Query,
    source: &S,
    options: &SearchOptions,
    cancel: &CancelToken,
) -> (Vec<FileResult>, SearchSummary) {
    let mut out = Vec::new();
    let summary = search(query, source, options, cancel, None, |batch| {
        out.extend(batch.files);
    });
    rank(&mut out);
    (out, summary)
}

/// Sorts results for display: file-name and title matches first, then
/// match density, then the most recently modified, then by path.
pub fn rank(results: &mut [FileResult]) {
    results.sort_by(|a, b| {
        let named = |r: &FileResult| r.name_match || r.title_match;
        named(b)
            .cmp(&named(a))
            .then_with(|| density(b).total_cmp(&density(a)))
            .then_with(|| b.mtime_ns.cmp(&a.mtime_ns))
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// Matches per kilobyte.
fn density(r: &FileResult) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let d = r.match_count as f64 * 1024.0 / r.text_len.max(1) as f64;
    d
}

/// Sorted ranges with overlaps merged.
fn merge(sorted: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::with_capacity(sorted.len());
    for r in sorted {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// Context snippets for sorted, merged content `hits`: one per line.
fn snippets(doc: &Doc<'_>, hits: &[Range<usize>], options: &SearchOptions) -> Vec<Snippet> {
    let text = doc.text();
    let mut out: Vec<Snippet> = Vec::new();
    let mut i = 0;
    while i < hits.len() && out.len() < options.max_snippets_per_file {
        let line = doc.line_of(hits[i].start);
        let Some((ls, le)) = doc.line_range(line) else {
            break;
        };
        // The hits starting on this line (a hit may run past its end).
        let mut j = i;
        while j < hits.len() && hits[j].start <= le.max(ls) && hits[j].start >= ls {
            j += 1;
        }
        let j = j.max(i + 1);
        let on_line = &hits[i..j];
        let max_len = options.context_bytes.saturating_mul(3).max(16);
        let (mut s, mut e) = (ls, le);
        if e - s > max_len {
            let first = on_line[0].start;
            s = first.saturating_sub(options.context_bytes).max(ls);
            e = (s + max_len).min(le);
            s = floor_boundary(text, s);
            e = ceil_boundary(text, e);
        }
        let highlights = on_line
            .iter()
            .filter(|h| h.start < e)
            .map(|h| h.start.max(s) - s..h.end.min(e) - s)
            .filter(|h| !h.is_empty())
            .collect();
        out.push(Snippet {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            range: s..e,
            text: text[s..e].to_owned(),
            highlights,
            clipped_start: s > ls,
            clipped_end: e < le,
        });
        i = j;
    }
    out
}

fn floor_boundary(text: &str, mut i: usize) -> usize {
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(text: &str, mut i: usize) -> usize {
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The texts of embedded notes (`![[Note]]`), and keeping embeds current.
//!
//! - [`text`] gives a note's text: the unsaved buffer when the note is
//!   open with changes ([`crate::files::overlay`]), else the file, kept in
//!   a process-wide cache keyed by path and checked against the file's
//!   modification time and size, so building an embed again does not read
//!   an unchanged note again. The cache holds at most [`CACHE_BYTES`] of
//!   text, dropping the least recently used notes first. It touches the
//!   disk: only builds off the UI thread call it.
//! - [`changed`] drops a note from the cache and tells the Reading views
//!   (the vault watcher saw the file change; a buffer was edited, saved or
//!   closed). A view that shows embeds checks for changes every
//!   [`EMBED_DEBOUNCE`] and builds the embeds of changed notes again off
//!   the UI thread,
//!   in place when it can ([`RenderTree::rebuild_embeds_where`]).
//! - Other jobs that ask the embedded notes again (images of embeds that
//!   landed) run off the UI thread the same way ([`TreeJob`]): a result
//!   is applied only to the tree it was computed from, else the job runs
//!   again on the current one.
//!
//! [`RenderTree::rebuild_embeds_where`]: gv_render_tree::RenderTree::rebuild_embeds_where

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use gpui::Context;
use gv_render_tree::{FrameKind, LinkTarget, RenderTree, UpdateReport};

use super::Reader;
use super::links::Resolver;
use super::load::{self, Loaded};

/// The largest note an embed shows (the render tree's budget).
pub(crate) const MAX_NOTE_BYTES: u64 = 2 << 20;

/// The most text the cache keeps.
pub(crate) const CACHE_BYTES: usize = 16 << 20;

/// The changes kept for views that have not checked yet.
const CHANGE_LOG: usize = 1024;

/// How often a Reading view that shows embeds checks for changes of
/// notes, so it builds its embeds again at most this long after a change
/// (typing in the note's editor reports every batch of edits).
pub(crate) const EMBED_DEBOUNCE: Duration = Duration::from_millis(300);

struct Entry {
    modified: SystemTime,
    size: u64,
    text: Arc<str>,
    /// The cache's clock at the last use.
    used: u64,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<PathBuf, Entry>,
    bytes: usize,
    clock: u64,
    /// The last changes ([`changed`]), numbered, oldest first.
    changes: VecDeque<(u64, PathBuf)>,
    /// The number of the last change.
    change: u64,
    /// Files read, by path (tests count them).
    #[cfg(test)]
    reads: HashMap<PathBuf, usize>,
}

impl Cache {
    fn remove(&mut self, path: &Path) {
        if let Some(e) = self.entries.remove(path) {
            self.bytes -= e.text.len();
        }
    }

    fn insert(&mut self, path: PathBuf, modified: SystemTime, size: u64, text: Arc<str>) {
        self.remove(&path);
        if text.len() > CACHE_BYTES {
            return;
        }
        self.clock += 1;
        self.bytes += text.len();
        let used = self.clock;
        self.entries.insert(
            path,
            Entry {
                modified,
                size,
                text,
                used,
            },
        );
        while self.bytes > CACHE_BYTES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(p, _)| p.clone())
            else {
                break;
            };
            self.remove(&oldest);
        }
    }
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

fn cache() -> MutexGuard<'static, Cache> {
    CACHE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The text of the note at `path` (absolute): its unsaved buffer, else
/// the file (from the cache while the file is unchanged). `None` for a
/// missing or unreadable file, or one over [`MAX_NOTE_BYTES`].
pub(crate) fn text(path: &Path) -> Option<Arc<str>> {
    if let Some(buffer) = crate::files::overlay::unsaved(path) {
        if buffer.len() as u64 > MAX_NOTE_BYTES {
            return None;
        }
        return Some(Arc::from(
            buffer.text_for_range(0..buffer.len()).unwrap_or_default(),
        ));
    }
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_NOTE_BYTES {
        return None;
    }
    if let Ok(modified) = meta.modified() {
        let mut c = cache();
        c.clock += 1;
        let now = c.clock;
        if let Some(e) = c.entries.get_mut(path)
            && e.size == meta.len()
            && e.modified == modified
        {
            e.used = now;
            return Some(Arc::clone(&e.text));
        }
    }
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if meta.len() > MAX_NOTE_BYTES {
        return None;
    }
    let mut s = String::new();
    file.read_to_string(&mut s).ok()?;
    let text: Arc<str> = s.into();
    let mut c = cache();
    #[cfg(test)]
    {
        *c.reads.entry(path.to_path_buf()).or_default() += 1;
    }
    if let Ok(modified) = meta.modified() {
        c.insert(path.to_path_buf(), modified, meta.len(), Arc::clone(&text));
    }
    Some(text)
}

/// The note at `path` (absolute) changed: on disk, or its unsaved buffer.
/// It is read again when next asked, and the Reading views that embed it
/// build their embeds again.
pub(crate) fn changed(path: &Path) {
    let mut c = cache();
    c.remove(path);
    c.change += 1;
    let n = c.change;
    c.changes.push_back((n, path.to_path_buf()));
    if c.changes.len() > CHANGE_LOG {
        c.changes.pop_front();
    }
}

/// The number of the last change, and the paths changed after change
/// `seen` (`None`: more than the log keeps).
fn changes_since(seen: u64) -> (u64, Option<HashSet<PathBuf>>) {
    let c = cache();
    if c.change == seen {
        return (seen, Some(HashSet::new()));
    }
    let complete = c
        .changes
        .front()
        .is_some_and(|(n, _)| *n <= seen.saturating_add(1));
    let paths = complete.then(|| {
        c.changes
            .iter()
            .filter(|(n, _)| *n > seen)
            .map(|(_, p)| p.clone())
            .collect()
    });
    (c.change, paths)
}

/// How many times the file at `path` was read.
#[cfg(test)]
pub(crate) fn reads_of(path: &Path) -> usize {
    cache().reads.get(path).copied().unwrap_or(0)
}

/// Whether the cache holds the note at `path`, and its total size.
#[cfg(test)]
fn cached(path: &Path) -> (bool, usize) {
    let c = cache();
    (c.entries.contains_key(path), c.bytes)
}

/// What a job off the UI thread asks the render tree again.
#[derive(Debug, Clone)]
pub(super) enum TreeJob {
    /// The sizes of the images at these destinations
    /// ([`gv_render_tree::image_key`]); with `redraw_unchanged`, the items
    /// showing them are drawn again when no size changed (new pixels).
    Images {
        dests: HashSet<String>,
        redraw_unchanged: bool,
    },
    /// The embeds of the notes at these paths (absolute).
    Embeds(HashSet<PathBuf>),
}

/// What a job gives back: the new tree and what changed, or `None` when
/// the document has to be built again.
type JobResult = Option<(Arc<RenderTree>, UpdateReport)>;

/// What a job started from.
struct Started {
    path: PathBuf,
    revision: u64,
    /// The job's number ([`Reader::tree_jobs`]).
    generation: u64,
    /// The tree it worked on (kept alive, so a pointer comparison is
    /// sound).
    base: Arc<RenderTree>,
}

/// Runs `job` on `doc` (off the UI thread).
fn run(mut doc: Loaded, links: &Resolver, job: &TreeJob) -> JobResult {
    let report = match job {
        TreeJob::Images { dests, .. } => load::refresh_images(&mut doc, links, dests),
        TreeJob::Embeds(paths) => {
            let which = |t: &LinkTarget<'_>| {
                links
                    .embed_file(t.destination, t.from)
                    .is_some_and(|p| paths.contains(&p))
            };
            load::rebuild_embeds(&mut doc, links, &which)?
        }
    };
    Some((doc.tree, report))
}

/// Whether `tree` shows a note embed.
fn has_embeds(tree: &RenderTree) -> bool {
    tree.nodes().iter().any(|n| {
        n.context
            .iter()
            .any(|f| matches!(f.kind, FrameKind::Embed { .. }))
    })
}

impl Reader {
    /// Starts checking for changes of embedded notes, every
    /// [`EMBED_DEBOUNCE`], once.
    pub(super) fn watch_embeds(&mut self, cx: &mut Context<'_, Self>) {
        if self._embed_watch.is_some() {
            return;
        }
        self.embeds_seen = cache().change;
        self._embed_watch = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(EMBED_DEBOUNCE).await;
                if this.update(cx, |r, cx| r.poll_embeds(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Builds again the embeds of the notes changed since the last check.
    fn poll_embeds(&mut self, cx: &mut Context<'_, Self>) {
        let (last, paths) = changes_since(self.embeds_seen);
        if last == self.embeds_seen {
            return;
        }
        self.embeds_seen = last;
        match paths {
            Some(paths) => self.embeds_changed(paths, cx),
            // Too many changes to tell: built again.
            None if self.shows_embeds() => self.rebuild(cx),
            None => {}
        }
    }

    /// Whether the shown document has a note embed.
    fn shows_embeds(&self) -> bool {
        self.state
            .borrow()
            .doc
            .as_ref()
            .is_some_and(|d| has_embeds(&d.tree))
    }

    /// The notes at `paths` changed: the embeds that show them are built
    /// again.
    pub(super) fn embeds_changed(&mut self, paths: HashSet<PathBuf>, cx: &mut Context<'_, Self>) {
        if !paths.is_empty() && self.shows_embeds() {
            self.run_tree_job(TreeJob::Embeds(paths), cx);
        }
    }

    /// Runs `job` on the shown document off the UI thread; the result is
    /// applied when it lands ([`Self::tree_job_landed`]).
    pub(super) fn run_tree_job(&mut self, job: TreeJob, cx: &mut Context<'_, Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let (doc, revision) = {
            let st = self.state.borrow();
            let Some(doc) = st.doc.clone() else {
                return;
            };
            (doc, st.revision)
        };
        let resolver = Resolver::statuses(&path, self.index.as_ref(), doc.profile);
        self.tree_jobs += 1;
        let generation = self.tree_jobs;
        let base = Arc::clone(&doc.tree);
        cx.spawn(async move |this, cx| {
            let work = job.clone();
            let result = cx
                .background_executor()
                .spawn(async move { run(doc, &resolver, &work) })
                .await;
            this.update(cx, |r, cx| {
                let started = Started {
                    path,
                    revision,
                    generation,
                    base,
                };
                r.tree_job_landed(started, job, result, cx);
            })
            .ok();
        })
        .detach();
    }

    /// A job's result landed: applied when the shown tree is the one it
    /// started from (same revision, same tree); else (the tree was
    /// replaced meanwhile) the job runs again on the current tree, unless
    /// another file is shown.
    fn tree_job_landed(
        &mut self,
        started: Started,
        job: TreeJob,
        result: JobResult,
        cx: &mut Context<'_, Self>,
    ) {
        if self.path.as_ref() != Some(&started.path) {
            return;
        }
        let current = {
            let st = self.state.borrow();
            st.revision == started.revision
                && st
                    .doc
                    .as_ref()
                    .is_some_and(|d| Arc::ptr_eq(&d.tree, &started.base))
        };
        if !current {
            tracing::trace!(
                generation = started.generation,
                "tree job outdated: run again"
            );
            self.run_tree_job(job, cx);
            return;
        }
        let Some((tree, report)) = result else {
            self.rebuild(cx);
            return;
        };
        if report.changed.is_empty() {
            if let TreeJob::Images {
                dests,
                redraw_unchanged: true,
            } = &job
            {
                // The same sizes: the items showing the images are drawn
                // again for the new pixels.
                let mut st = self.state.borrow_mut();
                let ids = st
                    .doc
                    .as_ref()
                    .map_or_else(Vec::new, |d| super::media::items_showing(&d.tree, dests));
                st.cache.forget(ids.iter().copied());
                st.stale.extend(ids);
                drop(st);
                cx.notify();
            }
            return;
        }
        let mut st = self.state.borrow_mut();
        if let Some(doc) = st.doc.as_mut() {
            doc.tree = tree;
        }
        st.cache.forget(report.changed.iter().copied());
        st.stale.extend(report.changed);
        drop(st);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_notes_are_read_once_and_changes_read_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("N.md");
        std::fs::write(&path, "one\n").expect("write");
        assert_eq!(text(&path).as_deref(), Some("one\n"));
        assert_eq!(text(&path).as_deref(), Some("one\n"));
        assert_eq!(reads_of(&path), 1);
        // Another size: read again.
        std::fs::write(&path, "three\n").expect("write");
        assert_eq!(text(&path).as_deref(), Some("three\n"));
        assert_eq!(reads_of(&path), 2);
        // A change the watcher saw: read again.
        changed(&path);
        assert!(!cached(&path).0);
        assert_eq!(text(&path).as_deref(), Some("three\n"));
        assert_eq!(reads_of(&path), 3);
        // Too large, or missing: nothing.
        let big = dir.path().join("big.md");
        std::fs::write(&big, "x".repeat(MAX_NOTE_BYTES as usize + 1)).expect("write");
        assert_eq!(text(&big), None);
        assert_eq!(text(&dir.path().join("none.md")), None);
    }

    #[test]
    fn unsaved_buffers_come_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("N.md");
        std::fs::write(&path, "disk\n").expect("write");
        crate::files::overlay::dirty(&path, &gv_text::TextSnapshot::new("buffer\n"));
        assert_eq!(text(&path).as_deref(), Some("buffer\n"));
        assert_eq!(reads_of(&path), 0);
        crate::files::overlay::clean(&path);
        assert_eq!(text(&path).as_deref(), Some("disk\n"));
    }

    #[test]
    fn the_cache_keeps_the_most_recently_used_notes_within_its_budget() {
        let mut c = Cache::default();
        let now = SystemTime::now();
        let note = |n: usize| Arc::<str>::from("x".repeat(n));
        let mb = 1 << 20;
        for i in 0..10 {
            c.insert(PathBuf::from(format!("/n{i}")), now, 0, note(2 * mb));
        }
        assert!(c.bytes <= CACHE_BYTES);
        assert_eq!(c.entries.len(), 8);
        // The oldest went first.
        assert!(!c.entries.contains_key(Path::new("/n0")));
        assert!(!c.entries.contains_key(Path::new("/n1")));
        assert!(c.entries.contains_key(Path::new("/n9")));
        // Too large for the cache: not kept.
        c.insert(PathBuf::from("/huge"), now, 0, note(CACHE_BYTES + 1));
        assert!(!c.entries.contains_key(Path::new("/huge")));
        c.remove(Path::new("/n9"));
        assert_eq!(c.bytes, 7 * 2 * mb);
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! State-level tests: incremental updates against a build from scratch,
//! overlays, placeholders, sync-conflict copies and the queries.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use proptest::prelude::*;

use super::state::{State, system_time_ns};
use super::store;
use super::{ContentOptions, ContentSnapshot, FileSource, RecordKind};
use crate::index::{Change, EntryKind, FileMeta, IndexConfig, IndexState, PathIndex};
use crate::path::VaultPath;
use crate::profile::ProfileId;
use crate::resolve::Status;

/// Files in memory; names containing `cloud` are placeholders.
#[derive(Default)]
struct Mem {
    files: Mutex<HashMap<String, String>>,
    reads: AtomicUsize,
}

impl FileSource for Mem {
    fn is_placeholder(&self, path: &VaultPath) -> io::Result<bool> {
        Ok(path.as_str().contains("cloud"))
    }
    fn read(&self, path: &VaultPath) -> io::Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.files
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .get(path.as_str())
            .map(|s| s.clone().into_bytes())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

fn options() -> Arc<ContentOptions> {
    let mut o = ContentOptions::for_profile(ProfileId::Obsidian);
    o.threads = 2;
    Arc::new(o)
}

fn meta(size: usize, tick: u64) -> FileMeta {
    FileMeta {
        size: size as u64,
        mtime: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + tick)),
        identity: None,
    }
}

fn vp(s: &str) -> VaultPath {
    VaultPath::new(s).expect("valid path")
}

/// A vault driven step by step, as the watcher and editor would.
struct Driver {
    mem: Arc<Mem>,
    paths: PathIndex,
    state: State,
    tick: u64,
    overlays: BTreeMap<String, String>,
    /// The app is closed: the disk changes, the index does not.
    closed: bool,
}

impl Driver {
    fn new(files: &[(&str, &str)]) -> Self {
        let mem = Arc::new(Mem::default());
        let mut d = Self {
            mem,
            paths: PathIndex::from_entries(
                "/vault",
                IndexConfig::default(),
                IndexState::Complete,
                Vec::<(VaultPath, EntryKind, FileMeta)>::new(),
            )
            .expect("empty index"),
            state: State::new(options()),
            tick: 0,
            overlays: BTreeMap::new(),
            closed: false,
        };
        let changes: Vec<Change> = files.iter().map(|(p, t)| d.write_change(p, t)).collect();
        d.paths = d.paths.apply(&changes).expect("apply");
        d.state
            .apply(
                Some(d.paths.clone()),
                Vec::new(),
                &*d.mem,
                &AtomicBool::new(false),
            )
            .expect("not cancelled");
        d
    }

    fn write_change(&mut self, path: &str, text: &str) -> Change {
        self.tick += 1;
        self.mem
            .files
            .lock()
            .expect("lock")
            .insert(path.to_owned(), text.to_owned());
        Change::Upsert {
            path: vp(path),
            kind: EntryKind::File,
            meta: meta(text.len(), self.tick),
        }
    }

    fn step(&mut self, changes: Vec<Change>, overlays: Vec<(VaultPath, Option<String>)>) {
        let paths = if changes.is_empty() {
            None
        } else {
            self.paths = self.paths.apply(&changes).expect("apply");
            Some(self.paths.clone())
        };
        if self.closed {
            return;
        }
        self.state
            .apply(paths, overlays, &*self.mem, &AtomicBool::new(false))
            .expect("not cancelled");
    }

    fn write(&mut self, path: &str, text: &str) {
        let c = self.write_change(path, text);
        self.step(vec![c], Vec::new());
    }

    fn delete(&mut self, path: &str) {
        if self.mem.files.lock().expect("lock").remove(path).is_some() {
            self.step(vec![Change::Remove { path: vp(path) }], Vec::new());
        }
    }

    fn rename(&mut self, from: &str, to: &str) {
        let mut files = self.mem.files.lock().expect("lock");
        if from == to || !files.contains_key(from) {
            return;
        }
        let text = files.remove(from).expect("present");
        files.insert(to.to_owned(), text);
        drop(files);
        self.step(
            vec![Change::Rename {
                from: vp(from),
                to: vp(to),
            }],
            Vec::new(),
        );
    }

    fn overlay(&mut self, path: &str, text: Option<&str>) {
        match text {
            Some(t) => self.overlays.insert(path.to_owned(), t.to_owned()),
            None => self.overlays.remove(path),
        };
        self.step(Vec::new(), vec![(vp(path), text.map(str::to_owned))]);
    }

    /// Writes the cache and closes the app.
    fn close(&mut self, dir: &std::path::Path) -> std::path::PathBuf {
        let file = dir.join("cache.gvci");
        let header = store::Header {
            profile: ProfileId::Obsidian,
            root: "/vault".into(),
            written_at: system_time_ns(SystemTime::now()),
            fingerprint: fingerprint(),
        };
        store::save(&file, &header, &self.state).expect("save");
        self.closed = true;
        // Unsaved buffers are not part of a restart.
        self.overlays.clear();
        file
    }

    /// Starts again from the cache, as a warm start does.
    fn reopen(&mut self, file: &std::path::Path) {
        let loaded = store::load(file, ProfileId::Obsidian, "/vault", fingerprint()).expect("load");
        let graph = loaded.graph.expect("the graph is stored");
        let mut state = State::with_records(options(), loaded.records);
        let previous = store::previous_index(
            std::path::Path::new("/vault"),
            &IndexConfig::default(),
            &graph.entries,
        )
        .expect("previous index");
        state.adopt_graph(previous, graph.resolved);
        state
            .apply(
                Some(self.paths.clone()),
                Vec::new(),
                &*self.mem,
                &AtomicBool::new(false),
            )
            .expect("not cancelled");
        self.state = state;
        self.closed = false;
    }

    fn snapshot(&self) -> ContentSnapshot {
        ContentSnapshot(Arc::new(self.state.clone()))
    }

    /// The same disk and overlays, indexed from scratch.
    fn scratch(&self) -> ContentSnapshot {
        let entries: Vec<(VaultPath, EntryKind, FileMeta)> = self
            .paths
            .iter()
            .filter(|e| !e.path.is_root())
            .map(|e| (e.path.clone(), e.kind.clone(), e.meta.clone()))
            .collect();
        let paths = PathIndex::from_entries(
            "/vault",
            IndexConfig::default(),
            IndexState::Complete,
            entries,
        )
        .expect("index");
        let mut state = State::new(options());
        let overlays = self
            .overlays
            .iter()
            .map(|(p, t)| (vp(p), Some(t.clone())))
            .collect();
        state
            .apply(Some(paths), overlays, &*self.mem, &AtomicBool::new(false))
            .expect("not cancelled");
        ContentSnapshot(Arc::new(state))
    }
}

#[test]
fn backlinks_follow_edits_renames_and_deletes() {
    let mut d = Driver::new(&[
        ("A.md", "# Alpha\n\nLinks to [[B]] and [[Missing]].\n"),
        ("B.md", "# Bee\n\nBack to [[A|alpha]] and [[a]].\n"),
        ("sub/C.md", "See [b](../B.md) #tag/x\n"),
    ]);
    let s = d.snapshot();
    assert_eq!(
        s.backlink_sources(&vp("B.md")),
        vec![vp("A.md"), vp("sub/C.md")]
    );
    let back = s.backlinks(&vp("A.md"));
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].heading_path, vec![Arc::from("Bee")]);
    assert_eq!(s.unresolved().len(), 1);
    assert_eq!(s.unresolved_targets(), vec![(Arc::from("Missing"), 1)]);

    // Creating the missing note resolves the link.
    d.write("Missing.md", "x\n");
    let s = d.snapshot();
    assert!(s.unresolved().is_empty());
    assert_eq!(s.backlink_sources(&vp("Missing.md")), vec![vp("A.md")]);

    // Renaming B breaks the wikilink but not the relative Markdown link's
    // target... which also breaks: it names the old path.
    d.rename("B.md", "Bee.md");
    let s = d.snapshot();
    assert!(s.backlink_sources(&vp("Bee.md")).is_empty());
    assert_eq!(s.unresolved().len(), 2);

    // Deleting a source drops its backlinks.
    d.delete("Bee.md");
    assert!(d.snapshot().backlinks(&vp("A.md")).is_empty());
    assert_eq!(d.snapshot().debug_dump(), d.scratch().debug_dump());
}

#[test]
fn unchanged_bytes_are_not_parsed_again() {
    let mut d = Driver::new(&[("A.md", "[[B]]\n"), ("B.md", "b\n")]);
    let reads = d.mem.reads.load(Ordering::Relaxed);
    // Touch: same bytes, new time.
    let c = d.write_change("A.md", "[[B]]\n");
    d.paths = d.paths.apply(&[c]).expect("apply");
    let report = d
        .state
        .apply(
            Some(d.paths.clone()),
            Vec::new(),
            &*d.mem,
            &AtomicBool::new(false),
        )
        .expect("ok");
    assert_eq!(d.mem.reads.load(Ordering::Relaxed), reads + 1);
    assert_eq!(report.parsed, 0);
    assert!(report.changed.is_empty());
}

#[test]
fn overlays_override_the_disk_until_cleared() {
    let mut d = Driver::new(&[("A.md", "nothing\n"), ("B.md", "b\n")]);
    d.overlay("A.md", Some("now [[B]] and #draft\n"));
    let s = d.snapshot();
    assert_eq!(s.backlink_sources(&vp("B.md")), vec![vp("A.md")]);
    assert_eq!(
        s.note(&vp("A.md")).map(|r| r.kind),
        Some(RecordKind::Overlay)
    );
    assert_eq!(s.tags()[0].name.as_ref(), "draft");
    // An unsaved new note.
    d.overlay("New.md", Some("[[B]]\n"));
    assert_eq!(
        d.snapshot().backlink_sources(&vp("B.md")),
        vec![vp("A.md"), vp("New.md")]
    );
    assert!(
        d.snapshot()
            .switcher_entries()
            .iter()
            .any(|e| e.path == vp("New.md"))
    );
    d.overlay("A.md", None);
    d.overlay("New.md", None);
    let s = d.snapshot();
    assert!(s.backlink_sources(&vp("B.md")).is_empty());
    assert_eq!(
        s.note(&vp("A.md")).map(|r| r.kind),
        Some(RecordKind::Indexed)
    );
    assert!(s.note(&vp("New.md")).is_none());
}

#[test]
fn placeholders_are_never_read() {
    let d = Driver::new(&[("A.md", "[[cloud note]]\n"), ("cloud note.md", "[[A]]\n")]);
    let s = d.snapshot();
    assert_eq!(s.placeholders(), vec![vp("cloud note.md")]);
    assert_eq!(d.mem.reads.load(Ordering::Relaxed), 1);
    // Links to a placeholder resolve; it has no links of its own.
    assert_eq!(s.backlink_sources(&vp("cloud note.md")), vec![vp("A.md")]);
    assert!(s.backlink_sources(&vp("A.md")).is_empty());
    assert_eq!(
        s.note(&vp("cloud note.md")).map(|r| r.title()),
        Some("cloud note")
    );
}

#[test]
fn sync_conflict_copies_are_listed_and_left_out_of_backlinks() {
    let d = Driver::new(&[
        ("Note.md", "[[Target]]\n"),
        ("Note (conflicted copy 2026-01-02).md", "[[Target]]\n"),
        ("Target.md", "t\n"),
    ]);
    let s = d.snapshot();
    assert_eq!(s.backlink_sources(&vp("Target.md")), vec![vp("Note.md")]);
    let c = s.sync_conflicts();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].original, vp("Note.md"));
}

#[test]
fn icloud_copies_change_when_the_original_appears() {
    // "Note 2" is an ordinary note until "Note" exists next to it.
    // (In a folder, so the link needs the basename lookup.)
    let mut d = Driver::new(&[("sub/Note 2.md", "two\n"), ("A.md", "[[Note 2]]\n")]);
    assert_eq!(
        d.snapshot().backlink_sources(&vp("sub/Note 2.md")),
        vec![vp("A.md")]
    );
    d.write("sub/Note.md", "one\n");
    assert_eq!(d.snapshot().debug_dump(), d.scratch().debug_dump());
    d.delete("sub/Note.md");
    assert_eq!(d.snapshot().debug_dump(), d.scratch().debug_dump());
}

#[test]
fn tags_switcher_and_titles() {
    let d = Driver::new(&[
        (
            "A.md",
            "---\ntitle: Alpha Title\naliases: [Al, First]\ntags: [Proj]\n---\n#proj/sub #Other\n",
        ),
        ("B.md", "# Bee\n#proj\n"),
        ("img.png", "png"),
    ]);
    let s = d.snapshot();
    let tags: Vec<(String, usize, usize)> = s
        .tags()
        .into_iter()
        .map(|t| (t.name.to_string(), t.count, t.notes))
        .collect();
    assert_eq!(
        tags,
        vec![
            ("Other".into(), 1, 1),
            ("Proj".into(), 2, 2),
            ("proj/sub".into(), 1, 1)
        ]
    );
    assert_eq!(s.notes_with_tag("#proj"), vec![vp("A.md"), vp("B.md")]);
    let entries = s.switcher_entries();
    assert_eq!(entries.len(), 5);
    assert!(
        entries
            .iter()
            .any(|e| e.alias.as_deref() == Some("First")
                && e.title.as_deref() == Some("Alpha Title"))
    );
    assert!(
        entries
            .iter()
            .any(|e| e.path == vp("img.png") && !e.is_note)
    );
    assert_eq!(s.note(&vp("B.md")).map(|r| r.title()), Some("Bee"));
}

#[test]
fn unlinked_mentions_skip_links_code_and_partial_words() {
    let d = Driver::new(&[
        ("Alpha.md", "---\naliases: [The First]\n---\nbody\n"),
        (
            "B.md",
            "alpha is here. [[Alpha]] is a link. `alpha` is code. alphabet is not.\nAlso the first one.\n",
        ),
    ]);
    let s = d.snapshot();
    let found = s
        .unlinked_mentions(&vp("Alpha.md"), &*d.mem, &AtomicBool::new(false))
        .expect("not cancelled");
    let got: Vec<(u32, &str)> = found.iter().map(|m| (m.line, &*m.term)).collect();
    assert_eq!(got, vec![(0, "Alpha"), (1, "The First")]);
}

#[test]
fn ambiguous_and_case_fold_statuses_are_kept() {
    let d = Driver::new(&[
        ("a/X.md", "x\n"),
        ("b/X.md", "x\n"),
        ("S.md", "[[X]] [[s]]\n"),
    ]);
    let s = d.snapshot();
    let statuses: Vec<Status> = s
        .outgoing(&vp("S.md"))
        .iter()
        .map(|o| o.target.status)
        .collect();
    assert!(matches!(statuses[0], Status::Ambiguous(2)));
    assert!(
        s.unresolved()
            .iter()
            .any(|u| matches!(u.status, Status::Ambiguous(_)))
    );
}

// --- Property test: incremental == from scratch -------------------------

const NAMES: &[&str] = &[
    "A.md",
    "B.md",
    "Note.md",
    "Note 2.md",
    "sub/Note.md",
    "sub/Note 2.md",
    "sub/A.md",
    "sub/D.md",
    "x/y/E.md",
    "Daily 2024.01.md",
    "cloud C.md",
    "img.png",
];

const PIECES: &[&str] = &[
    "[[A]] ",
    "[[B#Head]] ",
    "![[img.png]] ",
    "[c](sub/D.md) ",
    "[up](../A.md) ",
    "#tag1 ",
    "#Tag1/sub ",
    "\n# Head\n",
    "\n- [ ] task [[E]]\n",
    "[[Missing]] ",
    "[[note]] ",
    "[[Note 2]] ",
    "[[Daily 2024.01]] ",
    "[[sub/A|alias]] ",
    "`[[A]]` ",
    "plain words ",
    "^blk ",
    "[[cloud C]] ",
];

#[derive(Debug, Clone)]
enum Op {
    Write(usize, Vec<usize>),
    Delete(usize),
    Rename(usize, usize),
    Overlay(usize, Vec<usize>),
    Clear(usize),
}

fn op() -> impl Strategy<Value = Op> {
    let text = prop::collection::vec(0..PIECES.len(), 0..6);
    prop_oneof![
        4 => (0..NAMES.len(), text.clone()).prop_map(|(n, t)| Op::Write(n, t)),
        1 => (0..NAMES.len()).prop_map(Op::Delete),
        2 => (0..NAMES.len(), 0..NAMES.len()).prop_map(|(a, b)| Op::Rename(a, b)),
        1 => (0..NAMES.len(), text).prop_map(|(n, t)| Op::Overlay(n, t)),
        1 => (0..NAMES.len()).prop_map(Op::Clear),
    ]
}

fn text_of(pieces: &[usize]) -> String {
    let mut s: String = pieces.iter().map(|&i| PIECES[i]).collect();
    s.push('\n');
    s
}

fn fingerprint() -> u64 {
    store::graph_fingerprint(&options().params, &IndexConfig::default())
}

fn run(d: &mut Driver, op: Op) {
    match op {
        Op::Write(n, t) => d.write(NAMES[n], &text_of(&t)),
        Op::Delete(n) => d.delete(NAMES[n]),
        Op::Rename(a, b) => d.rename(NAMES[a], NAMES[b]),
        Op::Overlay(n, t) if NAMES[n].ends_with(".md") && !d.closed => {
            d.overlay(NAMES[n], Some(&text_of(&t)));
        }
        Op::Clear(n) if !d.closed => d.overlay(NAMES[n], None),
        Op::Overlay(..) | Op::Clear(_) => {}
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn warm_start_equals_scratch(
        initial in prop::collection::vec((0..NAMES.len(), prop::collection::vec(0..PIECES.len(), 0..6)), 0..6),
        before in prop::collection::vec(op(), 0..8),
        closed in prop::collection::vec(op(), 0..8),
    ) {
        let files: Vec<(&str, String)> = initial.iter().map(|(n, t)| (NAMES[*n], text_of(t))).collect();
        let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let mut d = Driver::new(&files);
        for op in before {
            run(&mut d, op);
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let file = d.close(dir.path());
        for op in closed {
            run(&mut d, op);
        }
        d.reopen(&file);
        prop_assert_eq!(d.snapshot().debug_dump(), d.scratch().debug_dump());
    }

    #[test]
    fn incremental_equals_scratch(
        initial in prop::collection::vec((0..NAMES.len(), prop::collection::vec(0..PIECES.len(), 0..6)), 0..6),
        ops in prop::collection::vec(op(), 1..14),
    ) {
        let files: Vec<(&str, String)> = initial.iter().map(|(n, t)| (NAMES[*n], text_of(t))).collect();
        let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let mut d = Driver::new(&files);
        for op in ops {
            match op {
                Op::Write(n, t) => d.write(NAMES[n], &text_of(&t)),
                Op::Delete(n) => d.delete(NAMES[n]),
                Op::Rename(a, b) => d.rename(NAMES[a], NAMES[b]),
                Op::Overlay(n, t) if NAMES[n].ends_with(".md") => d.overlay(NAMES[n], Some(&text_of(&t))),
                Op::Clear(n) => d.overlay(NAMES[n], None),
                Op::Overlay(..) => {}
            }
            prop_assert_eq!(d.snapshot().debug_dump(), d.scratch().debug_dump());
        }
    }
}

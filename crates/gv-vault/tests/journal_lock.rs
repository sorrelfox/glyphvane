// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The journal lock: a journal open in this process or another live one is
//! busy (not written, recovered or discarded from elsewhere); once its
//! process ends, however it ends, the journal is recoverable.
//!
//! The cross-process tests re-run this test binary as a child process (the
//! ignored `lock_holder_child` test, which does nothing unless the parent's
//! environment variable is set). The child opens a journal, records an edit,
//! prints `ready` and waits for a line on its standard input. The parent
//! records the child's PID and only ever signals that PID. All files live in
//! temporary directories.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use gv_text::{Buffer, Transaction};
use gv_vault::files::journal::{
    DocumentJournal, DocumentKey, JournalError, JournalId, JournalPolicy, JournalStore,
};
use gv_vault::files::{ContentHash, ManualClock, SystemClock};

const CHILD_DIR_ENV: &str = "GV_LOCK_CHILD_DIR";
const CHILD_DOC_ENV: &str = "GV_LOCK_CHILD_DOC";
const ORIGINAL: &str = "# Shared\n\nText.\n";
const CHILD_EDIT: &str = "child edit ";

fn store_in(dir: &Path) -> JournalStore {
    JournalStore::open(dir.join("journal")).expect("store")
}

fn begin<C: gv_vault::files::Clock>(
    store: &JournalStore,
    path: &Path,
    clock: C,
) -> Result<DocumentJournal<C>, JournalError> {
    let bytes = fs::read(path).expect("read");
    let buffer = Buffer::new(std::str::from_utf8(&bytes).expect("utf-8"));
    store.begin(
        DocumentKey::Path(path.to_path_buf()),
        buffer.snapshot(),
        buffer.format(),
        Some(ContentHash::of(&bytes)),
        JournalPolicy::default(),
        clock,
    )
}

/// Records one insertion at the start of `path`'s text.
fn record_edit<C: gv_vault::files::Clock>(journal: &mut DocumentJournal<C>, path: &Path) {
    let mut buffer = Buffer::new(&fs::read_to_string(path).expect("read"));
    let edit = Transaction::new().insert(0, CHILD_EDIT);
    buffer.apply(&edit).expect("edit");
    journal.record(&edit, buffer.text()).expect("record");
}

fn is_busy<T: std::fmt::Debug>(result: Result<T, JournalError>, journal: &Path) -> bool {
    match result {
        Err(JournalError::Busy { path }) => path == journal,
        other => panic!("expected Busy, got {other:?}"),
    }
}

#[cfg(unix)]
fn lock_files(store: &JournalStore) -> Vec<PathBuf> {
    fs::read_dir(store.dir())
        .expect("list")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "lock"))
        .collect()
}

/// Asserts that every way into journal `id` from outside its holder is
/// refused, and that it is listed as in use.
fn assert_busy(store: &JournalStore, path: &Path, id: &JournalId) {
    let journal_file = store.dir().join(id.as_str());
    assert!(journal_file.exists(), "the holder wrote its journal");
    assert!(is_busy(
        begin(store, path, ManualClock::new()),
        &journal_file
    ));
    assert!(is_busy(store.recover(id), &journal_file));
    assert!(is_busy(store.discard(id), &journal_file));
    assert!(is_busy(store.restore_backup(id), &journal_file));
    assert!(journal_file.exists(), "a busy journal is not discarded");
    let scan = store.scan().expect("scan");
    assert!(scan.recoverable.is_empty(), "{scan:?}");
    assert_eq!(scan.in_use.len(), 1, "{scan:?}");
    assert_eq!(&scan.in_use[0].id, id);
    assert_eq!(
        scan.in_use[0].key,
        Some(DocumentKey::Path(path.to_path_buf()))
    );
    assert!(store.recoverable().expect("list").is_empty());
}

/// Asserts that journal `id`, left by a holder that is gone, is recoverable
/// with the holder's edit, then discards it and starts a new journal.
fn assert_recoverable(store: &JournalStore, path: &Path, id: &JournalId) {
    let listed = store.recoverable().expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(&listed[0].id, id);
    assert!(store.scan().expect("scan").in_use.is_empty());
    let recovery = store.recover(id).expect("recover");
    assert_eq!(recovery.text, format!("{CHILD_EDIT}{ORIGINAL}"));
    assert!(matches!(
        begin(store, path, ManualClock::new()),
        Err(JournalError::PendingRecovery(_))
    ));
    store.discard(id).expect("discard");
    let journal = begin(store, path, ManualClock::new()).expect("a new journal");
    drop(journal);
    assert!(store.recoverable().expect("list").is_empty());
}

#[test]
fn a_second_handle_in_this_process_is_busy() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = store_in(dir.path());
    let path = dir.path().join("note.md");
    fs::write(&path, ORIGINAL).expect("write");
    let mut first = begin(&store, &path, ManualClock::new()).expect("journal");
    let id = first.id().clone();
    let journal_file = store.dir().join(id.as_str());
    // Locked from the start, before anything is written.
    assert!(!first.is_active());
    assert!(is_busy(
        begin(&store, &path, ManualClock::new()),
        &journal_file
    ));
    record_edit(&mut first, &path);
    assert_busy(&store, &path, &id);
    // The holder keeps journaling.
    let mut buffer = Buffer::new(&format!("{CHILD_EDIT}{ORIGINAL}"));
    let more = Transaction::new().insert(buffer.len(), "more");
    buffer.apply(&more).expect("edit");
    first.record(&more, buffer.text()).expect("record");
    first.record_snapshot(buffer.text()).expect("compact");
    assert_busy(&store, &path, &id);
    // Save As onto the open document is refused too, and the other
    // journal keeps its own key.
    let other = dir.path().join("other.md");
    fs::write(&other, "other\n").expect("write");
    let mut second = begin(&store, &other, ManualClock::new()).expect("journal");
    let saved = Buffer::new(ORIGINAL);
    assert!(is_busy(
        second.saved_as(
            DocumentKey::Path(path.clone()),
            saved.snapshot(),
            saved.format(),
            ContentHash::of(ORIGINAL.as_bytes()),
        ),
        &journal_file
    ));
    assert_eq!(second.key(), &DocumentKey::Path(other.clone()));
    drop(second);
    // Dropped without saving or discarding: a crash, as far as the journal
    // knows.
    drop(first);
    let recovery = store.recover(&id).expect("recover");
    assert_eq!(recovery.text, format!("{CHILD_EDIT}{ORIGINAL}more"));
    store.discard(&id).expect("discard");
    #[cfg(unix)]
    assert!(lock_files(&store).is_empty(), "{:?}", lock_files(&store));
}

#[test]
fn save_as_moves_the_lock_to_the_new_key() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = store_in(dir.path());
    let mut untitled = store
        .begin(
            DocumentKey::Untitled(1),
            Buffer::new("").snapshot(),
            Default::default(),
            None,
            JournalPolicy::default(),
            ManualClock::new(),
        )
        .expect("journal");
    let path = dir.path().join("saved.md");
    fs::write(&path, ORIGINAL).expect("write");
    let saved = Buffer::new(ORIGINAL);
    untitled
        .saved_as(
            DocumentKey::Path(path.clone()),
            saved.snapshot(),
            saved.format(),
            ContentHash::of(ORIGINAL.as_bytes()),
        )
        .expect("save as");
    // The new key is held, the old one is free.
    let journal_file = store.dir().join(untitled.id().as_str());
    assert!(is_busy(
        begin(&store, &path, ManualClock::new()),
        &journal_file
    ));
    let again = store
        .begin(
            DocumentKey::Untitled(1),
            Buffer::new("").snapshot(),
            Default::default(),
            None,
            JournalPolicy::default(),
            ManualClock::new(),
        )
        .expect("the untitled key is free again");
    drop(again);
    drop(untitled);
    #[cfg(unix)]
    assert!(lock_files(&store).is_empty(), "{:?}", lock_files(&store));
}

#[test]
fn temporary_files_of_a_journal_in_use_are_kept() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = store_in(dir.path());
    let path = dir.path().join("note.md");
    fs::write(&path, ORIGINAL).expect("write");
    let journal = begin(&store, &path, ManualClock::new()).expect("journal");
    let id = journal.id().clone();
    let busy_temp = store.dir().join(format!(".{id}.gv-tmp-abc"));
    let busy_backup_temp = store.dir().join(format!(".{id}.backup.gv-tmp-abc"));
    let free_temp = store.dir().join(format!(".{}.gv-tmp-abc", "0".repeat(32)));
    for file in [&busy_temp, &busy_backup_temp, &free_temp] {
        fs::write(file, "x").expect("write");
    }
    assert_eq!(store.remove_stale_temp_files().expect("clean"), 1);
    assert!(busy_temp.exists() && busy_backup_temp.exists());
    assert!(!free_temp.exists());
    drop(journal);
    assert_eq!(store.remove_stale_temp_files().expect("clean"), 2);
}

// --- Another process ---

/// The child: opens the journal, records an edit and waits.
#[test]
#[ignore = "child process of the cross-process lock tests; does nothing on its own"]
fn lock_holder_child() {
    let (Some(dir), Some(doc)) = (
        std::env::var_os(CHILD_DIR_ENV),
        std::env::var_os(CHILD_DOC_ENV),
    ) else {
        return;
    };
    let store = store_in(Path::new(&dir));
    let path = PathBuf::from(doc);
    let mut journal = begin(&store, &path, SystemClock).expect("child journal");
    record_edit(&mut journal, &path);
    journal.sync().expect("sync");
    let mut out = std::io::stdout().lock();
    writeln!(out, "\nready").expect("stdout");
    out.flush().expect("stdout");
    drop(out);
    // Until the parent says so (or goes away): quitting with unsaved text
    // leaves the journal for the next start.
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    drop(journal);
}

/// A child that holds a journal; killed by its PID if a test fails.
struct Holder {
    child: Option<Child>,
    pid: u32,
}

impl Holder {
    fn spawn(dir: &Path, doc: &Path) -> Self {
        let exe = std::env::current_exe().expect("test binary");
        let mut child = Command::new(exe)
            .args([
                "--exact",
                "lock_holder_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_DIR_ENV, dir)
            .env(CHILD_DOC_ENV, doc)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn child");
        // Only this PID is ever signalled.
        let pid = child.id();
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line.trim() == "ready" {
                    let _ = tx.send(());
                }
            }
        });
        let mut holder = Self {
            child: Some(child),
            pid,
        };
        if rx.recv_timeout(Duration::from_secs(60)).is_err() {
            let status = holder.kill();
            panic!("child {pid} did not get ready ({status:?})");
        }
        holder
    }

    fn child(&mut self) -> &mut Child {
        self.child.as_mut().expect("child still running")
    }

    /// Asks the child to exit normally and waits for it.
    fn exit(&mut self) -> ExitStatus {
        let mut stdin = self.child().stdin.take().expect("child stdin");
        writeln!(stdin, "exit").expect("tell the child to exit");
        drop(stdin);
        self.wait()
    }

    /// SIGKILL (on Unix; `TerminateProcess` on Windows) to the recorded PID.
    fn kill(&mut self) -> ExitStatus {
        assert_eq!(self.child().id(), self.pid);
        self.child().kill().expect("kill the child");
        self.wait()
    }

    fn wait(&mut self) -> ExitStatus {
        let mut child = self.child.take().expect("child still running");
        for _ in 0..600 {
            if let Some(status) = child.try_wait().expect("wait") {
                return status;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("child {} did not exit", self.pid);
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Starts a child holding the journal of a new document, and checks the
/// parent's view while it runs.
fn with_live_holder(dir: &Path) -> (Holder, JournalStore, PathBuf, JournalId) {
    let path = dir.join("note.md");
    fs::write(&path, ORIGINAL).expect("write");
    let holder = Holder::spawn(dir, &path);
    let store = store_in(dir);
    let id = JournalId::of(&DocumentKey::Path(path.clone()));
    assert_busy(&store, &path, &id);
    let temp = store.dir().join(format!(".{id}.gv-tmp-live"));
    fs::write(&temp, "x").expect("write");
    assert_eq!(store.remove_stale_temp_files().expect("clean"), 0);
    assert!(temp.exists(), "the live process may be writing it");
    fs::remove_file(&temp).expect("remove");
    (holder, store, path, id)
}

#[test]
fn a_journal_held_by_another_process_is_busy_until_it_exits() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (mut holder, store, path, id) = with_live_holder(dir.path());
    let status = holder.exit();
    assert!(status.success(), "child {} failed: {status:?}", holder.pid);
    assert_recoverable(&store, &path, &id);
}

#[test]
fn a_journal_held_by_a_killed_process_is_recoverable() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (mut holder, store, path, id) = with_live_holder(dir.path());
    let status = holder.kill();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "child {} was SIGKILLed",
            holder.pid
        );
        // The killed child could not remove its lock file; it is reused.
        assert_eq!(lock_files(&store).len(), 1);
    }
    #[cfg(not(unix))]
    assert!(!status.success());
    assert_recoverable(&store, &path, &id);
    #[cfg(unix)]
    assert!(lock_files(&store).is_empty(), "{:?}", lock_files(&store));
}

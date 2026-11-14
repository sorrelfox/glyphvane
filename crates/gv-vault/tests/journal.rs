// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Crash journal: recording, compaction, sync timing, recovery, discard,
//! in-place backups. All files live in temporary directories.

use std::fs;
use std::path::Path;
use std::time::Duration;

use gv_settings::Locations;
use gv_text::{Bom, Buffer, Encoding, LineEnding, TextFormat, TextSnapshot, Transaction};
use gv_vault::files::journal::{
    DiskComparison, DocumentJournal, DocumentKey, JournalError, JournalPolicy, JournalStore,
    JournalTail, NO_VAULT_ID,
};
use gv_vault::files::{Backup, Clock, ContentHash, DocumentFile, ManualClock, SaveOptions, merge3};

/// The default policy; the manual clocks below never advance, so records
/// are written but not synced one by one (recovery reads what the OS has).
fn policy() -> JournalPolicy {
    JournalPolicy::default()
}

fn begin(
    store: &JournalStore,
    path: &Path,
    buffer: &Buffer,
    policy: JournalPolicy,
) -> DocumentJournal<ManualClock> {
    let hash = fs::read(path).ok().map(|b| ContentHash::of(&b));
    store
        .begin(
            DocumentKey::Path(path.to_path_buf()),
            buffer.snapshot(),
            buffer.format(),
            hash,
            policy,
            ManualClock::new(),
        )
        .expect("journal")
}

/// A deterministic edit stream over `buffer`.
fn edit(buffer: &mut Buffer, journal: &mut DocumentJournal<ManualClock>, step: usize) {
    let len = buffer.len();
    let at = buffer.clip_offset((step * 7919) % (len + 1), gv_text::Bias::Left);
    let transaction = if step % 5 == 4 && len > 3 {
        let end = buffer.clip_offset((at + 3).min(len), gv_text::Bias::Right);
        Transaction::new().delete(at..end)
    } else {
        Transaction::new().insert(at, format!("[{step}ü]"))
    };
    buffer.apply(&transaction).expect("edit");
    journal.record(&transaction, buffer.text()).expect("record");
}

#[test]
fn edits_are_recovered_after_the_process_forgets_them() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let path = dir.path().join("note.md");
    fs::write(&path, "# Note\r\n\r\nText.\r\n").expect("write");
    let (_file, decoded) = DocumentFile::load(&path).expect("load");
    let mut buffer = Buffer::from_decoded(&decoded);
    let mut journal = begin(&store, &path, &buffer, policy());
    assert!(
        !journal.is_active(),
        "nothing written before the first edit"
    );
    for step in 0..200 {
        edit(&mut buffer, &mut journal, step);
    }
    // A crash: the journal is dropped without saving or discarding.
    drop(journal);

    let listed = store.recoverable().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, Some(DocumentKey::Path(path.clone())));
    assert!(listed[0].has_journal && !listed[0].has_backup);
    let recovery = store.recover(&listed[0].id).expect("recover");
    assert_eq!(recovery.text, buffer.to_string());
    assert_eq!(recovery.base.as_deref(), Some("# Note\r\n\r\nText.\r\n"));
    assert_eq!(recovery.disk, DiskComparison::MatchesBase);
    assert_eq!(recovery.tail, JournalTail::Complete);
    assert_eq!(recovery.format, decoded.format);
    assert_eq!(recovery.records, 200);

    // A new journal for the same document waits for a decision.
    let pending = store.begin_default(
        DocumentKey::Path(path.clone()),
        buffer.snapshot(),
        buffer.format(),
        None,
    );
    assert!(matches!(pending, Err(JournalError::PendingRecovery(_))));
    store.discard(&listed[0].id).expect("discard");
    assert!(store.recoverable().expect("list").is_empty());
}

#[test]
fn compaction_bounds_the_journal_and_keeps_the_text() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let path = dir.path().join("note.md");
    fs::write(&path, "start\n").expect("write");
    let mut buffer = Buffer::new("start\n");
    let small = JournalPolicy {
        compact_min_bytes: 2048,
        ..policy()
    };
    let mut journal = begin(&store, &path, &buffer, small);
    let journal_file = store.dir().join(journal.id().as_str());
    let mut largest = 0;
    for step in 0..3000 {
        edit(&mut buffer, &mut journal, step);
        largest = largest.max(fs::metadata(&journal_file).expect("journal").len());
    }
    let text_len = buffer.len() as u64;
    assert!(
        largest < 2 * (2048 + 3 * text_len) + 4096,
        "journal grew to {largest} bytes for a {text_len}-byte text"
    );
    drop(journal);
    let id = &store.recoverable().expect("list")[0].id;
    let recovery = store.recover(id).expect("recover");
    assert_eq!(recovery.text, buffer.to_string());
    assert_eq!(recovery.base.as_deref(), Some("start\n"));
}

#[test]
fn a_large_base_comes_from_disk_or_a_snapshot() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let path = dir.path().join("big.md");
    let base = "line of text\n".repeat(100);
    fs::write(&path, &base).expect("write");
    let mut buffer = Buffer::new(&base);
    let no_inline = JournalPolicy {
        inline_base_limit: 16,
        ..policy()
    };
    let mut journal = begin(&store, &path, &buffer, no_inline);
    for step in 0..10 {
        edit(&mut buffer, &mut journal, step);
    }
    let id = journal.id().clone();
    drop(journal);
    let recovery = store.recover(&id).expect("base from disk");
    assert_eq!(recovery.text, buffer.to_string());
    assert_eq!(recovery.base.as_deref(), Some(base.as_str()));

    // The file changed and no snapshot exists: the edits cannot be replayed.
    fs::write(&path, "changed by someone else\n").expect("write");
    assert!(matches!(
        store.recover(&id),
        Err(JournalError::BaseUnavailable(_))
    ));
    store.discard(&id).expect("discard");

    // After a snapshot, the base is not needed to rebuild the text.
    let mut buffer = Buffer::new("changed by someone else\n");
    let mut journal = begin(&store, &path, &buffer, no_inline);
    journal.record_snapshot(buffer.text()).expect("snapshot");
    edit(&mut buffer, &mut journal, 1);
    let id = journal.id().clone();
    drop(journal);
    fs::write(&path, "and again\n").expect("write");
    let recovery = store.recover(&id).expect("snapshot");
    assert_eq!(recovery.text, buffer.to_string());
    assert_eq!(recovery.base, None);
    assert_eq!(recovery.disk, DiskComparison::Changed);
}

#[test]
fn a_cut_short_record_loses_only_itself() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let path = dir.path().join("note.md");
    fs::write(&path, "abc\n").expect("write");
    let mut buffer = Buffer::new("abc\n");
    let mut journal = begin(&store, &path, &buffer, policy());
    for step in 0..5 {
        edit(&mut buffer, &mut journal, step);
    }
    let after_five = buffer.to_string();
    edit(&mut buffer, &mut journal, 5);
    let id = journal.id().clone();
    drop(journal);
    let journal_file = store.dir().join(id.as_str());
    let bytes = fs::read(&journal_file).expect("read");
    fs::write(&journal_file, &bytes[..bytes.len() - 3]).expect("truncate");
    let recovery = store.recover(&id).expect("recover");
    assert_eq!(recovery.text, after_five);
    assert!(matches!(recovery.tail, JournalTail::Truncated { .. }));
    assert_eq!(recovery.records, 5);
}

#[test]
fn format_changes_and_untitled_documents_are_journaled() {
    let dir = tempfile::tempdir().expect("temp dir");
    let locations = Locations::with_root(dir.path());
    let store = JournalStore::for_vault(&locations, NO_VAULT_ID).expect("store");
    assert_eq!(
        store.dir(),
        locations.data_dir().join("journal").join(NO_VAULT_ID)
    );
    assert!(JournalStore::for_vault(&locations, "../escape").is_err());

    let mut buffer = Buffer::new("");
    let mut journal = store
        .begin(
            DocumentKey::Untitled(3),
            buffer.snapshot(),
            TextFormat::default(),
            None,
            policy(),
            ManualClock::new(),
        )
        .expect("journal");
    let utf16 = TextFormat {
        encoding: Encoding::Utf16Le,
        bom: Bom::Present,
        line_ending: LineEnding::CrLf,
    };
    journal.record_format(utf16).expect("format");
    edit(&mut buffer, &mut journal, 0);
    drop(journal);
    let listed = store.recoverable().expect("list");
    assert_eq!(listed[0].id.as_str(), "untitled-3");
    let recovery = store.recover(&listed[0].id).expect("recover");
    assert_eq!(recovery.key, DocumentKey::Untitled(3));
    assert_eq!(recovery.text, buffer.to_string());
    assert_eq!(recovery.format, utf16);
    assert_eq!(recovery.disk, DiskComparison::NoFile);
    assert_eq!(recovery.base.as_deref(), Some(""));
}

#[test]
fn sync_runs_at_most_once_per_interval_and_on_demand() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let path = dir.path().join("note.md");
    fs::write(&path, "x\n").expect("write");
    let mut buffer = Buffer::new("x\n");
    let clock = ManualClock::new();
    let mut journal = store
        .begin(
            DocumentKey::Path(path.clone()),
            buffer.snapshot(),
            TextFormat::default(),
            None,
            JournalPolicy::default(),
            clock.clone(),
        )
        .expect("journal");
    assert_eq!(journal.next_sync_deadline(), None);
    let start = clock.now();
    let t = Transaction::new().insert(0, "a");
    buffer.apply(&t).expect("edit");
    journal.record(&t, buffer.text()).expect("record");
    // The journal's start (header and base) is synced at once; the edit
    // record after it waits for the interval.
    assert_eq!(
        journal.next_sync_deadline(),
        Some(start + Duration::from_secs(2))
    );
    clock.advance(Duration::from_millis(500));
    let t = Transaction::new().insert(0, "b");
    buffer.apply(&t).expect("edit");
    journal.record(&t, buffer.text()).expect("record");
    assert_eq!(
        journal.next_sync_deadline(),
        Some(start + Duration::from_secs(2))
    );
    assert!(!journal.sync_if_due().expect("sync"));
    clock.advance(Duration::from_millis(1500));
    assert!(journal.sync_if_due().expect("sync"));
    assert_eq!(journal.next_sync_deadline(), None);
    // Recorded data is readable before any sync (the OS has it).
    clock.advance(Duration::from_millis(100));
    let t = Transaction::new().insert(0, "c");
    buffer.apply(&t).expect("edit");
    journal.record(&t, buffer.text()).expect("record");
    let recovery = store.recover(journal.id()).expect("recover");
    assert_eq!(recovery.text, "cbax\n");
    journal.sync().expect("sync");
    journal.discard().expect("discard");
    assert!(store.recoverable().expect("list").is_empty());
}

#[test]
fn a_save_clears_the_journal_and_moves_the_base() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let path = dir.path().join("note.md");
    fs::write(&path, "one\n").expect("write");
    let (mut file, decoded) = DocumentFile::load(&path).expect("load");
    let mut buffer = Buffer::from_decoded(&decoded);
    let mut journal = begin(&store, &path, &buffer, policy());
    edit(&mut buffer, &mut journal, 1);
    file.save(buffer.text(), &SaveOptions::default(), &mut journal)
        .expect("save");
    journal
        .saved(
            buffer.snapshot(),
            file.format(),
            file.baseline().expect("saved").hash,
        )
        .expect("clear");
    assert!(store.recoverable().expect("list").is_empty());
    assert_eq!(journal.base().to_string(), buffer.to_string());

    // A dirty buffer meets an external change: merge against the journal
    // base.
    let base = journal.base().to_string();
    let t = Transaction::new().insert(0, "mine\n");
    buffer.apply(&t).expect("edit");
    journal.record(&t, buffer.text()).expect("record");
    fs::write(&path, format!("{base}theirs\n")).expect("external");
    let merged = merge3(&base, &buffer.to_string(), &format!("{base}theirs\n"));
    assert_eq!(
        merged.clean_text(),
        Some(format!("mine\n{base}theirs\n")),
        "non-overlapping changes merge"
    );
    drop(journal);
    let id = &store.recoverable().expect("list")[0].id;
    let recovery = store.recover(id).expect("recover");
    assert_eq!(recovery.disk, DiskComparison::Changed);
    assert_eq!(recovery.base.as_deref(), Some(base.as_str()));
    assert_eq!(recovery.text, buffer.to_string());
}

#[test]
fn an_interrupted_in_place_write_can_be_restored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let path = dir.path().join("linked.md");
    fs::write(&path, "original contents\n").expect("write");
    let buffer = Buffer::new("original contents\n");
    let mut journal = begin(&store, &path, &buffer, policy());
    // The backup is stored, then the "process dies" mid-write.
    journal
        .store(&path, b"original contents\n")
        .expect("backup");
    fs::write(&path, "origi").expect("half-written file");
    drop(journal);

    let listed = store.recoverable().expect("list");
    assert_eq!(listed.len(), 1);
    assert!(listed[0].has_backup && !listed[0].has_journal);
    assert_eq!(listed[0].key, Some(DocumentKey::Path(path.clone())));
    let recovery = store.recover(&listed[0].id).expect("recover");
    assert_eq!(
        recovery.backup.as_deref(),
        Some(&b"original contents\n"[..])
    );
    assert_eq!(recovery.disk, DiskComparison::Changed);
    assert_eq!(recovery.text, "original contents\n");
    assert_eq!(store.restore_backup(&listed[0].id).expect("restore"), path);
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "original contents\n"
    );
    assert!(store.recoverable().expect("list").is_empty());
}

#[test]
fn stale_temporary_files_are_removed_and_ignored() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    fs::write(store.dir().join(".0123.gv-tmp-abc"), "x").expect("write");
    fs::write(store.dir().join("unrelated.txt"), "x").expect("write");
    assert!(store.recoverable().expect("list").is_empty());
    assert_eq!(store.remove_stale_temp_files().expect("clean"), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(store.dir())
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    let _ = TextSnapshot::new("");
}

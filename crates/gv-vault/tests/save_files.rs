// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Save strategies and failure handling: atomic replace with metadata,
//! symlinks, hardlinks, read-only files, injected failures, external
//! changes, Save As. All files live in temporary directories.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gv_text::{Buffer, TextFormat, Transaction};
use gv_vault::files::journal::{DocumentKey, JournalStore};
use gv_vault::files::{
    Backup, CheckMode, DocumentFile, ExternalChange, Fault, InPlaceReason, OnExternalChange,
    SaveError, SaveOptions, SaveOutcome, WriteError, WriteMethod, WriteStrategy, merge3,
    reload_transaction,
};

/// A backup sink that keeps the copy in memory and counts calls.
#[derive(Default)]
struct MemoryBackup {
    stored: Option<Vec<u8>>,
    stores: usize,
    clears: usize,
}

impl Backup for MemoryBackup {
    fn store(&mut self, _target: &Path, original: &[u8]) -> io::Result<()> {
        self.stored = Some(original.to_vec());
        self.stores += 1;
        Ok(())
    }

    fn clear(&mut self, _target: &Path) -> io::Result<()> {
        self.stored = None;
        self.clears += 1;
        Ok(())
    }
}

fn setup(text: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("note.md");
    fs::write(&path, text).expect("write");
    (dir, path)
}

fn load_and_edit(path: &Path, insert: &str) -> (DocumentFile, Buffer) {
    let (file, decoded) = DocumentFile::load(path).expect("load");
    let mut buffer = Buffer::from_decoded(&decoded);
    let end = buffer.len();
    buffer
        .apply(&Transaction::new().insert(end, insert))
        .expect("edit");
    (file, buffer)
}

fn temp_files(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .expect("list")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".gv-tmp-"))
        .collect()
}

fn saved(outcome: SaveOutcome) -> gv_vault::files::WriteReport {
    match outcome {
        SaveOutcome::Written(report) => *report,
        SaveOutcome::Unchanged => panic!("expected a write"),
    }
}

#[cfg(unix)]
fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata("/proc/self")
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
}

#[cfg(unix)]
#[test]
fn atomic_replace_keeps_mode_owner_and_extended_attributes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (dir, path) = setup("one\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod");
    let xattr = rustix::fs::setxattr(
        &path,
        "user.glyphvane.test",
        b"tag-value",
        rustix::fs::XattrFlags::empty(),
    )
    .is_ok();
    let before = fs::metadata(&path).expect("stat");

    let (mut file, buffer) = load_and_edit(&path, "two\n");
    let report = saved(
        file.save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect("save"),
    );
    assert_eq!(report.method, WriteMethod::AtomicReplace);
    assert!(
        report.not_preserved.is_empty(),
        "{:?}",
        report.not_preserved
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "one\ntwo\n");
    let after = fs::metadata(&path).expect("stat");
    assert_ne!(after.ino(), before.ino(), "replaced by rename");
    assert_eq!(after.permissions().mode() & 0o7777, 0o640);
    assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
    if xattr {
        let mut value = [0u8; 64];
        let n = rustix::fs::getxattr(&path, "user.glyphvane.test", &mut value[..]).expect("xattr");
        assert_eq!(&value[..n], b"tag-value");
    } else {
        eprintln!("extended attributes not supported here; skipped that check");
    }
    assert!(temp_files(dir.path()).is_empty());
    assert_eq!(file.baseline().map(|b| b.len), Some(8));
}

#[cfg(unix)]
#[test]
fn a_symlink_target_is_written_and_the_link_kept() {
    let (dir, real) = setup("target\n");
    let link = dir.path().join("link.md");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let (mut file, buffer) = load_and_edit(&link, "more\n");
    let report = saved(
        file.save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect("save"),
    );
    assert_eq!(report.target, fs::canonicalize(&real).expect("canonical"));
    assert!(
        fs::symlink_metadata(&link)
            .expect("lstat")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&real).expect("read"), "target\nmore\n");
    assert_eq!(file.path(), link, "the document keeps its path");

    // A dangling link is refused.
    let dangling = dir.path().join("dangling.md");
    std::os::unix::fs::symlink(dir.path().join("missing.md"), &dangling).expect("symlink");
    let mut file = DocumentFile::new_file(&dangling, TextFormat::default());
    let err = file
        .save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect_err("dangling");
    assert!(
        matches!(err, SaveError::Write(WriteError::DanglingSymlink { .. })),
        "{err:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_hardlinked_file_is_written_in_place_after_a_backup() {
    use std::os::unix::fs::MetadataExt;
    let (dir, path) = setup("shared\n");
    let other = dir.path().join("other-name.md");
    fs::hard_link(&path, &other).expect("hard link");
    let before = fs::metadata(&path).expect("stat");

    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let (mut file, buffer) = load_and_edit(&path, "edited\n");
    let mut journal = store
        .begin_default(
            DocumentKey::Path(path.clone()),
            gv_text::TextSnapshot::new("shared\n"),
            file.format(),
            file.baseline().map(|b| b.hash),
        )
        .expect("journal");
    let report = saved(
        file.save(buffer.text(), &SaveOptions::default(), &mut journal)
            .expect("save"),
    );
    assert_eq!(
        report.method,
        WriteMethod::InPlace(InPlaceReason::Hardlinked { links: 2 })
    );
    let after = fs::metadata(&path).expect("stat");
    assert_eq!(after.ino(), before.ino());
    assert_eq!(after.nlink(), 2, "the link survives");
    assert_eq!(
        fs::read_to_string(&other).expect("read"),
        "shared\nedited\n"
    );
    assert!(
        store.recoverable().expect("list").is_empty(),
        "the backup was cleared after the write"
    );

    // A failure halfway through puts the original back.
    let mut buffer = buffer;
    buffer
        .apply(&Transaction::new().insert(0, "# "))
        .expect("edit");
    let mut backup = MemoryBackup::default();
    let options = SaveOptions {
        fault: Some(Fault::MidWrite),
        ..SaveOptions::default()
    };
    let err = file
        .save(buffer.text(), &options, &mut backup)
        .expect_err("injected");
    assert!(
        matches!(
            err,
            SaveError::Write(WriteError::InPlace { restored: true, .. })
        ),
        "{err:?}"
    );
    assert_eq!(
        fs::read_to_string(&other).expect("read"),
        "shared\nedited\n"
    );
    assert_eq!((backup.stores, backup.clears), (1, 1));
}

#[test]
fn in_place_on_request_keeps_the_file() {
    let (_dir, path) = setup("a\n");
    let (mut file, buffer) = load_and_edit(&path, "b\n");
    let options = SaveOptions {
        strategy: WriteStrategy::InPlace,
        ..SaveOptions::default()
    };
    let mut backup = MemoryBackup::default();
    let report = saved(
        file.save(buffer.text(), &options, &mut backup)
            .expect("save"),
    );
    assert_eq!(
        report.method,
        WriteMethod::InPlace(InPlaceReason::Requested)
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "a\nb\n");
    assert_eq!((backup.stores, backup.clears, backup.stored), (1, 1, None));
}

#[test]
fn a_failure_before_the_rename_leaves_the_original_and_no_temporary_file() {
    let (dir, path) = setup("original\n");
    let (mut file, buffer) = load_and_edit(&path, "lost?\n");
    let options = SaveOptions {
        fault: Some(Fault::BeforeRename),
        ..SaveOptions::default()
    };
    let err = file
        .save(buffer.text(), &options, &mut MemoryBackup::default())
        .expect_err("injected");
    assert!(
        matches!(err, SaveError::Write(WriteError::Io { .. })),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "original\n");
    assert!(
        temp_files(dir.path()).is_empty(),
        "{:?}",
        temp_files(dir.path())
    );
    // The baseline is unchanged, so a retry works.
    saved(
        file.save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect("retry"),
    );
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "original\nlost?\n"
    );
}

#[cfg(unix)]
#[test]
fn read_only_files_and_directories_refuse_cleanly() {
    use std::os::unix::fs::PermissionsExt;
    if is_root() {
        eprintln!("running as root: permission checks do not apply");
        return;
    }
    let (dir, path) = setup("keep\n");
    let (mut file, buffer) = load_and_edit(&path, "x\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).expect("chmod");
    let err = file
        .save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect_err("read-only");
    assert!(
        matches!(err, SaveError::Write(WriteError::ReadOnly { .. })),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "keep\n");

    // Writable file in a directory where no temporary file can be created.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).expect("chmod dir");
    let result = file.save(
        buffer.text(),
        &SaveOptions::default(),
        &mut MemoryBackup::default(),
    );
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).expect("restore dir");
    let err = result.expect_err("no temp file possible");
    assert!(
        matches!(err, SaveError::Write(WriteError::Io { .. })),
        "{err:?}"
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "keep\n");
    assert!(temp_files(dir.path()).is_empty());
}

#[test]
fn external_changes_are_never_overwritten_silently() {
    let (_dir, path) = setup("base\n");
    let (mut file, buffer) = load_and_edit(&path, "mine\n");
    fs::write(&path, "base\ntheirs\n").expect("external write");

    let err = file
        .save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect_err("conflict");
    let SaveError::ExternalChange { change, .. } = err else {
        panic!("expected ExternalChange, got {err:?}");
    };
    let ExternalChange::Modified { bytes, .. } = *change else {
        panic!("expected Modified");
    };
    assert_eq!(bytes, b"base\ntheirs\n");
    assert_eq!(fs::read_to_string(&path).expect("read"), "base\ntheirs\n");

    // Compare / merge against the base, then save the merge over theirs.
    let merged = merge3("base\n", &buffer.to_string(), "base\ntheirs\n");
    assert_eq!(merged.conflict_count(), 1, "both appended at the end");
    let options = SaveOptions {
        on_external_change: OnExternalChange::Overwrite,
        ..SaveOptions::default()
    };
    saved(
        file.save(buffer.text(), &options, &mut MemoryBackup::default())
            .expect("keep mine"),
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "base\nmine\n");

    // Deleted: refused, then recreated on request.
    fs::remove_file(&path).expect("delete");
    let mut buffer = buffer;
    buffer
        .apply(&Transaction::new().insert(0, "> "))
        .expect("edit");
    let err = file
        .save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect_err("deleted");
    assert!(
        matches!(&err, SaveError::ExternalChange { change, .. } if **change == ExternalChange::Deleted),
        "{err:?}"
    );
    let report = saved(
        file.save(buffer.text(), &options, &mut MemoryBackup::default())
            .expect("recreate"),
    );
    assert_eq!(report.method, WriteMethod::Created);
    assert_eq!(fs::read_to_string(&path).expect("read"), "> base\nmine\n");
}

#[test]
fn a_disk_that_already_holds_the_text_is_not_written() {
    let (_dir, path) = setup("a\n");
    let (mut file, buffer) = load_and_edit(&path, "b\n");
    fs::write(&path, "a\nb\n").expect("same text written elsewhere");
    let outcome = file
        .save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect("save");
    assert_eq!(outcome, SaveOutcome::Unchanged);
    assert_eq!(
        file.check_external_change(CheckMode::Thorough)
            .expect("check"),
        ExternalChange::Unchanged,
        "the baseline moved to the disk state"
    );
}

#[test]
fn a_clean_buffer_reloads_as_a_minimal_transaction() {
    let (_dir, path) = setup("# Title\n\nParagraph one.\nParagraph two.\n");
    let (mut file, decoded) = DocumentFile::load(&path).expect("load");
    let mut buffer = Buffer::from_decoded(&decoded);
    fs::write(&path, "# Title\n\nParagraph 1.\nParagraph two.\n").expect("external");
    let ExternalChange::Modified { state, bytes } =
        file.check_external_change(CheckMode::Quick).expect("check")
    else {
        panic!("expected a modification");
    };
    let new = gv_text::decode(&bytes).expect("text");
    let transaction = reload_transaction(&buffer.to_string(), &new.text);
    assert_eq!(transaction.replacements().len(), 1);
    assert_eq!(transaction.replacements()[0].range, 19..22);
    buffer.apply(&transaction).expect("apply");
    file.accept_disk_version(&new, state);
    assert_eq!(buffer.to_string(), new.text);
    assert_eq!(
        file.check_external_change(CheckMode::Thorough)
            .expect("check"),
        ExternalChange::Unchanged
    );
}

#[test]
fn save_as_refuses_to_replace_an_existing_file_unasked() {
    let (dir, path) = setup("source\n");
    let (mut file, buffer) = load_and_edit(&path, "edited\n");
    let copy = dir.path().join("copy.md");
    let report = saved(
        file.save_as(
            &copy,
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect("save as"),
    );
    assert_eq!(report.method, WriteMethod::Created);
    assert_eq!(file.path(), copy);
    assert_eq!(fs::read_to_string(&path).expect("read"), "source\n");

    let existing = dir.path().join("existing.md");
    fs::write(&existing, "keep me\n").expect("write");
    let err = file
        .save_as(
            &existing,
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect_err("exists");
    assert!(matches!(err, SaveError::ExternalChange { .. }), "{err:?}");
    assert_eq!(file.path(), copy, "still the previous target");
    assert_eq!(fs::read_to_string(&existing).expect("read"), "keep me\n");
    let options = SaveOptions {
        on_external_change: OnExternalChange::Overwrite,
        ..SaveOptions::default()
    };
    file.save_as(
        &existing,
        buffer.text(),
        &options,
        &mut MemoryBackup::default(),
    )
    .expect("confirmed");
    assert_eq!(
        fs::read_to_string(&existing).expect("read"),
        "source\nedited\n"
    );
}

#[test]
fn an_untitled_document_moves_from_the_journal_to_its_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let mut buffer = Buffer::new("");
    let mut journal = store
        .begin_default(
            DocumentKey::Untitled(7),
            buffer.snapshot(),
            TextFormat::default(),
            None,
        )
        .expect("journal");
    let edit = Transaction::new().insert(0, "draft\n");
    buffer.apply(&edit).expect("edit");
    journal.record(&edit, buffer.text()).expect("record");
    // Open here, so in use rather than recoverable.
    let scan = store.scan().expect("scan");
    assert!(scan.recoverable.is_empty());
    assert_eq!(scan.in_use.len(), 1);
    assert_eq!(scan.in_use[0].key, Some(DocumentKey::Untitled(7)));

    let path = dir.path().join("Draft.md");
    let mut file = DocumentFile::new_file(&path, TextFormat::default());
    saved(
        file.save(buffer.text(), &SaveOptions::default(), &mut journal)
            .expect("first save"),
    );
    journal
        .saved_as(
            DocumentKey::Path(path.clone()),
            buffer.snapshot(),
            file.format(),
            file.baseline().expect("saved").hash,
        )
        .expect("retarget");
    assert_eq!(store.scan().expect("scan"), Default::default());
    assert_eq!(fs::read_to_string(&path).expect("read"), "draft\n");
}

#[test]
fn touched_files_save_and_metadata_only_changes_are_reported() {
    let (_dir, path) = setup("same\n");
    let (mut file, buffer) = load_and_edit(&path, "more\n");
    let modified = fs::metadata(&path)
        .expect("stat")
        .modified()
        .expect("mtime");
    let handle = fs::File::options().write(true).open(&path).expect("open");
    handle
        .set_modified(modified + std::time::Duration::from_secs(10))
        .expect("touch");
    drop(handle);
    assert!(matches!(
        file.check_external_change(CheckMode::Quick).expect("check"),
        ExternalChange::Touched(_)
    ));
    saved(
        file.save(
            buffer.text(),
            &SaveOptions::default(),
            &mut MemoryBackup::default(),
        )
        .expect("a touch is not a conflict"),
    );
    assert_eq!(fs::read_to_string(&path).expect("read"), "same\nmore\n");
}

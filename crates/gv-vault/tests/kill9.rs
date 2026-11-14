// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! `kill -9` recovery.
//!
//! The parent test re-runs this test binary as a child process (the ignored
//! `kill9_child` test, which does nothing unless the parent's environment
//! variable is set). The child edits a document with a deterministic stream
//! of transactions, journals each one, saves every few dozen edits and
//! prints `ack N` after edit N is journaled. The parent kills the child with
//! SIGKILL (`Child::kill`, sent to the PID of the child it spawned) at a
//! random point, then checks:
//!
//! - the journal rebuilds the text after the last acknowledged edit, or the
//!   one after it (journaled but not yet acknowledged): nothing recorded is
//!   lost;
//! - the file on disk is always a complete saved version, never torn
//!   (atomic mode), or is restored from the journal's backup when an
//!   in-place write was interrupted (hardlink mode).
//!
//! Each round uses its own temporary directory. Set `GV_KILL9_SEED` to
//! replay a run, `GV_KILL9_ROUNDS` to change the number of rounds per mode.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gv_text::{Bias, Buffer, Transaction};
use gv_vault::files::journal::{DocumentKey, JournalPolicy, JournalStore, JournalTail};
use gv_vault::files::{DocumentFile, SaveOptions, SystemClock};

const CHILD_DIR_ENV: &str = "GV_KILL9_CHILD_DIR";
const INITIAL: &str = "# Kill test\r\n\r\nSome text to edit, with ünïcödé and 日本語.\nLast line\r";
const SAVE_EVERY: u64 = 37;
const MAX_STEPS: u64 = 20_000;

/// splitmix64: a tiny deterministic generator, the same in parent and
/// child.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Edit `step` of the stream, given the text before it.
fn transaction_for(buffer: &Buffer, step: u64) -> Transaction {
    let r = mix(step);
    let len = buffer.len() as u64;
    let at = buffer.clip_offset(usize::try_from(r % (len + 1)).unwrap_or(0), Bias::Left);
    const WORDS: [&str; 8] = [
        "alpha ",
        "β ",
        "日本 ",
        "😀",
        "\n",
        "\r\n",
        "\r",
        "- [ ] task ",
    ];
    let word = WORDS[usize::try_from((r >> 8) % 8).unwrap_or(0)];
    match (r >> 16) % 4 {
        // Delete a few bytes, but keep the text from shrinking to nothing.
        0 if len > 40 => {
            let end = buffer.clip_offset(
                at + usize::try_from((r >> 24) % 9).unwrap_or(0),
                Bias::Right,
            );
            Transaction::new().delete(at..end.min(buffer.len()))
        }
        1 if len > 40 => {
            let end = buffer.clip_offset(
                at + usize::try_from((r >> 24) % 5).unwrap_or(0),
                Bias::Right,
            );
            Transaction::new().replace(at..end.min(buffer.len()), format!("{word}{step}"))
        }
        _ => Transaction::new().insert(at, format!("{word}{step} ")),
    }
}

/// The text after `steps` edits.
fn texts_up_to(steps: u64) -> Vec<String> {
    let mut buffer = Buffer::new(INITIAL);
    let mut texts = vec![buffer.to_string()];
    for step in 1..=steps {
        let transaction = transaction_for(&buffer, step);
        buffer.apply(&transaction).expect("stream edit applies");
        texts.push(buffer.to_string());
    }
    texts
}

fn child_policy() -> JournalPolicy {
    JournalPolicy {
        // Compact often, so kills also land in the middle of compactions.
        compact_min_bytes: 2048,
        ..JournalPolicy::default()
    }
}

/// The child: edits, journals and saves until it is killed.
fn run_child(dir: &Path) {
    let doc = dir.join("doc.md");
    let store = JournalStore::open(dir.join("journal")).expect("store");
    let (mut file, decoded) = DocumentFile::load(&doc).expect("load");
    let mut buffer = Buffer::from_decoded(&decoded);
    let mut journal = store
        .begin(
            DocumentKey::Path(doc.clone()),
            buffer.snapshot(),
            file.format(),
            file.baseline().map(|b| b.hash),
            child_policy(),
            SystemClock,
        )
        .expect("journal");
    let mut out = std::io::stdout().lock();
    for step in 1..=MAX_STEPS {
        let transaction = transaction_for(&buffer, step);
        buffer.apply(&transaction).expect("edit");
        journal.record(&transaction, buffer.text()).expect("record");
        if step % SAVE_EVERY == 0 {
            file.save(buffer.text(), &SaveOptions::default(), &mut journal)
                .expect("save");
            let hash = file.baseline().expect("saved").hash;
            journal
                .saved(buffer.snapshot(), file.format(), hash)
                .expect("clear journal");
        }
        writeln!(out, "ack {step}").expect("stdout");
        out.flush().expect("stdout");
    }
}

#[test]
#[ignore = "child process of `kill9_recovery`; does nothing on its own"]
fn kill9_child() {
    if let Some(dir) = std::env::var_os(CHILD_DIR_ENV) {
        run_child(Path::new(&dir));
    }
}

struct Round {
    mode: &'static str,
    seed: u64,
    target: u64,
    spin: Duration,
}

/// Runs one child to `target` acknowledgements, kills it, and returns the
/// last acknowledged step.
fn run_round(exe: &Path, dir: &Path, round: &Round) -> u64 {
    let mut child = Command::new(exe)
        .args([
            "--exact",
            "kill9_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_DIR_ENV, dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");
    // Only this PID is ever signalled.
    let pid = child.id();
    let stdout = child.stdout.take().expect("child stdout");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            // The test harness may print its own text on the same line.
            if let Some(n) = line
                .split_once("ack ")
                .and_then(|(_, n)| n.trim().parse::<u64>().ok())
                && tx.send(n).is_err()
            {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = 0;
    while last < round.target {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(n) => last = n,
            Err(_) => {
                let _ = child.kill();
                let output = child.wait_with_output().ok();
                panic!(
                    "child {pid} stopped acknowledging at {last} (seed {}): {:?}",
                    round.seed,
                    output.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
                );
            }
        }
    }
    // Let the child run on for a moment so the kill lands anywhere in its
    // loop: mid-write, mid-compaction, mid-save.
    let spin_until = Instant::now() + round.spin;
    while Instant::now() < spin_until {
        std::hint::spin_loop();
    }
    child.kill().expect("SIGKILL the child");
    let status = child.wait().expect("reap the child");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "child {pid} was killed by SIGKILL"
        );
    }
    #[cfg(not(unix))]
    assert!(!status.success());
    reader.join().expect("reader thread");
    while let Ok(n) = rx.try_recv() {
        last = last.max(n);
    }
    last
}

/// Checks the state a killed child left behind.
fn verify(dir: &Path, round: &Round, acked: u64) -> &'static str {
    let doc = dir.join("doc.md");
    let texts = texts_up_to(acked + 1);
    let saved_versions: Vec<&String> = (0..=acked + 1)
        .filter(|s| s % SAVE_EVERY == 0)
        .map(|s| &texts[usize::try_from(s).unwrap_or(0)])
        .collect();
    let context = format!(
        "mode {} seed {} target {} acked {acked}",
        round.mode, round.seed, round.target
    );
    let store = JournalStore::open(dir.join("journal")).expect("store");
    let listed = store.recoverable().expect("list");
    let candidates = [&texts[texts.len() - 2], &texts[texts.len() - 1]];

    let outcome = match listed.as_slice() {
        [] => {
            // Killed between a save and the next edit: the disk holds it all.
            let disk = fs::read_to_string(&doc).expect("doc");
            assert!(candidates.contains(&&disk), "{context}: disk lost edits");
            "saved"
        }
        [doc_journal] => {
            let recovery = store.recover(&doc_journal.id).expect("recover");
            assert!(
                candidates.contains(&&recovery.text),
                "{context}: recovered text is not the text after edit {acked} or {}",
                acked + 1
            );
            assert!(
                !matches!(recovery.tail, JournalTail::Corrupt { .. }),
                "{context}: corrupt journal"
            );
            match &recovery.backup {
                Some(backup) => {
                    assert_eq!(round.mode, "hardlink", "{context}: backup in atomic mode");
                    let backup = String::from_utf8(backup.clone()).expect("utf-8");
                    assert!(
                        saved_versions.contains(&&backup),
                        "{context}: backup is not a saved version"
                    );
                    store.restore_backup(&doc_journal.id).expect("restore");
                    assert_eq!(fs::read_to_string(&doc).expect("doc"), backup);
                    "journal + backup"
                }
                None => {
                    if recovery.tail == JournalTail::Complete {
                        "journal"
                    } else {
                        "journal (cut short)"
                    }
                }
            }
        }
        more => panic!("{context}: more than one journal: {more:?}"),
    };
    // The file is a complete saved version (after any backup restore).
    let disk = fs::read_to_string(&doc).expect("doc");
    assert!(
        saved_versions.contains(&&disk),
        "{context}: the file on disk is not a saved version"
    );
    if round.mode == "hardlink" {
        let alias = fs::read_to_string(dir.join("alias.md")).expect("alias");
        assert_eq!(alias, disk, "{context}: the hard link was split");
    }
    outcome
}

fn prepare(dir: &Path, mode: &str) -> PathBuf {
    let doc = dir.join("doc.md");
    fs::write(&doc, INITIAL).expect("write doc");
    if mode == "hardlink" {
        fs::hard_link(&doc, dir.join("alias.md")).expect("hard link");
    }
    doc
}

#[test]
fn kill9_recovery() {
    let exe = std::env::current_exe().expect("test binary");
    let seed: u64 = std::env::var("GV_KILL9_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x005E_ED0F_6A11);
    let rounds: u64 = std::env::var("GV_KILL9_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);
    let modes: &[&'static str] = if cfg!(unix) {
        &["atomic", "hardlink"]
    } else {
        &["atomic"]
    };
    let mut tally: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for (m, &mode) in modes.iter().enumerate() {
        for i in 0..rounds {
            let r = mix(seed ^ mix(i + 1000 * m as u64));
            let round = Round {
                mode,
                seed,
                target: 1 + r % 400,
                spin: Duration::from_micros((r >> 32) % 3000),
            };
            let dir = tempfile::tempdir().expect("temp dir");
            prepare(dir.path(), mode);
            let acked = run_round(&exe, dir.path(), &round);
            let outcome = verify(dir.path(), &round, acked);
            *tally.entry(format!("{mode}: {outcome}")).or_default() += 1;
        }
    }
    eprintln!("kill -9 rounds (seed {seed}): {tally:?}");
    assert_eq!(tally.values().sum::<u64>(), rounds * modes.len() as u64);
}

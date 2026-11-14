// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The data-safety matrix: encodings × BOM × line-ending styles × edit
//! positions. Every save must reproduce the bytes outside the edit exactly,
//! a no-op edit must write nothing, and characters the encoding cannot hold
//! must refuse the save. All files live in temporary directories.

use std::fs;
use std::path::{Path, PathBuf};

use gv_text::encoding::encode;
use gv_text::{Bom, Buffer, Encoding, LineEnding, TextFormat, Transaction};
use gv_vault::files::journal::{DocumentJournal, DocumentKey, JournalStore};
use gv_vault::files::{
    Backup, DocumentFile, ReadOnlyReason, SaveError, SaveOptions, SaveOutcome, SystemClock,
};

#[derive(Clone, Copy, Debug)]
enum Eol {
    Lf,
    CrLf,
    Cr,
    Mixed,
}

const EOLS: [Eol; 4] = [Eol::Lf, Eol::CrLf, Eol::Cr, Eol::Mixed];

impl Eol {
    /// The terminator after line `i`.
    fn after(self, i: usize) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
            Self::Mixed => ["\n", "\r\n", "\r"][i % 3],
        }
    }
}

/// Joins lines with the style's terminators; the last line keeps one too.
fn join(lines: &[&str], eol: Eol) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{line}{}", eol.after(i)))
        .collect()
}

/// Converts LF-terminated text to the style.
fn convert(text: &str, eol: Eol) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut i = 0;
    for ch in text.chars() {
        if ch == '\n' {
            out.push_str(eol.after(i));
            i += 1;
        } else {
            out.push(ch);
        }
    }
    out
}

struct Case {
    name: &'static str,
    format: TextFormat,
    lines: Vec<&'static str>,
    /// Text the edits insert; representable in the encoding.
    insert: &'static str,
}

fn label(name: &str) -> Encoding {
    Encoding::for_label(name).expect("known encoding")
}

fn format(encoding: Encoding, bom: Bom) -> TextFormat {
    TextFormat {
        encoding,
        bom,
        line_ending: LineEnding::Lf,
    }
}

const UNICODE_LINES: &[&str] = &[
    "---",
    "title: \"Äpfel & Birnen\"",
    "tags: [obst, 日本]",
    "---",
    "# Überschrift 🍎",
    "",
    "Some text with ünïcödé, 日本語, Ελληνικά and emoji 😀.   ",
    "- [ ] a task\twith a tab",
    "> a quote with trailing space ",
    "",
    "```rust",
    "fn main() { println!(\"x\"); }",
    "```",
    "Line with a NUL-free control \u{1b}[0m escape.",
    "Last line.",
];

const LATIN_LINES: &[&str] = &[
    "# Überschrift über Äpfel",
    "",
    "Ein schöner Tag in der Straße, über den Fluß, mit Öl und Übermut.",
    "Ça coûte 5 € — «déjà vu», naïve façade, crème brûlée.",
    "Ärger, Öl und Übermut; größer, weißer, süßer.   ",
    "- [ ] Aufgabe mit Tabulator\tund Leerzeichen ",
    "> Zitat: „Grüße aus München“ – sagte er.",
    "Ein schöner Tag in der Straße, über den Fluß, mit Öl und Übermut.",
    "Ça coûte 5 € — «déjà vu», naïve façade, crème brûlée.",
    "Letzte Zeile.",
];

const JAPANESE_LINES: &[&str] = &[
    "# 日本語のテキスト",
    "",
    "これは日本語のテキストです。漢字とひらがなとカタカナ。",
    "東京都の天気は晴れです。明日は雨が降るでしょう。",
    "- [ ] タスク：買い物に行く",
    "> 引用：吾輩は猫である。名前はまだ無い。",
    "これは日本語のテキストです。漢字とひらがなとカタカナ。",
    "東京都の天気は晴れです。明日は雨が降るでしょう。",
    "最後の行。",
];

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "utf-8",
            format: format(Encoding::Utf8, Bom::Absent),
            lines: UNICODE_LINES.to_vec(),
            insert: "Neu: ✓ 新しい 🎉",
        },
        Case {
            name: "utf-8 bom",
            format: format(Encoding::Utf8, Bom::Present),
            lines: UNICODE_LINES.to_vec(),
            insert: "Neu: ✓ 新しい 🎉",
        },
        Case {
            name: "utf-16le bom",
            format: format(Encoding::Utf16Le, Bom::Present),
            lines: UNICODE_LINES.to_vec(),
            insert: "Neu: ✓ 新しい 🎉",
        },
        Case {
            name: "utf-16be bom",
            format: format(Encoding::Utf16Be, Bom::Present),
            lines: UNICODE_LINES.to_vec(),
            insert: "Neu: ✓ 新しい 🎉",
        },
        Case {
            name: "windows-1252",
            format: format(label("windows-1252"), Bom::Absent),
            lines: LATIN_LINES.to_vec(),
            insert: "Neu: Größe €",
        },
        Case {
            name: "shift_jis",
            format: format(label("shift_jis"), Bom::Absent),
            lines: JAPANESE_LINES.to_vec(),
            insert: "新しい行です",
        },
    ]
}

/// For saves that never write in place (no hardlinks here).
struct NoBackup;

impl Backup for NoBackup {
    fn store(&mut self, _target: &Path, _original: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("no in-place writes expected"))
    }

    fn clear(&mut self, _target: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

/// A journal per test file, in its own temporary store.
fn journal(
    store: &JournalStore,
    path: &Path,
    buffer: &Buffer,
    file: &DocumentFile,
) -> DocumentJournal<SystemClock> {
    store
        .begin_default(
            DocumentKey::Path(path.to_path_buf()),
            buffer.snapshot(),
            file.format(),
            file.baseline().map(|b| b.hash),
        )
        .expect("journal")
}

fn write_original(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write original");
    path
}

#[derive(Clone, Copy, Debug)]
enum Position {
    Start,
    Middle,
    End,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Insert,
    Replace,
    Delete,
}

/// The edit at `position` of `kind` in `text`, as (range, new text).
fn edit_for(
    text: &str,
    position: Position,
    kind: Kind,
    insert: &str,
    eol: &str,
) -> (std::ops::Range<usize>, String) {
    let at = match position {
        Position::Start => 0,
        Position::End => text.len(),
        Position::Middle => {
            // The start of a line near the middle, then a few characters in.
            let mid = text.len() / 2;
            let line_start = text[..mid].rfind(['\n', '\r']).map_or(0, |i| i + 1);
            text[line_start..]
                .char_indices()
                .nth(3)
                .map_or(line_start, |(i, _)| line_start + i)
        }
    };
    let next_char = |from: usize, n: usize| {
        text[from..]
            .char_indices()
            .nth(n)
            .map_or(text.len(), |(i, _)| from + i)
    };
    match kind {
        Kind::Insert => (at..at, format!("{insert}{eol}")),
        Kind::Replace => {
            let start = if at == text.len() {
                text.char_indices().rev().nth(4).map_or(0, |(i, _)| i)
            } else {
                at
            };
            (start..next_char(start, 5), insert.to_owned())
        }
        Kind::Delete => {
            let start = if at == text.len() {
                text.char_indices().rev().nth(6).map_or(0, |(i, _)| i)
            } else {
                at
            };
            (start..next_char(start, 7), String::new())
        }
    }
}

#[test]
fn bytes_outside_every_edit_are_identical() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let mut checked = 0;
    for case in cases() {
        for eol in EOLS {
            let text = join(&case.lines, eol);
            let original = encode(&text, &case.format).expect("sample encodes");
            for position in [Position::Start, Position::Middle, Position::End] {
                for kind in [Kind::Insert, Kind::Replace, Kind::Delete] {
                    let what = format!("{} / {eol:?} / {position:?} / {kind:?}", case.name);
                    let path = write_original(dir.path(), &format!("doc-{checked}.md"), &original);

                    let (mut file, decoded) = DocumentFile::load(&path).expect("load");
                    assert!(decoded.status.is_exact(), "{what}: {:?}", decoded.status);
                    assert_eq!(decoded.format.encoding, case.format.encoding, "{what}");
                    assert_eq!(decoded.format.bom, case.format.bom, "{what}");
                    assert_eq!(decoded.text, text, "{what}");

                    let mut buffer = Buffer::from_decoded(&decoded);
                    let mut journal = journal(&store, &path, &buffer, &file);
                    // New lines use the file's dominant ending.
                    let new_line = file.format().line_ending.as_str();
                    let (range, new_text) = edit_for(&text, position, kind, case.insert, new_line);
                    let transaction = Transaction::new().replace(range.clone(), new_text.clone());
                    buffer.apply(&transaction).expect("edit applies");
                    journal
                        .record(&transaction, buffer.text())
                        .expect("journal");

                    let outcome = file
                        .save(buffer.text(), &SaveOptions::default(), &mut journal)
                        .unwrap_or_else(|e| panic!("{what}: save failed: {e}"));
                    assert!(matches!(outcome, SaveOutcome::Written(_)), "{what}");
                    let saved = fs::read(&path).expect("read back");

                    // The prefix and suffix around the edit, in the original
                    // file's bytes (BOM included in the prefix).
                    let bom_len = if case.format.bom == Bom::Present {
                        case.format.encoding.bom().map_or(0, <[u8]>::len)
                    } else {
                        0
                    };
                    let body = TextFormat {
                        bom: Bom::Absent,
                        ..case.format
                    };
                    let prefix_len =
                        bom_len + encode(&text[..range.start], &body).expect("prefix").len();
                    let suffix_len = encode(&text[range.end..], &body).expect("suffix").len();
                    let middle = encode(&new_text, &body).expect("middle");
                    assert_eq!(
                        saved[..prefix_len],
                        original[..prefix_len],
                        "{what}: prefix"
                    );
                    assert_eq!(
                        saved[saved.len() - suffix_len..],
                        original[original.len() - suffix_len..],
                        "{what}: suffix"
                    );
                    assert_eq!(
                        saved[prefix_len..saved.len() - suffix_len],
                        middle[..],
                        "{what}: edit"
                    );
                    journal
                        .saved(
                            buffer.snapshot(),
                            file.format(),
                            file.baseline().expect("saved").hash,
                        )
                        .expect("clear journal");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 6 * 4 * 3 * 3);
    assert!(
        store.recoverable().expect("list").is_empty(),
        "every journal was cleared"
    );
}

#[test]
fn type_and_undo_writes_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    for case in cases() {
        for eol in EOLS {
            let what = format!("{} / {eol:?}", case.name);
            let text = join(&case.lines, eol);
            let original = encode(&text, &case.format).expect("sample encodes");
            let path = write_original(dir.path(), "noop.md", &original);
            let before = fs::metadata(&path).expect("stat");

            let (mut file, decoded) = DocumentFile::load(&path).expect("load");
            let mut buffer = Buffer::from_decoded(&decoded);
            let mut journal = journal(&store, &path, &buffer, &file);
            let at = text.len() / 2;
            let at = (at..text.len())
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(0);
            for transaction in [
                Transaction::new().insert(at, "x"),
                Transaction::new().delete(at..at + 1),
            ] {
                buffer.apply(&transaction).expect("edit");
                journal
                    .record(&transaction, buffer.text())
                    .expect("journal");
            }
            let outcome = file
                .save(buffer.text(), &SaveOptions::default(), &mut journal)
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(outcome, SaveOutcome::Unchanged, "{what}");
            assert_eq!(fs::read(&path).expect("read"), original, "{what}");
            let after = fs::metadata(&path).expect("stat");
            assert_eq!(
                after.modified().ok(),
                before.modified().ok(),
                "{what}: not rewritten"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert_eq!(after.ino(), before.ino(), "{what}: not replaced");
            }
            journal.discard().expect("discard");
        }
    }
}

/// Every CommonMark spec example × encodings × line-ending styles: open,
/// type and undo, save → identical bytes, or (for bytes a legacy encoding
/// cannot round-trip) read-only and untouched.
#[test]
fn spec_corpus_round_trips_byte_for_byte() {
    let spec = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/spec/commonmark-0.31.2.json");
    let json = fs::read_to_string(&spec).expect("spec fixtures");
    let examples: Vec<serde_json::Value> = serde_json::from_str(&json).expect("json");
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("example.md");
    let formats = [
        format(Encoding::Utf8, Bom::Absent),
        format(Encoding::Utf8, Bom::Present),
        format(Encoding::Utf16Le, Bom::Present),
        format(Encoding::Utf16Be, Bom::Present),
        format(label("windows-1252"), Bom::Absent),
        format(label("shift_jis"), Bom::Absent),
    ];
    let (mut identical, mut read_only, mut skipped) = (0, 0, 0);
    for example in &examples {
        let markdown = example["markdown"].as_str().expect("markdown");
        for eol in EOLS {
            let text = convert(markdown, eol);
            for format in &formats {
                let Ok(original) = encode(&text, format) else {
                    skipped += 1; // not representable in this legacy encoding
                    continue;
                };
                fs::write(&path, &original).expect("write");
                let (mut file, decoded) = match DocumentFile::load(&path) {
                    Ok(loaded) => loaded,
                    Err(gv_vault::files::LoadError::Binary { .. }) => {
                        skipped += 1; // control characters sniffed as binary
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                // The journal is covered by the other tests; its fsyncs
                // would dominate this loop's run time.
                let mut buffer = Buffer::from_decoded(&decoded);
                let at = buffer.len();
                for transaction in [
                    Transaction::new().insert(at, "x"),
                    Transaction::new().delete(at..at + 1),
                ] {
                    buffer.apply(&transaction).expect("edit");
                }
                match file.save(buffer.text(), &SaveOptions::default(), &mut NoBackup) {
                    Ok(SaveOutcome::Unchanged) => identical += 1,
                    Err(SaveError::ReadOnly(ReadOnlyReason::Lossy(_))) => read_only += 1,
                    other => panic!(
                        "example {}: {:?} {eol:?}: {other:?}",
                        example["example"], format.encoding
                    ),
                }
                assert_eq!(fs::read(&path).expect("read"), original);
            }
        }
    }
    assert!(examples.len() > 600);
    assert!(identical > examples.len() * 4 * 5, "{identical} identical");
    eprintln!("spec corpus: {identical} identical, {read_only} read-only, {skipped} skipped");
}

#[test]
fn unmappable_characters_refuse_the_save_with_a_position() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    let text = join(LATIN_LINES, Eol::CrLf);
    let latin = format(label("windows-1252"), Bom::Absent);
    let original = encode(&text, &latin).expect("encodes");
    let path = write_original(dir.path(), "latin.md", &original);

    let (mut file, decoded) = DocumentFile::load(&path).expect("load");
    let mut buffer = Buffer::from_decoded(&decoded);
    let mut journal = journal(&store, &path, &buffer, &file);
    // Line 3 (one-based), after "Ein ".
    let line_start = buffer.line_start(2).expect("line");
    let transaction = Transaction::new().insert(line_start + 4, "😀");
    buffer.apply(&transaction).expect("edit");
    journal
        .record(&transaction, buffer.text())
        .expect("journal");

    let err = file
        .save(buffer.text(), &SaveOptions::default(), &mut journal)
        .expect_err("emoji is not in windows-1252");
    match err {
        SaveError::Unmappable {
            line,
            column,
            ch,
            encoding,
            offset,
        } => {
            assert_eq!((line, column, ch), (3, 5, '😀'));
            assert_eq!(offset, line_start + 4);
            assert_eq!(encoding.name(), "windows-1252");
        }
        other => panic!("expected Unmappable, got {other:?}"),
    }
    assert_eq!(fs::read(&path).expect("read"), original, "untouched");

    // Converting explicitly to UTF-8 saves.
    let utf8 = TextFormat {
        encoding: Encoding::Utf8,
        ..file.format()
    };
    let options = SaveOptions {
        convert: Some(utf8),
        ..SaveOptions::default()
    };
    file.save(buffer.text(), &options, &mut journal)
        .expect("converted save");
    assert_eq!(
        fs::read(&path).expect("read"),
        buffer.to_string().as_bytes()
    );
    assert_eq!(file.format().encoding, Encoding::Utf8);
}

#[test]
fn lossy_files_open_read_only_and_need_an_explicit_encoding() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = JournalStore::open(dir.path().join("journal")).expect("store");
    // Invalid UTF-8 after a UTF-8 BOM.
    let original = b"\xEF\xBB\xBFab\xFFc\n".to_vec();
    let path = write_original(dir.path(), "broken.md", &original);
    let (mut file, decoded) = DocumentFile::load(&path).expect("load");
    assert!(matches!(
        file.read_only_reason(),
        Some(ReadOnlyReason::Lossy(_))
    ));
    let buffer = Buffer::from_decoded(&decoded);
    let mut journal = journal(&store, &path, &buffer, &file);
    let err = file
        .save(buffer.text(), &SaveOptions::default(), &mut journal)
        .expect_err("lossy");
    assert!(
        matches!(err, SaveError::ReadOnly(ReadOnlyReason::Lossy(_))),
        "{err:?}"
    );
    assert_eq!(fs::read(&path).expect("read"), original);

    let options = SaveOptions {
        convert: Some(file.format()),
        ..SaveOptions::default()
    };
    file.save(buffer.text(), &options, &mut journal)
        .expect("explicit");
    assert_eq!(
        fs::read(&path).expect("read"),
        "\u{FEFF}ab\u{FFFD}c\n".as_bytes()
    );
}

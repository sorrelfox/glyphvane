// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The journal's binary format: framed, checksummed records.
//!
//! A journal file starts with [`MAGIC`] and continues with records:
//!
//! ```text
//! u32 LE  payload length
//! u32 LE  CRC-32 of the payload
//! payload: u8 kind, then the kind's body
//! ```
//!
//! Integers are little endian. A string or byte run is a `u32` length and
//! the bytes. A path is a tag (`0` UTF-8, `1` raw Unix bytes, `2` UTF-16
//! code units) and a byte run.
//!
//! | Kind | Record | Body |
//! |---|---|---|
//! | 1 | Header (first record) | version `u16`; document key (`0` + path, or `1` + untitled number `u64`); format; base hash (`u8` flag + 32 bytes); base length `u64` |
//! | 2 | Base | the text the edits apply to (as last loaded or saved) |
//! | 3 | Snapshot | sequence `u64`; the whole current text |
//! | 4 | Edit | sequence `u64`; count `u32`; per replacement: start `u64`, end `u64`, text |
//! | 5 | Format | the on-disk format chosen since the base |
//!
//! A format is the encoding's name (byte run), the BOM (`u8`, 0 or 1) and
//! the line ending for new lines (`u8`: 0 LF, 1 CRLF, 2 CR).
//!
//! Reading stops at the first record that is cut short or fails its
//! checksum; everything before it is used.

use std::path::PathBuf;

use gv_text::{Bom, Encoding, LineEnding, TextFormat};

use crate::files::hash::ContentHash;

/// The first eight bytes of a journal file.
pub(crate) const MAGIC: &[u8; 8] = b"GVJRNL01";
/// The first eight bytes of an in-place backup file.
pub(crate) const BACKUP_MAGIC: &[u8; 8] = b"GVBACKUP";
/// The header version this code writes and reads.
const VERSION: u16 = 1;

const KIND_HEADER: u8 = 1;
const KIND_BASE: u8 = 2;
const KIND_SNAPSHOT: u8 = 3;
const KIND_EDIT: u8 = 4;
const KIND_FORMAT: u8 = 5;

/// Which document a journal belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DocumentKey {
    /// A file, by the path the app identifies it with.
    Path(PathBuf),
    /// An untitled document that was never saved, by a number the app
    /// assigns.
    Untitled(u64),
}

/// The first record of a journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub key: DocumentKey,
    pub format: TextFormat,
    pub base_hash: Option<ContentHash>,
    pub base_len: u64,
}

/// A record after the header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Record {
    Base(String),
    Snapshot {
        seq: u64,
        text: String,
    },
    Edit {
        seq: u64,
        replacements: Vec<(u64, u64, String)>,
    },
    Format(TextFormat),
}

/// How reading a journal ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalTail {
    /// Every byte was a complete, valid record.
    Complete,
    /// The last record was cut short or fails its checksum. Expected after
    /// a `kill -9` or power cut in the middle of a write; nothing that was
    /// fully written is lost.
    Truncated {
        /// Offset of the first byte not used.
        at: u64,
    },
    /// A record passed its checksum but could not be understood, or an
    /// edit did not apply. Recovery stops before it.
    Corrupt {
        /// Offset of the record.
        at: u64,
    },
}

/// Appends one framed record (`kind` + `body`) to `out`.
fn frame(out: &mut Vec<u8>, kind: u8, body: &[u8]) {
    let len = u32::try_from(body.len() + 1).unwrap_or(u32::MAX);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&[kind]);
    hasher.update(body);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&hasher.finalize().to_le_bytes());
    out.push(kind);
    out.extend_from_slice(body);
}

/// The largest text a record can carry (lengths are `u32`).
pub(crate) const MAX_RUN: usize = u32::MAX as usize - 64;

fn put_run(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

fn put_format(out: &mut Vec<u8>, format: &TextFormat) {
    put_run(out, format.encoding.name().as_bytes());
    out.push(u8::from(format.bom == Bom::Present));
    out.push(match format.line_ending {
        LineEnding::Lf => 0,
        LineEnding::CrLf => 1,
        LineEnding::Cr => 2,
    });
}

pub(crate) fn put_path(out: &mut Vec<u8>, path: &std::path::Path) {
    if let Some(s) = path.to_str() {
        out.push(0);
        put_run(out, s.as_bytes());
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        out.push(1);
        put_run(out, path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        out.push(2);
        let units: Vec<u8> = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        put_run(out, &units);
    }
    #[cfg(not(any(unix, windows)))]
    {
        out.push(0);
        put_run(out, path.to_string_lossy().as_bytes());
    }
}

/// The header record, framed.
pub(crate) fn header(out: &mut Vec<u8>, header: &Header) {
    let mut body = Vec::new();
    body.extend_from_slice(&VERSION.to_le_bytes());
    match &header.key {
        DocumentKey::Path(path) => {
            body.push(0);
            put_path(&mut body, path);
        }
        DocumentKey::Untitled(n) => {
            body.push(1);
            body.extend_from_slice(&n.to_le_bytes());
        }
    }
    put_format(&mut body, &header.format);
    match header.base_hash {
        Some(hash) => {
            body.push(1);
            body.extend_from_slice(hash.as_bytes());
        }
        None => body.push(0),
    }
    body.extend_from_slice(&header.base_len.to_le_bytes());
    frame(out, KIND_HEADER, &body);
}

/// A base or snapshot record carrying `text`, framed.
pub(crate) fn base(out: &mut Vec<u8>, text: &str) {
    let mut body = Vec::with_capacity(text.len() + 4);
    put_run(&mut body, text.as_bytes());
    frame(out, KIND_BASE, &body);
}

/// A snapshot record, framed. `chunks` are the text's chunks.
pub(crate) fn snapshot<'a>(
    out: &mut Vec<u8>,
    seq: u64,
    len: usize,
    chunks: impl Iterator<Item = &'a str>,
) {
    let mut body = Vec::with_capacity(len + 12);
    body.extend_from_slice(&seq.to_le_bytes());
    body.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes());
    for chunk in chunks {
        body.extend_from_slice(chunk.as_bytes());
    }
    frame(out, KIND_SNAPSHOT, &body);
}

/// An edit record, framed.
pub(crate) fn edit<'a>(
    out: &mut Vec<u8>,
    seq: u64,
    replacements: impl ExactSizeIterator<Item = (u64, u64, &'a str)>,
) {
    let mut body = Vec::new();
    body.extend_from_slice(&seq.to_le_bytes());
    body.extend_from_slice(
        &u32::try_from(replacements.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for (start, end, text) in replacements {
        body.extend_from_slice(&start.to_le_bytes());
        body.extend_from_slice(&end.to_le_bytes());
        put_run(&mut body, text.as_bytes());
    }
    frame(out, KIND_EDIT, &body);
}

/// A format record, framed.
pub(crate) fn format(out: &mut Vec<u8>, format: &TextFormat) {
    let mut body = Vec::new();
    put_format(&mut body, format);
    frame(out, KIND_FORMAT, &body);
}

/// A bounds-checked little-endian reader.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2)?.try_into().ok().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)?.try_into().ok().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)?.try_into().ok().map(u64::from_le_bytes)
    }

    fn run(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        self.take(len)
    }

    fn string(&mut self) -> Option<String> {
        String::from_utf8(self.run()?.to_vec()).ok()
    }

    fn done(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

fn read_format(r: &mut Reader<'_>) -> Option<TextFormat> {
    let name = std::str::from_utf8(r.run()?).ok()?;
    let encoding = Encoding::for_label(name)?;
    let bom = match r.u8()? {
        0 => Bom::Absent,
        1 => Bom::Present,
        _ => return None,
    };
    let line_ending = match r.u8()? {
        0 => LineEnding::Lf,
        1 => LineEnding::CrLf,
        2 => LineEnding::Cr,
        _ => return None,
    };
    Some(TextFormat {
        encoding,
        bom,
        line_ending,
    })
}

fn read_path(r: &mut Reader<'_>) -> Option<PathBuf> {
    let tag = r.u8()?;
    let bytes = r.run()?;
    match tag {
        0 => Some(PathBuf::from(std::str::from_utf8(bytes).ok()?)),
        1 => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
            }
            #[cfg(not(unix))]
            {
                Some(PathBuf::from(String::from_utf8_lossy(bytes).into_owned()))
            }
        }
        2 => {
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;
                Some(PathBuf::from(std::ffi::OsString::from_wide(&units)))
            }
            #[cfg(not(windows))]
            {
                Some(PathBuf::from(String::from_utf16_lossy(&units)))
            }
        }
        _ => None,
    }
}

pub(crate) fn read_path_from(bytes: &[u8]) -> Option<(PathBuf, usize)> {
    let mut r = Reader::new(bytes);
    let path = read_path(&mut r)?;
    Some((path, r.pos))
}

fn parse_header(body: &[u8]) -> Option<Header> {
    let mut r = Reader::new(body);
    if r.u16()? != VERSION {
        return None;
    }
    let key = match r.u8()? {
        0 => DocumentKey::Path(read_path(&mut r)?),
        1 => DocumentKey::Untitled(r.u64()?),
        _ => return None,
    };
    let format = read_format(&mut r)?;
    let base_hash = match r.u8()? {
        0 => None,
        1 => Some(ContentHash::from_bytes(r.take(32)?.try_into().ok()?)),
        _ => return None,
    };
    let base_len = r.u64()?;
    r.done().then_some(Header {
        key,
        format,
        base_hash,
        base_len,
    })
}

fn parse_record(kind: u8, body: &[u8]) -> Option<Record> {
    let mut r = Reader::new(body);
    let record = match kind {
        KIND_BASE => Record::Base(r.string()?),
        KIND_SNAPSHOT => {
            let seq = r.u64()?;
            Record::Snapshot {
                seq,
                text: r.string()?,
            }
        }
        KIND_EDIT => {
            let seq = r.u64()?;
            let count = r.u32()?;
            let mut replacements = Vec::new();
            for _ in 0..count {
                let start = r.u64()?;
                let end = r.u64()?;
                replacements.push((start, end, r.string()?));
            }
            Record::Edit { seq, replacements }
        }
        KIND_FORMAT => Record::Format(read_format(&mut r)?),
        _ => return None,
    };
    r.done().then_some(record)
}

/// A journal read back: the header (if it was readable), the records after
/// it with their offsets, and how reading ended.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub header: Option<Header>,
    pub records: Vec<(u64, Record)>,
    pub tail: JournalTail,
}

/// Reads a journal file's bytes. `None` if they do not start with
/// [`MAGIC`].
pub(crate) fn parse(bytes: &[u8]) -> Option<Parsed> {
    let rest = bytes.strip_prefix(MAGIC.as_slice())?;
    let mut parsed = Parsed {
        header: None,
        records: Vec::new(),
        tail: JournalTail::Complete,
    };
    let mut pos = 0usize;
    let base = MAGIC.len() as u64;
    loop {
        let at = base + pos as u64;
        if pos == rest.len() {
            break;
        }
        let mut r = Reader::new(&rest[pos..]);
        let (Some(len), Some(crc)) = (r.u32(), r.u32()) else {
            parsed.tail = JournalTail::Truncated { at };
            break;
        };
        let Some(payload) = usize::try_from(len).ok().and_then(|len| r.take(len)) else {
            parsed.tail = JournalTail::Truncated { at };
            break;
        };
        if payload.is_empty() || crc32fast::hash(payload) != crc {
            parsed.tail = JournalTail::Truncated { at };
            break;
        }
        pos += r.pos;
        let (kind, body) = (payload[0], &payload[1..]);
        if parsed.header.is_none() {
            match (kind == KIND_HEADER).then(|| parse_header(body)).flatten() {
                Some(header) => parsed.header = Some(header),
                None => {
                    parsed.tail = JournalTail::Corrupt { at };
                    break;
                }
            }
            continue;
        }
        match parse_record(kind, body) {
            Some(record) => parsed.records.push((at, record)),
            None => {
                parsed.tail = JournalTail::Corrupt { at };
                break;
            }
        }
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> Header {
        Header {
            key: DocumentKey::Path(PathBuf::from("/notes/Ärger.md")),
            format: TextFormat {
                encoding: Encoding::for_label("shift_jis").expect("label"),
                bom: Bom::Absent,
                line_ending: LineEnding::CrLf,
            },
            base_hash: Some(ContentHash::of(b"base")),
            base_len: 4,
        }
    }

    #[test]
    fn records_round_trip() {
        let mut out = MAGIC.to_vec();
        header(&mut out, &sample_header());
        base(&mut out, "base");
        edit(&mut out, 1, [(0u64, 0u64, "x"), (2, 4, "")].into_iter());
        format(&mut out, &TextFormat::default());
        snapshot(&mut out, 2, 3, ["x", "ba"].into_iter());
        let parsed = parse(&out).expect("journal");
        assert_eq!(parsed.header, Some(sample_header()));
        assert_eq!(parsed.tail, JournalTail::Complete);
        let records: Vec<Record> = parsed.records.into_iter().map(|(_, r)| r).collect();
        assert_eq!(
            records,
            vec![
                Record::Base("base".to_owned()),
                Record::Edit {
                    seq: 1,
                    replacements: vec![(0, 0, "x".to_owned()), (2, 4, String::new())]
                },
                Record::Format(TextFormat::default()),
                Record::Snapshot {
                    seq: 2,
                    text: "xba".to_owned()
                },
            ]
        );
    }

    #[test]
    fn every_truncation_keeps_the_complete_prefix() {
        let mut out = MAGIC.to_vec();
        header(&mut out, &sample_header());
        let after_header = out.len();
        edit(&mut out, 1, [(0u64, 0u64, "hello")].into_iter());
        let after_first = out.len();
        edit(&mut out, 2, [(5u64, 5u64, " world")].into_iter());
        for cut in after_header..out.len() {
            let parsed = parse(&out[..cut]).expect("journal");
            let expected = usize::from(cut >= after_first);
            assert_eq!(parsed.records.len(), expected, "cut at {cut}");
            if cut == after_header || cut == after_first {
                assert_eq!(parsed.tail, JournalTail::Complete);
            } else {
                assert!(matches!(parsed.tail, JournalTail::Truncated { .. }));
            }
        }
    }

    #[test]
    fn a_flipped_bit_stops_reading() {
        let mut out = MAGIC.to_vec();
        header(&mut out, &sample_header());
        let at = out.len();
        edit(&mut out, 1, [(0u64, 0u64, "hello")].into_iter());
        let last = out.len() - 1;
        out[last] ^= 0x20;
        let parsed = parse(&out).expect("journal");
        assert!(parsed.records.is_empty());
        assert_eq!(parsed.tail, JournalTail::Truncated { at: at as u64 });
        assert!(parse(b"not a journal").is_none());
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The on-disk cache of the content index: one versioned binary file per
//! vault.
//!
//! # Format
//!
//! All integers are little-endian; strings are a `u32` byte length and
//! UTF-8 bytes; `Option`s are a `u8` tag (0 or 1) and the value.
//!
//! | Part | Contents |
//! |---|---|
//! | Header | magic `GVCONTNT`, format version (`u32`), extract version (`u32`), profile id (string), root (string), written-at time (`i64`, ns since the epoch) |
//! | Records | count (`u32`), then per note: path, size (`u64`), mtime (`Option<i64>`), CRC-32 (`u32`), flags (`u8`: bit 0 doubt), kind (`u8`), and for indexed notes its [`NoteData`] (its `html_links` follow its links in one list, told apart by their kind) |
//! | Link graph | `u8` present; then the fingerprint of the resolver parameters and walk settings (`u64`), the path index's entries (count, then path and kind: 0 file, 1 folder), and the resolved links per note (count, then the source's entry index, its distinct targets as entry indices, per link status, candidate count and target, and its lookup keys) |
//! | Trailer | CRC-32 of everything before it (`u32`) |
//!
//! Only disk records are written; overlays never are, nor the resolutions
//! of notes that have one.
//!
//! The link graph makes a warm start cheap: the stored entries become the
//! "previous" path index, the stored resolutions are taken over, and only
//! the notes that changed or whose lookup keys name an entry that came or
//! went since are resolved again, as in any update. It is left out when
//! the path index has symbolic links, and ignored when the fingerprint
//! differs (other settings).
//!
//! # Versions
//!
//! - [`FORMAT_VERSION`] changes with the layout. A file of an older
//!   version is upgraded by a migration when one exists in [`migrate`];
//!   any other version (older without a migration, or newer) is not read,
//!   and the index is rebuilt from the notes.
//! - [`EXTRACT_VERSION`] changes when extraction changes (parser
//!   upgrades, new fields filled in): the records would differ, so the file
//!   is not used.
//! - A file for another profile or root is not used either.
//!
//! The file is written to a temporary file next to it and renamed over
//! it, so a crash leaves the old file or the new one. A damaged file (bad
//! magic, truncated, wrong checksum) is rebuilt.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::record::{
    FileStamp, IndexedBlockId, IndexedHeading, IndexedLink, IndexedTag, IndexedTask, NoteData,
    NoteRecord, Property, PropertyValue, RecordKind, Span, TitleSource,
};
use super::state::{Packed, Resolved, State};
use crate::index::{EntryKind, IndexState, PathIndex};
use crate::link::LinkKind;
use crate::path::VaultPath;
use crate::profile::ProfileId;

/// The file's magic bytes.
const MAGIC: &[u8; 8] = b"GVCONTNT";

/// The layout version.
pub const FORMAT_VERSION: u32 = 1;

/// The extraction version: bump whenever extracted records would change.
pub const EXTRACT_VERSION: u32 = 3;

/// The cache file's extension.
pub const CACHE_EXTENSION: &str = "gvci";

/// Why a cache file was not used.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// There is none.
    #[error("no cache file")]
    Missing,
    /// It cannot be read.
    #[error("cannot read the cache: {0}")]
    Io(#[from] io::Error),
    /// Another layout version without a migration.
    #[error("cache format version {found}, expected {FORMAT_VERSION}")]
    Version {
        /// The file's version.
        found: u32,
    },
    /// Written by other extraction code, for another profile or root.
    #[error("cache is stale: {0}")]
    Stale(&'static str),
    /// Damaged.
    #[error("cache is damaged: {0}")]
    Corrupt(&'static str),
}

/// What the header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Header {
    pub profile: ProfileId,
    pub root: String,
    pub written_at: i64,
    /// Of the resolver parameters and walk settings
    /// ([`graph_fingerprint`]).
    pub fingerprint: u64,
}

/// The stored link graph.
#[derive(Debug)]
pub(crate) struct Graph {
    /// The path index's entries (no root), when the cache was written.
    pub entries: Vec<(VaultPath, EntryKind)>,
    /// Resolutions of unchanged disk notes; target paths share the
    /// entries' strings.
    pub resolved: Vec<(VaultPath, Resolved)>,
}

/// What a cache file holds.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub header: Header,
    pub records: Vec<NoteRecord>,
    /// `None` when absent or written with other settings.
    pub graph: Option<Graph>,
}

/// A fingerprint of everything resolutions depend on besides the paths:
/// the resolver parameters and the walk settings.
#[must_use]
pub(crate) fn graph_fingerprint(
    params: &crate::profile::ProfileParams,
    config: &crate::index::IndexConfig,
) -> u64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("{params:?}\n{config:?}").as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(b)
}

/// The cache file of the vault at `root` in the cache folder `cache_dir`:
/// `<cache_dir>/index/<vault id>.gvci`, where the vault id is derived from
/// the absolute root path.
#[must_use]
pub fn cache_file(cache_dir: &Path, root: &Path) -> PathBuf {
    cache_dir
        .join("index")
        .join(format!("{}.{CACHE_EXTENSION}", vault_cache_id(root)))
}

/// A stable id for the vault at `root`: 16 hex digits of the SHA-256 of
/// its absolute path.
#[must_use]
pub fn vault_cache_id(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let digest = Sha256::digest(absolute.as_os_str().as_encoded_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes the disk records and link graph of `state` to `file`
/// atomically. Returns the bytes written.
pub(crate) fn save(file: &Path, header: &Header, state: &State) -> io::Result<u64> {
    let records = state.disk.iter().map(|(_, r)| &**r);
    let mut e = Enc::default();
    e.buf.extend_from_slice(MAGIC);
    e.u32(FORMAT_VERSION);
    e.u32(EXTRACT_VERSION);
    e.str(header.profile.as_str());
    e.str(&header.root);
    e.i64(header.written_at);
    let count_at = e.buf.len();
    e.u32(0);
    let mut count = 0u32;
    for r in records {
        record(&mut e, r);
        count += 1;
    }
    e.buf[count_at..count_at + 4].copy_from_slice(&count.to_le_bytes());
    graph(&mut e, header.fingerprint, state);
    let crc = crc32fast::hash(&e.buf);
    e.u32(crc);

    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}-{}.tmp",
        file.file_name().and_then(|n| n.to_str()).unwrap_or("index"),
        std::process::id(),
        super::state::system_time_ns(std::time::SystemTime::now())
    ));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&e.buf)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| e.buf.len() as u64)
}

/// Reads `file` written for `profile` at `root`; its link graph only if
/// it was written with `fingerprint`.
pub(crate) fn load(
    file: &Path,
    profile: ProfileId,
    root: &str,
    fingerprint: u64,
) -> Result<Loaded, LoadError> {
    let bytes = match fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(LoadError::Missing),
        Err(e) => return Err(LoadError::Io(e)),
    };
    decode(&bytes, profile, root, fingerprint)
}

pub(crate) fn decode(
    bytes: &[u8],
    profile: ProfileId,
    root: &str,
    fingerprint: u64,
) -> Result<Loaded, LoadError> {
    if bytes.len() < MAGIC.len() + 8 || &bytes[..MAGIC.len()] != MAGIC {
        return Err(LoadError::Corrupt("not a content index file"));
    }
    let mut d = Dec {
        b: bytes,
        at: MAGIC.len(),
    };
    let version = d.u32()?;
    let bytes = if version == FORMAT_VERSION {
        std::borrow::Cow::Borrowed(bytes)
    } else {
        std::borrow::Cow::Owned(migrate(version, bytes)?)
    };
    let (body, trailer) = bytes.split_at(bytes.len() - 4);
    let crc = u32::from_le_bytes(
        trailer
            .try_into()
            .map_err(|_| LoadError::Corrupt("trailer"))?,
    );
    if crc32fast::hash(body) != crc {
        return Err(LoadError::Corrupt("checksum mismatch"));
    }
    let mut d = Dec {
        b: body,
        at: MAGIC.len() + 4,
    };
    if d.u32()? != EXTRACT_VERSION {
        return Err(LoadError::Stale("extraction changed"));
    }
    let file_profile = d.str()?;
    if file_profile != profile.as_str() {
        return Err(LoadError::Stale("another profile"));
    }
    let file_root = d.str()?.to_owned();
    if file_root != root {
        return Err(LoadError::Stale("another root"));
    }
    let written_at = d.i64()?;
    let count = d.u32()? as usize;
    let mut records = Vec::with_capacity(count.min(1 << 20));
    for _ in 0..count {
        records.push(read_record(&mut d)?);
    }
    let (file_fingerprint, graph) = read_graph(&mut d)?;
    if d.at != body.len() {
        return Err(LoadError::Corrupt("trailing bytes"));
    }
    Ok(Loaded {
        header: Header {
            profile,
            root: file_root,
            written_at,
            fingerprint: file_fingerprint,
        },
        records,
        graph: graph.filter(|_| file_fingerprint == fingerprint),
    })
}

fn graph(e: &mut Enc, fingerprint: u64, state: &State) {
    let usable = state
        .paths
        .as_ref()
        .filter(|p| p.state() == IndexState::Complete && !p.has_symlinks());
    let Some(paths) = usable else {
        e.u8(0);
        return;
    };
    e.u8(1);
    e.u64(fingerprint);
    let entries: Vec<_> = paths.iter().filter(|x| !x.path.is_root()).collect();
    let mut index: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    e.len(entries.len());
    for (i, x) in entries.iter().enumerate() {
        e.str(x.path.as_str());
        e.u8(u8::from(x.kind.is_dir()));
        index.insert(x.path.as_str(), u32::try_from(i).unwrap_or(u32::MAX));
    }
    let count_at = e.buf.len();
    e.u32(0);
    let mut count = 0u32;
    for (source, resolved) in state.resolved.iter() {
        // Only disk notes: an overlay's links are not the file's.
        if state.disk.get(source).is_none() || state.overlay.get(source).is_some() {
            continue;
        }
        let Some(&s) = index.get(source.as_str()) else {
            continue;
        };
        let Some(targets) = resolved
            .paths
            .iter()
            .map(|p| index.get(p.as_str()).copied())
            .collect::<Option<Vec<u32>>>()
        else {
            continue;
        };
        e.u32(s);
        e.len(targets.len());
        for t in targets {
            e.u32(t);
        }
        e.len(resolved.targets.len());
        for p in &resolved.targets {
            e.u8(p.status);
            e.u32(p.count);
            e.u32(p.target);
        }
        e.strs(&resolved.keys);
        count += 1;
    }
    e.buf[count_at..count_at + 4].copy_from_slice(&count.to_le_bytes());
}

fn read_graph(d: &mut Dec<'_>) -> Result<(u64, Option<Graph>), LoadError> {
    if !d.flag()? {
        return Ok((0, None));
    }
    let fingerprint = d.u64()?;
    let n = d.len()?;
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        let path = VaultPath::new(d.str()?).map_err(|_| LoadError::Corrupt("bad path"))?;
        let kind = if d.flag()? {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        entries.push((path, kind));
    }
    let entry = |i: u32| {
        entries
            .get(i as usize)
            .map(|(p, _)| p.clone())
            .ok_or(LoadError::Corrupt("bad entry index"))
    };
    let n = d.u32()? as usize;
    let mut resolved = Vec::with_capacity(n.min(1 << 20));
    for _ in 0..n {
        let source = entry(d.u32()?)?;
        let m = d.len()?;
        let paths = (0..m)
            .map(|_| entry(d.u32()?))
            .collect::<Result<Box<[VaultPath]>, _>>()?;
        let m = d.len()?;
        let mut targets = Vec::with_capacity(m);
        for _ in 0..m {
            let p = Packed {
                status: d.u8()?,
                count: d.u32()?,
                target: d.u32()?,
            };
            if p.status > 8 || (p.target != Packed::NONE && p.target as usize >= paths.len()) {
                return Err(LoadError::Corrupt("bad resolution"));
            }
            targets.push(p);
        }
        let keys = d.strs()?.into_boxed_slice();
        resolved.push((
            source,
            Resolved {
                targets: targets.into_boxed_slice(),
                paths,
                keys,
            },
        ));
    }
    Ok((fingerprint, Some(Graph { entries, resolved })))
}

/// The path index the graph was resolved against, rebuilt from its
/// entries with `config`.
pub(crate) fn previous_index(
    root: &Path,
    config: &crate::index::IndexConfig,
    entries: &[(VaultPath, EntryKind)],
) -> Option<PathIndex> {
    PathIndex::from_entries(
        root,
        config.clone(),
        IndexState::Complete,
        entries
            .iter()
            .map(|(p, k)| (p.clone(), k.clone(), crate::index::FileMeta::default())),
    )
    .ok()
}

/// Upgrades a file of an older layout `version` to [`FORMAT_VERSION`]. No
/// migrations exist yet: every other version is rebuilt.
fn migrate(version: u32, _bytes: &[u8]) -> Result<Vec<u8>, LoadError> {
    Err(LoadError::Version { found: version })
}

// --- Encoding ---------------------------------------------------------

#[derive(Default)]
struct Enc {
    buf: Vec<u8>,
}

impl Enc {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.buf.extend_from_slice(s.as_bytes());
    }
    fn span(&mut self, s: Span) {
        self.u32(s.start);
        self.u32(s.end);
    }
    fn strs(&mut self, v: &[Arc<str>]) {
        self.len(v.len());
        for s in v {
            self.str(s);
        }
    }
}

fn kind_code(kind: RecordKind) -> u8 {
    match kind {
        RecordKind::Indexed => 0,
        RecordKind::Placeholder => 1,
        RecordKind::TooLarge => 2,
        RecordKind::Unreadable => 3,
        RecordKind::Overlay => 4,
    }
}

fn link_code(kind: LinkKind) -> u8 {
    match kind {
        LinkKind::Inline => 0,
        LinkKind::Reference => 1,
        LinkKind::Image => 2,
        LinkKind::Autolink => 3,
        LinkKind::Wikilink => 4,
        LinkKind::Embed => 5,
        LinkKind::HtmlHref => 6,
        LinkKind::HtmlSrc => 7,
        LinkKind::HtmlCard => 8,
    }
}

fn record(e: &mut Enc, r: &NoteRecord) {
    e.str(r.path.as_str());
    e.u64(r.stamp.size);
    match r.stamp.mtime_ns {
        Some(t) => {
            e.u8(1);
            e.i64(t);
        }
        None => e.u8(0),
    }
    e.u32(r.stamp.crc);
    e.u8(u8::from(r.stamp.doubt));
    e.u8(kind_code(r.kind));
    if r.kind == RecordKind::Indexed {
        data(e, &r.data);
    }
}

fn data(e: &mut Enc, d: &NoteData) {
    match &d.title {
        Some(t) => {
            e.u8(1);
            e.str(t);
        }
        None => e.u8(0),
    }
    e.u8(match d.title_source {
        TitleSource::FrontMatter => 0,
        TitleSource::Heading => 1,
        TitleSource::FileName => 2,
    });
    e.len(d.headings.len());
    for h in &d.headings {
        e.u8(h.level);
        e.str(&h.text);
        if h.source == h.text {
            e.u8(0);
        } else {
            e.u8(1);
            e.str(&h.source);
        }
        e.span(h.span);
        e.u32(h.line);
    }
    // The `<a href>`s follow the links in the same list (kind code 6),
    // and are told apart again when read.
    e.len(d.links.len() + d.html_links.len());
    for l in d.links.iter().chain(&d.html_links) {
        e.u8(link_code(l.kind) | if l.front_matter { 0x80 } else { 0 });
        e.str(&l.raw);
        e.span(l.span);
        e.u32(l.line);
    }
    e.len(d.tags.len());
    for t in &d.tags {
        e.str(&t.name);
        match t.span {
            Some(s) => {
                e.u8(1);
                e.span(s);
            }
            None => e.u8(0),
        }
        e.u32(t.line);
    }
    e.strs(&d.aliases);
    e.strs(&d.cssclasses);
    e.len(d.properties.len());
    for p in &d.properties {
        e.str(&p.key);
        match &p.value {
            PropertyValue::Null => e.u8(0),
            PropertyValue::Bool(b) => {
                e.u8(1);
                e.u8(u8::from(*b));
            }
            PropertyValue::Number(n) => {
                e.u8(2);
                e.str(n);
            }
            PropertyValue::Text(t) => {
                e.u8(3);
                e.str(t);
            }
            PropertyValue::List(v) => {
                e.u8(4);
                e.strs(v);
            }
            PropertyValue::Other(t) => {
                e.u8(5);
                e.str(t);
            }
        }
    }
    e.len(d.block_ids.len());
    for b in &d.block_ids {
        e.str(&b.id);
        e.span(b.span);
        e.u32(b.line);
    }
    e.len(d.tasks.len());
    for t in &d.tasks {
        e.u32(u32::from(t.status));
        e.str(&t.text);
        e.span(t.span);
        e.u32(t.line);
    }
    e.strs(&d.footnotes);
    match d.front_matter {
        Some(s) => {
            e.u8(1);
            e.span(s);
        }
        None => e.u8(0),
    }
    e.len(d.skip.len());
    for s in &d.skip {
        e.span(*s);
    }
}

// --- Decoding ---------------------------------------------------------

struct Dec<'a> {
    b: &'a [u8],
    at: usize,
}

const TRUNCATED: LoadError = LoadError::Corrupt("truncated");

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        let end = self.at.checked_add(n).ok_or(TRUNCATED)?;
        let s = self.b.get(self.at..end).ok_or(TRUNCATED)?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, LoadError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, LoadError> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }
    fn i64(&mut self) -> Result<i64, LoadError> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(i64::from_le_bytes(a))
    }
    fn len(&mut self) -> Result<usize, LoadError> {
        let n = self.u32()? as usize;
        // Every element takes at least one byte.
        if n > self.b.len() - self.at {
            return Err(TRUNCATED);
        }
        Ok(n)
    }
    fn str(&mut self) -> Result<&'a str, LoadError> {
        let n = self.u32()? as usize;
        std::str::from_utf8(self.take(n)?).map_err(|_| LoadError::Corrupt("invalid UTF-8"))
    }
    fn arc(&mut self) -> Result<Arc<str>, LoadError> {
        Ok(self.str()?.into())
    }
    fn span(&mut self) -> Result<Span, LoadError> {
        Ok(Span {
            start: self.u32()?,
            end: self.u32()?,
        })
    }
    fn strs(&mut self) -> Result<Vec<Arc<str>>, LoadError> {
        let n = self.len()?;
        (0..n).map(|_| self.arc()).collect()
    }
    fn flag(&mut self) -> Result<bool, LoadError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(LoadError::Corrupt("bad flag")),
        }
    }
}

fn read_record(d: &mut Dec<'_>) -> Result<NoteRecord, LoadError> {
    let path = VaultPath::new(d.str()?).map_err(|_| LoadError::Corrupt("bad path"))?;
    let size = d.u64()?;
    let mtime_ns = if d.flag()? { Some(d.i64()?) } else { None };
    let crc = d.u32()?;
    let doubt = d.u8()? & 1 == 1;
    let kind = match d.u8()? {
        0 => RecordKind::Indexed,
        1 => RecordKind::Placeholder,
        2 => RecordKind::TooLarge,
        3 => RecordKind::Unreadable,
        _ => return Err(LoadError::Corrupt("bad record kind")),
    };
    let data = if kind == RecordKind::Indexed {
        read_data(d)?
    } else {
        NoteData::default()
    };
    Ok(NoteRecord {
        path,
        stamp: FileStamp {
            size,
            mtime_ns,
            crc,
            doubt,
        },
        kind,
        data,
    })
}

fn read_data(d: &mut Dec<'_>) -> Result<NoteData, LoadError> {
    let title = if d.flag()? { Some(d.arc()?) } else { None };
    let title_source = match d.u8()? {
        0 => TitleSource::FrontMatter,
        1 => TitleSource::Heading,
        2 => TitleSource::FileName,
        _ => return Err(LoadError::Corrupt("bad title source")),
    };
    let n = d.len()?;
    let mut headings = Vec::with_capacity(n);
    for _ in 0..n {
        let level = d.u8()?;
        let text = d.arc()?;
        let source = if d.flag()? { d.arc()? } else { text.clone() };
        headings.push(IndexedHeading {
            level,
            text,
            source,
            span: d.span()?,
            line: d.u32()?,
        });
    }
    let n = d.len()?;
    let mut links = Vec::with_capacity(n);
    let mut html_links = Vec::new();
    for _ in 0..n {
        let code = d.u8()?;
        let kind = match code & 0x7f {
            0 => LinkKind::Inline,
            1 => LinkKind::Reference,
            2 => LinkKind::Image,
            3 => LinkKind::Autolink,
            4 => LinkKind::Wikilink,
            5 => LinkKind::Embed,
            6 => LinkKind::HtmlHref,
            7 => LinkKind::HtmlSrc,
            8 => LinkKind::HtmlCard,
            _ => return Err(LoadError::Corrupt("bad link kind")),
        };
        let link = IndexedLink {
            kind,
            raw: d.arc()?,
            span: d.span()?,
            line: d.u32()?,
            front_matter: code & 0x80 != 0,
        };
        if kind == LinkKind::HtmlHref {
            html_links.push(link);
        } else {
            links.push(link);
        }
    }
    let n = d.len()?;
    let mut tags = Vec::with_capacity(n);
    for _ in 0..n {
        let name = d.arc()?;
        let span = if d.flag()? { Some(d.span()?) } else { None };
        tags.push(IndexedTag {
            name,
            span,
            line: d.u32()?,
        });
    }
    let aliases = d.strs()?;
    let cssclasses = d.strs()?;
    let n = d.len()?;
    let mut properties = Vec::with_capacity(n);
    for _ in 0..n {
        let key = d.arc()?;
        let value = match d.u8()? {
            0 => PropertyValue::Null,
            1 => PropertyValue::Bool(d.flag()?),
            2 => PropertyValue::Number(d.arc()?),
            3 => PropertyValue::Text(d.arc()?),
            4 => PropertyValue::List(d.strs()?),
            5 => PropertyValue::Other(d.arc()?),
            _ => return Err(LoadError::Corrupt("bad property")),
        };
        properties.push(Property { key, value });
    }
    let n = d.len()?;
    let mut block_ids = Vec::with_capacity(n);
    for _ in 0..n {
        block_ids.push(IndexedBlockId {
            id: d.arc()?,
            span: d.span()?,
            line: d.u32()?,
        });
    }
    let n = d.len()?;
    let mut tasks = Vec::with_capacity(n);
    for _ in 0..n {
        let status = char::from_u32(d.u32()?).ok_or(LoadError::Corrupt("bad task status"))?;
        tasks.push(IndexedTask {
            status,
            text: d.arc()?,
            span: d.span()?,
            line: d.u32()?,
        });
    }
    let footnotes = d.strs()?;
    let front_matter = if d.flag()? { Some(d.span()?) } else { None };
    let n = d.len()?;
    let mut skip = Vec::with_capacity(n);
    for _ in 0..n {
        skip.push(d.span()?);
    }
    Ok(NoteData {
        title,
        title_source,
        headings,
        links,
        html_links,
        tags,
        aliases,
        cssclasses,
        properties,
        block_ids,
        tasks,
        footnotes,
        front_matter,
        skip,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<NoteRecord> {
        let text = "---\ntitle: T\ntags: [a]\naliases: [Al]\nn: 3\nup: \"[[P]]\"\n---\n# H\n\n- [x] done ^b1\n\n[[A#x|y]] ![[i.png]] [m](b.md) #t `code` <a href=\"c.md#h\">c</a>\n\n[^f]: note\n";
        let data = super::super::extract::extract(text, ProfileId::Obsidian);
        vec![
            NoteRecord {
                path: VaultPath::new("a/n.md").expect("path"),
                stamp: FileStamp {
                    size: text.len() as u64,
                    mtime_ns: Some(-5),
                    crc: 7,
                    doubt: true,
                },
                kind: RecordKind::Indexed,
                data,
            },
            NoteRecord {
                path: VaultPath::new("cloud.md").expect("path"),
                stamp: FileStamp::default(),
                kind: RecordKind::Placeholder,
                data: NoteData::default(),
            },
        ]
    }

    fn header() -> Header {
        Header {
            profile: ProfileId::Obsidian,
            root: "/v".into(),
            written_at: 42,
            fingerprint: 9,
        }
    }

    fn state(records: Vec<NoteRecord>) -> State {
        let options = super::super::ContentOptions::for_profile(ProfileId::Obsidian);
        State::with_records(Arc::new(options), records)
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("x").join("v.gvci");
        let records = sample();
        assert!(!records[0].data.links.is_empty() && !records[0].data.tasks.is_empty());
        assert_eq!(records[0].data.html_links.len(), 1);
        save(&file, &header(), &state(records.clone())).expect("save");
        let loaded = load(&file, ProfileId::Obsidian, "/v", 9).expect("load");
        // No path index yet: no graph.
        assert!(loaded.graph.is_none());
        assert_eq!(loaded.header.written_at, 42);
        let mut back = loaded.records;
        back.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(back, records);
    }

    #[test]
    fn mismatches_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("v.gvci");
        save(&file, &header(), &state(sample())).expect("save");
        assert!(matches!(
            load(&file, ProfileId::GfmFile, "/v", 9),
            Err(LoadError::Stale(_))
        ));
        assert!(matches!(
            load(&file, ProfileId::Obsidian, "/w", 9),
            Err(LoadError::Stale(_))
        ));
        let mut bytes = fs::read(&file).expect("read");
        // Another layout version.
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert!(matches!(
            decode(&bytes, ProfileId::Obsidian, "/v", 9),
            Err(LoadError::Version { found: 99 })
        ));
        bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        assert!(matches!(
            decode(&bytes, ProfileId::Obsidian, "/v", 9),
            Err(LoadError::Corrupt(_))
        ));
        assert!(matches!(
            decode(&bytes[..20], ProfileId::Obsidian, "/v", 9),
            Err(LoadError::Corrupt(_))
        ));
        assert!(matches!(
            load(&dir.path().join("none"), ProfileId::Obsidian, "/v", 9),
            Err(LoadError::Missing)
        ));
    }

    #[test]
    fn cache_ids_are_stable_and_distinct() {
        let a = vault_cache_id(Path::new("/vaults/a"));
        assert_eq!(a, vault_cache_id(Path::new("/vaults/a")));
        assert_ne!(a, vault_cache_id(Path::new("/vaults/b")));
        assert_eq!(a.len(), 16);
        assert!(
            cache_file(Path::new("/c"), Path::new("/vaults/a"))
                .ends_with(format!("index/{a}.gvci"))
        );
    }
}

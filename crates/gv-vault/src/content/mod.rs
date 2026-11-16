// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The content index: what every note in a vault contains, kept in step
//! with the disk and with unsaved buffers.
//!
//! # Parts
//!
//! | Part | What |
//! |---|---|
//! | [`NoteRecord`] / [`NoteData`] | Per note: title, headings, links and embeds, tags, aliases, `cssclasses` and other front matter properties, block ids, tasks, footnotes, the front matter span, and the file stamp it was validated by |
//! | [`extract()`] | Text to [`NoteData`], with the parser and document model the views use |
//! | [`ContentSnapshot`] | An immutable, `Send + Sync` snapshot with the resolved link graph and the queries: backlinks, outgoing links, unresolved links, tags, quick-switcher entries, sync-conflict copies, placeholders, unlinked mentions |
//! | [`ContentIndexer`] | The background service: builds the index (from the cache when it is valid), follows path-index snapshots from the watcher and dirty-buffer overlays, persists the cache, and stops when the vault closes |
//! | [`store`] | The versioned on-disk cache |
//! | [`check`] | The link checker: findings by severity for a note, a folder or the vault, with fixes |
//!
//! # Sources of truth
//!
//! - **The path index** decides which notes exist. Each path-index
//!   snapshot the watcher publishes (OS events, polling on network file
//!   systems, rescans after an overflow) goes to
//!   [`ContentIndexer::update_paths`]; the index compares it with its
//!   records by size and modification time and reads only what changed.
//! - **Unsaved buffers override the disk** ([`ContentIndexer::set_overlay`]):
//!   their headings and links count until the buffer is saved or closed
//!   ([`ContentIndexer::clear_overlay`]).
//! - **Links resolve through the resolver** ([`crate::resolve()`]) against
//!   the same path-index snapshot, with the vault's profile parameters.
//!
//! # Cloud sync
//!
//! - **Online-only placeholders** (macOS `SF_DATALESS`, Windows
//!   `RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN` and `OFFLINE`) are never
//!   read, which would download them: they are indexed by path only
//!   ([`RecordKind::Placeholder`]), and read once they are local.
//! - **Sync-conflict copies** are indexed like any note but are left out of
//!   backlinks and unresolved links, and listed by
//!   [`ContentSnapshot::sync_conflicts`]. The resolver already leaves them
//!   out of basename lookups.

pub mod check;
mod extract;
mod frontmatter;
mod query;
mod record;
mod service;
mod state;
pub mod store;
#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use check::{
    CheckOptions, CheckProgress, CheckReport, CheckScope, Finding, FindingKind, Fix, FixAction,
    Severity,
};
pub use extract::extract;
pub use query::{
    Backlink, ContentSnapshot, Mention, OutgoingLink, SwitcherEntry, SyncConflictCopy, TagCount,
    UnresolvedLink,
};
pub use record::{
    FileStamp, IndexedBlockId, IndexedHeading, IndexedLink, IndexedTag, IndexedTask, NoteData,
    NoteRecord, Property, PropertyValue, RecordKind, Span, TitleSource, file_stem,
};
pub use service::{CacheUse, ColdStats, ContentEvent, ContentHandle, ContentIndexer};
pub use state::{Cancelled, LinkTarget, UpdateReport};
pub use store::LoadError;

use crate::path::VaultPath;
use crate::profile::{ProfileId, ProfileParams};

/// Decides whether a file is an online-only placeholder, from its path and
/// metadata (`lstat`).
pub type PlaceholderCheck = Arc<dyn Fn(&Path, &fs::Metadata) -> bool + Send + Sync>;

/// Content index settings.
#[derive(Clone)]
pub struct ContentOptions {
    /// The vault's profile: parse options and switches.
    pub profile: ProfileId,
    /// Link resolution parameters.
    pub params: ProfileParams,
    /// File extensions of notes, without the dot (matched ignoring ASCII
    /// case).
    pub note_extensions: Vec<String>,
    /// Larger notes are indexed by path only.
    pub max_file_bytes: u64,
    /// Worker threads for reading, parsing and resolving.
    pub threads: usize,
    /// The cache file ([`store::cache_file`]); `None`: no persistence.
    pub cache_file: Option<PathBuf>,
    /// How long overlay changes wait for more before they are indexed.
    pub overlay_debounce: Duration,
    /// How often a changed index is written to the cache, at most.
    pub persist_interval: Duration,
    /// An extra placeholder test (tests; future sync clients). The OS
    /// test ([`is_cloud_placeholder`]) always runs.
    pub placeholder: Option<PlaceholderCheck>,
}

impl std::fmt::Debug for ContentOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentOptions")
            .field("profile", &self.profile)
            .field("note_extensions", &self.note_extensions)
            .field("max_file_bytes", &self.max_file_bytes)
            .field("threads", &self.threads)
            .field("cache_file", &self.cache_file)
            .finish_non_exhaustive()
    }
}

impl ContentOptions {
    /// Defaults for `profile`: `.md` and `.markdown` notes up to 16 MiB,
    /// half the CPUs (at most 8), no cache.
    #[must_use]
    pub fn for_profile(profile: ProfileId) -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, std::num::NonZero::get);
        Self {
            profile,
            params: ProfileParams::for_profile(profile),
            note_extensions: vec!["md".into(), "markdown".into()],
            max_file_bytes: 16 << 20,
            threads: (cpus / 2).clamp(1, 8),
            cache_file: None,
            overlay_debounce: Duration::from_millis(150),
            persist_interval: Duration::from_secs(30),
            placeholder: None,
        }
    }

    /// With the cache file of `root` in `cache_dir`.
    #[must_use]
    pub fn with_cache(mut self, cache_dir: &Path, root: &Path) -> Self {
        self.cache_file = Some(store::cache_file(cache_dir, root));
        self
    }
}

/// Where the index reads notes from.
pub trait FileSource: Send + Sync {
    /// Whether the file is an online-only placeholder that must not be
    /// read.
    fn is_placeholder(&self, path: &VaultPath) -> io::Result<bool>;
    /// The file's bytes.
    fn read(&self, path: &VaultPath) -> io::Result<Vec<u8>>;
}

/// Notes on disk under a root.
#[derive(Clone)]
pub struct DiskSource {
    root: PathBuf,
    limit: u64,
    placeholder: Option<PlaceholderCheck>,
}

impl std::fmt::Debug for DiskSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskSource")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl DiskSource {
    /// Notes under `root`, read up to `options.max_file_bytes`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, options: &ContentOptions) -> Self {
        Self {
            root: root.into(),
            limit: options.max_file_bytes,
            placeholder: options.placeholder.clone(),
        }
    }
}

impl FileSource for DiskSource {
    fn is_placeholder(&self, path: &VaultPath) -> io::Result<bool> {
        let full = path.to_fs_path(&self.root);
        // `lstat`: never follows or opens the file, so it cannot trigger a
        // download.
        let meta = fs::symlink_metadata(&full)?;
        Ok(is_cloud_placeholder(&meta)
            || self
                .placeholder
                .as_ref()
                .is_some_and(|check| check(&full, &meta)))
    }

    fn read(&self, path: &VaultPath) -> io::Result<Vec<u8>> {
        state::read_capped(&path.to_fs_path(&self.root), self.limit)
    }
}

/// Whether the OS reports the file as an online-only placeholder: macOS
/// `SF_DATALESS` (iCloud Drive, File Provider clients); Windows
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`, `RECALL_ON_OPEN` or `OFFLINE`
/// (OneDrive and other Cloud Files clients). Always `false` elsewhere.
#[must_use]
pub fn is_cloud_placeholder(meta: &fs::Metadata) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        const SF_DATALESS: u32 = 0x4000_0000;
        meta.st_flags() & SF_DATALESS != 0
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const OFFLINE: u32 = 0x0000_1000;
        const RECALL_ON_OPEN: u32 = 0x0004_0000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
        meta.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = meta;
        false
    }
}

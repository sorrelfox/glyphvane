// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The save pipeline: everything that writes a document's file, plus the
//! crash journal, autosave policy and external-change handling around it.
//! Nothing else in Glyphvane writes user files.
//!
//! # Principles
//!
//! - **Never write what the user did not change.** The buffer holds the
//!   text exactly as decoded (`gv-text`), and a save encodes it back with
//!   the same encoding, BOM and line endings. Nothing is normalized, so
//!   bytes outside the edits come out identical. A file whose bytes do not
//!   round-trip opens read-only.
//! - **Never lose a keystroke.** Every applied transaction goes to the crash
//!   journal at once; autosave writes the file after 1 s idle.
//! - **Fail closed.** A character the encoding cannot hold, a file changed
//!   by another program, a read-only file: the save is refused with the
//!   reason, the file is untouched and the buffer stays dirty.
//!
//! # Parts
//!
//! | Module | What |
//! |---|---|
//! | [`DocumentFile`] | Load (decode, size limits, read-only reasons) and save (encode, change check, no-op detection, write) |
//! | [`write_file`] | The write strategies: atomic replace, symlink targets, in place for hardlinks, new files, verification on network file systems |
//! | [`journal`] | The crash journal and recovery |
//! | [`Autosave`] | When to save, over a [`Clock`] |
//! | [`check_external_change`], [`DiskState`] | Whether the file changed on disk since it was loaded or saved |
//! | [`merge3`], [`reload_transaction`] | The 3-way merge for conflicts and the minimal reload diff |
//! | [`sync_conflict_copies`] | Conflict copies left by sync clients |
//!
//! # A document's life
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use gv_text::Buffer;
//! use gv_vault::files::journal::{DocumentKey, JournalStore};
//! use gv_vault::files::{DocumentFile, SaveOptions};
//!
//! let store = JournalStore::open("/data/journal/my-vault")?;
//! let (mut file, decoded) = DocumentFile::load("/notes/Note.md")?;
//! let mut buffer = Buffer::from_decoded(&decoded);
//! let mut journal = store.begin_default(
//!     DocumentKey::Path(file.path().to_path_buf()),
//!     buffer.snapshot(),
//!     file.format(),
//!     file.baseline().map(|b| b.hash),
//! )?;
//!
//! // Each edit: apply, then journal.
//! let edit = gv_text::Transaction::new().insert(0, "# ");
//! buffer.apply(&edit)?;
//! journal.record(&edit, buffer.text())?;
//!
//! // Autosave fired: save, then clear the journal.
//! file.save(buffer.text(), &SaveOptions::default(), &mut journal)?;
//! if let Some(baseline) = file.baseline() {
//!     journal.saved(buffer.snapshot(), file.format(), baseline.hash)?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Platforms
//!
//! | | Linux | macOS | Windows |
//! |---|---|---|---|
//! | Replace | rename | rename | `ReplaceFileW` (a rename where it does not apply), retried on sharing violations for up to 2 s |
//! | Kept | mode, owner/group (where permitted), extended attributes | mode, owner/group, extended attributes (Finder tags, `com.apple.*`), creation time | read-only flag, hidden/system/archive/not-indexed attributes, creation time, ACL, alternate data streams, short name, object id |
//! | Not kept | — | — | file id; on file systems without `ReplaceFileW`, the ACL and alternate data streams |
//! | Hardlinks | written in place | written in place | not detected (the link count needs an unstable API): replaced, which splits the link |
//! | Network detection | `statfs` type (NFS, SMB/CIFS, 9P, …; FUSE) | `statfs` type name (smbfs, nfs, afpfs, webdav; macFUSE) | UNC paths; mapped drives are unknown and treated like shares |

mod autosave;
mod clock;
mod conflicts;
mod disk;
mod document;
mod fskind;
mod hash;
pub mod journal;
mod merge;
mod write;

pub use autosave::{Autosave, AutosaveSettings, QuitDecision, SaveReason, SaveRequest, Suppressed};
pub use clock::{Clock, ManualClock, SystemClock};
pub use conflicts::{SyncConflictCopy, sync_conflict_copies};
pub use disk::{CheckMode, DiskState, ExternalChange, FileIdentity, check_external_change};
pub use document::{
    DocumentFile, LARGE_FILE_BYTES, LoadError, MAX_FILE_BYTES, OnExternalChange, READ_ONLY_BYTES,
    ReadOnlyReason, SaveError, SaveOptions, SaveOutcome, THOROUGH_CHECK_BYTES,
};
pub use fskind::{FsKind, filesystem_kind};
pub use hash::ContentHash;
pub use merge::{MarkerLabels, Merge, MergeChunk, merge3, reload_transaction};
#[doc(hidden)]
pub use write::Fault;
pub use write::{
    Backup, InPlaceReason, MetadataLoss, WriteError, WriteMethod, WriteReport, WriteStep,
    WriteStrategy, write_file,
};

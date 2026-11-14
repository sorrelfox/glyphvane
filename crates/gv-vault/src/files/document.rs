// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Loading a file for editing and saving it back byte for byte.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gv_text::encoding::{decode, decode_with, encode_chunks};
use gv_text::{DecodeStatus, Decoded, EncodeError, Encoding, LossReason, TextFormat, TextSnapshot};

use super::disk::{CheckMode, DiskState, ExternalChange, check_external_change};
use super::fskind::{FsKind, filesystem_kind};
use super::hash::ContentHash;
use super::write::{Backup, Fault, WriteError, WriteReport, WriteStrategy, write_file};

/// Above this size a file opens in large-file mode (full editing, fewer
/// features).
pub const LARGE_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Above this size a file opens read-only.
pub const READ_ONLY_BYTES: u64 = 200 * 1024 * 1024;
/// Above this size a file is not opened.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Up to this size the save-time change check always hashes the file; above
/// it, a matching length, time and identity count as unchanged (except on
/// network file systems, which are always hashed).
pub const THOROUGH_CHECK_BYTES: u64 = 16 * 1024 * 1024;

/// Why a file is open read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnlyReason {
    /// The bytes do not round-trip through the detected encoding; saving
    /// would change bytes the user did not touch. Saving needs an explicit
    /// encoding ([`SaveOptions::convert`]).
    Lossy(LossReason),
    /// The file is larger than [`READ_ONLY_BYTES`].
    TooLarge {
        /// Its length.
        len: u64,
    },
    /// The file may not be written by this process.
    Permissions,
}

impl std::fmt::Display for ReadOnlyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lossy(_) => f.write_str("the file does not round-trip through its encoding"),
            Self::TooLarge { len } => write!(f, "the file is too large to edit ({len} bytes)"),
            Self::Permissions => f.write_str("the file is read-only"),
        }
    }
}

/// Why a file could not be opened as text.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// Reading failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// The file is binary; open it with a viewer or externally.
    #[error("{}: the file is binary, not text", path.display())]
    Binary {
        /// The file.
        path: PathBuf,
    },
    /// The file is larger than [`MAX_FILE_BYTES`].
    #[error("{}: the file is too large to open ({len} bytes)", path.display())]
    TooLarge {
        /// The file.
        path: PathBuf,
        /// Its length.
        len: u64,
    },
    /// The path is a directory or a special file.
    #[error("{}: not a regular file", path.display())]
    NotAFile {
        /// The path.
        path: PathBuf,
    },
}

/// What [`DocumentFile::save`] does when the file changed on disk since it
/// was loaded or last saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnExternalChange {
    /// Refuse with [`SaveError::ExternalChange`] (the conflict banner).
    #[default]
    Refuse,
    /// Overwrite the other version ("Keep mine"), or recreate a deleted
    /// file.
    Overwrite,
}

/// Options for [`DocumentFile::save`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveOptions {
    /// Save in this format instead of the file's own ("Save with
    /// encoding…"). Without it, a character the file's encoding cannot
    /// represent fails the save, and a file that opened lossy cannot be
    /// saved at all.
    pub convert: Option<TextFormat>,
    /// What to do if the file changed on disk.
    pub on_external_change: OnExternalChange,
    /// Atomic replace (default) or in place.
    pub strategy: WriteStrategy,
    /// A failure to inject (data-safety tests).
    #[doc(hidden)]
    pub fault: Option<Fault>,
}

/// What a successful save did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The file already held these bytes; nothing was written.
    Unchanged,
    /// The file was written.
    Written(WriteReport),
}

/// Why a save failed. The file on disk is unchanged in every case (see
/// [`WriteError::InPlace`] for the one exception) and the buffer should stay
/// dirty.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The file is open read-only.
    #[error("not saved: {0}")]
    ReadOnly(ReadOnlyReason),
    /// A character cannot be written in the file's encoding.
    #[error(
        "line {line}, column {column}: {ch:?} (U+{code:04X}) cannot be written in {encoding}; save with another encoding",
        code = u32::from(*ch)
    )]
    Unmappable {
        /// Byte offset of the character in the text.
        offset: usize,
        /// One-based line.
        line: usize,
        /// One-based column, in characters.
        column: usize,
        /// The character.
        ch: char,
        /// The encoding.
        encoding: Encoding,
    },
    /// The text could not be encoded otherwise (for example, the chosen
    /// format asks for a byte-order mark its encoding does not have).
    #[error("not saved: {0}")]
    Encode(EncodeError),
    /// The file changed on disk (or, for a new file, one appeared at the
    /// path). Not overwritten; see [`OnExternalChange`].
    #[error("{}: the file changed on disk", path.display())]
    ExternalChange {
        /// The file.
        path: PathBuf,
        /// What changed.
        change: Box<ExternalChange>,
    },
    /// The file's current state could not be read for the change check.
    #[error("{}: checking the file on disk: {source}", path.display())]
    Check {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// Writing failed.
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// A document's file: where it lives, its on-disk format, and the state the
/// next save is checked against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentFile {
    path: PathBuf,
    format: TextFormat,
    status: DecodeStatus,
    baseline: Option<DiskState>,
    fs: FsKind,
}

impl DocumentFile {
    /// Reads and decodes the file at `path`, detecting the encoding. The
    /// decoded text goes into the editor's buffer
    /// (`gv_text::Buffer::from_decoded`).
    pub fn load(path: impl Into<PathBuf>) -> Result<(Self, Decoded), LoadError> {
        Self::load_inner(path.into(), None)
    }

    /// Like [`DocumentFile::load`], with an encoding the user picked.
    pub fn load_with_encoding(
        path: impl Into<PathBuf>,
        encoding: Encoding,
    ) -> Result<(Self, Decoded), LoadError> {
        Self::load_inner(path.into(), Some(encoding))
    }

    fn load_inner(path: PathBuf, encoding: Option<Encoding>) -> Result<(Self, Decoded), LoadError> {
        let io_err = |source| LoadError::Io {
            path: path.clone(),
            source,
        };
        let meta = fs::metadata(&path).map_err(io_err)?;
        if !meta.is_file() {
            return Err(LoadError::NotAFile { path });
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(LoadError::TooLarge {
                len: meta.len(),
                path,
            });
        }
        let (bytes, state) = DiskState::read(&path).map_err(io_err)?;
        let decoded = match encoding {
            Some(encoding) => decode_with(&bytes, encoding),
            None => decode(&bytes).map_err(|_| LoadError::Binary { path: path.clone() })?,
        };
        let fs = filesystem_kind(&path);
        Ok((
            Self {
                path,
                format: decoded.format,
                status: decoded.status,
                baseline: Some(state),
                fs,
            },
            decoded,
        ))
    }

    /// A file that does not exist yet: the target of the first save of an
    /// untitled document, or of "Save As". If a file appears at `path`
    /// before the save, the save refuses unless told to overwrite.
    pub fn new_file(path: impl Into<PathBuf>, format: TextFormat) -> Self {
        Self {
            path: path.into(),
            format,
            status: DecodeStatus::Exact,
            baseline: None,
            fs: FsKind::Unknown,
        }
    }

    /// The path, as opened (a symlink stays a symlink).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The on-disk format: encoding, BOM, and the line ending for new lines.
    pub fn format(&self) -> TextFormat {
        self.format
    }

    /// Whether the last load or save round-trips exactly.
    pub fn status(&self) -> DecodeStatus {
        self.status
    }

    /// The state the next save and change check compare against. `None` for
    /// a file not written yet.
    pub fn baseline(&self) -> Option<&DiskState> {
        self.baseline.as_ref()
    }

    /// The file system the file is on.
    pub fn fs_kind(&self) -> FsKind {
        self.fs
    }

    /// Whether the file is large enough for large-file mode.
    pub fn is_large(&self) -> bool {
        self.baseline
            .as_ref()
            .is_some_and(|b| b.len > LARGE_FILE_BYTES)
    }

    /// Why editing is blocked, if it is.
    pub fn read_only_reason(&self) -> Option<ReadOnlyReason> {
        if let DecodeStatus::Lossy(reason) = self.status {
            return Some(ReadOnlyReason::Lossy(reason));
        }
        let baseline = self.baseline.as_ref()?;
        if baseline.len > READ_ONLY_BYTES {
            return Some(ReadOnlyReason::TooLarge { len: baseline.len });
        }
        baseline.read_only.then_some(ReadOnlyReason::Permissions)
    }

    /// Compares the file on disk with the baseline: on focus, after a
    /// watcher event, before a save. A file not written yet reports
    /// [`ExternalChange::Unchanged`] while nothing exists at its path.
    pub fn check_external_change(&self, mode: CheckMode) -> io::Result<ExternalChange> {
        let mode = if self.fs.needs_verification() {
            CheckMode::Thorough
        } else {
            mode
        };
        match &self.baseline {
            Some(baseline) => check_external_change(&self.path, baseline, mode),
            None => match DiskState::read(&self.path) {
                Ok((bytes, state)) => Ok(ExternalChange::Modified { state, bytes }),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ExternalChange::Unchanged),
                Err(e) => Err(e),
            },
        }
    }

    /// Makes `state` the new baseline after the buffer took in the disk
    /// version (a reload, "Take theirs", or a merge the user accepted).
    /// `decoded` is how the new bytes were decoded.
    pub fn accept_disk_version(&mut self, decoded: &Decoded, state: DiskState) {
        self.format = decoded.format;
        self.status = decoded.status;
        self.baseline = Some(state);
    }

    /// Makes `state` the new baseline when only the metadata changed
    /// ([`ExternalChange::Touched`]).
    pub fn accept_metadata(&mut self, state: DiskState) {
        self.baseline = Some(state);
    }

    /// Saves `text` to the file.
    ///
    /// 1. Encodes the text in the file's format (or `options.convert`).
    ///    Nothing is normalized: line endings, BOM and every untouched byte
    ///    come out as they were read. A character the encoding cannot
    ///    represent fails with its position.
    /// 2. Compares the file on disk with the baseline and refuses to
    ///    overwrite another program's change unless told to.
    /// 3. Writes nothing if the file already holds these bytes.
    /// 4. Writes through [`write_file`] (atomic replace; in place for
    ///    hardlinks, with `backup` receiving the original first).
    ///
    /// On success the written state becomes the baseline. The caller then
    /// clears the crash journal
    /// ([`DocumentJournal::saved`](super::journal::DocumentJournal::saved)).
    pub fn save(
        &mut self,
        text: &TextSnapshot,
        options: &SaveOptions,
        backup: &mut dyn Backup,
    ) -> Result<SaveOutcome, SaveError> {
        if let Some(reason) = self.read_only_reason() {
            let blocks = match reason {
                ReadOnlyReason::Lossy(_) => options.convert.is_none(),
                ReadOnlyReason::TooLarge { .. } => true,
                // The write itself checks access again and reports it.
                ReadOnlyReason::Permissions => false,
            };
            if blocks {
                return Err(SaveError::ReadOnly(reason));
            }
        }
        let format = options.convert.unwrap_or(self.format);
        let bytes = encode_chunks(text.all_chunks(), &format).map_err(|e| encode_error(text, e))?;
        let hash = ContentHash::of(&bytes);

        if let Some(early) = self.check_before_save(hash, options)? {
            return Ok(early);
        }

        let report = write_file(&self.path, &bytes, options.strategy, backup, options.fault)?;
        self.baseline = Some(report.state.clone());
        self.format = format;
        self.status = DecodeStatus::Exact;
        self.fs = report.fs;
        Ok(SaveOutcome::Written(report))
    }

    /// Saves to another path ("Save As") and retargets this file to it. If a
    /// file exists there, the save refuses unless
    /// [`OnExternalChange::Overwrite`] is set (after the user confirmed).
    pub fn save_as(
        &mut self,
        path: impl Into<PathBuf>,
        text: &TextSnapshot,
        options: &SaveOptions,
        backup: &mut dyn Backup,
    ) -> Result<SaveOutcome, SaveError> {
        let mut target = Self {
            path: path.into(),
            format: self.format,
            status: self.status,
            baseline: None,
            fs: FsKind::Unknown,
        };
        let outcome = target.save(text, options, backup)?;
        *self = target;
        Ok(outcome)
    }

    /// The external-change check of [`DocumentFile::save`]. Returns an
    /// outcome when the save is already done (the disk holds `hash`).
    fn check_before_save(
        &mut self,
        hash: ContentHash,
        options: &SaveOptions,
    ) -> Result<Option<SaveOutcome>, SaveError> {
        let mode = match &self.baseline {
            Some(b) if b.len > THOROUGH_CHECK_BYTES => CheckMode::Quick,
            _ => CheckMode::Thorough,
        };
        let change = self
            .check_external_change(mode)
            .map_err(|source| SaveError::Check {
                path: self.path.clone(),
                source,
            })?;
        let refuse = options.on_external_change == OnExternalChange::Refuse;
        match change {
            ExternalChange::Unchanged => {}
            ExternalChange::Touched(state) => self.baseline = Some(state),
            ExternalChange::Modified { state, .. } if state.hash == hash => {
                // The disk already holds exactly this text.
                self.baseline = Some(state);
                return Ok(Some(SaveOutcome::Unchanged));
            }
            change @ (ExternalChange::Modified { .. } | ExternalChange::Deleted) if refuse => {
                return Err(SaveError::ExternalChange {
                    path: self.path.clone(),
                    change: Box::new(change),
                });
            }
            ExternalChange::Modified { .. } | ExternalChange::Deleted => {
                // Overwrite: the other version is replaced (or the file is
                // recreated), so the old baseline no longer applies.
                self.baseline = None;
                return Ok(None);
            }
        }
        if self.baseline.as_ref().is_some_and(|b| b.hash == hash) {
            return Ok(Some(SaveOutcome::Unchanged));
        }
        Ok(None)
    }
}

fn encode_error(text: &TextSnapshot, error: EncodeError) -> SaveError {
    match error {
        EncodeError::Unmappable {
            offset,
            ch,
            encoding,
        } => {
            let (line, column) = match text.offset_to_point(offset) {
                Ok(point) => {
                    let line_start = offset - point.column;
                    let chars = text
                        .text_for_range(line_start..offset)
                        .map_or(point.column, |s| s.chars().count());
                    (point.line + 1, chars + 1)
                }
                Err(_) => (0, 0),
            };
            SaveError::Unmappable {
                offset,
                line,
                column,
                ch,
                encoding,
            }
        }
        other => SaveError::Encode(other),
    }
}

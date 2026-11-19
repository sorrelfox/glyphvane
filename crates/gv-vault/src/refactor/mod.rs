// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! File operations and journaled refactors: rename and move with link
//! rewriting, undo, new folders, duplicates and the trash.
//!
//! # Rename and move
//!
//! 1. [`plan_moves`] takes the moves (files or folders) and finds the links
//!    they affect: the links to every moved file (the content index's
//!    backlinks) and the moved notes' own links, which change when their
//!    folder does. Each source note is read again and parsed, so the edits
//!    are planned against the bytes on disk. A link is rewritten only when
//!    it would no longer resolve to the same file as before, and then in
//!    its own style: a wikilink stays a wikilink with as many folders as
//!    it had (more only when needed), a relative Markdown link stays
//!    relative, a root-relative one root-relative, and the fragment,
//!    display text and query are kept. The result, a [`RefactorPlan`],
//!    is what the preview shows.
//! 2. [`apply`] checks that every file is still as planned (content hash),
//!    writes the plan with the original texts to the refactor journal,
//!    writes the edited notes (atomic saves), then moves the files. Any
//!    failure rolls everything back. The [`AppliedRefactor`] it returns
//!    holds what [`undo`] needs to revert the whole refactor in one step.
//! 3. [`recover`] rolls back refactors that a crash interrupted.
//!
//! A case-only rename (`A.md` → `a.md`) goes through a temporary name, so
//! it works on case-insensitive file systems. Collisions are checked
//! without regard to case on every OS, so a vault stays portable.
//!
//! # Heading and block-id renames
//!
//! [`plan_anchor_rename`] plans renaming a heading or a block id and
//! rewriting every link to it (see the `anchor` module): the note's own
//! edit for its editor buffer, and a [`RefactorPlan`] without moves for
//! the other notes and canvases, applied and undone like a file rename.
//!
//! # Not rewritten
//!
//! Links inside code are never touched (the index does not see them).
//! Bases `link()` literals are not rewritten yet. Canvas files are: a
//! file card's `file` and a group's `background` that name a moved entry
//! get its new path, with only the bytes of that string replaced
//! (`gv_canvas::patch`).
//! Notes that are not UTF-8 are listed as skipped.
//!
//! The `href` of `<a>` tags in raw HTML is rewritten too (percent-encoded,
//! as a URL), although it is not a link to Obsidian and so not a backlink
//! (see [`crate::content::NoteData::html_links`]): the notes holding one
//! are found by resolving the records' `html_links`. An `href` written
//! with character references (`&amp;`) is listed as skipped.

mod anchor;
#[cfg(test)]
mod anchor_tests;
mod apply;
mod ops;
mod plan;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

pub use anchor::{
    AnchorPlan, AnchorRename, AnchorTarget, block_marker_range, heading_text_range,
    plan_anchor_rename, validate_block_id, validate_heading_text,
};
pub use apply::{AppliedEdit, AppliedRefactor, Recovered, apply, recover, undo};
pub use ops::{
    SystemTrash, Trash, clean_name, create_folder, duplicate, duplicate_name, move_to_trash,
    unique_name, validate_name,
};
pub use plan::plan_moves;

use crate::content::Span;
use crate::files::ContentHash;
use crate::path::VaultPath;

/// One file or folder to move (or rename: a move within its folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    /// Where it is.
    pub from: VaultPath,
    /// Where it goes.
    pub to: VaultPath,
}

impl Move {
    /// Whether only the letter case of the name changes.
    #[must_use]
    pub fn is_case_only(&self) -> bool {
        self.from != self.to
            && crate::keys::fold_key(self.from.as_str()) == crate::keys::fold_key(self.to.as_str())
    }
}

/// One rewritten link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkChange {
    /// The link in the note's text (before the edit).
    pub span: Span,
    /// 0-based line.
    pub line: u32,
    /// The link as written.
    pub old: String,
    /// The link as it will be written.
    pub new: String,
}

/// The planned edits of one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEdit {
    /// The note, where it is now (before any move).
    pub path: VaultPath,
    /// Where it is after the moves (the same unless it moves).
    pub moved_to: VaultPath,
    /// Its text now.
    pub before: String,
    /// Its text after the edits.
    pub after: String,
    /// The hash of `before`'s bytes, checked again before writing.
    pub before_hash: ContentHash,
    /// The changes, in text order.
    pub changes: Vec<LinkChange>,
}

/// A link that should change but cannot be rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The note it is in.
    pub source: VaultPath,
    /// 0-based line (0 for a whole note).
    pub line: u32,
    /// The link as written, or empty for a whole note.
    pub link: String,
    /// Why.
    pub reason: String,
}

/// What a rename or move will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefactorPlan {
    /// The vault root.
    pub root: PathBuf,
    /// The moves, in order.
    pub moves: Vec<Move>,
    /// Every moved entry, old path → new path (folder contents included).
    pub moved: Vec<(VaultPath, VaultPath)>,
    /// The notes whose links change.
    pub edits: Vec<FileEdit>,
    /// Links that will break or could not be updated.
    pub skipped: Vec<Skipped>,
    /// Other things the user should know (a name that becomes ambiguous).
    pub warnings: Vec<String>,
}

impl RefactorPlan {
    /// The number of links rewritten.
    #[must_use]
    pub fn link_count(&self) -> usize {
        self.edits.iter().map(|e| e.changes.len()).sum()
    }

    /// Where `path` (an entry before the moves) is afterwards.
    #[must_use]
    pub fn new_path_of(&self, path: &VaultPath) -> Option<&VaultPath> {
        self.moved
            .iter()
            .find(|(from, _)| from == path)
            .map(|(_, to)| to)
    }
}

/// Why a file operation was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum RefactorError {
    /// The entry to move does not exist.
    #[error("“{0}” does not exist")]
    Missing(VaultPath),
    /// The new path is taken (in any letter case).
    #[error("“{0}” already exists")]
    Exists(VaultPath),
    /// Nothing would change.
    #[error("the name is unchanged")]
    Unchanged,
    /// A folder cannot move into itself.
    #[error("a folder cannot move into itself")]
    IntoItself,
    /// The vault root cannot be renamed, moved or deleted here.
    #[error("the vault folder itself cannot be changed here")]
    Root,
    /// The name is empty or not valid on every platform.
    #[error("{0}")]
    InvalidName(String),
    /// The destination folder does not exist.
    #[error("the folder “{0}” does not exist")]
    NoFolder(VaultPath),
    /// A file changed on disk since the plan was made.
    #[error("“{0}” changed since the preview; try again")]
    Changed(VaultPath),
    /// A file system call failed (everything was rolled back).
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
    /// The refactor journal could not be written (nothing was changed).
    #[error("the refactor journal cannot be written: {0}")]
    Journal(String),
    /// A step failed and the rollback could not undo everything.
    #[error("{0}")]
    Incomplete(String),
    /// The trash failed.
    #[error("cannot move to the trash: {0}")]
    Trash(String),
    /// A note to edit has unsaved changes in an editor.
    #[error("“{0}” has unsaved changes: save it first")]
    Unsaved(VaultPath),
    /// The heading or block to rename is not there (or cannot be renamed).
    #[error("{0}")]
    NoAnchor(String),
}

impl RefactorError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

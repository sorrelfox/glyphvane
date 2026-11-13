// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! [`Buffer`]: the editable text.

use std::ops::{Deref, Range};

use crate::edit::{self, EditError, EditSummary, Transaction};
use crate::encoding::{Decoded, EncodeError, TextFormat, encode_chunks};
use crate::snapshot::{Revision, TextSnapshot};

/// The editable text of one document, with its on-disk format.
///
/// A buffer owns the current [`TextSnapshot`] and replaces it with a new one
/// on every transaction. It dereferences to the snapshot, so every query and
/// conversion is available on the buffer too. [`Buffer::snapshot`] hands out
/// an O(1) copy for other threads.
#[derive(Clone, Debug, Default)]
pub struct Buffer {
    snapshot: TextSnapshot,
    format: TextFormat,
}

impl Buffer {
    /// A buffer holding `text` in the default format (UTF-8, no BOM, LF).
    pub fn new(text: &str) -> Self {
        Self {
            snapshot: TextSnapshot::new(text),
            format: TextFormat::default(),
        }
    }

    /// A buffer holding decoded file contents, keeping their format.
    pub fn from_decoded(decoded: &Decoded) -> Self {
        Self {
            snapshot: TextSnapshot::new(&decoded.text),
            format: decoded.format,
        }
    }

    /// The current snapshot, by reference.
    pub fn text(&self) -> &TextSnapshot {
        &self.snapshot
    }

    /// A copy of the current snapshot (O(1)).
    pub fn snapshot(&self) -> TextSnapshot {
        self.snapshot.clone()
    }

    /// The current revision.
    pub fn revision(&self) -> Revision {
        self.snapshot.revision()
    }

    /// The on-disk format (encoding, BOM, line ending for new lines).
    pub fn format(&self) -> TextFormat {
        self.format
    }

    /// Changes the on-disk format, for example for "Save with encoding".
    pub fn set_format(&mut self, format: TextFormat) {
        self.format = format;
    }

    /// Validates and applies a transaction in one pass. On error the buffer
    /// is unchanged. A transaction that changes nothing keeps the revision.
    pub fn apply(&mut self, transaction: &Transaction) -> Result<EditSummary, EditError> {
        transaction.validate(&self.snapshot)?;
        let (snapshot, summary) = edit::apply(&self.snapshot, transaction);
        self.snapshot = snapshot;
        Ok(summary)
    }

    /// Applies replacements given as `(range, text)` pairs. See
    /// [`Buffer::apply`].
    pub fn edit<I, S>(&mut self, edits: I) -> Result<EditSummary, EditError>
    where
        I: IntoIterator<Item = (Range<usize>, S)>,
        S: Into<String>,
    {
        let mut transaction = Transaction::new();
        for (range, text) in edits {
            transaction.push(range, text);
        }
        self.apply(&transaction)
    }

    /// Encodes the text in the buffer's format, for saving. The text is
    /// written exactly as it is: line endings are never converted.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        encode_chunks(self.snapshot.all_chunks(), &self.format)
    }
}

impl Deref for Buffer {
    type Target = TextSnapshot;

    fn deref(&self) -> &TextSnapshot {
        &self.snapshot
    }
}

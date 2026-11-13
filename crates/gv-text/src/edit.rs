// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Transactions, the edit summary they produce, and anchors.

use std::fmt;
use std::ops::Range;

use rope::Rope;

use crate::coords::Bias;
use crate::lines::AppliedEdit;
use crate::snapshot::{Revision, TextSnapshot};

/// A set of byte-range replacements applied together, in one pass.
///
/// The replacements must be sorted by position and must not overlap (two
/// insertions at the same offset are allowed and are applied in order).
/// Every range is in the coordinates of the text *before* the transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transaction {
    replacements: Vec<Replacement>,
}

/// One replacement in a [`Transaction`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Replacement {
    /// The byte range to replace, in the text before the transaction.
    pub range: Range<usize>,
    /// The new text.
    pub text: String,
}

impl Transaction {
    /// An empty transaction.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a replacement of `range` with `text`.
    #[must_use]
    pub fn replace(mut self, range: Range<usize>, text: impl Into<String>) -> Self {
        self.push(range, text);
        self
    }

    /// Adds an insertion of `text` at `offset`.
    #[must_use]
    pub fn insert(self, offset: usize, text: impl Into<String>) -> Self {
        self.replace(offset..offset, text)
    }

    /// Adds a deletion of `range`.
    #[must_use]
    pub fn delete(self, range: Range<usize>) -> Self {
        self.replace(range, String::new())
    }

    /// Adds a replacement of `range` with `text`.
    pub fn push(&mut self, range: Range<usize>, text: impl Into<String>) {
        self.replacements.push(Replacement {
            range,
            text: text.into(),
        });
    }

    /// The replacements, in the order they were added.
    pub fn replacements(&self) -> &[Replacement] {
        &self.replacements
    }

    /// Whether the transaction has no replacements.
    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty()
    }

    /// Checks the transaction against `text`: every range in bounds, on char
    /// boundaries and not inverted; ranges sorted and not overlapping.
    pub fn validate(&self, text: &TextSnapshot) -> Result<(), EditError> {
        let mut prev: Option<&Range<usize>> = None;
        for (index, replacement) in self.replacements.iter().enumerate() {
            let range = &replacement.range;
            if range.start > range.end {
                return Err(EditError::InvertedRange {
                    index,
                    range: range.clone(),
                });
            }
            if range.end > text.len() {
                return Err(EditError::OutOfBounds {
                    index,
                    range: range.clone(),
                    len: text.len(),
                });
            }
            for offset in [range.start, range.end] {
                if !text.is_char_boundary(offset) {
                    return Err(EditError::NotCharBoundary { index, offset });
                }
            }
            if let Some(prev) = prev {
                if range.start < prev.start {
                    return Err(EditError::Unsorted { index });
                }
                if range.start < prev.end {
                    return Err(EditError::Overlapping { index });
                }
            }
            prev = Some(range);
        }
        Ok(())
    }
}

/// Why a transaction was rejected. `index` is the position of the offending
/// replacement in the transaction.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditError {
    /// The range ends before it starts.
    InvertedRange {
        /// Which replacement.
        index: usize,
        /// Its range.
        range: Range<usize>,
    },
    /// The range extends past the end of the text.
    OutOfBounds {
        /// Which replacement.
        index: usize,
        /// Its range.
        range: Range<usize>,
        /// The text length.
        len: usize,
    },
    /// A range end falls inside a multi-byte character.
    NotCharBoundary {
        /// Which replacement.
        index: usize,
        /// The offending offset.
        offset: usize,
    },
    /// The replacement starts before the previous one.
    Unsorted {
        /// Which replacement.
        index: usize,
    },
    /// The replacement overlaps the previous one.
    Overlapping {
        /// Which replacement.
        index: usize,
    },
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvertedRange { index, range } => {
                write!(f, "edit {index}: range {range:?} ends before it starts")
            }
            Self::OutOfBounds { index, range, len } => write!(
                f,
                "edit {index}: range {range:?} is past the end of the text ({len} bytes)"
            ),
            Self::NotCharBoundary { index, offset } => {
                write!(f, "edit {index}: offset {offset} is inside a character")
            }
            Self::Unsorted { index } => {
                write!(f, "edit {index} starts before the previous edit")
            }
            Self::Overlapping { index } => write!(f, "edit {index} overlaps the previous edit"),
        }
    }
}

impl std::error::Error for EditError {}

/// One change in an [`EditSummary`]: the byte range it replaced in the old
/// text, and the range of the new text in the new text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Edit {
    /// Replaced range, in the text before the transaction.
    pub old: Range<usize>,
    /// Inserted range, in the text after the transaction.
    pub new: Range<usize>,
}

impl Edit {
    /// Length of the replaced text.
    pub fn old_len(&self) -> usize {
        self.old.end - self.old.start
    }

    /// Length of the inserted text.
    pub fn new_len(&self) -> usize {
        self.new.end - self.new.start
    }
}

/// What a transaction changed: its edits in order, with both old and new
/// ranges, between two revisions. This feeds the incremental parser and
/// moves anchors.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditSummary {
    old_revision: Revision,
    new_revision: Revision,
    edits: Vec<Edit>,
}

impl EditSummary {
    /// The revision the edits apply to.
    pub fn old_revision(&self) -> Revision {
        self.old_revision
    }

    /// The revision the edits produce.
    pub fn new_revision(&self) -> Revision {
        self.new_revision
    }

    /// The edits, sorted, non-overlapping. Replacements that changed nothing
    /// (empty range, empty text) are left out.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Whether the text did not change.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Maps an offset in the old text to the new text.
    ///
    /// The result is the same as moving the offset through the edits one
    /// after another, each with this rule:
    ///
    /// - Before the edit: unchanged. After its replaced range: shifted by the
    ///   change in length.
    /// - At an insertion point: [`Bias::Left`] stays before the inserted
    ///   text, [`Bias::Right`] moves after it.
    /// - At the start of a replaced range: the start of the new text.
    /// - Strictly inside a replaced range: the start ([`Bias::Left`]) or the
    ///   end ([`Bias::Right`]) of the new text.
    /// - At the end of a replaced range: the end of the new text.
    ///
    /// So a deletion followed directly by an insertion is not the same as one
    /// replacement: an offset at the start of the deletion with
    /// [`Bias::Right`] ends up after the inserted text.
    ///
    /// Costs O(log k) for k edits, plus the edits that touch the offset.
    pub fn transform_offset(&self, offset: usize, bias: Bias) -> usize {
        // Edits that end before `offset` only shift it.
        let first = self.edits.partition_point(|edit| edit.old.end < offset);
        let mut position = match self.edits.get(first) {
            Some(edit) => (offset + edit.new.start) - edit.old.start,
            None => match self.edits.last() {
                Some(edit) => (offset + edit.new.end) - edit.old.end,
                None => offset,
            },
        };
        // Once edits before it are applied, edit k covers
        // `new.start..new.start + old_len` of the intermediate text.
        for edit in &self.edits[first..] {
            let start = edit.new.start;
            let end = start + edit.old_len();
            if position < start {
                break;
            }
            position = if start == end && position == start {
                match bias {
                    Bias::Left => start,
                    Bias::Right => edit.new.end,
                }
            } else if position == start {
                start
            } else if position < end {
                match bias {
                    Bias::Left => start,
                    Bias::Right => edit.new.end,
                }
            } else {
                position - end + edit.new.end
            };
        }
        position
    }
}

/// A position that survives edits: a byte offset and the side it sticks to.
///
/// An anchor belongs to one revision. Move it to the next one with
/// [`Anchor::transform`] and the [`EditSummary`] of the transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Anchor {
    /// Byte offset in the text of the anchor's revision.
    pub offset: usize,
    /// Which side the anchor sticks to at an insertion point.
    pub bias: Bias,
}

impl Anchor {
    /// Creates an anchor.
    pub const fn new(offset: usize, bias: Bias) -> Self {
        Self { offset, bias }
    }

    /// An anchor that stays before text inserted at `offset`.
    pub const fn before(offset: usize) -> Self {
        Self::new(offset, Bias::Left)
    }

    /// An anchor that moves after text inserted at `offset`.
    pub const fn after(offset: usize) -> Self {
        Self::new(offset, Bias::Right)
    }

    /// The anchor moved through `edits` (see
    /// [`EditSummary::transform_offset`]).
    #[must_use]
    pub fn transform(self, edits: &EditSummary) -> Self {
        Self {
            offset: edits.transform_offset(self.offset, self.bias),
            bias: self.bias,
        }
    }
}

/// Applies a validated transaction in one pass over the rope and the line
/// index.
pub(crate) fn apply(
    snapshot: &TextSnapshot,
    transaction: &Transaction,
) -> (TextSnapshot, EditSummary) {
    let mut applied = Vec::with_capacity(transaction.replacements.len());
    let mut delta: isize = 0;
    for replacement in &transaction.replacements {
        let old = replacement.range.clone();
        if old.is_empty() && replacement.text.is_empty() {
            continue;
        }
        let start = old.start.saturating_add_signed(delta);
        let new = start..start + replacement.text.len();
        delta += replacement.text.len() as isize - old.len() as isize;
        applied.push(AppliedEdit {
            old,
            new,
            text: &replacement.text,
        });
    }
    if applied.is_empty() {
        return (
            snapshot.clone(),
            EditSummary {
                old_revision: snapshot.revision,
                new_revision: snapshot.revision,
                edits: Vec::new(),
            },
        );
    }

    let mut rope = Rope::new();
    let mut cursor = snapshot.rope.cursor(0);
    for edit in &applied {
        rope.append(cursor.slice(edit.old.start));
        cursor.seek_forward(edit.old.end);
        rope.push(edit.text);
    }
    rope.append(cursor.suffix());

    let lines = snapshot.lines.apply(&applied, &rope);
    let new_revision = snapshot.revision.next();
    let edits = applied
        .into_iter()
        .map(|edit| Edit {
            old: edit.old,
            new: edit.new,
        })
        .collect();
    (
        TextSnapshot {
            rope,
            lines,
            revision: new_revision,
        },
        EditSummary {
            old_revision: snapshot.revision,
            new_revision,
            edits,
        },
    )
}

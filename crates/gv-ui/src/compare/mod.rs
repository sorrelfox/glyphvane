// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The compare and merge view: two versions of a document side by side,
//! with a choice per changed region (FR-FILE-06).
//!
//! Not designed (SPEC § Not designed yet lists it). It is built from the
//! infobar's buttons, the panel header and the editor's monospaced text,
//! with existing tokens only, and kept plain:
//!
//! - a header with the title, the number of changes and of conflicts left,
//!   *Use all left*, *Use all right*, *Cancel* and *Apply*;
//! - the two side names above two columns;
//! - unchanged runs once across both columns (long ones shortened);
//! - each changed region ("hunk") as a bordered block with its two sides
//!   and *Use left*, *Use right* and *Use both*; the chosen side is tinted
//!   `accent.tint`, a conflict nobody chose yet has a `status.error`
//!   border.
//!
//! # Where it is used
//!
//! | Opened from | Left | Right | Merge |
//! |---|---|---|---|
//! | The conflict infobar's Compare (a file changed on disk with unsaved edits) | The buffer | The file on disk | 3-way, against the text last loaded or saved (the journal base) |
//! | The recovery infobar's Compare | The recovered text | The file on disk | 2-way |
//! | The sync-conflict infobar's Compare | The buffer | The conflict copy | 2-way |
//!
//! # Rules
//!
//! - The right side is only read: nothing is written to disk. *Apply*
//!   replaces the buffer with the result as **one undoable edit** (it is
//!   journaled and saved like any edit); *Cancel* changes nothing.
//! - Each region starts with the side that changed it (in a 2-way compare,
//!   the left one); a conflict starts unresolved and, if applied so, is
//!   written with conflict markers (plan 11: "conflicts marked inline").
//! - *Use both* writes the left side, then the right one.
//! - The view is made from the text as it was when it opened. The buffer
//!   cannot be typed in while it shows; should it have changed anyway,
//!   *Apply* compares again instead of overwriting the change.

#[cfg(test)]
mod tests;
mod view;

use gpui::SharedString;
use gv_text::LineEnding;
use gv_vault::files::{MergeChunk, merge3_regions};

pub use view::CompareView;

/// Which side a changed region takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Left,
    Right,
    /// The left side, then the right one.
    Both,
}

/// Who changed a region, against the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    /// Only the left side (in a 2-way compare: every region).
    Left,
    /// Only the right side.
    Right,
    /// Both, in the same way.
    Same,
    /// Both, differently.
    Conflict,
}

/// A changed region: the text of each side and the choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub kind: HunkKind,
    pub left: String,
    pub right: String,
    /// `None` for a conflict nobody chose yet.
    pub pick: Option<Pick>,
}

/// A region of the comparison, in text order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Region {
    /// The same on both sides.
    Unchanged(String),
    Changed(Hunk),
}

/// The names of the two sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sides {
    pub left: SharedString,
    pub right: SharedString,
}

/// Two versions of a text, region by region, with a choice per change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    regions: Vec<Region>,
    sides: Sides,
    line_ending: LineEnding,
}

impl Comparison {
    /// A 3-way comparison of `left` and `right`, which both started from
    /// `base`.
    pub fn three_way(
        base: &str,
        left: &str,
        right: &str,
        sides: Sides,
        line_ending: LineEnding,
    ) -> Self {
        let regions = merge3_regions(base, left, right)
            .into_iter()
            .map(|region| {
                let (kind, pick) = match region.chunk() {
                    MergeChunk::Unchanged(text) => return Region::Unchanged(text),
                    MergeChunk::Ours(_) => (HunkKind::Left, Some(Pick::Left)),
                    MergeChunk::Theirs(_) => (HunkKind::Right, Some(Pick::Right)),
                    MergeChunk::Both(_) => (HunkKind::Same, Some(Pick::Left)),
                    MergeChunk::Conflict { .. } => (HunkKind::Conflict, None),
                };
                Region::Changed(Hunk {
                    kind,
                    left: region.ours,
                    right: region.theirs,
                    pick,
                })
            })
            .collect();
        Self {
            regions,
            sides,
            line_ending,
        }
    }

    /// A 2-way comparison: every difference is the left side's change, and
    /// starts with the left side.
    pub fn two_way(left: &str, right: &str, sides: Sides, line_ending: LineEnding) -> Self {
        Self::three_way(right, left, right, sides, line_ending)
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    pub fn sides(&self) -> &Sides {
        &self.sides
    }

    /// The changed regions, in order.
    pub fn hunks(&self) -> impl Iterator<Item = &Hunk> {
        self.regions.iter().filter_map(|r| match r {
            Region::Changed(hunk) => Some(hunk),
            Region::Unchanged(_) => None,
        })
    }

    pub fn hunk_count(&self) -> usize {
        self.hunks().count()
    }

    /// Conflicts nobody chose a side for yet.
    pub fn unresolved(&self) -> usize {
        self.hunks().filter(|h| h.pick.is_none()).count()
    }

    /// Chooses `pick` for changed region `hunk` (counted among the changed
    /// ones).
    pub fn pick(&mut self, hunk: usize, pick: Pick) {
        if let Some(h) = self.hunks_mut().nth(hunk) {
            h.pick = Some(pick);
        }
    }

    /// Chooses `pick` for every changed region.
    pub fn pick_all(&mut self, pick: Pick) {
        for hunk in self.hunks_mut() {
            hunk.pick = Some(pick);
        }
    }

    fn hunks_mut(&mut self) -> impl Iterator<Item = &mut Hunk> {
        self.regions.iter_mut().filter_map(|r| match r {
            Region::Changed(hunk) => Some(hunk),
            Region::Unchanged(_) => None,
        })
    }

    /// The text the choices make. An unresolved conflict is written with
    /// conflict markers naming the sides.
    pub fn result(&self) -> String {
        let eol = self.line_ending.as_str();
        let mut out = String::new();
        // A side whose last line has no terminator gets one before what
        // follows it.
        let end_line = |out: &mut String| {
            if !out.is_empty() && !out.ends_with(['\n', '\r']) {
                out.push_str(eol);
            }
        };
        for region in &self.regions {
            match region {
                Region::Unchanged(text) => out.push_str(text),
                Region::Changed(hunk) => match hunk.pick {
                    Some(Pick::Left) => out.push_str(&hunk.left),
                    Some(Pick::Right) => out.push_str(&hunk.right),
                    Some(Pick::Both) => {
                        out.push_str(&hunk.left);
                        if !hunk.right.is_empty() {
                            end_line(&mut out);
                        }
                        out.push_str(&hunk.right);
                    }
                    None => {
                        end_line(&mut out);
                        out.push_str("<<<<<<< ");
                        out.push_str(&self.sides.left);
                        out.push_str(eol);
                        out.push_str(&hunk.left);
                        end_line(&mut out);
                        out.push_str("=======");
                        out.push_str(eol);
                        out.push_str(&hunk.right);
                        end_line(&mut out);
                        out.push_str(">>>>>>> ");
                        out.push_str(&self.sides.right);
                        out.push_str(eol);
                    }
                },
            }
        }
        out
    }
}

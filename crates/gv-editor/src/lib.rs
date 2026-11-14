// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Buffer model, selections, transactions, undo, Markdown commands and the display map.
//!
//! The crate is GUI-free: the editor view in `gv-ui` reads its snapshots
//! and injects what needs a font system (see
//! [`display_map::TextMeasure`]).
//!
//! # The editing model
//!
//! [`Editor`] owns a [`gv_text::Buffer`] and everything that follows its
//! edits:
//!
//! - [`Selections`]: one or more selections, sorted and merged, each a tail
//!   and a head [`gv_text::Anchor`] plus a [`SelectionGoal`] for vertical
//!   movement;
//! - the undo [`History`]: transactions grouped into steps (see
//!   [`EditKind`] for the grouping rules), with selections restored, a
//!   memory bound and save points;
//! - the [`display_map`] layers (replace and tab, and the wrap layer while
//!   soft wrap is on: [`Editor::set_wrap_width`]), synced on every edit;
//!   vertical motion runs on display rows (wrap rows when wrapping);
//! - folds: regions of the text shown as one placeholder (see
//!   [`Editor::fold_at`] and [`folds`] for the foldable regions); motions
//!   skip them, a caret placed inside one and an edit inside one unfold it,
//!   and undo brings back folds an edit removed;
//! - input-method state: marked text and the UTF-16 queries and
//!   replacements a platform text-input handler needs;
//! - observers, told about every change with its
//!   [`gv_text::EditSummary`]: the hook for the incremental parser and the
//!   render model.
//!
//! Every edit command is **one transaction** for all cursors: one pass over
//! the rope, one summary, one undo record.
//!
//! ```
//! use gv_editor::{DeleteUnit, Editor, Motion};
//! use gv_text::Buffer;
//!
//! let mut editor = Editor::new(Buffer::new("one\ntwo\n"));
//! editor.add_cursor(4);
//! editor.insert_at_selections("> ");
//! assert_eq!(editor.text().to_string(), "> one\n> two\n");
//! editor.move_selections(Motion::LineEnd, false);
//! editor.delete_backward(DeleteUnit::Word);
//! assert_eq!(editor.text().to_string(), "> \n> \n");
//! editor.undo();
//! editor.undo();
//! assert_eq!(editor.text().to_string(), "one\ntwo\n");
//! ```
//!
//! # Modules
//!
//! - [`boundaries`]: grapheme, word and line boundaries on a snapshot;
//! - [`movement`]: caret motions;
//! - [`commands`]: the Markdown-aware editing commands (list continuation,
//!   indentation, renumbering, formatting, headings, quotes, links);
//! - [`display_map`]: the replace layer (folds), the tab layer and the
//!   wrap layer, ported from Zed;
//! - [`folds`]: the foldable regions of a Markdown document.

pub mod boundaries;
mod clock;
pub mod commands;
pub mod display_map;
mod editor;
pub mod folds;
mod history;
pub mod movement;
mod selection;

pub use clock::{Clock, ManualClock, SystemClock};
pub use commands::{
    Availability, CommandSettings, Flavor, MarkdownCommand, MarkdownCommands, Outcome, SyntaxRef,
};
pub use editor::{
    DeleteUnit, EditCause, EditEvent, Editor, EditorConfig, EditorEvent, FoldPlaceholder,
    ObserverId, SavePoint, Utf16Selection,
};
pub use folds::{FoldCandidate, FoldKind, foldable_ranges};
pub use history::{EditKind, History, HistoryConfig, StateId, UndoOutcome};
pub use movement::Motion;
pub use selection::{Selection, SelectionGoal, SelectionId, SelectionSpec, Selections};

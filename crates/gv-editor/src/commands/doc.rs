// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The working copy a command edits in steps.
//!
//! A command that needs the parse of its own intermediate result (list
//! continuation followed by renumbering, indenting followed by renumbering
//! of both lists) applies each step to a copy of the buffer (O(1): the rope
//! is shared) and reparses incrementally. The steps' patches are composed,
//! so the editor finally receives **one** transaction with the net
//! replacements, recorded as one undo step.

use std::mem;
use std::ops::Range;

use gv_syntax::{Budget, Reparse, SyntaxTree};
use gv_text::{Bias, Buffer, EditSummary, LineEnding, TextSnapshot, Transaction};

use crate::editor::Editor;
use crate::history::{EditKind, PatchEdit, compose, patch_of};
use crate::selection::SelectionSpec;

use super::SyntaxRef;
use super::flavor::Flavor;

/// One line of the working text.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    /// 0-based line index.
    pub index: usize,
    /// Offset of the line start.
    pub start: usize,
    /// The line's content, without the terminator.
    pub text: String,
    /// The terminator (`None` on the last line).
    pub terminator: Option<LineEnding>,
}

impl Line {
    /// Offset of the end of the content (before the terminator).
    pub fn end(&self) -> usize {
        self.start + self.text.len()
    }

    /// Offset after the terminator.
    pub fn full_end(&self) -> usize {
        self.end() + self.terminator.map_or(0, LineEnding::byte_len)
    }

    /// The content bytes.
    pub fn bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    /// `offset` relative to the line start, clamped to the content.
    pub fn rel(&self, offset: usize) -> usize {
        offset.saturating_sub(self.start).min(self.text.len())
    }

    /// Whether the line holds only spaces and tabs from `rel` on.
    pub fn blank_from(&self, rel: usize) -> bool {
        self.bytes()
            .get(rel..)
            .is_none_or(|rest| rest.iter().all(|&b| b == b' ' || b == b'\t'))
    }
}

/// The text being edited, its parse, the composed patch and the selections.
pub(crate) struct Doc {
    buffer: Buffer,
    tree: SyntaxTree,
    patch: Vec<PatchEdit>,
    /// The selections, in the coordinates of the current text.
    pub sels: Vec<SelectionSpec>,
    /// Index of the primary selection in `sels`.
    pub primary: usize,
    /// The document's flavor.
    pub flavor: Flavor,
}

impl Doc {
    /// A working copy of `editor`'s text. `syntax` is used if it is the
    /// parse of exactly that text; otherwise the text is parsed with the
    /// flavor's options.
    pub fn new(editor: &Editor, syntax: Option<SyntaxRef<'_>>, flavor: Flavor) -> Doc {
        let buffer = editor.buffer().clone();
        let tree = match syntax {
            Some(syntax) if syntax.is_parse_of(buffer.text()) => syntax.tree.clone(),
            _ => SyntaxTree::parse_with(buffer.text(), flavor.parse_options()),
        };
        let (sels, primary) = editor.selections().specs();
        Doc {
            buffer,
            tree,
            patch: Vec::new(),
            sels,
            primary,
            flavor,
        }
    }

    /// The current text.
    pub fn text(&self) -> &TextSnapshot {
        self.buffer.text()
    }

    /// The parse of the current text.
    pub fn tree(&self) -> &SyntaxTree {
        &self.tree
    }

    /// The length of the current text.
    pub fn len(&self) -> usize {
        self.buffer.text().len()
    }

    /// The text of `range` (clipped).
    pub fn slice(&self, range: Range<usize>) -> String {
        let text = self.text();
        let start = text.clip_offset(range.start, Bias::Left);
        let end = text.clip_offset(range.end, Bias::Right).max(start);
        text.text_for_range(start..end).unwrap_or_default()
    }

    /// The line with index `index` (clamped to the last line).
    pub fn line(&self, index: usize) -> Line {
        let text = self.text();
        let index = index.min(text.line_count().saturating_sub(1));
        match text.line_info(index) {
            Ok(info) => Line {
                index,
                start: info.content.start,
                text: text.text_for_range(info.content).unwrap_or_default(),
                terminator: info.terminator,
            },
            Err(_) => Line {
                index,
                start: text.len(),
                text: String::new(),
                terminator: None,
            },
        }
    }

    /// The index of the line holding `offset` (clamped).
    pub fn line_of(&self, offset: usize) -> usize {
        let text = self.text();
        let offset = text.clip_offset(offset, Bias::Left);
        text.line_of_offset(offset)
            .unwrap_or(text.line_count().saturating_sub(1))
    }

    /// The line holding `offset`.
    pub fn line_at(&self, offset: usize) -> Line {
        self.line(self.line_of(offset))
    }

    /// The number of lines.
    pub fn line_count(&self) -> usize {
        self.text().line_count()
    }

    /// The line ending to insert on `line`: its own, else the one before
    /// it, else the buffer's.
    pub fn line_ending_for(&self, line: &Line) -> &'static str {
        if let Some(ending) = line.terminator {
            return ending.as_str();
        }
        if line.index > 0
            && let Some(ending) = self.line(line.index - 1).terminator
        {
            return ending.as_str();
        }
        self.buffer.format().line_ending.as_str()
    }

    /// Applies one step: replacements in the current coordinates, in any
    /// order. Overlapping replacements after the first are dropped. The
    /// tree is reparsed, the selections are moved, and the step joins the
    /// composed patch. Returns the summary if the text changed.
    pub fn apply(&mut self, mut edits: Vec<(Range<usize>, String)>) -> Option<EditSummary> {
        edits.retain(|(range, text)| !(range.is_empty() && text.is_empty()));
        if edits.is_empty() {
            return None;
        }
        edits.sort_by_key(|(range, _)| (range.start, range.end));
        let mut transaction = Transaction::new();
        let mut end = 0;
        for (range, text) in edits {
            if range.start < end || range.end > self.len() {
                continue;
            }
            end = range.end;
            transaction.push(range, text);
        }
        let old = self.buffer.snapshot();
        let patch = patch_of(&old, &transaction);
        let summary = self.buffer.apply(&transaction).ok()?;
        if summary.is_empty() {
            return None;
        }
        self.tree = match self
            .tree
            .reparse(self.buffer.text(), &summary, Budget::Unlimited)
        {
            Reparse::Done { tree, .. } => tree,
            Reparse::Partial { mut task, .. } => {
                task.run(self.buffer.text(), Budget::Unlimited);
                task.finish().0
            }
        };
        self.patch = compose(mem::take(&mut self.patch), patch);
        for spec in &mut self.sels {
            *spec = transform_spec(*spec, &summary);
        }
        Some(summary)
    }

    /// Hands the net change to `editor` as one command transaction and
    /// sets the selections. Returns whether the text changed.
    pub fn commit(self, editor: &mut Editor) -> bool {
        let edits: Vec<(Range<usize>, String)> = self
            .patch
            .into_iter()
            .map(|edit| (edit.old, edit.new_text))
            .collect();
        let changed =
            !edits.is_empty() && matches!(editor.edit(edits, EditKind::Command), Ok(Some(_)));
        debug_assert!(
            !changed || editor.text().text_eq(self.buffer.text()),
            "the composed patch does not reproduce the working text"
        );
        if self.sels.is_empty() {
            return changed;
        }
        let current = editor.selections().specs();
        let wanted: Vec<SelectionSpec> = self
            .sels
            .iter()
            .map(|spec| SelectionSpec::new(spec.tail, spec.head))
            .collect();
        let primary = self.primary.min(wanted.len() - 1);
        let same = current.1 == primary
            && current.0.len() == wanted.len()
            && current
                .0
                .iter()
                .zip(&wanted)
                .all(|(a, b)| a.tail == b.tail && a.head == b.head);
        if !same {
            editor.set_selections(&wanted, primary);
        }
        changed
    }
}

/// A selection moved through an edit the way the editor moves it: a caret
/// sticks after insertions, a selection grows with neither end.
fn transform_spec(spec: SelectionSpec, summary: &EditSummary) -> SelectionSpec {
    let (tail_bias, head_bias) = match spec.tail.cmp(&spec.head) {
        std::cmp::Ordering::Equal => (Bias::Right, Bias::Right),
        std::cmp::Ordering::Less => (Bias::Right, Bias::Left),
        std::cmp::Ordering::Greater => (Bias::Left, Bias::Right),
    };
    let tail = summary.transform_offset(spec.tail, tail_bias);
    let head = summary.transform_offset(spec.head, head_bias);
    SelectionSpec::new(tail, head)
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Rename heading or block ID (FR-NAV-11, `refactor.rename_heading`).
//!
//! - **What is renamed.** The heading on the caret's line, else the block
//!   with an id (`^id`) the caret is in or whose marker line it is on,
//!   else the heading of the section the caret is in.
//! - **The prompt** ([`crate::file_prompt`]) asks for the new heading text
//!   (its Markdown source) or id, all of it selected, and checks it as it
//!   is typed: a heading needs one line of text; an id letters, digits and
//!   `-`, not taken by another block of the note.
//! - **Planning** runs off the UI thread
//!   ([`gv_vault::refactor::plan_anchor_rename`]) on the buffer's text,
//!   saved or not, with the vault's content index: the note's own links
//!   and every other note's links, embeds and canvas card subpaths that
//!   name the heading or block, each kept in its style. Notes open with
//!   unsaved changes elsewhere are refused (save them first).
//! - **Preview.** When other files change, or some links cannot be
//!   updated, the prompt lists every file with each link as it is and as
//!   it will be; Enter applies. Otherwise the rename is applied at once.
//! - **Applying.** The note's own edit (the heading or marker and its own
//!   links) is one undo step of the buffer; the other files are written
//!   through the refactor journal ([`gv_vault::refactor::apply`]). If
//!   those writes fail, the buffer edit is taken back.
//! - **Undo.** The document's infobar says what was done and offers
//!   *Undo*, which reverts the other files and the buffer edit together.
//!   Edit ▸ Undo in the note takes back the buffer part alone.
//!
//! The prompt, the preview and the infobar are not designed; they reuse
//! the file prompt and the infobar of the file operations.

use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Div, InteractiveElement as _, SharedString, Stateful, Task, Window,
};
use gv_editor::EditKind;
use gv_vault::refactor::{
    self, AnchorPlan, AnchorRename, AnchorTarget, AppliedRefactor, LinkChange, RefactorError,
};

use super::EditorView;
use crate::file_prompt::{self, PromptField, PromptRequest, PromptRow, RowStyle};
use crate::files::notice::Notice;
use crate::keymap::actions::refactor as commands;

/// The last rename of this document, until it is undone or replaced, and
/// the work in flight.
#[derive(Default)]
pub(super) struct RenameState {
    undo: Option<RenameUndo>,
    _task: Option<Task<()>>,
}

/// What the infobar's Undo needs.
struct RenameUndo {
    /// The other files' edits, if any.
    applied: Option<AppliedRefactor>,
    /// The buffer's text right after the rename.
    after: String,
    /// The edits that take the buffer back (in `after`'s offsets).
    revert: Vec<(Range<usize>, String)>,
}

/// What the caret names, with its current name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnchorAtCaret {
    pub target: AnchorTarget,
    pub name: String,
}

impl EditorView {
    /// The handler of `refactor.rename_heading`, while the text can change.
    pub(super) fn rename_handlers(
        &self,
        host: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        if self.read_only || self.doc.is_none() || self.path.is_none() {
            return host;
        }
        host.on_action(
            cx.listener(|this, _: &commands::RenameHeading, window, cx| {
                this.rename_anchor(window, cx);
            }),
        )
    }

    /// The text of the buffer.
    fn buffer_text(&self) -> Option<String> {
        let text = self.doc.as_ref()?.editor.text();
        text.text_for_range(0..text.len()).ok()
    }

    /// The heading or block id the caret names.
    pub(crate) fn anchor_at_caret(&mut self) -> Option<AnchorAtCaret> {
        let note = self.current_note();
        let text = self.buffer_text()?;
        let caret = self.doc.as_ref()?.editor.selections().primary().head();
        let line = line_of(&text, caret);
        if let Some((i, h)) = note
            .headings
            .iter()
            .enumerate()
            .find(|(_, h)| h.line == line || h.span.contains(caret))
        {
            return Some(AnchorAtCaret {
                target: AnchorTarget::Heading(i),
                name: h.source.to_string(),
            });
        }
        for b in &note.block_ids {
            let marker = refactor::block_marker_range(&text, b.span, &b.id);
            let on_marker = marker.is_some_and(|m| line_of(&text, m.start) == line);
            if b.span.contains(caret) || on_marker {
                return Some(AnchorAtCaret {
                    target: AnchorTarget::Block(b.id.to_string()),
                    name: b.id.to_string(),
                });
            }
        }
        let (i, h) = note
            .headings
            .iter()
            .enumerate()
            .rfind(|(_, h)| h.span.start as usize <= caret)?;
        Some(AnchorAtCaret {
            target: AnchorTarget::Heading(i),
            name: h.source.to_string(),
        })
    }

    /// `refactor.rename_heading`: asks for the new name of the heading or
    /// block id at the caret.
    pub(crate) fn rename_anchor(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.read_only {
            return;
        }
        let Some(anchor) = self.anchor_at_caret() else {
            self.rename_failed(
                "Nothing to rename.",
                "Put the caret on a heading or a block with an id",
                cx,
            );
            return;
        };
        let is_block = matches!(anchor.target, AnchorTarget::Block(_));
        let taken: Vec<String> = self
            .current_note()
            .block_ids
            .iter()
            .map(|b| b.id.to_string())
            .filter(|id| *id != anchor.name)
            .collect();
        let validate: file_prompt::Validate = if is_block {
            Rc::new(move |text: &str| {
                let id = text.trim().trim_start_matches('^');
                if let Err(e) = refactor::validate_block_id(id) {
                    return Some(SharedString::from(e.to_string()));
                }
                taken
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case(id))
                    .then(|| format!("Another block already has the id ^{id}").into())
            })
        } else {
            Rc::new(|text: &str| {
                refactor::validate_heading_text(text)
                    .err()
                    .map(|e| SharedString::from(e.to_string()))
            })
        };
        let this = cx.weak_entity();
        let target = anchor.target.clone();
        let request = PromptRequest {
            title: if is_block {
                "Rename block ID"
            } else {
                "Rename heading"
            }
            .into(),
            message: Some(
                format!(
                    "“{}”: links to it in this vault are updated.",
                    if is_block {
                        format!("^{}", anchor.name)
                    } else {
                        anchor.name.clone()
                    }
                )
                .into(),
            ),
            field: Some(PromptField {
                select: 0..anchor.name.len(),
                initial: anchor.name,
            }),
            rows: Vec::new(),
            action: "rename".into(),
            validate: Some(validate),
            on_confirm: Rc::new(move |text, window, cx| {
                let target = target.clone();
                let _ = this.update(cx, |e, cx| e.rename_anchor_to(target, text, window, cx));
            }),
        };
        window.defer(cx, move |window, cx| {
            file_prompt::open(request, window, cx);
        });
    }

    /// Plans renaming `target` to `new_name` off the UI thread, then
    /// previews or applies it.
    pub(crate) fn rename_anchor_to(
        &mut self,
        target: AnchorTarget,
        new_name: String,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(text) = self.buffer_text() else {
            return;
        };
        let vault = self.vault();
        let (Some(root), Some(paths), Some(note)) = (vault.root, vault.index, vault.source) else {
            self.rename_failed("Not renamed.", "The note is not in an indexed folder", cx);
            return;
        };
        let content = vault.content;
        let params = vault.params;
        let rename = AnchorRename {
            note,
            target,
            new_name,
        };
        let task = cx.background_spawn(async move {
            refactor::plan_anchor_rename(&root, &paths, content.as_ref(), &params, &text, &rename)
        });
        self.rename._task = Some(cx.spawn_in(window, async move |this, cx| {
            let planned = task.await;
            let _ = this.update_in(cx, |e, window, cx| e.anchor_planned(planned, window, cx));
        }));
    }

    fn anchor_planned(
        &mut self,
        planned: Result<AnchorPlan, RefactorError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let plan = match planned {
            Ok(planned) => planned,
            Err(RefactorError::Unchanged) => return,
            Err(err) => {
                self.rename_failed("Not renamed.", &err.to_string(), cx);
                return;
            }
        };
        let others = &plan.others;
        if others.edits.is_empty() && others.skipped.is_empty() && others.warnings.is_empty() {
            self.apply_anchor_plan(plan, cx);
            return;
        }
        let request = preview_request(plan, cx.weak_entity());
        window.defer(cx, move |window, cx| {
            file_prompt::open(request, window, cx);
        });
    }

    /// Applies `plan`: the buffer edit as one undo step, then the other
    /// files through the journal, off the UI thread.
    pub(crate) fn apply_anchor_plan(&mut self, plan: AnchorPlan, cx: &mut Context<'_, Self>) {
        if self.read_only || self.buffer_text().as_deref() != Some(plan.own.before.as_str()) {
            self.rename_failed(
                "Not renamed.",
                "The note changed while the rename was prepared; try again",
                cx,
            );
            return;
        }
        let forward: Vec<(Range<usize>, String)> = plan
            .own
            .changes
            .iter()
            .map(|c| (c.span.range(), c.new.clone()))
            .collect();
        if !self.edit_buffer(forward, cx) {
            self.rename_failed("Not renamed.", "The note could not be edited", cx);
            return;
        }
        let after = self.buffer_text().unwrap_or_default();
        let revert = revert_edits(&plan.own.changes);
        let done = done_title(&plan);
        let detail = links_detail(plan.link_count(), plan.other_files());
        if plan.others.edits.is_empty() {
            self.rename.undo = Some(RenameUndo {
                applied: None,
                after,
                revert,
            });
            self.show_doc_notice(Some(Notice::file_op_done(done, detail, true)), cx);
            return;
        }
        let journal = journal_dir(cx);
        let others = plan.others;
        let task = cx.background_spawn(async move { refactor::apply(&others, journal.as_deref()) });
        self.rename._task = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |e, cx| match result {
                Ok(applied) => {
                    e.rename.undo = Some(RenameUndo {
                        applied: Some(applied),
                        after,
                        revert,
                    });
                    e.show_doc_notice(Some(Notice::file_op_done(done, detail, true)), cx);
                }
                Err(err) => {
                    // The other files are as they were: so is the note.
                    if e.buffer_text().as_deref() == Some(after.as_str()) {
                        e.edit_buffer(revert, cx);
                    }
                    e.rename_failed("Not renamed.", &err.to_string(), cx);
                }
            });
        }));
    }

    /// The infobar's Undo: reverts the last rename, the other files and
    /// the buffer edit together.
    pub(crate) fn undo_anchor_rename(&mut self, cx: &mut Context<'_, Self>) {
        let Some(undo) = self.rename.undo.take() else {
            return;
        };
        if self.buffer_text().as_deref() != Some(undo.after.as_str()) {
            self.rename_failed(
                "Not undone.",
                "The note changed since the rename; use Edit ▸ Undo in it",
                cx,
            );
            return;
        }
        let Some(applied) = undo.applied else {
            self.edit_buffer(undo.revert, cx);
            self.show_doc_notice(
                Some(Notice::file_op_done("Undone.".to_owned(), None, false)),
                cx,
            );
            return;
        };
        let journal = journal_dir(cx);
        let task = cx.background_spawn(async move {
            let result = refactor::undo(&applied, journal.as_deref());
            (applied, result)
        });
        let (after, revert) = (undo.after, undo.revert);
        self.rename._task = Some(cx.spawn(async move |this, cx| {
            let (applied, result) = task.await;
            let _ = this.update(cx, |e, cx| match result {
                Ok(()) => {
                    if e.buffer_text().as_deref() == Some(after.as_str()) {
                        e.edit_buffer(revert, cx);
                    }
                    e.show_doc_notice(
                        Some(Notice::file_op_done("Undone.".to_owned(), None, false)),
                        cx,
                    );
                }
                Err(err) => {
                    e.rename.undo = Some(RenameUndo {
                        applied: Some(applied),
                        after,
                        revert,
                    });
                    e.rename_failed("Not undone.", &err.to_string(), cx);
                }
            });
        }));
    }

    /// Replaces ranges of the buffer as one undo step.
    fn edit_buffer(
        &mut self,
        edits: Vec<(Range<usize>, String)>,
        cx: &mut Context<'_, Self>,
    ) -> bool {
        let Some(doc) = self.doc.as_mut() else {
            return false;
        };
        let caret = doc.editor.selections().primary().head();
        let ok = match doc.editor.edit(edits.clone(), EditKind::Command) {
            Ok(_) => {
                doc.editor.set_cursor(shift_offset(caret, &edits));
                true
            }
            Err(err) => {
                tracing::warn!("cannot apply the rename to the note: {err:?}");
                false
            }
        };
        self.input_changed(cx);
        ok
    }

    fn rename_failed(&mut self, title: &str, reason: &str, cx: &mut Context<'_, Self>) {
        self.show_doc_notice(Some(Notice::file_op_failed(title, reason)), cx);
    }
}

/// The 0-based line of byte `offset`.
fn line_of(text: &str, offset: usize) -> u32 {
    let end = offset.min(text.len());
    let n = text.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Where `offset` is after `edits` (sorted, not overlapping): moved by the
/// edits before it, at the end of an edit it was inside.
fn shift_offset(offset: usize, edits: &[(Range<usize>, String)]) -> usize {
    let mut delta: isize = 0;
    let mut sorted: Vec<&(Range<usize>, String)> = edits.iter().collect();
    sorted.sort_by_key(|(r, _)| r.start);
    for (range, text) in sorted {
        if range.start >= offset {
            break;
        }
        if range.end > offset {
            return range.start.saturating_add_signed(delta) + text.len();
        }
        delta += text.len() as isize - range.len() as isize;
    }
    offset.saturating_add_signed(delta)
}

/// The edits that take back `changes` (spans in the text before them),
/// in the offsets of the text after them.
fn revert_edits(changes: &[LinkChange]) -> Vec<(Range<usize>, String)> {
    let mut sorted: Vec<&LinkChange> = changes.iter().collect();
    sorted.sort_by_key(|c| c.span.start);
    let mut delta: isize = 0;
    let mut out = Vec::with_capacity(sorted.len());
    for c in sorted {
        let start = (c.span.start as usize).saturating_add_signed(delta);
        out.push((start..start + c.new.len(), c.old.clone()));
        delta += c.new.len() as isize - c.span.range().len() as isize;
    }
    out
}

/// "Renamed heading “A” to “B”."
fn done_title(plan: &AnchorPlan) -> String {
    if plan.is_block {
        format!("Renamed ^{} to ^{}.", plan.old_name, plan.new_name)
    } else {
        format!(
            "Renamed heading “{}” to “{}”.",
            plan.old_name, plan.new_name
        )
    }
}

/// "Updated 3 links in 2 notes." (other files), `None` without links.
fn links_detail(links: usize, files: usize) -> Option<String> {
    match (links, files) {
        (0, _) => None,
        (1, _) => Some("Updated 1 link.".to_owned()),
        (n, 0 | 1) => Some(format!("Updated {n} links.")),
        (n, m) => Some(format!("Updated {n} links in {m} other files.")),
    }
}

/// Where the refactor journal lives (the app's data folder).
fn journal_dir(cx: &gpui::App) -> Option<PathBuf> {
    cx.try_global::<crate::settings::AppSettings>()
        .and_then(|s| s.locations())
        .map(|l| l.data_dir().join("refactor"))
}

/// The preview of `plan`: every file with each link as it is and as it
/// will be, the warnings and the links that cannot be updated.
fn preview_request(plan: AnchorPlan, editor: gpui::WeakEntity<EditorView>) -> PromptRequest {
    let title = if plan.is_block {
        format!("Rename ^{} to ^{}?", plan.old_name, plan.new_name)
    } else {
        format!("Rename heading “{}” to “{}”?", plan.old_name, plan.new_name)
    };
    let links = plan.link_count();
    let files = plan.other_files();
    let message = match (links, files) {
        (0, _) => "No links change.".to_owned(),
        (1, _) => "1 link will be updated.".to_owned(),
        (n, 0) => format!("{n} links in this note will be updated."),
        (n, 1) => format!("{n} links will be updated, in this note and 1 other file."),
        (n, m) => format!("{n} links will be updated, in this note and {m} other files."),
    };
    let mut rows = Vec::new();
    for warning in &plan.others.warnings {
        rows.push(PromptRow::new(warning.clone(), RowStyle::Warning));
    }
    for skipped in &plan.others.skipped {
        let text = if skipped.link.is_empty() {
            format!("{} {}", skipped.source.as_str(), skipped.reason)
        } else {
            format!(
                "{}:{} {} not updated: {}",
                skipped.source.as_str(),
                skipped.line + 1,
                skipped.link,
                skipped.reason
            )
        };
        rows.push(PromptRow::new(text, RowStyle::Warning));
    }
    for edit in std::iter::once(&plan.own).chain(&plan.others.edits) {
        rows.push(PromptRow::new(
            format!("{} ({})", edit.path.as_str(), edit.changes.len()),
            RowStyle::Heading,
        ));
        for change in &edit.changes {
            rows.push(PromptRow::new(
                format!("{}: {}", change.line + 1, change.old),
                RowStyle::Removed,
            ));
            rows.push(PromptRow::new(
                format!("{}: {}", change.line + 1, change.new),
                RowStyle::Added,
            ));
        }
    }
    PromptRequest {
        title: title.into(),
        message: Some(message.into()),
        field: None,
        rows,
        action: "rename".into(),
        validate: None,
        on_confirm: Rc::new(move |_, _, cx| {
            let plan = plan.clone();
            let _ = editor.update(cx, |e, cx| e.apply_anchor_plan(plan, cx));
        }),
    }
}

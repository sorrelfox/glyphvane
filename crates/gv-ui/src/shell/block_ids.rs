// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! New block ids in other notes, asked by autocomplete
//! ([`DocEvent::BlockIdRequested`](crate::editor::DocEvent)): a block
//! link was written to a block of another note that had no id.
//!
//! - The note is open in a tab of this window: its buffer gets the id, one
//!   undo step, and is saved when it had no unsaved changes before.
//! - It has unsaved changes in another window: refused (that buffer is
//!   the note's text and cannot be edited from here).
//! - Else its file gets the id through the refactor journal
//!   ([`gv_vault::refactor::apply`]), off the UI thread: refused when the
//!   file is not as it was read (content hash), with nothing written.
//!
//! A refusal goes back to the Source view that asked, which takes the
//! link back out and says why in its infobar.

use gpui::{AppContext as _, Context, Entity};

use super::Workspace;
use super::vault::refactor_journal_dir;
use crate::editor::EditorView;

impl Workspace {
    /// Adds the block id `source` asks for to its note.
    pub(super) fn add_block_id(&mut self, source: &Entity<EditorView>, cx: &mut Context<'_, Self>) {
        let Some(request) = source.update(cx, |e, _| e.take_block_id_request()) else {
            return;
        };
        let id = request.id.clone();
        let name = request.name();
        let report =
            move |source: &Entity<EditorView>, result: Result<(), String>, cx: &mut gpui::App| {
                source.update(cx, |e, cx| e.block_id_done(&id, &name, result, cx));
            };
        let open = self.tabs.iter().position(|t| {
            t.path.as_deref() == Some(request.file.as_path())
                && t.editor
                    .as_ref()
                    .is_some_and(|e| e != source && e.read(cx).editor().is_some())
        });
        if let Some(index) = open {
            let result = if self.can_edit(index, cx) {
                let editor = self.tabs[index].editor.clone().expect("checked");
                editor.update(cx, |e, cx| e.add_block_id(&request, cx))
            } else {
                Err("it is open read-only".to_owned())
            };
            report(source, result, cx);
            return;
        }
        if crate::files::overlay::unsaved(&request.file).is_some() {
            report(
                source,
                Err("it has unsaved changes in another window: save it first".to_owned()),
                cx,
            );
            return;
        }
        let journal = refactor_journal_dir(cx);
        let task = cx.background_spawn(async move {
            crate::autocomplete::write_block_id(&request, journal.as_deref())
        });
        let source = source.downgrade();
        cx.spawn(async move |_, cx| {
            let result = task.await;
            cx.update(|cx| {
                if let Some(source) = source.upgrade() {
                    report(&source, result, cx);
                }
            });
        })
        .detach();
    }
}

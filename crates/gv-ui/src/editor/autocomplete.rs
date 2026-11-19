// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view's side of autocomplete ([`crate::autocomplete`]): when
//! the popover opens and closes, the vault it lists from, its keys, the
//! accepted edit, and the new block ids it asks the window to add to
//! other notes.

use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Div, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Stateful, Task, Window, anchored, deferred,
};
use gv_editor::EditKind;
use gv_syntax::LeafKind;
use gv_text::Revision;
use gv_vault::content::{ContentSnapshot, NoteData};
use gv_vault::{LinkKind, LinkRef, PathIndex, ProfileParams, VaultPath, resolve};

use super::DocEvent;
use super::markdown::current_syntax;
use super::{Cancel, EditorView, MoveDown, MoveUp};
use crate::appearance::Appearance;
use crate::autocomplete::{
    self, AcceptEnv, Block, BlockIdRequest, BlockJob, Item, Kind, LinkStyle, Linker, Pick, Scope,
    Sources, Trigger,
};
use crate::files::notice::Notice;
use crate::keymap::actions::editor as commands;
use crate::settings::AppSettings;

/// The document's index record, kept while its text stays the same.
#[derive(Debug, Default)]
pub(super) struct CurrentNote {
    at: Option<Revision>,
    data: Arc<NoteData>,
}

/// Reading other notes' blocks, and the new block ids asked of the
/// window ([`crate::autocomplete`]'s blocks).
#[derive(Default)]
pub(super) struct BlockIds {
    /// The read running.
    task: Option<Task<()>>,
    /// A new id for another note, until the window takes it.
    request: Option<BlockIdRequest>,
    /// The last id asked for, and what takes its link back out.
    sent: Option<Sent>,
}

struct Sent {
    id: String,
    /// The text right after the link was written.
    after: String,
    /// The edits that take the link back out (in `after`'s offsets).
    revert: Vec<(Range<usize>, String)>,
}

/// The vault as autocomplete sees it from this document.
pub(super) struct Vault {
    pub(super) root: Option<PathBuf>,
    pub(super) content: Option<ContentSnapshot>,
    pub(super) index: Option<PathIndex>,
    pub(super) source: Option<VaultPath>,
    pub(super) params: ProfileParams,
}

impl EditorView {
    /// The document's place in its vault: the content index of the vault
    /// that holds it, else the window's path index.
    pub(super) fn vault(&self) -> Vault {
        let profile = self.links.profile();
        let found = self
            .path
            .as_deref()
            .and_then(crate::files::overlay::content_of);
        let (root, content) = match found {
            Some((root, content)) => (Some(root), Some(content)),
            None => (None, None),
        };
        let index = content
            .as_ref()
            .and_then(|c| c.path_index().cloned())
            .or_else(|| self.autocomplete.index.clone());
        let root = root.or_else(|| index.as_ref().map(|i| i.root().to_path_buf()));
        let source = match (self.path.as_deref(), root.as_deref()) {
            (Some(path), Some(root)) => path
                .strip_prefix(root)
                .ok()
                .and_then(|p| VaultPath::from_relative_fs_path(p).ok()),
            _ => None,
        };
        let params = content
            .as_ref()
            .map(|c| c.options().params.clone())
            .filter(|p| p.profile == profile)
            .unwrap_or_else(|| ProfileParams::for_profile(profile));
        Vault {
            root,
            content,
            index,
            source,
            params,
        }
    }

    /// The document's headings, block ids, tags, footnotes and properties.
    pub(super) fn current_note(&mut self) -> Arc<NoteData> {
        let profile = self.links.profile();
        let Some(doc) = self.doc.as_ref() else {
            return Arc::default();
        };
        let text = doc.editor.text();
        if self.note_record.at != Some(text.revision()) {
            let string = text.text_for_range(0..text.len()).unwrap_or_default();
            self.note_record = CurrentNote {
                at: Some(text.revision()),
                data: Arc::new(gv_vault::content::extract(&string, profile)),
            };
        }
        self.note_record.data.clone()
    }

    /// The block the caret's line `line` is in.
    fn block_at(&self, line: usize, range: std::ops::Range<usize>) -> Block {
        let Some(doc) = self.doc.as_ref() else {
            return Block::Prose;
        };
        let Some(syntax) = current_syntax(doc) else {
            return Block::Prose;
        };
        let text = doc.editor.text();
        let range = if range.is_empty() {
            range.start..(range.start + 1).min(text.len())
        } else {
            range
        };
        let mut block = Block::Prose;
        let mut front: Option<(usize, usize)> = None;
        syntax.tree.walk_range(
            range,
            &mut autocomplete::Leaves(|leaf: &gv_syntax::Leaf<'_>| {
                if line < leaf.start_line || line > leaf.end_line {
                    return;
                }
                block = match leaf.kind {
                    LeafKind::FencedCode if line == leaf.start_line => Block::FenceOpen,
                    LeafKind::FencedCode
                    | LeafKind::IndentedCode
                    | LeafKind::Html { .. }
                    | LeafKind::MathBlock
                    | LeafKind::Comment => Block::Verbatim,
                    LeafKind::FrontMatter { .. } => {
                        if leaf.lines.iter().any(|l| l.line == line) {
                            front = Some((leaf.start_line, line));
                            Block::FrontMatter { list_key: None }
                        } else {
                            Block::Verbatim
                        }
                    }
                    _ => Block::Prose,
                };
            }),
        );
        if let (Block::FrontMatter { .. }, Some((first, line))) = (&block, front) {
            return Block::FrontMatter {
                list_key: list_key(text, first, line),
            };
        }
        block
    }

    /// The trigger at the caret, if any.
    fn find_trigger(&self) -> Option<Trigger> {
        if self.read_only {
            return None;
        }
        let editor = &self.doc.as_ref()?.editor;
        if editor.is_composing() || editor.selections().len() != 1 {
            return None;
        }
        let selection = editor.selections().primary();
        if !selection.is_empty() {
            return None;
        }
        let caret = selection.head();
        let text = editor.text();
        let line = text.line_of_offset(caret).ok()?;
        let start = text.line_start(line).ok()?;
        let end = line_end(text, line);
        let before = text.text_for_range(start..caret).ok()?;
        let after = text.text_for_range(caret..end.max(caret)).ok()?;
        let block = self.block_at(line, start..end);
        autocomplete::detect(&before, &after, &block, self.autocomplete.features)
            .map(|t| t.shifted(start))
    }

    /// After every change: typing opens the popover where a trigger is;
    /// other changes keep an open one in step, or close it.
    pub(super) fn update_autocomplete(&mut self, cx: &mut Context<'_, Self>) {
        let typed = std::mem::take(&mut self.autocomplete.typed);
        if self.plain.is_some() {
            return;
        }
        if !typed && !self.autocomplete.is_open() && !self.autocomplete.has_dismissed() {
            return;
        }
        let Some(trigger) = self.find_trigger() else {
            self.autocomplete.clear();
            return;
        };
        if self.autocomplete.is_dismissed(trigger.start) {
            self.autocomplete.close();
            return;
        }
        let same = self
            .autocomplete
            .open()
            .is_some_and(|o| o.trigger.start == trigger.start);
        if !typed && !same {
            self.autocomplete.close();
            return;
        }
        let (items, job) = self.completion_items(&trigger);
        self.autocomplete.show(trigger, items);
        if let Some(job) = job {
            self.read_blocks(job, cx);
        }
    }

    /// Reads other notes' blocks off the UI thread, then lists them when
    /// a block link is still being typed.
    fn read_blocks(&mut self, job: BlockJob, cx: &mut Context<'_, Self>) {
        let task = cx.background_spawn(async move { job.run() });
        self.block_ids.task = Some(cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |view, cx| {
                view.autocomplete.blocks.finish(done);
                let typing_blocks = view
                    .find_trigger()
                    .is_some_and(|t| matches!(t.kind, Kind::WikiBlock(_)));
                if typing_blocks {
                    view.autocomplete.typed = true;
                    view.update_autocomplete(cx);
                    cx.notify();
                }
            });
        }));
    }

    /// The items of `trigger`, and the read of other notes' blocks it
    /// needs.
    fn completion_items(&mut self, trigger: &Trigger) -> (Vec<Item>, Option<BlockJob>) {
        let Some(doc) = self.doc.as_ref() else {
            return (Vec::new(), None);
        };
        let text = doc.editor.text();
        let query = text
            .text_for_range(trigger.query.clone())
            .unwrap_or_default();
        let wants_paragraphs = matches!(
            trigger.kind,
            Kind::WikiBlock(Scope::Current | Scope::Note(_))
        );
        let blocks = if wants_paragraphs {
            let caret = trigger.query.end;
            let mut found = current_syntax(doc)
                .map(|s| autocomplete::blocks(s.tree, s.text))
                .unwrap_or_default();
            // Not the block the link is being typed in.
            found.retain(|b| !(b.start..=b.end).contains(&caret));
            found
        } else {
            Vec::new()
        };
        let current = if matches!(
            trigger.kind,
            Kind::WikiFile | Kind::LinkPath | Kind::Emoji | Kind::Fence | Kind::Callout
        ) {
            Arc::default()
        } else {
            self.current_note()
        };
        let vault = self.vault();
        let files = match (&trigger.kind, vault.root.as_deref()) {
            (Kind::WikiFile | Kind::LinkPath, Some(root)) => {
                self.autocomplete
                    .files(root, vault.content.as_ref(), vault.index.as_ref())
            }
            _ => Arc::default(),
        };
        let sections = match (&trigger.kind, vault.root.as_deref(), vault.content.as_ref()) {
            (Kind::WikiHeading(Scope::Vault), Some(root), Some(content)) => {
                self.autocomplete.sections(root, content, false)
            }
            (Kind::WikiBlock(Scope::Vault), Some(root), Some(content)) => {
                self.autocomplete.sections(root, content, true)
            }
            _ => Arc::default(),
        };
        let profile = self.links.profile();
        let mut job = None;
        let note_blocks = match (&trigger.kind, vault.root.as_deref(), vault.content.as_ref()) {
            (Kind::WikiBlock(Scope::Note(name)), Some(root), Some(content)) => {
                let target = match (vault.index.as_ref(), vault.source.as_ref()) {
                    (Some(index), Some(source)) => {
                        let link = LinkRef::new(LinkKind::Wikilink, name, source.clone(), profile);
                        resolve(index, &link, &vault.params)
                            .target_path
                            .filter(|p| p != source)
                    }
                    _ => None,
                };
                target.and_then(|path| {
                    let (blocks, read) =
                        self.autocomplete.blocks.note(root, content, &path, profile);
                    job = read;
                    blocks
                })
            }
            _ => None,
        };
        let vault_blocks = match (&trigger.kind, vault.root.as_deref(), vault.content.as_ref()) {
            (Kind::WikiBlock(Scope::Vault), Some(root), Some(content)) => {
                let (blocks, read) = self.autocomplete.blocks.vault(root, content, profile);
                job = read;
                blocks
            }
            _ => None,
        };
        let sources = Sources {
            content: vault.content.as_ref(),
            root: vault.root.as_deref(),
            index: vault.index.as_ref(),
            files: &files,
            sections: &sections,
            source: vault.source.as_ref(),
            params: &vault.params,
            profile: self.links.profile(),
            current: &current,
            blocks: &blocks,
            note_blocks: note_blocks.as_deref(),
            vault_blocks: vault_blocks.as_deref(),
            callouts: self.autocomplete.callouts,
        };
        (autocomplete::items(trigger, &query, &sources), job)
    }

    /// Accepts row `index` of the open popover: one transaction, one undo
    /// step.
    pub(super) fn accept_completion(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(open) = self.autocomplete.take() else {
            return;
        };
        let Some(item) = open.items.get(index) else {
            return;
        };
        let profile = self.links.profile();
        let vault = self.vault();
        let settings = cx
            .try_global::<AppSettings>()
            .map(|s| s.settings().links.clone());
        let style = LinkStyle::for_vault(vault.root.as_deref(), settings.as_ref());
        // A block of another note: its id is new there.
        let target = match &item.pick {
            Pick::Block {
                file: Some(file),
                id: None,
                target: Some(target),
                ..
            } => Some((file.clone(), target.clone())),
            _ => None,
        };
        let taken: Vec<String> = match &target {
            Some((_, target)) => target.taken.to_vec(),
            None => self
                .current_note()
                .block_ids
                .iter()
                .map(|b| b.id.to_string())
                .collect(),
        };
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let text = doc.editor.text();
        let len = text.len();
        let ends_with_newline = len > 0
            && text
                .text_for_range(len - 1..len)
                .is_ok_and(|s| s == "\n" || s == "\r");
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
            ^ (len as u64).rotate_left(17);
        let linker = vault.source.as_ref().map(|source| Linker {
            index: vault.index.as_ref(),
            params: &vault.params,
            source,
            profile,
            style,
        });
        let env = AcceptEnv {
            linker,
            profile,
            len,
            ends_with_newline,
            new_block_id: autocomplete::new_block_id(seed, &taken),
        };
        let completion = autocomplete::complete(&open.trigger, item, &env);
        let request = match (target, vault.root) {
            (Some((note, block)), Some(root)) => Some(BlockIdRequest {
                file: note.to_fs_path(&root),
                root,
                note,
                block,
                id: env.new_block_id.clone(),
                profile,
            }),
            _ => None,
        };
        let revert = request
            .as_ref()
            .map(|_| revert_edits(text, &completion.edits));
        let mut done = false;
        self.with_editor(window, cx, |editor, _| {
            match editor.edit(completion.edits, EditKind::Command) {
                Ok(_) => {
                    editor.set_cursor(completion.caret);
                    done = true;
                }
                Err(err) => tracing::warn!("cannot insert the completion: {err:?}"),
            }
        });
        if let (true, Some(request), Some(revert)) = (done, request, revert) {
            self.block_ids.sent = Some(Sent {
                id: request.id.clone(),
                after: self.whole_text().unwrap_or_default(),
                revert,
            });
            self.block_ids.request = Some(request);
            cx.emit(DocEvent::BlockIdRequested);
        }
    }

    /// The document's text.
    fn whole_text(&self) -> Option<String> {
        let text = self.doc.as_ref()?.editor.text();
        text.text_for_range(0..text.len()).ok()
    }

    /// The new block id the window should add to another note
    /// ([`DocEvent::BlockIdRequested`]).
    pub(crate) fn take_block_id_request(&mut self) -> Option<BlockIdRequest> {
        self.block_ids.request.take()
    }

    /// Adds the id of `request` to this document, where Obsidian puts it,
    /// as one undo step; saved when the document had no unsaved changes.
    pub(crate) fn add_block_id(
        &mut self,
        request: &BlockIdRequest,
        cx: &mut Context<'_, Self>,
    ) -> Result<(), String> {
        if self.read_only {
            return Err("it is read-only".to_owned());
        }
        let text = self.whole_text().ok_or("it is not loaded")?;
        let (at, insert) =
            autocomplete::insertion(&text, request.profile, &request.block, &request.id)?;
        let Some(doc) = self.doc.as_mut() else {
            return Err("it is not loaded".to_owned());
        };
        let was_modified = doc.editor.is_modified();
        match doc.editor.edit([(at..at, insert)], EditKind::Command) {
            Ok(Some(_)) => {}
            _ => return Err("it could not be edited".to_owned()),
        }
        self.input_changed(cx);
        if !was_modified {
            self.save(cx);
        }
        Ok(())
    }

    /// The window added the id `id` to its note, or could not: then the
    /// link goes back out (when the text is still as it left it) and the
    /// infobar says why.
    pub(crate) fn block_id_done(
        &mut self,
        id: &str,
        note: &str,
        result: Result<(), String>,
        cx: &mut Context<'_, Self>,
    ) {
        let sent = self.block_ids.sent.take_if(|s| s.id == id);
        let Err(reason) = result else {
            return;
        };
        if let Some(sent) = sent
            && self.whole_text().as_deref() == Some(sent.after.as_str())
            && let Some(doc) = self.doc.as_mut()
        {
            if let Err(err) = doc.editor.edit(sent.revert, EditKind::Command) {
                tracing::warn!("cannot take the block link back: {err:?}");
            }
            self.input_changed(cx);
        }
        self.show_doc_notice(
            Some(Notice::file_op_failed(
                "Block ID not added.",
                &format!("“{note}”: {reason}"),
            )),
            cx,
        );
    }

    /// While the popover is open: its keys, taken before the editor's
    /// (Up, Down, Enter, Tab, Esc), and the `Autocomplete` key context.
    pub(super) fn autocomplete_handlers(
        &self,
        host: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        if !self.autocomplete.is_open() {
            return host.key_context(super::KEY_CONTEXT);
        }
        host.key_context(format!("{} Autocomplete", super::KEY_CONTEXT).as_str())
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.autocomplete.step(false);
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.autocomplete.step(true);
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &Cancel, _, cx| {
                this.autocomplete.dismiss();
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &commands::Newline, window, cx| {
                cx.stop_propagation();
                this.accept_selected(window, cx);
            }))
            .capture_action(cx.listener(|this, _: &commands::Indent, window, cx| {
                cx.stop_propagation();
                this.accept_selected(window, cx);
            }))
    }

    fn accept_selected(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(selected) = self.autocomplete.open().map(|o| o.selected) {
            self.accept_completion(selected, window, cx);
        }
    }

    /// The popover, next to the typed text, when it is open and that text
    /// is on screen.
    pub(super) fn autocomplete_popover(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if !self.focus.contains_focused(window, cx) {
            // The keyboard went elsewhere.
            self.autocomplete.close();
            return None;
        }
        let open = self.autocomplete.open()?.clone();
        let at = open.trigger.query.start;
        let anchor = self.range_bounds(at..at, None, window, cx)?;
        let bounds = self.bounds?;
        if anchor.top() < bounds.top() || anchor.bottom() > bounds.bottom() {
            return None;
        }
        let size = autocomplete::panel_size(open.items.len());
        let origin = autocomplete::place(anchor, size, window.viewport_size());
        self.autocomplete_placed = Some(gpui::Bounds::new(origin, size));
        let this = cx.entity().downgrade();
        let click: autocomplete::RowClick = Rc::new(move |n, window, cx: &mut App| {
            this.update(cx, |view, cx| view.accept_completion(n, window, cx))
                .ok();
        });
        let this = cx.entity().downgrade();
        let wheel: autocomplete::RowWheel = Rc::new(move |rows, _, cx: &mut App| {
            this.update(cx, |view, cx| {
                if view.autocomplete.scroll(rows) {
                    cx.notify();
                }
            })
            .ok();
        });
        let theme = Appearance::theme(cx);
        let panel =
            autocomplete::panel(&open, Appearance::interface_font(cx), theme, &click, &wheel);
        Some(
            deferred(anchored().position(origin).child(panel))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// The labels of the open popover's rows, the highlighted one, and
    /// where it was last placed.
    #[cfg(test)]
    pub(crate) fn autocomplete_rows(&self) -> Option<(Vec<String>, usize)> {
        let open = self.autocomplete.open()?;
        Some((
            open.items.iter().map(|i| i.label.clone()).collect(),
            open.selected,
        ))
    }
}

/// The edits that undo `edits` (sorted, in `text`'s offsets), in the
/// offsets of the text after them.
fn revert_edits(
    text: &gv_text::TextSnapshot,
    edits: &[(Range<usize>, String)],
) -> Vec<(Range<usize>, String)> {
    let mut shift = 0isize;
    edits
        .iter()
        .map(|(range, new)| {
            let start = range.start.saturating_add_signed(shift);
            shift += new.len() as isize - range.len() as isize;
            let old = text.text_for_range(range.clone()).unwrap_or_default();
            (start..start + new.len(), old)
        })
        .collect()
}

/// The end of `line`'s text, before its line break.
fn line_end(text: &gv_text::TextSnapshot, line: usize) -> usize {
    let end = if line + 1 < text.line_count() {
        text.line_start(line + 1).unwrap_or(text.len())
    } else {
        text.len()
    };
    let start = text.line_start(line).unwrap_or(0);
    let content = text.text_for_range(start..end).unwrap_or_default();
    start + content.trim_end_matches(['\n', '\r']).len()
}

/// The key whose block list the front matter line `line` continues: the
/// nearest `key:` line above with no value, over `- item` lines only.
fn list_key(text: &gv_text::TextSnapshot, first: usize, line: usize) -> Option<String> {
    let mut l = line;
    while l > first {
        l -= 1;
        let start = text.line_start(l).ok()?;
        let content = text.text_for_range(start..line_end(text, l)).ok()?;
        let trimmed = content.trim_start();
        if trimmed.starts_with('-') {
            continue;
        }
        if content.starts_with([' ', '\t']) {
            return None;
        }
        let key = content.trim_end().strip_suffix(':')?;
        return (!key.is_empty() && !key.contains(':')).then(|| key.to_owned());
    }
    None
}

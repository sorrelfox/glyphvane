// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view's side of autocomplete ([`crate::autocomplete`]): when
//! the popover opens and closes, the vault it lists from, its keys, and
//! the accepted edit.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, Div, InteractiveElement as _, IntoElement as _, ParentElement as _,
    Stateful, Window, anchored, deferred,
};
use gv_editor::EditKind;
use gv_syntax::LeafKind;
use gv_text::Revision;
use gv_vault::content::{ContentSnapshot, NoteData};
use gv_vault::{PathIndex, ProfileParams, VaultPath};

use super::markdown::current_syntax;
use super::{Cancel, EditorView, MoveDown, MoveUp};
use crate::appearance::Appearance;
use crate::autocomplete::{
    self, AcceptEnv, Block, Item, Kind, LinkStyle, Linker, Scope, Sources, Trigger,
};
use crate::keymap::actions::editor as commands;
use crate::settings::AppSettings;

/// The document's index record, kept while its text stays the same.
#[derive(Debug, Default)]
pub(super) struct CurrentNote {
    at: Option<Revision>,
    data: Arc<NoteData>,
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
    fn current_note(&mut self) -> Arc<NoteData> {
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
    pub(super) fn update_autocomplete(&mut self) {
        let typed = std::mem::take(&mut self.autocomplete.typed);
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
        let items = self.completion_items(&trigger);
        self.autocomplete.show(trigger, items);
    }

    /// The items of `trigger`.
    fn completion_items(&mut self, trigger: &Trigger) -> Vec<Item> {
        let Some(doc) = self.doc.as_ref() else {
            return Vec::new();
        };
        let text = doc.editor.text();
        let query = text
            .text_for_range(trigger.query.clone())
            .unwrap_or_default();
        let wants_paragraphs = matches!(
            trigger.kind,
            Kind::WikiBlock(Scope::Current | Scope::Note(_))
        );
        let paragraphs = if wants_paragraphs {
            let caret = trigger.query.end;
            let mut found = current_syntax(doc)
                .map(|s| autocomplete::paragraphs(s.tree, s.text))
                .unwrap_or_default();
            // Not the paragraph the link is being typed in.
            found.retain(|p| !(p.start..=p.end).contains(&caret));
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
        let sources = Sources {
            content: vault.content.as_ref(),
            root: vault.root.as_deref(),
            index: vault.index.as_ref(),
            files: &files,
            source: vault.source.as_ref(),
            params: &vault.params,
            profile: self.links.profile(),
            current: &current,
            paragraphs: &paragraphs,
            callouts: self.autocomplete.callouts,
        };
        autocomplete::items(trigger, &query, &sources)
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
        let current = self.current_note();
        let taken: Vec<_> = current.block_ids.iter().map(|b| b.id.clone()).collect();
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
        self.with_editor(window, cx, |editor, _| {
            match editor.edit(completion.edits, EditKind::Command) {
                Ok(_) => editor.set_cursor(completion.caret),
                Err(err) => tracing::warn!("cannot insert the completion: {err:?}"),
            }
        });
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
        let theme = Appearance::theme(cx);
        let panel = autocomplete::panel(&open, Appearance::interface_font(cx), theme, &click);
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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Spell check in the Source view (07 § Spell check).
//!
//! - **When:** after every reparse that catches up with the text, a check
//!   runs on the background executor ([`gv_spell::check_document`]). Its
//!   block cache stays with the view, so after an edit only the changed
//!   blocks are checked again. A document over [`DEBOUNCE_SIZE`] waits for
//!   edits to pause first; the first check of one checks the blocks on
//!   screen before the rest. Large-file mode turns spell check off.
//! - **Drawing:** misspellings on screen get a wavy `status.error`
//!   underline, and a tick on the scrollbar. Until the check of the
//!   current text lands, the last results are drawn where their word is
//!   still at its place; a word whose end has an empty caret (the word
//!   being typed) is not marked.
//! - **Context menu:** opened on a misspelled word (right-click, or the
//!   menu key with the caret in it), the editor's menu starts with the
//!   word's suggestions: the fast pass at once (under 1 ms), the full list
//!   from the background when it arrives. *Add to dictionary*, *Add to
//!   vault dictionary* and *Ignore* act on that word ([`crate::spell`]).
//! - **Next misspelling** (Go menu, palette) selects the next misspelled
//!   word after the selection, wrapping around.

use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{
    AppContext as _, Context, Div, InteractiveElement as _, Pixels, Point, SharedString, Stateful,
    Subscription, Task, Window,
};
use gv_editor::{EditKind, Editor};
use gv_spell::{BlockCache, Checker, DocumentCheck, Misspelling, SuggestDepth};
use gv_text::{Revision, TextSnapshot};

use super::EditorView;
use crate::keymap::actions::{navigation, spell as commands};
use crate::menu::{ContextMenu, MAX_SUGGESTIONS, SpellMenu, context_menu_of};
use crate::settings::AppSettings;
use crate::spell::{SpellGlobal, SpellService};

/// Documents larger than this are checked once edits pause.
pub(super) const DEBOUNCE_SIZE: usize = 256 * 1024;

/// How long a large document waits for edits to pause.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Bytes after the top of the view checked first in a large document.
const FIRST_SCREEN: usize = 32 * 1024;

/// The Source view's spell-check state.
#[derive(Default)]
pub(super) struct SpellState {
    /// The misspellings found, in order.
    found: Arc<[Misspelling]>,
    /// What `found` is of.
    checked: Option<Key>,
    /// The check running or waiting.
    scheduled: Option<Key>,
    cache: Arc<Mutex<BlockCache>>,
    /// The checker of the last check, for suggestions.
    checker: Option<Arc<Checker>>,
    /// The misspelling the context menu was opened on.
    target: Option<Misspelling>,
    _check: Option<Task<()>>,
    _suggest: Option<Task<()>>,
    /// Follows the word lists and the settings.
    observing: Option<(Subscription, Subscription)>,
}

impl std::fmt::Debug for SpellState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpellState")
            .field("found", &self.found.len())
            .field("checked", &self.checked)
            .field("scheduled", &self.scheduled)
            .finish_non_exhaustive()
    }
}

impl SpellState {
    /// Forgets the document (a new one was loaded).
    pub fn reset(&mut self) {
        let observing = self.observing.take();
        *self = Self {
            observing,
            ..Self::default()
        };
    }

    /// The misspellings found by the last check.
    #[cfg(test)]
    pub fn found(&self) -> &[Misspelling] {
        &self.found
    }

    /// The text revision of the last check.
    pub fn checked_revision(&self) -> Option<Revision> {
        self.checked.map(|k| k.text)
    }

    /// The misspellings to draw in `visible`: still where they were found,
    /// and not the word being typed (an empty caret at its end).
    pub fn visible(&self, editor: &Editor, visible: Range<usize>) -> Vec<Range<usize>> {
        let text = editor.text();
        let current = self.checked_revision() == Some(text.revision());
        let found = &self.found;
        let from = found.partition_point(|m| m.range.end < visible.start);
        let carets: Vec<usize> = editor
            .selections()
            .iter()
            .filter(|s| s.is_empty())
            .map(|s| s.head())
            .collect();
        found[from..]
            .iter()
            .take_while(|m| m.range.start <= visible.end)
            .filter(|m| !carets.contains(&m.range.end))
            .filter(|m| current || still_there(text, m))
            .map(|m| m.range.clone())
            .collect()
    }

    /// The display rows with a misspelling, for the scrollbar's ticks.
    pub fn rows(&self, editor: &Editor) -> Vec<usize> {
        let text = editor.text();
        let map = super::wrap::RowMap::of(editor);
        let mut rows: Vec<usize> = Vec::new();
        let mut last_line = None;
        for m in self.found.iter() {
            let Ok(line) = text.line_of_offset(m.range.start.min(text.len())) else {
                continue;
            };
            if last_line == Some(line) {
                continue;
            }
            last_line = Some(line);
            rows.push(map.first_row(line));
        }
        rows
    }
}

/// What a check is of: the text revision, the spell service's revision
/// (words, settings) and the syntax tree's generation (a new profile
/// reparses the same text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    text: Revision,
    words: u64,
    syntax: u64,
}

/// Whether `m` is still the word it was in `text`.
fn still_there(text: &TextSnapshot, m: &Misspelling) -> bool {
    m.range.end <= text.len()
        && text
            .text_for_range(m.range.clone())
            .is_ok_and(|s| *s == *m.word)
}

impl EditorView {
    /// Checks the text again unless it was checked as it is.
    pub(super) fn schedule_spell(&mut self, cx: &mut Context<'_, Self>) {
        if self.spell.observing.is_none() {
            self.spell.observing = Some((
                cx.observe_global::<SpellGlobal>(|this, cx| this.schedule_spell(cx)),
                cx.observe_global::<AppSettings>(|this, cx| this.schedule_spell(cx)),
            ));
        }
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let setup = if doc.large_file {
            None
        } else {
            SpellService::setup(cx)
        };
        let Some(setup) = setup else {
            if !self.spell.found.is_empty() || self.spell.checked.is_some() {
                self.spell.reset();
                cx.notify();
            }
            return;
        };
        let text = doc.editor.snapshot();
        let revision = text.revision();
        let (tree, parsed) = doc.syntax.shown();
        if parsed.revision() != revision || tree.is_interim() {
            // The reparse that catches up schedules again.
            return;
        }
        let key = Key {
            text: revision,
            words: setup.revision,
            syntax: doc.syntax.generation(),
        };
        if self.spell.checked == Some(key) || self.spell.scheduled == Some(key) {
            return;
        }
        self.spell.scheduled = Some(key);
        let tree = tree.clone();
        let cache = Arc::clone(&self.spell.cache);
        let large = text.len() > DEBOUNCE_SIZE;
        let first = large && self.spell.checked.is_none();
        let top = self.top_offset().unwrap_or(0);
        // A new check replaces the one waiting; one already running on the
        // background executor finishes, and its result is dropped.
        self.spell._check = Some(cx.spawn(async move |this, cx| {
            if large && !first {
                cx.background_executor().timer(DEBOUNCE).await;
            }
            let checker = {
                let (tree, text) = (tree.clone(), text.clone());
                Arc::new(
                    cx.background_spawn(async move { setup.checker(&tree, &text) })
                        .await,
                )
            };
            if first {
                let (tree, text, checker, cache) = (
                    tree.clone(),
                    text.clone(),
                    Arc::clone(&checker),
                    Arc::clone(&cache),
                );
                let screen = top..(top + FIRST_SCREEN).min(text.len());
                let partial = cx
                    .background_spawn(async move {
                        let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
                        gv_spell::check_document(&tree, &text, &checker, &mut cache, Some(screen))
                    })
                    .await;
                let ok = this.update(cx, |view, cx| {
                    if view.spell.scheduled == Some(key) {
                        view.spell.found = partial.misspellings.into();
                        cx.notify();
                    }
                });
                if ok.is_err() {
                    return;
                }
            }
            let result = {
                let checker = Arc::clone(&checker);
                cx.background_spawn(async move {
                    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
                    gv_spell::check_document(&tree, &text, &checker, &mut cache, None)
                })
                .await
            };
            this.update(cx, |view, cx| view.spell_landed(key, checker, result, cx))
                .ok();
        }));
    }

    fn spell_landed(
        &mut self,
        key: Key,
        checker: Arc<Checker>,
        check: DocumentCheck,
        cx: &mut Context<'_, Self>,
    ) {
        if self.spell.scheduled != Some(key) {
            return;
        }
        self.spell.scheduled = None;
        self.spell.checked = Some(key);
        self.spell.found = check.misspellings.into();
        self.spell.checker = Some(checker);
        cx.notify();
    }

    /// The misspelling at `offset` (inside it or at either end).
    fn misspelling_at(&self, offset: usize) -> Option<Misspelling> {
        let doc = self.doc.as_ref()?;
        let text = doc.editor.text();
        let current = self.spell.checked_revision() == Some(text.revision());
        let found = &self.spell.found;
        let from = found.partition_point(|m| m.range.end < offset);
        found[from..]
            .iter()
            .take_while(|m| m.range.start <= offset)
            .find(|m| current || still_there(text, m))
            .cloned()
    }

    /// The word a spelling command acts on: the one the context menu was
    /// opened on, else the one at the primary caret.
    fn spell_subject(&mut self) -> Option<Misspelling> {
        if let Some(target) = self.spell.target.take()
            && self
                .doc
                .as_ref()
                .is_some_and(|d| still_there(d.editor.text(), &target))
        {
            return Some(target);
        }
        let head = self.doc.as_ref()?.editor.selections().primary().head();
        self.misspelling_at(head)
    }

    /// Forgets the context menu's word (the caret moved, the text changed).
    pub(super) fn spell_after_change(&mut self) {
        self.spell.target = None;
    }

    /// Makes the editor's open context menu about the misspelled word at
    /// `position` (a right-click), or at the primary caret (the menu key).
    pub(crate) fn attach_spell_menu(
        &mut self,
        position: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.spell.target = None;
        let offset = match position {
            Some(position) => self.offset_at(position, window, cx),
            None => self
                .doc
                .as_ref()
                .map(|d| d.editor.selections().primary().head()),
        };
        let Some(m) = offset.and_then(|o| self.misspelling_at(o)) else {
            return;
        };
        let Some(popover) = context_menu_of(window, cx) else {
            return;
        };
        if popover.read(cx).kind() != Some(ContextMenu::Editor) {
            return;
        }
        let word = m.word.to_string();
        let checker = self.spell.checker.clone();
        let fast: Vec<SharedString> = checker
            .as_ref()
            .map(|c| c.suggest(&word, SuggestDepth::Fast, MAX_SUGGESTIONS))
            .unwrap_or_default()
            .into_iter()
            .map(SharedString::from)
            .collect();
        let weak = cx.entity().downgrade();
        let range = m.range.clone();
        let replaced = word.clone();
        let menu = SpellMenu {
            word: word.clone().into(),
            suggestions: fast,
            in_vault: SpellService::in_vault(cx),
            read_only: self.read_only,
            apply: std::rc::Rc::new(move |with: &str, window: &mut Window, cx: &mut gpui::App| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |view, cx| {
                        view.replace_misspelling(range.clone(), &replaced, with, window, cx);
                    });
                }
            }),
        };
        popover.update(cx, |p, cx| p.set_spell(menu, window, cx));
        self.spell.target = Some(m);
        // The full list, off the UI thread.
        let Some(checker) = checker else {
            return;
        };
        let popover = popover.downgrade();
        self.spell._suggest = Some(cx.spawn(async move |_, cx| {
            let lookup = word.clone();
            let full = cx
                .background_spawn(async move {
                    checker.suggest(&lookup, SuggestDepth::Full, MAX_SUGGESTIONS)
                })
                .await;
            let full: Vec<SharedString> = full.into_iter().map(SharedString::from).collect();
            popover
                .update(cx, |p, cx| p.set_suggestions(&word, full, cx))
                .ok();
        }));
    }

    /// Replaces the misspelled `word` at `range` with `with`, if it is
    /// still there.
    fn replace_misspelling(
        &mut self,
        range: Range<usize>,
        word: &str,
        with: &str,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.read_only {
            return;
        }
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let m = Misspelling {
            range: range.clone(),
            word: word.into(),
        };
        if !still_there(doc.editor.text(), &m) {
            return;
        }
        let with = with.to_owned();
        self.with_editor(window, cx, |editor, _| {
            if let Err(err) = editor.edit([(range, with)], EditKind::Command) {
                tracing::warn!("spelling: {err}");
            }
        });
    }

    /// Adds the word to the user's (or the vault's) dictionary, or ignores
    /// it for the session.
    fn accept_word(&mut self, how: Accept, cx: &mut Context<'_, Self>) {
        let Some(m) = self.spell_subject() else {
            return;
        };
        match how {
            Accept::Add => SpellService::add_word(cx, &m.word, false),
            Accept::AddToVault => SpellService::add_word(cx, &m.word, true),
            Accept::Ignore => SpellService::ignore_word(cx, &m.word),
        }
        // At once, before the new check lands.
        let found: Vec<Misspelling> = self
            .spell
            .found
            .iter()
            .filter(|f| f.word != m.word)
            .cloned()
            .collect();
        self.spell.found = found.into();
        cx.notify();
    }

    /// Selects the next misspelled word after the primary selection,
    /// wrapping around.
    fn next_misspelling(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let text = doc.editor.text();
        let current = self.spell.checked_revision() == Some(text.revision());
        let after = doc.editor.selections().primary().end();
        let usable: Vec<&Misspelling> = self
            .spell
            .found
            .iter()
            .filter(|m| current || still_there(text, m))
            .collect();
        let next = usable
            .iter()
            .find(|m| m.range.start >= after)
            .or_else(|| usable.first());
        let Some(range) = next.map(|m| m.range.clone()) else {
            return;
        };
        self.with_editor(window, cx, |editor, _| editor.select_ranges([range]));
    }

    /// The spelling commands' handlers, while spell check is on.
    pub(super) fn spell_handlers(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let enabled = cx
            .try_global::<AppSettings>()
            .is_some_and(|s| s.settings().spelling.enabled);
        if self.doc.is_none() || !enabled {
            return root;
        }
        let root = root
            .on_action(
                cx.listener(|this, _: &navigation::NextMisspelling, window, cx| {
                    this.next_misspelling(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &commands::AddToDictionary, _, cx| {
                this.accept_word(Accept::Add, cx);
            }))
            .on_action(cx.listener(|this, _: &commands::Ignore, _, cx| {
                this.accept_word(Accept::Ignore, cx);
            }));
        if SpellService::in_vault(cx) {
            root.on_action(
                cx.listener(|this, _: &commands::AddToVaultDictionary, _, cx| {
                    this.accept_word(Accept::AddToVault, cx);
                }),
            )
        } else {
            root
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Accept {
    Add,
    AddToVault,
    Ignore,
}

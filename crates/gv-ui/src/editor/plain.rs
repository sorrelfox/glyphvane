// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Plain-text mode: the Source view of a text or code file that is not
//! Markdown (the built-in text viewer's Edit, plan 09 § Special targets).
//!
//! The file is loaded, journaled, saved and watched as a note is, byte for
//! byte (encoding, line endings and the last line ending as they were).
//! What only makes sense for Markdown is off:
//!
//! - **Highlighting:** no Markdown roles. A file whose name names a
//!   language the code highlighter knows is coloured by it as a whole (the
//!   shared highlighter, on the background executor, with its size cap);
//!   anything else is plain text.
//! - **Commands:** Enter inserts a line ending and the current line's
//!   indentation, Tab and Shift+Tab indent and outdent by the indent unit;
//!   the line commands (move, duplicate, sort, change case) stay. The
//!   formatting, heading, list, quote, link, table, footnote, comment and
//!   callout commands have no handler, so their menu items are disabled.
//! - **Off:** folding, autocomplete, smart pairs, smart paste (the
//!   clipboard is pasted as text; images are not saved as attachments),
//!   links (Mod+click, hover, the link menu), drops that write links, Go
//!   to heading and footnote, expand selection by syntax, rename heading,
//!   the hanging indent of list items, the conflict-marker check, and
//!   spell check for code (prose files such as `.txt` keep it).
//! - **Font:** code is shown in a monospace family even when the editor
//!   font set is proportional ([`super::metrics`]); prose files keep the
//!   editor font.

use std::path::Path;

use gpui::{Context, Div, InteractiveElement as _, Stateful, Window};
use gv_editor::commands::IndentUnit;
use gv_editor::{EditKind, MarkdownCommand};

use super::EditorView;
use crate::keymap::actions::editor as actions;

/// The plain-text mode of a view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Plain {
    /// The code highlighter's language for the file, if it has one.
    pub language: Option<String>,
    /// Whether misspellings are looked for (prose, not code).
    pub spell: bool,
    /// Whether the text is code, shown in a monospace family.
    pub mono: bool,
}

/// Spaces Shift+Tab takes away when the indent unit is a tab and a line
/// starts with spaces.
const TAB_SPACES: usize = 4;

/// Extensions of prose text files, which keep spell check.
const PROSE: &[&str] = &["txt", "text", "rst", "adoc", "asciidoc", "org"];

impl Plain {
    /// The mode for `path` (`None`: an untitled document).
    pub fn for_path(path: Option<&Path>) -> Self {
        let language = path.and_then(crate::file_viewer::language_info);
        let ext = path
            .and_then(Path::extension)
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let prose = match ext.as_deref() {
            None => true,
            Some(ext) => PROSE.contains(&ext),
        };
        Self {
            spell: language.is_none() && prose,
            language,
            mono: !prose,
        }
    }
}

/// Registers the handler of `command` (action `A`) as a plain-text edit.
fn on<A: gpui::Action>(
    root: Stateful<Div>,
    run: fn(&mut EditorView, &mut Window, &mut Context<'_, EditorView>),
    cx: &mut Context<'_, EditorView>,
) -> Stateful<Div> {
    root.on_action(cx.listener(move |this, _: &A, window, cx| run(this, window, cx)))
}

impl EditorView {
    /// Plain-text mode for a file that is not Markdown: no Markdown
    /// highlighting, commands, folds, links or completions.
    /// Set it before the document loads; the file's name picks the
    /// highlighting and whether spell check runs.
    pub fn set_plain(&mut self, plain: bool, cx: &mut Context<'_, Self>) {
        let mode = plain.then(|| Plain::for_path(self.path.as_deref()));
        if self.plain == mode {
            return;
        }
        self.plain = mode;
        self.availabilities.reset();
        self.autocomplete.clear();
        let language = self.plain.as_ref().map(|p| p.language.clone());
        if let Some(doc) = self.doc.as_mut() {
            doc.highlights = super::HighlightCache::default();
            doc.highlights.set_plain(language);
            doc.editor.set_wrap_indent(super::wrap_indent(plain));
        }
        self.live_settings_changed();
        self.sync_code_font(cx);
        self.schedule_spell(cx);
        cx.notify();
    }

    /// Whether the view edits a file that is not Markdown.
    pub fn is_plain(&self) -> bool {
        self.plain.is_some()
    }

    /// The plain-text mode follows the file opened.
    pub(super) fn plain_for_path(&mut self, cx: &mut Context<'_, Self>) {
        if self.plain.is_some() {
            self.plain = Some(Plain::for_path(self.path.as_deref()));
            self.sync_code_font(cx);
        }
    }

    /// Code in plain-text mode is shown in a monospace family.
    fn sync_code_font(&mut self, cx: &mut Context<'_, Self>) {
        let code = self.plain.as_ref().is_some_and(|p| p.mono);
        if self.metrics.code != code {
            let mut metrics = self.metrics.clone();
            metrics.code = code;
            self.set_metrics(metrics, cx);
        }
    }

    /// The highlighting mode for a new document: `None` for Markdown,
    /// else the code language, if any.
    pub(super) fn plain_language(&self) -> Option<Option<String>> {
        self.plain.as_ref().map(|p| p.language.clone())
    }

    /// The handlers of the commands that work in plain text: Enter,
    /// Shift+Enter, Tab, Shift+Tab and the line commands.
    pub(super) fn plain_handlers(
        &mut self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        if self.doc.is_none() || self.read_only {
            return root;
        }
        let root = on::<actions::Newline>(root, Self::plain_newline, cx);
        let root = on::<actions::NewlinePlain>(root, Self::plain_newline, cx);
        let root = on::<actions::Indent>(root, |v, w, cx| v.plain_indent(true, w, cx), cx);
        let root = on::<actions::Outdent>(root, |v, w, cx| v.plain_indent(false, w, cx), cx);
        let root = on::<actions::MoveLineUp>(
            root,
            |v, w, cx| v.run_markdown(MarkdownCommand::MoveLineUp, w, cx),
            cx,
        );
        let root = on::<actions::MoveLineDown>(
            root,
            |v, w, cx| v.run_markdown(MarkdownCommand::MoveLineDown, w, cx),
            cx,
        );
        let root = on::<actions::DuplicateLine>(
            root,
            |v, w, cx| v.run_markdown(MarkdownCommand::DuplicateLine, w, cx),
            cx,
        );
        let root = on::<actions::SortLines>(
            root,
            |v, w, cx| v.run_markdown(MarkdownCommand::SortLines, w, cx),
            cx,
        );
        on::<actions::ChangeCase>(
            root,
            |v, w, cx| v.run_markdown(MarkdownCommand::ChangeCase, w, cx),
            cx,
        )
    }

    /// Enter: the buffer's line ending and the leading whitespace of the
    /// caret's line (up to the caret), at every selection.
    fn plain_newline(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.editor.is_composing() {
            // The input method did not take Enter: commit as is.
            doc.editor.unmark_text();
            self.changed(window, cx);
            return;
        }
        let editor = &mut doc.editor;
        let ending = editor.buffer().format().line_ending.as_str().to_owned();
        let text = editor.text().clone();
        let texts: Vec<String> = editor
            .selections()
            .iter()
            .map(|selection| {
                let at = selection.start();
                let indent = text
                    .line_of_offset(at)
                    .and_then(|line| text.line_start(line))
                    .and_then(|start| text.text_for_range(start..at))
                    .map(|before| {
                        before
                            .chars()
                            .take_while(|c| *c == ' ' || *c == '\t')
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                format!("{ending}{indent}")
            })
            .collect();
        editor.insert_per_selection(&texts, EditKind::Typing);
        self.changed(window, cx);
    }

    /// Tab (`indent`) or Shift+Tab: with only carets, Tab inserts the
    /// indent unit; otherwise each line a selection covers gains or loses
    /// one unit at its start. One undo step.
    fn plain_indent(&mut self, indent: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        // What Tab inserts, and how many spaces Shift+Tab takes away where
        // a line does not start with a tab.
        let (unit, spaces) = match self.commands.settings.indent {
            IndentUnit::Tab => ("\t".to_owned(), TAB_SPACES),
            IndentUnit::Spaces(n) => {
                let n = usize::from(n.clamp(1, 16));
                (" ".repeat(n), n)
            }
        };
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let editor = &mut doc.editor;
        if editor.is_composing() {
            return;
        }
        let carets = editor.selections().iter().all(|s| s.start() == s.end());
        if indent && carets {
            editor.insert_at_selections(&unit);
            self.changed(window, cx);
            return;
        }
        let text = editor.text().clone();
        let mut lines: Vec<usize> = Vec::new();
        for selection in editor.selections().iter() {
            let (Ok(first), Ok(last)) = (
                text.line_of_offset(selection.start()),
                text.line_of_offset(selection.end()),
            ) else {
                continue;
            };
            // A selection ending at a line start leaves that line alone.
            let last = match text.line_start(last) {
                Ok(start) if last > first && start == selection.end() => last - 1,
                _ => last,
            };
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
        for line in lines {
            let Ok(range) = text.line_range(line) else {
                continue;
            };
            if indent {
                if !range.is_empty() {
                    edits.push((range.start..range.start, unit.clone()));
                }
                continue;
            }
            let head = text
                .text_for_range(range.start..(range.start + spaces).min(range.end))
                .unwrap_or_default();
            let remove = if head.starts_with('\t') {
                1
            } else {
                head.chars().take_while(|c| *c == ' ').count()
            };
            if remove > 0 {
                edits.push((range.start..range.start + remove, String::new()));
            }
        }
        if edits.is_empty() {
            return;
        }
        let _ = editor.edit(edits, EditKind::Command);
        self.changed(window, cx);
    }
}

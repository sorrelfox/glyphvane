// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Markdown commands in the Source view: Enter, Shift+Enter, Tab,
//! Shift+Tab, inline formatting, headings, quotes, links, task boxes,
//! renumbering, the line commands (move, duplicate, join, sort, change
//! case) and toggle comment ([`gv_editor::commands`]).
//!
//! - **Dispatch.** Each command's keymap action runs
//!   [`MarkdownCommands::run`] on every selection with the view's current
//!   syntax tree. A tree that is not the complete parse of the text (an
//!   interim tree while a long reparse finishes in the background) is
//!   replaced by the command's own parse. A command is one transaction and
//!   one undo step.
//! - **Availability.** The view registers a command's handler only while
//!   [`MarkdownCommands::availability`] allows it (not
//!   [`Availability::NotInFlavor`] for the tab's profile, not
//!   [`Availability::NotHere`] at every caret). Without a handler GPUI
//!   disables the command's menu item and its keys fall through to other
//!   bindings and to text input. The availabilities are computed once per
//!   text revision, selection state and tree; while the tree is interim
//!   only the profile is checked, and the handler checks again at the
//!   carets before it runs.
//! - **Input methods.** No command runs while an input method composes.
//!   Enter and Shift+Enter commit the composition as it is (the platform's
//!   input method normally takes them first); the other commands do
//!   nothing.
//! - **Backspace and typing.** Backspace runs
//!   [`MarkdownCommands::backspace`] first (task box, list marker, item
//!   indentation, `> `) and deletes as usual where it does not apply; one
//!   typed character goes through [`MarkdownCommands::type_text`] (smart
//!   pairs) before plain insertion. Neither applies while composing.
//! - **Insert link** reads the clipboard: an absolute URL there becomes the
//!   link's destination.
//! - **Viewer mode** ([`EditorView::set_read_only`]): no command has a
//!   handler.

use std::ops::Range;

use gpui::{Action, Context, Div, InteractiveElement as _, Stateful, Window};
use gv_editor::{
    Availability, DeleteUnit, Editor, MarkdownCommand, MarkdownCommands, Outcome, SyntaxRef,
};
use gv_text::Revision;

use super::{Doc, EditorView};
use crate::keymap::actions::editor as actions;

/// The availability of every command, and the state it was computed for.
#[derive(Debug, Default)]
pub(super) struct Availabilities {
    /// Text revision, selections and syntax generation.
    key: Option<(Revision, gv_editor::Selections, u64)>,
    available: Vec<(MarkdownCommand, Availability)>,
}

impl Availabilities {
    /// Forgets the state (a new document or profile).
    pub fn reset(&mut self) {
        self.key = None;
        self.available.clear();
    }

    /// Brings the availabilities up to date with `doc`.
    fn update(&mut self, commands: &MarkdownCommands, doc: &Doc) {
        let editor = &doc.editor;
        let key = (
            editor.text().revision(),
            editor.selections().clone(),
            doc.syntax.generation(),
        );
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let syntax = current_syntax(doc);
        self.available = match syntax {
            Some(syntax) => commands.availabilities(editor, Some(syntax)),
            // Parsing here would cost a full parse per frame while the
            // background reparse runs; the handler checks the carets.
            None => MarkdownCommand::ALL
                .into_iter()
                .map(|command| {
                    let availability = if commands.in_flavor(command) {
                        Availability::Available
                    } else {
                        Availability::NotInFlavor
                    };
                    (command, availability)
                })
                .collect(),
        };
        self.key = Some(key);
    }

    /// Whether `command` can run.
    pub fn allows(&self, command: MarkdownCommand) -> bool {
        self.available
            .iter()
            .any(|&(c, availability)| c == command && availability.is_available())
    }
}

/// The view's tree with its text, if it is the complete parse of the
/// editor's text.
pub(super) fn current_syntax(doc: &Doc) -> Option<SyntaxRef<'_>> {
    let (tree, text) = doc.syntax.shown();
    let syntax = SyntaxRef::new(tree, text);
    syntax.is_parse_of(doc.editor.text()).then_some(syntax)
}

/// Registers the handler of `command` (action `A`) on `root` if it can run.
fn on<A: Action>(
    root: Stateful<Div>,
    command: MarkdownCommand,
    available: &Availabilities,
    cx: &mut Context<'_, EditorView>,
) -> Stateful<Div> {
    if !available.allows(command) {
        return root;
    }
    root.on_action(cx.listener(move |this, _: &A, window, cx| {
        this.run_markdown(command, window, cx);
    }))
}

impl EditorView {
    /// Registers the handlers of the commands that can run now.
    pub(super) fn markdown_handlers(
        &mut self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        let Some(doc) = self.doc.as_ref().filter(|_| !self.read_only) else {
            return root;
        };
        self.availabilities.update(&self.commands, doc);
        let a = &self.availabilities;
        let root = on::<actions::Newline>(root, MarkdownCommand::Newline, a, cx);
        let root = on::<actions::NewlinePlain>(root, MarkdownCommand::NewlinePlain, a, cx);
        let root = on::<actions::Indent>(root, MarkdownCommand::Indent, a, cx);
        let root = on::<actions::Outdent>(root, MarkdownCommand::Outdent, a, cx);
        let root = on::<actions::RenumberList>(root, MarkdownCommand::RenumberList, a, cx);
        let root = on::<actions::ToggleTask>(root, MarkdownCommand::ToggleTask, a, cx);
        let root = on::<actions::Bold>(root, MarkdownCommand::Bold, a, cx);
        let root = on::<actions::Italic>(root, MarkdownCommand::Italic, a, cx);
        let root = on::<actions::Strikethrough>(root, MarkdownCommand::Strikethrough, a, cx);
        let root = on::<actions::Highlight>(root, MarkdownCommand::Highlight, a, cx);
        let root = on::<actions::InlineCode>(root, MarkdownCommand::InlineCode, a, cx);
        let root = on::<actions::InsertLink>(root, MarkdownCommand::InsertLink, a, cx);
        let root = on::<actions::Heading1>(root, MarkdownCommand::Heading(1), a, cx);
        let root = on::<actions::Heading2>(root, MarkdownCommand::Heading(2), a, cx);
        let root = on::<actions::Heading3>(root, MarkdownCommand::Heading(3), a, cx);
        let root = on::<actions::Heading4>(root, MarkdownCommand::Heading(4), a, cx);
        let root = on::<actions::Heading5>(root, MarkdownCommand::Heading(5), a, cx);
        let root = on::<actions::Heading6>(root, MarkdownCommand::Heading(6), a, cx);
        let root = on::<actions::Paragraph>(root, MarkdownCommand::Paragraph, a, cx);
        let root = on::<actions::Quote>(root, MarkdownCommand::Quote, a, cx);
        let root = on::<actions::MoveLineUp>(root, MarkdownCommand::MoveLineUp, a, cx);
        let root = on::<actions::MoveLineDown>(root, MarkdownCommand::MoveLineDown, a, cx);
        let root = on::<actions::DuplicateLine>(root, MarkdownCommand::DuplicateLine, a, cx);
        let root = on::<actions::JoinLines>(root, MarkdownCommand::JoinLines, a, cx);
        let root = on::<actions::SortLines>(root, MarkdownCommand::SortLines, a, cx);
        let root = on::<actions::ChangeCase>(root, MarkdownCommand::ChangeCase, a, cx);
        on::<actions::ToggleComment>(root, MarkdownCommand::ToggleComment, a, cx)
    }

    /// Runs `command` at every selection. A command that cannot run at the
    /// carets lets its key fall through.
    pub(super) fn run_markdown(
        &mut self,
        command: MarkdownCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(doc) = self.doc.as_mut().filter(|_| !self.read_only) else {
            cx.propagate();
            return;
        };
        if doc.editor.is_composing() {
            if matches!(
                command,
                MarkdownCommand::Newline | MarkdownCommand::NewlinePlain
            ) {
                // The input method did not take Enter: commit as is.
                doc.editor.unmark_text();
                self.changed(window, cx);
            }
            return;
        }
        // Queued edits are parsed first, so the tree is usually current.
        self.pump_syntax(cx);
        let url = (command == MarkdownCommand::InsertLink)
            .then(|| cx.read_from_clipboard().and_then(|item| item.text()))
            .flatten();
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let outcome = run(&self.commands, command, doc, url.as_deref());
        match outcome {
            None | Some(Outcome::NotInFlavor) => cx.propagate(),
            Some(Outcome::Edited | Outcome::Unchanged) => self.changed(window, cx),
        }
    }
}

impl EditorView {
    /// Backspace: removes a task box, list marker, item indentation or
    /// `> ` right before the carets ([`MarkdownCommands::backspace`], one
    /// undo step), else deletes as usual. The Markdown rule needs the
    /// current tree; while a long reparse runs it deletes as usual.
    pub(super) fn backspace(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.read_only {
            return;
        }
        if self
            .doc
            .as_ref()
            .is_some_and(|doc| !doc.editor.is_composing())
        {
            self.pump_syntax(cx);
        }
        let commands = self.commands;
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let handled = match current_syntax(doc) {
            Some(syntax) => {
                let (tree, text) = (syntax.tree.clone(), syntax.text.clone());
                commands
                    .backspace(&mut doc.editor, Some(SyntaxRef::new(&tree, &text)))
                    .is_some()
            }
            None => false,
        };
        if !handled {
            doc.editor.delete_backward(DeleteUnit::Grapheme);
        }
        self.changed(window, cx);
    }

    /// Typed text with smart pairs ([`MarkdownCommands::type_text`]): only
    /// for one character typed at the selections (no range other than the
    /// primary selection, no composition). Returns whether it was handled;
    /// if not, the caller inserts the text as usual.
    pub(super) fn type_smart(&mut self, range: Option<&Range<usize>>, text: &str) -> bool {
        let commands = self.commands;
        let Some(doc) = self.doc.as_mut() else {
            return false;
        };
        if doc.editor.is_composing() || text.chars().nth(1).is_some() {
            return false;
        }
        if range.is_some_and(|range| *range != doc.editor.selected_text_range_utf16().range) {
            return false;
        }
        match current_syntax(doc) {
            Some(syntax) => {
                let (tree, snapshot) = (syntax.tree.clone(), syntax.text.clone());
                commands.type_text(
                    &mut doc.editor,
                    Some(SyntaxRef::new(&tree, &snapshot)),
                    text,
                )
            }
            // A long reparse is running: no tree for the context.
            None => commands.type_text(&mut doc.editor, None, text),
        }
    }
}

/// Runs `command` on `doc` if it can run at the carets; `None` if it
/// cannot.
fn run(
    commands: &MarkdownCommands,
    command: MarkdownCommand,
    doc: &mut Doc,
    clipboard: Option<&str>,
) -> Option<Outcome> {
    let Doc { editor, syntax, .. } = doc;
    let (tree, text) = syntax.shown();
    let syntax = SyntaxRef::new(tree, text);
    // A stale tree is ignored by the commands, which parse the text
    // themselves.
    let syntax = Some(syntax);
    let anywhere = matches!(
        command,
        MarkdownCommand::Newline
            | MarkdownCommand::NewlinePlain
            | MarkdownCommand::Indent
            | MarkdownCommand::Outdent
    );
    let available = if anywhere {
        commands.in_flavor(command)
    } else {
        commands
            .availability(command, editor, syntax)
            .is_available()
    };
    if !available {
        return None;
    }
    Some(run_on(commands, command, editor, syntax, clipboard))
}

fn run_on(
    commands: &MarkdownCommands,
    command: MarkdownCommand,
    editor: &mut Editor,
    syntax: Option<SyntaxRef<'_>>,
    clipboard: Option<&str>,
) -> Outcome {
    if command == MarkdownCommand::InsertLink {
        let url = clipboard.map(str::trim).filter(|url| !url.is_empty());
        commands.insert_link(editor, syntax, url)
    } else {
        commands.run(command, editor, syntax)
    }
}

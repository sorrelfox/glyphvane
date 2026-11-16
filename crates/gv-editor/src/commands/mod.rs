// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Markdown-aware editing commands: list and quote continuation,
//! indentation, renumbering, task boxes, inline formatting, headings,
//! quotes and links.
//!
//! [`MarkdownCommands`] runs a [`MarkdownCommand`] on an [`Editor`]. Each
//! command reads the block structure at the carets from the syntax tree
//! ([`SyntaxRef`]: a tree and the text it is the parse of; a stale or
//! missing tree is replaced by a fresh parse), works on **every selection**,
//! and hands the editor **one transaction** of byte-range replacements,
//! recorded as one [`EditKind::Command`](crate::EditKind::Command) undo step.
//! Text outside the replaced ranges is never rewritten, and the line
//! endings of the document are kept (a new line takes the ending of the
//! line it is inserted on).
//!
//! # Behaviour
//!
//! | Command | Behaviour |
//! |---|---|
//! | [`Newline`](MarkdownCommand::Newline) (Enter) | In a list item: a new item at the item's indentation with the same marker and padding (W + N), the next number of an ordered list (or the same number if all items share one), `[ ]` for a task item; the text after the caret and the rest of the item move to the new item (re-indented for a wider marker); the following items are renumbered. On an empty item: a nested item is outdented, a top-level one ends the list (the marker is removed); in the middle of an ordered list the blank line left behind does not split the list, so the following items are renumbered to stay sequential. In a block quote: the `> ` chain continues; an empty quote line leaves the innermost quote. In code blocks the container prefixes continue. |
//! | [`NewlinePlain`](MarkdownCommand::NewlinePlain) (Shift+Enter) | A new line that continues the containers without a new marker: the line belongs to the same item or quote. A caret in a line's prefix or marker opens a line above instead. |
//! | [`Indent`](MarkdownCommand::Indent) (Tab) | List items the selections cover become children of their previous sibling (moved by its content offset), joining a list at its end; both lists are renumbered. In a quote, the paragraph gets one more `>` level. Elsewhere the indent unit is inserted. |
//! | [`Outdent`](MarkdownCommand::Outdent) (Shift+Tab) | A nested item becomes the next sibling of its parent, taking the parent list's marker; what followed it in the parent becomes its children. In a quote, one `>` level is removed. Elsewhere one indent unit is removed from the line starts. |
//! | [`RenumberList`](MarkdownCommand::RenumberList) | Renumbers the ordered list at each caret from its first number (per [`Numbering`]). |
//! | [`ToggleTask`](MarkdownCommand::ToggleTask) | `[ ]` ↔ `[x]` (a custom Obsidian status unchecks); adds a box to an item without one; turns a paragraph or blank line into a task item. Flavors with task lists only. |
//! | [`Bold`](MarkdownCommand::Bold), [`Italic`](MarkdownCommand::Italic), [`Strikethrough`](MarkdownCommand::Strikethrough), [`Highlight`](MarkdownCommand::Highlight), [`InlineCode`](MarkdownCommand::InlineCode) | Unwraps the innermost such span around the selection or caret; else wraps the selection (trimmed of surrounding whitespace) or the word at the caret, or inserts an empty pair with the caret inside. Strikethrough and highlight only in flavors that have them. |
//! | [`InsertLink`](MarkdownCommand::InsertLink) | `[selection]()` with the caret in the destination; a URL given (clipboard) fills it; a selected URL becomes the destination; `[[selection]]` in wikilink style. |
//! | [`Heading`](MarkdownCommand::Heading), [`Paragraph`](MarkdownCommand::Paragraph) | Sets the lines to an ATX heading of the level, or back to a paragraph (escaping text that would start another block). |
//! | [`Quote`](MarkdownCommand::Quote) | Adds a `> ` level to the blocks the selections cover, or removes the innermost one when they are directly in a quote. |
//! | [`MarkdownCommands::backspace`] (Backspace) | A caret right after a task box removes the box; right after a list marker (and its padding) removes the marker, keeping the text, and renumbers the rest of a sequential ordered list; right after the item indentation of a continuation line removes it (outdent); right after a `> ` removes it. Other selections delete as usual. |
//! | [`MarkdownCommands::type_text`] (typing) | Smart pairs: brackets, quotes and backticks pair at a caret, wrap a selection together with the emphasis delimiters, and a paired closer is typed over. Backspace between a pair deletes both. Never while an input method composes. |
//!
//! Continuation and renumbering keep the CommonMark structure exact: column
//! shifts never change a tab's width (a tab in a moved prefix is rewritten
//! as the spaces it spanned), and a marker whose width changes (`9.` →
//! `10.`) moves its item's continuation lines with it.
//!
//! # Availability
//!
//! [`MarkdownCommands::availability`] tells the editor view whether a
//! command can run for the document's [`Flavor`] and at the carets
//! ([`Availability`]), to disable menu entries and buttons.

mod block;
mod context;
mod doc;
mod flavor;
mod inline;
mod lines;
mod list;
mod pairs;
mod settings;

use gv_syntax::{LeafKind, SyntaxTree};
use gv_text::TextSnapshot;

use crate::editor::Editor;

use self::context::ctx_at;
use self::doc::Doc;
use self::inline::Span;

pub use self::flavor::Flavor;
pub use self::settings::{CommandSettings, EmphasisStyle, IndentUnit, LinkStyle, Numbering};

/// A syntax tree and the text it is the parse of.
#[derive(Clone, Copy, Debug)]
pub struct SyntaxRef<'a> {
    /// The tree.
    pub tree: &'a SyntaxTree,
    /// The text it was parsed from.
    pub text: &'a TextSnapshot,
}

impl<'a> SyntaxRef<'a> {
    /// The pair.
    pub fn new(tree: &'a SyntaxTree, text: &'a TextSnapshot) -> Self {
        Self { tree, text }
    }

    /// Whether the tree is a complete parse of `text` (same revision and
    /// length, not an interim tree).
    pub fn is_parse_of(&self, text: &TextSnapshot) -> bool {
        !self.tree.is_interim()
            && self.text.revision() == text.revision()
            && self.text.len() == text.len()
            && self.tree.len() == text.len()
    }
}

/// A Markdown command. [`MarkdownCommand::id`] is its id in the command
/// registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MarkdownCommand {
    /// Enter: continue the list or quote.
    Newline,
    /// Shift+Enter: a new line in the same item or quote, without a marker.
    NewlinePlain,
    /// Tab.
    Indent,
    /// Shift+Tab.
    Outdent,
    /// Renumber the ordered list.
    RenumberList,
    /// Toggle the task box.
    ToggleTask,
    /// Toggle strong emphasis.
    Bold,
    /// Toggle emphasis.
    Italic,
    /// Toggle strikethrough.
    Strikethrough,
    /// Toggle highlight.
    Highlight,
    /// Toggle a code span.
    InlineCode,
    /// Insert a link.
    InsertLink,
    /// Set an ATX heading of this level (1 to 6).
    Heading(u8),
    /// Set a paragraph.
    Paragraph,
    /// Toggle a block quote.
    Quote,
}

impl MarkdownCommand {
    /// Every command.
    pub const ALL: [MarkdownCommand; 20] = [
        MarkdownCommand::Newline,
        MarkdownCommand::NewlinePlain,
        MarkdownCommand::Indent,
        MarkdownCommand::Outdent,
        MarkdownCommand::RenumberList,
        MarkdownCommand::ToggleTask,
        MarkdownCommand::Bold,
        MarkdownCommand::Italic,
        MarkdownCommand::Strikethrough,
        MarkdownCommand::Highlight,
        MarkdownCommand::InlineCode,
        MarkdownCommand::InsertLink,
        MarkdownCommand::Heading(1),
        MarkdownCommand::Heading(2),
        MarkdownCommand::Heading(3),
        MarkdownCommand::Heading(4),
        MarkdownCommand::Heading(5),
        MarkdownCommand::Heading(6),
        MarkdownCommand::Paragraph,
        MarkdownCommand::Quote,
    ];

    /// The command id (`editor.bold`, …). `editor.newline` is bound to
    /// Enter by default; `editor.renumber_list` is not bound to a key.
    pub fn id(self) -> &'static str {
        match self {
            MarkdownCommand::Newline => "editor.newline",
            MarkdownCommand::NewlinePlain => "editor.newline_plain",
            MarkdownCommand::Indent => "editor.indent",
            MarkdownCommand::Outdent => "editor.outdent",
            MarkdownCommand::RenumberList => "editor.renumber_list",
            MarkdownCommand::ToggleTask => "editor.toggle_task",
            MarkdownCommand::Bold => "editor.bold",
            MarkdownCommand::Italic => "editor.italic",
            MarkdownCommand::Strikethrough => "editor.strikethrough",
            MarkdownCommand::Highlight => "editor.highlight",
            MarkdownCommand::InlineCode => "editor.inline_code",
            MarkdownCommand::InsertLink => "editor.insert_link",
            MarkdownCommand::Heading(1) => "editor.heading_1",
            MarkdownCommand::Heading(2) => "editor.heading_2",
            MarkdownCommand::Heading(3) => "editor.heading_3",
            MarkdownCommand::Heading(4) => "editor.heading_4",
            MarkdownCommand::Heading(5) => "editor.heading_5",
            MarkdownCommand::Heading(_) => "editor.heading_6",
            MarkdownCommand::Paragraph => "editor.paragraph",
            MarkdownCommand::Quote => "editor.quote",
        }
    }

    /// The command with id `id`.
    pub fn from_id(id: &str) -> Option<MarkdownCommand> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }
}

/// Whether a command can run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    /// It can.
    Available,
    /// The flavor has no such syntax (strikethrough in CommonMark, …).
    NotInFlavor,
    /// Not at any caret (inline formatting in a code block, renumbering
    /// outside an ordered list, …).
    NotHere,
}

impl Availability {
    /// Whether the command can run.
    pub fn is_available(self) -> bool {
        self == Availability::Available
    }
}

/// What running a command did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// The text changed (one undo step).
    Edited,
    /// Nothing changed (the selections may have moved).
    Unchanged,
    /// The command does not exist in the document's flavor; nothing
    /// changed.
    NotInFlavor,
}

/// The Markdown commands of one document: its flavor and the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MarkdownCommands {
    /// The document's flavor.
    pub flavor: Flavor,
    /// The command settings.
    pub settings: CommandSettings,
}

impl MarkdownCommands {
    /// Commands for `flavor` with `settings`.
    pub fn new(flavor: Flavor, settings: CommandSettings) -> Self {
        Self { flavor, settings }
    }

    /// Commands for the profile `id` with the default settings (an unknown
    /// profile is treated as Extended).
    pub fn for_profile(id: &str) -> Self {
        Self::new(Flavor::from_id_or_extended(id), CommandSettings::default())
    }

    /// Whether the flavor has the syntax `command` writes.
    pub fn in_flavor(&self, command: MarkdownCommand) -> bool {
        match command {
            MarkdownCommand::Strikethrough => self.flavor.has_strikethrough(),
            MarkdownCommand::Highlight => self.flavor.has_highlight(),
            MarkdownCommand::ToggleTask => self.flavor.has_tasks(),
            MarkdownCommand::Heading(level) => (1..=6).contains(&level),
            _ => true,
        }
    }

    /// Runs `command` on every selection of `editor`.
    pub fn run(
        &self,
        command: MarkdownCommand,
        editor: &mut Editor,
        syntax: Option<SyntaxRef<'_>>,
    ) -> Outcome {
        if command == MarkdownCommand::InsertLink {
            return self.insert_link(editor, syntax, None);
        }
        if !self.in_flavor(command) {
            return Outcome::NotInFlavor;
        }
        if editor.is_composing() && matches!(command, MarkdownCommand::Newline) {
            // No list continuation while an input method composes.
            return outcome(editor.newline());
        }
        let mut doc = Doc::new(editor, syntax, self.flavor);
        let settings = &self.settings;
        match command {
            MarkdownCommand::Newline => list::newline(&mut doc, settings, false),
            MarkdownCommand::NewlinePlain => list::newline(&mut doc, settings, true),
            MarkdownCommand::Indent => list::indent(&mut doc, settings),
            MarkdownCommand::Outdent => list::outdent(&mut doc, settings),
            MarkdownCommand::RenumberList => list::renumber_command(&mut doc, settings),
            MarkdownCommand::ToggleTask => list::toggle_task(&mut doc),
            MarkdownCommand::Bold => inline::toggle_span(&mut doc, Span::Bold, settings),
            MarkdownCommand::Italic => inline::toggle_span(&mut doc, Span::Italic, settings),
            MarkdownCommand::Strikethrough => {
                inline::toggle_span(&mut doc, Span::Strike, settings);
            }
            MarkdownCommand::Highlight => {
                inline::toggle_span(&mut doc, Span::Highlight, settings);
            }
            MarkdownCommand::InlineCode => inline::toggle_span(&mut doc, Span::Code, settings),
            MarkdownCommand::InsertLink => inline::insert_link(&mut doc, settings, None),
            MarkdownCommand::Heading(level) => block::set_heading(&mut doc, level),
            MarkdownCommand::Paragraph => block::set_heading(&mut doc, 0),
            MarkdownCommand::Quote => block::toggle_quote(&mut doc),
        }
        outcome(doc.commit(editor))
    }

    /// Backspace at every selection, where a caret is right after a list
    /// marker, a task box or a container prefix (see the module
    /// documentation): one command step. `None`, changing nothing, if no
    /// caret is at such a place, an input method composes, or `syntax` is
    /// not the current parse of a text over [`PARSE_LIMIT`] bytes: the
    /// caller then deletes as usual.
    pub fn backspace(&self, editor: &mut Editor, syntax: Option<SyntaxRef<'_>>) -> Option<Outcome> {
        if editor.is_composing() || !may_parse(editor, syntax) {
            return None;
        }
        let mut doc = Doc::new(editor, syntax, self.flavor);
        if !list::backspace(&mut doc, &self.settings) {
            return None;
        }
        Some(outcome(doc.commit(editor)))
    }

    /// Insert link, with the URL on the clipboard if there is one (it is
    /// used only if it is an absolute URL).
    pub fn insert_link(
        &self,
        editor: &mut Editor,
        syntax: Option<SyntaxRef<'_>>,
        url: Option<&str>,
    ) -> Outcome {
        let mut doc = Doc::new(editor, syntax, self.flavor);
        inline::insert_link(&mut doc, &self.settings, url);
        outcome(doc.commit(editor))
    }

    /// Whether `command` can run: in the flavor, and at one of the carets.
    pub fn availability(
        &self,
        command: MarkdownCommand,
        editor: &Editor,
        syntax: Option<SyntaxRef<'_>>,
    ) -> Availability {
        if !self.in_flavor(command) {
            return Availability::NotInFlavor;
        }
        let doc = Doc::new(editor, syntax, self.flavor);
        self.available_in(command, &doc)
    }

    /// The availability of every command (one parse at most).
    pub fn availabilities(
        &self,
        editor: &Editor,
        syntax: Option<SyntaxRef<'_>>,
    ) -> Vec<(MarkdownCommand, Availability)> {
        let doc = Doc::new(editor, syntax, self.flavor);
        MarkdownCommand::ALL
            .into_iter()
            .map(|command| {
                let availability = if self.in_flavor(command) {
                    self.available_in(command, &doc)
                } else {
                    Availability::NotInFlavor
                };
                (command, availability)
            })
            .collect()
    }

    fn available_in(&self, command: MarkdownCommand, doc: &Doc) -> Availability {
        let here = doc.sels.iter().any(|spec| {
            let ctx = ctx_at(doc, spec.head);
            let leaf = ctx.leaf.as_ref();
            let verbatim = ctx.in_verbatim();
            let inline = leaf.is_none_or(context::LeafInfo::is_inline) && !verbatim;
            let code_span = leaf
                .filter(|leaf| leaf.is_inline())
                .is_some_and(|leaf| inline::in_code_span(&inline::inlines(doc, leaf), spec.head));
            match command {
                MarkdownCommand::Newline
                | MarkdownCommand::NewlinePlain
                | MarkdownCommand::Indent
                | MarkdownCommand::Outdent => true,
                MarkdownCommand::RenumberList => ctx.innermost_item().is_some_and(|ii| {
                    ii > 0 && ctx.chain[ii - 1].list().is_some_and(|list| list.ordered)
                }),
                MarkdownCommand::ToggleTask => {
                    !verbatim
                        && (list::list_level(&ctx).is_some()
                            || leaf.is_some_and(|leaf| leaf.kind == LeafKind::Paragraph)
                            || (leaf.is_none() && ctx.blank))
                }
                MarkdownCommand::InlineCode => inline,
                MarkdownCommand::Bold
                | MarkdownCommand::Italic
                | MarkdownCommand::Strikethrough
                | MarkdownCommand::Highlight
                | MarkdownCommand::InsertLink => inline && !code_span,
                MarkdownCommand::Heading(_) | MarkdownCommand::Paragraph => {
                    leaf.is_none_or(|leaf| {
                        matches!(leaf.kind, LeafKind::Paragraph | LeafKind::Heading { .. })
                    })
                }
                MarkdownCommand::Quote => {
                    !leaf.is_some_and(|leaf| matches!(leaf.kind, LeafKind::FrontMatter { .. }))
                }
            }
        });
        if here {
            Availability::Available
        } else {
            Availability::NotHere
        }
    }
}

/// The largest text that the keystroke-level commands (Backspace at a
/// marker, smart pairs) parse themselves when they are not given the
/// current tree; above it they fall back to plain editing.
pub const PARSE_LIMIT: usize = 1 << 20;

/// Whether `syntax` is the current parse of `editor`'s text, or the text is
/// small enough to parse.
fn may_parse(editor: &Editor, syntax: Option<SyntaxRef<'_>>) -> bool {
    syntax.is_some_and(|syntax| syntax.is_parse_of(editor.text()))
        || editor.text().len() <= PARSE_LIMIT
}

fn outcome(changed: bool) -> Outcome {
    if changed {
        Outcome::Edited
    } else {
        Outcome::Unchanged
    }
}

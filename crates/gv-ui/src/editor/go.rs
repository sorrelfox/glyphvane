// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Go commands and pickers of the Source view: Go to line (Ctrl+G),
//! Go to heading (Mod+Shift+O), Go to footnote or back, Insert callout…,
//! and Expand and Shrink selection (Shift+Alt+→ / ←).
//!
//! - **Go to line** and **Go to heading** open the Go popover
//!   ([`crate::goto`]). While it is open the caret follows the line typed
//!   or the heading highlighted, and the view scrolls there; Enter keeps
//!   it, Esc puts the selections and the scroll position back.
//! - **Go to footnote or back** moves the caret from a `[^label]`
//!   reference to its definition, or from a definition to the first
//!   reference.
//! - **Insert callout…** opens the popover with the profile's callout
//!   types (Obsidian) or alert types (GitHub, Extended); Enter inserts one
//!   ([`MarkdownCommands::insert_callout`]). CommonMark has neither, so the
//!   command has no handler there.
//! - **Expand and shrink selection** work in viewer mode too: they only
//!   move the selections.
//!
//! None of them does anything while an input method composes.

use std::rc::Rc;

use gpui::{Context, Div, InteractiveElement as _, SharedString, Stateful, Window};
use gv_editor::commands::{SelectionHistory, callout_types, line_target_offset};
use gv_editor::{MarkdownCommands, SelectionSpec, SyntaxRef};

use super::EditorView;
use super::markdown::current_syntax;
use crate::goto::{self, Choice, GotoEvent, GotoItem, GotoKind, GotoRequest};
use crate::keymap::actions::editor as actions;
use crate::keymap::actions::navigation;

/// Where the view was when a popover opened: its selections and scroll.
#[derive(Debug, Clone)]
struct Saved {
    selections: (Vec<SelectionSpec>, usize),
    scroll_y: f64,
    scroll_x: f32,
}

/// What a popover of the view goes to.
#[derive(Debug, Clone)]
enum Target {
    Line,
    /// The heading start of each row.
    Headings(Vec<usize>),
    /// The callout type of each row.
    Callouts(Vec<&'static str>),
}

impl EditorView {
    /// Registers the handlers of the Go commands, the callout picker and
    /// expand/shrink selection.
    pub(super) fn go_handlers(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        if self.doc.is_none() {
            return root;
        }
        let root = root.on_action(cx.listener(|this, _: &navigation::GoToLine, window, cx| {
            this.go_to_line(window, cx);
        }));
        // Plain text has no headings, footnotes or syntax to step through.
        if self.plain.is_some() {
            return root;
        }
        let root = root
            .on_action(
                cx.listener(|this, _: &navigation::GoToHeading, window, cx| {
                    this.go_to_heading(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &navigation::GoToFootnote, window, cx| {
                    this.go_to_footnote(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &actions::ExpandSelection, window, cx| {
                    this.select_step(true, window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &actions::ShrinkSelection, window, cx| {
                    this.select_step(false, window, cx);
                }),
            );
        if self.read_only || !self.commands.has_callouts() {
            return root;
        }
        root.on_action(cx.listener(|this, _: &actions::InsertCallout, window, cx| {
            this.pick_callout(window, cx);
        }))
    }

    fn saved(&self) -> Option<Saved> {
        let doc = self.doc.as_ref()?;
        Some(Saved {
            selections: doc.editor.selections().specs(),
            scroll_y: self.scroll_y,
            scroll_x: self.scroll_x,
        })
    }

    /// Whether a popover may open: a document is there and no input method
    /// composes.
    fn can_pick(&self) -> bool {
        self.doc
            .as_ref()
            .is_some_and(|doc| !doc.editor.is_composing())
    }

    /// Opens a popover whose events come back to [`Self::go_event`].
    fn open_picker(
        &mut self,
        title: &'static str,
        kind: GotoKind,
        target: Target,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(saved) = self.saved() else {
            return;
        };
        let view = cx.entity().downgrade();
        let on_event = Rc::new(
            move |event: GotoEvent, window: &mut Window, cx: &mut gpui::App| {
                view.update(cx, |this, cx| {
                    this.go_event(event, &target, &saved, window, cx)
                })
                .ok();
            },
        );
        goto::open(
            GotoRequest {
                title: title.into(),
                kind,
                on_event,
            },
            window,
            cx,
        );
    }

    /// Ctrl+G.
    fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.can_pick() {
            return;
        }
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let text = doc.editor.text();
        let current = text
            .line_of_offset(doc.editor.selections().primary().head())
            .unwrap_or(0)
            + 1;
        let count = text.line_count().max(1);
        self.open_picker(
            "Go to line",
            GotoKind::Line { current, count },
            Target::Line,
            window,
            cx,
        );
    }

    /// Mod+Shift+O.
    fn go_to_heading(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.can_pick() {
            return;
        }
        self.pump_syntax(cx);
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let headings = match current_syntax(doc) {
            Some(syntax) => {
                let (tree, text) = (syntax.tree.clone(), syntax.text.clone());
                self.commands
                    .headings(&doc.editor, Some(SyntaxRef::new(&tree, &text)))
            }
            None => self.commands.headings(&doc.editor, None),
        };
        let caret = doc.editor.selections().primary().head();
        let top = headings.iter().map(|h| h.level).min().unwrap_or(1);
        let initial = headings
            .iter()
            .rposition(|h| h.offset <= caret)
            .unwrap_or(0);
        let items = headings
            .iter()
            .map(|h| GotoItem {
                label: if h.text.is_empty() {
                    SharedString::from("(empty heading)")
                } else {
                    h.text.clone().into()
                },
                detail: Some(format!("H{}", h.level).into()),
                depth: h.level.saturating_sub(top),
            })
            .collect();
        let starts = headings.iter().map(|h| h.text_start).collect();
        self.open_picker(
            "Go to heading",
            GotoKind::List {
                placeholder: "Go to heading".into(),
                action: "go".into(),
                empty: "No headings".into(),
                items,
                initial,
            },
            Target::Headings(starts),
            window,
            cx,
        );
    }

    /// Insert callout…: the type picker.
    fn pick_callout(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.can_pick() || self.read_only {
            return;
        }
        let types = callout_types(self.commands.flavor);
        if types.is_empty() {
            cx.propagate();
            return;
        }
        let items = types
            .iter()
            .map(|kind| GotoItem {
                label: (*kind).into(),
                detail: None,
                depth: 0,
            })
            .collect();
        self.open_picker(
            "Insert callout",
            GotoKind::List {
                placeholder: "Callout type".into(),
                action: "insert".into(),
                empty: "No such type".into(),
                items,
                initial: 0,
            },
            Target::Callouts(types.to_vec()),
            window,
            cx,
        );
    }

    /// What a popover of the view chose.
    fn go_event(
        &mut self,
        event: GotoEvent,
        target: &Target,
        saved: &Saved,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match (event, target) {
            (GotoEvent::Cancel, Target::Callouts(_)) => {}
            (GotoEvent::Cancel, _) => {
                if let Some(doc) = self.doc.as_mut() {
                    let (specs, primary) = &saved.selections;
                    doc.editor.set_selections(specs, *primary);
                }
                self.scroll_y = saved.scroll_y;
                self.scroll_x = saved.scroll_x;
                self.pending_top = None;
                self.autoscroll = false;
                cx.notify();
            }
            (GotoEvent::Preview(_), Target::Callouts(_)) => {}
            (GotoEvent::Confirm(Choice::Item(i)), Target::Callouts(types)) => {
                let Some(kind) = types.get(i) else {
                    return;
                };
                self.insert_callout(kind, window, cx);
            }
            (GotoEvent::Preview(choice) | GotoEvent::Confirm(choice), _) => {
                self.go_to_choice(choice, target, cx);
            }
        }
    }

    /// Puts the caret at a line or heading and scrolls there.
    fn go_to_choice(&mut self, choice: Choice, target: &Target, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        match (choice, target) {
            (Choice::Line(line, column), Target::Line) => {
                let offset = line_target_offset(doc.editor.text(), line, column);
                doc.editor.set_cursor(offset);
                self.autoscroll = true;
            }
            (Choice::Item(i), Target::Headings(starts)) => {
                let Some(&offset) = starts.get(i) else {
                    return;
                };
                doc.editor.set_cursor(offset);
                self.scroll_to_offset(offset, cx);
                self.autoscroll = false;
            }
            _ => return,
        }
        cx.notify();
    }

    /// Runs Insert callout with `kind` at the selections.
    fn insert_callout(&mut self, kind: &str, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.read_only {
            return;
        }
        self.pump_syntax(cx);
        let commands: MarkdownCommands = self.commands;
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        if doc.editor.is_composing() {
            return;
        }
        let (tree, text) = doc.syntax.shown();
        let (tree, text) = (tree.clone(), text.clone());
        commands.insert_callout(&mut doc.editor, Some(SyntaxRef::new(&tree, &text)), kind);
        self.changed(window, cx);
    }

    /// Go to footnote or back.
    fn go_to_footnote(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.can_pick() {
            return;
        }
        self.pump_syntax(cx);
        let commands = self.commands;
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let (tree, text) = doc.syntax.shown();
        let (tree, text) = (tree.clone(), text.clone());
        let Some(target) =
            commands.footnote_target(&doc.editor, Some(SyntaxRef::new(&tree, &text)))
        else {
            cx.propagate();
            return;
        };
        // On the label, so that the command goes back from there.
        doc.editor.set_cursor(target.start + 2);
        self.changed(window, cx);
    }

    /// Expand (`grow`) or shrink selection.
    fn select_step(&mut self, grow: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.pump_syntax(cx);
        let commands = self.commands;
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let (tree, text) = doc.syntax.shown();
        let (tree, text) = (tree.clone(), text.clone());
        let syntax = Some(SyntaxRef::new(&tree, &text));
        let history: &mut SelectionHistory = &mut self.selection_history;
        let changed = if grow {
            commands.expand_selection(&mut doc.editor, syntax, history)
        } else {
            commands.shrink_selection(&mut doc.editor, syntax, history)
        };
        if changed {
            self.changed(window, cx);
        }
    }
}

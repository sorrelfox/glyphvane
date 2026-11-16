// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Smart pairs: typed brackets, quotes and Markdown delimiters.
//!
//! A typed character is planned per selection:
//!
//! - **Wrap.** Over a selection on one line, `(` `[` `{` `"` and a
//!   backtick, and the emphasis delimiters `*` `_` (plus `~` and `=` in
//!   flavors with strikethrough and highlight), wrap it; the text stays
//!   selected, so typing `*` twice makes it strong.
//! - **Pair.** At a caret, `(` `[` `{` `"` and a backtick insert their
//!   closer after the caret, if the next character is whitespace, the end
//!   of the line or closing punctuation (`)` `]` `}` `>` `,` `.` `;` `:`
//!   `!` `?`), and the previous character is not a backslash. A quote or
//!   backtick also needs a previous character that is not a letter, digit,
//!   `_` or the same character (so an apostrophe-like use, a word's end and
//!   a third backtick of a fence do not pair). The emphasis delimiters
//!   never pair at a caret (they start list items and thematic breaks).
//! - **Over.** Typing a closer just before a closer that smart pairs
//!   inserted moves over it.
//! - Nothing is paired or wrapped in code, HTML and front-matter blocks or
//!   in a code span.
//!
//! The whole keystroke is one typing transaction for every selection.

use std::ops::Range;

use gv_text::TextSnapshot;

use crate::editor::{Editor, TypedEdit, char_after, char_before};

use super::context::ctx_at;
use super::doc::Doc;
use super::flavor::Flavor;
use super::inline::{in_code_span, inlines};
use super::{MarkdownCommands, SyntaxRef, may_parse};

/// The closer `c` pairs with at a caret.
fn pair_closer(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' | '`' => Some(c),
        _ => None,
    }
}

/// The closer `c` wraps a selection with.
fn wrap_closer(c: char, flavor: Flavor) -> Option<char> {
    match c {
        '*' | '_' => Some(c),
        '~' if flavor.has_strikethrough() => Some(c),
        '=' if flavor.has_highlight() => Some(c),
        _ => pair_closer(c),
    }
}

/// Whether typing `c` can move over a remembered closer.
fn is_closer(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '`')
}

/// What one selection does with the typed character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan {
    /// Insert it as typed.
    Plain,
    /// Insert it and its closer.
    Pair(char),
    /// Wrap the selection.
    Wrap(char),
    /// Move over the closer after the caret.
    Over,
}

/// Whether a caret at `at` may get a pair for `c`, by the characters
/// around it.
fn pairs_here(text: &TextSnapshot, at: usize, c: char) -> bool {
    let before = char_before(text, at);
    if before == Some('\\') {
        return false;
    }
    let next_ok = match char_after(text, at) {
        None => true,
        Some(n) => {
            n.is_whitespace()
                || matches!(n, ')' | ']' | '}' | '>' | ',' | '.' | ';' | ':' | '!' | '?')
        }
    };
    if !next_ok {
        return false;
    }
    if matches!(c, '"' | '`') {
        return !before.is_some_and(|b| b.is_alphanumeric() || b == '_' || b == c);
    }
    true
}

/// Whether pairing and wrapping are allowed at `at`: not in a code, HTML
/// or front-matter block, not in a code span.
fn markup_allowed(doc: &Doc, at: usize) -> bool {
    let ctx = ctx_at(doc, at);
    if ctx.in_verbatim() {
        return false;
    }
    match ctx.leaf.as_ref().filter(|leaf| leaf.is_inline()) {
        Some(leaf) => !in_code_span(&inlines(doc, leaf), at),
        None => true,
    }
}

impl MarkdownCommands {
    /// Types `text` (one character, not from an input-method composition)
    /// at every selection with smart pairs. Returns false, changing
    /// nothing, when no selection pairs, wraps or moves over a closer, or
    /// smart pairs are off: the caller then inserts the text as usual.
    pub fn type_text(
        &self,
        editor: &mut Editor,
        syntax: Option<SyntaxRef<'_>>,
        text: &str,
    ) -> bool {
        if !self.settings.smart_pairs || editor.is_composing() {
            return false;
        }
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return false;
        };
        let wraps = wrap_closer(c, self.flavor).is_some();
        if !wraps && !is_closer(c) {
            return false;
        }
        let snapshot = editor.snapshot();
        let selections: Vec<(Range<usize>, bool)> = editor
            .selections()
            .iter()
            .map(|selection| (selection.range(), selection.tail() <= selection.head()))
            .collect();
        // The plans that need no syntax first.
        let mut plans: Vec<Option<Plan>> = selections
            .iter()
            .map(|(range, _)| {
                if range.is_empty() {
                    let at = range.start;
                    if is_closer(c) && editor.auto_closer_at(at) == Some(c) {
                        return Some(Plan::Over);
                    }
                    match pair_closer(c) {
                        Some(_) if pairs_here(&snapshot, at, c) => None,
                        _ => Some(Plan::Plain),
                    }
                } else {
                    let one_line = snapshot
                        .text_for_range(range.clone())
                        .is_ok_and(|t| !t.contains(['\n', '\r']));
                    if wraps && one_line {
                        None
                    } else {
                        Some(Plan::Plain)
                    }
                }
            })
            .collect();
        if plans.iter().any(Option::is_none) {
            // Without the current tree of a large text, characters that
            // need the context are typed as they are.
            let doc = may_parse(editor, syntax).then(|| Doc::new(editor, syntax, self.flavor));
            for (plan, (range, _)) in plans.iter_mut().zip(&selections) {
                if plan.is_some() {
                    continue;
                }
                let allowed = doc.as_ref().is_some_and(|doc| {
                    markup_allowed(doc, range.start)
                        && (range.is_empty() || markup_allowed(doc, range.end))
                });
                *plan = Some(match (allowed, range.is_empty()) {
                    (false, _) => Plan::Plain,
                    (true, true) => Plan::Pair(pair_closer(c).unwrap_or(c)),
                    (true, false) => Plan::Wrap(wrap_closer(c, self.flavor).unwrap_or(c)),
                });
            }
        }
        let plans: Vec<Plan> = plans
            .into_iter()
            .map(|p| p.unwrap_or(Plan::Plain))
            .collect();
        if plans.iter().all(|plan| *plan == Plan::Plain) {
            return false;
        }
        let edits = plans
            .into_iter()
            .zip(selections)
            .map(|(plan, (range, forward))| typed_edit(&snapshot, plan, c, range, forward))
            .collect();
        editor.type_edits(edits)
    }
}

fn typed_edit(
    text: &TextSnapshot,
    plan: Plan,
    c: char,
    range: Range<usize>,
    forward: bool,
) -> TypedEdit {
    let width = c.len_utf8();
    match plan {
        Plan::Plain => TypedEdit {
            range,
            text: c.to_string(),
            select: width..width,
            closer: None,
            over: false,
        },
        Plan::Pair(closer) => TypedEdit {
            range,
            text: format!("{c}{closer}"),
            select: width..width,
            closer: Some(width),
            over: false,
        },
        Plan::Wrap(closer) => {
            let inner = text.text_for_range(range.clone()).unwrap_or_default();
            let end = width + inner.len();
            TypedEdit {
                range,
                text: format!("{c}{inner}{closer}"),
                select: if forward { width..end } else { end..width },
                closer: None,
                over: false,
            }
        }
        Plan::Over => TypedEdit {
            range: range.start..range.start,
            text: String::new(),
            select: width..width,
            closer: None,
            over: true,
        },
    }
}

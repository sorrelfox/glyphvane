// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Go popovers: Go to line (Ctrl+G), Go to heading (Mod+Shift+O) and
//! the callout type picker (Insert callout…).
//!
//! None of them is designed. They are drawn like the command palette and
//! the quick switcher (SPEC § Popovers): 540 wide, 84 below the top of the
//! window and centred, an input row 40 high, rows 28 high, a footer with
//! key hints. Go to line has no list: a line under the field says which
//! line the caret is on and what can be typed. Heading rows are indented
//! by level and show the level (`H2`) at the right.
//!
//! # Behaviour
//!
//! - **Opening.** A view asks for a popover with a [`GotoRequest`]: what
//!   it lists and a callback that hears [`GotoEvent`]s. The popover takes
//!   the keyboard and gives it back when it closes.
//! - **Preview.** As the line typed changes, or the highlighted row moves,
//!   the view is told ([`GotoEvent::Preview`]) and moves its caret there,
//!   so the text shows where Enter will go.
//! - **Enter** confirms ([`GotoEvent::Confirm`]) and closes; on a line
//!   that is not a number it does nothing. A click on a row confirms it.
//! - **Esc** closes and tells the view to go back to where it was
//!   ([`GotoEvent::Cancel`]); so does a click outside.
//! - Up and Down move the highlight, wrapping around.
//!
//! The popover is a layer of the window root (GPUI Base's `Root` plugin),
//! like the palette, so it covers any window opened after [`init`].

#[cfg(test)]
mod tests;
mod view;

use std::ops::Range;
use std::rc::Rc;

use gpui::base::Root;
use gpui::{App, Entity, KeyBinding, SharedString, Window};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub use view::GotoPopover;

gpui::actions!(
    goto,
    [
        /// Highlight the previous row.
        SelectPrevious,
        /// Highlight the next row.
        SelectNext,
        /// Go to the highlighted row or the line typed.
        Confirm,
        /// Close and go back.
        Cancel,
    ]
);

/// The popover's key context.
pub const KEY_CONTEXT: &str = "GotoPopover";

/// The popover's own keys, bound on its field as well, where the text
/// field's own Up, Down, Enter and Esc would otherwise take them.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let field = "GotoPopover > Input";
    let mut keys = Vec::new();
    for context in [KEY_CONTEXT, field] {
        keys.extend([
            KeyBinding::new("up", SelectPrevious, Some(context)),
            KeyBinding::new("down", SelectNext, Some(context)),
            KeyBinding::new("enter", Confirm, Some(context)),
            KeyBinding::new("escape", Cancel, Some(context)),
        ]);
    }
    keys
}

/// Binds the popover's keys and adds it to the window root. Called by
/// [`crate::init`], before any window opens.
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
    Root::register_plugin::<GotoPopover>(cx, GotoPopover::new);
}

/// The Go popover of `window`, if its root has one.
pub fn goto_of(window: &Window, cx: &App) -> Option<Entity<GotoPopover>> {
    window
        .root::<Root>()
        .flatten()?
        .read(cx)
        .plugin::<GotoPopover>()
}

/// Opens the popover of `window` for `request` (replacing one that is
/// open). Returns whether the window has a popover.
pub fn open(request: GotoRequest, window: &mut Window, cx: &mut App) -> bool {
    let Some(popover) = goto_of(window, cx) else {
        return false;
    };
    popover.update(cx, |popover, cx| popover.open(request, window, cx));
    true
}

/// A row of a list popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GotoItem {
    /// What the row says (and what the query is matched against).
    pub label: SharedString,
    /// A note at the right of the row (a heading's level).
    pub detail: Option<SharedString>,
    /// Indentation steps (a heading's level less one).
    pub depth: u8,
}

/// What a popover offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GotoKind {
    /// Go to line: a field for `line` or `line:column`.
    Line {
        /// The caret's line, 1-based.
        current: usize,
        /// The number of lines.
        count: usize,
    },
    /// A list to pick from.
    List {
        /// What the field says while empty.
        placeholder: SharedString,
        /// What the footer calls Enter ("go", "insert").
        action: SharedString,
        /// What the list says when nothing matches or there is nothing.
        empty: SharedString,
        /// The rows, in document order.
        items: Vec<GotoItem>,
        /// The row highlighted at first (the heading the caret is in).
        initial: usize,
    },
}

/// What was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// A 1-based line and column.
    Line(usize, Option<usize>),
    /// An index into the request's items.
    Item(usize),
}

/// What the popover tells the view that opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotoEvent {
    /// Show where the choice goes (the popover stays open).
    Preview(Choice),
    /// Go there; the popover has closed.
    Confirm(Choice),
    /// Go back to where it was; the popover has closed.
    Cancel,
}

/// The callback of a request.
pub type GotoCallback = Rc<dyn Fn(GotoEvent, &mut Window, &mut App)>;

/// A popover to open.
#[derive(Clone)]
pub struct GotoRequest {
    /// The popover's accessible name ("Go to line").
    pub title: SharedString,
    pub kind: GotoKind,
    pub on_event: GotoCallback,
}

impl std::fmt::Debug for GotoRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GotoRequest")
            .field("title", &self.title)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// A row shown for a query: the item and its matched byte ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub index: usize,
    pub matched: Vec<Range<usize>>,
}

/// The rows of `items` for `query`: all of them in order when it is empty,
/// else the fuzzy matches, best first (ties in document order).
pub(crate) fn filter(items: &[GotoItem], query: &str) -> Vec<Row> {
    if query.trim().is_empty() {
        return (0..items.len())
            .map(|index| Row {
                index,
                matched: Vec::new(),
            })
            .collect();
    }
    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    let mut scored: Vec<(u32, Row)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        indices.clear();
        let label = item.label.as_ref();
        let Some(score) =
            pattern.indices(Utf32Str::new(label, &mut buf), &mut matcher, &mut indices)
        else {
            continue;
        };
        scored.push((
            score,
            Row {
                index,
                matched: char_ranges(label, &mut indices),
            },
        ));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.index.cmp(&b.1.index)));
    scored.into_iter().map(|(_, row)| row).collect()
}

/// The byte ranges of the characters at the matcher's `indices` (it counts
/// bytes in ASCII text and grapheme clusters otherwise), merged where they
/// touch.
fn char_ranges(text: &str, indices: &mut Vec<u32>) -> Vec<Range<usize>> {
    use unicode_segmentation::UnicodeSegmentation as _;
    indices.sort_unstable();
    indices.dedup();
    let spans: Vec<Range<usize>> = if text.is_ascii() {
        indices
            .iter()
            .map(|&i| i as usize..i as usize + 1)
            .collect()
    } else {
        text.grapheme_indices(true)
            .enumerate()
            .filter(|(i, _)| indices.binary_search(&(*i as u32)).is_ok())
            .map(|(_, (start, g))| start..start + g.len())
            .collect()
    };
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if last.end == span.start => last.end = span.end,
            _ => merged.push(span),
        }
    }
    merged
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Shared helpers: editors from marked text, and marked text from editors.
//!
//! In marked text, `|` is a caret, and `{` … `}` a selection whose tail is
//! at `{` and head at `}` (so `}…{` is a backwards selection). The last
//! selection written is primary.

#![allow(dead_code)]

use std::time::Duration;

use gv_editor::{Editor, EditorConfig, HistoryConfig, ManualClock, SelectionSpec};
use gv_text::Buffer;

/// Text and selections from marked text.
pub fn parse(marked: &str) -> (String, Vec<SelectionSpec>) {
    let mut text = String::new();
    let mut specs = Vec::new();
    let mut open: Option<(char, usize)> = None;
    for c in marked.chars() {
        match c {
            '|' => specs.push(SelectionSpec::caret(text.len())),
            '{' | '}' => match open.take() {
                None => open = Some((c, text.len())),
                Some(('{', tail)) => specs.push(SelectionSpec::new(tail, text.len())),
                Some((_, head)) => specs.push(SelectionSpec::new(text.len(), head)),
            },
            _ => text.push(c),
        }
    }
    assert!(open.is_none(), "unclosed selection in {marked:?}");
    (text, specs)
}

/// The editor's text with its selections marked.
pub fn render(editor: &Editor) -> String {
    let text = editor.text().to_string();
    let mut marks: Vec<(usize, char)> = Vec::new();
    for selection in editor.selections().iter() {
        if selection.is_empty() {
            marks.push((selection.head(), '|'));
        } else {
            marks.push((selection.tail(), '{'));
            marks.push((selection.head(), '}'));
        }
    }
    // At one offset, a selection end comes before the next one's start.
    marks.sort_by_key(|&(offset, mark)| (offset, mark != '}'));
    let mut out = String::new();
    let mut pos = 0;
    for (offset, mark) in marks {
        out.push_str(&text[pos..offset]);
        out.push(mark);
        pos = offset;
    }
    out.push_str(&text[pos..]);
    out
}

/// An editor over marked text with a manual clock (returned too).
pub fn editor(marked: &str) -> (Editor, ManualClock) {
    editor_with(marked, HistoryConfig::default())
}

/// Like [`editor`], with a history configuration.
pub fn editor_with(marked: &str, history: HistoryConfig) -> (Editor, ManualClock) {
    let (text, specs) = parse(marked);
    let clock = ManualClock::new();
    let config = EditorConfig {
        history,
        ..EditorConfig::default()
    };
    let mut editor = Editor::with_config(Buffer::new(&text), config, Box::new(clock.clone()));
    if !specs.is_empty() {
        let primary = specs.len() - 1;
        editor.set_selections(&specs, primary);
    }
    (editor, clock)
}

/// Types `text` one character at a time, `gap` apart.
pub fn type_chars(editor: &mut Editor, clock: &ManualClock, text: &str, gap: Duration) {
    let mut buf = [0u8; 4];
    for c in text.chars() {
        clock.advance(gap);
        editor.insert_at_selections(c.encode_utf8(&mut buf));
    }
}

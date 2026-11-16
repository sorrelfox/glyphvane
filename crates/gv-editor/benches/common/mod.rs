// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Inputs shared by the `wrap` and `icount` benchmarks.

use gv_editor::{Editor, Motion};
use gv_text::Buffer;

/// The wrap width in columns of the monospace measure.
pub const COLUMNS: f32 = 80.0;

/// A Markdown-like document of about `len` bytes, with short lines, lines
/// a little over 80 columns and long paragraphs. Deterministic.
pub fn document(len: usize) -> String {
    const PIECES: &[&str] = &[
        "# Heading with **bold** text\n\n",
        "A paragraph with *emphasis*, `code`, a [[wikilink]] and a [link](https://example.com).\n",
        "- [ ] task item with émoji 🎉 and ünïcödé that goes on long enough to wrap once\n",
        "> quoted line 引用文 with 日本語のテキストが続きます and more words after it\r\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "Plain prose continues here without anything special at all.\n\n",
        "A long paragraph line, as in prose written without hard breaks: it runs on \
         for several hundred characters, so soft wrap breaks it into a few rows at \
         every width, with words of different lengths, punctuation, and the odd \
         extraordinarily-long-hyphenated-compound that has to be broken somewhere.\n",
    ];
    let mut text = String::with_capacity(len + 512);
    let mut i = 0usize;
    while text.len() < len {
        text.push_str(PIECES[i % PIECES.len()]);
        i = i.wrapping_mul(31).wrapping_add(7);
    }
    text
}

/// An editor with the caret at the end of the line in the middle of
/// `text`, wrapped at 80 columns or not.
pub fn make_editor(text: &str, wrapped: bool) -> Editor {
    let mut editor = Editor::new(Buffer::new(text));
    if wrapped {
        editor.set_wrap_width(Some(COLUMNS));
    }
    editor.set_cursor(text.len() / 2);
    editor.move_selections(Motion::LineEnd, false);
    editor
}

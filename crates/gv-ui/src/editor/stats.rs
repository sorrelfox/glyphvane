// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! What the status bar shows about the Source view (SPEC § Status bar, 07
//! § Statistics): the primary caret's line and column, the characters
//! selected, the word count, the file's encoding and line endings.
//!
//! - The caret's place is computed when the status bar asks and kept until
//!   the text or the selections change.
//! - Words are counted as Preview counts them (the rendered text, by
//!   UAX #29 words, CJK per character) on the background executor, after
//!   each change; a large document waits until edits pause
//!   ([`COUNT_DELAY`]). Until a count lands the last one is shown.

use std::cell::RefCell;
use std::time::Duration;

use gpui::{AppContext as _, Context, Task};
use gv_editor::{Editor, Selections};
use gv_model::DocModel;
use gv_render_tree::{AllPending, CopyOptions, NoImageSizes, RenderOptions, RenderTree, Sources};
use gv_syntax::{ParseOptions, SyntaxTree};
use gv_text::{Bom, Encoding, LineEnding, LineEndingStyle, Revision, TextFormat, TextSnapshot};

use super::EditorView;

/// Documents up to this size are counted at once after each change.
const COUNT_AT_ONCE: usize = 256 * 1024;

/// How long a larger document waits for edits to pause before it is
/// counted.
pub(super) const COUNT_DELAY: Duration = Duration::from_millis(300);

/// The Source view's facts for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorFacts {
    /// The primary caret's line, from 1.
    pub line: usize,
    /// The primary caret's column in characters, from 1.
    pub column: usize,
    /// Characters selected, over every selection.
    pub selected: usize,
    /// Words, as last counted; `None` before the first count.
    pub words: Option<usize>,
    /// `UTF-8`, `UTF-8 BOM`, `UTF-16 LE`, `windows-1252`, …
    pub encoding: &'static str,
    /// `LF`, `CRLF`, `CR` or `Mixed`.
    pub line_endings: &'static str,
}

/// The primary caret's place and the selected characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Caret {
    line: usize,
    column: usize,
    selected: usize,
}

/// The status bar's view of the editor: the caret's place for one text
/// revision and selection state, and the word count.
#[derive(Default)]
pub(super) struct Stats {
    caret: RefCell<Option<(Revision, Selections, Caret)>>,
    /// The last count and the revision it is of.
    words: Option<(Revision, usize)>,
    /// The revision being counted.
    counting: Option<Revision>,
    _count: Option<Task<()>>,
}

impl Stats {
    /// Forgets the document (a new one was loaded).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn caret(&self, editor: &Editor) -> Caret {
        let revision = editor.text().revision();
        let mut cache = self.caret.borrow_mut();
        if let Some((r, s, caret)) = cache.as_ref()
            && *r == revision
            && s == editor.selections()
        {
            return *caret;
        }
        let caret = caret_of(editor);
        *cache = Some((revision, editor.selections().clone(), caret));
        caret
    }
}

/// Where the primary caret is, and how many characters are selected.
fn caret_of(editor: &Editor) -> Caret {
    let text = editor.text();
    let head = editor.selections().primary().head().min(text.len());
    let line = text.line_of_offset(head).unwrap_or(0);
    let start = text.line_start(line).unwrap_or(0);
    let selected = editor
        .selections()
        .iter()
        .map(|s| chars_in(text, s.range()))
        .sum();
    Caret {
        line: line + 1,
        column: chars_in(text, start..head) + 1,
        selected,
    }
}

fn chars_in(text: &TextSnapshot, range: std::ops::Range<usize>) -> usize {
    if range.is_empty() {
        return 0;
    }
    if range.start == 0 && range.end >= text.len() {
        return text.char_count();
    }
    text.chunks(range)
        .map(|chunks| chunks.map(|c| c.chars().count()).sum())
        .unwrap_or(0)
}

/// The encoding as the status bar names it.
pub(crate) fn encoding_label(format: TextFormat) -> &'static str {
    match (format.encoding, format.bom) {
        (Encoding::Utf8, Bom::Absent) => "UTF-8",
        (Encoding::Utf8, Bom::Present) => "UTF-8 BOM",
        (Encoding::Utf16Le, _) => "UTF-16 LE",
        (Encoding::Utf16Be, _) => "UTF-16 BE",
        (Encoding::Legacy(legacy), _) => legacy.name(),
    }
}

/// The line endings as the status bar names them: the text's own, or for
/// a text without line breaks the ones a new line gets.
pub(crate) fn line_endings_label(text: &TextSnapshot, format: TextFormat) -> &'static str {
    let name = |ending| match ending {
        LineEnding::Lf => "LF",
        LineEnding::CrLf => "CRLF",
        LineEnding::Cr => "CR",
    };
    match text.line_ending_stats().style() {
        LineEndingStyle::None => name(format.line_ending),
        LineEndingStyle::Uniform(ending) => name(ending),
        LineEndingStyle::Mixed { .. } => "Mixed",
    }
}

/// The words of `text` as Preview counts them: the plain text of its
/// rendered document. `tree` is the parse of `text`, if there is one.
fn document_words(tree: Option<SyntaxTree>, text: &TextSnapshot, options: ParseOptions) -> usize {
    let tree = tree.unwrap_or_else(|| SyntaxTree::parse_with(text, options));
    let model = DocModel::build(&tree, text);
    let sources = Sources {
        links: &AllPending,
        images: &NoImageSizes,
    };
    let rendered = RenderTree::build(&tree, &model, text, RenderOptions::commonmark(), sources);
    let opts = CopyOptions {
        list_markers: false,
        blank_lines: false,
    };
    crate::shell::count_words(&rendered.plain_text(0..rendered.len(), opts))
}

impl EditorView {
    /// The facts the status bar shows in Edit and Split mode, once the
    /// document is loaded.
    pub fn status_facts(&self) -> Option<EditorFacts> {
        let doc = self.doc.as_ref()?;
        let editor = &doc.editor;
        let caret = self.stats.caret(editor);
        let format = editor.buffer().format();
        Some(EditorFacts {
            line: caret.line,
            column: caret.column,
            selected: caret.selected,
            words: self.stats.words.map(|(_, n)| n),
            encoding: encoding_label(format),
            line_endings: line_endings_label(editor.text(), format),
        })
    }

    /// Counts the words of the current text, unless they are counted or
    /// being counted.
    pub(super) fn schedule_word_count(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let text = doc.editor.snapshot();
        let revision = text.revision();
        let counted = self.stats.words.is_some_and(|(r, _)| r == revision);
        if counted || self.stats.counting == Some(revision) {
            return;
        }
        let (tree, parsed) = doc.syntax.shown();
        let tree = (parsed.revision() == revision && !tree.is_interim()).then(|| tree.clone());
        let options = self.options;
        let delay = (text.len() > COUNT_AT_ONCE).then_some(COUNT_DELAY);
        self.stats.counting = Some(revision);
        // A new count replaces the one waiting or running, which is dropped.
        self.stats._count = Some(cx.spawn(async move |this, cx| {
            if let Some(delay) = delay {
                cx.background_executor().timer(delay).await;
            }
            let words = cx
                .background_spawn(async move { document_words(tree, &text, options) })
                .await;
            this.update(cx, |view, cx| {
                if view.stats.counting == Some(revision) {
                    view.stats.counting = None;
                }
                view.stats.words = Some((revision, words));
                cx.notify();
            })
            .ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use gv_text::{Buffer, decode};

    use super::*;

    #[test]
    fn encodings_and_line_endings_have_labels() {
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("a\r\nb\r\n".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let decoded = decode(&utf16).expect("text");
        let buffer = Buffer::from_decoded(&decoded);
        assert_eq!(encoding_label(buffer.format()), "UTF-16 LE");
        assert_eq!(line_endings_label(buffer.text(), buffer.format()), "CRLF");

        let bom = decode(b"\xEF\xBB\xBFa\nb\r\n").expect("text");
        let buffer = Buffer::from_decoded(&bom);
        assert_eq!(encoding_label(buffer.format()), "UTF-8 BOM");
        assert_eq!(line_endings_label(buffer.text(), buffer.format()), "Mixed");

        let plain = Buffer::new("one line");
        assert_eq!(encoding_label(plain.format()), "UTF-8");
        assert_eq!(line_endings_label(plain.text(), plain.format()), "LF");
        let cr = Buffer::new("a\rb\r");
        assert_eq!(line_endings_label(cr.text(), cr.format()), "CR");
    }

    #[test]
    fn words_are_counted_as_preview_counts_them() {
        let text = TextSnapshot::new("# Title\n\n- **bold** [link](https://x.org/a/b)\n");
        // The URL and the markers are not words in the rendered text.
        assert_eq!(document_words(None, &text, ParseOptions::default()), 3);
    }
}

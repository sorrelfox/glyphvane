// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Folding in the Source view (07 § Folding, SPEC § Editor).
//!
//! The folds live in the editor's replace layer ([`gv_editor::Editor`]
//! moves carets out of them, skips them in motions and unfolds them when a
//! caret lands inside). This module is the view's side:
//!
//! - **Visual lines.** A display line of the replace layer: a buffer line,
//!   and when folds start on it, the placeholder of each fold and the rest
//!   of the line the fold ends on ([`VisualLine`]). Rows are shaped per
//!   visual line, in *columns* of its content: the buffer text of its
//!   pieces with the placeholder `⋯` between them.
//! - **Placeholders** are drawn as a chip: `surface.sunken` (`state.press`
//!   on the code fill and the current line, where `surface.sunken` would
//!   not show), radius `radius::CHIP`, the `⋯` in `text.secondary` with a
//!   space of room on each side (the wrap measure counts that room too).
//!   Clicking a chip unfolds it. (Not designed: built from the inline-code
//!   chip.)
//! - **Chevrons.** In the Split view the gutter shows a 10 px chevron in
//!   `text.tertiary` beside the line number of every line where a region
//!   can fold: `chevron-down`, or `chevron-right` while folded (SPEC §
//!   Editor, screens `split.*`). Clicking one folds or unfolds. Edit mode
//!   has no chevrons, as designed; its folds come from the commands.
//! - **Commands** (View menu and palette): Fold and Unfold at the carets,
//!   Fold all, Unfold all, and Fold to heading level 1 to 3 (the headings
//!   down to that level stay visible).
//! - **Foldable regions** are found from the syntax tree when a document
//!   loads and, after edits, once the complete tree has caught up with the
//!   text and the text has rested for [`FOLDABLE_DELAY`] (a fold command
//!   finds them at once); edits move the regions found before in the
//!   meantime.
//!
//! Folds belong to the tab's editor, so they stay through mode switches.
//! They are not saved with the session yet.

use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{Context, Pixels, Point, Window};
use gv_editor::SyntaxRef;
use gv_editor::display_map::{PLACEHOLDER, ReplaceSnapshot};
use gv_text::{Bias, Point as BufferPoint, TextSnapshot};

use super::highlight::{HighlightCache, LineStyle, Style};
use super::wrap::RowMap;
use super::{EditorLayout, EditorView, LINE};
use crate::inline::Token;
use crate::metrics::size::split;

/// How long the text must rest after a reparse before the foldable regions
/// are found again.
pub(crate) const FOLDABLE_DELAY: Duration = Duration::from_millis(200);

/// With `GLYPHVANE_DEBUG_FOLD=LINE,LINE,…` (1-based line numbers) the
/// first Source view that shows a document folds the region on each of
/// those lines, for screenshots.
pub const DEBUG_FOLD_ENV: &str = "GLYPHVANE_DEBUG_FOLD";

/// One per process: the first Source view that shows a document takes it.
static DEBUG_FOLD_TAKEN: AtomicBool = AtomicBool::new(false);

/// The placeholder's character.
pub(crate) const PLACEHOLDER_CHAR: char = '\u{22ef}';

const _: () = assert!(PLACEHOLDER.len() == PLACEHOLDER_CHAR.len_utf8());

/// A run of buffer text in a visual line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    /// Its buffer bytes.
    buffer: Range<usize>,
    /// Where it starts in the visual line's content.
    at: usize,
    /// Its buffer line and that line's start.
    line: usize,
    line_start: usize,
}

/// A fold placeholder in a visual line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mark {
    /// The `⋯` in the content.
    pub columns: Range<usize>,
    /// The buffer bytes it hides (a run of folds).
    pub buffer: Range<usize>,
}

/// A display line of the replace layer (see the module documentation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VisualLine {
    /// Its first and last buffer lines.
    pub first: usize,
    pub last: usize,
    /// From the start of `first` to the end of `last` (no terminator).
    pub range: Range<usize>,
    /// The text shaped: the pieces with a placeholder between each two.
    pub content: String,
    pieces: Vec<Piece>,
    pub marks: Vec<Mark>,
}

impl VisualLine {
    /// The visual line that starts with buffer line `first` (a line that
    /// starts a row of the replace layer).
    pub fn new(text: &TextSnapshot, replace: &ReplaceSnapshot, first: usize) -> Option<Self> {
        let line_range = text.line_range(first).ok()?;
        if !replace.has_folds() || !replace.is_line_folded(first) {
            let content = text.text_for_range(line_range.clone()).ok()?;
            return Some(Self {
                first,
                last: first,
                range: line_range.clone(),
                content,
                pieces: vec![Piece {
                    buffer: line_range.clone(),
                    at: 0,
                    line: first,
                    line_start: line_range.start,
                }],
                marks: Vec::new(),
            });
        }
        let row = replace
            .to_replace_point(BufferPoint::new(first, 0), Bias::Left)
            .row();
        let mut content = String::new();
        let mut pieces = Vec::new();
        let mut marks = Vec::new();
        let (mut line, mut line_start) = (first, line_range.start);
        let mut pos = line_range.start;
        for placeholder in replace.placeholders(row..row + 1) {
            let hidden = placeholder.buffer_range;
            if hidden.start < pos {
                continue;
            }
            pieces.push(Piece {
                buffer: pos..hidden.start,
                at: content.len(),
                line,
                line_start,
            });
            content.push_str(&text.text_for_range(pos..hidden.start).ok()?);
            let at = content.len();
            content.push_str(PLACEHOLDER);
            marks.push(Mark {
                columns: at..content.len(),
                buffer: hidden.clone(),
            });
            pos = hidden.end;
            line = text.line_of_offset(pos).ok()?;
            line_start = text.line_start(line).ok()?;
        }
        let end = text.line_range(line).ok()?.end.max(pos);
        pieces.push(Piece {
            buffer: pos..end,
            at: content.len(),
            line,
            line_start,
        });
        content.push_str(&text.text_for_range(pos..end).ok()?);
        Some(Self {
            first,
            last: line,
            range: line_range.start..end,
            content,
            pieces,
            marks,
        })
    }

    /// The column of buffer offset `offset`, clamped to the line. An offset
    /// hidden in a fold is the placeholder's start (`Bias::Left`) or end.
    pub fn local(&self, offset: usize, bias: Bias) -> usize {
        if offset <= self.range.start {
            return 0;
        }
        if let Some(piece) = self
            .pieces
            .iter()
            .find(|p| p.buffer.start <= offset && offset <= p.buffer.end)
        {
            return piece.at + offset - piece.buffer.start;
        }
        if let Some(mark) = self
            .marks
            .iter()
            .find(|m| m.buffer.start < offset && offset < m.buffer.end)
        {
            return match bias {
                Bias::Left => mark.columns.start,
                Bias::Right => mark.columns.end,
            };
        }
        self.content.len()
    }

    /// The buffer offset of column `column` (a column inside a placeholder
    /// is the start of the text it hides).
    pub fn to_buffer(&self, column: usize) -> usize {
        if let Some(piece) = self
            .pieces
            .iter()
            .find(|p| p.at <= column && column <= p.at + p.buffer.len())
        {
            return piece.buffer.start + column - piece.at;
        }
        self.marks
            .iter()
            .find(|m| m.columns.contains(&column))
            .map_or(self.range.end, |m| m.buffer.start)
    }

    /// Whether caret offset `offset` is on this line.
    pub fn holds(&self, offset: usize) -> bool {
        self.range.start <= offset && offset <= self.range.end
    }

    /// The placeholders' columns.
    pub fn mark_columns(&self) -> Vec<Range<usize>> {
        self.marks.iter().map(|m| m.columns.clone()).collect()
    }

    /// The buffer lines whose highlighting the line shows.
    pub fn styled_lines(&self) -> impl Iterator<Item = usize> + '_ {
        self.pieces
            .iter()
            .enumerate()
            .filter(|(i, p)| *i == 0 || !p.buffer.is_empty())
            .map(|(_, p)| p.line)
    }

    /// The highlighting of the line: that of each piece's buffer line,
    /// moved to the piece's columns, and `text.secondary` for the
    /// placeholders. The buffer line's own (cached) style when nothing is
    /// folded.
    pub fn style(&self, highlights: &HighlightCache) -> Option<Rc<LineStyle>> {
        if self.marks.is_empty() {
            return highlights.get(self.first).cloned();
        }
        let mut runs: Vec<(Range<usize>, Style)> = Vec::new();
        let mut code = false;
        for (i, piece) in self.pieces.iter().enumerate() {
            let from = piece.buffer.start - piece.line_start;
            let to = piece.buffer.end - piece.line_start;
            let style = highlights.get(piece.line);
            if i == 0 {
                code = style.is_some_and(|s| s.code);
            }
            let line_runs = style.map(|s| s.clamped(to)).unwrap_or_else(|| {
                if to > 0 {
                    vec![(0..to, Style::PLAIN)]
                } else {
                    Vec::new()
                }
            });
            for (r, s) in line_runs {
                let (a, b) = (r.start.max(from), r.end.min(to));
                if a < b {
                    runs.push((piece.at + a - from..piece.at + b - from, s));
                }
            }
            if let Some(mark) = self.marks.get(i) {
                runs.push((mark.columns.clone(), Style::of(Token::TextSecondary)));
            }
        }
        Some(Rc::new(LineStyle { runs, code }))
    }
}

/// What a press on a fold control does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FoldHit {
    /// A chevron: fold the region on buffer line `line`, or unfold the
    /// placeholders of its row.
    Chevron {
        line: usize,
        folded: Vec<Range<usize>>,
    },
    /// A placeholder chip: unfold the folds it hides.
    Chip(Range<usize>),
}

/// The fold commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FoldCommand {
    /// Fold at each caret's line.
    Fold,
    /// Unfold at each caret's line.
    Unfold,
    FoldAll,
    UnfoldAll,
    /// Keep the headings down to this level visible, fold their sections.
    Level(u8),
}

impl EditorView {
    /// Finds the foldable regions in the tree, if it is the complete parse
    /// of the current text and the regions are older.
    pub(super) fn refresh_foldable(&mut self) {
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let revision = doc.editor.text().revision();
        if doc.foldable_at == Some(revision) {
            return;
        }
        let (tree, text) = doc.syntax.shown();
        let syntax = SyntaxRef::new(tree, text);
        if syntax.is_parse_of(doc.editor.text()) {
            doc.editor.update_foldable(syntax);
            doc.foldable_at = Some(revision);
        }
    }

    /// After a reparse: finds the foldable regions again once the text has
    /// rested for [`FOLDABLE_DELAY`] (a later reparse starts the wait
    /// again).
    pub(super) fn schedule_foldable(&mut self, cx: &mut Context<'_, Self>) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        if doc.foldable_at == Some(doc.editor.text().revision()) {
            return;
        }
        self._foldable = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FOLDABLE_DELAY).await;
            this.update(cx, |view, cx| {
                view._foldable = None;
                view.refresh_foldable();
                cx.notify();
            })
            .ok();
        }));
    }

    /// The folds of [`DEBUG_FOLD_ENV`], once per process, when a document
    /// shows.
    pub(super) fn debug_fold(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if DEBUG_FOLD_TAKEN.load(Ordering::Relaxed) {
            return;
        }
        let Ok(value) = std::env::var(DEBUG_FOLD_ENV) else {
            DEBUG_FOLD_TAKEN.store(true, Ordering::Relaxed);
            return;
        };
        if DEBUG_FOLD_TAKEN.swap(true, Ordering::Relaxed) {
            return;
        }
        self.refresh_foldable();
        let lines: Vec<usize> = value
            .split(',')
            .filter_map(|n| n.trim().parse::<usize>().ok()?.checked_sub(1))
            .collect();
        let changed = self
            .keep_top_line(|editor| {
                lines
                    .into_iter()
                    .fold(false, |any, line| editor.fold_at(line) | any)
            })
            .unwrap_or(false);
        if changed {
            self.folds_changed(false, window, cx);
        }
    }

    /// The visual line at caret offset `offset` with the terminator of its
    /// last line: what selecting a line selects, folded text included.
    pub(super) fn visual_line_at(&self, offset: usize) -> Option<Range<usize>> {
        let editor = self.editor()?;
        let text = editor.text();
        let line = text.line_of_offset(offset).ok()?;
        let start = RowMap::of(editor).visual_start(line);
        let visual = VisualLine::new(text, editor.display().replace_snapshot(), start)?;
        let end = text.line_range_with_terminator(visual.last).ok()?.end;
        Some(visual.range.start..end)
    }

    /// Runs a fold command.
    pub(crate) fn fold_command(
        &mut self,
        command: FoldCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.refresh_foldable();
        let changed = self
            .keep_top_line(|editor| {
                let lines: Vec<usize> = {
                    let text = editor.text();
                    let mut lines: Vec<usize> = editor
                        .selections()
                        .iter()
                        .filter_map(|s| text.line_of_offset(s.head()).ok())
                        .collect();
                    lines.dedup();
                    lines
                };
                match command {
                    FoldCommand::Fold => lines
                        .into_iter()
                        .fold(false, |any, line| editor.fold_at(line) | any),
                    FoldCommand::Unfold => lines
                        .into_iter()
                        .fold(false, |any, line| editor.unfold_at(line) | any),
                    FoldCommand::FoldAll => editor.fold_all(),
                    FoldCommand::UnfoldAll => editor.unfold_all(),
                    FoldCommand::Level(level) => {
                        let unfolded = editor.unfold_headings(1);
                        editor.fold_headings(level) | unfolded
                    }
                }
            })
            .unwrap_or(false);
        if changed {
            self.folds_changed(true, window, cx);
        }
    }

    /// After the folds changed: redraw, and bring the caret into view when
    /// asked to.
    fn folds_changed(&mut self, to_caret: bool, window: &mut Window, cx: &mut Context<'_, Self>) {
        if to_caret {
            self.autoscroll = true;
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }

    /// Runs a press on a fold control.
    pub(super) fn press_fold(
        &mut self,
        hit: FoldHit,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let changed = self
            .keep_top_line(|editor| match hit {
                FoldHit::Chevron { line, folded } if folded.is_empty() => editor.fold_at(line),
                FoldHit::Chevron { folded, .. } => editor.unfold_ranges(folded, false),
                FoldHit::Chip(hidden) => editor.unfold_ranges([hidden], false),
            })
            .unwrap_or(false);
        if changed {
            self.folds_changed(false, window, cx);
        }
    }

    /// The fold control under window point `position`: a chevron in the
    /// Split view's gutter, or a placeholder chip.
    pub(super) fn fold_hit(
        &mut self,
        position: Point<Pixels>,
        window: &Window,
        cx: &gpui::App,
    ) -> Option<FoldHit> {
        let columns = self.columns(None, window)?;
        let (rows, row) = {
            let map = self.row_map()?;
            let y = f64::from(f32::from(position.y) - columns.top) + self.scroll_y;
            if y < 0.0 {
                return None;
            }
            (map.row_count(), (y / f64::from(LINE)).floor() as usize)
        };
        if row >= rows {
            return None;
        }
        let line = self.row_map()?.line_of_row(row);
        let x = f32::from(position.x);
        if self.layout == EditorLayout::Pane
            && x >= columns.numbers_right
            && x < columns.numbers_right + chevron_slot()
        {
            let (first, vl, _) = self.line_row(line, window, cx)?;
            if first != row {
                return None;
            }
            let folded: Vec<Range<usize>> = vl.marks.iter().map(|m| m.buffer.clone()).collect();
            let editor = self.editor()?;
            if folded.is_empty() && !editor.is_foldable(vl.first) {
                return None;
            }
            return Some(FoldHit::Chevron {
                line: vl.first,
                folded,
            });
        }
        if x < columns.text_x {
            return None;
        }
        let (first, vl, shaped) = self.line_row(line, window, cx)?;
        let segment = row.checked_sub(first)?;
        let x = x - self.text_origin_x(&columns);
        vl.marks.iter().find_map(|mark| {
            let (a, b) = chip_span(&shaped, segment, mark)?;
            (a <= x && x < b).then(|| FoldHit::Chip(mark.buffer.clone()))
        })
    }
}

/// The width of the Split gutter's chevron slot: from the line numbers to
/// the end of the gutter.
pub(crate) fn chevron_slot() -> f32 {
    split::GUTTER - split::LINE_NUMBER_WIDTH
}

/// The x span (from the left of the text) of placeholder `mark`'s chip in
/// segment `segment` of `row`, if the placeholder is on that segment.
pub(crate) fn chip_span(row: &super::rows::Row, segment: usize, mark: &Mark) -> Option<(f32, f32)> {
    use crate::metrics::size::editor::FOLD_CHIP_INSET_X;
    if row.segment_of(mark.columns.start) != segment {
        return None;
    }
    let x0 = row.x_in(segment, mark.columns.start);
    let x1 = row.x_in(segment, mark.columns.end);
    Some((
        x0 + FOLD_CHIP_INSET_X,
        (x1 - FOLD_CHIP_INSET_X).max(x0 + 1.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gv_editor::Editor;
    use gv_text::Buffer;

    fn folded(text: &str, ranges: impl IntoIterator<Item = Range<usize>>) -> Editor {
        let mut editor = Editor::new(Buffer::new(text));
        editor.fold_ranges(ranges);
        editor
    }

    fn line(editor: &Editor, first: usize) -> VisualLine {
        VisualLine::new(editor.text(), editor.display().replace_snapshot(), first).expect("a line")
    }

    #[test]
    fn an_unfolded_line_is_its_buffer_line() {
        let editor = folded("one\ntwo\n", None);
        let vl = line(&editor, 1);
        assert_eq!((vl.first, vl.last, vl.range.clone()), (1, 1, 4..7));
        assert_eq!(vl.content, "two");
        assert_eq!(vl.local(5, Bias::Left), 1);
        assert_eq!(vl.to_buffer(3), 7);
    }

    #[test]
    fn a_fold_joins_its_lines_with_a_placeholder() {
        // "# A" folds to the end of "b"; "after" stays on the row.
        let text = "# A\nb\nc\n# B\n";
        let editor = folded(text, Some(3..7));
        let vl = line(&editor, 0);
        assert_eq!((vl.first, vl.last), (0, 2));
        assert_eq!(vl.content, "# A⋯");
        assert_eq!(vl.range, 0..7);
        assert_eq!(vl.marks[0].columns, 3..6);
        assert_eq!(vl.marks[0].buffer, 3..7);
        // Ends of the fold are its placeholder's ends.
        assert_eq!(vl.local(3, Bias::Left), 3);
        assert_eq!(vl.local(7, Bias::Left), 6);
        // Hidden offsets go to either end.
        assert_eq!(vl.local(5, Bias::Left), 3);
        assert_eq!(vl.local(5, Bias::Right), 6);
        assert_eq!(vl.to_buffer(6), 7);
        assert_eq!(vl.to_buffer(4), 3);
        // The next visual line starts after the fold.
        assert_eq!(line(&editor, 3).content, "# B");
    }

    #[test]
    fn text_after_a_fold_is_on_its_row() {
        let text = "ab\ncd\nef\n";
        let editor = folded(text, [1..4, 7..8]);
        let vl = line(&editor, 0);
        assert_eq!(vl.content, "a⋯d");
        assert_eq!(vl.marks.len(), 1, "the second fold is on another row");
        let editor = folded(text, [1..4, 4..5]);
        let vl = line(&editor, 0);
        assert_eq!(vl.content, "a⋯⋯");
        assert_eq!(vl.marks.len(), 2, "touching folds keep two placeholders");
        assert_eq!(vl.last, 1);
        assert_eq!(vl.local(4, Bias::Left), 4, "between the placeholders");
    }

    #[test]
    fn styles_move_with_their_pieces() {
        let text = "ab\ncd\n";
        let editor = folded(text, Some(1..4));
        let vl = line(&editor, 0);
        let style = vl.style(&HighlightCache::default()).expect("a style");
        assert_eq!(
            style.runs,
            vec![
                (0..1, Style::PLAIN),
                (1..4, Style::of(Token::TextSecondary)),
                (4..5, Style::PLAIN),
            ]
        );
    }
}

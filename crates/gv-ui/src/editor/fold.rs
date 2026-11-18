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
//!   visual line from the display map's chunks of its row
//!   ([`super::rows`]), in *columns* of the tab layer's row.
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
//! The session keeps them per tab as byte ranges ([`EditorView::fold_ranges`]);
//! a restored tab gets them back with [`EditorView::restore_folds`] once its
//! document has loaded and its foldable regions are found: only ranges that
//! are still foldable regions fold, and none if the text was edited first.

use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{Context, Pixels, Point, Window};
use gv_editor::display_map::{Highlight, PLACEHOLDER, ReplaceSnapshot, StyleId};
use gv_editor::{Editor, SyntaxRef};
use gv_text::{Bias, Point as BufferPoint, TextSnapshot};

use super::highlight::{HighlightCache, LineStyle, Style};
use super::wrap::RowMap;
use super::{EditorLayout, EditorView};
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
    /// Its buffer line and that line's start.
    line: usize,
    line_start: usize,
}

/// A fold placeholder in a visual line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mark {
    /// The `⋯`'s byte columns in the replace layer's row.
    pub input: Range<usize>,
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
    /// Its row in the replace and tab layers.
    pub row: u32,
    /// The buffer text it shows, between the placeholders.
    pieces: Vec<Piece>,
    pub marks: Vec<Mark>,
}

/// The highlighting a visual line is shaped with: the cached style of each
/// buffer line it shows, as highlights over buffer bytes for the display
/// map's chunks, with the styles their ids stand for.
#[derive(Debug, Clone, Default)]
pub(crate) struct LineStyles {
    /// The cached style of each piece's line (what a cached row is checked
    /// against).
    pub lines: Vec<Option<Rc<LineStyle>>>,
    /// A fenced code line: drawn on the code fill.
    pub code: bool,
}

impl LineStyles {
    /// Whether two are the same highlighting.
    pub fn same(&self, other: &Self) -> bool {
        self.code == other.code
            && self.lines.len() == other.lines.len()
            && self.lines.iter().zip(&other.lines).all(|pair| match pair {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b) || **a == **b,
                (None, None) => true,
                _ => false,
            })
    }
}

impl VisualLine {
    /// The visual line that starts with buffer line `first` (a line that
    /// starts a row of the replace layer).
    pub fn new(text: &TextSnapshot, replace: &ReplaceSnapshot, first: usize) -> Option<Self> {
        let line_range = text.line_range(first).ok()?;
        let plain = !replace.has_folds() && replace.replacements().is_empty();
        if plain || !replace.is_line_folded(first) {
            let row = if plain {
                u32::try_from(first).ok()?
            } else {
                replace
                    .to_replace_point(BufferPoint::new(first, 0), Bias::Left)
                    .row()
            };
            return Some(Self {
                first,
                last: first,
                range: line_range.clone(),
                row,
                pieces: vec![Piece {
                    buffer: line_range.clone(),
                    line: first,
                    line_start: line_range.start,
                }],
                marks: Vec::new(),
            });
        }
        let row = replace
            .to_replace_point(BufferPoint::new(first, 0), Bias::Left)
            .row();
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
                line,
                line_start,
            });
            marks.push(Mark {
                input: placeholder.range.start.column() as usize
                    ..placeholder.range.end.column() as usize,
                buffer: hidden.clone(),
            });
            pos = hidden.end;
            line = text.line_of_offset(pos).ok()?;
            line_start = text.line_start(line).ok()?;
        }
        let end = text.line_range(line).ok()?.end.max(pos);
        pieces.push(Piece {
            buffer: pos..end,
            line,
            line_start,
        });
        Some(Self {
            first,
            last: line,
            range: line_range.start..end,
            row,
            pieces,
            marks,
        })
    }

    /// Whether caret offset `offset` is on this line.
    pub fn holds(&self, offset: usize) -> bool {
        self.range.start <= offset && offset <= self.range.end
    }

    /// The buffer lines whose highlighting the line shows.
    pub fn styled_lines(&self) -> impl Iterator<Item = usize> + '_ {
        self.pieces
            .iter()
            .enumerate()
            .filter(|(i, p)| *i == 0 || !p.buffer.is_empty())
            .map(|(_, p)| p.line)
    }

    /// The cached highlighting of the buffer lines the line shows.
    pub fn styles(&self, highlights: &HighlightCache) -> LineStyles {
        let lines: Vec<Option<Rc<LineStyle>>> = self
            .pieces
            .iter()
            .map(|piece| highlights.get(piece.line).cloned())
            .collect();
        let code = lines
            .first()
            .is_some_and(|s| s.as_ref().is_some_and(|s| s.code));
        LineStyles { lines, code }
    }

    /// The highlights of `styles` over the buffer text the line shows,
    /// with the styles their ids stand for (id `i` is `styles[i]`). Text
    /// without a highlight is plain.
    pub fn highlights(&self, styles: &LineStyles) -> (Vec<Highlight>, Vec<Style>) {
        let mut highlights: Vec<Highlight> = Vec::new();
        let mut table: Vec<Style> = Vec::new();
        for (piece, style) in self.pieces.iter().zip(&styles.lines) {
            let Some(style) = style else {
                continue;
            };
            let from = piece.buffer.start - piece.line_start;
            let to = piece.buffer.end - piece.line_start;
            for (r, s) in style.clamped(to) {
                let (a, b) = (r.start.max(from), r.end.min(to));
                if a >= b || s == Style::PLAIN {
                    continue;
                }
                let id = match table.iter().position(|t| *t == s) {
                    Some(id) => id,
                    None => {
                        table.push(s);
                        table.len() - 1
                    }
                };
                highlights.push(Highlight {
                    range: piece.line_start + a..piece.line_start + b,
                    style: StyleId(u32::try_from(id).unwrap_or(u32::MAX)),
                });
            }
        }
        (highlights, table)
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
        // Plain text has no folds.
        let Some(doc) = self.doc.as_mut().filter(|_| self.plain.is_none()) else {
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
        let Some(doc) = self.doc.as_ref().filter(|_| self.plain.is_none()) else {
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

    /// The folded byte ranges, for the session.
    pub fn fold_ranges(&self) -> Vec<Range<usize>> {
        if self.status != super::EditorStatus::Ready {
            return Vec::new();
        }
        self.editor().map(Editor::folded_ranges).unwrap_or_default()
    }

    /// Folds `ranges` (from [`Self::fold_ranges`] of an earlier session)
    /// once the document has loaded; see the module notes.
    pub fn restore_folds(&mut self, ranges: Vec<Range<usize>>) {
        self.restored_folds = (!ranges.is_empty()).then_some((ranges, None));
    }

    /// The folds of [`Self::restore_folds`], when the foldable regions of
    /// the loaded text are known.
    pub(super) fn apply_restored_folds(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.restored_folds.is_none() {
            return;
        }
        self.refresh_foldable();
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let revision = doc.editor.text().revision();
        let Some((ranges, seen)) = self.restored_folds.as_mut() else {
            return;
        };
        if seen.is_some_and(|seen| seen != revision) {
            // Edited before the regions were found: the ranges are stale.
            self.restored_folds = None;
            return;
        }
        *seen = Some(revision);
        if doc.foldable_at != Some(revision) {
            return;
        }
        let ranges = std::mem::take(ranges);
        self.restored_folds = None;
        let foldable: std::collections::HashSet<Range<usize>> =
            doc.editor.foldable().into_iter().map(|f| f.range).collect();
        let ranges: Vec<Range<usize>> = ranges
            .into_iter()
            .filter(|r| foldable.contains(r))
            .collect();
        if ranges.is_empty() {
            return;
        }
        let changed = self
            .keep_top_line(|editor| editor.fold_ranges(ranges))
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
            let line = f64::from(self.metrics.line);
            (map.row_count(), (y / line).floor() as usize)
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
        let inset = self.metrics.chip_inset;
        vl.marks.iter().find_map(|mark| {
            let (a, b) = chip_span(&shaped, segment, mark, inset)?;
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
/// segment `segment` of `row`, if the placeholder is on that segment; the
/// chip is `inset` in from the ends of the `⋯` and its spaces.
pub(crate) fn chip_span(
    row: &super::rows::Row,
    segment: usize,
    mark: &Mark,
    inset: f32,
) -> Option<(f32, f32)> {
    let (start, end) = (
        row.text.column_of_input(mark.input.start),
        row.text.column_of_input(mark.input.end),
    );
    if row.segment_of(start) != segment {
        return None;
    }
    let x0 = row.x_in(segment, start);
    let x1 = row.x_in(segment, end);
    Some((x0 + inset, (x1 - inset).max(x0 + 1.0)))
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
        assert_eq!(
            (vl.first, vl.last, vl.range.clone(), vl.row),
            (1, 1, 4..7, 1)
        );
        assert!(vl.marks.is_empty());
        assert_eq!(vl.styled_lines().collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn a_fold_joins_its_lines_with_a_placeholder() {
        // "# A" folds to the end of "c"; the next line starts a row.
        let text = "# A\nb\nc\n# B\n";
        let editor = folded(text, Some(3..7));
        let vl = line(&editor, 0);
        assert_eq!((vl.first, vl.last, vl.row), (0, 2, 0));
        assert_eq!(vl.range, 0..7);
        assert_eq!(vl.marks[0].input, 3..6);
        assert_eq!(vl.marks[0].buffer, 3..7);
        let next = line(&editor, 3);
        assert_eq!((next.range.clone(), next.row), (8..11, 1));
    }

    #[test]
    fn text_after_a_fold_is_on_its_row() {
        let text = "ab\ncd\nef\n";
        let editor = folded(text, [1..4, 7..8]);
        let vl = line(&editor, 0);
        assert_eq!(vl.marks.len(), 1, "the second fold is on another row");
        assert_eq!(vl.styled_lines().collect::<Vec<_>>(), [0, 1]);
        let editor = folded(text, [1..4, 4..5]);
        let vl = line(&editor, 0);
        assert_eq!(vl.marks.len(), 2, "touching folds keep two placeholders");
        assert_eq!(vl.marks[1].input, 4..7);
        assert_eq!(vl.last, 1);
    }

    #[test]
    fn highlights_cover_the_pieces_shown() {
        let text = "ab\ncd\n";
        let editor = folded(text, Some(1..4));
        let vl = line(&editor, 0);
        let mut cache = HighlightCache::default();
        let bold = Style {
            bold: true,
            ..Style::PLAIN
        };
        cache.insert_for_test(
            0,
            LineStyle {
                runs: vec![(0..1, bold), (1..2, Style::PLAIN)],
                code: true,
            },
        );
        cache.insert_for_test(
            1,
            LineStyle {
                runs: vec![(0..1, Style::MARKER), (1..2, bold)],
                code: false,
            },
        );
        let styles = vl.styles(&cache);
        assert!(styles.code, "the first line's");
        let (highlights, table) = vl.highlights(&styles);
        // "a" bold; "c" folded away; "d" bold.
        assert_eq!(table, [bold]);
        let ranges: Vec<Range<usize>> = highlights.iter().map(|h| h.range.clone()).collect();
        assert_eq!(ranges, [0..1, 4..5]);
        assert!(styles.same(&vl.styles(&cache)));
        assert!(!styles.same(&LineStyles::default()));
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view's text for assistive technology (NFR-A11Y-06).
//!
//! The editor's element (its focused `div`) is a multi-line text input
//! whose children are AccessKit text runs: one per display row (soft
//! wrap gives a line several), split at [`MAX_RUN`] characters, for the
//! lines on screen, a margin of lines above and below them (one screen,
//! [`MARGIN`] lines at least and [`MAX_MARGIN`] at most, so that reading
//! by line goes on past the screen's edges) and the primary caret's line
//! when it is outside them.
//! A run's characters are grapheme clusters of what the row shows: Live
//! Preview's hidden markers are not there, a hard tab is one `\t`, and the
//! last row of a line ends in `\n` (one character, as AccessKit wants).
//! Each character keeps the buffer offset it starts at, so the node's
//! text selection (the primary selection's anchor and focus) points into
//! the runs, and a screen reader's `SetTextSelection` maps back to buffer
//! offsets. A selection the screen reader puts outside the screen
//! scrolls the caret into view like any other caret move, and its
//! `ScrollIntoView`/`ScrollToPoint` on a run and `ScrollUp`/`ScrollDown`
//! on the input scroll the view.
//!
//! Live Preview: an image's run text is its alt text (or its file name),
//! inline math's its TeX; a screen reader's selection inside a block's
//! nodes puts the caret at the block's source, which shows it as text.
//! Several carets: the primary one is the selection, and the node's
//! description says how many there are.
//!
//! Built only while assistive technology is connected (and in tests):
//! the element fills a [`SharedSource`] each frame, the focused `div`
//! hangs the nodes on itself after its children are prepainted.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{A11ySubtreeBuilder, Bounds, Pixels, Role, accesskit, point, px, size};
use gv_editor::display_map::{ReplaceSnapshot, Replaced, WidgetId};
use gv_text::{Bias, TextSnapshot};
use unicode_segmentation::UnicodeSegmentation as _;

use super::rows::Row;
use crate::a11y::document::DocNode;

/// The most characters in one run: word starts are `u8` indices.
pub(crate) const MAX_RUN: usize = 255;

/// The longest character, in bytes (character lengths are `u8`).
const MAX_CHAR: usize = 255;

/// A box widget's text (an image, inline math) is the object replacement
/// character; screen readers take that for an embedded object, so it is
/// reported as what the widget stands for (its alt text, its TeX), or as
/// a space (a task's checkbox).
const OBJECT_REPLACEMENT: &str = "\u{fffc}";

/// The fewest and the most lines described above and below the screen.
pub(crate) const MARGIN: usize = 50;
pub(crate) const MAX_MARGIN: usize = 200;

/// How many lines to describe above and below `on_screen` lines.
pub(crate) fn margin(on_screen: usize) -> usize {
    on_screen.clamp(MARGIN, MAX_MARGIN)
}

/// One text run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Run {
    /// Stable while the line stays: its first buffer line, its row in the
    /// line and its piece in the row.
    pub key: (usize, usize, usize),
    pub text: String,
    /// Each character's length in bytes of `text`.
    pub lengths: Vec<u8>,
    /// The characters that start words.
    pub word_starts: Vec<u8>,
    /// Each character's x and advance, from the run's left, in px.
    pub positions: Vec<f32>,
    pub widths: Vec<f32>,
    /// The buffer offset each character starts at, and the run's end.
    pub offsets: Vec<usize>,
    /// In window px.
    pub bounds: Bounds<Pixels>,
    /// The run goes on in the next run on the same row (it was split).
    pub next_on_line: bool,
    pub previous_on_line: bool,
}

impl Run {
    pub fn len(&self) -> usize {
        self.lengths.len()
    }
}

/// A place in the runs: a run and a character index in it (its length
/// at its end).
pub(crate) type Pos = (usize, usize);

/// What one frame tells assistive technology about the Source view.
#[derive(Debug, Default)]
pub(crate) struct SourceA11y {
    pub runs: Vec<Run>,
    /// The primary selection: anchor and focus.
    pub selection: Option<(Pos, Pos)>,
    /// Live Preview blocks among the rows: their tops, where their source
    /// starts, and their nodes.
    pub blocks: Vec<(f32, usize, Vec<DocNode>)>,
    /// Window px to the platform's.
    pub scale: f32,
    pub read_only: bool,
    /// The number of carets (selections).
    pub carets: usize,
    /// The frame's scroll offset and the view's top and bottom, in window
    /// px: what scrolling a run into view works from.
    pub scroll_y: f64,
    pub viewport: (f32, f32),
    /// The runs' node ids, as the last frame gave them.
    ids: Vec<accesskit::NodeId>,
    /// The Live Preview blocks' node ids, with where each block's source
    /// starts.
    block_ids: Vec<(accesskit::NodeId, usize)>,
}

/// Filled by the element, read by the focused `div` and its action.
pub(crate) type SharedSource = Rc<RefCell<SourceA11y>>;

impl SourceA11y {
    /// The buffer offset of a place in the runs.
    pub fn offset_at(&self, (run, index): Pos) -> Option<usize> {
        let run = self.runs.get(run)?;
        run.offsets.get(index.min(run.len())).copied()
    }

    /// The buffer offset of an AccessKit text position, by the node ids
    /// of the last frame: in a run, or the source start of the block whose
    /// node it is in.
    pub fn offset_of(&self, position: accesskit::TextPosition) -> Option<usize> {
        if let Some(run) = self.ids.iter().position(|id| *id == position.node) {
            return self.offset_at((run, position.character_index));
        }
        self.block_ids
            .iter()
            .find(|(id, _)| *id == position.node)
            .map(|(_, start)| *start)
    }

    /// The runs' node ids, as the last frame gave them.
    pub fn ids(&self) -> &[accesskit::NodeId] {
        &self.ids
    }

    /// The scroll offset that shows run `run` whole (the nearest edge),
    /// or `None` when it is shown.
    pub fn scroll_to_show(&self, run: usize) -> Option<f64> {
        let bounds = self.runs.get(run)?.bounds;
        let (top, bottom) = self.viewport;
        let (y0, y1) = (f32::from(bounds.top()), f32::from(bounds.bottom()));
        if y0 < top {
            Some(self.scroll_y - f64::from(top - y0))
        } else if y1 > bottom {
            Some(self.scroll_y + f64::from(y1 - bottom))
        } else {
            None
        }
    }

    /// The scroll offset that puts run `run`'s top at `y` (window px).
    pub fn scroll_to_point(&self, run: usize, y: f32) -> Option<f64> {
        let top = f32::from(self.runs.get(run)?.bounds.top());
        Some(self.scroll_y + f64::from(top - y))
    }

    /// Where buffer offset `offset` is in the runs: in the run that holds
    /// it, else clamped to the first run's start or the last run's end.
    pub fn locate(&self, offset: usize) -> Option<Pos> {
        let first = self.runs.first()?;
        if offset < first.offsets[0] {
            return Some((0, 0));
        }
        for (i, run) in self.runs.iter().enumerate() {
            let end = run.offsets[run.len()];
            if offset >= run.offsets[0] && offset < end {
                let index = run.offsets[..run.len()].partition_point(|&o| o <= offset);
                return Some((i, index.saturating_sub(1)));
            }
        }
        let last = self.runs.len() - 1;
        Some((last, self.runs[last].len()))
    }
}

/// What [`line_runs`] needs besides the row.
pub(crate) struct LineEnv<'a> {
    pub text: &'a TextSnapshot,
    pub replace: &'a ReplaceSnapshot,
    /// The line's first buffer line (the runs' key).
    pub line: usize,
    /// Its buffer range, without the terminator.
    pub range: Range<usize>,
    /// The buffer range of the line's terminator, if it has one.
    pub terminator: Option<Range<usize>>,
    /// The left of the text, and the top of each of its rows, in window px.
    pub origin_x: f32,
    pub tops: &'a dyn Fn(usize) -> f32,
    pub row_h: f32,
    /// What a box widget stands for (alt text, TeX).
    pub widget_text: &'a dyn Fn(WidgetId) -> Option<Arc<str>>,
}

/// A character of a row, before it goes into a run.
struct Char {
    text: String,
    column: usize,
    offset: usize,
    x: f32,
}

/// The runs of one visual line (all its rows), in order, and where the
/// buffer offsets in `wanted` fall in them (`None` for one not on the
/// line).
pub(crate) fn line_runs(
    row: &Row,
    env: &LineEnv<'_>,
    wanted: &[usize],
) -> (Vec<Run>, Vec<Option<Pos>>) {
    let content = &row.text.content;
    let last = row.segments.len() - 1;
    let mut runs = Vec::new();
    let mut found = vec![None; wanted.len()];
    for (k, segment) in row.segments.iter().enumerate() {
        let cols = segment.columns.clone();
        let mut chars: Vec<Char> = Vec::new();
        let piece = content.get(cols.clone()).unwrap_or_default();
        for (i, g) in piece.grapheme_indices(true) {
            let column = cols.start + i;
            let tab = row.text.is_tab(column);
            if !tab && row.text.tab_start(column) != column {
                // Inside an expanded tab: one `\t` stands for it.
                continue;
            }
            let spoken = (g == OBJECT_REPLACEMENT)
                .then(|| widget_at(row, column))
                .flatten()
                .and_then(env.widget_text)
                .filter(|t| !t.trim().is_empty());
            let shown = if tab {
                "\t"
            } else if g == OBJECT_REPLACEMENT {
                " "
            } else {
                g
            };
            let in_placeholder = row
                .text
                .placeholders
                .iter()
                .any(|p| p.start <= column && column < p.end);
            // Shown text starts after the markers Live Preview hides before
            // it; a fold placeholder stands at the start of what it hides.
            let bias = if in_placeholder {
                Bias::Left
            } else {
                Bias::Right
            };
            let offset = row.offset_of_biased(env.text, env.replace, column, bias);
            let x = row.x_in(k, column);
            if let Some(spoken) = spoken {
                // A widget's text: its characters spread over its width,
                // all at its source's start.
                let x1 = row.x_in(k, column + g.len());
                let count = spoken.graphemes(true).count().max(1);
                for (i, c) in spoken.graphemes(true).enumerate() {
                    let mut end = c.len().min(MAX_CHAR);
                    while !c.is_char_boundary(end) {
                        end -= 1;
                    }
                    chars.push(Char {
                        text: c[..end].to_owned(),
                        column,
                        offset,
                        x: x + (x1 - x) * i as f32 / count as f32,
                    });
                }
            } else if shown.len() <= MAX_CHAR {
                chars.push(Char {
                    text: shown.to_owned(),
                    column,
                    offset,
                    x,
                });
            } else {
                // A cluster too long for a `u8` length: in pieces.
                let mut at = 0;
                while at < shown.len() {
                    let mut end = (at + MAX_CHAR).min(shown.len());
                    while !shown.is_char_boundary(end) {
                        end -= 1;
                    }
                    chars.push(Char {
                        text: shown[at..end].to_owned(),
                        column,
                        offset: offset + at,
                        x,
                    });
                    at = end;
                }
            }
        }
        let end_x = row.x_in(k, cols.end);
        let mut end_offset = if k == last {
            row.offset_of_biased(env.text, env.replace, cols.end, Bias::Left)
        } else {
            row.offset_of_biased(env.text, env.replace, cols.end, Bias::Right)
        };
        if k == last
            && let Some(term) = &env.terminator
        {
            chars.push(Char {
                text: "\n".to_owned(),
                column: content.len(),
                offset: term.start,
                x: end_x,
            });
            end_offset = term.end;
        }

        // Where the wanted offsets are: in this row when their column is.
        for (w, &offset) in wanted.iter().enumerate() {
            if found[w].is_some() {
                continue;
            }
            if offset < env.range.start || offset > env.range.end {
                continue;
            }
            let column = row.column_of(env.text, env.replace, offset, Bias::Left);
            if row.segment_of(column) != k {
                continue;
            }
            let index = chars.partition_point(|c| c.column < column);
            let pieces = chars.len().div_ceil(MAX_RUN).max(1);
            let piece = (index / MAX_RUN).min(pieces - 1);
            found[w] = Some((runs.len() + piece, index - piece * MAX_RUN));
        }

        // Words: a word starts where a non-blank word segment does;
        // trailing blanks belong to the word before.
        let mut starts_at = Vec::with_capacity(chars.len());
        let mut joined = String::new();
        for c in &chars {
            starts_at.push(joined.len());
            joined.push_str(&c.text);
        }
        let word_bytes: Vec<usize> = joined
            .split_word_bound_indices()
            .filter(|(_, w)| !w.chars().all(char::is_whitespace))
            .map(|(at, _)| at)
            .collect();
        let top = (env.tops)(k);
        let pieces = chars.len().div_ceil(MAX_RUN).max(1);
        for p in 0..pieces {
            let from = p * MAX_RUN;
            let to = ((p + 1) * MAX_RUN).min(chars.len());
            let part = &chars[from..to];
            let x0 = part.first().map_or(end_x, |c| c.x);
            let x1 = chars.get(to).map_or(end_x, |c| c.x);
            let mut text = String::new();
            let mut lengths = Vec::with_capacity(part.len());
            let mut positions = Vec::with_capacity(part.len());
            let mut widths = Vec::with_capacity(part.len());
            let mut offsets = Vec::with_capacity(part.len() + 1);
            for (i, c) in part.iter().enumerate() {
                text.push_str(&c.text);
                lengths.push(u8::try_from(c.text.len()).unwrap_or(u8::MAX));
                positions.push(c.x - x0);
                let next = chars.get(from + i + 1).map_or(end_x, |n| n.x);
                widths.push((next - c.x).max(0.0));
                offsets.push(c.offset);
            }
            offsets.push(chars.get(to).map_or(end_offset, |c| c.offset));
            let word_starts = (from..to)
                .filter(|&i| word_bytes.binary_search(&starts_at[i]).is_ok())
                .filter_map(|i| u8::try_from(i - from).ok())
                .collect();
            runs.push(Run {
                key: (env.line, k, p),
                text,
                lengths,
                word_starts,
                positions,
                widths,
                offsets,
                bounds: Bounds::new(
                    point(px(env.origin_x + x0), px(top)),
                    size(px((x1 - x0).max(0.0)), px(env.row_h)),
                ),
                next_on_line: p + 1 < pieces,
                previous_on_line: p > 0,
            });
        }
    }
    (runs, found)
}

/// The box widget at `column` of `row`, if one is there.
fn widget_at(row: &Row, column: usize) -> Option<WidgetId> {
    row.text.spans.iter().find_map(|span| match span.replaced {
        Some(Replaced::Widget(id)) if span.columns.contains(&column) => Some(id),
        _ => None,
    })
}

/// The node ids of `nodes` and their descendants, as
/// [`crate::a11y::document::push`] gives them.
fn node_ids(
    nodes: &[DocNode],
    builder: &A11ySubtreeBuilder<'_>,
    start: usize,
    out: &mut Vec<(accesskit::NodeId, usize)>,
) {
    for node in nodes {
        out.push((builder.synthetic_node_id(node.key), start));
        node_ids(&node.children, builder, start, out);
    }
}

/// Hangs the runs (and the Live Preview blocks among them) on the text
/// input node and sets its selection. Called by the focused `div` after
/// its children are prepainted.
pub(crate) fn push(shared: &SharedSource, builder: &mut A11ySubtreeBuilder<'_>) {
    let mut source = shared.borrow_mut();
    let source = &mut *source;
    let scale = if source.scale > 0.0 {
        source.scale
    } else {
        1.0
    };
    let rect = |b: &Bounds<Pixels>| accesskit::Rect {
        x0: f64::from(f32::from(b.origin.x) * scale),
        y0: f64::from(f32::from(b.origin.y) * scale),
        x1: f64::from(f32::from(b.origin.x + b.size.width) * scale),
        y1: f64::from(f32::from(b.origin.y + b.size.height) * scale),
    };
    let ids: Vec<accesskit::NodeId> = source
        .runs
        .iter()
        .map(|run| builder.synthetic_node_id(run.key))
        .collect();
    let mut block_ids = Vec::new();
    let mut blocks = std::mem::take(&mut source.blocks).into_iter().peekable();
    for (i, run) in source.runs.iter().enumerate() {
        while let Some((_, start, nodes)) =
            blocks.next_if(|(top, _, _)| *top <= f32::from(run.bounds.origin.y))
        {
            node_ids(&nodes, builder, start, &mut block_ids);
            crate::a11y::document::push(&nodes, builder);
        }
        let mut node = accesskit::Node::new(Role::TextRun);
        node.set_bounds(rect(&run.bounds));
        node.set_value(run.text.clone());
        node.set_character_lengths(run.lengths.clone());
        node.set_word_starts(run.word_starts.clone());
        node.set_character_positions(run.positions.iter().map(|p| p * scale).collect::<Vec<_>>());
        node.set_character_widths(run.widths.iter().map(|w| w * scale).collect::<Vec<_>>());
        node.set_text_direction(accesskit::TextDirection::LeftToRight);
        node.add_action(accesskit::Action::ScrollIntoView);
        node.add_action(accesskit::Action::ScrollToPoint);
        if run.next_on_line {
            node.set_next_on_line(ids[i + 1]);
        }
        if run.previous_on_line {
            node.set_previous_on_line(ids[i - 1]);
        }
        builder.push_child(ids[i], node);
    }
    for (_, start, nodes) in blocks {
        node_ids(&nodes, builder, start, &mut block_ids);
        crate::a11y::document::push(&nodes, builder);
    }
    let parent = builder.parent_node();
    if source.read_only {
        parent.set_read_only();
    }
    if source.carets > 1 {
        parent.set_description(carets_description(source.carets));
    }
    let position = |(run, index): Pos| accesskit::TextPosition {
        node: ids[run],
        character_index: index,
    };
    if let Some((anchor, focus)) = source.selection
        && anchor.0 < ids.len()
        && focus.0 < ids.len()
    {
        parent.set_text_selection(accesskit::TextSelection {
            anchor: position(anchor),
            focus: position(focus),
        });
    }
    source.ids = ids;
    source.block_ids = block_ids;
}

/// The input's description with several carets.
pub(crate) fn carets_description(carets: usize) -> String {
    format!("{carets} carets")
}

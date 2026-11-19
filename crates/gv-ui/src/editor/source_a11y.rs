// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view's text for assistive technology (NFR-A11Y-06).
//!
//! The editor's element (its focused `div`) is a multi-line text input
//! whose children are AccessKit text runs: one per display row (soft
//! wrap gives a line several), split at [`MAX_RUN`] characters, for the
//! lines on screen and the primary caret's line when it is off screen.
//! A run's characters are grapheme clusters of what the row shows: Live
//! Preview's hidden markers are not there, a hard tab is one `\t`, and the
//! last row of a line ends in `\n` (one character, as AccessKit wants).
//! Each character keeps the buffer offset it starts at, so the node's
//! text selection (the primary selection's anchor and focus) points into
//! the runs, and a screen reader's `SetTextSelection` maps back to buffer
//! offsets.
//!
//! Built only while assistive technology is connected (and in tests):
//! the element fills a [`SharedSource`] each frame, the focused `div`
//! hangs the nodes on itself after its children are prepainted.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{A11ySubtreeBuilder, Bounds, Pixels, Role, accesskit, point, px, size};
use gv_editor::display_map::ReplaceSnapshot;
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
/// reported as a space.
const OBJECT_REPLACEMENT: &str = "\u{fffc}";

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
    /// Live Preview blocks among the rows: their tops and nodes.
    pub blocks: Vec<(f32, Vec<DocNode>)>,
    /// Window px to the platform's.
    pub scale: f32,
    pub read_only: bool,
    /// The runs' node ids, as the last frame gave them.
    ids: Vec<accesskit::NodeId>,
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
    /// of the last frame.
    pub fn offset_of(&self, position: accesskit::TextPosition) -> Option<usize> {
        let run = self.ids.iter().position(|id| *id == position.node)?;
        self.offset_at((run, position.character_index))
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
            if shown.len() <= MAX_CHAR {
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
    let mut blocks = std::mem::take(&mut source.blocks).into_iter().peekable();
    for (i, run) in source.runs.iter().enumerate() {
        while let Some((_, nodes)) =
            blocks.next_if(|(top, _)| *top <= f32::from(run.bounds.origin.y))
        {
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
        if run.next_on_line {
            node.set_next_on_line(ids[i + 1]);
        }
        if run.previous_on_line {
            node.set_previous_on_line(ids[i - 1]);
        }
        builder.push_child(ids[i], node);
    }
    for (_, nodes) in blocks {
        crate::a11y::document::push(&nodes, builder);
    }
    let parent = builder.parent_node();
    if source.read_only {
        parent.set_read_only();
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
}

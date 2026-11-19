// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Reading order on a page in two columns.
//!
//! The text as drawn is cut into pieces: its lines, and a line again
//! where it jumps back or skips at least [`GUTTER`] em (a PDF that draws
//! both columns row by row puts a left and a right part on one line); an
//! em is the font size most characters have. The pieces of text that
//! runs left to right and is at least [`NARROW`] em wide are laid over
//! the text block's width; the **gutter** is the widest run of the
//! block's middle three fifths ([`EDGE_SHARE`] off each side) crossed by
//! the fewest of them. Pieces wholly left of it are the left column,
//! wholly right of it the right column, the others are across (a narrow
//! one in the gutter beside the columns, such as a line number, goes with
//! the nearer column). The page is in two columns when:
//!
//! - at most as many wide pieces are across as beside the gutter, and at
//!   least [`MIN_LINES`] are on each side;
//! - the columns are at least [`GUTTER`] em apart, each at least
//!   [`MIN_WIDTH`] em wide and the narrower at least [`WIDTH_RATIO`] of
//!   the wider;
//! - the columns are filled: most left pieces end near the column's right
//!   edge and most right pieces start near its left edge ([`NEAR`] em, or
//!   [`NEAR_SHARE`] of the column's width), which tells columns of prose
//!   from a table or code with comments lined up;
//! - the columns cover at least [`HEIGHT_SHARE`] of the text block's
//!   height.
//!
//! Then the pieces across (a title, a full-width figure's caption, a
//! centred page number) are read where they are, top to bottom; between
//! two of them, the left column's pieces and then the right column's,
//! each in the order drawn. Text that does not run left to right (a
//! rotated margin note) comes last. A page that is not in two columns
//! keeps the order drawn, exactly. Pieces are joined by line ends; the
//! spaces where a line was cut are dropped.
//!
//! Not handled: three or more columns, columns of different widths
//! further down the page, text in columns that do not run left to right.

use std::ops::Range;

use crate::protocol::TextBox;

/// The narrowest gutter, and the jump that cuts a line, in em.
const GUTTER: f32 = 0.8;

/// How much of the text block's width on each side is never a gutter.
const EDGE_SHARE: f32 = 0.2;

/// The fewest pieces on each side of a gutter.
const MIN_LINES: usize = 3;

/// A piece ends (starts) near the gutter within this many em, or within
/// [`NEAR_SHARE`] of its column's width if that is more.
const NEAR: f32 = 2.0;
const NEAR_SHARE: f32 = 0.15;

/// The share of the text block's height the columns cover at least.
const HEIGHT_SHARE: f32 = 0.5;

/// A piece narrower than this (em) does not count in finding the gutter
/// (a page number, a line number, a footnote mark).
const NARROW: f32 = 3.0;

/// The narrowest column (em), and how much narrower than the other one a
/// column may be.
const MIN_WIDTH: f32 = 8.0;
const WIDTH_RATIO: f32 = 1.0 / 3.0;

/// The most bins the block's width is cut into.
const MAX_BINS: usize = 4096;

/// Where a piece is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
    Across,
}

/// A run of characters on one line, without spaces at its ends.
#[derive(Debug, Clone)]
struct Piece {
    chars: Range<usize>,
    upright: bool,
    /// The box around its characters (spaces left out).
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Piece {
    fn middle(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }
}

/// The text and boxes in reading order, when `text` (one box and one
/// `upright` flag per character) is a page in two columns; `None` keeps
/// the order drawn.
pub(super) fn order_columns(
    text: &str,
    boxes: &[TextBox],
    upright: &[bool],
) -> Option<(String, Vec<TextBox>)> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() != boxes.len() || chars.len() != upright.len() {
        return None;
    }
    let em = typical_em(&chars, boxes, upright)?;
    let pieces = pieces(&chars, boxes, upright, em * GUTTER);
    let flat: Vec<&Piece> = pieces.iter().filter(|p| p.upright).collect();
    let wide = |p: &Piece| p.x1 - p.x0 >= NARROW * em;
    if flat.iter().filter(|p| wide(p)).count() < 2 * MIN_LINES {
        return None;
    }
    let x0 = flat.iter().map(|p| p.x0).fold(f32::MAX, f32::min);
    let x1 = flat.iter().map(|p| p.x1).fold(f32::MIN, f32::max);
    let y0 = flat.iter().map(|p| p.y0).fold(f32::MAX, f32::min);
    let y1 = flat.iter().map(|p| p.y1).fold(f32::MIN, f32::max);
    let width = x1 - x0;
    if !(width.is_finite() && width > 0.0 && y1 > y0) {
        return None;
    }

    // How many wide pieces cross each bin of the block's width.
    let bin = (width / MAX_BINS as f32).max(0.5);
    let bins = ((width / bin).ceil() as usize).clamp(1, MAX_BINS);
    let bin_of = |x: f32| (((x - x0) / bin).floor().max(0.0) as usize).min(bins - 1);
    let mut diff = vec![0_i64; bins + 1];
    for p in flat.iter().filter(|p| wide(p)) {
        diff[bin_of(p.x0)] += 1;
        diff[bin_of(p.x1) + 1] -= 1;
    }
    let mut cover = Vec::with_capacity(bins);
    let mut running = 0_i64;
    for d in &diff[..bins] {
        running += d;
        cover.push(running);
    }

    // The gutter: the widest run crossed by the fewest, in the middle.
    let from = bin_of(x0 + EDGE_SHARE * width);
    let to = bin_of(x1 - EDGE_SHARE * width);
    let least = *cover.get(from..=to)?.iter().min()?;
    let mut best: Option<Range<usize>> = None;
    let mut b = from;
    while b <= to {
        if cover[b] != least {
            b += 1;
            continue;
        }
        let mut start = b;
        while start > 0 && cover[start - 1] == least {
            start -= 1;
        }
        let mut end = b;
        while end + 1 < bins && cover[end + 1] == least {
            end += 1;
        }
        if best.as_ref().is_none_or(|r| end + 1 - start > r.len()) {
            best = Some(start..end + 1);
        }
        b = end + 1;
    }
    let run = best?;

    // Each upright piece: left of the gutter, right of it, or across.
    let side_of = |p: &Piece| {
        if bin_of(p.x1) < run.start {
            Side::Left
        } else if bin_of(p.x0) >= run.end {
            Side::Right
        } else {
            Side::Across
        }
    };
    let beside = || {
        flat.iter()
            .filter(|p| wide(p) && side_of(p) != Side::Across)
    };
    let top = beside().map(|p| p.y0).fold(f32::MAX, f32::min);
    let bottom = beside().map(|p| p.y1).fold(f32::MIN, f32::max);
    let middle = (x0 + run.start as f32 * bin + x0 + run.end as f32 * bin) / 2.0;
    let sides: Vec<Option<Side>> = pieces
        .iter()
        .map(|p| {
            if !p.upright {
                return None;
            }
            let side = side_of(p);
            // A narrow piece in the gutter beside the columns (a line
            // number, a footnote mark) goes with the nearer column.
            if side == Side::Across && !wide(p) && (top..=bottom).contains(&p.middle()) {
                return Some(if (p.x0 + p.x1) / 2.0 < middle {
                    Side::Left
                } else {
                    Side::Right
                });
            }
            Some(side)
        })
        .collect();
    let on = |side: Side| {
        pieces
            .iter()
            .zip(&sides)
            .filter(move |(p, s)| **s == Some(side) && wide(p))
            .map(|(p, _)| p)
    };
    let left = on(Side::Left).count();
    let right = on(Side::Right).count();
    let across = on(Side::Across).count();
    if left < MIN_LINES || right < MIN_LINES || across > left + right {
        return None;
    }
    // Two columns of text, side by side: wide enough, and not too unlike.
    let left_x0 = on(Side::Left).map(|p| p.x0).fold(f32::MAX, f32::min);
    let left_x1 = on(Side::Left).map(|p| p.x1).fold(f32::MIN, f32::max);
    let right_x0 = on(Side::Right).map(|p| p.x0).fold(f32::MAX, f32::min);
    let right_x1 = on(Side::Right).map(|p| p.x1).fold(f32::MIN, f32::max);
    let (lw, rw) = (left_x1 - left_x0, right_x1 - right_x0);
    if right_x0 - left_x1 < GUTTER * em
        || lw.min(rw) < MIN_WIDTH * em
        || lw.min(rw) < WIDTH_RATIO * lw.max(rw)
    {
        return None;
    }
    // Filled up to the gutter.
    let near_left = (NEAR * em).max(NEAR_SHARE * lw);
    let near_right = (NEAR * em).max(NEAR_SHARE * rw);
    let filled_left = on(Side::Left)
        .filter(|p| p.x1 >= left_x1 - near_left)
        .count();
    let filled_right = on(Side::Right)
        .filter(|p| p.x0 <= right_x0 + near_right)
        .count();
    if 2 * filled_left < left || 2 * filled_right < right {
        return None;
    }
    // And over most of the block's height.
    if bottom - top < HEIGHT_SHARE * (y1 - y0) {
        return None;
    }

    // Reading order: the pieces across, top to bottom, with the columns
    // between them.
    let mut spans: Vec<usize> = (0..pieces.len())
        .filter(|i| sides[*i] == Some(Side::Across))
        .collect();
    spans.sort_by(|a, b| pieces[*a].middle().total_cmp(&pieces[*b].middle()));
    let middles: Vec<f32> = spans.iter().map(|i| pieces[*i].middle()).collect();
    // Between which pieces across each piece beside the gutter is.
    let bands: Vec<usize> = pieces
        .iter()
        .map(|p| middles.partition_point(|m| *m < p.middle()))
        .collect();
    let mut order: Vec<usize> = Vec::with_capacity(pieces.len());
    for k in 0..=spans.len() {
        for side in [Side::Left, Side::Right] {
            order.extend((0..pieces.len()).filter(|i| sides[*i] == Some(side) && bands[*i] == k));
        }
        order.extend(spans.get(k));
    }
    order.extend((0..pieces.len()).filter(|i| sides[*i].is_none()));

    let mut out = String::with_capacity(text.len());
    let mut out_boxes: Vec<TextBox> = Vec::with_capacity(boxes.len());
    for (n, i) in order.into_iter().enumerate() {
        let piece = &pieces[i];
        if n > 0
            && let Some(b) = out_boxes.last().copied()
        {
            out.push('\n');
            out_boxes.push(TextBox {
                x0: b.x1,
                y0: b.y0,
                x1: b.x1,
                y1: b.y1,
            });
        }
        out.extend(&chars[piece.chars.clone()]);
        out_boxes.extend_from_slice(&boxes[piece.chars.clone()]);
    }
    Some((out, out_boxes))
}

/// The font size (box height) most upright characters have, the median.
fn typical_em(chars: &[char], boxes: &[TextBox], upright: &[bool]) -> Option<f32> {
    let mut heights: Vec<f32> = chars
        .iter()
        .zip(boxes)
        .zip(upright)
        .filter(|((c, _), up)| **up && !c.is_whitespace())
        .map(|((_, b), _)| b.y1 - b.y0)
        .filter(|h| h.is_finite() && *h > 0.0)
        .collect();
    if heights.is_empty() {
        return None;
    }
    let mid = heights.len() / 2;
    let (_, m, _) = heights.select_nth_unstable_by(mid, f32::total_cmp);
    Some(*m)
}

/// The pieces of the text: its lines, cut again where the text jumps
/// back or skips `jump` points or more, or turns.
fn pieces(chars: &[char], boxes: &[TextBox], upright: &[bool], jump: f32) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let mut current: Option<Piece> = None;
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            out.extend(current.take());
            continue;
        }
        if c.is_whitespace() {
            continue;
        }
        let b = boxes[i];
        let (bx0, bx1) = (b.x0.min(b.x1), b.x0.max(b.x1));
        let (by0, by1) = (b.y0.min(b.y1), b.y0.max(b.y1));
        if let Some(p) = &mut current {
            // The last character kept is the piece's end.
            let last = boxes[p.chars.end - 1];
            let last_x1 = last.x0.max(last.x1);
            let last_x0 = last.x0.min(last.x1);
            let cut = p.upright != upright[i]
                || (upright[i] && (bx0 - last_x1 >= jump || bx1 <= last_x0));
            if !cut {
                p.chars.end = i + 1;
                p.x0 = p.x0.min(bx0);
                p.y0 = p.y0.min(by0);
                p.x1 = p.x1.max(bx1);
                p.y1 = p.y1.max(by1);
                continue;
            }
            out.extend(current.take());
        }
        current = Some(Piece {
            chars: i..i + 1,
            upright: upright[i],
            x0: bx0,
            y0: by0,
            x1: bx1,
            y1: by1,
        });
    }
    out.extend(current);
    out
}

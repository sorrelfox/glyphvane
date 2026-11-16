// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Shaped rows: a visual line ([`VisualLine`]: a buffer line, or one
//! joined to others by folds) built from the display map's chunks of its
//! row in the tab layer, with the styles the chunks carry, shaped as one
//! GPUI [`ShapedLine`] per wrap row ([`Segment`]), kept across frames while
//! the line's text, folds, highlighting and wraps stay the same.
//!
//! Columns are byte columns of the tab layer's row: the line's text with
//! tabs expanded and fold placeholders in place of folded text. Highlights
//! go to the chunks as [`Highlight`]s over buffer bytes and come back as
//! style ids on them; a buffer offset finds its column through the replace
//! layer, and soft-wrap rows start at the wrap layer's columns.
//!
//! GPUI also caches glyph layouts for the lines of the last two frames, by
//! text and fonts; this cache adds the display text, the run list and the
//! decorations, and answers geometry queries between frames (the input
//! handler's) without shaping again.

use std::collections::HashMap;
use std::ops::Range;

use gpui::{Font, Hsla, ShapedLine, SharedString, StrikethroughStyle, TextRun, Window, px};
use gv_editor::display_map::{
    Highlight, ReplacePoint, ReplaceSnapshot, Replaced, TabPoint, TabSnapshot,
};
use gv_text::{Bias, Point as BufferPoint, TextSnapshot};

use super::fold::{LineStyles, VisualLine};
use super::highlight::{Style, Underline};
use super::layout::RowText;
use super::wrap::Break;
use crate::inline::{FontFamilies, Palette, Token, font_of};
use crate::metrics::{HAIRLINE, text};
use crate::theme::ThemeId;

/// The four faces of the editor font.
#[derive(Debug, Clone)]
pub(crate) struct Fonts {
    regular: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
}

impl Fonts {
    pub fn new(families: &FontFamilies) -> Self {
        let face = |weight, italic| font_of(families.mono.clone(), weight, italic);
        Self {
            regular: face(text::weight::REGULAR, false),
            bold: face(text::weight::BOLD, false),
            italic: face(text::weight::REGULAR, true),
            bold_italic: face(text::weight::BOLD, true),
        }
    }

    pub fn regular(&self) -> &Font {
        &self.regular
    }

    fn of(&self, style: &Style) -> &Font {
        match (style.bold, style.italic) {
            (false, false) => &self.regular,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (true, true) => &self.bold_italic,
        }
    }
}

/// An underline to draw under display range `range` of a row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Decoration {
    pub range: Range<usize>,
    pub kind: Underline,
    pub color: Token,
}

/// One wrap row of a line, shaped.
#[derive(Debug, Clone)]
pub(crate) struct Segment {
    /// Its columns: from its break to the next one, or to the end of the
    /// content.
    pub columns: Range<usize>,
    /// Its display bytes; the last segment holds a `↵` mark too.
    pub display: Range<usize>,
    /// The continuation indent, in px from the left of the text.
    pub indent: f32,
    /// Shaped with the plain colours (no selection).
    pub shaped: ShapedLine,
}

/// One shaped line, in one segment per wrap row.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    /// What it was made from: the buffer text of the line's range, the
    /// folded ranges and the highlighting.
    source: String,
    folded: Vec<Range<usize>>,
    styles: LineStyles,
    whitespace: bool,
    theme: ThemeId,
    tab_size: u32,
    breaks: Vec<Break>,
    space: f32,
    /// Its row in the replace and tab layers.
    pub row: u32,
    /// The display text and its mapping.
    pub text: RowText,
    /// The line's runs, in columns.
    pub runs: Vec<(Range<usize>, Style)>,
    /// One per wrap row, at least one.
    pub segments: Vec<Segment>,
    /// Link underlines, in display bytes of the whole line.
    pub decorations: Vec<Decoration>,
    /// Fenced code: drawn on the code fill.
    pub code: bool,
}

impl Row {
    /// The column of buffer offset `offset`: an offset hidden in a fold is
    /// the placeholder's start ([`Bias::Left`]) or end; an offset before
    /// the line is its start and one after it its end.
    pub fn column_of(
        &self,
        text: &TextSnapshot,
        replace: &ReplaceSnapshot,
        offset: usize,
        bias: Bias,
    ) -> usize {
        let Ok(point) = text.offset_to_point(offset.min(text.len())) else {
            return 0;
        };
        let at = replace.to_replace_point(point, bias);
        match at.row().cmp(&self.row) {
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Greater => self.content_len(),
            std::cmp::Ordering::Equal => self.text.column_of_input(at.column() as usize),
        }
    }

    /// The buffer offset of column `column` (a column inside a placeholder
    /// is the start of the text it hides, one inside a tab the tab's).
    pub fn offset_of(
        &self,
        text: &TextSnapshot,
        replace: &ReplaceSnapshot,
        column: usize,
    ) -> usize {
        let input = self.text.input_of_column(column);
        let point = replace.to_buffer_point(
            ReplacePoint::new(self.row, u32::try_from(input).unwrap_or(u32::MAX)),
            Bias::Left,
        );
        text.point_to_offset(BufferPoint::new(point.line, point.column))
            .unwrap_or(text.len())
    }

    /// The segment a caret at column `column` is drawn in: a column where a
    /// row wraps is the start of the next row.
    pub fn segment_of(&self, column: usize) -> usize {
        self.segments
            .partition_point(|s| s.columns.start <= column)
            .saturating_sub(1)
    }

    /// The x of column `column` in segment `segment` (clamped to it), from
    /// the left of the text.
    pub fn x_in(&self, segment: usize, column: usize) -> f32 {
        let Some(s) = self.segments.get(segment) else {
            return 0.0;
        };
        let column = column.clamp(s.columns.start, s.columns.end);
        let display = self
            .text
            .to_display(column)
            .clamp(s.display.start, s.display.end);
        s.indent + f32::from(s.shaped.x_for_index(display - s.display.start))
    }

    /// The segment of column `column` and its x there.
    pub fn x(&self, column: usize) -> (usize, f32) {
        let segment = self.segment_of(column);
        (segment, self.x_in(segment, column))
    }

    /// The widest segment's content (without a `↵` mark).
    pub fn content_width(&self) -> f32 {
        let last = self.segments.len() - 1;
        self.segments
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let end = if i == last {
                    self.text
                        .content_end()
                        .clamp(s.display.start, s.display.end)
                } else {
                    s.display.end
                };
                s.indent + f32::from(s.shaped.x_for_index(end - s.display.start))
            })
            .fold(0.0, f32::max)
    }

    /// The column of segment `segment` closest to `x` (from the left of the
    /// text). Past the end of a row that wraps, the column before its last
    /// character: the end is the next row's start.
    pub fn column_at(&self, segment: usize, x: f32) -> usize {
        let Some(s) = self.segments.get(segment) else {
            return self.content_len();
        };
        let x = x - s.indent;
        if x <= 0.0 {
            return s.columns.start;
        }
        let last = segment + 1 == self.segments.len();
        let content_end = self
            .text
            .content_end()
            .clamp(s.display.start, s.display.end);
        let display = s.display.start + closest_index(&s.shaped, x);
        let column = self.text.to_column(display.min(content_end));
        if last {
            return column.min(s.columns.end);
        }
        let before_last = self.text.content[..s.columns.end]
            .char_indices()
            .next_back()
            .map_or(s.columns.start, |(at, _)| at)
            .max(s.columns.start);
        column.clamp(s.columns.start, before_last)
    }

    pub fn content_len(&self) -> usize {
        self.text.content.len()
    }

    /// Segment `segment` shaped again with tertiary text inside `selected`
    /// (columns) drawn in `text.secondary`: `text.tertiary` must not
    /// sit on `selection.text`.
    pub fn with_selection(
        &self,
        segment: usize,
        selected: &[Range<usize>],
        fonts: &Fonts,
        palette: &Palette,
        window: &Window,
    ) -> ShapedLine {
        let selected: Vec<Range<usize>> = selected
            .iter()
            .map(|r| self.text.display_range(r.clone()))
            .collect();
        let runs = text_runs(&self.text, &self.runs, &selected, fonts, palette);
        let display = self.segments[segment].display.clone();
        shape(
            window,
            &self.text.display[display.clone()],
            &slice_runs(&runs, display),
        )
    }
}

/// The runs of display range `range`, from the runs of the whole display
/// text.
fn slice_runs(runs: &[TextRun], range: Range<usize>) -> Vec<TextRun> {
    let mut out = Vec::new();
    let mut at = 0;
    for run in runs {
        let (start, end) = (at, at + run.len);
        at = end;
        let (a, b) = (start.max(range.start), end.min(range.end));
        if a < b {
            out.push(TextRun {
                len: b - a,
                ..run.clone()
            });
        }
        if end >= range.end {
            break;
        }
    }
    out
}

/// The glyph boundary closest to `x`, the end of the line included.
/// (`LineLayout::closest_index_for_x` answers the line's end anywhere past
/// the last glyph's start.)
fn closest_index(line: &ShapedLine, x: f32) -> usize {
    let mut best = (line.len(), (f32::from(line.width()) - x).abs());
    for run in &line.runs {
        for glyph in &run.glyphs {
            let distance = (f32::from(glyph.position.x) - x).abs();
            if distance < best.1 {
                best = (glyph.index, distance);
            }
        }
    }
    best.0
}

fn shape(window: &Window, display: &str, runs: &[TextRun]) -> ShapedLine {
    window.text_system().shape_line(
        SharedString::from(display.to_owned()),
        px(text::EDITOR.size),
        runs,
        None,
    )
}

/// What a display byte is drawn as.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    style: Style,
    color: Token,
}

/// The GPUI runs of a row: styles mapped to display bytes, whitespace
/// marks and control pictures in `text.tertiary`, and tertiary text in
/// `selected` (display ranges) as `text.secondary`.
fn text_runs(
    text: &RowText,
    runs: &[(Range<usize>, Style)],
    selected: &[Range<usize>],
    fonts: &Fonts,
    palette: &Palette,
) -> Vec<TextRun> {
    let len = text.display.len();
    if len == 0 {
        return Vec::new();
    }
    // Every display offset where something may change.
    let mut cuts: Vec<usize> = vec![0, len];
    let display_runs: Vec<(Range<usize>, Style)> = runs
        .iter()
        .map(|(r, s)| (text.display_range(r.clone()), *s))
        .collect();
    for (r, _) in &display_runs {
        cuts.extend([r.start, r.end]);
    }
    for r in text.marks.iter().chain(selected) {
        cuts.extend([r.start, r.end]);
    }
    cuts.retain(|&c| c <= len && text.display.is_char_boundary(c));
    cuts.sort_unstable();
    cuts.dedup();

    // Cuts, runs and marks are all in order: one pass over each.
    let (mut run, mut mark) = (0, 0);
    let mut out: Vec<(usize, Look)> = Vec::new();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        while display_runs.get(run).is_some_and(|(r, _)| r.end <= a) {
            run += 1;
        }
        while text.marks.get(mark).is_some_and(|m| m.end <= a) {
            mark += 1;
        }
        let style = display_runs
            .get(run)
            .filter(|(r, _)| r.start <= a)
            .map_or(Style::PLAIN, |(_, s)| *s);
        let mut color = style.color;
        if text.marks.get(mark).is_some_and(|m| m.start <= a) {
            color = Token::TextTertiary;
        }
        if color == Token::TextTertiary && selected.iter().any(|s| s.start <= a && a < s.end) {
            color = Token::TextSecondary;
        }
        let look = Look { style, color };
        match out.last_mut() {
            Some((n, last)) if *last == look => *n += b - a,
            _ => out.push((b - a, look)),
        }
    }
    out.into_iter()
        .map(|(len, look)| {
            let color: Hsla = palette.color(look.color);
            TextRun {
                len,
                font: fonts.of(&look.style).clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: look.style.strike.then_some(StrikethroughStyle {
                    thickness: px(HAIRLINE),
                    color: Some(color),
                }),
            }
        })
        .collect()
}

/// The runs of a row's spans: the style each id stands for (`styles[id]`),
/// `text.secondary` for fold placeholders, plain without either.
fn runs_of(text: &RowText, styles: &[Style]) -> Vec<(Range<usize>, Style)> {
    text.spans
        .iter()
        .filter(|span| !span.columns.is_empty())
        .map(|span| {
            let style = match span.replaced {
                Some(Replaced::Fold(_)) => Style::of(Token::TextSecondary),
                _ => span
                    .style
                    .and_then(|id| styles.get(id.0 as usize))
                    .copied()
                    .unwrap_or(Style::PLAIN),
            };
            (span.columns.clone(), style)
        })
        .collect()
}

/// Whether `text`'s bytes in `range` are `stored`, without copying them.
fn same_text(text: &TextSnapshot, range: Range<usize>, stored: &str) -> bool {
    if range.len() != stored.len() {
        return false;
    }
    let Ok(chunks) = text.chunks(range) else {
        return false;
    };
    let mut at = 0;
    for chunk in chunks {
        if stored.get(at..at + chunk.len()) != Some(chunk) {
            return false;
        }
        at += chunk.len();
    }
    at == stored.len()
}

/// The underlines of a row's link targets.
fn decorations(text: &RowText, runs: &[(Range<usize>, Style)]) -> Vec<Decoration> {
    let mut out: Vec<Decoration> = Vec::new();
    for (r, style) in runs {
        let Some(kind) = style.underline else {
            continue;
        };
        let color = match kind {
            Underline::Wavy => Token::StatusWarning,
            Underline::Dotted | Underline::Dashed => style.color,
        };
        let range = text.display_range(r.clone());
        match out.last_mut() {
            Some(last)
                if last.range.end == range.start && last.kind == kind && last.color == color =>
            {
                last.range.end = range.end;
            }
            _ => out.push(Decoration { range, kind, color }),
        }
    }
    out
}

/// What a row is made with, besides its line.
#[derive(Clone, Copy)]
pub(crate) struct RowEnv<'a> {
    /// The text and its display map's tab layer.
    pub text: &'a TextSnapshot,
    pub tabs: &'a TabSnapshot,
    pub fonts: &'a Fonts,
    pub palette: &'a Palette,
    pub theme: ThemeId,
    pub whitespace: bool,
    pub tab_size: u32,
    /// The advance of a space, for continuation indents.
    pub space: f32,
}

/// Rows by line, kept while they are drawn.
#[derive(Debug, Default)]
pub(crate) struct RowCache {
    rows: HashMap<usize, Row>,
    /// Lines used since the last [`Self::end_frame`].
    used: Vec<usize>,
}

/// Rows kept beyond the ones drawn in the last frame.
const SPARE_ROWS: usize = 256;

impl RowCache {
    /// The row of visual line `line`, made from the display map's chunks
    /// and shaped now unless the cached one still fits. The line has the
    /// highlighting `styles` and wraps at `breaks` (one at column 0 when it
    /// does not wrap).
    pub fn row(
        &mut self,
        line: &VisualLine,
        styles: LineStyles,
        breaks: Vec<Break>,
        env: RowEnv<'_>,
        window: &Window,
    ) -> Option<&Row> {
        let key = line.first;
        self.used.push(key);
        let fresh = match self.rows.get_mut(&key) {
            Some(row) => {
                let stale = row.whitespace != env.whitespace
                    || row.theme != env.theme
                    || row.tab_size != env.tab_size
                    || row.breaks != breaks
                    || row.space != env.space
                    || !row
                        .folded
                        .iter()
                        .eq(line.marks.iter().map(|mark| &mark.buffer))
                    || !row.styles.same(&styles)
                    || !same_text(env.text, line.range.clone(), &row.source);
                // Folds above move the line to another row.
                row.row = line.row;
                stale
            }
            None => true,
        };
        if fresh {
            let row = make_row(line, styles, breaks, env, window);
            self.rows.insert(key, row);
        }
        self.rows.get(&key)
    }

    /// Forgets rows that were not used in the frame that ends.
    pub fn end_frame(&mut self) {
        if self.rows.len() > self.used.len() + SPARE_ROWS {
            let used: std::collections::HashSet<usize> = self.used.iter().copied().collect();
            self.rows.retain(|line, _| used.contains(line));
        }
        self.used.clear();
    }
}

fn make_row(
    line: &VisualLine,
    styles: LineStyles,
    mut breaks: Vec<Break>,
    env: RowEnv<'_>,
    window: &Window,
) -> Row {
    let (highlights, table): (Vec<Highlight>, Vec<Style>) = line.highlights(&styles);
    let row_end = TabPoint::new(line.row, env.tabs.line_len(line.row));
    let text = RowText::from_chunks(
        env.tabs
            .highlighted_chunks(TabPoint::new(line.row, 0)..row_end, &highlights),
        env.whitespace,
    );
    let runs = runs_of(&text, &table);
    let all_runs = text_runs(&text, &runs, &[], env.fonts, env.palette);
    let key = breaks.clone();
    let content = &text.content;
    // Breaks inside the content, in order, the first at column 0; a row
    // that starts inside an expanded tab starts with the tab.
    for b in &mut breaks {
        b.column = text.tab_start(b.column);
    }
    breaks.retain(|b| b.column < content.len() && content.is_char_boundary(b.column));
    if breaks.first().is_none_or(|b| b.column != 0) {
        breaks.insert(
            0,
            Break {
                column: 0,
                indent: 0,
            },
        );
    }
    breaks.dedup_by(|b, a| b.column <= a.column);
    let segments = if breaks.len() == 1 {
        vec![Segment {
            columns: 0..content.len(),
            display: 0..text.display.len(),
            indent: 0.0,
            shaped: shape(window, &text.display, &all_runs),
        }]
    } else {
        breaks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let last = i + 1 == breaks.len();
                let end = breaks.get(i + 1).map_or(content.len(), |n| n.column);
                let display_end = if last {
                    text.display.len()
                } else {
                    text.to_display(end)
                };
                let display = text.to_display(b.column).min(display_end)..display_end;
                let shaped = shape(
                    window,
                    &text.display[display.clone()],
                    &slice_runs(&all_runs, display.clone()),
                );
                Segment {
                    columns: b.column..end,
                    display,
                    indent: b.indent as f32 * env.space,
                    shaped,
                }
            })
            .collect()
    };
    let decorations = decorations(&text, &runs);
    let code = styles.code;
    Row {
        source: env
            .text
            .text_for_range(line.range.clone())
            .unwrap_or_default(),
        folded: line.marks.iter().map(|mark| mark.buffer.clone()).collect(),
        styles,
        whitespace: env.whitespace,
        theme: env.theme,
        tab_size: env.tab_size,
        breaks: key,
        space: env.space,
        row: line.row,
        text,
        runs,
        segments,
        decorations,
        code,
    }
}

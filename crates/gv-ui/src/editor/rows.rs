// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Shaped rows: one GPUI [`ShapedLine`] per buffer line, with its styled
//! runs, kept across frames while the line's text and highlighting stay
//! the same.
//!
//! GPUI also caches glyph layouts for the lines of the last two frames, by
//! text and fonts; this cache adds the display text, the run list and the
//! decorations, and answers geometry queries between frames (the input
//! handler's) without shaping again.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{Font, Hsla, ShapedLine, SharedString, StrikethroughStyle, TextRun, Window, px};
use gv_text::TextSnapshot;

use super::highlight::{LineStyle, Style, Underline};
use super::layout::{RowText, ends_in_hard_break};
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

/// One shaped line.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    /// What it was made from.
    content: String,
    style: Option<Rc<LineStyle>>,
    whitespace: bool,
    theme: ThemeId,
    tab_size: u32,
    /// The display text and its mapping.
    pub text: RowText,
    /// The line's runs (line-relative buffer bytes).
    pub runs: Vec<(Range<usize>, Style)>,
    /// Shaped with the plain colours (no selection).
    pub shaped: ShapedLine,
    pub decorations: Vec<Decoration>,
    /// Fenced code: drawn on the code fill.
    pub code: bool,
}

impl Row {
    /// The x of buffer column `column` (line-relative bytes).
    pub fn x(&self, column: usize) -> f32 {
        f32::from(self.shaped.x_for_index(self.text.to_display(column)))
    }

    /// The width of the content (without a `↵` mark).
    pub fn content_width(&self) -> f32 {
        f32::from(self.shaped.x_for_index(self.text.content_end()))
    }

    /// The buffer column closest to `x`.
    pub fn column_at(&self, x: f32) -> usize {
        if x <= 0.0 {
            return 0;
        }
        let display = closest_index(&self.shaped, x);
        self.text.to_buffer(display.min(self.text.content_end()))
    }

    pub fn content_len(&self) -> usize {
        self.content.len()
    }

    /// The row shaped again with tertiary text inside `selected` (buffer
    /// columns) drawn in `text.secondary`: `text.tertiary` must not sit on
    /// `selection.text`.
    pub fn with_selection(
        &self,
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
        shape(window, &self.text.display, &runs)
    }
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
    pub text: &'a TextSnapshot,
    pub fonts: &'a Fonts,
    pub palette: &'a Palette,
    pub theme: ThemeId,
    pub whitespace: bool,
    pub tab_size: u32,
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
    /// The row of `line`, shaped now unless the cached one still fits.
    pub fn row(
        &mut self,
        line: usize,
        style: Option<&Rc<LineStyle>>,
        env: RowEnv<'_>,
        window: &Window,
    ) -> Option<&Row> {
        let range = env.text.line_range(line).ok()?;
        let content = env.text.text_for_range(range).ok()?;
        self.used.push(line);
        let fresh = match self.rows.get(&line) {
            Some(row) => {
                row.content != content
                    || row.whitespace != env.whitespace
                    || row.theme != env.theme
                    || row.tab_size != env.tab_size
                    || match (&row.style, style) {
                        (Some(a), Some(b)) => !Rc::ptr_eq(a, b) && **a != **b,
                        (None, None) => false,
                        _ => true,
                    }
            }
            None => true,
        };
        if fresh {
            let row = make_row(content, style.cloned(), env, window);
            self.rows.insert(line, row);
        }
        self.rows.get(&line)
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
    content: String,
    style: Option<Rc<LineStyle>>,
    env: RowEnv<'_>,
    window: &Window,
) -> Row {
    let hard_break = env.whitespace && ends_in_hard_break(&content);
    let text = RowText::new(&content, env.tab_size, env.whitespace, hard_break);
    let runs = match &style {
        Some(style) => style.clamped(content.len()),
        None if content.is_empty() => Vec::new(),
        None => vec![(0..content.len(), Style::PLAIN)],
    };
    let shaped = shape(
        window,
        &text.display,
        &text_runs(&text, &runs, &[], env.fonts, env.palette),
    );
    let decorations = decorations(&text, &runs);
    let code = style.as_ref().is_some_and(|s| s.code);
    Row {
        content,
        style,
        whitespace: env.whitespace,
        theme: env.theme,
        tab_size: env.tab_size,
        text,
        runs,
        shaped,
        decorations,
        code,
    }
}

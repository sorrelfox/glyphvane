// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Soft wrap in the Source view (SPEC § Editor).
//!
//! The editor's wrap layer breaks lines at the width of the text column,
//! measured with [`EditorMeasure`] (GPUI's text system, the editor font at
//! the editor size), and gives list items and quotes a hanging indent of
//! two characters
//! ([`MarkdownHangingIndent`](gv_editor::display_map::MarkdownHangingIndent)). The view paints wrap rows:
//! each buffer line is shaped in one piece per wrap row ([`super::rows`]),
//! from the line's [`Break`]s.
//!
//! - **Width:** the text column's width. A new width (a resize) re-wraps at
//!   once for a document up to [`SYNC_REWRAP_BYTES`]; a larger one keeps
//!   its old rows while the width keeps changing and re-wraps once it
//!   settles ([`REWRAP_DELAY`]).
//! - **Large-file mode** (the `editor.large_file_*` settings) keeps wrap
//!   off: long lines then scroll sideways.
//! - The scroll position is kept by line: the line at the top stays at
//!   the top when the rows above it change.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Context, Font, FontId, Pixels, TextSystem, px};
use gv_editor::display_map::{TextMeasure, WrapPoint, WrapRow, WrapSnapshot};
use gv_text::{Bias, Point as BufferPoint, TextSnapshot};

use super::{EditorView, LINE};
use crate::metrics::size::document;
use crate::metrics::text;

/// The largest document re-wrapped on the UI thread as soon as the width
/// changes. About 3 ms of wrapping (some 11 ms per MB); larger documents
/// wait for the width to settle.
pub(crate) const SYNC_REWRAP_BYTES: usize = 256 * 1024;

/// How long the width of a large document must stay the same before it
/// is re-wrapped.
pub(crate) const REWRAP_DELAY: Duration = Duration::from_millis(150);

/// Advance widths of the editor font through GPUI's text system, one
/// character at a time (shaped alone, so font fallback picks the face a
/// row would use), cached.
pub(crate) struct EditorMeasure {
    text_system: Arc<TextSystem>,
    font: FontId,
    size: Pixels,
    ascii: [f32; 128],
    other: HashMap<char, f32>,
    /// The width of `m`, once needed.
    em: f32,
}

impl std::fmt::Debug for EditorMeasure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorMeasure")
            .field("font", &self.font)
            .field("size", &self.size)
            .field("cached", &self.other.len())
            .finish_non_exhaustive()
    }
}

impl EditorMeasure {
    /// Measures `font` at the editor size.
    pub fn new(text_system: Arc<TextSystem>, font: &Font) -> Self {
        let id = text_system.resolve_font(font);
        Self {
            text_system,
            font: id,
            size: px(text::EDITOR.size),
            ascii: [f32::NAN; 128],
            other: HashMap::new(),
            em: f32::NAN,
        }
    }

    fn shaped_width(&self, c: char) -> f32 {
        f32::from(self.text_system.layout_width(self.font, self.size, c))
    }
}

impl TextMeasure for EditorMeasure {
    fn char_width(&mut self, c: char) -> f32 {
        if let Some(slot) = self.ascii.get(c as usize) {
            if slot.is_nan() {
                let width = self.visible_width(c);
                self.ascii[c as usize] = width;
                return width;
            }
            return *slot;
        }
        if let Some(&width) = self.other.get(&c) {
            return width;
        }
        // Combining marks and joiners take no room of their own.
        let width = if is_zero_width(c) {
            0.0
        } else {
            self.visible_width(c)
        };
        self.other.insert(c, width);
        width
    }
}

impl EditorMeasure {
    /// The width of `c` as a row draws it: a control character as its
    /// control picture, a character no font has as a box at least an `m`
    /// wide.
    fn visible_width(&mut self, c: char) -> f32 {
        let drawn = match c as u32 {
            n @ 0..0x20 => char::from_u32(0x2400 + n).unwrap_or(c),
            0x7f => '\u{2421}',
            _ => c,
        };
        let width = self.shaped_width(drawn);
        if width > 0.0 {
            return width;
        }
        if self.em.is_nan() {
            self.em = self.shaped_width('m');
        }
        self.em
    }
}

/// Characters drawn on the one before: combining marks, joiners,
/// variation selectors.
fn is_zero_width(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x200B..=0x200F
        | 0x20D0..=0x20FF
        | 0x2060..=0x2064
        | 0xFE00..=0xFE0F
        | 0xFE20..=0xFE2F
        | 0xE0100..=0xE01EF
    )
}

/// Where a wrap row of a line starts: a buffer column (line-relative
/// bytes) and the continuation indent in columns (0 on the first row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Break {
    pub column: usize,
    pub indent: u32,
}

/// The rows the view shows: wrap rows while soft wrap is on, buffer lines
/// otherwise.
#[derive(Clone, Copy)]
pub(crate) enum RowMap<'a> {
    Lines(&'a TextSnapshot),
    Wrap(&'a WrapSnapshot),
}

impl<'a> RowMap<'a> {
    pub fn new(text: &'a TextSnapshot, wrap: Option<&'a WrapSnapshot>) -> Self {
        match wrap {
            Some(wrap) => Self::Wrap(wrap),
            None => Self::Lines(text),
        }
    }

    pub fn is_wrapping(self) -> bool {
        matches!(self, Self::Wrap(_))
    }

    /// The number of rows.
    pub fn row_count(self) -> usize {
        match self {
            Self::Lines(text) => text.line_count(),
            Self::Wrap(wrap) => wrap.max_point().row().0 as usize + 1,
        }
    }

    /// The line row `row` belongs to (the last line past the end).
    pub fn line_of_row(self, row: usize) -> usize {
        match self {
            Self::Lines(text) => row.min(text.line_count().saturating_sub(1)),
            Self::Wrap(wrap) => {
                let max = wrap.max_point().row().0;
                let row = u32::try_from(row).unwrap_or(u32::MAX).min(max);
                wrap.wrap_point_to_buffer_point(WrapPoint::new(WrapRow(row), 0), Bias::Left)
                    .line
            }
        }
    }

    /// The first row of `line`.
    pub fn first_row(self, line: usize) -> usize {
        match self {
            Self::Lines(_) => line,
            Self::Wrap(wrap) => {
                wrap.buffer_point_to_wrap_point(BufferPoint::new(line, 0), Bias::Left)
                    .row()
                    .0 as usize
            }
        }
    }

    /// Where the rows of `line` start: one break at column 0 for an
    /// unwrapped line.
    pub fn breaks(self, line: usize) -> Vec<Break> {
        let first = Break {
            column: 0,
            indent: 0,
        };
        let Self::Wrap(wrap) = self else {
            return vec![first];
        };
        let mut breaks = vec![first];
        let mut row = WrapRow(self.first_row(line) as u32);
        while let Some(indent) = wrap.soft_wrap_indent(row) {
            row = row + WrapRow(1);
            let point = wrap.wrap_point_to_buffer_point(WrapPoint::new(row, indent), Bias::Left);
            if point.line != line {
                break;
            }
            // A row inside one expanded tab starts nowhere new.
            if breaks.last().is_some_and(|b| point.column > b.column) {
                breaks.push(Break {
                    column: point.column,
                    indent,
                });
            }
        }
        breaks
    }
}

/// Whether a document trips large-file mode: by size, line count or the
/// longest line, with the thresholds of the settings.
pub(crate) fn is_large_file(text: &TextSnapshot, limits: &gv_settings::Editor) -> bool {
    if text.len() as u64 > limits.large_file_bytes {
        return true;
    }
    let lines = text.line_count();
    if lines as u64 > u64::from(limits.large_file_lines) {
        return true;
    }
    let max_chars = limits.large_file_line_chars as usize;
    (0..lines).any(|line| {
        text.line_range(line).is_ok_and(|range| {
            // Bytes bound characters from above: count only a long line.
            range.len() > max_chars
                && text
                    .text_for_range(range)
                    .is_ok_and(|s| s.chars().count() > max_chars)
        })
    })
}

/// The view's soft-wrap state.
#[derive(Debug)]
pub(crate) struct SoftWrap {
    /// Soft wrap is wanted (outside large-file mode).
    pub enabled: bool,
    /// The editor has its measure and indent.
    pub ready: bool,
    /// A width waiting for a large document's re-wrap.
    pub pending: Option<f32>,
    /// Ignore large-file mode (timing runs).
    #[cfg(test)]
    pub force: bool,
}

impl Default for SoftWrap {
    fn default() -> Self {
        Self {
            enabled: true,
            ready: false,
            pending: None,
            #[cfg(test)]
            force: false,
        }
    }
}

impl EditorView {
    /// Whether soft wrap is wanted; large-file mode still keeps it off.
    pub fn soft_wrap(&self) -> bool {
        self.wrap.enabled
    }

    /// Turns soft wrap on or off.
    pub fn set_soft_wrap(&mut self, on: bool, cx: &mut Context<'_, Self>) {
        self.wrap.enabled = on;
        cx.notify();
    }

    /// Turns soft wrap on or off, whichever it is not.
    pub fn toggle_soft_wrap(&mut self, cx: &mut Context<'_, Self>) {
        self.set_soft_wrap(!self.wrap.enabled, cx);
    }

    /// Whether the document is in large-file mode.
    pub fn is_large_file(&self) -> bool {
        self.doc.as_ref().is_some_and(|d| d.large_file)
    }

    /// The rows the view shows now.
    pub(crate) fn row_map(&self) -> Option<RowMap<'_>> {
        let editor = &self.doc.as_ref()?.editor;
        Some(RowMap::new(editor.text(), editor.wrap()))
    }

    /// The wrap width wanted for a text column `text_w` wide.
    fn wanted_wrap_width(&self, text_w: f32) -> Option<f32> {
        let doc = self.doc.as_ref()?;
        #[cfg(test)]
        let large = doc.large_file && !self.wrap.force;
        #[cfg(not(test))]
        let large = doc.large_file;
        (self.wrap.enabled && !large).then_some(text_w)
    }

    /// Brings the editor's wrap width in line with the text column, before
    /// a frame is laid out.
    pub(crate) fn sync_wrap(&mut self, text_w: f32, cx: &mut Context<'_, Self>) {
        let wanted = self.wanted_wrap_width(text_w);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let have = doc.editor.wrap_width();
        if wanted == have {
            self.wrap.pending = None;
            return;
        }
        if wanted.is_some() && !self.wrap.ready {
            let measure = EditorMeasure::new(cx.text_system().clone(), self.fonts.regular());
            doc.editor.set_measure(Box::new(measure));
            self.wrap.ready = true;
        }
        let large = doc.editor.text().len() > SYNC_REWRAP_BYTES;
        if have.is_some() && wanted.is_some() && large {
            if self.wrap.pending != wanted {
                self.wrap.pending = wanted;
                self._rewrap = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(REWRAP_DELAY).await;
                    this.update(cx, |view, cx| {
                        if view.wrap.pending == wanted {
                            view.wrap.pending = None;
                            view.apply_wrap_width(wanted);
                            cx.notify();
                        }
                    })
                    .ok();
                }));
            }
            return;
        }
        self.wrap.pending = None;
        self.apply_wrap_width(wanted);
    }

    /// Sets the editor's wrap width, keeping the line at the top in place.
    pub(crate) fn apply_wrap_width(&mut self, width: Option<f32>) {
        let pad = f64::from(document::PAD_TOP);
        let row_px = f64::from(LINE);
        let Some(doc) = self.doc.as_mut() else {
            return;
        };
        let (line, delta) = {
            let map = RowMap::new(doc.editor.text(), doc.editor.wrap());
            let row = ((self.scroll_y - pad) / row_px).floor().max(0.0) as usize;
            let line = map.line_of_row(row);
            let line_top = pad + map.first_row(line) as f64 * row_px;
            (line, self.scroll_y - line_top)
        };
        doc.editor.set_wrap_width(width);
        if self.scroll_y > 0.0 {
            let map = RowMap::new(doc.editor.text(), doc.editor.wrap());
            let rows = map.breaks(line).len() as f64;
            let line_top = pad + map.first_row(line) as f64 * row_px;
            self.scroll_y = (line_top + delta.min(rows * row_px - 1.0)).max(0.0);
        }
        if width.is_some() {
            self.scroll_x = 0.0;
        }
    }
}

/// The large-file thresholds in effect.
pub(crate) fn large_file_limits(cx: &App) -> gv_settings::Editor {
    cx.try_global::<crate::AppSettings>()
        .map(|s| s.settings().editor.clone())
        .unwrap_or_default()
}

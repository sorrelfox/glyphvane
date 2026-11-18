// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Soft wrap in the Source view (SPEC § Editor).
//!
//! The editor's wrap layer breaks lines at the width of the text column,
//! measured with [`EditorMeasure`] (GPUI's text system, the editor font at
//! the view's text size, [`super::metrics`]), and gives list items and quotes a hanging indent of
//! two characters
//! ([`MarkdownHangingIndent`](gv_editor::display_map::MarkdownHangingIndent)). The view paints wrap rows:
//! each buffer line is shaped in one piece per wrap row ([`super::rows`]),
//! from the line's [`Break`]s.
//!
//! - **Width:** the text column's width. A new width (a resize) re-wraps at
//!   once for a document up to [`SYNC_REWRAP_BYTES`]. A larger document is
//!   re-wrapped on GPUI's background executor ([`EditorView::start_rewrap`]):
//!   the editor hands out a [`RewrapJob`] and shows the old wraps (lines
//!   unwrapped, when soft wrap was just turned on) until the job lands;
//!   edits made meanwhile are kept. The worker measures with its own
//!   [`EditorMeasure`]: GPUI's text system is `Send + Sync` (its platform
//!   text system is behind locks), so the worker shapes characters itself,
//!   each once per job. While the width keeps changing (a resize drag), a
//!   large document waits for it to settle ([`REWRAP_DELAY`]) before a job
//!   starts; a newer job cancels the one running.
//! - **Large-file mode** (the `editor.large_file_*` settings) keeps wrap
//!   off: long lines then scroll sideways.
//! - **A new font or size** (the zoom, the editor font settings) re-wraps
//!   the same way with a new measure: at once up to
//!   [`SYNC_REWRAP_BYTES`], else in the background
//!   ([`gv_editor::Editor::defer_measure`]).
//! - The scroll position is kept by line: the line at the top stays at
//!   the top when the rows above it change, or get taller.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, Context, Font, FontId, Pixels, Task, TextSystem, px};
use gv_editor::Editor;
use gv_editor::display_map::{
    LineWrapper, RewrapJob, TabPoint, TabSnapshot, TextMeasure, WrapPoint, WrapRow, WrapSnapshot,
};
use gv_text::TextSnapshot;

use super::EditorView;
use super::fold::{PLACEHOLDER_CHAR, VisualLine};

/// The largest document re-wrapped on the UI thread as soon as the width
/// changes. About 3 ms of wrapping (some 11 ms per MB); larger documents
/// are re-wrapped in the background.
pub(crate) const SYNC_REWRAP_BYTES: usize = 256 * 1024;

/// How long the width of a large document must stay the same before it
/// is re-wrapped.
pub(crate) const REWRAP_DELAY: Duration = Duration::from_millis(150);

/// Tab rows a background re-wrap wraps between looks at its cancel flag.
const REWRAP_STEP_ROWS: u32 = 4096;

/// The worker measures with GPUI's text system off the UI thread.
const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    const fn send<T: Send>() {}
    send_sync::<TextSystem>();
    send::<EditorMeasure>();
    send::<RewrapJob>();
};

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
    /// Measures `font` at `size` px.
    pub fn new(text_system: Arc<TextSystem>, font: &Font, size: f32) -> Self {
        let id = text_system.resolve_font(font);
        Self {
            text_system,
            font: id,
            size: px(size),
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
        if c == PLACEHOLDER_CHAR {
            // A fold placeholder is drawn with a space on each side
            // (`super::layout`).
            return self.shaped_width(c) + 2.0 * self.shaped_width(' ');
        }
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

/// Where a wrap row of a line starts: a column of the line's tab row and
/// the continuation indent in columns (0 on the first row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Break {
    pub column: usize,
    pub indent: u32,
}

/// The rows the view shows: wrap rows while soft wrap is on, else the rows
/// of the tab layer (buffer lines, less the lines folds hide).
///
/// Lines are named by buffer line; a line hidden in a fold belongs to the
/// visual line ([`VisualLine`]) its fold is on.
#[derive(Clone, Copy)]
pub(crate) enum RowMap<'a> {
    /// No folds, no wrap: a row per buffer line.
    Lines(&'a TextSnapshot),
    /// Folds, no wrap: a row per tab row.
    Tabs(&'a TabSnapshot),
    Wrap(&'a WrapSnapshot),
}

impl<'a> RowMap<'a> {
    pub fn new(
        text: &'a TextSnapshot,
        tabs: &'a TabSnapshot,
        wrap: Option<&'a WrapSnapshot>,
    ) -> Self {
        match wrap {
            Some(wrap) => Self::Wrap(wrap),
            None if tabs.replace_snapshot().has_folds() => Self::Tabs(tabs),
            None => Self::Lines(text),
        }
    }

    /// The rows of `editor`.
    pub fn of(editor: &'a Editor) -> Self {
        Self::new(editor.text(), editor.display(), editor.wrap())
    }

    pub fn is_wrapping(self) -> bool {
        matches!(self, Self::Wrap(_))
    }

    /// The number of rows.
    pub fn row_count(self) -> usize {
        match self {
            Self::Lines(text) => text.line_count(),
            Self::Tabs(tabs) => tabs.max_point().row() as usize + 1,
            Self::Wrap(wrap) => wrap.max_point().row().0 as usize + 1,
        }
    }

    /// The first buffer line of the visual line row `row` belongs to (the
    /// last one past the end).
    pub fn line_of_row(self, row: usize) -> usize {
        match self {
            Self::Lines(text) => row.min(text.line_count().saturating_sub(1)),
            Self::Tabs(tabs) => {
                let max = tabs.max_point().row();
                let row = u32::try_from(row).unwrap_or(u32::MAX).min(max);
                tabs.replace_snapshot().buffer_line(row)
            }
            Self::Wrap(wrap) => {
                let max = wrap.max_point().row().0;
                let row = u32::try_from(row).unwrap_or(u32::MAX).min(max);
                let tab_row = wrap.to_tab_point(WrapPoint::new(WrapRow(row), 0)).row();
                wrap.tab_snapshot().replace_snapshot().buffer_line(tab_row)
            }
        }
    }

    /// The tab row of buffer line `line`'s visual line.
    fn tab_row(tabs: &TabSnapshot, line: usize) -> u32 {
        tabs.buffer_line_to_tab_row(line)
    }

    /// The first row of the visual line that shows `line`.
    pub fn first_row(self, line: usize) -> usize {
        match self {
            Self::Lines(_) => line,
            Self::Tabs(tabs) => Self::tab_row(tabs, line) as usize,
            Self::Wrap(wrap) => {
                let tab_row = Self::tab_row(wrap.tab_snapshot(), line);
                wrap.tab_point_to_wrap_point(TabPoint::new(tab_row, 0))
                    .row()
                    .0 as usize
            }
        }
    }

    /// One past the last row of the visual line that shows `line`.
    pub fn row_end(self, line: usize) -> usize {
        match self {
            Self::Lines(_) | Self::Tabs(_) => self.first_row(line) + 1,
            Self::Wrap(wrap) => {
                let tabs = wrap.tab_snapshot();
                let tab_row = Self::tab_row(tabs, line);
                if tab_row >= tabs.max_point().row() {
                    return self.row_count();
                }
                wrap.tab_point_to_wrap_point(TabPoint::new(tab_row + 1, 0))
                    .row()
                    .0 as usize
            }
        }
    }

    /// The first buffer line of the visual line that shows `line`.
    pub fn visual_start(self, line: usize) -> usize {
        match self {
            Self::Lines(_) => line,
            Self::Tabs(tabs) => tabs
                .replace_snapshot()
                .buffer_line(Self::tab_row(tabs, line)),
            Self::Wrap(wrap) => {
                let tabs = wrap.tab_snapshot();
                tabs.replace_snapshot()
                    .buffer_line(Self::tab_row(tabs, line))
            }
        }
    }

    /// Where the rows of visual line `line` start, in columns of its tab
    /// row (the wrap layer's row starts): one break at column 0 for a line
    /// that does not wrap.
    pub fn breaks(self, line: &VisualLine) -> Vec<Break> {
        let first = Break {
            column: 0,
            indent: 0,
        };
        let Self::Wrap(wrap) = self else {
            return vec![first];
        };
        let mut breaks = vec![first];
        let mut row = wrap
            .tab_point_to_wrap_point(TabPoint::new(line.row, 0))
            .row();
        while let Some(indent) = wrap.soft_wrap_indent(row) {
            row = row + WrapRow(1);
            let point = wrap.to_tab_point(WrapPoint::new(row, indent));
            if point.row() != line.row {
                break;
            }
            let column = point.column() as usize;
            if breaks.last().is_some_and(|b| column > b.column) {
                breaks.push(Break { column, indent });
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
    /// A width waiting to settle before a large document's re-wrap.
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
        // A width waiting to settle is for the old state.
        self.wrap.pending = None;
        self._rewrap_wait = None;
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
        Some(RowMap::of(&self.doc.as_ref()?.editor))
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
            // In step, or a background re-wrap to this width is running.
            self.wrap.pending = None;
            self._rewrap_wait = None;
            return;
        }
        if wanted.is_some() && !self.wrap.ready {
            let measure = EditorMeasure::new(
                cx.text_system().clone(),
                self.fonts.regular(),
                self.metrics.text_size,
            );
            doc.editor.set_measure(Box::new(measure));
            self.wrap.ready = true;
        }
        let large = doc.editor.text().len() > SYNC_REWRAP_BYTES;
        match (have, wanted) {
            // A new width: once it settles.
            (Some(_), Some(width)) if large => {
                if self.wrap.pending != wanted {
                    self.wrap.pending = wanted;
                    self._rewrap_wait = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(REWRAP_DELAY).await;
                        this.update(cx, |view, cx| {
                            if view.wrap.pending == wanted {
                                view.wrap.pending = None;
                                view.start_rewrap(width, cx);
                                cx.notify();
                            }
                        })
                        .ok();
                    }));
                }
            }
            // Soft wrap turned on (the first frame of a document too).
            (None, Some(width)) if large => {
                self.wrap.pending = None;
                self._rewrap_wait = None;
                self.start_rewrap(width, cx);
            }
            _ => {
                self.wrap.pending = None;
                self._rewrap_wait = None;
                self._rewrap = None;
                self.apply_wrap_width(wanted);
            }
        }
    }

    /// Sets the editor's wrap width, re-wrapping at once and keeping the
    /// line at the top in place.
    pub(crate) fn apply_wrap_width(&mut self, width: Option<f32>) {
        self.keep_top_line(|editor| editor.set_wrap_width(width));
        if width.is_some() {
            self.scroll_x = 0.0;
        }
    }

    /// Sets the editor's wrap width to `width` and re-wraps on the
    /// background executor; the job replaces (and cancels) one running.
    /// The editor shows interpolated rows until the job lands.
    pub(crate) fn start_rewrap(&mut self, width: f32, cx: &mut Context<'_, Self>) {
        // Measured on the worker like the editor's own measure.
        let measure = EditorMeasure::new(
            cx.text_system().clone(),
            self.fonts.regular(),
            self.metrics.text_size,
        );
        let job = self
            .keep_top_line(|editor| editor.defer_wrap_width(Some(width)))
            .flatten();
        self.scroll_x = 0.0;
        let Some(job) = job else {
            return;
        };
        self.spawn_rewrap(job, measure, cx);
    }

    /// Runs `change` on the editor, keeping the line at the top in place
    /// (and the offset into it, within its new rows).
    pub(super) fn keep_top_line<R>(&mut self, change: impl FnOnce(&mut Editor) -> R) -> Option<R> {
        let line = self.metrics.line;
        self.keep_top_line_across(line, line, change)
    }

    /// Like [`Self::keep_top_line`], for rows that were `old_px` tall and
    /// are `new_px` tall after `change` (the offset into the line scales
    /// with them).
    pub(super) fn keep_top_line_across<R>(
        &mut self,
        old_px: f32,
        new_px: f32,
        change: impl FnOnce(&mut Editor) -> R,
    ) -> Option<R> {
        let pad = f64::from(self.layout.pad_top());
        let (old_px, new_px) = (f64::from(old_px), f64::from(new_px));
        let doc = self.doc.as_mut()?;
        let (line, delta) = {
            let map = RowMap::of(&doc.editor);
            // Live Preview blocks: rows are where the block layer has them.
            let blocks = doc.editor.blocks();
            let blocks = (blocks.block_count() > 0).then_some(blocks);
            let row = match blocks {
                Some(blocks) => {
                    let y = gv_editor::display_map::Y::from_px((self.scroll_y - pad).max(0.0));
                    match blocks.item_at(y) {
                        Some(gv_editor::display_map::DisplayItem::Row { row, .. }) => row as usize,
                        Some(gv_editor::display_map::DisplayItem::Block(item)) => {
                            item.rows.start as usize
                        }
                        None => map.row_count(),
                    }
                }
                None => ((self.scroll_y - pad) / old_px).floor().max(0.0) as usize,
            };
            let line = map.line_of_row(row);
            let line_top = pad
                + match blocks {
                    Some(blocks) => blocks.row_top(map.first_row(line) as u32).to_px(),
                    None => map.first_row(line) as f64 * old_px,
                };
            // Into the line's rows: scaled with them; above the first row
            // (the top padding, which does not scale): kept.
            let delta = self.scroll_y - line_top;
            (
                line,
                if delta > 0.0 {
                    delta * new_px / old_px
                } else {
                    delta
                },
            )
        };
        let result = change(&mut doc.editor);
        if self.scroll_y > 0.0 {
            let map = RowMap::of(&doc.editor);
            let first = map.first_row(line);
            let rows = map.row_end(line).saturating_sub(first).max(1) as f64;
            let line_top = if doc.editor.blocks().block_count() > 0 {
                doc.editor
                    .set_row_height(gv_editor::display_map::Height::from_px(new_px as f32));
                pad + doc.editor.blocks().row_top(first as u32).to_px()
            } else {
                pad + first as f64 * new_px
            };
            self.scroll_y = (line_top + delta.min(rows * new_px - 1.0)).max(0.0);
        }
        Some(result)
    }
}

/// A re-wrap running on the background executor; dropping it cancels it.
pub(crate) struct Rewrap {
    _task: Task<()>,
    cancel: Arc<AtomicBool>,
}

impl Rewrap {
    /// The re-wrap `task` runs, which gives up once `cancel` is set.
    pub(super) fn new(task: Task<()>, cancel: Arc<AtomicBool>) -> Self {
        Self {
            _task: task,
            cancel,
        }
    }
}

impl Drop for Rewrap {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Runs `job` to the end with a wrapper around `measure`, in steps, or
/// gives up once `stop` is set.
pub(super) fn run_rewrap(
    mut job: RewrapJob,
    measure: EditorMeasure,
    stop: &AtomicBool,
) -> Option<RewrapJob> {
    let mut wrapper = LineWrapper::new(Box::new(measure));
    while !job.run(&mut wrapper, REWRAP_STEP_ROWS) {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
    }
    Some(job)
}

/// The large-file thresholds in effect.
pub(crate) fn large_file_limits(cx: &App) -> gv_settings::Editor {
    cx.try_global::<crate::AppSettings>()
        .map(|s| s.settings().editor.clone())
        .unwrap_or_default()
}

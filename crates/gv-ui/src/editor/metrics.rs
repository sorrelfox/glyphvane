// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The Source view's sizes from the Appearance settings: the editor font
//! and size, the zoom ([`crate::zoom`]) and the readable line width.
//!
//! - **Scale**: the editor font size over the designed 12.5 px, times the
//!   zoom. The text, the line numbers, the row height (the design's 20 px
//!   rows: 1.6 times the text size, rounded to whole pixels), the caret and
//!   the fold chips scale with it; paddings and the gutter's gaps do not,
//!   as the Reading view keeps its paddings.
//! - **Column**: in Edit mode the text column is the Reading view's: at
//!   most `readable_line_width_chars` characters of the preview body wide,
//!   scaled with the preview font size and the zoom, so the text keeps its
//!   place when the mode changes; with the readable line width off it fills
//!   the view between its side paddings.
//! - **Family**: the editor font as set, when it is installed; else
//!   JetBrains Mono or a common monospace family, as before.
//!
//! The view follows the settings live ([`EditorView::sync_metrics`]): rows
//! are shaped again, soft wrap re-wraps with the new measure (in the
//! background for a large document, as for a new width) and the line at
//! the top of the view stays there.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::{App, Context, SharedString};

use super::EditorView;
use super::rows::Fonts;
use super::wrap::{EditorMeasure, Rewrap, SYNC_REWRAP_BYTES, run_rewrap};
use crate::inline::FontFamilies;
use crate::metrics::size::{document, editor as editor_size};
use crate::metrics::text;
use crate::settings::AppSettings;

/// The designed readable width in characters: 680 px at the 15 px preview
/// body (as the Reading view counts it).
const DESIGN_CHARS: f32 = 72.0;

/// The Source view's sizes, in px.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EditorMetrics {
    /// The factor over the design's sizes.
    pub scale: f32,
    /// The text size.
    pub text_size: f32,
    /// The height of a row.
    pub line: f32,
    /// The line numbers' size.
    pub number_size: f32,
    /// The caret's height.
    pub caret_height: f32,
    /// A fold placeholder chip's height, and its inset from the `⋯`.
    pub chip_height: f32,
    pub chip_inset: f32,
    /// The widest the Edit mode text column gets (infinite: no limit).
    pub column_max: f32,
    /// The editor font family as set.
    pub family: SharedString,
}

impl Default for EditorMetrics {
    fn default() -> Self {
        Self::from_settings(&gv_settings::Appearance::default(), 1.0)
    }
}

/// `value` when it is a usable size, else `fallback`.
fn positive(value: f32, fallback: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

impl EditorMetrics {
    /// The sizes of the settings in effect.
    pub fn from_app(cx: &App) -> Self {
        let zoom = crate::zoom::factor(cx);
        match cx.try_global::<AppSettings>() {
            Some(settings) => Self::from_settings(&settings.settings().appearance, zoom),
            None => Self::from_settings(&gv_settings::Appearance::default(), zoom),
        }
    }

    /// The sizes of `appearance` at `zoom` (1.0 at 100 %).
    pub fn from_settings(appearance: &gv_settings::Appearance, zoom: f32) -> Self {
        let zoom = positive(zoom, 1.0);
        let size = positive(appearance.editor_font_size, text::EDITOR.size);
        let scale = size / text::EDITOR.size * zoom;
        let preview = positive(appearance.preview_font_size, text::PREVIEW_BODY.size);
        let column_max = if appearance.readable_line_width {
            let char_w = document::COLUMN_MAX / DESIGN_CHARS / text::PREVIEW_BODY.size;
            (appearance.readable_line_width_chars as f32 * char_w * preview * zoom).round()
        } else {
            f32::INFINITY
        };
        Self {
            scale,
            text_size: size * zoom,
            line: (text::EDITOR.line * scale).round().max(1.0),
            number_size: text::EDITOR_LINE_NUMBER.size * scale,
            caret_height: (editor_size::CARET_HEIGHT * scale).round().max(1.0),
            chip_height: (editor_size::FOLD_CHIP_HEIGHT * scale).round().max(1.0),
            chip_inset: editor_size::FOLD_CHIP_INSET_X * scale,
            column_max,
            family: appearance.editor_font.clone().into(),
        }
    }

    /// The editor family to use among the `installed` ones: the one set
    /// when it is installed, else the usual fallback.
    pub fn families(&self, installed: &[String]) -> FontFamilies {
        let mut families = FontFamilies::detect(installed);
        if installed
            .iter()
            .any(|name| name.as_str() == self.family.as_ref())
        {
            families.mono = self.family.clone();
        }
        families
    }

    /// Whether the text is measured differently in `other` (the wraps
    /// change).
    pub fn measure_differs(&self, other: &Self) -> bool {
        self.text_size != other.text_size || self.family != other.family
    }
}

impl EditorView {
    /// The sizes in effect.
    #[cfg(test)]
    pub(crate) fn metrics(&self) -> &EditorMetrics {
        &self.metrics
    }

    /// Follows the Appearance settings and the zoom: see the module
    /// documentation.
    pub(crate) fn sync_metrics(&mut self, cx: &mut Context<'_, Self>) {
        let new = EditorMetrics::from_app(cx);
        self.set_metrics(new, cx);
    }

    /// Changes the sizes to `new`, keeping the line at the top in place.
    pub(crate) fn set_metrics(&mut self, new: EditorMetrics, cx: &mut Context<'_, Self>) {
        if new == self.metrics {
            return;
        }
        let old = std::mem::replace(&mut self.metrics, new);
        let remeasure = old.measure_differs(&self.metrics);
        if old.family != self.metrics.family {
            let families = self.metrics.families(&cx.text_system().all_font_names());
            self.fonts = Fonts::new(&families, self.metrics.text_size);
        } else {
            self.fonts.set_size(self.metrics.text_size);
        }
        self.advances = None;
        self.widest = 0.0;
        if let Some(doc) = self.doc.as_mut() {
            // Every row is shaped again at the new size.
            doc.rows = super::rows::RowCache::default();
        }
        let (old_line, new_line) = (old.line, self.metrics.line);
        if remeasure && self.wrap.ready {
            let measure = EditorMeasure::new(
                cx.text_system().clone(),
                self.fonts.regular(),
                self.metrics.text_size,
            );
            let large = self
                .doc
                .as_ref()
                .is_some_and(|d| d.editor.text().len() > SYNC_REWRAP_BYTES);
            if large {
                // As for a new width: the old wraps show until the job
                // lands.
                let worker = EditorMeasure::new(
                    cx.text_system().clone(),
                    self.fonts.regular(),
                    self.metrics.text_size,
                );
                let job = self
                    .keep_top_line_across(old_line, new_line, |editor| {
                        editor.defer_measure(Box::new(measure))
                    })
                    .flatten();
                self._rewrap = None;
                if let Some(job) = job {
                    self.spawn_rewrap(job, worker, cx);
                }
            } else {
                self._rewrap = None;
                self.keep_top_line_across(old_line, new_line, |editor| {
                    editor.set_measure(Box::new(measure));
                });
            }
        } else {
            self.keep_top_line_across(old_line, new_line, |_| ());
        }
        cx.notify();
    }

    /// Runs `job` with `measure` on the background executor and installs
    /// it when it lands, keeping the line at the top in place; it replaces
    /// (and cancels) a re-wrap running.
    pub(super) fn spawn_rewrap(
        &mut self,
        job: gv_editor::display_map::RewrapJob,
        measure: EditorMeasure,
        cx: &mut Context<'_, Self>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let task = cx.spawn(async move |this, cx| {
            let job = cx
                .background_executor()
                .spawn(async move { run_rewrap(job, measure, &stop) })
                .await;
            let Some(job) = job else {
                return;
            };
            this.update(cx, |view, cx| {
                view.keep_top_line(|editor| editor.finish_rewrap(job));
                cx.notify();
            })
            .ok();
        });
        self._rewrap = Some(Rewrap::new(task, cancel));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_design() {
        let m = EditorMetrics::default();
        assert_eq!(m.scale, 1.0);
        assert_eq!(m.text_size, text::EDITOR.size);
        assert_eq!(m.line, text::EDITOR.line);
        assert_eq!(m.number_size, text::EDITOR_LINE_NUMBER.size);
        assert_eq!(m.caret_height, editor_size::CARET_HEIGHT);
        assert_eq!(m.chip_height, editor_size::FOLD_CHIP_HEIGHT);
        assert_eq!(m.column_max, document::COLUMN_MAX);
        assert_eq!(m.family.as_ref(), "JetBrains Mono");
    }

    #[test]
    fn zoom_and_font_size_multiply() {
        let mut a = gv_settings::Appearance::default();
        let m = EditorMetrics::from_settings(&a, 1.5);
        assert_eq!(m.text_size, 12.5 * 1.5);
        assert_eq!(m.line, 30.0);
        assert_eq!(m.caret_height, 24.0);
        assert_eq!(m.column_max, (document::COLUMN_MAX * 1.5).round());
        a.editor_font_size = 15.0;
        let m = EditorMetrics::from_settings(&a, 1.0);
        assert_eq!(m.text_size, 15.0);
        assert_eq!(m.line, 24.0, "1.6 times the size");
        assert_eq!(m.number_size, 11.0 * 1.2);
        // The column follows the preview's size, not the editor's.
        assert_eq!(m.column_max, document::COLUMN_MAX);
        a.editor_font_size = f32::NAN;
        assert_eq!(
            EditorMetrics::from_settings(&a, 0.0),
            EditorMetrics::default()
        );
    }

    #[test]
    fn readable_width_off_has_no_column_limit() {
        let a = gv_settings::Appearance {
            readable_line_width: false,
            ..Default::default()
        };
        assert!(
            EditorMetrics::from_settings(&a, 1.0)
                .column_max
                .is_infinite()
        );
    }

    #[test]
    fn the_family_set_is_used_when_installed() {
        let a = gv_settings::Appearance {
            editor_font: "Fira Code".to_owned(),
            ..Default::default()
        };
        let m = EditorMetrics::from_settings(&a, 1.0);
        let installed = ["Fira Code".to_owned(), "JetBrains Mono".to_owned()];
        assert_eq!(m.families(&installed).mono.as_ref(), "Fira Code");
        assert_eq!(
            m.families(&installed[1..]).mono.as_ref(),
            "JetBrains Mono",
            "not installed: the fallback"
        );
    }
}

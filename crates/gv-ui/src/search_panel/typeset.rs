// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Context rows with typeset formulas, shared by the Search panel's result
//! lines and the Backlinks panel's context lines: a row's text, its marks
//! and its formulas ([`Line`]) cut into text runs and typeset formulas
//! ([`segments`]), and a formula drawn at the row's size ([`math_run`]).

use std::ops::Range;
use std::sync::Arc;

use gpui::{Styled as _, canvas, point, px};
use gv_math::{MacroSet, MathLayout};

use super::excerpt;
use crate::metrics::{font, text};

/// A context row as drawn: its text with the ellipses, its marks and its
/// formulas (byte ranges of the text).
#[derive(Debug, Clone, Default)]
pub(crate) struct Line {
    pub text: String,
    pub marks: Vec<Range<usize>>,
    pub math: Vec<excerpt::Formula>,
}

/// A run of a context row: text with its marks (byte ranges of the run),
/// or a typeset formula.
#[derive(Debug, Clone)]
pub(crate) enum Segment {
    Text {
        text: String,
        marks: Vec<Range<usize>>,
    },
    Math {
        /// The formula's TeX (what the tests check).
        #[cfg_attr(not(test), allow(dead_code))]
        tex: String,
        layout: Arc<MathLayout>,
    },
}

/// `line` cut into text and typeset formulas, as the row draws it.
///
/// Formulas are typeset through the Reading view's math engine and its
/// layout cache (a formula is typeset once; later frames look it up), in
/// text style: a row holds one line, so display math (`$$…$$`) is typeset
/// at inline size too. The note's own macros are not known here: a formula
/// that needs them, or does not typeset, stays as its TeX. A formula that
/// a match touches stays as its TeX too, so the match is marked on the
/// characters it matched.
pub(crate) fn segments(line: &Line) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut at = 0;
    let text = |range: Range<usize>, out: &mut Vec<Segment>| {
        if range.is_empty() {
            return;
        }
        let marks = line
            .marks
            .iter()
            .filter(|m| m.start < range.end && range.start < m.end)
            .map(|m| m.start.max(range.start) - range.start..m.end.min(range.end) - range.start)
            .collect();
        out.push(Segment::Text {
            text: line.text[range].to_owned(),
            marks,
        });
    };
    let empty = MacroSet::empty();
    for f in &line.math {
        let r = f.out.clone();
        let fits = r.start >= at
            && r.end <= line.text.len()
            && line.text.is_char_boundary(r.start)
            && line.text.is_char_boundary(r.end);
        let marked = line
            .marks
            .iter()
            .any(|m| m.start < r.end && r.start < m.end);
        if !fits || marked {
            continue;
        }
        let Ok(layout) = crate::reader::math_engine().render(&f.tex, false, &empty) else {
            continue;
        };
        text(at..r.start, &mut out);
        out.push(Segment::Math {
            tex: f.tex.clone(),
            layout,
        });
        at = r.end;
    }
    text(at..line.text.len(), &mut out);
    out
}

/// A typeset formula in a context row: sized to the row's line as the
/// editor's inline formulas are (the body text's baseline, scaled down
/// when it would leave the row), in `color`.
pub(crate) fn math_run(layout: Arc<MathLayout>, color: gpui::Hsla) -> gpui::Canvas<()> {
    let (size, line) = (text::UI_SECTION.size, text::UI_SECTION.line);
    let ink = (layout.height + layout.depth).max(f32::EPSILON);
    let em = size.min(line * 0.9 / ink);
    let natural = (line + size * 0.7) / 2.0;
    let baseline = natural
        .max(layout.height * em)
        .min(line - layout.depth * em);
    let width = (layout.width * em).max(1.0);
    let text_font = gpui::font(font::SYSTEM_FALLBACK);
    canvas(
        |_, _, _| {},
        move |bounds, (), window, cx| {
            let origin = point(
                bounds.origin.x,
                bounds.origin.y + px(baseline - layout.height * em),
            );
            crate::inline::paint_math(window, cx, &layout, origin, em, color, &text_font);
        },
    )
    .flex_none()
    .w(px(width))
    .h(px(line))
}

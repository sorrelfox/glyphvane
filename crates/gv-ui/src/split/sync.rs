// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Scroll sync's mapping between the two panes.
//!
//! Both panes' tops are expressed as a **fractional source line**: line
//! `n` plus the part of its height above the top edge. The editor gives
//! it from its display rows ([`EditorView::top_position`]); in the preview
//! every item covers the source lines of its source range, and the gap
//! above an item covers the blank lines before it. A position inside an
//! item is interpolated linearly by lines, so a long code block or a long
//! paragraph scrolls through in step with its lines. Negative positions
//! are the top padding of either pane (-1 at the very top).
//!
//! [`EditorView::top_position`]: crate::editor::EditorView::top_position

use gv_render_tree::RenderTree;
use gv_text::TextSnapshot;

use crate::reader::Spot;

/// The preview's items as scroll sync sees them.
pub(crate) trait Lines {
    /// The number of items.
    fn len(&self) -> usize;
    /// Item `ix`'s first source line and the line after its last.
    fn lines(&self, ix: usize) -> (usize, usize);
    /// The item holding the start of source line `line`, or the first one
    /// after it; `None` past the last item.
    fn item_at_line(&self, line: usize) -> Option<usize>;
}

/// The preview's render tree with the line index of the text it was built
/// from.
pub(crate) struct Preview<'a> {
    pub tree: &'a RenderTree,
    pub text: &'a TextSnapshot,
}

impl Lines for Preview<'_> {
    fn len(&self) -> usize {
        self.tree.items().len()
    }

    fn lines(&self, ix: usize) -> (usize, usize) {
        let len = self.text.len();
        let source = &self.tree.items()[ix].source;
        let line = |offset: usize| self.text.line_of_offset(offset.min(len)).unwrap_or(0);
        let start = line(source.start);
        let last = line(source.end.saturating_sub(1).max(source.start));
        (start, last.max(start) + 1)
    }

    fn item_at_line(&self, line: usize) -> Option<usize> {
        let last = self.text.line_count().saturating_sub(1);
        let offset = self.text.line_start(line.min(last)).ok()?;
        self.tree.item_at(offset)
    }
}

/// The line after the previous item's last line: where the gap above item
/// `ix` starts.
fn gap_start(doc: &impl Lines, ix: usize) -> usize {
    ix.checked_sub(1).map_or(0, |prev| doc.lines(prev).1)
}

/// The place in the preview for fractional source line `position` at the
/// editor's top.
pub(crate) fn to_preview(doc: &impl Lines, position: f64) -> Option<(usize, Spot)> {
    let n = doc.len();
    if n == 0 {
        return None;
    }
    if position < 0.0 {
        return Some((0, Spot::Lead(position.max(-1.0) as f32)));
    }
    let Some(ix) = doc.item_at_line(position.floor() as usize) else {
        return Some((n - 1, Spot::Content(1.0)));
    };
    let (start, end) = doc.lines(ix);
    if position < start as f64 {
        let from = gap_start(doc, ix).min(start);
        let span = (start - from) as f64;
        let g = if span > 0.0 {
            (position - from as f64) / span
        } else {
            1.0
        };
        return Some((ix, Spot::Gap(g.clamp(0.0, 1.0) as f32)));
    }
    let span = end.saturating_sub(start).max(1) as f64;
    let c = (position - start as f64) / span;
    Some((ix, Spot::Content(c.clamp(0.0, 1.0) as f32)))
}

/// The fractional source line for the editor's top, for place `spot` of
/// item `ix` at the preview's top.
pub(crate) fn to_editor(doc: &impl Lines, ix: usize, spot: Spot) -> f64 {
    let ix = ix.min(doc.len().saturating_sub(1));
    match spot {
        Spot::Lead(f) => f64::from(f).clamp(-1.0, 0.0),
        Spot::Gap(g) => {
            let (start, _) = doc.lines(ix);
            let from = gap_start(doc, ix).min(start);
            from as f64 + f64::from(g.clamp(0.0, 1.0)) * (start - from) as f64
        }
        Spot::Content(c) => {
            let (start, end) = doc.lines(ix);
            start as f64 + f64::from(c.clamp(0.0, 1.0)) * end.saturating_sub(start) as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Items as (first line, line after the last).
    struct Fake(Vec<(usize, usize)>);

    impl Lines for Fake {
        fn len(&self) -> usize {
            self.0.len()
        }

        fn lines(&self, ix: usize) -> (usize, usize) {
            self.0[ix]
        }

        fn item_at_line(&self, line: usize) -> Option<usize> {
            let k = self.0.partition_point(|&(_, end)| end <= line);
            (k < self.0.len()).then_some(k)
        }
    }

    /// A heading on line 0, a paragraph on 2..5, code on 6..56 and a
    /// paragraph on 57, with blank lines between.
    fn doc() -> Fake {
        Fake(vec![(0, 1), (2, 5), (6, 56), (57, 58)])
    }

    #[test]
    fn lines_map_into_items_and_gaps() {
        let d = doc();
        assert_eq!(to_preview(&d, 0.0), Some((0, Spot::Content(0.0))));
        assert_eq!(to_preview(&d, 0.5), Some((0, Spot::Content(0.5))));
        // The blank line 1 is the gap above the paragraph.
        assert_eq!(to_preview(&d, 1.5), Some((1, Spot::Gap(0.5))));
        assert_eq!(to_preview(&d, 3.5), Some((1, Spot::Content(0.5))));
        // Halfway through the code block.
        assert_eq!(to_preview(&d, 31.0), Some((2, Spot::Content(0.5))));
        assert_eq!(to_preview(&d, -0.25), Some((0, Spot::Lead(-0.25))));
        // Past the last item: its end.
        assert_eq!(to_preview(&d, 80.0), Some((3, Spot::Content(1.0))));
    }

    #[test]
    fn the_mapping_goes_both_ways() {
        let d = doc();
        for p in [
            -1.0, -0.5, 0.0, 0.75, 1.0, 1.25, 2.0, 4.9, 5.5, 6.0, 30.25, 55.0, 56.5, 57.0,
        ] {
            let (ix, spot) = to_preview(&d, p).expect("items");
            let back = to_editor(&d, ix, spot);
            assert!((back - p).abs() < 1e-5, "{p} -> {ix} {spot:?} -> {back}");
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The outline: the tree of headings, one entry per heading.
//!
//! A heading's parent is the nearest earlier heading of a lower level, so
//! skipped levels (`#` then `###`) nest one step deep. A heading's
//! **section** runs to the next heading of the same or a higher level
//! (lower number): the range a `#Heading` embed shows and in which an
//! Obsidian chain `#A#B` looks for `B`.

/// The outline entry of one heading (same index as the heading).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutlineEntry {
    /// The parent heading, if any.
    pub parent: Option<usize>,
    /// Depth in the outline tree (0 for top-level entries).
    pub depth: u32,
    /// The first heading after this one's section (the next heading of the
    /// same or a higher level), or the number of headings.
    pub section_end: usize,
}

/// Builds the outline from heading levels in document order.
pub(crate) fn build(levels: impl ExactSizeIterator<Item = u8>) -> Vec<OutlineEntry> {
    let n = levels.len();
    let mut out: Vec<OutlineEntry> = Vec::with_capacity(n);
    // Open headings, outermost first: (index, level).
    let mut stack: Vec<(usize, u8)> = Vec::new();
    for (i, level) in levels.enumerate() {
        while let Some(&(j, l)) = stack.last() {
            if l < level {
                break;
            }
            out[j].section_end = i;
            stack.pop();
        }
        let parent = stack.last().map(|&(j, _)| j);
        out.push(OutlineEntry {
            parent,
            depth: u32::try_from(stack.len()).unwrap_or(u32::MAX),
            section_end: n,
        });
        stack.push((i, level));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nesting() {
        let o = build([1u8, 3, 2, 2, 1, 4].into_iter());
        let parents: Vec<_> = o.iter().map(|e| e.parent).collect();
        assert_eq!(parents, [None, Some(0), Some(0), Some(0), None, Some(4)]);
        let depths: Vec<_> = o.iter().map(|e| e.depth).collect();
        assert_eq!(depths, [0, 1, 1, 1, 0, 1]);
        let ends: Vec<_> = o.iter().map(|e| e.section_end).collect();
        assert_eq!(ends, [4, 2, 3, 4, 6, 6]);
        assert!(build(std::iter::empty::<u8>()).is_empty());
    }
}

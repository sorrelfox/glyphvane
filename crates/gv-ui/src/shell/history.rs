// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Navigation history. Each tab keeps its own back and forward stacks; the
//! window keeps the tabs it closed, with their history, so that they can be
//! reopened.
//!
//! An entry is a place: a file, the view mode and where the Reading view
//! was scrolled (in a PDF, the page, how far down it and the zoom). Following a link (also inside one document) and jumping
//! from the outline push the place being left. Scrolling does not.

use std::path::PathBuf;

use super::ViewMode;
use crate::file_viewer::PdfPlace;
use crate::reader::ScrollTarget;

/// Most entries a tab's back stack keeps.
pub(super) const HISTORY_LIMIT: usize = 100;

/// Most closed tabs a window keeps for reopening.
pub(super) const CLOSED_TABS_LIMIT: usize = 50;

/// A place a tab was at.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub path: PathBuf,
    pub mode: ViewMode,
    /// Where the Reading view was scrolled: the start of the block at the
    /// top, never pixels.
    pub scroll: Option<ScrollTarget>,
    /// Where the PDF viewer was, in a PDF.
    pub pdf: Option<PdfPlace>,
}

/// A tab's back and forward stacks, most recent last.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct History {
    back: Vec<HistoryEntry>,
    forward: Vec<HistoryEntry>,
}

impl History {
    /// The places Back goes to, oldest first.
    pub fn back(&self) -> &[HistoryEntry] {
        &self.back
    }

    /// The places Forward goes to, the next one last.
    pub fn forward(&self) -> &[HistoryEntry] {
        &self.forward
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The tab leaves `left` for a new place: `left` becomes the way back
    /// and the way forward is forgotten.
    pub fn push(&mut self, left: HistoryEntry) {
        if self.back.len() >= HISTORY_LIMIT {
            self.back.remove(0);
        }
        self.back.push(left);
        self.forward.clear();
    }

    /// Goes back from `current`, which becomes the way forward. `None`
    /// when there is nothing to go back to.
    pub fn go_back(&mut self, current: HistoryEntry) -> Option<HistoryEntry> {
        let to = self.back.pop()?;
        self.forward.push(current);
        Some(to)
    }

    /// Goes forward from `current`, which becomes the way back.
    pub fn go_forward(&mut self, current: HistoryEntry) -> Option<HistoryEntry> {
        let to = self.forward.pop()?;
        if self.back.len() >= HISTORY_LIMIT {
            self.back.remove(0);
        }
        self.back.push(current);
        Some(to)
    }
}

/// A closed tab, as it is reopened: where it was, its place in the strip
/// and its history.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ClosedTab {
    pub index: usize,
    pub place: HistoryEntry,
    pub history: History,
}

/// The tabs a window closed, the last closed last.
#[derive(Debug, Clone, Default)]
pub(super) struct ClosedTabs(Vec<ClosedTab>);

impl ClosedTabs {
    pub fn push(&mut self, tab: ClosedTab) {
        if self.0.len() >= CLOSED_TABS_LIMIT {
            self.0.remove(0);
        }
        self.0.push(tab);
    }

    pub fn pop(&mut self) -> Option<ClosedTab> {
        self.0.pop()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(name: &str, offset: usize) -> HistoryEntry {
        HistoryEntry {
            path: PathBuf::from(name),
            mode: ViewMode::Preview,
            scroll: Some(ScrollTarget::Offset(offset)),
            pdf: None,
        }
    }

    #[test]
    fn back_and_forward_trade_places() {
        let mut h = History::default();
        assert!(!h.can_go_back() && !h.can_go_forward());
        h.push(at("a.md", 0));
        h.push(at("b.md", 10));
        // Now at c.md.
        assert_eq!(h.go_back(at("c.md", 5)), Some(at("b.md", 10)));
        assert_eq!(h.go_back(at("b.md", 12)), Some(at("a.md", 0)));
        assert_eq!(h.go_back(at("a.md", 0)), None);
        assert!(h.can_go_forward());
        assert_eq!(h.go_forward(at("a.md", 3)), Some(at("b.md", 12)));
        assert_eq!(h.back(), [at("a.md", 3)]);
        // A new place forgets the way forward.
        h.push(at("b.md", 12));
        assert!(!h.can_go_forward());
        assert_eq!(h.back().len(), 2);
    }

    #[test]
    fn the_back_stack_is_bounded() {
        let mut h = History::default();
        for i in 0..HISTORY_LIMIT + 5 {
            h.push(at("a.md", i));
        }
        assert_eq!(h.back().len(), HISTORY_LIMIT);
        assert_eq!(h.back()[0], at("a.md", 5));
    }

    #[test]
    fn closed_tabs_are_bounded() {
        let mut closed = ClosedTabs::default();
        for i in 0..CLOSED_TABS_LIMIT + 1 {
            closed.push(ClosedTab {
                index: i,
                place: at("a.md", i),
                history: History::default(),
            });
        }
        assert_eq!(closed.len(), CLOSED_TABS_LIMIT);
        assert_eq!(closed.pop().map(|t| t.index), Some(CLOSED_TABS_LIMIT));
    }
}

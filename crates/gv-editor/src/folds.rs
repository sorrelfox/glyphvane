// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Foldable regions of a Markdown document, from its syntax tree.
//!
//! [`foldable_ranges`] walks the block structure once and returns a
//! [`FoldCandidate`] per region the editor can fold:
//!
//! | Region | Kind | Folds |
//! |---|---|---|
//! | A heading outside any container | [`FoldKind::Heading`] | its section: up to the next heading of the same or a higher level (also outside containers) or the end of the text |
//! | A list item with more than one block (nested lists, more paragraphs, code) | [`FoldKind::ListItem`] | the item after its first line |
//! | A fenced or indented code block of two lines or more | [`FoldKind::FencedCode`], [`FoldKind::IndentedCode`] | the block after its first line, the closing fence included |
//! | A block quote of two lines or more | [`FoldKind::BlockQuote`] | the quote after its first line |
//! | Front matter | [`FoldKind::FrontMatter`] | the block after its opening delimiter |
//! | An HTML block of two lines or more | [`FoldKind::Html`] | the block after its first line |
//!
//! Every range runs from the end of the region's first line (for a setext
//! heading, its underline) to the end of its last line that is not blank,
//! so a folded region shows as its first line with the placeholder at the
//! end, and blank lines before the next block stay visible. Ranges never
//! include a line terminator at either end.

use std::ops::Range;

use gv_syntax::{Container, ContainerKind, Leaf, LeafKind, SyntaxTree, Visitor};
use gv_text::TextSnapshot;

/// What a foldable region is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FoldKind {
    /// A heading's section.
    Heading {
        /// 1 to 6.
        level: u8,
    },
    /// A list item with nested content.
    ListItem,
    /// A fenced code block.
    FencedCode,
    /// An indented code block.
    IndentedCode,
    /// A block quote.
    BlockQuote,
    /// Front matter.
    FrontMatter,
    /// An HTML block.
    Html,
}

/// A region the editor can fold.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FoldCandidate {
    /// The line the region starts on: where its fold marker goes.
    pub line: usize,
    /// The bytes to fold (see the [module documentation](self)).
    pub range: Range<usize>,
    /// What the region is.
    pub kind: FoldKind,
}

/// The foldable regions of `text`, whose parse `tree` is, sorted by line
/// (the larger region first on one line).
///
/// O(document): one walk of the tree plus a look at the lines that end
/// regions.
pub fn foldable_ranges(tree: &SyntaxTree, text: &TextSnapshot) -> Vec<FoldCandidate> {
    let mut collector = Collector {
        text,
        candidates: Vec::new(),
        headings: Vec::new(),
        containers: Vec::new(),
    };
    tree.walk(&mut collector);
    let Collector {
        mut candidates,
        headings,
        ..
    } = collector;

    for (index, heading) in headings.iter().enumerate() {
        let section_end = headings[index + 1..]
            .iter()
            .find(|next| next.level <= heading.level)
            .map_or(text.len(), |next| next.start);
        let last = last_line_with_text(text, heading.end_line, section_end);
        if let Some(range) = range_between(text, heading.end_line, last) {
            candidates.push(FoldCandidate {
                line: heading.line,
                range,
                kind: FoldKind::Heading {
                    level: heading.level,
                },
            });
        }
    }

    candidates.sort_by(|a, b| {
        a.line
            .cmp(&b.line)
            .then_with(|| b.range.end.cmp(&a.range.end))
            .then_with(|| a.range.start.cmp(&b.range.start))
    });
    candidates.dedup_by(|a, b| a.range == b.range);
    candidates
}

/// A heading outside containers.
struct Heading {
    level: u8,
    /// Its first line.
    line: usize,
    /// Its last line (the underline of a setext heading).
    end_line: usize,
    /// Byte offset of its first line's start.
    start: usize,
}

/// An open container, counting its direct child blocks.
struct Open {
    kind: ContainerKind,
    children: usize,
}

struct Collector<'a> {
    text: &'a TextSnapshot,
    candidates: Vec<FoldCandidate>,
    headings: Vec<Heading>,
    containers: Vec<Open>,
}

impl Collector<'_> {
    fn count_child(&mut self) {
        if let Some(parent) = self.containers.last_mut() {
            parent.children += 1;
        }
    }

    /// A candidate for a block on lines `first..` that ends before byte
    /// `end`.
    fn push_block(&mut self, kind: FoldKind, first: usize, end: usize) {
        let last = last_line_with_text(self.text, first, end);
        if let Some(range) = range_between(self.text, first, last) {
            self.candidates.push(FoldCandidate {
                line: first,
                range,
                kind,
            });
        }
    }
}

impl Visitor for Collector<'_> {
    fn enter(&mut self, container: &Container) {
        self.count_child();
        self.containers.push(Open {
            kind: container.kind,
            children: 0,
        });
    }

    fn exit(&mut self, container: &Container) {
        let Some(open) = self.containers.pop() else {
            return;
        };
        let kind = match open.kind {
            ContainerKind::BlockQuote => FoldKind::BlockQuote,
            ContainerKind::Item if open.children > 1 => FoldKind::ListItem,
            _ => return,
        };
        self.push_block(kind, container.line, container.range.end);
    }

    fn leaf(&mut self, leaf: &Leaf<'_>) {
        self.count_child();
        let kind = match leaf.kind {
            LeafKind::Heading { level, .. } => {
                if self.containers.is_empty() {
                    let start = self
                        .text
                        .line_start(leaf.start_line)
                        .unwrap_or(leaf.range.start);
                    self.headings.push(Heading {
                        level,
                        line: leaf.start_line,
                        end_line: leaf.end_line,
                        start,
                    });
                }
                return;
            }
            LeafKind::FencedCode => FoldKind::FencedCode,
            LeafKind::IndentedCode => FoldKind::IndentedCode,
            LeafKind::Html { .. } => FoldKind::Html,
            LeafKind::FrontMatter { .. } => FoldKind::FrontMatter,
            LeafKind::Paragraph
            | LeafKind::ThematicBreak
            | LeafKind::LinkRefDef
            | LeafKind::Table => return,
        };
        self.push_block(kind, leaf.start_line, leaf.range.end);
    }
}

/// The last line from `first` on that starts before `end` and is not
/// blank (`first` itself if there is none).
fn last_line_with_text(text: &TextSnapshot, first: usize, end: usize) -> usize {
    let end = end.min(text.len());
    let Ok(mut line) = text.line_of_offset(end) else {
        return first;
    };
    // A line that starts at `end` belongs to what follows.
    if line > first && text.line_start(line).is_ok_and(|start| start >= end) {
        line -= 1;
    }
    while line > first && is_blank(text, line) {
        line -= 1;
    }
    line.max(first)
}

fn is_blank(text: &TextSnapshot, line: usize) -> bool {
    text.line_range(line)
        .ok()
        .and_then(|range| text.text_for_range(range).ok())
        .is_none_or(|content| content.chars().all(|c| c == ' ' || c == '\t'))
}

/// From the end of line `first` to the end of line `last`, if `last` is
/// after `first`.
fn range_between(text: &TextSnapshot, first: usize, last: usize) -> Option<Range<usize>> {
    if last <= first {
        return None;
    }
    let start = text.line_range(first).ok()?.end;
    let end = text.line_range(last).ok()?.end;
    (start < end).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(text: &str) -> Vec<(usize, &str, FoldKind)> {
        let snapshot = TextSnapshot::new(text);
        let tree = gv_syntax::parse(&snapshot);
        foldable_ranges(&tree, &snapshot)
            .into_iter()
            .map(|candidate| (candidate.line, &text[candidate.range], candidate.kind))
            .collect()
    }

    #[test]
    fn a_section_ends_before_trailing_blank_lines() {
        assert_eq!(
            candidates("# A\ntext\n\n\n# B\n"),
            [(0, "\ntext", FoldKind::Heading { level: 1 })]
        );
    }

    #[test]
    fn single_line_blocks_do_not_fold() {
        for text in [
            "# A\n# B\n",
            "> one\n",
            "    code\n",
            "- item\n- two\n",
            "<div>\n",
        ] {
            assert!(candidates(text).is_empty(), "{text:?}");
        }
    }
}

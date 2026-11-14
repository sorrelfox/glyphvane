// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! How the wrap layer indents continuation rows: a [`WrapIndent`] callback,
//! asked once per input row when the row is wrapped.
//!
//! The answer is an [`IndentAdjustment`], applied by the line wrapper:
//!
//! - [`IndentAdjustment::SameIndent`] (the default, through
//!   `IndentAdjustment` itself): continuation rows line up with the row's
//!   leading whitespace;
//! - [`IndentAdjustment::ExtraColumns`]: that plus some columns, which is a
//!   **hanging indent** ([`MarkdownHangingIndent`] gives list items and
//!   quotes two extra columns, so their wrapped text lines up after `- `
//!   or `> `);
//! - [`IndentAdjustment::Columns`]: an absolute column, for a callback that
//!   knows the item's content offset from the syntax tree;
//! - [`IndentAdjustment::NoIndent`].
//!
//! The wrapper drops the indent when it would leave less than two columns
//! (or the rest of the word being carried over) on the row.
//!
//! Rows are asked about only when they are (re)wrapped: after an edit, the
//! rows the edit touched. A callback whose answer for a row depends on other
//! rows (a lazy continuation line of a list item, say) must ask for the rows
//! it changed with [`WrapMap::refresh_rows`](super::WrapMap::refresh_rows).

use super::line_wrapper::IndentAdjustment;
use super::tab_map::TabSnapshot;

/// Chooses the indent of the continuation rows of one row.
///
/// Shared between threads (a re-wrap can run in the background), hence
/// `Send + Sync`.
pub trait WrapIndent: Send + Sync {
    /// The indent for the continuation rows of tab row `row`, whose text
    /// (tabs expanded, without the line break) is `line`.
    fn indent(&self, tabs: &TabSnapshot, row: u32, line: &str) -> IndentAdjustment;
}

/// The same adjustment for every row.
impl WrapIndent for IndentAdjustment {
    fn indent(&self, _tabs: &TabSnapshot, _row: u32, _line: &str) -> IndentAdjustment {
        *self
    }
}

/// A hanging indent for Markdown list items and block quotes, found from
/// the row text alone.
///
/// A row whose first non-blank characters are a bullet (`-`, `+`, `*`) or
/// an ordered-list marker (1–9 digits and `.` or `)`) followed by a space,
/// or a `>`, gets [`IndentAdjustment::ExtraColumns`]`(columns)`: its
/// continuation rows start `columns` past its leading whitespace. Other rows
/// keep their leading whitespace ([`IndentAdjustment::SameIndent`]).
///
/// This is a heuristic over the text: it also matches such lines inside
/// fenced code. A callback that reads the syntax tree can do better.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MarkdownHangingIndent {
    /// The extra columns for list and quote continuations.
    pub columns: u32,
}

impl Default for MarkdownHangingIndent {
    /// Two columns, the editor's hanging indent.
    fn default() -> Self {
        Self { columns: 2 }
    }
}

impl MarkdownHangingIndent {
    /// Whether `line` starts (after spaces) with a list marker or a quote.
    pub fn is_list_or_quote(line: &str) -> bool {
        let rest = line.trim_start_matches(' ');
        let bytes = rest.as_bytes();
        match bytes.first() {
            Some(b'>') => true,
            Some(b'-' | b'+' | b'*') => bytes.get(1) == Some(&b' '),
            Some(b'0'..=b'9') => {
                let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
                digits <= 9
                    && matches!(bytes.get(digits), Some(b'.' | b')'))
                    && bytes.get(digits + 1) == Some(&b' ')
            }
            _ => false,
        }
    }
}

impl WrapIndent for MarkdownHangingIndent {
    fn indent(&self, _tabs: &TabSnapshot, _row: u32, line: &str) -> IndentAdjustment {
        if Self::is_list_or_quote(line) {
            IndentAdjustment::ExtraColumns(self.columns)
        } else {
            IndentAdjustment::SameIndent
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_and_quote_detection() {
        for line in [
            "- item",
            "  * nested",
            "+ plus",
            "1. one",
            "123456789) many digits",
            "> quote",
            ">no space",
            "   > indented quote",
            "- [ ] task",
        ] {
            assert!(MarkdownHangingIndent::is_list_or_quote(line), "{line:?}");
        }
        for line in [
            "",
            "plain",
            "-dash",
            "---",
            "**bold**",
            "1.5 numbers",
            "1234567890. too many digits",
            "2.",
            "#heading",
        ] {
            assert!(!MarkdownHangingIndent::is_list_or_quote(line), "{line:?}");
        }
    }
}

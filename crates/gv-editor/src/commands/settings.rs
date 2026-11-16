// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Settings of the Markdown commands.

/// What Tab inserts outside lists and block quotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IndentUnit {
    /// A tab character.
    Tab,
    /// This many spaces (1 to 16).
    Spaces(u8),
}

/// The delimiter character of emphasis the commands write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmphasisStyle {
    /// `*italic*` and `**bold**`.
    Asterisk,
    /// `_italic_` and `__bold__` (inside a word, where underscores do not
    /// work, asterisks are written instead).
    Underscore,
}

/// How a new or renumbered ordered list is numbered when the list itself
/// does not show a style (a list of two or more items keeps its own: all
/// numbers equal, or sequential).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Numbering {
    /// `1.` `2.` `3.` from the start number.
    Sequential,
    /// Every item has the start number (`1.` `1.` `1.`).
    Ones,
}

/// The link syntax "Insert link" writes without a URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkStyle {
    /// `[text]()`.
    Markdown,
    /// `[[text]]`, in flavors that have wikilinks (others write Markdown
    /// links).
    Wikilink,
}

/// Where "Insert footnote" puts the definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FootnotePlacement {
    /// At the end of the document.
    EndOfDocument,
    /// After the top-level block (paragraph, list, quote) the caret is in.
    AfterParagraph,
}

/// Settings of the Markdown commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandSettings {
    /// What Tab inserts outside lists and quotes.
    pub indent: IndentUnit,
    /// The emphasis delimiter.
    pub emphasis: EmphasisStyle,
    /// The numbering of ordered lists without a style of their own.
    pub numbering: Numbering,
    /// The link syntax of "Insert link".
    pub link_style: LinkStyle,
    /// Whether typed brackets, quotes and delimiters pair, wrap the
    /// selection and are typed over
    /// ([`MarkdownCommands::type_text`](super::MarkdownCommands::type_text)).
    pub smart_pairs: bool,
    /// Where "Insert footnote" puts the definition.
    pub footnotes: FootnotePlacement,
}

impl Default for CommandSettings {
    /// A tab, asterisks, sequential numbering, Markdown links, smart
    /// pairs on, footnote definitions at the end of the document.
    fn default() -> Self {
        Self {
            indent: IndentUnit::Tab,
            emphasis: EmphasisStyle::Asterisk,
            numbering: Numbering::Sequential,
            link_style: LinkStyle::Markdown,
            smart_pairs: true,
            footnotes: FootnotePlacement::EndOfDocument,
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The intermediate document between the HTML tree and Markdown text.
//!
//! Inline content is a flat list of [`Run`]s, each carrying its full
//! [`Style`]. Formatting is inherited down the HTML tree rather than kept
//! as structure, so a block inside a formatting element (Google Docs wraps
//! a whole document in `<b style="font-weight:normal">`) and a style
//! attribute that switches formatting off both fall out naturally. The
//! emitter rebuilds properly nested delimiters from the flat runs.

/// Inline formatting of a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Style {
    pub strong: bool,
    pub em: bool,
    pub strike: bool,
    pub sub: bool,
    pub sup: bool,
    pub mark: bool,
    /// Monospace: rendered as a code span.
    pub code: bool,
    /// Index into the converter's link table.
    pub link: Option<u32>,
}

/// One piece of inline content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Inline {
    /// Text with white space already collapsed.
    Text(String),
    /// An image with a sanitized source.
    Image {
        src: String,
        alt: String,
        title: Option<String>,
    },
    /// A hard line break.
    Break,
    /// TeX source of inline math.
    Math(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Run {
    pub item: Inline,
    pub style: Style,
}

/// A sanitized link destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub dest: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cell {
    pub runs: Vec<Run>,
    pub align: Align,
    pub header: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    /// `Some(checked)` for a task list item.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct List {
    pub ordered: bool,
    pub start: u64,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    Para(Vec<Run>),
    Heading(u8, Vec<Run>),
    Code {
        lang: Option<String>,
        text: String,
    },
    Quote(Vec<Block>),
    /// A GitHub alert / callout (`note`, `tip`, `important`, `warning`,
    /// `caution`).
    Alert {
        kind: &'static str,
        blocks: Vec<Block>,
    },
    List(List),
    Rule,
    Table(Vec<Vec<Cell>>),
    /// A Word list paragraph, grouped into lists once its siblings are
    /// known.
    MsoItem {
        level: u32,
        /// Which Word list the paragraph belongs to.
        key: String,
        ordered: bool,
        start: Option<u64>,
        blocks: Vec<Block>,
    },
}

impl Run {
    pub(crate) fn is_visible(&self) -> bool {
        match &self.item {
            Inline::Text(t) => !t.trim().is_empty(),
            Inline::Image { .. } | Inline::Math(_) => true,
            Inline::Break => false,
        }
    }
}

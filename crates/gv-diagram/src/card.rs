// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Fallback-card data (ADR-017): what the Reading view shows instead of a
//! diagram. The highlighted source is always available below the card.

use std::sync::Arc;

/// Which card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardKind {
    /// The source does not parse: merman's message, and the position when
    /// known.
    Error,
    /// Over a cap or the deadline. Offers *Render anyway* (when not already
    /// trusted) and *Copy source*.
    TooLarge,
    /// The renderer failed (a caught panic or an internal error). Not
    /// retried until the source changes.
    RendererError,
}

impl CardKind {
    /// The card's title (English-only UI in 1.0).
    pub fn title(self) -> &'static str {
        match self {
            CardKind::Error => "Diagram error",
            CardKind::TooLarge => "Diagram too large to render",
            CardKind::RendererError => "Renderer error",
        }
    }
}

/// A fallback card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackCard {
    pub kind: CardKind,
    /// One line: the parse diagnostic, the limit that was hit, or the
    /// failure.
    pub message: String,
    /// Byte range of the problem in the source, when known.
    pub span: Option<(usize, usize)>,
    /// 1-based line and column of `span`'s start, when known.
    pub line_col: Option<(usize, usize)>,
    /// Whether *Render anyway* is offered (too large at the interactive
    /// caps).
    pub render_anyway: bool,
    /// The diagram source, for *Copy source* and the source view.
    pub source: Arc<str>,
}

impl FallbackCard {
    pub(crate) fn new(kind: CardKind, message: String, source: &str) -> Self {
        Self {
            kind,
            message,
            span: None,
            line_col: None,
            render_anyway: false,
            source: source.into(),
        }
    }

    pub(crate) fn with_span(mut self, start: usize, end: usize) -> Self {
        let start = start.min(self.source.len());
        let end = end.clamp(start, self.source.len());
        let before = self.source.get(..start).unwrap_or("");
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        self.span = Some((start, end));
        self.line_col = Some((line, col));
        self
    }

    pub(crate) fn with_render_anyway(mut self, on: bool) -> Self {
        self.render_anyway = on;
        self
    }

    /// The card's title.
    pub fn title(&self) -> &'static str {
        self.kind.title()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_gives_line_and_column() {
        let card = FallbackCard::new(CardKind::Error, "x".into(), "graph TD\n  A --> \n  é!")
            .with_span(20, 21);
        assert_eq!(card.line_col, Some((3, 3)));
        assert_eq!(card.title(), "Diagram error");
    }
}

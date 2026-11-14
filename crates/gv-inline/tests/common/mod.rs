// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Shared fixture: paragraphs laid out with `FakeMono`. At size 10 a Latin
//! grapheme is 6 px, CJK and emoji 10 px; the strut is ascent 8, descent 2
//! with a 16 px line height, so a plain line has 11 px above the baseline
//! and 5 below.

// Each test binary uses a different part of the fixture.
#![allow(dead_code)]

use std::sync::Arc;

use gv_inline::{
    Background, BoxKind, ColorRole, FakeMono, InlineBox, Layout, LineHeight, OBJECT, Paragraph,
    ParagraphBuilder, ParagraphStyle, RunCache, RunStyle, Shaped, StyleId,
};

pub const SIZE: f32 = 10.0;

pub fn body() -> RunStyle {
    RunStyle::new(StyleId(0), SIZE)
}

pub fn bold() -> RunStyle {
    RunStyle::new(StyleId(1), SIZE)
}

pub fn code() -> RunStyle {
    RunStyle::new(StyleId(2), SIZE).with_background(Background {
        role: ColorRole(7),
        padding_x: 5.0,
        padding_y: 1.0,
        radius: 4.0,
    })
}

pub fn paragraph_style() -> ParagraphStyle {
    ParagraphStyle::new(StyleId(0), SIZE, LineHeight::Px(16.0))
}

/// One item to build.
#[derive(Debug, Clone)]
pub enum Piece {
    Text(String, RunStyle),
    Box(InlineBox),
    Break,
}

pub fn text(s: impl Into<String>, style: RunStyle) -> Piece {
    Piece::Text(s.into(), style)
}

pub fn math(width: f32, ascent: f32, descent: f32) -> Piece {
    Piece::Box(InlineBox::new(BoxKind::Math, width, ascent, descent))
}

/// Builds a paragraph whose source equals its logical text byte for byte.
pub fn build(pieces: &[Piece], style: ParagraphStyle) -> Paragraph {
    let mut b = ParagraphBuilder::new(style);
    let mut at = 0;
    for p in pieces {
        match p {
            Piece::Text(t, s) => {
                b.text(t, *s, at..at + t.len());
                at += t.len();
            }
            Piece::Box(bx) => {
                b.inline_box(*bx, at..at + 3);
                at += 3;
            }
            Piece::Break => {
                b.hard_break(at..at + 1);
                at += 1;
            }
        }
    }
    b.build()
}

pub struct Fixture {
    pub paragraph: Arc<Paragraph>,
    pub shaped: Arc<Shaped>,
    pub measure: FakeMono,
    pub runs: RunCache,
}

impl Fixture {
    pub fn new(pieces: Vec<Piece>) -> Self {
        Self::with_style(pieces, paragraph_style())
    }

    pub fn with_style(pieces: Vec<Piece>, style: ParagraphStyle) -> Self {
        Self::from_paragraph(build(&pieces, style), FakeMono::default())
    }

    pub fn from_paragraph(paragraph: Paragraph, mut measure: FakeMono) -> Self {
        let paragraph = Arc::new(paragraph);
        let mut runs = RunCache::default();
        let shaped = Arc::new(Shaped::new(paragraph.clone(), &mut measure, &mut runs));
        Self {
            paragraph,
            shaped,
            measure,
            runs,
        }
    }

    pub fn layout(&self, width: f32) -> Layout {
        Layout::new(self.shaped.clone(), width)
    }

    /// The lines as logical text, trailing white space trimmed, boxes as `☐`.
    pub fn lines(&self, width: f32) -> Vec<String> {
        self.layout(width)
            .lines()
            .iter()
            .map(|l| {
                self.paragraph.text()[l.range.clone()]
                    .trim_end()
                    .replace(OBJECT, "☐")
            })
            .collect()
    }
}

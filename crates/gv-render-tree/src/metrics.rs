// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Block metrics of the preview: vertical spacing, indentation, code block
//! chrome, and the average character advances that height estimates use.

/// Vertical margins of a block, collapsed with its neighbours' as CSS
/// collapses block margins (the larger one wins).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Spacing {
    pub top: f32,
    pub bottom: f32,
}

impl Spacing {
    pub const fn new(top: f32, bottom: f32) -> Self {
        Self { top, bottom }
    }

    const fn scaled(self, f: f32) -> Self {
        Self {
            top: self.top * f,
            bottom: self.bottom * f,
        }
    }
}

/// Lengths in px that place blocks, from the design where it gives them.
///
/// Values the design does not give (marked "not designed" below) are
/// derived from the nearest designed component.
///
/// # The text column
///
/// The design's 680 px column is the width of the **text** (a CSS
/// content box): the column's side padding (40 px) lies outside it. Block
/// content is laid out in that width less its containers' indents.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Average advance of the text family, in em (Inter: about 0.53).
    pub text_advance_em: f32,
    /// Average advance of the mono family, in em (JetBrains Mono: 0.6).
    pub mono_advance_em: f32,
    /// Paragraphs: 0 above, 12 below.
    pub paragraph: Spacing,
    /// Headings by level: H1 0/6 and H2 4/8 (designed); H3 to H6 like H2
    /// (not designed).
    pub headings: [Spacing; 6],
    /// Code and HTML blocks, and tables: 0/12 (not designed; as
    /// paragraphs).
    pub code: Spacing,
    /// Thematic breaks: 12/12 around a 1 px rule (not designed).
    pub rule: Spacing,
    /// Thickness of a thematic break.
    pub rule_height: f32,
    /// Lists: 0/12; lists nested in a list item: 0/0.
    pub list: Spacing,
    /// Block quotes: 0/12 (not designed).
    pub quote: Spacing,
    /// Front matter properties: 0/12.
    pub properties: Spacing,
    /// The key column of front matter properties (68) and the gap after
    /// it (8).
    pub property_key: f32,
    pub property_gap: f32,
    /// Callouts and alerts: 0/12 outside (as paragraphs).
    pub callout: Spacing,
    /// A callout's padding: 9 above, 10 below, 12 at the sides.
    pub callout_padding_top: f32,
    pub callout_padding_bottom: f32,
    pub callout_padding_x: f32,
    /// A callout's title row: 14/20.
    pub callout_title: f32,
    /// Space between the title row and the first block: 3.
    pub callout_title_gap: f32,
    /// Indentation of a list item's content; the marker sits in it (not
    /// designed: 24).
    pub list_indent: f32,
    /// Indentation of a block quote's content (not designed: 16).
    pub quote_indent: f32,
    /// Width of a block quote's bar (not designed: 3, in `border.strong`).
    pub quote_bar: f32,
    /// Height of a code block's head (language, copy and wrap buttons).
    pub code_head: f32,
    /// Code block padding: 8 12.
    pub code_padding_y: f32,
    pub code_padding_x: f32,
    /// Code block border: 1.
    pub code_border: f32,
    /// Table cells' horizontal padding: 10.
    pub table_padding_x: f32,
    /// Header cells' vertical padding: 4.
    pub table_header_padding_y: f32,
    /// Body cells' vertical padding: 5.
    pub table_padding_y: f32,
    /// A table's frame and the lines between its rows: 1, in
    /// `border.default`, with a radius of 6.
    pub table_border: f32,
    /// Widest an image is laid out: the text column, 680. Wider images
    /// are scaled down to it, keeping their aspect ratio.
    pub image_max_width: f32,
    /// Whether code blocks soft-wrap rather than scroll horizontally, for
    /// blocks without their own setting
    /// ([`RenderTree::set_code_wrap`](crate::RenderTree::set_code_wrap)).
    pub wrap_code: bool,
}

impl Default for Metrics {
    fn default() -> Self {
        let heading = Spacing::new(4.0, 8.0);
        Self {
            text_advance_em: 0.53,
            mono_advance_em: 0.6,
            paragraph: Spacing::new(0.0, 12.0),
            headings: [
                Spacing::new(0.0, 6.0),
                heading,
                heading,
                heading,
                heading,
                heading,
            ],
            code: Spacing::new(0.0, 12.0),
            rule: Spacing::new(12.0, 12.0),
            rule_height: 1.0,
            list: Spacing::new(0.0, 12.0),
            quote: Spacing::new(0.0, 12.0),
            properties: Spacing::new(0.0, 12.0),
            property_key: 68.0,
            property_gap: 8.0,
            callout: Spacing::new(0.0, 12.0),
            callout_padding_top: 9.0,
            callout_padding_bottom: 10.0,
            callout_padding_x: 12.0,
            callout_title: 20.0,
            callout_title_gap: 3.0,
            list_indent: 24.0,
            quote_indent: 16.0,
            quote_bar: 3.0,
            code_head: 26.0,
            code_padding_y: 8.0,
            code_padding_x: 12.0,
            code_border: 1.0,
            table_padding_x: 10.0,
            table_header_padding_y: 4.0,
            table_padding_y: 5.0,
            table_border: 1.0,
            image_max_width: 680.0,
            wrap_code: false,
        }
    }
}

impl Metrics {
    /// Every length multiplied by `zoom` (advances are in em and stay).
    #[must_use]
    pub fn zoomed(self, zoom: f32) -> Self {
        let f = if zoom.is_finite() && zoom > 0.0 {
            zoom
        } else {
            1.0
        };
        Self {
            paragraph: self.paragraph.scaled(f),
            headings: self.headings.map(|s| s.scaled(f)),
            code: self.code.scaled(f),
            rule: self.rule.scaled(f),
            rule_height: self.rule_height * f,
            list: self.list.scaled(f),
            quote: self.quote.scaled(f),
            properties: self.properties.scaled(f),
            property_key: self.property_key * f,
            property_gap: self.property_gap * f,
            callout: self.callout.scaled(f),
            callout_padding_top: self.callout_padding_top * f,
            callout_padding_bottom: self.callout_padding_bottom * f,
            callout_padding_x: self.callout_padding_x * f,
            callout_title: self.callout_title * f,
            callout_title_gap: self.callout_title_gap * f,
            list_indent: self.list_indent * f,
            quote_indent: self.quote_indent * f,
            quote_bar: self.quote_bar * f,
            code_head: self.code_head * f,
            code_padding_y: self.code_padding_y * f,
            code_padding_x: self.code_padding_x * f,
            code_border: self.code_border * f,
            table_padding_x: self.table_padding_x * f,
            table_header_padding_y: self.table_header_padding_y * f,
            table_padding_y: self.table_padding_y * f,
            table_border: self.table_border * f,
            image_max_width: self.image_max_width * f,
            ..self
        }
    }

    /// The spacing of a heading level (clamped to 1..=6).
    pub fn heading(&self, level: u8) -> Spacing {
        self.headings[usize::from(level.clamp(1, 6) - 1)]
    }
}

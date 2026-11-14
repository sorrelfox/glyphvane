// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Styles of inline content: style keys, colour roles, decorations, atomic
//! boxes and paragraph settings. Plain data; nothing here names a font or a
//! colour, so the crate stays GUI-free.

/// Everything about a text run that changes its shaping, except the font
/// size: family, weight, slant, features.
///
/// Opaque to this crate. The caller's [`Measure`](crate::Measure)
/// implementation maps it to a font. Colour and decorations are kept out of
/// it on purpose, so a link and plain text in the same font share one shaped
/// run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct StyleId(pub u32);

/// A colour role (a theme token) for text, decorations and backgrounds.
/// Opaque; the painter maps it to a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ColorRole(pub u32);

/// The line style of an underline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnderlineStyle {
    Solid,
    Dotted,
    Dashed,
    Wavy,
}

/// An underline drawn by the painter, not by the text system.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Underline {
    pub style: UnderlineStyle,
    pub color: ColorRole,
    /// Distance from the run's baseline down to the top of the line.
    pub offset: f32,
    pub thickness: f32,
}

impl Underline {
    pub const fn new(style: UnderlineStyle, color: ColorRole, offset: f32, thickness: f32) -> Self {
        Self {
            style,
            color,
            offset,
            thickness,
        }
    }
}

/// A line through the middle of the x-height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Strikethrough {
    pub color: ColorRole,
    pub thickness: f32,
}

/// A filled box behind a run: inline code, a keycap, a highlight.
///
/// Horizontal padding takes space in the line at the run's outer ends only
/// (a run split across lines is padded on its first and last piece);
/// vertical padding does not, as for CSS inline boxes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Background {
    /// What the painter fills (and possibly borders) the box with.
    pub role: ColorRole,
    pub padding_x: f32,
    pub padding_y: f32,
    pub radius: f32,
}

/// The style of one text run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunStyle {
    /// Shaping identity (family, weight, slant), mapped to a font by the
    /// measurer.
    pub font: StyleId,
    /// Font size in px.
    pub size: f32,
    pub color: ColorRole,
    /// Extra space in px after every grapheme cluster (CSS
    /// `letter-spacing`); may be negative. Applied by the layout, so it is
    /// not part of the shaped-run cache key.
    pub letter_spacing: f32,
    /// Baseline shift in px: positive raises (superscript), negative lowers
    /// (subscript).
    pub baseline_shift: f32,
    pub underline: Option<Underline>,
    pub strikethrough: Option<Strikethrough>,
    pub background: Option<Background>,
}

impl RunStyle {
    /// A plain run: colour role 0, no spacing, shift or decorations.
    pub const fn new(font: StyleId, size: f32) -> Self {
        Self {
            font,
            size,
            color: ColorRole(0),
            letter_spacing: 0.0,
            baseline_shift: 0.0,
            underline: None,
            strikethrough: None,
            background: None,
        }
    }

    pub const fn with_color(mut self, color: ColorRole) -> Self {
        self.color = color;
        self
    }

    pub const fn with_letter_spacing(mut self, px: f32) -> Self {
        self.letter_spacing = px;
        self
    }

    pub const fn with_baseline_shift(mut self, px: f32) -> Self {
        self.baseline_shift = px;
        self
    }

    pub const fn with_underline(mut self, underline: Underline) -> Self {
        self.underline = Some(underline);
        self
    }

    pub const fn with_strikethrough(mut self, strikethrough: Strikethrough) -> Self {
        self.strikethrough = Some(strikethrough);
        self
    }

    pub const fn with_background(mut self, background: Background) -> Self {
        self.background = Some(background);
        self
    }

    /// Horizontal padding at each outer end of the run.
    pub fn padding_x(&self) -> f32 {
        self.background.map_or(0.0, |b| b.padding_x)
    }

    /// Finite lengths; size, paddings and thicknesses not negative.
    pub(crate) fn sanitized(mut self) -> Self {
        self.size = non_negative(self.size);
        self.letter_spacing = finite(self.letter_spacing);
        self.baseline_shift = finite(self.baseline_shift);
        if let Some(u) = &mut self.underline {
            u.offset = finite(u.offset);
            u.thickness = non_negative(u.thickness);
        }
        if let Some(st) = &mut self.strikethrough {
            st.thickness = non_negative(st.thickness);
        }
        if let Some(b) = &mut self.background {
            b.padding_x = non_negative(b.padding_x);
            b.padding_y = non_negative(b.padding_y);
            b.radius = non_negative(b.radius);
        }
        self
    }
}

/// What an atomic inline box shows. The layout only needs its metrics; the
/// painter uses the kind and [`InlineBox::id`] to find the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoxKind {
    /// Inline math.
    Math,
    /// An inline image (also emoji drawn as images).
    Image,
    /// A task-list checkbox.
    Checkbox { checked: bool },
    /// Anything else: a tag pill, a footnote reference, an inline embed.
    Other,
}

/// How an atomic box sits on the line, as CSS `vertical-align`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BoxAlign {
    /// The box's baseline (`ascent` below its top) on the line's baseline.
    #[default]
    Baseline,
    /// The box's middle at half the paragraph font's x-height above the
    /// baseline.
    Middle,
    /// The box's top at the paragraph font's ascent.
    TextTop,
    /// The box's bottom at the paragraph font's descent.
    TextBottom,
}

/// An atomic inline: it never breaks inside, and its baseline sits on the
/// line's baseline (for [`BoxAlign::Baseline`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InlineBox {
    pub kind: BoxKind,
    /// The caller's handle for the content (an image or a math layout).
    pub id: u32,
    pub width: f32,
    /// Height above the box's baseline.
    pub ascent: f32,
    /// Depth below the box's baseline.
    pub descent: f32,
    pub margin_left: f32,
    pub margin_right: f32,
    pub align: BoxAlign,
}

impl InlineBox {
    /// A box on the baseline, without margins, with id 0.
    pub const fn new(kind: BoxKind, width: f32, ascent: f32, descent: f32) -> Self {
        Self {
            kind,
            id: 0,
            width,
            ascent,
            descent,
            margin_left: 0.0,
            margin_right: 0.0,
            align: BoxAlign::Baseline,
        }
    }

    pub const fn with_id(mut self, id: u32) -> Self {
        self.id = id;
        self
    }

    pub const fn with_margins(mut self, left: f32, right: f32) -> Self {
        self.margin_left = left;
        self.margin_right = right;
        self
    }

    pub const fn with_align(mut self, align: BoxAlign) -> Self {
        self.align = align;
        self
    }

    /// Height of the box (ascent plus descent).
    pub fn height(&self) -> f32 {
        self.ascent + self.descent
    }

    /// Horizontal space the box takes in the line, margins included.
    pub fn advance(&self) -> f32 {
        self.margin_left + self.width + self.margin_right
    }

    /// Finite lengths; width and margins not negative. Ascent and descent
    /// may be negative (a box entirely below or above the baseline).
    pub(crate) fn sanitized(mut self) -> Self {
        self.width = non_negative(self.width);
        self.margin_left = non_negative(self.margin_left);
        self.margin_right = non_negative(self.margin_right);
        self.ascent = finite(self.ascent);
        self.descent = finite(self.descent);
        self
    }
}

/// Line height of a paragraph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// A fixed line height in px (CSS `line-height: 24px`).
    Px(f32),
    /// The primary font's ascent + descent + line gap (CSS `normal`).
    Normal,
}

/// Horizontal alignment of the lines of a paragraph. Hanging white space at
/// a line end does not take part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Paragraph-level settings: the strut (primary font and line height) and
/// the alignment.
///
/// Every line is at least as tall as the strut. Text runs add their primary
/// font's content area (ascent and descent, never the fallback faces'), so
/// a run in a smaller font or a fallback emoji does not grow the line; tall
/// boxes and shifted runs do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParagraphStyle {
    /// The paragraph's primary font.
    pub font: StyleId,
    pub size: f32,
    pub line_height: LineHeight,
    pub align: Align,
}

impl ParagraphStyle {
    pub const fn new(font: StyleId, size: f32, line_height: LineHeight) -> Self {
        Self {
            font,
            size,
            line_height,
            align: Align::Left,
        }
    }

    pub const fn with_align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// A finite, non-negative size; a non-finite line height becomes
    /// `Normal`.
    pub(crate) fn sanitized(mut self) -> Self {
        self.size = non_negative(self.size);
        if let LineHeight::Px(px) = self.line_height {
            self.line_height = if px.is_finite() {
                LineHeight::Px(px.max(0.0))
            } else {
                LineHeight::Normal
            };
        }
        self
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

fn non_negative(v: f32) -> f32 {
    finite(v).max(0.0)
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Inline content on GPUI: the GPUI side of `gv-inline`.
//!
//! `gv-inline` lays a paragraph of styled runs and atomic boxes out behind
//! a measurer trait. This module supplies the measurer ([`GpuiMeasure`],
//! shaping with GPUI's text system) and paints the layouts
//! ([`InlineParagraph`]).
//!
//! # State
//!
//! - [`InlineText`]: one per view (or window). The [`FontTable`] that maps
//!   the paragraphs' style ids to fonts, and the byte-budgeted cache of
//!   shaped runs that all paragraphs share.
//! - [`InlineBlock`]: one per paragraph, kept by the view across frames. The
//!   paragraph, its shaping, its layouts at the last few widths (GPUI's
//!   layout engine measures a block at a max-content probe and then at its
//!   real width every frame), the content of its boxes, and where it was
//!   painted last, for hit testing.
//! - [`InlineParagraph`]: the element, made every frame from the two and the
//!   things it paints on top: a selection and a caret, both in source
//!   offsets.
//!
//! The selection model lives in the view, in source offsets. The view hit
//! tests with [`InlineBlock::hit`] (window position → source offset) and
//! passes the range to the elements, which paint what falls inside their
//! paragraph.
//!
//! # Painting rules
//!
//! - Order: run backgrounds (inline code), selection, then per line one
//!   `paint_layer` with the glyphs and the underlines and strikethroughs;
//!   then boxes, the translucent selection tint on selected boxes, and the
//!   caret. Without the layer every glyph goes into the scene's bounds tree
//!   and painting is about three times slower.
//! - Lines outside the content mask are not painted.
//! - Letter spacing and baseline shift come from the layout: glyphs move by
//!   [`gv_inline::Shaped::glyph_shift`] and sit on
//!   `line.baseline - fragment.shift`.
//! - Underlines are drawn here, not by GPUI: solid and wavy with GPUI's
//!   underline primitive, dashed (3 on, 2 off) and dotted (1 on, 1 off) as
//!   one quad per dash or dot. GPUI's dashed border draws dashes of about
//!   2 px at 1 px thickness, which cannot be told from dotted.
//! - Colours are theme tokens: a [`gv_inline::ColorRole`] is a [`Token`].

mod element;
mod fonts;
mod math;
mod palette;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Bounds, ImageSource, Pixels, Point, SharedString, WindowTextSystem, point, px, size};
use gv_inline::{
    Bias, BlockCache, BlockCacheStats, ColorRole, Hit, Layout, Paragraph, RunCache, RunCacheStats,
};

pub use element::{InlineParagraph, PaintStats};
pub use fonts::{FontFamilies, FontTable, Glyphs, GpuiMeasure, font_of};
pub use palette::{Palette, Token};

/// Fonts and shaped runs shared by the paragraphs of a view.
#[derive(Debug)]
pub struct InlineText {
    fonts: FontTable,
    runs: RunCache<Glyphs>,
    /// Bumped when the fonts change; blocks shaped under an older
    /// generation reshape.
    generation: u64,
}

impl InlineText {
    pub fn new(fonts: FontTable) -> Self {
        Self {
            fonts,
            runs: RunCache::default(),
            generation: 0,
        }
    }

    pub fn fonts(&self) -> &FontTable {
        &self.fonts
    }

    /// Replaces the fonts. Every block reshapes on its next layout.
    pub fn set_fonts(&mut self, fonts: FontTable) {
        self.fonts = fonts;
        self.runs.clear();
        self.generation += 1;
    }

    pub fn run_stats(&self) -> RunCacheStats {
        self.runs.stats()
    }

    /// Number of cached runs and the bytes they hold.
    pub fn run_cache_size(&self) -> (usize, usize) {
        (self.runs.len(), self.runs.bytes())
    }

    /// Drops the shaping of a block made with older fonts.
    fn sync(&self, block: &mut InlineBlock) {
        if block.generation != self.generation {
            block.cache.invalidate();
            block.generation = self.generation;
        }
    }

    /// The layout of `block` at `width` (`f32::INFINITY` for max-content):
    /// cached, re-broken from the cached shaping, or shaped first.
    pub fn layout(
        &mut self,
        block: &mut InlineBlock,
        width: f32,
        text_system: &WindowTextSystem,
    ) -> Arc<Layout<Glyphs>> {
        self.sync(block);
        let mut measure = GpuiMeasure::new(text_system, &self.fonts);
        block.cache.layout(width, &mut measure, &mut self.runs)
    }

    /// The narrowest width `block` can take without breaking inside a word
    /// (CSS min-content).
    pub fn min_content_width(
        &mut self,
        block: &mut InlineBlock,
        text_system: &WindowTextSystem,
    ) -> f32 {
        self.sync(block);
        let mut measure = GpuiMeasure::new(text_system, &self.fonts);
        block
            .cache
            .shaped(&mut measure, &mut self.runs)
            .min_content_width()
    }
}

/// What a box shows. Boxes without content are drawn as placeholders:
/// checkboxes as checkboxes, everything else as a sunken rectangle.
#[derive(Clone)]
pub enum BoxContent {
    /// An image, scaled to the box. A placeholder is drawn until it loads.
    Image(ImageSource),
    /// An SVG icon from the application's assets, drawn in a token colour
    /// and centred in the box.
    Icon {
        path: SharedString,
        color: ColorRole,
    },
    /// Nothing: the box only takes space (for example the gap before a
    /// link status marker).
    Blank,
    /// A typeset formula, `em` px per em, the surrounding text colour in
    /// `color`; characters outside the math fonts use `text_font`.
    Math {
        layout: Arc<gv_math::MathLayout>,
        em: f32,
        color: ColorRole,
        text_font: gpui::Font,
    },
}

impl std::fmt::Debug for BoxContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Image(_) => f.write_str("Image(..)"),
            Self::Icon { path, color } => f
                .debug_struct("Icon")
                .field("path", path)
                .field("color", color)
                .finish(),
            Self::Blank => f.write_str("Blank"),
            Self::Math { em, color, .. } => f
                .debug_struct("Math")
                .field("em", em)
                .field("color", color)
                .finish_non_exhaustive(),
        }
    }
}

/// Where and how a block was painted last.
#[derive(Debug, Clone)]
pub(crate) struct Painted {
    pub bounds: Bounds<Pixels>,
    pub layout: Arc<Layout<Glyphs>>,
    pub stats: PaintStats,
}

/// The layout state of one paragraph, kept across frames.
#[derive(Debug)]
pub struct InlineBlock {
    cache: BlockCache<Glyphs>,
    boxes: HashMap<u32, BoxContent>,
    generation: u64,
    painted: Option<Painted>,
}

impl InlineBlock {
    pub fn new(paragraph: Arc<Paragraph>) -> Self {
        Self {
            cache: BlockCache::new(paragraph),
            boxes: HashMap::new(),
            generation: 0,
            painted: None,
        }
    }

    pub fn paragraph(&self) -> &Arc<Paragraph> {
        self.cache.paragraph()
    }

    /// Replaces the content; shaping and layouts are dropped, box content
    /// is kept (box ids are the caller's).
    pub fn set_paragraph(&mut self, paragraph: Arc<Paragraph>) {
        self.cache.set_paragraph(paragraph);
        self.painted = None;
    }

    /// Sets what the boxes with [`gv_inline::InlineBox::id`] `id` show.
    pub fn set_box_content(&mut self, id: u32, content: BoxContent) {
        self.boxes.insert(id, content);
    }

    pub fn box_content(&self, id: u32) -> Option<&BoxContent> {
        self.boxes.get(&id)
    }

    pub fn cache_stats(&self) -> BlockCacheStats {
        self.cache.stats()
    }

    /// The bounds of the last paint, in window coordinates.
    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.painted.as_ref().map(|p| p.bounds)
    }

    /// The layout painted last.
    pub fn layout(&self) -> Option<&Arc<Layout<Glyphs>>> {
        self.painted.as_ref().map(|p| &p.layout)
    }

    /// What the last paint drew.
    pub fn paint_stats(&self) -> PaintStats {
        self.painted.as_ref().map(|p| p.stats).unwrap_or_default()
    }

    /// Hit test at a window position against the last paint: the nearest
    /// caret stop, with its source offset in [`Hit::source`]. Positions
    /// above or below the block give its first or last line. `None` before
    /// the block was painted.
    pub fn hit(&self, position: Point<Pixels>) -> Option<Hit> {
        let p = self.painted.as_ref()?;
        let local = position - p.bounds.origin;
        Some(p.layout.hit(f32::from(local.x), f32::from(local.y)))
    }

    /// The item (for example a link run) under a window position, padding
    /// and margins included, for hover regions.
    pub fn item_at(&self, position: Point<Pixels>) -> Option<usize> {
        let p = self.painted.as_ref()?;
        let local = position - p.bounds.origin;
        p.layout
            .item_at_point(f32::from(local.x), f32::from(local.y))
    }

    /// The caret for a source offset, in window coordinates, as painted
    /// last (for autoscroll and input method placement).
    pub fn caret_bounds(&self, source: usize, bias: Bias) -> Option<Bounds<Pixels>> {
        let p = self.painted.as_ref()?;
        let c = p.layout.caret_for_source(source, bias);
        Some(Bounds::new(
            p.bounds.origin + point(px(c.x), px(c.top)),
            size(px(0.0), px(c.height)),
        ))
    }
}

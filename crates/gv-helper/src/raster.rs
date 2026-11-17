// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! PDF page raster inside the helper: one page of a PDF drawn by hayro
//! (pure Rust) on white, scaled to fit a box, for previews and
//! thumbnails.
//!
//! The image is capped at [`MAX_SIDE`] pixels a side (hayro wraps its
//! sizes above 65,535 silently); the parent's deadline stops a page that
//! takes too long. A panic inside hayro is caught and reported as an
//! error.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings};

use crate::protocol::{PageImage, PageJob};

/// The largest side of a page image, in pixels.
pub const MAX_SIDE: u32 = crate::protocol::MAX_PAGE_SIDE;

/// Draws page `job.page` (0-based) of `job.pdf` to fit `job.max_width` ×
/// `job.max_height` pixels, keeping its proportions.
pub fn page(job: &PageJob) -> Result<PageImage, String> {
    catch_unwind(AssertUnwindSafe(|| draw(job)))
        .unwrap_or_else(|_| Err("the PDF page could not be drawn".to_owned()))
}

fn draw(job: &PageJob) -> Result<PageImage, String> {
    let pdf = Pdf::new(job.pdf.clone()).map_err(|e| format!("not a readable PDF: {e:?}"))?;
    let pages = pdf.pages();
    let count = u32::try_from(pages.len()).unwrap_or(u32::MAX);
    let page = pages
        .get(job.page as usize)
        .ok_or_else(|| format!("the PDF has no page {}", job.page + 1))?;
    let (w, h) = page.render_dimensions();
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err("the page has no size".to_owned());
    }
    let max_w = job.max_width.clamp(1, MAX_SIDE) as f32;
    let max_h = job.max_height.clamp(1, MAX_SIDE) as f32;
    let scale = (max_w / w).min(max_h / h);
    let width = ((w * scale).floor() as u32).clamp(1, MAX_SIDE);
    let height = ((h * scale).floor() as u32).clamp(1, MAX_SIDE);
    let settings = RenderSettings {
        x_scale: scale,
        y_scale: scale,
        width: Some(width as u16),
        height: Some(height as u16),
        bg_color: WHITE,
    };
    let cache = RenderCache::new();
    let pixmap = hayro::render(page, &cache, &InterpreterSettings::default(), &settings);
    let rgba = pixmap
        .take_unpremultiplied()
        .into_iter()
        .flat_map(|p| [p.r, p.g, p.b, p.a])
        .collect();
    Ok(PageImage {
        width,
        height,
        rgba,
        pages: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_an_error() {
        let job = PageJob {
            pdf: b"not a pdf".to_vec(),
            page: 0,
            max_width: 100,
            max_height: 100,
        };
        assert!(page(&job).is_err());
    }
}

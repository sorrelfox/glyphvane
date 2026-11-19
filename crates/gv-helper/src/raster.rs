// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! PDF page raster inside the helper, by hayro (pure Rust), on white.
//!
//! - [`page`]: one page of a PDF scaled to fit a box (previews,
//!   thumbnails, embeds); nothing is kept.
//! - [`open`] and [`Session`]: a PDF read once and drawn page by page at a
//!   scale (the PDF viewer). A session keeps hayro's caches (fonts,
//!   glyphs, decoded resources) and one reused drawing context while the
//!   parent draws pages of the same PDF. It also reads the text of its
//!   pages ([`crate::text`]), with caches of its own.
//!
//! Limits: an image is at most [`MAX_SIDE`] (fit) or
//! [`protocol::MAX_RENDER_SIDE`] pixels a side and
//! [`protocol::MAX_RENDER_PIXELS`] pixels in all (hayro wraps its sizes
//! above 65,535 silently); a page whose images would decode to more than
//! [`protocol::MAX_IMAGE_PIXELS`] is refused before it is drawn (hayro
//! decodes images at full size); the parent's deadline stops a page that
//! takes too long, and the helper's address-space limit one that takes
//! too much memory. A panic inside hayro is caught and reported as an
//! error.

use std::panic::{AssertUnwindSafe, catch_unwind};

use hayro::hayro_interpret::util::TransformExt as _;
use hayro::hayro_interpret::{InterpreterCache, InterpreterSettings};
use hayro::hayro_syntax::object::{Dict, Name, Stream};
use hayro::hayro_syntax::page::Page;
use hayro::hayro_syntax::{DecryptionError, LoadPdfError, Pdf};
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings};

use crate::protocol::{
    self, DocInfo, MAX_IMAGE_PIXELS, MAX_RENDER_PIXELS, MAX_RENDER_SIDE, PageImage, PageJob,
    PageSize, PageText, RenderJob, TextJob,
};

/// The largest side of a page image fitted in a box, in pixels.
pub const MAX_SIDE: u32 = protocol::MAX_PAGE_SIDE;

/// How deep the image pre-scan follows form XObjects.
const SCAN_DEPTH: u32 = 8;

/// How many XObjects the pre-scan looks at on one page.
const SCAN_OBJECTS: usize = 4096;

/// Reads `pdf`, with a sentence for the user when it cannot be read.
pub fn open(pdf: Vec<u8>) -> Result<Pdf, String> {
    catch_unwind(AssertUnwindSafe(|| Pdf::new(pdf)))
        .map_err(|_| "The file is not a readable PDF.".to_owned())?
        .map_err(|e| match e {
            LoadPdfError::Decryption(
                DecryptionError::PasswordProtected | DecryptionError::MissingIDEntry,
            ) => "The PDF is protected by a password.".to_owned(),
            LoadPdfError::Decryption(_) => {
                "The PDF is encrypted in a way Glyphvane cannot read.".to_owned()
            }
            LoadPdfError::Invalid => "The file is not a readable PDF.".to_owned(),
        })
}

/// The pages of `pdf` and their sizes.
pub fn info(pdf: &Pdf) -> DocInfo {
    DocInfo {
        pages: pdf
            .pages()
            .iter()
            .map(|page| {
                let (width, height) = page.render_dimensions();
                PageSize { width, height }
            })
            .collect(),
    }
}

/// Draws page `job.page` (0-based) of `job.pdf` to fit `job.max_width` ×
/// `job.max_height` pixels, keeping its proportions.
pub fn page(job: &PageJob) -> Result<PageImage, String> {
    catch_unwind(AssertUnwindSafe(|| fit(job)))
        .unwrap_or_else(|_| Err("The PDF page could not be drawn.".to_owned()))
}

fn fit(job: &PageJob) -> Result<PageImage, String> {
    let pdf = open(job.pdf.clone())?;
    let pages = pdf.pages();
    let count = u32::try_from(pages.len()).unwrap_or(u32::MAX);
    let page = pages
        .get(job.page as usize)
        .ok_or_else(|| format!("The PDF has no page {}.", job.page + 1))?;
    let (w, h) = sized(page)?;
    let max_w = job.max_width.clamp(1, MAX_SIDE) as f32;
    let max_h = job.max_height.clamp(1, MAX_SIDE) as f32;
    let scale = (max_w / w).min(max_h / h);
    let width = ((w * scale).floor() as u32).clamp(1, MAX_SIDE);
    let height = ((h * scale).floor() as u32).clamp(1, MAX_SIDE);
    prescan(page)?;
    let cache = RenderCache::new();
    let mut ctx = None;
    let rgba = draw(page, &cache, &mut ctx, scale, width, height);
    Ok(PageImage {
        width,
        height,
        rgba,
        pages: count,
    })
}

/// The page's size in points, refused when it has none.
fn sized(page: &Page<'_>) -> Result<(f32, f32), String> {
    let (w, h) = page.render_dimensions();
    if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
        Ok((w, h))
    } else {
        Err("The page has no size.".to_owned())
    }
}

/// The pixel size of a page of `w` × `h` points at `scale` pixels per
/// point, made smaller (keeping its proportions) to stay within the side
/// cap and `max_pixels`; and the scale used.
pub fn render_size(w: f32, h: f32, scale: f32, max_pixels: u64) -> (u32, u32, f32) {
    let max_pixels = max_pixels.clamp(1, MAX_RENDER_PIXELS) as f32;
    let side = MAX_RENDER_SIDE as f32;
    let mut scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    scale = scale.min(side / w).min(side / h);
    let area = w * scale * h * scale;
    if area > max_pixels {
        scale *= (max_pixels / area).sqrt();
    }
    let width = ((w * scale).floor() as u32).clamp(1, MAX_RENDER_SIDE);
    let height = ((h * scale).floor() as u32).clamp(1, MAX_RENDER_SIDE);
    (width, height, scale)
}

/// Draws `page` at `scale` into a `width` × `height` image on white,
/// reusing the drawing context in `ctx`; straight-alpha RGBA rows.
fn draw<'a>(
    page: &'a Page<'a>,
    cache: &RenderCache<'a>,
    ctx: &mut Option<RenderContext>,
    scale: f32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    // The sizes are capped far below u16::MAX by the callers.
    let (w, h) = (
        u16::try_from(width).unwrap_or(u16::MAX),
        u16::try_from(height).unwrap_or(u16::MAX),
    );
    let ctx = match ctx {
        Some(ctx) => {
            ctx.reset_and_resize(w, h);
            ctx
        }
        None => ctx.insert(RenderContext::new(w, h)),
    };
    let transform = Affine::scale(f64::from(scale)) * page.initial_transform(true).to_kurbo();
    hayro::render_into(
        page,
        cache,
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        ctx,
        transform,
    );
    ctx.flush();
    let mut pixmap = Pixmap::new(w, h);
    ctx.render_with(
        &mut pixmap,
        &mut Resources::default(),
        RasterizerSettings {
            target_init: TargetInit::Clear(WHITE),
            ..RasterizerSettings::default()
        },
    );
    pixmap.take_rgba8(ImageAlphaType::Alpha)
}

/// Refuses a page whose images would decode to more than
/// [`MAX_IMAGE_PIXELS`] (each, or all together), looking into form
/// XObjects too.
pub fn prescan(page: &Page<'_>) -> Result<(), String> {
    let mut total = 0u64;
    let mut seen = 0usize;
    scan(&page.resources().x_objects, 0, &mut total, &mut seen)
}

fn scan(x_objects: &Dict<'_>, depth: u32, total: &mut u64, seen: &mut usize) -> Result<(), String> {
    if depth > SCAN_DEPTH {
        return Ok(());
    }
    let too_large = || Err("The page has an image too large to draw.".to_owned());
    for key in x_objects.keys() {
        *seen += 1;
        if *seen > SCAN_OBJECTS {
            return Ok(());
        }
        let Some(stream) = x_objects.get::<Stream<'_>>(key.as_ref()) else {
            continue;
        };
        let dict = stream.dict();
        let subtype = dict.get::<Name<'_>>(b"Subtype");
        match subtype.as_deref() {
            Some(b"Image") => {
                let w = dict.get::<f64>(b"Width").unwrap_or(0.0).max(0.0);
                let h = dict.get::<f64>(b"Height").unwrap_or(0.0).max(0.0);
                let pixels = w * h;
                if !pixels.is_finite() || pixels > MAX_IMAGE_PIXELS as f64 {
                    return too_large();
                }
                *total = total.saturating_add(pixels as u64);
                if *total > MAX_IMAGE_PIXELS {
                    return too_large();
                }
                // A soft mask is an image of its own.
                if let Some(mask) = dict.get::<Stream<'_>>(b"SMask") {
                    let md = mask.dict();
                    let mw = md.get::<f64>(b"Width").unwrap_or(0.0).max(0.0);
                    let mh = md.get::<f64>(b"Height").unwrap_or(0.0).max(0.0);
                    *total = total.saturating_add((mw * mh).min(u64::MAX as f64) as u64);
                    if *total > MAX_IMAGE_PIXELS {
                        return too_large();
                    }
                }
            }
            Some(b"Form") => {
                if let Some(resources) = dict.get::<Dict<'_>>(b"Resources")
                    && let Some(inner) = resources.get::<Dict<'_>>(b"XObject")
                {
                    scan(&inner, depth + 1, total, seen)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A PDF kept for drawing page by page ([`crate::protocol::Job::Render`]):
/// hayro's caches and the drawing context live as long as the session.
pub struct Session<'a> {
    pdf: &'a Pdf,
    cache: RenderCache<'a>,
    ctx: Option<RenderContext>,
    /// The fonts and objects read for the pages' text.
    text_cache: InterpreterCache<'a>,
    /// A panic left the caches in an unknown state: the session ends.
    pub broken: bool,
}

impl std::fmt::Debug for Session<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("pages", &self.pdf.pages().len())
            .field("broken", &self.broken)
            .finish_non_exhaustive()
    }
}

impl<'a> Session<'a> {
    pub fn new(pdf: &'a Pdf) -> Self {
        Self {
            pdf,
            cache: RenderCache::new(),
            ctx: None,
            text_cache: InterpreterCache::new(),
            broken: false,
        }
    }

    /// The text of page `job.page` and each character's box.
    pub fn text(&mut self, job: &TextJob) -> Result<PageText, String> {
        let pdf = self.pdf;
        let pages = pdf.pages();
        let page = pages
            .get(job.page as usize)
            .ok_or_else(|| format!("The PDF has no page {}.", job.page + 1))?;
        let result = crate::text::extract_caught(page, &self.text_cache);
        if result.is_err() {
            self.broken = true;
            self.ctx = None;
        }
        result
    }

    /// Draws page `job.page` at `job.scale` pixels per point, smaller when
    /// the image would pass the caps.
    pub fn render(&mut self, job: &RenderJob) -> Result<PageImage, String> {
        let pdf = self.pdf;
        let cache = &self.cache;
        let ctx = &mut self.ctx;
        let result = catch_unwind(AssertUnwindSafe(|| {
            let pages = pdf.pages();
            let count = u32::try_from(pages.len()).unwrap_or(u32::MAX);
            let page = pages
                .get(job.page as usize)
                .ok_or_else(|| format!("The PDF has no page {}.", job.page + 1))?;
            let (w, h) = sized(page)?;
            prescan(page)?;
            let (width, height, scale) = render_size(w, h, job.scale, job.max_pixels);
            let rgba = draw(page, cache, ctx, scale, width, height);
            Ok(PageImage {
                width,
                height,
                rgba,
                pages: count,
            })
        }));
        match result {
            Ok(result) => result,
            Err(_) => {
                self.broken = true;
                self.ctx = None;
                Err("The PDF page could not be drawn.".to_owned())
            }
        }
    }
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
        assert!(open(b"not a pdf".to_vec()).is_err());
    }

    #[test]
    fn render_sizes_keep_within_the_caps() {
        // A Letter page at 2 px per point.
        let (w, h, s) = render_size(612.0, 792.0, 2.0, MAX_RENDER_PIXELS);
        assert_eq!((w, h), (1224, 1584));
        assert!((s - 2.0).abs() < 1e-6);
        // A huge page is drawn smaller, in proportion.
        let (w, h, _) = render_size(14_400.0, 14_400.0, 4.0, MAX_RENDER_PIXELS);
        assert!(u64::from(w) * u64::from(h) <= MAX_RENDER_PIXELS);
        assert_eq!(w, h);
        // A long strip keeps under the side cap.
        let (w, h, _) = render_size(100.0, 100_000.0, 1.0, MAX_RENDER_PIXELS);
        assert!(h <= MAX_RENDER_SIDE && w >= 1);
        // A small pixel budget.
        let (w, h, _) = render_size(612.0, 792.0, 2.0, 10_000);
        assert!(u64::from(w) * u64::from(h) <= 10_000);
        // Nonsense scales draw at 1.
        let (w, _, _) = render_size(100.0, 100.0, f32::NAN, MAX_RENDER_PIXELS);
        assert_eq!(w, 100);
    }
}

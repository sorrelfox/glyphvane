// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The PDF engine of the app and of `glyphvane export --pdf`: generated
//! Typst documents compiled by the separate `glyphvane-helper` executable,
//! found next to this one, started on demand and supervised (deadline,
//! cancellation, memory limit, kill and restart). The helper also draws
//! pages of a PDF: for the export dialog's preview, canvas cards and PDF
//! embeds (a page fitted in a box), and for the PDF viewer (a PDF opened
//! once, drawn page by page at a scale, read again when a new helper no
//! longer holds it), and reads the text of its pages for the viewer's
//! find and selection.

use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use gv_export::TypstDocument;
use gv_helper::protocol::MAX_RENDER_PIXELS;
use gv_helper::{Helper, HelperError, PageJob, PdfJob, RenderJob, TextJob};
use gv_ui::export::{PageRaster, PdfEngine, PdfError, PdfPageText};

/// The longest a PDF may take (plan: 60 s; 300 pages took 0.33 s).
const DEADLINE: Duration = Duration::from_secs(60);

/// The longest drawing a page may take (plan: 5 s per page).
const PAGE_DEADLINE: Duration = Duration::from_secs(5);

/// The longest reading a PDF for the viewer may take.
const OPEN_DEADLINE: Duration = Duration::from_secs(10);

/// The largest PDF accepted, in bytes (under the IPC frame cap).
const MAX_OUTPUT: u64 = 200 * 1024 * 1024;

/// The helper, found on first use.
#[derive(Debug, Default)]
pub struct HelperEngine {
    helper: Mutex<Option<Helper>>,
}

impl HelperEngine {
    fn helper(&self) -> Result<Helper, PdfError> {
        let mut slot = self.helper.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(helper) = slot.as_ref() {
            return Ok(helper.clone());
        }
        let helper = Helper::installed().map_err(|e| PdfError::Failed(e.to_string()))?;
        *slot = Some(helper.clone());
        Ok(helper)
    }
}

impl PdfEngine for HelperEngine {
    fn render(
        &self,
        doc: &TypstDocument,
        max_pages: u32,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, PdfError> {
        let helper = self.helper()?;
        let job = PdfJob {
            source: doc.source.clone(),
            assets: doc.assets.clone(),
            fonts: doc.fonts.clone(),
            max_pages,
            ident: doc.ident.clone(),
        };
        match helper.pdf(job, DEADLINE, MAX_OUTPUT, cancel) {
            Ok(out) => {
                for warning in &out.warnings {
                    tracing::debug!("PDF export: {warning}");
                }
                Ok(out.pdf)
            }
            Err(HelperError::Cancelled) => Err(PdfError::Cancelled),
            Err(e) => Err(PdfError::Failed(e.to_string())),
        }
    }

    fn draws_pages(&self) -> bool {
        true
    }

    fn page(
        &self,
        pdf: &[u8],
        page: u32,
        max_width: u32,
        max_height: u32,
        cancel: &AtomicBool,
    ) -> Result<PageRaster, PdfError> {
        let helper = self.helper()?;
        let job = PageJob {
            pdf: pdf.to_vec(),
            page,
            max_width,
            max_height,
        };
        match helper.page(job, PAGE_DEADLINE, cancel) {
            Ok(image) => Ok(PageRaster {
                width: image.width,
                height: image.height,
                rgba: image.rgba,
                pages: image.pages,
            }),
            Err(HelperError::Cancelled) => Err(PdfError::Cancelled),
            Err(e) => Err(PdfError::Failed(e.to_string())),
        }
    }

    fn open_document(
        &self,
        key: u64,
        pdf: &[u8],
        cancel: &AtomicBool,
    ) -> Result<Vec<(f32, f32)>, PdfError> {
        let helper = self.helper()?;
        match helper.open(key, pdf.to_vec(), OPEN_DEADLINE, cancel) {
            Ok(info) => Ok(info.pages.iter().map(|p| (p.width, p.height)).collect()),
            Err(e) => Err(failed(e)),
        }
    }

    fn render_page(
        &self,
        key: u64,
        pdf: &[u8],
        page: u32,
        scale: f32,
        cancel: &AtomicBool,
    ) -> Result<PageRaster, PdfError> {
        let helper = self.helper()?;
        let job = RenderJob {
            key,
            page,
            scale,
            max_pixels: MAX_RENDER_PIXELS,
        };
        // A helper started since (after a deadline, a crash or the idle
        // stop), or one that opened another PDF since, reads it again.
        let mut image = helper
            .render(job.clone(), PAGE_DEADLINE, cancel)
            .map_err(failed)?;
        if image.is_none() {
            helper
                .open(key, pdf.to_vec(), OPEN_DEADLINE, cancel)
                .map_err(failed)?;
            image = helper.render(job, PAGE_DEADLINE, cancel).map_err(failed)?;
        }
        let image =
            image.ok_or_else(|| PdfError::Failed("the PDF engine lost the document".to_owned()))?;
        Ok(PageRaster {
            width: image.width,
            height: image.height,
            rgba: image.rgba,
            pages: image.pages,
        })
    }

    fn page_text(
        &self,
        key: u64,
        pdf: &[u8],
        page: u32,
        cancel: &AtomicBool,
    ) -> Result<PdfPageText, PdfError> {
        let text = self.in_document(key, pdf, cancel, |helper| {
            helper.text(TextJob { key, page }, PAGE_DEADLINE, cancel)
        })?;
        Ok(PdfPageText {
            text: text.text,
            boxes: text
                .boxes
                .iter()
                .map(|b| [b.x0, b.y0, b.x1, b.y1])
                .collect(),
        })
    }
}

impl HelperEngine {
    /// Runs `job` for the PDF `pdf` kept as `key`, opening it again once
    /// when the helper no longer holds it.
    fn in_document<T>(
        &self,
        key: u64,
        pdf: &[u8],
        cancel: &AtomicBool,
        job: impl Fn(&Helper) -> Result<Option<T>, HelperError>,
    ) -> Result<T, PdfError> {
        let helper = self.helper()?;
        let mut out = job(&helper).map_err(failed)?;
        if out.is_none() {
            helper
                .open(key, pdf.to_vec(), OPEN_DEADLINE, cancel)
                .map_err(failed)?;
            out = job(&helper).map_err(failed)?;
        }
        out.ok_or_else(|| PdfError::Failed("the PDF engine lost the document".to_owned()))
    }
}

/// A helper error as the engine's.
fn failed(e: HelperError) -> PdfError {
    match e {
        HelperError::Cancelled => PdfError::Cancelled,
        e => PdfError::Failed(e.to_string()),
    }
}

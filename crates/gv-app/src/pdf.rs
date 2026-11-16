// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The PDF engine of the app and of `glyphvane export --pdf`: generated
//! Typst documents compiled by the separate `glyphvane-helper` executable,
//! found next to this one, started on demand and supervised (deadline,
//! cancellation, memory limit, kill and restart).

use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use gv_export::TypstDocument;
use gv_helper::{Helper, HelperError, PdfJob};
use gv_ui::export::{PdfEngine, PdfError};

/// The longest a PDF may take (plan: 60 s; 300 pages took 0.33 s).
const DEADLINE: Duration = Duration::from_secs(60);

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
}

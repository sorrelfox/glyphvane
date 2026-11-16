// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Printing (FR-EXPORT-03): the document's PDF handed to the platform's
//! print dialog through a [`Printer`] the application installs.
//!
//! A print runs in two steps, so the PDF can be laid out for the paper the
//! user picks:
//!
//! 1. [`Printer::prepare`] shows the dialog where the platform asks first
//!    (the desktop portal on Linux) and returns the page set-up chosen, or
//!    `None` when the user cancels. Platforms whose dialog shows the PDF
//!    itself answer at once, without a page set-up.
//! 2. The window makes the PDF off the UI thread, then
//!    [`Printer::print`] hands it over.
//!
//! Without a native print path, [`ViewerPrinter`] writes the PDF to a
//! private temporary folder and opens it in the system's PDF viewer, to
//! print from there.

use std::any::Any;
use std::path::PathBuf;
use std::rc::Rc;

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{App, Global, Window};

use crate::platform;

/// The page a print dialog chose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSetup {
    /// The paper's size in millimetres, portrait.
    pub width_mm: f32,
    pub height_mm: f32,
    pub landscape: bool,
    /// The smallest margin the dialog asked for, in millimetres.
    pub margin_mm: Option<f32>,
}

/// What [`Printer::prepare`] returns and [`Printer::print`] takes back.
pub struct PrintSetup {
    /// The page to lay the PDF out on; `None` keeps the default page.
    pub page: Option<PageSetup>,
    /// The printer's own state between the two steps.
    pub state: Box<dyn Any>,
}

impl std::fmt::Debug for PrintSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintSetup")
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

/// How a print ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Printed {
    /// The job went to the printer (or the user cancelled the second
    /// dialog, which the platform does not tell apart).
    Sent,
    /// The user cancelled.
    Cancelled,
    /// The PDF was opened in the system's viewer, to print from there.
    InViewer(PathBuf),
}

/// The platform's print path.
pub trait Printer {
    /// Asks how to print document `title`: `Ok(None)` when the user
    /// cancels.
    fn prepare(
        &self,
        title: &str,
        window: &Window,
        cx: &mut App,
    ) -> LocalBoxFuture<'static, Result<Option<PrintSetup>, String>>;

    /// Prints `pdf` as `setup` says.
    fn print(
        &self,
        setup: PrintSetup,
        title: &str,
        pdf: Vec<u8>,
        cx: &mut App,
    ) -> LocalBoxFuture<'static, Result<Printed, String>>;
}

/// Prints by opening the PDF in the system's viewer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ViewerPrinter;

impl Printer for ViewerPrinter {
    fn prepare(
        &self,
        _: &str,
        _: &Window,
        _: &mut App,
    ) -> LocalBoxFuture<'static, Result<Option<PrintSetup>, String>> {
        async {
            Ok(Some(PrintSetup {
                page: None,
                state: Box::new(()),
            }))
        }
        .boxed_local()
    }

    fn print(
        &self,
        _: PrintSetup,
        title: &str,
        pdf: Vec<u8>,
        cx: &mut App,
    ) -> LocalBoxFuture<'static, Result<Printed, String>> {
        let name = format!("{}.pdf", safe_name(title));
        let result = crate::export::write_temporary_bytes(&pdf, &name)
            .map_err(|e| e.to_string())
            .map(|path| {
                platform::launcher(cx).open_with_default_app(&path, cx);
                Printed::InViewer(path)
            });
        async move { result }.boxed_local()
    }
}

/// `title` as a file name: no separators or control characters.
pub fn safe_name(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '-'
            } else {
                c
            }
        })
        .take(100)
        .collect();
    let name = name.trim().trim_matches('.').to_owned();
    if name.is_empty() {
        "Untitled".to_owned()
    } else {
        name
    }
}

struct InstalledPrinter(Rc<dyn Printer>);

impl Global for InstalledPrinter {}

/// Uses `printer` for *Print…*.
pub fn set_printer(cx: &mut App, printer: impl Printer + 'static) {
    cx.set_global(InstalledPrinter(Rc::new(printer)));
}

/// The installed printer, else [`ViewerPrinter`].
pub(crate) fn printer(cx: &App) -> Rc<dyn Printer> {
    match cx.try_global::<InstalledPrinter>() {
        Some(installed) => installed.0.clone(),
        None => Rc::new(ViewerPrinter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_become_safe_names() {
        assert_eq!(safe_name("a/b: c?"), "a-b- c-");
        assert_eq!(safe_name("  ..  "), "Untitled");
        assert_eq!(safe_name("Notes"), "Notes");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Print on Linux through the desktop portal (`org.freedesktop.portal.Print`):
//! `PreparePrint` shows the desktop's own dialog (Qt's on KDE, GTK's
//! elsewhere), modal to the window; the PDF is then made for the paper
//! chosen and handed to `Print` as an in-memory file, so no temporary
//! file is written and no second dialog shows.
//!
//! Without the portal (or without a print backend for it), the PDF opens
//! in the system's viewer instead ([`ViewerPrinter`]).

use std::fs::File;
use std::io::{Seek as _, SeekFrom, Write as _};

use ashpd::WindowIdentifier;
use ashpd::desktop::ResponseError;
use ashpd::desktop::print::{
    Orientation, PageSetup as PortalPage, PreparePrintOptions, PrintOptions, PrintProxy, Settings,
};
use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{App, Window};
use gv_ui::print::{PageSetup, PrintSetup, Printed, Printer, ViewerPrinter};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

/// The portal's print dialog.
#[derive(Debug, Clone, Copy, Default)]
pub struct PortalPrinter;

/// Between the two steps: the portal's token and the parent window.
struct Prepared {
    token: u32,
    parent: Option<WindowIdentifier>,
}

impl Printer for PortalPrinter {
    fn prepare(
        &self,
        title: &str,
        window: &Window,
        _: &mut App,
    ) -> LocalBoxFuture<'static, Result<Option<PrintSetup>, String>> {
        let handle = HasWindowHandle::window_handle(window)
            .ok()
            .map(|h| h.as_raw());
        let display = HasDisplayHandle::display_handle(window)
            .ok()
            .map(|h| h.as_raw());
        let title = title.to_owned();
        async move {
            let Ok(proxy) = PrintProxy::new().await else {
                tracing::info!("no print portal: the PDF opens in the viewer");
                return Ok(Some(PrintSetup {
                    page: None,
                    state: Box::new(()),
                }));
            };
            let parent = match &handle {
                Some(handle) => WindowIdentifier::from_raw_handle(handle, display.as_ref()).await,
                None => None,
            };
            let request = proxy
                .prepare_print(
                    parent.as_ref(),
                    &title,
                    Settings::default(),
                    PortalPage::default(),
                    PreparePrintOptions::default()
                        .set_modal(true)
                        .set_accept_label("Print"),
                )
                .await
                .map_err(|e| format!("the print dialog could not be shown: {e}"))?;
            match request.response() {
                Ok(prepared) => Ok(Some(PrintSetup {
                    page: page_setup(&prepared.page_setup),
                    state: Box::new(Prepared {
                        token: prepared.token,
                        parent,
                    }),
                })),
                Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(None),
                Err(e) => Err(format!("the print dialog failed: {e}")),
            }
        }
        .boxed_local()
    }

    fn print(
        &self,
        setup: PrintSetup,
        title: &str,
        pdf: Vec<u8>,
        cx: &mut App,
    ) -> LocalBoxFuture<'static, Result<Printed, String>> {
        let prepared = match setup.state.downcast::<Prepared>() {
            Ok(prepared) => *prepared,
            Err(state) => {
                return ViewerPrinter.print(PrintSetup { page: None, state }, title, pdf, cx);
            }
        };
        let title = title.to_owned();
        async move {
            let file = memfd(&pdf).map_err(|e| format!("the PDF could not be passed on: {e}"))?;
            let proxy = PrintProxy::new()
                .await
                .map_err(|e| format!("the print portal is gone: {e}"))?;
            let request = proxy
                .print(
                    prepared.parent.as_ref(),
                    &title,
                    &file,
                    PrintOptions::default()
                        .set_token(prepared.token)
                        .set_modal(true),
                )
                .await
                .map_err(|e| format!("the PDF could not be printed: {e}"))?;
            match request.response() {
                Ok(()) => Ok(Printed::Sent),
                Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(Printed::Cancelled),
                Err(e) => Err(format!("the PDF could not be printed: {e}")),
            }
        }
        .boxed_local()
    }
}

/// The page set-up the dialog chose, when it gave the paper's size.
fn page_setup(page: &PortalPage) -> Option<PageSetup> {
    let (width, height) = (page.width?, page.height?);
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let margins = [
        page.margin_top,
        page.margin_bottom,
        page.margin_left,
        page.margin_right,
    ];
    let margin = margins
        .iter()
        .flatten()
        .copied()
        .filter(|m| m.is_finite() && *m >= 0.0)
        .fold(None, |a: Option<f64>, m| Some(a.map_or(m, |a| a.max(m))));
    let (short, long) = (width.min(height), width.max(height));
    Some(PageSetup {
        width_mm: short as f32,
        height_mm: long as f32,
        landscape: matches!(
            page.orientation,
            Some(Orientation::Landscape | Orientation::ReverseLandscape)
        ),
        margin_mm: margin.map(|m| m as f32),
    })
}

/// The PDF as an anonymous in-memory file, rewound: the portal's backend
/// reads it through the same open file description.
fn memfd(bytes: &[u8]) -> std::io::Result<File> {
    let fd = rustix::fs::memfd_create("glyphvane-print.pdf", rustix::fs::MemfdFlags::CLOEXEC)?;
    let mut file = File::from(fd);
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    #[test]
    fn the_memory_file_is_rewound_and_whole() {
        let mut file = memfd(b"%PDF-1.7 bytes").expect("memfd");
        let mut back = Vec::new();
        file.read_to_end(&mut back).expect("read");
        assert_eq!(back, b"%PDF-1.7 bytes");
    }

    #[test]
    fn the_paper_size_comes_from_the_page_setup() {
        let page = PortalPage::default()
            .set_width(215.9)
            .set_height(279.4)
            .set_margin_top(6.0)
            .set_margin_left(12.0)
            .set_orientation(Orientation::Landscape);
        let setup = page_setup(&page).expect("a size");
        assert!((setup.width_mm - 215.9).abs() < 0.01);
        assert!((setup.height_mm - 279.4).abs() < 0.01);
        assert!(setup.landscape);
        assert_eq!(setup.margin_mm, Some(12.0));
        assert_eq!(page_setup(&PortalPage::default()), None);
    }
}

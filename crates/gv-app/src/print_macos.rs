// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Print on macOS: PDFKit makes a print operation for the PDF and AppKit
//! shows the standard print panel as a sheet on the document window, with
//! its own preview, printer and paper choice and the PDF menu ("Save as
//! PDF", "Open in Preview"). Pages are drawn as vectors.
//!
//! macOS asks everything in one panel, after the PDF exists, so the two
//! steps of [`Printer`] are:
//!
//! 1. [`Printer::prepare`] reads the paper and orientation of the shared
//!    print info (the default printer's paper, or the last one used here)
//!    and the PDF is made for that page. No dialog shows yet.
//! 2. [`Printer::print`] shows the panel. Paper changed there is fitted by
//!    PDFKit (scaled down, rotated to the paper) and remembered in the
//!    shared print info, so the next print lays the PDF out on it.
//!
//! Everything runs on the main thread (AppKit), inside GPUI's foreground
//! tasks; the sheet does not block it.

#![allow(unsafe_code)] // AppKit and PDFKit calls; each block says why it is sound.

use std::cell::Cell;
use std::ffi::c_void;

use futures::FutureExt as _;
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use gpui::{App, Window};
use gv_ui::print::{PageSetup, PrintSetup, Printed, Printer, ViewerPrinter};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{
    AnyThread as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    sel,
};
use objc2_app_kit::{NSPaperOrientation, NSPrintInfo, NSPrintOperation, NSView, NSWindow};
use objc2_foundation::{NSData, NSObjectProtocol, NSString};
use objc2_pdf_kit::{PDFDocument, PDFPrintScalingMode};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The PDFKit print sheet.
#[derive(Debug, Clone, Copy, Default)]
pub struct MacPrinter;

/// Between the two steps: the window the sheet attaches to.
struct Prepared {
    window: Retained<NSWindow>,
}

impl Printer for MacPrinter {
    fn prepare(
        &self,
        _: &str,
        window: &Window,
        _: &mut App,
    ) -> LocalBoxFuture<'static, Result<Option<PrintSetup>, String>> {
        let result = match (MainThreadMarker::new(), ns_window(window)) {
            (Some(_), Some(window)) => Ok(Some(PrintSetup {
                page: shared_page(),
                state: Box::new(Prepared { window }),
            })),
            (None, _) => Err("printing must start on the main thread".to_owned()),
            (_, None) => Err("the window has no native handle".to_owned()),
        };
        async move { result }.boxed_local()
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
            let printed = show_sheet(&prepared.window, &title, &pdf)?.await;
            Ok(match printed {
                Ok(true) => Printed::Sent,
                // Cancelled, or the sheet went away with its window.
                Ok(false) | Err(_) => Printed::Cancelled,
            })
        }
        .boxed_local()
    }
}

/// The `NSWindow` behind GPUI's window.
fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    // SAFETY: GPUI's handle points to the window's live content view.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

/// The page of the shared print info: its paper (in points) and
/// orientation.
fn shared_page() -> Option<PageSetup> {
    let info = NSPrintInfo::sharedPrintInfo();
    let size = info.paperSize();
    page_from_points(
        size.width,
        size.height,
        info.orientation() == NSPaperOrientation::Landscape,
    )
}

/// A page set-up from a paper size in points (1/72 inch).
fn page_from_points(width: f64, height: f64, landscape: bool) -> Option<PageSetup> {
    if !(width > 0.0 && height > 0.0 && width.is_finite() && height.is_finite()) {
        return None;
    }
    let mm = |pt: f64| (pt * 25.4 / 72.0) as f32;
    Some(PageSetup {
        width_mm: mm(width.min(height)),
        height_mm: mm(width.max(height)),
        landscape,
        // The panel's margins are the printer's; the PDF keeps its own.
        margin_mm: None,
    })
}

/// Starts the print sheet for `pdf` on `window`; the receiver answers
/// whether the job was sent (`false` when cancelled).
fn show_sheet(
    window: &NSWindow,
    title: &str,
    pdf: &[u8],
) -> Result<oneshot::Receiver<bool>, String> {
    let mtm = MainThreadMarker::new().ok_or("printing must run on the main thread")?;
    if !window.isVisible() {
        return Err("the window was closed".to_owned());
    }
    if window.attachedSheet().is_some() {
        return Err("the window is showing another dialog".to_owned());
    }
    let data = NSData::with_bytes(pdf);
    // SAFETY: PDFKit's initialiser; it returns nil for data that is not a
    // PDF.
    let document = unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &data) }
        .ok_or("the PDF could not be read")?;
    // SAFETY: a plain getter.
    if unsafe { document.pageCount() } == 0 {
        return Err("the PDF has no pages".to_owned());
    }
    let info = NSPrintInfo::sharedPrintInfo();
    // SAFETY: PDFKit's print operation (macOS 10.7+). Pages are only
    // scaled down, so a page larger than the paper picked in the panel
    // shrinks instead of being cropped, and turned to the paper's
    // orientation.
    let operation = unsafe {
        document.printOperationForPrintInfo_scalingMode_autoRotate(
            Some(&info),
            PDFPrintScalingMode::PageScaleDownToFit,
            true,
            mtm,
        )
    }
    .ok_or("the print panel could not be prepared")?;
    operation.setJobTitle(Some(&NSString::from_str(title)));
    operation.setShowsPrintPanel(true);
    operation.setShowsProgressPanel(true);

    let (sender, receiver) = oneshot::channel();
    let delegate = SheetDelegate::new(mtm, sender, operation.clone());
    let object: &AnyObject = &delegate;
    // SAFETY: the selector matches the delegate's method below, which
    // takes the operation, the result and the (unused) context pointer.
    // The delegate keeps itself alive until that method runs.
    unsafe {
        operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
            window,
            Some(object),
            Some(sel!(printOperationDidRun:success:contextInfo:)),
            std::ptr::null_mut(),
        );
    }
    Ok(receiver)
}

/// The sheet's delegate state.
struct SheetIvars {
    done: Cell<Option<oneshot::Sender<bool>>>,
    /// The delegate itself, until the sheet ends: AppKit does not retain
    /// delegates, and the task awaiting the result may be dropped.
    keep: Cell<Option<Retained<SheetDelegate>>>,
    operation: Retained<NSPrintOperation>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and the class does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GlyphvanePrintSheetDelegate"]
    #[ivars = SheetIvars]
    struct SheetDelegate;

    impl SheetDelegate {
        #[unsafe(method(printOperationDidRun:success:contextInfo:))]
        fn did_run(&self, _: &NSPrintOperation, success: bool, _: *mut c_void) {
            let ivars = self.ivars();
            if success {
                remember_paper(&ivars.operation.printInfo());
            }
            if let Some(done) = ivars.done.take() {
                let _ = done.send(success);
            }
            // Released when the run loop drains its pool, after this call.
            if let Some(this) = ivars.keep.take() {
                let _ = Retained::autorelease_ptr(this);
            }
        }
    }

    unsafe impl NSObjectProtocol for SheetDelegate {}
);

impl SheetDelegate {
    fn new(
        mtm: MainThreadMarker,
        done: oneshot::Sender<bool>,
        operation: Retained<NSPrintOperation>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SheetIvars {
            done: Cell::new(Some(done)),
            keep: Cell::new(None),
            operation,
        });
        // SAFETY: NSObject's designated initialiser.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.ivars().keep.set(Some(this.clone()));
        this
    }
}

/// Keeps the paper and orientation of a finished print for the next one
/// (not its copies or page range).
fn remember_paper(used: &NSPrintInfo) {
    let shared = NSPrintInfo::sharedPrintInfo();
    match used.paperName() {
        Some(name) => shared.setPaperName(Some(&name)),
        None => shared.setPaperSize(used.paperSize()),
    }
    shared.setOrientation(used.orientation());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paper_comes_from_points() {
        // US Letter, landscape: 792 × 612 pt.
        let page = page_from_points(792.0, 612.0, true).expect("a page");
        assert!((page.width_mm - 215.9).abs() < 0.1);
        assert!((page.height_mm - 279.4).abs() < 0.1);
        assert!(page.landscape);
        assert_eq!(page_from_points(0.0, 612.0, false), None);
        assert_eq!(page_from_points(f64::NAN, 612.0, false), None);
    }
}

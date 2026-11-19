// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Print on Windows with operating-system components only (Windows 10 and
//! later), in the same two steps as the desktop portal on Linux:
//!
//! 1. The common print dialog (`PrintDlgEx`), owned by the document window,
//!    picks the printer, copies, page range, paper and orientation. The
//!    printer's paper size is read back from the chosen device mode
//!    (`CreateIC` + `GetDeviceCaps`), so the PDF is made for that paper.
//! 2. `Windows.Data.Pdf` draws each page into a Direct2D command list, and
//!    a Direct2D print control spools the pages to the printer as a vector
//!    (XPS) job, with a print ticket made from the dialog's device mode.
//!
//! Both steps run on threads of their own: the dialog's modal loop sends
//! messages to its owner, so the window's thread must keep pumping them
//! (the same pattern GPUI uses for its own dialogs), and spooling a long
//! document must not stall the window.
//!
//! No PDF viewer, pdfium or print verb of another application is involved:
//! `ShellExecute("print")` depends on the default PDF application, prints
//! without a dialog or opens its own, and reports nothing back.

#![allow(unsafe_code)] // Win32, COM and Direct2D calls; each block says why it is sound.

use std::future::Future;

use futures::FutureExt as _;
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use gpui::{App, Window};
use gv_ui::print::{PageSetup, PrintSetup, Printed, Printer, ViewerPrinter};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Data::Pdf::PdfDocument;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::Foundation::{GlobalFree, HGLOBAL, HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F;
use windows::Win32::Graphics::Direct2D::{
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_PRINT_CONTROL_PROPERTIES, D2D1CreateFactory, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    CreateICW, DEVMODEA, DEVMODEW, DeleteDC, GetDeviceCaps, LOGPIXELSX, LOGPIXELSY, PHYSICALHEIGHT,
    PHYSICALWIDTH,
};
use windows::Win32::Graphics::Imaging::CLSID_WICImagingFactory2;
use windows::Win32::Graphics::Imaging::D2D::IWICImagingFactory2;
use windows::Win32::Graphics::Printing::PrintTicket::{
    PTCloseProvider, PTConvertDevModeToPrintTicket, PTOpenProvider, kPTJobScope,
};
use windows::Win32::Storage::Xps::Printing::{
    IPrintDocumentPackageTarget, IPrintDocumentPackageTargetFactory,
    PrintDocumentPackageTargetFactory,
};
use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoCreateInstance,
    CoInitializeEx, CoUninitialize, STREAM_SEEK_SET,
};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::WinRT::Pdf::{PDF_RENDER_PARAMS, PdfCreateRenderer};
use windows::Win32::UI::Controls::Dialogs::{
    DEVNAMES, PD_NOCURRENTPAGE, PD_NOSELECTION, PD_PAGENUMS, PD_RESULT_PRINT,
    PD_USEDEVMODECOPIESANDCOLLATE, PRINTDLGEXW, PRINTPAGERANGE, PrintDlgExW, START_PAGE_GENERAL,
};
use windows::core::{Interface as _, PCWSTR};

/// The common print dialog and a Direct2D print job.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsPrinter;

/// The page count the dialog accepts in its page range before the PDF
/// exists (its pages depend on the paper chosen in the same dialog).
const MAX_PAGE: u32 = 9999;

/// How many page ranges the dialog may return.
const MAX_RANGES: usize = 16;

/// Between the two steps: what the dialog chose.
struct Prepared {
    printer: String,
    devmode: DevMode,
    /// 1-based inclusive page ranges; empty for every page.
    ranges: Vec<(u32, u32)>,
}

/// A copy of the dialog's `DEVMODEW` (which ends in driver data), aligned
/// for the structure.
struct DevMode {
    /// The whole global block, which may be longer than the device mode.
    words: Vec<u64>,
    /// The device mode's own length: `dmSize + dmDriverExtra`.
    len: usize,
}

impl DevMode {
    /// A copy of the device mode at the start of `bytes`: `None` unless
    /// `bytes` holds a whole `DEVMODEW` and the driver data its header
    /// announces.
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let field = |offset: usize| {
            let at = bytes.get(offset..offset + 2)?;
            Some(usize::from(u16::from_ne_bytes([at[0], at[1]])))
        };
        let size = field(std::mem::offset_of!(DEVMODEW, dmSize))?;
        let len = size + field(std::mem::offset_of!(DEVMODEW, dmDriverExtra))?;
        if bytes.len() < std::mem::size_of::<DEVMODEW>() || size == 0 || len > bytes.len() {
            return None;
        }
        let words = bytes
            .chunks(8)
            .map(|chunk| {
                let mut word = [0u8; 8];
                word[..chunk.len()].copy_from_slice(chunk);
                u64::from_ne_bytes(word)
            })
            .collect();
        Some(Self { words, len })
    }

    fn as_ptr(&self) -> *const DEVMODEW {
        self.words.as_ptr().cast()
    }
}

impl Printer for WindowsPrinter {
    fn prepare(
        &self,
        _: &str,
        window: &Window,
        _: &mut App,
    ) -> LocalBoxFuture<'static, Result<Option<PrintSetup>, String>> {
        let owner = owner_of(window);
        async move {
            let owner = owner.ok_or("the window has no native handle")?;
            let chosen = on_thread("print-dialog", COINIT_APARTMENTTHREADED, move || {
                show_dialog(HWND(owner as *mut _))
            })
            .await?;
            Ok(chosen.map(|(prepared, page)| PrintSetup {
                page,
                state: Box::new(prepared),
            }))
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
        on_thread("print-spool", COINIT_MULTITHREADED, move || {
            spool(&pdf, &title, &prepared).map(|_| Printed::Sent)
        })
        .boxed_local()
    }
}

/// The window's `HWND`, as a number (it crosses to the dialog's thread).
fn owner_of(window: &Window) -> Option<isize> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

/// Runs `work` on a new thread in a COM apartment of kind `apartment`.
fn on_thread<T: Send + 'static>(
    name: &str,
    apartment: COINIT,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> impl Future<Output = Result<T, String>> + 'static {
    let (sender, receiver) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _apartment = Apartment::enter(apartment);
            let _ = sender.send(work());
        });
    async move {
        spawned.map_err(|e| format!("the print thread could not start: {e}"))?;
        receiver
            .await
            .map_err(|_| "the print thread stopped".to_owned())?
    }
}

/// A COM apartment for the current thread, left on drop.
struct Apartment(bool);

impl Apartment {
    fn enter(kind: COINIT) -> Self {
        // SAFETY: COM initialisation of a thread this module spawned; the
        // reserved argument is null. Success (including S_FALSE) is
        // balanced by `CoUninitialize` on drop, after the work, which
        // releases its COM objects before it returns.
        Self(unsafe { CoInitializeEx(None, kind) }.is_ok())
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `CoInitializeEx` above.
            unsafe { CoUninitialize() };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn failed(what: &str) -> impl Fn(windows::core::Error) -> String + '_ {
    move |e| format!("{what}: {}", e.message())
}

/// Shows the common print dialog owned by `owner`: `Ok(None)` when the user
/// cancels.
fn show_dialog(owner: HWND) -> Result<Option<(Prepared, Option<PageSetup>)>, String> {
    let mut ranges = [PRINTPAGERANGE {
        nFromPage: 1,
        nToPage: 1,
    }; MAX_RANGES];
    let mut dialog = PRINTDLGEXW {
        lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
        hwndOwner: owner,
        // Copies and collation go into the device mode, so the print ticket
        // carries them; there is no selection or current page to print.
        Flags: PD_USEDEVMODECOPIESANDCOLLATE | PD_NOSELECTION | PD_NOCURRENTPAGE,
        nPageRanges: 0,
        nMaxPageRanges: MAX_RANGES as u32,
        lpPageRanges: ranges.as_mut_ptr(),
        nMinPage: 1,
        nMaxPage: MAX_PAGE,
        nCopies: 1,
        nStartPage: START_PAGE_GENERAL,
        ..Default::default()
    };
    // SAFETY: `lStructSize` is the structure's size, the owner is a live
    // window (of another thread, whose loop keeps pumping), every other
    // handle and pointer is null, and `lpPageRanges` points to
    // `nMaxPageRanges` entries in `ranges`, which outlives the call. This
    // thread is in a single-threaded apartment, as the dialog requires.
    unsafe { PrintDlgExW(&mut dialog) }.map_err(failed("the print dialog could not be shown"))?;
    let result = if dialog.dwResultAction == PD_RESULT_PRINT {
        read_choice(&dialog, &ranges)
    } else {
        Ok(None)
    };
    // SAFETY: the dialog allocated both blocks (or left them null) for the
    // caller to free; neither is locked or used after this.
    unsafe {
        if !dialog.hDevMode.is_invalid() {
            let _ = GlobalFree(Some(dialog.hDevMode));
        }
        if !dialog.hDevNames.is_invalid() {
            let _ = GlobalFree(Some(dialog.hDevNames));
        }
    }
    result
}

/// The printer, device mode and page ranges of a finished dialog.
fn read_choice(
    dialog: &PRINTDLGEXW,
    ranges: &[PRINTPAGERANGE],
) -> Result<Option<(Prepared, Option<PageSetup>)>, String> {
    if dialog.hDevNames.is_invalid() || dialog.hDevMode.is_invalid() {
        return Err("the print dialog returned no printer".to_owned());
    }
    // SAFETY: the dialog's DEVNAMES block is `GlobalSize` bytes (read as
    // whole UTF-16 units), locked while it is read; GlobalLock's memory is
    // aligned for any type. `device_name` checks the header, the name's
    // offset and its terminator against the block, so nothing past it is
    // read.
    let printer = unsafe {
        let units = GlobalSize(dialog.hDevNames) / 2;
        let names = GlobalLock(dialog.hDevNames) as *const DEVNAMES;
        if names.is_null() {
            return Err("the print dialog returned no printer".to_owned());
        }
        let name = device_name(std::slice::from_raw_parts(names.cast::<u16>(), units));
        let _ = GlobalUnlock(dialog.hDevNames);
        name.ok_or("the print dialog returned no printer")?
    };
    // SAFETY: the device mode block is `GlobalSize` bytes, locked while it
    // is copied (`DevMode` checks what it holds).
    let devmode = unsafe {
        let size = GlobalSize(dialog.hDevMode);
        let bytes = GlobalLock(dialog.hDevMode) as *const u8;
        if bytes.is_null() {
            return Err("the print dialog returned no device mode".to_owned());
        }
        let devmode = DevMode::from_bytes(std::slice::from_raw_parts(bytes, size));
        let _ = GlobalUnlock(dialog.hDevMode);
        devmode.ok_or("the print dialog returned no device mode")?
    };
    let ranges = if dialog.Flags.contains(PD_PAGENUMS) {
        page_ranges(&ranges[..(dialog.nPageRanges as usize).min(ranges.len())])
    } else {
        Vec::new()
    };
    let page = paper(&printer, &devmode);
    Ok(Some((
        Prepared {
            printer,
            devmode,
            ranges,
        },
        page,
    )))
}

/// The device name in a DEVNAMES block (as UTF-16 units): `None` when the
/// block is shorter than its header, or the name's offset or terminator is
/// outside it.
fn device_name(block: &[u16]) -> Option<String> {
    // The header: four offsets or flags of one unit each.
    let header = std::mem::size_of::<DEVNAMES>() / 2;
    let field = std::mem::offset_of!(DEVNAMES, wDeviceOffset) / 2;
    let offset = usize::from(*block.get(..header)?.get(field)?);
    if offset < header {
        return None;
    }
    let rest = block.get(offset..)?;
    let len = rest.iter().position(|&unit| unit == 0)?;
    Some(String::from_utf16_lossy(&rest[..len]))
}

/// The dialog's page ranges as inclusive pairs, in order.
fn page_ranges(ranges: &[PRINTPAGERANGE]) -> Vec<(u32, u32)> {
    ranges
        .iter()
        .map(|r| (r.nFromPage.min(r.nToPage), r.nFromPage.max(r.nToPage)))
        .filter(|&(from, _)| from >= 1)
        .collect()
}

/// The paper the printer uses with `devmode`: the physical page in device
/// units over the device's resolution, which already reflects the paper
/// size, custom sizes and orientation the dialog set.
fn paper(printer: &str, devmode: &DevMode) -> Option<PageSetup> {
    let name = wide(printer);
    // SAFETY: the printer name is NUL-terminated and the device mode is a
    // whole DEVMODEW with its driver data (`DevMode`), both outliving the
    // call; the information context is deleted on the only path that has
    // one.
    unsafe {
        let ic = CreateICW(
            PCWSTR::null(),
            PCWSTR(name.as_ptr()),
            PCWSTR::null(),
            Some(devmode.as_ptr()),
        );
        if ic.is_invalid() {
            return None;
        }
        let caps = [PHYSICALWIDTH, PHYSICALHEIGHT, LOGPIXELSX, LOGPIXELSY]
            .map(|index| GetDeviceCaps(Some(ic), index));
        let _ = DeleteDC(ic);
        let [width, height, dpi_x, dpi_y] = caps;
        page_from_device(width, height, dpi_x, dpi_y)
    }
}

/// A page set-up from the physical page size in device units and the
/// device's dots per inch.
fn page_from_device(width: i32, height: i32, dpi_x: i32, dpi_y: i32) -> Option<PageSetup> {
    if width <= 0 || height <= 0 || dpi_x <= 0 || dpi_y <= 0 {
        return None;
    }
    let width_mm = width as f32 / dpi_x as f32 * 25.4;
    let height_mm = height as f32 / dpi_y as f32 * 25.4;
    Some(PageSetup {
        width_mm: width_mm.min(height_mm),
        height_mm: width_mm.max(height_mm),
        landscape: width_mm > height_mm,
        // The dialog has no margins; the PDF keeps its own.
        margin_mm: None,
    })
}

/// Spools `pdf` to the chosen printer as a vector job named `title`.
fn spool(pdf: &[u8], title: &str, prepared: &Prepared) -> Result<u32, String> {
    let printer = wide(&prepared.printer);
    let devmode_len = u32::try_from(prepared.devmode.len)
        .map_err(|_| "the printer's settings are too large".to_owned())?;
    // SAFETY: Win32 and COM calls on this thread's (multi-threaded)
    // apartment. The wide strings are NUL-terminated and outlive their
    // calls; the device mode pointer is aligned and valid for `len` bytes,
    // the length its header gives (`DevMode`); the provider is closed on
    // every path once opened; the COM objects are released when this
    // function returns, before the apartment is left.
    unsafe {
        // The device mode as a job print ticket (XML) in a memory stream,
        // made first so that no early return leaves the provider open.
        let ticket = CreateStreamOnHGlobal(HGLOBAL::default(), true)
            .map_err(failed("the print ticket could not be made"))?;
        let provider = PTOpenProvider(PCWSTR(printer.as_ptr()), 1)
            .map_err(failed("the printer's settings could not be read"))?;
        // windows-rs types this parameter as DEVMODEA; the function takes
        // the Unicode DEVMODE the print dialog returned.
        let converted = PTConvertDevModeToPrintTicket(
            provider,
            devmode_len,
            prepared.devmode.as_ptr().cast::<DEVMODEA>(),
            kPTJobScope,
            &ticket,
        );
        let _ = PTCloseProvider(provider);
        converted.map_err(failed("the print ticket could not be made"))?;
        ticket
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(failed("the print ticket could not be made"))?;

        // Direct3D, DXGI and Direct2D devices.
        let d3d = d3d_device().map_err(failed("no Direct3D device for printing"))?;
        let dxgi: IDXGIDevice = d3d.cast().map_err(failed("no DXGI device for printing"))?;
        let factory: ID2D1Factory1 = D2D1CreateFactory(
            D2D1_FACTORY_TYPE_SINGLE_THREADED,
            Some(&D2D1_FACTORY_OPTIONS::default()),
        )
        .map_err(failed("no Direct2D factory for printing"))?;
        let device = factory
            .CreateDevice(&dxgi)
            .map_err(failed("no Direct2D device for printing"))?;
        let context = device
            .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
            .map_err(failed("no Direct2D context for printing"))?;

        // The PDF, read from memory by Windows.Data.Pdf.
        let document = load_pdf(pdf).map_err(failed("the PDF could not be read"))?;
        let count = document
            .PageCount()
            .map_err(failed("the PDF could not be read"))?;
        let pages: Vec<u32> = (1..=count)
            .filter(|&page| wanted(&prepared.ranges, page))
            .collect();
        if pages.is_empty() {
            return Err(format!(
                "the pages chosen are not in the document ({count} pages)"
            ));
        }

        // The print job and Direct2D's print control on it.
        let wic: IWICImagingFactory2 =
            CoCreateInstance(&CLSID_WICImagingFactory2, None, CLSCTX_ALL)
                .map_err(failed("no imaging factory for printing"))?;
        let targets: IPrintDocumentPackageTargetFactory =
            CoCreateInstance(&PrintDocumentPackageTargetFactory, None, CLSCTX_ALL)
                .map_err(failed("the print job could not be started"))?;
        let job = wide(title);
        let target = targets
            .CreateDocumentPackageTargetForPrintJob(
                PCWSTR(printer.as_ptr()),
                PCWSTR(job.as_ptr()),
                None,
                &ticket,
            )
            .map_err(failed("the print job could not be started"))?;
        // From here on, any failure cancels the job so no half-built job
        // is left in the queue; a closed print control disarms it.
        let mut cancel = CancelJob(Some(&target));
        let properties = D2D1_PRINT_CONTROL_PROPERTIES {
            // Only for content Direct2D cannot keep as vectors.
            rasterDPI: 300.0,
            ..Default::default()
        };
        let control = device
            .CreatePrintControl(&wic, &target, Some(&properties))
            .map_err(failed("the print job could not be started"))?;

        let renderer = PdfCreateRenderer(&dxgi).map_err(failed("no PDF renderer"))?;
        let params = PDF_RENDER_PARAMS {
            IgnoreHighContrast: true,
            ..Default::default()
        };
        for &number in &pages {
            let page = document
                .GetPage(number - 1)
                .map_err(failed("a page could not be read"))?;
            let list = context
                .CreateCommandList()
                .map_err(failed("a page could not be drawn"))?;
            context.SetTarget(&list);
            context.BeginDraw();
            let drawn = renderer.RenderPageToDeviceContext(&page, &context, Some(&params));
            let ended = context.EndDraw(None, None);
            drawn.map_err(failed("a page could not be drawn"))?;
            ended.map_err(failed("a page could not be drawn"))?;
            list.Close().map_err(failed("a page could not be drawn"))?;
            // The page's size in device-independent pixels (1/96 inch).
            let size = page.Size().map_err(failed("a page could not be read"))?;
            control
                .AddPage(
                    &list,
                    D2D_SIZE_F {
                        width: size.Width,
                        height: size.Height,
                    },
                    None,
                    None,
                    None,
                )
                .map_err(failed("a page could not be printed"))?;
        }
        control
            .Close()
            .map_err(failed("the print job could not be finished"))?;
        cancel.0 = None;
        Ok(pages.len() as u32)
    }
}

/// Cancels a print job when dropped while still armed.
struct CancelJob<'a>(Option<&'a IPrintDocumentPackageTarget>);

impl Drop for CancelJob<'_> {
    fn drop(&mut self) {
        if let Some(target) = self.0 {
            // SAFETY: `target` is a live COM reference borrowed from
            // `spool`, used on the thread that made it.
            if let Err(e) = unsafe { target.Cancel() } {
                tracing::debug!("print: the job could not be cancelled: {e}");
            }
        }
    }
}

/// Whether 1-based `page` is in `ranges` (every page when empty).
fn wanted(ranges: &[(u32, u32)], page: u32) -> bool {
    ranges.is_empty() || ranges.iter().any(|&(from, to)| (from..=to).contains(&page))
}

/// A Direct3D 11 device: the hardware one, else the WARP software
/// rasterizer (virtual machines, remote sessions without a GPU).
fn d3d_device() -> windows::core::Result<ID3D11Device> {
    let make = |driver: D3D_DRIVER_TYPE| {
        let mut device = None;
        // SAFETY: device creation with no adapter, module, feature levels
        // or immediate context; the device out pointer is a local that
        // outlives the call.
        unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .and_then(|()| device.ok_or_else(windows::core::Error::empty))
    };
    make(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| make(D3D_DRIVER_TYPE_WARP))
}

/// `pdf` as a Windows.Data.Pdf document, from an in-memory stream (no
/// temporary file).
fn load_pdf(pdf: &[u8]) -> windows::core::Result<PdfDocument> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream.GetOutputStreamAt(0)?)?;
    writer.WriteBytes(pdf)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    PdfDocument::LoadFromStreamAsync(&stream)?.join()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_paper_comes_from_the_device() {
        // A4 at 600 dpi, landscape: 7016 × 4961 device units.
        let page = page_from_device(7016, 4961, 600, 600).expect("a page");
        assert!((page.width_mm - 210.0).abs() < 0.1);
        assert!((page.height_mm - 297.0).abs() < 0.1);
        assert!(page.landscape);
        assert_eq!(page.margin_mm, None);
        assert_eq!(page_from_device(0, 4961, 600, 600), None);
    }

    #[test]
    fn the_device_name_stays_inside_its_block() {
        let header = std::mem::size_of::<DEVNAMES>() / 2;
        let mut block = vec![0u16; header];
        block[1] = header as u16;
        block.extend("Printer".encode_utf16().chain(Some(0)));
        assert_eq!(device_name(&block).as_deref(), Some("Printer"));
        // No terminator, an offset past the end, a short header.
        assert_eq!(device_name(&block[..block.len() - 1]), None);
        block[1] = 1000;
        assert_eq!(device_name(&block), None);
        block[1] = 0;
        assert_eq!(device_name(&block), None);
        assert_eq!(device_name(&block[..header - 1]), None);
    }

    #[test]
    fn the_device_mode_has_the_length_its_header_gives() {
        let size = std::mem::size_of::<DEVMODEW>();
        let mut bytes = vec![0u8; size + 16];
        let put = |bytes: &mut Vec<u8>, offset: usize, value: usize| {
            let value = u16::try_from(value).expect("fits").to_ne_bytes();
            bytes[offset..offset + 2].copy_from_slice(&value);
        };
        put(&mut bytes, std::mem::offset_of!(DEVMODEW, dmSize), size);
        put(&mut bytes, std::mem::offset_of!(DEVMODEW, dmDriverExtra), 8);
        let devmode = DevMode::from_bytes(&bytes).expect("a device mode");
        assert_eq!(devmode.len, size + 8);
        assert!(devmode.words.len() * 8 >= bytes.len());
        // Driver data past the block, or a block shorter than DEVMODEW.
        put(
            &mut bytes,
            std::mem::offset_of!(DEVMODEW, dmDriverExtra),
            17,
        );
        assert!(DevMode::from_bytes(&bytes).is_none());
        assert!(DevMode::from_bytes(&bytes[..size - 1]).is_none());
    }

    #[test]
    fn page_ranges_are_ordered_and_filter_pages() {
        let ranges = page_ranges(&[
            PRINTPAGERANGE {
                nFromPage: 3,
                nToPage: 2,
            },
            PRINTPAGERANGE {
                nFromPage: 5,
                nToPage: 5,
            },
        ]);
        assert_eq!(ranges, vec![(2, 3), (5, 5)]);
        assert!(wanted(&ranges, 2) && wanted(&ranges, 5));
        assert!(!wanted(&ranges, 1) && !wanted(&ranges, 4));
        assert!(wanted(&[], 7));
    }
}

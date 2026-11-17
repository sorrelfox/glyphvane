// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The export options dialog of *Export as HTML…*, *Export as PDF…* and
//! *Print…* (FR-EXPORT-01, -02, -03, -05).
//!
//! The options are the plan's:
//!
//! - **HTML:** a table of contents; images inside the file (as `data:`
//!   URIs) or copied to an `assets/` folder beside it; *embed remote
//!   images* (off by default: remote images stay references), which
//!   fetches only what the remote-content setting allows ([`RemoteEmbed`],
//!   the setting as it is when the dialog is confirmed). The page is
//!   always the light, print-friendly one.
//! - **PDF:** paper (A4, A5, A3, Letter, Legal), orientation, margins
//!   (5–60 mm by 5), the title at the top of every page after the first,
//!   page numbers, a page break before each level-1 heading, a table of
//!   contents; and a **preview** of the first page, drawn by the helper
//!   process as the options change.
//! - **Print:** the same, without paper, orientation and margins, which
//!   the system's print dialog asks for next.
//!
//! The choices are remembered per kind for the rest of the run (the plan
//! names no setting for them, so nothing is stored on disk).
//!
//! # Behaviour
//!
//! - **Opening.** A view asks for the dialog with an [`ExportRequest`]. It
//!   takes the keyboard (on the dialog itself) and gives it back when it
//!   closes. Tab moves through the controls; Enter and Space work them.
//! - **Enter** on the dialog itself, or its *Export…* / *Print…* button,
//!   confirms: the callback gets the choices, which are remembered.
//! - **Esc**, *Cancel* or a click outside closes it and the callback gets
//!   `None`.
//!
//! Not designed: drawn like the file prompt (SPEC § Popovers:
//! `surface.raised`, `shadow.popover`, radius 10, centred 84 below the
//! top), wider for the PDF preview, with the settings window's rows and
//! controls; a plain primary button on `accent.default`.
//!
//! The dialog is a layer of the window root (GPUI Base's `Root` plugin),
//! like the file prompt: [`init`] registers it before any window opens.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::base::{Root, RootPlugin};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, BoxShadow, Context, Entity, FocusHandle, FontWeight, Global,
    InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, MouseButton,
    ParentElement as _, Render, RenderImage, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, img, point, px,
};
use gv_export::{ImageMode, PageOptions, Paper};

use super::{PageRaster, PdfError, RemoteEmbed};
use crate::a11y::{A11yExt as _, AccessibleExt as _, Role};
use crate::appearance::Appearance;
use crate::metrics::size::switcher as size;
use crate::metrics::spacing::switcher as spacing;
use crate::metrics::{HAIRLINE, radius, text};
use crate::settings_window::controls::{self, OnClick, Segment};
use crate::theme::Theme;

gpui::actions!(
    export_dialog,
    [
        /// Close the dialog without exporting.
        Cancel,
    ]
);

/// The dialog's key context.
pub const KEY_CONTEXT: &str = "ExportDialog";

/// The dialog's width with a preview (not designed: room for the rows and
/// a page).
const WIDE: f32 = 700.0;
/// The padding around the rows (not designed: the file prompt's sides).
const BODY_PAD_X: f32 = 14.0;
const BODY_PAD_Y: f32 = 6.0;
/// The title block's padding (as the file prompt).
const TITLE_PAD_Y: f32 = 10.0;
/// The preview box, in logical pixels: an A4 page 168 wide (not designed).
pub const PREVIEW_WIDTH: f32 = 168.0;
pub const PREVIEW_HEIGHT: f32 = 238.0;
/// The preview is drawn at twice its size, for high-density screens.
const PREVIEW_SCALE: f32 = 2.0;
/// The gap between the rows and the preview, and between the buttons.
const GAP: f32 = 12.0;
/// Button padding (the trust prompt's).
const BUTTON_PAD_X: f32 = 12.0;
/// Margin steps, in millimetres.
const MARGIN_STEP: f32 = 5.0;
const MARGIN_MIN: f32 = 5.0;
const MARGIN_MAX: f32 = 60.0;

/// What is being exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportKind {
    Html,
    Pdf,
    Print,
}

impl ExportKind {
    /// The dialog's title.
    pub fn title(self) -> &'static str {
        match self {
            Self::Html => "Export as HTML",
            Self::Pdf => "Export as PDF",
            Self::Print => "Print",
        }
    }

    /// The confirming button's label.
    pub fn action(self) -> &'static str {
        match self {
            Self::Html | Self::Pdf => "Export…",
            Self::Print => "Print…",
        }
    }
}

/// What the dialog chose.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportChoices {
    /// A table of contents from the headings (HTML and PDF).
    pub toc: bool,
    /// Where the images of an HTML export go.
    pub images: ImageMode,
    /// Fetch and embed remote images (HTML), as the remote-content setting
    /// allows.
    pub remote_images: bool,
    /// The fetcher and setting for `remote_images`, taken when the dialog
    /// is confirmed; without it remote images stay references.
    pub remote: Option<RemoteEmbed>,
    /// The page of a PDF; for Print, only the header, page numbers and
    /// page breaks are used (the print dialog gives the paper).
    pub page: PageOptions,
}

impl Default for ExportChoices {
    fn default() -> Self {
        Self {
            toc: false,
            images: ImageMode::Embed,
            remote_images: false,
            remote: None,
            page: PageOptions::default(),
        }
    }
}

impl ExportChoices {
    /// Sets the HTML options of `opts` from these choices.
    pub fn apply(&self, opts: &mut gv_export::ExportOptions) {
        opts.toc = self.toc;
        opts.images = self.images;
        opts.remote = if self.remote_images {
            self.remote
                .clone()
                .map(|r| Arc::new(r) as Arc<dyn gv_export::RemoteImages>)
        } else {
            None
        };
    }

    /// The page for printing on a page set-up from the print dialog:
    /// `page`'s paper, orientation and margins with these choices' header,
    /// page numbers and page breaks.
    pub fn print_page(&self, mut page: PageOptions) -> PageOptions {
        page.header = self.page.header;
        page.page_numbers = self.page.page_numbers;
        page.break_before_h1 = self.page.break_before_h1;
        page
    }
}

/// The choices made in this run, per kind.
#[derive(Default)]
struct Remembered(std::collections::HashMap<ExportKind, ExportChoices>);

impl Global for Remembered {}

/// The choices last confirmed for `kind` in this run, else the defaults.
pub fn remembered(kind: ExportKind, cx: &App) -> ExportChoices {
    cx.try_global::<Remembered>()
        .and_then(|r| r.0.get(&kind).cloned())
        .unwrap_or_default()
}

/// Draws the first page of the PDF for some choices, fitting a box in
/// pixels. Called off the UI thread; stops early once the flag is set.
pub type PreviewFn = Arc<
    dyn Fn(&ExportChoices, (u32, u32), &AtomicBool) -> Result<PageRaster, PdfError> + Send + Sync,
>;

/// The callback of a request: the choices, or `None` when cancelled.
pub type ExportCallback = Rc<dyn Fn(Option<ExportChoices>, &mut Window, &mut App)>;

/// A dialog to open.
#[derive(Clone)]
pub struct ExportRequest {
    pub kind: ExportKind,
    /// The choices shown at first ([`remembered`] for the kind).
    pub choices: ExportChoices,
    /// Draws the preview (PDF only).
    pub preview: Option<PreviewFn>,
    pub on_done: ExportCallback,
}

impl std::fmt::Debug for ExportRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportRequest")
            .field("kind", &self.kind)
            .field("choices", &self.choices)
            .field("preview", &self.preview.is_some())
            .finish_non_exhaustive()
    }
}

/// The dialog's keys.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("escape", Cancel, Some(KEY_CONTEXT))]
}

/// Binds the dialog's keys and adds it to the window root. Called by
/// [`crate::init`], before any window opens.
pub fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
    Root::register_plugin::<ExportDialog>(cx, ExportDialog::new);
}

/// The export dialog of `window`, if its root has one.
pub fn dialog_of(window: &Window, cx: &App) -> Option<Entity<ExportDialog>> {
    window
        .root::<Root>()
        .flatten()?
        .read(cx)
        .plugin::<ExportDialog>()
}

/// Opens the dialog of `window` for `request` (cancelling one that is
/// open). Returns whether the window has a dialog.
pub fn open(request: ExportRequest, window: &mut Window, cx: &mut App) -> bool {
    let Some(dialog) = dialog_of(window, cx) else {
        return false;
    };
    dialog.update(cx, |dialog, cx| dialog.open(request, window, cx));
    true
}

/// The state of the preview.
#[derive(Clone)]
pub enum Preview {
    /// Being drawn.
    Loading,
    /// The first page, and the number of pages.
    Ready {
        image: Arc<RenderImage>,
        width: u32,
        height: u32,
        pages: u32,
    },
    /// It could not be drawn, and why.
    Failed(SharedString),
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loading => write!(f, "Loading"),
            Self::Ready {
                width,
                height,
                pages,
                ..
            } => write!(f, "Ready({width}x{height}, {pages} pages)"),
            Self::Failed(reason) => write!(f, "Failed({reason})"),
        }
    }
}

/// The export dialog of one window.
pub struct ExportDialog {
    focus: FocusHandle,
    request: Option<ExportRequest>,
    choices: ExportChoices,
    /// What had the keyboard before the dialog took it.
    restore: Option<FocusHandle>,
    preview: Option<Preview>,
    /// Stops the preview being drawn (set on every change and on close).
    preview_cancel: Arc<AtomicBool>,
    _preview_task: Option<Task<()>>,
}

impl std::fmt::Debug for ExportDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportDialog")
            .field("request", &self.request)
            .field("choices", &self.choices)
            .field("preview", &self.preview)
            .finish_non_exhaustive()
    }
}

impl RootPlugin for ExportDialog {}

impl ExportDialog {
    pub fn new(_: &mut Window, cx: &mut Context<'_, Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            request: None,
            choices: ExportChoices::default(),
            restore: None,
            preview: None,
            preview_cancel: Arc::new(AtomicBool::new(false)),
            _preview_task: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.request.is_some()
    }

    /// The kind of the open dialog.
    pub fn kind(&self) -> Option<ExportKind> {
        self.request.as_ref().map(|r| r.kind)
    }

    /// The choices as they stand.
    pub fn choices(&self) -> &ExportChoices {
        &self.choices
    }

    /// The preview, for PDF.
    pub fn preview(&self) -> Option<&Preview> {
        self.preview.as_ref()
    }

    /// Opens the dialog for `request` and takes the keyboard.
    pub fn open(
        &mut self,
        request: ExportRequest,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.request.is_some() {
            self.cancel(window, cx);
        }
        self.restore = window.focused(cx);
        self.choices = request.choices.clone();
        self.request = Some(request);
        self.focus.focus(window, cx);
        self.refresh_preview(cx);
        cx.notify();
    }

    /// Changes the choices, as the controls do, and redraws the preview.
    pub fn update_choices(
        &mut self,
        change: impl FnOnce(&mut ExportChoices),
        cx: &mut Context<'_, Self>,
    ) {
        let before = self.choices.clone();
        change(&mut self.choices);
        if self.choices != before {
            self.refresh_preview(cx);
        }
        cx.notify();
    }

    /// Starts drawing the preview for the current choices (PDF only); the
    /// one being drawn is cancelled.
    fn refresh_preview(&mut self, cx: &mut Context<'_, Self>) {
        self.preview_cancel.store(true, Ordering::Relaxed);
        let Some(preview) = self
            .request
            .as_ref()
            .filter(|r| r.kind == ExportKind::Pdf)
            .and_then(|r| r.preview.clone())
        else {
            self.preview = None;
            self._preview_task = None;
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.preview_cancel = cancel.clone();
        self.preview = Some(Preview::Loading);
        let choices = self.choices.clone();
        let box_px = (
            (PREVIEW_WIDTH * PREVIEW_SCALE) as u32,
            (PREVIEW_HEIGHT * PREVIEW_SCALE) as u32,
        );
        let flag = cancel.clone();
        let drawn = cx.background_spawn(async move {
            preview(&choices, box_px, &flag).map(|raster| {
                let pages = raster.pages;
                (raster_image(raster), pages)
            })
        });
        self._preview_task = Some(cx.spawn(async move |this, cx| {
            let drawn = drawn.await;
            this.update(cx, |this, cx| {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                this.preview = Some(match drawn {
                    Ok((Some((image, width, height)), pages)) => Preview::Ready {
                        image,
                        width,
                        height,
                        pages,
                    },
                    Ok((None, _)) => Preview::Failed("the page could not be drawn".into()),
                    Err(PdfError::Cancelled) => return,
                    Err(PdfError::Failed(reason)) => Preview::Failed(reason.into()),
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// Closes the dialog and gives the keyboard back.
    fn close(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> Option<ExportRequest> {
        let request = self.request.take()?;
        self.preview_cancel.store(true, Ordering::Relaxed);
        self.preview = None;
        self._preview_task = None;
        match self.restore.take() {
            Some(focus) => focus.focus(window, cx),
            None => window.blur(cx),
        }
        cx.notify();
        Some(request)
    }

    /// Closes without exporting.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if let Some(request) = self.close(window, cx) {
            (request.on_done)(None, window, cx);
        }
    }

    /// Remembers the choices, closes and hands them over (with the
    /// remote-content fetcher and setting of this moment when remote
    /// images are embedded).
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let mut choices = self.choices.clone();
        choices.remote = None;
        if let Some(request) = self.close(window, cx) {
            cx.default_global::<Remembered>()
                .0
                .insert(request.kind, choices.clone());
            if choices.remote_images && request.kind == ExportKind::Html {
                choices.remote = Some(RemoteEmbed::of_app(cx));
            }
            (request.on_done)(Some(choices), window, cx);
        }
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let m = &event.keystroke.modifiers;
        // Enter on a control works the control; on the dialog it confirms.
        if event.keystroke.key == "enter"
            && !(m.control || m.alt || m.platform || m.shift || m.function)
            && self.focus.is_focused(window)
        {
            cx.stop_propagation();
            self.confirm(window, cx);
        }
    }
}

/// The GPUI image of a drawn page, and its size.
pub(crate) fn raster_image(raster: PageRaster) -> Option<(Arc<RenderImage>, u32, u32)> {
    let PageRaster {
        width,
        height,
        mut rgba,
        ..
    } = raster;
    // GPUI wants BGRA.
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
    let frame = image::Frame::new(buffer);
    Some((Arc::new(RenderImage::new(vec![frame])), width, height))
}

/// `shadow.popover`, the application's only shadow.
fn popover_shadow(theme: &Theme) -> Vec<BoxShadow> {
    theme
        .shadow
        .popover
        .iter()
        .map(|l| BoxShadow {
            color: l.color.into(),
            offset: point(px(l.x), px(l.y)),
            blur_radius: px(l.blur),
            spread_radius: px(l.spread),
            inset: false,
        })
        .collect()
}

/// A dialog button: secondary (the trust prompt's) or primary
/// (`accent.default` with `accent.on` text; not designed).
fn button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    on_click: OnClick,
    theme: &Theme,
) -> AnyElement {
    let (hover, press) = (theme.state.hover, theme.state.press);
    div()
        .id(id)
        .a11y_button(label)
        .keyboard_control(theme)
        .debug_selector(move || id.to_owned())
        .flex_none()
        .h(px(crate::metrics::size::CONTROL))
        .px(px(BUTTON_PAD_X))
        .flex()
        .items_center()
        .rounded(px(radius::CONTROL))
        .border_1()
        .font_weight(FontWeight(text::weight::ACTIVE))
        .map(|d| {
            if primary {
                d.border_color(theme.accent.default)
                    .bg(theme.accent.default)
                    .text_color(theme.accent.on)
            } else {
                d.border_color(theme.border.strong)
                    .bg(theme.surface.raised)
                    .text_color(theme.text.primary)
                    .hover(move |s| s.bg(hover))
                    .active(move |s| s.bg(press))
            }
        })
        .on_click(on_click)
        .child(label)
        .accessible()
        .into_any_element()
}

impl ExportDialog {
    /// A click handler that changes the choices.
    fn change(
        cx: &mut Context<'_, Self>,
        change: impl Fn(&mut ExportChoices) + 'static,
    ) -> OnClick {
        Box::new(cx.listener(move |this, _, _, cx| this.update_choices(&change, cx)))
    }

    #[allow(clippy::too_many_arguments)]
    fn switch_row(
        &self,
        id: &'static str,
        label: &'static str,
        description: Option<&'static str>,
        on: bool,
        toggle: impl Fn(&mut ExportChoices) + 'static,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let switch = controls::switch(
            SharedString::from(format!("{id}-switch")),
            on,
            true,
            Self::change(cx, toggle),
            theme,
        )
        .aria_label(label)
        .debug_selector(move || format!("{id}-switch"))
        .accessible();
        controls::row(
            id,
            label,
            description.map(SharedString::from),
            None,
            vec![switch.into_any_element()],
            theme,
        )
        .accessible()
        .into_any_element()
    }

    fn rows(&self, kind: ExportKind, theme: &Theme, cx: &mut Context<'_, Self>) -> Vec<AnyElement> {
        let c = &self.choices;
        let mut rows = Vec::new();
        if kind == ExportKind::Pdf {
            let papers: Vec<Segment> =
                [Paper::A4, Paper::A5, Paper::A3, Paper::Letter, Paper::Legal]
                    .into_iter()
                    .map(|paper| Segment {
                        id: format!("export-paper-{}", paper.name()).into(),
                        label: paper_label(paper).into(),
                        active: c.page.paper == paper,
                        on_click: Some(Self::change(cx, move |c| c.page.paper = paper)),
                    })
                    .collect();
            rows.push(
                controls::row(
                    "export-paper",
                    "Paper",
                    None,
                    None,
                    vec![controls::segmented(papers, true, theme).into_any_element()],
                    theme,
                )
                .accessible()
                .into_any_element(),
            );
            let orientation = vec![
                Segment {
                    id: "export-portrait".into(),
                    label: "Portrait".into(),
                    active: !c.page.landscape,
                    on_click: Some(Self::change(cx, |c| c.page.landscape = false)),
                },
                Segment {
                    id: "export-landscape".into(),
                    label: "Landscape".into(),
                    active: c.page.landscape,
                    on_click: Some(Self::change(cx, |c| c.page.landscape = true)),
                },
            ];
            rows.push(
                controls::row(
                    "export-orientation",
                    "Orientation",
                    None,
                    None,
                    vec![controls::segmented(orientation, true, theme).into_any_element()],
                    theme,
                )
                .accessible()
                .into_any_element(),
            );
            let margin = margin_step(c.page.margin_mm);
            let minus = (margin > MARGIN_MIN).then(|| {
                Self::change(cx, |c| {
                    c.page.margin_mm =
                        (margin_step(c.page.margin_mm) - MARGIN_STEP).max(MARGIN_MIN);
                })
            });
            let plus = (margin < MARGIN_MAX).then(|| {
                Self::change(cx, |c| {
                    c.page.margin_mm =
                        (margin_step(c.page.margin_mm) + MARGIN_STEP).min(MARGIN_MAX);
                })
            });
            rows.push(
                controls::row(
                    "export-margins",
                    "Margins",
                    Some("On every side.".into()),
                    None,
                    vec![
                        controls::stepper(
                            "export-margin",
                            format!("{margin:.0} mm"),
                            minus,
                            plus,
                            theme,
                        )
                        .into_any_element(),
                    ],
                    theme,
                )
                .accessible()
                .into_any_element(),
            );
        }
        if kind == ExportKind::Html {
            let images = vec![
                Segment {
                    id: "export-images-embed".into(),
                    label: "In the file".into(),
                    active: c.images == ImageMode::Embed,
                    on_click: Some(Self::change(cx, |c| c.images = ImageMode::Embed)),
                },
                Segment {
                    id: "export-images-assets".into(),
                    label: "In a folder".into(),
                    active: c.images == ImageMode::Assets,
                    on_click: Some(Self::change(cx, |c| c.images = ImageMode::Assets)),
                },
            ];
            rows.push(
                controls::row(
                    "export-images",
                    "Images",
                    Some("Inside the page, or copied to an assets folder beside it.".into()),
                    None,
                    vec![controls::segmented(images, true, theme).into_any_element()],
                    theme,
                )
                .accessible()
                .into_any_element(),
            );
            rows.push(self.switch_row(
                "export-remote-images",
                "Embed remote images",
                Some("Only what the remote-content setting allows; others stay links."),
                c.remote_images,
                |c| c.remote_images = !c.remote_images,
                theme,
                cx,
            ));
        }
        if kind != ExportKind::Html {
            rows.push(self.switch_row(
                "export-header",
                "Title on every page",
                Some("At the top of every page after the first."),
                c.page.header,
                |c| c.page.header = !c.page.header,
                theme,
                cx,
            ));
            rows.push(self.switch_row(
                "export-page-numbers",
                "Page numbers",
                None,
                c.page.page_numbers,
                |c| c.page.page_numbers = !c.page.page_numbers,
                theme,
                cx,
            ));
            rows.push(self.switch_row(
                "export-page-breaks",
                "New page for each top heading",
                None,
                c.page.break_before_h1,
                |c| c.page.break_before_h1 = !c.page.break_before_h1,
                theme,
                cx,
            ));
        }
        rows.push(self.switch_row(
            "export-toc",
            "Table of contents",
            Some("From the headings, at the start."),
            c.toc,
            |c| c.toc = !c.toc,
            theme,
            cx,
        ));
        rows
    }

    fn render_preview(&self, theme: &Theme) -> AnyElement {
        let frame = div()
            .id("export-preview")
            .debug_selector(|| "export-preview".into())
            .flex_none()
            .w(px(PREVIEW_WIDTH))
            .h(px(PREVIEW_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .border(px(HAIRLINE))
            .border_color(theme.border.default)
            .bg(theme.surface.sunken)
            .text_size(px(text::UI_SMALL.size))
            .line_height(px(text::UI_SMALL.line))
            .text_color(theme.text.secondary);
        let (frame, caption): (_, SharedString) = match &self.preview {
            Some(Preview::Ready {
                image,
                width,
                height,
                pages,
            }) => (
                frame.child(
                    div()
                        .id("export-preview-page")
                        .debug_selector(|| "export-preview-page".into())
                        .role(Role::Image)
                        .aria_label("The first page")
                        .bg(theme.surface.page)
                        .child(
                            img(image.clone())
                                .w(px(*width as f32 / PREVIEW_SCALE))
                                .h(px(*height as f32 / PREVIEW_SCALE)),
                        ),
                ),
                if *pages == 1 {
                    "1 page".into()
                } else {
                    format!("{pages} pages").into()
                },
            ),
            Some(Preview::Failed(reason)) => (
                frame
                    .p(px(GAP / 2.0))
                    .child(format!("No preview: {reason}.")),
                "".into(),
            ),
            Some(Preview::Loading) | None => (frame.child("Preparing preview…"), "".into()),
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(GAP / 2.0))
            .child(frame)
            .child(
                div()
                    .debug_selector(|| "export-preview-caption".into())
                    .text_size(px(text::UI_SMALL.size))
                    .text_color(theme.text.secondary)
                    .child(caption),
            )
            .into_any_element()
    }
}

/// How the dialog names a paper size.
fn paper_label(paper: Paper) -> &'static str {
    match paper {
        Paper::A4 => "A4",
        Paper::A5 => "A5",
        Paper::A3 => "A3",
        Paper::Letter => "Letter",
        Paper::Legal => "Legal",
    }
}

/// `mm` on the stepper's 5 mm grid, within its range.
fn margin_step(mm: f32) -> f32 {
    if !mm.is_finite() {
        return PageOptions::default().margin_mm;
    }
    ((mm / MARGIN_STEP).round() * MARGIN_STEP).clamp(MARGIN_MIN, MARGIN_MAX)
}

impl Render for ExportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let Some(kind) = self.kind() else {
            return div().into_any_element();
        };
        let theme = Appearance::theme(cx);
        let viewport = window.viewport_size();
        let wanted = if kind == ExportKind::Pdf {
            WIDE
        } else {
            size::WIDTH
        };
        let width = px(wanted)
            .min(viewport.width - px(2.0 * size::SIDE_MARGIN))
            .max(px(0.0));
        let title = kind.title();
        let header = div()
            .flex_none()
            .px(px(spacing::INPUT_PAD_X))
            .py(px(TITLE_PAD_Y))
            .child(
                div()
                    .debug_selector(|| "export-title".into())
                    .font_weight(FontWeight(text::weight::STRONG))
                    .text_size(px(text::UI_TITLE.size))
                    .child(title),
            )
            .when(kind == ExportKind::Print, |d| {
                d.child(
                    div()
                        .text_color(theme.text.secondary)
                        .child("The paper and margins are chosen in the print dialog next."),
                )
            });
        let rows = self.rows(kind, theme, cx);
        let body = div()
            .flex_none()
            .flex()
            .gap(px(GAP))
            .px(px(BODY_PAD_X))
            .py(px(BODY_PAD_Y))
            .child(div().flex_1().min_w_0().flex().flex_col().children(rows))
            .when(kind == ExportKind::Pdf, |d| {
                d.child(self.render_preview(theme))
            });
        let cancel = button(
            "export-cancel",
            "Cancel",
            false,
            Box::new(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
            theme,
        );
        let confirm = button(
            "export-confirm",
            kind.action(),
            true,
            Box::new(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
            theme,
        );
        let footer = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(GAP / 2.0))
            .pt(px(spacing::FOOTER_PAD_TOP))
            .pb(px(spacing::FOOTER_PAD_BOTTOM))
            .px(px(spacing::FOOTER_PAD_X))
            .border_t(px(HAIRLINE))
            .border_color(theme.border.default)
            .child(cancel)
            .child(confirm);
        let panel = div()
            .id("export-dialog")
            .a11y_group(Role::Dialog, title)
            .debug_selector(|| "export-dialog".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .top(px(size::TOP))
            .left((viewport.width - width) / 2.0)
            .w(width)
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(radius::SWITCHER))
            .bg(theme.surface.raised)
            .border(px(HAIRLINE))
            .border_color(theme.border.strong)
            .shadow(popover_shadow(theme))
            .text_color(theme.text.primary)
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(body)
            .child(footer)
            .accessible()
            .modal();
        div()
            .id("export-dialog-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .font_family(Appearance::interface_font(cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.cancel(window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.cancel(window, cx);
                }),
            )
            .child(panel)
            .into_any_element()
    }
}

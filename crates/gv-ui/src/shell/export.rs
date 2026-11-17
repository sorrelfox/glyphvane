// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! *Export as HTML…*, *Export as PDF…*, *Print…*, *Open in browser* and
//! *Copy as rich text* (FR-EXPORT-01 to -04, -06; see [`crate::export`]
//! and [`crate::print`]).
//!
//! - **Export as HTML…** asks for the options first (the export dialog:
//!   table of contents, where images go), then where to save (the
//!   platform's save dialog, in the document's folder with its name and
//!   `.html`), then exports the tab's text off the UI thread: the editor's
//!   text when the tab has one (saved or not), else the file's. The file is
//!   written whole.
//! - **Export as PDF…** asks for the options (paper, orientation, margins,
//!   header, page numbers, page breaks, table of contents, with a preview
//!   of the first page), then where to save (`name.pdf`), then generates
//!   the document and has the PDF engine (the helper process) compile it,
//!   off the UI thread. A neutral infobar shows meanwhile; its Cancel
//!   stops the engine.
//! - **Print…** asks for the options (header, page numbers, page breaks,
//!   table of contents), then the platform's print dialog (where it asks
//!   before the PDF exists), makes the PDF for the paper chosen, then
//!   hands it over. Without a native print path the PDF opens in the
//!   system's viewer.
//! - **Open in browser** exports to a new private temporary folder and
//!   hands the file to the system's default app for HTML.
//! - **Copy as rich text** copies the selection of the view with the
//!   focus (the Reading view's, or the editor's) as HTML with a plain-text
//!   alternative.
//!
//! Failures show in the tab's infobar (not designed).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use std::rc::Rc;

use gpui::{
    App, AppContext as _, ClipboardItem, Context, EntityId, Focusable as _, Global, WeakEntity,
    Window,
};
use gv_export::PageOptions;
use gv_profile::CompiledProfile;
use gv_vault::PathIndex;

use super::{ViewMode, Workspace};
use crate::export::{self, ExportChoices, ExportKind, ExportRequest, PdfError, PreviewFn};
use crate::files::notice::{Notice, NoticeAction, NoticeButton, NoticeKind, Topic};
use crate::files::save_as::{self, SavePrompt};
use crate::platform;
use crate::print::{self, PageSetup, Printed};

/// The PDF job running in each window, for its infobar's Cancel.
#[derive(Default)]
struct PdfJobs(HashMap<EntityId, Arc<AtomicBool>>);

impl Global for PdfJobs {}

/// A tab notice made here (not designed).
fn notice(kind: NoticeKind, title: &str, detail: Option<String>) -> Notice {
    Notice {
        topic: Topic::NotOpened,
        kind,
        title: title.to_owned().into(),
        detail: detail.map(Into::into),
        buttons: Vec::new(),
    }
}

/// The infobar while a PDF is made: its Cancel (and its dismiss button)
/// stop the job.
fn progress(title: &str) -> Notice {
    let mut n = notice(NoticeKind::Neutral, title, None);
    n.buttons.push(NoticeButton {
        action: NoticeAction::Cancel,
        label: "Cancel".into(),
        disabled: None,
    });
    n
}

#[cfg(test)]
pub(super) fn progress_for_tests(title: &str) -> Notice {
    progress(title)
}

/// Whether `n` is a PDF job's infobar.
pub(super) fn is_progress(n: &Notice) -> bool {
    n.kind == NoticeKind::Neutral && n.topic == Topic::NotOpened && n.offers(&NoticeAction::Cancel)
}

fn pdf_failed(reason: &str) -> Notice {
    notice(
        NoticeKind::Error,
        "Not exported.",
        Some(format!("The PDF could not be made: {reason}.")),
    )
}

fn print_failed(reason: &str) -> Notice {
    notice(
        NoticeKind::Error,
        "Not printed.",
        Some(format!("{}.", capitalized(reason))),
    )
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// The PDF engine is not installed (a build without the helper).
const NO_ENGINE: &str = "the PDF engine is not available";

/// The page of a PDF for the page set-up a print dialog chose.
fn print_page(setup: Option<PageSetup>) -> PageOptions {
    let mut page = PageOptions::default();
    if let Some(setup) = setup {
        page.size_mm = Some((setup.width_mm, setup.height_mm));
        page.landscape = setup.landscape;
        if let Some(m) = setup.margin_mm {
            page.margin_mm = m.max(page.margin_mm.min(10.0));
        }
    }
    page
}

/// What an export works from.
#[derive(Clone)]
struct Source {
    text: String,
    path: Option<PathBuf>,
    profile: Arc<CompiledProfile>,
    index: Option<PathIndex>,
    root: Option<PathBuf>,
}

/// Asks `kind`'s options in the window's export dialog, then calls
/// `then` on the workspace with them; without a dialog (a window without
/// the root layer) the remembered choices are used at once.
fn ask_options(
    kind: ExportKind,
    preview: Option<PreviewFn>,
    window: &mut Window,
    cx: &mut Context<'_, Workspace>,
    then: impl Fn(&mut Workspace, ExportChoices, &mut Window, &mut Context<'_, Workspace>) + 'static,
) {
    let choices = export::dialog::remembered(kind, cx);
    let this: WeakEntity<Workspace> = cx.weak_entity();
    let then = Rc::new(then);
    let on_done = {
        let then = then.clone();
        let this = this.clone();
        Rc::new(
            move |choices: Option<ExportChoices>, window: &mut Window, cx: &mut App| {
                if let Some(choices) = choices {
                    let then = then.clone();
                    this.update(cx, |ws, cx| then(ws, choices, window, cx)).ok();
                }
            },
        )
    };
    let request = ExportRequest {
        kind,
        choices: choices.clone(),
        preview,
        on_done,
    };
    // The dialog is opened once this command's update has finished.
    window.defer(cx, move |window, cx| {
        if !export::dialog::open(request, window, cx) {
            this.update(cx, |ws, cx| then(ws, choices, window, cx)).ok();
        }
    });
}

impl Workspace {
    /// The document of tab `index`: its editor's text, else its file's.
    fn export_source(
        &mut self,
        index: usize,
        cx: &mut Context<'_, Self>,
    ) -> Result<Source, String> {
        let tab = self.tabs.get(index).ok_or("no document")?;
        let path = tab
            .path
            .clone()
            .map(|p| std::path::absolute(&p).unwrap_or(p));
        let edited = tab
            .editor
            .as_ref()
            .and_then(|e| e.read(cx).editor().map(|e| e.text().to_string()));
        let text = match (edited, &path) {
            (Some(text), _) => text,
            (None, Some(path)) => {
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                gv_text::decode(&bytes).map_err(|e| e.to_string())?.text
            }
            (None, None) => return Err("the tab has no document".to_owned()),
        };
        let profile = self.tab_profile(index, cx).compiled;
        let vault = self.vault.as_ref().map(|v| v.read(cx));
        let index = vault.map(|v| v.snapshot());
        let root = vault.map(|v| v.root().to_path_buf());
        Ok(Source {
            text,
            path,
            profile,
            index,
            root,
        })
    }

    /// `file.export_html`.
    pub(super) fn export_html_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let tab = self.command_tab(cx);
        let source = match self.export_source(tab, cx) {
            Ok(source) => source,
            Err(reason) => return self.ask(Notice::export_failed(&reason), cx),
        };
        ask_options(
            ExportKind::Html,
            None,
            window,
            cx,
            move |ws, choices, _, cx| {
                ws.export_html_with(source.clone(), choices, cx);
            },
        );
    }

    /// Asks where to save, then exports `source` as HTML with `choices`.
    fn export_html_with(
        &mut self,
        source: Source,
        choices: ExportChoices,
        cx: &mut Context<'_, Self>,
    ) {
        let directory = source
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| source.root.clone())
            .or_else(gv_vault::root::home_dir)
            .unwrap_or_default();
        let prompt = SavePrompt {
            directory,
            suggested_name: export::suggested_name(source.path.as_deref()),
        };
        let answer = save_as::dialog(cx).ask(&prompt, cx);
        cx.spawn(async move |this, cx| {
            let target = match answer.await {
                Ok(Some(target)) => export::with_html_extension(target),
                Ok(None) => return,
                Err(reason) => {
                    this.update(cx, |ws, cx| ws.ask(Notice::save_dialog_failed(&reason), cx))
                        .ok();
                    return;
                }
            };
            let written = cx
                .background_spawn(async move {
                    let export = export::export_text_with(
                        &source.text,
                        source.path.as_deref(),
                        source.root.as_deref(),
                        &source.profile,
                        source.index.as_ref(),
                        &choices,
                    );
                    export::write_export(&target, &export)
                        .map(|()| export.notes.len())
                        .map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |ws, cx| match written {
                Ok(0) => {}
                Ok(skipped) => ws.ask(Notice::export_images_skipped(skipped), cx),
                Err(reason) => ws.ask(Notice::export_failed(&reason), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Starts a PDF job in this window: shows its infobar and returns its
    /// cancel flag; `None` when one runs already.
    pub(super) fn start_pdf_job(
        &mut self,
        title: &str,
        cx: &mut Context<'_, Self>,
    ) -> Option<Arc<AtomicBool>> {
        let id = cx.entity_id();
        let jobs = cx.default_global::<PdfJobs>();
        if jobs.0.contains_key(&id) {
            self.ask(
                notice(
                    NoticeKind::Information,
                    "A PDF is being made.",
                    Some("Wait for it, or cancel it first.".to_owned()),
                ),
                cx,
            );
            return None;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.0.insert(id, cancel.clone());
        self.ask(progress(title), cx);
        Some(cancel)
    }

    /// Ends this window's PDF job: removes its infobar.
    pub(super) fn end_pdf_job(&mut self, cx: &mut Context<'_, Self>) {
        let id = cx.entity_id();
        cx.default_global::<PdfJobs>().0.remove(&id);
        for tab in &mut self.tabs {
            if tab.notice.as_ref().is_some_and(is_progress) {
                tab.notice = None;
            }
        }
        cx.notify();
    }

    /// Cancels this window's PDF job (its infobar was answered).
    pub(super) fn cancel_pdf_job(&mut self, cx: &mut Context<'_, Self>) {
        if let Some(cancel) = cx
            .try_global::<PdfJobs>()
            .and_then(|jobs| jobs.0.get(&cx.entity_id()))
        {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// `file.export_pdf`.
    pub(super) fn export_pdf_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let tab = self.command_tab(cx);
        let source = match self.export_source(tab, cx) {
            Ok(source) => source,
            Err(reason) => return self.ask(pdf_failed(&reason), cx),
        };
        let Some(engine) = export::pdf_engine(cx) else {
            return self.ask(pdf_failed(NO_ENGINE), cx);
        };
        let preview = engine.draws_pages().then(|| {
            let (source, engine) = (source.clone(), engine.clone());
            let preview: PreviewFn = Arc::new(move |choices, size, cancel| {
                export::preview_pdf(
                    &source.text,
                    source.path.as_deref(),
                    source.root.as_deref(),
                    &source.profile,
                    source.index.as_ref(),
                    choices,
                    size,
                    &*engine,
                    cancel,
                )
            });
            preview
        });
        ask_options(
            ExportKind::Pdf,
            preview,
            window,
            cx,
            move |ws, choices, _, cx| {
                ws.export_pdf_with(source.clone(), engine.clone(), choices, cx);
            },
        );
    }

    /// Asks where to save, then exports `source` as a PDF with `choices`.
    fn export_pdf_with(
        &mut self,
        source: Source,
        engine: Arc<dyn export::PdfEngine>,
        choices: ExportChoices,
        cx: &mut Context<'_, Self>,
    ) {
        let directory = source
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| source.root.clone())
            .or_else(gv_vault::root::home_dir)
            .unwrap_or_default();
        let prompt = SavePrompt {
            directory,
            suggested_name: export::suggested_name_with(source.path.as_deref(), "pdf"),
        };
        let answer = save_as::dialog(cx).ask(&prompt, cx);
        cx.spawn(async move |this, cx| {
            let target = match answer.await {
                Ok(Some(target)) => export::with_extension(target, "pdf"),
                Ok(None) => return,
                Err(reason) => {
                    this.update(cx, |ws, cx| ws.ask(Notice::save_dialog_failed(&reason), cx))
                        .ok();
                    return;
                }
            };
            let Ok(Some(cancel)) = this.update(cx, |ws, cx| ws.start_pdf_job("Exporting PDF…", cx))
            else {
                return;
            };
            let flag = cancel.clone();
            let written = cx
                .background_spawn(async move {
                    let pdf = export::export_pdf_with(
                        &source.text,
                        source.path.as_deref(),
                        source.root.as_deref(),
                        &source.profile,
                        source.index.as_ref(),
                        &choices,
                        &*engine,
                        &flag,
                    )?;
                    if flag.load(Ordering::Relaxed) {
                        return Err(PdfError::Cancelled);
                    }
                    export::write_atomically(&target, &pdf.bytes)
                        .map_err(|e| PdfError::Failed(e.to_string()))?;
                    Ok(pdf.notes.len())
                })
                .await;
            this.update(cx, |ws, cx| {
                ws.end_pdf_job(cx);
                match written {
                    Ok(0) | Err(PdfError::Cancelled) => {}
                    Ok(skipped) => ws.ask(Notice::export_images_skipped(skipped), cx),
                    Err(PdfError::Failed(reason)) => ws.ask(pdf_failed(&reason), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// `file.print`.
    pub(super) fn print_command(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let tab = self.command_tab(cx);
        let source = match self.export_source(tab, cx) {
            Ok(source) => source,
            Err(reason) => return self.ask(print_failed(&reason), cx),
        };
        let Some(engine) = export::pdf_engine(cx) else {
            return self.ask(print_failed(NO_ENGINE), cx);
        };
        ask_options(
            ExportKind::Print,
            None,
            window,
            cx,
            move |ws, choices, window, cx| {
                ws.print_with(source.clone(), engine.clone(), choices, window, cx);
            },
        );
    }

    /// Asks the print dialog, then prints `source` with `choices`.
    fn print_with(
        &mut self,
        source: Source,
        engine: Arc<dyn export::PdfEngine>,
        choices: ExportChoices,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) {
        let title = source
            .path
            .as_deref()
            .and_then(Path::file_stem)
            .map_or_else(
                || "Untitled".to_owned(),
                |s| s.to_string_lossy().into_owned(),
            );
        let printer = print::printer(cx);
        let prepared = printer.prepare(&title, window, cx);
        cx.spawn(async move |this, cx| {
            let setup = match prepared.await {
                Ok(Some(setup)) => setup,
                Ok(None) => return,
                Err(reason) => {
                    this.update(cx, |ws, cx| ws.ask(print_failed(&reason), cx))
                        .ok();
                    return;
                }
            };
            let mut choices = choices;
            choices.page = choices.print_page(print_page(setup.page));
            let Ok(Some(cancel)) =
                this.update(cx, |ws, cx| ws.start_pdf_job("Preparing to print…", cx))
            else {
                return;
            };
            let flag = cancel.clone();
            let made = cx
                .background_spawn(async move {
                    export::export_pdf_with(
                        &source.text,
                        source.path.as_deref(),
                        source.root.as_deref(),
                        &source.profile,
                        source.index.as_ref(),
                        &choices,
                        &*engine,
                        &flag,
                    )
                })
                .await;
            let Ok(pdf) = this.update(cx, |ws, cx| {
                ws.end_pdf_job(cx);
                match made {
                    Ok(_) if cancel.load(Ordering::Relaxed) => None,
                    Ok(pdf) => Some(pdf),
                    Err(PdfError::Cancelled) => None,
                    Err(PdfError::Failed(reason)) => {
                        ws.ask(print_failed(&reason), cx);
                        None
                    }
                }
            }) else {
                return;
            };
            let Some(pdf) = pdf else {
                return;
            };
            let printed = cx
                .update(|cx| printer.print(setup, &title, pdf.bytes, cx))
                .await;
            this.update(cx, |ws, cx| match printed {
                Ok(Printed::Sent | Printed::Cancelled) => {}
                Ok(Printed::InViewer(_)) => ws.ask(
                    notice(
                        NoticeKind::Information,
                        "Opened in the PDF viewer.",
                        Some(
                            "No print dialog is available here: print from the viewer.".to_owned(),
                        ),
                    ),
                    cx,
                ),
                Err(reason) => ws.ask(print_failed(&reason), cx),
            })
            .ok();
        })
        .detach();
    }

    /// `file.open_in_browser`.
    pub(super) fn open_in_browser_command(&mut self, cx: &mut Context<'_, Self>) {
        let tab = self.command_tab(cx);
        let source = match self.export_source(tab, cx) {
            Ok(source) => source,
            Err(reason) => return self.ask(Notice::export_failed(&reason), cx),
        };
        let name = export::suggested_name(source.path.as_deref());
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_spawn(async move {
                    let export = export::export_text(
                        &source.text,
                        source.path.as_deref(),
                        source.root.as_deref(),
                        &source.profile,
                        source.index.as_ref(),
                    );
                    export::write_temporary(&export, &name).map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |ws, cx| match written {
                Ok(path) => platform::launcher(cx).open_with_default_app(&path, cx),
                Err(reason) => ws.ask(Notice::export_failed(&reason), cx),
            })
            .ok();
        })
        .detach();
    }

    /// `editor.copy_as_rich_text`: the selection of the focused view (or,
    /// without one, of the view the mode shows).
    pub(super) fn copy_as_rich_text_command(
        &mut self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) {
        let index = self.active;
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let split = tab.split.as_ref().map(|s| {
            let s = s.read(cx);
            (s.editor().clone(), s.reader().clone())
        });
        let editors: Vec<_> = tab
            .editor
            .iter()
            .cloned()
            .chain(split.iter().map(|s| s.0.clone()))
            .collect();
        let readers: Vec<_> = tab
            .reader
            .iter()
            .cloned()
            .chain(split.iter().map(|s| s.1.clone()))
            .collect();
        let focused_editor = editors
            .iter()
            .find(|e| e.focus_handle(cx).contains_focused(window, cx))
            .cloned();
        let focused_reader = readers
            .iter()
            .find(|r| r.focus_handle(cx).contains_focused(window, cx))
            .cloned();
        let use_editor = match (&focused_editor, &focused_reader) {
            (Some(_), _) => true,
            (None, Some(_)) => false,
            (None, None) => self.mode() != ViewMode::Preview,
        };
        let path = tab
            .path
            .clone()
            .map(|p| std::path::absolute(&p).unwrap_or(p));
        let selected = if use_editor {
            focused_editor
                .or_else(|| editors.first().cloned())
                .and_then(|e| {
                    let view = e.read(cx);
                    let editor = view.editor()?;
                    let selections = editor.selections();
                    let range = selections.ranges().nth(selections.primary_index())?;
                    Some((editor.text().to_string(), range))
                })
        } else {
            focused_reader
                .or_else(|| readers.first().cloned())
                .and_then(|r| r.read(cx).selection_source())
                .map(|(text, range)| (text.to_string(), range))
        };
        let Some((text, range)) = selected.filter(|(_, r)| !r.is_empty()) else {
            return;
        };
        let profile = self.tab_profile(index, cx).compiled;
        let snapshot = self.vault.as_ref().map(|v| v.read(cx).snapshot());
        let copy = export::rich_copy(&text, range, path.as_deref(), &profile, snapshot.as_ref());
        let written = match export::rich_clipboard(cx) {
            Some(clipboard) => clipboard.write(&copy.html, &copy.text),
            None => Err("no rich-text clipboard".to_owned()),
        };
        if let Err(reason) = written {
            tracing::info!("copied as plain text: {reason}");
            cx.write_to_clipboard(ClipboardItem::new_string(copy.text));
            self.ask(Notice::rich_copy_plain(&reason), cx);
        }
    }
}

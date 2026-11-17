// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Settings export and import (`settings.export`, `settings.import`,
//! FR-SET-05) in the settings window.
//!
//! - **Export** asks where to save (the platform's save dialog) and writes
//!   the settings for all vaults that are valid, as a settings file
//!   ([`gv_settings::export_settings`]).
//! - **Import** asks for a file (the platform's open dialog), reads it
//!   ([`gv_settings::preview_import`]) and shows a preview in place of the
//!   section: one row per setting it would change, with the current and
//!   the new value and a switch to leave it out. Settings the file does not
//!   set stay as they are. Values less strict than the current ones in
//!   security-relevant settings are marked and start switched off. *Apply*
//!   writes the chosen values to `settings.toml` one key at a time
//!   (keeping its comments); *Cancel* changes nothing. Keys the file holds
//!   that cannot be used are listed below the changes.
//!
//! Not designed (NFR-UI-01): the rows in the About section, the preview
//! and the status line.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gv_settings::{ImportPreview, display_value};

use super::controls;
use super::metrics::row as row_metrics;
use super::{SettingsWindow, eid, secondary_button};
use crate::files::open_dialog::{self, OpenPrompt};
use crate::files::save_as::{self, SavePrompt};
use crate::metrics::{HAIRLINE, radius, text};
use crate::settings::{AppSettings, Target};
use crate::theme::Theme;

/// The largest file an import reads.
const MAX_IMPORT_BYTES: u64 = 1024 * 1024;

/// An import waiting for *Apply* or *Cancel*.
#[derive(Debug, Clone)]
pub struct PendingImport {
    pub path: PathBuf,
    pub preview: ImportPreview,
    /// The keys that will be applied.
    pub chosen: HashSet<&'static str>,
}

impl SettingsWindow {
    /// The import waiting for an answer, if any.
    pub fn pending_import(&self) -> Option<&PendingImport> {
        self.import.as_ref()
    }

    /// The last export or import message (not designed).
    pub fn transfer_status(&self) -> Option<&str> {
        self.transfer_status.as_deref()
    }

    /// Export settings…: asks where, then writes the file.
    pub fn export_settings(&mut self, cx: &mut Context<'_, Self>) {
        let prompt = SavePrompt {
            directory: default_folder(),
            suggested_name: gv_settings::EXPORT_FILE_NAME.to_owned(),
        };
        let answer = save_as::dialog(cx).ask(&prompt, cx);
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| match answer {
                Ok(Some(path)) => this.export_to(&path, cx),
                Ok(None) => {}
                Err(err) => this.set_status(format!("The save dialog failed: {err}"), cx),
            });
        })
        .detach();
    }

    /// Writes the export to `path`.
    pub fn export_to(&mut self, path: &Path, cx: &mut Context<'_, Self>) {
        let settings = cx.global::<AppSettings>();
        let text = match settings.global_layer() {
            Some(layer) => gv_settings::export_settings(layer),
            None => gv_settings::export_settings(&gv_settings::Layer::empty(
                gv_settings::LayerKind::Global,
            )),
        };
        let message = match gv_settings::atomic_write(path, text.as_bytes()) {
            Ok(()) => format!("Settings exported to {}.", path.display()),
            Err(err) => format!("Cannot write {}: {err}.", path.display()),
        };
        self.set_status(message, cx);
    }

    /// Import settings…: asks for a file, then shows the preview.
    pub fn import_settings(&mut self, cx: &mut Context<'_, Self>) {
        let answer = open_dialog::dialog(cx).ask(OpenPrompt::Files, cx);
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| match answer {
                Ok(Some(paths)) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.import_from(&path, cx);
                    }
                }
                Ok(None) => {}
                Err(err) => this.set_status(format!("The open dialog failed: {err}"), cx),
            });
        })
        .detach();
    }

    /// Reads `path` and shows what importing it would change.
    pub fn import_from(&mut self, path: &Path, cx: &mut Context<'_, Self>) {
        let text = match read_capped(path) {
            Ok(text) => text,
            Err(err) => {
                self.set_status(format!("Cannot read {}: {err}.", path.display()), cx);
                return;
            }
        };
        let settings = cx.global::<AppSettings>();
        match gv_settings::preview_import(&text, Some(path), settings.global_layer()) {
            Ok(preview) => {
                let chosen = preview
                    .changes
                    .iter()
                    .filter(|c| !c.loosens)
                    .map(|c| c.key)
                    .collect();
                self.import = Some(PendingImport {
                    path: path.to_path_buf(),
                    preview,
                    chosen,
                });
                self.transfer_status = None;
                self.open_select = None;
                cx.notify();
            }
            Err(err) => self.set_status(format!("Cannot import {}: {err}", path.display()), cx),
        }
    }

    /// Switches one change of the pending import on or off.
    pub fn toggle_import_key(&mut self, key: &'static str, cx: &mut Context<'_, Self>) {
        if let Some(import) = self.import.as_mut()
            && !import.chosen.remove(key)
        {
            import.chosen.insert(key);
        }
        cx.notify();
    }

    /// Applies the chosen changes to the settings for all vaults.
    pub fn apply_import(&mut self, cx: &mut Context<'_, Self>) {
        let Some(import) = self.import.take() else {
            return;
        };
        let mut applied = 0;
        let mut refused = Vec::new();
        for change in &import.preview.changes {
            if !import.chosen.contains(change.key) {
                continue;
            }
            match AppSettings::set_value(cx, Target::AllVaults, change.key, &change.new) {
                Ok(()) => applied += 1,
                Err(err) => refused.push(format!("{}: {err}", change.key)),
            }
        }
        let mut message = match applied {
            1 => "Imported 1 setting.".to_owned(),
            n => format!("Imported {n} settings."),
        };
        if !refused.is_empty() {
            message.push_str(&format!(" Not imported: {}.", refused.join("; ")));
        }
        self.set_status(message, cx);
    }

    /// Leaves the preview without changing anything.
    pub fn cancel_import(&mut self, cx: &mut Context<'_, Self>) {
        self.import = None;
        cx.notify();
    }

    fn set_status(&mut self, message: String, cx: &mut Context<'_, Self>) {
        tracing::info!("settings: {message}");
        self.transfer_status = Some(message);
        cx.notify();
    }

    /// The rows of the About section for export and import.
    pub(super) fn transfer_rows(&self, theme: &Theme, cx: &mut Context<'_, Self>) -> AnyElement {
        let export = secondary_button(
            "settings-export",
            "Export…",
            self.on_click(cx, |this, _, cx| this.export_settings(cx)),
            theme,
        )
        .into_any_element();
        let import = secondary_button(
            "settings-import",
            "Import…",
            self.on_click(cx, |this, _, cx| this.import_settings(cx)),
            theme,
        )
        .into_any_element();
        div()
            .child(controls::heading("Export and import", false, theme))
            .child(
                div()
                    .border_b(px(HAIRLINE))
                    .border_color(theme.border.default)
                    .child(controls::row(
                        "settings-export-row",
                        "Export settings",
                        Some("Save the settings for all vaults to a file.".into()),
                        None,
                        vec![export],
                        theme,
                    ))
                    .child(controls::row(
                        "settings-import-row",
                        "Import settings",
                        Some(
                            "Read settings from a file. The changes are shown before anything \
                             is saved; settings the file does not set stay as they are."
                                .into(),
                        ),
                        None,
                        vec![import],
                        theme,
                    )),
            )
            .when_some(self.transfer_status.clone(), |d, status| {
                d.child(
                    div()
                        .debug_selector(|| "settings-transfer-status".into())
                        .my(px(row_metrics::NOTE_MARGIN_Y))
                        .text_color(theme.text.secondary)
                        .child(status),
                )
            })
            .into_any_element()
    }

    /// The preview of a pending import, in place of the section.
    pub(super) fn import_preview(
        &self,
        theme: &Theme,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let Some(import) = &self.import else {
            return Vec::new();
        };
        let mut out = Vec::new();
        out.push(
            div()
                .mb(px(row_metrics::NOTE_MARGIN_Y))
                .text_color(theme.text.secondary)
                .child(format!(
                    "From {}. Settings the file does not set stay as they are.",
                    import.path.display()
                ))
                .into_any_element(),
        );
        if import.preview.changes.is_empty() {
            out.push(
                div()
                    .debug_selector(|| "settings-import-nothing".into())
                    .my(px(row_metrics::NOTE_MARGIN_Y))
                    .child(format!(
                        "Nothing to change: the file's {} settings match yours.",
                        import.preview.unchanged
                    ))
                    .into_any_element(),
            );
        } else {
            let rows: Vec<AnyElement> = import
                .preview
                .changes
                .iter()
                .map(|change| {
                    let key = change.key;
                    let on = import.chosen.contains(key);
                    let description: SharedString = format!(
                        "{} → {}",
                        display_value(&change.current),
                        display_value(&change.new)
                    )
                    .into();
                    let warning: Option<SharedString> = change
                        .loosens
                        .then(|| "Less strict than your current setting.".into());
                    let label = gv_settings::spec(key)
                        .map(|s| format!("{} ({})", super::model::row_label(s), key))
                        .unwrap_or_else(|| key.to_owned());
                    controls::row(
                        eid("import", key),
                        label,
                        Some(description),
                        warning,
                        vec![
                            controls::switch(
                                eid("import-switch", key),
                                on,
                                true,
                                self.on_click(cx, move |this, _, cx| {
                                    this.toggle_import_key(key, cx)
                                }),
                                theme,
                            )
                            .debug_selector(move || format!("settings-import-switch-{key}"))
                            .into_any_element(),
                        ],
                        theme,
                    )
                    .into_any_element()
                })
                .collect();
            out.push(
                div()
                    .child(controls::heading(
                        format!("Changes ({})", import.preview.changes.len()),
                        true,
                        theme,
                    ))
                    .child(
                        div()
                            .border_b(px(HAIRLINE))
                            .border_color(theme.border.default)
                            .children(rows),
                    )
                    .into_any_element(),
            );
        }
        if !import.preview.problems.is_empty() {
            out.push(
                div()
                    .child(controls::heading("Not imported", false, theme))
                    .children(import.preview.problems.iter().map(|p| {
                        div()
                            .py(px(row_metrics::PAD_Y))
                            .text_color(theme.text.secondary)
                            .child(match &p.key {
                                Some(key) => format!("{key}: {}", p.message),
                                None => p.message.clone(),
                            })
                    }))
                    .into_any_element(),
            );
        }
        let chosen = import.chosen.len();
        let apply_label: SharedString = match chosen {
            0 => "Apply".into(),
            1 => "Apply 1 change".into(),
            n => format!("Apply {n} changes").into(),
        };
        let (hover, press) = (theme.state.hover, theme.state.press);
        out.push(
            div()
                .mt(px(row_metrics::HEADING_GAP))
                .flex()
                .gap(px(row_metrics::CONTROL_GAP))
                .child(
                    div()
                        .id("settings-import-apply")
                        .debug_selector(|| "settings-import-apply".into())
                        .flex_none()
                        .h(px(crate::metrics::size::CONTROL))
                        .px(px(row_metrics::SEGMENT_PAD_X))
                        .flex()
                        .items_center()
                        .rounded(px(radius::CONTROL))
                        .bg(theme.accent.default)
                        .text_color(theme.accent.on)
                        .font_weight(FontWeight(text::weight::ACTIVE))
                        .when(chosen == 0, |d| d.opacity(crate::metrics::DISABLED_OPACITY))
                        .when(chosen > 0, |d| {
                            d.hover(move |s| s.opacity(0.9))
                                .active(move |s| s.opacity(0.8))
                                .on_click(cx.listener(|this, _, _, cx| this.apply_import(cx)))
                        })
                        .child(apply_label),
                )
                .child(
                    div()
                        .id("settings-import-cancel")
                        .debug_selector(|| "settings-import-cancel".into())
                        .flex_none()
                        .h(px(crate::metrics::size::CONTROL))
                        .px(px(row_metrics::SEGMENT_PAD_X))
                        .flex()
                        .items_center()
                        .rounded(px(radius::CONTROL))
                        .border_1()
                        .border_color(theme.border.strong)
                        .bg(theme.surface.raised)
                        .font_weight(FontWeight(text::weight::ACTIVE))
                        .hover(move |s| s.bg(hover))
                        .active(move |s| s.bg(press))
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_import(cx)))
                        .child("Cancel"),
                )
                .into_any_element(),
        );
        out
    }
}

/// Reads at most [`MAX_IMPORT_BYTES`] of UTF-8 text.
fn read_capped(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(std::io::Error::other("the file is larger than 1 MB"));
    }
    String::from_utf8(bytes).map_err(|_| std::io::Error::other("the file is not UTF-8 text"))
}

/// Where the save dialog opens: the documents folder, else home.
fn default_folder() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| {
            let documents = home.join("Documents");
            if documents.is_dir() { documents } else { home }
        })
        .unwrap_or_else(std::env::temp_dir)
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Daily notes and static templates (FR-EDIT-20, Should; plan 10 § Daily
//! notes, templates and Git).
//!
//! - **`notes.daily_note`** opens today's note of the window's vault:
//!   `<folder>/<format>.md` from `.obsidian/daily-notes.json` (`folder`,
//!   `format`, `template`), else Obsidian's defaults (the vault root,
//!   `YYYY-MM-DD`, no template). A missing note is created from the
//!   template (never replacing a file) and opens in Edit mode.
//! - **`notes.previous_daily_note`**, **`notes.next_daily_note`**: the
//!   nearest existing daily note before or after the active one (or today
//!   when the active note is not a daily note), up to 20 years back and 5
//!   ahead.
//! - **`notes.insert_template`** (in the Source view) lists the notes of
//!   the templates folder (`.obsidian/templates.json` `folder`, else a
//!   `Templates` folder at the root) and inserts the chosen one at the
//!   caret as one undo step.
//!
//! Templates substitute `{{title}}` (the note's name), `{{date}}`,
//! `{{time}}`, `{{date:FORMAT}}` and `{{time:FORMAT}}` (Moment.js tokens,
//! [`gv_bases::Date::format`]; the default formats from `templates.json`
//! `dateFormat` / `timeFormat`, else `YYYY-MM-DD` / `HH:mm`). Anything else
//! is copied as written: Templater syntax is never run (D8). The Obsidian
//! files are only read (NFR-SAFE-04).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{AppContext as _, Context, Div, InteractiveElement as _, Stateful, Window};
use gv_bases::Date;
use serde_json::Value;

use super::{OpenTarget, Workspace};
use crate::files::notice::Notice;
use crate::goto::{self, Choice, GotoEvent, GotoItem, GotoKind, GotoRequest};
use crate::keymap::actions::notes as commands;

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// How far back and ahead previous and next daily notes are looked for.
const YEARS_BACK: i64 = 20;
const YEARS_AHEAD: i64 = 5;

/// The daily notes' settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DailyConfig {
    /// Vault-relative, `/`-separated; empty for the root.
    pub folder: String,
    pub format: String,
    /// Vault-relative path of the template note (`.md` optional); empty
    /// for none.
    pub template: String,
}

impl Default for DailyConfig {
    fn default() -> Self {
        Self {
            folder: String::new(),
            format: "YYYY-MM-DD".to_owned(),
            template: String::new(),
        }
    }
}

/// The templates' settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TemplatesConfig {
    /// Vault-relative; `None` when not set.
    pub folder: Option<String>,
    pub date_format: String,
    pub time_format: String,
}

impl Default for TemplatesConfig {
    fn default() -> Self {
        Self {
            folder: None,
            date_format: "YYYY-MM-DD".to_owned(),
            time_format: "HH:mm".to_owned(),
        }
    }
}

/// A non-empty string value of `key`, without surrounding blanks and
/// slashes.
fn setting(json: &Value, key: &str) -> Option<String> {
    let value = json.get(key)?.as_str()?.trim().trim_matches('/').trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `.obsidian/daily-notes.json` of the vault at `root`, with defaults.
pub(crate) fn daily_config(root: &Path) -> DailyConfig {
    let mut config = DailyConfig::default();
    if let Some(json) = read_json(&root.join(".obsidian").join("daily-notes.json")) {
        if let Some(folder) = setting(&json, "folder") {
            config.folder = folder;
        }
        if let Some(format) = setting(&json, "format") {
            config.format = format;
        }
        if let Some(template) = setting(&json, "template") {
            config.template = template;
        }
    }
    config
}

/// `.obsidian/templates.json` of the vault at `root`, with defaults; the
/// folder falls back to a `Templates` (or `templates`) folder at the root.
pub(crate) fn templates_config(root: &Path) -> TemplatesConfig {
    let mut config = TemplatesConfig::default();
    if let Some(json) = read_json(&root.join(".obsidian").join("templates.json")) {
        config.folder = setting(&json, "folder");
        if let Some(format) = setting(&json, "dateFormat") {
            config.date_format = format;
        }
        if let Some(format) = setting(&json, "timeFormat") {
            config.time_format = format;
        }
    }
    if config.folder.is_none() {
        config.folder = ["Templates", "templates"]
            .into_iter()
            .find(|name| root.join(name).is_dir())
            .map(str::to_owned);
    }
    config
}

/// Now, local time.
pub(crate) fn now() -> Date {
    use chrono::{Datelike as _, Timelike as _};
    let now = chrono::Local::now();
    Date::from_civil(
        i64::from(now.year()),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.timestamp_subsec_millis().min(999),
    )
}

/// `date` at midnight.
fn day_of(date: Date) -> Date {
    Date {
        ms: date.ms.div_euclid(MS_PER_DAY) * MS_PER_DAY,
        has_time: true,
    }
}

/// The daily note of `day`, vault-relative (`/`-separated, with `.md`).
pub(crate) fn daily_path(config: &DailyConfig, day: Date) -> String {
    let name = day.format(&config.format);
    let name = name.trim_matches('/');
    if config.folder.is_empty() {
        format!("{name}.md")
    } else {
        format!("{}/{name}.md", config.folder)
    }
}

/// `text` with the template variables of a note named `title` filled in
/// for `date` (`{{date}}`, `{{date:FMT}}`) and `time` (`{{time}}`,
/// `{{time:FMT}}`). Everything else is kept as written.
pub(crate) fn expand(
    text: &str,
    title: &str,
    date: Date,
    time: Date,
    config: &TemplatesConfig,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = after[..end].trim();
        let (name, format) = match inner.split_once(':') {
            Some((name, format)) => (name.trim(), Some(format.trim())),
            None => (inner, None),
        };
        let value = match (name.to_ascii_lowercase().as_str(), format) {
            ("title", None) => Some(title.to_owned()),
            ("date", None) => Some(date.format(&config.date_format)),
            ("time", None) => Some(time.format(&config.time_format)),
            ("date", Some(f)) if !f.is_empty() => Some(date.format(f)),
            ("time", Some(f)) if !f.is_empty() => Some(time.format(f)),
            _ => None,
        };
        match value {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// The notes below `folder` (absolute), relative to it, `/`-separated.
fn notes_below(folder: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
                && let Ok(rel) = path.strip_prefix(folder)
            {
                let rel: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(rel.join("/"));
            }
        }
    }
    out.sort_by(|a, b| super::tree::natural_cmp(a, b));
    out
}

/// The existing daily notes of the vault at `root`, oldest first, with
/// their day, between [`YEARS_BACK`] before and [`YEARS_AHEAD`] after
/// `today`.
pub(crate) fn existing_daily_notes(
    root: &Path,
    config: &DailyConfig,
    today: Date,
) -> Vec<(Date, String)> {
    let base = if config.folder.is_empty() {
        root.to_path_buf()
    } else {
        root.join(&config.folder)
    };
    let prefix = if config.folder.is_empty() {
        String::new()
    } else {
        format!("{}/", config.folder)
    };
    let files: HashSet<String> = notes_below(&base)
        .into_iter()
        .map(|rel| format!("{prefix}{rel}"))
        .collect();
    if files.is_empty() {
        return Vec::new();
    }
    let today = day_of(today);
    let first = today.ms - YEARS_BACK * 366 * MS_PER_DAY;
    let last = today.ms + YEARS_AHEAD * 366 * MS_PER_DAY;
    let mut out: Vec<(Date, String)> = Vec::new();
    let mut ms = first;
    while ms <= last {
        let day = Date { ms, has_time: true };
        let path = daily_path(config, day);
        if files.contains(&path) && out.last().is_none_or(|(_, p)| *p != path) {
            out.push((day, path));
        }
        ms += MS_PER_DAY;
    }
    out
}

/// The daily note before (`forward` false) or after `current`, a
/// vault-relative path: when `current` is not a daily note, today's day is
/// the reference.
pub(crate) fn neighbour(
    notes: &[(Date, String)],
    current: Option<&str>,
    today: Date,
    forward: bool,
) -> Option<String> {
    let reference = current
        .and_then(|c| notes.iter().find(|(_, p)| p == c))
        .map_or_else(|| day_of(today), |(d, _)| *d);
    if forward {
        notes
            .iter()
            .find(|(d, _)| d.ms > reference.ms)
            .map(|(_, p)| p.clone())
    } else {
        notes
            .iter()
            .rev()
            .find(|(d, _)| d.ms < reference.ms)
            .map(|(_, p)| p.clone())
    }
}

/// The name a note is shown by: its file name without `.md`.
fn title_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads the template at `template` (vault-relative, `.md` optional).
fn read_template(root: &Path, template: &str) -> Option<String> {
    let with_md = if template.to_ascii_lowercase().ends_with(".md") {
        template.to_owned()
    } else {
        format!("{template}.md")
    };
    std::fs::read_to_string(root.join(&with_md))
        .or_else(|_| std::fs::read_to_string(root.join(template)))
        .ok()
}

/// Creates today's daily note when missing. Returns its path and whether
/// it was created.
pub(crate) fn ensure_daily_note(root: &Path, now: Date) -> std::io::Result<(PathBuf, bool)> {
    let config = daily_config(root);
    let rel = daily_path(&config, day_of(now));
    let vault_path = gv_vault::VaultPath::new(&rel)
        .map_err(|e| std::io::Error::other(format!("“{rel}” is not a note name: {e}")))?;
    let path = vault_path.to_fs_path(root);
    if path.exists() {
        return Ok((path, false));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = if config.template.is_empty() {
        String::new()
    } else {
        match read_template(root, &config.template) {
            Some(template) => {
                // As in Obsidian, `{{date}}` of a daily note is its name's date.
                let mut formats = templates_config(root);
                formats.date_format.clone_from(&config.format);
                expand(&template, &title_of(&path), day_of(now), now, &formats)
            }
            None => {
                tracing::warn!(
                    "daily notes: template “{}” not found; the note starts empty",
                    config.template
                );
                String::new()
            }
        }
    };
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok((path, false)),
        Err(err) => return Err(err),
    };
    std::io::Write::write_all(&mut file, text.as_bytes())?;
    file.sync_all()?;
    Ok((path, true))
}

/// `path` (absolute) relative to `root`, `/`-separated.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let rel = path.strip_prefix(root).ok()?;
    gv_vault::VaultPath::from_relative_fs_path(rel)
        .ok()
        .map(|p| p.as_str().to_owned())
}

impl Workspace {
    /// The daily note and template commands.
    pub(super) fn daily_note_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<'_, Self>,
    ) -> Stateful<Div> {
        root.on_action(cx.listener(|this, _: &commands::DailyNote, _, cx| {
            this.open_daily_note(cx);
        }))
        .on_action(cx.listener(|this, _: &commands::PreviousDailyNote, _, cx| {
            this.step_daily_note(false, cx);
        }))
        .on_action(cx.listener(|this, _: &commands::NextDailyNote, _, cx| {
            this.step_daily_note(true, cx);
        }))
        .on_action(
            cx.listener(|this, _: &commands::InsertTemplate, window, cx| {
                this.insert_template_command(window, cx);
            }),
        )
    }

    /// The window's vault root (made from the active tab's file when the
    /// window has no vault yet), or an infobar saying there is none.
    fn notes_root(&mut self, what: &str, cx: &mut Context<'_, Self>) -> Option<PathBuf> {
        self.ensure_vault(cx);
        let root = self.vault_root(cx);
        if root.is_none() {
            self.ask(
                Notice::link_not_followed(
                    format!("No {what}."),
                    "Open a folder first: daily notes and templates live in a vault.".to_owned(),
                ),
                cx,
            );
        }
        root
    }

    /// `notes.daily_note`: today's note, created when missing.
    pub(super) fn open_daily_note(&mut self, cx: &mut Context<'_, Self>) {
        let Some(root) = self.notes_root("daily note", cx) else {
            return;
        };
        let handle = self.window_handle;
        let now = now();
        let task = cx.background_spawn({
            let root = root.clone();
            async move { ensure_daily_note(&root, now) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| match result {
                    Ok((path, true)) => {
                        ws.open_tab_for_edit(OpenTarget::new(path), None, window, cx);
                    }
                    Ok((path, false)) => ws.open_path(path, window, cx),
                    Err(err) => {
                        ws.ask(Notice::new_note_failed(&root, &err.to_string()), cx);
                    }
                })
            });
        })
        .detach();
    }

    /// `notes.previous_daily_note` and `notes.next_daily_note`.
    pub(super) fn step_daily_note(&mut self, forward: bool, cx: &mut Context<'_, Self>) {
        let Some(root) = self.notes_root("daily note", cx) else {
            return;
        };
        let current = self
            .active_tab()
            .path
            .as_deref()
            .and_then(|p| relative(&root, p));
        let handle = self.window_handle;
        let today = now();
        let task = cx.background_spawn({
            let root = root.clone();
            async move {
                let config = daily_config(&root);
                let notes = existing_daily_notes(&root, &config, today);
                neighbour(&notes, current.as_deref(), today, forward)
            }
        });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |ws, cx| match found {
                    Some(rel) => match gv_vault::VaultPath::new(&rel) {
                        Ok(path) => ws.open_path(path.to_fs_path(&root), window, cx),
                        Err(err) => tracing::warn!("daily notes: “{rel}”: {err}"),
                    },
                    None => {
                        let which = if forward { "after" } else { "before" };
                        ws.ask(
                            Notice::link_not_followed(
                                format!("No daily note {which} this one."),
                                "Daily notes are found by the folder and date format of the \
                                 daily notes settings."
                                    .to_owned(),
                            ),
                            cx,
                        );
                    }
                })
            });
        })
        .detach();
    }

    /// `notes.insert_template`: picks a template of the templates folder
    /// and inserts it at the caret of the Source view on screen.
    pub(super) fn insert_template_command(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.shown_editor().is_none() {
            return;
        }
        let Some(root) = self.notes_root("template inserted", cx) else {
            return;
        };
        let config = templates_config(&root);
        let Some(folder) = config.folder.clone() else {
            self.ask(
                Notice::link_not_followed(
                    "No templates folder.".to_owned(),
                    "Put templates in a “Templates” folder of the vault, or choose the folder \
                     in Obsidian's Templates options."
                        .to_owned(),
                ),
                cx,
            );
            return;
        };
        let folder_path = root.join(&folder);
        let templates = notes_below(&folder_path);
        let items = templates
            .iter()
            .map(|t| GotoItem {
                label: t.strip_suffix(".md").unwrap_or(t).to_owned().into(),
                detail: None,
                depth: 0,
            })
            .collect();
        let workspace = cx.weak_entity();
        let request = GotoRequest {
            title: "Insert template".into(),
            kind: GotoKind::List {
                placeholder: "Template".into(),
                action: "insert".into(),
                empty: format!("No templates in “{folder}”").into(),
                items,
                initial: 0,
            },
            on_event: Rc::new(move |event, window, cx| {
                let GotoEvent::Confirm(Choice::Item(i)) = event else {
                    return;
                };
                let Some(template) = templates.get(i) else {
                    return;
                };
                let path = folder_path.join(template);
                let config = config.clone();
                let _ = workspace.update(cx, |ws, cx| {
                    ws.insert_template(&path, &config, window, cx);
                });
            }),
        };
        window.defer(cx, move |window, cx| {
            goto::open(request, window, cx);
        });
    }

    /// Inserts the template at `path`, filled in, at the caret of the
    /// active tab's Source view: one undo step.
    pub(super) fn insert_template(
        &mut self,
        path: &Path,
        config: &TemplatesConfig,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                self.ask(
                    Notice::file_op_failed("No template inserted.", &err.to_string()),
                    cx,
                );
                return;
            }
        };
        let Some(editor) = self.active_tab().editor.clone() else {
            return;
        };
        let title = self
            .active_tab()
            .path
            .as_deref()
            .map_or_else(|| self.active_tab().label.name.to_string(), title_of);
        let now = now();
        let text = expand(&text, &title, now, now, config);
        editor.update(cx, |e, cx| {
            e.insert_text_at(None, &text, window, cx);
        });
        self.focus_document(window, cx);
    }
}

/// Today's daily note of the vault at `root` (tests).
#[cfg(test)]
pub(crate) fn today_path(root: &Path) -> PathBuf {
    root.join(daily_path(&daily_config(root), day_of(now())))
}

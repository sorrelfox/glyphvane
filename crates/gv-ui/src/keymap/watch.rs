// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Live reload of `keymap.toml`, the same way as the settings file.

use std::path::PathBuf;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::App;
use gv_settings::SettingsError;
use gv_settings::keymap::{Platform, UserKeymap};
use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, new_debouncer};

/// How long the file must be quiet before it is read again. Editors and
/// atomic writes produce several events per save.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// Watches the keymap file and applies every change to the running app.
///
/// The folder holding the file is watched rather than the file, because
/// saves usually replace the file (atomic rename) and the file may not exist
/// yet; the folder is created if needed. The watcher thread reads the file
/// and hands it to the UI thread, which applies it.
pub(super) fn watch(path: PathBuf, read_only: bool, cx: &mut App) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::warn!(
            "keymap: cannot create {}, so changes to the keymap need a restart: {err}",
            dir.display()
        );
        return;
    }
    let (sender, mut receiver) = mpsc::unbounded::<Result<UserKeymap, SettingsError>>();
    let name = name.to_owned();
    let file_path = path.clone();
    let platform = Platform::current();
    let debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| {
        match result {
            Ok(events) => {
                // Reading the file is an event too (inotify reports opens),
                // so only changes count; otherwise every read would trigger
                // the next.
                let touched = events
                    .iter()
                    .filter(|event| !event.kind.is_access())
                    .flat_map(|event| &event.paths)
                    .any(|p| p.file_name() == Some(name.as_os_str()));
                if touched {
                    // The receiver is gone only when the app is quitting.
                    drop(sender.unbounded_send(super::read(&file_path, platform, read_only)));
                }
            }
            Err(errors) => {
                for err in errors {
                    tracing::warn!("keymap: file watcher: {err}");
                }
            }
        }
    });
    let mut debouncer = match debouncer {
        Ok(debouncer) => debouncer,
        Err(err) => {
            tracing::warn!("keymap: cannot watch the keymap, so changes need a restart: {err}");
            return;
        }
    };
    if let Err(err) = debouncer.watch(dir, RecursiveMode::NonRecursive) {
        tracing::warn!(
            "keymap: cannot watch {}, so changes to the keymap need a restart: {err}",
            dir.display()
        );
        return;
    }
    cx.spawn(async move |cx| {
        // The watcher lives as long as this task, which lives as long as the
        // application.
        let _debouncer = debouncer;
        while let Some(user) = receiver.next().await {
            tracing::info!("keymap: {} changed, applying it", path.display());
            cx.update(|cx| super::apply_read(cx, user));
        }
    })
    .detach();
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Local crash reports (FR-PLAT-08, plan 12 § Privacy).
//!
//! With `privacy_and_security.crash_reports` on (opt-in, asked at first
//! run), a panic writes a plain-text report to the crash-report folder
//! next to the logs ([`gv_settings::Locations::crash_reports_dir`]): the
//! version, the operating system, the thread, where the panic happened,
//! its message and a backtrace. That includes panics the rendering
//! engines contain (a diagram that fails to draw), which do not end the
//! application. **Nothing is ever sent**: the user may attach a report to
//! a GitHub issue by hand.
//!
//! The application installs the panic hook (`gv-app`); this module writes
//! and lists the reports and, at the next start, shows an application
//! notice: *Copy report* puts the newest report on the clipboard, *Show
//! report* shows it in the file manager. The notice comes once per report.
//!
//! Not designed (NFR-UI-01): the notice.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{App, ClipboardItem};

use crate::app_state;
use crate::files::notice::NoticeKind;
use crate::settings::AppSettings;
use crate::updates::notices::{self, AppButton, AppNotice, AppTopic};

/// The setting that turns reports on.
pub const SETTING: &str = "privacy_and_security.crash_reports";

/// Reports kept; older ones are deleted when a new one is written.
pub const KEEP: usize = 20;

const PREFIX: &str = "crash-";
const EXTENSION: &str = "txt";

/// What a report says about one panic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrashInfo {
    /// The panic message.
    pub message: String,
    /// `file:line:column` of the panic.
    pub location: Option<String>,
    /// The thread's name.
    pub thread: Option<String>,
    /// The captured backtrace.
    pub backtrace: String,
}

/// The text of a report written at `unix_time` (seconds).
pub fn format_report(info: &CrashInfo, unix_time: u64) -> String {
    let time = i64::try_from(unix_time)
        .ok()
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map_or_else(|| unix_time.to_string(), |t| t.to_rfc3339());
    let mut text = String::new();
    text.push_str("Glyphvane crash report\n");
    text.push_str(
        "This file was written on this computer only; nothing has been sent anywhere.\n\
         Read it before sharing it: the message may quote a file name or a piece of text.\n\n",
    );
    text.push_str(&format!("Version: {}\n", env!("CARGO_PKG_VERSION")));
    text.push_str(&format!(
        "System: {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    text.push_str(&format!("Time: {time}\n"));
    text.push_str(&format!(
        "Thread: {}\n",
        info.thread.as_deref().unwrap_or("(unnamed)")
    ));
    text.push_str(&format!(
        "Location: {}\n",
        info.location.as_deref().unwrap_or("(unknown)")
    ));
    text.push_str(&format!("Message: {}\n\nBacktrace:\n", info.message));
    text.push_str(&info.backtrace);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Writes a report into `dir` (created when missing) and deletes the
/// oldest beyond [`KEEP`]. The name sorts by time: `crash-<time>-<pid>.txt`.
pub fn write_report(dir: &Path, info: &CrashInfo, unix_time: u64) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let mut path = dir.join(format!(
        "{PREFIX}{unix_time:012}-{}.{EXTENSION}",
        std::process::id()
    ));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!(
            "{PREFIX}{unix_time:012}-{}-{n}.{EXTENSION}",
            std::process::id()
        ));
        n += 1;
    }
    std::fs::write(&path, format_report(info, unix_time))?;
    let all = reports(dir);
    if all.len() > KEEP {
        for old in &all[..all.len() - KEEP] {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(path)
}

/// The reports in `dir`, oldest first.
pub fn reports(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| e == EXTENSION)
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(PREFIX))
        })
        .collect();
    found.sort();
    found
}

/// The newest report the user has not been told about, if any.
pub fn unseen(dir: &Path, seen: Option<&str>) -> Option<PathBuf> {
    let newest = reports(dir).pop()?;
    let name = newest.file_name()?.to_str()?;
    match seen {
        Some(seen) if name <= seen => None,
        _ => Some(newest),
    }
}

/// Whether reports are written (the opt-in setting).
pub fn enabled(cx: &App) -> bool {
    cx.try_global::<AppSettings>()
        .is_some_and(|s| s.settings().privacy_and_security.crash_reports)
}

/// At start-up: shows the notice when a report the user has not seen is
/// waiting. Call after [`crate::app_state::install`].
pub fn announce(cx: &mut App) {
    let Some(dir) = cx
        .try_global::<AppSettings>()
        .and_then(|s| s.locations())
        .map(|l| l.crash_reports_dir())
    else {
        return;
    };
    let seen = app_state::get(cx).crash_report_seen;
    if let Some(report) = unseen(&dir, seen.as_deref()) {
        notices::show(cx, notice(report));
    }
}

/// Records `report` as seen.
fn mark_seen(report: &Path, cx: &mut App) {
    let name = report
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned);
    app_state::update(cx, |state| state.crash_report_seen = name);
}

/// The notice about `report`.
fn notice(report: PathBuf) -> AppNotice {
    let (copy, show, dismissed) = (report.clone(), report.clone(), report);
    AppNotice {
        topic: AppTopic::CrashReport,
        kind: NoticeKind::Information,
        title: "Glyphvane ran into a problem last time".into(),
        detail: Some(
            "A crash report was saved on this computer. Nothing was sent; you can add it to a bug report."
                .into(),
        ),
        buttons: vec![
            AppButton {
                label: "Copy report".into(),
                run: Rc::new(move |_, cx| {
                    match std::fs::read_to_string(&copy) {
                        Ok(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                        Err(err) => tracing::warn!(
                            "crash reports: cannot read {}: {err}",
                            copy.display()
                        ),
                    }
                    mark_seen(&copy, cx);
                }),
                dismisses: false,
            },
            AppButton {
                label: "Show report".into(),
                run: Rc::new(move |_, cx| {
                    let launcher = crate::platform::launcher(cx);
                    launcher.reveal(&show, cx);
                    mark_seen(&show, cx);
                }),
                dismisses: false,
            },
        ],
        on_dismiss: Some(Rc::new(move |cx| mark_seen(&dismissed, cx))),
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use gv_settings::{LayerKind, Locations, SettingsFile};

    use super::*;
    use crate::app_state::StateStore;
    use crate::theme::ThemeChoice;

    fn info() -> CrashInfo {
        CrashInfo {
            message: "index out of bounds".into(),
            location: Some("src/x.rs:1:2".into()),
            thread: Some("main".into()),
            backtrace: "   0: frame".into(),
        }
    }

    #[test]
    fn reports_are_written_listed_and_capped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let folder = dir.path().join("crash-reports");
        assert!(reports(&folder).is_empty());
        let first = write_report(&folder, &info(), 1_800_000_000).expect("written");
        let text = std::fs::read_to_string(&first).expect("read");
        assert!(text.starts_with("Glyphvane crash report\n"));
        assert!(text.contains("nothing has been sent"));
        assert!(text.contains("Message: index out of bounds"));
        assert!(text.contains("Location: src/x.rs:1:2"));
        assert!(text.contains("Time: 2027-01-15T08:00:00+00:00"));
        assert!(text.ends_with("frame\n"));
        // The same second twice gets two files.
        let second = write_report(&folder, &info(), 1_800_000_000).expect("written");
        assert_ne!(first, second);
        for t in 0..KEEP as u64 + 5 {
            write_report(&folder, &info(), 1_900_000_000 + t).expect("written");
        }
        let all = reports(&folder);
        assert_eq!(all.len(), KEEP);
        assert!(!first.exists(), "the oldest went first");
        // Other files in the folder are not reports.
        std::fs::write(folder.join("notes.txt"), "x").expect("write");
        assert_eq!(reports(&folder).len(), KEEP);
    }

    #[test]
    fn unseen_follows_the_newest_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(unseen(dir.path(), None), None);
        let a = write_report(dir.path(), &info(), 100).expect("written");
        let name = a.file_name().and_then(|n| n.to_str()).expect("name");
        assert_eq!(unseen(dir.path(), None), Some(a.clone()));
        assert_eq!(unseen(dir.path(), Some(name)), None);
        let b = write_report(dir.path(), &info(), 200).expect("written");
        assert_eq!(unseen(dir.path(), Some(name)), Some(b));
    }

    #[gpui::test]
    fn the_next_start_offers_the_report_once(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().expect("temp dir");
        let locations = Locations::with_root(dir.path());
        std::fs::create_dir_all(locations.config_dir()).expect("config");
        std::fs::write(
            locations.settings_file(),
            "version = 1\n[privacy_and_security]\ncrash_reports = true\n",
        )
        .expect("write");
        let file = SettingsFile::load(locations.settings_file(), LayerKind::Global).expect("load");
        let report = write_report(&locations.crash_reports_dir(), &info(), 100).expect("written");
        let state_file = locations.app_state_file();
        cx.update(|cx| {
            crate::init(
                cx,
                AppSettings::new(Some(file), Some(ThemeChoice::Light))
                    .with_locations(locations.clone()),
            );
            crate::app_state::install(cx);
            assert!(enabled(cx));
            announce(cx);
            let notice = notices::current(cx).expect("a notice");
            assert_eq!(notice.topic, AppTopic::CrashReport);
            assert_eq!(notice.buttons.len(), 2);
            notices::dismiss(cx);
            assert!(notices::current(cx).is_none());
        });
        let state = gv_settings::AppState::load(&state_file);
        assert_eq!(
            state.crash_report_seen.as_deref(),
            report.file_name().and_then(|n| n.to_str())
        );
        // The next start says nothing about the same report.
        cx.update(|cx| {
            crate::app_state::set_store(cx, StateStore::load(Some(state_file.clone())));
            announce(cx);
            assert!(notices::current(cx).is_none());
        });
    }
}

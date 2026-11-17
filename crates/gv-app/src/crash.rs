// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The panic hook that writes local crash reports (FR-PLAT-08).
//!
//! The hook is installed for every GUI run and writes only while the
//! opt-in setting `privacy_and_security.crash_reports` is on ([`set_enabled`]
//! follows the setting live). It writes at most [`MAX_PER_RUN`] reports per
//! run, then hands over to the default hook, which prints the panic as
//! before. The report's format and folder are `gv_ui::crash_reports`'s;
//! nothing is sent anywhere.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gv_ui::crash_reports::{CrashInfo, write_report};

/// Reports one run writes at most: a panic that repeats (an engine panic
/// contained per diagram) must not fill the disk.
pub const MAX_PER_RUN: usize = 5;

static ENABLED: AtomicBool = AtomicBool::new(false);
static WRITTEN: AtomicUsize = AtomicUsize::new(0);
static FOLDER: OnceLock<PathBuf> = OnceLock::new();

/// Installs the hook. `folder` is where reports go (`None`: no reports,
/// as without a home folder); `enabled` is the setting at start-up.
pub fn install(folder: Option<PathBuf>, enabled: bool) {
    let Some(folder) = folder else {
        return;
    };
    if FOLDER.set(folder).is_err() {
        return;
    }
    set_enabled(enabled);
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record(info);
        default(info);
    }));
}

/// Follows the setting.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

fn record(info: &std::panic::PanicHookInfo<'_>) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Some(folder) = FOLDER.get() else {
        return;
    };
    if WRITTEN.fetch_add(1, Ordering::Relaxed) >= MAX_PER_RUN {
        return;
    }
    let info = crash_info(info, std::backtrace::Backtrace::force_capture().to_string());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    // A report that cannot be written is lost; the default hook still
    // prints the panic.
    let _ = write_report(folder, &info, now);
}

/// What the report says about `info`.
fn crash_info(info: &std::panic::PanicHookInfo<'_>, backtrace: String) -> CrashInfo {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_owned());
    CrashInfo {
        message,
        location: info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())),
        thread: std::thread::current().name().map(str::to_owned),
        backtrace,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn panic_info_becomes_a_report() {
        let seen: Arc<Mutex<Option<CrashInfo>>> = Arc::default();
        let sink = seen.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            *sink.lock().expect("lock") = Some(crash_info(info, "bt".to_owned()));
        }));
        let result = std::thread::Builder::new()
            .name("worker".to_owned())
            .spawn(|| panic!("boom {}", 7))
            .expect("spawned")
            .join();
        std::panic::set_hook(previous);
        assert!(result.is_err());
        let info = seen.lock().expect("lock").clone().expect("recorded");
        assert_eq!(info.message, "boom 7");
        assert_eq!(info.thread.as_deref(), Some("worker"));
        assert!(info.location.is_some_and(|l| l.contains("crash.rs")));
        assert_eq!(info.backtrace, "bt");
    }
}

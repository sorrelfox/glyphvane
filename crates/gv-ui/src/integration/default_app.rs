// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The offer to open Markdown files with Glyphvane (FR-SET-06, plan 14 §
//! Per-OS integration), made from first-run onboarding. Only when the user
//! clicks:
//!
//! - **Linux:** `xdg-mime default io.github.sorrelfox.Glyphvane.desktop
//!   text/markdown`, which writes the user's `mimeapps.list`. It needs the
//!   desktop file the packages install.
//! - **Windows:** opens *Settings ▸ Default apps*, where Windows lets only
//!   the user choose.
//! - **macOS:** no API without extra frameworks: the onboarding explains
//!   Finder's *Get Info ▸ Open with ▸ Change All*.

use gpui::{App, AppContext as _};

/// What the platform allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// Glyphvane can set itself as the default (Linux).
    SetDirectly,
    /// Glyphvane can open the system's own settings (Windows).
    OpenSystemSettings,
    /// Only an explanation (macOS).
    Explain,
}

/// The offer on this platform.
pub fn offer() -> Offer {
    if cfg!(target_os = "windows") {
        Offer::OpenSystemSettings
    } else if cfg!(target_os = "macos") {
        Offer::Explain
    } else {
        Offer::SetDirectly
    }
}

/// The sentence the onboarding shows with the offer.
pub fn description() -> &'static str {
    match offer() {
        Offer::SetDirectly => {
            "Open .md files with Glyphvane when you double-click them in the file manager."
        }
        Offer::OpenSystemSettings => {
            "Windows lets you choose the app for .md files in Settings ▸ Default apps."
        }
        Offer::Explain => {
            "In Finder, select a .md file, choose File ▸ Get Info, pick Glyphvane under \
             Open with, then click Change All."
        }
    }
}

/// The desktop file the packages install.
const DESKTOP_FILE: &str = "io.github.sorrelfox.Glyphvane.desktop";

/// The MIME types made to open with Glyphvane.
const MIME_TYPES: &[&str] = &["text/markdown", "text/x-markdown"];

/// Carries out the offer; `done` gets a sentence for the user.
pub fn apply(cx: &mut App, done: impl FnOnce(Result<String, String>, &mut App) + 'static) {
    match offer() {
        Offer::OpenSystemSettings => {
            crate::platform::launcher(cx).open_url("ms-settings:defaultapps", cx);
            done(Ok("Settings ▸ Default apps is open.".to_owned()), cx);
        }
        Offer::Explain => done(Ok(description().to_owned()), cx),
        Offer::SetDirectly => {
            let task = cx.background_spawn(async { set_with_xdg_mime() });
            cx.spawn(async move |cx| {
                let result = task.await;
                cx.update(|cx| done(result, cx));
            })
            .detach();
        }
    }
}

#[cfg(not(test))]
fn set_with_xdg_mime() -> Result<String, String> {
    let mut args = vec!["default", DESKTOP_FILE];
    args.extend_from_slice(MIME_TYPES);
    match std::process::Command::new("xdg-mime").args(&args).status() {
        Ok(status) if status.success() => Ok("Markdown files now open with Glyphvane.".to_owned()),
        Ok(status) => Err(format!("xdg-mime failed ({status})")),
        Err(err) => Err(format!("cannot run xdg-mime: {err}")),
    }
}

/// Unit tests never change the desktop's defaults.
#[cfg(test)]
fn set_with_xdg_mime() -> Result<String, String> {
    let _ = (DESKTOP_FILE, MIME_TYPES);
    Ok("test: not run".to_owned())
}

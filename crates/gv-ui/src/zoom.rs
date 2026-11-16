// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! View zoom (FR-VIEW-09): `view.zoom_in` (Mod+=), `view.zoom_out` (Mod+-)
//! and `view.zoom_reset` (Mod+0).
//!
//! - **One zoom for the application**: the `appearance.zoom_percent`
//!   setting, the same value the settings window's Zoom stepper edits. It
//!   applies to every window and tab at once and is saved in the settings
//!   file, so it persists across sessions. Where the open vault's overlay
//!   sets the zoom itself, the commands change that overlay; otherwise the
//!   global file. In safe mode the change lasts for the session.
//! - **Steps**: 10 percentage points, as the stepper; a value off the grid
//!   (115 typed in the file) goes to the next step in the direction asked
//!   (120 in, 110 out). **Limits**: the setting's range, 50 to 300.
//!   Reset zoom returns to 100.
//! - **What scales**: the Reading view (type, spacing and the readable
//!   column), the Source view (text, rows, line numbers, caret and the
//!   column, [`crate::editor`]) and both panes of the Split view. The
//!   interface does not. The status bar shows the zoom in Preview.

use gpui::App;
use gv_settings::Kind;

use crate::keymap::actions::view::{ZoomIn, ZoomOut, ZoomReset};
use crate::settings::{AppSettings, Target};

/// The setting that holds the zoom.
pub const KEY: &str = "appearance.zoom_percent";
/// The zoom with nothing changed.
pub const DEFAULT: u32 = 100;
/// One step of the zoom commands, in percentage points.
pub const STEP: u32 = 10;

/// The setting's range: (min, max).
pub fn limits() -> (u32, u32) {
    match gv_settings::spec(KEY).map(|s| s.kind) {
        Some(Kind::Integer { min, max }) => (
            u32::try_from(min).unwrap_or(DEFAULT),
            u32::try_from(max).unwrap_or(DEFAULT),
        ),
        _ => (DEFAULT, DEFAULT),
    }
}

/// The zoom one step in from `percent`, within the limits.
pub fn step_in(percent: u32) -> u32 {
    let (min, max) = limits();
    ((percent / STEP + 1) * STEP).clamp(min, max)
}

/// The zoom one step out from `percent`, within the limits.
pub fn step_out(percent: u32) -> u32 {
    let (min, max) = limits();
    (percent.div_ceil(STEP).saturating_sub(1) * STEP).clamp(min, max)
}

/// The zoom in effect, in percent ([`DEFAULT`] without settings).
pub fn percent(cx: &App) -> u32 {
    let (min, max) = limits();
    cx.try_global::<AppSettings>()
        .map_or(DEFAULT, |s| s.settings().appearance.zoom_percent)
        .clamp(min, max)
}

/// The zoom in effect as a factor (1.0 at 100 %).
pub fn factor(cx: &App) -> f32 {
    percent(cx) as f32 / 100.0
}

/// The layer the zoom commands write: the vault's overlay when it sets the
/// zoom itself (the global value would not show), else the global file.
fn target(settings: &AppSettings) -> Target {
    if settings.vault().is_some() && settings.sets(Target::Vault, KEY) {
        Target::Vault
    } else {
        Target::AllVaults
    }
}

/// Sets the zoom to `percent` and saves it (see the module documentation).
pub fn set(cx: &mut App, percent: u32) {
    let Some(settings) = cx.try_global::<AppSettings>() else {
        return;
    };
    let target = target(settings);
    let value = toml::Value::Integer(i64::from(percent));
    if let Err(err) = AppSettings::set_value(cx, target, KEY, &value) {
        tracing::warn!("zoom: cannot set {KEY} to {percent}: {}", err.0);
    }
}

/// Zoom in by one step.
pub fn zoom_in(cx: &mut App) {
    set(cx, step_in(percent(cx)));
}

/// Zoom out by one step.
pub fn zoom_out(cx: &mut App) {
    set(cx, step_out(percent(cx)));
}

/// Back to 100 %.
pub fn reset(cx: &mut App) {
    set(cx, DEFAULT);
}

/// Installs the handlers of the zoom commands. They are application-wide:
/// the zoom is one for every window.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &ZoomIn, cx| zoom_in(cx));
    cx.on_action(|_: &ZoomOut, cx| zoom_out(cx));
    cx.on_action(|_: &ZoomReset, cx| reset(cx));
}

#[cfg(test)]
mod tests;

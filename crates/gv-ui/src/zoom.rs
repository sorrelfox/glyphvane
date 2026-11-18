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
//! - **Keys**: Mod+= and Mod++ (Shift+=, and the keypad's +), Mod+- (the
//!   keypad's - too), Mod+0 (the keypad's 0 too); Mod is Cmd on macOS. The
//!   keypad and shifted keys arrive as `+`, `-` and `0` on every platform
//!   (on Linux the keypad's `add` and `subtract` match through the
//!   character they type).
//! - **Wheel and pinch** ([`wheel_layer`]): Mod with the mouse wheel over
//!   the document zooms one step per notch (per [`PIXELS_PER_STEP`] of a
//!   smooth scroll), up in, down out; a trackpad pinch one step per
//!   [`PINCH_PER_STEP`] of its scale. The canvas, bases and image viewers
//!   keep their own zoom.
//! - **Status bar menu** ([`menu`], not designed): a click on the zoom
//!   opens Zoom in, Zoom out and Reset zoom, then the levels of
//!   [`LEVELS`] with the current one checked.
//! - **Saving**: the change applies at once and is written
//!   [`crate::settings::SAVE_DELAY`] after the last one, so a wheel spin
//!   writes the file once; quitting writes it at once.

use gpui::{
    App, DispatchPhase, Global, HitboxBehavior, IntoElement, PinchEvent, ScrollDelta, ScrollEvent,
    Styled as _, canvas,
};
use gv_settings::Kind;

use crate::keymap::actions::view::{ZoomIn, ZoomOut, ZoomReset};
use crate::settings::{AppSettings, Target};

/// The setting that holds the zoom.
pub const KEY: &str = "appearance.zoom_percent";
/// The zoom with nothing changed.
pub const DEFAULT: u32 = 100;
/// One step of the zoom commands, in percentage points.
pub const STEP: u32 = 10;
/// The levels the status bar's zoom menu lists.
pub const LEVELS: [u32; 11] = [50, 70, 80, 90, 100, 110, 120, 150, 200, 250, 300];
/// A smooth scroll (trackpad) of this many pixels is one zoom step.
pub const PIXELS_PER_STEP: f32 = 50.0;
/// A pinch scaling by this much is one zoom step.
pub const PINCH_PER_STEP: f32 = 0.1;

pub mod menu;

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

/// Sets the zoom to `percent`; it is saved shortly after (see the module
/// documentation).
pub fn set(cx: &mut App, percent: u32) {
    let Some(settings) = cx.try_global::<AppSettings>() else {
        return;
    };
    if settings.settings().appearance.zoom_percent == percent {
        return;
    }
    let target = target(settings);
    let value = toml::Value::Integer(i64::from(percent));
    if let Err(err) = AppSettings::set_value_later(cx, target, KEY, &value) {
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
/// the zoom is one for every window. Quitting saves a zoom not saved yet.
pub(crate) fn init(cx: &mut App) {
    cx.on_action(|_: &ZoomIn, cx| zoom_in(cx));
    cx.on_action(|_: &ZoomOut, cx| zoom_out(cx));
    cx.on_action(|_: &ZoomReset, cx| reset(cx));
    cx.on_app_quit(|cx| {
        AppSettings::save_unsaved(cx);
        async {}
    })
    .detach();
}

/// Zoom steps from wheel and pinch input, with the rest of a step carried
/// over to the next event.
#[derive(Debug, Default)]
pub(crate) struct Gesture {
    rest: f32,
}

impl Global for Gesture {}

impl Gesture {
    /// Adds `amount` (in steps, positive in) and returns the whole steps
    /// reached: +n in, -n out. Turning around drops the rest.
    pub(crate) fn add(&mut self, amount: f32) -> i32 {
        if self.rest != 0.0 && self.rest.signum() != amount.signum() {
            self.rest = 0.0;
        }
        self.rest += amount;
        let steps = self.rest.trunc();
        self.rest -= steps;
        steps as i32
    }
}

/// Zooms by `amount` steps of wheel or pinch input (see [`Gesture`]).
fn gesture(amount: f32, cx: &mut App) {
    let steps = cx.default_global::<Gesture>().add(amount);
    if steps != 0 {
        zoom_by(steps, cx);
    }
}

/// The steps of a Mod+wheel event: one per notch, one per
/// [`PIXELS_PER_STEP`] of a smooth scroll; up zooms in.
pub(crate) fn wheel_steps(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Lines(lines) => lines.y,
        ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / PIXELS_PER_STEP,
    }
}

/// Zooms by `steps` (positive in).
pub(crate) fn zoom_by(steps: i32, cx: &mut App) {
    let mut percent = percent(cx);
    for _ in 0..steps.unsigned_abs() {
        percent = if steps > 0 {
            step_in(percent)
        } else {
            step_out(percent)
        };
    }
    set(cx, percent);
}

/// A layer over the document area that zooms on Mod+wheel and on a pinch
/// there, before the views below scroll. Put it in the area as its first
/// child; it takes no room.
pub fn wheel_layer() -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        |_, hitbox, window, _| {
            {
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture
                        || !e.modifiers.secondary(cx)
                        || !hitbox.should_handle_scroll(window)
                    {
                        return;
                    }
                    cx.stop_propagation();
                    gesture(wheel_steps(e.delta), cx);
                });
            }
            window.on_mouse_event(move |e: &PinchEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || !hitbox.is_hovered(window) {
                    return;
                }
                cx.stop_propagation();
                gesture(e.delta / PINCH_PER_STEP, cx);
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

#[cfg(test)]
mod tests;

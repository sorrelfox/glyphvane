// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The binary's side of the start-up trace (`GLYPHVANE_DEBUG_STARTUP`, see
//! [`gv_ui::reader::startup`]): the origin at `main()` and the binary's
//! milestones.

use std::time::{Duration, Instant};

use gv_ui::reader::startup::{Mode, Origin};

pub use gv_ui::reader::startup::mark;

/// Starts the trace when the environment asks for it. Called first thing in
/// `main()`.
pub fn start() {
    let Some(mode) = Mode::from_env() else {
        return;
    };
    let at = Instant::now();
    let monotonic = monotonic_now();
    gv_ui::reader::startup::start(
        mode,
        Origin {
            at,
            monotonic,
            since_process_start: since_process_start(),
        },
    );
}

/// Gives the trace the window's presentation probe, where there is one, so
/// it can report when the first frame with content reached the screen.
/// Does nothing when the trace is off.
pub fn attach(window: gpui::AnyWindowHandle, cx: &mut gpui::App) {
    if !gv_ui::reader::startup::enabled() {
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if let Some(probe) = window
        .update(cx, |_, window, _| crate::present::Probe::new(window))
        .ok()
        .flatten()
    {
        gv_ui::reader::startup::set_probe(Box::new(probe));
        mark("presentation probe attached (wp_presentation)");
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let _ = (window, cx);
}

/// CLOCK_MONOTONIC now, the clock a script reads with
/// `time.clock_gettime_ns(time.CLOCK_MONOTONIC)`.
#[cfg(unix)]
fn monotonic_now() -> Option<Duration> {
    timespec(rustix::time::clock_gettime(
        rustix::time::ClockId::Monotonic,
    ))
}

#[cfg(not(unix))]
fn monotonic_now() -> Option<Duration> {
    None
}

#[cfg(unix)]
fn timespec(t: rustix::time::Timespec) -> Option<Duration> {
    Some(Duration::new(
        u64::try_from(t.tv_sec).ok()?,
        u32::try_from(t.tv_nsec).ok()?,
    ))
}

/// How long ago this process was created, from its start time in
/// `/proc/self/stat` (clock ticks since boot, rounded down) and
/// CLOCK_BOOTTIME now. The kernel stamps the start at fork, which a
/// launcher follows with exec at once.
#[cfg(target_os = "linux")]
fn since_process_start() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let ticks = start_ticks(&stat)?;
    let hz = rustix::param::clock_ticks_per_second();
    if hz == 0 {
        return None;
    }
    let started = Duration::from_nanos(ticks.checked_mul(1_000_000_000)? / hz);
    timespec(rustix::time::clock_gettime(rustix::time::ClockId::Boottime))?.checked_sub(started)
}

#[cfg(not(target_os = "linux"))]
fn since_process_start() -> Option<Duration> {
    None
}

/// Field 22 of `/proc/PID/stat`, the start time in clock ticks. The command
/// name (field 2) is in parentheses and may hold spaces and parentheses, so
/// the fields are counted from the last `)`.
#[cfg(any(target_os = "linux", test))]
fn start_ticks(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    // `rest` starts with field 3 (the state).
    rest.split_ascii_whitespace().nth(22 - 3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_time_is_field_22() {
        let stat = "4242 (gly (ph) vane) S 1 4242 4242 0 -1 4194560 1234 0 0 0 \
                    5 2 0 0 20 0 9 0 81234567 123456789 4567 18446744073709551615";
        assert_eq!(start_ticks(stat), Some(81_234_567));
        assert_eq!(start_ticks("4242 (glyphvane) S 1"), None);
        assert_eq!(start_ticks("no parenthesis"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_started_before_now() {
        let before = since_process_start().expect("Linux has /proc");
        // A test process is younger than a day.
        assert!(before < Duration::from_secs(86_400), "{before:?}");
    }
}

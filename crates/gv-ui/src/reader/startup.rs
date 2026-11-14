// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A debug trace of start-up: monotonic milestones from `main()` to the
//! first presented frame with content.
//!
//! With `GLYPHVANE_DEBUG_STARTUP=1` every milestone prints one line on
//! standard error, whatever `RUST_LOG` says:
//!
//! ```text
//! glyphvane startup:     0.000 ms  main() entered; CLOCK_MONOTONIC 81234.567890123 s; process created ≈ 2 ms before (/proc, 10 ms steps)
//! glyphvane startup:     0.412 ms  single-instance claim done (primary)
//! glyphvane startup:   104.733 ms  first frame with content painted (README.md, 14 items)
//! glyphvane startup:   105.020 ms  first frame with content submitted
//! glyphvane startup:   105.071 ms  next frame request
//! glyphvane startup:   117.954 ms  first frame with content presented (frame callback of the next frame)
//! ```
//!
//! Times are milliseconds since `main()` was entered. The first line gives
//! the CLOCK_MONOTONIC time of that moment (where the platform has it), so a
//! script that takes the same clock just before it starts the process gets
//! exec → milestone exactly; on Linux it also estimates the time from the
//! creation of the process to `main()`, from `/proc/self/stat` (a start time
//! in clock ticks, so in 10 ms steps, rounded down: the estimate is up to one
//! step long).
//!
//! The milestones:
//!
//! 1. the start-up of the binary: `main()`, the single-instance claim, the
//!    settings, the GPUI application and the window (the binary marks them);
//! 2. loading the first document: read, decoded, parsed, modelled and the
//!    render tree built (on the background thread);
//! 3. the first frame with content: painted (the end of the Reading view's
//!    paint), submitted (the frame handed to the GPU and the presentation
//!    queued, when the window's update that drew it ends), the next frame
//!    request, and presented. GPUI has no presentation callback, and on
//!    Wayland the next frame request can answer a frame callback that an
//!    earlier commit asked for, before the content is on screen. So the
//!    trace draws one more frame (the same content, from GPUI's caches) and
//!    takes "presented" at the frame request after it: on Wayland the
//!    compositor's frame callback for that frame, sent once the compositor
//!    has shown it, elsewhere the display link's next tick. It is an upper
//!    bound: by at most one refresh interval when the content frame made an
//!    earlier refresh than the extra one.
//!
//! The trace ends with the presented frame; later documents are not
//! traced. With `GLYPHVANE_DEBUG_STARTUP=exit` the application then quits,
//! so a script can also time exec → exit.
//!
//! Unset, the trace costs one environment lookup at start and a check of an
//! empty cell per milestone.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The environment variable that turns the trace on: `1` to trace, `exit`
/// to trace and quit after the first presented frame with content.
pub const DEBUG_STARTUP_ENV: &str = "GLYPHVANE_DEBUG_STARTUP";

/// What the trace does at its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Print the milestones.
    Trace,
    /// Print them, then quit after the first presented frame with content.
    Exit,
}

impl Mode {
    /// The mode asked for in the environment; `None` when the trace is off.
    pub fn from_env() -> Option<Self> {
        Self::parse(&std::env::var_os(DEBUG_STARTUP_ENV)?.to_string_lossy())
    }

    /// `exit` quits; any other value but empty and `0` traces.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "" | "0" => None,
            v if v.eq_ignore_ascii_case("exit") => Some(Self::Exit),
            _ => Some(Self::Trace),
        }
    }
}

/// When `main()` was entered.
#[derive(Debug, Clone, Copy)]
pub struct Origin {
    pub at: Instant,
    /// CLOCK_MONOTONIC at `at`, where the platform has it.
    pub monotonic: Option<Duration>,
    /// How long before `at` the process was created, where known.
    pub since_process_start: Option<Duration>,
}

#[derive(Debug)]
struct Trace {
    origin: Origin,
    mode: Mode,
    finished: AtomicBool,
}

static TRACE: OnceLock<Trace> = OnceLock::new();

/// Starts the trace, which prints its first line. Only the first call
/// counts.
pub fn start(mode: Mode, origin: Origin) {
    if TRACE
        .set(Trace {
            origin,
            mode,
            finished: AtomicBool::new(false),
        })
        .is_ok()
    {
        mark(&first_line(&origin));
    }
}

fn first_line(origin: &Origin) -> String {
    let mut line = String::from("main() entered");
    if let Some(mono) = origin.monotonic {
        line.push_str(&format!(
            "; CLOCK_MONOTONIC {}.{:09} s",
            mono.as_secs(),
            mono.subsec_nanos()
        ));
    }
    if let Some(before) = origin.since_process_start {
        line.push_str(&format!(
            "; process created ≈ {} ms before (/proc, 10 ms steps)",
            before.as_millis()
        ));
    }
    line
}

fn active() -> Option<&'static Trace> {
    TRACE
        .get()
        .filter(|trace| !trace.finished.load(Ordering::Relaxed))
}

/// Whether milestones are being traced; for callers that build a label.
pub fn enabled() -> bool {
    active().is_some()
}

/// Prints the milestone `what` with the time since `main()`.
pub fn mark(what: &str) {
    if let Some(trace) = active() {
        eprintln!("{}", line(trace.origin.at.elapsed(), what));
    }
}

fn line(elapsed: Duration, what: &str) -> String {
    format!(
        "glyphvane startup: {:>9.3} ms  {what}",
        elapsed.as_secs_f64() * 1e3
    )
}

/// The first frame with content was painted: marks it, then marks the
/// submit and the present that follow and ends the trace (quitting in
/// [`Mode::Exit`]). Does nothing after the first time.
pub(crate) fn first_content_painted(what: &str, window: &mut gpui::Window, cx: &mut gpui::App) {
    let Some(trace) = active() else {
        return;
    };
    // Only the first content frame of the process.
    static PAINTED: AtomicBool = AtomicBool::new(false);
    if PAINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    mark(&format!("first frame with content painted ({what})"));
    // Deferred work runs when the update that draws the frame ends, which
    // is after GPUI presented it to the GPU surface.
    cx.defer(|_| mark("first frame with content submitted"));
    let mode = trace.mode;
    window.on_next_frame(move |window, _| {
        mark("next frame request");
        // This request may answer a frame callback asked for by an earlier
        // commit (GPUI on Wayland keeps one pending callback), so it can come
        // before the content reached the screen. Draw once more: that frame
        // asks for its own callback, which the compositor sends once it has
        // shown it.
        window.refresh();
        window.on_next_frame(move |_, cx| {
            mark("first frame with content presented (frame callback of the next frame)");
            trace.finished.store(true, Ordering::Relaxed);
            if mode == Mode::Exit {
                cx.quit();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_modes_and_the_lines() {
        assert_eq!(Mode::parse(""), None);
        assert_eq!(Mode::parse("0"), None);
        assert_eq!(Mode::parse("1"), Some(Mode::Trace));
        assert_eq!(Mode::parse("yes"), Some(Mode::Trace));
        assert_eq!(Mode::parse("exit"), Some(Mode::Exit));
        assert_eq!(Mode::parse(" EXIT\n"), Some(Mode::Exit));
        assert_eq!(
            line(Duration::from_micros(104_733), "window created"),
            "glyphvane startup:   104.733 ms  window created"
        );
        let origin = Origin {
            at: Instant::now(),
            monotonic: Some(Duration::new(81_234, 567_890_123)),
            since_process_start: Some(Duration::from_millis(2)),
        };
        assert_eq!(
            first_line(&origin),
            "main() entered; CLOCK_MONOTONIC 81234.567890123 s; \
             process created ≈ 2 ms before (/proc, 10 ms steps)"
        );
    }
}

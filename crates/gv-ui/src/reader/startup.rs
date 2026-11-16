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
//! glyphvane startup:   117.954 ms  first frame with content: frame callback of the next frame
//! glyphvane startup:   128.312 ms  first frame with content presented (wp_presentation, refresh 6.944 ms)
//! //! ```
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
//!    request, a frame callback, and presented.
//!
//!    - **presented**: where the binary gave the trace a presentation probe
//!      (Wayland compositors with `wp_presentation`), the time the
//!      compositor reports the content frame turned into light. The line is
//!      printed when the answer arrives. A discarded frame is reported as such.
//!    - **frame callback**: the next frame request after one more frame
//!      (the same content, from GPUI's caches). The next frame request
//!      itself can answer a frame callback that an earlier commit asked for,
//!      before the content is on screen; the one after the extra frame is
//!      the compositor's frame callback for that frame on Wayland, elsewhere
//!      the display link's next tick. It is *not* the presentation time:
//!      on KWin it comes 10 to 16 ms (about a refresh) *before* the content
//!      frame is presented, and other compositors may differ.
//!      Without a probe it is the only end point the trace has.
//!
//! The first frame with content is that of the first document view drawn:
//! the Reading view (Preview and Split mode) or the Source view (Edit mode,
//! as for an untitled document recovered from the journal). A frame drawn
//! with no document view (a window with nothing to show) is marked as such;
//! when no document view follows within [`EMPTY_WAIT`], the trace ends
//! there with a line that says so.
//!
//! The trace ends with the presented frame; later documents are not
//! traced. With `GLYPHVANE_DEBUG_STARTUP=exit` the application then quits,
//! so a script can also time exec → exit.
//!
//! Unset, the trace costs one environment lookup at start and a check of an
//! empty cell per milestone.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::latency::PresentProbe;

/// How long the trace waits for the compositor's presentation answer.
const PRESENT_WAIT: Duration = Duration::from_secs(2);
/// How often it looks for that answer.
const PRESENT_POLL: Duration = Duration::from_millis(1);
/// How long the trace waits for a document view after a frame drawn
/// without one (documents recovered from the journal open a little after
/// the window).
pub const EMPTY_WAIT: Duration = Duration::from_secs(2);

/// Set by the first frame with content of the process.
static PAINTED: AtomicBool = AtomicBool::new(false);
/// Set by the first frame drawn with no document view.
static EMPTY_PAINTED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static PROBE: RefCell<Option<Box<dyn PresentProbe>>> = const { RefCell::new(None) };
}

/// Gives the trace a presentation probe for the first window (the binary
/// makes one per platform). Does nothing when the trace is off.
pub fn set_probe(probe: Box<dyn PresentProbe>) {
    if enabled() {
        PROBE.with(|p| *p.borrow_mut() = Some(probe));
    }
}

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

/// Prints the milestone `what` that happened at `at`.
fn mark_at(at: Instant, what: &str) {
    if let Some(trace) = active() {
        eprintln!(
            "{}",
            line(at.saturating_duration_since(trace.origin.at), what)
        );
    }
}

fn presented_what(refresh: Option<Duration>) -> String {
    match refresh {
        Some(r) => format!(
            "first frame with content presented (wp_presentation, refresh {:.3} ms)",
            r.as_secs_f64() * 1e3
        ),
        None => "first frame with content presented (wp_presentation)".to_owned(),
    }
}

fn line(elapsed: Duration, what: &str) -> String {
    format!(
        "glyphvane startup: {:>9.3} ms  {what}",
        elapsed.as_secs_f64() * 1e3
    )
}

/// The first frame with content was painted: marks it, then marks the
/// submit, the frame callback and (with a probe) the presentation that
/// follow, and ends the trace (quitting in [`Mode::Exit`]). Does nothing
/// after the first time.
pub(crate) fn first_content_painted(what: &str, window: &mut gpui::Window, cx: &mut gpui::App) {
    let Some(trace) = active() else {
        return;
    };
    // Only the first content frame of the process.
    if PAINTED.swap(true, Ordering::Relaxed) {
        return;
    }
    mark(&format!("first frame with content painted ({what})"));
    // Ask for the presentation time of the frame being drawn: the request
    // applies to the surface's next commit, GPUI's present of this frame.
    let probed = PROBE.with(|p| {
        p.borrow_mut()
            .as_mut()
            .map(|probe| probe.request(window, 0))
            .is_some()
    });
    // The trace ends when the frame callback and the presentation answer
    // (when one is coming) are both in.
    let open = Rc::new(Cell::new(if probed { 2u8 } else { 1 }));
    let done = Rc::new(move |cx: &mut gpui::App| {
        open.set(open.get().saturating_sub(1));
        if open.get() == 0 {
            finish(trace, cx);
        }
    });
    // Deferred work runs when the update that draws the frame ends, which
    // is after GPUI presented it to the GPU surface.
    cx.defer(|_| mark("first frame with content submitted"));
    if probed {
        let done = done.clone();
        cx.spawn(async move |cx| {
            let start = Instant::now();
            let answer = loop {
                cx.background_executor().timer(PRESENT_POLL).await;
                let answer = PROBE.with(|p| {
                    p.borrow_mut()
                        .as_mut()
                        .and_then(|probe| probe.poll().into_iter().find(|(f, _)| *f == 0))
                });
                if let Some((_, answer)) = answer {
                    break Some(answer);
                }
                if start.elapsed() > PRESENT_WAIT {
                    break None;
                }
            };
            match answer {
                Some(Some((at, refresh))) => mark_at(at, &presented_what(refresh)),
                Some(None) => mark("first frame with content discarded by the compositor"),
                None => mark("first frame with content: no presentation answer"),
            }
            cx.update(|cx| done(cx));
        })
        .detach();
    }
    window.on_next_frame(move |window, _| {
        mark("next frame request");
        // This request may answer a frame callback asked for by an earlier
        // commit (GPUI on Wayland keeps one pending callback), so it can come
        // before the content reached the screen. Draw once more: that frame
        // asks for its own callback, which the compositor sends once it has
        // shown it.
        window.refresh();
        window.on_next_frame(move |_, cx| {
            mark("first frame with content: frame callback of the next frame");
            done(cx);
        });
    });
}

/// Ends the trace, quitting in [`Mode::Exit`].
fn finish(trace: &Trace, cx: &mut gpui::App) {
    trace.finished.store(true, Ordering::Relaxed);
    PROBE.with(|p| p.borrow_mut().take());
    if trace.mode == Mode::Exit {
        cx.quit();
    }
}

/// A frame was painted with no document view (the window has nothing to
/// show yet): marks it the first time, and ends the trace when no frame
/// with content follows within [`EMPTY_WAIT`].
pub(crate) fn empty_painted(cx: &mut gpui::App) {
    if active().is_none()
        || PAINTED.load(Ordering::Relaxed)
        || EMPTY_PAINTED.swap(true, Ordering::Relaxed)
    {
        return;
    }
    mark("first frame painted with no document view (nothing to show)");
    cx.spawn(async move |cx| {
        cx.background_executor().timer(EMPTY_WAIT).await;
        if let Some(trace) = active()
            && !PAINTED.load(Ordering::Relaxed)
        {
            mark(&empty_end_what());
            cx.update(|cx| finish(trace, cx));
        }
    })
    .detach();
}

fn empty_end_what() -> String {
    format!(
        "no document view drawn within {} ms of the first frame: the trace ends without a frame with content",
        EMPTY_WAIT.as_millis()
    )
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
            presented_what(Some(Duration::from_micros(6_944))),
            "first frame with content presented (wp_presentation, refresh 6.944 ms)"
        );
        assert_eq!(
            empty_end_what(),
            "no document view drawn within 2000 ms of the first frame: \
             the trace ends without a frame with content"
        );
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

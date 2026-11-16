// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! A debug log of end-to-end keystroke latency: from the keystroke to the
//! frame that shows it on screen.
//!
//! The binary turns it on (`GLYPHVANE_DEBUG_LATENCY`, see the binary's
//! `latency` module) with [`start`]. Every keystroke then gets one record,
//! with these times:
//!
//! - **input**: when the keystroke reached the application: the window's
//!   keystroke interceptor, the first application code that sees a key.
//!   GPUI does not pass on the platform's event time (Wayland's
//!   `wl_keyboard.key` time and X11's event time are dropped), so the time
//!   the platform delivered the event to the process is not known; for
//!   keystrokes the binary injects itself it is the injection time.
//! - **handled**: the Source view applied the edit (its input handler or a
//!   command that edits, before the reparse budget and the redraw request).
//! - **drawn**: the end of the Source view's paint in the frame that GPUI
//!   then presents. Draws that are not presented (GPUI draws a dirty window
//!   before it dispatches a key, to know where the focus is) do not count.
//! - **submitted**: the window's update that drew and presented the frame
//!   ended: the frame is handed to the GPU and queued for presentation.
//! - **frame callback**: GPUI's next frame request for the window. On
//!   Wayland that is the compositor's frame callback for the commit of the
//!   drawn frame (GPUI asks for one with each presented frame and does not
//!   draw again before it arrives), sent when the compositor has used the
//!   frame for an output refresh. On other platforms it is the next tick of
//!   the platform's frame source.
//! - **presented**: when the compositor says the frame turned into light,
//!   where a [`PresentProbe`] can ask it (Wayland's `wp_presentation`), with
//!   the output's refresh interval.
//!
//! A record is written once its frame is presented (or the probe gives
//! up), as one tab-separated line: see [`HEADER`]. Times are milliseconds
//! after **input**; `-` is a time that is not known.
//!
//! Off, the hooks cost one relaxed atomic load each.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{App, KeystrokeEvent, Window};

/// The first line of the log; the columns of every line after it.
pub const HEADER: &str =
    "# seq\tkey\tactive\tat_ms\thandled\tdrawn\tsubmitted\tframe_callback\tpresented\trefresh";

/// A probe's answer for a frame: the frame, and when it was presented with
/// the output's refresh interval, or `None` when the compositor discarded
/// it.
pub type Answer = (u64, Option<(Instant, Option<Duration>)>);

/// Asks the compositor when frames are presented.
pub trait PresentProbe {
    /// Ask for the presentation time of the frame that `window` presents
    /// next, which GPUI is drawing. `frame` names it in [`Self::poll`].
    fn request(&mut self, window: &Window, frame: u64);

    /// The answers that arrived since the last call.
    fn poll(&mut self) -> Vec<Answer>;
}

static ON: AtomicBool = AtomicBool::new(false);

/// Whether the log is on.
#[inline]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
struct Record {
    seq: u64,
    key: String,
    active: bool,
    input: Instant,
    handled: Option<Instant>,
    drawn: Option<Instant>,
    frame: Option<u64>,
    submitted: Option<Instant>,
    frame_callback: Option<Instant>,
    presented: Option<Instant>,
    refresh: Option<Duration>,
    /// The probe answered (presented or discarded).
    answered: bool,
}

struct State {
    out: Box<dyn Write>,
    probe: Option<Box<dyn PresentProbe>>,
    origin: Option<Instant>,
    next_seq: u64,
    next_frame: u64,
    /// GPUI started a frame request and has not drawn yet: the next paint
    /// is presented.
    in_frame_request: bool,
    /// A next-frame callback that opens a frame request is registered.
    armed: bool,
    pending: VecDeque<Record>,
    done: Vec<Record>,
    editor_painted: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Turns the log on: records go to `out`, presentation times come from
/// `probe` when there is one (see [`set_probe`]).
pub fn start(out: Box<dyn Write>, cx: &mut App) {
    let mut out = out;
    let _ = writeln!(out, "{HEADER}");
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            out,
            probe: None,
            origin: None,
            next_seq: 0,
            next_frame: 0,
            in_frame_request: false,
            armed: false,
            pending: VecDeque::new(),
            done: Vec::new(),
            editor_painted: false,
        });
    });
    ON.store(true, Ordering::Relaxed);
    cx.intercept_keystrokes(|event, window, _| keystroke(event, window))
        .detach();
}

/// Sets the presentation probe (the binary makes one per platform).
pub fn set_probe(probe: Box<dyn PresentProbe>) {
    with(|s| s.probe = Some(probe));
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

fn keystroke(event: &KeystrokeEvent, window: &mut Window) {
    let now = Instant::now();
    let arm = with(|s| {
        // Input is never dispatched inside a frame request.
        s.in_frame_request = false;
        let seq = s.next_seq;
        s.next_seq += 1;
        s.origin.get_or_insert(now);
        s.pending.push_back(Record {
            seq,
            key: event.keystroke.key.clone(),
            active: window.is_window_active(),
            input: now,
            handled: None,
            drawn: None,
            frame: None,
            submitted: None,
            frame_callback: None,
            presented: None,
            refresh: None,
            answered: false,
        });
        !std::mem::replace(&mut s.armed, true)
    });
    if arm == Some(true) {
        window.on_next_frame(|_, _| {
            with(|s| {
                s.armed = false;
                s.in_frame_request = true;
            });
        });
    }
}

/// Hook: the Source view applied an edit.
#[inline]
pub(crate) fn handled() {
    if enabled() {
        let now = Instant::now();
        with(|s| {
            if let Some(r) = s.pending.iter_mut().rev().find(|r| r.drawn.is_none()) {
                r.handled.get_or_insert(now);
            }
        });
    }
}

/// Hook: the end of the Source view's paint.
#[inline]
pub(crate) fn painted(window: &mut Window, cx: &mut App) {
    if enabled() {
        painted_slow(window, cx);
    }
}

fn painted_slow(window: &mut Window, cx: &mut App) {
    let now = Instant::now();
    let frame = with(|s| {
        s.editor_painted = true;
        if !std::mem::take(&mut s.in_frame_request) {
            return None;
        }
        let frame = s.next_frame;
        let mut any = false;
        for r in s
            .pending
            .iter_mut()
            .filter(|r| r.handled.is_some() && r.drawn.is_none())
        {
            r.drawn = Some(now);
            r.frame = Some(frame);
            any = true;
        }
        if !any {
            return None;
        }
        s.next_frame += 1;
        if let Some(probe) = s.probe.as_mut() {
            probe.request(window, frame);
        }
        Some(frame)
    })
    .flatten();
    let Some(frame) = frame else {
        return;
    };
    // Runs when the update that draws and presents this frame ends.
    window.defer(cx, move |_, _| {
        let now = Instant::now();
        with(|s| {
            for r in s.pending.iter_mut().filter(|r| r.frame == Some(frame)) {
                r.submitted = Some(now);
            }
        });
    });
    window.on_next_frame(move |_, _| {
        let now = Instant::now();
        with(|s| {
            for r in s.pending.iter_mut().filter(|r| r.frame == Some(frame)) {
                r.frame_callback = Some(now);
            }
        });
        poll();
    });
}

/// Whether a Source view has painted since the log started.
pub fn editor_painted() -> bool {
    with(|s| s.editor_painted).unwrap_or(false)
}

/// Collects the probe's answers and writes the records that are complete.
/// The binary calls it on a timer, since answers arrive between frames.
pub fn poll() {
    with(|s| {
        let answers = s.probe.as_mut().map(|p| p.poll()).unwrap_or_default();
        for (frame, answer) in answers {
            for r in s.pending.iter_mut().filter(|r| r.frame == Some(frame)) {
                r.answered = true;
                if let Some((at, refresh)) = answer {
                    r.presented = Some(at);
                    r.refresh = refresh;
                }
            }
        }
        let has_probe = s.probe.is_some();
        let now = Instant::now();
        while let Some(r) = s.pending.front() {
            let complete = r.frame_callback.is_some() && (r.answered || !has_probe);
            // A key that edits nothing (a modifier, a command), or a frame
            // whose answer never came.
            let stale = now.duration_since(r.input) > Duration::from_secs(2);
            if !(complete || stale) {
                break;
            }
            let Some(r) = s.pending.pop_front() else {
                break;
            };
            if r.handled.is_some() {
                let origin = s.origin.unwrap_or(r.input);
                let _ = writeln!(s.out, "{}", line(&r, origin));
                s.done.push(r);
            }
        }
        let _ = s.out.flush();
    });
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn after(r: &Record, t: Option<Instant>) -> String {
    t.map_or_else(
        || "-".to_owned(),
        |t| format!("{:.3}", ms(t.saturating_duration_since(r.input))),
    )
}

fn line(r: &Record, origin: Instant) -> String {
    format!(
        "{}\t{}\t{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}",
        r.seq,
        r.key,
        u8::from(r.active),
        ms(r.input.saturating_duration_since(origin)),
        after(r, r.handled),
        after(r, r.drawn),
        after(r, r.submitted),
        after(r, r.frame_callback),
        after(r, r.presented),
        r.refresh
            .map_or_else(|| "-".to_owned(), |d| format!("{:.3}", ms(d))),
    )
}

/// Writes every remaining record and a summary of the run (skipping the
/// first `warm_up` records) to the log, and returns the summary.
pub fn finish(warm_up: usize) -> String {
    poll();
    with(|s| {
        let records: Vec<String> = s
            .done
            .iter()
            .map(|r| line(r, s.origin.unwrap_or(r.input)))
            .collect();
        let summary = summary(&records, warm_up);
        for l in summary.lines() {
            let _ = writeln!(s.out, "# {l}");
        }
        let _ = s.out.flush();
        summary
    })
    .unwrap_or_default()
}

/// Percentiles of each column of the log `lines` (records only; comments
/// are skipped), after the first `warm_up` records.
pub fn summary(lines: &[String], warm_up: usize) -> String {
    const COLUMNS: [(usize, &str); 5] = [
        (4, "input → handled"),
        (5, "input → drawn"),
        (6, "input → submitted"),
        (7, "input → frame callback"),
        (8, "input → presented"),
    ];
    let rows: Vec<Vec<&str>> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|f| f.len() >= 10)
        .skip(warm_up)
        .collect();
    let mut out = String::new();
    let inactive = rows.iter().filter(|f| f[2] == "0").count();
    let _ = writeln!(
        out,
        "{} keystrokes ({} warm-up skipped){}",
        rows.len(),
        warm_up,
        if inactive > 0 {
            format!("; {inactive} while the window was not active (frames may be throttled)")
        } else {
            String::new()
        }
    );
    for (col, name) in COLUMNS {
        let mut v: Vec<f64> = rows.iter().filter_map(|f| f[col].parse().ok()).collect();
        if v.is_empty() {
            let _ = writeln!(out, "{name}: no samples");
            continue;
        }
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        let _ = writeln!(
            out,
            "{name}: p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms (n {})",
            at(0.5),
            at(0.95),
            at(0.99),
            at(1.0),
            v.len()
        );
    }
    let refreshes: Vec<(f64, f64)> = rows
        .iter()
        .filter_map(|f| Some((f[8].parse().ok()?, f[9].parse().ok()?)))
        .collect();
    if !refreshes.is_empty() {
        let within = refreshes.iter().filter(|(p, r)| p <= r).count();
        let mut frames: Vec<f64> = refreshes.iter().map(|(p, r)| p / r).collect();
        frames.sort_by(f64::total_cmp);
        let at = |q: f64| frames[((frames.len() - 1) as f64 * q).round() as usize];
        let _ = writeln!(
            out,
            "input → presented in refreshes ({:.3} ms): p50 {:.2}, p95 {:.2}, p99 {:.2}; within 1 refresh: {} of {} ({:.1} %)",
            refreshes[0].1,
            at(0.5),
            at(0.95),
            at(0.99),
            within,
            refreshes.len(),
            100.0 * within as f64 / refreshes.len() as f64
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_a_log() {
        let mut lines = vec![HEADER.to_owned()];
        for i in 0..100 {
            let p = 5.0 + f64::from(i) * 0.1;
            lines.push(format!(
                "{i}\ta\t1\t{i}.000\t0.050\t0.900\t1.000\t{:.3}\t{p:.3}\t13.333",
                p - 1.0
            ));
        }
        lines.push("100\tb\t0\t100.000\t-\t-\t-\t-\t20.000\t13.333".to_owned());
        let s = summary(&lines, 0);
        assert!(
            s.starts_with("101 keystrokes (0 warm-up skipped); 1 while"),
            "{s}"
        );
        assert!(s.contains("input → handled: p50 0.05 ms"), "{s}");
        assert!(s.contains("input → presented: p50 10.00 ms"), "{s}");
        assert!(s.contains("within 1 refresh: 84 of 101"), "{s}");
        assert!(summary(&lines, 200).contains("input → drawn: no samples"));
    }

    #[test]
    fn writes_unknown_times_as_dashes() {
        let t = Instant::now();
        let r = Record {
            seq: 3,
            key: "space".into(),
            active: true,
            input: t,
            handled: Some(t + Duration::from_micros(40)),
            drawn: None,
            frame: None,
            submitted: None,
            frame_callback: None,
            presented: None,
            refresh: None,
            answered: false,
        };
        assert_eq!(line(&r, t), "3\tspace\t1\t0.000\t0.040\t-\t-\t-\t-\t-");
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The binary's side of the keystroke latency log (`gv_ui::latency`).
//!
//! - `GLYPHVANE_DEBUG_LATENCY=FILE` writes one record per keystroke to
//!   `FILE` (`-` or `1`: standard error). Type in a Source view; the summary
//!   is printed when the application quits. `cargo xtask latency FILE`
//!   summarises a log.
//! - `GLYPHVANE_DEBUG_LATENCY_TYPE=COUNT[:INTERVAL_MS]` also types `COUNT`
//!   keystrokes into the first window, one every `INTERVAL_MS` (default 47)
//!   plus up to a quarter of that at random, once a Source view has painted
//!   (open a file with `GLYPHVANE_DEBUG_EDIT=1`), then prints the summary
//!   and quits. The keystrokes go through the window's normal key dispatch,
//!   not through the platform, so the time the platform takes to deliver a
//!   key is not in the numbers.
//!
//! On Wayland the compositor's presentation feedback (`wp_presentation`)
//! gives the time each frame was presented, on GPUI's own connection. On
//! X11 and elsewhere the log has no presentation time; the frame callback
//! column is the closest stand-in.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::time::Duration;

use gpui::{AnyWindowHandle, App, Keystroke};

/// The environment variable that turns the log on.
pub const DEBUG_LATENCY_ENV: &str = "GLYPHVANE_DEBUG_LATENCY";
/// The environment variable that types keystrokes and quits.
pub const DEBUG_LATENCY_TYPE_ENV: &str = "GLYPHVANE_DEBUG_LATENCY_TYPE";

/// Records left out of the summary: the first frames shape new glyphs.
const WARM_UP: usize = 20;
/// How often the presentation answers are collected.
const POLL: Duration = Duration::from_millis(50);
/// The keys typed, in turn.
const KEYS: [&str; 8] = ["a", "b", "c", "space", "x", "y", "z", "space"];

/// An automatic typing run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Typing {
    count: usize,
    interval: Duration,
}

impl Typing {
    fn parse(spec: &str) -> Option<Self> {
        let (count, interval) = match spec.split_once(':') {
            Some((c, i)) => (c, i.trim().parse().ok()?),
            None => (spec, 47),
        };
        Some(Self {
            count: count.trim().parse().ok()?,
            interval: Duration::from_millis(interval),
        })
    }
}

/// Starts the log when the environment asks for it. Call once, inside
/// `Application::run`, before opening a window.
pub fn install(cx: &mut App) {
    let typing = std::env::var(DEBUG_LATENCY_TYPE_ENV)
        .ok()
        .and_then(|s| Typing::parse(&s));
    let target = match std::env::var_os(DEBUG_LATENCY_ENV) {
        Some(t) if !t.is_empty() && t != "0" => Some(t),
        _ if typing.is_some() => Some("-".into()),
        _ => None,
    };
    let Some(target) = target else {
        return;
    };
    let out: Box<dyn Write> = if target == "-" || target == "1" {
        Box::new(io::stderr())
    } else {
        match File::create(&target) {
            Ok(file) => Box::new(BufWriter::new(file)),
            Err(err) => {
                eprintln!(
                    "glyphvane: {DEBUG_LATENCY_ENV}: cannot create {}: {err}",
                    target.to_string_lossy()
                );
                return;
            }
        }
    };
    gv_ui::latency::start(out, cx);
    cx.spawn(async |cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            cx.update(|_| gv_ui::latency::poll());
        }
    })
    .detach();
    cx.on_app_quit(|_| {
        if std::env::var_os(DEBUG_LATENCY_TYPE_ENV).is_none() {
            eprint!("{}", gv_ui::latency::finish(WARM_UP));
        }
        async {}
    })
    .detach();
}

/// Attaches the presentation probe to `window` and starts the typing run,
/// when the log is on.
pub fn attach(window: AnyWindowHandle, cx: &mut App) {
    if !gv_ui::latency::enabled() {
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let probe = window
            .update(cx, |_, window, _| wayland::Probe::new(window))
            .ok()
            .flatten();
        match probe {
            Some(probe) => gv_ui::latency::set_probe(Box::new(probe)),
            None => eprintln!(
                "glyphvane: {DEBUG_LATENCY_ENV}: no Wayland presentation feedback; \
                 the log has no presentation times"
            ),
        }
    }
    let Some(typing) = std::env::var(DEBUG_LATENCY_TYPE_ENV)
        .ok()
        .and_then(|s| Typing::parse(&s))
    else {
        return;
    };
    cx.spawn(async move |cx| {
        let mut waited = Duration::ZERO;
        while !cx.update(|_| gv_ui::latency::editor_painted()) {
            cx.background_executor().timer(POLL).await;
            waited += POLL;
            if waited > Duration::from_secs(10) {
                eprintln!(
                    "glyphvane: {DEBUG_LATENCY_TYPE_ENV}: no Source view painted; \
                     open a file with GLYPHVANE_DEBUG_EDIT=1"
                );
                cx.update(|cx| cx.quit());
                return;
            }
        }
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        // A fixed seed: runs type the same keys at the same offsets.
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let quarter = u64::try_from(typing.interval.as_micros() / 4).unwrap_or(0);
        for i in 0..typing.count {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let jitter = Duration::from_micros(seed % quarter.max(1));
            cx.background_executor()
                .timer(typing.interval + jitter)
                .await;
            let Ok(key) = Keystroke::parse(KEYS[i % KEYS.len()]) else {
                continue;
            };
            let typed = window.update(cx, |_, window, cx| {
                window.dispatch_keystroke(key, cx);
            });
            if typed.is_err() {
                return;
            }
        }
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        let summary = cx.update(|_| gv_ui::latency::finish(WARM_UP));
        eprint!("{summary}");
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[allow(unsafe_code)] // Two foreign Wayland pointers, see `Probe::new`.
mod wayland {
    use std::mem::ManuallyDrop;
    use std::time::{Duration, Instant};

    use gpui::Window;
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use wayland_backend::client::{Backend, ObjectId};
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
    use wayland_protocols::wp::presentation_time::client::{
        wp_presentation::{self, WpPresentation},
        wp_presentation_feedback::{self, WpPresentationFeedback},
    };

    /// `CLOCK_MONOTONIC`, the clock `Instant` uses on Linux.
    const CLOCK_MONOTONIC: u32 = 1;

    use gv_ui::latency::Answer;

    #[derive(Debug, Default)]
    struct State {
        clock: Option<u32>,
        answers: Vec<Answer>,
    }

    /// Presentation feedback for one window's surface, on GPUI's Wayland
    /// connection with an event queue of its own.
    ///
    /// The Wayland objects are never dropped: GPUI may have closed the
    /// connection by the time the probe would be, and destroying a proxy
    /// then is a use after free.
    pub struct Probe {
        conn: ManuallyDrop<Connection>,
        queue: ManuallyDrop<EventQueue<State>>,
        qh: QueueHandle<State>,
        presentation: ManuallyDrop<WpPresentation>,
        surface: ManuallyDrop<WlSurface>,
        state: State,
    }

    impl std::fmt::Debug for Probe {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Probe")
                .field("state", &self.state)
                .finish_non_exhaustive()
        }
    }

    impl Probe {
        /// `None` when the window is not on Wayland or the compositor has
        /// no `wp_presentation`.
        pub fn new(window: &Window) -> Option<Self> {
            let RawDisplayHandle::Wayland(display) =
                HasDisplayHandle::display_handle(window).ok()?.as_raw()
            else {
                return None;
            };
            let RawWindowHandle::Wayland(handle) =
                HasWindowHandle::window_handle(window).ok()?.as_raw()
            else {
                return None;
            };
            // SAFETY: GPUI's display stays connected while its windows are
            // open, and the probe's objects are never dropped (see `Probe`),
            // so nothing uses the display after GPUI closes it except
            // through the timer that polls, which stops with the app.
            let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
            let conn = Connection::from_backend(backend);
            // SAFETY: the window's surface lives as long as the window; the
            // probe only names it in feedback requests while GPUI draws
            // that window.
            let id = unsafe {
                ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast())
            }
            .ok()?;
            let surface = WlSurface::from_id(&conn, id).ok()?;
            let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
            let qh = queue.handle();
            let presentation: WpPresentation = globals.bind(&qh, 1..=1, ()).ok()?;
            let mut state = State::default();
            queue.roundtrip(&mut state).ok()?;
            if state.clock != Some(CLOCK_MONOTONIC) {
                eprintln!(
                    "glyphvane: the compositor's presentation clock is {:?}, not CLOCK_MONOTONIC; \
                     no presentation times",
                    state.clock
                );
                return None;
            }
            Some(Self {
                conn: ManuallyDrop::new(conn),
                queue: ManuallyDrop::new(queue),
                qh,
                presentation: ManuallyDrop::new(presentation),
                surface: ManuallyDrop::new(surface),
                state,
            })
        }
    }

    impl gv_ui::latency::PresentProbe for Probe {
        fn request(&mut self, _window: &Window, frame: u64) {
            // Applies to the surface's next commit: GPUI's present of the
            // frame it is drawing, which also flushes the request.
            self.presentation.feedback(&self.surface, &self.qh, frame);
        }

        fn poll(&mut self) -> Vec<Answer> {
            let _ = self.conn.flush();
            let _ = self.queue.dispatch_pending(&mut self.state);
            std::mem::take(&mut self.state.answers)
        }
    }

    /// The `Instant` of a CLOCK_MONOTONIC time.
    fn instant_of(sec: u64, nsec: u32) -> Option<Instant> {
        let then = Duration::new(sec, nsec);
        let now_instant = Instant::now();
        let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let now = Duration::new(
            u64::try_from(ts.tv_sec).ok()?,
            u32::try_from(ts.tv_nsec).ok()?,
        );
        match now.checked_sub(then) {
            Some(ago) => now_instant.checked_sub(ago),
            None => now_instant.checked_add(then - now),
        }
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<WpPresentation, ()> for State {
        fn event(
            state: &mut Self,
            _: &WpPresentation,
            event: wp_presentation::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wp_presentation::Event::ClockId { clk_id } = event {
                state.clock = Some(clk_id);
            }
        }
    }

    impl Dispatch<WpPresentationFeedback, u64> for State {
        fn event(
            state: &mut Self,
            _: &WpPresentationFeedback,
            event: wp_presentation_feedback::Event,
            frame: &u64,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                wp_presentation_feedback::Event::Presented {
                    tv_sec_hi,
                    tv_sec_lo,
                    tv_nsec,
                    refresh,
                    ..
                } => {
                    let sec = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                    let at = instant_of(sec, tv_nsec);
                    let refresh = (refresh > 0).then(|| Duration::from_nanos(u64::from(refresh)));
                    state.answers.push((*frame, at.map(|at| (at, refresh))));
                }
                wp_presentation_feedback::Event::Discarded => {
                    state.answers.push((*frame, None));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_typing_spec() {
        assert_eq!(
            Typing::parse("400"),
            Some(Typing {
                count: 400,
                interval: Duration::from_millis(47)
            })
        );
        assert_eq!(
            Typing::parse("10:100"),
            Some(Typing {
                count: 10,
                interval: Duration::from_millis(100)
            })
        );
        assert_eq!(Typing::parse("x"), None);
    }
}

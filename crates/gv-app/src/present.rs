// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! When frames reach the screen: the compositor's presentation feedback
//! (`wp_presentation`) for a window, on Wayland, behind the
//! [`gv_ui::latency::PresentProbe`] trait. The keystroke latency log and the
//! start-up trace use it; it is made only when one of them is on.

#![allow(unsafe_code)] // Two foreign Wayland pointers, see `Probe::new`.

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
        let id =
            unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }
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

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The audio player in headless windows, on a silent output that never
//! opens a device: loading, play and pause by click and Space, seeking by
//! keys and by dragging the bar, mute, and the error states.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext,
    WindowBounds, WindowOptions, point, px, size,
};
use gv_media::audio::{NullBackend, NullOutput, OutputFormat};

use super::{AudioEngine, AudioError, AudioPlayerEvent, AudioPlayerView, PlayerState, format_time};
use crate::{AppSettings, ThemeChoice};

/// A 48 kHz stereo 16-bit WAV of `frames` frames of a quiet ramp.
fn wav(dir: &Path, name: &str, frames: usize) -> PathBuf {
    let mut data = Vec::with_capacity(frames * 4);
    for f in 0..frames {
        let v = (f % 1000) as i16 + 1;
        data.extend_from_slice(&v.to_le_bytes());
        data.extend_from_slice(&(-v).to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    let path = dir.join(name);
    std::fs::write(&path, out).expect("writes the test file");
    path
}

fn draw(vcx: &mut VisualTestContext) {
    for _ in 0..2 {
        vcx.run_until_parked();
        vcx.update(|window, cx| window.draw(cx).clear(cx));
    }
}

struct Fixture {
    view: Entity<AudioPlayerView>,
    vcx: VisualTestContext,
    output: NullOutput,
    events: Rc<RefCell<Vec<AudioPlayerEvent>>>,
}

/// A window holding only a player of `path`, on a silent output (or one
/// without a device).
fn player(cx: &mut TestAppContext, path: PathBuf, with_device: bool) -> Fixture {
    let (backend, output) = if with_device {
        NullBackend::new(OutputFormat {
            sample_rate: 48_000,
            channels: 2,
        })
    } else {
        NullBackend::without_device()
    };
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(600.0), px(200.0)));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..WindowOptions::default()
    };
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&events);
    let (window, view) = cx.update(|cx| {
        crate::init(cx, AppSettings::new(None, Some(ThemeChoice::Light)));
        super::set_engine(cx, AudioEngine::new(backend));
        let (window, view) = gpui::open_window(options, cx, |_, cx| {
            cx.new(|cx| AudioPlayerView::new(path, cx))
        })
        .expect("test window opens");
        cx.subscribe(&view, move |_, event: &AudioPlayerEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
        (window, view)
    });
    let mut vcx = VisualTestContext::from_window(window, cx);
    draw(&mut vcx);
    Fixture {
        view,
        vcx,
        output,
        events,
    }
}

impl Fixture {
    /// Runs the window until `cond` holds; the decoder runs on its own
    /// thread, so this also waits in real time.
    fn wait(&mut self, what: &str, cond: impl Fn(&AudioPlayerView) -> bool) {
        let start = Instant::now();
        loop {
            self.vcx.executor().advance_clock(super::LOADING_TICK);
            draw(&mut self.vcx);
            if self.view.read_with(&self.vcx, |v, _| cond(v)) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn state(&self) -> PlayerState {
        self.view.read_with(&self.vcx, |v, _| v.state().clone())
    }

    fn label(&self) -> String {
        self.view
            .read_with(&self.vcx, |v, _| v.time_label().to_string())
    }

    fn position(&self) -> Duration {
        self.view.read_with(&self.vcx, |v, _| v.player.position())
    }

    fn center_of(&mut self, selector: &'static str) -> gpui::Point<gpui::Pixels> {
        self.vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center()
    }

    fn focus(&mut self) {
        let view = self.view.clone();
        self.vcx.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        draw(&mut self.vcx);
    }

    fn keys(&mut self, keys: &str) {
        self.vcx.simulate_keystrokes(keys);
        draw(&mut self.vcx);
    }
}

#[gpui::test]
fn loads_plays_and_pauses_by_click(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = wav(dir.path(), "a.wav", 96_000);
    let mut f = player(cx, path, true);
    f.wait("loaded", |v| *v.state() == PlayerState::Stopped);
    assert_eq!(
        f.view.read_with(&f.vcx, |v, _| v.duration()),
        Some(Duration::from_secs(2))
    );
    assert_eq!(f.label(), "0:00 / 0:02");
    assert_eq!(f.output.opens(), 0, "no device until the first play");

    let play = f.center_of("audio-play");
    f.vcx.simulate_click(play, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.state(), PlayerState::Playing);
    assert!(f.output.is_running());

    // The output plays a second; the ticker brings the position to the view.
    for _ in 0..48 {
        f.output.pull(1_000);
        std::thread::sleep(Duration::from_millis(1));
    }
    f.vcx.executor().advance_clock(Duration::from_millis(500));
    draw(&mut f.vcx);
    let shown = f.view.read_with(&f.vcx, |v, _| v.position());
    assert!(shown > Duration::from_millis(100), "position {shown:?}");

    f.vcx.simulate_click(play, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.state(), PlayerState::Paused);
    assert!(!f.output.is_running());
    assert!(f.events.borrow().is_empty());
}

#[gpui::test]
fn keys_toggle_and_seek(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = wav(dir.path(), "a.wav", 48_000 * 12);
    let mut f = player(cx, path, true);
    f.wait("loaded", |v| *v.state() == PlayerState::Stopped);
    f.focus();

    f.keys("space");
    assert_eq!(f.state(), PlayerState::Playing);
    f.keys("space");
    assert_eq!(f.state(), PlayerState::Paused);

    f.keys("right");
    assert_eq!(f.position(), Duration::from_secs(5));
    f.keys("right right");
    assert_eq!(f.position(), Duration::from_secs(12), "clamped to the end");
    f.keys("left");
    assert_eq!(f.position(), Duration::from_secs(7));
    f.keys("home");
    assert_eq!(f.position(), Duration::ZERO);
    f.keys("end");
    assert_eq!(f.position(), Duration::from_secs(12));
    assert_eq!(f.label(), "0:12 / 0:12");
}

#[gpui::test]
fn dragging_the_bar_seeks_on_release(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = wav(dir.path(), "a.wav", 48_000 * 8);
    let mut f = player(cx, path, true);
    f.wait("loaded", |v| *v.state() == PlayerState::Stopped);
    let bar = f.vcx.debug_bounds("audio-seek").expect("the bar is drawn");
    let at = |fraction: f32| {
        point(
            bar.origin.x + bar.size.width * fraction,
            bar.origin.y + bar.size.height / 2.0,
        )
    };

    f.vcx
        .simulate_mouse_down(at(0.25), MouseButton::Left, Modifiers::none());
    draw(&mut f.vcx);
    f.vcx
        .simulate_mouse_move(at(0.5), Some(MouseButton::Left), Modifiers::none());
    draw(&mut f.vcx);
    // Not seeked yet: the bar and the time follow the drag.
    assert_eq!(f.position(), Duration::ZERO);
    assert_eq!(f.label(), "0:04 / 0:08");
    // Past the end of the bar clamps.
    f.vcx.simulate_mouse_move(
        point(bar.origin.x + bar.size.width * 2.0, bar.origin.y),
        Some(MouseButton::Left),
        Modifiers::none(),
    );
    draw(&mut f.vcx);
    f.vcx
        .simulate_mouse_move(at(0.75), Some(MouseButton::Left), Modifiers::none());
    draw(&mut f.vcx);
    f.vcx
        .simulate_mouse_up(at(0.75), MouseButton::Left, Modifiers::none());
    draw(&mut f.vcx);
    let pos = f.position().as_secs_f64();
    assert!((pos - 6.0).abs() < 0.05, "seeked to {pos}");
    assert!(f.view.read_with(&f.vcx, |v, _| v.drag.is_none()));
    assert_eq!(f.state(), PlayerState::Paused);
}

#[gpui::test]
fn mute_toggles_the_volume(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = wav(dir.path(), "a.wav", 4_800);
    let mut f = player(cx, path, true);
    f.wait("loaded", |v| *v.state() == PlayerState::Stopped);
    let mute = f.center_of("audio-mute");
    f.vcx.simulate_click(mute, Modifiers::none());
    draw(&mut f.vcx);
    assert!(f.view.read_with(&f.vcx, |v, _| v.is_muted()));
    assert_eq!(f.view.read_with(&f.vcx, |v, _| v.player.volume()), 0.0);
    f.vcx.simulate_click(mute, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.view.read_with(&f.vcx, |v, _| v.player.volume()), 1.0);
}

#[gpui::test]
fn unsupported_file_reports_failure(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("x.mp3");
    std::fs::write(&path, vec![0x5a; 2048]).expect("writes the test file");
    let mut f = player(cx, path, true);
    f.wait("error", |v| v.error().is_some());
    assert_eq!(
        f.events.borrow().as_slice(),
        [AudioPlayerEvent::Failed(AudioError::UnsupportedFormat)]
    );
    assert!(f.vcx.debug_bounds("audio-status").is_some());
    // The button does nothing.
    let play = f.center_of("audio-play");
    f.vcx.simulate_click(play, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.state(), PlayerState::Error(AudioError::UnsupportedFormat));
    assert_eq!(f.output.opens(), 0);
}

#[gpui::test]
fn missing_device_shows_an_error_and_retries(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = wav(dir.path(), "a.wav", 4_800);
    let mut f = player(cx, path, false);
    f.wait("loaded", |v| *v.state() == PlayerState::Stopped);
    let play = f.center_of("audio-play");
    f.vcx.simulate_click(play, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.state(), PlayerState::Error(AudioError::NoOutputDevice));
    assert!(f.vcx.debug_bounds("audio-status").is_some());
    assert!(
        f.events.borrow().is_empty(),
        "output errors are not failures"
    );
    // Clicking again retries (and fails the same way).
    f.vcx.simulate_click(play, Modifiers::none());
    draw(&mut f.vcx);
    assert_eq!(f.state(), PlayerState::Error(AudioError::NoOutputDevice));
}

#[test]
fn time_format() {
    assert_eq!(format_time(Duration::ZERO), "0:00");
    assert_eq!(format_time(Duration::from_millis(65_900)), "1:05");
    assert_eq!(format_time(Duration::from_secs(3_600 + 62)), "1:01:02");
}

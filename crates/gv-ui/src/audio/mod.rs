// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The audio player of an audio embed (`![[x.mp3]]`, `<audio src>`;
//! FR-RENDER-07): play/pause, a seek bar that follows drags, the time, and a
//! mute toggle. Not designed: sizes and colours follow the controls of the
//! design (24 px icon buttons, `accent.default` for the played part,
//! `border.default` for the rest).
//!
//! Decoding and output are `gv_media::audio`'s. Every player of the
//! application shares one [`AudioEngine`] (a GPUI global, created on first
//! use; the output device opens on the first play), so starting one embed
//! pauses the one that was playing.
//!
//! Keys while the player has the focus (a click on it focuses it): Space
//! plays or pauses, Left and Right seek 5 seconds, Home goes to the start,
//! End to the end.
//!
//! Embedding (the media card): create the view once per embed and keep it
//! with the card, then place it as a child.
//!
//! ```ignore
//! let player = cx.new(|cx| AudioPlayerView::new(path, cx));
//! cx.subscribe(&player, |card, _, event: &AudioPlayerEvent, cx| match event {
//!     // Unsupported or broken file: show the card's "Open externally".
//!     AudioPlayerEvent::Failed(error) => card.show_fallback(error.message(), cx),
//! });
//! // In render: div().child(player.clone())
//! ```

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, Context, DispatchPhase, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, canvas, div, px,
};
use gv_media::audio::Player;
pub use gv_media::audio::{AudioEngine, AudioError, PlayerState};

use crate::appearance::Appearance;
use crate::icons::Icon;
use crate::metrics::size::icon_button;
use crate::metrics::{HAIRLINE, icon, radius, text};

/// The key context of a player.
pub const KEY_CONTEXT: &str = "AudioPlayer";

gpui::actions!(
    audio_player,
    [
        /// Play or pause the focused player.
        TogglePlay,
        /// Go to the start.
        SeekStart,
        /// Go to the end.
        SeekEnd,
    ]
);

/// The player's keys that a surrounding view also binds (the Reading
/// view's Space, Home and End scroll): bound in the player's context so
/// they are the player's while it has the focus. Left and Right are read
/// as plain key presses.
pub fn key_bindings() -> Vec<gpui::KeyBinding> {
    let context = Some(KEY_CONTEXT);
    vec![
        gpui::KeyBinding::new("space", TogglePlay, context),
        gpui::KeyBinding::new("home", SeekStart, context),
        gpui::KeyBinding::new("end", SeekEnd, context),
    ]
}

/// How far Left and Right seek.
pub const SEEK_STEP: Duration = Duration::from_secs(5);

/// How often the time and the bar follow playback.
const TICK: Duration = Duration::from_millis(200);
/// How often a loading player is looked at.
const LOADING_TICK: Duration = Duration::from_millis(50);

/// Height of the seek bar's track and its hit area (not designed).
const TRACK_HEIGHT: f32 = 4.0;
const BAR_HIT_HEIGHT: f32 = 16.0;
/// Diameter of the seek bar's knob (not designed).
const KNOB: f32 = 10.0;
/// Gap between the row's parts (not designed).
const GAP: f32 = 8.0;

/// The application's audio engine.
struct AudioOutput(AudioEngine);

impl gpui::Global for AudioOutput {}

/// The engine every player uses: the one set with [`set_engine`], or the
/// system's default output, created on first use (no device is opened
/// until something plays).
pub fn engine(cx: &mut App) -> AudioEngine {
    if let Some(output) = cx.try_global::<AudioOutput>() {
        return output.0.clone();
    }
    let engine = AudioEngine::system();
    cx.set_global(AudioOutput(engine.clone()));
    engine
}

/// Replaces the engine for players created from now on (tests use a
/// `gv_media::audio::NullBackend`, which never opens a device).
pub fn set_engine(cx: &mut App, engine: AudioEngine) {
    cx.set_global(AudioOutput(engine));
}

/// What a player tells its embed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioPlayerEvent {
    /// The file cannot be played (unsupported, broken, unreadable). Output
    /// errors are not reported here: the player shows them and retries on
    /// the next play.
    Failed(AudioError),
}

/// The player of one audio embed.
pub struct AudioPlayerView {
    path: PathBuf,
    player: Player,
    state: PlayerState,
    duration: Option<Duration>,
    position: Duration,
    /// While the seek bar is dragged: where, as a fraction of the length.
    drag: Option<f32>,
    /// The seek bar's bounds, from the last paint.
    bar: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The volume before muting.
    unmuted: Option<f32>,
    focus: FocusHandle,
    /// The file was handed to the player (see [`Self::lazy`]).
    loaded: bool,
    /// The follow-up task is running.
    ticking: bool,
    ticker: Option<Task<()>>,
}

impl std::fmt::Debug for AudioPlayerView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPlayerView")
            .field("path", &self.path)
            .field("state", &self.state)
            .field("position", &self.position)
            .field("duration", &self.duration)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<AudioPlayerEvent> for AudioPlayerView {}

impl Focusable for AudioPlayerView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AudioPlayerView {
    /// A player for `path`, loading it at once on a background thread.
    pub fn new(path: impl Into<PathBuf>, cx: &mut Context<'_, Self>) -> Self {
        let mut view = Self::lazy(path, cx);
        view.ensure_loaded(cx);
        view
    }

    /// A player for `path` that reads nothing until the first play or
    /// seek: a note with many embeds decodes and parks a thread only for
    /// those that are played.
    pub fn lazy(path: impl Into<PathBuf>, cx: &mut Context<'_, Self>) -> Self {
        let path = path.into();
        let player = engine(cx).player();
        let mut view = Self {
            path,
            state: PlayerState::Empty,
            player,
            duration: None,
            position: Duration::ZERO,
            drag: None,
            bar: Rc::new(Cell::new(None)),
            unmuted: None,
            focus: cx.focus_handle(),
            loaded: false,
            ticking: false,
            ticker: None,
        };
        view.sync(cx);
        view
    }

    /// Hands the file to the player, once.
    fn ensure_loaded(&mut self, cx: &mut Context<'_, Self>) {
        if !self.loaded {
            self.loaded = true;
            self.player.load(&self.path);
            self.sync(cx);
        }
    }

    /// Whether the file has been handed to the player.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> &PlayerState {
        &self.state
    }

    /// The error that stops playback, if any.
    pub fn error(&self) -> Option<&AudioError> {
        match &self.state {
            PlayerState::Error(e) => Some(e),
            _ => None,
        }
    }

    pub fn position(&self) -> Duration {
        self.position
    }

    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub fn is_muted(&self) -> bool {
        self.unmuted.is_some()
    }

    pub fn toggle(&mut self, cx: &mut Context<'_, Self>) {
        self.ensure_loaded(cx);
        self.player.toggle();
        self.sync(cx);
    }

    pub fn pause(&mut self, cx: &mut Context<'_, Self>) {
        self.player.pause();
        self.sync(cx);
    }

    /// Seeks to `time`, clamped to the length.
    pub fn seek_to(&mut self, time: Duration, cx: &mut Context<'_, Self>) {
        let time = match self.duration {
            Some(d) => time.min(d),
            None => time,
        };
        self.player.seek(time);
        self.sync(cx);
    }

    /// Seeks by `delta` seconds (negative: back).
    pub fn seek_by(&mut self, delta: f64, cx: &mut Context<'_, Self>) {
        let now = self.player.position().as_secs_f64();
        let to = (now + delta).max(0.0);
        self.seek_to(Duration::from_secs_f64(to), cx);
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<'_, Self>) {
        match self.unmuted.take() {
            Some(volume) => self.player.set_volume(volume),
            None => {
                self.unmuted = Some(self.player.volume());
                self.player.set_volume(0.0);
            }
        }
        cx.notify();
    }

    /// Reads the player's state, position and length; reports a file that
    /// cannot play. Returns how soon to look again: while loading or
    /// playing, the player changes on its own.
    fn refresh(&mut self, cx: &mut Context<'_, Self>) -> Option<Duration> {
        let state = self.player.state();
        if state != self.state
            && let PlayerState::Error(e) = &state
            && !e.is_output()
        {
            cx.emit(AudioPlayerEvent::Failed(e.clone()));
        }
        self.state = state;
        self.position = self.player.position();
        if self.duration.is_none() {
            self.duration = self.player.duration();
        }
        cx.notify();
        match self.state {
            PlayerState::Loading => Some(LOADING_TICK),
            PlayerState::Playing => Some(TICK),
            _ => None,
        }
    }

    /// [`Self::refresh`], and starts following the player while it changes
    /// on its own. The player is polled rather than listened to: its events
    /// come from other threads.
    fn sync(&mut self, cx: &mut Context<'_, Self>) {
        let Some(first) = self.refresh(cx) else {
            return;
        };
        if self.ticking {
            return;
        }
        self.ticking = true;
        self.ticker = Some(cx.spawn(async move |this, cx| {
            let mut delay = first;
            loop {
                cx.background_executor().timer(delay).await;
                let next = this.update(cx, |view, cx| {
                    let next = view.refresh(cx);
                    if next.is_none() {
                        view.ticking = false;
                    }
                    next
                });
                match next {
                    Ok(Some(next)) => delay = next,
                    _ => break,
                }
            }
        }));
    }

    /// The fraction of the length under `x`, from the last painted bar.
    fn fraction_at(&self, x: Pixels) -> Option<f32> {
        let bar = self.bar.get()?;
        let width = f32::from(bar.size.width);
        if width <= 0.0 {
            return None;
        }
        Some((f32::from(x - bar.origin.x) / width).clamp(0.0, 1.0))
    }

    fn can_seek(&self) -> bool {
        self.duration.is_some_and(|d| !d.is_zero())
            && !matches!(self.state, PlayerState::Error(ref e) if !e.is_output())
            && !matches!(self.state, PlayerState::Empty | PlayerState::Loading)
    }

    fn drag_to(&mut self, x: Pixels, cx: &mut Context<'_, Self>) {
        if self.drag.is_some()
            && let Some(f) = self.fraction_at(x)
        {
            self.drag = Some(f);
            cx.notify();
        }
    }

    fn end_drag(&mut self, cx: &mut Context<'_, Self>) {
        if let (Some(f), Some(d)) = (self.drag.take(), self.duration) {
            self.seek_to(d.mul_f32(f), cx);
        }
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<'_, Self>) {
        let keystroke = &event.keystroke;
        let m = &keystroke.modifiers;
        if m.control || m.alt || m.platform || m.function {
            return;
        }
        match keystroke.key.as_str() {
            "space" if !m.shift => self.toggle(cx),
            "left" => self.seek_by(-SEEK_STEP.as_secs_f64(), cx),
            "right" => self.seek_by(SEEK_STEP.as_secs_f64(), cx),
            "home" => self.seek_to(Duration::ZERO, cx),
            "end" => match self.duration {
                Some(d) => self.seek_to(d, cx),
                None => return,
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The played fraction to draw.
    fn shown_fraction(&self) -> f32 {
        if let Some(f) = self.drag {
            return f;
        }
        match self.duration {
            Some(d) if !d.is_zero() => {
                (self.position.as_secs_f64() / d.as_secs_f64()).clamp(0.0, 1.0) as f32
            }
            _ => 0.0,
        }
    }

    fn time_label(&self) -> SharedString {
        let shown = match (self.drag, self.duration) {
            (Some(f), Some(d)) => d.mul_f32(f),
            _ => self.position,
        };
        match self.duration {
            Some(d) => format!("{} / {}", format_time(shown), format_time(d)).into(),
            None => format_time(shown).into(),
        }
    }
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn format_time(time: Duration) -> String {
    let secs = time.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl Render for AudioPlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = Appearance::theme(cx);
        let focused = self.focus.is_focused(window);
        let playing = self.state.is_playing();
        let failed = matches!(self.state, PlayerState::Error(ref e) if !e.is_output());
        let ready = !failed
            && (!self.loaded || !matches!(self.state, PlayerState::Empty | PlayerState::Loading));
        let can_seek = self.can_seek();
        let fraction = self.shown_fraction();

        let play_button = div()
            .id("audio-play")
            .debug_selector(|| "audio-play".into())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(icon_button::SQUARE))
            .rounded(px(radius::CONTROL))
            .text_color(if ready {
                theme.text.primary
            } else {
                theme.text.tertiary
            })
            .when(ready, |b| {
                b.cursor_pointer()
                    .hover(|s| s.bg(theme.state.hover))
                    .active(|s| s.bg(theme.state.press))
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        this.toggle(cx);
                    }))
            })
            .child((if playing { Icon::Pause } else { Icon::Play }).sized(icon::ROW));

        let bar_cell = Rc::clone(&self.bar);
        let weak = cx.entity().downgrade();
        let dragging = self.drag.is_some();
        let seek_bar = div()
            .id("audio-seek")
            .debug_selector(|| "audio-seek".into())
            .relative()
            .flex_1()
            .min_w(px(40.0))
            .h(px(BAR_HIT_HEIGHT))
            .flex()
            .items_center()
            .when(can_seek, |b| b.cursor_pointer())
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(TRACK_HEIGHT))
                    .rounded(px(TRACK_HEIGHT / 2.0))
                    .bg(theme.border.default)
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(gpui::relative(fraction))
                            .rounded(px(TRACK_HEIGHT / 2.0))
                            .bg(if can_seek {
                                theme.accent.default
                            } else {
                                theme.text.tertiary
                            }),
                    ),
            )
            .when(can_seek, |b| {
                b.child(
                    div()
                        .absolute()
                        .top(px((BAR_HIT_HEIGHT - KNOB) / 2.0))
                        .left(gpui::relative(fraction))
                        .ml(px(-KNOB / 2.0))
                        .size(px(KNOB))
                        .rounded_full()
                        .bg(theme.accent.default),
                )
            })
            .child(
                canvas(
                    move |bounds, _, _| bounds,
                    move |_, bounds, window, _| {
                        bar_cell.set(Some(bounds));
                        if !dragging {
                            return;
                        }
                        // While dragging, follow the mouse anywhere in the
                        // window and finish on release.
                        let moved = weak.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                let _ = moved.update(cx, |v, cx| v.drag_to(e.position.x, cx));
                            }
                        });
                        let released = weak.clone();
                        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                                let _ = released.update(cx, |v, cx| {
                                    v.drag_to(e.position.x, cx);
                                    v.end_drag(cx);
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .when(can_seek, |b| {
                b.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &MouseDownEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        if let Some(f) = this.fraction_at(e.position.x) {
                            this.drag = Some(f);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }),
                )
            });

        let mute_button = div()
            .id("audio-mute")
            .debug_selector(|| "audio-mute".into())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(icon_button::SQUARE))
            .rounded(px(radius::CONTROL))
            .text_color(theme.text.secondary)
            .cursor_pointer()
            .hover(|s| s.bg(theme.state.hover))
            .active(|s| s.bg(theme.state.press))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_mute(cx)))
            .child(
                (if self.is_muted() {
                    Icon::Muted
                } else {
                    Icon::Volume
                })
                .sized(icon::ROW),
            );

        let status: Option<(SharedString, gpui::Hsla)> = match &self.state {
            PlayerState::Error(e) => Some((e.message().into(), theme.status.error.into())),
            PlayerState::Loading => None,
            _ => None,
        };

        div()
            .id("audio-player")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.on_key(e, cx)))
            .on_action(cx.listener(|this, _: &TogglePlay, _, cx| this.toggle(cx)))
            .on_action(cx.listener(|this, _: &SeekStart, _, cx| {
                this.seek_to(Duration::ZERO, cx);
            }))
            .on_action(cx.listener(|this, _: &SeekEnd, _, cx| {
                if let Some(d) = this.duration {
                    this.seek_to(d, cx);
                }
            }))
            .flex()
            .flex_col()
            .w_full()
            .px(px(GAP / 2.0))
            .py(px(2.0))
            .rounded(px(radius::CONTROL))
            .border(px(HAIRLINE))
            .border_color(if focused {
                theme.accent.default.into()
            } else {
                gpui::transparent_black()
            })
            .text_size(px(text::UI.size))
            .line_height(px(text::UI.line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(GAP))
                    .child(play_button)
                    .child(seek_bar)
                    .child(
                        div()
                            .flex_none()
                            .debug_selector(|| "audio-time".into())
                            .text_color(theme.text.secondary)
                            .child(self.time_label()),
                    )
                    .child(mute_button),
            )
            .when_some(status, |el, (message, color)| {
                el.child(
                    div()
                        .debug_selector(|| "audio-status".into())
                        .pl(px(icon_button::SQUARE + GAP))
                        .text_size(px(text::UI_SMALL.size))
                        .line_height(px(text::UI_SMALL.line))
                        .text_color(color)
                        .child(message),
                )
            })
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! The engine (one output stream for the application) and its players.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use super::output::{AudioBackend, CpalBackend, OutputFormat, OutputStream, RenderFn};
use super::source::{AudioSource, duration_to_frames, frames_to_duration};
use super::voice::{Listener, Voice, capacity_for};
use super::{AudioError, PlayerEvent, PlayerState};

/// The application's audio output: one stream, opened on the first play,
/// shared by every [`Player`]; one player plays at a time. Cheap to clone.
#[derive(Clone)]
pub struct AudioEngine {
    shared: Arc<EngineShared>,
}

impl std::fmt::Debug for AudioEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioEngine").finish_non_exhaustive()
    }
}

struct EngineShared {
    backend: Box<dyn AudioBackend>,
    mixer: Arc<Mixer>,
    output: Mutex<Option<Box<dyn OutputStream>>>,
    next_id: AtomicU64,
}

/// What the output callback reads: the voice that may play.
#[derive(Default)]
struct Mixer {
    active: Mutex<Option<Arc<Voice>>>,
}

impl Mixer {
    fn active(&self) -> Option<Arc<Voice>> {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn render(&self, out: &mut [f32], format: OutputFormat) {
        match self.active() {
            Some(voice) => voice.render(out, format),
            None => out.fill(0.0),
        }
    }
}

impl AudioEngine {
    /// An engine on `backend`. Nothing is opened until a player plays.
    pub fn new(backend: impl AudioBackend + 'static) -> Self {
        Self {
            shared: Arc::new(EngineShared {
                backend: Box::new(backend),
                mixer: Arc::new(Mixer::default()),
                output: Mutex::new(None),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// An engine on the system's default output device.
    pub fn system() -> Self {
        Self::new(CpalBackend)
    }

    /// A new, empty player.
    pub fn player(&self) -> Player {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        Player {
            voice: Arc::new(Voice::new(id)),
            engine: Arc::clone(&self.shared),
        }
    }
}

impl EngineShared {
    fn lock_output(&self) -> std::sync::MutexGuard<'_, Option<Box<dyn OutputStream>>> {
        self.output.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes `voice` the one that plays, opening the output if needed, and
    /// pauses the one that played before.
    fn start(&self, voice: &Arc<Voice>) -> Result<(), AudioError> {
        let previous = {
            let mut output = self.lock_output();
            if output.as_ref().is_some_and(|s| s.is_broken()) {
                *output = None;
            }
            if output.is_none() {
                let mixer = Arc::clone(&self.mixer);
                let render: RenderFn = Box::new(move |buf, format| mixer.render(buf, format));
                *output = Some(self.backend.open(render)?);
            }
            let previous = self
                .mixer
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .replace(Arc::clone(voice));
            if let Some(stream) = output.as_mut() {
                stream.resume()?;
            }
            previous
        };
        if let Some(previous) = previous.filter(|p| p.id != voice.id) {
            let event = {
                let mut inner = previous.lock();
                if inner.state.is_playing() {
                    Voice::transition(&mut inner, PlayerState::Paused)
                } else {
                    None
                }
            };
            if let Some(event) = event {
                previous.emit(event);
            }
        }
        Ok(())
    }

    /// Suspends the output if `voice` is the active one and is not playing.
    fn release(&self, voice: &Voice) {
        let mut output = self.lock_output();
        let active = self.mixer.active();
        if active.as_ref().is_some_and(|a| a.id == voice.id)
            && !voice.lock().state.is_playing()
            && let Some(stream) = output.as_mut()
        {
            stream.suspend();
        }
    }

    /// Forgets `voice` (its player is gone).
    fn detach(&self, voice: &Voice) {
        let mut output = self.lock_output();
        let mut active = self
            .mixer
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if active.as_ref().is_some_and(|a| a.id == voice.id) {
            *active = None;
            if let Some(stream) = output.as_mut() {
                stream.suspend();
            }
        }
    }

    /// Plays `voice` from wherever it is; starts over after the end.
    fn play(&self, voice: &Arc<Voice>) {
        let event = {
            let mut inner = voice.lock();
            match &inner.state {
                PlayerState::Empty | PlayerState::Playing => return,
                PlayerState::Loading => {
                    inner.play_when_ready = true;
                    return;
                }
                PlayerState::Error(e) if !e.is_output() || inner.info.is_none() => return,
                PlayerState::Ended => {
                    inner.reposition(0);
                    inner.pending_seek = Some(0);
                }
                _ => {}
            }
            Voice::transition(&mut inner, PlayerState::Playing)
        };
        voice.wake.notify_all();
        if let Some(event) = event {
            voice.emit(event);
        }
        if let Err(e) = self.start(voice) {
            log::warn!("audio output unavailable: {e}");
            voice.set_state(PlayerState::Error(e));
        }
    }
}

/// One embed's player. Dropping it stops its decoding thread.
pub struct Player {
    voice: Arc<Voice>,
    engine: Arc<EngineShared>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("id", &self.voice.id)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Loads `path` on a background thread: [`PlayerState::Loading`], then
    /// [`PlayerState::Stopped`] (or an error state). Replaces any earlier
    /// file.
    pub fn load(&self, path: &Path) {
        let (load, event) = {
            let mut inner = self.voice.lock();
            inner.load += 1;
            inner.info = None;
            inner.reposition(0);
            inner.pending_seek = None;
            inner.play_when_ready = false;
            inner.shutdown = false;
            let event = Voice::transition(&mut inner, PlayerState::Loading);
            (inner.load, event)
        };
        self.voice.wake.notify_all();
        if let Some(event) = event {
            self.voice.emit(event);
        }
        self.engine.release(&self.voice);
        let voice = Arc::clone(&self.voice);
        let engine = Arc::downgrade(&self.engine);
        let path = path.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("gv-audio-decode".into())
            .spawn(move || decode_thread(&voice, &engine, &path, load));
        if let Err(e) = spawned {
            self.voice
                .set_state(PlayerState::Error(AudioError::Io(e.to_string())));
        }
    }

    /// Plays; when still loading, plays once loaded. After the end, starts
    /// over. Pauses whichever other player was playing.
    pub fn play(&self) {
        self.engine.play(&self.voice);
    }

    pub fn pause(&self) {
        let event = {
            let mut inner = self.voice.lock();
            inner.play_when_ready = false;
            if !inner.state.is_playing() {
                return;
            }
            Voice::transition(&mut inner, PlayerState::Paused)
        };
        if let Some(event) = event {
            self.voice.emit(event);
        }
        self.engine.release(&self.voice);
    }

    /// Play when not playing, pause when playing.
    pub fn toggle(&self) {
        if self.state().is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Stops and goes back to the start.
    pub fn stop(&self) {
        let event = {
            let mut inner = self.voice.lock();
            inner.play_when_ready = false;
            if inner.info.is_none() || matches!(inner.state, PlayerState::Error(_)) {
                return;
            }
            inner.reposition(0);
            inner.pending_seek = Some(0);
            Voice::transition(&mut inner, PlayerState::Stopped)
        };
        self.voice.wake.notify_all();
        if let Some(event) = event {
            self.voice.emit(event);
        }
        self.engine.release(&self.voice);
    }

    /// Moves to `time` (clamped to the length). Keeps playing if playing;
    /// when stopped or ended, the player is paused at `time` (stopped at 0).
    pub fn seek(&self, time: Duration) {
        let event = {
            let mut inner = self.voice.lock();
            let Some(info) = inner.info else { return };
            if matches!(inner.state, PlayerState::Error(ref e) if !e.is_output()) {
                return;
            }
            let mut frame = duration_to_frames(time, info.sample_rate);
            if let Some(total) = info.frames {
                frame = frame.min(total);
            }
            inner.reposition(frame);
            inner.pending_seek = Some(frame);
            match inner.state {
                PlayerState::Ended | PlayerState::Stopped if frame == 0 => {
                    Voice::transition(&mut inner, PlayerState::Stopped)
                }
                PlayerState::Ended | PlayerState::Stopped => {
                    Voice::transition(&mut inner, PlayerState::Paused)
                }
                _ => None,
            }
        };
        self.voice.wake.notify_all();
        if let Some(event) = event {
            self.voice.emit(event);
        }
    }

    /// Where playback is.
    pub fn position(&self) -> Duration {
        let inner = self.voice.lock();
        match inner.info {
            Some(info) => frames_to_duration(inner.position, info.sample_rate),
            None => Duration::ZERO,
        }
    }

    /// The length, once loaded, when the file says.
    pub fn duration(&self) -> Option<Duration> {
        self.voice.lock().info.and_then(|i| i.duration())
    }

    /// Decoded frames waiting for the output (for tests).
    #[doc(hidden)]
    pub fn queued_frames(&self) -> usize {
        let inner = self.voice.lock();
        inner
            .info
            .map_or(0, |info| inner.queue.len() / info.channels.max(1))
    }

    pub fn state(&self) -> PlayerState {
        self.voice.lock().state.clone()
    }

    /// 0 to 1; the gain is its square, which sounds about linear.
    pub fn volume(&self) -> f32 {
        self.voice.volume()
    }

    pub fn set_volume(&self, volume: f32) {
        self.voice.set_volume(volume);
    }

    /// Receives this player's events from now on (replacing any earlier
    /// listener).
    pub fn set_listener(&self, listener: impl Fn(PlayerEvent) + Send + Sync + 'static) {
        let listener: Listener = Arc::new(listener);
        self.voice.set_listener(Some(listener));
    }

    pub fn clear_listener(&self) {
        self.voice.set_listener(None);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.voice.set_listener(None);
        self.voice.lock().shutdown = true;
        self.voice.wake.notify_all();
        self.engine.detach(&self.voice);
    }
}

/// Moves `voice` to an error state, if `load` is still its current load.
fn fail(voice: &Voice, engine: &Weak<EngineShared>, load: u64, error: AudioError) {
    let event = {
        let mut inner = voice.lock();
        if inner.load != load || inner.shutdown {
            return;
        }
        inner.queue.clear();
        Voice::transition(&mut inner, PlayerState::Error(error))
    };
    if let Some(event) = event {
        voice.emit(event);
    }
    if let Some(engine) = engine.upgrade() {
        engine.release(voice);
    }
}

/// A load's decoding thread: probes, then keeps the queue filled until the
/// player is dropped or loads something else.
fn decode_thread(voice: &Arc<Voice>, engine: &Weak<EngineShared>, path: &Path, load: u64) {
    let path: PathBuf = path.to_path_buf();
    let mut source = match catch_unwind(|| AudioSource::open(&path)) {
        Ok(Ok(source)) => source,
        Ok(Err(e)) => return fail(voice, engine, load, e),
        Err(_) => return fail(voice, engine, load, AudioError::Panicked),
    };
    if source.info().frames.is_none()
        && let Ok(Some(frames)) = catch_unwind(|| AudioSource::count_frames(&path))
    {
        source.set_frames(frames);
    }
    let info = source.info();
    let (event, play) = {
        let mut inner = voice.lock();
        if inner.load != load || inner.shutdown {
            return;
        }
        inner.info = Some(info);
        inner.capacity = capacity_for(info);
        let event = Voice::transition(&mut inner, PlayerState::Stopped);
        (event, std::mem::take(&mut inner.play_when_ready))
    };
    voice.emit(PlayerEvent::Loaded {
        duration: info.duration(),
    });
    if let Some(event) = event {
        voice.emit(event);
    }
    if play && let Some(engine) = engine.upgrade() {
        engine.play(voice);
    }

    let mut chunk = Vec::new();
    // The demuxer reached the end: seek by reopening (some cannot seek
    // after it).
    let mut at_end = false;
    // The last seek, until a chunk decodes after it: a decode error then
    // is retried once from a fresh source.
    let mut after_seek: Option<u64> = None;
    loop {
        // Wait for work: a seek, or room in the queue.
        let (seek, generation) = {
            let mut inner = voice.lock();
            loop {
                if inner.load != load || inner.shutdown {
                    return;
                }
                if let Some(frame) = inner.pending_seek.take() {
                    break (Some(frame), inner.generation);
                }
                if inner.eof {
                    if inner.queue.is_empty() && inner.state.is_playing() {
                        if let Some(total) = info.frames {
                            inner.position = inner.position.max(total);
                        }
                        let event = Voice::transition(&mut inner, PlayerState::Ended);
                        drop(inner);
                        if let Some(event) = event {
                            voice.emit(event);
                        }
                        if let Some(engine) = engine.upgrade() {
                            engine.release(voice);
                        }
                        inner = voice.lock();
                        continue;
                    }
                } else if inner.queue.len() < inner.capacity {
                    break (None, inner.generation);
                }
                inner = voice
                    .wake
                    .wait(inner)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        if let Some(frame) = seek {
            chunk.clear();
            let seeked = if at_end || frame == 0 {
                None
            } else {
                match catch_unwind(AssertUnwindSafe(|| source.seek(frame))) {
                    Ok(Ok(landed)) => Some(landed),
                    Ok(Err(e)) => {
                        log::debug!("audio seek failed, reopening: {e}");
                        None
                    }
                    Err(_) => None,
                }
            };
            let landed = match seeked {
                Some(landed) => landed,
                None => match reopen(&path, frame) {
                    Ok((fresh, leftover)) => {
                        source = fresh;
                        chunk = leftover;
                        frame
                    }
                    Err(e) => return fail(voice, engine, load, e),
                },
            };
            at_end = false;
            after_seek = Some(landed);
            let mut inner = voice.lock();
            if inner.generation == generation {
                inner.position = landed;
                inner.queue.extend(chunk.iter().copied());
            }
            chunk.clear();
            continue;
        }
        chunk.clear();
        let result = match catch_unwind(AssertUnwindSafe(|| source.next_chunk(&mut chunk))) {
            Ok(result) => result,
            Err(_) => Err(AudioError::Panicked),
        };
        let more = match result {
            Ok(more) => more,
            Err(e) => match after_seek.take() {
                Some(frame) => {
                    log::debug!("audio decode failed after a seek, reopening: {e}");
                    match reopen(&path, frame) {
                        Ok((fresh, leftover)) => {
                            source = fresh;
                            chunk = leftover;
                            true
                        }
                        Err(e) => return fail(voice, engine, load, e),
                    }
                }
                None => return fail(voice, engine, load, e),
            },
        };
        if more {
            after_seek = None;
        } else {
            at_end = true;
        }
        let mut inner = voice.lock();
        if inner.generation == generation && inner.load == load {
            inner.queue.extend(chunk.iter().copied());
            if !more {
                inner.eof = true;
            }
        }
    }
}

fn reopen(path: &Path, frame: u64) -> Result<(AudioSource, Vec<f32>), AudioError> {
    match catch_unwind(|| AudioSource::open_at(path, frame)) {
        Ok(result) => result,
        Err(_) => Err(AudioError::Panicked),
    }
}

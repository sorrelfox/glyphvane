// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! One player's shared state: the decoded frames queued for the output, and
//! the conversion to the device's rate and channels.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use super::output::OutputFormat;
use super::source::SourceInfo;
use super::{PlayerEvent, PlayerState};

/// How much decoded audio is queued ahead of the output, in seconds.
const QUEUE_SECONDS: f32 = 0.5;

pub(crate) type Listener = Arc<dyn Fn(PlayerEvent) + Send + Sync>;

/// A player's state, shared by the handle, its decoding thread and the
/// output.
pub(crate) struct Voice {
    pub(crate) id: u64,
    inner: Mutex<VoiceInner>,
    /// Wakes the decoding thread: room in the queue, a seek, a new load.
    pub(crate) wake: Condvar,
    /// `f32` bits of the volume, 0 to 1.
    volume: AtomicU32,
    listener: Mutex<Option<Listener>>,
}

#[derive(Default)]
pub(crate) struct VoiceInner {
    pub(crate) state: PlayerState,
    pub(crate) info: Option<SourceInfo>,
    /// Interleaved frames in the source's format.
    pub(crate) queue: VecDeque<f32>,
    /// Samples the queue holds at most.
    pub(crate) capacity: usize,
    /// The decoder has delivered its last frame.
    pub(crate) eof: bool,
    /// Bumped by every seek and load: frames decoded before it are dropped.
    pub(crate) generation: u64,
    /// Bumped by every load: the decoding thread of an older load exits.
    pub(crate) load: u64,
    pub(crate) pending_seek: Option<u64>,
    pub(crate) shutdown: bool,
    /// Start playing as soon as the file is probed.
    pub(crate) play_when_ready: bool,
    /// The source frame the output is at.
    pub(crate) position: u64,
    pub(crate) resampler: Resampler,
}

impl VoiceInner {
    /// Drops queued frames and moves the output to `frame`.
    pub(crate) fn reposition(&mut self, frame: u64) {
        self.queue.clear();
        self.eof = false;
        self.generation += 1;
        self.position = frame;
        self.resampler.reset();
    }
}

impl Voice {
    pub(crate) fn new(id: u64) -> Self {
        Self {
            id,
            inner: Mutex::new(VoiceInner::default()),
            wake: Condvar::new(),
            volume: AtomicU32::new(1.0f32.to_bits()),
            listener: Mutex::new(None),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, VoiceInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    pub(crate) fn set_volume(&self, volume: f32) {
        let v = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn set_listener(&self, listener: Option<Listener>) {
        *self.listener.lock().unwrap_or_else(PoisonError::into_inner) = listener;
    }

    /// Calls the listener. Never with `inner` locked.
    pub(crate) fn emit(&self, event: PlayerEvent) {
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(listener) = listener {
            listener(event);
        }
    }

    /// Sets the state under the lock the caller holds; returns the event to
    /// emit once the lock is released.
    pub(crate) fn transition(inner: &mut VoiceInner, state: PlayerState) -> Option<PlayerEvent> {
        if inner.state == state {
            return None;
        }
        inner.state = state.clone();
        Some(PlayerEvent::StateChanged(state))
    }

    pub(crate) fn set_state(&self, state: PlayerState) {
        let event = Self::transition(&mut self.lock(), state);
        if let Some(event) = event {
            self.emit(event);
        }
    }

    /// Fills `out` (interleaved, in `format`) from the queue, scaled by the
    /// volume. Silence when not playing or when the decoder is behind.
    pub(crate) fn render(&self, out: &mut [f32], format: OutputFormat) {
        let gain = {
            let v = self.volume();
            v * v
        };
        let mut inner = self.lock();
        let Some(info) = inner.info else {
            out.fill(0.0);
            return;
        };
        if !inner.state.is_playing() || format.channels == 0 {
            out.fill(0.0);
            return;
        }
        let step = f64::from(info.sample_rate) / f64::from(format.sample_rate.max(1));
        let before = inner.queue.len();
        let VoiceInner {
            queue,
            resampler,
            position,
            ..
        } = &mut *inner;
        let mut frames = out.chunks_exact_mut(format.channels);
        for frame in frames.by_ref() {
            if !resampler.next_frame(queue, info.channels, step, position) {
                frame.fill(0.0);
                break;
            }
            map_channels(resampler.frame(), frame, gain);
        }
        for frame in frames {
            frame.fill(0.0);
        }
        let low = inner.capacity / 2;
        let after = inner.queue.len();
        drop(inner);
        if after < low || (after == 0 && before > 0) {
            self.wake.notify_all();
        }
    }
}

/// The queue's capacity in samples for a source.
pub(crate) fn capacity_for(info: SourceInfo) -> usize {
    let frames = (info.sample_rate as f32 * QUEUE_SECONDS) as usize;
    frames.max(1024) * info.channels
}

/// Linear interpolation between two source frames, stepping by the ratio of
/// the rates. At equal rates it passes frames through unchanged.
#[derive(Debug, Default)]
pub(crate) struct Resampler {
    cur: Vec<f32>,
    next: Vec<f32>,
    /// Frames loaded: 0, 1 (`cur`) or 2 (`cur` and `next`).
    have: u8,
    /// Between `cur` (0) and `next` (1).
    pos: f64,
    out: Vec<f32>,
}

impl Resampler {
    pub(crate) fn reset(&mut self) {
        self.have = 0;
        self.pos = 0.0;
    }

    /// The frame computed by the last [`Resampler::next_frame`].
    pub(crate) fn frame(&self) -> &[f32] {
        &self.out
    }

    /// Computes the next output frame from `queue` (interleaved,
    /// `channels` per frame); `position` counts the source frames passed.
    /// `false` when the queue runs dry.
    pub(crate) fn next_frame(
        &mut self,
        queue: &mut VecDeque<f32>,
        channels: usize,
        step: f64,
        position: &mut u64,
    ) -> bool {
        if self.cur.len() != channels {
            self.cur.resize(channels, 0.0);
            self.next.resize(channels, 0.0);
            self.out.resize(channels, 0.0);
            self.have = 0;
        }
        loop {
            while self.have < 2 {
                if queue.len() < channels {
                    return false;
                }
                let dst = if self.have == 0 {
                    &mut self.cur
                } else {
                    &mut self.next
                };
                for (d, s) in dst.iter_mut().zip(queue.drain(..channels)) {
                    *d = s;
                }
                self.have += 1;
            }
            if self.pos >= 1.0 {
                self.pos -= 1.0;
                std::mem::swap(&mut self.cur, &mut self.next);
                self.have = 1;
                *position += 1;
                continue;
            }
            break;
        }
        let t = self.pos as f32;
        for ((o, a), b) in self.out.iter_mut().zip(&self.cur).zip(&self.next) {
            *o = a + (b - a) * t;
        }
        self.pos += step;
        true
    }
}

/// Writes a source frame into a device frame: the same channels copied,
/// mono spread to every channel, a mono device gets the average, extra
/// device channels stay silent.
pub(crate) fn map_channels(src: &[f32], dst: &mut [f32], gain: f32) {
    match (src.len(), dst.len()) {
        (s, d) if s == d => {
            for (o, i) in dst.iter_mut().zip(src) {
                *o = i * gain;
            }
        }
        (1, _) => dst.fill(src[0] * gain),
        (s, 1) => dst[0] = src.iter().sum::<f32>() / s as f32 * gain,
        _ => {
            for (c, o) in dst.iter_mut().enumerate() {
                *o = src.get(c).map_or(0.0, |i| i * gain);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &[f32], channels: usize, step: f64, n: usize) -> (Vec<f32>, u64) {
        let mut queue: VecDeque<f32> = src.iter().copied().collect();
        let mut r = Resampler::default();
        let mut pos = 0;
        let mut out = Vec::new();
        for _ in 0..n {
            if !r.next_frame(&mut queue, channels, step, &mut pos) {
                break;
            }
            out.extend_from_slice(r.frame());
        }
        (out, pos)
    }

    #[test]
    fn equal_rates_pass_frames_through() {
        let src: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let (out, pos) = run(&src, 2, 1.0, 100);
        // The last frame waits for its successor; the position is the frame
        // being played.
        assert_eq!(out, src[..8].to_vec());
        assert_eq!(pos, 4);
    }

    #[test]
    fn upsampling_interpolates() {
        let src = [0.0, 1.0, 2.0];
        let (out, pos) = run(&src, 1, 0.5, 100);
        assert_eq!(out, vec![0.0, 0.5, 1.0, 1.5]);
        assert_eq!(pos, 2);
    }

    #[test]
    fn downsampling_skips() {
        let src: Vec<f32> = (0..9).map(|i| i as f32).collect();
        let (out, _) = run(&src, 1, 2.0, 100);
        assert_eq!(out, vec![0.0, 2.0, 4.0, 6.0]);
    }

    #[test]
    fn channel_mapping() {
        let mut d = [0.0; 2];
        map_channels(&[0.5], &mut d, 1.0);
        assert_eq!(d, [0.5, 0.5]);
        let mut d = [0.0; 1];
        map_channels(&[0.2, 0.4], &mut d, 1.0);
        assert!((d[0] - 0.3).abs() < 1e-6);
        let mut d = [9.0; 6];
        map_channels(&[0.1, 0.2], &mut d, 0.5);
        assert_eq!(d, [0.05, 0.1, 0.0, 0.0, 0.0, 0.0]);
        let mut d = [0.0; 2];
        map_channels(&[0.1, 0.2, 0.3], &mut d, 1.0);
        assert_eq!(d, [0.1, 0.2]);
    }
}

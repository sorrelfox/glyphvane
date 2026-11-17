// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Audio decoding and the player's state machine, on WAV files written by
//! the tests and a silent output that never opens a device.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gv_media::audio::{
    AudioEngine, AudioError, AudioSource, NullBackend, NullOutput, OutputFormat, Player,
    PlayerEvent, PlayerState,
};

/// The test signal: never zero, so silence is told apart.
fn sample(frame: usize, channel: usize) -> i16 {
    let v = (frame % 1000) as i16 + 1;
    if channel == 0 { v } else { -v }
}

fn to_f32(s: i16) -> f32 {
    f32::from(s) / 32768.0
}

/// A 16-bit PCM WAV of `frames` frames.
fn wav_i16(dir: &Path, name: &str, rate: u32, channels: u16, frames: usize) -> PathBuf {
    let mut data = Vec::with_capacity(frames * usize::from(channels) * 2);
    for f in 0..frames {
        for c in 0..usize::from(channels) {
            data.extend_from_slice(&sample(f, c).to_le_bytes());
        }
    }
    write_wav(dir, name, 1, rate, channels, 16, &data)
}

/// A 32-bit float WAV of the given mono samples.
fn wav_f32(dir: &Path, name: &str, rate: u32, samples: &[f32]) -> PathBuf {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    write_wav(dir, name, 3, rate, 1, 32, &data)
}

fn write_wav(
    dir: &Path,
    name: &str,
    format: u16,
    rate: u32,
    channels: u16,
    bits: u16,
    data: &[u8],
) -> PathBuf {
    let block = channels * bits / 8;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&format.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    let path = dir.join(name);
    std::fs::write(&path, out).unwrap();
    path
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_state(player: &Player, state: &PlayerState) {
    wait_until(&format!("{state:?}"), || player.state() == *state);
}

/// Pulls `frames` frames once the decoder has them queued, so the output
/// never underruns.
fn pull(player: &Player, output: &NullOutput, frames: usize) -> Vec<f32> {
    wait_until("decoded frames", || player.queued_frames() > frames);
    output.pull(frames)
}

fn stereo_48k() -> (AudioEngine, NullOutput) {
    let (backend, output) = NullBackend::new(OutputFormat {
        sample_rate: 48_000,
        channels: 2,
    });
    (AudioEngine::new(backend), output)
}

fn recorder(player: &Player) -> Arc<Mutex<Vec<PlayerEvent>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    player.set_listener(move |e| sink.lock().unwrap().push(e));
    events
}

fn expected_stereo(from: usize, frames: usize, gain: f32) -> Vec<f32> {
    (from..from + frames)
        .flat_map(|f| [to_f32(sample(f, 0)) * gain, to_f32(sample(f, 1)) * gain])
        .collect()
}

// ---------------------------------------------------------------------------
// Decoding

#[test]
fn source_decodes_a_wav_in_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav_i16(dir.path(), "a.wav", 44_100, 2, 50_000);
    let mut source = AudioSource::open(&path).unwrap();
    let info = source.info();
    assert_eq!(info.sample_rate, 44_100);
    assert_eq!(info.channels, 2);
    assert_eq!(info.frames, Some(50_000));
    let d = info.duration().unwrap();
    assert!((d.as_secs_f64() - 50_000.0 / 44_100.0).abs() < 1e-6);

    let mut all = Vec::new();
    let mut chunks = 0;
    while source.next_chunk(&mut all).unwrap() {
        chunks += 1;
    }
    assert!(chunks > 1, "decoded in more than one chunk");
    assert_eq!(all.len(), 50_000 * 2);
    for (f, frame) in all.as_chunks::<2>().0.iter().enumerate().step_by(997) {
        assert_eq!(*frame, [to_f32(sample(f, 0)), to_f32(sample(f, 1))]);
    }
    assert_eq!(source.position(), 50_000);
}

#[test]
fn source_seeks_to_the_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav_i16(dir.path(), "a.wav", 8_000, 1, 40_000);
    let mut source = AudioSource::open(&path).unwrap();
    let landed = source.seek(12_345).unwrap();
    assert_eq!(landed, 12_345);
    let mut out = Vec::new();
    assert!(source.next_chunk(&mut out).unwrap());
    assert_eq!(out[0], to_f32(sample(12_345, 0)));
    assert_eq!(out[1], to_f32(sample(12_346, 0)));
    // Past the end clamps.
    let landed = source.seek(1_000_000).unwrap();
    assert!(landed <= 40_000);
}

#[test]
fn source_reads_float_wav() {
    let dir = tempfile::tempdir().unwrap();
    let samples: Vec<f32> = (0..4000).map(|i| (i as f32 / 4000.0) - 0.5).collect();
    let path = wav_f32(dir.path(), "f.wav", 16_000, &samples);
    let mut source = AudioSource::open(&path).unwrap();
    let mut all = Vec::new();
    while source.next_chunk(&mut all).unwrap() {}
    assert_eq!(all, samples);
}

#[test]
fn bad_files_are_errors_not_panics() {
    let dir = tempfile::tempdir().unwrap();
    let garbage = dir.path().join("x.mp3");
    std::fs::write(
        &garbage,
        (0..5000u32)
            .map(|i| (i * 7919 % 251) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(
        AudioSource::open(&garbage).unwrap_err(),
        AudioError::UnsupportedFormat
    );
    let empty = dir.path().join("e.ogg");
    std::fs::write(&empty, b"").unwrap();
    assert!(AudioSource::open(&empty).is_err());
    let missing = dir.path().join("missing.wav");
    assert!(matches!(
        AudioSource::open(&missing).unwrap_err(),
        AudioError::Io(_)
    ));
    // A WAV header cut short.
    let path = wav_i16(dir.path(), "cut.wav", 8_000, 1, 100);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..30]).unwrap();
    assert!(AudioSource::open(&path).is_err());
}

// ---------------------------------------------------------------------------
// Player

#[test]
fn player_loads_plays_pauses_seeks_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav_i16(dir.path(), "a.wav", 48_000, 2, 96_000);
    let (engine, output) = stereo_48k();
    let player = engine.player();
    let events = recorder(&player);
    assert_eq!(player.state(), PlayerState::Empty);

    player.load(&path);
    wait_state(&player, &PlayerState::Stopped);
    assert_eq!(player.duration(), Some(Duration::from_secs(2)));
    assert_eq!(output.opens(), 0, "the device opens on the first play");
    assert!(output.pull(64).iter().all(|s| *s == 0.0));

    player.play();
    assert_eq!(player.state(), PlayerState::Playing);
    assert_eq!(output.opens(), 1);
    assert!(output.is_running());
    let out = pull(&player, &output, 1000);
    assert_eq!(out, expected_stereo(0, 1000, 1.0));
    let out = pull(&player, &output, 500);
    assert_eq!(out, expected_stereo(1000, 500, 1.0));
    let pos = player.position().as_secs_f64() * 48_000.0;
    assert!((1498.5..=1500.5).contains(&pos), "position {pos}");

    player.pause();
    assert_eq!(player.state(), PlayerState::Paused);
    assert!(!output.is_running(), "the stream is suspended while idle");
    let before = player.position();
    assert!(output.pull(256).iter().all(|s| *s == 0.0));
    assert_eq!(player.position(), before);

    player.seek(Duration::from_millis(500));
    assert_eq!(player.position(), Duration::from_millis(500));
    player.play();
    let out = pull(&player, &output, 300);
    assert_eq!(out, expected_stereo(24_000, 300, 1.0));

    player.set_volume(0.5);
    let out = pull(&player, &output, 100);
    assert_eq!(out, expected_stereo(24_300, 100, 0.25));
    player.set_volume(1.0);

    player.stop();
    assert_eq!(player.state(), PlayerState::Stopped);
    assert_eq!(player.position(), Duration::ZERO);
    assert_eq!(output.opens(), 1, "one stream for the whole run");

    let events = events.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            PlayerEvent::StateChanged(PlayerState::Loading),
            PlayerEvent::Loaded {
                duration: Some(Duration::from_secs(2))
            },
            PlayerEvent::StateChanged(PlayerState::Stopped),
            PlayerEvent::StateChanged(PlayerState::Playing),
            PlayerEvent::StateChanged(PlayerState::Paused),
            PlayerEvent::StateChanged(PlayerState::Playing),
            PlayerEvent::StateChanged(PlayerState::Stopped),
        ]
    );
}

#[test]
fn playing_to_the_end_ends_and_play_starts_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav_i16(dir.path(), "a.wav", 48_000, 2, 10_000);
    let (engine, output) = stereo_48k();
    let player = engine.player();
    player.load(&path);
    wait_state(&player, &PlayerState::Stopped);
    player.seek(Duration::from_secs_f64(9_000.0 / 48_000.0));
    player.play();
    wait_until("the tail decoded", || player.queued_frames() >= 999);
    let out = output.pull(2000);
    assert_eq!(&out[..2 * 999], &expected_stereo(9_000, 999, 1.0)[..]);
    wait_state(&player, &PlayerState::Ended);
    assert_eq!(player.position(), player.duration().unwrap());
    assert!(!output.is_running());

    // Seeking after the end pauses there.
    player.seek(Duration::from_millis(100));
    assert_eq!(player.state(), PlayerState::Paused);

    player.stop();
    player.play();
    let out = pull(&player, &output, 10);
    assert_eq!(out, expected_stereo(0, 10, 1.0));
}

#[test]
fn ended_player_plays_from_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav_i16(dir.path(), "a.wav", 48_000, 2, 3_000);
    let (engine, output) = stereo_48k();
    let player = engine.player();
    player.load(&path);
    wait_state(&player, &PlayerState::Stopped);
    player.play();
    wait_until("all decoded", || player.queued_frames() >= 2_999);
    output.pull(4_000);
    wait_state(&player, &PlayerState::Ended);
    player.play();
    assert_eq!(player.state(), PlayerState::Playing);
    assert_eq!(player.position(), Duration::ZERO);
    let out = pull(&player, &output, 10);
    assert_eq!(out, expected_stereo(0, 10, 1.0));
}

#[test]
fn only_one_player_plays() {
    let dir = tempfile::tempdir().unwrap();
    let a = wav_i16(dir.path(), "a.wav", 48_000, 2, 48_000);
    let b = wav_i16(dir.path(), "b.wav", 48_000, 2, 48_000);
    let (engine, output) = stereo_48k();
    let first = engine.player();
    let second = engine.player();
    let first_events = recorder(&first);
    first.load(&a);
    second.load(&b);
    wait_state(&first, &PlayerState::Stopped);
    wait_state(&second, &PlayerState::Stopped);
    first.play();
    pull(&first, &output, 100);
    second.play();
    assert_eq!(first.state(), PlayerState::Paused);
    assert_eq!(second.state(), PlayerState::Playing);
    assert_eq!(
        first_events.lock().unwrap().last(),
        Some(&PlayerEvent::StateChanged(PlayerState::Paused))
    );
    let out = pull(&second, &output, 50);
    assert_eq!(out, expected_stereo(0, 50, 1.0));
    assert_eq!(output.opens(), 1);

    // Dropping the playing player silences the output.
    drop(second);
    assert!(!output.is_running());
    assert!(output.pull(16).iter().all(|s| *s == 0.0));
}

#[test]
fn no_output_device_is_an_error_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav_i16(dir.path(), "a.wav", 48_000, 2, 4_800);
    let (backend, _output) = NullBackend::without_device();
    let engine = AudioEngine::new(backend);
    let player = engine.player();
    player.load(&path);
    wait_state(&player, &PlayerState::Stopped);
    player.play();
    assert_eq!(
        player.state(),
        PlayerState::Error(AudioError::NoOutputDevice)
    );
    // Retrying fails the same way, without panicking.
    player.play();
    assert_eq!(
        player.state(),
        PlayerState::Error(AudioError::NoOutputDevice)
    );
    assert_eq!(player.duration(), Some(Duration::from_millis(100)));
}

#[test]
fn unsupported_and_missing_files_end_in_error_states() {
    let dir = tempfile::tempdir().unwrap();
    let garbage = dir.path().join("x.m4a");
    std::fs::write(&garbage, vec![0x5a; 4096]).unwrap();
    let (engine, output) = stereo_48k();
    let player = engine.player();
    player.load(&garbage);
    wait_state(&player, &PlayerState::Error(AudioError::UnsupportedFormat));
    player.play();
    player.seek(Duration::from_secs(1));
    player.stop();
    assert_eq!(
        player.state(),
        PlayerState::Error(AudioError::UnsupportedFormat)
    );
    assert_eq!(output.opens(), 0);

    player.load(&dir.path().join("missing.flac"));
    wait_until("io error", || {
        matches!(player.state(), PlayerState::Error(AudioError::Io(_)))
    });
}

#[test]
fn play_while_loading_plays_once_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav_i16(dir.path(), "a.wav", 48_000, 2, 48_000);
    let (engine, output) = stereo_48k();
    let player = engine.player();
    player.load(&path);
    player.play();
    wait_state(&player, &PlayerState::Playing);
    let out = pull(&player, &output, 20);
    assert_eq!(out, expected_stereo(0, 20, 1.0));
}

#[test]
fn loading_another_file_replaces_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let a = wav_i16(dir.path(), "a.wav", 48_000, 2, 48_000);
    let b = wav_i16(dir.path(), "b.wav", 8_000, 1, 4_000);
    let (engine, _output) = stereo_48k();
    let player = engine.player();
    player.load(&a);
    player.load(&b);
    wait_state(&player, &PlayerState::Stopped);
    assert_eq!(player.duration(), Some(Duration::from_millis(500)));
}

#[test]
fn other_rates_and_channels_are_converted() {
    let dir = tempfile::tempdir().unwrap();
    // 24 kHz mono into a 48 kHz stereo device: every source frame twice,
    // the second halfway to the next.
    let path = wav_i16(dir.path(), "m.wav", 24_000, 1, 24_000);
    let (engine, output) = stereo_48k();
    let player = engine.player();
    player.load(&path);
    wait_state(&player, &PlayerState::Stopped);
    player.play();
    let out = pull(&player, &output, 8);
    let s = |f| to_f32(sample(f, 0));
    let expected: Vec<f32> = (0..4)
        .flat_map(|f| {
            let mid = s(f) + (s(f + 1) - s(f)) * 0.5;
            [s(f), s(f), mid, mid]
        })
        .collect();
    assert_eq!(out, expected);
}

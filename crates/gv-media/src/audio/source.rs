// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Decoding: a file probed with symphonia, read packet by packet (never the
//! whole file at once), as interleaved `f32` frames.

use std::fs::File;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use super::AudioError;

/// More channels than this are refused (no file the plan lists has them).
const MAX_CHANNELS: usize = 32;
/// Packets that fail to decode in a row before the file counts as broken.
const MAX_BAD_PACKETS: u32 = 64;

/// What a loaded file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceInfo {
    /// Frames per second.
    pub sample_rate: u32,
    /// Interleaved samples per frame.
    pub channels: usize,
    /// Playable frames, when the container says.
    pub frames: Option<u64>,
}

impl SourceInfo {
    /// The length, when the container says.
    pub fn duration(&self) -> Option<Duration> {
        self.frames.map(|f| frames_to_duration(f, self.sample_rate))
    }
}

pub(crate) fn frames_to_duration(frames: u64, rate: u32) -> Duration {
    if rate == 0 {
        return Duration::ZERO;
    }
    let secs = frames / u64::from(rate);
    let rest = frames % u64::from(rate);
    Duration::new(secs, (rest * 1_000_000_000 / u64::from(rate)) as u32)
}

pub(crate) fn duration_to_frames(time: Duration, rate: u32) -> u64 {
    let nanos = time.as_nanos() * u128::from(rate) / 1_000_000_000;
    u64::try_from(nanos).unwrap_or(u64::MAX)
}

/// The decoders we register: symphonia's enabled ones plus libopus.
fn codecs() -> &'static CodecRegistry {
    static REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
        registry
    })
}

/// A file being decoded.
pub struct AudioSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    time_base: Option<TimeBase>,
    info: SourceInfo,
    /// After a seek: the first frame (in track timestamps) to keep.
    skip_until: Option<i64>,
    /// Frames delivered since the last seek, plus where that seek landed:
    /// the position of the next frame [`AudioSource::next_chunk`] returns.
    position: u64,
}

impl std::fmt::Debug for AudioSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioSource")
            .field("track_id", &self.track_id)
            .field("info", &self.info)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

impl AudioSource {
    /// Opens and probes `path`; the extension is a hint only.
    pub fn open(path: &Path) -> Result<Self, AudioError> {
        let file = File::open(path).map_err(|e| AudioError::Io(e.to_string()))?;
        let ext = path.extension().and_then(|e| e.to_str());
        Self::from_source(Box::new(file), ext)
    }

    /// Probes any seekable byte source.
    pub fn from_source(
        source: Box<dyn MediaSource>,
        extension: Option<&str>,
    ) -> Result<Self, AudioError> {
        let mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(ext) = extension {
            hint.with_extension(ext);
        }
        let reader = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| match e {
                SymError::IoError(e) if e.kind() != std::io::ErrorKind::UnexpectedEof => {
                    AudioError::Io(e.to_string())
                }
                _ => AudioError::UnsupportedFormat,
            })?;
        let track = match reader.default_track(TrackType::Audio) {
            Some(track) => track.clone(),
            // Tracks whose codec symphonia does not know have no type (AMR
            // in 3GP voice notes).
            None if !reader.tracks().is_empty() => {
                return Err(AudioError::UnsupportedCodec("unknown codec".into()));
            }
            None => return Err(AudioError::NoAudioTrack),
        };
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(AudioError::NoAudioTrack)?
            .clone();
        let decoder = codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| AudioError::UnsupportedCodec(e.to_string()))?;
        let sample_rate = params
            .sample_rate
            .filter(|r| *r > 0)
            .ok_or_else(|| AudioError::Malformed("no sample rate".into()))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|c| (1..=MAX_CHANNELS).contains(c))
            .ok_or_else(|| AudioError::Malformed("no usable channel layout".into()))?;
        let frames = track.num_frames.or_else(|| {
            let tb = track.time_base?;
            let time = tb.calc_duration(track.duration?)?;
            let nanos = u128::try_from(time.as_nanos()).ok()?;
            let secs = u64::try_from(nanos / 1_000_000_000).ok()?;
            let rest = u32::try_from(nanos % 1_000_000_000).ok()?;
            Some(duration_to_frames(Duration::new(secs, rest), sample_rate))
        });
        Ok(Self {
            reader,
            decoder,
            track_id: track.id,
            time_base: track.time_base,
            info: SourceInfo {
                sample_rate,
                channels,
                frames,
            },
            skip_until: None,
            position: 0,
        })
    }

    /// Opens `path` positioned at `frame`, for when seeking an open source
    /// fails: some demuxers cannot seek once they reached the end, or at
    /// all (a WebM without cues). Seeks the fresh source, and failing that
    /// decodes up to `frame`. Returns the source and the frames from
    /// `frame` on that were decoded on the way.
    pub fn open_at(path: &Path, frame: u64) -> Result<(Self, Vec<f32>), AudioError> {
        let mut source = Self::open(path)?;
        if frame == 0 {
            return Ok((source, Vec::new()));
        }
        if source.seek(frame).is_ok() {
            return Ok((source, Vec::new()));
        }
        let mut source = Self::open(path)?;
        let channels = source.info.channels;
        let mut chunk = Vec::new();
        loop {
            chunk.clear();
            let start = source.position;
            if !source.next_chunk(&mut chunk)? {
                return Ok((source, Vec::new()));
            }
            if source.position > frame {
                let skip = usize::try_from(frame.saturating_sub(start)).unwrap_or(usize::MAX);
                let from = skip.saturating_mul(channels).min(chunk.len());
                return Ok((source, chunk.split_off(from)));
            }
        }
    }

    /// Counts the playable frames of a stream whose container does not say
    /// (WebM from a recorder) by reading its packets without decoding.
    pub fn count_frames(path: &Path) -> Option<u64> {
        let mut source = Self::open(path).ok()?;
        let mut ticks: u64 = 0;
        loop {
            match source.reader.next_packet() {
                Ok(Some(packet)) if packet.track_id == source.track_id => {
                    ticks = ticks.saturating_add(packet.dur.get());
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(_) => return None,
            }
        }
        let ticks = i64::try_from(ticks).ok()?;
        Some(source.ticks_to_frames(ticks))
    }

    /// Sets the length found by [`AudioSource::count_frames`].
    pub fn set_frames(&mut self, frames: u64) {
        self.info.frames = Some(frames);
    }

    pub fn info(&self) -> SourceInfo {
        self.info
    }

    /// The frame the next chunk starts at.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Appends the next decoded frames to `out` (interleaved, `channels`
    /// per frame). `Ok(false)` at the end of the stream.
    pub fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, AudioError> {
        let mut bad = 0;
        let mut scratch: Vec<f32> = Vec::new();
        loop {
            let packet = match self.reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymError::IoError(e)) => return Err(AudioError::Io(e.to_string())),
                Err(e) => return Err(AudioError::Malformed(e.to_string())),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let pts = packet.pts.get();
            let decoded = match self.decoder.decode(&packet) {
                Ok(buf) => buf,
                Err(SymError::DecodeError(msg)) => {
                    bad += 1;
                    if bad > MAX_BAD_PACKETS {
                        return Err(AudioError::Malformed(msg.to_owned()));
                    }
                    continue;
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymError::Unsupported(what)) => {
                    return Err(AudioError::UnsupportedCodec(what.to_owned()));
                }
                Err(e) => return Err(AudioError::Malformed(e.to_string())),
            };
            bad = 0;
            let frames = decoded.frames();
            let channels = decoded.spec().channels().count();
            if frames == 0 || channels == 0 {
                continue;
            }
            scratch.clear();
            decoded.copy_to_vec_interleaved(&mut scratch);
            let mut skip = 0usize;
            if let Some(until) = self.skip_until {
                let skip_ticks = until.saturating_sub(pts).max(0);
                let skip_frames = self.ticks_to_frames(skip_ticks);
                if skip_frames >= frames as u64 {
                    continue;
                }
                skip = skip_frames as usize;
                self.skip_until = None;
            }
            let kept = frames - skip;
            append_mapped(
                out,
                &scratch[skip * channels..],
                channels,
                self.info.channels,
            );
            self.position += kept as u64;
            return Ok(true);
        }
    }

    /// Seeks so that the next chunk starts at `frame` (sample-accurate when
    /// the container allows). Returns the frame playback continues from.
    pub fn seek(&mut self, frame: u64) -> Result<u64, AudioError> {
        let target = match self.info.frames {
            Some(total) => frame.min(total),
            None => frame,
        };
        let time = frames_to_duration(target, self.info.sample_rate);
        let time = Time::try_from_secs_f64(time.as_secs_f64())
            .ok_or_else(|| AudioError::Malformed("seek target out of range".into()))?;
        let seeked = self
            .reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| AudioError::Seek(e.to_string()))?;
        self.decoder.reset();
        let required = seeked.required_ts.get();
        let actual = seeked.actual_ts.get();
        self.skip_until = (actual < required).then_some(required);
        let landed = self.ticks_to_frames(required.max(actual));
        self.position = landed;
        Ok(landed)
    }

    /// Track timestamp ticks to frames.
    fn ticks_to_frames(&self, ticks: i64) -> u64 {
        let ticks = ticks.max(0);
        match self.time_base {
            Some(tb) => {
                let time = tb.calc_time_saturating(Timestamp::new(ticks));
                let nanos = time.as_nanos().max(0) as u128;
                let frames = nanos * u128::from(self.info.sample_rate) / 1_000_000_000;
                u64::try_from(frames).unwrap_or(u64::MAX)
            }
            None => ticks as u64,
        }
    }
}

/// Appends `src` (interleaved, `from` channels) as `to` channels. A decoder
/// whose layout changes mid-stream is mapped back to the track's layout.
fn append_mapped(out: &mut Vec<f32>, src: &[f32], from: usize, to: usize) {
    if from == to {
        out.extend_from_slice(src);
        return;
    }
    for frame in src.chunks_exact(from) {
        for c in 0..to {
            out.push(frame.get(c).copied().unwrap_or(frame[0]));
        }
    }
}

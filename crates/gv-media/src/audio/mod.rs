// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Audio playback for audio embeds (FR-RENDER-07), behind the `audio`
//! feature.
//!
//! - [`AudioEngine`]: one per application. It owns the single output stream,
//!   opened lazily on the first [`Player::play`], and lets one player play
//!   at a time: starting one pauses the one that was playing.
//! - [`Player`]: one per embed. [`Player::load`] probes and decodes on a
//!   background thread, a fraction of a second ahead of the output, so long
//!   files are never held in memory. Play, pause, stop, seek, position,
//!   duration and volume; [`PlayerEvent`]s report state changes.
//! - Formats: MP3, AAC-LC (M4A, 3GP), ALAC, FLAC, Vorbis, PCM WAV through
//!   symphonia; Opus (Ogg, WebM) through libopus. Anything else ends in
//!   [`PlayerState::Error`] with [`AudioError::UnsupportedFormat`] or
//!   [`AudioError::UnsupportedCodec`]: the embed shows its card.
//! - Output: cpal on the system's default device ([`CpalBackend`]); no
//!   device is an error state, never a panic. Tests and headless runs use
//!   [`NullBackend`], which never opens a device.
//!
//! ```no_run
//! # use gv_media::audio::*;
//! let engine = AudioEngine::system();
//! let player = engine.player();
//! player.set_listener(|event| println!("{event:?}"));
//! player.load("voice.mp3".as_ref());
//! player.play(); // starts once loaded
//! ```

mod engine;
mod output;
mod source;
mod voice;

use std::fmt;
use std::time::Duration;

pub use engine::{AudioEngine, Player};
pub use output::{
    AudioBackend, CpalBackend, NullBackend, NullOutput, OutputFormat, OutputStream, RenderFn,
};
pub use source::{AudioSource, SourceInfo};

/// Why a file does not play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// The file could not be read.
    Io(String),
    /// Not a container we read (or not audio at all).
    UnsupportedFormat,
    /// A container we read, with a codec we do not decode (AMR, for
    /// example).
    UnsupportedCodec(String),
    /// A container without an audio track.
    NoAudioTrack,
    /// A broken stream.
    Malformed(String),
    /// The stream could not be repositioned.
    Seek(String),
    /// There is no output device. Playing again retries.
    NoOutputDevice,
    /// The output device refused the stream. Playing again retries.
    Output(String),
    /// The decoder panicked; the panic was caught.
    Panicked,
}

impl AudioError {
    /// Whether the file itself is fine and only the output failed: playing
    /// again may work.
    pub fn is_output(&self) -> bool {
        matches!(self, Self::NoOutputDevice | Self::Output(_))
    }

    /// One short sentence for the embed.
    pub fn message(&self) -> String {
        match self {
            Self::Io(e) => format!("The file could not be read: {e}"),
            Self::UnsupportedFormat => "This audio format is not supported".into(),
            Self::UnsupportedCodec(_) => "This audio codec is not supported".into(),
            Self::NoAudioTrack => "The file has no audio track".into(),
            Self::Malformed(_) | Self::Panicked => "The audio file is damaged".into(),
            Self::Seek(_) => "The audio file cannot be repositioned".into(),
            Self::NoOutputDevice => "No sound output device".into(),
            Self::Output(e) => format!("Sound output failed: {e}"),
        }
    }
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedCodec(c) => write!(f, "unsupported audio codec: {c}"),
            Self::Malformed(e) => write!(f, "malformed audio: {e}"),
            Self::Seek(e) => write!(f, "seek failed: {e}"),
            _ => f.write_str(&self.message()),
        }
    }
}

impl std::error::Error for AudioError {}

/// Where a player is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PlayerState {
    /// Nothing loaded.
    #[default]
    Empty,
    /// Probing the file.
    Loading,
    /// Loaded, not playing, at the start (or after [`Player::stop`]).
    Stopped,
    Playing,
    Paused,
    /// Played to the end. Playing again starts over.
    Ended,
    Error(AudioError),
}

impl PlayerState {
    pub fn is_playing(&self) -> bool {
        matches!(self, Self::Playing)
    }
}

/// What a player reports to its listener. Listeners run on the player's
/// decoding thread or on the thread that called the player, never on the
/// output thread; they must not block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The file is probed; its length when known.
    Loaded {
        duration: Option<Duration>,
    },
    StateChanged(PlayerState),
}
